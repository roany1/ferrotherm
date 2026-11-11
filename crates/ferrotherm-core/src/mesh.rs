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
        Grid::conforming_merged(lo, hi, planes, max_cell, min_cells_per_interval, 0.0)
    }

    /// As [`Grid::conforming`], with the geometry planes closer than `merge`
    /// (m) merged into one (the first of a cluster is kept; the domain
    /// bounds always stay). Objects are painted by cell centre, so a block
    /// thinner than `merge` can end up a cell thinner or thicker.
    pub fn conforming_merged(
        lo: [f64; 3],
        hi: [f64; 3],
        planes: [Vec<f64>; 3],
        max_cell: [f64; 3],
        min_cells_per_interval: usize,
        merge: f64,
    ) -> Grid {
        Grid::conforming_limited(
            lo,
            hi,
            planes,
            max_cell,
            min_cells_per_interval,
            merge,
            Default::default(),
        )
    }

    /// As [`Grid::conforming_merged`], with local cell-size limits per axis:
    /// `(from, to, size)` (m) caps the cells whose centre lies in
    /// [from, to]. A block that needs at least n cells gives the limit
    /// (lo, hi, (hi − lo)/n): intervals already finer (from other geometry
    /// planes) are not split again.
    pub fn conforming_limited(
        lo: [f64; 3],
        hi: [f64; 3],
        planes: [Vec<f64>; 3],
        max_cell: [f64; 3],
        min_cells_per_interval: usize,
        merge: f64,
        limits: [Vec<(f64, f64, f64)>; 3],
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
            if merge > SNAP {
                let last = *p.last().unwrap();
                let mut kept: Vec<f64> = Vec::with_capacity(p.len());
                for v in p {
                    match kept.last() {
                        Some(k) if v - k < merge => {}
                        _ => kept.push(v),
                    }
                }
                // the upper domain bound replaces a kept plane too close to it
                if *kept.last().unwrap() != last {
                    if last - kept.last().unwrap() < merge && kept.len() > 1 {
                        kept.pop();
                    }
                    kept.push(last);
                }
                p = kept;
            }
            let mut out = vec![p[0]];
            for w in p.windows(2) {
                let len = w[1] - w[0];
                let mid = 0.5 * (w[0] + w[1]);
                let cap = limits[a]
                    .iter()
                    .filter(|(f, t, _)| mid >= *f && mid <= *t)
                    .fold(max_cell[a], |m, &(_, _, s)| m.min(s * (1.0 + 1e-9)));
                let n = ((len / cap).ceil() as usize).max(min_cells_per_interval.max(1));
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

    #[test]
    fn merged_planes() {
        // 0.30 and 0.302 merge (gap < 0.01); 0.995 merges into the bound 1.0
        let g = Grid::conforming_merged(
            [0.0; 3],
            [1.0, 1.0, 1.0],
            [vec![0.30, 0.302, 0.6, 0.995], vec![], vec![]],
            [1.0; 3],
            1,
            0.01,
        );
        assert_eq!(g.nodes[0], vec![0.0, 0.30, 0.6, 1.0]);
        let total: f64 = (0..g.ncells()).map(|c| g.volume(c)).sum();
        assert!((total - 1.0).abs() < 1e-12);
    }

    #[test]
    fn local_size_limit() {
        // block [0.2, 0.6] needs 4 cells: the coarse part [0.2, 0.5] is split
        // to 0.1 cells, the fine intervals of [0.5, 0.6] are left alone
        let g = Grid::conforming_limited(
            [0.0; 3],
            [1.0; 3],
            [vec![0.2, 0.5, 0.52, 0.54, 0.6], vec![], vec![]],
            [1.0; 3],
            1,
            0.0,
            [vec![(0.2, 0.6, 0.1)], vec![], vec![]],
        );
        let x: Vec<f64> = g.nodes[0].iter().map(|v| (v * 1e6).round() / 1e6).collect();
        assert_eq!(x, vec![0.0, 0.2, 0.3, 0.4, 0.5, 0.52, 0.54, 0.6, 1.0]);
    }
}
