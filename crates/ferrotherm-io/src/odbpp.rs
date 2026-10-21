// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ODB++ (v7/v8) reader: product folder or .tgz/.zip archive.
//!
//! Read: matrix (layer rows, context, type), step profile, board/layer
//! attributes (.board_thickness, .copper_weight, .layer_dielectric), feature
//! files of signal/power layers (lines, arcs, pads with standard symbols
//! r/s/rect/oval/di/donut, surfaces with islands and holes, negative
//! polarity), drill layers, components (comp_+_top / comp_+_bot with
//! .comp_height) and package outlines from eda/data. `.z` (compress) files
//! are decoded. Coordinates in INCH or MM; symbol sizes in mils or microns.

use crate::archive::{text_of, Files};
use crate::ecad::{Board, Drill, Layer, LayerKind, Placement, Side};
use crate::geom::{arc_points, bbox, rotate, Poly, Raster, P2};
use std::collections::HashMap;

struct Tree {
    files: HashMap<String, (String, Vec<u8>)>,
    /// common top folder (product name) stripped
    root: String,
}

impl Tree {
    fn new(files: Files) -> Tree {
        // find the folder containing matrix/matrix
        let root = files
            .iter()
            .find_map(|(p, _)| {
                let l = p.to_ascii_lowercase();
                l.strip_suffix("matrix/matrix")
                    .or_else(|| l.strip_suffix("matrix/matrix.z"))
                    .map(|r| r.to_string())
            })
            .unwrap_or_default();
        let mut map = HashMap::new();
        for (p, d) in files {
            let l = p.to_ascii_lowercase();
            if let Some(rel) = l.strip_prefix(&root) {
                map.insert(rel.to_string(), (p, d));
            }
        }
        Tree { files: map, root }
    }
    fn get(&self, rel: &str) -> Option<String> {
        let rel = rel.to_ascii_lowercase();
        for cand in [rel.clone(), format!("{rel}.z")] {
            if let Some((p, d)) = self.files.get(&cand) {
                return Some(text_of(p, d));
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
struct MatrixLayer {
    row: i64,
    context: String,
    typ: String,
    name: String,
}

fn parse_matrix(text: &str) -> (Vec<String>, Vec<MatrixLayer>) {
    let mut steps = Vec::new();
    let mut layers = Vec::new();
    let mut cur: Option<(String, HashMap<String, String>)> = None;
    for line in text.lines() {
        let l = line.trim();
        if l.ends_with('{') {
            cur = Some((
                l.trim_end_matches('{').trim().to_ascii_uppercase(),
                HashMap::new(),
            ));
        } else if l == "}" {
            if let Some((kind, kv)) = cur.take() {
                if kind == "STEP" {
                    if let Some(n) = kv.get("NAME") {
                        steps.push(n.to_ascii_lowercase());
                    }
                } else if kind == "LAYER" {
                    layers.push(MatrixLayer {
                        row: kv.get("ROW").and_then(|v| v.parse().ok()).unwrap_or(0),
                        context: kv
                            .get("CONTEXT")
                            .cloned()
                            .unwrap_or_default()
                            .to_ascii_uppercase(),
                        typ: kv
                            .get("TYPE")
                            .cloned()
                            .unwrap_or_default()
                            .to_ascii_uppercase(),
                        name: kv
                            .get("NAME")
                            .cloned()
                            .unwrap_or_default()
                            .to_ascii_lowercase(),
                    });
                }
            }
        } else if let Some((k, v)) = l.split_once('=') {
            if let Some((_, kv)) = cur.as_mut() {
                kv.insert(k.trim().to_ascii_uppercase(), v.trim().to_string());
            }
        }
    }
    layers.sort_by_key(|l| l.row);
    (steps, layers)
}

/// Key=value attributes of an attrlist file.
fn attrlist(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.trim().split_once('='))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect()
}

fn units_of(text: &str) -> f64 {
    for l in text.lines().take(50) {
        let l = l.trim();
        if let Some(u) = l.strip_prefix("UNITS=").or_else(|| l.strip_prefix("U ")) {
            return if u.trim().eq_ignore_ascii_case("MM") {
                1.0
            } else {
                25.4
            };
        }
    }
    25.4 // ODB++ default unit is the inch
}

#[derive(Debug, Clone)]
enum Sym {
    Round(f64),
    Rect(f64, f64),
    Oval(f64, f64),
    Diamond(f64, f64),
    Donut(f64, f64),
    Unknown,
}

/// Parse a standard symbol name; sizes in mils (inch) or microns (mm).
fn parse_symbol(name: &str, sym_scale: f64) -> Sym {
    let n = name.to_ascii_lowercase();
    let nums = |s: &str| -> Vec<f64> {
        s.split('x')
            .map(|p| p.trim_start_matches(|c: char| c.is_ascii_alphabetic()))
            .filter_map(|p| p.parse::<f64>().ok())
            .collect()
    };
    if let Some(r) = n.strip_prefix("donut_r") {
        let v = nums(r);
        if v.len() >= 2 {
            return Sym::Donut(v[0] * sym_scale, v[1] * sym_scale);
        }
    } else if let Some(r) = n.strip_prefix("rect") {
        let v = nums(r);
        if v.len() >= 2 {
            return Sym::Rect(v[0] * sym_scale, v[1] * sym_scale);
        }
    } else if let Some(r) = n.strip_prefix("oval") {
        let v = nums(r);
        if v.len() >= 2 {
            return Sym::Oval(v[0] * sym_scale, v[1] * sym_scale);
        }
    } else if let Some(r) = n.strip_prefix("di") {
        let v = nums(r);
        if v.len() >= 2 {
            return Sym::Diamond(v[0] * sym_scale, v[1] * sym_scale);
        }
    } else if let Some(r) = n.strip_prefix('s') {
        if let Ok(v) = r.parse::<f64>() {
            return Sym::Rect(v * sym_scale, v * sym_scale);
        }
    } else if let Some(r) = n.strip_prefix('r') {
        if let Ok(v) = r.parse::<f64>() {
            return Sym::Round(v * sym_scale);
        }
    }
    Sym::Unknown
}

/// Surface records (OB/OS/OC/OE) → contours.
fn surface_contours(lines: &[&str], scale: f64) -> Vec<Poly> {
    let mut out = Vec::new();
    let mut cur: Vec<P2> = Vec::new();
    for l in lines {
        let f: Vec<&str> = l.split_whitespace().collect();
        let num = |k: usize| f.get(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) * scale;
        match f.first().copied() {
            Some("OB") => {
                cur.clear();
                cur.push([num(1), num(2)]);
            }
            Some("OS") => cur.push([num(1), num(2)]),
            Some("OC") => {
                let end = [num(1), num(2)];
                let c = [num(3), num(4)];
                let cw = f.get(5).map(|v| v.starts_with('Y')).unwrap_or(false);
                if let Some(prev) = cur.last().copied() {
                    let a0 = (prev[1] - c[1]).atan2(prev[0] - c[0]);
                    let a1 = (end[1] - c[1]).atan2(end[0] - c[0]);
                    let mut sw = (a1 - a0).to_degrees();
                    if cw {
                        if sw >= -1e-9 {
                            sw -= 360.0;
                        }
                    } else if sw <= 1e-9 {
                        sw += 360.0;
                    }
                    arc_points(c, prev, sw, &mut cur);
                    if let Some(l) = cur.last_mut() {
                        *l = end;
                    }
                }
            }
            Some("OE") if cur.len() >= 3 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => {}
        }
    }
    out
}

/// Draw a feature file into a raster (and collect pad positions for drills).
fn draw_features(text: &str, r: &mut Raster, pads: &mut Vec<(P2, f64)>) {
    let scale = units_of(text);
    let sym_scale = scale / 1000.0;
    let mut syms: HashMap<usize, Sym> = HashMap::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim();
        i += 1;
        if let Some(rest) = l.strip_prefix('$') {
            let mut it = rest.split_whitespace();
            if let (Some(k), Some(name)) = (it.next(), it.next()) {
                if let Ok(k) = k.parse::<usize>() {
                    syms.insert(k, parse_symbol(name, sym_scale));
                }
            }
            continue;
        }
        let body = l.split(';').next().unwrap_or("");
        let f: Vec<&str> = body.split_whitespace().collect();
        let num = |k: usize| f.get(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        match f.first().copied() {
            Some("L") if f.len() >= 7 => {
                let a = [num(1) * scale, num(2) * scale];
                let b = [num(3) * scale, num(4) * scale];
                let dark = f[6] != "N";
                let w = match syms.get(&(num(5) as usize)) {
                    Some(Sym::Round(d)) => *d,
                    Some(Sym::Rect(w, h)) | Some(Sym::Oval(w, h)) => w.min(*h),
                    _ => 0.0,
                };
                if w > 0.0 {
                    r.segment(a, b, w, dark);
                }
            }
            Some("A") if f.len() >= 10 => {
                let s = [num(1) * scale, num(2) * scale];
                let e = [num(3) * scale, num(4) * scale];
                let c = [num(5) * scale, num(6) * scale];
                let dark = f[8] != "N";
                let cw = f.get(10).map(|v| v.starts_with('Y')).unwrap_or(false);
                let w = match syms.get(&(num(7) as usize)) {
                    Some(Sym::Round(d)) => *d,
                    _ => 0.0,
                };
                let a0 = (s[1] - c[1]).atan2(s[0] - c[0]);
                let a1 = (e[1] - c[1]).atan2(e[0] - c[0]);
                let mut sw = (a1 - a0).to_degrees();
                if cw {
                    if sw >= -1e-9 {
                        sw -= 360.0;
                    }
                } else if sw <= 1e-9 {
                    sw += 360.0;
                }
                let mut pts = vec![s];
                arc_points(c, s, sw, &mut pts);
                for seg in pts.windows(2) {
                    r.segment(seg[0], seg[1], w, dark);
                }
            }
            Some("P") if f.len() >= 6 => {
                let c = [num(1) * scale, num(2) * scale];
                let (sym_k, rest) = if f[3] == "-1" {
                    (num(4) as usize, 5)
                } else {
                    (num(3) as usize, 4)
                };
                let dark = f.get(rest).map(|v| *v != "N").unwrap_or(true);
                let orient = f
                    .get(rest + 2)
                    .and_then(|v| v.parse::<i32>().ok())
                    .unwrap_or(0);
                // ODB++ angles are clockwise
                let rot = match orient {
                    0..=7 => -90.0 * (orient % 4) as f64,
                    _ => -f
                        .get(rest + 3)
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(0.0),
                };
                match syms.get(&sym_k) {
                    Some(Sym::Round(d)) => {
                        r.disc(c, 0.5 * d, dark);
                        pads.push((c, *d));
                    }
                    Some(Sym::Rect(w, h)) => r.rect(c, *w, *h, rot, dark),
                    Some(Sym::Oval(w, h)) => r.obround(c, *w, *h, rot, dark),
                    Some(Sym::Diamond(w, h)) => {
                        let pts: Vec<P2> = [
                            [0.5 * w, 0.0],
                            [0.0, 0.5 * h],
                            [-0.5 * w, 0.0],
                            [0.0, -0.5 * h],
                        ]
                        .iter()
                        .map(|p| {
                            let q = rotate(*p, rot);
                            [q[0] + c[0], q[1] + c[1]]
                        })
                        .collect();
                        r.fill_poly(&pts, dark);
                    }
                    Some(Sym::Donut(od, id)) => {
                        // ring as a thick circle
                        let rm = 0.25 * (od + id);
                        let mut ring = vec![[c[0] + rm, c[1]]];
                        arc_points(c, [c[0] + rm, c[1]], 360.0, &mut ring);
                        for s in ring.windows(2) {
                            r.segment(s[0], s[1], 0.5 * (od - id), dark);
                        }
                    }
                    _ => {}
                }
            }
            Some("S") => {
                let dark = f.get(1).map(|v| *v != "N").unwrap_or(true);
                let st = i;
                while i < lines.len() && !lines[i].trim().starts_with("SE") {
                    i += 1;
                }
                let rec: Vec<&str> = lines[st..i].iter().map(|s| s.trim()).collect();
                i += 1;
                let cs = surface_contours(&rec, scale);
                let refs: Vec<&[P2]> = cs.iter().map(|p| p.as_slice()).collect();
                r.fill_polys(&refs, dark);
            }
            _ => {}
        }
    }
}

/// Read an ODB++ product (files from a folder or archive).
pub fn read(files: Files) -> Result<Board, String> {
    let tree = Tree::new(files);
    let matrix = tree
        .get("matrix/matrix")
        .ok_or("not an ODB++ product (matrix/matrix missing)")?;
    let (steps, layers) = parse_matrix(&matrix);
    // step: prefer "pcb", else the first
    let step = steps
        .iter()
        .find(|s| s.as_str() == "pcb")
        .or(steps.first())
        .cloned()
        .ok_or("the matrix declares no STEP")?;
    let sdir = format!("steps/{step}");
    let mut board = Board {
        format: "ODB++".into(),
        name: if tree.root.is_empty() {
            step.clone()
        } else {
            tree.root.trim_end_matches('/').to_string()
        },
        ..Default::default()
    };
    // thickness
    if let Some(a) = tree.get(&format!("{sdir}/attrlist")) {
        let u = units_of(&a);
        let kv = attrlist(&a);
        if let Some(t) = kv
            .get(".board_thickness")
            .and_then(|v| v.parse::<f64>().ok())
        {
            // stored in microns (MM) or mils (INCH)
            board.thickness_mm = Some(t * u / 1000.0);
        }
    }
    // profile
    if let Some(p) = tree.get(&format!("{sdir}/profile")) {
        let scale = units_of(&p);
        let lines: Vec<&str> = p.lines().map(|l| l.trim()).collect();
        board.outline = surface_contours(&lines, scale);
    }
    if board.outline.is_empty() {
        board
            .warnings
            .push("no step profile; using the copper/component extent".into());
    }
    // packages
    let mut pkgs: Vec<(String, (P2, P2))> = Vec::new();
    if let Some(eda) = tree.get(&format!("{sdir}/eda/data")) {
        let scale = units_of(&eda);
        for l in eda.lines() {
            let l = l.split(';').next().unwrap_or("").trim();
            if let Some(rest) = l.strip_prefix("PKG ") {
                let f: Vec<&str> = rest.split_whitespace().collect();
                let num =
                    |k: usize| f.get(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) * scale;
                pkgs.push((
                    f.first().unwrap_or(&"").to_string(),
                    ([num(2), num(3)], [num(4), num(5)]),
                ));
            }
        }
    }
    // components
    for (side, layer) in [(Side::Top, "comp_+_top"), (Side::Bottom, "comp_+_bot")] {
        let Some(t) = tree.get(&format!("{sdir}/layers/{layer}/components")) else {
            continue;
        };
        let scale = units_of(&t);
        let mut attr_names: HashMap<usize, String> = HashMap::new();
        for l in t.lines() {
            let l = l.trim();
            if let Some(rest) = l.strip_prefix('@') {
                if let Some((k, n)) = rest.split_once(' ') {
                    if let Ok(k) = k.parse::<usize>() {
                        attr_names.insert(k, n.trim().to_ascii_lowercase());
                    }
                }
                continue;
            }
            let Some(rest) = l.strip_prefix("CMP ") else {
                continue;
            };
            let (rec, attrs) = rest.split_once(';').unwrap_or((rest, ""));
            let f: Vec<&str> = rec.split_whitespace().collect();
            if f.len() < 6 {
                continue;
            }
            let num = |k: usize| f.get(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
            let pkg = num(0) as usize;
            let (pname, pbox) = pkgs
                .get(pkg)
                .map(|p| (p.0.clone(), Some(p.1)))
                .unwrap_or((format!("pkg{pkg}"), None));
            // ODB++ rotation is clockwise
            let mut pl =
                Placement::new(f[5], &pname, num(1) * scale, num(2) * scale, -num(3), side);
            pl.part = f.get(6).unwrap_or(&"").to_string();
            pl.local_bbox = pbox;
            for kv in attrs.split(',') {
                if let Some((k, v)) = kv.split_once('=') {
                    if let Ok(k) = k.trim().parse::<usize>() {
                        if attr_names
                            .get(&k)
                            .map(|n| n == ".comp_height")
                            .unwrap_or(false)
                        {
                            pl.height_mm = v.trim().parse::<f64>().ok().map(|h| h * scale);
                        }
                    }
                }
            }
            board.components.push(pl);
        }
    }
    // copper and drills
    board.ensure_frame(0.1);
    for l in layers.iter().filter(|l| l.context == "BOARD") {
        let feat = tree.get(&format!("{sdir}/layers/{}/features", l.name));
        let attrs = tree.get(&format!("{sdir}/layers/{}/attrlist", l.name));
        let (u, kv) = match &attrs {
            Some(a) => (units_of(a), attrlist(a)),
            None => (25.4, HashMap::new()),
        };
        match l.typ.as_str() {
            "SIGNAL" | "POWER_GROUND" | "MIXED" => {
                let mut r = board.new_raster();
                let mut pads = Vec::new();
                if let Some(t) = &feat {
                    draw_features(t, &mut r, &mut pads);
                }
                // .copper_weight: microns (MM) or oz (INCH)
                let t = kv
                    .get(".copper_weight")
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(|w| if u == 1.0 { w / 1000.0 } else { w * 0.035 });
                board.layers.push(Layer {
                    name: l.name.clone(),
                    kind: LayerKind::Copper,
                    thickness_mm: t,
                    material: None,
                    copper: Some(r),
                });
            }
            "DIELECTRIC" => {
                let t = kv
                    .get(".layer_dielectric")
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(|v| v * u);
                board.layers.push(Layer {
                    name: l.name.clone(),
                    kind: LayerKind::Dielectric,
                    thickness_mm: t,
                    material: None,
                    copper: None,
                });
            }
            "DRILL" => {
                if let Some(t) = &feat {
                    let mut sink = Raster::new([0.0, 0.0], [1.0, 1.0], 1.0);
                    let mut pads = Vec::new();
                    draw_features(t, &mut sink, &mut pads);
                    let plated = !l.name.contains("npth") && !l.name.contains("non");
                    for (c, d) in pads {
                        board.drills.push(Drill {
                            x: c[0],
                            y: c[1],
                            d_mm: d,
                            plated,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    if board.copper_layers().count() == 0 {
        board
            .warnings
            .push("no SIGNAL/POWER_GROUND layers in the matrix".into());
    }
    if board.outline.is_empty() {
        let pts: Vec<P2> = board
            .components
            .iter()
            .filter_map(|c| c.footprint())
            .flat_map(|(a, b)| [a, b])
            .collect();
        if let Some((lo, hi)) = bbox(&pts) {
            board.outline = vec![vec![lo, [hi[0], lo[1]], hi, [lo[0], hi[1]]]];
        }
    }
    Ok(board)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A product as written by the KiCad ODB++ exporter (MM units).
    fn sample() -> Files {
        let f = |p: &str, t: &str| (format!("demo/{p}"), t.as_bytes().to_vec());
        vec![
            f("matrix/matrix", "STEP {\n   COL=1\n   NAME=PCB\n}\n\nLAYER {\n   ROW=1\n   CONTEXT=BOARD\n   TYPE=SIGNAL\n   NAME=F.CU\n   POLARITY=POSITIVE\n}\n\nLAYER {\n   ROW=2\n   CONTEXT=BOARD\n   TYPE=DIELECTRIC\n   NAME=DIELECTRIC_1\n   POLARITY=POSITIVE\n}\n\nLAYER {\n   ROW=3\n   CONTEXT=BOARD\n   TYPE=SIGNAL\n   NAME=B.CU\n   POLARITY=POSITIVE\n}\n\nLAYER {\n   ROW=4\n   CONTEXT=BOARD\n   TYPE=DRILL\n   NAME=DRILL\n   POLARITY=POSITIVE\n}\n\nLAYER {\n   ROW=5\n   CONTEXT=BOARD\n   TYPE=COMPONENT\n   NAME=COMP_+_TOP\n   POLARITY=POSITIVE\n}\n"),
            f("steps/pcb/attrlist", "UNITS=MM\n.board_thickness=1600\n"),
            f("steps/pcb/profile", "UNITS=MM\n#\n#Num Features\n#\nF 1\n#\n#Layer features\n#\nS P 0\nOB 0 0 I\nOS 50 0\nOS 50 30\nOS 0 30\nOS 0 0\nOE\nSE\n"),
            f("steps/pcb/layers/f.cu/attrlist", "UNITS=MM\n.copper_weight=35\n"),
            f("steps/pcb/layers/f.cu/features", "UNITS=MM\n#\nF 3\n#\n$0 r1000\n$1 rect2000x1000\n#\nL 5 5 35 5 0 P 0;0=1\nP 10 20 1 P 0 8 270\nS P 0\nOB 40 20 I\nOS 45 20\nOS 45 25\nOS 40 25\nOS 40 20\nOE\nSE\n"),
            f("steps/pcb/layers/dielectric_1/attrlist", "UNITS=MM\n.layer_dielectric=1.53\n"),
            f("steps/pcb/layers/b.cu/attrlist", "UNITS=MM\n.copper_weight=35\n"),
            f("steps/pcb/layers/b.cu/features", "UNITS=MM\nF 1\nS P 0\nOB 0 0 I\nOS 50 0\nOS 50 30\nOS 0 30\nOS 0 0\nOE\nOB 10 10 H\nOS 10 20\nOS 20 20\nOS 20 10\nOS 10 10\nOE\nSE\n"),
            f("steps/pcb/layers/drill/features", "UNITS=MM\n$0 r300\n$1 r3200\nP 25 25 0 P 0 0\nP 5 25 1 P 0 0\n"),
            f("steps/pcb/eda/data", "# test\nHDR KiCad EDA\nUNITS=MM\nLYR F.CU B.CU\n# PKG 0\nPKG SOIC-8 1.27 -2.5 -2 2.5 2\nRC -2.5 -2 5 4\nPIN 1 S -2 -1.9 0 E S\n"),
            f("steps/pcb/layers/comp_+_top/components", "UNITS=MM\n@0 .comp_height\n@1 .comp_mount_type\n# CMP 0\nCMP 0 20 15 270 N U1 LM358;0=1.75,1=1\nTOP 0 18 13 0 N 0 0 1\n#\n"),
        ]
    }

    #[test]
    fn kicad_style_odbpp() {
        let mut b = read(sample()).unwrap();
        assert!((b.thickness_mm.unwrap() - 1.6).abs() < 1e-12);
        let s = b.summary();
        assert_eq!(s.copper_layers.len(), 2);
        assert!((s.copper_layers[0].1 - 0.035).abs() < 1e-12);
        assert!((s.size_mm[0] - 50.0).abs() < 1e-9 && (s.size_mm[1] - 30.0).abs() < 1e-9);
        // F.Cu: 30x1 line + caps + 2x1 pad + 5x5 surface
        let fcu = s.copper_layers[0].2 * 1500.0;
        assert!(
            (fcu - (30.0 + std::f64::consts::FRAC_PI_4 + 2.0 + 25.0)).abs() < 1.0,
            "{fcu}"
        );
        // B.Cu: plane with a 10x10 hole
        let bcu = s.copper_layers[1].2 * 1500.0;
        assert!((bcu - 1400.0).abs() < 3.0, "{bcu}");
        let u1 = &b.components[0];
        assert_eq!(u1.refdes, "U1");
        assert_eq!(u1.height_mm, Some(1.75));
        let (lo, hi) = u1.footprint().unwrap();
        // 270° clockwise = 90° CCW: 5 x 4 becomes 4 x 5
        assert!(((hi[0] - lo[0]) - 4.0).abs() < 1e-9 && ((hi[1] - lo[1]) - 5.0).abs() < 1e-9);
        assert_eq!(b.drills.len(), 2);
        assert!((b.drills[1].d_mm - 3.2).abs() < 1e-9);
    }

    #[test]
    fn symbols() {
        assert!(matches!(parse_symbol("r10", 0.0254), Sym::Round(d) if (d - 0.254).abs() < 1e-12));
        assert!(
            matches!(parse_symbol("rect20x10xr2", 1.0), Sym::Rect(w, h) if w == 20.0 && h == 10.0)
        );
        assert!(matches!(parse_symbol("oval5x3", 1.0), Sym::Oval(..)));
        assert!(matches!(parse_symbol("s7", 1.0), Sym::Rect(w, _) if w == 7.0));
        assert!(matches!(parse_symbol("donut_r10x5", 1.0), Sym::Donut(..)));
    }
}
