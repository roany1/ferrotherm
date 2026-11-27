// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Close the holes left by removed fasteners (OpenCascade).
//!
//! For every part that stays, the holes whose axis passes through a removed
//! fastener (its placed bounding box) and whose diameter fits that fastener
//! are removed from the solid with `BRepAlgoAPI_Defeaturing`: the faces
//! around the hole are extended over it, so the hole is filled with the
//! part's own solid — same body, hence same material — and the plate keeps
//! its thickness and outline. Countersinks and counterbores go with their
//! hole. A part whose holes cannot be rebuilt is left as it was.
//!
//! The new solids are returned as small STEP texts (one solid each, in the
//! file's length unit) for [`crate::fasteners`] to splice into the original
//! file in place of the old solid.

use crate::defeature::{
    analyse, e, flatten, handle, healthy, DefeatureOptions, Instance, Kind, ID, M34, V,
};
use crate::occt;
use occt_wasm::{OcctKernel, ShapeHandle};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Holes closed in one part definition.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HoleFill {
    pub part: String,
    pub holes: usize,
    /// Holes found under a fastener but not rebuildable by OCCT.
    pub skipped: usize,
    pub volume_added_mm3: f64,
    pub note: String,
}

/// Inverse of a 3×4 affine transform.
fn inverse(m: &M34) -> M34 {
    let a = [[m[0], m[1], m[2]], [m[4], m[5], m[6]], [m[8], m[9], m[10]]];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    let d = if det.abs() < 1e-12 { 1.0 } else { det };
    let inv = [
        [
            (a[1][1] * a[2][2] - a[1][2] * a[2][1]) / d,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) / d,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) / d,
        ],
        [
            (a[1][2] * a[2][0] - a[1][0] * a[2][2]) / d,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) / d,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) / d,
        ],
        [
            (a[1][0] * a[2][1] - a[1][1] * a[2][0]) / d,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) / d,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) / d,
        ],
    ];
    let t = [m[3], m[7], m[11]];
    let mut r = [0.0; 12];
    for i in 0..3 {
        for j in 0..3 {
            r[4 * i + j] = inv[i][j];
        }
        r[4 * i + 3] = -(inv[i][0] * t[0] + inv[i][1] * t[1] + inv[i][2] * t[2]);
    }
    r
}

fn apply(m: &M34, p: V) -> V {
    [0, 1, 2].map(|i| m[4 * i] * p[0] + m[4 * i + 1] * p[1] + m[4 * i + 2] * p[2] + m[4 * i + 3])
}

type Bx = (V, V);

fn placed_box(k: &mut OcctKernel, i: &Instance) -> Option<Bx> {
    let s = handle(i.proto);
    let placed = if i.xf == ID {
        s
    } else {
        k.transform(s, &i.xf).ok()?
    };
    let bb = k.get_bounding_box(placed, false).ok()?;
    Some((
        [bb.min.x, bb.min.y, bb.min.z],
        [bb.max.x, bb.max.y, bb.max.z],
    ))
}

/// Axis-aligned box of a box after a transform (the 8 corners).
fn transform_box(m: &M34, b: &Bx) -> Bx {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for c in 0..8 {
        let p = [
            if c & 1 == 0 { b.0[0] } else { b.1[0] },
            if c & 2 == 0 { b.0[1] } else { b.1[1] },
            if c & 4 == 0 { b.0[2] } else { b.1[2] },
        ];
        let q = apply(m, p);
        for k in 0..3 {
            lo[k] = lo[k].min(q[k]);
            hi[k] = hi[k].max(q[k]);
        }
    }
    (lo, hi)
}

fn overlap(a: &Bx, b: &Bx, m: f64) -> bool {
    (0..3).all(|k| a.0[k] - m <= b.1[k] && b.0[k] - m <= a.1[k])
}

/// Does the line p + t·d cross the box (enlarged by m)?
fn line_hits_box(p: V, d: V, b: &Bx, m: f64) -> bool {
    let (mut t0, mut t1) = (f64::NEG_INFINITY, f64::INFINITY);
    for k in 0..3 {
        let (lo, hi) = (b.0[k] - m, b.1[k] + m);
        if d[k].abs() < 1e-12 {
            if p[k] < lo || p[k] > hi {
                return false;
            }
        } else {
            let (a, c) = ((lo - p[k]) / d[k], (hi - p[k]) / d[k]);
            t0 = t0.max(a.min(c));
            t1 = t1.min(a.max(c));
        }
    }
    t0 <= t1
}

/// Largest size of a box across a direction.
fn lateral(b: &Bx, d: V) -> f64 {
    (0..3)
        .map(|k| (b.1[k] - b.0[k]) * (1.0 - d[k] * d[k]).max(0.0).sqrt())
        .fold(0.0, f64::max)
}

/// Close, in the parts that stay, the holes the removed parts went through.
/// `removed`: names of the removed part definitions (XCAF prototype names).
/// `unit_mm`: length unit of the file (the new solids are written in it).
/// Returns the new solids by part name (one-solid STEP texts) and a report.
pub fn fill_holes(
    text: &str,
    removed: &HashSet<String>,
    unit_mm: f64,
    max_diameter_mm: f64,
) -> Result<(HashMap<String, String>, Vec<HoleFill>), String> {
    let mut k = occt::kernel()?;
    let doc = k.xcaf_import_step(text).map_err(e)?;
    let mut inst: Vec<Instance> = Vec::new();
    for root in k.xcaf_get_root_labels(doc).map_err(e)? {
        flatten(&mut k, doc, root, ID, None, &mut inst, 0)?;
    }
    // placed boxes of the removed instances
    let mut gone: Vec<Bx> = Vec::new();
    for i in inst.iter().filter(|i| removed.contains(&i.proto_name)) {
        if let Some(b) = placed_box(&mut k, i) {
            gone.push(b);
        }
    }
    // per kept part definition: the removed boxes in its own frame
    let mut local: HashMap<i32, (String, u32, Vec<Bx>)> = HashMap::new();
    let mut names_seen: HashMap<String, HashSet<i32>> = HashMap::new();
    for i in inst.iter().filter(|i| !removed.contains(&i.proto_name)) {
        names_seen
            .entry(i.proto_name.clone())
            .or_default()
            .insert(i.proto_label);
        let Some(hb) = placed_box(&mut k, i) else {
            continue;
        };
        let inv = inverse(&i.xf);
        let entry = local
            .entry(i.proto_label)
            .or_insert_with(|| (i.proto_name.clone(), i.proto, Vec::new()));
        for g in &gone {
            if overlap(&hb, g, 0.5) {
                entry.2.push(transform_box(&inv, g));
            }
        }
    }
    let opts = DefeatureOptions {
        hole_max_diameter_mm: Some(max_diameter_mm),
        boss_max_diameter_mm: None,
        fillet_max_radius_mm: None,
        chamfer_max_width_mm: None,
        small_part_max_volume_mm3: None,
        box_fill_ratio: None,
        unify: true,
    };
    let mut out = HashMap::new();
    let mut report = Vec::new();
    let mut labels: Vec<i32> = local.keys().copied().collect();
    labels.sort_unstable();
    for lab in labels {
        let (name, proto, boxes) = &local[&lab];
        if boxes.is_empty() {
            continue;
        }
        let mut rep = HoleFill {
            part: name.clone(),
            ..Default::default()
        };
        if names_seen.get(name).is_some_and(|s| s.len() > 1) {
            rep.note = "several parts share this name: not changed".into();
            report.push(rep);
            continue;
        }
        let s = handle(*proto);
        let Ok(bb0) = k.get_bounding_box(s, false) else {
            continue;
        };
        let b0: Bx = (
            [bb0.min.x, bb0.min.y, bb0.min.z],
            [bb0.max.x, bb0.max.y, bb0.max.z],
        );
        let holes: Vec<(Vec<ShapeHandle>, f64)> = analyse(&mut k, s, &opts)
            .into_iter()
            .filter(|f| f.kind == Kind::Hole)
            .filter(|f| {
                let Some((a, d, r)) = f.axis else {
                    return false;
                };
                boxes
                    .iter()
                    .any(|b| line_hits_box(a, d, b, 0.3) && 2.0 * r <= 1.15 * lateral(b, d) + 0.6)
            })
            .map(|f| {
                // most material a closed hole can add: r² π × the part's
                // length along the axis
                let (_, d, r) = f.axis.unwrap_or(([0.0; 3], [0.0, 0.0, 1.0], 0.0));
                let len: f64 = (0..3).map(|q| (b0.1[q] - b0.0[q]) * d[q].abs()).sum();
                (f.faces, std::f64::consts::PI * r * r * len)
            })
            .collect();
        if holes.is_empty() {
            continue;
        }
        let v0 = k.get_volume(s).unwrap_or(0.0);
        // a closed hole never makes the part larger, and adds at most the
        // hole's own volume (OCCT sometimes extends a face far away)
        let sane = |k: &mut OcctKernel, r: ShapeHandle, v_before: f64, v_max: f64| -> bool {
            if !healthy(k, r) {
                return false;
            }
            let Ok(bb) = k.get_bounding_box(r, false) else {
                return false;
            };
            let tol = 1e-3 * (0..3).map(|q| b0.1[q] - b0.0[q]).fold(1.0, f64::max);
            let inside = [bb.min.x, bb.min.y, bb.min.z]
                .iter()
                .zip(&b0.0)
                .all(|(a, b)| *a >= b - tol)
                && [bb.max.x, bb.max.y, bb.max.z]
                    .iter()
                    .zip(&b0.1)
                    .all(|(a, b)| *a <= b + tol);
            let dv = k.get_volume(r).unwrap_or(0.0) - v_before;
            inside && dv > 1e-6 && dv <= 1.05 * v_max + 1e-6
        };
        let all: Vec<ShapeHandle> = holes.iter().flat_map(|h| h.0.clone()).collect();
        let v_all: f64 = holes.iter().map(|h| h.1).sum();
        let mut cur = s;
        match k.defeature(s, &all, 1e-4) {
            Ok(r) if sane(&mut k, r, v0, v_all) => {
                cur = r;
                rep.holes = holes.len();
            }
            _ => {
                // one hole at a time (faces found again by position)
                for (h, vmax) in &holes {
                    let sig: Vec<(V, f64)> = h
                        .iter()
                        .filter_map(|f| {
                            let c = k.get_surface_center_of_mass(*f).ok()?;
                            Some(([c[0], c[1], c[2]], k.get_surface_area(*f).ok()?))
                        })
                        .collect();
                    let faces = k.get_sub_shapes(cur, "face").unwrap_or_default();
                    let mut found = Vec::new();
                    for f in faces {
                        let (Ok(c), Ok(a)) =
                            (k.get_surface_center_of_mass(f), k.get_surface_area(f))
                        else {
                            continue;
                        };
                        if sig.iter().any(|(p, ar)| {
                            (p[0] - c[0]).abs() + (p[1] - c[1]).abs() + (p[2] - c[2]).abs() < 1e-6
                                && (ar - a).abs() < 1e-6 * ar.max(1.0)
                        }) {
                            found.push(f);
                        }
                    }
                    let vb = k.get_volume(cur).unwrap_or(0.0);
                    match k.defeature(cur, &found, 1e-4) {
                        Ok(r) if !found.is_empty() && sane(&mut k, r, vb, *vmax) => {
                            cur = r;
                            rep.holes += 1;
                        }
                        _ => rep.skipped += 1,
                    }
                }
            }
        }
        if rep.holes == 0 {
            rep.note = "OpenCascade could not close these holes: part not changed".into();
            report.push(rep);
            continue;
        }
        if let Ok(u) = k.unify_same_domain(cur) {
            let vc = k.get_volume(cur).unwrap_or(0.0);
            if healthy(&mut k, u)
                && (k.get_volume(u).unwrap_or(0.0) - vc).abs() <= 1e-6 * vc.max(1.0)
            {
                cur = u;
            }
        }
        rep.volume_added_mm3 = k.get_volume(cur).unwrap_or(v0) - v0;
        // write the solid alone, in the file's unit
        let solid = if (unit_mm - 1.0).abs() > 1e-12 {
            k.scale(cur, 0.0, 0.0, 0.0, 1.0 / unit_mm).map_err(e)?
        } else {
            cur
        };
        let d2 = k.xcaf_new_document().map_err(e)?;
        let l = k.xcaf_add_shape(d2, solid).map_err(e)?;
        let _ = k.xcaf_set_name(d2, l, name);
        let step = k.xcaf_export_step(d2).map_err(e)?;
        let _ = k.xcaf_close(d2);
        out.insert(name.clone(), step);
        report.push(rep);
    }
    let _ = k.xcaf_close(doc);
    Ok((out, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_and_line_box() {
        let m: M34 = [
            0.0, -1.0, 0.0, 10.0, 1.0, 0.0, 0.0, 5.0, 0.0, 0.0, 1.0, -2.0,
        ];
        let p = [1.0, 2.0, 3.0];
        let q = apply(&inverse(&m), apply(&m, p));
        assert!((0..3).all(|k| (q[k] - p[k]).abs() < 1e-12));
        let b = ([0.0, 0.0, 0.0], [2.0, 2.0, 2.0]);
        assert!(line_hits_box([1.0, 1.0, -5.0], [0.0, 0.0, 1.0], &b, 0.0));
        assert!(!line_hits_box([3.0, 1.0, -5.0], [0.0, 0.0, 1.0], &b, 0.0));
        assert!((lateral(&b, [0.0, 0.0, 1.0]) - 2.0).abs() < 1e-12);
    }
}
