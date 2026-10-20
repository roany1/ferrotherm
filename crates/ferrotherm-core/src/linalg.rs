// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Symmetric positive-definite operator for the structured 7-point stencil
//! plus arbitrary extra couplings, and a preconditioned conjugate-gradient
//! solver with an incomplete-Cholesky IC(0) preconditioner.

use crate::error::{Error, Result};
use rayon::prelude::*;

/// `A x = diag·x − Σ g_face (x_neighbour) − Σ links`, all off-diagonals ≤ 0.
pub struct Operator<'a> {
    pub n: [usize; 3],
    pub ncells: usize,
    pub diag: Vec<f64>,
    /// Conductance to the +axis neighbour, per cell.
    pub g: [&'a [f64]; 3],
    /// Extra symmetric couplings (a, b, g).
    pub links: Vec<(usize, usize, f64)>,
}

const PAR_CHUNK: usize = 4096;

impl<'a> Operator<'a> {
    pub fn len(&self) -> usize {
        self.diag.len()
    }

    pub fn is_empty(&self) -> bool {
        self.diag.is_empty()
    }

    pub fn apply(&self, x: &[f64], y: &mut [f64]) {
        let [nx, ny, _] = self.n;
        let s = [1usize, nx, nx * ny];
        let nc = self.ncells;
        let g = self.g;
        let diag = &self.diag;
        y[..nc]
            .par_chunks_mut(PAR_CHUNK)
            .enumerate()
            .for_each(|(ci, chunk)| {
                let base = ci * PAR_CHUNK;
                for (o, yv) in chunk.iter_mut().enumerate() {
                    let c = base + o;
                    let mut v = diag[c] * x[c];
                    for a in 0..3 {
                        let gp = g[a][c];
                        if gp != 0.0 {
                            v -= gp * x[c + s[a]];
                        }
                        if c >= s[a] {
                            let gm = g[a][c - s[a]];
                            if gm != 0.0 {
                                v -= gm * x[c - s[a]];
                            }
                        }
                    }
                    *yv = v;
                }
            });
        for i in nc..self.len() {
            y[i] = diag[i] * x[i];
        }
        for &(a, b, gl) in &self.links {
            y[a] -= gl * x[b];
            y[b] -= gl * x[a];
        }
    }
}

/// Modified incomplete Cholesky MIC(0) factor of the structured part
/// (relaxation `omega`; 0 = plain IC(0)). Extra nodes get a Jacobi block.
pub struct Ic0 {
    d: Vec<f64>,
}

/// Relaxation of the modified incomplete Cholesky factorisation.
pub const MIC_OMEGA: f64 = 0.98;

impl Ic0 {
    pub fn new(op: &Operator) -> Ic0 {
        Self::with_omega(op, MIC_OMEGA)
    }

    pub fn with_omega(op: &Operator, omega: f64) -> Ic0 {
        let [nx, ny, _] = op.n;
        let s = [1usize, nx, nx * ny];
        let mut d = op.diag.clone();
        for c in 0..op.ncells {
            let mut v = op.diag[c];
            for a in 0..3 {
                if c >= s[a] {
                    let j = c - s[a];
                    let gm = op.g[a][j];
                    if gm != 0.0 {
                        // fill-in that IC(0) drops: couplings of j to its other upper neighbours
                        let mut other = 0.0;
                        for b in 0..3 {
                            if b != a {
                                other += op.g[b][j];
                            }
                        }
                        v -= (gm * gm + omega * gm * other) / d[j];
                    }
                }
            }
            // Guard against breakdown (cannot happen for an M-matrix, but cheap).
            d[c] = if v > 1e-14 * op.diag[c] {
                v
            } else {
                op.diag[c]
            };
        }
        Ic0 { d }
    }

    /// z = M⁻¹ r with M = (D+L) D⁻¹ (D+Lᵀ).
    pub fn apply(&self, op: &Operator, r: &[f64], z: &mut [f64]) {
        let [nx, ny, _] = op.n;
        let s = [1usize, nx, nx * ny];
        let nc = op.ncells;
        let d = &self.d;
        // forward
        for c in 0..nc {
            let mut v = r[c];
            for a in 0..3 {
                if c >= s[a] {
                    let gm = op.g[a][c - s[a]];
                    if gm != 0.0 {
                        v += gm * z[c - s[a]];
                    }
                }
            }
            z[c] = v / d[c];
        }
        // backward
        for c in (0..nc).rev() {
            let mut v = 0.0;
            for a in 0..3 {
                let gp = op.g[a][c];
                if gp != 0.0 {
                    v += gp * z[c + s[a]];
                }
            }
            z[c] += v / d[c];
        }
        for i in nc..op.len() {
            z[i] = r[i] / d[i];
        }
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.par_chunks(PAR_CHUNK)
        .zip(b.par_chunks(PAR_CHUNK))
        .map(|(x, y)| x.iter().zip(y).map(|(p, q)| p * q).sum::<f64>())
        .sum()
}

#[derive(Debug, Clone, Copy)]
pub struct SolveStats {
    pub iterations: usize,
    pub relative_residual: f64,
}

/// Solve `A x = b` in place (x holds the initial guess).
pub fn pcg(
    op: &Operator,
    b: &[f64],
    x: &mut [f64],
    tol: f64,
    max_iter: usize,
) -> Result<SolveStats> {
    pcg_monitored(op, b, x, tol, max_iter, &mut |_, _| true)
}

/// Like [`pcg`], calling `monitor(iteration, relative_residual)` every few
/// iterations; returning `false` aborts with [`Error::Cancelled`].
pub fn pcg_monitored(
    op: &Operator,
    b: &[f64],
    x: &mut [f64],
    tol: f64,
    max_iter: usize,
    monitor: &mut dyn FnMut(usize, f64) -> bool,
) -> Result<SolveStats> {
    let n = op.len();
    let pre = Ic0::new(op);
    let mut r = vec![0.0; n];
    op.apply(x, &mut r);
    r.par_iter_mut()
        .zip(b.par_iter())
        .for_each(|(ri, bi)| *ri = bi - *ri);
    let bnorm = dot(b, b).sqrt();
    // Scale for the relative residual: if b is zero, use the diagonal scale.
    let scale = if bnorm > 0.0 { bnorm } else { 1.0 };
    let mut rnorm = dot(&r, &r).sqrt();
    if rnorm / scale <= tol {
        return Ok(SolveStats {
            iterations: 0,
            relative_residual: rnorm / scale,
        });
    }
    let mut z = vec![0.0; n];
    pre.apply(op, &r, &mut z);
    let mut p = z.clone();
    let mut rz = dot(&r, &z);
    let mut q = vec![0.0; n];
    for it in 1..=max_iter {
        op.apply(&p, &mut q);
        let pq = dot(&p, &q);
        if pq <= 0.0 {
            return Err(Error::Invalid(
                "matrix is not positive definite (check that every region is connected to a heat sink)".into(),
            ));
        }
        let alpha = rz / pq;
        x.par_iter_mut()
            .zip(p.par_iter())
            .for_each(|(xi, pi)| *xi += alpha * pi);
        r.par_iter_mut()
            .zip(q.par_iter())
            .for_each(|(ri, qi)| *ri -= alpha * qi);
        rnorm = dot(&r, &r).sqrt();
        if rnorm / scale <= tol {
            return Ok(SolveStats {
                iterations: it,
                relative_residual: rnorm / scale,
            });
        }
        if it % 10 == 0 && !monitor(it, rnorm / scale) {
            return Err(Error::Cancelled);
        }
        pre.apply(op, &r, &mut z);
        let rz_new = dot(&r, &z);
        let beta = rz_new / rz;
        rz = rz_new;
        p.par_iter_mut()
            .zip(z.par_iter())
            .for_each(|(pi, zi)| *pi = zi + beta * *pi);
    }
    Err(Error::NoConvergence {
        residual: rnorm / scale,
        iterations: max_iter,
        tolerance: tol,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_1d_chain_with_extra_node() {
        // 5 cells in x, unit conductances, cell 0 tied to ground (g=1), extra node 5
        // linked to cell 4 with g=2 and a source of 1 W.
        let g0 = vec![1.0, 1.0, 1.0, 1.0, 0.0];
        let z = vec![0.0; 5];
        let mut diag = vec![0.0; 6];
        for c in 0..5 {
            diag[c] += g0[c];
            if c > 0 {
                diag[c] += g0[c - 1];
            }
        }
        diag[0] += 1.0;
        diag[4] += 2.0;
        diag[5] += 2.0;
        let op = Operator {
            n: [5, 1, 1],
            ncells: 5,
            diag,
            g: [&g0, &z, &z],
            links: vec![(5, 4, 2.0)],
        };
        let mut b = vec![0.0; 6];
        b[5] = 1.0;
        let mut x = vec![0.0; 6];
        pcg(&op, &b, &mut x, 1e-12, 100).unwrap();
        // series: ground-1-c0-1-c1-1-c2-1-c3-1-c4-0.5-junction
        let expect = [1.0, 2.0, 3.0, 4.0, 5.0, 5.5];
        for i in 0..6 {
            assert!((x[i] - expect[i]).abs() < 1e-9, "{i}: {}", x[i]);
        }
    }
}
