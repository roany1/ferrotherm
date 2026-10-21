// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! IDF 3.0 (Intermediate Data Format) board (.emn/.brd) and library
//! (.emp/.lib) files: board outline and cut-outs with arcs, thickness,
//! drilled holes, component placements with outlines and heights.
//! Also writes IDF 3.0 for MCAD round trips.

use crate::ecad::{Board, Drill, Placement, Side};
use crate::geom::{arc_points, bbox, Poly, P2};
use std::collections::HashMap;

/// Split a line into fields, honouring double quotes.
fn fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q = false;
    for ch in line.chars() {
        match ch {
            '"' => q = !q,
            c if c.is_whitespace() && !q => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

struct Section {
    name: String,
    #[allow(dead_code)]
    header: Vec<String>,
    lines: Vec<Vec<String>>,
}

fn sections(text: &str) -> Vec<Section> {
    let mut out = Vec::new();
    let mut cur: Option<Section> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix(".END_") {
            let _ = rest;
            if let Some(s) = cur.take() {
                out.push(s);
            }
            continue;
        }
        if line.starts_with('.') {
            if let Some(s) = cur.take() {
                out.push(s);
            }
            let f = fields(line);
            cur = Some(Section {
                name: f[0].trim_start_matches('.').to_ascii_uppercase(),
                header: f[1..].to_vec(),
                lines: vec![],
            });
            continue;
        }
        if let Some(s) = cur.as_mut() {
            s.lines.push(fields(line));
        }
    }
    if let Some(s) = cur {
        out.push(s);
    }
    out
}

/// Outline loops from `label x y angle` records.
fn loops(lines: &[Vec<String>], scale: f64) -> Vec<Poly> {
    let mut out: Vec<Poly> = Vec::new();
    let mut cur: Vec<P2> = Vec::new();
    let mut label: Option<String> = None;
    for f in lines {
        if f.len() < 4 {
            continue;
        }
        let (Ok(x), Ok(y), Ok(ang)) = (
            f[1].parse::<f64>(),
            f[2].parse::<f64>(),
            f[3].parse::<f64>(),
        ) else {
            continue;
        };
        let p = [x * scale, y * scale];
        if label.as_deref() != Some(f[0].as_str()) {
            if cur.len() >= 3 {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
            label = Some(f[0].clone());
            cur.push(p);
            continue;
        }
        let prev = *cur.last().unwrap();
        if ang == 0.0 {
            cur.push(p);
        } else if (ang.abs() - 360.0).abs() < 1e-9 {
            // full circle: previous point is the centre
            cur.pop();
            let mut c = vec![p];
            arc_points(prev, p, 360.0, &mut c);
            cur.extend(c);
        } else {
            // arc from prev to p with included angle `ang` (CCW positive)
            let mid = [(prev[0] + p[0]) * 0.5, (prev[1] + p[1]) * 0.5];
            let chord = ((p[0] - prev[0]).powi(2) + (p[1] - prev[1]).powi(2)).sqrt();
            let th = ang.to_radians();
            let d = 0.5 * chord / (0.5 * th).tan(); // centre offset from the chord midpoint
            let n = [-(p[1] - prev[1]) / chord, (p[0] - prev[0]) / chord];
            let c = [mid[0] + n[0] * d, mid[1] + n[1] * d];
            arc_points(c, prev, ang, &mut cur);
            if let Some(l) = cur.last_mut() {
                *l = p;
            }
        }
    }
    if cur.len() >= 3 {
        out.push(cur);
    }
    // closing point duplicated in IDF loops
    for l in out.iter_mut() {
        if l.len() > 3 {
            let (a, b) = (l[0], *l.last().unwrap());
            if (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9 {
                l.pop();
            }
        }
    }
    out
}

fn unit_scale(s: &str) -> f64 {
    match s.to_ascii_uppercase().as_str() {
        "THOU" | "MIL" => 0.0254,
        "IN" | "INCH" => 25.4,
        _ => 1.0,
    }
}

/// Library: (geometry, part) → (local outline bbox, height mm).
pub type Library = HashMap<(String, String), ((P2, P2), f64)>;

pub fn read_library(text: &str) -> Library {
    let mut lib = Library::new();
    for s in sections(text) {
        if s.name != "ELECTRICAL" && s.name != "MECHANICAL" {
            continue;
        }
        let Some(h) = s.lines.first() else { continue };
        if h.len() < 4 {
            continue;
        }
        let scale = unit_scale(&h[2]);
        let height = h[3].parse::<f64>().unwrap_or(0.0) * scale;
        let rec: Vec<Vec<String>> = s.lines[1..]
            .iter()
            .filter(|f| !f.first().map(|x| x == "PROP").unwrap_or(false))
            .cloned()
            .collect();
        let pts: Vec<P2> = loops(&rec, scale).into_iter().flatten().collect();
        if let Some(bb) = bbox(&pts) {
            lib.insert((h[0].clone(), h[1].clone()), (bb, height));
        }
    }
    lib
}

/// Read an IDF board file with its library.
pub fn read(board_text: &str, lib_text: Option<&str>) -> Result<Board, String> {
    let secs = sections(board_text);
    let header = secs
        .iter()
        .find(|s| s.name == "HEADER")
        .ok_or("not an IDF file (no .HEADER)")?;
    let scale = header
        .lines
        .get(1)
        .and_then(|f| f.get(1))
        .map(|u| unit_scale(u))
        .unwrap_or(1.0);
    let name = header
        .lines
        .get(1)
        .and_then(|f| f.first())
        .cloned()
        .unwrap_or_default();
    let ver = header
        .lines
        .first()
        .and_then(|f| f.get(1))
        .cloned()
        .unwrap_or_default();
    let mut board = Board {
        format: format!("IDF {ver}"),
        name,
        ..Default::default()
    };
    if !ver.starts_with('3') && !ver.is_empty() {
        board
            .warnings
            .push(format!("IDF version {ver}: read as 3.0"));
    }
    let lib = lib_text.map(read_library).unwrap_or_default();
    for s in &secs {
        match s.name.as_str() {
            "BOARD_OUTLINE" | "PANEL_OUTLINE" if board.outline.is_empty() => {
                let t = s
                    .lines
                    .first()
                    .and_then(|f| f.first())
                    .and_then(|v| v.parse::<f64>().ok());
                board.thickness_mm = t.map(|t| t * scale);
                board.outline = loops(&s.lines[1..], scale);
            }
            "DRILLED_HOLES" => {
                for f in &s.lines {
                    if f.len() < 4 {
                        continue;
                    }
                    let (Ok(d), Ok(x), Ok(y)) = (
                        f[0].parse::<f64>(),
                        f[1].parse::<f64>(),
                        f[2].parse::<f64>(),
                    ) else {
                        continue;
                    };
                    board.drills.push(Drill {
                        x: x * scale,
                        y: y * scale,
                        d_mm: d * scale,
                        plated: f[3].eq_ignore_ascii_case("PTH"),
                    });
                }
            }
            "PLACEMENT" => {
                let mut it = s.lines.iter();
                while let (Some(a), Some(b)) = (it.next(), it.next()) {
                    if a.len() < 3 || b.len() < 5 {
                        continue;
                    }
                    let (geo, part, refdes) = (&a[0], &a[1], &a[2]);
                    let num =
                        |k: usize| b.get(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
                    let side = if b[4].eq_ignore_ascii_case("BOTTOM") {
                        Side::Bottom
                    } else {
                        Side::Top
                    };
                    let mut p =
                        Placement::new(refdes, geo, num(0) * scale, num(1) * scale, num(3), side);
                    p.part = part.clone();
                    match lib.get(&(geo.clone(), part.clone())) {
                        Some((bb, h)) => {
                            p.local_bbox = Some(*bb);
                            p.height_mm = Some(*h + num(2) * scale);
                        }
                        None => {
                            if lib_text.is_some() {
                                board.warnings.push(format!(
                                    "{refdes}: '{geo}' / '{part}' not in the IDF library"
                                ));
                            }
                        }
                    }
                    board.components.push(p);
                }
            }
            _ => {}
        }
    }
    if board.outline.is_empty() {
        board.warnings.push("no BOARD_OUTLINE section".into());
    }
    if lib_text.is_none() {
        board
            .warnings
            .push("no IDF library (.emp): component outlines and heights are unknown".into());
    }
    Ok(board)
}

/// Write an IDF 3.0 board + library pair (MM units). `parts`: refdes,
/// package, centre x/y, size x/y, height, side.
pub struct IdfPart {
    pub refdes: String,
    pub package: String,
    pub center: P2,
    pub size: P2,
    pub height: f64,
    pub side: Side,
}

pub fn write(name: &str, outline: &[P2], thickness: f64, parts: &[IdfPart]) -> (String, String) {
    let date = "2026/01/01.00:00:00";
    let mut emn = format!(
        ".HEADER\nBOARD_FILE 3.0 \"FerroTherm (Roany Romano)\" {date} 1\n\"{name}\" MM\n.END_HEADER\n.BOARD_OUTLINE MCAD\n{thickness:.4}\n"
    );
    for p in outline.iter().chain(outline.first()) {
        emn += &format!("0 {:.4} {:.4} 0\n", p[0], p[1]);
    }
    emn += ".END_BOARD_OUTLINE\n.PLACEMENT\n";
    let mut emp =
        format!(".HEADER\nLIBRARY_FILE 3.0 \"FerroTherm (Roany Romano)\" {date} 1\n.END_HEADER\n");
    let mut seen = std::collections::HashSet::new();
    for p in parts {
        let geo = format!(
            "{}_{:.2}x{:.2}x{:.2}",
            p.package, p.size[0], p.size[1], p.height
        )
        .replace(' ', "_");
        emn += &format!(
            "\"{geo}\" \"{}\" {}\n{:.4} {:.4} 0 0 {} PLACED\n",
            p.package,
            p.refdes,
            p.center[0],
            p.center[1],
            if p.side == Side::Top { "TOP" } else { "BOTTOM" }
        );
        if seen.insert(geo.clone()) {
            let (w, h) = (0.5 * p.size[0], 0.5 * p.size[1]);
            emp += &format!(
                ".ELECTRICAL\n\"{geo}\" \"{}\" MM {:.4}\n",
                p.package, p.height
            );
            for (x, y) in [(-w, -h), (w, -h), (w, h), (-w, h), (-w, -h)] {
                emp += &format!("0 {x:.4} {y:.4} 0\n");
            }
            emp += ".END_ELECTRICAL\n";
        }
    }
    emn += ".END_PLACEMENT\n";
    (emn, emp)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Board with a rounded corner (90° arc), a circular cut-out, two parts
    /// (one rotated 90°, one on the bottom), in THOU units.
    #[test]
    fn idf_round_trip_and_arcs() {
        let emn = r#".HEADER
BOARD_FILE 3.0 "Test" 2026/01/01.00:00:00 1
test_board THOU
.END_HEADER
.BOARD_OUTLINE ECAD
62.0
0 0.0 0.0 0.0
0 4000.0 0.0 0.0
0 4000.0 2000.0 0.0
0 3000.0 3000.0 90.0
0 0.0 3000.0 0.0
0 0.0 0.0 0.0
1 2000.0 1500.0 0.0
1 2100.0 1500.0 360.0
.END_BOARD_OUTLINE
.DRILLED_HOLES
30.0 1800.0 100.0 PTH J1 PIN ECAD
.END_DRILLED_HOLES
.PLACEMENT
cs13_a pn-cap C1
1000.0 1000.0 0.0 90.0 TOP PLACED
"qfp 44" "pn 2" U1
2500.0 1000.0 0.0 0.0 BOTTOM PLACED
.END_PLACEMENT
"#;
        let emp = r#".HEADER
LIBRARY_FILE 3.0 "Test" 2026/01/01.00:00:00 1
.END_HEADER
.ELECTRICAL
cs13_a pn-cap THOU 150.0
0 -100.0 -50.0 0.0
0 100.0 -50.0 0.0
0 100.0 50.0 0.0
0 -100.0 50.0 0.0
0 -100.0 -50.0 0.0
PROP CAPACITANCE 100.0
.END_ELECTRICAL
.ELECTRICAL
"qfp 44" "pn 2" MM 1.6
0 -5.0 -5.0 0.0
0 5.0 -5.0 0.0
0 5.0 5.0 0.0
0 -5.0 5.0 0.0
.END_ELECTRICAL
"#;
        let mut b = read(emn, Some(emp)).unwrap();
        assert!((b.thickness_mm.unwrap() - 62.0 * 0.0254).abs() < 1e-9);
        assert_eq!(b.outline.len(), 2);
        // area: 4000x3000 minus the corner cut (1000x1000 square minus a quarter circle r=1000)
        // plus nothing: the 90° arc bulges outwards from (4000,2000) to (3000,3000)
        let a0 = crate::geom::poly_area(&b.outline[0]).abs() / 0.0254f64.powi(2);
        let exact = 4000.0 * 3000.0 - 1000.0 * 1000.0
            + (std::f64::consts::PI / 4.0 - 0.5) * 1000.0 * 1000.0
            + 0.5 * 1000.0 * 1000.0;
        assert!((a0 / exact - 1.0).abs() < 2e-3, "{a0} {exact}");
        let hole = crate::geom::poly_area(&b.outline[1]).abs() / 0.0254f64.powi(2);
        assert!((hole / (std::f64::consts::PI * 100.0 * 100.0) - 1.0).abs() < 5e-3);
        let c1 = &b.components[0];
        let (lo, hi) = c1.footprint().unwrap();
        assert!(((hi[0] - lo[0]) / 0.0254 - 100.0).abs() < 1e-6); // rotated: 200x100 → 100x200
        assert!(((hi[1] - lo[1]) / 0.0254 - 200.0).abs() < 1e-6);
        assert!((c1.height_mm.unwrap() - 150.0 * 0.0254).abs() < 1e-9);
        let u1 = &b.components[1];
        assert_eq!(u1.side, Side::Bottom);
        assert!((u1.height_mm.unwrap() - 1.6).abs() < 1e-9);
        assert_eq!(b.drills.len(), 1);
        let s = b.summary();
        assert_eq!(s.components, 2);
        // write → read back
        let (wemn, wemp) = write(
            "rt",
            &[[0.0, 0.0], [100.0, 0.0], [100.0, 80.0], [0.0, 80.0]],
            1.6,
            &[IdfPart {
                refdes: "U9".into(),
                package: "BGA".into(),
                center: [50.0, 40.0],
                size: [17.0, 17.0],
                height: 2.0,
                side: Side::Top,
            }],
        );
        let b2 = read(&wemn, Some(&wemp)).unwrap();
        assert_eq!(b2.components.len(), 1);
        let (lo, hi) = b2.components[0].footprint().unwrap();
        assert!((lo[0] - 41.5).abs() < 1e-6 && (hi[1] - 48.5).abs() < 1e-6);
        assert!((b2.thickness_mm.unwrap() - 1.6).abs() < 1e-9);
    }
}
