// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! KiCad board files (`.kicad_pcb`, S-expression, KiCad 5–9).
//!
//! Read: Edge.Cuts outline (lines, arcs, circles, rectangles, polygons),
//! stack-up (setup/stackup), copper tracks, arcs, vias, pads and filled
//! zones, footprints (reference, position, rotation, side; body extent from
//! the courtyard, else fabrication layer, else pads). KiCad's y axis points
//! down: coordinates are mirrored to y-up.

use crate::ecad::{Board, Drill, Layer, LayerKind, Placement, Side};
use crate::geom::{arc3_points, arc_points, bbox, rotate, Raster, P2};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Sx {
    Atom(String),
    List(Vec<Sx>),
}

impl Sx {
    pub fn head(&self) -> Option<&str> {
        match self {
            Sx::List(v) => match v.first() {
                Some(Sx::Atom(a)) => Some(a.as_str()),
                _ => None,
            },
            _ => None,
        }
    }
    pub fn items(&self) -> &[Sx] {
        match self {
            Sx::List(v) => v,
            _ => &[],
        }
    }
    pub fn atom(&self) -> Option<&str> {
        match self {
            Sx::Atom(a) => Some(a),
            _ => None,
        }
    }
    /// First child list with this head.
    pub fn child(&self, name: &str) -> Option<&Sx> {
        self.items().iter().find(|c| c.head() == Some(name))
    }
    pub fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Sx> + 'a {
        self.items().iter().filter(move |c| c.head() == Some(name))
    }
    /// Numeric argument k (1-based after the head).
    pub fn num(&self, k: usize) -> Option<f64> {
        self.items().get(k)?.atom()?.parse().ok()
    }
    pub fn str_arg(&self, k: usize) -> Option<&str> {
        self.items().get(k)?.atom()
    }
}

/// Parse an S-expression document.
pub fn parse_sx(text: &str) -> Result<Sx, String> {
    let b = text.as_bytes();
    let mut stack: Vec<Vec<Sx>> = vec![Vec::new()];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'(' => {
                stack.push(Vec::new());
                i += 1;
            }
            b')' => {
                let l = stack.pop().ok_or("unbalanced ')'")?;
                stack.last_mut().ok_or("unbalanced ')'")?.push(Sx::List(l));
                i += 1;
            }
            b'"' => {
                let mut s = String::new();
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                    }
                    s.push(b[i] as char);
                    i += 1;
                }
                i += 1;
                stack.last_mut().unwrap().push(Sx::Atom(s));
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => {
                let st = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'(' && b[i] != b')' {
                    i += 1;
                }
                stack
                    .last_mut()
                    .unwrap()
                    .push(Sx::Atom(String::from_utf8_lossy(&b[st..i]).to_string()));
            }
        }
    }
    let mut top = stack.pop().ok_or("empty document")?;
    if !stack.is_empty() {
        return Err("unbalanced '('".into());
    }
    top.pop().ok_or_else(|| "empty document".into())
}

fn xy(n: &Sx) -> Option<P2> {
    Some([n.num(1)?, -n.num(2)?])
}

fn layer_of(n: &Sx) -> Option<String> {
    n.child("layer")
        .and_then(|l| l.str_arg(1))
        .map(|s| s.to_string())
}

/// Layers a pad/zone covers ("*.Cu", "F&B.Cu" patterns expanded later).
fn layers_of(n: &Sx) -> Vec<String> {
    match n.child("layers") {
        Some(l) => l.items()[1..]
            .iter()
            .filter_map(|a| a.atom().map(|s| s.to_string()))
            .collect(),
        None => layer_of(n).into_iter().collect(),
    }
}

/// Graphic item → polyline (y up), for outlines.
fn graphic_points(g: &Sx) -> Option<Vec<P2>> {
    let h = g.head()?;
    let kind = h.trim_start_matches("gr_").trim_start_matches("fp_");
    match kind {
        "line" => Some(vec![xy(g.child("start")?)?, xy(g.child("end")?)?]),
        "arc" => {
            let s = xy(g.child("start")?)?;
            if let Some(m) = g.child("mid") {
                let mut v = vec![s];
                arc3_points(s, xy(m)?, xy(g.child("end")?)?, &mut v);
                Some(v)
            } else {
                // KiCad 5: start = centre, end = start point, angle (clockwise in y-down)
                let c = s;
                let st = xy(g.child("end")?)?;
                let ang = g.child("angle").and_then(|a| a.num(1)).unwrap_or(90.0);
                let mut v = vec![st];
                arc_points(c, st, -ang, &mut v);
                Some(v)
            }
        }
        "circle" => {
            let c = xy(g.child("center")?)?;
            let e = xy(g.child("end")?)?;
            let mut v = vec![e];
            arc_points(c, e, 360.0, &mut v);
            Some(v)
        }
        "rect" => {
            let a = xy(g.child("start")?)?;
            let b = xy(g.child("end")?)?;
            Some(vec![a, [b[0], a[1]], b, [a[0], b[1]], a])
        }
        "poly" => {
            let pts = g.child("pts")?;
            let mut v: Vec<P2> = pts.children("xy").filter_map(xy).collect();
            if let Some(f) = v.first().copied() {
                v.push(f);
            }
            Some(v)
        }
        _ => None,
    }
}

fn local_to_board(p: P2, at: P2, rot: f64) -> P2 {
    let r = rotate(p, rot);
    [r[0] + at[0], r[1] + at[1]]
}

/// Read a `.kicad_pcb` document.
pub fn read(text: &str) -> Result<Board, String> {
    let root = parse_sx(text)?;
    if root.head() != Some("kicad_pcb") {
        return Err("not a KiCad board (missing kicad_pcb)".into());
    }
    let mut board = Board {
        format: "KiCad".into(),
        ..Default::default()
    };
    board.thickness_mm = root
        .child("general")
        .and_then(|g| g.child("thickness"))
        .and_then(|t| t.num(1));

    // ---- copper layer order ----
    let mut cu_names: Vec<String> = Vec::new();
    if let Some(ls) = root.child("layers") {
        for l in ls.items()[1..].iter() {
            let name = l.str_arg(1).unwrap_or("");
            let kind = l.str_arg(2).unwrap_or("");
            if name.ends_with(".Cu") || matches!(kind, "signal" | "power" | "mixed" | "jumper") {
                cu_names.push(name.to_string());
            }
        }
    }
    // order: F.Cu, In1.Cu, ..., B.Cu
    cu_names.sort_by_key(|n| {
        if n == "F.Cu" {
            0
        } else if n == "B.Cu" {
            10_000
        } else {
            n.trim_start_matches("In")
                .trim_end_matches(".Cu")
                .parse::<i32>()
                .unwrap_or(5000)
        }
    });
    // stack-up thicknesses
    let mut cu_thick: HashMap<String, f64> = HashMap::new();
    let mut dielectrics: Vec<(usize, f64, Option<String>)> = Vec::new(); // after copper index
    if let Some(st) = root.child("setup").and_then(|s| s.child("stackup")) {
        let mut last_cu = 0usize;
        for l in st.children("layer") {
            let name = l.str_arg(1).unwrap_or("");
            let typ = l.child("type").and_then(|t| t.str_arg(1)).unwrap_or("");
            let t = l.child("thickness").and_then(|t| t.num(1));
            if typ == "copper" {
                if let Some(t) = t {
                    cu_thick.insert(name.to_string(), t);
                }
                last_cu = cu_names.iter().position(|n| n == name).unwrap_or(last_cu);
            } else if typ == "core" || typ == "prepreg" {
                dielectrics.push((
                    last_cu,
                    t.unwrap_or(0.0),
                    l.child("material")
                        .and_then(|m| m.str_arg(1))
                        .map(|s| s.to_string()),
                ));
            }
        }
    }

    // ---- outline ----
    let mut lines: Vec<Vec<P2>> = Vec::new();
    for g in root.items() {
        let Some(h) = g.head() else { continue };
        if h.starts_with("gr_") && layer_of(g).as_deref() == Some("Edge.Cuts") {
            if let Some(p) = graphic_points(g) {
                lines.push(p);
            }
        }
    }
    // footprint edge cuts too
    for fp in root
        .items()
        .iter()
        .filter(|c| matches!(c.head(), Some("footprint") | Some("module")))
    {
        let at = fp.child("at").and_then(xy).unwrap_or([0.0, 0.0]);
        let rot = fp.child("at").and_then(|a| a.num(3)).unwrap_or(0.0);
        for g in fp.items() {
            if g.head().map(|h| h.starts_with("fp_")).unwrap_or(false)
                && layer_of(g).as_deref() == Some("Edge.Cuts")
            {
                if let Some(p) = graphic_points(g) {
                    lines.push(p.into_iter().map(|q| local_to_board(q, at, rot)).collect());
                }
            }
        }
    }
    board.outline = crate::gerber::chain_segments(&lines, 0.01);
    if board.outline.is_empty() && !lines.is_empty() {
        board
            .warnings
            .push("Edge.Cuts does not form a closed contour; using its extent".into());
        let pts: Vec<P2> = lines.iter().flatten().copied().collect();
        if let Some((lo, hi)) = bbox(&pts) {
            board.outline = vec![vec![lo, [hi[0], lo[1]], hi, [lo[0], hi[1]]]];
        }
    }

    // ---- footprints ----
    struct PadDraw {
        layer: String,
        shape: String,
        c: P2,
        size: [f64; 2],
        rot: f64,
        poly: Option<Vec<P2>>,
    }
    let mut pads: Vec<PadDraw> = Vec::new();
    for fp in root
        .items()
        .iter()
        .filter(|c| matches!(c.head(), Some("footprint") | Some("module")))
    {
        let name = fp.str_arg(1).unwrap_or("").to_string();
        let at = fp.child("at").and_then(xy).unwrap_or([0.0, 0.0]);
        // KiCad rotation is CCW in its y-down view = CCW on screen; with y flipped it stays CCW
        let rot = fp.child("at").and_then(|a| a.num(3)).unwrap_or(0.0);
        let side = match layer_of(fp).as_deref() {
            Some("B.Cu") => Side::Bottom,
            _ => Side::Top,
        };
        let mut refdes = String::new();
        for p in fp.children("property") {
            if p.str_arg(1) == Some("Reference") {
                refdes = p.str_arg(2).unwrap_or("").to_string();
            }
        }
        if refdes.is_empty() {
            for t in fp.children("fp_text") {
                if t.str_arg(1) == Some("reference") {
                    refdes = t.str_arg(2).unwrap_or("").to_string();
                }
            }
        }
        // body extent in local coordinates (unrotated): courtyard > fab > pads
        let collect = |suffix: &str| -> Vec<P2> {
            let mut pts = Vec::new();
            for g in fp.items() {
                let Some(h) = g.head() else { continue };
                if !h.starts_with("fp_") {
                    continue;
                }
                if layer_of(g).map(|l| l.ends_with(suffix)).unwrap_or(false) {
                    if let Some(p) = graphic_points(g) {
                        pts.extend(p);
                    }
                }
            }
            pts
        };
        let mut local = collect("CrtYd");
        if local.is_empty() {
            local = collect("Fab");
        }
        let mut pad_pts = Vec::new();
        for pad in fp.children("pad") {
            let shape = pad.str_arg(3).unwrap_or("").to_string();
            let pat = pad.child("at");
            let pc = pat.and_then(xy).unwrap_or([0.0, 0.0]);
            let prot_abs = pat.and_then(|a| a.num(3)).unwrap_or(0.0);
            let size = pad
                .child("size")
                .map(|s| [s.num(1).unwrap_or(0.0), s.num(2).unwrap_or(0.0)])
                .unwrap_or([0.0, 0.0]);
            pad_pts.push([pc[0] - 0.5 * size[0], pc[1] - 0.5 * size[1]]);
            pad_pts.push([pc[0] + 0.5 * size[0], pc[1] + 0.5 * size[1]]);
            // KiCad 6+: pad angle is absolute (includes the footprint rotation)
            let poly = if shape == "custom" {
                pad.child("primitives")
                    .and_then(|pr| pr.child("gr_poly"))
                    .and_then(graphic_points)
            } else {
                None
            };
            let c = local_to_board(pc, at, rot);
            let prot = if root.child("version").and_then(|v| v.num(1)).unwrap_or(0.0) >= 20210000.0
            {
                prot_abs
            } else {
                prot_abs + rot
            };
            for l in layers_of(pad) {
                pads.push(PadDraw {
                    layer: l,
                    shape: shape.clone(),
                    c,
                    size,
                    rot: prot,
                    poly: poly
                        .clone()
                        .map(|p| p.into_iter().map(|q| local_to_board(q, at, rot)).collect()),
                });
            }
            // plated through holes act as vias thermally
            if pad.str_arg(2) == Some("thru_hole") {
                if let Some(d) = pad.child("drill").and_then(|d| d.num(1)) {
                    board.drills.push(Drill {
                        x: c[0],
                        y: c[1],
                        d_mm: d,
                        plated: true,
                    });
                }
            } else if pad.str_arg(2) == Some("np_thru_hole") {
                if let Some(d) = pad.child("drill").and_then(|d| d.num(1)) {
                    board.drills.push(Drill {
                        x: c[0],
                        y: c[1],
                        d_mm: d,
                        plated: false,
                    });
                }
            }
        }
        if local.is_empty() {
            local = pad_pts;
        }
        let mut pl = Placement::new(&refdes, &name, at[0], at[1], rot, side);
        // the local frame of a bottom part is already mirrored in KiCad files
        pl.local_bbox = bbox(&local).map(|(lo, hi)| {
            if side == Side::Bottom {
                ([-hi[0], lo[1]], [-lo[0], hi[1]])
            } else {
                (lo, hi)
            }
        });
        board.components.push(pl);
    }

    // ---- copper rasters ----
    board.ensure_frame(0.1);
    let mut rasters: HashMap<String, Raster> = HashMap::new();
    for n in &cu_names {
        rasters.insert(n.clone(), board.new_raster());
    }
    let expand = |l: &str| -> Vec<String> {
        if l == "*.Cu" {
            cu_names.clone()
        } else if l == "F&B.Cu" {
            vec!["F.Cu".into(), "B.Cu".into()]
        } else {
            vec![l.to_string()]
        }
    };
    for item in root.items() {
        match item.head() {
            Some("segment") => {
                let (Some(a), Some(b)) = (
                    item.child("start").and_then(xy),
                    item.child("end").and_then(xy),
                ) else {
                    continue;
                };
                let w = item.child("width").and_then(|w| w.num(1)).unwrap_or(0.25);
                if let Some(r) = layer_of(item).and_then(|l| rasters.get_mut(&l)) {
                    r.segment(a, b, w, true);
                }
            }
            Some("arc") => {
                let w = item.child("width").and_then(|w| w.num(1)).unwrap_or(0.25);
                if let Some(pts) = graphic_points(item) {
                    if let Some(r) = layer_of(item).and_then(|l| rasters.get_mut(&l)) {
                        for s in pts.windows(2) {
                            r.segment(s[0], s[1], w, true);
                        }
                    }
                }
            }
            Some("via") => {
                let Some(c) = item.child("at").and_then(xy) else {
                    continue;
                };
                let size = item.child("size").and_then(|s| s.num(1)).unwrap_or(0.6);
                let drill = item.child("drill").and_then(|s| s.num(1)).unwrap_or(0.3);
                for l in layers_of(item) {
                    for l in expand(&l) {
                        // through vias list only their end layers: cover all between
                        let _ = l;
                    }
                }
                let ls = layers_of(item);
                let (i0, i1) = match (ls.first(), ls.last()) {
                    (Some(a), Some(b)) => (
                        cu_names.iter().position(|n| n == a).unwrap_or(0),
                        cu_names
                            .iter()
                            .position(|n| n == b)
                            .unwrap_or(cu_names.len().saturating_sub(1)),
                    ),
                    _ => (0, cu_names.len().saturating_sub(1)),
                };
                for n in &cu_names[i0.min(i1)..=i0.max(i1).min(cu_names.len().saturating_sub(1))] {
                    if let Some(r) = rasters.get_mut(n) {
                        r.disc(c, 0.5 * size, true);
                    }
                }
                board.drills.push(Drill {
                    x: c[0],
                    y: c[1],
                    d_mm: drill,
                    plated: true,
                });
            }
            Some("zone") => {
                for fp in item.children("filled_polygon") {
                    let layer = layer_of(fp).or_else(|| layer_of(item));
                    let Some(pts) = fp.child("pts") else { continue };
                    let poly: Vec<P2> = pts.children("xy").filter_map(xy).collect();
                    if let Some(r) = layer.and_then(|l| rasters.get_mut(&l)) {
                        r.fill_poly(&poly, true);
                    }
                }
            }
            _ => {}
        }
    }
    for p in &pads {
        for l in expand(&p.layer) {
            let Some(r) = rasters.get_mut(&l) else {
                continue;
            };
            match p.shape.as_str() {
                "circle" => r.disc(p.c, 0.5 * p.size[0], true),
                "oval" => r.obround(p.c, p.size[0], p.size[1], p.rot, true),
                "custom" => match &p.poly {
                    Some(poly) => r.fill_poly(poly, true),
                    None => r.rect(p.c, p.size[0], p.size[1], p.rot, true),
                },
                _ => r.rect(p.c, p.size[0], p.size[1], p.rot, true),
            }
        }
    }
    // ---- stack-up ----
    for (k, n) in cu_names.iter().enumerate() {
        board.layers.push(Layer {
            name: n.clone(),
            kind: LayerKind::Copper,
            thickness_mm: cu_thick.get(n).copied().or(Some(0.035)),
            material: None,
            copper: rasters.remove(n),
        });
        for d in dielectrics.iter().filter(|d| d.0 == k) {
            board.layers.push(Layer {
                name: "dielectric".into(),
                kind: LayerKind::Dielectric,
                thickness_mm: Some(d.1),
                material: d.2.clone(),
                copper: None,
            });
        }
    }
    board.name = "kicad_pcb".into();
    Ok(board)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_board() {
        let txt = r#"(kicad_pcb (version 20221018) (generator pcbnew)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user))
  (gr_rect (start 0 0) (end 50 -40) (layer "Edge.Cuts") (width 0.1))
  (footprint "Package_SO:SOIC-8" (layer "F.Cu") (at 25 -20 90)
    (property "Reference" "U1")
    (fp_line (start -3 -2.5) (end 3 -2.5) (layer "F.CrtYd") (width 0.05))
    (fp_line (start 3 -2.5) (end 3 2.5) (layer "F.CrtYd") (width 0.05))
    (fp_line (start 3 2.5) (end -3 2.5) (layer "F.CrtYd") (width 0.05))
    (fp_line (start -3 2.5) (end -3 -2.5) (layer "F.CrtYd") (width 0.05))
    (pad "1" smd rect (at -2.5 -1.9 90) (size 0.6 1.5) (layers "F.Cu"))
  )
  (segment (start 10 -10) (end 40 -10) (width 1) (layer "F.Cu"))
  (zone (net 0) (layer "B.Cu") (filled_polygon (layer "B.Cu") (pts (xy 0 0) (xy 50 0) (xy 50 -40) (xy 0 -40))))
)"#;
        let mut b = read(txt).unwrap();
        let s = b.summary();
        assert_eq!(s.copper_layers.len(), 2);
        assert!((s.size_mm[0] - 50.0).abs() < 1e-9 && (s.size_mm[1] - 40.0).abs() < 1e-9);
        // B.Cu fully covered; F.Cu ~ 30 mm² track + pad over 2000 mm²
        assert!(s.copper_layers[1].2 > 0.95);
        assert!(s.copper_layers[0].2 > 0.01 && s.copper_layers[0].2 < 0.03);
        let u1 = &b.components[0];
        let (lo, hi) = u1.footprint().unwrap();
        // rotated by 90°: 6 x 5 courtyard becomes 5 x 6
        assert!(((hi[0] - lo[0]) - 5.0).abs() < 1e-9 && ((hi[1] - lo[1]) - 6.0).abs() < 1e-9);
        assert!((0.5 * (lo[1] + hi[1]) - 20.0).abs() < 1e-9);
    }
}
