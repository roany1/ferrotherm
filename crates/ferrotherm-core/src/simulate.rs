// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Steady and transient solution of the discrete model.
//!
//! Radiation is linearised per boundary element
//! (h_rad = εσ(Ts²+Tr²)(Ts+Tr), absolute temperatures) and the system is
//! re-solved (Picard) until the largest temperature change is below the
//! nonlinear tolerance. Transient: implicit (backward) Euler.

use crate::case::{Mode, SolverSettings};
use crate::error::{Error, Result};
use crate::linalg::{pcg_monitored, Operator};
use crate::model::{Model, SurfaceBc};

pub const SIGMA: f64 = 5.670_374_419e-8;
pub const KELVIN: f64 = 273.15;

/// Temperature field at one instant (all unknowns: cells then junctions), °C.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub time_s: f64,
    pub t: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct HeatFlow {
    /// Heat leaving through each case boundary, W (index = case boundary).
    pub per_boundary: Vec<f64>,
    pub total_out: f64,
    pub total_in: f64,
    /// Rate of change of stored energy, W (transient only).
    pub storage_rate: f64,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub snapshots: Vec<Snapshot>,
    /// (time, junction temperatures in component order)
    pub junction_history: Vec<(f64, Vec<f64>)>,
    pub linear_iterations: usize,
    pub nonlinear_iterations: usize,
    pub heat: HeatFlow,
    /// Surface temperature at each boundary element (final).
    pub surface_t: Vec<f64>,
    pub elapsed_s: f64,
}

impl RunResult {
    pub fn last(&self) -> &Snapshot {
        self.snapshots.last().expect("at least one snapshot")
    }
}

/// Equivalent conductance and sink temperature of a boundary element given
/// the current estimate of its surface temperature.
#[inline]
fn element_g(r_int: f64, area: f64, bc: SurfaceBc, ts: f64) -> (f64, f64) {
    match bc {
        SurfaceBc::Fixed { t } => (1.0 / r_int, t),
        SurfaceBc::Exchange {
            h,
            t_amb,
            emissivity,
            t_rad,
        } => {
            let hr = if emissivity > 0.0 {
                let a = ts + KELVIN;
                let b = t_rad + KELVIN;
                emissivity * SIGMA * (a * a + b * b) * (a + b)
            } else {
                0.0
            };
            let htot = h + hr;
            if htot <= 0.0 {
                return (0.0, 0.0);
            }
            let teq = (h * t_amb + hr * t_rad) / htot;
            (1.0 / (r_int + 1.0 / (htot * area)), teq)
        }
    }
}

struct System<'a> {
    op: Operator<'a>,
    b: Vec<f64>,
}

fn assemble<'a>(m: &'a Model, ts: &[f64], cap_dt: Option<(&[f64], f64)>) -> System<'a> {
    let n = m.grid.n();
    let nc = m.ncells;
    let mut diag = vec![0.0; m.nnodes];
    let stride = [1, n[0], n[0] * n[1]];
    for c in 0..nc {
        for a in 0..3 {
            let gp = m.g[a][c];
            if gp != 0.0 {
                diag[c] += gp;
                diag[c + stride[a]] += gp;
            }
        }
    }
    let mut b = m.source.clone();
    let mut links = Vec::with_capacity(m.links.len());
    for l in &m.links {
        diag[l.a] += l.g;
        diag[l.b] += l.g;
        links.push((l.a, l.b, l.g));
    }
    for (e, &tsurf) in m.boundary.iter().zip(ts) {
        let (g, teq) = element_g(e.r_int, e.area, e.bc, tsurf);
        diag[e.node] += g;
        b[e.node] += g * teq;
    }
    if let Some((t_old, dt)) = cap_dt {
        for i in 0..m.nnodes {
            let cdt = m.capacity[i] / dt;
            diag[i] += cdt;
            b[i] += cdt * t_old[i];
        }
    }
    System {
        op: Operator {
            n,
            ncells: nc,
            diag,
            g: [&m.g[0], &m.g[1], &m.g[2]],
            links,
        },
        b,
    }
}

/// Surface temperature of each boundary element for a given field.
fn surface_temps(m: &Model, t: &[f64], ts_prev: &[f64]) -> Vec<f64> {
    m.boundary
        .iter()
        .zip(ts_prev)
        .map(|(e, &tp)| {
            let (g, teq) = element_g(e.r_int, e.area, e.bc, tp);
            let q = g * (t[e.node] - teq);
            t[e.node] - q * e.r_int
        })
        .collect()
}

fn has_radiation(m: &Model) -> bool {
    m.boundary
        .iter()
        .any(|e| matches!(e.bc, SurfaceBc::Exchange { emissivity, .. } if emissivity > 0.0))
}

pub fn heat_flow(m: &Model, t: &[f64], ts: &[f64], nb: usize, storage_rate: f64) -> HeatFlow {
    let mut per = vec![0.0; nb];
    for (e, &tsurf) in m.boundary.iter().zip(ts) {
        let (g, teq) = element_g(e.r_int, e.area, e.bc, tsurf);
        per[e.source] += g * (t[e.node] - teq);
    }
    HeatFlow {
        total_out: per.iter().sum(),
        per_boundary: per,
        total_in: m.total_power(),
        storage_rate,
    }
}

fn junctions(m: &Model, t: &[f64]) -> Vec<f64> {
    m.components.iter().map(|c| t[c.node]).collect()
}

/// Progress events emitted while solving.
#[derive(Debug, Clone, Copy)]
pub enum Event<'a> {
    /// Free-text status.
    Message(&'a str),
    /// Linear-solver iteration and relative residual vs its tolerance.
    Iteration {
        iteration: usize,
        residual: f64,
        tolerance: f64,
    },
    /// Radiation (outer) iteration number.
    Outer(usize),
    /// Transient time step completed.
    Step {
        step: usize,
        steps: usize,
        time_s: f64,
    },
}

/// Run the simulation described by `settings`.
pub fn run(
    m: &Model,
    settings: &SolverSettings,
    mut progress: impl FnMut(&str),
) -> Result<RunResult> {
    run_observed(m, settings, &mut |e| {
        match e {
            Event::Message(s) => progress(s),
            Event::Step {
                step,
                steps,
                time_s,
            } if step % (steps / 20).max(1) == 0 => {
                progress(&format!("t = {time_s:.3} s ({step}/{steps})"))
            }
            _ => {}
        }
        true
    })
}

/// Run with a full event observer. The observer returns `false` to cancel,
/// in which case [`Error::Cancelled`] is returned.
pub fn run_observed(
    m: &Model,
    settings: &SolverSettings,
    observer: &mut dyn FnMut(Event) -> bool,
) -> Result<RunResult> {
    let obs = std::cell::RefCell::new(observer);
    let start = std::time::Instant::now();
    let rad = has_radiation(m);
    let mut t = vec![m.initial_c; m.nnodes];
    let mut ts = vec![m.initial_c; m.boundary.len()];
    let mut lin_it = 0;
    let mut nl_it = 0;
    let nb = m.boundary_names.len();

    // One nonlinear solve (steady, or one implicit step).
    let solve = |t: &mut Vec<f64>,
                 ts: &mut Vec<f64>,
                 cap: Option<(&[f64], f64)>,
                 lin_it: &mut usize,
                 nl_it: &mut usize|
     -> Result<()> {
        let max_nl = if rad {
            settings.max_nonlinear_iterations.max(1)
        } else {
            1
        };
        // Radiation iterations: solve loosely while the field is still moving,
        // and only accept convergence after a solve at the full tolerance.
        let loose = settings.tolerance.max(1e-6);
        let mut tight = !rad;
        for it in 0..max_nl {
            let sys = assemble(m, ts, cap);
            let prev = t.clone();
            let tol = if tight { settings.tolerance } else { loose };
            if rad && !(obs.borrow_mut())(Event::Outer(*nl_it + 1)) {
                return Err(Error::Cancelled);
            }
            let st = pcg_monitored(
                &sys.op,
                &sys.b,
                t,
                tol,
                settings.max_iterations,
                &mut |iteration, residual| {
                    (obs.borrow_mut())(Event::Iteration {
                        iteration,
                        residual,
                        tolerance: tol,
                    })
                },
            )?;
            *lin_it += st.iterations;
            *nl_it += 1;
            let new_ts = surface_temps(m, t, ts);
            let dmax = t
                .iter()
                .zip(&prev)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max);
            *ts = new_ts;
            if !rad {
                return Ok(());
            }
            if it > 0 && dmax < settings.nonlinear_tolerance_k {
                if tight {
                    return Ok(());
                }
                tight = true;
            } else if it > 0 && dmax < 10.0 * settings.nonlinear_tolerance_k {
                tight = true;
            }
        }
        if rad {
            return Err(Error::Invalid(format!(
                "radiation iterations did not converge in {} iterations",
                settings.max_nonlinear_iterations
            )));
        }
        Ok(())
    };

    let mut snapshots = Vec::new();
    let mut history = Vec::new();
    let storage_rate;
    match settings.mode {
        Mode::Steady => {
            if !(obs.borrow_mut())(Event::Message("solving steady state")) {
                return Err(Error::Cancelled);
            }
            solve(&mut t, &mut ts, None, &mut lin_it, &mut nl_it)?;
            history.push((0.0, junctions(m, &t)));
            snapshots.push(Snapshot {
                time_s: 0.0,
                t: t.clone(),
            });
            storage_rate = 0.0;
        }
        Mode::Transient => {
            let tr = settings.transient.as_ref().ok_or_else(|| {
                Error::Invalid("transient mode needs [solver.transient] dt_s and end_s".into())
            })?;
            if !(tr.dt_s > 0.0 && tr.end_s > 0.0) {
                return Err(Error::Invalid(
                    "transient dt_s and end_s must be > 0".into(),
                ));
            }
            let nsteps = (tr.end_s / tr.dt_s).round().max(1.0) as usize;
            let dt = tr.end_s / nsteps as f64;
            let every = tr
                .output_every_s
                .map(|e| ((e / dt).round() as usize).max(1));
            snapshots.push(Snapshot {
                time_s: 0.0,
                t: t.clone(),
            });
            history.push((0.0, junctions(m, &t)));
            let mut t_old = t.clone();
            let mut rate = 0.0;
            for step in 1..=nsteps {
                let time = step as f64 * dt;
                solve(&mut t, &mut ts, Some((&t_old, dt)), &mut lin_it, &mut nl_it)?;
                rate = (0..m.nnodes)
                    .map(|i| m.capacity[i] * (t[i] - t_old[i]) / dt)
                    .sum();
                history.push((time, junctions(m, &t)));
                let write = step == nsteps || every.map(|e| step % e == 0).unwrap_or(false);
                if write {
                    snapshots.push(Snapshot {
                        time_s: time,
                        t: t.clone(),
                    });
                }
                if !(obs.borrow_mut())(Event::Step {
                    step,
                    steps: nsteps,
                    time_s: time,
                }) {
                    return Err(Error::Cancelled);
                }
                t_old.clone_from(&t);
            }
            storage_rate = rate;
        }
    }
    let heat = heat_flow(m, &t, &ts, nb, storage_rate);
    Ok(RunResult {
        snapshots,
        junction_history: history,
        linear_iterations: lin_it,
        nonlinear_iterations: nl_it,
        heat,
        surface_t: ts,
        elapsed_s: start.elapsed().as_secs_f64(),
    })
}
