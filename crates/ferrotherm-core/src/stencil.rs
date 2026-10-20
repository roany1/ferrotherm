// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! General 7-point finite-volume matrices on a structured block of unknowns
//! (cells or staggered faces), with optional extra unknowns coupled by
//! symmetric links, plus DILU-preconditioned BiCGSTAB (non-symmetric) and
//! conjugate gradient (symmetric) solvers.
//!
//! Row `c`:  `ap[c]·x[c] − Σ_a (pos[a][c]·x[c+s_a] + neg[a][c]·x[c−s_a]) − Σ links = b[c]`.

use crate::error::{Error, Result};
use rayon::prelude::*;

const CHUNK: usize = 4096;

#[derive(Debug, Clone)]
pub struct Stencil {
    /// Dimensions of the structured block.
    pub n: [usize; 3],
    /// Number of structured unknowns (n0·n1·n2).
    pub ns: usize,
    /// Diagonal of every unknown (structured, then extra).
    pub ap: Vec<f64>,
    /// Coupling to the +axis neighbour.
    pub pos: [Vec<f64>; 3],
    /// Coupling to the −axis neighbour.
    pub neg: [Vec<f64>; 3],
    /// Extra symmetric couplings (i, j, g): row i gets −g·x[j] and vice versa.
    pub links: Vec<(usize, usize, f64)>,
}

impl Stencil {
    pub fn new(n: [usize; 3], extra: usize) -> Stencil {
        let ns = n[0] * n[1] * n[2];
        Stencil {
            n,
            ns,
            ap: vec![0.0; ns + extra],
            pos: [vec![0.0; ns], vec![0.0; ns], vec![0.0; ns]],
            neg: [vec![0.0; ns], vec![0.0; ns], vec![0.0; ns]],
            links: Vec::new(),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.ap.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ap.is_empty()
    }

    #[inline]
    pub fn strides(&self) -> [usize; 3] {
        [1, self.n[0], self.n[0] * self.n[1]]
    }

    /// Make row `i` an identity row fixing `x[i] = value` (b must be set by the caller).
    pub fn fix(&mut self, i: usize) {
        self.ap[i] = 1.0;
        if i < self.ns {
            for a in 0..3 {
                self.pos[a][i] = 0.0;
                self.neg[a][i] = 0.0;
            }
        }
    }

    /// y = A x
    pub fn apply(&self, x: &[f64], y: &mut [f64]) {
        let s = self.strides();
        let ns = self.ns;
        let (pos, neg, ap) = (&self.pos, &self.neg, &self.ap);
        y[..ns]
            .par_chunks_mut(CHUNK)
            .enumerate()
            .for_each(|(ci, chunk)| {
                let base = ci * CHUNK;
                for (o, yv) in chunk.iter_mut().enumerate() {
                    let c = base + o;
                    let mut v = ap[c] * x[c];
                    for a in 0..3 {
                        let p = pos[a][c];
                        if p != 0.0 {
                            v -= p * x[c + s[a]];
                        }
                        let q = neg[a][c];
                        if q != 0.0 {
                            v -= q * x[c - s[a]];
                        }
                    }
                    *yv = v;
                }
            });
        for i in ns..self.len() {
            y[i] = ap[i] * x[i];
        }
        for &(i, j, g) in &self.links {
            y[i] -= g * x[j];
            y[j] -= g * x[i];
        }
    }

    /// Residual norm ‖b − A x‖₁.
    pub fn residual_l1(&self, x: &[f64], b: &[f64]) -> f64 {
        let mut y = vec![0.0; self.len()];
        self.apply(x, &mut y);
        y.iter().zip(b).map(|(a, b)| (b - a).abs()).sum()
    }
}

/// Diagonal ILU(0) of the structured part (extra unknowns: Jacobi).
pub struct Dilu {
    d: Vec<f64>,
}

impl Dilu {
    pub fn new(m: &Stencil) -> Dilu {
        let s = m.strides();
        let mut d = m.ap.clone();
        for c in 0..m.ns {
            let mut v = m.ap[c];
            for a in 0..3 {
                let q = m.neg[a][c];
                if q != 0.0 {
                    let j = c - s[a];
                    v -= q * m.pos[a][j] / d[j];
                }
            }
            d[c] = if v.abs() > 1e-12 * m.ap[c].abs() && v.is_finite() {
                v
            } else {
                m.ap[c]
            };
        }
        for v in d.iter_mut() {
            if *v == 0.0 {
                *v = 1.0;
            }
        }
        Dilu { d }
    }

    /// z = M⁻¹ r, M = (D + L) D⁻¹ (D + U).
    pub fn apply(&self, m: &Stencil, r: &[f64], z: &mut [f64]) {
        let s = m.strides();
        let d = &self.d;
        for c in 0..m.ns {
            let mut v = r[c];
            for a in 0..3 {
                let q = m.neg[a][c];
                if q != 0.0 {
                    v += q * z[c - s[a]];
                }
            }
            z[c] = v / d[c];
        }
        for c in (0..m.ns).rev() {
            let mut v = 0.0;
            for a in 0..3 {
                let p = m.pos[a][c];
                if p != 0.0 {
                    v += p * z[c + s[a]];
                }
            }
            z[c] += v / d[c];
        }
        for i in m.ns..m.len() {
            z[i] = r[i] / d[i];
        }
    }
}

/// Preconditioner choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pre {
    /// Diagonal ILU(0): cheap, for well-conditioned systems (momentum).
    Dilu,
    /// Additive-correction multigrid V-cycle: stiff systems (pressure, energy).
    Mg,
}

enum Precond {
    D(Dilu),
    M(crate::mg::Multigrid),
}

impl Precond {
    fn new(m: &Stencil, p: Pre) -> Precond {
        match p {
            Pre::Dilu => Precond::D(Dilu::new(m)),
            Pre::Mg => Precond::M(crate::mg::Multigrid::new(m)),
        }
    }
    fn apply(&self, m: &Stencil, r: &[f64], z: &mut [f64]) {
        match self {
            Precond::D(d) => d.apply(m, r, z),
            Precond::M(g) => g.apply(m, r, z),
        }
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.par_chunks(CHUNK)
        .zip(b.par_chunks(CHUNK))
        .map(|(x, y)| x.iter().zip(y).map(|(p, q)| p * q).sum::<f64>())
        .sum()
}

fn norm(a: &[f64]) -> f64 {
    dot(a, a).sqrt()
}

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub iterations: usize,
    /// Final ‖r‖/‖r0‖ (relative to the initial residual).
    pub reduction: f64,
    pub converged: bool,
}

/// BiCGSTAB with DILU preconditioning. Stops when ‖r‖ ≤ rtol·‖r0‖ or
/// ‖r‖ ≤ atol. Non-convergence is reported in the stats, not as an error
/// (outer iterations decide).
pub fn bicgstab(m: &Stencil, b: &[f64], x: &mut [f64], rtol: f64, atol: f64, max_iter: usize) -> Stats {
    bicgstab_with(m, b, x, rtol, atol, max_iter, Pre::Dilu)
}

pub fn bicgstab_with(
    m: &Stencil,
    b: &[f64],
    x: &mut [f64],
    rtol: f64,
    atol: f64,
    max_iter: usize,
    pre: Pre,
) -> Stats {
    let n = m.len();
    let pre = Precond::new(m, pre);
    let mut r = vec![0.0; n];
    m.apply(x, &mut r);
    for i in 0..n {
        r[i] = b[i] - r[i];
    }
    let r0n = norm(&r);
    if r0n <= atol || r0n == 0.0 {
        return Stats {
            iterations: 0,
            reduction: 0.0,
            converged: true,
        };
    }
    let rhat = r.clone();
    let (mut rho, mut alpha, mut omega) = (1.0f64, 1.0f64, 1.0f64);
    let mut v = vec![0.0; n];
    let mut p = vec![0.0; n];
    let mut y = vec![0.0; n];
    let mut sv = vec![0.0; n];
    let mut z = vec![0.0; n];
    let mut t = vec![0.0; n];
    let mut rn = r0n;
    for it in 1..=max_iter {
        let rho_new = dot(&rhat, &r);
        if rho_new.abs() < 1e-300 {
            break;
        }
        let beta = (rho_new / rho) * (alpha / omega);
        rho = rho_new;
        p.par_iter_mut()
            .zip(r.par_iter().zip(v.par_iter()))
            .for_each(|(pi, (ri, vi))| *pi = ri + beta * (*pi - omega * vi));
        pre.apply(m, &p, &mut y);
        m.apply(&y, &mut v);
        let rv = dot(&rhat, &v);
        if rv.abs() < 1e-300 {
            break;
        }
        alpha = rho / rv;
        sv.par_iter_mut()
            .zip(r.par_iter().zip(v.par_iter()))
            .for_each(|(si, (ri, vi))| *si = ri - alpha * vi);
        let sn = norm(&sv);
        if sn <= rtol * r0n || sn <= atol {
            x.par_iter_mut()
                .zip(y.par_iter())
                .for_each(|(xi, yi)| *xi += alpha * yi);
            return Stats {
                iterations: it,
                reduction: sn / r0n,
                converged: true,
            };
        }
        pre.apply(m, &sv, &mut z);
        m.apply(&z, &mut t);
        let tt = dot(&t, &t);
        omega = if tt > 0.0 { dot(&t, &sv) / tt } else { 0.0 };
        x.par_iter_mut()
            .zip(y.par_iter().zip(z.par_iter()))
            .for_each(|(xi, (yi, zi))| *xi += alpha * yi + omega * zi);
        r.par_iter_mut()
            .zip(sv.par_iter().zip(t.par_iter()))
            .for_each(|(ri, (si, ti))| *ri = si - omega * ti);
        rn = norm(&r);
        if rn <= rtol * r0n || rn <= atol {
            return Stats {
                iterations: it,
                reduction: rn / r0n,
                converged: true,
            };
        }
        if omega == 0.0 {
            break;
        }
    }
    Stats {
        iterations: max_iter,
        reduction: rn / r0n,
        converged: false,
    }
}

/// Preconditioned conjugate gradient for symmetric positive (semi-)definite
/// stencils (pressure correction, wall distance).
pub fn pcg(m: &Stencil, b: &[f64], x: &mut [f64], rtol: f64, atol: f64, max_iter: usize) -> Result<Stats> {
    pcg_with(m, b, x, rtol, atol, max_iter, Pre::Dilu)
}

pub fn pcg_with(
    m: &Stencil,
    b: &[f64],
    x: &mut [f64],
    rtol: f64,
    atol: f64,
    max_iter: usize,
    pre: Pre,
) -> Result<Stats> {
    let n = m.len();
    let pre = Precond::new(m, pre);
    let mut r = vec![0.0; n];
    m.apply(x, &mut r);
    for i in 0..n {
        r[i] = b[i] - r[i];
    }
    let r0n = norm(&r);
    if r0n <= atol || r0n == 0.0 {
        return Ok(Stats {
            iterations: 0,
            reduction: 0.0,
            converged: true,
        });
    }
    let mut z = vec![0.0; n];
    pre.apply(m, &r, &mut z);
    let mut p = z.clone();
    let mut rz = dot(&r, &z);
    let mut q = vec![0.0; n];
    let mut rn = r0n;
    for it in 1..=max_iter {
        m.apply(&p, &mut q);
        let pq = dot(&p, &q);
        if pq <= 0.0 {
            if rn <= 1e-3 * r0n {
                break;
            }
            return Err(Error::Invalid(
                "pressure/wall-distance matrix is not positive definite".into(),
            ));
        }
        let alpha = rz / pq;
        x.par_iter_mut()
            .zip(p.par_iter())
            .for_each(|(xi, pi)| *xi += alpha * pi);
        r.par_iter_mut()
            .zip(q.par_iter())
            .for_each(|(ri, qi)| *ri -= alpha * qi);
        rn = norm(&r);
        if rn <= rtol * r0n || rn <= atol {
            return Ok(Stats {
                iterations: it,
                reduction: rn / r0n,
                converged: true,
            });
        }
        pre.apply(m, &r, &mut z);
        let rz_new = dot(&r, &z);
        let beta = rz_new / rz;
        rz = rz_new;
        p.par_iter_mut()
            .zip(z.par_iter())
            .for_each(|(pi, zi)| *pi = zi + beta * *pi);
    }
    Ok(Stats {
        iterations: max_iter,
        reduction: rn / r0n,
        converged: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1D convection–diffusion with upwinding: compare with a direct solve.
    #[test]
    fn bicgstab_solves_nonsymmetric_chain() {
        let n = 50;
        let mut m = Stencil::new([n, 1, 1], 0);
        let (d, f) = (1.0, 3.0); // diffusion and flow to +x
        let mut b = vec![0.0; n];
        for c in 0..n {
            if c + 1 < n {
                m.pos[0][c] = d; // downstream neighbour
            }
            if c > 0 {
                m.neg[0][c] = d + f; // upstream neighbour
            }
            m.ap[c] = m.pos[0][c] + m.neg[0][c];
        }
        // x(0) boundary = 1 via the upstream link, outflow at the end
        m.ap[0] += d + f;
        b[0] = (d + f) * 1.0;
        m.ap[n - 1] += 0.5; // small loss so the problem is well posed
        let mut x = vec![0.0; n];
        let st = bicgstab(&m, &b, &mut x, 1e-12, 0.0, 500);
        assert!(st.converged);
        let mut y = vec![0.0; n];
        m.apply(&x, &mut y);
        let err: f64 = y.iter().zip(&b).map(|(a, b)| (a - b).abs()).sum();
        assert!(err < 1e-9, "{err}");
    }

    #[test]
    fn pcg_symmetric_with_link() {
        let mut m = Stencil::new([4, 1, 1], 1);
        for c in 0..4 {
            if c + 1 < 4 {
                m.pos[0][c] = 1.0;
                m.neg[0][c + 1] = 1.0;
            }
        }
        for c in 0..4 {
            m.ap[c] = m.pos[0][c] + m.neg[0][c];
        }
        m.ap[0] += 1.0; // ground
        m.links.push((4, 3, 2.0));
        m.ap[3] += 2.0;
        m.ap[4] = 2.0;
        let mut b = vec![0.0; 5];
        b[4] = 1.0;
        let mut x = vec![0.0; 5];
        pcg(&m, &b, &mut x, 1e-12, 0.0, 100).unwrap();
        let expect = [1.0, 2.0, 3.0, 4.0, 4.5];
        for i in 0..5 {
            assert!((x[i] - expect[i]).abs() < 1e-9);
        }
    }
}

#[cfg(test)]
mod mg_tests {
    use super::*;

    /// 3-D Poisson with Dirichlet sides: MG-PCG needs far fewer iterations.
    #[test]
    fn multigrid_preconditioner_reduces_iterations() {
        let n = [40, 40, 40];
        let mut m = Stencil::new(n, 0);
        let s = m.strides();
        for k in 0..n[2] {
            for j in 0..n[1] {
                for i in 0..n[0] {
                    let c = i + n[0] * (j + n[1] * k);
                    let p = [i, j, k];
                    for a in 0..3 {
                        // strong anisotropy/contrast in one region, like metal vs air
                        let kk = if i < 20 { 1000.0 } else { 1.0 };
                        if p[a] + 1 < n[a] {
                            m.pos[a][c] = kk;
                            m.neg[a][c + s[a]] = kk;
                        }
                    }
                }
            }
        }
        for c in 0..m.ns {
            let mut d = 0.0;
            for a in 0..3 {
                d += m.pos[a][c] + m.neg[a][c];
            }
            m.ap[c] = d + if c % n[0] == n[0] - 1 { 1.0 } else { 0.0 };
        }
        let b = vec![1.0; m.ns];
        let mut x1 = vec![0.0; m.ns];
        let t0 = std::time::Instant::now();
        let a = pcg_with(&m, &b, &mut x1, 1e-8, 0.0, 5000, Pre::Dilu).unwrap();
        let ta = t0.elapsed();
        let mut x2 = vec![0.0; m.ns];
        let t0 = std::time::Instant::now();
        let g = pcg_with(&m, &b, &mut x2, 1e-8, 0.0, 5000, Pre::Mg).unwrap();
        let tg = t0.elapsed();
        eprintln!("DILU {} it {:?}, MG {} it {:?}", a.iterations, ta, g.iterations, tg);
        assert!(a.converged && g.converged);
        assert!(g.iterations * 3 < a.iterations);
        let err = x1.iter().zip(&x2).map(|(p, q)| (p - q).abs()).fold(0.0, f64::max);
        let scale = x1.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        assert!(err < 1e-5 * scale);
    }
}
