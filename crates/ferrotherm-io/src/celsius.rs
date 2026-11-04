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
//! turbulence, iterations, surface-to-surface radiation with the materials'
//! surface emissivities), materials, PCBs (layered stack, any orientation
//! that is a multiple of 90°, with the parts mounted on them), components
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
    Block, Boundary, BoundaryKind, Case, Component, Face, FlowSettings, RadiationSettings,
    Turbulence,
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
            // rack units (EIA-310)
            "U" | "u" | "RU" => 44.45,
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
    // "x,y, x,y <ux,uy>": the unit tag may arrive as "&lt;ux,uy&gt;" (the
    // XML reader keeps entities), so each value is the leading number of
    // its comma-separated token
    let body = s.split('<').next().unwrap_or("");
    let body = body.split("&lt;").next().unwrap_or("");
    let nums: Vec<f64> = body
        .split(',')
        .filter_map(|t| split(t).map(|x| x.0))
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
    /// Surface emissivity for radiation (from the Celsius material).
    pub emissivity: Option<f64>,
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
    /// Frame of the chassis (bay, slot…) being read, in the top-level
    /// Celsius frame: object locations are local to it.
    base: Pose,
    mats: HashMap<String, String>,
    /// FerroTherm material id → surface emissivity of the Celsius material.
    eps: HashMap<String, f64>,
    case: Case,
    cad: Vec<CadBody>,
    warnings: Vec<String>,
    stats: BTreeMap<String, usize>,
    skipped: BTreeMap<String, usize>,
    uninstalled: usize,
    names: HashMap<String, usize>,
    /// Name prefix of the objects being read (bay/slot of a nested server).
    prefix: String,
    /// Openings found on the domain faces (vents, fans in outer walls),
    /// appended after the whole-face boundaries so that they win.
    openings: Vec<Boundary>,
}

impl Ctx {
    fn count(&mut self, k: &str) {
        *self.stats.entry(k.to_string()).or_default() += 1;
    }
    fn skip(&mut self, k: &str) {
        *self.skipped.entry(k.to_string()).or_default() += 1;
    }
    fn pref(&self, base: &str) -> String {
        if self.prefix.is_empty() {
            base.to_string()
        } else {
            format!("{}/{base}", self.prefix)
        }
    }
    fn unique(&mut self, base: &str) -> String {
        let base = self.pref(base);
        let n = self.names.entry(base.clone()).or_default();
        *n += 1;
        if *n == 1 {
            base
        } else {
            format!("{base}~{n}")
        }
    }
    fn mat(&self, link: Option<&str>, fallback: &str) -> String {
        link.and_then(|l| self.mats.get(l.trim()).cloned())
            .unwrap_or_else(|| fallback.to_string())
    }
    /// Celsius minimum cell counts (local width x, depth z) of the last
    /// block if it is `name`, mapped to the FerroTherm axes.
    fn min_cells_last(&mut self, pose: &Pose, name_prefix: &str, cells: Option<(usize, usize)>) {
        let Some((cw, cd)) = cells else { return };
        let Some(b) = self.case.blocks.last_mut() else {
            return;
        };
        // the block was not pushed (degenerate) when the last one is another
        let full = if self.prefix.is_empty() {
            name_prefix.to_string()
        } else {
            format!("{}/{name_prefix}", self.prefix)
        };
        let own = b.name == full
            || b.name
                .strip_prefix(full.as_str())
                .is_some_and(|r| r.starts_with('~'));
        if !own {
            return;
        }
        let mut m = [0usize; 3];
        for (local, n) in [(0usize, cw), (2usize, cd)] {
            let (ca, s) = pose.axis(local);
            m[ft_axis(ca, s).0] = n;
        }
        b.min_cells = Some(m);
    }
    /// Board block given in the local frame of `pose`.
    fn block_pcb_in(&mut self, pose: &Pose, name: &str, st: PcbStack, lo: V3, hi: V3, power: f64) {
        let (lo, hi) = pose.bx(lo, hi);
        let (a, b) = self.f.bx(lo, hi);
        if (0..3).any(|k| b[k] - a[k] <= 1e-9) {
            return;
        }
        let name = self.unique(name);
        self.case.blocks.push(Block {
            name,
            material: None,
            pcb: Some(st),
            min: a,
            max: b,
            power_w: power,
            emissivity: None,
            finish: None,
            outline: None,
            outline_axis: None,
            min_cells: None,
        });
    }
    /// Block given in the local frame of `pose`; false when degenerate.
    fn block_in(
        &mut self,
        pose: &Pose,
        name: &str,
        mat: String,
        lo: V3,
        hi: V3,
        power: f64,
    ) -> bool {
        let (lo, hi) = pose.bx(lo, hi);
        let (a, b) = self.f.bx(lo, hi);
        if (0..3).any(|k| b[k] - a[k] <= 1e-9) {
            return false;
        }
        let name = self.unique(name);
        let emissivity = self.eps.get(&mat).copied();
        self.case.blocks.push(Block {
            name,
            material: Some(mat),
            pcb: None,
            min: a,
            max: b,
            power_w: power,
            emissivity,
            finish: None,
            outline: None,
            outline_axis: None,
            min_cells: None,
        });
        true
    }
    /// Block (local box of `pose`) with holes: polygons in the local plane
    /// normal to the local `axis` (points given in local 3D coordinates).
    #[allow(clippy::too_many_arguments)]
    fn block_holes(
        &mut self,
        pose: &Pose,
        name: &str,
        mat: String,
        lo: V3,
        hi: V3,
        axis: usize,
        holes: &[Vec<V3>],
    ) {
        if !self.block_in(pose, name, mat, lo, hi, 0.0) || holes.is_empty() {
            return;
        }
        let (ca, sg) = pose.axis(axis);
        let fa = ft_axis(ca, sg).0;
        let tg = ferrotherm_core::model::tangential(fa);
        let ring: Vec<Vec<[f64; 2]>> = holes
            .iter()
            .map(|h| {
                h.iter()
                    .map(|p| {
                        let w = self.f.p(pose.apply(*p));
                        [w[tg[0]], w[tg[1]]]
                    })
                    .collect()
            })
            .collect();
        let b = self.case.blocks.last_mut().unwrap();
        let mut rings = vec![vec![
            [b.min[tg[0]], b.min[tg[1]]],
            [b.max[tg[0]], b.min[tg[1]]],
            [b.max[tg[0]], b.max[tg[1]]],
            [b.min[tg[0]], b.max[tg[1]]],
        ]];
        rings.extend(ring);
        b.outline = Some(rings);
        b.outline_axis = Some(AXIS_NAME[fa].to_string());
    }
    /// The domain face a FerroTherm plane (axis, coordinate mm) lies on.
    fn domain_face(&self, a: usize, c: f64) -> Option<Face> {
        let d = &self.case.domain;
        if (c - d.min[a]).abs() < 1e-6 {
            Some([Face::Xmin, Face::Ymin, Face::Zmin][a])
        } else if (c - d.max[a]).abs() < 1e-6 {
            Some([Face::Xmax, Face::Ymax, Face::Zmax][a])
        } else {
            None
        }
    }
}

mod rack;

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
                if let Some(e) = txt(m, "Heat_Radiation_Properties/Surface_Emissivity/Value")
                    .and_then(val)
                    .filter(|e| (0.0..=1.0).contains(e))
                {
                    ctx.eps.insert(mid.clone(), e);
                }
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

// ------------------------------------------------------------------ orientation

/// 3×3 rotation, row-major.
type M3 = [[f64; 3]; 3];

const I3: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn mul3(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

fn mv3(a: &M3, v: V3) -> V3 {
    [0, 1, 2].map(|i| a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2])
}

/// Right-handed rotation by `deg` about the unit vector `u` (Rodrigues).
fn rot_about(u: V3, deg: f64) -> M3 {
    let n = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
    if n < 1e-12 || deg.abs() < 1e-12 {
        return I3;
    }
    let [x, y, z] = [u[0] / n, u[1] / n, u[2] / n];
    let (s, c) = deg.to_radians().sin_cos();
    let t = 1.0 - c;
    let mut m = [
        [t * x * x + c, t * x * y - s * z, t * x * z + s * y],
        [t * x * y + s * z, t * y * y + c, t * y * z - s * x],
        [t * x * z - s * y, t * y * z + s * x, t * z * z + c],
    ];
    // exact zeros and ones for the usual multiples of 90°
    for r in m.iter_mut() {
        for v in r.iter_mut() {
            for e in [-1.0, 0.0, 1.0] {
                if (*v - e).abs() < 1e-12 {
                    *v = e;
                }
            }
        }
    }
    m
}

/// Orientation of an object in its parent frame (Celsius axes), and a note
/// when the conversion relies on a convention not verified on real models.
///
/// Verified (model exported to STEP by Celsius EC Solver 2024.36): an
/// "Ordered" rotation of +90° about X turns a board's local +Z into −Y and
/// its local +Y (the board normal) into +Z, i.e. right-handed rotations
/// about the object's placement point (its low corner).
fn orientation(n: &XNode) -> (M3, Option<String>) {
    let Some(o) = at(n, "Placement/Orientation") else {
        return (I3, None);
    };
    let num = |k: &str| txt(o, k).and_then(val).unwrap_or(0.0);
    let ang = |k: &str| txt(o, k).and_then(deg).unwrap_or(0.0);
    if txt(o, "Rotation_Mechanism").is_some_and(|m| m.eq_ignore_ascii_case("Angle and Axis")) {
        let a = ang("Angle");
        let r = rot_about([num("X_Axis"), num("Y_Axis"), num("Z_Axis")], a);
        return (r, None);
    }
    let order = txt(o, "Rotation_Order")
        .unwrap_or("XYZ")
        .to_ascii_uppercase();
    let angles = [ang("Angle_1"), ang("Angle_2"), ang("Angle_3")];
    let mut r = I3;
    for (c, a) in order.chars().zip(angles) {
        let u = match c {
            'X' => [1.0, 0.0, 0.0],
            'Y' => [0.0, 1.0, 0.0],
            _ => [0.0, 0.0, 1.0],
        };
        r = mul3(&r, &rot_about(u, a));
    }
    let turned = angles.iter().filter(|a| a.abs() > 1e-9).count();
    let note = (turned > 1).then(|| {
        format!("rotation {order} {angles:?}° combines several axes: the order convention is not verified yet, check the placement")
    });
    (r, note)
}

/// True when the rotation maps axes onto axes (multiples of 90°).
fn axis_aligned(r: &M3) -> bool {
    r.iter()
        .all(|row| row.iter().filter(|v| v.abs() > 1e-9).count() == 1)
}

/// Rigid placement of a local frame in the Celsius frame: p ↦ R p + t.
#[derive(Clone, Copy)]
struct Pose {
    r: M3,
    t: V3,
}

impl Pose {
    const ID: Pose = Pose { r: I3, t: [0.0; 3] };
    /// `self` after `inner`: p ↦ self(inner(p)).
    fn then(&self, inner: &Pose) -> Pose {
        let t = mv3(&self.r, inner.t);
        Pose {
            r: mul3(&self.r, &inner.r),
            t: [t[0] + self.t[0], t[1] + self.t[1], t[2] + self.t[2]],
        }
    }
    fn translated(&self, d: V3) -> Pose {
        self.then(&Pose { r: I3, t: d })
    }
    fn apply(&self, p: V3) -> V3 {
        let q = mv3(&self.r, p);
        [q[0] + self.t[0], q[1] + self.t[1], q[2] + self.t[2]]
    }
    /// Bounding box of a local box (exact when the rotation is axis-aligned).
    fn bx(&self, lo: V3, hi: V3) -> (V3, V3) {
        let mut a = [f64::INFINITY; 3];
        let mut b = [f64::NEG_INFINITY; 3];
        for i in 0..8 {
            let c = [
                if i & 1 == 0 { lo[0] } else { hi[0] },
                if i & 2 == 0 { lo[1] } else { hi[1] },
                if i & 4 == 0 { lo[2] } else { hi[2] },
            ];
            let p = self.apply(c);
            for k in 0..3 {
                a[k] = a[k].min(p[k]);
                b[k] = b[k].max(p[k]);
            }
        }
        (a, b)
    }
    /// Celsius axis and sign of the local axis `k` (axis-aligned rotations).
    fn axis(&self, k: usize) -> (usize, f64) {
        let col = [self.r[0][k], self.r[1][k], self.r[2][k]];
        let a = (0..3)
            .max_by(|&i, &j| col[i].abs().total_cmp(&col[j].abs()))
            .unwrap_or(1);
        (a, col[a].signum())
    }
}

/// FerroTherm axis and sign of a Celsius axis (x = X, y = depth − Z, z = Y).
fn ft_axis(celsius_axis: usize, sign: f64) -> (usize, f64) {
    match celsius_axis {
        0 => (0, sign),
        1 => (2, sign),
        _ => (1, -sign),
    }
}

const AXIS_NAME: [&str; 3] = ["x", "y", "z"];

// ------------------------------------------------------------------ objects

/// Board frame: local x across the width, y through the thickness (top
/// side at y = t), z along the depth; origin at the placement point.
#[derive(Clone, Copy)]
struct BoardRef {
    pose: Pose,
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
    // The stored location is the low corner of the footprint, whatever the
    // displayed Origin_Point ("Low Centre" included): verified against the
    // STEP export of a Celsius EC Solver 2024.36 model.
    let x0 = mm(n, "Placement/X_Location").unwrap_or(0.0);
    let y = mm(n, "Placement/Y_Location").unwrap_or(0.0);
    let z0 = mm(n, "Placement/Z_Location").unwrap_or(0.0);
    if ang.abs() > 1e-9 {
        ctx.warnings.push(format!(
            "component {name}: footprint rotated by {ang}° on the board, placed by the low corner of the rotated footprint (convention not verified)"
        ));
    }
    let bottom = txt(n, "Placement/Board_Side").is_some_and(|s| s.eq_ignore_ascii_case("Bottom"));
    // local box (board frame, or the chassis frame without a board)
    let (pose, lo, hi) = match board {
        Some(b) => {
            let (ylo, yhi) = if bottom {
                (-y - h, -y)
            } else {
                (b.t + y, b.t + y + h)
            };
            (b.pose, [x0, ylo, z0], [x0 + w, yhi, z0 + d])
        }
        None => (ctx.base, [x0, y, z0], [x0 + w, y + h, z0 + d]),
    };
    // board normal (local +y) in FerroTherm axes; the case faces away from the board
    let (cax, csign) = pose.axis(1);
    let (normal, nsign) = ft_axis(cax, csign);
    let case_up = nsign * if bottom { -1.0 } else { 1.0 } > 0.0;
    let mut power = power_of(n);
    let level = txt(n, "Construction/Modelling_Level").unwrap_or("");
    // A "Detailed" package keeps its power in its own parts (typically the
    // die obstruction), not at the component level: add it, since the
    // package is imported as one block.
    let mut inner = Vec::new();
    n.descendants("Solid_Obstruction", &mut inner);
    let inner_power: f64 = inner
        .iter()
        .filter(|o| installed(o))
        .map(|o| {
            txt(o, "Cooling/Heat_Dissipated")
                .and_then(power_w)
                .unwrap_or(0.0)
                * txt(o, "Cooling/Utilisation").and_then(pct).unwrap_or(1.0)
        })
        .sum();
    // Detailed package: body + its internal parts (die, substrate, …) as
    // separate blocks, so the die temperature is resolved.
    if level.eq_ignore_ascii_case("Detailed") {
        let parts: Vec<&XNode> = inner.iter().copied().filter(|o| installed(o)).collect();
        let mut subs = Vec::new();
        n.descendants("Substrate", &mut subs);
        let subs: Vec<&XNode> = subs
            .into_iter()
            .filter(|s| installed(s) && at(s, "Geometry").is_some())
            .collect();
        if !parts.is_empty() || !subs.is_empty() {
            match detailed_parts(ctx, &parts, &subs, lo, hi, &pose, ang, bottom, normal) {
                Ok((list, convention)) => {
                    let mat = ctx.mat(txt(n, "Construction/MaterialLink"), "mold-compound");
                    ctx.block_in(&pose, &name, mat, lo, hi, power);
                    ctx.min_cells_last(&pose, &name, min_cells_of(n));
                    let np = list.len();
                    for p in list {
                        let pose_p = if p.chassis { ctx.base } else { pose };
                        let pname = format!("{name}/{}", p.name);
                        match p.fill {
                            PartFill::Mat(m) => {
                                ctx.block_in(&pose_p, &pname, m, p.lo, p.hi, p.power);
                            }
                            PartFill::Pcb(st) => {
                                ctx.block_pcb_in(&pose_p, &pname, st, p.lo, p.hi, p.power)
                            }
                        }
                        ctx.min_cells_last(&pose_p, &pname, p.cells);
                    }
                    ctx.warnings.push(format!(
                        "component {name}: detailed package resolved into its body and {np} internal part(s), {inner_power} W in the parts (placement: {convention})"
                    ));
                    rack::component_extras(ctx, n, &pose, lo, hi, bottom, power + inner_power);
                    return;
                }
                Err(why) => {
                    ctx.warnings.push(format!(
                        "component {name}: detailed package lumped into one block ({why})"
                    ));
                }
            }
        }
    }
    if inner_power > 0.0 {
        power += inner_power;
        ctx.warnings.push(format!(
            "component {name}: detailed package imported as one block with the {inner_power} W of its internal parts"
        ));
    }
    let rjc = txt(n, "Construction/Junction-to-Case_Thermal_Resistance").and_then(val);
    let rjb = txt(n, "Construction/Junction-to-Board_Thermal_Resistance").and_then(val);
    if level.eq_ignore_ascii_case("2R") && (rjc.is_some() || rjb.is_some()) {
        let (lo, hi) = pose.bx(lo, hi);
        let (a, b) = ctx.f.bx(lo, hi);
        let name = ctx.unique(&name);
        ctx.case.components.push(Component {
            name,
            package: "generic".into(),
            position: a,
            // [L, W, H]: the two in-plane extents (increasing axis order), then the height
            size: Some({
                let e = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let t: Vec<usize> = (0..3).filter(|&k| k != normal).collect();
                [e[t[0]], e[t[1]], e[normal]]
            }),
            power_w: power,
            r_jc: rjc,
            r_jb: rjb,
            case_face: Some(if case_up {
                CaseFace::Top
            } else {
                CaseFace::Bottom
            }),
            mount_axis: AXIS_NAME[normal].into(),
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
        ctx.block_in(&pose, &name, mat, lo, hi, power);
        ctx.min_cells_last(&pose, &name, min_cells_of(n));
    }
    rack::component_extras(ctx, n, &pose, lo, hi, bottom, power);
}

/// Celsius "Minimum Number Of Cells" (width, depth) of an object, if any
/// is above 1.
fn min_cells_of(n: &XNode) -> Option<(usize, usize)> {
    let g = at(n, "Geometry/Grid/Minimum_Number_Of_Cells")?;
    let w = txt(g, "Width_Direction")
        .and_then(val)
        .unwrap_or(0.0)
        .max(0.0) as usize;
    let d = txt(g, "Depth_Direction")
        .and_then(val)
        .unwrap_or(0.0)
        .max(0.0) as usize;
    (w > 1 || d > 1).then_some((w.min(64), d.min(64)))
}

/// Type, material, height and level (µm) shared by an array of parts.
type ArrayKey = (String, String, i64, i64);

enum PartFill {
    Mat(String),
    Pcb(PcbStack),
}

struct InnerPart {
    name: String,
    fill: PartFill,
    lo: V3,
    hi: V3,
    power: f64,
    /// Box in the chassis frame (else in the board frame of the package).
    chassis: bool,
    cells: Option<(usize, usize)>,
}

/// Paint order of a part type: later parts own the cells they share with
/// earlier ones (the die wins over wire bonds and encapsulant).
fn part_rank(typ: &str) -> u8 {
    let t = typ.to_ascii_lowercase();
    if t.contains("encapsulant") || t.contains("mold") || t.contains("lid") {
        0
    } else if t == "die attach" || t.contains("attach") {
        2
    } else if t == "die" {
        3
    } else {
        1
    }
}

/// Internal parts of a "Detailed" package as boxes.
///
/// * Cuboids and cylinders (`Diameter`; the Width/Depth of a cylinder are
///   unused defaults) keep their own material and power.
/// * The package `Substrate` becomes a board stack-up block.
/// * Arrays of small identical parts (solder balls, bumps, pads: 8 or more
///   of one type and height) become one equivalent layer: through the layer
///   k = f·k_s + (1 − f)·k_air, across it the Maxwell–Garnett value for
///   parallel cylinders, f = solder area fraction of the array footprint.
///
/// Celsius does not state the frame of the part locations; three readings
/// are tried and the first that puts every part inside the package body is
/// used: package low corner (height from the board face), board frame,
/// chassis frame. On the real 2024.36 models it is the package corner.
#[allow(clippy::too_many_arguments)]
fn detailed_parts(
    ctx: &mut Ctx,
    parts: &[&XNode],
    subs: &[&XNode],
    lo: V3,
    hi: V3,
    pose: &Pose,
    ang: f64,
    bottom: bool,
    normal: usize,
) -> Result<(Vec<InnerPart>, &'static str), String> {
    if ang.abs() > 1e-9 {
        return Err("rotated footprint".into());
    }
    struct Raw {
        name: String,
        typ: String,
        fill: PartFill,
        p: V3,
        size: V3,
        power: f64,
        /// Footprint area of one part (for arrays), mm².
        area: f64,
        cells: Option<(usize, usize)>,
    }
    let place = |o: &XNode, w: f64, d: f64| -> V3 {
        let mut p = [
            mm(o, "Placement/X_Location").unwrap_or(0.0),
            mm(o, "Placement/Y_Location").unwrap_or(0.0),
            mm(o, "Placement/Z_Location").unwrap_or(0.0),
        ];
        if txt(o, "Placement/Origin_Point").is_some_and(|x| x.contains("Centre")) {
            p[0] -= 0.5 * w;
            p[2] -= 0.5 * d;
        }
        p
    };
    let mut raw = Vec::new();
    for (i, o) in parts.iter().enumerate() {
        let shape = txt(o, "Geometry/Shape").unwrap_or("Cuboid");
        let (r, _) = orientation(o);
        if r != I3 {
            return Err("an internal part is rotated".into());
        }
        let h = mm(o, "Geometry/Height").ok_or("an internal part has no height")?;
        let (w, d, area) = if shape.eq_ignore_ascii_case("Cylinder") {
            let dia = mm(o, "Geometry/Diameter").ok_or("a cylinder part has no diameter")?;
            (dia, dia, std::f64::consts::PI * 0.25 * dia * dia)
        } else if shape.eq_ignore_ascii_case("Cuboid") {
            let w = mm(o, "Geometry/Width").ok_or("an internal part has no width")?;
            let d = mm(o, "Geometry/Depth").ok_or("an internal part has no depth")?;
            (w, d, w * d)
        } else {
            return Err(format!("an internal part has shape '{shape}'"));
        };
        let mut nm = name_of(o);
        if nm.is_empty() || nm == "?" {
            nm = format!("part{}", i + 1);
        }
        raw.push(Raw {
            name: nm,
            typ: txt(o, "Identification/Type").unwrap_or("").to_string(),
            fill: PartFill::Mat(ctx.mat(
                txt(o, "Cooling/MaterialLink").or_else(|| txt(o, "Construction/MaterialLink")),
                "mold-compound",
            )),
            p: place(o, w, d),
            size: [w, h, d],
            power: txt(o, "Cooling/Heat_Dissipated")
                .and_then(power_w)
                .unwrap_or(0.0)
                * txt(o, "Cooling/Utilisation").and_then(pct).unwrap_or(1.0),
            area,
            cells: min_cells_of(o),
        });
    }
    // arrays of small identical parts → one equivalent layer each
    let key = |r: &Raw| {
        let m = match &r.fill {
            PartFill::Mat(m) => m.clone(),
            PartFill::Pcb(_) => String::new(),
        };
        (
            r.typ.clone(),
            m,
            (r.size[1] * 1e6).round() as i64,
            (r.p[1] * 1e6).round() as i64,
        )
    };
    let mut counts: HashMap<ArrayKey, usize> = HashMap::new();
    for r in &raw {
        *counts.entry(key(r)).or_default() += 1;
    }
    let mut merged: Vec<Raw> = Vec::new();
    let mut arrays: Vec<(ArrayKey, Vec<Raw>)> = Vec::new();
    for r in raw {
        let k = key(&r);
        if counts[&k] >= 8 {
            match arrays.iter_mut().find(|(a, _)| *a == k) {
                Some((_, v)) => v.push(r),
                None => arrays.push((k, vec![r])),
            }
        } else {
            merged.push(r);
        }
    }
    let k_air = ctx
        .case
        .materials
        .iter()
        .find(|m| m.id == ctx.case.domain.material)
        .map(|m| m.k.as_vec()[0])
        .unwrap_or(0.026);
    for ((typ, mat, _, _), v) in arrays {
        let mut a = [f64::INFINITY; 3];
        let mut b = [f64::NEG_INFINITY; 3];
        for r in &v {
            for q in 0..3 {
                a[q] = a[q].min(r.p[q]);
                b[q] = b[q].max(r.p[q] + r.size[q]);
            }
        }
        let foot = (b[0] - a[0]) * (b[2] - a[2]);
        let f = (v.iter().map(|r| r.area).sum::<f64>() / foot).clamp(0.0, 1.0);
        let base = ctx.case.materials.iter().find(|m| m.id == mat).cloned();
        let (ks, rho, cp) = match &base {
            Some(m) => (m.k.as_vec()[0], m.density, m.cp),
            None => (50.0, 8500.0, 180.0), // tin–lead/SAC solder, typical
        };
        let kn = f * ks + (1.0 - f) * k_air;
        let kt = k_air * ((ks + k_air) + f * (ks - k_air)) / ((ks + k_air) - f * (ks - k_air));
        let mut kv = [kt; 3];
        kv[normal] = kn;
        let label = if typ.is_empty() {
            "array"
        } else {
            typ.as_str()
        };
        let id = format!("ecs-{}-layer-{}", slug(label), ctx.case.materials.len() + 1);
        ctx.case.materials.push(Material {
            id: id.clone(),
            name: format!("{} layer ({} parts, {:.0} % solid)", label, v.len(), f * 100.0),
            category: "imported".into(),
            k: Conductivity::Orthotropic(kv),
            density: f * rho + (1.0 - f) * 1.2,
            cp,
            viscosity: None,
            beta: None,
            k_table: None,
            specs: vec![],
            source: "Equivalent layer of a Celsius package part array (FerroTherm)".into(),
            notes: format!(
                "through the layer f·k_s + (1−f)·k_air, across it Maxwell–Garnett; f = {f:.3}, k_s = {ks}"
            ),
        });
        merged.push(Raw {
            name: format!("{} ×{}", label, v.len()),
            typ,
            fill: PartFill::Mat(id),
            p: a,
            size: [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            power: v.iter().map(|r| r.power).sum(),
            area: foot,
            // an array is not resolved part by part
            cells: None,
        });
    }
    for (i, sb) in subs.iter().enumerate() {
        let t = mm(sb, "Geometry/Thickness").ok_or("the substrate has no thickness")?;
        let w = mm(sb, "Geometry/Width").ok_or("the substrate has no width")?;
        let d = mm(sb, "Geometry/Depth").ok_or("the substrate has no depth")?;
        let (r, _) = orientation(sb);
        if r != I3 {
            return Err("the substrate is rotated".into());
        }
        let stack = stack_of(ctx, at(sb, "Construction"), t, normal);
        let mut nm = name_of(sb);
        if nm.is_empty() || nm == "?" {
            nm = format!("substrate{}", i + 1);
        }
        merged.push(Raw {
            name: nm,
            typ: "Substrate".into(),
            fill: PartFill::Pcb(stack),
            p: place(sb, w, d),
            size: [w, t, d],
            power: 0.0,
            area: w * d,
            cells: None,
        });
    }
    merged.sort_by_key(|r| part_rank(&r.typ));
    let tol = 1e-3;
    let inside = |a: V3, b: V3, blo: V3, bhi: V3| {
        (0..3).all(|k| a[k] >= blo[k] - tol && b[k] <= bhi[k] + tol && b[k] - a[k] > 1e-9)
    };
    let fill_of = |r: &Raw| match &r.fill {
        PartFill::Mat(m) => PartFill::Mat(m.clone()),
        PartFill::Pcb(s) => PartFill::Pcb(s.clone()),
    };
    let build = |f: &dyn Fn(&Raw) -> (V3, V3), chassis: bool| -> Vec<InnerPart> {
        merged
            .iter()
            .map(|r| {
                let (a, b) = f(r);
                InnerPart {
                    name: r.name.clone(),
                    fill: fill_of(r),
                    lo: a,
                    hi: b,
                    power: r.power,
                    chassis,
                    cells: r.cells,
                }
            })
            .collect()
    };
    // 1. relative to the package low corner, height from the board face
    let rel = |r: &Raw| -> (V3, V3) {
        let (y0, y1) = if bottom {
            (hi[1] - r.p[1] - r.size[1], hi[1] - r.p[1])
        } else {
            (lo[1] + r.p[1], lo[1] + r.p[1] + r.size[1])
        };
        (
            [lo[0] + r.p[0], y0, lo[2] + r.p[2]],
            [lo[0] + r.p[0] + r.size[0], y1, lo[2] + r.p[2] + r.size[2]],
        )
    };
    let cands = build(&rel, false);
    if cands.iter().all(|c| inside(c.lo, c.hi, lo, hi)) {
        return Ok((cands, "relative to the package corner"));
    }
    // 2. board frame
    let abs = |r: &Raw| -> (V3, V3) {
        (
            r.p,
            [r.p[0] + r.size[0], r.p[1] + r.size[1], r.p[2] + r.size[2]],
        )
    };
    let cands = build(&abs, false);
    if cands.iter().all(|c| inside(c.lo, c.hi, lo, hi)) {
        return Ok((cands, "board frame"));
    }
    // 3. chassis frame
    let (clo, chi) = pose.bx(lo, hi);
    let cands = build(&abs, true);
    if cands.iter().all(|c| inside(c.lo, c.hi, clo, chi)) {
        return Ok((cands, "chassis frame"));
    }
    Err("internal parts do not fit the package in any known frame".into())
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
    // extruded fins run over the whole base depth (Fin_Depth belongs to
    // the pin shapes): checked against the STEP export of a Celsius 2024.36
    // model, whose 50 mm deep heat sinks store Fin_Depth = 35 mm
    let fd = bd;
    // rectangular cutouts through the fins: (x0, x1, z0, z1) in the base frame
    let mut cuts: Vec<[f64; 4]> = Vec::new();
    if let Some(l) = at(n, "Cutouts") {
        for c in l.children("Heatsink_Cutout").filter(|c| installed(c)) {
            let shape = txt(c, "Geometry/Shape").unwrap_or("Rectangle");
            if !shape.eq_ignore_ascii_case("Rectangle") {
                ctx.warnings.push(format!(
                    "heat sink {}: cutout shape '{shape}' ignored",
                    name_of(n)
                ));
                continue;
            }
            if yes(c, "Geometry/Cutout_Heatsink_Base") {
                ctx.warnings.push(format!(
                    "heat sink {}: cutout through the base not modelled (fins only)",
                    name_of(n)
                ));
            }
            let x0 = mm(c, "Placement/X_Offset").unwrap_or(0.0);
            let z0 = mm(c, "Placement/Z_Offset").unwrap_or(0.0);
            let w = mm(c, "Geometry/Width").unwrap_or(0.0);
            let d = mm(c, "Geometry/Depth").unwrap_or(0.0);
            cuts.push([x0, x0 + w, z0, z0 + d]);
        }
    }
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
    // base low corner in the board frame (or the chassis frame)
    let (pose, bx, by, bz, dir) = match board {
        Some(b) if bottom => (b.pose, x0, -yoff, z0, -1.0),
        Some(b) => (b.pose, x0, b.t + yoff, z0, 1.0),
        None => (
            ctx.base,
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
    ctx.block_in(
        &pose,
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
            // depth intervals of the fin left by the cutouts
            let mut segs = vec![(zf, zf + fd)];
            for c in &cuts {
                let (cx0, cx1) = (bx + c[0], bx + c[1]);
                if cx1 <= xf + 1e-9 || cx0 >= xf + ft - 1e-9 {
                    continue;
                }
                let (cz0, cz1) = (bz + c[2], bz + c[3]);
                segs = segs
                    .into_iter()
                    .flat_map(|(a, b)| {
                        let mut v = Vec::new();
                        if cz0 > a {
                            v.push((a, cz0.min(b)));
                        }
                        if cz1 < b {
                            v.push((cz1.max(a), b));
                        }
                        v
                    })
                    .filter(|(a, b)| b - a > 1e-9)
                    .collect();
            }
            for (k, (z0, z1)) in segs.iter().enumerate() {
                let fname = if segs.len() == 1 {
                    format!("{name}/fin{}", i + 1)
                } else {
                    format!("{name}/fin{}.{}", i + 1, k + 1)
                };
                ctx.block_in(
                    &pose,
                    &fname,
                    fin_mat.clone(),
                    [xf, f0, *z0],
                    [xf + ft, f1, *z1],
                    0.0,
                );
            }
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
    // offsets from the board placement point in the board frame
    let pose = board.map(|b| b.pose).unwrap_or(ctx.base);
    let lo = p;
    let mat = ctx.mat(
        txt(n, "Cooling/MaterialLink").or_else(|| txt(n, "Construction/MaterialLink")),
        "tim-gap-pad-3",
    );
    ctx.block_in(
        &pose,
        &name,
        mat,
        lo,
        [lo[0] + w, lo[1] + h, lo[2] + d],
        0.0,
    );
}

/// Board stack-up from a Celsius `Construction` node (PCBs and package
/// substrates).
fn stack_of(ctx: &Ctx, c: Option<&XNode>, t: f64, normal: usize) -> PcbStack {
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
        normal: AXIS_NAME[normal].into(),
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
    stack
}

fn pcb(ctx: &mut Ctx, n: &XNode) {
    ctx.count("PCB");
    if !installed(n) {
        ctx.uninstalled += 1;
        return;
    }
    let name = name_of(n);
    let t = mm(n, "Geometry/Thickness").unwrap_or(1.6);
    // footprint in the board plane (local x, z)
    let mut fx = [0.0, mm(n, "Geometry/Width").unwrap_or(100.0)];
    let mut fz = [0.0, mm(n, "Geometry/Depth").unwrap_or(100.0)];
    let mut poly: Vec<[f64; 2]> = vec![];
    if let Some(s) = txt(n, "Geometry/Shape") {
        if !s.eq_ignore_ascii_case("Rectangle") {
            // Width/Depth of a polygon board can be stale: the outline is the
            // geometry (its area matches the stored Surface_Area).
            let pts = curve(txt(n, "Geometry/Outline").unwrap_or(""));
            if pts.len() >= 3 {
                fx = [f64::INFINITY, f64::NEG_INFINITY];
                fz = fx;
                for p in &pts {
                    fx = [fx[0].min(p[0]), fx[1].max(p[0])];
                    fz = [fz[0].min(p[1]), fz[1].max(p[1])];
                }
                poly = pts;
            } else {
                ctx.warnings.push(format!(
                    "PCB {name}: shape '{s}' without a readable outline, imported as a rectangle"
                ));
            }
        }
    }
    let mut y = mm(n, "Placement/Y_Location").unwrap_or(0.0);
    if yes(n, "Placement/Height_From_Stand_Offs") {
        y += mm(n, "Placement/Stand_Off_Height").unwrap_or(0.0);
    }
    let (r, note) = orientation(n);
    if let Some(note) = note {
        ctx.warnings.push(format!("PCB {name}: {note}"));
    }
    if !axis_aligned(&r) {
        ctx.warnings.push(format!(
            "PCB {name}: tilted (not a multiple of 90°), imported as its axis-aligned bounding box"
        ));
    }
    let pose = ctx.base.then(&Pose {
        r,
        t: [
            mm(n, "Placement/X_Location").unwrap_or(0.0),
            y,
            mm(n, "Placement/Z_Location").unwrap_or(0.0),
        ],
    });
    let (nax, _) = pose.axis(1);
    let normal = ft_axis(nax, 1.0).0;
    let stack = stack_of(ctx, at(n, "Construction"), t, normal);
    let power = txt(n, "Power/Top_Distributed_Power")
        .and_then(power_w)
        .unwrap_or(0.0)
        + txt(n, "Power/Bottom_Distributed_Power")
            .and_then(power_w)
            .unwrap_or(0.0);
    let (lo, hi) = pose.bx([fx[0], 0.0, fz[0]], [fx[1], t, fz[1]]);
    let (a, b) = ctx.f.bx(lo, hi);
    let bname = ctx.unique(&name);
    // polygon outline mapped to the FerroTherm in-plane axes of the normal
    let outline = if poly.is_empty() {
        None
    } else {
        let tg = ferrotherm_core::model::tangential(normal);
        let ring: Vec<[f64; 2]> = poly
            .iter()
            .map(|q| {
                let w = ctx.f.p(pose.apply([q[0], 0.0, q[1]]));
                [w[tg[0]], w[tg[1]]]
            })
            .collect();
        Some(vec![ring])
    };
    let outline_axis = outline.as_ref().map(|_| AXIS_NAME[normal].to_string());
    ctx.case.blocks.push(Block {
        name: bname,
        material: None,
        pcb: Some(stack),
        min: a,
        max: b,
        power_w: power,
        emissivity: txt(n, "Construction/Top_Surface_Emissivity").and_then(val),
        finish: None,
        outline,
        outline_axis,
        min_cells: None,
    });
    let board = BoardRef { pose, t };
    board_contents(ctx, n, &board);
}

/// Parts mounted on a board (any board: PCB, daughter card, DIMM).
fn board_contents(ctx: &mut Ctx, n: &XNode, board: &BoardRef) {
    let board = *board;
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
    if let Some(l) = at(n, "Chip_Sockets") {
        for c in l.children("Chip_Socket") {
            rack::chip_socket(ctx, c, &board);
        }
    }
    if let Some(l) = at(n, "Daughter_Board_Sockets") {
        for c in l.children("Daughter_Board_Socket") {
            rack::board_socket(ctx, c, &board);
        }
    }
    for other in ["TIMs", "Holes", "Batteries"] {
        if let Some(l) = at(n, other) {
            for c in l.children.iter().filter(|c| !c.children.is_empty()) {
                if installed(c) {
                    ctx.skip(&format!("PCB {other}"));
                } else {
                    ctx.uninstalled += 1;
                }
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

fn obstruction(ctx: &mut Ctx, n: &XNode, cad: &Cad) {
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
        // The obstruction's own location places the solid (the enclosing
        // groups' locations are not added): verified against the STEP export
        // of a Celsius EC Solver 2024.36 model.
        let at_ = [
            mm(n, "Placement/X_Location").unwrap_or(0.0),
            mm(n, "Placement/Y_Location").unwrap_or(0.0),
            mm(n, "Placement/Z_Location").unwrap_or(0.0),
        ];
        let tris: Vec<[V3; 3]> = solid_tris(solid)
            .into_iter()
            .map(|t| {
                t.map(|mut p| {
                    for m in &chain {
                        p = apply(m, p);
                    }
                    ctx.f.p(ctx
                        .base
                        .apply([p[0] + at_[0], p[1] + at_[1], p[2] + at_[2]]))
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
            emissivity: ctx.eps.get(&mat).copied(),
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
        let mut p = [0.0; 3];
        if txt(n, "Placement/Origin_Point").is_some_and(|o| o.contains("Centre")) {
            p[0] -= 0.5 * w;
            p[2] -= 0.5 * d;
        }
        let (r, note) = orientation(n);
        if let Some(note) = note {
            ctx.warnings.push(format!("obstruction {name}: {note}"));
        }
        if !axis_aligned(&r) {
            ctx.warnings.push(format!(
                "obstruction {name}: tilted (not a multiple of 90°), imported as its axis-aligned bounding box"
            ));
        }
        let pose = ctx.base.then(&Pose {
            r,
            t: [
                mm(n, "Placement/X_Location").unwrap_or(0.0),
                mm(n, "Placement/Y_Location").unwrap_or(0.0),
                mm(n, "Placement/Z_Location").unwrap_or(0.0),
            ],
        });
        ctx.block_in(&pose, &name, mat, p, [p[0] + w, p[1] + h, p[2] + d], power);
    }
}

/// Walk a list of chassis-level objects. Objects inside groups carry
/// positions in the chassis frame: the group's own location is not added
/// (verified against the STEP export of a Celsius EC Solver 2024.36 model).
fn objects(ctx: &mut Ctx, list: &XNode, cad: &Cad) {
    for o in &list.children {
        match o.name.as_str() {
            "Solid_Obstruction" => obstruction(ctx, o, cad),
            "Group_Obstruction" => {
                ctx.count("Group_Obstruction");
                if !installed(o) {
                    ctx.uninstalled += 1;
                    continue;
                }
                if let Some(l) = o.child("Obstructions") {
                    objects(ctx, l, cad);
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
    let (r, note) = orientation(ch);
    if let Some(note) = note {
        ctx.warnings
            .push(format!("chassis {}: {note}", name_of(ch)));
    }
    // frame of the walls (the contents keep the current base frame)
    let wall_pose = ctx.base.then(&Pose { r, t: o });
    if top {
        let (lo, hi) = wall_pose.bx([0.0; 3], size);
        let (a, b) = ctx.f.bx(lo, hi);
        ctx.case.domain.min = a;
        ctx.case.domain.max = b;
    }
    // walls with their vents and fans
    let wall_t = |side: &str| -> f64 {
        at(ch, &format!("{side}/Chassis_Side"))
            .filter(|s| installed(s))
            .map(|s| mm(s, "Construction/Thickness").unwrap_or(1.0).max(0.01))
            .unwrap_or(0.0)
    };
    let (tl, tr, tb, tt) = (
        wall_t("Left_Side"),
        wall_t("Right_Side"),
        wall_t("Bottom"),
        wall_t("Top"),
    );
    for (side, axis, max) in rack::SIDES {
        let Some(s) = at(ch, &format!("{side}/Chassis_Side")) else {
            continue;
        };
        if !installed(s) {
            continue;
        }
        let t = mm(s, "Construction/Thickness").unwrap_or(1.0).max(0.01);
        let mut lo = [0.0; 3];
        let mut hi = size;
        if axis != 1 {
            lo[1] = tb;
            hi[1] = size[1] - tt;
        }
        if axis == 2 {
            lo[0] = tl;
            hi[0] = size[0] - tr;
        }
        let mat = ctx.mat(txt(s, "Construction/MaterialLink"), "al-6061-t6");
        ctx.count("Chassis_Side");
        let feats = rack::Features::of(s);
        rack::wall_in(
            ctx,
            &format!("{}/{}", name_of(ch), side.to_ascii_lowercase()),
            &wall_pose,
            size,
            (lo, hi),
            (axis, max),
            t,
            mat,
            &feats,
        );
        for feat in ["Vents", "Heatsinks", "Cable_Sockets"] {
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
        objects(ctx, l, cad);
    }
    if let Some(l) = ch.child("Subchassis") {
        for c in l.children("Chassis") {
            chassis(ctx, c, cad, false);
        }
    }
    if let Some(l) = ch.child("Power_Supplies") {
        for c in l.children("Power_Supply") {
            rack::power_supply(ctx, c);
        }
    }
    if let Some(l) = ch.child("Drive_Bays") {
        for c in l.children("Drive_Bay") {
            rack::drive_bay(ctx, c);
        }
    }
    if let Some(l) = ch.child("Card_Bays") {
        for c in l.children("Card_Bay") {
            rack::card_bay(ctx, c);
        }
    }
    if let Some(l) = ch.child("Chassis_Bays") {
        for c in l.children("Chassis_Bay") {
            rack::chassis_bay(ctx, c, cad);
        }
    }
    let mapped = [
        "PCBs",
        "Components",
        "Heatsinks",
        "Obstructions",
        "Subchassis",
        "Power_Supplies",
        "Drive_Bays",
        "Card_Bays",
        "Chassis_Bays",
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

/// Box of an imported component: `size` is [L, W, H], the in-plane extents
/// in increasing axis order and H along the mount axis.
fn component_extent(c: &Component) -> (V3, V3) {
    let s = c.size.unwrap_or([0.0; 3]);
    let n = match c.mount_axis.as_str() {
        "x" => 0,
        "y" => 1,
        _ => 2,
    };
    let t: Vec<usize> = (0..3).filter(|&k| k != n).collect();
    let mut e = [0.0; 3];
    e[t[0]] = s[0];
    e[t[1]] = s[1];
    e[n] = s[2];
    let p = c.position;
    (p, [p[0] + e[0], p[1] + e[1], p[2] + e[2]])
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
        base: Pose::ID,
        mats: HashMap::new(),
        eps: HashMap::new(),
        case,
        cad: vec![],
        warnings: vec![],
        stats: BTreeMap::new(),
        skipped: BTreeMap::new(),
        uninstalled: 0,
        names: HashMap::new(),
        prefix: String::new(),
        openings: vec![],
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
            // Celsius relaxes momentum strongly (false time step 0.1 s) and the
            // temperature hardly at all; the FerroTherm default momentum
            // relaxation (0.6) diverged on sealed conduction-cooled chassis
            // with radiation, 0.3 converges them.
            let mut fs = FlowSettings {
                relax_velocity: 0.3,
                ..FlowSettings::default()
            };
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
                    t if t.contains("epsilon")
                        || t.contains("k-e")
                        || t.contains("k_e")
                        || t == "ke"
                        || t.ends_with(" ke") =>
                    {
                        fs.turbulence = Turbulence::KEpsilon
                    }
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

    // internal surface-to-surface radiation (openings radiate to the ambient)
    if sc.is_some_and(|s| yes(s, "Heat_Radiation/Active")) {
        ctx.case.radiation = Some(RadiationSettings::default());
    }

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
    for mut b in std::mem::take(&mut ctx.openings) {
        b.t_ambient_c = Some(amb);
        if flow_on {
            ctx.case.boundaries.push(b);
        }
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

    // objects outside the top-level chassis point at a placement convention
    // this importer does not handle yet
    let (dlo, dhi) = (ctx.case.domain.min, ctx.case.domain.max);
    let out = |lo: V3, hi: V3| (0..3).any(|k| lo[k] < dlo[k] - 1e-3 || hi[k] > dhi[k] + 1e-3);
    let mut outside: Vec<String> = ctx
        .case
        .blocks
        .iter()
        .filter(|b| out(b.min, b.max))
        .map(|b| b.name.clone())
        .collect();
    outside.extend(
        ctx.case
            .components
            .iter()
            .filter(|c| {
                let (lo, hi) = component_extent(c);
                out(lo, hi)
            })
            .map(|c| c.name.clone()),
    );
    outside.extend(
        ctx.cad
            .iter()
            .filter(|c| out(c.body.min, c.body.max))
            .map(|c| c.body.name.clone()),
    );
    if !outside.is_empty() {
        let n = outside.len();
        outside.truncate(8);
        ctx.warnings.push(format!(
            "{n} object(s) lie partly outside the chassis ({}{}): their placement may use a convention not verified yet, check them",
            outside.join(", "),
            if n > 8 { ", …" } else { "" }
        ));
    }

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
                    emissivity: c.emissivity,
                    finish: None,
                    outline: None,
                    outline_axis: None,
                    min_cells: None,
                });
            }
        }
        case
    }

    /// How CAD solids become blocks unless the user chooses: 1 mm voxels
    /// (coarser only beyond 400 voxels on the largest side). A bounding box
    /// would fill a hollow chassis with metal, and coarser voxels shift thin
    /// features: on a conduction-cooled chassis 2 mm voxels moved the card
    /// guides by 1 mm against the boards and inverted the vertical order of
    /// the part temperatures computed by Celsius.
    pub fn default_cad_mode(&self) -> crate::step::BlockMode {
        let d = &self.case.domain;
        let big = (0..3).map(|a| d.max[a] - d.min[a]).fold(0.0, f64::max);
        if self.cad.is_empty() {
            crate::step::BlockMode::BoundingBox
        } else {
            crate::step::BlockMode::Voxels((big / 400.0).max(1.0))
        }
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
        // U1: 2R on top, low corner (50, 40) on the board at (10, 5) → x 60..70, Celsius z 45..51 → y 99..105
        let u1 = c.components.iter().find(|x| x.name == "U1").unwrap();
        assert_eq!(u1.r_jc, Some(5.0));
        assert_eq!(u1.r_jb, Some(10.0));
        assert_eq!(u1.power_w, 2.0);
        assert!((u1.position[0] - 60.0).abs() < 1e-9);
        assert!((u1.position[1] - 99.0).abs() < 1e-9);
        assert!((u1.position[2] - 21.6).abs() < 1e-9);
        assert_eq!(u1.mount_axis, "z");
        assert_eq!(u1.case_face, Some(CaseFace::Top));
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

    /// Rack objects, with the conventions checked on the STEP export of the
    /// Celsius "Full Enclosure" tutorial: walls (Front at +Z), a perforated
    /// vent and a fan with its curve in cfm, a chassis bay whose slot holds
    /// a server rotated by 90°, a chip socket with a centred chip, its heat
    /// sink with a cutout and a TIM, a DIMM socket, a drive bay, a power
    /// supply.
    #[test]
    fn rack_objects() {
        let side = |t: f64, extra: &str| {
            format!("<Chassis_Side><Installed>Yes</Installed><Construction><Thickness>{t} mm</Thickness><MaterialLink>10-1</MaterialLink></Construction>{extra}</Chassis_Side>")
        };
        let vent = "<Vent_Openings><Vent_Opening><Installed>Yes</Installed><Identification><Name>Grille</Name></Identification>\
            <Geometry><Geometry_Definition>Rectangular</Geometry_Definition><Width>50 mm</Width><Length>40 mm</Length></Geometry>\
            <Placement><Offset_1>10 mm</Offset_1><Offset_2>20 mm</Offset_2></Placement>\
            <Construction><Specification>Perforated</Specification><Perforation_Details><Hole_Size>3 mm</Hole_Size><Open_Area>50 %</Open_Area></Perforation_Details></Construction>\
            </Vent_Opening></Vent_Openings>";
        let fan = "<Fans><Fan><Installed>Yes</Installed><Identification><Reference_Designator>F1</Reference_Designator></Identification>\
            <Geometry visibility=\"Locked\"><Overall_Size>40 mm</Overall_Size><Fan_Diameter>40 mm</Fan_Diameter><Hub_Diameter>20 mm</Hub_Diameter><Thickness>10 mm</Thickness></Geometry>\
            <Placement><Mount_Location>Inside</Mount_Location><Offset_1>100 mm</Offset_1><Offset_2>50 mm</Offset_2></Placement>\
            <Status><Fan_Status>On</Fan_Status></Status>\
            <Cooling><Flow_Direction>Out</Flow_Direction><Flow_Rate_Option>Curve</Flow_Rate_Option><Fan_Curve>0,100, 20,0 &lt;cfm,Pa&gt;</Fan_Curve>\
            <Rated_Speed>1000 rpm</Rated_Speed><Operating_Speed>Fixed</Operating_Speed><Fixed_Speed>1000 rpm</Fixed_Speed>\
            <Heat_Dissipation><Heat_Specification>Fixed Heat Dissipation</Heat_Specification><Heat_Dissipated>2 W</Heat_Dissipated></Heat_Dissipation></Cooling>\
            </Fan></Fans>";
        let component = "<Component><Installed>Yes</Installed><Identification><Reference_Designator>U1</Reference_Designator></Identification>\
            <Geometry><Shape>Rectangle</Shape><Height>1 mm</Height><Width>10 mm</Width><Depth>10 mm</Depth></Geometry>\
            <Placement><Origin_Point>Low Centre</Origin_Point><Board_Side>Top</Board_Side><X_Location>0 mm</X_Location><Y_Location>0 mm</Y_Location><Z_Location>0 mm</Z_Location></Placement>\
            <Construction><Modelling_Level>Simplified</Modelling_Level><MaterialLink>13-1</MaterialLink></Construction>\
            <Power><Thermal_Design_Power>5 W</Thermal_Design_Power></Power>\
            <TIMs><TIM><Installed>Yes</Installed><Identification><Reference_Designator>TIM1</Reference_Designator></Identification><Construction><Solid-Solid_Resistance>0.1 C/W</Solid-Solid_Resistance><Model_Thickness>No</Model_Thickness></Construction></TIM></TIMs>\
            <HeatsinkLink><Heatsink><Installed>Yes</Installed><Identification><Reference_Designator>HS1</Reference_Designator></Identification>\
            <Geometry><Type>Extruded</Type><Base_Width>20 mm</Base_Width><Base_Depth>20 mm</Base_Depth><Base_Thickness>2 mm</Base_Thickness><Fin_Thickness>1 mm</Fin_Thickness><Fin_Height>5 mm</Fin_Height><Number_of_Fins>4</Number_of_Fins><Fin_Depth>12 mm</Fin_Depth></Geometry>\
            <Placement><Origin_Point>Centre</Origin_Point><Board_Side>Top</Board_Side><X_Offset>0 mm</X_Offset><Y_Offset>0 mm</Y_Offset><Z_Offset>0 mm</Z_Offset></Placement>\
            <Construction><MaterialLink>10-1</MaterialLink></Construction>\
            <Cutouts><Heatsink_Cutout><Installed>Yes</Installed><Geometry><Shape>Rectangle</Shape><Width>20 mm</Width><Depth>2 mm</Depth><Cutout_Heatsink_Base>No</Cutout_Heatsink_Base></Geometry><Placement><X_Offset>0 mm</X_Offset><Z_Offset>9 mm</Z_Offset></Placement></Heatsink_Cutout></Cutouts>\
            </Heatsink></HeatsinkLink></Component>";
        let pcb = |rd: &str, w: f64, d: f64, at: &str, power: f64, body: &str| {
            format!("<PCB><Installed>Yes</Installed><Identification><Reference_Designator>{rd}</Reference_Designator></Identification>\
                <Geometry><Shape>Rectangle</Shape><Thickness>1.6 mm</Thickness><Width>{w} mm</Width><Depth>{d} mm</Depth></Geometry>\
                <Placement><Origin_Point>Low Corner</Origin_Point><Board_Side>Top</Board_Side>{at}</Placement>\
                <Construction><Conductor_Material>11-1</Conductor_Material><Dielectric_Material>12-1</Dielectric_Material><Modelling_Level>Simplified Average</Modelling_Level><Number_Of_Conductor_Layers>2</Number_Of_Conductor_Layers></Construction>\
                <Power><Top_Distributed_Power>{power} W</Top_Distributed_Power><Bottom_Distributed_Power>0 W</Bottom_Distributed_Power></Power>{body}</PCB>")
        };
        let dimm = pcb(
            "DIMM1",
            40.0,
            15.0,
            "<Centre_Offset>0 mm</Centre_Offset>",
            1.0,
            "",
        );
        let board = pcb(
            "MB",
            50.0,
            150.0,
            "<X_Location>2 mm</X_Location><Y_Location>5 mm</Y_Location><Z_Location>10 mm</Z_Location>",
            0.0,
            &format!(
                "<Chip_Sockets><Chip_Socket><Installed>Yes</Installed><Identification><Reference_Designator>J1</Reference_Designator></Identification>\
                 <Geometry><Height>2 mm</Height><Width>20 mm</Width><Depth>20 mm</Depth></Geometry>\
                 <Placement><Board_Side>Top</Board_Side><X_Location>10 mm</X_Location><Z_Location>50 mm</Z_Location><Orientation>0 degrees</Orientation></Placement>\
                 <Construction><MaterialLink>13-1</MaterialLink></Construction><ComponentLink>{component}</ComponentLink></Chip_Socket></Chip_Sockets>\
                 <Daughter_Board_Sockets><Daughter_Board_Socket><Installed>Yes</Installed><Identification><Reference_Designator>J2</Reference_Designator></Identification>\
                 <Geometry><Height>5 mm</Height><Width>5 mm</Width><Length>40 mm</Length><Angle>0 degrees</Angle></Geometry>\
                 <Placement><Board_Side>Top</Board_Side><X_Location>40 mm</X_Location><Z_Location>20 mm</Z_Location><Orientation>0 degrees</Orientation></Placement>\
                 <Construction><MaterialLink>13-1</MaterialLink></Construction><Daughter_Board>{dimm}</Daughter_Board></Daughter_Board_Socket></Daughter_Board_Sockets>"
            ),
        );
        let server = format!(
            "<Chassis><Installed>Yes</Installed><Identification><Reference_Designator>SRV</Reference_Designator></Identification>\
             <Geometry><Height>50 mm</Height><Width>60 mm</Width><Depth>200 mm</Depth></Geometry>\
             <Placement><X_Location>0 mm</X_Location><Y_Location>0 mm</Y_Location><Z_Location>0 mm</Z_Location></Placement>\
             <Left_Side>{}</Left_Side><PCBs>{board}</PCBs>\
             <Drive_Bays><Drive_Bay><Installed>Yes</Installed><Identification><Reference_Designator>DB1</Reference_Designator></Identification>\
             <Geometry><Height>15 mm</Height><Width>30 mm</Width><Depth>40 mm</Depth></Geometry>\
             <Placement><X_Location>5 mm</X_Location><Y_Location>20 mm</Y_Location><Z_Location>150 mm</Z_Location></Placement>\
             <DriveLink><Drive><Installed>Yes</Installed><Identification><Reference_Designator>D1</Reference_Designator></Identification>\
             <Geometry><Height>10 mm</Height><Width>20 mm</Width><Depth>30 mm</Depth></Geometry>\
             <Mount_Location><Depth_Offset>0 mm</Depth_Offset><Sideways_Mounting_Position>Centre</Sideways_Mounting_Position><Vertical_Mounting_Position>Centre</Vertical_Mounting_Position></Mount_Location>\
             <Construction><Mounting_Rails>Yes</Mounting_Rails><Mounting_Rail_Height>6 mm</Mounting_Rail_Height><Drive_Material>10-1</Drive_Material></Construction>\
             <Power><Thermal_Design_Power>6 W</Thermal_Design_Power></Power></Drive></DriveLink></Drive_Bay></Drive_Bays></Chassis>",
            side(1.0, "")
        );
        let bay_side = "<Chassis_Bay_Side><Installed>Yes</Installed><Construction><Thickness>1 mm</Thickness><MaterialLink>10-1</MaterialLink></Construction></Chassis_Bay_Side>";
        let bay = format!(
            "<Chassis_Bays><Chassis_Bay><Installed>Yes</Installed><Identification><Reference_Designator>Zone</Reference_Designator></Identification>\
             <Geometry><Height>60 mm</Height><Width>100 mm</Width><Depth>200 mm</Depth></Geometry>\
             <Placement><X_Location>10 mm</X_Location><Y_Location>10 mm</Y_Location><Z_Location>100 mm</Z_Location></Placement>\
             <Construction><Slot_Definition><Slot_Size>50 mm</Slot_Size><First_Slot_Offset>0 mm</First_Slot_Offset><Slot_Direction>Left to Right</Slot_Direction><SlotsLink>\
             <Chassis_Slot><Construction><Chassis_Alignment>Rear</Chassis_Alignment><Orientation_Angle>90</Orientation_Angle></Construction><Index>2</Index><ChassisLink>{server}</ChassisLink></Chassis_Slot>\
             </SlotsLink></Slot_Definition></Construction>\
             <Left_Side>{bay_side}</Left_Side><Right_Side>{bay_side}</Right_Side><Top>{bay_side}</Top><Bottom>{bay_side}</Bottom></Chassis_Bay></Chassis_Bays>"
        );
        let psu = "<Power_Supplies><Power_Supply><Installed>Yes</Installed><Identification><Reference_Designator>PS1</Reference_Designator></Identification>\
            <Geometry><Height>30 mm</Height><Width>60 mm</Width><Depth>100 mm</Depth></Geometry>\
            <Placement><X_Location>120 mm</X_Location><Y_Location>10 mm</Y_Location><Z_Location>150 mm</Z_Location></Placement>\
            <Construction><Internal_Blockage_Percentage>40 %</Internal_Blockage_Percentage><Thickness>1 mm</Thickness><MaterialLink>10-1</MaterialLink></Construction>\
            <Power><Calculated_Power>20 W</Calculated_Power></Power>\
            <Vent_Openings><Vent_Opening><Installed>Yes</Installed><Identification><Name>Exhaust</Name></Identification>\
            <Geometry><Geometry_Definition>Rectangular</Geometry_Definition><Width>30 mm</Width><Length>10 mm</Length></Geometry>\
            <Placement><Side_Of_Supply>Rear</Side_Of_Supply><Offset_1>5 mm</Offset_1><Offset_2>5 mm</Offset_2></Placement>\
            <Construction><Specification>Perforated</Specification><Perforation_Details><Hole_Size>2 mm</Hole_Size><Open_Area>60 %</Open_Area></Perforation_Details></Construction></Vent_Opening></Vent_Openings>\
            <Fans><Fan><Installed>Yes</Installed><Identification><Reference_Designator>FP</Reference_Designator></Identification>\
            <Geometry><Overall_Size>20 mm</Overall_Size><Fan_Diameter>20 mm</Fan_Diameter><Thickness>5 mm</Thickness></Geometry>\
            <Placement><Side>Front</Side><Offset_1>30 mm</Offset_1><Offset_2>15 mm</Offset_2></Placement>\
            <Cooling><Flow_Direction>In</Flow_Direction><Flow_Rate_Option>Fixed Flow</Flow_Rate_Option><Fan_Flow_Rate>0.002 m^3/s</Fan_Flow_Rate></Cooling></Fan></Fans>\
            </Power_Supply></Power_Supplies>";
        let start = MODEL.find("<Geometry><Height>50 mm</Height><Width>200 mm</Width><Depth>150 mm</Depth></Geometry>").unwrap();
        let end = MODEL.find("</Chassis></ChassisLink>").unwrap();
        let rack = format!(
            "<Geometry><Height>100 mm</Height><Width>200 mm</Width><Depth>300 mm</Depth></Geometry>\
             <Placement><X_Location>0 mm</X_Location><Y_Location>0 mm</Y_Location><Z_Location>0 mm</Z_Location></Placement>\
             <Front>{}</Front><Rear>{}</Rear><Bottom>{}</Bottom>{bay}{psu}",
            side(1.0, vent),
            side(1.0, fan),
            side(1.0, "")
        );
        let model = format!("{}{}{}", &MODEL[..start], rack, &MODEL[end..]);
        let m = read(&container(&model)).unwrap();
        let c = &m.case;
        eprintln!("{:?}", m.warnings);
        let close = |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-6);
        let blk = |n: &str| {
            c.blocks.iter().find(|b| b.name == n).unwrap_or_else(|| {
                panic!(
                    "{n} missing: {:?}",
                    c.blocks.iter().map(|b| &b.name).collect::<Vec<_>>()
                )
            })
        };
        let boxed = |n: &str, lo: [f64; 3], hi: [f64; 3]| {
            let b = blk(n);
            assert!(
                close(b.min, lo) && close(b.max, hi),
                "{n}: {:?} {:?}",
                b.min,
                b.max
            );
            b
        };
        // FerroTherm x = X, y = 300 − Z, z = Y; Front (Z 299…300) at y 0…1
        let front = boxed("Box/front", [0.0, 0.0, 1.0], [200.0, 1.0, 100.0]);
        assert_eq!(front.outline.as_ref().unwrap().len(), 2, "vent hole");
        boxed("Box/rear", [0.0, 299.0, 1.0], [200.0, 300.0, 100.0]);
        // vent: opening on the front face with the perforated-plate loss
        let v = c
            .boundaries
            .iter()
            .find(|b| b.name == "Box/front/Grille")
            .unwrap();
        assert_eq!(v.face, Face::Ymin);
        let r = v.region.as_ref().unwrap();
        assert_eq!((r.min, r.max), ([10.0, 20.0], [60.0, 60.0]));
        let l = v.loss.as_ref().unwrap();
        assert!((l.zeta - (0.707 * 0.5f64.sqrt() + 0.5).powi(2) / 0.25).abs() < 1e-9);
        // fan: centre plane at the inner face of the housing, blowing out
        let f = c.fans.iter().find(|f| f.name == "Box/rear/F1").unwrap();
        assert!(close(f.center, [100.0, 290.0, 50.0]), "{:?}", f.center);
        assert_eq!(f.direction, "+y");
        assert!(
            (f.curve[1][0] - 20.0 * 4.719_474_432e-4).abs() < 1e-12,
            "cfm"
        );
        let hub = blk("Box/rear/F1/hub");
        assert_eq!(hub.power_w, 2.0);
        assert!((hub.min[1] - 290.0).abs() < 1e-9 && (hub.max[1] - 299.0).abs() < 1e-9);
        assert!(c
            .boundaries
            .iter()
            .any(|b| b.name == "Box/rear/F1/opening" && b.face == Face::Ymax));
        // server in slot 2 of the bay (X 60…110), rotated +90° about Z:
        // local x → +Y, local y → −X: Celsius X = 110 − y, Y = 10 + x, Z = 100 + z
        boxed(
            "Zone/S2/SRV/left_side",
            [60.0, 0.0, 10.0],
            [110.0, 200.0, 11.0],
        );
        boxed("Zone/S2/MB", [103.4, 40.0, 12.0], [105.0, 190.0, 62.0]);
        boxed("Zone/S2/J1", [101.4, 120.0, 22.0], [103.4, 140.0, 42.0]);
        // chip centred on its socket, 5 W
        let u1 = boxed("Zone/S2/U1", [100.4, 125.0, 27.0], [101.4, 135.0, 37.0]);
        assert_eq!(u1.power_w, 5.0);
        // heat sink centred on the chip, fins over the whole base depth
        // (not Fin_Depth) split by the cutout at 9…11 mm
        boxed(
            "Zone/S2/HS1/base",
            [98.4, 120.0, 22.0],
            [100.4, 140.0, 42.0],
        );
        boxed(
            "Zone/S2/HS1/fin1.1",
            [93.4, 131.0, 22.0],
            [98.4, 140.0, 23.0],
        );
        boxed(
            "Zone/S2/HS1/fin1.2",
            [93.4, 120.0, 22.0],
            [98.4, 129.0, 23.0],
        );
        assert!(m
            .warnings
            .iter()
            .any(|w| w.contains("TIM1") && w.contains("0.50 K")));
        // DIMM standing on the board along the socket length, centred on its width
        boxed("Zone/S2/J2", [98.4, 130.0, 52.0], [103.4, 170.0, 57.0]);
        let d = boxed(
            "Zone/S2/J2/DIMM1",
            [88.4, 130.0, 53.7],
            [103.4, 170.0, 55.3],
        );
        assert_eq!(d.pcb.as_ref().unwrap().normal, "z");
        assert_eq!(d.power_w, 1.0);
        // drive centred in its bay, rails filling the sides
        let dr = boxed("Zone/S2/DB1/D1", [77.5, 20.0, 20.0], [87.5, 50.0, 40.0]);
        assert_eq!(dr.power_w, 6.0);
        boxed(
            "Zone/S2/DB1/D1/rail1",
            [79.5, 20.0, 15.0],
            [85.5, 50.0, 20.0],
        );
        // power supply: rear vent → planar resistance, front fan blowing in
        let ex = c
            .resistances
            .iter()
            .find(|r| r.name == "PS1/rear/Exhaust")
            .unwrap();
        assert!(close(ex.center, [140.0, 150.0, 20.0]), "{:?}", ex.center);
        assert_eq!((ex.axis.as_str(), ex.size), ("y", [30.0, 10.0]));
        let fp = c.fans.iter().find(|f| f.name == "PS1/front/FP").unwrap();
        assert!(close(fp.center, [150.0, 55.0, 25.0]), "{:?}", fp.center);
        assert_eq!((fp.direction.as_str(), fp.flow_m3_s), ("+y", Some(0.002)));
        let plates: f64 = c
            .blocks
            .iter()
            .filter(|b| b.name.starts_with("PS1/internals"))
            .map(|b| b.power_w)
            .sum();
        assert!((plates - 20.0).abs() < 1e-9);
        // the whole case reads back
        Case::from_toml(&c.to_toml().unwrap()).unwrap();
    }

    /// U1 turned into a "Detailed" package with a substrate and a 1.5 W die.
    fn detailed_model(sub_at: &str, die_at: &str) -> String {
        MODEL.replace(
            "<Construction><Modelling_Level>2R</Modelling_Level><Junction-to-Case_Thermal_Resistance>5 K/W</Junction-to-Case_Thermal_Resistance><Junction-to-Board_Thermal_Resistance>10 C/W</Junction-to-Board_Thermal_Resistance></Construction>",
            &format!(
                "<Construction><Modelling_Level>Detailed</Modelling_Level></Construction>\
                 <Solid_Obstructions>\
                 <Solid_Obstruction><Identification><Name>substrate</Name></Identification><Geometry><Shape>Cuboid</Shape><Width>10 mm</Width><Height>0.4 mm</Height><Depth>6 mm</Depth></Geometry>\
                 <Placement><Origin_Point>Low Corner</Origin_Point>{sub_at}</Placement>\
                 <Cooling><MaterialLink>13-1</MaterialLink></Cooling></Solid_Obstruction>\
                 <Solid_Obstruction><Identification><Name>die</Name></Identification><Geometry><Shape>Cuboid</Shape><Width>4 mm</Width><Height>0.5 mm</Height><Depth>2 mm</Depth></Geometry>\
                 <Placement><Origin_Point>Low Corner</Origin_Point>{die_at}</Placement>\
                 <Cooling><Heat_Dissipated>1.5 W</Heat_Dissipated></Cooling></Solid_Obstruction>\
                 </Solid_Obstructions>"
            ),
        )
    }

    /// Detailed package resolved: body, substrate and die as separate
    /// blocks, the die carrying its own power, whichever frame its location
    /// is written in (package corner or board frame).
    #[test]
    fn detailed_package_resolved() {
        let loc = |x: f64, y: f64, z: f64| {
            format!("<X_Location>{x} mm</X_Location><Y_Location>{y} mm</Y_Location><Z_Location>{z} mm</Z_Location>")
        };
        // package low corner in the board frame: (50, 1.6, 40)
        let cases = [
            (loc(0.0, 0.0, 0.0), loc(3.0, 0.4, 2.0), "package corner"),
            (loc(50.0, 1.6, 40.0), loc(53.0, 2.0, 42.0), "board frame"),
        ];
        for (sub_at, die_at, conv) in cases {
            let m = read(&container(&detailed_model(&sub_at, &die_at))).unwrap();
            let c = &m.case;
            assert!(c.components.iter().all(|x| x.name != "U1"));
            let body = c.blocks.iter().find(|b| b.name == "U1").unwrap();
            let sub = c.blocks.iter().find(|b| b.name == "U1/substrate").unwrap();
            let die = c.blocks.iter().find(|b| b.name == "U1/die").unwrap();
            assert!(
                m.warnings
                    .iter()
                    .any(|w| w.contains("resolved") && w.contains(conv)),
                "{:?}",
                m.warnings
            );
            // body: x 60..70, y 99..105, z 21.6..23.6 (as the 2R part)
            let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
            assert!(close(body.min[0], 60.0) && close(body.max[2], 23.6));
            assert_eq!(body.power_w, 2.0);
            // substrate on the board face
            assert!(close(sub.min[2], 21.6) && close(sub.max[2], 22.0));
            // die: board-frame x 53..57, height 2.0..2.5 above the board face
            // (+1.6 board), depth 42..44 → y = 150 − (5 + 44 … 5 + 42)
            assert!(
                close(die.min[0], 63.0) && close(die.max[0], 67.0),
                "{:?}",
                die
            );
            assert!(close(die.min[1], 101.0) && close(die.max[1], 103.0));
            assert!(close(die.min[2], 22.0) && close(die.max[2], 22.5));
            assert_eq!(die.power_w, 1.5);
            // later blocks own the cells: body painted first
            let ib = c.blocks.iter().position(|b| b.name == "U1").unwrap();
            let id = c.blocks.iter().position(|b| b.name == "U1/die").unwrap();
            assert!(ib < id);
        }
    }

    /// The resolved die runs hotter than the package body around it
    /// (conduction only, so the check is quick).
    #[test]
    fn detailed_package_solves() {
        let at = |x: f64, y: f64, z: f64| {
            format!("<X_Location>{x} mm</X_Location><Y_Location>{y} mm</Y_Location><Z_Location>{z} mm</Z_Location>")
        };
        let m = read(&container(&detailed_model(
            &at(0.0, 0.0, 0.0),
            &at(3.0, 0.4, 2.0),
        )))
        .unwrap();
        let mut case = m.case.clone();
        case.flow = None;
        case.grid.max_cell_mm = ferrotherm_core::case::MaybeVec3::Scalar(5.0);
        let lib = ferrotherm_core::model::load_library(&case, std::path::Path::new(".")).unwrap();
        let sim = ferrotherm_core::Simulation::with_library(case, lib).unwrap();
        let out = sim.run(|_| {}).unwrap();
        let get = |n: &str| {
            out.report
                .blocks
                .iter()
                .find(|b| b.name == n)
                .unwrap()
                .clone()
        };
        let (die, body) = (get("U1/die"), get("U1"));
        eprintln!(
            "DETAILED: die {:.2} °C, body max {:.2} °C",
            die.t_max_c, body.t_max_c
        );
        assert!(die.t_max_c > body.t_avg_c);
        assert!(out.report.energy_imbalance_pct.abs() < 0.01);
    }

    /// U1 as the FPBGA of the reference model (Celsius 2024.36): parts in a
    /// subcomponent group, a 2-layer substrate, cylindrical solder balls
    /// whose Width/Depth are unused 500 mm defaults (the size is the
    /// Diameter), wire bonds overlapping the die.
    #[test]
    fn detailed_package_real_structure() {
        let obs = |name: &str, typ: &str, shape: &str, size: &str, at: &str, extra: &str| {
            format!(
                "<Solid_Obstruction><Installed>Yes</Installed><Identification><Name>{name}</Name><Type>{typ}</Type></Identification>\
                 <Geometry><Shape>{shape}</Shape>{size}</Geometry>\
                 <Placement><Origin_Point>Low Corner</Origin_Point>{at}</Placement>{extra}</Solid_Obstruction>"
            )
        };
        let loc = |x: f64, y: f64, z: f64| {
            format!("<X_Location>{x} mm</X_Location><Y_Location>{y} mm</Y_Location><Z_Location>{z} mm</Z_Location>")
        };
        let cub = |w: f64, h: f64, d: f64| {
            format!("<Height>{h} mm</Height><Width>{w} mm</Width><Depth>{d} mm</Depth>")
        };
        let mut o = String::new();
        o += &obs(
            "Encapsulant",
            "Encapsulant",
            "Cuboid",
            &cub(10.0, 1.0, 6.0),
            &loc(0.0, 1.0, 0.0),
            "",
        );
        o += &obs(
            "Die Attach",
            "Die Attach",
            "Cuboid",
            &cub(2.0, 0.04, 2.0),
            &loc(4.0, 1.0, 2.0),
            "",
        );
        let die_size = cub(2.0, 0.3, 2.0)
            + "<Grid><Minimum_Number_Of_Cells><Width_Direction>5</Width_Direction><Depth_Direction>4</Depth_Direction></Minimum_Number_Of_Cells></Grid>";
        o += &obs("Die", "Die", "Cuboid", &die_size, &loc(4.0, 1.04, 2.0),
            "<Cooling><Heat_Dissipated>1 W</Heat_Dissipated><MaterialLink>13-1</MaterialLink></Cooling>");
        o += &obs(
            "Wire Bonds",
            "Other",
            "Cuboid",
            &cub(4.0, 0.065, 4.0),
            &loc(3.0, 1.0, 1.0),
            "",
        );
        for i in 0..3 {
            for j in 0..3 {
                o += &obs(
                    &format!("Ball {i},{j}"),
                    "Solder Pad",
                    "Cylinder",
                    "<Height>0.5 mm</Height><Width>500 mm</Width><Depth>500 mm</Depth><Diameter>0.4 mm</Diameter>",
                    &loc(1.0 + 3.0 * i as f64, 0.0, 1.0 + 2.0 * j as f64),
                    "",
                );
            }
        }
        let sub = format!(
            "<Substrate><Installed>Yes</Installed><Identification><Reference_Designator>Substrate2</Reference_Designator></Identification>\
             <Geometry><Shape>Rectangle</Shape><Thickness>0.5 mm</Thickness><Width>10 mm</Width><Depth>6 mm</Depth></Geometry>\
             <Placement><Origin_Point>Low Corner</Origin_Point>{}</Placement>\
             <Construction><Conductor_Material>11-1</Conductor_Material><Dielectric_Material>12-1</Dielectric_Material>\
             <Modelling_Level>Explicit Layers</Modelling_Level><Number_Of_Conductor_Layers>2</Number_Of_Conductor_Layers>\
             <Conductor_Layers><Conductor_Layer><Installed>Yes</Installed><Construction><Conductor_Thickness>0.035 mm</Conductor_Thickness><Conductor_Planar_Percentage>25 %</Conductor_Planar_Percentage></Construction></Conductor_Layer>\
             <Conductor_Layer><Installed>Yes</Installed><Construction><Conductor_Thickness>0.035 mm</Conductor_Thickness><Conductor_Planar_Percentage>25 %</Conductor_Planar_Percentage></Construction></Conductor_Layer></Conductor_Layers>\
             </Construction></Substrate>",
            loc(0.0, 0.5, 0.0)
        );
        let model = MODEL.replace(
            "<Construction><Modelling_Level>2R</Modelling_Level><Junction-to-Case_Thermal_Resistance>5 K/W</Junction-to-Case_Thermal_Resistance><Junction-to-Board_Thermal_Resistance>10 C/W</Junction-to-Board_Thermal_Resistance></Construction>",
            &format!(
                "<Construction><Modelling_Level>Detailed</Modelling_Level></Construction>\
                 <SubcomponentGroups><Subcomponent_Group><Installed>Yes</Installed><Obstructions>{o}{sub}</Obstructions></Subcomponent_Group></SubcomponentGroups>"
            ),
        );
        let m = read(&container(&model)).unwrap();
        let c = &m.case;
        assert!(
            m.warnings
                .iter()
                .any(|w| w.contains("resolved") && w.contains("package corner")),
            "{:?}",
            m.warnings
        );
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        let pos = |n: &str| {
            c.blocks
                .iter()
                .position(|b| b.name == n)
                .unwrap_or_else(|| panic!("{n}"))
        };
        let blk = |n: &str| &c.blocks[pos(n)];
        // board top at z 21.6; balls 0..0.5, substrate 0.5..1.0, die 1.04..1.34
        let balls = blk("U1/Solder Pad ×9");
        assert!(close(balls.min[2], 21.6) && close(balls.max[2], 22.1));
        // array footprint: x 1 .. 7.4, depth 1 .. 5.4 (balls of Ø 0.4)
        assert!(close(balls.max[0] - balls.min[0], 6.4) && close(balls.max[1] - balls.min[1], 4.4));
        let mat = c
            .materials
            .iter()
            .find(|x| Some(&x.id) == balls.material.as_ref())
            .unwrap();
        let f = 9.0 * std::f64::consts::PI * 0.04 / (6.4 * 4.4);
        let k = mat.k.as_vec();
        assert!(k[2] > 10.0 * k[0], "{k:?}");
        assert!(mat.notes.contains(&format!("{f:.3}")), "{}", mat.notes);
        let sub = blk("U1/Substrate2");
        assert!(close(sub.min[2], 22.1) && close(sub.max[2], 22.6));
        let st = sub.pcb.as_ref().unwrap();
        assert_eq!(st.normal, "z");
        assert_eq!(st.copper_layers.len(), 2);
        let die = blk("U1/Die");
        assert!(close(die.min[2], 22.64) && close(die.max[2], 22.94));
        assert_eq!(die.power_w, 1.0);
        // Celsius minimum cells: width → x, depth → FerroTherm y (board normal z)
        assert_eq!(die.min_cells, Some([5, 4, 0]));
        assert_eq!(blk("U1/Solder Pad ×9").min_cells, None);
        // paint order: encapsulant first, die after wire bonds and die attach
        assert!(pos("U1/Encapsulant") < pos("U1/Wire Bonds"));
        assert!(pos("U1/Wire Bonds") < pos("U1/Die Attach"));
        assert!(pos("U1/Die Attach") < pos("U1/Die"));
    }

    /// Internal parts that fit no frame: one block with the total power.
    #[test]
    fn detailed_package_fallback() {
        let at = "<X_Location>0 mm</X_Location><Y_Location>0 mm</Y_Location><Z_Location>0 mm</Z_Location>";
        let far = "<X_Location>500 mm</X_Location><Y_Location>0 mm</Y_Location><Z_Location>0 mm</Z_Location>";
        let m = read(&container(&detailed_model(at, far))).unwrap();
        let u1 = m.case.blocks.iter().find(|b| b.name == "U1").unwrap();
        assert!((u1.power_w - 3.5).abs() < 1e-12);
        assert!(m.case.blocks.iter().all(|b| b.name != "U1/die"));
        assert!(m.warnings.iter().any(|w| w.contains("lumped")));
    }

    /// Polygon board: the outline is kept (mapped to the FerroTherm in-plane
    /// axes) and the box is its bounding rectangle.
    #[test]
    fn polygon_board() {
        let model = MODEL.replace(
            "<Geometry><Shape>Rectangle</Shape><Thickness>1.6 mm</Thickness><Width>100 mm</Width><Depth>80 mm</Depth></Geometry>",
            "<Geometry><Shape>Polygon</Shape><Thickness>1.6 mm</Thickness><Width>1 mm</Width><Depth>1 mm</Depth><Outline>0,0,100,0,100,40,50,40,50,80,0,80</Outline></Geometry>",
        );
        let m = read(&container(&model)).unwrap();
        let b = m.case.blocks.iter().find(|b| b.name == "BOARD").unwrap();
        // box as the rectangular board of the synthetic model
        assert_eq!(b.min, [10.0, 65.0, 20.0]);
        assert_eq!(b.max[0], 110.0);
        assert_eq!(b.max[1], 145.0);
        assert_eq!(b.outline_axis.as_deref(), Some("z"));
        // local (x, z) → FerroTherm (10 + x, 150 − (5 + z))
        assert!(b.outline_contains(10.0 + 75.0, 145.0 - 20.0));
        assert!(!b.outline_contains(10.0 + 75.0, 145.0 - 60.0), "notch");
        assert!(b.outline_contains(10.0 + 25.0, 145.0 - 60.0));
        let back = Case::from_toml(&m.case.to_toml().unwrap()).unwrap();
        assert!(back.blocks.iter().any(|x| x.outline.is_some()));
    }

    /// A vertical board as in the reference model (Celsius 2024.36, checked
    /// against its STEP export): "Ordered" XZY rotation of +90°, a 2R part on
    /// the top side and a block part.
    #[test]
    fn rotated_board() {
        let model = MODEL
            .replace(
                "<Geometry><Height>50 mm</Height><Width>200 mm</Width><Depth>150 mm</Depth></Geometry>",
                "<Geometry><Height>200 mm</Height><Width>207 mm</Width><Depth>200 mm</Depth></Geometry>",
            )
            .replace(
                "<X_Location>10 mm</X_Location><Y_Location>20 mm</Y_Location><Z_Location>5 mm</Z_Location><Height_From_Stand_Offs>No</Height_From_Stand_Offs>",
                "<X_Location>26 mm</X_Location><Y_Location>122 mm</Y_Location><Z_Location>29 mm</Z_Location><Height_From_Stand_Offs>No</Height_From_Stand_Offs><Orientation><Rotation_Mechanism>Ordered</Rotation_Mechanism><Rotation_Order>XZY</Rotation_Order><Angle_1>90 degrees</Angle_1><Angle_2>0 degrees</Angle_2><Angle_3>0 degrees</Angle_3></Orientation>",
            )
            .replace(
                "<Width>100 mm</Width><Depth>80 mm</Depth>",
                "<Width>160 mm</Width><Depth>100 mm</Depth>",
            )
            // U1 as FPGA U1 of the reference: 35 × 35 × 3 at (62.5, 32.5)
            .replace(
                "<Height>2 mm</Height><Width>10 mm</Width><Depth>6 mm</Depth>",
                "<Height>3 mm</Height><Width>35 mm</Width><Depth>35 mm</Depth>",
            )
            .replace("<X_Location>50 mm</X_Location>", "<X_Location>62.5 mm</X_Location>")
            .replace(
                "<Y_Location>0 mm</Y_Location><Z_Location>40 mm</Z_Location>",
                "<Y_Location>0 mm</Y_Location><Z_Location>32.5 mm</Z_Location>",
            );
        let m = read(&container(&model)).unwrap();
        let c = &m.case;
        assert!(
            !m.warnings
                .iter()
                .any(|w| w.contains("outside") || w.contains("several axes")),
            "{:?}",
            m.warnings
        );
        let d = 200.0;
        // Celsius box → FerroTherm box (x = X, y = D − Z, z = Y)
        let ft = |lo: V3, hi: V3| ([lo[0], d - hi[2], lo[1]], [hi[0], d - lo[2], hi[1]]);
        let close = |a: V3, b: V3| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-9);
        // board: X 26..186, Y 22..122, Z 29..30.6 (STEP export)
        let b = c.blocks.iter().find(|b| b.name == "BOARD").unwrap();
        let (lo, hi) = ft([26.0, 22.0, 29.0], [186.0, 122.0, 30.6]);
        assert!(
            close(b.min, lo) && close(b.max, hi),
            "{:?} {:?}",
            b.min,
            b.max
        );
        assert_eq!(b.pcb.as_ref().unwrap().normal, "y");
        // U1: X 88.5..123.5, Y 54.5..89.5, Z 30.6..33.6 (STEP export)
        let u1 = c.components.iter().find(|x| x.name == "U1").unwrap();
        let (lo, hi) = ft([88.5, 54.5, 30.6], [123.5, 89.5, 33.6]);
        // size is [L, W, H] with H along the mount axis (y): extents x, z, y
        let s = u1.size.unwrap();
        let top = [
            u1.position[0] + s[0],
            u1.position[1] + s[2],
            u1.position[2] + s[1],
        ];
        assert!(
            close(u1.position, lo) && close(top, hi),
            "{:?} {:?}",
            u1.position,
            s
        );
        // the board normal is FerroTherm y and the case looks towards −y
        assert_eq!(u1.mount_axis, "y");
        assert_eq!(u1.case_face, Some(CaseFace::Bottom));
        // Q1 on the bottom side sits on the other face of the board (Z < 29)
        let q1 = c.blocks.iter().find(|x| x.name == "Q1").unwrap();
        assert!((q1.min[1] - (d - 29.0)).abs() < 1e-9 && (q1.max[1] - (d - 26.0)).abs() < 1e-9);
    }

    #[test]
    fn rotations() {
        let r = rot_about([1.0, 0.0, 0.0], 90.0);
        assert_eq!(mv3(&r, [0.0, 0.0, 1.0]), [0.0, -1.0, 0.0]);
        assert_eq!(mv3(&r, [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]);
        let r = rot_about([0.0, 1.0, 0.0], 180.0);
        assert_eq!(mv3(&r, [1.0, 0.0, 0.0]), [-1.0, 0.0, 0.0]);
        assert!(axis_aligned(&r));
        assert!(!axis_aligned(&rot_about([0.0, 0.0, 1.0], 30.0)));
        let p = Pose {
            r: rot_about([1.0, 0.0, 0.0], 90.0),
            t: [26.0, 122.0, 29.0],
        };
        assert_eq!(p.axis(1), (2, 1.0));
        assert_eq!(p.axis(2), (1, -1.0));
        assert_eq!(ft_axis(2, 1.0), (1, -1.0));
    }
}
