// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Non-uniform rectilinear (Cartesian) grid conforming to every geometry plane.

/// Rectilinear grid. Coordinates in metres. Cell index `c = i + nx*(j + ny*k)`.
#[derive(Debug, Clone)]
pub struct Grid {
    /// Node (face) coordinates per axis, length n_axis + 1.
    pub nodes: [Vec<f64>; 3],
}

const SNAP: f64 = 1e-9; // 1 nm: planes closer than this are merged

impl Grid {
    /// Build a grid whose node planes include every coordinate in `planes`
    /// (per axis), then subdivide each interval so no cell exceeds `max_cell`.
    pub fn conforming(
        lo: [f64; 3],
        hi: [f64; 3],
        planes: [Vec<f64>; 3],
        max_cell: [f64; 3],
        min_cells_per_interval: usize,
    ) -> Grid {
        let mut nodes: [Vec<f64>; 3] = Default::default();
        for a in 0..3 {
            let mut p: Vec<f64> = planes[a]
                .iter()
                .copied()
                .filter(|v| *v > lo[a] + SNAP && *v < hi[a] - SNAP)
                .collect();
            p.push(lo[a]);
            p.push(hi[a]);
            p.sort_by(|x, y| x.partial_cmp(y).unwrap());
            p.dedup_by(|x, y| (*x - *y).abs() < SNAP);
            let mut out = vec![p[0]];
            for w in p.windows(2) {
                let len = w[1] - w[0];
                let n = ((len / max_cell[a]).ceil() as usize).max(min_cells_per_interval.max(1));
                for s in 1..=n {
                    out.push(w[0] + len * s as f64 / n as f64);
                }
            }
            *out.last_mut().unwrap() = p[p.len() - 1];
            nodes[a] = out;
        }
        Grid { nodes }
    }

    #[inline]
    pub fn n(&self) -> [usize; 3] {
        [
            self.nodes[0].len() - 1,
            self.nodes[1].len() - 1,
            self.nodes[2].len() - 1,
        ]
    }
    #[inline]
    pub fn ncells(&self) -> usize {
        let n = self.n();
        n[0] * n[1] * n[2]
    }
    #[inline]
    pub fn idx(&self, i: usize, j: usize, k: usize) -> usize {
        let n = self.n();
        i + n[0] * (j + n[1] * k)
    }
    #[inline]
    pub fn ijk(&self, c: usize) -> [usize; 3] {
        let n = self.n();
        [c % n[0], (c / n[0]) % n[1], c / (n[0] * n[1])]
    }
    #[inline]
    pub fn width(&self, axis: usize, i: usize) -> f64 {
        self.nodes[axis][i + 1] - self.nodes[axis][i]
    }
    #[inline]
    pub fn center(&self, axis: usize, i: usize) -> f64 {
        0.5 * (self.nodes[axis][i + 1] + self.nodes[axis][i])
    }
    pub fn volume(&self, c: usize) -> f64 {
        let [i, j, k] = self.ijk(c);
        self.width(0, i) * self.width(1, j) * self.width(2, k)
    }
    /// Area of the cell face normal to `axis`.
    pub fn face_area(&self, c: usize, axis: usize) -> f64 {
        let p = self.ijk(c);
        let [t0, t1] = match axis {
            0 => [1, 2],
            1 => [0, 2],
            _ => [0, 1],
        };
        self.width(t0, p[t0]) * self.width(t1, p[t1])
    }

    /// Cell index range `[a, b)` along `axis` whose centres lie in `[lo, hi]`.
    pub fn range(&self, axis: usize, lo: f64, hi: f64) -> (usize, usize) {
        let n = self.n()[axis];
        let mut a = n;
        let mut b = 0;
        for i in 0..n {
            let c = self.center(axis, i);
            if c > lo && c < hi {
                a = a.min(i);
                b = b.max(i + 1);
            }
        }
        if a >= b {
            (0, 0)
        } else {
            (a, b)
        }
    }

    /// Index of the node plane closest to `x` along `axis` (exact for conforming planes).
    pub fn plane_index(&self, axis: usize, x: f64) -> usize {
        let v = &self.nodes[axis];
        let mut best = 0;
        for (i, p) in v.iter().enumerate() {
            if (p - x).abs() < (v[best] - x).abs() {
                best = i;
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conforming_contains_planes_and_respects_max() {
        let g = Grid::conforming(
            [0.0; 3],
            [1.0, 1.0, 1.0],
            [vec![0.33], vec![], vec![0.1, 0.1 + 1e-12]],
            [0.2; 3],
            1,
        );
        assert!(g.nodes[0].iter().any(|v| (v - 0.33).abs() < 1e-12));
        assert!(g.nodes[2].iter().any(|v| (v - 0.1).abs() < 1e-12));
        for a in 0..3 {
            for w in g.nodes[a].windows(2) {
                assert!(w[1] - w[0] <= 0.2 + 1e-12);
                assert!(w[1] > w[0]);
            }
        }
        let total: f64 = (0..g.ncells()).map(|c| g.volume(c)).sum();
        assert!((total - 1.0).abs() < 1e-12);
    }
}
