// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Conjugate heat transfer with incompressible flow.
//!
//! * Finite volumes on the conforming Cartesian grid, velocities on a
//!   staggered (MAC) arrangement, pressure and temperature at cell centres.
//! * SIMPLEC pressure–velocity coupling (Van Doormaal & Raithby 1984),
//!   Patankar power-law (or upwind) convection, Boussinesq buoyancy.
//! * Openings: static ambient pressure with Bernoulli inflow
//!   (p_amb − p = ½ρ(1+ζ)u² in, p − p_amb = ½ρζu² out).
//! * Fans: planar momentum source from the P–Q curve, linearised around the
//!   current operating point; or a fixed volume flow.
//! * Planar resistances: Δp = A·u + B·u² (see [`crate::case::Loss`]).
//! * Turbulence: laminar or LVEL (wall distance from ∇²φ = −1,
//!   L = √(|∇φ|² + 2φ) − |∇φ|; ν_t from Spalding's law of the wall,
//!   κ = 0.41, E = 8.6).
//! * Energy: conduction everywhere + advection in the fluid, k_eff = k + ρ c_p ν_t / Pr_t.

use crate::case::{BoundaryKind, Case, Face, FlowSettings, Mode, Scheme, Turbulence};
use crate::error::{Error, Result};
use crate::library::interp;
use crate::model::{axis_of, conductances, direction_of, fan_extent, tangential, Model};
use crate::radiation::S2s;
use crate::simulate::{element_terms, heat_flow, surface_temps, Event, RunResult, Snapshot};
use crate::stencil::{bicgstab, bicgstab_with, pcg_with, Pre, Stencil};
use serde::Serialize;

const MM: f64 = 1e-3;
const PR_T: f64 = 0.9;
const KAPPA: f64 = 0.41;
const E_WALL: f64 = 8.6;

const WALL: u8 = 0;
const ACTIVE: u8 = 1;
const OPENING: u8 = 2;
const FIXED: u8 = 3;

#[derive(Debug, Clone, Serialize)]
pub struct FanResult {
    pub name: String,
    /// Volume flow in the fan direction, m³/s.
    pub flow_m3_s: f64,
    /// Static pressure rise at the operating point, Pa.
    pub pressure_pa: f64,
    pub area_m2: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpeningResult {
    pub name: String,
    pub inflow_m3_s: f64,
    pub outflow_m3_s: f64,
    /// Heat carried out by the air relative to the ambient, W.
    pub heat_out_w: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorResult {
    pub name: String,
    pub position_mm: [f64; 3],
    pub t_c: f64,
    pub speed_m_s: f64,
    /// Temperature per outer iteration (steady) or time step (transient).
    pub history: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowResult {
    pub iterations: usize,
    pub converged: bool,
    /// Stopped because junction/monitor temperatures stopped changing
    /// (range over 50 iterations below 0.05 K or 0.2 % of the rise above
    /// ambient) with a low continuity residual.
    pub converged_by_monitors: bool,
    /// Per iteration: [mass, momentum, energy] normalised residuals.
    pub residuals: Vec<[f64; 3]>,
    pub fans: Vec<FanResult>,
    pub openings: Vec<OpeningResult>,
    pub monitors: Vec<MonitorResult>,
    pub max_speed_m_s: f64,
    pub fluid: String,
    pub turbulence: String,
    /// Not converged: the flow fields were averaged over this many final
    /// iterations (the steady iteration oscillates, typically because the
    /// flow is physically unsteady). 0 when converged.
    pub averaged_iterations: usize,
    /// Largest peak-to-peak variation of a key temperature (junctions,
    /// monitors, hottest cell) over the last 50 iterations, K.
    pub oscillation_k: f64,
}

#[inline]
fn fdims(n: [usize; 3], a: usize) -> [usize; 3] {
    let mut d = n;
    d[a] += 1;
    d
}
#[inline]
fn fidx(d: [usize; 3], p: [usize; 3]) -> usize {
    p[0] + d[0] * (p[1] + d[1] * p[2])
}
#[inline]
fn fijk(d: [usize; 3], f: usize) -> [usize; 3] {
    [f % d[0], (f / d[0]) % d[1], f / (d[0] * d[1])]
}
#[inline]
fn plaw(scheme: Scheme, pe: f64) -> f64 {
    match scheme {
        Scheme::Upwind => 1.0,
        Scheme::PowerLaw => {
            let t = 1.0 - 0.1 * pe.abs();
            if t > 0.0 {
                t.powi(5)
            } else {
                0.0
            }
        }
    }
}

/// Coefficient of a control-volume face (diffusion D, outward mass/heat flux F).
#[inline]
fn anb(scheme: Scheme, d: f64, f_out: f64) -> f64 {
    let diff = if d > 0.0 {
        d * plaw(scheme, f_out / d)
    } else {
        0.0
    };
    diff + (-f_out).max(0.0)
}

struct FanDef {
    name: String,
    axis: usize,
    dir: f64,
    faces: Vec<usize>,
    area: f64,
    boundary: bool,
    curve: Vec<[f64; 2]>,
    fixed: Option<f64>,
    /// Linearisation (P0, slope, Q0).
    lin: (f64, f64, f64),
}

impl FanDef {
    fn pressure(&self, q: f64) -> f64 {
        let c = &self.curve;
        if c.is_empty() {
            return 0.0;
        }
        if q <= c[0][0] {
            return c[0][1];
        }
        let last = c.len() - 1;
        if q >= c[last][0] && last > 0 {
            // extrapolate past free delivery: the fan becomes a resistance
            let s = (c[last][1] - c[last - 1][1]) / (c[last][0] - c[last - 1][0]);
            return c[last][1] + s * (q - c[last][0]);
        }
        interp(c, q)
    }
    fn slope(&self, q: f64) -> f64 {
        let h = 1e-6 + 1e-3 * self.curve.last().map(|r| r[0].abs()).unwrap_or(1.0);
        ((self.pressure(q + h) - self.pressure(q - h)) / (2.0 * h)).min(0.0)
    }
}

struct Props {
    rho: f64,
    mu: f64,
    cp: f64,
    beta: f64,
}

/// State of the flow solution.
struct State {
    u: [Vec<f64>; 3],
    p: Vec<f64>,
    t: Vec<f64>,
    ts: Vec<f64>,
    nut: Vec<f64>,
}

struct Solver<'a> {
    m: &'a Model,
    cfg: FlowSettings,
    scheme: Scheme,
    props: Props,
    n: [usize; 3],
    fd: [[usize; 3]; 3],
    fluid: Vec<bool>,
    kind: [Vec<u8>; 3],
    /// Fixed velocities (FIXED faces) and current opening velocities.
    fixed_u: [Vec<f64>; 3],
    /// Case boundary of boundary faces (−1 none).
    face_bnd: [Vec<i32>; 3],
    /// Fan index per face (−1 none).
    face_fan: [Vec<i32>; 3],
    /// Planar resistance (linear, quadratic) per face.
    res_lin: [Vec<f64>; 3],
    res_quad: [Vec<f64>; 3],
    fans: Vec<FanDef>,
    /// Inflow temperature per case boundary.
    t_in: Vec<f64>,
    /// Opening loss (linear, quadratic) per case boundary.
    open_loss: Vec<(f64, f64)>,
    t_ref: f64,
    t_amb: f64,
    /// Opening resistance coefficient per boundary face, d = 1/R.
    d_open: [Vec<f64>; 3],
    wall_dist: Vec<f64>,
    pins: Vec<usize>,
    s2s: Option<&'a S2s>,
    monitors: Vec<(String, [f64; 3], usize)>,
}

impl<'a> Solver<'a> {
    fn new(m: &'a Model, case: &Case, s2s: Option<&'a S2s>) -> Result<Solver<'a>> {
        let cfg = case.flow.clone().unwrap_or_default();
        let g = &m.grid;
        let n = g.n();
        let nc = m.ncells;
        // ---- fluid ----
        let mut fluid = vec![false; nc];
        let mut fluid_mat: Option<u32> = None;
        for c in 0..nc {
            let mi = m.cell_material[c];
            if m.materials[mi as usize].is_fluid() {
                match fluid_mat {
                    None => fluid_mat = Some(mi),
                    Some(f) if f != mi => {
                        return Err(Error::Invalid(format!(
                            "only one flowing fluid per model is supported ('{}' and '{}')",
                            m.materials[f as usize].id, m.materials[mi as usize].id
                        )))
                    }
                    _ => {}
                }
                fluid[c] = true;
            }
        }
        let Some(fm) = fluid_mat else {
            return Err(Error::Invalid(
                "flow is enabled but no material flows: use a fluid with a viscosity (e.g. `air` instead of `air-still`)".into(),
            ));
        };
        let mat = &m.materials[fm as usize];
        let props = Props {
            rho: mat.density,
            mu: mat.viscosity.unwrap_or(1.8e-5),
            cp: mat.cp,
            beta: mat.beta.unwrap_or(0.0),
        };
        // ambient: environment preset, else the air temperature of the first
        // opening/inlet, else the initial temperature
        let t_amb = m
            .ambient_c
            .or_else(|| {
                case.boundaries
                    .iter()
                    .filter(|b| matches!(b.kind, BoundaryKind::Opening | BoundaryKind::Inlet))
                    .find_map(|b| b.t_ambient_c)
            })
            .unwrap_or(m.initial_c);
        let t_ref = cfg.reference_temperature_c.unwrap_or(t_amb);
        let fd = [fdims(n, 0), fdims(n, 1), fdims(n, 2)];

        // ---- inflow temperatures and losses per boundary ----
        let t_in: Vec<f64> = case
            .boundaries
            .iter()
            .map(|b| b.t_ambient_c.unwrap_or(t_amb))
            .collect();
        let open_loss: Vec<(f64, f64)> = case
            .boundaries
            .iter()
            .map(|b| {
                b.loss
                    .as_ref()
                    .map(|l| l.coefficients(props.rho, props.mu))
                    .unwrap_or((0.0, 0.0))
            })
            .collect();

        // ---- face kinds ----
        let mut kind: [Vec<u8>; 3] = Default::default();
        let mut fixed_u: [Vec<f64>; 3] = Default::default();
        let mut face_bnd: [Vec<i32>; 3] = Default::default();
        for a in 0..3 {
            let nf = fd[a][0] * fd[a][1] * fd[a][2];
            let mut k = vec![WALL; nf];
            let mut fb = vec![-1i32; nf];
            for f in 0..nf {
                let p = fijk(fd[a], f);
                let i = p[a];
                if i > 0 && i < n[a] {
                    let mut pl = p;
                    pl[a] = i - 1;
                    if fluid[g.idx(pl[0], pl[1], pl[2])] && fluid[g.idx(p[0], p[1], p[2])] {
                        k[f] = ACTIVE;
                    }
                } else {
                    let mut pc = p;
                    let face;
                    if i == 0 {
                        face = [Face::Xmin, Face::Ymin, Face::Zmin][a];
                    } else {
                        pc[a] = n[a] - 1;
                        face = [Face::Xmax, Face::Ymax, Face::Zmax][a];
                    }
                    let c = g.idx(pc[0], pc[1], pc[2]);
                    if let Some(bi) = m.bc_at(face, pc) {
                        fb[f] = bi as i32;
                        if fluid[c] {
                            match case.boundaries[bi].kind {
                                BoundaryKind::Opening => k[f] = OPENING,
                                BoundaryKind::Inlet => k[f] = FIXED,
                                _ => {}
                            }
                        }
                    }
                }
            }
            kind[a] = k;
            fixed_u[a] = vec![0.0; nf];
            face_bnd[a] = fb;
        }
        // inlet velocities
        for (bi, b) in case.boundaries.iter().enumerate() {
            if b.kind != BoundaryKind::Inlet {
                continue;
            }
            let a = b.face.axis();
            let sign = if b.face.is_max() { -1.0 } else { 1.0 };
            let faces: Vec<usize> = (0..kind[a].len())
                .filter(|&f| kind[a][f] == FIXED && face_bnd[a][f] == bi as i32)
                .collect();
            let area: f64 = faces.iter().map(|&f| face_area(m, fd[a], a, f)).sum();
            if faces.is_empty() {
                continue;
            }
            let v = match (b.velocity_m_s, b.flow_m3_s) {
                (Some(v), _) => v,
                (None, Some(q)) => q / area,
                _ => 0.0,
            };
            for f in faces {
                fixed_u[a][f] = sign * v;
            }
        }

        // ---- fans ----
        let mut face_fan: [Vec<i32>; 3] = [
            vec![-1; kind[0].len()],
            vec![-1; kind[1].len()],
            vec![-1; kind[2].len()],
        ];
        let mut fans = Vec::new();
        for f in &case.fans {
            let (a, dir) = direction_of(&f.direction)?;
            let size = fan_extent(f)?;
            let i = g.plane_index(a, f.center[a] * MM);
            if (g.nodes[a][i] - f.center[a] * MM).abs() > 1e-6 {
                return Err(Error::Invalid(format!(
                    "fan '{}' is outside the domain",
                    f.name
                )));
            }
            let t = tangential(a);
            let mut faces = Vec::new();
            let mut area = 0.0;
            for v in 0..n[t[1]] {
                for u in 0..n[t[0]] {
                    let cu = g.center(t[0], u) / MM - f.center[t[0]];
                    let cv = g.center(t[1], v) / MM - f.center[t[1]];
                    let inside = match f.diameter {
                        Some(dm) if f.size.is_none() => {
                            let r2 = cu * cu + cv * cv;
                            let hub = f.hub_diameter.unwrap_or(0.0);
                            r2 < 0.25 * dm * dm && r2 >= 0.25 * hub * hub
                        }
                        _ => cu.abs() < 0.5 * size[0] && cv.abs() < 0.5 * size[1],
                    };
                    if !inside {
                        continue;
                    }
                    let mut p = [0usize; 3];
                    p[a] = i;
                    p[t[0]] = u;
                    p[t[1]] = v;
                    let fi = fidx(fd[a], p);
                    let boundary = i == 0 || i == n[a];
                    if boundary {
                        // a fan in the domain wall is an opening with a pressure rise
                        let mut pc = p;
                        if i == n[a] {
                            pc[a] = n[a] - 1;
                        }
                        if !fluid[g.idx(pc[0], pc[1], pc[2])] {
                            continue;
                        }
                        kind[a][fi] = OPENING;
                    } else if kind[a][fi] != ACTIVE {
                        continue;
                    }
                    face_fan[a][fi] = fans.len() as i32;
                    area += face_area(m, fd[a], a, fi);
                    faces.push(fi);
                }
            }
            if faces.is_empty() {
                return Err(Error::Invalid(format!(
                    "fan '{}' has no free flow faces (blocked by solids or outside the grid)",
                    f.name
                )));
            }
            if f.curve.is_empty() && f.flow_m3_s.is_none() {
                return Err(Error::Invalid(format!(
                    "fan '{}': give a `curve` or `flow_m3_s`",
                    f.name
                )));
            }
            if f.curve.windows(2).any(|w| w[1][0] <= w[0][0]) {
                return Err(Error::Invalid(format!(
                    "fan '{}': curve flows must increase",
                    f.name
                )));
            }
            let boundary = i == 0 || i == n[a];
            if let Some(q) = f.flow_m3_s {
                for &fi in &faces {
                    kind[a][fi] = FIXED;
                    fixed_u[a][fi] = dir * q / area;
                }
            }
            fans.push(FanDef {
                name: f.name.clone(),
                axis: a,
                dir,
                faces,
                area,
                boundary,
                curve: f.curve.clone(),
                fixed: f.flow_m3_s,
                lin: (f.curve.first().map(|r| r[1]).unwrap_or(0.0), 0.0, 0.0),
            });
        }

        // ---- planar resistances ----
        let mut res_lin: [Vec<f64>; 3] = [
            vec![0.0; kind[0].len()],
            vec![0.0; kind[1].len()],
            vec![0.0; kind[2].len()],
        ];
        let mut res_quad = res_lin.clone();
        for r in &case.resistances {
            let a = axis_of(&r.axis)?;
            let i = g.plane_index(a, r.center[a] * MM);
            let t = tangential(a);
            let (lin, quad) = r.loss.coefficients(props.rho, props.mu);
            let mut count = 0;
            for v in 0..n[t[1]] {
                for u in 0..n[t[0]] {
                    let cu = g.center(t[0], u) / MM - r.center[t[0]];
                    let cv = g.center(t[1], v) / MM - r.center[t[1]];
                    if cu.abs() < 0.5 * r.size[0] && cv.abs() < 0.5 * r.size[1] {
                        let mut p = [0usize; 3];
                        p[a] = i;
                        p[t[0]] = u;
                        p[t[1]] = v;
                        let fi = fidx(fd[a], p);
                        if kind[a][fi] == ACTIVE {
                            res_lin[a][fi] += lin;
                            res_quad[a][fi] += quad;
                            count += 1;
                        }
                    }
                }
            }
            if count == 0 {
                return Err(Error::Invalid(format!(
                    "resistance '{}' has no free flow faces (must lie inside the air, not on the domain boundary)",
                    r.name
                )));
            }
        }

        let monitors = case
            .monitors
            .iter()
            .map(|mo| {
                let mut p = [0usize; 3];
                for a in 0..3 {
                    let x = mo.position[a] * MM;
                    let v = &g.nodes[a];
                    p[a] = (0..n[a]).find(|&i| x >= v[i] && x <= v[i + 1]).unwrap_or(0);
                }
                (mo.name.clone(), mo.position, g.idx(p[0], p[1], p[2]))
            })
            .collect();

        let mut s = Solver {
            m,
            scheme: cfg.scheme,
            cfg,
            props,
            n,
            fd,
            fluid,
            kind,
            fixed_u,
            face_bnd,
            face_fan,
            res_lin,
            res_quad,
            fans,
            t_in,
            open_loss,
            t_ref,
            t_amb,
            d_open: Default::default(),
            wall_dist: vec![0.0; nc],
            pins: vec![],
            s2s,
            monitors,
        };
        s.d_open = [
            vec![0.0; s.kind[0].len()],
            vec![0.0; s.kind[1].len()],
            vec![0.0; s.kind[2].len()],
        ];
        s.find_pins()?;
        if s.cfg.turbulence == Turbulence::Lvel {
            s.wall_distance()?;
        }
        Ok(s)
    }

    #[inline]
    fn cell(&self, p: [usize; 3]) -> usize {
        self.m.grid.idx(p[0], p[1], p[2])
    }

    /// Domain boundary kind of the boundary face (axis a) in front of cell p on side `hi`.
    fn wall_kind(&self, a: usize, p: [usize; 3], hi: bool) -> Option<BoundaryKind> {
        let face = match (a, hi) {
            (0, false) => Face::Xmin,
            (0, true) => Face::Xmax,
            (1, false) => Face::Ymin,
            (1, true) => Face::Ymax,
            (_, false) => Face::Zmin,
            (_, true) => Face::Zmax,
        };
        self.m.bc_at(face, p).map(|bi| self.m.boundary_kinds[bi])
    }

    /// Pressure reference cells for fluid regions without openings.
    fn find_pins(&mut self) -> Result<()> {
        let n = self.n;
        let nc = self.m.ncells;
        let s = [1usize, n[0], n[0] * n[1]];
        let mut comp = vec![u32::MAX; nc];
        let mut ncomp = 0u32;
        let mut stack = Vec::new();
        for start in 0..nc {
            if !self.fluid[start] || comp[start] != u32::MAX {
                continue;
            }
            comp[start] = ncomp;
            stack.push(start);
            let mut open = false;
            let mut fixed_in = 0.0f64;
            while let Some(c) = stack.pop() {
                let p = self.m.grid.ijk(c);
                for a in 0..3 {
                    let mut ph = p;
                    ph[a] += 1;
                    let fl = fidx(self.fd[a], p);
                    let fh = fidx(self.fd[a], ph);
                    for (f, nb) in [(fl, c.checked_sub(s[a])), (fh, Some(c + s[a]))] {
                        match self.kind[a][f] {
                            ACTIVE => {
                                let nb = nb.unwrap();
                                if comp[nb] == u32::MAX {
                                    comp[nb] = ncomp;
                                    stack.push(nb);
                                }
                            }
                            OPENING => open = true,
                            FIXED => {
                                let area = face_area(self.m, self.fd[a], a, f);
                                let sign = if f == fl { 1.0 } else { -1.0 };
                                let pf = fijk(self.fd[a], f);
                                if pf[a] == 0 || pf[a] == n[a] {
                                    fixed_in += sign * self.fixed_u[a][f] * area;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            if !open {
                if fixed_in.abs() > 1e-12 {
                    return Err(Error::Invalid(
                        "an inlet feeds a closed air region with no opening: add an opening for the air to leave".into(),
                    ));
                }
                self.pins.push(start);
            }
            ncomp += 1;
        }
        Ok(())
    }

    /// Wall distance (LVEL): ∇²φ = −1, φ = 0 on walls.
    fn wall_distance(&mut self) -> Result<()> {
        let g = &self.m.grid;
        let n = self.n;
        let nc = self.m.ncells;
        let st = [1usize, n[0], n[0] * n[1]];
        let mut mat = Stencil::new(n, 0);
        let mut b = vec![0.0; nc];
        for c in 0..nc {
            if !self.fluid[c] {
                mat.fix(c);
                continue;
            }
            let p = g.ijk(c);
            b[c] = g.volume(c);
            for a in 0..3 {
                let area = g.face_area(c, a);
                let w = g.width(a, p[a]);
                // −a side
                if p[a] > 0 {
                    let nb = c - st[a];
                    if self.fluid[nb] {
                        let dd = area / (0.5 * (w + g.width(a, p[a] - 1)));
                        mat.neg[a][c] = dd;
                        mat.ap[c] += dd;
                    } else {
                        mat.ap[c] += area / (0.5 * w);
                    }
                } else if !matches!(
                    self.wall_kind(a, p, false),
                    Some(BoundaryKind::Opening | BoundaryKind::Inlet | BoundaryKind::Symmetry)
                ) {
                    mat.ap[c] += area / (0.5 * w);
                }
                // +a side
                if p[a] + 1 < n[a] {
                    let nb = c + st[a];
                    if self.fluid[nb] {
                        let dd = area / (0.5 * (w + g.width(a, p[a] + 1)));
                        mat.pos[a][c] = dd;
                        mat.ap[c] += dd;
                    } else {
                        mat.ap[c] += area / (0.5 * w);
                    }
                } else if !matches!(
                    self.wall_kind(a, p, true),
                    Some(BoundaryKind::Opening | BoundaryKind::Inlet | BoundaryKind::Symmetry)
                ) {
                    mat.ap[c] += area / (0.5 * w);
                }
            }
        }
        // fluid cells with no wall at all (fully open/symmetric): keep finite
        for c in 0..nc {
            if self.fluid[c] && mat.ap[c] == 0.0 {
                mat.fix(c);
                b[c] = 0.0;
            }
        }
        let mut phi = vec![0.0; nc];
        let stats = pcg_with(&mat, &b, &mut phi, 1e-8, 0.0, 5000, Pre::Mg)?;
        if !stats.converged && stats.reduction > 1e-3 {
            return Err(Error::Invalid(
                "wall-distance equation did not converge".into(),
            ));
        }
        // L = sqrt(|∇φ|² + 2φ) − |∇φ|
        for c in 0..nc {
            if !self.fluid[c] {
                continue;
            }
            let p = g.ijk(c);
            let mut g2 = 0.0;
            for a in 0..3 {
                let w = g.width(a, p[a]);
                let (xm, fm) = if p[a] > 0 && self.fluid[c - st[a]] {
                    (g.center(a, p[a] - 1), phi[c - st[a]])
                } else {
                    (g.center(a, p[a]) - 0.5 * w, 0.0)
                };
                let (xp, fp) = if p[a] + 1 < n[a] && self.fluid[c + st[a]] {
                    (g.center(a, p[a] + 1), phi[c + st[a]])
                } else {
                    (g.center(a, p[a]) + 0.5 * w, 0.0)
                };
                let open_lo = p[a] == 0
                    && matches!(
                        self.wall_kind(a, p, false),
                        Some(BoundaryKind::Opening | BoundaryKind::Inlet | BoundaryKind::Symmetry)
                    );
                let open_hi = p[a] + 1 == n[a]
                    && matches!(
                        self.wall_kind(a, p, true),
                        Some(BoundaryKind::Opening | BoundaryKind::Inlet | BoundaryKind::Symmetry)
                    );
                let grad = if open_lo && open_hi {
                    0.0
                } else if open_lo {
                    (fp - phi[c]) / (xp - g.center(a, p[a]))
                } else if open_hi {
                    (phi[c] - fm) / (g.center(a, p[a]) - xm)
                } else {
                    (fp - fm) / (xp - xm)
                };
                g2 += grad * grad;
            }
            let gr = g2.sqrt();
            self.wall_dist[c] = ((g2 + 2.0 * phi[c].max(0.0)).sqrt() - gr).max(0.0);
        }
        Ok(())
    }

    /// Cell-centred velocity.
    fn cell_velocity(&self, st: &State, c: usize) -> [f64; 3] {
        let p = self.m.grid.ijk(c);
        let mut v = [0.0; 3];
        for a in 0..3 {
            let mut ph = p;
            ph[a] += 1;
            v[a] = 0.5 * (st.u[a][fidx(self.fd[a], p)] + st.u[a][fidx(self.fd[a], ph)]);
        }
        v
    }

    /// LVEL turbulent viscosity from Spalding's law of the wall.
    fn update_nut(&self, st: &mut State) {
        let nu = self.props.mu / self.props.rho;
        for c in 0..self.m.ncells {
            if !self.fluid[c] {
                continue;
            }
            let v = self.cell_velocity(st, c);
            let speed = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            let re = speed * self.wall_dist[c] / nu;
            let ratio = spalding_nut_ratio(re);
            st.nut[c] = 0.5 * st.nut[c] + 0.5 * nu * ratio;
        }
    }

    fn mu_eff(&self, st: &State, c: usize) -> f64 {
        self.props.mu + self.props.rho * st.nut[c]
    }

    /// Update the linearisation of every curve fan from the current flow.
    fn update_fans(&mut self, st: &State) {
        for fan in self.fans.iter_mut() {
            if fan.fixed.is_some() {
                continue;
            }
            let a = fan.axis;
            let q: f64 = fan
                .faces
                .iter()
                .map(|&f| fan.dir * st.u[a][f] * face_area(self.m, self.fd[a], a, f))
                .sum();
            let p0 = fan.pressure(q);
            let s = fan.slope(q);
            fan.lin = (p0, s, q);
        }
    }

    /// Momentum equation for axis `a`: returns (matrix, rhs, d coefficients).
    fn momentum(
        &self,
        st: &State,
        a: usize,
        prev_u: Option<(&[f64], f64)>,
    ) -> (Stencil, Vec<f64>, Vec<f64>) {
        let g = &self.m.grid;
        let n = self.n;
        let fd = self.fd[a];
        let fs = [1usize, fd[0], fd[0] * fd[1]];
        let nf = fd[0] * fd[1] * fd[2];
        let mut mat = Stencil::new(fd, 0);
        let mut b = vec![0.0; nf];
        let mut d = vec![0.0; nf];
        let rho = self.props.rho;
        let alpha = self.cfg.relax_velocity.clamp(0.05, 1.0);
        let grav = self.cfg.gravity[a];
        let u = &st.u;
        for f in 0..nf {
            if self.kind[a][f] != ACTIVE {
                mat.fix(f);
                b[f] = u[a][f];
                continue;
            }
            let p = fijk(fd, f);
            let i = p[a];
            let mut pl = p;
            pl[a] = i - 1;
            let cl = self.cell(pl);
            let ch = self.cell(p);
            let wl = g.width(a, i - 1);
            let wh = g.width(a, i);
            let la = 0.5 * (wl + wh);
            let t = tangential(a);
            let area = g.width(t[0], p[t[0]]) * g.width(t[1], p[t[1]]);
            let vol = la * area;
            let mut sum_anb = 0.0;
            let mut df = 0.0;
            let mut ap_extra = 0.0;
            let mut rhs = 0.0;

            // along a: east (cell h centre) and west (cell l centre)
            {
                let fe = f + fs[a];
                let fw = f - fs[a];
                let f_e = rho * 0.5 * (u[a][f] + u[a][fe]) * area;
                let d_e = self.mu_eff(st, ch) * area / wh;
                let c_e = anb(self.scheme, d_e, f_e);
                sum_anb += c_e;
                df += f_e;
                if self.kind[a][fe] == ACTIVE {
                    mat.pos[a][f] = c_e;
                } else {
                    rhs += c_e * u[a][fe];
                }
                let f_w = rho * 0.5 * (u[a][fw] + u[a][f]) * area;
                let d_w = self.mu_eff(st, cl) * area / wl;
                let c_w = anb(self.scheme, d_w, -f_w);
                sum_anb += c_w;
                df -= f_w;
                if self.kind[a][fw] == ACTIVE {
                    mat.neg[a][f] = c_w;
                } else {
                    rhs += c_w * u[a][fw];
                }
            }
            // tangential directions
            for (q, &bx) in t.iter().enumerate() {
                let cx = t[1 - q];
                let fdb = self.fd[bx];
                let wb = g.width(bx, p[bx]);
                let wc = g.width(cx, p[cx]);
                let an = la * wc;
                for hi in [false, true] {
                    // convecting velocity through this CV face (component bx)
                    let mut plb = pl;
                    let mut phb = p;
                    if hi {
                        plb[bx] += 1;
                        phb[bx] += 1;
                    }
                    let vl = st.u[bx][fidx(fdb, plb)];
                    let vh = st.u[bx][fidx(fdb, phb)];
                    let flux = rho * (vl * 0.5 * wl + vh * 0.5 * wh) * wc;
                    let f_out = if hi { flux } else { -flux };
                    let at_edge = if hi { p[bx] + 1 == n[bx] } else { p[bx] == 0 };
                    if at_edge {
                        let kl = self.wall_kind(bx, pl, hi);
                        let kh = self.wall_kind(bx, p, hi);
                        let open = |k: Option<BoundaryKind>| {
                            matches!(k, Some(BoundaryKind::Opening | BoundaryKind::Inlet))
                        };
                        let sym =
                            |k: Option<BoundaryKind>| matches!(k, Some(BoundaryKind::Symmetry));
                        if open(kl) || open(kh) || sym(kl) || sym(kh) {
                            // no shear; momentum leaves with the outflow, inflow carries none
                            let c_n = anb(self.scheme, 0.0, f_out);
                            sum_anb += c_n;
                            df += f_out;
                        } else {
                            let dw = self.props.mu * an / (0.5 * wb);
                            sum_anb += dw;
                        }
                        continue;
                    }
                    let mut pn = p;
                    if hi {
                        pn[bx] += 1;
                    } else {
                        pn[bx] -= 1;
                    }
                    let fnb = fidx(fd, pn);
                    let mut pln = pl;
                    pln[bx] = pn[bx];
                    let cln = self.cell(pln);
                    let chn = self.cell(pn);
                    match self.kind[a][fnb] {
                        ACTIVE | FIXED => {
                            let delta = 0.5 * (wb + g.width(bx, pn[bx]));
                            let mu_e = 0.25
                                * (self.mu_eff(st, cl)
                                    + self.mu_eff(st, ch)
                                    + self.mu_eff(st, cln)
                                    + self.mu_eff(st, chn));
                            let c_n = anb(self.scheme, mu_e * an / delta, f_out);
                            sum_anb += c_n;
                            df += f_out;
                            if self.kind[a][fnb] == ACTIVE {
                                if hi {
                                    mat.pos[bx][f] = c_n;
                                } else {
                                    mat.neg[bx][f] = c_n;
                                }
                            } else {
                                rhs += c_n * u[a][fnb];
                            }
                        }
                        _ => {
                            if !self.fluid[cln] && !self.fluid[chn] {
                                // solid wall on the plane between the rows
                                sum_anb += self.props.mu * an / (0.5 * wb);
                            } else {
                                // face on the side of a solid: u = 0 at the neighbour node
                                let delta = 0.5 * (wb + g.width(bx, pn[bx]));
                                let c_n = anb(self.scheme, self.props.mu * an / delta, f_out);
                                sum_anb += c_n;
                                df += f_out;
                            }
                        }
                    }
                }
            }
            // pressure gradient
            rhs += (st.p[cl] - st.p[ch]) * area;
            // Boussinesq buoyancy
            if grav != 0.0 && self.props.beta != 0.0 {
                let tf = (st.t[cl] * wh + st.t[ch] * wl) / (wl + wh);
                rhs += -rho * self.props.beta * (tf - self.t_ref) * grav * vol;
            }
            // planar resistance
            if self.res_lin[a][f] != 0.0 || self.res_quad[a][f] != 0.0 {
                ap_extra += (self.res_lin[a][f] + self.res_quad[a][f] * u[a][f].abs()) * area;
            }
            // fan (curve)
            let fi = self.face_fan[a][f];
            if fi >= 0 {
                let fan = &self.fans[fi as usize];
                let (p0, s, q0) = fan.lin;
                rhs += fan.dir * area * (p0 - s * q0);
                ap_extra += -s * area * fan.area;
            }
            // transient
            if let Some((uold, dt)) = prev_u {
                let c = rho * vol / dt;
                ap_extra += c;
                rhs += c * uold[f];
            }
            let ap = sum_anb + df.max(0.0) + ap_extra;
            let ap_r = ap / alpha;
            rhs += (ap_r - ap) * u[a][f];
            mat.ap[f] = ap_r;
            b[f] = rhs;
            let denom = (ap_r - sum_anb).max(1e-12 * ap_r);
            d[f] = area / denom;
        }
        (mat, b, d)
    }

    /// Predictor for opening faces: u* = (p_L − p_H + dir·Δp_fan)/R.
    fn open_faces(&mut self, st: &mut State) {
        let rho = self.props.rho;
        for a in 0..3 {
            let fd = self.fd[a];
            for f in 0..self.kind[a].len() {
                if self.kind[a][f] != OPENING {
                    continue;
                }
                let p = fijk(fd, f);
                let lo_side = p[a] == 0;
                let mut pc = p;
                if !lo_side {
                    pc[a] = self.n[a] - 1;
                }
                let c = self.cell(pc);
                let bi = self.face_bnd[a][f];
                let (lin, quad) = if bi >= 0 {
                    self.open_loss[bi as usize]
                } else {
                    (0.0, 0.0)
                };
                let u0 = st.u[a][f];
                // inflow when the velocity points into the domain
                let inflow = if lo_side { u0 > 0.0 } else { u0 < 0.0 };
                let zeta = quad / (0.5 * rho);
                let cdyn = if inflow { 1.0 + zeta } else { zeta.max(0.01) };
                let mut r = lin + 0.5 * rho * cdyn * u0.abs().max(0.005);
                let (pl, ph) = if lo_side {
                    (0.0, st.p[c])
                } else {
                    (st.p[c], 0.0)
                };
                let mut num = pl - ph;
                let fi = self.face_fan[a][f];
                if fi >= 0 {
                    let fan = &self.fans[fi as usize];
                    let (p0, s, q0) = fan.lin;
                    num += fan.dir * (p0 - s * q0);
                    r += -s * fan.area;
                }
                st.u[a][f] = num / r;
                self.d_open[a][f] = 1.0 / r;
            }
        }
    }

    /// Sum of |ρ u A| over all faces of fluid cells (mass-flow scale), kg/s.
    fn mass_scale(&self, st: &State) -> f64 {
        let g = &self.m.grid;
        let rho = self.props.rho;
        let mut s = 0.0;
        for c in 0..self.m.ncells {
            if !self.fluid[c] {
                continue;
            }
            let p = g.ijk(c);
            for a in 0..3 {
                let mut ph = p;
                ph[a] += 1;
                s += rho
                    * g.face_area(c, a)
                    * 0.5
                    * (st.u[a][fidx(self.fd[a], p)].abs() + st.u[a][fidx(self.fd[a], ph)].abs());
            }
        }
        s
    }

    /// Mass imbalance per cell (inflow − outflow), kg/s.
    fn imbalance(&self, st: &State) -> Vec<f64> {
        let g = &self.m.grid;
        let rho = self.props.rho;
        (0..self.m.ncells)
            .map(|c| {
                if !self.fluid[c] {
                    return 0.0;
                }
                let p = g.ijk(c);
                let mut s = 0.0;
                for a in 0..3 {
                    let area = g.face_area(c, a);
                    let mut ph = p;
                    ph[a] += 1;
                    s +=
                        rho * area * (st.u[a][fidx(self.fd[a], p)] - st.u[a][fidx(self.fd[a], ph)]);
                }
                s
            })
            .collect()
    }

    /// Pressure correction; returns the normalised continuity residual (raw sum).
    fn pressure_correction(&self, st: &mut State, d: &[Vec<f64>; 3]) -> Result<f64> {
        let g = &self.m.grid;
        let n = self.n;
        let nc = self.m.ncells;
        let rho = self.props.rho;
        let s = [1usize, n[0], n[0] * n[1]];
        let mut mat = Stencil::new(n, 0);
        let b = self.imbalance(st);
        let res: f64 = b.iter().map(|x| x.abs()).sum();
        for c in 0..nc {
            if !self.fluid[c] {
                mat.fix(c);
                continue;
            }
            let p = g.ijk(c);
            for a in 0..3 {
                let area = g.face_area(c, a);
                let mut ph = p;
                ph[a] += 1;
                let fl = fidx(self.fd[a], p);
                let fh = fidx(self.fd[a], ph);
                match self.kind[a][fl] {
                    ACTIVE => {
                        let co = rho * area * d[a][fl];
                        mat.neg[a][c] = co;
                        mat.ap[c] += co;
                    }
                    OPENING => mat.ap[c] += rho * area * self.d_open[a][fl],
                    _ => {}
                }
                match self.kind[a][fh] {
                    ACTIVE => {
                        let co = rho * area * d[a][fh];
                        mat.pos[a][c] = co;
                        mat.ap[c] += co;
                    }
                    OPENING => mat.ap[c] += rho * area * self.d_open[a][fh],
                    _ => {}
                }
            }
            if mat.ap[c] == 0.0 {
                mat.fix(c);
            }
        }
        let mut b = b;
        for (i, v) in b.iter_mut().enumerate() {
            if !self.fluid[i] {
                *v = 0.0;
            }
        }
        // Dirichlet pins for closed regions
        for &c in &self.pins {
            for a in 0..3 {
                if mat.pos[a][c] != 0.0 {
                    mat.neg[a][c + s[a]] = 0.0;
                }
                if mat.neg[a][c] != 0.0 {
                    mat.pos[a][c - s[a]] = 0.0;
                }
            }
            mat.fix(c);
            b[c] = 0.0;
        }
        let mut pp = vec![0.0; nc];
        let ps = pcg_with(&mat, &b, &mut pp, 0.02, 0.0, 400, Pre::Mg)?;
        if std::env::var_os("FERROTHERM_PROFILE").is_some() {
            eprintln!("prof: pressure pcg {} it", ps.iterations);
        }
        // correct velocities
        for a in 0..3 {
            let fd = self.fd[a];
            for f in 0..self.kind[a].len() {
                match self.kind[a][f] {
                    ACTIVE => {
                        let p = fijk(fd, f);
                        let mut pl = p;
                        pl[a] -= 1;
                        st.u[a][f] += d[a][f] * (pp[self.cell(pl)] - pp[self.cell(p)]);
                    }
                    OPENING => {
                        let p = fijk(fd, f);
                        if p[a] == 0 {
                            st.u[a][f] -= self.d_open[a][f] * pp[self.cell(p)];
                        } else {
                            let mut pc = p;
                            pc[a] = n[a] - 1;
                            st.u[a][f] += self.d_open[a][f] * pp[self.cell(pc)];
                        }
                    }
                    _ => {}
                }
            }
        }
        let ap = self.cfg.relax_pressure.clamp(0.05, 1.0);
        for c in 0..nc {
            if self.fluid[c] {
                st.p[c] += ap * pp[c];
            }
        }
        Ok(res)
    }

    /// Effective conductivity per cell.
    fn k_eff(&self, st: &State) -> Vec<[f64; 3]> {
        let mut k = if self.m.has_kt {
            self.m.k_at(&st.t)
        } else {
            self.m.k.clone()
        };
        let rc = self.props.rho * self.props.cp / PR_T;
        for c in 0..self.m.ncells {
            if self.fluid[c] && st.nut[c] > 0.0 {
                let add = rc * st.nut[c];
                for a in 0..3 {
                    k[c][a] += add;
                }
            }
        }
        k
    }

    /// Energy equation (all unknowns). `relax` < 1: steady under-relaxation.
    ///
    /// `conservative = false` drops the net-outflow term ΔF·T_P of each fluid
    /// cell (Patankar 1980, §5.7): while the flow is not yet mass-conserving
    /// the equation stays bounded and its residual is not polluted by
    /// continuity errors. `true` keeps the conservative form, with the
    /// ambient as temperature datum, for the final solve (exact global
    /// balance) and for transients. Both coincide on a converged flow.
    fn energy(
        &self,
        st: &State,
        relax: f64,
        cap: Option<(&[f64], f64)>,
        conservative: bool,
    ) -> (Stencil, Vec<f64>) {
        let m = self.m;
        let g = &m.grid;
        let n = self.n;
        let nc = m.ncells;
        let s = [1usize, n[0], n[0] * n[1]];
        let extra = m.nnodes - nc;
        let mut mat = Stencil::new(n, extra);
        let mut b = m.source.clone();
        let rc = self.props.rho * self.props.cp;
        let k = self.k_eff(st);
        let gc = conductances(g, &k);
        let bnd = m.boundary_at(&k);
        // net mass outflow × cp of every cell, W/K
        let mut fnet = vec![0.0; nc];
        for c in 0..nc {
            let p = g.ijk(c);
            for a in 0..3 {
                if p[a] + 1 >= n[a] {
                    continue;
                }
                let c2 = c + s[a];
                let gf = gc[a][c];
                let mut ph = p;
                ph[a] += 1;
                let ff = if self.fluid[c] && self.fluid[c2] {
                    rc * st.u[a][fidx(self.fd[a], ph)] * g.face_area(c, a)
                } else {
                    0.0
                };
                if gf == 0.0 && ff == 0.0 {
                    continue;
                }
                let dif = if gf > 0.0 {
                    gf * plaw(self.scheme, ff / gf)
                } else {
                    0.0
                };
                fnet[c] += ff;
                fnet[c2] -= ff;
                mat.pos[a][c] = dif + (-ff).max(0.0);
                mat.ap[c] += dif + ff.max(0.0);
                mat.neg[a][c2] = dif + ff.max(0.0);
                mat.ap[c2] += dif + (-ff).max(0.0);
            }
        }
        for l in &m.links {
            mat.ap[l.a] += l.g;
            mat.ap[l.b] += l.g;
            mat.links.push((l.a, l.b, l.g));
        }
        for (e, &tsurf) in bnd.iter().zip(&st.ts) {
            let et = element_terms(e, tsurf);
            mat.ap[e.node] += et.g;
            b[e.node] += et.g * et.teq + et.q_node;
        }
        // advection through the domain boundary
        for a in 0..3 {
            for f in 0..self.kind[a].len() {
                let k = self.kind[a][f];
                if k != OPENING && k != FIXED {
                    continue;
                }
                let p = fijk(self.fd[a], f);
                let lo = p[a] == 0;
                if !(lo || p[a] == n[a]) {
                    continue;
                }
                let mut pc = p;
                if !lo {
                    pc[a] = n[a] - 1;
                }
                let c = self.cell(pc);
                let uin = if lo { st.u[a][f] } else { -st.u[a][f] };
                let fl = rc * uin.abs() * g.face_area(c, a);
                if uin > 0.0 {
                    b[c] += fl * self.t_in_face(a, f);
                    fnet[c] -= fl;
                } else {
                    mat.ap[c] += fl;
                    fnet[c] += fl;
                }
            }
        }
        for c in 0..nc {
            if !self.fluid[c] || fnet[c] == 0.0 {
                continue;
            }
            if conservative {
                b[c] += fnet[c] * self.t_amb;
            } else {
                mat.ap[c] -= fnet[c];
            }
        }
        if let Some(r) = self.s2s {
            for (node, sp, sc) in r.sources(&st.t) {
                mat.ap[node] += sp;
                b[node] += sc;
            }
        }
        if let Some((told, dt)) = cap {
            for i in 0..m.nnodes {
                let c = m.capacity[i] / dt;
                mat.ap[i] += c;
                b[i] += c * told[i];
            }
        }
        for i in 0..m.nnodes {
            if mat.ap[i] == 0.0 {
                mat.ap[i] = 1.0;
                b[i] = st.t[i];
            }
        }
        if relax < 1.0 {
            for i in 0..m.nnodes {
                let ap = mat.ap[i];
                mat.ap[i] = ap / relax;
                b[i] += (mat.ap[i] - ap) * st.t[i];
            }
        }
        (mat, b)
    }

    fn t_in_face(&self, a: usize, f: usize) -> f64 {
        let bi = self.face_bnd[a][f];
        if bi >= 0 {
            self.t_in[bi as usize]
        } else {
            self.t_amb
        }
    }

    /// Net heat advected out through the domain boundary (relative to T_amb) per case boundary, and
    /// in/out volume flows.
    fn boundary_flows(&self, st: &State) -> (Vec<(f64, f64, f64)>, f64) {
        let g = &self.m.grid;
        let n = self.n;
        let rc = self.props.rho * self.props.cp;
        let mut per = vec![(0.0, 0.0, 0.0); self.t_in.len()];
        let mut total = 0.0;
        for a in 0..3 {
            for f in 0..self.kind[a].len() {
                let k = self.kind[a][f];
                if k != OPENING && k != FIXED {
                    continue;
                }
                let p = fijk(self.fd[a], f);
                let lo = p[a] == 0;
                if !(lo || p[a] == n[a]) {
                    continue;
                }
                let mut pc = p;
                if !lo {
                    pc[a] = n[a] - 1;
                }
                let c = self.cell(pc);
                let area = g.face_area(c, a);
                let uin = if lo { st.u[a][f] } else { -st.u[a][f] };
                let q = uin.abs() * area;
                let tin = self.t_in_face(a, f);
                let (heat, qi, qo) = if uin > 0.0 {
                    (-rc * q * tin, q, 0.0)
                } else {
                    (rc * q * st.t[c], 0.0, q)
                };
                total += heat;
                let bi = self.face_bnd[a][f];
                if bi >= 0 {
                    let e = &mut per[bi as usize];
                    let rel = if uin > 0.0 {
                        -rc * q * (tin - self.t_amb)
                    } else {
                        rc * q * (st.t[c] - self.t_amb)
                    };
                    e.0 += qi;
                    e.1 += qo;
                    e.2 += rel;
                }
            }
        }
        (per, total)
    }

    fn snapshot(&self, st: &State, time_s: f64) -> Snapshot {
        let nc = self.m.ncells;
        let vel = (0..nc)
            .map(|c| {
                if self.fluid[c] {
                    let v = self.cell_velocity(st, c);
                    [v[0] as f32, v[1] as f32, v[2] as f32]
                } else {
                    [0.0; 3]
                }
            })
            .collect();
        Snapshot {
            time_s,
            t: st.t.clone(),
            vel: Some(vel),
            p: Some(st.p.iter().map(|x| *x as f32).collect()),
        }
    }
}

fn face_area(m: &Model, fd: [usize; 3], a: usize, f: usize) -> f64 {
    let p = fijk(fd, f);
    let t = tangential(a);
    m.grid.width(t[0], p[t[0]]) * m.grid.width(t[1], p[t[1]])
}

/// ν_t/ν from Spalding's law for the local Reynolds number Re = |U|·L/ν.
pub fn spalding_nut_ratio(re: f64) -> f64 {
    if !(re > 0.0) {
        return 0.0;
    }
    let yplus = |up: f64| {
        let k = KAPPA * up;
        up + ((k).exp() - 1.0 - k - k * k / 2.0 - k * k * k / 6.0) / E_WALL
    };
    let dyplus = |up: f64| {
        let k = KAPPA * up;
        1.0 + KAPPA * ((k).exp() - 1.0 - k - k * k / 2.0) / E_WALL
    };
    // solve u+·y+(u+) = Re with Newton (bracketed)
    let mut up = re.sqrt().min(30.0);
    for _ in 0..60 {
        let f = up * yplus(up) - re;
        let df = yplus(up) + up * dyplus(up);
        let next = up - f / df;
        let next = if next <= 0.0 {
            0.5 * up
        } else {
            next.min(up * 2.0 + 1.0)
        };
        if (next - up).abs() < 1e-10 * up.max(1.0) {
            up = next;
            break;
        }
        up = next;
    }
    (dyplus(up) - 1.0).max(0.0)
}

/// Solve the conjugate flow + heat transfer problem.
pub fn run(
    m: &Model,
    case: &Case,
    s2s: Option<&S2s>,
    observer: &mut dyn FnMut(Event) -> bool,
) -> Result<RunResult> {
    let start = std::time::Instant::now();
    let mut sv = Solver::new(m, case, s2s)?;
    let nc = m.ncells;
    let mut st = State {
        u: sv.fixed_u.clone(),
        p: vec![0.0; nc],
        t: vec![m.initial_c; m.nnodes],
        ts: vec![m.initial_c; m.boundary.len()],
        nut: vec![0.0; nc],
    };
    let lvel = sv.cfg.turbulence == Turbulence::Lvel;
    let tol = sv.cfg.tolerance;
    let mut residuals: Vec<[f64; 3]> = Vec::new();
    let mut norm = [0.0f64; 3];
    let mut lin_it = 0usize;
    let mut monitor_hist: Vec<Vec<f64>> = vec![Vec::new(); sv.monitors.len()];
    let power = m.total_power().abs();

    // one outer SIMPLEC iteration; returns raw residuals [mass, momentum, energy]
    let iterate = |sv: &mut Solver,
                   st: &mut State,
                   prev: Option<(&State, f64)>,
                   lin_it: &mut usize|
     -> Result<[f64; 3]> {
        let prof = std::env::var_os("FERROTHERM_PROFILE").is_some();
        let t0 = std::time::Instant::now();
        if lvel {
            sv.update_nut(st);
        }
        sv.update_fans(st);
        let t1 = t0.elapsed();
        let mut d: [Vec<f64>; 3] = Default::default();
        let mut mom = 0.0;
        let mut mom_scale = 0.0;
        let mut mom_asm = std::time::Duration::ZERO;
        let mut mom_it = 0;
        for a in 0..3 {
            let pu = prev.map(|(p, dt)| (p.u[a].as_slice(), dt));
            let ta = std::time::Instant::now();
            let (mat, b, da) = sv.momentum(st, a, pu);
            mom_asm += ta.elapsed();
            mom += mat.residual_l1(&st.u[a], &b);
            mom_scale += (0..mat.ns)
                .filter(|&f| sv.kind[a][f] == ACTIVE)
                .map(|f| (mat.ap[f] * st.u[a][f]).abs())
                .sum::<f64>();
            let s = bicgstab(&mat, &b, &mut st.u[a], 0.05, 0.0, 30);
            *lin_it += s.iterations;
            mom_it += s.iterations;
            d[a] = da;
        }
        let t2 = t0.elapsed();
        sv.open_faces(st);
        let mass_scale = sv.mass_scale(st);
        let mass = sv.pressure_correction(st, &d)?;
        let t3 = t0.elapsed();
        // energy
        let relax = if prev.is_some() {
            1.0
        } else {
            sv.cfg.relax_temperature.clamp(0.05, 1.0)
        };
        let cap = prev.map(|(p, dt)| (p.t.as_slice(), dt));
        let conservative = prev.is_some();
        let (emat0, eb0) = sv.energy(st, 1.0, cap, conservative);
        let eres = emat0.residual_l1(&st.t, &eb0);
        let (emat, eb) = if relax < 1.0 {
            sv.energy(st, relax, cap, conservative)
        } else {
            (emat0, eb0)
        };
        let t4 = t0.elapsed();
        let s = bicgstab_with(&emat, &eb, &mut st.t, 0.05, 0.0, 40, Pre::Mg);
        *lin_it += s.iterations;
        st.ts = surface_temps(sv.m, &st.t, &st.ts);
        if prof {
            eprintln!(
                "prof: nut/fans {:?} momentum {:?} (asm {:?}, {} it) pressure {:?} energy-asm {:?} energy-solve {:?} ({} it, red {:.1e})",
                t1, t2 - t1, mom_asm, mom_it, t3 - t2, t4 - t3, t0.elapsed() - t4, s.iterations, s.reduction
            );
        }
        // energy scale: imposed power or the heat crossing the boundaries
        let hf = heat_flow(sv.m, &st.t, &st.ts, sv.m.boundary_names.len(), 0.0);
        let e_scale = power.max(0.5 * hf.per_boundary.iter().map(|q| q.abs()).sum::<f64>());
        Ok([
            mass / mass_scale.max(1e-30),
            mom / mom_scale.max(1e-30),
            eres / e_scale.max(1e-6),
        ])
    };

    let normalise = |raw: [f64; 3], _norm: &mut [f64; 3], _it: usize| -> [f64; 3] { raw };

    let mut snapshots = Vec::new();
    let mut history = Vec::new();
    let mut converged = false;
    let mut steady_stop = false;
    let mut iters = 0usize;
    let mut averaged = 0usize;
    let mut oscillation = 0.0f64;
    let junctions = |t: &[f64]| m.components.iter().map(|c| t[c.node]).collect::<Vec<f64>>();
    let storage_rate;
    match case.solver.mode {
        Mode::Steady => {
            if !observer(Event::Message("solving flow and heat transfer")) {
                return Err(Error::Cancelled);
            }
            let mut window: std::collections::VecDeque<Vec<f64>> =
                std::collections::VecDeque::new();
            let maxit = sv.cfg.max_iterations;
            let navg = (maxit / 4).clamp(1, 200);
            let mut sum_u: [Vec<f64>; 3] = [
                vec![0.0; st.u[0].len()],
                vec![0.0; st.u[1].len()],
                vec![0.0; st.u[2].len()],
            ];
            let mut sum_nut = vec![0.0; st.nut.len()];
            let mut nsum = 0usize;
            for it in 1..=maxit {
                let raw = iterate(&mut sv, &mut st, None, &mut lin_it)?;
                let r = normalise(raw, &mut norm, it);
                residuals.push(r);
                for (k, mo) in sv.monitors.iter().enumerate() {
                    monitor_hist[k].push(st.t[mo.2]);
                }
                iters = it;
                if !observer(Event::FlowIteration {
                    iteration: it,
                    max_iterations: sv.cfg.max_iterations,
                    residuals: r,
                }) {
                    return Err(Error::Cancelled);
                }
                // key temperatures: junctions, monitors, hottest cell
                let mut keys: Vec<f64> = m.components.iter().map(|c| st.t[c.node]).collect();
                keys.extend(sv.monitors.iter().map(|mo| st.t[mo.2]));
                keys.push(st.t[..nc].iter().cloned().fold(f64::NEG_INFINITY, f64::max));
                window.push_back(keys);
                if window.len() > 50 {
                    window.pop_front();
                }
                oscillation = if window.len() >= 2 {
                    let k = window[0].len();
                    (0..k)
                        .map(|j| {
                            let (lo, hi) = window
                                .iter()
                                .fold((f64::INFINITY, f64::NEG_INFINITY), |a, w| {
                                    (a.0.min(w[j]), a.1.max(w[j]))
                                });
                            hi - lo
                        })
                        .fold(0.0, f64::max)
                } else {
                    0.0
                };
                if it + navg > maxit {
                    for a in 0..3 {
                        for (s, v) in sum_u[a].iter_mut().zip(&st.u[a]) {
                            *s += v;
                        }
                    }
                    for (s, v) in sum_nut.iter_mut().zip(&st.nut) {
                        *s += v;
                    }
                    nsum += 1;
                }
                let steady = window.len() == 50 && {
                    let k = window[0].len();
                    (0..k).all(|j| {
                        let (lo, hi) = window
                            .iter()
                            .fold((f64::INFINITY, f64::NEG_INFINITY), |a, w| {
                                (a.0.min(w[j]), a.1.max(w[j]))
                            });
                        // 0.05 K or 0.2 % of the rise above ambient
                        hi - lo < (0.002 * (hi - sv.t_amb).abs()).max(0.05)
                    })
                };
                if std::env::var_os("FERROTHERM_PROFILE").is_some()
                    && it % 25 == 0
                    && window.len() == 50
                {
                    let k = window[0].len();
                    let ranges: Vec<String> = (0..k)
                        .map(|j| {
                            let (lo, hi) = window
                                .iter()
                                .fold((f64::INFINITY, f64::NEG_INFINITY), |a, w| {
                                    (a.0.min(w[j]), a.1.max(w[j]))
                                });
                            format!("{:.4}", hi - lo)
                        })
                        .collect();
                    eprintln!("prof: it {it} key ranges {}", ranges.join(" "));
                }
                if it >= 10 && r.iter().all(|x| *x < tol) {
                    converged = true;
                    break;
                }
                if it >= 100 && steady_stop_allowed(steady, r, tol) {
                    converged = true;
                    steady_stop = true;
                    break;
                }
                if r.iter().any(|x| !x.is_finite()) {
                    return Err(Error::Invalid(
                        "the flow solution diverged: lower relax_velocity / relax_temperature or refine the grid".into(),
                    ));
                }
            }
            if !converged && nsum > 1 {
                // time-average of the oscillating iteration (continuity is
                // linear, so the average stays mass-conserving)
                let w = 1.0 / nsum as f64;
                for a in 0..3 {
                    for (u, s) in st.u[a].iter_mut().zip(&sum_u[a]) {
                        *u = s * w;
                    }
                }
                for (u, s) in st.nut.iter_mut().zip(&sum_nut) {
                    *u = s * w;
                }
                averaged = nsum;
            }
            // final energy solve on the frozen flow, without relaxation
            for _ in 0..3 {
                let (mat, b) = sv.energy(&st, 1.0, None, true);
                let s = bicgstab_with(&mat, &b, &mut st.t, 1e-10, 0.0, 2000, Pre::Mg);
                lin_it += s.iterations;
                st.ts = surface_temps(m, &st.t, &st.ts);
            }
            history.push((0.0, junctions(&st.t)));
            snapshots.push(sv.snapshot(&st, 0.0));
            storage_rate = 0.0;
        }
        Mode::Transient => {
            let tr = case.solver.transient.as_ref().ok_or_else(|| {
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
            snapshots.push(sv.snapshot(&st, 0.0));
            history.push((0.0, junctions(&st.t)));
            let mut rate = 0.0;
            converged = true;
            for step in 1..=nsteps {
                let old = State {
                    u: st.u.clone(),
                    p: st.p.clone(),
                    t: st.t.clone(),
                    ts: st.ts.clone(),
                    nut: st.nut.clone(),
                };
                let mut step_ok = false;
                for inner in 1..=sv.cfg.iterations_per_step.max(1) {
                    let raw = iterate(&mut sv, &mut st, Some((&old, dt)), &mut lin_it)?;
                    let r = normalise(raw, &mut norm, iters + 1);
                    iters += 1;
                    residuals.push(r);
                    if r.iter().any(|x| !x.is_finite()) {
                        return Err(Error::Invalid(
                            "the transient flow solution diverged: reduce dt_s".into(),
                        ));
                    }
                    if inner >= 3 && r.iter().all(|x| *x < tol) {
                        step_ok = true;
                        break;
                    }
                }
                converged &= step_ok;
                rate = (0..m.nnodes)
                    .map(|i| m.capacity[i] * (st.t[i] - old.t[i]) / dt)
                    .sum();
                let time = step as f64 * dt;
                for (k, mo) in sv.monitors.iter().enumerate() {
                    monitor_hist[k].push(st.t[mo.2]);
                }
                history.push((time, junctions(&st.t)));
                if step == nsteps || every.map(|e| step % e == 0).unwrap_or(false) {
                    snapshots.push(sv.snapshot(&st, time));
                }
                if !observer(Event::Step {
                    step,
                    steps: nsteps,
                    time_s: time,
                }) {
                    return Err(Error::Cancelled);
                }
            }
            storage_rate = rate;
        }
    }

    // ---- results ----
    let nb = m.boundary_names.len();
    let mut heat = heat_flow(m, &st.t, &st.ts, nb, storage_rate);
    let (per_open, adv) = sv.boundary_flows(&st);
    heat.advected_out = adv;
    heat.total_out += adv;
    if let Some(r) = s2s {
        heat.radiation_to_openings = r.loss_to_openings(&st.t);
        heat.total_out += heat.radiation_to_openings;
    }
    let mut openings = Vec::new();
    for (bi, b) in case.boundaries.iter().enumerate() {
        if matches!(b.kind, BoundaryKind::Opening | BoundaryKind::Inlet) {
            let (qi, qo, h) = per_open[bi];
            heat.per_boundary[bi] += h;
            openings.push(OpeningResult {
                name: m.boundary_names[bi].clone(),
                inflow_m3_s: qi,
                outflow_m3_s: qo,
                heat_out_w: h,
            });
        }
    }
    sv.update_fans(&st);
    let fans = sv
        .fans
        .iter()
        .map(|f| {
            let q = f
                .faces
                .iter()
                .map(|&fi| f.dir * st.u[f.axis][fi] * face_area(m, sv.fd[f.axis], f.axis, fi))
                .sum::<f64>();
            let p = if f.fixed.is_some() {
                // pressure difference across the plane (interior fans)
                if f.boundary {
                    f64::NAN
                } else {
                    let mut dp = 0.0;
                    let mut w = 0.0;
                    for &fi in &f.faces {
                        let pf = fijk(sv.fd[f.axis], fi);
                        let mut pl = pf;
                        pl[f.axis] -= 1;
                        let ar = face_area(m, sv.fd[f.axis], f.axis, fi);
                        dp += f.dir * (st.p[sv.cell(pf)] - st.p[sv.cell(pl)]) * ar;
                        w += ar;
                    }
                    dp / w
                }
            } else {
                f.pressure(q)
            };
            FanResult {
                name: f.name.clone(),
                flow_m3_s: q,
                pressure_pa: p,
                area_m2: f.area,
            }
        })
        .collect();
    let mut vmax = 0.0f64;
    for c in 0..nc {
        if sv.fluid[c] {
            let v = sv.cell_velocity(&st, c);
            vmax = vmax.max((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt());
        }
    }
    let monitors = sv
        .monitors
        .iter()
        .enumerate()
        .map(|(k, (name, pos, c))| {
            let v = sv.cell_velocity(&st, *c);
            MonitorResult {
                name: name.clone(),
                position_mm: *pos,
                t_c: st.t[*c],
                speed_m_s: (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt(),
                history: std::mem::take(&mut monitor_hist[k]),
            }
        })
        .collect();
    let fluid_name = m
        .materials
        .iter()
        .find(|x| x.is_fluid())
        .map(|x| x.id.clone())
        .unwrap_or_default();
    Ok(RunResult {
        snapshots,
        junction_history: history,
        linear_iterations: lin_it,
        nonlinear_iterations: iters,
        heat,
        surface_t: st.ts.clone(),
        elapsed_s: start.elapsed().as_secs_f64(),
        flow: Some(FlowResult {
            iterations: iters,
            converged,
            converged_by_monitors: steady_stop,
            residuals,
            fans,
            openings,
            monitors,
            max_speed_m_s: vmax,
            fluid: fluid_name,
            turbulence: match sv.cfg.turbulence {
                Turbulence::Laminar => "laminar".into(),
                Turbulence::Lvel => "LVEL".into(),
            },
            averaged_iterations: averaged,
            oscillation_k: oscillation,
        }),
    })
}

/// Early stop of the steady iteration on settled key temperatures.
///
/// Settled junction/monitor/hottest temperatures alone are not enough: a
/// slowly drifting energy field (e.g. a sealed chassis whose temperature
/// keeps rising by a fraction of a percent per iteration) looks "settled"
/// relative to its rise. The mass *and* energy residuals must also be within
/// ten times the tolerance; otherwise the iteration continues and, at
/// `max_iterations`, is reported as not converged.
fn steady_stop_allowed(steady: bool, r: [f64; 3], tol: f64) -> bool {
    steady && r[0] < 10.0 * tol && r[2] < 10.0 * tol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_stop_needs_energy_residual() {
        let tol = 1e-3;
        // settled temperatures with converged residuals: stop
        assert!(steady_stop_allowed(true, [5e-4, 1e-2, 6e-3], tol));
        // settled temperatures but the energy field still drifting (the
        // sealed-chassis case reported converged at 32 000 °C): continue
        assert!(!steady_stop_allowed(true, [1e-5, 2e-3, 1.0], tol));
        // mass not converged: continue
        assert!(!steady_stop_allowed(true, [5e-2, 1e-3, 1e-3], tol));
        // temperatures still moving: continue
        assert!(!steady_stop_allowed(false, [1e-5, 1e-5, 1e-5], tol));
    }

    #[test]
    fn spalding_limits() {
        // laminar sublayer: tiny Re → ν_t ≈ 0
        assert!(spalding_nut_ratio(1.0) < 1e-3);
        // fully turbulent: ν_t/ν grows with Re
        let a = spalding_nut_ratio(1e3);
        let b = spalding_nut_ratio(1e5);
        assert!(a > 1.0 && b > a, "{a} {b}");
    }
}
