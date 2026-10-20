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
use crate::model::{conductances, BoundaryElement, Model, SurfaceBc};
use crate::radiation::S2s;

pub const SIGMA: f64 = 5.670_374_419e-8;
pub const KELVIN: f64 = 273.15;

/// Fields at one instant.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub time_s: f64,
    /// Temperature of every unknown (cells, then network nodes), °C.
    pub t: Vec<f64>,
    /// Cell-centred velocity, m/s (flow runs).
    pub vel: Option<Vec<[f32; 3]>>,
    /// Cell pressure relative to ambient, Pa (flow runs).
    pub p: Option<Vec<f32>>,
}

impl Snapshot {
    pub fn thermal(time_s: f64, t: Vec<f64>) -> Snapshot {
        Snapshot {
            time_s,
            t,
            vel: None,
            p: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HeatFlow {
    /// Heat leaving through each case boundary, W (index = case boundary).
    pub per_boundary: Vec<f64>,
    pub total_out: f64,
    pub total_in: f64,
    /// Rate of change of stored energy, W (transient only).
    pub storage_rate: f64,
    /// Heat carried out by the air through openings/inlets, W (in `total_out`).
    pub advected_out: f64,
    /// Heat radiated out through openings (internal radiation), W (in `total_out`).
    pub radiation_to_openings: f64,
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
    /// Flow results (CFD runs).
    pub flow: Option<crate::flow::FlowResult>,
}

impl RunResult {
    pub fn last(&self) -> &Snapshot {
        self.snapshots.last().expect("at least one snapshot")
    }
}

/// Linearised contribution of a boundary element to its node equation,
/// given the current estimate of its surface temperature.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ElemTerms {
    /// Conductance node → sink, W/K.
    pub g: f64,
    /// Sink temperature, °C.
    pub teq: f64,
    /// Part of the absorbed surface heat that reaches the node, W.
    pub q_node: f64,
    /// External surface conductance (h·A), W/K; infinite for a fixed temperature.
    pub g_ext: f64,
}

#[inline]
pub(crate) fn element_terms(e: &BoundaryElement, ts: f64) -> ElemTerms {
    match e.bc {
        SurfaceBc::Fixed { t } => ElemTerms {
            g: 1.0 / e.r_int,
            teq: t,
            q_node: 0.0,
            g_ext: f64::INFINITY,
        },
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
                return ElemTerms {
                    g: 0.0,
                    teq: 0.0,
                    q_node: e.q_abs,
                    g_ext: 0.0,
                };
            }
            let g_ext = htot * e.area;
            let teq = (h * t_amb + hr * t_rad) / htot;
            ElemTerms {
                g: 1.0 / (e.r_int + 1.0 / g_ext),
                teq,
                q_node: e.q_abs / (1.0 + g_ext * e.r_int),
                g_ext,
            }
        }
    }
}

/// Surface temperature and heat leaving to the surroundings, W.
#[inline]
pub(crate) fn element_state(e: &BoundaryElement, tn: f64, ts_prev: f64) -> (f64, f64) {
    let et = element_terms(e, ts_prev);
    if et.g_ext.is_infinite() {
        let ts = et.teq;
        return (ts, (tn - ts) / e.r_int + e.q_abs);
    }
    if et.g_ext == 0.0 {
        // adiabatic: all the absorbed heat enters the node
        return (tn + e.q_abs * e.r_int, 0.0);
    }
    let ts = (tn / e.r_int + e.q_abs + et.g_ext * et.teq) / (1.0 / e.r_int + et.g_ext);
    (ts, et.g_ext * (ts - et.teq))
}

struct System<'a> {
    op: Operator<'a>,
    b: Vec<f64>,
}

/// Extra linearised sources per node: (node, coefficient added to the
/// diagonal, constant added to the right-hand side).
pub(crate) type NodeSources = Vec<(usize, f64, f64)>;

fn assemble<'a>(
    m: &'a Model,
    g: &'a [Vec<f64>; 3],
    bnd: &[BoundaryElement],
    ts: &[f64],
    cap_dt: Option<(&[f64], f64)>,
    extra: &NodeSources,
) -> System<'a> {
    let n = m.grid.n();
    let nc = m.ncells;
    let mut diag = vec![0.0; m.nnodes];
    let stride = [1, n[0], n[0] * n[1]];
    for c in 0..nc {
        for a in 0..3 {
            let gp = g[a][c];
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
    for (e, &tsurf) in bnd.iter().zip(ts) {
        let et = element_terms(e, tsurf);
        diag[e.node] += et.g;
        b[e.node] += et.g * et.teq + et.q_node;
    }
    for &(node, sp, sc) in extra {
        diag[node] += sp;
        b[node] += sc;
    }
    if let Some((t_old, dt)) = cap_dt {
        for i in 0..m.nnodes {
            let cdt = m.capacity[i] / dt;
            diag[i] += cdt;
            b[i] += cdt * t_old[i];
        }
    }
    // isolated network nodes without any conductance (guard)
    for (i, d) in diag.iter_mut().enumerate().skip(nc) {
        if *d == 0.0 {
            *d = 1.0;
            b[i] = m.initial_c;
        }
    }
    System {
        op: Operator {
            n,
            ncells: nc,
            diag,
            g: [&g[0], &g[1], &g[2]],
            links,
        },
        b,
    }
}

/// Surface temperature of each boundary element for a given field.
pub(crate) fn surface_temps(m: &Model, t: &[f64], ts_prev: &[f64]) -> Vec<f64> {
    surface_temps_with(&m.boundary, t, ts_prev)
}

pub(crate) fn surface_temps_with(bnd: &[BoundaryElement], t: &[f64], ts_prev: &[f64]) -> Vec<f64> {
    bnd.iter()
        .zip(ts_prev)
        .map(|(e, &tp)| element_state(e, t[e.node], tp).0)
        .collect()
}

fn has_radiation(m: &Model) -> bool {
    m.boundary
        .iter()
        .any(|e| matches!(e.bc, SurfaceBc::Exchange { emissivity, .. } if emissivity > 0.0))
}

pub fn heat_flow(m: &Model, t: &[f64], ts: &[f64], nb: usize, storage_rate: f64) -> HeatFlow {
    let mut per = vec![0.0; nb];
    let bnd = m.boundary_at(&m.k_at(t));
    for (e, &tsurf) in bnd.iter().zip(ts) {
        per[e.source] += element_state(e, t[e.node], tsurf).1;
    }
    HeatFlow {
        total_out: per.iter().sum(),
        per_boundary: per,
        total_in: m.total_power(),
        storage_rate,
        advected_out: 0.0,
        radiation_to_openings: 0.0,
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
    /// Flow (SIMPLEC) outer iteration with normalised [mass, momentum, energy] residuals.
    FlowIteration {
        iteration: usize,
        max_iterations: usize,
        residuals: [f64; 3],
    },
    /// Transient time step completed.
    Step {
        step: usize,
        steps: usize,
        time_s: f64,
    },
}

/// Run the simulation described by `settings` (conduction).
pub fn run(
    m: &Model,
    settings: &SolverSettings,
    mut progress: impl FnMut(&str),
) -> Result<RunResult> {
    run_observed(m, settings, None, &mut |e| {
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

/// Conduction solve with a full event observer. The observer returns
/// `false` to cancel, in which case [`Error::Cancelled`] is returned.
pub fn run_observed(
    m: &Model,
    settings: &SolverSettings,
    s2s: Option<&S2s>,
    observer: &mut dyn FnMut(Event) -> bool,
) -> Result<RunResult> {
    let obs = std::cell::RefCell::new(observer);
    let start = std::time::Instant::now();
    let nonlinear = has_radiation(m) || m.has_kt || s2s.is_some();
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
        let max_nl = if nonlinear {
            settings.max_nonlinear_iterations.max(1)
        } else {
            1
        };
        // Nonlinear iterations: solve loosely while the field is still moving,
        // and only accept convergence after a solve at the full tolerance.
        let loose = settings.tolerance.max(1e-6);
        let mut tight = !nonlinear;
        for it in 0..max_nl {
            let g_t;
            let k_now = if m.has_kt { m.k_at(t) } else { Vec::new() };
            let g = if m.has_kt {
                g_t = conductances(&m.grid, &k_now);
                &g_t
            } else {
                &m.g
            };
            let bnd = m.boundary_at(&k_now);
            let extra = match s2s {
                Some(r) => r.sources(t),
                None => Vec::new(),
            };
            let sys = assemble(m, g, &bnd, ts, cap, &extra);
            let prev = t.clone();
            let tol = if tight { settings.tolerance } else { loose };
            if nonlinear && !(obs.borrow_mut())(Event::Outer(*nl_it + 1)) {
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
            let new_ts = surface_temps_with(&bnd, t, ts);
            let dmax = t
                .iter()
                .zip(&prev)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max);
            *ts = new_ts;
            if !nonlinear {
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
        if nonlinear {
            return Err(Error::Invalid(format!(
                "nonlinear (radiation / k(T)) iterations did not converge in {} iterations",
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
            snapshots.push(Snapshot::thermal(0.0, t.clone()));
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
            snapshots.push(Snapshot::thermal(0.0, t.clone()));
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
                    snapshots.push(Snapshot::thermal(time, t.clone()));
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
    let mut heat = heat_flow(m, &t, &ts, nb, storage_rate);
    if let Some(r) = s2s {
        heat.radiation_to_openings = r.loss_to_openings(&t);
        heat.total_out += heat.radiation_to_openings;
    }
    Ok(RunResult {
        snapshots,
        junction_history: history,
        linear_iterations: lin_it,
        nonlinear_iterations: nl_it,
        heat,
        surface_t: ts,
        elapsed_s: start.elapsed().as_secs_f64(),
        flow: None,
    })
}
