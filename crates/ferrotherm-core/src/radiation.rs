// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Internal surface-to-surface radiation between grey, diffuse surfaces.
//!
//! * Patches: solid faces bordering a transparent gas cell (air), grouped by
//!   owner object, orientation and a square tile of `patch_size_mm`.
//!   Domain faces of gas cells are black patches at ambient temperature when
//!   they are openings/inlets, re-radiating (no net exchange) otherwise.
//! * View factors: Monte-Carlo ray tracing through the grid (cosine-weighted
//!   rays, 3-D DDA), then reciprocity smoothing A_i F_ij = A_j F_ji and row
//!   normalisation.
//! * Radiosity: J_i = ε_i σ T_i⁴ + (1 − ε_i) Σ_j F_ij J_j, solved by
//!   Gauss–Seidel; net heat Q_i = A_i (J_i − G_i), applied to the cells as a
//!   source linearised with h = 4 ε σ T³.
//!
//! Compact component bodies are treated as re-radiating surfaces.

use crate::case::{BoundaryKind, Case, Face, RadiationSettings};
use crate::model::{Model, Owner};
use crate::simulate::{KELVIN, SIGMA};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchKind {
    Surface,
    Opening,
    Reradiating,
}

#[derive(Debug, Clone)]
pub struct Patch {
    pub kind: PatchKind,
    pub area: f64,
    pub eps: f64,
    /// (node, area) of the cells behind the patch (surface patches).
    pub members: Vec<(usize, f64)>,
    /// Faces of the patch: (gas cell, direction 0..6 = axis·2 + hi).
    faces: Vec<(usize, u8, f64)>,
}

pub struct S2s {
    pub patches: Vec<Patch>,
    /// Sparse view factors per row: (column, F).
    pub f: Vec<Vec<(u32, f32)>>,
    t_amb: f64,
    j: Mutex<Vec<f64>>,
}

fn transparent(m: &Model, c: usize) -> bool {
    let mat = m.material(c);
    mat.category == "fluid" && mat.density < 10.0
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        ((self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

impl S2s {
    /// Build patches and view factors. Returns `None` when nothing radiates.
    pub fn build(m: &Model, case: &Case, cfg: &RadiationSettings) -> Option<S2s> {
        if !cfg.enabled {
            return None;
        }
        let g = &m.grid;
        let n = g.n();
        let st = [1usize, n[0], n[0] * n[1]];
        let tile = (cfg.patch_size_mm.max(0.1)) * 1e-3;
        let mut key_to_patch: HashMap<(i64, u8, i64, i64), usize> = HashMap::new();
        let mut patches: Vec<Patch> = Vec::new();
        let mut face_patch: HashMap<(usize, u8), u32> = HashMap::new();
        for c in 0..m.ncells {
            if !transparent(m, c) {
                continue;
            }
            let p = g.ijk(c);
            for a in 0..3 {
                for hi in [false, true] {
                    let dir = (a * 2 + hi as usize) as u8;
                    let edge = if hi { p[a] + 1 == n[a] } else { p[a] == 0 };
                    let t = crate::model::tangential(a);
                    let area = g.face_area(c, a);
                    let tile_of = |q: usize| (g.center(t[q], p[t[q]]) / tile).floor() as i64;
                    let (kind, owner_tag, node, eps) = if edge {
                        let face = match (a, hi) {
                            (0, false) => Face::Xmin,
                            (0, true) => Face::Xmax,
                            (1, false) => Face::Ymin,
                            (1, true) => Face::Ymax,
                            (_, false) => Face::Zmin,
                            (_, true) => Face::Zmax,
                        };
                        let open = m
                            .bc_at(face, p)
                            .map(|bi| {
                                matches!(
                                    case.boundaries[bi].kind,
                                    BoundaryKind::Opening | BoundaryKind::Inlet
                                )
                            })
                            .unwrap_or(false);
                        if open {
                            (PatchKind::Opening, -2, None, 1.0)
                        } else {
                            (PatchKind::Reradiating, -3, None, 0.0)
                        }
                    } else {
                        let nb = if hi { c + st[a] } else { c - st[a] };
                        if transparent(m, nb) {
                            continue;
                        }
                        match m.owner[nb] {
                            Owner::Component(ci) => {
                                (PatchKind::Reradiating, -(10 + ci as i64), None, 0.0)
                            }
                            Owner::Block(b) => (
                                PatchKind::Surface,
                                b as i64,
                                Some(nb),
                                m.blocks[b].emissivity.unwrap_or(cfg.default_emissivity),
                            ),
                            Owner::Fill => {
                                (PatchKind::Surface, -1, Some(nb), cfg.default_emissivity)
                            }
                        }
                    };
                    let key = (owner_tag, dir, tile_of(0), tile_of(1));
                    let pi = *key_to_patch.entry(key).or_insert_with(|| {
                        patches.push(Patch {
                            kind,
                            area: 0.0,
                            eps,
                            members: vec![],
                            faces: vec![],
                        });
                        patches.len() - 1
                    });
                    let pt = &mut patches[pi];
                    pt.area += area;
                    pt.faces.push((c, dir, area));
                    if let Some(node) = node {
                        pt.members.push((node, area));
                    }
                    face_patch.insert((c, dir), pi as u32);
                }
            }
        }
        if !patches.iter().any(|p| p.kind == PatchKind::Surface) {
            return None;
        }
        let rays = cfg.rays_per_patch.max(16);
        // ---- view factors by ray tracing ----
        let rows: Vec<Vec<(u32, f32)>> = patches
            .par_iter()
            .enumerate()
            .map(|(pi, pt)| {
                let mut rng = Rng(0x9E3779B97F4A7C15 ^ ((pi as u64 + 1) * 0xD1B54A32D192ED03));
                let mut hits: HashMap<u32, u32> = HashMap::new();
                let mut total = 0u32;
                // cumulative areas for member selection
                let mut cum = Vec::with_capacity(pt.faces.len());
                let mut s = 0.0;
                for f in &pt.faces {
                    s += f.2;
                    cum.push(s);
                }
                for _ in 0..rays {
                    let r = rng.next() * s;
                    let k = cum.partition_point(|x| *x < r).min(pt.faces.len() - 1);
                    let (c, dir, _) = pt.faces[k];
                    let a = (dir / 2) as usize;
                    let hi = dir % 2 == 1;
                    let p = g.ijk(c);
                    let t = crate::model::tangential(a);
                    // start on the face, ray points into the gas cell (away from the face)
                    let mut o = [0.0f64; 3];
                    o[a] = if hi {
                        g.nodes[a][p[a] + 1]
                    } else {
                        g.nodes[a][p[a]]
                    };
                    for q in 0..2 {
                        let lo = g.nodes[t[q]][p[t[q]]];
                        let hi2 = g.nodes[t[q]][p[t[q]] + 1];
                        o[t[q]] = lo + rng.next() * (hi2 - lo);
                    }
                    // cosine-weighted direction around the inward normal
                    let r1 = rng.next();
                    let r2 = rng.next();
                    let phi = 2.0 * std::f64::consts::PI * r1;
                    let sr = r2.sqrt();
                    let mut d = [0.0f64; 3];
                    d[a] = if hi {
                        -(1.0 - r2).sqrt()
                    } else {
                        (1.0 - r2).sqrt()
                    };
                    d[t[0]] = sr * phi.cos();
                    d[t[1]] = sr * phi.sin();
                    if let Some(hit) = trace(m, c, o, d, &face_patch) {
                        *hits.entry(hit).or_insert(0) += 1;
                    }
                    total += 1;
                }
                let mut row: Vec<(u32, f32)> = hits
                    .into_iter()
                    .map(|(j, h)| (j, h as f32 / total as f32))
                    .collect();
                row.sort_by_key(|x| x.0);
                row
            })
            .collect();
        // reciprocity smoothing + closure
        let np = patches.len();
        let mut map: Vec<HashMap<u32, f64>> = rows
            .iter()
            .map(|r| r.iter().map(|(j, f)| (*j, *f as f64)).collect())
            .collect();
        let mut sym: Vec<HashMap<u32, f64>> = vec![HashMap::new(); np];
        for i in 0..np {
            for (&j, &fij) in &map[i] {
                let ai = patches[i].area;
                let aj = patches[j as usize].area;
                let fji = map[j as usize].get(&(i as u32)).copied().unwrap_or(0.0);
                let v = 0.5 * (ai * fij + aj * fji);
                *sym[i].entry(j).or_insert(0.0) = v / ai;
                *sym[j as usize].entry(i as u32).or_insert(0.0) = v / aj;
            }
        }
        map.clear();
        let f: Vec<Vec<(u32, f32)>> = sym
            .into_iter()
            .map(|row| {
                let s: f64 = row.values().sum();
                let mut r: Vec<(u32, f32)> = row
                    .into_iter()
                    .map(|(j, v)| (j, if s > 0.0 { (v / s) as f32 } else { 0.0 }))
                    .collect();
                r.sort_by_key(|x| x.0);
                r
            })
            .collect();
        let t_amb = m.ambient_c.unwrap_or(m.initial_c);
        let j0 = vec![SIGMA * (t_amb + KELVIN).powi(4); np];
        Some(S2s {
            patches,
            f,
            t_amb,
            j: Mutex::new(j0),
        })
    }

    fn patch_t(&self, i: usize, t: &[f64]) -> f64 {
        let p = &self.patches[i];
        let mut s = 0.0;
        for (node, a) in &p.members {
            s += t[*node] * a;
        }
        s / p.area.max(1e-30) + KELVIN
    }

    /// Solve the radiosities; returns (J, G).
    fn solve(&self, t: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let np = self.patches.len();
        let mut j = self.j.lock().unwrap().clone();
        let eb: Vec<f64> = (0..np)
            .map(|i| match self.patches[i].kind {
                PatchKind::Surface => SIGMA * self.patch_t(i, t).powi(4),
                PatchKind::Opening => SIGMA * (self.t_amb + KELVIN).powi(4),
                PatchKind::Reradiating => 0.0,
            })
            .collect();
        let mut gv = vec![0.0; np];
        for _ in 0..1000 {
            let mut change = 0.0f64;
            let mut scale = 0.0f64;
            for i in 0..np {
                let gi: f64 = self.f[i]
                    .iter()
                    .map(|(k, f)| *f as f64 * j[*k as usize])
                    .sum();
                gv[i] = gi;
                let p = &self.patches[i];
                let new = match p.kind {
                    PatchKind::Surface => p.eps * eb[i] + (1.0 - p.eps) * gi,
                    PatchKind::Opening => eb[i],
                    PatchKind::Reradiating => gi,
                };
                change = change.max((new - j[i]).abs());
                scale = scale.max(new.abs());
                j[i] = new;
            }
            if change <= 1e-10 * scale.max(1e-30) {
                break;
            }
        }
        for i in 0..np {
            gv[i] = self.f[i]
                .iter()
                .map(|(k, f)| *f as f64 * j[*k as usize])
                .sum();
        }
        *self.j.lock().unwrap() = j.clone();
        (j, gv)
    }

    /// Linearised node sources (node, diag, rhs) for temperatures `t`.
    pub fn sources(&self, t: &[f64]) -> Vec<(usize, f64, f64)> {
        let (j, gv) = self.solve(t);
        let mut out = Vec::new();
        for (i, p) in self.patches.iter().enumerate() {
            if p.kind != PatchKind::Surface || p.members.is_empty() {
                continue;
            }
            let q_net = p.area * (j[i] - gv[i]); // W leaving the surface
            for &(node, a) in &p.members {
                let frac = a / p.area;
                let tk = t[node] + KELVIN;
                let h = 4.0 * p.eps * SIGMA * tk.powi(3) * a;
                out.push((node, h, -q_net * frac + h * t[node]));
            }
        }
        out
    }

    /// Heat radiated out through openings, W.
    pub fn loss_to_openings(&self, t: &[f64]) -> f64 {
        let (j, gv) = self.solve(t);
        self.patches
            .iter()
            .enumerate()
            .filter(|(_, p)| p.kind == PatchKind::Opening)
            .map(|(i, p)| p.area * (gv[i] - j[i]))
            .sum()
    }

    pub fn surface_patches(&self) -> usize {
        self.patches
            .iter()
            .filter(|p| p.kind == PatchKind::Surface)
            .count()
    }
}

/// Follow a ray from `o` (on a face of gas cell `c`) along `d`; returns the
/// patch hit.
fn trace(
    m: &Model,
    mut c: usize,
    o: [f64; 3],
    d: [f64; 3],
    face_patch: &HashMap<(usize, u8), u32>,
) -> Option<u32> {
    let g = &m.grid;
    let n = g.n();
    let st = [1usize, n[0], n[0] * n[1]];
    let mut pos = o;
    for _ in 0..(4 * (n[0] + n[1] + n[2]) + 16) {
        let p = g.ijk(c);
        // distance to the exit plane along each axis
        let mut tmin = f64::INFINITY;
        let mut amin = 0;
        for a in 0..3 {
            if d[a] > 1e-15 {
                let tt = (g.nodes[a][p[a] + 1] - pos[a]) / d[a];
                if tt < tmin {
                    tmin = tt;
                    amin = a;
                }
            } else if d[a] < -1e-15 {
                let tt = (g.nodes[a][p[a]] - pos[a]) / d[a];
                if tt < tmin {
                    tmin = tt;
                    amin = a;
                }
            }
        }
        let tmin = tmin.max(0.0);
        for a in 0..3 {
            pos[a] += d[a] * tmin;
        }
        let hi = d[amin] > 0.0;
        let dir = (amin * 2 + hi as usize) as u8;
        let edge = if hi {
            p[amin] + 1 == n[amin]
        } else {
            p[amin] == 0
        };
        if edge {
            return face_patch.get(&(c, dir)).copied();
        }
        let nb = if hi { c + st[amin] } else { c - st[amin] };
        if !transparent(m, nb) {
            return face_patch.get(&(c, dir)).copied();
        }
        c = nb;
    }
    None
}
