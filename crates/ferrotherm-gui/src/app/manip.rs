//! Direct manipulation in the 3D view: drag objects from the object panel
//! into the scene, and move / resize the selected object with handles.

use super::{BoxMm, FerroApp, Sel, Src};
use crate::objects::Pick;
use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Stroke, Ui, Vec2};
use ferrotherm_core::model::{axis_of, component_box, direction_of, fan_extent, tangential};

/// A handle on the selected object.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Handle {
    /// Move along an axis (arrow tip).
    Move(usize),
    /// Move one face along its axis (resize).
    Face(usize, bool),
}

/// What the handles act on.
#[derive(Debug, Clone)]
pub(super) struct Target {
    sel: Sel,
    pub(super) lo: [f64; 3],
    pub(super) hi: [f64; 3],
    /// Axes along which the object can be resized.
    resize: [bool; 3],
    movable: bool,
    /// Blocks moved / resized together (an assembly such as a chassis).
    group: Vec<(usize, BoxMm)>,
}

impl Target {
    pub(super) fn is_group(&self) -> bool {
        !self.group.is_empty()
    }
}

/// A handle drag in progress.
pub(super) struct HandleDrag {
    handle: Handle,
    target: Target,
    /// Screen points per mm along the handle axis, fixed at the start.
    dir: Vec2,
    /// Pointer travel since the press, points.
    acc: Vec2,
}

/// Handles drawn in the last frame: screen position, handle, axis direction.
pub(super) type DrawnHandles = Vec<(Pos2, Handle)>;

const HANDLE_R: f32 = 6.0;
const ARROW: f32 = 46.0;
const MIN_SIZE: f64 = 0.05;
const AXIS_COLOR: [Color32; 3] = [
    Color32::from_rgb(230, 70, 70),
    Color32::from_rgb(70, 190, 90),
    Color32::from_rgb(80, 130, 240),
];

/// Snap a length to the grid (0.1 mm, or 1 mm with Ctrl).
fn snap(v: f64, coarse: bool) -> f64 {
    let g = if coarse { 1.0 } else { 0.1 };
    (v / g).round() * g
}

/// Name prefix shared by the walls of a chassis made in the object panel.
fn chassis_prefix(name: &str) -> Option<&str> {
    let (head, _) = name.split_once('/')?;
    head.starts_with("chassis_").then_some(head)
}

impl FerroApp {
    /// The manipulation target for the current selection.
    pub(super) fn manip_target(&self) -> Option<Target> {
        let all = [true; 3];
        let t = |lo, hi, resize, movable| Target {
            sel: self.sel,
            lo,
            hi,
            resize,
            movable,
            group: Vec::new(),
        };
        match self.sel {
            Sel::Block(i) => {
                let b = self.case.blocks.get(i)?;
                if let Some(pre) = chassis_prefix(&b.name) {
                    let group: Vec<(usize, BoxMm)> = self
                        .case
                        .blocks
                        .iter()
                        .enumerate()
                        .filter(|(_, x)| chassis_prefix(&x.name) == Some(pre))
                        .map(|(k, x)| (k, (x.min, x.max)))
                        .collect();
                    let mut lo = [f64::INFINITY; 3];
                    let mut hi = [f64::NEG_INFINITY; 3];
                    for (_, (a, b)) in &group {
                        for k in 0..3 {
                            lo[k] = lo[k].min(a[k]);
                            hi[k] = hi[k].max(b[k]);
                        }
                    }
                    let mut tg = t(lo, hi, all, true);
                    tg.group = group;
                    return Some(tg);
                }
                Some(t(b.min, b.max, all, true))
            }
            Sel::Component(i) => {
                let (lo, hi) = component_box(self.case.components.get(i)?, &self.library).ok()?;
                Some(t(lo, hi, all, true))
            }
            Sel::HeatSink(i) => {
                let h = self.case.heatsinks.get(i)?;
                let mut lo = [f64::INFINITY; 3];
                let mut hi = [f64::NEG_INFINITY; 3];
                for b in h.blocks().ok()? {
                    for k in 0..3 {
                        lo[k] = lo[k].min(b.min[k]);
                        hi[k] = hi[k].max(b.max[k]);
                    }
                }
                Some(t(lo, hi, all, true))
            }
            Sel::Fan(i) => {
                let f = self.case.fans.get(i)?;
                let (a, _) = direction_of(&f.direction).ok()?;
                let (lo, hi) = self.fan_boxes().into_iter().find(|x| x.0 == i)?.1;
                let mut r = all;
                r[a] = false;
                Some(t(lo, hi, r, true))
            }
            Sel::Resistance(i) => {
                let a = axis_of(&self.case.resistances.get(i)?.axis).ok()?;
                let (lo, hi) = self.resistance_boxes().into_iter().find(|x| x.0 == i)?.1;
                let mut r = all;
                r[a] = false;
                Some(t(lo, hi, r, true))
            }
            Sel::Monitor(i) => {
                let (lo, hi) = self.monitor_boxes().into_iter().find(|x| x.0 == i)?.1;
                Some(t(lo, hi, [false; 3], true))
            }
            Sel::Domain => Some(t(self.case.domain.min, self.case.domain.max, all, false)),
            _ => None,
        }
    }

    /// Write a new bounding box back into the object.
    fn apply_box(&mut self, tg: &Target, lo: [f64; 3], hi: [f64; 3]) {
        let ext = [0, 1, 2].map(|k| hi[k] - lo[k]);
        let mid = [0, 1, 2].map(|k| 0.5 * (lo[k] + hi[k]));
        match tg.sel {
            Sel::Block(_) if !tg.group.is_empty() => {
                // every coordinate keeps its offset to the nearer face of the
                // assembly box, so walls keep their thickness
                let map = |x: f64, k: usize| {
                    if x - tg.lo[k] <= tg.hi[k] - x {
                        lo[k] + (x - tg.lo[k])
                    } else {
                        hi[k] - (tg.hi[k] - x)
                    }
                };
                for (bi, (a, b)) in &tg.group {
                    if let Some(bl) = self.case.blocks.get_mut(*bi) {
                        for k in 0..3 {
                            bl.min[k] = map(a[k], k);
                            bl.max[k] = map(b[k], k).max(bl.min[k] + MIN_SIZE);
                        }
                    }
                }
            }
            Sel::Block(i) => {
                if let Some(b) = self.case.blocks.get_mut(i) {
                    b.min = lo;
                    b.max = hi;
                }
            }
            Sel::Component(i) => {
                if let Some(c) = self.case.components.get_mut(i) {
                    let Ok(n) = axis_of(&c.mount_axis) else {
                        return;
                    };
                    let tg2 = tangential(n);
                    c.position = lo;
                    let s = [ext[tg2[0]], ext[tg2[1]], ext[n]];
                    // keep following the package size until the user resizes
                    let pkg = self
                        .library
                        .package(&c.package)
                        .ok()
                        .and_then(|p| p.size_mm);
                    let same =
                        |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-9);
                    if c.size.is_some() || pkg.map(|p| !same(p, s)).unwrap_or(true) {
                        c.size = Some(s);
                    }
                }
            }
            Sel::HeatSink(i) => {
                if let Some(h) = self.case.heatsinks.get_mut(i) {
                    let (Ok(n), Ok(f)) = (axis_of(&h.normal), axis_of(&h.fin_axis)) else {
                        return;
                    };
                    let w = 3 - n - f;
                    h.position = lo;
                    h.base[0] = ext[f];
                    h.base[1] = ext[w];
                    h.fin_height = (ext[n] - h.base[2]).max(MIN_SIZE);
                    let max_fins = (h.base[1] / h.fin_thickness).floor().max(1.0) as usize;
                    h.fin_count = h.fin_count.min(max_fins);
                }
            }
            Sel::Fan(i) => {
                if let Some(f) = self.case.fans.get_mut(i) {
                    let Ok((a, _)) = direction_of(&f.direction) else {
                        return;
                    };
                    let tg2 = tangential(a);
                    let old = fan_extent(f).unwrap_or([0.0; 2]);
                    let new = [ext[tg2[0]], ext[tg2[1]]];
                    f.center = mid;
                    if (new[0] - old[0]).abs() > 1e-9 || (new[1] - old[1]).abs() > 1e-9 {
                        if f.size.is_none() && (new[0] - new[1]).abs() < 1e-9 {
                            f.diameter = Some(new[0]);
                        } else {
                            f.size = Some(new);
                        }
                    }
                }
            }
            Sel::Resistance(i) => {
                if let Some(r) = self.case.resistances.get_mut(i) {
                    let Ok(a) = axis_of(&r.axis) else { return };
                    let tg2 = tangential(a);
                    r.center = mid;
                    r.size = [ext[tg2[0]], ext[tg2[1]]];
                }
            }
            Sel::Monitor(i) => {
                if let Some(m) = self.case.monitors.get_mut(i) {
                    m.position = mid;
                }
            }
            Sel::Domain => {
                self.case.domain.min = lo;
                self.case.domain.max = hi;
            }
            _ => {}
        }
    }

    /// Handle under a screen point (from the last frame).
    fn handle_at(&self, p: Pos2) -> Option<Handle> {
        self.handles
            .iter()
            .filter(|(q, _)| q.distance(p) <= HANDLE_R + 3.0)
            .min_by(|a, b| a.0.distance(p).total_cmp(&b.0.distance(p)))
            .map(|x| x.1)
    }

    /// Start a handle drag if the primary button was pressed on a handle.
    /// Call before `Viewport::begin`.
    pub(super) fn handles_before_view(&mut self, ui: &Ui) {
        let (pressed, origin, down, delta, ctrl) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.press_origin(),
                i.pointer.primary_down(),
                i.pointer.delta(),
                i.modifiers.command,
            )
        });
        if pressed && self.handle_drag.is_none() {
            if let Some(h) = origin.and_then(|p| self.handle_at(p)) {
                if let Some(target) = self.manip_target() {
                    let axis = match h {
                        Handle::Move(a) | Handle::Face(a, _) => a,
                    };
                    self.handle_drag = Some(HandleDrag {
                        handle: h,
                        target,
                        dir: self.vp.screen_axis(axis),
                        acc: Vec2::ZERO,
                    });
                }
            }
        }
        if let Some(d) = &mut self.handle_drag {
            if !down {
                self.handle_drag = None;
            } else if !pressed {
                d.acc += delta;
                let s2 = d.dir.length_sq().max(1e-9);
                let dw = snap((d.acc.dot(d.dir) / s2) as f64, ctrl);
                let tg = d.target.clone();
                let (mut lo, mut hi) = (tg.lo, tg.hi);
                match d.handle {
                    Handle::Move(a) => {
                        lo[a] += dw;
                        hi[a] += dw;
                    }
                    Handle::Face(a, true) => hi[a] = (tg.hi[a] + dw).max(lo[a] + MIN_SIZE),
                    Handle::Face(a, false) => lo[a] = (tg.lo[a] + dw).min(hi[a] - MIN_SIZE),
                }
                if tg.sel != Sel::Domain {
                    // stay inside the domain
                    let (dlo, dhi) = (self.case.domain.min, self.case.domain.max);
                    for k in 0..3 {
                        if matches!(d.handle, Handle::Move(_)) {
                            let w = hi[k] - lo[k];
                            if hi[k] > dhi[k] {
                                hi[k] = dhi[k];
                                lo[k] = hi[k] - w;
                            }
                            if lo[k] < dlo[k] {
                                lo[k] = dlo[k];
                                hi[k] = lo[k] + w;
                            }
                        } else {
                            hi[k] = hi[k].min(dhi[k]);
                            lo[k] = lo[k].max(dlo[k]);
                        }
                    }
                }
                self.apply_box(&tg, lo, hi);
            }
        }
        self.vp.grab = self.handle_drag.is_some();
    }

    /// Draw the handles of the selection and remember where they are.
    pub(super) fn draw_handles(&mut self, ui: &Ui, rect: Rect) {
        self.handles.clear();
        if self.mode == super::ViewMode::Results {
            return;
        }
        let Some(tg) = self.manip_target() else {
            return;
        };
        let f = |v: [f64; 3]| v.map(|x| x as f32);
        let painter = ui.painter_at(rect);
        let c = [0, 1, 2].map(|k| 0.5 * (tg.lo[k] + tg.hi[k]));
        let hover = ui.input(|i| i.pointer.hover_pos());
        let active = self.handle_drag.as_ref().map(|d| d.handle);
        let mut hovered = None;
        // resize handles at the face centres
        for a in 0..3 {
            if !tg.resize[a] {
                continue;
            }
            let s = self.vp.screen_axis(a);
            if s.length() < 0.2 * self.vp.cam.scale {
                continue; // axis points at the viewer
            }
            let face_pt = |pos: bool| {
                let mut p = c;
                p[a] = if pos { tg.hi[a] } else { tg.lo[a] };
                self.vp.project(rect, f(p))
            };
            if face_pt(false).distance(face_pt(true)) < 4.0 * HANDLE_R {
                continue; // too small on screen: zoom in to resize
            }
            for pos in [false, true] {
                let q = face_pt(pos);
                let h = Handle::Face(a, pos);
                let hot = active == Some(h)
                    || (active.is_none() && hover.is_some_and(|m| m.distance(q) <= HANDLE_R + 3.0));
                if hot {
                    hovered = Some(h);
                }
                let r = Rect::from_center_size(q, Vec2::splat(2.0 * HANDLE_R));
                painter.rect_filled(r, 1.5, if hot { Color32::WHITE } else { AXIS_COLOR[a] });
                painter.rect_stroke(
                    r,
                    1.5,
                    Stroke::new(1.0, Color32::BLACK),
                    egui::StrokeKind::Middle,
                );
                self.handles.push((q, h));
            }
        }
        // move arrows from the centre
        if tg.movable {
            let o = self.vp.project(rect, f(c));
            for a in 0..3 {
                let s = self.vp.screen_axis(a);
                if s.length() < 0.2 * self.vp.cam.scale {
                    continue;
                }
                let tip = o + s.normalized() * ARROW;
                let h = Handle::Move(a);
                let hot = active == Some(h)
                    || (active.is_none()
                        && hover.is_some_and(|m| m.distance(tip) <= HANDLE_R + 3.0));
                if hot {
                    hovered = Some(h);
                }
                let col = if hot { Color32::WHITE } else { AXIS_COLOR[a] };
                painter.line_segment([o, tip], Stroke::new(2.5, col));
                painter.circle_filled(tip, HANDLE_R, col);
                painter.circle_stroke(tip, HANDLE_R, Stroke::new(1.0, Color32::BLACK));
                painter.text(
                    tip + s.normalized() * 11.0,
                    Align2::CENTER_CENTER,
                    ["X", "Y", "Z"][a],
                    FontId::proportional(11.0),
                    col,
                );
                self.handles.push((tip, h));
            }
        }
        if let Some(h) = hovered {
            ui.ctx().set_cursor_icon(match h {
                Handle::Move(_) => CursorIcon::Move,
                Handle::Face(..) => CursorIcon::Grab,
            });
        }
        // live readout while dragging
        if let (Some(d), Some(m)) = (&self.handle_drag, hover) {
            let a = match d.handle {
                Handle::Move(a) | Handle::Face(a, _) => a,
            };
            let ax = ["x", "y", "z"][a];
            let txt = match d.handle {
                Handle::Move(_) => format!("{ax} {:.1} … {:.1} mm", tg.lo[a], tg.hi[a]),
                Handle::Face(..) => format!("{ax} size {:.2} mm", tg.hi[a] - tg.lo[a]),
            };
            let galley = painter.layout_no_wrap(txt, FontId::proportional(12.0), Color32::WHITE);
            let at = m + Vec2::new(14.0, -24.0);
            let r = Rect::from_min_size(at, galley.size()).expand(4.0);
            painter.rect_filled(r, 3.0, Color32::from_black_alpha(200));
            painter.galley(at, galley, Color32::WHITE);
        }
    }

    /// Where a dropped object goes: a point, and the face it lands on
    /// (axis, outward side) if the pointer is over geometry.
    fn drop_point(&self, rect: Rect, p: Pos2) -> ([f64; 3], Option<(usize, bool)>) {
        let (o, dir) = self.vp.ray(rect, p);
        let at = |a: usize, x: f32| -> Option<[f64; 3]> {
            if dir[a].abs() < 1e-6 {
                return None;
            }
            let t = (x - o[a]) / dir[a];
            Some([0, 1, 2].map(|k| (o[k] + t * dir[k]) as f64))
        };
        if let Some(h) = self.hit(rect, p, false) {
            if h.0 == Src::Geometry {
                if let Some(q) = self.geometry_quads().get(h.1) {
                    let a = q.axis as usize;
                    if let Some(pt) = at(a, q.v[0][a]) {
                        return (pt, Some((a, q.positive)));
                    }
                }
            }
        }
        // empty space: the domain mid-plane facing the viewer most
        let d = &self.case.domain;
        let a = (0..3)
            .max_by(|&i, &j| dir[i].abs().total_cmp(&dir[j].abs()))
            .unwrap_or(2);
        let mid = 0.5 * (d.min[a] + d.max[a]) as f32;
        (at(a, mid).unwrap_or(self.domain_center()), None)
    }

    /// Drag-and-drop from the object panel into the 3D view.
    pub(super) fn handle_drop(&mut self, ui: &Ui, frame: &crate::view3d::Frame) {
        let rect = frame.rect;
        if let (Some(pick), Some(p)) = (
            frame.response.dnd_hover_payload::<Pick>(),
            ui.input(|i| i.pointer.hover_pos()),
        ) {
            if rect.contains(p) {
                let (pt, face) = self.drop_point(rect, p);
                let q = self.vp.project(rect, pt.map(|x| x as f32));
                let painter = ui.painter_at(rect);
                painter.circle_stroke(q, 7.0, Stroke::new(2.0, Color32::from_rgb(255, 165, 0)));
                painter.line_segment(
                    [q - Vec2::X * 11.0, q + Vec2::X * 11.0],
                    Stroke::new(1.0, Color32::WHITE),
                );
                painter.line_segment(
                    [q - Vec2::Y * 11.0, q + Vec2::Y * 11.0],
                    Stroke::new(1.0, Color32::WHITE),
                );
                let what = match &*pick {
                    Pick::Std(a) => format!("{a:?}"),
                    Pick::Package(id) | Pick::Material(id) => id.clone(),
                };
                let on = if face.is_some() { " on this face" } else { "" };
                painter.text(
                    q + Vec2::new(14.0, -14.0),
                    Align2::LEFT_BOTTOM,
                    format!("Drop {what}{on}"),
                    FontId::proportional(12.0),
                    Color32::WHITE,
                );
            }
        }
        if let (Some(pick), Some(p)) = (
            frame.response.dnd_release_payload::<Pick>(),
            ui.input(|i| i.pointer.interact_pos()),
        ) {
            let (pt, face) = self.drop_point(rect, p);
            self.drop_at((*pick).clone(), pt, face);
        }
    }

    /// Add a picked object and move it to a point (sitting on a face if given).
    pub(super) fn drop_at(&mut self, pick: Pick, pt: [f64; 3], face: Option<(usize, bool)>) {
        let before = self.counts();
        self.add_pick(pick);
        let after = self.counts();
        // everything that was just added and can be moved
        let mut sels = Vec::new();
        for i in before[0]..after[0] {
            sels.push(Sel::Block(i));
        }
        for i in before[1]..after[1] {
            sels.push(Sel::Component(i));
        }
        for i in before[2]..after[2] {
            sels.push(Sel::Fan(i));
        }
        for i in before[3]..after[3] {
            sels.push(Sel::Resistance(i));
        }
        for i in before[4]..after[4] {
            sels.push(Sel::HeatSink(i));
        }
        for i in before[5]..after[5] {
            sels.push(Sel::Monitor(i));
        }
        let keep = self.sel;
        let mut boxes = Vec::new();
        for s in &sels {
            self.sel = *s;
            if let Some(t) = self.manip_target() {
                boxes.push(t);
            }
        }
        if boxes.is_empty() {
            self.sel = keep;
            return;
        }
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for t in &boxes {
            for k in 0..3 {
                lo[k] = lo[k].min(t.lo[k]);
                hi[k] = hi[k].max(t.hi[k]);
            }
        }
        let mut d = [0, 1, 2].map(|k| pt[k] - 0.5 * (lo[k] + hi[k]));
        if let Some((a, positive)) = face {
            d[a] = if positive {
                pt[a] - lo[a]
            } else {
                pt[a] - hi[a]
            };
        }
        // keep it inside the domain
        let dm = &self.case.domain;
        for k in 0..3 {
            let (nlo, nhi) = (lo[k] + d[k], hi[k] + d[k]);
            if nhi > dm.max[k] {
                d[k] -= nhi - dm.max[k];
            }
            if nlo + 0.0 < dm.min[k] {
                d[k] += dm.min[k] - (lo[k] + d[k]);
            }
        }
        if let Some((a, positive)) = face {
            let wanted = if positive {
                pt[a] - lo[a]
            } else {
                pt[a] - hi[a]
            };
            if (d[a] - wanted).abs() > 1e-6 {
                self.warn(
                    "No room outside that face inside the domain: the object was placed \
                     overlapping it. Enlarge the domain (Test Chamber) or move the object.",
                );
            }
        }
        let d = d.map(|x| snap(x, false));
        let mut moved_groups: Vec<usize> = Vec::new();
        for t in boxes {
            // an assembly (chassis) moves all its walls at once: only once
            if let Some(&(first, _)) = t.group.first() {
                if moved_groups.contains(&first) {
                    continue;
                }
                moved_groups.push(first);
            }
            let lo2 = [0, 1, 2].map(|k| t.lo[k] + d[k]);
            let hi2 = [0, 1, 2].map(|k| t.hi[k] + d[k]);
            self.apply_box(&t, lo2, hi2);
        }
        self.sel = keep;
    }

    fn counts(&self) -> [usize; 6] {
        [
            self.case.blocks.len(),
            self.case.components.len(),
            self.case.fans.len(),
            self.case.resistances.len(),
            self.case.heatsinks.len(),
            self.case.monitors.len(),
        ]
    }
}

/// Property sheet of a chassis made in the object panel (all its walls).
/// Returns true when something changed.
pub(super) fn chassis_sheet(
    ui: &mut Ui,
    blocks: &mut [ferrotherm_core::case::Block],
    i: usize,
    lib: &ferrotherm_core::Library,
) -> bool {
    let Some(pre) = blocks
        .get(i)
        .and_then(|b| chassis_prefix(&b.name))
        .map(str::to_string)
    else {
        return false;
    };
    let idx: Vec<usize> = (0..blocks.len())
        .filter(|&k| chassis_prefix(&blocks[k].name) == Some(pre.as_str()))
        .collect();
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut t = f64::INFINITY;
    for &k in &idx {
        let b = &blocks[k];
        for a in 0..3 {
            lo[a] = lo[a].min(b.min[a]);
            hi[a] = hi[a].max(b.max[a]);
        }
        t = t.min(
            (0..3)
                .map(|a| b.max[a] - b.min[a])
                .fold(f64::INFINITY, f64::min),
        );
    }
    let mut size = [0, 1, 2].map(|a| hi[a] - lo[a]);
    let mut material = blocks[idx[0]].material.clone().unwrap_or_default();
    let (lo0, size0, t0, mat0) = (lo, size, t, material.clone());
    ui.label(
        egui::RichText::new(format!("CHASSIS · {pre}"))
            .small()
            .weak(),
    );
    egui::Grid::new("chassis-sheet")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            ui.label("Position (min)");
            ui.horizontal(|ui| {
                for v in lo.iter_mut() {
                    crate::inspector::num(ui, v, 0.5, " mm");
                }
            });
            ui.end_row();
            ui.label("Size");
            ui.horizontal(|ui| {
                for v in size.iter_mut() {
                    crate::inspector::num(ui, v, 0.5, " mm");
                }
            });
            ui.end_row();
            ui.label("Wall thickness");
            crate::inspector::num(ui, &mut t, 0.1, " mm");
            ui.end_row();
            ui.label("Material (all walls)");
            crate::inspector::material_combo(ui, &format!("chmat{pre}"), &mut material, lib);
            ui.end_row();
        });
    ui.label(
        egui::RichText::new(format!(
            "{} walls. Drag the handles in the 3D view to move or resize the chassis; \
             delete a wall in the model tree to leave that side open.",
            idx.len()
        ))
        .small()
        .weak(),
    );
    let changed = lo != lo0 || size != size0 || t != t0 || material != mat0;
    if changed {
        let t = t.max(MIN_SIZE);
        let size = size.map(|x| x.max(2.0 * t + MIN_SIZE));
        let hi = [0, 1, 2].map(|a| lo[a] + size[a]);
        for &k in &idx {
            let b = &mut blocks[k];
            let side = b.name.rsplit('/').next().unwrap_or("");
            let wall = match side {
                "left" => Some((0, false)),
                "right" => Some((0, true)),
                "front" => Some((1, false)),
                "back" => Some((1, true)),
                "bottom" => Some((2, false)),
                "top" => Some((2, true)),
                _ => None,
            };
            if let Some((a, high)) = wall {
                b.min = lo;
                b.max = hi;
                if high {
                    b.min[a] = hi[a] - t;
                } else {
                    b.max[a] = lo[a] + t;
                }
            }
            if material != mat0 {
                b.material = Some(material.clone());
            }
        }
    }
    ui.separator();
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::Add;

    #[test]
    fn dropped_object_sits_on_the_face_and_chassis_resize_keeps_walls() {
        let mut app = FerroApp::with_example();
        let n = app.case.blocks.len();
        let c = app.domain_center();
        app.drop_at(
            Pick::Std(Add::Pcb),
            [c[0] + 3.0, c[1], c[2]],
            Some((2, true)),
        );
        let b = &app.case.blocks[n];
        assert!((b.min[2] - c[2]).abs() < 0.06, "{:?}", b.min);
        assert!((0.5 * (b.min[0] + b.max[0]) - c[0] - 3.0).abs() < 0.06);

        // chassis: resizing the assembly keeps the 2 mm walls
        let n = app.case.blocks.len();
        let c = app.domain_center();
        app.drop_at(Pick::Std(Add::Chassis), [c[0] + 1.0, c[1], c[2]], None);
        app.sel = Sel::Block(n);
        let t = app.manip_target().expect("target");
        assert_eq!(t.group.len(), 6);
        let mut hi = t.hi;
        hi[0] += 10.0;
        app.apply_box(&t, t.lo, hi);
        for (k, _) in &t.group {
            let b = &app.case.blocks[*k];
            let th = (0..3)
                .map(|a| b.max[a] - b.min[a])
                .fold(f64::INFINITY, f64::min);
            assert!((th - 2.0).abs() < 1e-9, "{} {th}", b.name);
        }
        let right = app
            .case
            .blocks
            .iter()
            .find(|b| b.name.ends_with("/right"))
            .unwrap();
        assert!((right.max[0] - hi[0]).abs() < 1e-9);
    }

    #[test]
    fn component_and_fan_boxes_round_trip() {
        let mut app = FerroApp::with_example();
        app.add_pick(Pick::Std(Add::Component));
        let t = app.manip_target().expect("component");
        let mut hi = t.hi;
        hi[0] += 3.0;
        app.apply_box(&t, t.lo, hi);
        let t2 = app.manip_target().unwrap();
        assert!((t2.hi[0] - hi[0]).abs() < 1e-9);
        assert!((t2.hi[2] - t.hi[2]).abs() < 1e-9);

        app.add_pick(Pick::Std(Add::Fan));
        let t = app.manip_target().expect("fan");
        let a = (0..3).find(|&k| !t.resize[k]).unwrap();
        let k = (a + 1) % 3;
        let mut hi = t.hi;
        hi[k] += 8.0;
        app.apply_box(&t, t.lo, hi);
        let t2 = app.manip_target().unwrap();
        assert!((t2.hi[k] - t2.lo[k] - (t.hi[k] - t.lo[k] + 8.0)).abs() < 1e-9);
    }
}
