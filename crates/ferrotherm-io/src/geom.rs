// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! 2-D geometry helpers and a coverage rasteriser for board copper.

pub type P2 = [f64; 2];

/// Polygon outline (closed implicitly), mm.
pub type Poly = Vec<P2>;

/// Approximate an arc from `start` around `center` by `sweep_deg` (CCW positive).
pub fn arc_points(center: P2, start: P2, sweep_deg: f64, out: &mut Vec<P2>) {
    let r = ((start[0] - center[0]).powi(2) + (start[1] - center[1]).powi(2)).sqrt();
    let a0 = (start[1] - center[1]).atan2(start[0] - center[0]);
    let sweep = sweep_deg.to_radians();
    let n = ((sweep.abs() / (2.0 * std::f64::consts::PI)) * 72.0).ceil().max(2.0) as usize;
    for k in 1..=n {
        let a = a0 + sweep * k as f64 / n as f64;
        out.push([center[0] + r * a.cos(), center[1] + r * a.sin()]);
    }
}

/// Arc through three points (start, mid, end) as a polyline (start excluded).
pub fn arc3_points(s: P2, m: P2, e: P2, out: &mut Vec<P2>) {
    // circumcentre
    let d = 2.0 * (s[0] * (m[1] - e[1]) + m[0] * (e[1] - s[1]) + e[0] * (s[1] - m[1]));
    if d.abs() < 1e-12 {
        out.push(m);
        out.push(e);
        return;
    }
    let sq = |p: P2| p[0] * p[0] + p[1] * p[1];
    let ux = (sq(s) * (m[1] - e[1]) + sq(m) * (e[1] - s[1]) + sq(e) * (s[1] - m[1])) / d;
    let uy = (sq(s) * (e[0] - m[0]) + sq(m) * (s[0] - e[0]) + sq(e) * (m[0] - s[0])) / d;
    let c = [ux, uy];
    let ang = |p: P2| (p[1] - c[1]).atan2(p[0] - c[0]);
    let (a0, am, a1) = (ang(s), ang(m), ang(e));
    let norm = |a: f64| {
        let t = std::f64::consts::TAU;
        ((a % t) + t) % t
    };
    // CCW sweep from s to e passing through m?
    let ccw = norm(am - a0) < norm(a1 - a0);
    let sweep = if ccw { norm(a1 - a0) } else { -norm(a0 - a1) };
    arc_points(c, s, sweep.to_degrees(), out);
}

pub fn bbox(points: &[P2]) -> Option<(P2, P2)> {
    let mut it = points.iter();
    let f = it.next()?;
    let mut lo = *f;
    let mut hi = *f;
    for p in it {
        lo[0] = lo[0].min(p[0]);
        lo[1] = lo[1].min(p[1]);
        hi[0] = hi[0].max(p[0]);
        hi[1] = hi[1].max(p[1]);
    }
    Some((lo, hi))
}

pub fn poly_area(p: &[P2]) -> f64 {
    let mut a = 0.0;
    for i in 0..p.len() {
        let j = (i + 1) % p.len();
        a += p[i][0] * p[j][1] - p[j][0] * p[i][1];
    }
    0.5 * a
}

pub fn rotate(p: P2, deg: f64) -> P2 {
    let (s, c) = deg.to_radians().sin_cos();
    [p[0] * c - p[1] * s, p[0] * s + p[1] * c]
}

/// Bit raster over a rectangle, used for copper coverage.
#[derive(Clone)]
pub struct Raster {
    pub lo: P2,
    pub res: f64,
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Raster {
    pub fn new(lo: P2, hi: P2, res: f64) -> Raster {
        let w = (((hi[0] - lo[0]) / res).ceil() as usize).max(1);
        let h = (((hi[1] - lo[1]) / res).ceil() as usize).max(1);
        Raster {
            lo,
            res,
            w,
            h,
            px: vec![0; w * h],
        }
    }

    #[inline]
    fn set(&mut self, i: usize, j: usize, v: bool) {
        self.px[i + self.w * j] = v as u8;
    }

    /// Fill a polygon (even–odd rule) with value `v` (true = copper, false = clear).
    pub fn fill_poly(&mut self, poly: &[P2], v: bool) {
        self.fill_polys(&[poly], v);
    }

    /// Fill several contours together with the even–odd rule (holes allowed).
    pub fn fill_polys(&mut self, polys: &[&[P2]], v: bool) {
        let mut ymin = f64::INFINITY;
        let mut ymax = f64::NEG_INFINITY;
        for p in polys {
            for q in p.iter() {
                ymin = ymin.min(q[1]);
                ymax = ymax.max(q[1]);
            }
        }
        if !ymin.is_finite() {
            return;
        }
        let j0 = (((ymin - self.lo[1]) / self.res).floor().max(0.0)) as usize;
        let j1 = ((((ymax - self.lo[1]) / self.res).ceil()) as isize).clamp(0, self.h as isize) as usize;
        let mut xs: Vec<f64> = Vec::new();
        for j in j0..j1 {
            let y = self.lo[1] + (j as f64 + 0.5) * self.res;
            xs.clear();
            for p in polys {
                let n = p.len();
                for k in 0..n {
                    let a = p[k];
                    let b = p[(k + 1) % n];
                    if (a[1] <= y && b[1] > y) || (b[1] <= y && a[1] > y) {
                        xs.push(a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
                    }
                }
            }
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            for pair in xs.chunks(2) {
                if pair.len() < 2 {
                    break;
                }
                let i0 = ((pair[0] - self.lo[0]) / self.res - 0.5).ceil().max(0.0) as usize;
                let i1 = (((pair[1] - self.lo[0]) / self.res - 0.5).floor() as isize).min(self.w as isize - 1);
                if i1 < 0 {
                    continue;
                }
                for i in i0..=(i1 as usize) {
                    if i < self.w {
                        self.set(i, j, v);
                    }
                }
            }
        }
    }

    /// Filled disc.
    pub fn disc(&mut self, c: P2, r: f64, v: bool) {
        let i0 = (((c[0] - r - self.lo[0]) / self.res).floor().max(0.0)) as usize;
        let i1 = ((((c[0] + r - self.lo[0]) / self.res).ceil()) as isize).clamp(0, self.w as isize) as usize;
        let j0 = (((c[1] - r - self.lo[1]) / self.res).floor().max(0.0)) as usize;
        let j1 = ((((c[1] + r - self.lo[1]) / self.res).ceil()) as isize).clamp(0, self.h as isize) as usize;
        let r2 = r * r;
        for j in j0..j1 {
            let y = self.lo[1] + (j as f64 + 0.5) * self.res - c[1];
            for i in i0..i1 {
                let x = self.lo[0] + (i as f64 + 0.5) * self.res - c[0];
                if x * x + y * y <= r2 {
                    self.set(i, j, v);
                }
            }
        }
    }

    /// Thick line with round caps (a Gerber draw with a circular aperture).
    pub fn segment(&mut self, a: P2, b: P2, width: f64, v: bool) {
        let r = 0.5 * width;
        let (lo, hi) = bbox(&[a, b]).unwrap();
        let i0 = (((lo[0] - r - self.lo[0]) / self.res).floor().max(0.0)) as usize;
        let i1 = ((((hi[0] + r - self.lo[0]) / self.res).ceil()) as isize).clamp(0, self.w as isize) as usize;
        let j0 = (((lo[1] - r - self.lo[1]) / self.res).floor().max(0.0)) as usize;
        let j1 = ((((hi[1] + r - self.lo[1]) / self.res).ceil()) as isize).clamp(0, self.h as isize) as usize;
        let d = [b[0] - a[0], b[1] - a[1]];
        let l2 = d[0] * d[0] + d[1] * d[1];
        let r2 = r * r;
        for j in j0..j1 {
            let y = self.lo[1] + (j as f64 + 0.5) * self.res;
            for i in i0..i1 {
                let x = self.lo[0] + (i as f64 + 0.5) * self.res;
                let t = if l2 > 0.0 {
                    (((x - a[0]) * d[0] + (y - a[1]) * d[1]) / l2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let px = a[0] + t * d[0] - x;
                let py = a[1] + t * d[1] - y;
                if px * px + py * py <= r2 {
                    self.set(i, j, v);
                }
            }
        }
    }

    /// Rotated rectangle centred at `c` (w × h, rotation in degrees).
    pub fn rect(&mut self, c: P2, w: f64, h: f64, rot_deg: f64, v: bool) {
        let pts: Vec<P2> = [[-0.5 * w, -0.5 * h], [0.5 * w, -0.5 * h], [0.5 * w, 0.5 * h], [-0.5 * w, 0.5 * h]]
            .iter()
            .map(|p| {
                let q = rotate(*p, rot_deg);
                [q[0] + c[0], q[1] + c[1]]
            })
            .collect();
        self.fill_poly(&pts, v);
    }

    /// Obround (stadium) w × h centred at `c`.
    pub fn obround(&mut self, c: P2, w: f64, h: f64, rot_deg: f64, v: bool) {
        if (w - h).abs() < 1e-12 {
            self.disc(c, 0.5 * w, v);
            return;
        }
        let (len, dia) = if w > h { (w - h, h) } else { (h - w, w) };
        let dir = if w > h { [1.0, 0.0] } else { [0.0, 1.0] };
        let d = rotate([dir[0] * 0.5 * len, dir[1] * 0.5 * len], rot_deg);
        self.segment([c[0] - d[0], c[1] - d[1]], [c[0] + d[0], c[1] + d[1]], dia, v);
    }

    /// Fraction of set pixels in a rectangle (board coordinates).
    pub fn coverage(&self, lo: P2, hi: P2) -> f64 {
        let i0 = (((lo[0] - self.lo[0]) / self.res).floor().max(0.0)) as usize;
        let i1 = ((((hi[0] - self.lo[0]) / self.res).ceil()) as isize).clamp(0, self.w as isize) as usize;
        let j0 = (((lo[1] - self.lo[1]) / self.res).floor().max(0.0)) as usize;
        let j1 = ((((hi[1] - self.lo[1]) / self.res).ceil()) as isize).clamp(0, self.h as isize) as usize;
        let mut set = 0usize;
        let mut tot = 0usize;
        for j in j0..j1 {
            for i in i0..i1 {
                tot += 1;
                set += self.px[i + self.w * j] as usize;
            }
        }
        if tot == 0 {
            0.0
        } else {
            set as f64 / tot as f64
        }
    }

    pub fn total_coverage(&self) -> f64 {
        self.px.iter().map(|&v| v as usize).sum::<usize>() as f64 / self.px.len().max(1) as f64
    }

    /// Logical AND with another raster of the same frame (e.g. copper ∩ outline).
    pub fn and(&mut self, o: &Raster) {
        for (a, b) in self.px.iter_mut().zip(&o.px) {
            *a &= *b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_areas() {
        let mut r = Raster::new([0.0, 0.0], [10.0, 10.0], 0.02);
        r.disc([5.0, 5.0], 2.0, true);
        let a = r.total_coverage() * 100.0;
        assert!((a - std::f64::consts::PI * 4.0).abs() < 0.05, "{a}");
        let mut r = Raster::new([0.0, 0.0], [10.0, 10.0], 0.02);
        r.segment([2.0, 5.0], [8.0, 5.0], 1.0, true);
        let a = r.total_coverage() * 100.0;
        let exact = 6.0 + std::f64::consts::PI * 0.25;
        assert!((a - exact).abs() < 0.05, "{a} {exact}");
        // polygon with a hole (even-odd)
        let mut r = Raster::new([0.0, 0.0], [10.0, 10.0], 0.02);
        let outer = vec![[1.0, 1.0], [9.0, 1.0], [9.0, 9.0], [1.0, 9.0]];
        let hole = vec![[3.0, 3.0], [7.0, 3.0], [7.0, 7.0], [3.0, 7.0]];
        r.fill_polys(&[&outer, &hole], true);
        let a = r.total_coverage() * 100.0;
        assert!((a - 48.0).abs() < 0.05, "{a}");
    }

    #[test]
    fn three_point_arc() {
        let mut v = Vec::new();
        arc3_points([1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], &mut v);
        let last = v.last().unwrap();
        assert!((last[0] + 1.0).abs() < 1e-9 && last[1].abs() < 1e-9);
        assert!(v.iter().all(|p| p[1] >= -1e-9)); // upper half
    }
}
