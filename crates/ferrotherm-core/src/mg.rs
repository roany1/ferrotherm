// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Additive-correction multigrid (Hutchinson & Raithby 1986) used as a
//! preconditioner: cells are agglomerated 2×2×2 into coarse cells, the
//! coarse equations are the sums of the fine ones (piecewise-constant
//! prolongation, Galerkin with summation restriction), and a V-cycle with
//! Gauss–Seidel smoothing is applied. Symmetric for symmetric matrices
//! (forward sweep before, backward sweep after the coarse correction).

use crate::stencil::Stencil;

struct Level {
    mat: Stencil,
    /// Fine (previous level) structured index → coarse index.
    map: Vec<u32>,
}

pub struct Multigrid {
    levels: Vec<Level>,
    /// Over-correction factor of the coarse-grid correction.
    pub omega: f64,
    /// Link adjacency of the finest level: per unknown, (other, g).
    adj: Vec<Vec<(usize, f64)>>,
    pub sweeps: usize,
}

fn coarsen(m: &Stencil) -> Option<Level> {
    let n = m.n;
    if n.iter().all(|&x| x <= 2) || m.ns < 64 {
        return None;
    }
    let nc = [n[0].div_ceil(2), n[1].div_ceil(2), n[2].div_ceil(2)];
    let mut c = Stencil::new(nc, 0);
    let s = m.strides();
    let cs = c.strides();
    let mut map = vec![0u32; m.ns];
    for k in 0..n[2] {
        for j in 0..n[1] {
            for i in 0..n[0] {
                let f = i + n[0] * (j + n[1] * k);
                map[f] = ((i / 2) + nc[0] * ((j / 2) + nc[1] * (k / 2))) as u32;
            }
        }
    }
    for f in 0..m.ns {
        let cf = map[f] as usize;
        c.ap[cf] += m.ap[f];
        for a in 0..3 {
            let p = m.pos[a][f];
            if p != 0.0 {
                let g = f + s[a];
                let cg = map[g] as usize;
                if cg == cf {
                    c.ap[cf] -= p;
                } else {
                    debug_assert_eq!(cg, cf + cs[a]);
                    c.pos[a][cf] += p;
                }
            }
            let q = m.neg[a][f];
            if q != 0.0 {
                let g = f - s[a];
                let cg = map[g] as usize;
                if cg == cf {
                    c.ap[cf] -= q;
                } else {
                    c.neg[a][cf] += q;
                }
            }
        }
    }
    for v in c.ap.iter_mut() {
        if *v <= 0.0 || !v.is_finite() {
            *v = 1.0;
        }
    }
    Some(Level { mat: c, map })
}

impl Multigrid {
    pub fn new(m: &Stencil) -> Multigrid {
        let mut levels = Vec::new();
        let mut cur = coarsen(m);
        while let Some(l) = cur {
            cur = coarsen(&l.mat);
            levels.push(l);
            if levels.len() >= 12 {
                break;
            }
        }
        let mut adj = vec![Vec::new(); m.len()];
        for &(i, j, g) in &m.links {
            adj[i].push((j, g));
            adj[j].push((i, g));
        }
        // tuned on a 3-D conduction problem with a 1000:1 contrast:
        // 1 sweep + 1.5 over-correction was ~2.4× faster than DILU-PCG
        let sweeps = 1;
        let omega = 1.5;
        Multigrid {
            levels,
            omega,
            adj,
            sweeps,
        }
    }

    fn gs(m: &Stencil, adj: Option<&[Vec<(usize, f64)>]>, b: &[f64], x: &mut [f64], forward: bool) {
        let s = m.strides();
        let n = m.len();
        let row = |c: usize, x: &mut [f64]| {
            let mut v = b[c];
            if c < m.ns {
                for a in 0..3 {
                    let p = m.pos[a][c];
                    if p != 0.0 {
                        v += p * x[c + s[a]];
                    }
                    let q = m.neg[a][c];
                    if q != 0.0 {
                        v += q * x[c - s[a]];
                    }
                }
            }
            if let Some(adj) = adj {
                for &(o, g) in &adj[c] {
                    v += g * x[o];
                }
            }
            x[c] = v / m.ap[c];
        };
        if forward {
            for c in 0..n {
                row(c, x);
            }
        } else {
            for c in (0..n).rev() {
                row(c, x);
            }
        }
    }

    fn residual(m: &Stencil, b: &[f64], x: &[f64], r: &mut [f64]) {
        m.apply(x, r);
        for i in 0..r.len() {
            r[i] = b[i] - r[i];
        }
    }

    fn cycle(&self, k: usize, fine: &Stencil, b: &[f64], x: &mut [f64]) {
        let adj = if k == 0 { Some(self.adj.as_slice()) } else { None };
        if k == self.levels.len() {
            // coarsest: a few sweeps
            for _ in 0..20 {
                Self::gs(fine, adj, b, x, true);
                Self::gs(fine, adj, b, x, false);
            }
            return;
        }
        for _ in 0..self.sweeps {
            Self::gs(fine, adj, b, x, true);
        }
        let mut r = vec![0.0; fine.len()];
        Self::residual(fine, b, x, &mut r);
        let lvl = &self.levels[k];
        let mut rc = vec![0.0; lvl.mat.len()];
        for f in 0..fine.ns {
            rc[lvl.map[f] as usize] += r[f];
        }
        let mut ec = vec![0.0; lvl.mat.len()];
        self.cycle(k + 1, &lvl.mat, &rc, &mut ec);
        for f in 0..fine.ns {
            x[f] += self.omega * ec[lvl.map[f] as usize];
        }
        for _ in 0..self.sweeps {
            Self::gs(fine, adj, b, x, false);
        }
    }

    /// z = M⁻¹ r (one V-cycle from zero).
    pub fn apply(&self, m: &Stencil, r: &[f64], z: &mut [f64]) {
        for v in z.iter_mut() {
            *v = 0.0;
        }
        self.cycle(0, m, r, z);
    }
}
