// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Small CPU rasteriser with a depth buffer for the opaque part of the 3D view.
//!
//! Orthographic projection makes depth linear in screen space, so it is
//! interpolated exactly. Besides colour it keeps a per-pixel face id (exact
//! picking) and draws outlines where the "group" id changes between
//! neighbouring pixels (object edges and silhouettes). When the camera is at
//! rest the image is rendered at 2× and box-filtered down (anti-aliasing).

use eframe::egui::{Color32, ColorImage};

/// Screen-space input triangle data for one quad.
pub struct Prim {
    /// Pixel coordinates (x, y) and depth (larger = farther) of the 4 corners.
    pub p: [[f32; 3]; 4],
    pub color: Color32,
    /// Picking id (quad index + 1; 0 = background).
    pub id: u32,
    /// Outline group: edges are drawn between different groups.
    pub group: u32,
}

#[derive(Default)]
pub struct Raster {
    pub w: usize,
    pub h: usize,
    color: Vec<Color32>,
    depth: Vec<f32>,
    id: Vec<u32>,
    group: Vec<u32>,
    /// Supersampling factor of the last render.
    pub ss: usize,
}

#[inline]
fn edge(a: [f32; 3], b: [f32; 3], x: f32, y: f32) -> f32 {
    (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0])
}

impl Raster {
    pub fn clear(&mut self, w: usize, h: usize, ss: usize, bg: Color32) {
        self.w = w;
        self.h = h;
        self.ss = ss;
        let n = w * h;
        self.color.clear();
        self.color.resize(n, bg);
        self.depth.clear();
        self.depth.resize(n, f32::INFINITY);
        self.id.clear();
        self.id.resize(n, 0);
        self.group.clear();
        self.group.resize(n, 0);
    }

    fn tri(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3], color: Color32, id: u32, group: u32) {
        let area = edge(a, b, c[0], c[1]);
        if area.abs() < 1e-9 {
            return;
        }
        let minx = a[0].min(b[0]).min(c[0]).floor().max(0.0) as usize;
        let maxx = (a[0].max(b[0]).max(c[0]).ceil() as isize).min(self.w as isize - 1);
        let miny = a[1].min(b[1]).min(c[1]).floor().max(0.0) as usize;
        let maxy = (a[1].max(b[1]).max(c[1]).ceil() as isize).min(self.h as isize - 1);
        if maxx < 0 || maxy < 0 {
            return;
        }
        let (maxx, maxy) = (maxx as usize, maxy as usize);
        let inv = 1.0 / area;
        for y in miny..=maxy {
            let py = y as f32 + 0.5;
            let row = y * self.w;
            for x in minx..=maxx {
                let px = x as f32 + 0.5;
                let w0 = edge(b, c, px, py) * inv;
                let w1 = edge(c, a, px, py) * inv;
                let w2 = edge(a, b, px, py) * inv;
                // small tolerance closes hairline cracks between adjacent quads
                if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                    continue;
                }
                let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
                let i = row + x;
                if z < self.depth[i] {
                    self.depth[i] = z;
                    self.color[i] = color;
                    self.id[i] = id;
                    self.group[i] = group;
                }
            }
        }
    }

    /// Depth-tested line (pixel coords + depth), drawn slightly in front so
    /// edges lying on faces stay visible.
    pub fn line(&mut self, a: [f32; 3], b: [f32; 3], color: Color32, width: usize) {
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let n = dx.abs().max(dy.abs()).ceil().max(1.0) as usize;
        let bias = 1e-3 * (1.0 + a[2].abs().max(b[2].abs()));
        let r = (width.max(1) as isize - 1) / 2;
        for k in 0..=n {
            let t = k as f32 / n as f32;
            let x = (a[0] + dx * t) as isize;
            let y = (a[1] + dy * t) as isize;
            let z = a[2] + (b[2] - a[2]) * t - bias;
            for oy in -r..=r + (width as isize + 1) % 2 - 1 {
                for ox in -r..=r + (width as isize + 1) % 2 - 1 {
                    let (xx, yy) = (x + ox, y + oy);
                    if xx < 0 || yy < 0 || xx as usize >= self.w || yy as usize >= self.h {
                        continue;
                    }
                    let i = yy as usize * self.w + xx as usize;
                    if z <= self.depth[i] {
                        self.color[i] = color;
                    }
                }
            }
        }
    }

    pub fn draw(&mut self, prims: &[Prim]) {
        for q in prims {
            self.tri(q.p[0], q.p[1], q.p[2], q.color, q.id, q.group);
            self.tri(q.p[0], q.p[2], q.p[3], q.color, q.id, q.group);
        }
    }

    /// Darken pixels on group boundaries; `thickness` in buffer pixels.
    pub fn outlines(&mut self, strength: f32, thickness: usize) {
        let (w, h) = (self.w, self.h);
        let k = 1.0 - strength;
        let mut mark = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let g = self.group[i];
                let right = x + 1 < w && self.group[i + 1] != g;
                let down = y + 1 < h && self.group[i + w] != g;
                if right || down {
                    // darken the nearer side so silhouettes sit on the object
                    if g != 0 {
                        mark[i] = true;
                    }
                    if right && self.group[i + 1] != 0 && self.depth[i + 1] < self.depth[i] {
                        mark[i + 1] = true;
                    }
                    if down && self.group[i + w] != 0 && self.depth[i + w] < self.depth[i] {
                        mark[i + w] = true;
                    }
                }
            }
        }
        for _ in 1..thickness.max(1) {
            let prev = mark.clone();
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    if prev[i] {
                        continue;
                    }
                    let g = self.group[i];
                    let near = (x > 0 && prev[i - 1] && self.group[i - 1] == g)
                        || (y > 0 && prev[i - w] && self.group[i - w] == g);
                    if near && g != 0 {
                        mark[i] = true;
                    }
                }
            }
        }
        for (c, m) in self.color.iter_mut().zip(mark) {
            if m {
                *c = Color32::from_rgb(
                    (c.r() as f32 * k) as u8,
                    (c.g() as f32 * k) as u8,
                    (c.b() as f32 * k) as u8,
                );
            }
        }
    }

    /// Final image, box-filtered by the supersampling factor.
    pub fn image(&self) -> ColorImage {
        let s = self.ss.max(1);
        let (ow, oh) = (self.w / s, self.h / s);
        if s == 1 {
            return ColorImage::new([ow, oh], self.color.clone());
        }
        let mut px = Vec::with_capacity(ow * oh);
        let n = (s * s) as u32;
        for y in 0..oh {
            for x in 0..ow {
                let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
                for dy in 0..s {
                    let row = (y * s + dy) * self.w + x * s;
                    for dx in 0..s {
                        let c = self.color[row + dx];
                        r += c.r() as u32;
                        g += c.g() as u32;
                        b += c.b() as u32;
                    }
                }
                px.push(Color32::from_rgb(
                    (r / n) as u8,
                    (g / n) as u8,
                    (b / n) as u8,
                ));
            }
        }
        ColorImage::new([ow, oh], px)
    }

    /// Face id at a position in output pixels (0 = background).
    pub fn id_at(&self, x: f32, y: f32) -> u32 {
        let s = self.ss.max(1) as f32;
        let (xi, yi) = ((x * s) as isize, (y * s) as isize);
        if xi < 0 || yi < 0 || xi as usize >= self.w || yi as usize >= self.h {
            return 0;
        }
        self.id[yi as usize * self.w + xi as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f32, y0: f32, x1: f32, y1: f32, z: f32, id: u32) -> Prim {
        Prim {
            p: [[x0, y0, z], [x1, y0, z], [x1, y1, z], [x0, y1, z]],
            color: Color32::from_rgb(id as u8 * 50, 0, 0),
            id,
            group: id,
        }
    }

    #[test]
    fn nearer_quad_wins_regardless_of_order() {
        for order in [[0usize, 1], [1, 0]] {
            let mut r = Raster::default();
            r.clear(20, 20, 1, Color32::BLACK);
            let q = [
                square(0.0, 0.0, 20.0, 20.0, 5.0, 1),
                square(5.0, 5.0, 15.0, 15.0, 1.0, 2),
            ];
            let prims: Vec<Prim> = order
                .iter()
                .map(|&i| Prim {
                    p: q[i].p,
                    color: q[i].color,
                    id: q[i].id,
                    group: q[i].group,
                })
                .collect();
            r.draw(&prims);
            assert_eq!(r.id_at(10.0, 10.0), 2);
            assert_eq!(r.id_at(2.0, 2.0), 1);
        }
    }

    #[test]
    fn supersampled_image_has_output_size() {
        let mut r = Raster::default();
        r.clear(40, 20, 2, Color32::BLACK);
        r.draw(&[square(0.0, 0.0, 40.0, 20.0, 1.0, 1)]);
        let img = r.image();
        assert_eq!(img.size, [20, 10]);
        assert_eq!(r.id_at(5.0, 5.0), 1);
    }
}
