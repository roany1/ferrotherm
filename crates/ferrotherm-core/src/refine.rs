// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Local grid refinement by nested grids, coupled both ways.
//!
//! The grid of FerroTherm is a conforming tensor grid: a plane needed by a
//! thin fin runs through the whole domain. Here a region is instead solved
//! on a grid of its own, finer, and coupled with the main grid by
//! overlapping (Schwarz) iteration:
//!
//! 1. the main grid is solved with the fine solution restricted to its
//!    cells inside the region, less one cell along each face of the region
//!    (the overlap), see [`crate::flow::Nudge`]: the temperatures are pulled
//!    to the fine ones (shape imposed, energy balance kept by an offset per
//!    region), and the momentum equations of the faces between such cells
//!    get the τ-correction of the fine velocities and pressures, which
//!    carries the fine pressure drop (and so the flow split) to the main
//!    grid;
//! 2. each region is solved on its fine grid with the flow and temperatures
//!    of the main grid on its faces, as a sub-model ([`crate::submodel`]);
//! 3. repeated until no block temperature in a region changes by more than
//!    `tolerance_k` between cycles.
//!
//! In the overlap the main grid keeps solving its own equations, so the
//! flow and the heat that the fine grid hands back reach the rest of the
//! domain through the main-grid equations. Blocks, components and monitors
//! lying entirely inside a region are reported from its fine grid. The
//! faces of a region should cross plain air or solids that are not hot
//! spots: the main-grid cells of the overlap are not corrected.

use crate::case::{Case, RefineRegion};
use crate::error::{Error, Result};
use crate::flow::{Checkpoint, Nudge, RunOptions};
use crate::mesh::Grid;
use crate::report::RefinedRegion;
use crate::simulate::Event;
use crate::submodel::{box_of_blocks, submodel};
use crate::{Output, Simulation};

const MM: f64 = 1e-3;
/// Penalty weight of the temperature nudge: the main grid follows the fine
/// temperatures to about 1 %.
const NUDGE_WEIGHT: f64 = 100.0;

/// Box of a refinement region, mm.
pub fn region_box(case: &Case, r: &RefineRegion) -> Result<([f64; 3], [f64; 3])> {
    let (mut lo, mut hi) = match (r.min, r.max) {
        (Some(lo), Some(hi)) => (lo, hi),
        _ if !r.around.is_empty() => box_of_blocks(case, &r.around)?,
        _ => {
            return Err(Error::Invalid(format!(
                "refinement region '{}': give `around` or `min` and `max`",
                r.name
            )))
        }
    };
    let m = match r.margin_mm.len() {
        0 => [0.0; 6],
        1 => [r.margin_mm[0]; 6],
        6 => [
            r.margin_mm[0],
            r.margin_mm[1],
            r.margin_mm[2],
            r.margin_mm[3],
            r.margin_mm[4],
            r.margin_mm[5],
        ],
        _ => {
            return Err(Error::Invalid(format!(
                "refinement region '{}': margin_mm takes 1 or 6 values",
                r.name
            )))
        }
    };
    for a in 0..3 {
        lo[a] = (lo[a] - m[a]).max(case.domain.min[a]);
        hi[a] = (hi[a] + m[a + 3]).min(case.domain.max[a]);
        if !(hi[a] > lo[a]) {
            return Err(Error::Invalid(format!(
                "refinement region '{}' is empty",
                r.name
            )));
        }
    }
    Ok((lo, hi))
}

/// Overlap of [x0, x1] with the cells of `nodes`: (cell, length).
fn overlaps(nodes: &[f64], x0: f64, x1: f64) -> Vec<(usize, f64)> {
    let mut v = Vec::new();
    for i in 0..nodes.len().saturating_sub(1) {
        let l = nodes[i].max(x0);
        let h = nodes[i + 1].min(x1);
        if h > l {
            v.push((i, h - l));
        }
    }
    v
}

/// Main-grid cell range [lo, hi) of the box (snapped outwards as the
/// sub-model does), and of the nudged cells (less one cell of overlap on
/// faces inside the domain).
fn cell_ranges(g: &Grid, bmin: [f64; 3], bmax: [f64; 3]) -> ([usize; 3], [usize; 3]) {
    let n = g.n();
    let mut lo = [0usize; 3];
    let mut hi = [0usize; 3];
    for a in 0..3 {
        let nodes = &g.nodes[a];
        let i0 = nodes
            .iter()
            .rposition(|&x| x <= bmin[a] * MM + 1e-9)
            .unwrap_or(0);
        let i1 = nodes
            .iter()
            .position(|&x| x >= bmax[a] * MM - 1e-9)
            .unwrap_or(n[a]);
        lo[a] = if i0 == 0 { 0 } else { i0 + 1 };
        hi[a] = if i1 == n[a] {
            n[a]
        } else {
            i1.saturating_sub(1)
        };
        if hi[a] < lo[a] {
            hi[a] = lo[a];
        }
    }
    (lo, hi)
}

/// Fine solution restricted to the nudged main-grid cells and faces.
fn restrict(
    cg: &Grid,
    fm: &crate::Model,
    fine: &Checkpoint,
    lo: [usize; 3],
    hi: [usize; 3],
    group: usize,
    nd: &mut Nudge,
) {
    let fg = &fm.grid;
    let fnn = fg.n();
    let ft = &fine.fields[4];
    let fp = &fine.fields[3];
    let ffluid = |c: usize| fm.materials[fm.cell_material[c] as usize].is_fluid();
    // per axis and main-grid cell: overlapping fine cells
    let ov: [Vec<Vec<(usize, f64)>>; 3] = [0, 1, 2].map(|a| {
        (0..cg.n()[a])
            .map(|i| {
                if i >= lo[a] && i < hi[a] {
                    overlaps(&fg.nodes[a], cg.nodes[a][i], cg.nodes[a][i + 1])
                } else {
                    Vec::new()
                }
            })
            .collect()
    });
    for k in lo[2]..hi[2] {
        for j in lo[1]..hi[1] {
            for i in lo[0]..hi[0] {
                let (mut s, mut w) = (0.0, 0.0);
                let (mut sp, mut wp) = (0.0, 0.0);
                for &(fi, lx) in &ov[0][i] {
                    for &(fj, ly) in &ov[1][j] {
                        for &(fk, lz) in &ov[2][k] {
                            let v = lx * ly * lz;
                            let c = fg.idx(fi, fj, fk);
                            s += ft[c] * v;
                            w += v;
                            if ffluid(c) {
                                sp += fp[c] * v;
                                wp += v;
                            }
                        }
                    }
                }
                if w > 0.0 {
                    nd.t.push((cg.idx(i, j, k), s / w, group));
                }
                if wp > 0.0 {
                    nd.p.push((cg.idx(i, j, k), sp / wp));
                }
            }
        }
    }
    let cn = cg.n();
    for a in 0..3 {
        let t = [(a + 1) % 3, (a + 2) % 3];
        let (t0, t1) = (t[0].min(t[1]), t[0].max(t[1]));
        let mut cd = cn;
        cd[a] += 1;
        let mut fdm = fnn;
        fdm[a] += 1;
        let u = &fine.fields[a];
        for pa in (lo[a] + 1)..hi[a] {
            let x = cg.nodes[a][pa];
            // nearest fine plane
            let fpl = fg.nodes[a]
                .iter()
                .enumerate()
                .min_by(|p, q| (p.1 - x).abs().total_cmp(&(q.1 - x).abs()))
                .map(|p| p.0)
                .unwrap_or(0);
            for q1 in lo[t1]..hi[t1] {
                for q0 in lo[t0]..hi[t0] {
                    let (mut s, mut w) = (0.0, 0.0);
                    for &(f0, l0) in &ov[t0][q0] {
                        for &(f1, l1) in &ov[t1][q1] {
                            let mut p = [0usize; 3];
                            p[a] = fpl;
                            p[t0] = f0;
                            p[t1] = f1;
                            let v = l0 * l1;
                            s += u[p[0] + fdm[0] * (p[1] + fdm[1] * p[2])] * v;
                            w += v;
                        }
                    }
                    if w > 0.0 {
                        let mut p = [0usize; 3];
                        p[a] = pa;
                        p[t0] = q0;
                        p[t1] = q1;
                        nd.u[a].push((p[0] + cd[0] * (p[1] + cd[1] * p[2]), s / w));
                    }
                }
            }
        }
    }
}

/// Progress saved next to the checkpoint so that a restarted run keeps its
/// cycle count and can measure the change of its first cycle.
#[derive(serde::Serialize, serde::Deserialize)]
struct Progress {
    /// Cycle (0-based) in progress when saved.
    cycle: usize,
    /// Per region: block → max temperature of its last solve.
    prev: Vec<Vec<(String, f64)>>,
}

struct Fine {
    name: String,
    min: [f64; 3],
    max: [f64; 3],
    state: Option<Checkpoint>,
    out: Option<Output>,
    cells: usize,
    /// Block → max temperature in the previous cycle.
    prev: Vec<(String, f64)>,
    change: f64,
}

/// Fine-grid case of a region, cut out of the main-grid solution `cstate`.
fn fine_sim(
    sim: &Simulation,
    cstate: &Checkpoint,
    fine: &Fine,
    r: &RefineRegion,
    iterations: usize,
) -> Result<(Simulation, crate::submodel::SubmodelInfo)> {
    let (mut fcase, info) = submodel(sim, cstate, fine.min, fine.max)?;
    fcase.refinement = None;
    fcase.grid.max_cell_mm = r.max_cell_mm;
    if let Some(mc) = r.min_cells_per_interval {
        fcase.grid.min_cells_per_interval = mc;
    }
    if let Some(f) = fcase.flow.as_mut() {
        f.max_iterations = iterations;
    }
    Ok((Simulation::with_library(fcase, sim.library.clone())?, info))
}

/// Solve `sim` with its refinement regions (see the module documentation).
///
/// With a checkpoint file `x.ftc` in the run options, the solution of
/// region k is saved as `x.region<k>.ftc` after each of its solves; a run
/// restarted from `x.ftc` picks them up and starts from the coupled state.
pub fn run(sim: &Simulation, observer: &mut dyn FnMut(Event) -> bool) -> Result<Output> {
    let cfg = sim
        .case
        .refinement
        .clone()
        .ok_or_else(|| Error::Invalid("no [refinement]".into()))?;
    let cg = &sim.model.grid;
    let mut fines: Vec<Fine> = Vec::new();
    for (k, r) in cfg.regions.iter().enumerate() {
        let (lo, hi) = region_box(&sim.case, r)?;
        let name = if r.name.is_empty() {
            r.around
                .first()
                .cloned()
                .unwrap_or_else(|| format!("region {}", k + 1))
        } else {
            r.name.clone()
        };
        fines.push(Fine {
            name,
            min: lo,
            max: hi,
            state: None,
            out: None,
            cells: 0,
            prev: Vec::new(),
            change: f64::INFINITY,
        });
    }
    let fine_path = |k: usize| {
        sim.options
            .checkpoint
            .as_ref()
            .map(|p| p.with_extension(format!("region{}.ftc", k + 1)))
    };
    let progress_path = sim
        .options
        .checkpoint
        .as_ref()
        .map(|p| p.with_extension("refine.json"));
    let mut coarse_state = sim.options.restart.clone();
    let mut nudge: Option<Nudge> = None;
    let mut first_cycle = 0;
    // restart: fine solutions saved by an earlier run
    if let Some(cs) = &coarse_state {
        let mut nd = Nudge {
            weight: NUDGE_WEIGHT,
            ..Default::default()
        };
        let mut any = false;
        for (gi, (fine, r)) in fines.iter_mut().zip(&cfg.regions).enumerate() {
            let Some(path) = fine_path(gi) else { continue };
            let Ok(st) = Checkpoint::read(&path) else {
                continue;
            };
            let (fsim, info) = fine_sim(sim, cs, fine, r, cfg.fine_iterations)?;
            if st.cells != fsim.model.grid.n() {
                continue;
            }
            let (lo, hi) = cell_ranges(cg, info.min, info.max);
            restrict(cg, &fsim.model, &st, lo, hi, gi, &mut nd);
            fine.state = Some(st);
            any = true;
        }
        if any {
            nudge = Some(nd);
            // cycle count and block temperatures of the earlier run
            if let Some(pr) = progress_path
                .as_ref()
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| serde_json::from_slice::<Progress>(&b).ok())
            {
                if pr.prev.len() == fines.len() {
                    first_cycle = pr.cycle.min(cfg.cycles.max(1) - 1);
                    for (f, p) in fines.iter_mut().zip(pr.prev) {
                        f.prev = p;
                    }
                }
            }
        }
    }
    let mut coarse_out: Option<Output> = None;
    let mut cycles = 0;
    let mut settled = false;
    let msg = |observer: &mut dyn FnMut(Event) -> bool, s: &str| -> Result<()> {
        if observer(Event::Message(s)) {
            Ok(())
        } else {
            Err(Error::Cancelled)
        }
    };
    for cycle in first_cycle..cfg.cycles.max(1) {
        cycles = cycle + 1;
        // ---- main grid ----
        msg(
            observer,
            &format!("refinement cycle {}/{}: main grid", cycle + 1, cfg.cycles),
        )?;
        let mut case = sim.case.clone();
        if cycle > 0 {
            if let Some(f) = case.flow.as_mut() {
                f.max_iterations = cfg.coarse_iterations;
            }
        }
        let opts = RunOptions {
            restart: coarse_state.take(),
            checkpoint: sim.options.checkpoint.clone(),
            checkpoint_every: sim.options.checkpoint_every,
            nudge: nudge.clone(),
        };
        let res = crate::flow::run(&sim.model, &case, sim.s2s.as_ref(), &opts, observer)?;
        coarse_state = res.flow.as_ref().and_then(|f| f.state.clone());
        let cstate = coarse_state
            .clone()
            .ok_or_else(|| Error::Invalid("the main-grid solve returned no state".into()))?;
        coarse_out = Some(sim.finish(res));
        if settled {
            break;
        }
        // ---- fine grids ----
        let mut nd = Nudge {
            weight: NUDGE_WEIGHT,
            ..Default::default()
        };
        let mut worst: f64 = 0.0;
        let mut prevs: Vec<Vec<(String, f64)>> = fines.iter().map(|f| f.prev.clone()).collect();
        for (gi, (fine, r)) in fines.iter_mut().zip(&cfg.regions).enumerate() {
            let (mut fsim, info) = fine_sim(sim, &cstate, fine, r, cfg.fine_iterations)?;
            fine.cells = fsim.model.ncells;
            msg(
                observer,
                &format!(
                    "refinement cycle {}/{}: region '{}' ({} cells)",
                    cycle + 1,
                    cfg.cycles,
                    fine.name,
                    fine.cells
                ),
            )?;
            if let Some(s) = fine.state.take() {
                if s.cells == fsim.model.grid.n() {
                    fsim.options.restart = Some(s);
                }
            }
            let fout = fsim.run_observed(observer)?;
            let fstate = fout
                .result
                .flow
                .as_ref()
                .and_then(|f| f.state.clone())
                .ok_or_else(|| Error::Invalid("the fine-grid solve returned no state".into()))?;
            if let Some(path) = fine_path(gi) {
                fstate.write(&path)?;
            }
            let (lo, hi) = cell_ranges(cg, info.min, info.max);
            restrict(cg, &fsim.model, &fstate, lo, hi, gi, &mut nd);
            // change of the block temperatures since the last cycle
            let now: Vec<(String, f64)> = fout
                .report
                .blocks
                .iter()
                .map(|b| (b.name.clone(), b.t_max_c))
                .collect();
            fine.change = if fine.prev.is_empty() {
                f64::INFINITY
            } else {
                now.iter()
                    .filter_map(|(n, t)| {
                        fine.prev
                            .iter()
                            .find(|p| &p.0 == n)
                            .map(|p| (t - p.1).abs())
                    })
                    .fold(0.0, f64::max)
            };
            worst = worst.max(fine.change);
            fine.prev = now;
            prevs[gi] = fine.prev.clone();
            if let Some(path) = &progress_path {
                let pr = Progress {
                    cycle,
                    prev: prevs.clone(),
                };
                // best effort: losing it only costs one extra cycle on restart
                let _ = std::fs::write(path, serde_json::to_vec(&pr).unwrap_or_default());
            }
            fine.min = info.min;
            fine.max = info.max;
            fine.state = Some(fstate);
            fine.out = Some(fout);
        }
        nudge = Some(nd);
        if worst <= cfg.tolerance_k {
            // one more main-grid solve with the settled fine solution
            settled = true;
        }
        if cycle + 1 == cfg.cycles && !settled {
            break;
        }
    }
    let mut out = coarse_out.ok_or_else(|| Error::Invalid("nothing solved".into()))?;
    merge(&sim.case, &mut out, &fines, cycles, settled);
    Ok(out)
}

/// Report the blocks, components and monitors entirely inside a region from
/// its fine grid.
fn merge(case: &Case, out: &mut Output, fines: &[Fine], cycles: usize, settled: bool) {
    let rep = &mut out.report;
    for f in fines {
        let Some(fo) = &f.out else { continue };
        let inside_box = |lo: &[f64; 3], hi: &[f64; 3]| {
            (0..3).all(|a| lo[a] >= f.min[a] - 1e-6 && hi[a] <= f.max[a] + 1e-6)
        };
        for b in &fo.report.blocks {
            let whole = case
                .blocks
                .iter()
                .find(|cb| cb.name == b.name)
                .is_some_and(|cb| inside_box(&cb.min, &cb.max));
            if !whole {
                continue;
            }
            if let Some(cb) = rep.blocks.iter_mut().find(|x| x.name == b.name) {
                *cb = b.clone();
            }
            if b.t_max_c > rep.t_max_c {
                rep.t_max_c = b.t_max_c;
            }
        }
        for c in &fo.report.components {
            if let Some(cc) = rep.components.iter_mut().find(|x| x.name == c.name) {
                *cc = c.clone();
            }
        }
        if let (Some(cf), Some(ff)) = (rep.flow.as_mut(), fo.report.flow.as_ref()) {
            for mo in &ff.monitors {
                if let Some(cm) = cf.monitors.iter_mut().find(|x| x.name == mo.name) {
                    *cm = mo.clone();
                }
            }
        }
        let ff = fo.result.flow.as_ref();
        rep.refined.push(RefinedRegion {
            name: f.name.clone(),
            min_mm: f.min,
            max_mm: f.max,
            cells: f.cells,
            cycles,
            last_change_k: f.change,
            settled,
            fine_iterations: ff.map_or(0, |x| x.iterations),
            fine_converged: ff.is_some_and(|x| x.converged),
        });
    }
    if !settled {
        rep.warnings.push(format!(
            "refinement: the fine-grid temperatures had not settled after {cycles} cycles (raise refinement.cycles)"
        ));
    }
}
