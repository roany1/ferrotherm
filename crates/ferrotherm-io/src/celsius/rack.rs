// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Rack and server objects of a Celsius model: vents and fans in walls,
//! power supplies, chassis bays with slots holding nested servers, card
//! bays with their cards and backplane, drive bays, chip sockets, daughter
//! board (DIMM) sockets, heat sinks and TIMs attached to a component.
//!
//! Placement conventions were checked against the STEP export made by
//! Celsius EC Solver 2024.36 of its "Full Enclosure" tutorial model:
//! * a box side "Front" is at +Z, "Rear" at Z = 0;
//! * vent offsets are the low corner of the vent, fan offsets the centre of
//!   the fan (Offset_1 along X on front/rear walls, Offset_2 along Y);
//! * a fan mounted inside spans its thickness inwards from the outer face
//!   of the wall; the hub is a non-flowing disc;
//! * a server in a chassis-bay slot is rotated by Orientation_Angle about
//!   the bay depth axis and fills the slot from its low corner, at the bay
//!   rear ("Rear" alignment);
//! * card-bay slots are counted from the top ("Top to Bottom"), the socket
//!   sits Offset_2 above the slot floor, the card runs from the socket into
//!   the bay, centred on the socket thickness, and the backplane is behind
//!   the socket face (outside the bay box);
//! * a chip sits centred on its socket; a DIMM stands on the board along the
//!   socket length, centred on the socket width;
//! * drives are centred in their bay with rails filling the side gaps.
//!
//! Bay sides are exported by Celsius as zero-thickness plates; here they get
//! their thickness inside the bay box.

use super::*;
use ferrotherm_core::case::{Fan, Loss, Region2, Resistance};
use std::f64::consts::PI;

/// Sides of a Celsius box: (name, local axis, at the max end).
pub(super) const SIDES: [(&str, usize, bool); 6] = [
    ("Left_Side", 0, false),
    ("Right_Side", 0, true),
    ("Bottom", 1, false),
    ("Top", 1, true),
    // Front is +Z, Rear Z = 0 (STEP export of Celsius 2024.36)
    ("Front", 2, true),
    ("Rear", 2, false),
];

/// In-plane local axes (u = Offset_1, v = Offset_2) of a side normal to `axis`.
fn side_axes(axis: usize) -> (usize, usize) {
    match axis {
        0 => (2, 1),
        1 => (0, 2),
        _ => (0, 1),
    }
}

/// Short side name of feature placements ("Left") → `SIDES` name.
fn side_name(s: &str) -> &'static str {
    match s.trim().to_ascii_lowercase().as_str() {
        "left" | "left_side" => "Left_Side",
        "right" | "right_side" => "Right_Side",
        "top" => "Top",
        "bottom" => "Bottom",
        "rear" | "back" => "Rear",
        _ => "Front",
    }
}

/// Vents and fans of one side.
pub(super) struct Features<'a> {
    pub vents: Vec<&'a XNode>,
    pub fans: Vec<&'a XNode>,
}

impl<'a> Features<'a> {
    pub fn of(side: &'a XNode) -> Features<'a> {
        Features {
            vents: side
                .child("Vent_Openings")
                .map(|l| l.children("Vent_Opening").collect())
                .unwrap_or_default(),
            fans: side
                .child("Fans")
                .map(|l| l.children("Fan").collect())
                .unwrap_or_default(),
        }
    }
}

fn point(axis: usize, at: f64, a: f64, b: f64) -> V3 {
    let (u, v) = side_axes(axis);
    let mut p = [0.0; 3];
    p[axis] = at;
    p[u] = a;
    p[v] = b;
    p
}

fn ft_box(ctx: &Ctx, pose: &Pose, pts: &[V3]) -> (V3, V3) {
    let mut a = [f64::INFINITY; 3];
    let mut b = [f64::NEG_INFINITY; 3];
    for p in pts {
        let w = ctx.f.p(pose.apply(*p));
        for k in 0..3 {
            a[k] = a[k].min(w[k]);
            b[k] = b[k].max(w[k]);
        }
    }
    (a, b)
}

fn opening(name: String, face: Face, lo: V3, hi: V3, loss: Option<Loss>) -> Boundary {
    let t = face.tangential();
    Boundary {
        name,
        face,
        region: Some(Region2 {
            min: [lo[t[0]], lo[t[1]]],
            max: [hi[t[0]], hi[t[1]]],
        }),
        kind: BoundaryKind::Opening,
        t_c: None,
        h: None,
        t_ambient_c: None,
        emissivity: None,
        finish: None,
        t_radiation_c: None,
        heat_flux_w_m2: None,
        solar_w_m2: None,
        absorptivity: None,
        velocity_m_s: None,
        flow_m3_s: None,
        loss,
    }
}

/// A wall of a box (local frame `pose`, box 0..size) with the holes of its
/// vents and fans; vents become openings (outer domain faces) or planar
/// resistances, fans become fans.
#[allow(clippy::too_many_arguments)]
pub(super) fn wall(
    ctx: &mut Ctx,
    name: &str,
    pose: &Pose,
    size: V3,
    (axis, max): (usize, bool),
    t: f64,
    mat: String,
    feats: &Features,
) {
    wall_in(
        ctx,
        name,
        pose,
        size,
        ([0.0; 3], size),
        (axis, max),
        t,
        mat,
        feats,
    );
}

/// As [`wall`], the wall spanning only `extent` in its plane (the walls of
/// a Celsius chassis do not overlap: Top and Bottom are whole, Left and
/// Right fit between them, Front and Rear inside all four).
#[allow(clippy::too_many_arguments)]
pub(super) fn wall_in(
    ctx: &mut Ctx,
    name: &str,
    pose: &Pose,
    size: V3,
    extent: (V3, V3),
    (axis, max): (usize, bool),
    t: f64,
    mat: String,
    feats: &Features,
) {
    let mut lo = extent.0;
    let mut hi = extent.1;
    lo[axis] = 0.0;
    hi[axis] = size[axis];
    if max {
        lo[axis] = size[axis] - t;
    } else {
        hi[axis] = t;
    }
    let mut holes = Vec::new();
    for v in &feats.vents {
        if let Some(h) = vent(ctx, name, pose, size, (axis, max), v) {
            holes.push(h);
        }
    }
    for f in &feats.fans {
        if let Some(h) = fan(ctx, name, pose, size, (axis, max), t, &mat, f) {
            holes.push(h);
        }
    }
    ctx.block_holes(pose, name, mat, lo, hi, axis, &holes);
}

/// Pressure loss of a vent (None: open hole).
fn vent_loss(ctx: &mut Ctx, n: &XNode, name: &str) -> Option<Loss> {
    let c = at(n, "Construction")?;
    let spec = txt(c, "Specification")
        .unwrap_or("Open Hole")
        .to_ascii_lowercase();
    if spec.contains("open") {
        return None;
    }
    let (det, size) = if spec.contains("perf") {
        ("Perforation_Details", "Hole_Size")
    } else if spec.contains("slot") {
        ("Slotted_Details", "Slot_Width")
    } else if spec.contains("mesh") {
        ("Mesh_Details", "Strand_Diameter")
    } else {
        ctx.warnings.push(format!(
            "vent {name}: specification '{spec}' imported as an open hole"
        ));
        return None;
    };
    let d = at(c, det)?;
    let f = txt(d, "Open_Area").and_then(pct)?;
    let hole = mm(d, size).or_else(|| mm(d, "Hole_Diameter"));
    Some(Loss::thin_plate(f, hole))
}

fn vent(
    ctx: &mut Ctx,
    owner: &str,
    pose: &Pose,
    size: V3,
    (axis, max): (usize, bool),
    n: &XNode,
) -> Option<Vec<V3>> {
    ctx.count("Vent_Opening");
    if !installed(n) {
        ctx.uninstalled += 1;
        return None;
    }
    let name = format!("{owner}/{}", name_of(n));
    let o1 = mm(n, "Placement/Offset_1").unwrap_or(0.0);
    let o2 = mm(n, "Placement/Offset_2").unwrap_or(0.0);
    let def = txt(n, "Geometry/Geometry_Definition")
        .unwrap_or("Rectangular")
        .to_ascii_lowercase();
    let (w, l) = if def.starts_with("rect") {
        (mm(n, "Geometry/Width")?, mm(n, "Geometry/Length")?)
    } else if def.starts_with("circ") {
        let d = mm(n, "Geometry/Diameter")?;
        ctx.warnings.push(format!(
            "vent {name}: circular vent imported as the square of the same area at its offsets (convention not verified)"
        ));
        let s = d * (PI / 4.0).sqrt();
        (s, s)
    } else {
        ctx.warnings.push(format!(
            "vent {name}: geometry '{def}' not supported, skipped"
        ));
        return None;
    };
    let outer = if max { size[axis] } else { 0.0 };
    let ring = vec![
        point(axis, outer, o1, o2),
        point(axis, outer, o1 + w, o2),
        point(axis, outer, o1 + w, o2 + l),
        point(axis, outer, o1, o2 + l),
    ];
    let loss = vent_loss(ctx, n, &name);
    let (a, b) = ft_box(ctx, pose, &ring);
    let (ca, sg) = pose.axis(axis);
    let fa = ft_axis(ca, sg).0;
    let vname = ctx.unique(&name);
    if let Some(face) = ctx.domain_face(fa, a[fa]) {
        ctx.openings.push(opening(vname, face, a, b, loss));
    } else if let Some(loss) = loss {
        let tg = ferrotherm_core::model::tangential(fa);
        let mut center = [0.0; 3];
        for k in 0..3 {
            center[k] = 0.5 * (a[k] + b[k]);
        }
        ctx.case.resistances.push(Resistance {
            name: vname,
            center,
            axis: AXIS_NAME[fa].into(),
            size: [b[tg[0]] - a[tg[0]], b[tg[1]] - a[tg[1]]],
            loss,
        });
    }
    Some(ring)
}

/// Volume flow in m³/s from a value with its unit.
fn flow_m3s(v: f64, unit: &str) -> f64 {
    let u = unit.to_ascii_lowercase().replace(['^', ' '], "");
    v * if u.contains("cfm") || u.contains("ft3/min") {
        4.719_474_432e-4
    } else if u.contains("l/s") {
        1e-3
    } else if u.contains("l/min") {
        1e-3 / 60.0
    } else if u.contains("m3/h") {
        1.0 / 3600.0
    } else if u.contains("m3/min") {
        1.0 / 60.0
    } else {
        1.0
    }
}

/// Fan curve (m³/s, Pa) or fixed flow; speed scaled by the fan laws.
fn fan_flow(n: &XNode) -> (Vec<[f64; 2]>, Option<f64>) {
    let Some(c) = at(n, "Cooling") else {
        return (vec![], None);
    };
    let rated = txt(c, "Rated_Speed").and_then(val);
    let op = if txt(c, "Operating_Speed").is_none_or(|s| s.eq_ignore_ascii_case("Fixed")) {
        txt(c, "Fixed_Speed").and_then(val)
    } else {
        None
    };
    let k = match (rated, op) {
        (Some(r), Some(o)) if r > 0.0 && o > 0.0 => o / r,
        _ => 1.0,
    };
    let fixed =
        txt(c, "Flow_Rate_Option").is_some_and(|o| !o.to_ascii_lowercase().contains("curve"));
    if fixed {
        let q = txt(c, "Fan_Flow_Rate").and_then(|s| split(s).map(|(v, u)| flow_m3s(v, u)));
        return (vec![], q);
    }
    let text = txt(c, "Fan_Curve").unwrap_or("");
    let lower = text.to_ascii_lowercase();
    // the unit tag "<cfm,Pa>" arrives with or without its brackets (the
    // XML reader drops entities): look for the flow unit in the whole text
    let unit_q = lower.clone();
    let p_scale = if lower.contains("inh2o") || lower.contains("in h2o") {
        249.0889
    } else if lower.contains("mmh2o") || lower.contains("mm h2o") {
        9.80665
    } else {
        1.0
    };
    let mut pts: Vec<[f64; 2]> = curve(text)
        .into_iter()
        .map(|p| [flow_m3s(p[0], &unit_q) * k, p[1] * p_scale * k * k])
        .collect();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
    pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-12);
    (pts, None)
}

/// A fan in a wall: hole, hub block (with the fan heat), fan plane at the
/// inner face of the housing, opening on the outer domain face.
#[allow(clippy::too_many_arguments)]
fn fan(
    ctx: &mut Ctx,
    owner: &str,
    pose: &Pose,
    size: V3,
    (axis, max): (usize, bool),
    wall_t: f64,
    mat: &str,
    n: &XNode,
) -> Option<Vec<V3>> {
    ctx.count("Fan");
    if !installed(n) {
        ctx.uninstalled += 1;
        return None;
    }
    let name = format!("{owner}/{}", name_of(n));
    let g = at(n, "Geometry")?;
    let dia = mm(g, "Fan_Diameter").or_else(|| mm(g, "Overall_Size"))?;
    let housing = mm(g, "Overall_Size").unwrap_or(dia).max(dia);
    let th = mm(g, "Thickness")
        .or_else(|| mm(g, "Fan_Housing_Thickness"))
        .unwrap_or(25.0)
        .max(wall_t + 0.1);
    let hub = mm(g, "Hub_Diameter").filter(|h| *h > 0.0 && *h < dia);
    let o1 = mm(n, "Placement/Offset_1").unwrap_or(0.0);
    let o2 = mm(n, "Placement/Offset_2").unwrap_or(0.0);
    if txt(n, "Placement/Mount_Location").is_some_and(|m| m.eq_ignore_ascii_case("Outside")) {
        ctx.warnings.push(format!(
            "fan {name}: mounted outside, imported inside the wall"
        ));
    }
    let outer = if max { size[axis] } else { 0.0 };
    let s_in = if max { -1.0 } else { 1.0 };
    let plane = outer + s_in * th;
    let r = 0.5 * dia;
    let ring: Vec<V3> = (0..32)
        .map(|k| {
            let a = 2.0 * PI * k as f64 / 32.0;
            point(axis, outer, o1 + r * a.cos(), o2 + r * a.sin())
        })
        .collect();
    // fan heat into the hub (a square of the hub area)
    let heat = at(n, "Cooling/Heat_Dissipation")
        .filter(|h| {
            txt(h, "Heat_Specification").is_none_or(|s| s.to_ascii_lowercase().contains("fixed"))
        })
        .and_then(|h| txt(h, "Heat_Dissipated"))
        .and_then(power_w)
        .unwrap_or(0.0);
    let hub_d = hub.unwrap_or(0.0);
    if hub_d > 0.0 || heat > 0.0 {
        let a = if hub_d > 0.0 { hub_d } else { 0.2 * dia } * PI.sqrt() / 2.0;
        let (z0, z1) = {
            let p = outer + s_in * wall_t;
            (p.min(plane), p.max(plane))
        };
        let mut lo = point(axis, z0, o1 - a / 2.0, o2 - a / 2.0);
        let mut hi = point(axis, z1, o1 + a / 2.0, o2 + a / 2.0);
        for k in 0..3 {
            let (x, y) = (lo[k].min(hi[k]), lo[k].max(hi[k]));
            lo[k] = x;
            hi[k] = y;
        }
        ctx.block_in(pose, &format!("{name}/hub"), mat.to_string(), lo, hi, heat);
    }
    let out = txt(n, "Cooling/Flow_Direction").is_some_and(|d| d.eq_ignore_ascii_case("Out"));
    let dir_local = if max { 1.0 } else { -1.0 } * if out { 1.0 } else { -1.0 };
    let (ca, sg) = pose.axis(axis);
    let (fa, fs) = ft_axis(ca, sg * dir_local);
    let center = ctx.f.p(pose.apply(point(axis, plane, o1, o2)));
    let fname = ctx.unique(&name);
    let on = txt(n, "Status/Fan_Status").is_none_or(|s| !s.eq_ignore_ascii_case("Off"));
    if on {
        let (curve, flow) = fan_flow(n);
        if curve.len() < 2 && flow.is_none() {
            ctx.warnings
                .push(format!("fan {name}: no usable fan curve, skipped"));
        } else {
            ctx.case.fans.push(Fan {
                name: fname.clone(),
                center,
                direction: format!("{}{}", if fs > 0.0 { "+" } else { "-" }, AXIS_NAME[fa]),
                diameter: Some(dia),
                hub_diameter: hub,
                size: None,
                curve,
                flow_m3_s: flow,
            });
        }
    } else {
        let f = txt(n, "Status/Open_Area").and_then(pct).unwrap_or(0.3);
        ctx.case.resistances.push(Resistance {
            name: fname.clone(),
            center,
            axis: AXIS_NAME[fa].into(),
            size: [dia, dia],
            loss: Loss::thin_plate(f, None),
        });
        ctx.warnings.push(format!(
            "fan {name}: switched off, imported as a grille of {:.0} % open area",
            f * 100.0
        ));
    }
    // a fan in an outer wall opens the domain face, unless a vent does
    let sq = [
        point(axis, outer, o1 - housing / 2.0, o2 - housing / 2.0),
        point(axis, outer, o1 + housing / 2.0, o2 + housing / 2.0),
    ];
    let (a, b) = ft_box(ctx, pose, &sq);
    if let Some(face) = ctx.domain_face(fa, a[fa]) {
        let t = face.tangential();
        let covered = ctx.openings.iter().any(|o| {
            o.face == face
                && o.region.as_ref().is_some_and(|r| {
                    r.min[0] <= a[t[0]] + 1e-6
                        && r.min[1] <= a[t[1]] + 1e-6
                        && r.max[0] >= b[t[0]] - 1e-6
                        && r.max[1] >= b[t[1]] - 1e-6
                })
        });
        if !covered {
            ctx.openings
                .push(opening(format!("{fname}/opening"), face, a, b, None));
        }
    }
    Some(ring)
}

fn box_pose(ctx: &mut Ctx, n: &XNode, what: &str) -> Pose {
    let (r, note) = orientation(n);
    if let Some(note) = note {
        ctx.warnings.push(format!("{what} {}: {note}", name_of(n)));
    }
    if !axis_aligned(&r) {
        ctx.warnings.push(format!(
            "{what} {}: tilted (not a multiple of 90°), imported as its axis-aligned bounding box",
            name_of(n)
        ));
    }
    ctx.base.then(&Pose {
        r,
        t: [
            mm(n, "Placement/X_Location").unwrap_or(0.0),
            mm(n, "Placement/Y_Location").unwrap_or(0.0),
            mm(n, "Placement/Z_Location").unwrap_or(0.0),
        ],
    })
}

fn size_of(n: &XNode) -> Option<V3> {
    Some([
        mm(n, "Geometry/Width")?,
        mm(n, "Geometry/Height")?,
        mm(n, "Geometry/Depth")?,
    ])
}

/// Bay sides (`kind` = "Chassis_Bay_Side", …): thickness inside the box.
fn bay_sides(ctx: &mut Ctx, n: &XNode, name: &str, pose: &Pose, size: V3, kind: &str) {
    for (side, axis, max) in SIDES {
        let Some(s) = at(n, &format!("{side}/{kind}")) else {
            continue;
        };
        if !installed(s) {
            ctx.uninstalled += 1;
            continue;
        }
        let t = mm(s, "Construction/Thickness").unwrap_or(1.0).max(0.01);
        let mat = ctx.mat(txt(s, "Construction/MaterialLink"), "al-6061-t6");
        let feats = Features::of(s);
        wall(
            ctx,
            &format!("{name}/{}", side.to_ascii_lowercase()),
            pose,
            size,
            (axis, max),
            t,
            mat,
            &feats,
        );
    }
}

/// Distributed power of a board.
fn board_power(p: &XNode) -> f64 {
    txt(p, "Power/Top_Distributed_Power")
        .and_then(power_w)
        .unwrap_or(0.0)
        + txt(p, "Power/Bottom_Distributed_Power")
            .and_then(power_w)
            .unwrap_or(0.0)
}

// ------------------------------------------------------------------ power supply

pub(super) fn power_supply(ctx: &mut Ctx, n: &XNode) {
    ctx.count("Power_Supply");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let Some(size) = size_of(n) else {
        ctx.warnings
            .push(format!("power supply {name}: no size, skipped"));
        return;
    };
    let pose = box_pose(ctx, n, "power supply");
    let t = mm(n, "Construction/Thickness").unwrap_or(1.0).max(0.01);
    let mat = ctx.mat(txt(n, "Construction/MaterialLink"), "al-6061-t6");
    let vents: Vec<&XNode> = n
        .child("Vent_Openings")
        .map(|l| l.children("Vent_Opening").collect())
        .unwrap_or_default();
    let fans: Vec<&XNode> = n
        .child("Fans")
        .map(|l| l.children("Fan").collect())
        .unwrap_or_default();
    let mut flow_axis = None;
    let mut margin = [0.0f64; 2]; // fan housings at the low / high end of the flow axis
    for (side, axis, max) in SIDES {
        let feats = Features {
            vents: vents
                .iter()
                .copied()
                .filter(|v| {
                    side_name(txt(v, "Placement/Side_Of_Supply").unwrap_or("Front")) == side
                })
                .collect(),
            fans: fans
                .iter()
                .copied()
                .filter(|f| side_name(txt(f, "Placement/Side").unwrap_or("Front")) == side)
                .collect(),
        };
        if !feats.fans.is_empty() || !feats.vents.is_empty() {
            flow_axis.get_or_insert(axis);
        }
        for f in &feats.fans {
            let th = mm(f, "Geometry/Thickness").unwrap_or(0.0);
            let e = &mut margin[max as usize];
            *e = e.max(th);
        }
        wall(
            ctx,
            &format!("{name}/{}", side.to_ascii_lowercase()),
            &pose,
            size,
            (axis, max),
            t,
            mat.clone(),
            &feats,
        );
    }
    // internals: the blockage as heated plates along the flow
    let power = txt(n, "Power/Calculated_Power")
        .and_then(power_w)
        .filter(|p| *p > 0.0)
        .unwrap_or_else(|| power_of(n));
    let block = txt(n, "Construction/Internal_Blockage_Percentage")
        .and_then(pct)
        .unwrap_or(0.4)
        .clamp(0.05, 0.9);
    let fa = flow_axis.unwrap_or(2);
    // plates normal to the widest cross axis
    let cross: Vec<usize> = (0..3).filter(|&k| k != fa).collect();
    let (pn, other) = if size[cross[0]] >= size[cross[1]] {
        (cross[0], cross[1])
    } else {
        (cross[1], cross[0])
    };
    let np = 10usize;
    let span = size[pn] - 2.0 * t;
    let tp = block * span / np as f64;
    let gap = (span - np as f64 * tp) / np as f64;
    let z0 = t + margin[0] + 2.0;
    let z1 = size[fa] - t - margin[1] - 2.0;
    if span > 0.0 && z1 > z0 {
        for k in 0..np {
            let mut lo = [0.0; 3];
            let mut hi = [0.0; 3];
            lo[pn] = t + 0.5 * gap + k as f64 * (tp + gap);
            hi[pn] = lo[pn] + tp;
            lo[other] = t;
            hi[other] = size[other] - t;
            lo[fa] = z0;
            hi[fa] = z1;
            ctx.block_in(
                &pose,
                &format!("{name}/internals{}", k + 1),
                mat.clone(),
                lo,
                hi,
                power / np as f64,
            );
        }
        ctx.warnings.push(format!(
            "power supply {name}: {power:.1} W in {np} plates along the flow blocking {:.0} % of the section (Celsius: internal blockage)",
            block * 100.0
        ));
    }
}

// ------------------------------------------------------------------ bays

pub(super) fn chassis_bay(ctx: &mut Ctx, n: &XNode, cad: &Cad) {
    ctx.count("Chassis_Bay");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let Some(size) = size_of(n) else { return };
    let pose = box_pose(ctx, n, "chassis bay");
    bay_sides(ctx, n, &name, &pose, size, "Chassis_Bay_Side");
    let Some(sd) = at(n, "Construction/Slot_Definition") else {
        return;
    };
    let ss = mm(sd, "Slot_Size").unwrap_or(size[0]);
    let first = mm(sd, "First_Slot_Offset").unwrap_or(0.0);
    let dir = txt(sd, "Slot_Direction").unwrap_or("Left to Right");
    let Some(slots) = sd.child("SlotsLink") else {
        return;
    };
    for slot in slots.children("Chassis_Slot") {
        let idx = txt(slot, "Index").and_then(val).unwrap_or(1.0).max(1.0) as usize;
        let Some(srv) = at(slot, "ChassisLink/Chassis") else {
            continue;
        };
        if !installed(srv) {
            ctx.uninstalled += 1;
            continue;
        }
        let Some(ssize) = size_of(srv) else { continue };
        let c = at(slot, "Construction");
        let ang = c
            .and_then(|c| txt(c, "Orientation_Angle"))
            .and_then(val)
            .unwrap_or(0.0);
        if (ang / 90.0 - (ang / 90.0).round()).abs() > 1e-6 {
            ctx.warnings.push(format!(
                "chassis bay {name}: slot {idx} angle {ang}° not a multiple of 90°, imported as its bounding box"
            ));
        }
        let rz = rot_about([0.0, 0.0, 1.0], ang);
        let (rlo, rhi) = Pose { r: rz, t: [0.0; 3] }.bx([0.0; 3], ssize);
        let ext = [rhi[0] - rlo[0], rhi[1] - rlo[1], rhi[2] - rlo[2]];
        let (x0, y0) = slot_origin(dir, idx, ss, first, size);
        let off = |k: &str| c.and_then(|c| mm(c, k)).unwrap_or(0.0);
        let front = c
            .and_then(|c| txt(c, "Chassis_Alignment"))
            .is_some_and(|a| a.eq_ignore_ascii_case("Front"));
        let tz = if front {
            size[2] - ext[2] - off("Chassis_Depth_Offset")
        } else {
            off("Chassis_Depth_Offset")
        };
        let target = [
            x0 + off("Chassis_Horizontal_Offset"),
            y0 + off("Chassis_Vertical_Offset"),
            tz,
        ];
        let local = Pose {
            r: rz,
            t: [target[0] - rlo[0], target[1] - rlo[1], target[2] - rlo[2]],
        };
        let saved = (ctx.base, ctx.prefix.clone());
        ctx.base = pose.then(&local);
        ctx.prefix = ctx.pref(&format!("{name}/S{idx}"));
        chassis(ctx, srv, cad, false);
        (ctx.base, ctx.prefix) = saved;
    }
}

/// Low corner (x, y) of slot `idx` (1-based) in a bay of `size`.
fn slot_origin(dir: &str, idx: usize, ss: f64, first: f64, size: V3) -> (f64, f64) {
    let i = idx as f64;
    match dir.to_ascii_lowercase().as_str() {
        "right to left" => (size[0] - first - i * ss, 0.0),
        "bottom to top" => (0.0, first + (i - 1.0) * ss),
        "top to bottom" => (0.0, size[1] - first - i * ss),
        _ => (first + (i - 1.0) * ss, 0.0),
    }
}

pub(super) fn card_bay(ctx: &mut Ctx, n: &XNode) {
    ctx.count("Card_Bay");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let Some(size) = size_of(n) else { return };
    let pose = box_pose(ctx, n, "card bay");
    bay_sides(ctx, n, &name, &pose, size, "Card_Bay_Side");
    // backplane behind the socket face (z < 0)
    if let Some(p) = at(n, "PCBLink/PCB").filter(|p| installed(p)) {
        let t = mm(p, "Geometry/Thickness").unwrap_or(1.6);
        let w = mm(p, "Geometry/Width").unwrap_or(size[0]);
        let d = mm(p, "Geometry/Depth").unwrap_or(size[1]);
        let o1 = mm(p, "Placement/Offset_1").unwrap_or(0.0);
        let o2 = mm(p, "Placement/Offset_2").unwrap_or(0.0);
        let bp = pose.translated([o1, o2, -t]);
        let normal = ft_axis(pose.axis(2).0, 1.0).0;
        let stack = stack_of(ctx, at(p, "Construction"), t, normal);
        ctx.block_pcb_in(
            &bp,
            &format!("{name}/{}", name_of(p)),
            stack,
            [0.0; 3],
            [w, d, t],
            board_power(p),
        );
        ctx.count("PCB");
    }
    let Some(sd) = at(n, "Construction/Slot_Definition") else {
        return;
    };
    let ss = mm(sd, "Slot_Size").unwrap_or(size[1]);
    let first = mm(sd, "First_Slot_Offset").unwrap_or(0.0);
    let dir = txt(sd, "Slot_Direction").unwrap_or("Top to Bottom");
    if !dir.to_ascii_lowercase().contains("top") {
        ctx.warnings.push(format!(
            "card bay {name}: slots stacked '{dir}' (side by side) not supported yet, cards skipped"
        ));
        return;
    }
    let Some(slots) = sd.child("SlotsLink") else {
        return;
    };
    for slot in slots.children("Card_Slot") {
        let idx = txt(slot, "Index").and_then(val).unwrap_or(1.0).max(1.0) as usize;
        let Some(s) = at(slot, "DBSLink/Daughter_Board_Socket") else {
            continue;
        };
        ctx.count("Daughter_Board_Socket");
        if !installed(s) {
            ctx.uninstalled += 1;
            continue;
        }
        let (_, y0) = slot_origin(dir, idx, ss, first, size);
        let l = mm(s, "Geometry/Length").unwrap_or(100.0);
        let h = mm(s, "Geometry/Height").unwrap_or(10.0);
        let w = mm(s, "Geometry/Width")
            .or_else(|| mm(s, "Geometry/Socket_Depth"))
            .unwrap_or(10.0);
        let o1 = mm(s, "Placement/Offset_1").unwrap_or(0.0);
        let o2 = mm(s, "Placement/Offset_2").unwrap_or(0.0);
        // socket on the backplane face (z = 0), its length along x
        let (slo, shi) = ([o1, y0 + o2, 0.0], [o1 + l, y0 + o2 + h, w]);
        let sname = name_of(s);
        let mat = ctx.mat(txt(s, "Construction/MaterialLink"), "fr4");
        ctx.block_in(&pose, &format!("{name}/{sname}"), mat, slo, shi, 0.0);
        let Some(p) = at(s, "Daughter_Board/PCB").filter(|p| installed(p)) else {
            continue;
        };
        ctx.count("PCB");
        let tb = mm(p, "Geometry/Thickness").unwrap_or(1.6);
        let wb = mm(p, "Geometry/Width").unwrap_or(l);
        let db = mm(p, "Geometry/Depth").unwrap_or(size[2]);
        let co = mm(p, "Placement/Centre_Offset").unwrap_or(0.0);
        // the card: horizontal, from the socket face into the bay, centred
        // on the socket thickness, its centre Centre_Offset from the socket's
        let card = pose.translated([
            0.5 * (slo[0] + shi[0]) + co - 0.5 * wb,
            0.5 * (slo[1] + shi[1]) - 0.5 * tb,
            0.0,
        ]);
        // card frame: x width (bay x), y normal (bay y), z depth (bay z)
        let normal = ft_axis(pose.axis(1).0, 1.0).0;
        let stack = stack_of(ctx, at(p, "Construction"), tb, normal);
        let pname = format!("{name}/{sname}/{}", name_of(p));
        ctx.block_pcb_in(&card, &pname, stack, [0.0; 3], [wb, tb, db], board_power(p));
        board_contents(ctx, p, &BoardRef { pose: card, t: tb });
    }
}

pub(super) fn drive_bay(ctx: &mut Ctx, n: &XNode) {
    ctx.count("Drive_Bay");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let Some(size) = size_of(n) else { return };
    let pose = box_pose(ctx, n, "drive bay");
    bay_sides(ctx, n, &name, &pose, size, "Drive_Bay_Side");
    let Some(d) = at(n, "DriveLink/Drive") else {
        return;
    };
    ctx.count("Drive");
    if !installed(d) {
        ctx.uninstalled += 1;
        return;
    }
    let Some(ds) = size_of(d) else { return };
    let m = at(d, "Mount_Location");
    let pos = |k: &str, def: &str| {
        m.and_then(|m| txt(m, k))
            .unwrap_or(def)
            .to_ascii_lowercase()
    };
    let moff = |k: &str| m.and_then(|m| mm(m, k)).unwrap_or(0.0);
    let x0 = match pos("Sideways_Mounting_Position", "Centre").as_str() {
        "left" => 0.0,
        "right" => size[0] - ds[0],
        _ => 0.5 * (size[0] - ds[0]),
    } + moff("Sideways_Offset");
    let y0 = match pos("Vertical_Mounting_Position", "Centre").as_str() {
        "bottom" => 0.0,
        "top" => size[1] - ds[1],
        _ => 0.5 * (size[1] - ds[1]),
    } + moff("Vertical_Offset");
    let z0 = moff("Depth_Offset");
    let mat = ctx.mat(txt(d, "Construction/Drive_Material"), "al-6061-t6");
    let dname = format!("{name}/{}", name_of(d));
    ctx.block_in(
        &pose,
        &dname,
        mat.clone(),
        [x0, y0, z0],
        [x0 + ds[0], y0 + ds[1], z0 + ds[2]],
        power_of(d),
    );
    if yes(d, "Construction/Mounting_Rails") {
        let rh = mm(d, "Construction/Mounting_Rail_Height")
            .unwrap_or(ds[1])
            .min(ds[1]);
        let yc =
            y0 + 0.5 * ds[1] + mm(d, "Construction/Mounting_Rail_Vertical_Offset").unwrap_or(0.0);
        for (k, (a, b)) in [(0.0, x0), (x0 + ds[0], size[0])].into_iter().enumerate() {
            if b - a > 1e-6 {
                ctx.block_in(
                    &pose,
                    &format!("{dname}/rail{}", k + 1),
                    mat.clone(),
                    [a, yc - rh / 2.0, z0],
                    [b, yc + rh / 2.0, z0 + ds[2]],
                    0.0,
                );
            }
        }
    }
}

// ------------------------------------------------------------------ board parts

/// Chip socket on a board, with the component it holds centred on it.
pub(super) fn chip_socket(ctx: &mut Ctx, n: &XNode, b: &BoardRef) {
    ctx.count("Chip_Socket");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let (Some(w), Some(d), Some(h)) = (
        mm(n, "Geometry/Width"),
        mm(n, "Geometry/Depth"),
        mm(n, "Geometry/Height"),
    ) else {
        return;
    };
    if txt(n, "Placement/Board_Side").is_some_and(|s| s.eq_ignore_ascii_case("Bottom")) {
        ctx.warnings.push(format!(
            "chip socket {name}: bottom side not supported yet, skipped"
        ));
        return;
    }
    if txt(n, "Placement/Orientation")
        .and_then(deg)
        .is_some_and(|a| a.abs() > 1e-9)
    {
        ctx.warnings
            .push(format!("chip socket {name}: rotation on the board ignored"));
    }
    let x = mm(n, "Placement/X_Location").unwrap_or(0.0);
    let z = mm(n, "Placement/Z_Location").unwrap_or(0.0);
    let mat = ctx.mat(txt(n, "Construction/MaterialLink"), "fr4");
    ctx.block_in(
        &b.pose,
        &name,
        mat,
        [x, b.t, z],
        [x + w, b.t + h, z + d],
        0.0,
    );
    if let Some(c) = at(n, "ComponentLink/Component") {
        let cw = mm(c, "Geometry/Width").unwrap_or(w);
        let cd = mm(c, "Geometry/Depth").unwrap_or(d);
        let s = BoardRef {
            pose: b
                .pose
                .translated([x + 0.5 * (w - cw), 0.0, z + 0.5 * (d - cd)]),
            t: b.t + h,
        };
        component(ctx, c, Some(&s));
    }
}

/// Daughter-board (DIMM) socket on a board: the card stands on the board
/// along the socket length, centred on the socket width.
pub(super) fn board_socket(ctx: &mut Ctx, n: &XNode, b: &BoardRef) {
    ctx.count("Daughter_Board_Socket");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let l = mm(n, "Geometry/Length").unwrap_or(100.0);
    let w = mm(n, "Geometry/Width").unwrap_or(7.0);
    let h = mm(n, "Geometry/Height").unwrap_or(7.0);
    if txt(n, "Placement/Board_Side").is_some_and(|s| s.eq_ignore_ascii_case("Bottom")) {
        ctx.warnings.push(format!(
            "daughter board socket {name}: bottom side not supported yet, skipped"
        ));
        return;
    }
    let rot = txt(n, "Placement/Orientation").and_then(deg).unwrap_or(0.0);
    let tilt = txt(n, "Geometry/Angle").and_then(deg).unwrap_or(0.0);
    if rot.abs() > 1e-9 || tilt.abs() > 1e-9 {
        ctx.warnings.push(format!(
            "daughter board socket {name}: rotation {rot}° / angle {tilt}° not supported yet, imported unrotated"
        ));
    }
    let x = mm(n, "Placement/X_Location").unwrap_or(0.0);
    let z = mm(n, "Placement/Z_Location").unwrap_or(0.0);
    let mat = ctx.mat(txt(n, "Construction/MaterialLink"), "fr4");
    ctx.block_in(
        &b.pose,
        &name,
        mat,
        [x, b.t, z],
        [x + w, b.t + h, z + l],
        0.0,
    );
    let Some(p) = at(n, "Daughter_Board/PCB").filter(|p| installed(p)) else {
        return;
    };
    ctx.count("PCB");
    let tb = mm(p, "Geometry/Thickness").unwrap_or(1.6);
    let wb = mm(p, "Geometry/Width").unwrap_or(l);
    let db = mm(p, "Geometry/Depth").unwrap_or(30.0);
    let co = mm(p, "Placement/Centre_Offset").unwrap_or(0.0);
    let (xc, zc) = (x + 0.5 * w, z + 0.5 * l + co);
    // card frame: x (width) → board z, y (normal) → board x, z (height) → board y
    let card = b.pose.then(&Pose {
        r: [[0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]],
        t: [xc - 0.5 * tb, b.t, zc - 0.5 * wb],
    });
    let normal = ft_axis(card.axis(1).0, 1.0).0;
    let stack = stack_of(ctx, at(p, "Construction"), tb, normal);
    let pname = format!("{name}/{}", name_of(p));
    ctx.block_pcb_in(&card, &pname, stack, [0.0; 3], [wb, tb, db], board_power(p));
    board_contents(ctx, p, &BoardRef { pose: card, t: tb });
}

/// Heat sink and TIMs attached to a component (`lo`/`hi`: its box in the
/// board frame `pose`).
#[allow(clippy::too_many_arguments)]
pub(super) fn component_extras(
    ctx: &mut Ctx,
    n: &XNode,
    pose: &Pose,
    lo: V3,
    hi: V3,
    bottom: bool,
    power: f64,
) {
    let name = name_of(n);
    if let Some(l) = n.child("TIMs") {
        for t in l.children("TIM").filter(|t| installed(t)) {
            if yes(t, "Construction/Model_Thickness") {
                ctx.warnings.push(format!(
                    "component {name}: TIM with modelled thickness not supported yet, ignored"
                ));
            } else {
                let r = txt(t, "Construction/Solid-Solid_Resistance").and_then(val);
                ctx.warnings.push(match r {
                    Some(r) => format!(
                        "component {name}: TIM {} of {r} °C/W not modelled (about {:.2} K at {power} W)",
                        name_of(t),
                        r * power
                    ),
                    None => format!("component {name}: TIM {} not modelled", name_of(t)),
                });
            }
        }
    }
    let Some(hs) = at(n, "HeatsinkLink/Heatsink") else {
        return;
    };
    if !installed(hs) {
        ctx.uninstalled += 1;
        return;
    }
    if bottom {
        ctx.warnings.push(format!(
            "component {name}: heat sink on a bottom-side part not supported yet, skipped"
        ));
        return;
    }
    let bw = mm(hs, "Geometry/Base_Width").unwrap_or(hi[0] - lo[0]);
    let bd = mm(hs, "Geometry/Base_Depth").unwrap_or(hi[2] - lo[2]);
    let centred = txt(hs, "Placement/Origin_Point").is_none_or(|o| o.contains("Centre"));
    let corner = if centred {
        [
            0.5 * (lo[0] + hi[0]) - 0.5 * bw,
            0.0,
            0.5 * (lo[2] + hi[2]) - 0.5 * bd,
        ]
    } else {
        [lo[0], 0.0, lo[2]]
    };
    let r = BoardRef {
        pose: pose.translated(corner),
        t: hi[1],
    };
    heatsink(ctx, hs, Some(&r));
}
