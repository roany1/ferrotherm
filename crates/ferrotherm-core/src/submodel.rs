// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sub-model ("zoom-in") of a solved case: a box cut out of the model, to be
//! solved again on a finer grid with the flow and the temperatures of the
//! coarse solution imposed on its faces.
//!
//! The coupling is one way: the sub-model does not change the coarse
//! solution. On every face cell of the coarse grid on the box:
//! - air on both sides: the coarse normal velocity is imposed, with the
//!   coarse temperature of the air coming in;
//! - a solid on the outside, or on both sides: the coarse temperature is
//!   imposed (wall);
//! - the box face with the largest outflow is left open at constant
//!   pressure instead, so that the flow imposed on the others always finds
//!   its way out (the finer grid does not see exactly the same open area).
//!
//! Faces of the box lying on a face of the model keep its boundaries.

use crate::case::{Boundary, BoundaryKind, Case, Face, Region2};
use crate::error::{Error, Result};
use crate::flow::Checkpoint;
use crate::Simulation;

const MM: f64 = 1e-3;

/// Coarse face cell on the box: (face, cell indices along the two
/// tangential axes, velocity into the box, fluid inside, fluid outside,
/// temperature inside, temperature outside).
type Patch = (Face, [usize; 2], f64, bool, bool, f64, f64);

/// What [`submodel`] did, for the user.
#[derive(Debug, Clone)]
pub struct SubmodelInfo {
    /// Box of the sub-model, mm (snapped outwards to the coarse grid).
    pub min: [f64; 3],
    pub max: [f64; 3],
    /// Air flow imposed into the box and out of it, m³/s (the rest leaves
    /// through the open face).
    pub flow_in_m3_s: f64,
    pub flow_out_m3_s: f64,
    /// Open face (largest coarse outflow).
    pub open_face: Option<Face>,
    /// Heat dissipated inside the box, W.
    pub power_w: f64,
    pub warnings: Vec<String>,
}

fn boundary(name: String, face: Face, kind: BoundaryKind, region: Option<Region2>) -> Boundary {
    Boundary {
        name,
        face,
        region,
        kind,
        t_c: None,
        h: None,
        t_ambient_c: None,
        emissivity: None,
        finish: None,
        t_radiation_c: None,
        heat_flux_w_m2: None,
        solar_w_m2: None,
        absorptivity: None,
        velocity_m_s: None,
        flow_m3_s: None,
        loss: None,
    }
}

/// Smallest box of the blocks whose name starts with one of `prefixes`, mm.
pub fn box_of_blocks(case: &Case, prefixes: &[String]) -> Result<([f64; 3], [f64; 3])> {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for b in &case.blocks {
        if prefixes.iter().any(|p| b.name.starts_with(p.as_str())) {
            for a in 0..3 {
                lo[a] = lo[a].min(b.min[a]);
                hi[a] = hi[a].max(b.max[a]);
            }
        }
    }
    if lo[0] > hi[0] {
        return Err(Error::Invalid(format!(
            "no block name starts with {}",
            prefixes.join(" or ")
        )));
    }
    Ok((lo, hi))
}

/// Cut the box [lo, hi] (mm) out of the solved case `sim` (solution `sol`).
/// The box is snapped outwards to the planes of the coarse grid. The grid
/// settings of the returned case are those of the original: set them finer
/// before solving.
pub fn submodel(
    sim: &Simulation,
    sol: &Checkpoint,
    lo: [f64; 3],
    hi: [f64; 3],
) -> Result<(Case, SubmodelInfo)> {
    let m = &sim.model;
    let g = &m.grid;
    let n = g.n();
    if sol.cells != n || sol.fields.len() < 5 {
        return Err(Error::Invalid(format!(
            "the solution file is for another grid ({} × {} × {} cells, this case has {} × {} × {})",
            sol.cells[0], sol.cells[1], sol.cells[2], n[0], n[1], n[2]
        )));
    }
    let temp = &sol.fields[4];
    // cell index range [i0, i1) of the box per axis
    let mut i0 = [0usize; 3];
    let mut i1 = [0usize; 3];
    for a in 0..3 {
        let nodes = &g.nodes[a];
        let (l, h) = (lo[a] * MM, hi[a] * MM);
        if !(h > l) {
            return Err(Error::Invalid("the box of the sub-model is empty".into()));
        }
        i0[a] = nodes.iter().rposition(|&x| x <= l + 1e-9).unwrap_or(0);
        i1[a] = nodes
            .iter()
            .position(|&x| x >= h - 1e-9)
            .unwrap_or(n[a])
            .max(i0[a] + 1);
    }
    let bmin = [0, 1, 2].map(|a| g.nodes[a][i0[a]] / MM);
    let bmax = [0, 1, 2].map(|a| g.nodes[a][i1[a]] / MM);
    let fluid: Vec<bool> = m
        .cell_material
        .iter()
        .map(|&mi| m.materials[mi as usize].is_fluid())
        .collect();
    let mut warnings = Vec::new();

    let mut case = sim.case.clone();
    case.meta.title = format!("{} — sub-model", case.meta.title);
    case.domain.min = bmin;
    case.domain.max = bmax;

    // ---- contents ----
    let inside = |p: &[f64; 3]| (0..3).all(|a| p[a] >= bmin[a] && p[a] <= bmax[a]);
    let mut power = 0.0;
    let mut blocks = Vec::new();
    for b in &sim.case.blocks {
        let mut frac = 1.0;
        let mut any = true;
        for a in 0..3 {
            let l = b.min[a].max(bmin[a]);
            let h = b.max[a].min(bmax[a]);
            let full = b.max[a] - b.min[a];
            if h < l || (h == l && full > 0.0) {
                any = false;
                break;
            }
            if full > 0.0 {
                frac *= (h - l) / full;
            }
        }
        if !any {
            continue;
        }
        let mut nb = b.clone();
        if frac < 1.0 - 1e-9 {
            // the model clips the block to the domain: keep its heat density
            nb.power_w = b.power_w * frac;
        }
        power += nb.power_w;
        blocks.push(nb);
    }
    case.blocks = blocks;
    case.components.retain(|c| inside(&c.position));
    power += case.components.iter().map(|c| c.power_w).sum::<f64>();
    case.monitors.retain(|x| inside(&x.position));
    case.heatsinks.retain(|x| inside(&x.position));
    let strictly = |p: &[f64; 3], axis: usize| {
        (0..3).all(|a| {
            if a == axis {
                p[a] > bmin[a] + 1e-6 && p[a] < bmax[a] - 1e-6
            } else {
                p[a] >= bmin[a] && p[a] <= bmax[a]
            }
        })
    };
    let before = case.fans.len() + case.resistances.len();
    case.fans.retain(|f| {
        crate::model::axis_of(f.direction.trim_start_matches(['+', '-']))
            .map(|a| strictly(&f.center, a))
            .unwrap_or(false)
    });
    case.resistances.retain(|r| {
        crate::model::axis_of(&r.axis)
            .map(|a| strictly(&r.center, a))
            .unwrap_or(false)
    });
    let _ = before;

    // ---- boundaries ----
    let fd = |a: usize| {
        let mut d = n;
        d[a] += 1;
        d
    };
    // net coarse outflow of each interior face of the box → the open one
    let mut patches: Vec<Patch> = Vec::new();
    let mut outflow = [0.0f64; 6];
    let mut interior = [false; 6];
    for (fi, &face) in Face::ALL.iter().enumerate() {
        let a = face.axis();
        let t = face.tangential();
        let plane = if face.is_max() { i1[a] } else { i0[a] };
        if plane == 0 || plane == n[a] {
            continue;
        }
        interior[fi] = true;
        let d = fd(a);
        let u = &sol.fields[a];
        for q in i0[t[1]]..i1[t[1]] {
            for p in i0[t[0]]..i1[t[0]] {
                let mut pin = [0usize; 3];
                pin[t[0]] = p;
                pin[t[1]] = q;
                let mut pout = pin;
                let mut pf = pin;
                pf[a] = plane;
                if face.is_max() {
                    pin[a] = plane - 1;
                    pout[a] = plane;
                } else {
                    pin[a] = plane;
                    pout[a] = plane - 1;
                }
                let cin = g.idx(pin[0], pin[1], pin[2]);
                let cout = g.idx(pout[0], pout[1], pout[2]);
                let area = g.width(t[0], p) * g.width(t[1], q);
                let v = u[pf[0] + d[0] * (pf[1] + d[1] * pf[2])];
                // velocity into the box
                let vin = if face.is_max() { -v } else { v };
                if fluid[cin] && fluid[cout] {
                    outflow[fi] -= vin * area;
                }
                patches.push((
                    face,
                    [p, q],
                    vin,
                    fluid[cin],
                    fluid[cout],
                    temp[cin],
                    temp[cout],
                ));
            }
        }
    }
    let open = (0..6)
        .filter(|&f| interior[f] && outflow[f] > 0.0)
        .max_by(|&x, &y| outflow[x].total_cmp(&outflow[y]));
    let mut bnds = Vec::new();
    // boundaries of the model on the faces the box shares with it
    for (fi, &face) in Face::ALL.iter().enumerate() {
        if interior[fi] {
            continue;
        }
        let t = face.tangential();
        for b in sim.case.boundaries.iter().filter(|b| b.face == face) {
            let mut nb = b.clone();
            if let Some(r) = &b.region {
                let rmin = [r.min[0].max(bmin[t[0]]), r.min[1].max(bmin[t[1]])];
                let rmax = [r.max[0].min(bmax[t[0]]), r.max[1].min(bmax[t[1]])];
                if rmin[0] >= rmax[0] || rmin[1] >= rmax[1] {
                    continue;
                }
                if b.flow_m3_s.is_some() && (rmin != r.min || rmax != r.max) {
                    warnings.push(format!(
                        "inlet '{}' is cut by the box: its volume flow is kept whole",
                        b.name
                    ));
                }
                nb.region = Some(Region2 {
                    min: rmin,
                    max: rmax,
                });
            }
            bnds.push(nb);
        }
    }
    let (mut qin, mut qout) = (0.0, 0.0);
    for (face, pq, vin, fin, fout, tin, tout) in patches {
        let fi = Face::ALL.iter().position(|x| *x == face).unwrap();
        let t = face.tangential();
        let region = Some(Region2 {
            min: [g.nodes[t[0]][pq[0]] / MM, g.nodes[t[1]][pq[1]] / MM],
            max: [g.nodes[t[0]][pq[0] + 1] / MM, g.nodes[t[1]][pq[1] + 1] / MM],
        });
        let name = format!("{} {},{}", face.name(), pq[0], pq[1]);
        let area = g.width(t[0], pq[0]) * g.width(t[1], pq[1]);
        match (fin, fout) {
            (true, true) => {
                if Some(fi) == open {
                    let mut b = boundary(name, face, BoundaryKind::Opening, region);
                    b.t_ambient_c = Some(tout);
                    bnds.push(b);
                } else {
                    let mut b = boundary(name, face, BoundaryKind::Inlet, region);
                    b.velocity_m_s = Some(vin);
                    b.t_ambient_c = Some(tout);
                    if vin > 0.0 {
                        qin += vin * area;
                    } else {
                        qout -= vin * area;
                    }
                    bnds.push(b);
                }
            }
            // wall at the coarse temperature of the solid outside (or of
            // the cut solid)
            (true, false) => {
                let mut b = boundary(name, face, BoundaryKind::Fixed, region);
                b.t_c = Some(tout);
                bnds.push(b);
            }
            (false, false) => {
                let mut b = boundary(name, face, BoundaryKind::Fixed, region);
                b.t_c = Some(0.5 * (tin + tout));
                bnds.push(b);
            }
            // solid surface wetted by air outside the box: left adiabatic
            (false, true) => {}
        }
    }
    if open.is_none() && qin > 0.0 {
        warnings.push("no face of the box has a net outflow: nothing was left open".into());
    }
    if case.fans.len() + case.resistances.len() < before {
        warnings.push(
            "fans or resistances outside the box (or on its faces) were left out: their effect is in the imposed flow"
                .into(),
        );
    }
    case.boundaries = bnds;
    if let Some(f) = case.flow.as_mut() {
        if f.reference_temperature_c.is_none() {
            f.reference_temperature_c = m.ambient_c.or_else(|| {
                sim.case
                    .boundaries
                    .iter()
                    .filter(|b| matches!(b.kind, BoundaryKind::Opening | BoundaryKind::Inlet))
                    .find_map(|b| b.t_ambient_c)
            });
        }
    }
    Ok((
        case,
        SubmodelInfo {
            min: bmin,
            max: bmax,
            flow_in_m3_s: qin,
            flow_out_m3_s: qout,
            open_face: open.map(|f| Face::ALL[f]),
            power_w: power,
            warnings,
        },
    ))
}
