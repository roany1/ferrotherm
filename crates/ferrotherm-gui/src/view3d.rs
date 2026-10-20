// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lightweight 3D viewport drawn with the egui painter: orthographic camera,
//! axis-aligned quads sorted back-to-front (painter's algorithm), picking,
//! clipping plane and colour legend. No custom GPU code, so it runs on every
//! renderer eframe supports (wgpu: Metal/DX12/Vulkan, or OpenGL).

use crate::colormap::Palette;
use crate::raster::{Prim, Raster};
use eframe::egui::{
    self, epaint::Mesh, Align2, Color32, FontId, PointerButton, Pos2, Rect, Sense, Shape, Stroke,
    Vec2,
};
use ferrotherm_core::model::{Model, Owner};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

pub type V3 = [f32; 3];

#[inline]
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
#[inline]
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
#[inline]
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
#[inline]
fn mul(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
#[inline]
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
#[inline]
fn normalize(a: V3) -> V3 {
    let l = dot(a, a).sqrt().max(1e-12);
    mul(a, 1.0 / l)
}

/// What a drawn face belongs to (used for picking and tooltips).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Block(usize),
    Component(usize),
    Boundary(usize),
    Cell(usize),
}

/// One axis-aligned face. `value` is a temperature (results) or NaN.
#[derive(Debug, Clone, Copy)]
pub struct Quad {
    pub v: [V3; 4],
    pub axis: u8,
    pub positive: bool,
    pub color: Color32,
    pub value: f32,
    pub tag: Tag,
}

/// The six faces of a box, vertices in mm.
pub fn box_quads(lo: V3, hi: V3, color: Color32, tag: Tag, out: &mut Vec<Quad>) {
    for axis in 0..3 {
        let (t0, t1) = match axis {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        for positive in [false, true] {
            let x = if positive { hi[axis] } else { lo[axis] };
            let mut v = [[0.0f32; 3]; 4];
            let corners = [
                (lo[t0], lo[t1]),
                (hi[t0], lo[t1]),
                (hi[t0], hi[t1]),
                (lo[t0], hi[t1]),
            ];
            for (k, (a, b)) in corners.iter().enumerate() {
                v[k][axis] = x;
                v[k][t0] = *a;
                v[k][t1] = *b;
            }
            out.push(Quad {
                v,
                axis: axis as u8,
                positive,
                color,
                value: f32::NAN,
                tag,
            });
        }
    }
}

/// Darken a colour according to the face orientation, so shapes read well
/// without a lighting model (top brightest, then x, then y faces).
pub fn shade(c: Color32, axis: u8) -> Color32 {
    let f = match axis {
        2 => 1.0,
        0 => 0.82,
        _ => 0.67,
    };
    Color32::from_rgba_unmultiplied(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
        c.a(),
    )
}

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub target: V3,
    pub yaw: f32,
    pub pitch: f32,
    /// Pixels per mm.
    pub scale: f32,
}

#[derive(Debug, Clone, Copy)]
struct Basis {
    fwd: V3,
    right: V3,
    up: V3,
}

impl Camera {
    fn basis(&self) -> Basis {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        // direction from the target towards the eye
        let eye_dir = [cp * cy, cp * sy, sp];
        let fwd = mul(eye_dir, -1.0);
        let right = normalize(cross(fwd, [0.0, 0.0, 1.0]));
        let up = cross(right, fwd);
        Basis { fwd, right, up }
    }

    #[inline]
    fn project(&self, b: &Basis, center: Pos2, p: V3) -> (Pos2, f32) {
        let d = sub(p, self.target);
        (
            Pos2::new(
                center.x + dot(d, b.right) * self.scale,
                center.y - dot(d, b.up) * self.scale,
            ),
            dot(d, b.fwd),
        )
    }

    pub fn fit(&mut self, lo: V3, hi: V3, rect: Rect) {
        self.target = mul(add(lo, hi), 0.5);
        let diag = dot(sub(hi, lo), sub(hi, lo)).sqrt().max(1e-6);
        self.scale = 0.85 * rect.width().min(rect.height()) / diag;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Clip {
    pub enabled: bool,
    pub axis: usize,
    /// mm
    pub pos: f64,
    /// Keep the part above `pos` instead of below.
    pub flip: bool,
}

impl Clip {
    /// Clip an axis-aligned box; `None` if fully removed.
    pub fn apply(&self, mut lo: [f64; 3], mut hi: [f64; 3]) -> Option<([f64; 3], [f64; 3])> {
        if !self.enabled {
            return Some((lo, hi));
        }
        let a = self.axis;
        if self.flip {
            if hi[a] <= self.pos {
                return None;
            }
            lo[a] = lo[a].max(self.pos);
        } else {
            if lo[a] >= self.pos {
                return None;
            }
            hi[a] = hi[a].min(self.pos);
        }
        Some((lo, hi))
    }

    pub fn keeps(&self, x: f64) -> bool {
        !self.enabled
            || if self.flip {
                x >= self.pos
            } else {
                x <= self.pos
            }
    }
}

/// Exterior faces of the solid cells of a solved model, coloured later from
/// `value`. Component bodies take their junction temperature.
pub fn result_quads(
    m: &Model,
    t: &[f64],
    hidden_blocks: &[bool],
    show_air: bool,
    clip: &Clip,
) -> (Vec<Quad>, f32, f32) {
    let g = &m.grid;
    let n = g.n();
    let solid = |c: usize| -> bool {
        let keep = match m.owner[c] {
            Owner::Fill => show_air,
            Owner::Block(b) => !hidden_blocks.get(b).copied().unwrap_or(false),
            Owner::Component(_) => true,
        };
        if !keep || !clip.enabled {
            return keep;
        }
        let p = g.ijk(c);
        clip.keeps(g.center(clip.axis, p[clip.axis]) * 1e3)
    };
    let stride = [1usize, n[0], n[0] * n[1]];
    let mut out = Vec::new();
    let (mut vmin, mut vmax) = (f32::INFINITY, f32::NEG_INFINITY);
    for c in 0..m.ncells {
        if !solid(c) {
            continue;
        }
        let p = g.ijk(c);
        let value = match m.owner[c] {
            Owner::Component(ci) => t[m.components[ci].node],
            _ => t[c],
        } as f32;
        let lo = [0, 1, 2].map(|a| (g.nodes[a][p[a]] * 1e3) as f32);
        let hi = [0, 1, 2].map(|a| (g.nodes[a][p[a] + 1] * 1e3) as f32);
        for axis in 0..3 {
            for positive in [false, true] {
                let exposed = if positive {
                    p[axis] + 1 == n[axis] || !solid(c + stride[axis])
                } else {
                    p[axis] == 0 || !solid(c - stride[axis])
                };
                if !exposed {
                    continue;
                }
                let (t0, t1) = match axis {
                    0 => (1, 2),
                    1 => (0, 2),
                    _ => (0, 1),
                };
                let x = if positive { hi[axis] } else { lo[axis] };
                let mut v = [[0.0f32; 3]; 4];
                let corners = [
                    (lo[t0], lo[t1]),
                    (hi[t0], lo[t1]),
                    (hi[t0], hi[t1]),
                    (lo[t0], hi[t1]),
                ];
                for (k, (a, b)) in corners.iter().enumerate() {
                    v[k][axis] = x;
                    v[k][t0] = *a;
                    v[k][t1] = *b;
                }
                out.push(Quad {
                    v,
                    axis: axis as u8,
                    positive,
                    color: Color32::WHITE,
                    value,
                    tag: Tag::Cell(c),
                });
                vmin = vmin.min(value);
                vmax = vmax.max(value);
            }
        }
    }
    (out, vmin, vmax)
}

/// A face as it was drawn in the last frame (front-most first), for picking.
#[derive(Debug, Clone, Copy)]
pub struct Drawn {
    pub quad: usize,
    pub layer: u8,
    pub pts: [Pos2; 4],
}

fn point_in_quad(p: Pos2, q: &[Pos2; 4]) -> bool {
    let mut sign = 0.0f32;
    for k in 0..4 {
        let a = q[k];
        let b = q[(k + 1) % 4];
        let c = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if sign != c.signum() {
            return false;
        }
    }
    sign != 0.0
}

/// One layer of quads to draw. Layers are drawn in order; within a layer the
/// quads are depth-sorted.
pub struct Layer<'a> {
    pub quads: &'a [Quad],
    /// Draw back faces too (needed for translucent overlays and clipped shells).
    pub two_sided: bool,
    pub outline: Option<Stroke>,
    /// Override fill colour (e.g. from a colour map).
    pub color: &'a dyn Fn(&Quad) -> Color32,
    pub pickable: bool,
}

pub struct Frame {
    pub rect: Rect,
    pub response: egui::Response,
}

/// Viewport state that survives between frames.
pub struct Viewport {
    pub cam: Camera,
    pub needs_fit: bool,
    pub drawn: Vec<Drawn>,
    raster: Raster,
    tex: Option<egui::TextureHandle>,
    raster_key: u64,
    last_motion: Option<Instant>,
    /// Device pixels per point used for the last raster.
    ppp: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            cam: Camera {
                target: [0.0; 3],
                yaw: (-55.0f32).to_radians(),
                pitch: 30.0f32.to_radians(),
                scale: 1.0,
            },
            needs_fit: true,
            drawn: Vec::new(),
            raster: Raster::default(),
            tex: None,
            raster_key: 0,
            last_motion: None,
            ppp: 1.0,
        }
    }
}

pub enum Preset {
    Iso,
    Top,
    Bottom,
    Front,
    Back,
    Left,
    Right,
}

impl Viewport {
    pub fn preset(&mut self, p: Preset) {
        let (yaw, pitch) = match p {
            Preset::Iso => (-55.0, 30.0),
            Preset::Top => (-90.0, 89.9),
            Preset::Bottom => (-90.0, -89.9),
            Preset::Front => (-90.0, 0.0),
            Preset::Back => (90.0, 0.0),
            Preset::Left => (180.0, 0.0),
            Preset::Right => (0.0, 0.0),
        };
        self.cam.yaw = (yaw as f32).to_radians();
        self.cam.pitch = (pitch as f32).to_radians();
        self.needs_fit = true;
    }

    /// Allocate the viewport and handle orbit / pan / zoom.
    pub fn begin(&mut self, ui: &mut egui::Ui, bounds: (V3, V3)) -> Frame {
        // never extend past the visible area (an overflowing toolbar can widen the ui)
        let clip = ui.clip_rect();
        let cur = ui.cursor().min;
        let size = Vec2::new(
            ui.available_size().x.min(clip.right() - cur.x),
            ui.available_size().y.min(clip.bottom() - cur.y),
        )
        .max(Vec2::splat(50.0));
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        if self.needs_fit {
            self.cam.fit(bounds.0, bounds.1, rect);
            self.needs_fit = false;
        }
        let shift = ui.input(|i| i.modifiers.shift);
        let delta = response.drag_delta();
        let b = self.cam.basis();
        let panning = response.dragged_by(PointerButton::Secondary)
            || response.dragged_by(PointerButton::Middle)
            || (response.dragged_by(PointerButton::Primary) && shift);
        if panning {
            let s = self.cam.scale;
            self.cam.target = add(
                self.cam.target,
                add(mul(b.right, -delta.x / s), mul(b.up, delta.y / s)),
            );
        } else if response.dragged_by(PointerButton::Primary) {
            self.cam.yaw -= delta.x * 0.008;
            self.cam.pitch = (self.cam.pitch + delta.y * 0.008).clamp(-1.5705, 1.5705);
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                if let Some(pos) = response.hover_pos() {
                    let o = pos - rect.center();
                    let s0 = self.cam.scale;
                    let s1 = (s0 * (scroll * 0.0025).exp()).clamp(1e-4, 1e5);
                    let p = add(
                        self.cam.target,
                        add(mul(b.right, o.x / s0), mul(b.up, -o.y / s0)),
                    );
                    self.cam.target = add(p, add(mul(b.right, -o.x / s1), mul(b.up, o.y / s1)));
                    self.cam.scale = s1;
                }
            }
        }
        if response.double_clicked() {
            self.needs_fit = true;
        }
        let scrolled = response.hovered() && ui.input(|i| i.smooth_scroll_delta.y != 0.0);
        if response.dragged() || scrolled {
            self.last_motion = Some(Instant::now());
        }
        Frame { rect, response }
    }

    /// Draw layers; returns nothing, but remembers what was drawn for picking.
    pub fn draw(&mut self, ui: &egui::Ui, frame: &Frame, layers: &[Layer]) {
        let painter = ui.painter_at(frame.rect);
        let b = self.cam.basis();
        let center = frame.rect.center();
        self.drawn.clear();
        let mut shapes: Vec<Shape> = Vec::new();
        for (li, layer) in layers.iter().enumerate() {
            let mut items: Vec<(f32, Drawn)> = Vec::with_capacity(layer.quads.len());
            for (qi, q) in layer.quads.iter().enumerate() {
                let facing = if q.positive {
                    b.fwd[q.axis as usize]
                } else {
                    -b.fwd[q.axis as usize]
                };
                if !layer.two_sided && facing >= 0.0 {
                    continue; // back face
                }
                let mut pts = [Pos2::ZERO; 4];
                let mut depth = 0.0;
                for k in 0..4 {
                    let (p, d) = self.cam.project(&b, center, q.v[k]);
                    pts[k] = p;
                    depth += d;
                }
                let bb = Rect::from_points(&pts);
                if !bb.intersects(frame.rect) {
                    continue;
                }
                items.push((
                    depth,
                    Drawn {
                        quad: qi,
                        layer: li as u8,
                        pts,
                    },
                ));
            }
            // back to front
            items.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            if let Some(stroke) = layer.outline {
                for (_, d) in &items {
                    let q = &layer.quads[d.quad];
                    let c = (layer.color)(q);
                    let mut mesh = Mesh::default();
                    for p in d.pts {
                        mesh.colored_vertex(p, c);
                    }
                    mesh.add_triangle(0, 1, 2);
                    mesh.add_triangle(0, 2, 3);
                    shapes.push(Shape::mesh(mesh));
                    shapes.push(Shape::closed_line(d.pts.to_vec(), stroke));
                }
            } else {
                let mut mesh = Mesh::default();
                mesh.reserve_vertices(items.len() * 4);
                mesh.reserve_triangles(items.len() * 2);
                for (_, d) in &items {
                    let q = &layer.quads[d.quad];
                    let c = (layer.color)(q);
                    let base = mesh.vertices.len() as u32;
                    for p in d.pts {
                        mesh.colored_vertex(p, c);
                    }
                    mesh.add_triangle(base, base + 1, base + 2);
                    mesh.add_triangle(base, base + 2, base + 3);
                }
                shapes.push(Shape::mesh(mesh));
            }
            if layer.pickable {
                self.drawn.extend(items.into_iter().map(|(_, d)| d));
            }
        }
        painter.extend(shapes);
        // front-most first for picking: later layers are on top, and within a
        // layer the last drawn is the nearest
        self.drawn.reverse();
    }

    /// Render opaque quads with a depth buffer and show the image.
    /// `scene_hash` must change whenever the quads or their colours change.
    #[allow(clippy::too_many_arguments)]
    pub fn render_opaque(
        &mut self,
        ui: &egui::Ui,
        frame: &Frame,
        quads: &[Quad],
        color: &dyn Fn(&Quad) -> Color32,
        group: &dyn Fn(&Quad) -> u32,
        lines: &[([V3; 2], Color32)],
        scene_hash: u64,
        bg: Color32,
        outline: f32,
    ) {
        let ppp = ui.ctx().pixels_per_point();
        let w = (frame.rect.width() * ppp).round().max(1.0) as usize;
        let h = (frame.rect.height() * ppp).round().max(1.0) as usize;
        let moving = self
            .last_motion
            .map(|t| t.elapsed() < Duration::from_millis(180))
            .unwrap_or(false);
        let ss = if moving || w * h > 3_000_000 { 1 } else { 2 };
        if moving {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        scene_hash.hash(&mut hs);
        for v in [
            self.cam.target[0],
            self.cam.target[1],
            self.cam.target[2],
            self.cam.yaw,
            self.cam.pitch,
            self.cam.scale,
            ppp,
        ] {
            v.to_bits().hash(&mut hs);
        }
        (w, h, ss, bg.to_array(), outline.to_bits()).hash(&mut hs);
        for (seg, col) in lines {
            for v in seg.iter().flatten() {
                v.to_bits().hash(&mut hs);
            }
            col.to_array().hash(&mut hs);
        }
        let key = hs.finish();
        if key != self.raster_key || self.tex.is_none() {
            self.raster_key = key;
            self.ppp = ppp;
            let b = self.cam.basis();
            let s = self.cam.scale * ppp * ss as f32;
            let (cx, cy) = ((w * ss) as f32 / 2.0, (h * ss) as f32 / 2.0);
            let mut prims = Vec::with_capacity(quads.len());
            for (qi, q) in quads.iter().enumerate() {
                let facing = if q.positive {
                    b.fwd[q.axis as usize]
                } else {
                    -b.fwd[q.axis as usize]
                };
                if facing >= 0.0 {
                    continue;
                }
                let mut p = [[0.0f32; 3]; 4];
                for k in 0..4 {
                    let d = sub(q.v[k], self.cam.target);
                    p[k] = [
                        cx + dot(d, b.right) * s,
                        cy - dot(d, b.up) * s,
                        dot(d, b.fwd),
                    ];
                }
                prims.push(Prim {
                    p,
                    color: color(q),
                    id: qi as u32 + 1,
                    group: group(q),
                });
            }
            self.raster.clear(w * ss, h * ss, ss, bg);
            self.raster.draw(&prims);
            if outline > 0.0 {
                self.raster.outlines(outline, ss);
            }
            for (seg, col) in lines {
                let pr = |v: V3| {
                    let d = sub(v, self.cam.target);
                    [
                        cx + dot(d, b.right) * s,
                        cy - dot(d, b.up) * s,
                        dot(d, b.fwd),
                    ]
                };
                self.raster.line(pr(seg[0]), pr(seg[1]), *col, ss);
            }
            let img = self.raster.image();
            match &mut self.tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => {
                    self.tex = Some(ui.ctx().load_texture(
                        "ferrotherm-view",
                        img,
                        egui::TextureOptions::LINEAR,
                    ))
                }
            }
        }
        if let Some(t) = &self.tex {
            ui.painter_at(frame.rect).image(
                t.id(),
                frame.rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }

    /// Opaque face (quad index) under `pos`, from the last raster.
    pub fn pick_opaque(&self, rect: Rect, pos: Pos2) -> Option<usize> {
        let o = (pos - rect.min) * self.ppp;
        match self.raster.id_at(o.x, o.y) {
            0 => None,
            id => Some(id as usize - 1),
        }
    }

    /// Front-most pickable face under `pos`: (layer, quad index).
    pub fn pick(&self, pos: Pos2) -> Option<(usize, usize)> {
        self.drawn
            .iter()
            .find(|d| point_in_quad(pos, &d.pts))
            .map(|d| (d.layer as usize, d.quad))
    }

    pub fn project(&self, rect: Rect, p: V3) -> Pos2 {
        let b = self.cam.basis();
        self.cam.project(&b, rect.center(), p).0
    }

    /// Box edges as lines (domain outline, clip plane, selection).
    pub fn draw_box_edges(&self, ui: &egui::Ui, rect: Rect, lo: V3, hi: V3, stroke: Stroke) {
        let painter = ui.painter_at(rect);
        let c = |x: usize, y: usize, z: usize| {
            self.project(
                rect,
                [
                    if x == 0 { lo[0] } else { hi[0] },
                    if y == 0 { lo[1] } else { hi[1] },
                    if z == 0 { lo[2] } else { hi[2] },
                ],
            )
        };
        let edges = [
            ((0, 0, 0), (1, 0, 0)),
            ((0, 1, 0), (1, 1, 0)),
            ((0, 0, 1), (1, 0, 1)),
            ((0, 1, 1), (1, 1, 1)),
            ((0, 0, 0), (0, 1, 0)),
            ((1, 0, 0), (1, 1, 0)),
            ((0, 0, 1), (0, 1, 1)),
            ((1, 0, 1), (1, 1, 1)),
            ((0, 0, 0), (0, 0, 1)),
            ((1, 0, 0), (1, 0, 1)),
            ((0, 1, 0), (0, 1, 1)),
            ((1, 1, 0), (1, 1, 1)),
        ];
        for (a, b) in edges {
            painter.line_segment([c(a.0, a.1, a.2), c(b.0, b.1, b.2)], stroke);
        }
    }

    /// Small XYZ triad in the lower-left corner.
    pub fn draw_axes(&self, ui: &egui::Ui, rect: Rect) {
        let painter = ui.painter_at(rect);
        let b = self.cam.basis();
        let origin = Pos2::new(rect.left() + 42.0, rect.bottom() - 42.0);
        let cols = [
            Color32::from_rgb(230, 80, 80),
            Color32::from_rgb(90, 200, 90),
            Color32::from_rgb(90, 140, 255),
        ];
        for (a, name) in ["X", "Y", "Z"].iter().enumerate() {
            let mut e = [0.0; 3];
            e[a] = 1.0;
            let tip = origin + Vec2::new(dot(e, b.right), -dot(e, b.up)) * 28.0;
            painter.line_segment([origin, tip], Stroke::new(2.0, cols[a]));
            painter.text(
                tip + (tip - origin).normalized() * 8.0,
                Align2::CENTER_CENTER,
                *name,
                FontId::proportional(12.0),
                cols[a],
            );
        }
    }
}

/// The 12 edges of a box as line segments.
pub fn box_edges(lo: V3, hi: V3, color: Color32, out: &mut Vec<([V3; 2], Color32)>) {
    let c = |x: usize, y: usize, z: usize| {
        [
            if x == 0 { lo[0] } else { hi[0] },
            if y == 0 { lo[1] } else { hi[1] },
            if z == 0 { lo[2] } else { hi[2] },
        ]
    };
    for (a, b) in [
        ((0, 0, 0), (1, 0, 0)),
        ((0, 1, 0), (1, 1, 0)),
        ((0, 0, 1), (1, 0, 1)),
        ((0, 1, 1), (1, 1, 1)),
        ((0, 0, 0), (0, 1, 0)),
        ((1, 0, 0), (1, 1, 0)),
        ((0, 0, 1), (0, 1, 1)),
        ((1, 0, 1), (1, 1, 1)),
        ((0, 0, 0), (0, 0, 1)),
        ((1, 0, 0), (1, 0, 1)),
        ((0, 1, 0), (0, 1, 1)),
        ((1, 1, 0), (1, 1, 1)),
    ] {
        out.push(([c(a.0, a.1, a.2), c(b.0, b.1, b.2)], color));
    }
}

/// Hash of a quad list (geometry changes invalidate the raster).
pub fn hash_quads(quads: &[Quad]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    quads.len().hash(&mut h);
    for q in quads {
        for v in q.v.iter().flatten() {
            v.to_bits().hash(&mut h);
        }
        q.color.to_array().hash(&mut h);
        (q.axis, q.positive).hash(&mut h);
    }
    h.finish()
}

/// Vertical colour bar on the right edge of the viewport.
pub fn draw_colorbar(
    ui: &egui::Ui,
    rect: Rect,
    palette: Palette,
    vmin: f32,
    vmax: f32,
    unit: &str,
    text_color: Color32,
) {
    let painter = ui.painter_at(rect);
    let h = (rect.height() * 0.55).clamp(120.0, 420.0);
    let bar = Rect::from_min_size(
        Pos2::new(rect.right() - 70.0, rect.center().y - h / 2.0),
        Vec2::new(18.0, h),
    );
    let mut mesh = Mesh::default();
    let steps = 64;
    for s in 0..steps {
        let y0 = bar.bottom() - bar.height() * s as f32 / steps as f32;
        let y1 = bar.bottom() - bar.height() * (s + 1) as f32 / steps as f32;
        let c0 = palette.map(s as f32 / steps as f32);
        let c1 = palette.map((s + 1) as f32 / steps as f32);
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(Pos2::new(bar.left(), y0), c0);
        mesh.colored_vertex(Pos2::new(bar.right(), y0), c0);
        mesh.colored_vertex(Pos2::new(bar.right(), y1), c1);
        mesh.colored_vertex(Pos2::new(bar.left(), y1), c1);
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    painter.add(Shape::mesh(mesh));
    painter.rect_stroke(
        bar,
        0.0,
        Stroke::new(1.0, text_color.gamma_multiply(0.6)),
        egui::StrokeKind::Outside,
    );
    let ticks = 6;
    for k in 0..=ticks {
        let f = k as f32 / ticks as f32;
        let y = bar.bottom() - bar.height() * f;
        let v = vmin + (vmax - vmin) * f;
        painter.line_segment(
            [Pos2::new(bar.right(), y), Pos2::new(bar.right() + 4.0, y)],
            Stroke::new(1.0, text_color),
        );
        painter.text(
            Pos2::new(bar.right() + 7.0, y),
            Align2::LEFT_CENTER,
            format!("{v:.1}"),
            FontId::proportional(11.0),
            text_color,
        );
    }
    painter.text(
        Pos2::new(bar.center().x, bar.top() - 10.0),
        Align2::CENTER_BOTTOM,
        unit,
        FontId::proportional(12.0),
        text_color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_in_quad_both_windings() {
        let q = [
            Pos2::new(0.0, 0.0),
            Pos2::new(10.0, 0.0),
            Pos2::new(10.0, 10.0),
            Pos2::new(0.0, 10.0),
        ];
        let mut r = q;
        r.reverse();
        assert!(point_in_quad(Pos2::new(5.0, 5.0), &q));
        assert!(point_in_quad(Pos2::new(5.0, 5.0), &r));
        assert!(!point_in_quad(Pos2::new(15.0, 5.0), &q));
    }

    #[test]
    fn clip_box() {
        let c = Clip {
            enabled: true,
            axis: 2,
            pos: 5.0,
            flip: false,
        };
        assert_eq!(
            c.apply([0.0; 3], [1.0, 1.0, 10.0]),
            Some(([0.0; 3], [1.0, 1.0, 5.0]))
        );
        assert_eq!(c.apply([0.0, 0.0, 6.0], [1.0, 1.0, 10.0]), None);
    }

    #[test]
    fn camera_right_and_up_are_orthonormal() {
        let cam = Camera {
            target: [0.0; 3],
            yaw: 0.3,
            pitch: 0.7,
            scale: 1.0,
        };
        let b = cam.basis();
        assert!(dot(b.right, b.up).abs() < 1e-6);
        assert!(dot(b.right, b.fwd).abs() < 1e-6);
        assert!((dot(b.up, b.up) - 1.0).abs() < 1e-5);
        assert!(b.up[2] > 0.0, "z should point up on screen");
    }
}
