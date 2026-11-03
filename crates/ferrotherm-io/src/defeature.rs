// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CAD defeaturing on the B-rep, with OpenCascade (`BRepAlgoAPI_Defeaturing`):
//! close holes, remove bosses/pins, fillets and chamfers, drop small parts
//! (screws, nuts) and replace nearly box-shaped parts by their box — the
//! usual clean-up of a chassis before a thermal model.
//!
//! The STEP assembly is flattened: every part instance keeps its name and
//! global placement, every part definition is simplified once. The output is
//! a STEP file (AP214) that the normal STEP import reads.
//!
//! Feature recognition works on the analytic faces OCCT reports:
//! * cylinder faces → axis from two surface points and normals (the face
//!   normal points towards the axis on a hole, away from it on a boss);
//!   faces sharing radius and axis are grouped, a group spanning 360° is a
//!   hole (concave) or a boss/pin (convex), a partial cylinder is a fillet or
//!   round;
//! * coaxial cones (countersinks, chamfered pins), coaxial planar faces no
//!   larger than the circle (blind-hole bottoms, boss tops) and coaxial
//!   smaller holes (counterbores, hollow standoffs) go with their group;
//! * torus faces with a small minor radius are fillets at corners;
//! * inclined planar strips no wider than the limit are chamfers.
//!
//! A removal that OCCT cannot rebuild is skipped and reported; the part is
//! never left invalid (the original is kept if the result fails the check).

use crate::occt;
use occt_wasm::{OcctKernel, ShapeHandle};
use serde::Serialize;
use std::collections::HashMap;
use std::f64::consts::PI;

/// What to remove. `None` disables an operation.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct DefeatureOptions {
    /// Close holes (through or blind, with countersinks/counterbores) up to
    /// this diameter, mm.
    pub hole_max_diameter_mm: Option<f64>,
    /// Remove cylindrical bosses, pins and standoffs up to this diameter, mm.
    pub boss_max_diameter_mm: Option<f64>,
    /// Remove fillets and rounds up to this radius, mm.
    pub fillet_max_radius_mm: Option<f64>,
    /// Remove inclined chamfer faces up to this width, mm.
    pub chamfer_max_width_mm: Option<f64>,
    /// Drop whole parts up to this volume, mm³ (screws, nuts, washers).
    pub small_part_max_volume_mm3: Option<f64>,
    /// Replace a part by its bounding box when it fills at least this
    /// fraction of it (0–1), after the other operations.
    pub box_fill_ratio: Option<f64>,
    /// Merge coplanar/co-cylindrical faces after the removals.
    pub unify: bool,
}

impl Default for DefeatureOptions {
    fn default() -> Self {
        DefeatureOptions {
            hole_max_diameter_mm: Some(10.0),
            boss_max_diameter_mm: None,
            fillet_max_radius_mm: Some(3.0),
            chamfer_max_width_mm: Some(2.0),
            small_part_max_volume_mm3: None,
            box_fill_ratio: None,
            unify: true,
        }
    }
}

/// Result for one part definition.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PartReport {
    pub name: String,
    pub instances: usize,
    pub holes: usize,
    pub bosses: usize,
    pub fillets: usize,
    pub chamfers: usize,
    /// Features found but not removable by OCCT.
    pub skipped: usize,
    pub removed: bool,
    pub boxed: bool,
    pub faces_before: usize,
    pub faces_after: usize,
    pub volume_before_mm3: f64,
    pub volume_after_mm3: f64,
    pub note: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DefeatureReport {
    pub parts: Vec<PartReport>,
    pub instances_in: usize,
    pub instances_out: usize,
}

impl DefeatureReport {
    pub fn totals(&self) -> (usize, usize, usize, usize, usize, usize) {
        let mut t = (0, 0, 0, 0, 0, 0);
        for p in &self.parts {
            t.0 += p.holes;
            t.1 += p.bosses;
            t.2 += p.fillets;
            t.3 += p.chamfers;
            t.4 += p.removed as usize;
            t.5 += p.skipped;
        }
        t
    }
    pub fn summary(&self) -> String {
        let (h, b, f, c, r, s) = self.totals();
        let boxed = self.parts.iter().filter(|p| p.boxed).count();
        let mut out = format!(
            "{} part definitions, {} instances in, {} out: {h} holes closed, {b} bosses, {f} fillets and {c} chamfers removed, {r} small parts dropped, {boxed} parts boxed",
            self.parts.len(),
            self.instances_in,
            self.instances_out
        );
        if s > 0 {
            out += &format!(", {s} features kept (OpenCascade could not remove them)");
        }
        out
    }
}

/// Handle of a shape known by its arena id (XCAF label shapes).
fn handle(id: u32) -> ShapeHandle {
    occt_wasm::EvolutionData {
        result_id: id,
        modified: vec![],
        generated: vec![],
        deleted: vec![],
    }
    .result()
}

type M34 = [f64; 12];
const ID: M34 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];

/// a ∘ b (apply b first), 3×4 row-major.
fn compose(a: &M34, b: &M34) -> M34 {
    let mut r = [0.0; 12];
    for i in 0..3 {
        for j in 0..4 {
            let mut s = 0.0;
            for k in 0..3 {
                s += a[4 * i + k] * b[4 * k + j];
            }
            if j == 3 {
                s += a[4 * i + 3];
            }
            r[4 * i + j] = s;
        }
    }
    r
}

fn e<T: std::fmt::Display>(x: T) -> String {
    x.to_string()
}

/// A part instance of the flattened assembly.
struct Instance {
    name: String,
    proto: u32,
    /// Part definition label (instances of one definition share it).
    proto_label: i32,
    proto_name: String,
    xf: M34,
}

fn flatten(
    k: &mut OcctKernel,
    doc: occt_wasm::DocumentHandle,
    label: i32,
    xf: M34,
    inst_name: Option<String>,
    out: &mut Vec<Instance>,
    depth: usize,
) -> Result<(), String> {
    if depth > 64 {
        return Err("assembly nesting too deep".into());
    }
    let info = k.xcaf_get_label_info(doc, label).map_err(e)?;
    if info.is_component {
        let loc = k.xcaf_get_label_location(doc, label).map_err(e)?;
        let mut m = ID;
        if loc.len() == 12 {
            m.copy_from_slice(&loc);
        }
        let proto = k.xcaf_get_referred_label(doc, label).map_err(e)?;
        // OCCT shows "=>[0:1:…]" for a component without its own name
        let name = if info.name.trim().is_empty() || info.name.starts_with("=>") {
            None
        } else {
            Some(info.name.clone())
        };
        return flatten(
            k,
            doc,
            proto,
            compose(&xf, &m),
            name.or(inst_name),
            out,
            depth + 1,
        );
    }
    if info.is_assembly {
        for c in k.xcaf_get_child_labels(doc, label).map_err(e)? {
            flatten(k, doc, c, xf, None, out, depth + 1)?;
        }
        return Ok(());
    }
    if info.shape_id != 0 {
        out.push(Instance {
            name: inst_name.unwrap_or_else(|| info.name.clone()),
            proto: info.shape_id,
            proto_label: label,
            proto_name: info.name.clone(),
            xf,
        });
    }
    Ok(())
}

type V = [f64; 3];
fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V, b: V) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V, b: V) -> V {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V) -> f64 {
    dot(a, a).sqrt()
}
fn v3(x: &[f64]) -> V {
    [x[0], x[1], x[2]]
}
/// Distance of p from the line (a, unit d).
fn line_dist(p: V, a: V, d: V) -> f64 {
    let w = sub(p, a);
    let t = dot(w, d);
    norm(sub(w, [d[0] * t, d[1] * t, d[2] * t]))
}

#[derive(Clone)]
struct Cyl {
    face: ShapeHandle,
    r: f64,
    concave: bool,
    a: V,
    d: V,
    span: f64,
}

#[derive(Clone)]
struct Face {
    face: ShapeHandle,
    kind: String,
    area: f64,
    centroid: V,
    normal: V,
    bbox_diag: f64,
    bbox_max: f64,
}

fn face_info(k: &mut OcctKernel, f: ShapeHandle) -> Option<Face> {
    let kind = k.surface_type(f).ok()?;
    let uv = k.uv_bounds(f).ok()?;
    let (um, vm) = (0.5 * (uv[0] + uv[1]), 0.5 * (uv[2] + uv[3]));
    let n = k.surface_normal(f, um, vm).ok()?;
    let c = k.get_surface_center_of_mass(f).ok()?;
    let bb = k.get_bounding_box(f, false).ok()?;
    let ext = [
        bb.max.x - bb.min.x,
        bb.max.y - bb.min.y,
        bb.max.z - bb.min.z,
    ];
    Some(Face {
        face: f,
        kind,
        area: k.get_surface_area(f).ok()?,
        centroid: v3(&c),
        normal: v3(&n),
        bbox_diag: norm(ext),
        bbox_max: ext.iter().cloned().fold(0.0, f64::max),
    })
}

fn cyl_info(k: &mut OcctKernel, f: ShapeHandle) -> Option<Cyl> {
    let r = *k.get_face_cylinder_data(f).ok()?.first()?;
    if r.is_nan() || r <= 0.0 {
        return None;
    }
    let uv = k.uv_bounds(f).ok()?;
    let span = uv[1] - uv[0];
    if span < 1e-4 {
        return None;
    }
    let vm = 0.5 * (uv[2] + uv[3]);
    let (ua, ub) = (uv[0] + 0.2 * span, uv[0] + 0.45 * span);
    let p1 = v3(&k.point_on_surface(f, ua, vm).ok()?);
    let n1 = v3(&k.surface_normal(f, ua, vm).ok()?);
    let p2 = v3(&k.point_on_surface(f, ub, vm).ok()?);
    let n2 = v3(&k.surface_normal(f, ub, vm).ok()?);
    let c = cross(n1, n2);
    let cn = norm(c);
    if cn < 1e-6 {
        return None;
    }
    let d = [c[0] / cn, c[1] / cn, c[2] / cn];
    let tol = 1e-4 * r.max(1.0);
    for concave in [true, false] {
        let s = if concave { r } else { -r };
        let a = [p1[0] + s * n1[0], p1[1] + s * n1[1], p1[2] + s * n1[2]];
        if (line_dist(p2, a, d) - r).abs() < tol {
            return Some(Cyl {
                face: f,
                r,
                concave,
                a,
                d,
                span,
            });
        }
    }
    None
}

/// A recognised feature: the faces OCCT must remove together.
#[derive(Clone)]
struct Feature {
    kind: Kind,
    faces: Vec<ShapeHandle>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Hole,
    Boss,
    Fillet,
    Chamfer,
}

fn analyse(k: &mut OcctKernel, s: ShapeHandle, o: &DefeatureOptions) -> Vec<Feature> {
    let faces = k.get_sub_shapes(s, "face").unwrap_or_default();
    let mut infos: Vec<Face> = faces.iter().filter_map(|f| face_info(k, *f)).collect();
    infos.dedup_by(|a, b| a.face == b.face);
    let mut cyls: Vec<Cyl> = infos
        .iter()
        .filter(|f| f.kind == "cylinder")
        .filter_map(|f| cyl_info(k, f.face))
        .collect();
    // groups of faces of the same cylinder
    let mut groups: Vec<Vec<Cyl>> = Vec::new();
    for c in cyls.drain(..) {
        let tol = 1e-4 * c.r.max(1.0);
        match groups.iter_mut().find(|g| {
            let h = &g[0];
            (h.r - c.r).abs() < 1e-6 * c.r.max(1.0)
                && h.concave == c.concave
                && dot(h.d, c.d).abs() > 1.0 - 1e-6
                && line_dist(c.a, h.a, h.d) < tol
        }) {
            Some(g) => g.push(c),
            None => groups.push(vec![c]),
        }
    }
    let full = |g: &Vec<Cyl>| g.iter().map(|c| c.span).sum::<f64>() > 2.0 * PI - 1e-3;
    let mut used: std::collections::HashSet<u32> = Default::default();
    let mut out = Vec::new();
    // coaxial helpers
    let coaxial_extra = |g: &Vec<Cyl>, rmax: f64, used: &std::collections::HashSet<u32>| {
        let (a, d) = (g[0].a, g[0].d);
        let tol = 1e-3 * rmax.max(1.0);
        let mut v = Vec::new();
        for f in &infos {
            if used.contains(&f.face.id()) {
                continue;
            }
            let on_axis = line_dist(f.centroid, a, d) < tol;
            if !on_axis {
                continue;
            }
            let keep = match f.kind.as_str() {
                "plane" => {
                    dot(f.normal, d).abs() > 1.0 - 1e-6 && f.area <= PI * rmax * rmax * 1.001
                }
                "cone" => f.bbox_max <= 4.0 * rmax,
                _ => false,
            };
            if keep {
                v.push(f.face);
            }
        }
        v
    };
    // holes (largest first, so counterbores take their smaller steps)
    if let Some(dmax) = o.hole_max_diameter_mm {
        let mut idx: Vec<usize> = (0..groups.len())
            .filter(|&i| groups[i][0].concave && full(&groups[i]) && 2.0 * groups[i][0].r <= dmax)
            .collect();
        idx.sort_by(|&x, &y| groups[y][0].r.partial_cmp(&groups[x][0].r).unwrap());
        for i in idx {
            let g = &groups[i];
            if g.iter().any(|c| used.contains(&c.face.id())) {
                continue;
            }
            let mut fs: Vec<ShapeHandle> = g.iter().map(|c| c.face).collect();
            // smaller coaxial holes (stepped holes)
            for (j, h) in groups.iter().enumerate() {
                if j != i
                    && h[0].concave
                    && full(h)
                    && h[0].r < g[0].r
                    && dot(h[0].d, g[0].d).abs() > 1.0 - 1e-6
                    && line_dist(h[0].a, g[0].a, g[0].d) < 1e-4 * g[0].r.max(1.0)
                {
                    fs.extend(h.iter().map(|c| c.face));
                }
            }
            for f in &fs {
                used.insert(f.id());
            }
            let extra = coaxial_extra(g, g[0].r, &used);
            for f in &extra {
                used.insert(f.id());
            }
            fs.extend(extra);
            out.push(Feature {
                kind: Kind::Hole,
                faces: fs,
            });
        }
    }
    if let Some(dmax) = o.boss_max_diameter_mm {
        for i in 0..groups.len() {
            let g = &groups[i];
            if g[0].concave || !full(g) || 2.0 * g[0].r > dmax {
                continue;
            }
            if g.iter().any(|c| used.contains(&c.face.id())) {
                continue;
            }
            let mut fs: Vec<ShapeHandle> = g.iter().map(|c| c.face).collect();
            // hollow standoff: coaxial inner hole
            for (j, h) in groups.iter().enumerate() {
                if j != i
                    && h[0].concave
                    && full(h)
                    && h[0].r < g[0].r
                    && dot(h[0].d, g[0].d).abs() > 1.0 - 1e-6
                    && line_dist(h[0].a, g[0].a, g[0].d) < 1e-4 * g[0].r.max(1.0)
                    && !h.iter().any(|c| used.contains(&c.face.id()))
                {
                    fs.extend(h.iter().map(|c| c.face));
                }
            }
            for f in &fs {
                used.insert(f.id());
            }
            let extra = coaxial_extra(g, g[0].r, &used);
            for f in &extra {
                used.insert(f.id());
            }
            fs.extend(extra);
            out.push(Feature {
                kind: Kind::Boss,
                faces: fs,
            });
        }
    }
    if let Some(rmax) = o.fillet_max_radius_mm {
        for g in &groups {
            if full(g) || g[0].r > rmax {
                continue;
            }
            for c in g {
                if used.insert(c.face.id()) {
                    out.push(Feature {
                        kind: Kind::Fillet,
                        faces: vec![c.face],
                    });
                }
            }
        }
        for f in &infos {
            if f.kind != "torus" || used.contains(&f.face.id()) {
                continue;
            }
            let Ok(uv) = k.uv_bounds(f.face) else {
                continue;
            };
            let Ok(kv) = k.surface_curvature(f.face, 0.5 * (uv[0] + uv[1]), 0.5 * (uv[2] + uv[3]))
            else {
                continue;
            };
            let kmax = kv.iter().take(2).fold(0.0f64, |m, x| m.max(x.abs()));
            if kmax > 0.0 && 1.0 / kmax <= rmax * (1.0 + 1e-6) {
                used.insert(f.face.id());
                out.push(Feature {
                    kind: Kind::Fillet,
                    faces: vec![f.face],
                });
            }
        }
    }
    if let Some(wmax) = o.chamfer_max_width_mm {
        for f in &infos {
            if f.kind != "plane" || used.contains(&f.face.id()) {
                continue;
            }
            let inclined = f.normal.iter().all(|c| c.abs() < 0.995);
            if !inclined || f.bbox_diag <= 0.0 {
                continue;
            }
            let width = f.area / f.bbox_diag;
            if width <= wmax && f.bbox_diag >= 3.0 * width {
                used.insert(f.face.id());
                out.push(Feature {
                    kind: Kind::Chamfer,
                    faces: vec![f.face],
                });
            }
        }
    }
    out
}

/// Geometric signature of a face, to find it again after a removal.
fn signature(k: &mut OcctKernel, f: ShapeHandle) -> Option<(String, V, f64)> {
    let i = face_info(k, f)?;
    Some((i.kind, i.centroid, i.area))
}

fn find_face(k: &mut OcctKernel, s: ShapeHandle, sig: &(String, V, f64)) -> Option<ShapeHandle> {
    let faces = k.get_sub_shapes(s, "face").ok()?;
    for f in faces {
        if let Some(i) = signature(k, f) {
            if i.0 == sig.0
                && norm(sub(i.1, sig.1)) < 1e-6 * (1.0 + norm(sig.1))
                && (i.2 - sig.2).abs() < 1e-6 * sig.2.max(1.0)
            {
                return Some(f);
            }
        }
    }
    None
}

fn healthy(k: &mut OcctKernel, s: ShapeHandle) -> bool {
    k.is_valid(s).unwrap_or(false) && k.get_volume(s).map(|v| v > 0.0).unwrap_or(false)
}

/// Remove the features from one part definition.
fn simplify_part(
    k: &mut OcctKernel,
    s: ShapeHandle,
    o: &DefeatureOptions,
    rep: &mut PartReport,
) -> ShapeHandle {
    rep.faces_before = k.sub_shape_count(s, "face").unwrap_or(0).max(0) as usize;
    rep.volume_before_mm3 = k.get_volume(s).unwrap_or(0.0);
    let feats = analyse(k, s, o);
    let mut cur = s;
    if !feats.is_empty() {
        let all: Vec<ShapeHandle> = feats.iter().flat_map(|f| f.faces.clone()).collect();
        let done_all = match k.defeature(s, &all, 1e-4) {
            Ok(r) if healthy(k, r) => {
                cur = r;
                true
            }
            _ => false,
        };
        let mut ok = vec![done_all; feats.len()];
        if !done_all {
            // one feature at a time, finding its faces again on the
            // current shape; chamfers and fillets before holes and bosses
            let sigs: Vec<Vec<(String, V, f64)>> = feats
                .iter()
                .map(|f| f.faces.iter().filter_map(|x| signature(k, *x)).collect())
                .collect();
            let mut order: Vec<usize> = (0..feats.len()).collect();
            order.sort_by_key(|&i| match feats[i].kind {
                Kind::Chamfer => 0,
                Kind::Fillet => 1,
                Kind::Hole => 2,
                Kind::Boss => 3,
            });
            for i in order {
                let faces: Option<Vec<ShapeHandle>> =
                    sigs[i].iter().map(|sg| find_face(k, cur, sg)).collect();
                let Some(faces) = faces else {
                    continue;
                };
                if faces.is_empty() {
                    continue;
                }
                if let Ok(r) = k.defeature(cur, &faces, 1e-4) {
                    if healthy(k, r) {
                        cur = r;
                        ok[i] = true;
                    }
                }
            }
        }
        for (f, done) in feats.iter().zip(&ok) {
            if !done {
                rep.skipped += 1;
                continue;
            }
            match f.kind {
                Kind::Hole => rep.holes += 1,
                Kind::Boss => rep.bosses += 1,
                Kind::Fillet => rep.fillets += 1,
                Kind::Chamfer => rep.chamfers += 1,
            }
        }
    }
    if o.unify && cur != s {
        if let Ok(u) = k.unify_same_domain(cur) {
            if healthy(k, u) {
                cur = u;
            }
        }
    }
    if let Some(ratio) = o.box_fill_ratio {
        if let (Ok(bb), Ok(v)) = (k.get_bounding_box(cur, false), k.get_volume(cur)) {
            let bv = (bb.max.x - bb.min.x) * (bb.max.y - bb.min.y) * (bb.max.z - bb.min.z);
            if bv > 0.0 && v / bv >= ratio {
                if let Ok(b) = k.make_box_from_corners(
                    bb.min.x, bb.min.y, bb.min.z, bb.max.x, bb.max.y, bb.max.z,
                ) {
                    cur = b;
                    rep.boxed = true;
                }
            }
        }
    }
    rep.faces_after = k.sub_shape_count(cur, "face").unwrap_or(0).max(0) as usize;
    rep.volume_after_mm3 = k.get_volume(cur).unwrap_or(0.0);
    cur
}

/// Simplify a STEP file. Returns the simplified STEP text and a report.
pub fn defeature_step(
    text: &str,
    o: &DefeatureOptions,
) -> Result<(String, DefeatureReport), String> {
    let mut k = occt::kernel()?;
    let doc = k.xcaf_import_step(text).map_err(e)?;
    let mut inst: Vec<Instance> = Vec::new();
    for root in k.xcaf_get_root_labels(doc).map_err(e)? {
        flatten(&mut k, doc, root, ID, None, &mut inst, 0)?;
    }
    if inst.is_empty() {
        return Err("no solids found in the STEP file".into());
    }
    let mut report = DefeatureReport {
        instances_in: inst.len(),
        ..Default::default()
    };
    let mut done: HashMap<i32, (Option<ShapeHandle>, usize)> = HashMap::new();
    // Instance names: XCAF keeps the STEP occurrence names only for the
    // glTF export used by the normal import, so take them from there,
    // matching every instance by its placed bounding box.
    let named = occt::read_step(text, 0.05).ok();
    let mut taken = vec![false; named.as_ref().map(|m| m.bodies.len()).unwrap_or(0)];
    for i in inst.iter_mut() {
        let Some(m) = &named else {
            break;
        };
        let s = handle(i.proto);
        let placed = if i.xf == ID {
            s
        } else {
            match k.transform(s, &i.xf) {
                Ok(p) => p,
                Err(_) => continue,
            }
        };
        let Ok(bb) = k.get_bounding_box(placed, false) else {
            continue;
        };
        let (lo, hi) = (
            [bb.min.x, bb.min.y, bb.min.z],
            [bb.max.x, bb.max.y, bb.max.z],
        );
        let size = norm(sub(hi, lo));
        let tol = 0.1 + 1e-3 * size;
        let best = m
            .bodies
            .iter()
            .enumerate()
            .filter(|(j, _)| !taken[*j])
            .map(|(j, b)| {
                let d = (0..3)
                    .map(|q| (b.min[q] - lo[q]).abs().max((b.max[q] - hi[q]).abs()))
                    .fold(0.0, f64::max);
                (j, d)
            })
            .filter(|(_, d)| *d < tol)
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        if let Some((j, _)) = best {
            taken[j] = true;
            if !m.bodies[j].name.trim().is_empty() {
                i.name = m.bodies[j].name.clone();
            }
        }
    }
    let out = k.xcaf_new_document().map_err(e)?;
    let mut names: HashMap<String, usize> = HashMap::new();
    for i in &inst {
        let entry = match done.get(&i.proto_label) {
            Some(x) => *x,
            None => {
                let s = handle(i.proto);
                let mut rep = PartReport {
                    name: if i.proto_name.is_empty() {
                        i.name.clone()
                    } else {
                        i.proto_name.clone()
                    },
                    ..Default::default()
                };
                let vol = k.get_volume(s).unwrap_or(0.0);
                let shape = if o.small_part_max_volume_mm3.is_some_and(|v| vol <= v) {
                    rep.removed = true;
                    rep.volume_before_mm3 = vol;
                    rep.note = "small part dropped".into();
                    None
                } else {
                    Some(simplify_part(&mut k, s, o, &mut rep))
                };
                report.parts.push(rep);
                let x = (shape, report.parts.len() - 1);
                done.insert(i.proto_label, x);
                x
            }
        };
        report.parts[entry.1].instances += 1;
        let Some(shape) = entry.0 else {
            continue;
        };
        let placed = if i.xf == ID {
            shape
        } else {
            k.transform(shape, &i.xf).map_err(e)?
        };
        let l = k.xcaf_add_shape(out, placed).map_err(e)?;
        // keep instance names unique
        let n = names.entry(i.name.clone()).or_insert(0);
        *n += 1;
        let name = if *n == 1 {
            i.name.clone()
        } else {
            format!("{}_{}", i.name, n)
        };
        let _ = k.xcaf_set_name(out, l, &name);
        report.instances_out += 1;
    }
    let step = k.xcaf_export_step(out).map_err(e)?;
    let _ = k.xcaf_close(out);
    let _ = k.xcaf_close(doc);
    Ok((step, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plate with a through hole, a blind hole, a countersunk hole, a boss
    /// and filleted vertical edges, plus a screw-sized cylinder.
    fn sample(k: &mut OcctKernel) -> (String, f64) {
        let plate = k.make_box(100.0, 60.0, 4.0).unwrap();
        let edges = k.get_sub_shapes(plate, "edge").unwrap();
        // fillet the 4 vertical edges (length 4 along z)
        let vert: Vec<ShapeHandle> = edges
            .into_iter()
            .filter(|e| {
                let b = k.get_bounding_box(*e, false).unwrap();
                (b.max.z - b.min.z) > 3.9 && (b.max.x - b.min.x) < 1e-6
            })
            .collect();
        assert_eq!(vert.len(), 4);
        let mut s = k.fillet(plate, &vert, 2.0).unwrap();
        let cut = |k: &mut OcctKernel, s, r: f64, x: f64, y: f64, z0: f64, h: f64| {
            let c = k.make_cylinder(r, h).unwrap();
            let c = k.translate(c, x, y, z0).unwrap();
            k.cut(s, c).unwrap()
        };
        s = cut(k, s, 3.0, 20.0, 20.0, -1.0, 10.0); // through hole d 6
        s = cut(k, s, 2.0, 40.0, 20.0, 2.0, 10.0); // blind hole d 4, depth 2
        s = cut(k, s, 1.5, 60.0, 20.0, -1.0, 10.0); // counterbored hole d 3 …
        s = cut(k, s, 3.0, 60.0, 20.0, 2.5, 10.0); // … with d 6 counterbore
        let boss = k.make_cylinder(4.0, 6.0).unwrap();
        let boss = k.translate(boss, 80.0, 40.0, 4.0).unwrap();
        s = k.fuse(s, boss).unwrap();
        s = k.unify_same_domain(s).unwrap();
        let screw = k.make_cylinder(1.5, 8.0).unwrap();
        let screw = k.translate(screw, 150.0, 0.0, 0.0).unwrap();
        let doc = k.xcaf_new_document().unwrap();
        let l = k.xcaf_add_shape(doc, s).unwrap();
        k.xcaf_set_name(doc, l, "CHASSIS").unwrap();
        let l = k.xcaf_add_shape(doc, screw).unwrap();
        k.xcaf_set_name(doc, l, "SCREW").unwrap();
        let v = k.get_volume(s).unwrap();
        (k.xcaf_export_step(doc).unwrap(), v)
    }

    #[test]
    fn closes_holes_removes_boss_fillets_and_screws() {
        let mut k = occt::kernel().unwrap();
        let (step, v0) = sample(&mut k);
        if let Some(p) = std::env::var_os("FT_DUMP_STEP") {
            std::fs::write(p, &step).unwrap();
        }
        let o = DefeatureOptions {
            hole_max_diameter_mm: Some(8.0),
            boss_max_diameter_mm: Some(10.0),
            fillet_max_radius_mm: Some(2.5),
            chamfer_max_width_mm: None,
            small_part_max_volume_mm3: Some(100.0),
            box_fill_ratio: None,
            unify: true,
        };
        let (out, rep) = defeature_step(&step, &o).unwrap();
        eprintln!("{}", rep.summary());
        for p in &rep.parts {
            eprintln!(
                "  {}: holes {} bosses {} fillets {} skipped {} faces {}→{} volume {:.2}→{:.2}",
                p.name,
                p.holes,
                p.bosses,
                p.fillets,
                p.skipped,
                p.faces_before,
                p.faces_after,
                p.volume_before_mm3,
                p.volume_after_mm3
            );
        }
        let ch = rep.parts.iter().find(|p| p.name == "CHASSIS").unwrap();
        assert_eq!(ch.holes, 3, "through, blind and stepped hole");
        assert_eq!(ch.bosses, 1);
        assert_eq!(ch.fillets, 4);
        assert_eq!(ch.skipped, 0);
        // a plain 100 × 60 × 4 box remains
        assert!(
            (ch.volume_after_mm3 - 24000.0).abs() < 1e-3,
            "{}",
            ch.volume_after_mm3
        );
        assert!((ch.volume_before_mm3 - v0).abs() < 1e-6);
        assert_eq!(ch.faces_after, 6);
        assert!(
            rep.parts
                .iter()
                .find(|p| p.name == "SCREW")
                .unwrap()
                .removed
        );
        assert_eq!(rep.instances_out, 1);
        // the STEP output reads back with the instance name
        let m = crate::step::read(&out, 0.1).unwrap();
        assert_eq!(m.bodies.len(), 1);
        assert_eq!(m.bodies[0].name, "CHASSIS");
        assert!((m.bodies[0].volume() - 24000.0).abs() < 1.0);
    }

    /// Chassis shell (120 × 80 × 40, 2 mm walls) with filleted vertical
    /// edges, a vent pattern of 12 Ø4 holes in a side wall and 4 Ø3 screw
    /// holes in the floor: everything closed gives the plain shell.
    #[test]
    fn chassis_shell_vents_and_screw_holes() {
        let mut k = occt::kernel().unwrap();
        let outer = k.make_box(120.0, 80.0, 40.0).unwrap();
        let edges = k.get_sub_shapes(outer, "edge").unwrap();
        let vert: Vec<ShapeHandle> = edges
            .into_iter()
            .filter(|e| {
                let b = k.get_bounding_box(*e, false).unwrap();
                (b.max.z - b.min.z) > 39.0
            })
            .collect();
        let outer = k.fillet(outer, &vert, 3.0).unwrap();
        let inner = k
            .make_box_from_corners(2.0, 2.0, 2.0, 118.0, 78.0, 42.0)
            .unwrap();
        let mut s = k.cut(outer, inner).unwrap();
        // vents in the x = 0 wall: 4 × 3 holes along y/z, axis x
        let mut tools = Vec::new();
        for i in 0..4 {
            for j in 0..3 {
                let c = k.make_cylinder(2.0, 10.0).unwrap();
                // cylinder along z → rotate to x by a transform
                let m = [
                    0.0,
                    0.0,
                    1.0,
                    -4.0,
                    0.0,
                    1.0,
                    0.0,
                    20.0 + 12.0 * i as f64,
                    -1.0,
                    0.0,
                    0.0,
                    12.0 + 8.0 * j as f64,
                ];
                tools.push(k.transform(c, &m).unwrap());
            }
        }
        for (x, y) in [(10.0, 10.0), (110.0, 10.0), (10.0, 70.0), (110.0, 70.0)] {
            let c = k.make_cylinder(1.5, 10.0).unwrap();
            tools.push(k.translate(c, x, y, -1.0).unwrap());
        }
        for t in tools {
            s = k.cut(s, t).unwrap();
        }
        let v_holes = k.get_volume(s).unwrap();
        let doc = k.xcaf_new_document().unwrap();
        let l = k.xcaf_add_shape(doc, s).unwrap();
        k.xcaf_set_name(doc, l, "HOUSING").unwrap();
        let step = k.xcaf_export_step(doc).unwrap();
        let o = DefeatureOptions {
            hole_max_diameter_mm: Some(5.0),
            boss_max_diameter_mm: None,
            fillet_max_radius_mm: Some(3.0),
            chamfer_max_width_mm: None,
            small_part_max_volume_mm3: None,
            box_fill_ratio: None,
            unify: true,
        };
        let (_, rep) = defeature_step(&step, &o).unwrap();
        let p = &rep.parts[0];
        eprintln!(
            "HOUSING: holes {} fillets {} skipped {} faces {}→{} volume {:.2}→{:.2}",
            p.holes,
            p.fillets,
            p.skipped,
            p.faces_before,
            p.faces_after,
            v_holes,
            p.volume_after_mm3
        );
        assert_eq!(p.holes, 16);
        assert_eq!(p.fillets, 4);
        assert_eq!(p.skipped, 0);
        let shell = 120.0 * 80.0 * 40.0 - 116.0 * 76.0 * 38.0;
        assert!(
            (p.volume_after_mm3 - shell).abs() < 1e-3,
            "{}",
            p.volume_after_mm3
        );
        assert_eq!(p.faces_after, 11, "5 outer + 5 inner faces + rim");
    }

    #[test]
    fn keeps_what_is_not_selected() {
        let mut k = occt::kernel().unwrap();
        let (step, v0) = sample(&mut k);
        // only holes up to 4 mm: the 6 mm through hole and the 6 mm
        // counterbore stay, as do boss and fillets
        let o = DefeatureOptions {
            hole_max_diameter_mm: Some(4.0),
            boss_max_diameter_mm: None,
            fillet_max_radius_mm: None,
            chamfer_max_width_mm: None,
            small_part_max_volume_mm3: None,
            box_fill_ratio: None,
            unify: true,
        };
        let (_, rep) = defeature_step(&step, &o).unwrap();
        let ch = rep.parts.iter().find(|p| p.name == "CHASSIS").unwrap();
        // the blind d 4 hole and the d 3 step under the d 6 counterbore
        assert_eq!((ch.holes, ch.bosses, ch.fillets), (2, 0, 0));
        let filled = PI * 4.0 * 2.0 + PI * 2.25 * 2.5;
        assert!((ch.volume_after_mm3 - v0 - filled).abs() < 1e-3);
        assert_eq!(rep.instances_out, 2);
    }

    #[test]
    fn chamfers_and_boxing() {
        let mut k = occt::kernel().unwrap();
        let b = k.make_box(50.0, 30.0, 10.0).unwrap();
        let edges = k.get_sub_shapes(b, "edge").unwrap();
        let top: Vec<ShapeHandle> = edges
            .into_iter()
            .filter(|e| {
                let bb = k.get_bounding_box(*e, false).unwrap();
                (bb.min.z - 10.0).abs() < 1e-9 && (bb.max.z - 10.0).abs() < 1e-9
            })
            .collect();
        let c = k.chamfer(b, &top, 1.0).unwrap();
        let doc = k.xcaf_new_document().unwrap();
        let l = k.xcaf_add_shape(doc, c).unwrap();
        k.xcaf_set_name(doc, l, "COVER").unwrap();
        let step = k.xcaf_export_step(doc).unwrap();
        let o = DefeatureOptions {
            hole_max_diameter_mm: None,
            boss_max_diameter_mm: None,
            fillet_max_radius_mm: None,
            chamfer_max_width_mm: Some(2.0),
            small_part_max_volume_mm3: None,
            box_fill_ratio: None,
            unify: true,
        };
        let (_, rep) = defeature_step(&step, &o).unwrap();
        let p = &rep.parts[0];
        eprintln!(
            "chamfers {} skipped {} vol {}",
            p.chamfers, p.skipped, p.volume_after_mm3
        );
        assert_eq!(p.chamfers, 4);
        assert!((p.volume_after_mm3 - 15000.0).abs() < 1e-3);
        // boxing instead: the chamfered cover fills > 95 % of its box
        let o = DefeatureOptions {
            chamfer_max_width_mm: None,
            box_fill_ratio: Some(0.95),
            ..o
        };
        let (_, rep) = defeature_step(&step, &o).unwrap();
        assert!(rep.parts[0].boxed);
        assert!((rep.parts[0].volume_after_mm3 - 15000.0).abs() < 1e-3);
    }

    #[test]
    fn assembly_instances_keep_names_and_placement() {
        let mut k = occt::kernel().unwrap();
        // one plate definition with a hole, used twice at different places
        let p = k.make_box(20.0, 10.0, 2.0).unwrap();
        let h = k.make_cylinder(2.0, 5.0).unwrap();
        let h = k.translate(h, 10.0, 5.0, -1.0).unwrap();
        let p = k.cut(p, h).unwrap();
        let asm = k.make_compound(&[]).unwrap();
        let doc = k.xcaf_new_document().unwrap();
        let root = k.xcaf_add_assembly(doc, asm).unwrap();
        k.xcaf_set_name(doc, root, "ASM").unwrap();
        let c1 = k
            .xcaf_add_component(doc, root, p, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
            .unwrap();
        k.xcaf_set_name(doc, c1, "LEFT").unwrap();
        let c2 = k
            .xcaf_add_component(doc, root, p, 100.0, 50.0, 10.0, 0.0, 0.0, 0.0)
            .unwrap();
        k.xcaf_set_name(doc, c2, "RIGHT").unwrap();
        let step = k.xcaf_export_step(doc).unwrap();
        let before = crate::step::read(&step, 0.1).unwrap();
        let o = DefeatureOptions {
            hole_max_diameter_mm: Some(5.0),
            ..Default::default()
        };
        let (out, rep) = defeature_step(&step, &o).unwrap();
        eprintln!("{}", rep.summary());
        let after = crate::step::read(&out, 0.1).unwrap();
        assert_eq!(after.bodies.len(), before.bodies.len());
        for b in &before.bodies {
            let a = after
                .bodies
                .iter()
                .find(|x| x.name == b.name)
                .unwrap_or_else(|| panic!("{} missing", b.name));
            for q in 0..3 {
                assert!((a.min[q] - b.min[q]).abs() < 1e-3 && (a.max[q] - b.max[q]).abs() < 1e-3);
            }
            assert!((a.volume() - 400.0).abs() < 0.5, "{}", a.volume());
        }
        // one part definition, simplified once, used twice
        assert_eq!(rep.parts.len(), 1);
        assert_eq!(rep.parts[0].instances, 2);
        assert_eq!(rep.parts[0].holes, 1);
    }
}
