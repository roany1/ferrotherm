// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! IPC-2581 (rev. B/C, XML) reader.
//!
//! Read: units (CadHeader), layers and their functions, stack-up thicknesses
//! (Stackup/StackupGroup/StackupLayer), board profile (Profile with
//! Polygon/Cutout, PolyStepSegment and PolyStepCurve), packages (Outline,
//! height), components (refDes, packageRef, layerRef, Xform rotation/mirror,
//! Location), and copper features of conductor layers (Line, Arc, Polyline,
//! Contour/Polygon with cut-outs, Pad with standard primitives from the
//! dictionaries, UserSpecial groups) plus plated holes.

use crate::ecad::{Board, Drill, Layer, LayerKind, Placement, Side};
use crate::geom::{arc_points, bbox, rotate, Raster, P2};
use crate::xml::{parse, XNode};
use std::collections::HashMap;

#[derive(Debug, Clone)]
enum Shape {
    Circle(f64),
    Rect(f64, f64),
    Oval(f64, f64),
    Poly(Vec<Vec<P2>>),
}

struct Ctx {
    scale: f64,
    shapes: HashMap<String, Shape>,
    line_w: HashMap<String, f64>,
}

impl Ctx {
    fn s(&self, n: &XNode, k: &str) -> Option<f64> {
        n.num(k).map(|v| v * self.scale)
    }
}

/// Polygon element (PolyBegin + steps) → points.
fn polygon(ctx: &Ctx, p: &XNode) -> Vec<P2> {
    let mut pts: Vec<P2> = Vec::new();
    for st in &p.children {
        match st.name.as_str() {
            "PolyBegin" | "PolyStepSegment" => {
                if let (Some(x), Some(y)) = (ctx.s(st, "x"), ctx.s(st, "y")) {
                    pts.push([x, y]);
                }
            }
            "PolyStepCurve" => {
                let (Some(x), Some(y), Some(cx), Some(cy)) =
                    (ctx.s(st, "x"), ctx.s(st, "y"), ctx.s(st, "centerX"), ctx.s(st, "centerY"))
                else {
                    continue;
                };
                let Some(prev) = pts.last().copied() else {
                    pts.push([x, y]);
                    continue;
                };
                let cw = st.attr("clockwise").map(|v| v == "true").unwrap_or(false);
                let a0 = (prev[1] - cy).atan2(prev[0] - cx);
                let a1 = (y - cy).atan2(x - cx);
                let mut sw = (a1 - a0).to_degrees();
                if cw {
                    if sw >= -1e-9 {
                        sw -= 360.0;
                    }
                } else if sw <= 1e-9 {
                    sw += 360.0;
                }
                arc_points([cx, cy], prev, sw, &mut pts);
                if let Some(l) = pts.last_mut() {
                    *l = [x, y];
                }
            }
            _ => {}
        }
    }
    if pts.len() > 3 {
        let (a, b) = (pts[0], *pts.last().unwrap());
        if (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9 {
            pts.pop();
        }
    }
    pts
}

/// Contour-like element (Polygon + Cutouts) → list of contours (even-odd).
fn contours(ctx: &Ctx, n: &XNode) -> Vec<Vec<P2>> {
    let mut out = Vec::new();
    for c in &n.children {
        if c.name == "Polygon" || c.name == "Cutout" {
            let p = polygon(ctx, c);
            if p.len() >= 3 {
                out.push(p);
            }
        }
    }
    out
}

fn line_width(ctx: &Ctx, n: &XNode) -> f64 {
    if let Some(ld) = n.child("LineDesc") {
        return ctx.s(ld, "lineWidth").unwrap_or(0.0);
    }
    if let Some(r) = n.child("LineDescRef") {
        return r.attr("id").and_then(|id| ctx.line_w.get(id)).copied().unwrap_or(0.0);
    }
    0.0
}

fn shape_def(ctx: &Ctx, n: &XNode) -> Option<Shape> {
    Some(match n.name.as_str() {
        "Circle" => Shape::Circle(ctx.s(n, "diameter")?),
        "RectCenter" | "RectRound" | "RectCham" | "RectCorner" => {
            Shape::Rect(ctx.s(n, "width")?, ctx.s(n, "height")?)
        }
        "Oval" => Shape::Oval(ctx.s(n, "width")?, ctx.s(n, "height")?),
        "Contour" => Shape::Poly(contours(ctx, n)),
        "Polygon" => Shape::Poly(vec![polygon(ctx, n)]),
        "UserSpecial" => {
            // union of children; approximate with the first usable child
            return n.children.iter().find_map(|c| shape_def(ctx, c));
        }
        _ => return None,
    })
}

fn draw_shape(r: &mut Raster, s: &Shape, c: P2, rot: f64) {
    match s {
        Shape::Circle(d) => r.disc(c, 0.5 * d, true),
        Shape::Rect(w, h) => r.rect(c, *w, *h, rot, true),
        Shape::Oval(w, h) => r.obround(c, *w, *h, rot, true),
        Shape::Poly(ps) => {
            let moved: Vec<Vec<P2>> = ps
                .iter()
                .map(|p| {
                    p.iter()
                        .map(|q| {
                            let t = rotate(*q, rot);
                            [t[0] + c[0], t[1] + c[1]]
                        })
                        .collect()
                })
                .collect();
            let refs: Vec<&[P2]> = moved.iter().map(|p| p.as_slice()).collect();
            r.fill_polys(&refs, true);
        }
    }
}

fn location(ctx: &Ctx, n: &XNode) -> P2 {
    n.child("Location")
        .map(|l| [ctx.s(l, "x").unwrap_or(0.0), ctx.s(l, "y").unwrap_or(0.0)])
        .unwrap_or([0.0, 0.0])
}

fn xform(n: &XNode) -> (f64, bool) {
    match n.child("Xform") {
        Some(x) => (
            x.num("rotation").unwrap_or(0.0),
            x.attr("mirror").map(|v| v == "true").unwrap_or(false),
        ),
        None => (0.0, false),
    }
}

/// Draw the copper features of one element (recursively) into the raster.
fn draw_features(ctx: &Ctx, n: &XNode, r: &mut Raster, drills: &mut Vec<Drill>) {
    match n.name.as_str() {
        "Line" => {
            if let (Some(sx), Some(sy), Some(ex), Some(ey)) =
                (ctx.s(n, "startX"), ctx.s(n, "startY"), ctx.s(n, "endX"), ctx.s(n, "endY"))
            {
                let w = line_width(ctx, n);
                if w > 0.0 {
                    r.segment([sx, sy], [ex, ey], w, true);
                }
            }
        }
        "Arc" => {
            if let (Some(sx), Some(sy), Some(ex), Some(ey), Some(cx), Some(cy)) = (
                ctx.s(n, "startX"),
                ctx.s(n, "startY"),
                ctx.s(n, "endX"),
                ctx.s(n, "endY"),
                ctx.s(n, "centerX"),
                ctx.s(n, "centerY"),
            ) {
                let w = line_width(ctx, n);
                let cw = n.attr("clockwise").map(|v| v == "true").unwrap_or(false);
                let a0 = (sy - cy).atan2(sx - cx);
                let a1 = (ey - cy).atan2(ex - cx);
                let mut sw = (a1 - a0).to_degrees();
                if cw {
                    if sw >= -1e-9 {
                        sw -= 360.0;
                    }
                } else if sw <= 1e-9 {
                    sw += 360.0;
                }
                let mut pts = vec![[sx, sy]];
                arc_points([cx, cy], [sx, sy], sw, &mut pts);
                for s in pts.windows(2) {
                    r.segment(s[0], s[1], w, true);
                }
            }
        }
        "Polyline" => {
            let w = line_width(ctx, n);
            let pts = polygon(ctx, n);
            for s in pts.windows(2) {
                r.segment(s[0], s[1], w, true);
            }
        }
        "Contour" => {
            let cs = contours(ctx, n);
            let refs: Vec<&[P2]> = cs.iter().map(|p| p.as_slice()).collect();
            r.fill_polys(&refs, true);
        }
        "Polygon" => {
            let p = polygon(ctx, n);
            r.fill_poly(&p, true);
        }
        "Pad" => {
            let c = location(ctx, n);
            let (rot, _) = xform(n);
            let shape = n
                .child("StandardPrimitiveRef")
                .or_else(|| n.child("UserPrimitiveRef"))
                .and_then(|s| s.attr("id"))
                .and_then(|id| ctx.shapes.get(id).cloned())
                .or_else(|| n.children.iter().find_map(|ch| shape_def(ctx, ch)));
            if let Some(s) = shape {
                draw_shape(r, &s, c, rot);
            }
        }
        "Hole" => {
            if let (Some(x), Some(y), Some(d)) = (ctx.s(n, "x"), ctx.s(n, "y"), ctx.s(n, "diameter")) {
                let plated = !matches!(n.attr("platingStatus"), Some("NONPLATED"));
                drills.push(Drill { x, y, d_mm: d, plated });
            }
        }
        _ => {
            for c in &n.children {
                draw_features(ctx, c, r, drills);
            }
        }
    }
}

pub fn read(text: &str) -> Result<Board, String> {
    let root = parse(text)?;
    if root.name != "IPC-2581" {
        return Err(format!("not an IPC-2581 file (root element <{}>)", root.name));
    }
    let rev = root.attr("revision").unwrap_or("?").to_string();
    let mut board = Board {
        format: format!("IPC-2581 rev {rev}"),
        ..Default::default()
    };
    let ecad = root.find("Ecad").ok_or("no <Ecad> section")?;
    let units = ecad.find("CadHeader").and_then(|h| h.attr("units")).unwrap_or("MILLIMETER");
    let scale = match units {
        "INCH" => 25.4,
        "MICRON" => 1e-3,
        _ => 1.0,
    };
    let mut ctx = Ctx {
        scale,
        shapes: HashMap::new(),
        line_w: HashMap::new(),
    };
    // dictionaries
    let content = root.find("Content");
    if let Some(c) = content {
        let mut entries = Vec::new();
        c.descendants("EntryStandard", &mut entries);
        c.descendants("EntryUser", &mut entries);
        for e in entries {
            if let (Some(id), Some(s)) = (e.attr("id"), e.children.iter().find_map(|ch| shape_def(&ctx, ch))) {
                ctx.shapes.insert(id.to_string(), s);
            }
        }
        let mut lds = Vec::new();
        c.descendants("EntryLineDesc", &mut lds);
        for e in lds {
            if let (Some(id), Some(ld)) = (e.attr("id"), e.child("LineDesc")) {
                ctx.line_w.insert(id.to_string(), ctx.s(ld, "lineWidth").unwrap_or(0.0));
            }
        }
    }
    let cad = ecad.find("CadData").ok_or("no <CadData>")?;
    // layers
    let mut layer_fn: HashMap<String, (String, String)> = HashMap::new();
    let mut layer_order: Vec<String> = Vec::new();
    for l in cad.children("Layer") {
        if let Some(n) = l.attr("name") {
            layer_fn.insert(
                n.to_string(),
                (l.attr("layerFunction").unwrap_or("").to_string(), l.attr("side").unwrap_or("").to_string()),
            );
            layer_order.push(n.to_string());
        }
    }
    let is_copper = |f: &str| matches!(f, "CONDUCTOR" | "SIGNAL" | "PLANE" | "MIXED" | "CONDFILM" | "CONDFOIL");
    let is_diel = |f: &str| f.starts_with("DIEL");
    // stack-up
    let mut stack: Vec<(String, f64, i64)> = Vec::new();
    if let Some(su) = cad.find("Stackup") {
        board.thickness_mm = su.num("overallThickness").map(|t| t * scale);
        let mut ls = Vec::new();
        su.descendants("StackupLayer", &mut ls);
        for (k, l) in ls.iter().enumerate() {
            let name = l.attr("layerOrGroupRef").unwrap_or("").to_string();
            stack.push((name, l.num("thickness").unwrap_or(0.0) * scale, l.num("sequence").map(|s| s as i64).unwrap_or(k as i64)));
        }
        stack.sort_by_key(|s| s.2);
        if board.thickness_mm.is_none() {
            board.thickness_mm = su.find("StackupGroup").and_then(|g| g.num("thickness")).map(|t| t * scale);
        }
    }
    let step = cad.find("Step").ok_or("no <Step>")?;
    board.name = step.attr("name").unwrap_or("").to_string();
    // profile
    if let Some(p) = step.child("Profile") {
        board.outline = contours(&ctx, p);
    }
    if board.outline.is_empty() {
        board.warnings.push("no board Profile; using the copper/component extent".into());
    }
    // packages
    let mut pkgs: HashMap<String, ((P2, P2), Option<f64>)> = HashMap::new();
    for p in step.children("Package") {
        let Some(name) = p.attr("name") else { continue };
        let mut pts = Vec::new();
        if let Some(o) = p.child("Outline") {
            for poly in o.children("Polygon") {
                pts.extend(polygon(&ctx, poly));
            }
        }
        if pts.is_empty() {
            // fall back to pin locations
            for pin in p.children("Pin") {
                pts.push(location(&ctx, pin));
            }
        }
        if let Some(bb) = bbox(&pts) {
            pkgs.insert(name.to_string(), (bb, ctx.s(p, "height")));
        }
    }
    // components
    for c in step.children("Component") {
        let refdes = c.attr("refDes").unwrap_or("").to_string();
        let pkg = c.attr("packageRef").unwrap_or("").to_string();
        let lay = c.attr("layerRef").unwrap_or("");
        let side = match layer_fn.get(lay).map(|x| x.1.as_str()) {
            Some("BOTTOM") => Side::Bottom,
            _ => Side::Top,
        };
        let (rot, mirror) = xform(c);
        let loc = location(&ctx, c);
        let side = if mirror { Side::Bottom } else { side };
        let mut pl = Placement::new(&refdes, &pkg, loc[0], loc[1], rot, side);
        pl.part = c.attr("part").unwrap_or("").to_string();
        if let Some((bb, h)) = pkgs.get(&pkg) {
            pl.local_bbox = Some(*bb);
            pl.height_mm = *h;
        } else {
            board.warnings.push(format!("{refdes}: package '{pkg}' not defined"));
        }
        board.components.push(pl);
    }
    // copper
    board.ensure_frame(0.1);
    let mut copper: HashMap<String, Raster> = HashMap::new();
    let mut drills = Vec::new();
    for lf in step.children("LayerFeature") {
        let Some(lr) = lf.attr("layerRef") else { continue };
        let f = layer_fn.get(lr).map(|x| x.0.as_str()).unwrap_or("");
        if is_copper(f) {
            let r = copper.entry(lr.to_string()).or_insert_with(|| {
                let (lo, hi, res) = board.frame.unwrap();
                Raster::new(lo, hi, res)
            });
            draw_features(&ctx, lf, r, &mut Vec::new());
        } else if f == "DRILL" {
            let mut sink = Raster::new([0.0, 0.0], [1.0, 1.0], 1.0);
            draw_features(&ctx, lf, &mut sink, &mut drills);
        }
    }
    board.drills = drills;
    // layers in stack order (fall back to declaration order)
    let order: Vec<String> = if stack.is_empty() {
        layer_order.clone()
    } else {
        stack.iter().map(|s| s.0.clone()).collect()
    };
    for name in order {
        let Some((f, _)) = layer_fn.get(&name) else { continue };
        let t = stack.iter().find(|s| s.0 == name).map(|s| s.1);
        if is_copper(f) {
            board.layers.push(Layer {
                name: name.clone(),
                kind: LayerKind::Copper,
                thickness_mm: t,
                material: None,
                copper: copper.remove(&name),
            });
        } else if is_diel(f) {
            board.layers.push(Layer {
                name: name.clone(),
                kind: LayerKind::Dielectric,
                thickness_mm: t,
                material: None,
                copper: None,
            });
        }
    }
    Ok(board)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Structure as written by the KiCad IPC-2581 exporter (rev C).
    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <DictionaryLineDesc units="MILLIMETER">
      <EntryLineDesc id="LINE_1"><LineDesc lineEnd="ROUND" lineWidth="1.0"/></EntryLineDesc>
    </DictionaryLineDesc>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="RECT_1"><RectCenter width="2.0" height="1.0"/></EntryStandard>
      <EntryStandard id="CIRCLE_1"><Circle diameter="1.0"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad name="Design">
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Layer name="DIELECTRIC_1" layerFunction="DIELCORE" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="B.Cu" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="DRILL_1_2" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
      <Stackup name="Primary_Stackup" overallThickness="1.6" whereMeasured="METAL">
        <StackupGroup name="Primary_Stackup_Group" thickness="1.6">
          <StackupLayer layerOrGroupRef="F.Cu" thickness="0.035" sequence="0"/>
          <StackupLayer layerOrGroupRef="DIELECTRIC_1" thickness="1.53" sequence="1"/>
          <StackupLayer layerOrGroupRef="B.Cu" thickness="0.035" sequence="2"/>
        </StackupGroup>
      </Stackup>
      <Step name="board">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="40" y="0"/>
            <PolyStepCurve x="50" y="10" centerX="40" centerY="10" clockwise="false"/>
            <PolyStepSegment x="50" y="30"/>
            <PolyStepSegment x="0" y="30"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <Package name="SOIC8" type="OTHER" pinOne="1" height="1.75">
          <Outline><Polygon>
            <PolyBegin x="-2.5" y="-2.0"/><PolyStepSegment x="2.5" y="-2.0"/>
            <PolyStepSegment x="2.5" y="2.0"/><PolyStepSegment x="-2.5" y="2.0"/>
            <PolyStepSegment x="-2.5" y="-2.0"/>
          </Polygon><LineDescRef id="LINE_1"/></Outline>
        </Package>
        <Component refDes="U1" packageRef="SOIC8" part="LM358" layerRef="F.Cu" mountType="SMT">
          <Xform rotation="90.00"/>
          <Location x="20" y="15"/>
        </Component>
        <Component refDes="U2" packageRef="SOIC8" part="LM358" layerRef="B.Cu" mountType="SMT">
          <Xform rotation="0" mirror="true"/>
          <Location x="35" y="15"/>
        </Component>
        <LayerFeature layerRef="F.Cu">
          <Set net="GND"><Features><Line startX="5" startY="5" endX="35" endY="5"><LineDescRef id="LINE_1"/></Line></Features></Set>
          <Set padUsage="TERMINATION"><Pad><Xform rotation="90"/><Location x="10" y="20"/><StandardPrimitiveRef id="RECT_1"/></Pad></Set>
        </LayerFeature>
        <LayerFeature layerRef="B.Cu">
          <Set net="GND"><Features><Contour><Polygon>
            <PolyBegin x="0" y="0"/><PolyStepSegment x="50" y="0"/><PolyStepSegment x="50" y="30"/>
            <PolyStepSegment x="0" y="30"/><PolyStepSegment x="0" y="0"/>
          </Polygon><Cutout>
            <PolyBegin x="10" y="10"/><PolyStepSegment x="20" y="10"/><PolyStepSegment x="20" y="20"/>
            <PolyStepSegment x="10" y="20"/><PolyStepSegment x="10" y="10"/>
          </Cutout></Contour></Features></Set>
        </LayerFeature>
        <LayerFeature layerRef="DRILL_1_2">
          <Set><Hole name="H1" diameter="0.3" platingStatus="VIA" plusTol="0" minusTol="0" x="25" y="25"/></Set>
          <Set><Hole name="H2" diameter="3.2" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="5" y="25"/></Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

    #[test]
    fn kicad_style_ipc2581() {
        let mut b = read(SAMPLE).unwrap();
        assert!((b.thickness_mm.unwrap() - 1.6).abs() < 1e-12);
        let s = b.summary();
        assert_eq!(s.copper_layers.len(), 2);
        assert_eq!(b.layers.len(), 3);
        // outline area: 50x30 minus the corner square plus the quarter disc (r = 10)
        let a = crate::geom::poly_area(&b.outline[0]).abs();
        let exact = 1500.0 - 100.0 + std::f64::consts::PI * 25.0;
        assert!((a / exact - 1.0).abs() < 2e-3, "{a} {exact}");
        // F.Cu: line 30x1 + caps + 1x2 pad = 30 + pi/4 + 2 mm²
        let fcu = s.copper_layers[0].2 * exact;
        assert!((fcu - (32.0 + std::f64::consts::FRAC_PI_4)).abs() < 1.0, "{fcu}");
        // B.Cu: plane with a 10x10 cut-out, clipped to the outline
        let bcu = s.copper_layers[1].2 * exact;
        assert!((bcu - (exact - 100.0)).abs() < 3.0, "{bcu}");
        let u1 = b.components.iter().find(|c| c.refdes == "U1").unwrap();
        let (lo, hi) = u1.footprint().unwrap();
        assert!(((hi[0] - lo[0]) - 4.0).abs() < 1e-9 && ((hi[1] - lo[1]) - 5.0).abs() < 1e-9);
        assert_eq!(u1.height_mm, Some(1.75));
        let u2 = b.components.iter().find(|c| c.refdes == "U2").unwrap();
        assert_eq!(u2.side, Side::Bottom);
        assert_eq!(b.drills.len(), 2);
        assert!(b.drills[0].plated && !b.drills[1].plated);
    }
}
