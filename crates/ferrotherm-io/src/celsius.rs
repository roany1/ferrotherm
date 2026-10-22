// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Import of Cadence Celsius EC Solver models (`.equipment`).
//!
//! An `.equipment` file is a zip archive; the member with the same
//! extension holds the model: a header (application name and version as
//! length-prefixed UTF-16BE strings) followed by zlib-compressed blocks
//! (`[u32 compressed length][u32 uncompressed length][zlib data]`, the
//! first one preceded by four zero bytes) whose concatenation is an XML
//! document with values and units as text (`<Width>233.35 mm</Width>`).
//!
//! Celsius works with Y up (gravity −Y), X across and Z in depth, all
//! measured from the low corner of the top-level chassis. FerroTherm uses
//! z up; the import maps (X, Y, Z) → (x = X, y = D − Z, z = Y), D being the
//! chassis depth (a proper rotation: the model is not mirrored).
//!
//! Mapped objects: chassis (domain, walls), environment (ambient, external
//! convection and radiation), fluid, solver settings (flow, gravity,
//! turbulence, iterations), materials, PCBs (layered stack), components
//! (2R compact models or solid blocks with power), extruded heat sinks, gap
//! pads, solid obstructions (cuboids and CAD solids, voxelised) and groups.
//! Every object type that is present but not mapped is listed in the
//! warnings.
//!
//! Format knowledge comes from reading saved models (versions 17.44 and
//! 2024.36); it is not an official specification.

use crate::step::{StepBody, V3};
use crate::xml::{self, XNode};
use ferrotherm_core::case::{
    Block, Boundary, BoundaryKind, Case, Component, Face, FlowSettings, Turbulence,
};
use ferrotherm_core::library::{CaseFace, Conductivity, Material};
use ferrotherm_core::pcb::{CopperLayer, PcbStack};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;

const OZ_MM: f64 = 0.035;

// ------------------------------------------------------------------ container

/// Model XML from an `.equipment` file (zip), its inner model file, or XML.
/// Returns (application, version, xml).
pub fn extract(data: &[u8]) -> Result<(String, String, String), String> {
    if data.starts_with(b"PK") {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| e.to_string())?;
        let mut best: Option<(usize, Vec<u8>)> = None;
        for i in 0..z.len() {
            let mut f = z.by_index(i).map_err(|e| e.to_string())?;
            let name = f.name().to_ascii_lowercase();
            let is_model = name.ends_with(".equipment")
                || name.ends_with(".project")
                || name.ends_with(".rack");
            if !is_model || f.is_dir() {
                continue;
            }
            let mut v = Vec::new();
            f.read_to_end(&mut v).map_err(|e| e.to_string())?;
            if best.as_ref().is_none_or(|b| v.len() > b.0) {
                best = Some((v.len(), v));
            }
        }
        let (_, inner) = best.ok_or("the archive holds no Celsius model (.equipment) file")?;
        return decode(&inner);
    }
    let head = String::from_utf8_lossy(&data[..data.len().min(256)]).to_string();
    if head.contains("<DataDefinition") || head.contains("<Celsius_EC_Solver_Project") {
        return Ok((
            "ET Constructor".into(),
            String::new(),
            String::from_utf8_lossy(data).to_string(),
        ));
    }
    decode(data)
}

fn be32(d: &[u8], o: usize) -> Option<usize> {
    d.get(o..o + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

fn utf16be(d: &[u8]) -> String {
    let u: Vec<u16> = d
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u)
}

/// Decode the header + zlib blocks of the inner model file.
pub fn decode(d: &[u8]) -> Result<(String, String, String), String> {
    let bad = || "not a Celsius EC Solver model file".to_string();
    let mut o = 0;
    let n = be32(d, o).ok_or_else(bad)?;
    if n == 0 || n > 512 || o + 4 + n > d.len() {
        return Err(bad());
    }
    let app = utf16be(&d[o + 4..o + 4 + n]);
    o += 4 + n;
    let m = be32(d, o).ok_or_else(bad)?;
    if m > 512 || o + 4 + m > d.len() {
        return Err(bad());
    }
    let ver = utf16be(&d[o + 4..o + 4 + m]);
    o += 4 + m;
    let mut xml = Vec::new();
    let mut first = true;
    while o < d.len() {
        // blocks: [u32 0 (first only)] [u32 compressed] [u32 uncompressed] [zlib]
        if first && be32(d, o) == Some(0) {
            o += 4;
        }
        first = false;
        // the length fields are not always consistent: the zlib stream end
        // decides where the next block starts
        let _cl = be32(d, o).ok_or("truncated Celsius model")?;
        let ul = be32(d, o + 4).ok_or("truncated Celsius model")?;
        o += 8;
        let mut z = flate2::read::ZlibDecoder::new(&d[o..]);
        let mut out = Vec::with_capacity(ul.min(64 << 20));
        z.read_to_end(&mut out)
            .map_err(|e| format!("Celsius model block at byte {o}: {e}"))?;
        if out.len() != ul {
            return Err(format!(
                "Celsius model block at byte {o}: expected {ul} bytes, got {}",
                out.len()
            ));
        }
        xml.extend(out);
        if xml.len() > 1 << 31 {
            return Err("Celsius model too large".into());
        }
        o += z.total_in() as usize;
    }
    Ok((app, ver, String::from_utf8_lossy(&xml).to_string()))
}

// ------------------------------------------------------------------ values

/// Number and unit of a value like `233.35 mm`.
fn split(s: &str) -> Option<(f64, &str)> {
    let s = s.trim();
    let end = s
        .char_indices()
        .find(|(i, c)| {
            !(c.is_ascii_digit()
                || *c == '.'
                || ((*c == '-' || *c == '+')
                    && (*i == 0 || matches!(s.as_bytes()[i - 1], b'e' | b'E')))
                || ((*c == 'e' || *c == 'E') && *i > 0))
        })
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let v: f64 = s[..end].parse().ok()?;
    Some((v, s[end..].trim()))
}

fn len_mm(s: &str) -> Option<f64> {
    let (v, u) = split(s)?;
    Some(
        v * match u {
            "" | "mm" => 1.0,
            "m" => 1000.0,
            "cm" => 10.0,
            "um" | "µm" | "micron" => 1e-3,
            "in" | "inch" | "\"" => 25.4,
            "ft" => 304.8,
            "mil" | "thou" => 0.0254,
            _ => return None,
        },
    )
}

fn power_w(s: &str) -> Option<f64> {
    let (v, u) = split(s)?;
    Some(
        v * match u {
            "" | "W" => 1.0,
            "mW" => 1e-3,
            "kW" => 1e3,
            "BTU/h" | "BTU/hr" => 0.293_071,
            _ => return None,
        },
    )
}

fn temp_c(s: &str) -> Option<f64> {
    let (v, u) = split(s)?;
    match u {
        "" | "C" | "°C" | "degC" => Some(v),
        "K" => Some(v - 273.15),
        "F" | "°F" => Some((v - 32.0) / 1.8),
        _ => None,
    }
}

/// Plain number (unit ignored): conductivities, resistances, densities…
/// are stored in SI-like units in every file seen.
fn val(s: &str) -> Option<f64> {
    split(s).map(|x| x.0)
}

fn pct(s: &str) -> Option<f64> {
    split(s).map(|(v, u)| if u == "%" { v / 100.0 } else { v })
}

fn deg(s: &str) -> Option<f64> {
    let (v, u) = split(s)?;
    Some(if u.starts_with("rad") {
        v.to_degrees()
    } else {
        v
    })
}

/// Celsius curve "x1,y1, x2,y2 <unitx,unity>" → points.
fn curve(s: &str) -> Vec<[f64; 2]> {
    let body = s.split('<').next().unwrap_or("");
    let nums: Vec<f64> = body
        .split(',')
        .filter_map(|t| t.trim().parse().ok())
        .collect();
    nums.chunks_exact(2).map(|c| [c[0], c[1]]).collect()
}

// ------------------------------------------------------------------ DOM helpers

fn at<'a>(n: &'a XNode, path: &str) -> Option<&'a XNode> {
    let mut cur = n;
    for p in path.split('/') {
        cur = cur.child(p)?;
    }
    Some(cur)
}

fn txt<'a>(n: &'a XNode, path: &str) -> Option<&'a str> {
    at(n, path).map(|x| x.text.trim()).filter(|t| !t.is_empty())
}

fn yes(n: &XNode, path: &str) -> bool {
    txt(n, path).is_some_and(|t| t.eq_ignore_ascii_case("yes"))
}

fn installed(n: &XNode) -> bool {
    // objects without the flag are installed
    txt(n, "Installed").is_none_or(|t| t.eq_ignore_ascii_case("yes"))
}

fn name_of(n: &XNode) -> String {
    txt(n, "Identification/Reference_Designator")
        .or_else(|| txt(n, "Identification/Name"))
        .or_else(|| txt(n, "Name"))
        .unwrap_or("unnamed")
        .to_string()
}

fn mm(n: &XNode, path: &str) -> Option<f64> {
    txt(n, path).and_then(len_mm)
}

// ------------------------------------------------------------------ model

/// A CAD solid of the model, kept as triangles until the user chooses how
/// to turn it into blocks.
#[derive(Debug, Clone)]
pub struct CadBody {
    pub body: StepBody,
    pub material: String,
    pub power_w: f64,
}

#[derive(Debug, Clone)]
pub struct CelsiusModel {
    pub version: String,
    /// Complete case (without the CAD solids).
    pub case: Case,
    pub cad: Vec<CadBody>,
    pub warnings: Vec<String>,
    /// (object type, count) of everything read.
    pub stats: BTreeMap<String, usize>,
}

/// Celsius frame → FerroTherm frame.
#[derive(Clone, Copy)]
struct Frame {
    depth: f64,
}

impl Frame {
    fn p(&self, c: V3) -> V3 {
        [c[0], self.depth - c[2], c[1]]
    }
    /// Axis-aligned Celsius box → FerroTherm box.
    fn bx(&self, lo: V3, hi: V3) -> (V3, V3) {
        let a = self.p(lo);
        let b = self.p(hi);
        (
            [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
            [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
        )
    }
}

struct Ctx {
    f: Frame,
    mats: HashMap<String, String>,
    case: Case,
    cad: Vec<CadBody>,
    warnings: Vec<String>,
    stats: BTreeMap<String, usize>,
    skipped: BTreeMap<String, usize>,
    uninstalled: usize,
    names: HashMap<String, usize>,
}

impl Ctx {
    fn count(&mut self, k: &str) {
        *self.stats.entry(k.to_string()).or_default() += 1;
    }
    fn skip(&mut self, k: &str) {
        *self.skipped.entry(k.to_string()).or_default() += 1;
    }
    fn unique(&mut self, base: &str) -> String {
        let n = self.names.entry(base.to_string()).or_default();
        *n += 1;
        if *n == 1 {
            base.to_string()
        } else {
            format!("{base}~{n}")
        }
    }
    fn mat(&self, link: Option<&str>, fallback: &str) -> String {
        link.and_then(|l| self.mats.get(l.trim()).cloned())
            .unwrap_or_else(|| fallback.to_string())
    }
    fn block(&mut self, name: &str, mat: String, lo: V3, hi: V3, power: f64) {
        let (a, b) = self.f.bx(lo, hi);
        if (0..3).any(|k| b[k] - a[k] <= 1e-9) {
            return;
        }
        let name = self.unique(name);
        self.case.blocks.push(Block {
            name,
            material: Some(mat),
            pcb: None,
            min: a,
            max: b,
            power_w: power,
            emissivity: None,
            finish: None,
        });
    }
}

fn slug(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            o.push(c.to_ascii_lowercase());
        } else if !o.ends_with('-') {
            o.push('-');
        }
    }
    o.trim_matches('-').chars().take(40).collect()
}

fn material(n: &XNode, id: String) -> Option<Material> {
    let name = txt(n, "Name")
        .or_else(|| txt(n, "Identification/Name"))
        .unwrap_or("material")
        .to_string();
    let k = at(n, "Conductivity")?;
    let typ = txt(k, "Type").unwrap_or("Isotropic");
    let iso = txt(k, "Value").and_then(val);
    let cond = if typ.eq_ignore_ascii_case("Isotropic") {
        Conductivity::Isotropic(iso?)
    } else if let (Some(k1), Some(k2), Some(k3)) = (
        txt(k, "K1").and_then(val),
        txt(k, "K2").and_then(val),
        txt(k, "K3").and_then(val),
    ) {
        // Celsius (X, Y up, Z) → FerroTherm (x, y = −Z, z = Y)
        Conductivity::Orthotropic([k1, k3, k2])
    } else if let (Some(nk), Some(pk)) = (
        txt(k, "Normal").and_then(val),
        txt(k, "Planar").and_then(val),
    ) {
        Conductivity::Orthotropic([pk, pk, nk])
    } else {
        Conductivity::Isotropic(iso?)
    };
    let k_table = (txt(k, "Option").is_some_and(|o| !o.eq_ignore_ascii_case("Constant")))
        .then(|| txt(k, "Conductivity_Temperature_Curve").map(curve))
        .flatten()
        .filter(|c| c.len() >= 2 && matches!(cond, Conductivity::Isotropic(_)));
    Some(Material {
        id,
        name: format!("{name} (Celsius)"),
        category: "imported".into(),
        k: cond,
        density: txt(n, "Density/Value")
            .and_then(val)
            .filter(|v| *v > 0.0)
            .unwrap_or(1000.0),
        cp: txt(n, "Specific_Heat/Value")
            .and_then(val)
            .filter(|v| *v > 0.0)
            .unwrap_or(1000.0),
        viscosity: None,
        beta: None,
        k_table,
        specs: vec![],
        source: "Imported from a Cadence Celsius EC Solver model".into(),
        notes: String::new(),
    })
}

fn fluid(n: &XNode) -> Option<Material> {
    let k = txt(n, "Conductivity/Value").and_then(val)?;
    Some(Material {
        id: format!("ecs-{}", slug(txt(n, "Name").unwrap_or("fluid"))),
        name: format!("{} (Celsius fluid)", txt(n, "Name").unwrap_or("fluid")),
        category: "fluid".into(),
        k: Conductivity::Isotropic(k),
        density: txt(n, "Density/Value").and_then(val)?,
        cp: txt(n, "Specific_Heat/Value").and_then(val)?,
        viscosity: txt(n, "Laminar_Viscosity/Value").and_then(val),
        beta: txt(n, "Expansivity/Value").and_then(val),
        k_table: None,
        specs: vec![],
        source: "Imported from a Cadence Celsius EC Solver model".into(),
        notes: String::new(),
    })
}

/// Read every `<Material id=…>` anywhere in the model.
fn materials(root: &XNode, ctx: &mut Ctx) {
    let mut all = Vec::new();
    root.descendants("Material", &mut all);
    let mut ids: HashMap<String, usize> = HashMap::new();
    for m in all {
        let Some(id) = m.attr("id") else { continue };
        if ctx.mats.contains_key(id) {
            continue;
        }
        let base = format!("ecs-{}", slug(txt(m, "Name").unwrap_or("material")));
        let n = ids.entry(base.clone()).or_default();
        *n += 1;
        let mid = if *n == 1 { base } else { format!("{base}-{n}") };
        match material(m, mid.clone()) {
            Some(mat) => {
                ctx.mats.insert(id.to_string(), mid);
                ctx.case.materials.push(mat);
                ctx.count("Material");
            }
            None => ctx.warnings.push(format!(
                "material '{}' has no usable conductivity: objects using it get 'al-6061-t6'",
                txt(m, "Name").unwrap_or(id)
            )),
        }
    }
}

// ------------------------------------------------------------------ objects

/// Board in Celsius coordinates: low corner and thickness.
struct BoardRef {
    lo: V3,
    t: f64,
}

/// Footprint rotation from the placement (rotation about the board normal).
fn footprint_angle(n: &XNode, warnings: &mut Vec<String>, what: &str) -> f64 {
    let mut a = txt(n, "Placement/Angle_To_Board")
        .and_then(deg)
        .unwrap_or(0.0);
    if let Some(o) = at(n, "Placement/Orientation").or_else(|| at(n, "Placement")) {
        if txt(o, "Rotation_Mechanism") == Some("Angle and Axis") {
            let axis = [
                txt(o, "X_Axis").and_then(val).unwrap_or(0.0),
                txt(o, "Y_Axis").and_then(val).unwrap_or(1.0),
                txt(o, "Z_Axis").and_then(val).unwrap_or(0.0),
            ];
            let ang = txt(o, "Angle").and_then(deg).unwrap_or(0.0);
            if ang.abs() > 1e-9 {
                if axis[1].abs() > 0.999 {
                    a += ang * axis[1].signum();
                } else {
                    warnings.push(format!("{what}: rotation about a tilted axis ignored"));
                }
            }
        } else {
            let tilt: f64 = ["Angle_1", "Angle_2", "Angle_3"]
                .iter()
                .filter_map(|k| txt(o, k).and_then(deg))
                .map(f64::abs)
                .sum();
            if tilt > 1e-9 {
                warnings.push(format!("{what}: ordered rotation angles ignored"));
            }
        }
    }
    a
}

/// Footprint extents after a rotation about the normal (bounding box).
fn rotated(w: f64, d: f64, angle_deg: f64) -> (f64, f64) {
    let r = angle_deg.to_radians();
    let (s, c) = (r.sin().abs(), r.cos().abs());
    (w * c + d * s, w * s + d * c)
}

fn power_of(n: &XNode) -> f64 {
    let p = at(n, "Power");
    let base = p
        .and_then(|p| txt(p, "Thermal_Design_Power").or_else(|| txt(p, "Total_Power")))
        .and_then(power_w)
        .or_else(|| txt(n, "Cooling/Heat_Dissipated").and_then(power_w))
        .unwrap_or(0.0);
    let util = p
        .and_then(|p| txt(p, "Power_Utilisation/Utilisation"))
        .or_else(|| txt(n, "Cooling/Utilisation"))
        .and_then(pct)
        .unwrap_or(1.0);
    base * util
}

fn component(ctx: &mut Ctx, n: &XNode, board: Option<&BoardRef>) {
    ctx.count("Component");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let w = mm(n, "Geometry/Width").unwrap_or(1.0);
    let d = mm(n, "Geometry/Depth").unwrap_or(1.0);
    let h = mm(n, "Geometry/Height").unwrap_or(1.0);
    let (w, d) = match txt(n, "Geometry/Shape") {
        Some(s) if s.eq_ignore_ascii_case("Cylinder") || s.eq_ignore_ascii_case("Circle") => {
            let dia = mm(n, "Geometry/Diameter").unwrap_or(w);
            (dia, dia)
        }
        _ => (w, d),
    };
    let ang = footprint_angle(n, &mut ctx.warnings, &format!("component {name}"));
    let (w, d) = rotated(w, d, ang);
    let x = mm(n, "Placement/X_Location").unwrap_or(0.0);
    let y = mm(n, "Placement/Y_Location").unwrap_or(0.0);
    let z = mm(n, "Placement/Z_Location").unwrap_or(0.0);
    let centre = txt(n, "Placement/Origin_Point").is_none_or(|o| o.contains("Centre"));
    let (x0, z0) = if centre {
        (x - 0.5 * w, z - 0.5 * d)
    } else {
        (x, z)
    };
    let bottom = txt(n, "Placement/Board_Side").is_some_and(|s| s.eq_ignore_ascii_case("Bottom"));
    let (lo, hi) = match board {
        Some(b) => {
            let (ylo, yhi) = if bottom {
                (b.lo[1] - y - h, b.lo[1] - y)
            } else {
                (b.lo[1] + b.t + y, b.lo[1] + b.t + y + h)
            };
            (
                [b.lo[0] + x0, ylo, b.lo[2] + z0],
                [b.lo[0] + x0 + w, yhi, b.lo[2] + z0 + d],
            )
        }
        None => ([x0, y, z0], [x0 + w, y + h, z0 + d]),
    };
    let power = power_of(n);
    let level = txt(n, "Construction/Modelling_Level").unwrap_or("");
    let rjc = txt(n, "Construction/Junction-to-Case_Thermal_Resistance").and_then(val);
    let rjb = txt(n, "Construction/Junction-to-Board_Thermal_Resistance").and_then(val);
    if level.eq_ignore_ascii_case("2R") && (rjc.is_some() || rjb.is_some()) {
        let (a, b) = ctx.f.bx(lo, hi);
        let name = ctx.unique(&name);
        ctx.case.components.push(Component {
            name,
            package: "generic".into(),
            position: a,
            size: Some([b[0] - a[0], b[1] - a[1], b[2] - a[2]]),
            power_w: power,
            r_jc: rjc,
            r_jb: rjb,
            // the case is the face away from the board
            case_face: Some(if bottom {
                CaseFace::Bottom
            } else {
                CaseFace::Top
            }),
            mount_axis: "z".into(),
            derating: None,
            tj_max_c: None,
            network: None,
        });
    } else {
        if !level.is_empty()
            && !level.eq_ignore_ascii_case("Simplified")
            && !level.eq_ignore_ascii_case("Detailed")
        {
            ctx.warnings.push(format!(
                "component {name}: modelling level '{level}' imported as a solid block"
            ));
        }
        let mat = ctx.mat(txt(n, "Construction/MaterialLink"), "mold-compound");
        ctx.block(&name, mat, lo, hi, power);
    }
}

fn heatsink(ctx: &mut Ctx, n: &XNode, board: Option<&BoardRef>) {
    ctx.count("Heatsink");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let g = match at(n, "Geometry") {
        Some(g) => g,
        None => return,
    };
    let typ = txt(g, "Type").unwrap_or("");
    let bw = mm(g, "Base_Width").unwrap_or(10.0);
    let bd = mm(g, "Base_Depth").unwrap_or(10.0);
    let bt = mm(g, "Base_Thickness").unwrap_or(1.0);
    let fh = mm(g, "Fin_Height").unwrap_or(0.0);
    let ft = mm(g, "Fin_Thickness").unwrap_or(1.0);
    let nf = txt(g, "Number_of_Fins")
        .and_then(val)
        .unwrap_or(0.0)
        .max(0.0) as usize;
    let fd = mm(g, "Fin_Depth").unwrap_or(bd).min(bd);
    if !typ.eq_ignore_ascii_case("Extruded") {
        ctx.warnings.push(format!(
            "heat sink {name}: type '{typ}' imported as an extruded (plate-fin) heat sink"
        ));
    }
    let mat = ctx.mat(
        txt(n, "Construction/Base_Material").or_else(|| txt(n, "Construction/MaterialLink")),
        "al-6061-t6",
    );
    let fin_mat = ctx.mat(txt(n, "Construction/Fin_Material"), &mat);
    let bottom = txt(n, "Placement/Board_Side").is_some_and(|s| s.eq_ignore_ascii_case("Bottom"));
    // base low corner (board-local offsets, height above the board surface)
    let (x0, yoff, z0) = (
        mm(n, "Placement/X_Offset").unwrap_or(0.0),
        mm(n, "Placement/Y_Offset").unwrap_or(0.0),
        mm(n, "Placement/Z_Offset").unwrap_or(0.0),
    );
    let (bx, by, bz, dir) = match board {
        Some(b) if bottom => (b.lo[0] + x0, b.lo[1] - yoff, b.lo[2] + z0, -1.0),
        Some(b) => (b.lo[0] + x0, b.lo[1] + b.t + yoff, b.lo[2] + z0, 1.0),
        None => (
            mm(n, "Placement/X_Location").unwrap_or(0.0),
            mm(n, "Placement/Y_Location").unwrap_or(0.0),
            mm(n, "Placement/Z_Location").unwrap_or(0.0),
            1.0,
        ),
    };
    let span = |a: f64, len: f64| {
        if dir > 0.0 {
            (a, a + len)
        } else {
            (a - len, a)
        }
    };
    let (y0, y1) = span(by, bt);
    ctx.block(
        &format!("{name}/base"),
        mat,
        [bx, y0, bz],
        [bx + bw, y1, bz + bd],
        0.0,
    );
    // fins: across the width, running in depth
    if nf > 0 && fh > 0.0 {
        let pitch = if nf > 1 {
            (bw - ft) / (nf - 1) as f64
        } else {
            0.0
        };
        let (f0, f1) = span(if dir > 0.0 { y1 } else { y0 }, fh);
        let zf = bz + 0.5 * (bd - fd);
        for i in 0..nf {
            let xf = if nf > 1 {
                bx + i as f64 * pitch
            } else {
                bx + 0.5 * (bw - ft)
            };
            ctx.block(
                &format!("{name}/fin{}", i + 1),
                fin_mat.clone(),
                [xf, f0, zf],
                [xf + ft, f1, zf + fd],
                0.0,
            );
        }
    }
}

fn gap_pad(ctx: &mut Ctx, n: &XNode, board: Option<&BoardRef>) {
    ctx.count("Gap_Pad");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let w = mm(n, "Geometry/Width").unwrap_or(1.0);
    let h = mm(n, "Geometry/Height").unwrap_or(1.0);
    let d = mm(n, "Geometry/Depth").unwrap_or(1.0);
    let p = [
        mm(n, "Placement/X_Location").unwrap_or(0.0),
        mm(n, "Placement/Y_Location").unwrap_or(0.0),
        mm(n, "Placement/Z_Location").unwrap_or(0.0),
    ];
    let lo = match board {
        Some(b) => [b.lo[0] + p[0], b.lo[1] + p[1], b.lo[2] + p[2]],
        None => p,
    };
    let mat = ctx.mat(
        txt(n, "Cooling/MaterialLink").or_else(|| txt(n, "Construction/MaterialLink")),
        "tim-gap-pad-3",
    );
    ctx.block(&name, mat, lo, [lo[0] + w, lo[1] + h, lo[2] + d], 0.0);
}

fn pcb(ctx: &mut Ctx, n: &XNode) {
    ctx.count("PCB");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let w = mm(n, "Geometry/Width").unwrap_or(100.0);
    let d = mm(n, "Geometry/Depth").unwrap_or(100.0);
    let t = mm(n, "Geometry/Thickness").unwrap_or(1.6);
    if let Some(s) = txt(n, "Geometry/Shape") {
        if !s.eq_ignore_ascii_case("Rectangle") {
            ctx.warnings.push(format!(
                "PCB {name}: shape '{s}' imported as its bounding rectangle"
            ));
        }
    }
    let mut y = mm(n, "Placement/Y_Location").unwrap_or(0.0);
    if yes(n, "Placement/Height_From_Stand_Offs") {
        y += mm(n, "Placement/Stand_Off_Height").unwrap_or(0.0);
    }
    let lo = [
        mm(n, "Placement/X_Location").unwrap_or(0.0),
        y,
        mm(n, "Placement/Z_Location").unwrap_or(0.0),
    ];
    if footprint_angle(n, &mut ctx.warnings, &format!("PCB {name}")).abs() > 1e-9 {
        ctx.warnings
            .push(format!("PCB {name}: rotation in the board plane ignored"));
    }
    let c = at(n, "Construction");
    let level = c
        .and_then(|c| txt(c, "Modelling_Level"))
        .unwrap_or("Simplified Average");
    let copper = ctx.mat(c.and_then(|c| txt(c, "Conductor_Material")), "cu-c11000");
    let dielectric = ctx.mat(c.and_then(|c| txt(c, "Dielectric_Material")), "fr4");
    let mut stack = PcbStack {
        thickness_mm: t,
        dielectric,
        copper,
        copper_layers: vec![],
        layers: None,
        copper_oz: None,
        coverage: None,
        via_fraction: 0.0,
        normal: "z".into(),
    };
    let per_layer: Vec<CopperLayer> = c
        .and_then(|c| at(c, "Conductor_Layers"))
        .map(|ls| {
            ls.children("Conductor_Layer")
                .filter(|l| installed(l))
                .filter_map(|l| {
                    Some(CopperLayer {
                        oz: mm(l, "Construction/Conductor_Thickness")? / OZ_MM,
                        coverage: txt(l, "Construction/Conductor_Planar_Percentage")
                            .and_then(pct)
                            .unwrap_or(0.5),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n_layers = c
        .and_then(|c| txt(c, "Number_Of_Conductor_Layers"))
        .and_then(val)
        .map(|v| v as usize);
    let simplified = level.to_ascii_lowercase().contains("simplified") || per_layer.is_empty();
    if simplified {
        stack.layers = Some(n_layers.unwrap_or(per_layer.len().max(2)).max(1));
        stack.copper_oz = Some(
            c.and_then(|c| mm(c, "Conductor_Thickness"))
                .unwrap_or(OZ_MM)
                / OZ_MM,
        );
        stack.coverage = Some(
            c.and_then(|c| txt(c, "Conductor_Planar_Percentage"))
                .and_then(pct)
                .unwrap_or(0.5),
        );
    } else {
        stack.copper_layers = per_layer;
    }
    let power = txt(n, "Power/Top_Distributed_Power")
        .and_then(power_w)
        .unwrap_or(0.0)
        + txt(n, "Power/Bottom_Distributed_Power")
            .and_then(power_w)
            .unwrap_or(0.0);
    let (a, b) = ctx.f.bx(lo, [lo[0] + w, lo[1] + t, lo[2] + d]);
    let bname = ctx.unique(&name);
    ctx.case.blocks.push(Block {
        name: bname,
        material: None,
        pcb: Some(stack),
        min: a,
        max: b,
        power_w: power,
        emissivity: txt(n, "Construction/Top_Surface_Emissivity").and_then(val),
        finish: None,
    });
    let board = BoardRef { lo, t };
    for (list, item) in [
        ("Components", "Component"),
        ("Heatsinks", "Heatsink"),
        ("GapPads", "Gap_Pad"),
    ] {
        if let Some(l) = at(n, list) {
            for o in l.children(item) {
                match item {
                    "Component" => component(ctx, o, Some(&board)),
                    "Heatsink" => heatsink(ctx, o, Some(&board)),
                    _ => gap_pad(ctx, o, Some(&board)),
                }
            }
        }
    }
    for other in ["TIMs", "Holes", "Batteries", "Chip_Sockets"] {
        if let Some(l) = at(n, other) {
            let k = l.children.iter().filter(|c| !c.children.is_empty()).count();
            for _ in 0..k {
                ctx.skip(&format!("PCB {other}"));
            }
        }
    }
}

// ------------------------------------------------------------------ CAD solids

type Mat3x4 = [f64; 12];

fn transform(n: &XNode) -> Mat3x4 {
    let v: Vec<f64> = txt(n, "Placement/Transform")
        .unwrap_or("1,0,0,0,1,0,0,0,1,0,0,0")
        .split(',')
        .filter_map(|t| t.trim().parse().ok())
        .collect();
    if v.len() == 12 {
        let mut m = [0.0; 12];
        m.copy_from_slice(&v);
        m
    } else {
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]
    }
}

/// Column-major 3×3 rotation followed by the translation (verified on
/// saved models: the transformed CAD bounding boxes match the object origins).
fn apply(m: &Mat3x4, p: V3) -> V3 {
    [
        m[0] * p[0] + m[3] * p[1] + m[6] * p[2] + m[9],
        m[1] * p[0] + m[4] * p[1] + m[7] * p[2] + m[10],
        m[2] * p[0] + m[5] * p[1] + m[8] * p[2] + m[11],
    ]
}

struct Cad<'a> {
    /// id → (instance node, parent instance id)
    instances: HashMap<String, (&'a XNode, Option<String>)>,
    solids: HashMap<String, &'a XNode>,
}

fn index_cad<'a>(root: &'a XNode) -> Cad<'a> {
    let mut cad = Cad {
        instances: HashMap::new(),
        solids: HashMap::new(),
    };
    fn walk<'a>(n: &'a XNode, parent: Option<String>, cad: &mut Cad<'a>) {
        for c in &n.children {
            match c.name.as_str() {
                "CAD_Instance" => {
                    let id = c.attr("id").unwrap_or("").to_string();
                    cad.instances
                        .entry(id.clone())
                        .or_insert((c, parent.clone()));
                    if let Some(ins) = c.child("Instances") {
                        walk(ins, Some(id), cad);
                    }
                }
                "CAD_Solid" => {
                    if let Some(id) = c.attr("id") {
                        cad.solids.insert(id.to_string(), c);
                    }
                    walk(c, parent.clone(), cad);
                }
                _ => walk(c, parent.clone(), cad),
            }
        }
    }
    walk(root, None, &mut cad);
    cad
}

/// Triangles of a CAD solid in its local frame.
fn solid_tris(s: &XNode) -> Vec<[V3; 3]> {
    let mut faces = Vec::new();
    s.descendants("CAD_Face", &mut faces);
    let mut out = Vec::new();
    for f in faces {
        let v: Vec<f64> = txt(f, "Shape/Vertices")
            .unwrap_or("")
            .split(',')
            .filter_map(|t| t.trim().parse().ok())
            .collect();
        let idx: Vec<usize> = txt(f, "Shape/Facets")
            .unwrap_or("")
            .split(',')
            .filter_map(|t| t.trim().parse().ok())
            .collect();
        let nv = v.len() / 3;
        let p = |i: usize| [v[3 * i], v[3 * i + 1], v[3 * i + 2]];
        // facets: three (vertex, normal) index pairs per triangle
        for t in idx.chunks_exact(6) {
            let (a, b, c) = (t[0], t[2], t[4]);
            if a < nv && b < nv && c < nv {
                out.push([p(a), p(b), p(c)]);
            }
        }
    }
    out
}

fn obstruction(ctx: &mut Ctx, n: &XNode, offset: V3, cad: &Cad) {
    ctx.count("Solid_Obstruction");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let mat = ctx.mat(
        txt(n, "Cooling/MaterialLink").or_else(|| txt(n, "Construction/MaterialLink")),
        "al-6061-t6",
    );
    let power = txt(n, "Cooling/Heat_Dissipated")
        .and_then(power_w)
        .unwrap_or(0.0)
        * txt(n, "Cooling/Utilisation").and_then(pct).unwrap_or(1.0);
    let shape = txt(n, "Geometry/Shape").unwrap_or("Cuboid");
    if shape.eq_ignore_ascii_case("Solid Definition") {
        let link = txt(n, "Geometry/Solid").unwrap_or("").trim().to_string();
        let Some((inst, _)) = cad.instances.get(&link) else {
            ctx.warnings
                .push(format!("obstruction {name}: CAD solid not found, skipped"));
            return;
        };
        let Some(solid) = txt(inst, "Solid").and_then(|s| cad.solids.get(s.trim())) else {
            ctx.warnings
                .push(format!("obstruction {name}: CAD solid not found, skipped"));
            return;
        };
        // instance chain transforms, innermost first
        let mut chain = vec![transform(inst)];
        let mut parent = cad.instances.get(&link).and_then(|x| x.1.clone());
        while let Some(pid) = parent {
            let Some((pn, pp)) = cad.instances.get(&pid) else {
                break;
            };
            chain.push(transform(pn));
            parent = pp.clone();
        }
        let tris: Vec<[V3; 3]> = solid_tris(solid)
            .into_iter()
            .map(|t| {
                t.map(|mut p| {
                    for m in &chain {
                        p = apply(m, p);
                    }
                    ctx.f
                        .p([p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]])
                })
            })
            .collect();
        if tris.is_empty() {
            ctx.warnings.push(format!(
                "obstruction {name}: CAD solid without triangles, skipped"
            ));
            return;
        }
        let mut mn = [f64::INFINITY; 3];
        let mut mx = [f64::NEG_INFINITY; 3];
        for t in &tris {
            for p in t {
                for a in 0..3 {
                    mn[a] = mn[a].min(p[a]);
                    mx[a] = mx[a].max(p[a]);
                }
            }
        }
        let name = ctx.unique(&name);
        ctx.cad.push(CadBody {
            body: StepBody {
                path: name.clone(),
                name,
                tris,
                min: mn,
                max: mx,
                bbox_only: false,
            },
            material: mat,
            power_w: power,
        });
    } else {
        if !shape.eq_ignore_ascii_case("Cuboid") {
            ctx.warnings.push(format!(
                "obstruction {name}: shape '{shape}' imported as its bounding box"
            ));
        }
        let w = mm(n, "Geometry/Width").unwrap_or(1.0);
        let h = mm(n, "Geometry/Height").unwrap_or(1.0);
        let d = mm(n, "Geometry/Depth").unwrap_or(1.0);
        let mut p = [
            mm(n, "Placement/X_Location").unwrap_or(0.0) + offset[0],
            mm(n, "Placement/Y_Location").unwrap_or(0.0) + offset[1],
            mm(n, "Placement/Z_Location").unwrap_or(0.0) + offset[2],
        ];
        if txt(n, "Placement/Origin_Point").is_some_and(|o| o.contains("Centre")) {
            p[0] -= 0.5 * w;
            p[2] -= 0.5 * d;
        }
        ctx.block(&name, mat, p, [p[0] + w, p[1] + h, p[2] + d], power);
    }
}

/// Walk a list of chassis-level objects (with the accumulated group offset).
fn objects(ctx: &mut Ctx, list: &XNode, offset: V3, cad: &Cad) {
    for o in &list.children {
        match o.name.as_str() {
            "Solid_Obstruction" => obstruction(ctx, o, offset, cad),
            "Group_Obstruction" => {
                ctx.count("Group_Obstruction");
                if !installed(o) {
                    ctx.uninstalled += 1;
                    continue;
                }
                let off = [
                    offset[0] + mm(o, "Placement/X_Location").unwrap_or(0.0),
                    offset[1] + mm(o, "Placement/Y_Location").unwrap_or(0.0),
                    offset[2] + mm(o, "Placement/Z_Location").unwrap_or(0.0),
                ];
                if let Some(l) = o.child("Obstructions") {
                    objects(ctx, l, off, cad);
                }
            }
            _ if !o.children.is_empty() => ctx.skip(&o.name),
            _ => {}
        }
    }
}

fn chassis(ctx: &mut Ctx, ch: &XNode, cad: &Cad, top: bool) {
    ctx.count("Chassis");
    let size = [
        mm(ch, "Geometry/Width").unwrap_or(100.0),
        mm(ch, "Geometry/Height").unwrap_or(100.0),
        mm(ch, "Geometry/Depth").unwrap_or(100.0),
    ];
    let o = [
        mm(ch, "Placement/X_Location").unwrap_or(0.0),
        mm(ch, "Placement/Y_Location").unwrap_or(0.0),
        mm(ch, "Placement/Z_Location").unwrap_or(0.0),
    ];
    if top {
        let (a, b) = ctx
            .f
            .bx(o, [o[0] + size[0], o[1] + size[1], o[2] + size[2]]);
        ctx.case.domain.min = a;
        ctx.case.domain.max = b;
    }
    // walls: (side, axis in Celsius, at max?)
    for (side, axis, max) in [
        ("Left_Side", 0usize, false),
        ("Right_Side", 0, true),
        ("Bottom", 1, false),
        ("Top", 1, true),
        ("Front", 2, false),
        ("Rear", 2, true),
    ] {
        let Some(s) = at(ch, &format!("{side}/Chassis_Side")) else {
            continue;
        };
        if !installed(s) {
            continue;
        }
        let t = mm(s, "Construction/Thickness").unwrap_or(1.0).max(0.01);
        let mut lo = o;
        let mut hi = [o[0] + size[0], o[1] + size[1], o[2] + size[2]];
        if max {
            lo[axis] = hi[axis] - t;
        } else {
            hi[axis] = lo[axis] + t;
        }
        let mat = ctx.mat(txt(s, "Construction/MaterialLink"), "al-6061-t6");
        ctx.count("Chassis_Side");
        ctx.block(
            &format!("{}/{}", name_of(ch), side.to_ascii_lowercase()),
            mat,
            lo,
            hi,
            0.0,
        );
        for feat in [
            "Fans",
            "Vents",
            "Vent_Openings",
            "Heatsinks",
            "Cable_Sockets",
        ] {
            if let Some(l) = s.child(feat) {
                for _ in l.children.iter().filter(|c| !c.children.is_empty()) {
                    ctx.skip(&format!("chassis-side {feat}"));
                }
            }
        }
    }
    if let Some(l) = ch.child("PCBs") {
        for p in l.children("PCB") {
            pcb(ctx, p);
        }
    }
    if let Some(l) = ch.child("Components") {
        for c in l.children("Component") {
            component(ctx, c, None);
        }
    }
    if let Some(l) = ch.child("Heatsinks") {
        for c in l.children("Heatsink") {
            heatsink(ctx, c, None);
        }
    }
    if let Some(l) = ch.child("Obstructions") {
        objects(ctx, l, [0.0; 3], cad);
    }
    if let Some(l) = ch.child("Subchassis") {
        for c in l.children("Chassis") {
            chassis(ctx, c, cad, false);
        }
    }
    let mapped = [
        "PCBs",
        "Components",
        "Heatsinks",
        "Obstructions",
        "Subchassis",
    ];
    for list in [
        "Subassemblies",
        "Batteries",
        "Fans",
        "Fan_Trays",
        "Radial_Fans",
        "Blowers",
        "Flow_Devices",
        "Pumps",
        "CoolingDucts",
        "ColdPlates",
        "TECs",
        "Power_Supplies",
        "Chassis_Bays",
        "Card_Bays",
        "Drive_Bays",
        "Heat_Exchangers",
        "SimplifiedEquipment",
        "Cables",
        "Sensors",
        "Sensor_Arrays",
        "Heat_Sources",
    ] {
        if mapped.contains(&list) {
            continue;
        }
        if let Some(l) = ch.child(list) {
            for c in l.children.iter().filter(|c| !c.children.is_empty()) {
                ctx.skip(&c.name);
            }
        }
    }
}

// ------------------------------------------------------------------ entry point

/// Read a Celsius EC Solver model (`.equipment` archive, its inner file or the XML).
pub fn read(data: &[u8]) -> Result<CelsiusModel, String> {
    let (app, version, text) = extract(data)?;
    // the decoded stream is a sequence of root elements
    let body = match text.find("?>") {
        Some(i) if text.trim_start().starts_with("<?xml") => &text[i + 2..],
        _ => &text[..],
    };
    let wrapped = format!("<FerroThermRoot>{body}</FerroThermRoot>");
    let root = xml::parse(&wrapped).map_err(|e| format!("Celsius model XML: {e}"))?;
    let project = root
        .find("Celsius_EC_Solver_Project")
        .ok_or("no Celsius_EC_Solver_Project in the file (not a Celsius EC Solver model?)")?;
    let top = at(project, "ChassisLink/Chassis").ok_or("the model has no chassis")?;
    let depth =
        mm(top, "Geometry/Depth").unwrap_or(100.0) + mm(top, "Placement/Z_Location").unwrap_or(0.0);
    let mut case = Case::from_toml(
        "[domain]\nmin = [0, 0, 0]\nmax = [100, 100, 100]\nmaterial = \"air-still\"\n[grid]\nmax_cell_mm = 3.0\n",
    )
    .map_err(|e| e.to_string())?;
    case.meta.title = format!(
        "{} (from Celsius EC Solver)",
        txt(project, "Identification/Name").unwrap_or("model")
    );
    case.meta.description = format!("Imported from a {app} {version} model");
    let mut ctx = Ctx {
        f: Frame { depth },
        mats: HashMap::new(),
        case,
        cad: vec![],
        warnings: vec![],
        stats: BTreeMap::new(),
        skipped: BTreeMap::new(),
        uninstalled: 0,
        names: HashMap::new(),
    };
    materials(project, &mut ctx);

    // fluid and solver settings
    let fl = at(project, "Project_Fluid/Fluid").and_then(fluid);
    let sc = at(project, "SolutionControlLink/Solution_Control");
    let solve_flow = sc
        .and_then(|s| txt(s, "Solution_Scheme/Solution_Type"))
        .is_none_or(|t| t.to_ascii_lowercase().contains("flow"));
    if let Some(f) = &fl {
        ctx.case.materials.push(f.clone());
    }
    if solve_flow {
        if let Some(f) = &fl {
            ctx.case.domain.material = f.id.clone();
            let mut fs = FlowSettings::default();
            if let Some(s) = sc {
                if let Some(g) = at(s, "Gravity") {
                    if txt(g, "Active").is_some_and(|a| a.eq_ignore_ascii_case("no")) {
                        fs.gravity = [0.0; 3];
                    } else {
                        let gx = txt(g, "Definition/Gx").and_then(val).unwrap_or(0.0);
                        let gy = txt(g, "Definition/Gy").and_then(val).unwrap_or(-9.81);
                        let gz = txt(g, "Definition/Gz").and_then(val).unwrap_or(0.0);
                        fs.gravity = [gx, -gz, gy];
                    }
                }
                match txt(s, "Turbulence/Model")
                    .unwrap_or("LVEL")
                    .to_ascii_lowercase()
                {
                    t if t.contains("laminar") => fs.turbulence = Turbulence::Laminar,
                    t if t.contains("lvel") => fs.turbulence = Turbulence::Lvel,
                    t => {
                        fs.turbulence = Turbulence::Lvel;
                        ctx.warnings.push(format!(
                            "turbulence model '{t}' is not available: LVEL used"
                        ));
                    }
                }
                if let Some(it) = txt(s, "Iterations/Number_of_Iterations").and_then(val) {
                    fs.max_iterations = (it as usize).clamp(50, 5000);
                }
            }
            ctx.case.flow = Some(fs);
        }
    }

    // environment: ambient, external convection and radiation
    let env = at(project, "Environments/Environment");
    let amb = env
        .and_then(|e| txt(e, "Temperature/Temperature"))
        .and_then(temp_c)
        .unwrap_or(25.0);
    let h = env
        .and_then(|e| {
            txt(
                e,
                "Temperature/External_Heat_Transfer/Heat_Transfer_Coefficient",
            )
        })
        .and_then(val);
    let rad = env.and_then(|e| at(e, "Temperature/External_Heat_Transfer/Heat_Radiation"));
    let rad_on = rad.is_some_and(|r| yes(r, "Active"));
    let t_rad = rad
        .and_then(|r| txt(r, "Radiant_Temperature"))
        .and_then(temp_c);
    let eps = rad
        .filter(|r| yes(r, "Specify_Emissivity"))
        .and_then(|r| txt(r, "Emissivity"))
        .and_then(val);

    let cad = index_cad(project);
    chassis(&mut ctx, top, &cad, true);

    // domain faces
    let open =
        txt(top, "Identification/Model").is_some_and(|m| m.to_ascii_lowercase().contains("open"));
    let flow_on = ctx.case.flow.is_some();
    for f in Face::ALL {
        let kind = if open && flow_on {
            BoundaryKind::Opening
        } else {
            BoundaryKind::Convection
        };
        ctx.case.boundaries.push(Boundary {
            name: format!("ambient_{}", f.name()),
            face: f,
            region: None,
            kind,
            t_c: None,
            h: (kind == BoundaryKind::Convection).then(|| h.unwrap_or(5.0)),
            t_ambient_c: Some(amb),
            emissivity: (kind == BoundaryKind::Convection && rad_on).then(|| eps.unwrap_or(0.9)),
            finish: None,
            t_radiation_c: if kind == BoundaryKind::Convection && rad_on {
                t_rad
            } else {
                None
            },
            heat_flux_w_m2: None,
            solar_w_m2: None,
            absorptivity: None,
            velocity_m_s: None,
            flow_m3_s: None,
            loss: None,
        });
    }
    if open && !flow_on {
        ctx.warnings.push(
            "open-boundary model solved without flow: the domain faces get the environment convection coefficient".into(),
        );
    }
    if rad_on && eps.is_none() {
        ctx.warnings.push(
            "external radiation: surface emissivity not specified in the environment, 0.9 used"
                .into(),
        );
    }

    // grid: about 60 cells on the largest side, within 1–6 mm
    let d = &ctx.case.domain;
    let big = (0..3).map(|a| d.max[a] - d.min[a]).fold(0.0, f64::max);
    ctx.case.grid.max_cell_mm =
        ferrotherm_core::case::MaybeVec3::Scalar((big / 60.0).clamp(1.0, 6.0));

    if ctx.uninstalled > 0 {
        ctx.warnings.push(format!(
            "{} objects marked 'not installed' in Celsius were skipped",
            ctx.uninstalled
        ));
    }
    for (k, n) in &ctx.skipped {
        ctx.warnings
            .push(format!("{n} × {k}: not supported yet, skipped"));
    }
    ctx.warnings.push(
        "check the orientation: Celsius (X, Y up, Z) was mapped to FerroTherm (x = X, y = depth − Z, z = Y)".into(),
    );
    Ok(CelsiusModel {
        version,
        case: ctx.case,
        cad: ctx.cad,
        warnings: ctx.warnings,
        stats: ctx.stats,
    })
}

impl CelsiusModel {
    /// The complete case, CAD solids turned into blocks with `mode`.
    pub fn to_case(&self, mode: crate::step::BlockMode) -> Case {
        let mut case = self.case.clone();
        for c in &self.cad {
            let boxes = crate::step::body_boxes(&c.body, mode);
            let vol: f64 = boxes
                .iter()
                .map(|(a, b)| (0..3).map(|k| b[k] - a[k]).product::<f64>())
                .sum();
            let multi = boxes.len() > 1;
            for (k, (lo, hi)) in boxes.into_iter().enumerate() {
                let v = (0..3).map(|a| hi[a] - lo[a]).product::<f64>();
                case.blocks.push(Block {
                    name: if multi {
                        format!("{}#{}", c.body.name, k + 1)
                    } else {
                        c.body.name.clone()
                    },
                    material: Some(c.material.clone()),
                    pcb: None,
                    min: lo,
                    max: hi,
                    power_w: if vol > 0.0 { c.power_w * v / vol } else { 0.0 },
                    emissivity: None,
                    finish: None,
                });
            }
        }
        case
    }

    pub fn summary(&self) -> String {
        let s: Vec<String> = self.stats.iter().map(|(k, v)| format!("{v} {k}")).collect();
        format!(
            "Celsius EC Solver {} model: {}; {} CAD solids",
            self.version,
            s.join(", "),
            self.cad.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a container like Celsius does: header + one zlib block.
    fn container(xml: &str) -> Vec<u8> {
        use std::io::Write;
        let mut out = Vec::new();
        for s in ["ET Constructor", "2024.36"] {
            let u: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_be_bytes()).collect();
            out.extend((u.len() as u32).to_be_bytes());
            out.extend(u);
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(xml.as_bytes()).unwrap();
        let c = z.finish().unwrap();
        out.extend(0u32.to_be_bytes());
        out.extend((c.len() as u32).to_be_bytes());
        out.extend((xml.len() as u32).to_be_bytes());
        out.extend(c);
        out
    }

    const MODEL: &str = r#"<DataDefinition label="ET Constructor" version="2024.36">
<Celsius_EC_Solver_Project id="1-1">
<Identification><Name>Test project</Name></Identification>
<ChassisLink><Chassis id="2-1"><Installed>Yes</Installed>
<Identification><Type>Enclosure</Type><Model>Enclosure</Model><Reference_Designator>Box</Reference_Designator></Identification>
<Geometry><Height>50 mm</Height><Width>200 mm</Width><Depth>150 mm</Depth></Geometry>
<Placement><Origin_Point>Low Corner</Origin_Point><X_Location>0 mm</X_Location><Y_Location>0 mm</Y_Location><Z_Location>0 mm</Z_Location></Placement>
<Bottom><Chassis_Side><Installed>Yes</Installed><Construction><Thickness>2 mm</Thickness><MaterialLink>10-1</MaterialLink></Construction></Chassis_Side></Bottom>
<Top><Chassis_Side><Installed>No</Installed><Construction><Thickness>2 mm</Thickness><MaterialLink>10-1</MaterialLink></Construction></Chassis_Side></Top>
<PCBs><PCB id="3-1"><Installed>Yes</Installed>
<Identification><Model>PCB</Model><Reference_Designator>BOARD</Reference_Designator></Identification>
<Geometry><Shape>Rectangle</Shape><Thickness>1.6 mm</Thickness><Width>100 mm</Width><Depth>80 mm</Depth></Geometry>
<Placement><Origin_Point>Low Corner</Origin_Point><Board_Side>Top</Board_Side><X_Location>10 mm</X_Location><Y_Location>20 mm</Y_Location><Z_Location>5 mm</Z_Location><Height_From_Stand_Offs>No</Height_From_Stand_Offs></Placement>
<Construction><Conductor_Material>11-1</Conductor_Material><Dielectric_Material>12-1</Dielectric_Material><Modelling_Level>Simplified Average</Modelling_Level><Conductor_Thickness>0.035 mm</Conductor_Thickness><Conductor_Planar_Percentage>60 %</Conductor_Planar_Percentage><Number_Of_Conductor_Layers>4</Number_Of_Conductor_Layers></Construction>
<Power><Top_Distributed_Power>0 W</Top_Distributed_Power><Bottom_Distributed_Power>0 W</Bottom_Distributed_Power></Power>
<Components>
<Component><Installed>Yes</Installed><Identification><Model>X</Model><Reference_Designator>U1</Reference_Designator></Identification>
<Geometry><Shape>Rectangle</Shape><Height>2 mm</Height><Width>10 mm</Width><Depth>6 mm</Depth></Geometry>
<Placement><Origin_Point>Low Centre</Origin_Point><Board_Side>Top</Board_Side><X_Location>50 mm</X_Location><Y_Location>0 mm</Y_Location><Z_Location>40 mm</Z_Location><Angle_To_Board>0 degrees</Angle_To_Board></Placement>
<Construction><Modelling_Level>2R</Modelling_Level><Junction-to-Case_Thermal_Resistance>5 K/W</Junction-to-Case_Thermal_Resistance><Junction-to-Board_Thermal_Resistance>10 C/W</Junction-to-Board_Thermal_Resistance></Construction>
<Power><Thermal_Design_Power>2 W</Thermal_Design_Power><Power_Utilisation><Utilisation>100 %</Utilisation></Power_Utilisation></Power>
</Component>
<Component><Installed>No</Installed><Identification><Reference_Designator>U2</Reference_Designator></Identification></Component>
<Component><Installed>Yes</Installed><Identification><Reference_Designator>Q1</Reference_Designator></Identification>
<Geometry><Shape>Rectangle</Shape><Height>3 mm</Height><Width>8 mm</Width><Depth>4 mm</Depth></Geometry>
<Placement><Origin_Point>Low Centre</Origin_Point><Board_Side>Bottom</Board_Side><X_Location>20 mm</X_Location><Y_Location>0 mm</Y_Location><Z_Location>20 mm</Z_Location><Angle_To_Board>90 degrees</Angle_To_Board></Placement>
<Construction><Modelling_Level>Simplified</Modelling_Level><MaterialLink>13-1</MaterialLink></Construction>
<Power><Thermal_Design_Power>1.5 W</Thermal_Design_Power></Power>
</Component>
</Components>
</PCB></PCBs>
</Chassis></ChassisLink>
<Materials>
<Material id="10-1"><Name>Aluminium, Pure</Name><Conductivity><Type>Isotropic</Type><Option>Constant</Option><Value>220 W/(m K)</Value></Conductivity><Density><Value>2710 kg/m^3</Value></Density><Specific_Heat><Value>896 J/(kg K)</Value></Specific_Heat></Material>
<Material id="11-1"><Name>Copper, Pure</Name><Conductivity><Type>Isotropic</Type><Value>386 W/(m K)</Value></Conductivity><Density><Value>8950 kg/m^3</Value></Density><Specific_Heat><Value>380 J/(kg K)</Value></Specific_Heat></Material>
<Material id="12-1"><Name>FR4</Name><Conductivity><Type>Isotropic</Type><Value>0.3 W/(m K)</Value></Conductivity><Density><Value>1200 kg/m^3</Value></Density><Specific_Heat><Value>880 J/(kg K)</Value></Specific_Heat></Material>
<Material id="13-1"><Name>Typical Component</Name><Conductivity><Type>Isotropic</Type><Value>10 W/(m K)</Value></Conductivity><Density><Value>3000 kg/m^3</Value></Density><Specific_Heat><Value>500 J/(kg K)</Value></Specific_Heat></Material>
</Materials>
<Project_Fluid><Fluid><Name>Air</Name><Density><Value>1.19 kg/m^3</Value></Density><Laminar_Viscosity><Value>0.0000184 kg/ms</Value></Laminar_Viscosity><Conductivity><Value>0.0261 W/(m K)</Value></Conductivity><Specific_Heat><Value>1005 J/(kg K)</Value></Specific_Heat><Expansivity><Value>0.0033333 1/C</Value></Expansivity></Fluid></Project_Fluid>
<Environments><Environment><Identification><Name>Default Environment</Name></Identification>
<Temperature><Temperature>40 C</Temperature><External_Heat_Transfer><Heat_Transfer_Coefficient>6 W/(m^2K)</Heat_Transfer_Coefficient><Heat_Radiation><Active>No</Active></Heat_Radiation></External_Heat_Transfer></Temperature></Environment></Environments>
<SolutionControlLink><Solution_Control><Solution_Scheme><Solution_Type>Flow and Temperature</Solution_Type></Solution_Scheme>
<Iterations><Number_of_Iterations>300</Number_of_Iterations></Iterations><Turbulence><Model>LVEL</Model></Turbulence>
<Gravity><Active>Yes</Active><Definition><Gx>0 m/s^2</Gx><Gy>-9.81 m/s^2</Gy><Gz>0 m/s^2</Gz></Definition></Gravity></Solution_Control></SolutionControlLink>
</Celsius_EC_Solver_Project>
</DataDefinition>
"#;

    #[test]
    fn units() {
        assert_eq!(len_mm("233.35 mm"), Some(233.35));
        assert_eq!(len_mm("0.5 m"), Some(500.0));
        assert_eq!(len_mm("1e-3 m"), Some(1.0));
        assert_eq!(temp_c("49 C"), Some(49.0));
        assert!((temp_c("300 K").unwrap() - 26.85).abs() < 1e-9);
        assert_eq!(pct("80 %"), Some(0.8));
        assert_eq!(power_w("6.5 W"), Some(6.5));
        assert_eq!(
            curve("0,220, 200,220 <C,W/(m K)>"),
            vec![[0.0, 220.0], [200.0, 220.0]]
        );
    }

    #[test]
    fn synthetic_model() {
        let m = read(&container(MODEL)).unwrap();
        let c = &m.case;
        eprintln!("{}\n{:?}", m.summary(), m.warnings);
        // domain: W 200 (x), D 150 (y), H 50 (z)
        assert_eq!(c.domain.max, [200.0, 150.0, 50.0]);
        assert_eq!(c.domain.material, "ecs-air");
        assert!(c.flow.is_some());
        assert_eq!(c.flow.as_ref().unwrap().gravity, [0.0, 0.0, -9.81]);
        // bottom wall only (top not installed)
        let walls: Vec<_> = c
            .blocks
            .iter()
            .filter(|b| b.name.starts_with("Box/"))
            .collect();
        assert_eq!(walls.len(), 1);
        assert_eq!(walls[0].min[2], 0.0);
        assert_eq!(walls[0].max[2], 2.0);
        // board: x 10..110, z 20..21.6, y = 150 − (5..85) = 65..145
        let b = c.blocks.iter().find(|b| b.name == "BOARD").unwrap();
        assert_eq!(b.min, [10.0, 65.0, 20.0]);
        assert!((b.max[2] - 21.6).abs() < 1e-9);
        let st = b.pcb.as_ref().unwrap();
        assert_eq!(st.layers, Some(4));
        assert!((st.coverage.unwrap() - 0.6).abs() < 1e-12);
        assert_eq!(st.dielectric, "ecs-fr4");
        // U1: 2R on top, centre (50, 40) → x 55..65, Celsius z 42..48 → y 102..108
        let u1 = c.components.iter().find(|x| x.name == "U1").unwrap();
        assert_eq!(u1.r_jc, Some(5.0));
        assert_eq!(u1.r_jb, Some(10.0));
        assert_eq!(u1.power_w, 2.0);
        assert!((u1.position[0] - 55.0).abs() < 1e-9);
        assert!((u1.position[1] - 102.0).abs() < 1e-9);
        assert!((u1.position[2] - 21.6).abs() < 1e-9);
        // U2 not installed; Q1 bottom, rotated 90°: footprint 4 × 8, below the board
        assert!(
            c.components.iter().all(|x| x.name != "U2") && c.blocks.iter().all(|x| x.name != "U2")
        );
        let q1 = c.blocks.iter().find(|x| x.name == "Q1").unwrap();
        assert!((q1.max[0] - q1.min[0] - 4.0).abs() < 1e-9);
        assert!((q1.max[1] - q1.min[1] - 8.0).abs() < 1e-9);
        assert!((q1.max[2] - 20.0).abs() < 1e-9 && (q1.min[2] - 17.0).abs() < 1e-9);
        assert_eq!(q1.power_w, 1.5);
        assert_eq!(q1.material.as_deref(), Some("ecs-typical-component"));
        // environment
        let bnd = &c.boundaries[0];
        assert_eq!(bnd.t_ambient_c, Some(40.0));
        assert_eq!(bnd.h, Some(6.0));
        // the case solves
        let text = c.to_toml().unwrap();
        let back = Case::from_toml(&text).unwrap();
        assert_eq!(back.materials.len(), c.materials.len());
    }
}
