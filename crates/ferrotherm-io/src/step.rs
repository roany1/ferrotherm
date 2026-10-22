// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! STEP (ISO 10303-21, AP203/AP214/AP242) import.
//!
//! The product structure is read here: products, shape representations,
//! representation relationships, assembly occurrences
//! (NEXT_ASSEMBLY_USAGE_OCCURRENCE + CONTEXT_DEPENDENT_SHAPE_REPRESENTATION
//! with ITEM_DEFINED_TRANSFORMATION), MAPPED_ITEM instances and the length
//! unit. With the default `occt` feature the B-rep is imported and
//! tessellated by OpenCascade (see [`crate::occt`]); the built-in reader is the
//! fallback and gives each solid the bounding box of its vertices. Bodies are converted to FerroTherm blocks either as bounding
//! boxes or as voxels merged into boxes.

use std::collections::{HashMap, HashSet};

pub type V3 = [f64; 3];

// ------------------------------------------------------------------ Part 21

#[derive(Debug, Clone)]
pub enum Param {
    Ref(u64),
    Num(f64),
    Str(String),
    Enum(String),
    List(Vec<Param>),
    Typed(String, Box<Param>),
    Null,
}

impl Param {
    fn as_ref(&self) -> Option<u64> {
        match self {
            Param::Ref(r) => Some(*r),
            _ => None,
        }
    }
    fn as_str(&self) -> &str {
        match self {
            Param::Str(s) => s,
            _ => "",
        }
    }
    fn as_list(&self) -> &[Param] {
        match self {
            Param::List(v) => v,
            _ => &[],
        }
    }
    fn as_num(&self) -> Option<f64> {
        match self {
            Param::Num(n) => Some(*n),
            Param::Typed(_, p) => p.as_num(),
            _ => None,
        }
    }
}

/// One entity: one or more (TYPE, params) records (complex entities have several).
pub type Entity = Vec<(String, Vec<Param>)>;

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Lexer<'a> {
    fn ws(&mut self) {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"/*") {
                match self.s[self.i + 2..].windows(2).position(|w| w == b"*/") {
                    Some(p) => self.i += p + 4,
                    None => self.i = self.s.len(),
                }
                continue;
            }
            break;
        }
    }
    fn ident(&mut self) -> String {
        self.ws();
        let st = self.i;
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_')
        {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[st..self.i]).to_ascii_uppercase()
    }
    fn param(&mut self) -> Result<Param, String> {
        self.ws();
        let c = *self.s.get(self.i).ok_or("unexpected end")?;
        match c {
            b'#' => {
                self.i += 1;
                let st = self.i;
                while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                    self.i += 1;
                }
                Ok(Param::Ref(
                    std::str::from_utf8(&self.s[st..self.i])
                        .unwrap()
                        .parse()
                        .unwrap_or(0),
                ))
            }
            b'\'' => {
                self.i += 1;
                let mut v = Vec::new();
                while self.i < self.s.len() {
                    if self.s[self.i] == b'\'' {
                        if self.s.get(self.i + 1) == Some(&b'\'') {
                            v.push(b'\'');
                            self.i += 2;
                            continue;
                        }
                        self.i += 1;
                        break;
                    }
                    v.push(self.s[self.i]);
                    self.i += 1;
                }
                Ok(Param::Str(String::from_utf8_lossy(&v).to_string()))
            }
            b'.' => {
                self.i += 1;
                let st = self.i;
                while self.i < self.s.len() && self.s[self.i] != b'.' {
                    self.i += 1;
                }
                let e = String::from_utf8_lossy(&self.s[st..self.i]).to_string();
                self.i += 1;
                Ok(Param::Enum(e))
            }
            b'(' => {
                self.i += 1;
                let mut v = Vec::new();
                loop {
                    self.ws();
                    if self.s.get(self.i) == Some(&b')') {
                        self.i += 1;
                        break;
                    }
                    v.push(self.param()?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b')') => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err(format!("bad list at byte {}", self.i)),
                    }
                }
                Ok(Param::List(v))
            }
            b'$' | b'*' => {
                self.i += 1;
                Ok(Param::Null)
            }
            b'"' => {
                // binary
                self.i += 1;
                while self.i < self.s.len() && self.s[self.i] != b'"' {
                    self.i += 1;
                }
                self.i += 1;
                Ok(Param::Null)
            }
            c if c == b'-' || c == b'+' || c.is_ascii_digit() => {
                let st = self.i;
                self.i += 1;
                while self.i < self.s.len()
                    && (self.s[self.i].is_ascii_digit()
                        || matches!(self.s[self.i], b'.' | b'E' | b'e' | b'-' | b'+'))
                {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[st..self.i]).unwrap();
                Ok(Param::Num(t.parse().unwrap_or(0.0)))
            }
            c if c.is_ascii_alphabetic() => {
                let name = self.ident();
                self.ws();
                let inner = if self.s.get(self.i) == Some(&b'(') {
                    let l = self.param()?;
                    match l {
                        Param::List(mut v) if v.len() == 1 => v.pop().unwrap(),
                        other => other,
                    }
                } else {
                    Param::Null
                };
                Ok(Param::Typed(name, Box::new(inner)))
            }
            _ => Err(format!("unexpected '{}' at byte {}", c as char, self.i)),
        }
    }
    fn record(&mut self) -> Result<(String, Vec<Param>), String> {
        let name = self.ident();
        match self.param()? {
            Param::List(v) => Ok((name, v)),
            _ => Err(format!("record {name} without parameters")),
        }
    }
}

/// Parse the DATA section into entities.
pub fn parse_part21(text: &str) -> Result<HashMap<u64, Entity>, String> {
    let start = text
        .find("DATA;")
        .ok_or("not a STEP file (no DATA section)")?
        + 5;
    let b = text.as_bytes();
    let mut lx = Lexer { s: b, i: start };
    let mut out = HashMap::new();
    loop {
        lx.ws();
        if lx.i >= b.len() || b[lx.i..].starts_with(b"ENDSEC") {
            break;
        }
        if b[lx.i] != b'#' {
            // skip to next ';'
            while lx.i < b.len() && b[lx.i] != b';' {
                lx.i += 1;
            }
            lx.i += 1;
            continue;
        }
        let id = match lx.param()? {
            Param::Ref(r) => r,
            _ => unreachable!(),
        };
        lx.ws();
        if b.get(lx.i) != Some(&b'=') {
            return Err(format!("expected '=' after #{id}"));
        }
        lx.i += 1;
        lx.ws();
        let ent = if b.get(lx.i) == Some(&b'(') {
            lx.i += 1;
            let mut recs = Vec::new();
            loop {
                lx.ws();
                if b.get(lx.i) == Some(&b')') {
                    lx.i += 1;
                    break;
                }
                recs.push(lx.record()?);
            }
            recs
        } else {
            vec![lx.record()?]
        };
        lx.ws();
        if b.get(lx.i) == Some(&b';') {
            lx.i += 1;
        }
        out.insert(id, ent);
    }
    Ok(out)
}

// ------------------------------------------------------------------ transforms

#[derive(Debug, Clone, Copy)]
pub struct Xf {
    /// rotation (rows) and translation
    pub r: [[f64; 3]; 3],
    pub t: V3,
}

impl Xf {
    pub const ID: Xf = Xf {
        r: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        t: [0.0; 3],
    };
    pub fn apply(&self, p: V3) -> V3 {
        let mut o = self.t;
        for i in 0..3 {
            for j in 0..3 {
                o[i] += self.r[i][j] * p[j];
            }
        }
        o
    }
    /// self ∘ other (apply other first)
    pub fn then(&self, other: &Xf) -> Xf {
        let mut r = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                for k in 0..3 {
                    r[i][j] += self.r[i][k] * other.r[k][j];
                }
            }
        }
        Xf {
            r,
            t: self.apply(other.t),
        }
    }
    pub fn inverse(&self) -> Xf {
        let mut r = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                r[i][j] = self.r[j][i];
            }
        }
        let mut t = [0.0; 3];
        for i in 0..3 {
            for j in 0..3 {
                t[i] -= r[i][j] * self.t[j];
            }
        }
        Xf { r, t }
    }
}

fn norm(v: V3) -> V3 {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-300);
    [v[0] / l, v[1] / l, v[2] / l]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

// ------------------------------------------------------------------ model

#[derive(Debug, Clone)]
pub struct StepBody {
    /// Product name of the solid (and instance path).
    pub name: String,
    pub path: String,
    /// Triangles in mm, global coordinates.
    pub tris: Vec<[V3; 3]>,
    pub min: V3,
    pub max: V3,
    /// True when the solid could not be tessellated (bounding box of its vertices only).
    pub bbox_only: bool,
}

impl StepBody {
    /// Enclosed volume from the triangles (divergence theorem), mm³.
    pub fn volume(&self) -> f64 {
        let mut v = 0.0;
        for t in &self.tris {
            v += t[0][0] * (t[1][1] * t[2][2] - t[2][1] * t[1][2])
                - t[1][0] * (t[0][1] * t[2][2] - t[2][1] * t[0][2])
                + t[2][0] * (t[0][1] * t[1][2] - t[1][1] * t[0][2]);
        }
        (v / 6.0).abs()
    }
}

#[derive(Debug, Clone, Default)]
pub struct StepModel {
    pub bodies: Vec<StepBody>,
    pub unit_mm: f64,
    pub warnings: Vec<String>,
    pub products: usize,
    pub occurrences: usize,
}

struct Ctx<'a> {
    e: &'a HashMap<u64, Entity>,
}

impl<'a> Ctx<'a> {
    fn rec(&self, id: u64, name: &str) -> Option<&'a Vec<Param>> {
        self.e
            .get(&id)?
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, p)| p)
    }
    fn first(&self, id: u64) -> Option<&'a (String, Vec<Param>)> {
        self.e.get(&id)?.first()
    }
    fn point(&self, id: u64) -> Option<V3> {
        let p = self.rec(id, "CARTESIAN_POINT")?;
        let c = p.get(1)?.as_list();
        Some([
            c.first().and_then(|v| v.as_num()).unwrap_or(0.0),
            c.get(1).and_then(|v| v.as_num()).unwrap_or(0.0),
            c.get(2).and_then(|v| v.as_num()).unwrap_or(0.0),
        ])
    }
    fn dir(&self, id: Option<u64>, default: V3) -> V3 {
        id.and_then(|id| self.rec(id, "DIRECTION"))
            .and_then(|p| {
                let c = p.get(1)?.as_list();
                Some(norm([
                    c.first().and_then(|v| v.as_num()).unwrap_or(0.0),
                    c.get(1).and_then(|v| v.as_num()).unwrap_or(0.0),
                    c.get(2).and_then(|v| v.as_num()).unwrap_or(0.0),
                ]))
            })
            .unwrap_or(default)
    }
    /// AXIS2_PLACEMENT_3D → local-to-parent transform.
    fn placement(&self, id: u64, scale: f64) -> Xf {
        let Some(p) = self.rec(id, "AXIS2_PLACEMENT_3D") else {
            return Xf::ID;
        };
        let o = p
            .get(1)
            .and_then(|r| r.as_ref())
            .and_then(|r| self.point(r))
            .unwrap_or([0.0; 3]);
        let z = self.dir(p.get(2).and_then(|r| r.as_ref()), [0.0, 0.0, 1.0]);
        let mut x = self.dir(p.get(3).and_then(|r| r.as_ref()), [1.0, 0.0, 0.0]);
        // orthogonalise x against z
        let d = x[0] * z[0] + x[1] * z[1] + x[2] * z[2];
        x = norm([x[0] - d * z[0], x[1] - d * z[1], x[2] - d * z[2]]);
        if x.iter().all(|v| v.abs() < 1e-9) {
            x = if z[0].abs() < 0.9 {
                norm(cross([0.0, 1.0, 0.0], z))
            } else {
                norm(cross(z, [0.0, 0.0, 1.0]))
            };
        }
        let y = cross(z, x);
        Xf {
            r: [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]],
            t: [o[0] * scale, o[1] * scale, o[2] * scale],
        }
    }
    /// Collect every CARTESIAN_POINT reachable from an entity (fallback bbox).
    fn points_under(&self, id: u64, seen: &mut HashSet<u64>, out: &mut Vec<V3>) {
        if !seen.insert(id) {
            return;
        }
        let Some(ent) = self.e.get(&id) else { return };
        for (name, params) in ent {
            if name == "CARTESIAN_POINT" {
                if let Some(p) = self.point(id) {
                    out.push(p);
                }
                return;
            }
            // do not walk into surfaces' control nets through placements only
            fn refs(p: &Param, out: &mut Vec<u64>) {
                match p {
                    Param::Ref(r) => out.push(*r),
                    Param::List(v) => v.iter().for_each(|x| refs(x, out)),
                    Param::Typed(_, b) => refs(b, out),
                    _ => {}
                }
            }
            let mut rs = Vec::new();
            for p in params {
                refs(p, &mut rs);
            }
            for r in rs {
                // only follow topology/vertices (VERTEX_POINT → point)
                if let Some((n, _)) = self.first(r) {
                    if matches!(
                        n.as_str(),
                        "CLOSED_SHELL"
                            | "OPEN_SHELL"
                            | "ADVANCED_FACE"
                            | "FACE_SURFACE"
                            | "FACE_OUTER_BOUND"
                            | "FACE_BOUND"
                            | "EDGE_LOOP"
                            | "ORIENTED_EDGE"
                            | "EDGE_CURVE"
                            | "VERTEX_POINT"
                            | "CARTESIAN_POINT"
                            | "POLY_LOOP"
                            | "ORIENTED_CLOSED_SHELL"
                            | "MANIFOLD_SOLID_BREP"
                            | "BREP_WITH_VOIDS"
                    ) {
                        self.points_under(r, seen, out);
                    }
                }
            }
        }
    }
}

/// Length unit of a representation context, in mm.
fn context_unit_mm(c: &Ctx, ctx_id: u64) -> Option<f64> {
    let units = c
        .rec(ctx_id, "GLOBAL_UNIT_ASSIGNED_CONTEXT")?
        .first()?
        .as_list();
    for u in units {
        let Some(uid) = u.as_ref() else { continue };
        let ent = c.e.get(&uid)?;
        if !ent.iter().any(|(n, _)| n == "LENGTH_UNIT") {
            continue;
        }
        if let Some(si) = ent.iter().find(|(n, _)| n == "SI_UNIT") {
            let prefix = match si.1.first() {
                Some(Param::Enum(p)) => p.as_str(),
                _ => "",
            };
            return Some(match prefix {
                "MILLI" => 1.0,
                "CENTI" => 10.0,
                "DECI" => 100.0,
                "MICRO" => 1e-3,
                "KILO" => 1e6,
                _ => 1000.0,
            });
        }
        if let Some(cb) = ent.iter().find(|(n, _)| n == "CONVERSION_BASED_UNIT") {
            let name =
                cb.1.first()
                    .map(|p| p.as_str().to_ascii_uppercase())
                    .unwrap_or_default();
            // factor from LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(25.4), #mm)
            let f =
                cb.1.get(1)
                    .and_then(|r| r.as_ref())
                    .and_then(|r| c.rec(r, "LENGTH_MEASURE_WITH_UNIT"))
                    .and_then(|p| {
                        let v = p.first()?.as_num()?;
                        let base = p.get(1)?.as_ref()?;
                        let base_mm =
                            c.e.get(&base)
                                .and_then(|e| e.iter().find(|(n, _)| n == "SI_UNIT"))
                                .map(|si| match si.1.first() {
                                    Some(Param::Enum(p)) if p == "MILLI" => 1.0,
                                    Some(Param::Enum(p)) if p == "CENTI" => 10.0,
                                    _ => 1000.0,
                                })
                                .unwrap_or(1.0);
                        Some(v * base_mm)
                    });
            return Some(f.unwrap_or(match name.as_str() {
                "INCH" => 25.4,
                "FOOT" => 304.8,
                _ => 1.0,
            }));
        }
    }
    None
}

/// Read a STEP file into tessellated bodies (global coordinates, mm).
/// `tolerance_mm`: chordal tolerance of the tessellation.
pub fn read(text: &str, tolerance_mm: f64) -> Result<StepModel, String> {
    #[cfg(feature = "occt")]
    {
        match crate::occt::read_step(text, tolerance_mm) {
            Ok(m) if !m.bodies.is_empty() => return Ok(m),
            Ok(_) => {}
            Err(e) => {
                let mut m = read_structure(text)?;
                m.warnings.insert(
                    0,
                    format!("OpenCascade import failed ({e}); using the built-in reader"),
                );
                return Ok(m);
            }
        }
    }
    read_structure(text)
}

/// Built-in reader: product structure + vertex bounding boxes (no kernel).
pub fn read_structure(text: &str) -> Result<StepModel, String> {
    let ents = parse_part21(text)?;
    let c = Ctx { e: &ents };
    let mut model = StepModel::default();

    // ---- units (first length unit found) ----
    let mut unit = None;
    for (id, ent) in &ents {
        if ent.iter().any(|(n, _)| n == "GLOBAL_UNIT_ASSIGNED_CONTEXT") {
            if let Some(u) = context_unit_mm(&c, *id) {
                unit = Some(u);
                break;
            }
        }
    }
    let scale = unit.unwrap_or(1.0);
    model.unit_mm = scale;

    // ---- representations and their solid items ----
    let is_rep = |n: &str| {
        n.ends_with("REPRESENTATION")
            && !n.contains("RELATIONSHIP")
            && n != "DEFINITIONAL_REPRESENTATION"
    };
    let mut rep_items: HashMap<u64, Vec<u64>> = HashMap::new();
    for (id, ent) in &ents {
        for (n, p) in ent {
            if is_rep(n) {
                let items: Vec<u64> = p
                    .get(1)
                    .map(|l| l.as_list().iter().filter_map(|x| x.as_ref()).collect())
                    .unwrap_or_default();
                rep_items.insert(*id, items);
            }
        }
    }
    // ---- product of each representation ----
    // PRODUCT_DEFINITION → name
    let pd_name = |pd: u64| -> String {
        c.rec(pd, "PRODUCT_DEFINITION")
            .and_then(|p| p.get(2)?.as_ref())
            .and_then(|f| {
                let e = c.e.get(&f)?;
                let pr = e
                    .iter()
                    .find(|(n, _)| n.starts_with("PRODUCT_DEFINITION_FORMATION"))?
                    .1
                    .get(2)?
                    .as_ref()?;
                let p = c.rec(pr, "PRODUCT")?;
                let n = p.first()?.as_str();
                let n2 = p.get(1).map(|x| x.as_str()).unwrap_or("");
                Some(if n.is_empty() {
                    n2.to_string()
                } else {
                    n.to_string()
                })
            })
            .unwrap_or_else(|| format!("#{pd}"))
    };
    // shape rep → product definition (via SHAPE_DEFINITION_REPRESENTATION)
    let mut rep_pd: HashMap<u64, u64> = HashMap::new();
    for ent in ents.values() {
        if let Some((_, p)) = ent
            .iter()
            .find(|(n, _)| n == "SHAPE_DEFINITION_REPRESENTATION")
        {
            let (Some(pds), Some(rep)) = (
                p.first().and_then(|x| x.as_ref()),
                p.get(1).and_then(|x| x.as_ref()),
            ) else {
                continue;
            };
            if let Some(def) = c
                .rec(pds, "PRODUCT_DEFINITION_SHAPE")
                .and_then(|q| q.get(2)?.as_ref())
            {
                if c.rec(def, "PRODUCT_DEFINITION").is_some() {
                    rep_pd.insert(rep, def);
                }
            }
        }
    }
    // reps linked without transform (same product), and occurrences with transform
    let mut rep_links: Vec<(u64, u64)> = Vec::new();
    struct Occ {
        parent_pd: u64,
        child_pd: u64,
        xf: Option<(u64, u64, bool)>, // (item1, item2, rep1_is_child)
        rep1: u64,
        rep2: u64,
    }
    let mut occs: Vec<Occ> = Vec::new();
    for ent in ents.values() {
        let names: Vec<&str> = ent.iter().map(|(n, _)| n.as_str()).collect();
        if names.contains(&"SHAPE_REPRESENTATION_RELATIONSHIP")
            && !names.contains(&"REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION")
        {
            let p = &ent
                .iter()
                .find(|(n, _)| n == "SHAPE_REPRESENTATION_RELATIONSHIP")
                .unwrap()
                .1;
            if let (Some(a), Some(b)) = (
                p.get(2).and_then(|x| x.as_ref()),
                p.get(3).and_then(|x| x.as_ref()),
            ) {
                rep_links.push((a, b));
            }
        }
    }
    for ent in ents.values() {
        let Some((_, p)) = ent
            .iter()
            .find(|(n, _)| n == "CONTEXT_DEPENDENT_SHAPE_REPRESENTATION")
        else {
            continue;
        };
        let (Some(rr), Some(pds)) = (
            p.first().and_then(|x| x.as_ref()),
            p.get(1).and_then(|x| x.as_ref()),
        ) else {
            continue;
        };
        let Some(rre) = ents.get(&rr) else { continue };
        let rel = rre
            .iter()
            .find(|(n, _)| n == "REPRESENTATION_RELATIONSHIP")
            .map(|x| &x.1);
        let tr = rre
            .iter()
            .find(|(n, _)| n == "REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION")
            .map(|x| &x.1);
        let (Some(rel), Some(tr)) = (rel, tr) else {
            continue;
        };
        let (Some(rep1), Some(rep2)) = (
            rel.get(2).and_then(|x| x.as_ref()),
            rel.get(3).and_then(|x| x.as_ref()),
        ) else {
            continue;
        };
        let Some(nauo) = c
            .rec(pds, "PRODUCT_DEFINITION_SHAPE")
            .and_then(|q| q.get(2)?.as_ref())
        else {
            continue;
        };
        let Some(np) = c.rec(nauo, "NEXT_ASSEMBLY_USAGE_OCCURRENCE") else {
            continue;
        };
        let (Some(parent_pd), Some(child_pd)) = (
            np.get(3).and_then(|x| x.as_ref()),
            np.get(4).and_then(|x| x.as_ref()),
        ) else {
            continue;
        };
        let xf = tr.first().and_then(|x| x.as_ref()).and_then(|idt| {
            let q = c.rec(idt, "ITEM_DEFINED_TRANSFORMATION")?;
            Some((q.get(2)?.as_ref()?, q.get(3)?.as_ref()?))
        });
        occs.push(Occ {
            parent_pd,
            child_pd,
            xf: xf.map(|(a, b)| (a, b, true)),
            rep1,
            rep2,
        });
    }
    model.occurrences = occs.len();
    // all reps of a product definition (closure over rep_links)
    let mut pd_reps: HashMap<u64, Vec<u64>> = HashMap::new();
    for (rep, pd) in &rep_pd {
        pd_reps.entry(*pd).or_default().push(*rep);
    }
    let mut changed = true;
    let mut guard = 0;
    while changed && guard < 20 {
        changed = false;
        guard += 1;
        for (a, b) in &rep_links {
            let pa = rep_pd.get(a).copied();
            let pb = rep_pd.get(b).copied();
            match (pa, pb) {
                (Some(p), None) => {
                    rep_pd.insert(*b, p);
                    pd_reps.entry(p).or_default().push(*b);
                    changed = true;
                }
                (None, Some(p)) => {
                    rep_pd.insert(*a, p);
                    pd_reps.entry(p).or_default().push(*a);
                    changed = true;
                }
                _ => {}
            }
        }
    }
    model.products = pd_reps.len();
    // decide child side of each occurrence
    for o in occs.iter_mut() {
        if let Some((a, b, _)) = o.xf {
            let rep1_child = rep_pd
                .get(&o.rep1)
                .map(|p| *p == o.child_pd)
                .unwrap_or(true)
                || rep_pd
                    .get(&o.rep2)
                    .map(|p| *p == o.parent_pd)
                    .unwrap_or(false);
            o.xf = Some((a, b, rep1_child));
        }
    }

    // ---- solids of a representation (incl. MAPPED_ITEM instances) ----
    #[derive(Clone)]
    struct Solid {
        shells: Vec<u64>,
        xf: Xf,
        root: u64,
    }
    fn solids_of(
        c: &Ctx,
        rep_items: &HashMap<u64, Vec<u64>>,
        rep: u64,
        xf: Xf,
        scale: f64,
        depth: usize,
        out: &mut Vec<Solid>,
    ) {
        if depth > 16 {
            return;
        }
        for &it in rep_items.get(&rep).map(|v| v.as_slice()).unwrap_or(&[]) {
            let Some((n, p)) = c.first(it) else { continue };
            match n.as_str() {
                "MANIFOLD_SOLID_BREP" | "FACETED_BREP" => {
                    if let Some(s) = p.get(1).and_then(|x| x.as_ref()) {
                        out.push(Solid {
                            shells: vec![s],
                            xf,
                            root: it,
                        });
                    }
                }
                "BREP_WITH_VOIDS" => {
                    if let Some(s) = p.get(1).and_then(|x| x.as_ref()) {
                        out.push(Solid {
                            shells: vec![s],
                            xf,
                            root: it,
                        });
                    }
                }
                "SHELL_BASED_SURFACE_MODEL" => {
                    let shells: Vec<u64> = p
                        .get(1)
                        .map(|l| l.as_list().iter().filter_map(|x| x.as_ref()).collect())
                        .unwrap_or_default();
                    if !shells.is_empty() {
                        out.push(Solid {
                            shells,
                            xf,
                            root: it,
                        });
                    }
                }
                "MAPPED_ITEM" => {
                    // MAPPED_ITEM(name, REPRESENTATION_MAP(origin, rep), target)
                    let (Some(map), Some(target)) = (
                        p.get(1).and_then(|x| x.as_ref()),
                        p.get(2).and_then(|x| x.as_ref()),
                    ) else {
                        continue;
                    };
                    let Some(mp) = c.rec(map, "REPRESENTATION_MAP") else {
                        continue;
                    };
                    let (Some(origin), Some(mrep)) = (
                        mp.first().and_then(|x| x.as_ref()),
                        mp.get(1).and_then(|x| x.as_ref()),
                    ) else {
                        continue;
                    };
                    let t = c
                        .placement(target, scale)
                        .then(&c.placement(origin, scale).inverse());
                    solids_of(c, rep_items, mrep, xf.then(&t), scale, depth + 1, out);
                }
                _ => {}
            }
        }
    }

    // ---- walk the assembly tree from the roots ----
    let children: HashMap<u64, Vec<usize>> = {
        let mut m: HashMap<u64, Vec<usize>> = HashMap::new();
        for (k, o) in occs.iter().enumerate() {
            m.entry(o.parent_pd).or_default().push(k);
        }
        m
    };
    let child_set: HashSet<u64> = occs.iter().map(|o| o.child_pd).collect();
    let mut roots: Vec<u64> = pd_reps
        .keys()
        .copied()
        .filter(|p| !child_set.contains(p))
        .collect();
    roots.sort();
    let mut instances: Vec<(String, String, Solid)> = Vec::new(); // (name, path, solid)
    fn walk(
        c: &Ctx,
        pd: u64,
        xf: Xf,
        path: String,
        depth: usize,
        pd_reps: &HashMap<u64, Vec<u64>>,
        children: &HashMap<u64, Vec<usize>>,
        occs: &[Occ],
        rep_items: &HashMap<u64, Vec<u64>>,
        scale: f64,
        name_of: &dyn Fn(u64) -> String,
        out: &mut Vec<(String, String, Solid)>,
    ) {
        if depth > 32 {
            return;
        }
        let name = name_of(pd);
        let path = if path.is_empty() {
            name.clone()
        } else {
            format!("{path}/{name}")
        };
        for &rep in pd_reps.get(&pd).map(|v| v.as_slice()).unwrap_or(&[]) {
            let mut s = Vec::new();
            solids_of(c, rep_items, rep, xf, scale, 0, &mut s);
            for so in s {
                out.push((name.clone(), path.clone(), so));
            }
        }
        for &k in children.get(&pd).map(|v| v.as_slice()).unwrap_or(&[]) {
            let o = &occs[k];
            let t = match o.xf {
                Some((a, b, rep1_child)) => {
                    let (from, to) = if rep1_child { (a, b) } else { (b, a) };
                    c.placement(to, scale)
                        .then(&c.placement(from, scale).inverse())
                }
                None => Xf::ID,
            };
            walk(
                c,
                o.child_pd,
                xf.then(&t),
                path.clone(),
                depth + 1,
                pd_reps,
                children,
                occs,
                rep_items,
                scale,
                name_of,
                out,
            );
        }
    }
    let name_of = |pd: u64| pd_name(pd);
    for r in &roots {
        walk(
            &c,
            *r,
            Xf::ID,
            String::new(),
            0,
            &pd_reps,
            &children,
            &occs,
            &rep_items,
            scale,
            &name_of,
            &mut instances,
        );
    }
    // representations with solids not attached to any product (plain geometry files)
    if instances.is_empty() {
        let mut reps: Vec<u64> = rep_items.keys().copied().collect();
        reps.sort();
        for rep in reps {
            let mut s = Vec::new();
            solids_of(&c, &rep_items, rep, Xf::ID, scale, 0, &mut s);
            for so in s {
                instances.push((format!("solid#{}", so.root), String::new(), so));
            }
        }
    }

    let mut failed = 0;
    for (name, path, so) in instances {
        let tris = Vec::new();
        let bbox_only = true;
        let mut pts: Vec<V3> = Vec::new();
        {
            failed += 1;
            let mut seen = HashSet::new();
            let mut raw = Vec::new();
            for s in &so.shells {
                c.points_under(*s, &mut seen, &mut raw);
            }
            pts.extend(
                raw.into_iter()
                    .map(|p| so.xf.apply([p[0] * scale, p[1] * scale, p[2] * scale])),
            );
        }
        if pts.is_empty() {
            continue;
        }
        let mut mn = [f64::INFINITY; 3];
        let mut mx = [f64::NEG_INFINITY; 3];
        for p in &pts {
            for a in 0..3 {
                mn[a] = mn[a].min(p[a]);
                mx[a] = mx[a].max(p[a]);
            }
        }
        model.bodies.push(StepBody {
            name,
            path,
            tris,
            min: mn,
            max: mx,
            bbox_only,
        });
    }
    if failed > 0 {
        model.warnings.push(format!(
            "{failed} solid(s) are represented by the bounding box of their vertices (built-in reader, no B-rep tessellation)"
        ));
    }
    if model.bodies.is_empty() {
        model
            .warnings
            .push("no solid bodies found in the STEP file".into());
    }
    Ok(model)
}

// ------------------------------------------------------------------ voxels

/// How a body becomes blocks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlockMode {
    BoundingBox,
    /// Voxels of this size (mm), merged into boxes.
    Voxels(f64),
}

/// Convert a body to axis-aligned boxes (mm).
pub fn body_boxes(b: &StepBody, mode: BlockMode) -> Vec<(V3, V3)> {
    let res = match mode {
        BlockMode::BoundingBox => return vec![(b.min, b.max)],
        BlockMode::Voxels(r) if b.bbox_only || b.tris.is_empty() => {
            let _ = r;
            return vec![(b.min, b.max)];
        }
        BlockMode::Voxels(r) => r.max(1e-3),
    };
    // voxels on a global lattice (multiples of the voxel size), so that
    // bodies voxelised separately share their grid planes; at most 400 per axis
    let mut h = [res; 3];
    let mut o = [0.0; 3];
    let mut n = [1usize; 3];
    for a in 0..3 {
        let m = ((b.max[a] - b.min[a]) / res / 400.0).ceil().max(1.0);
        h[a] = res * m;
        o[a] = (b.min[a] / h[a]).floor() * h[a];
        n[a] = (((b.max[a] - o[a]) / h[a] - 1e-9).ceil() as usize).max(1);
    }
    let b = &StepBody {
        min: o,
        max: [0, 1, 2].map(|a| o[a] + n[a] as f64 * h[a]),
        ..b.clone()
    };
    // bin triangles by xy footprint
    let mut bins: Vec<Vec<u32>> = vec![Vec::new(); n[0] * n[1]];
    for (k, t) in b.tris.iter().enumerate() {
        let lo = [0, 1].map(|a| t.iter().map(|p| p[a]).fold(f64::INFINITY, f64::min));
        let hi = [0, 1].map(|a| t.iter().map(|p| p[a]).fold(f64::NEG_INFINITY, f64::max));
        let i0 = (((lo[0] - b.min[0]) / h[0] - 0.5).floor().max(0.0)) as usize;
        let i1 = ((((hi[0] - b.min[0]) / h[0] - 0.5).ceil()) as usize).min(n[0] - 1);
        let j0 = (((lo[1] - b.min[1]) / h[1] - 0.5).floor().max(0.0)) as usize;
        let j1 = ((((hi[1] - b.min[1]) / h[1] - 0.5).ceil()) as usize).min(n[1] - 1);
        for j in j0..=j1 {
            for i in i0..=i1 {
                bins[i + n[0] * j].push(k as u32);
            }
        }
    }
    // inside test along z for each column (parity of crossings)
    let mut vox = vec![false; n[0] * n[1] * n[2]];
    for j in 0..n[1] {
        for i in 0..n[0] {
            // tiny offset avoids hitting edges exactly
            let x = b.min[0] + (i as f64 + 0.5) * h[0] + 1.234e-7;
            let y = b.min[1] + (j as f64 + 0.5) * h[1] + 2.345e-7;
            let mut zs: Vec<f64> = Vec::new();
            for &k in &bins[i + n[0] * j] {
                let t = &b.tris[k as usize];
                let (a, bb, c) = (t[0], t[1], t[2]);
                let d = (bb[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (bb[1] - a[1]);
                if d.abs() < 1e-15 {
                    continue;
                }
                let u = ((x - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (y - a[1])) / d;
                let v = ((bb[0] - a[0]) * (y - a[1]) - (x - a[0]) * (bb[1] - a[1])) / d;
                if u >= 0.0 && v >= 0.0 && u + v <= 1.0 {
                    zs.push(a[2] + u * (bb[2] - a[2]) + v * (c[2] - a[2]));
                }
            }
            zs.sort_by(|p, q| p.partial_cmp(q).unwrap());
            zs.dedup_by(|p, q| (*p - *q).abs() < 1e-9);
            for pair in zs.chunks(2) {
                if pair.len() < 2 {
                    break;
                }
                for k in 0..n[2] {
                    let z = b.min[2] + (k as f64 + 0.5) * h[2];
                    if z > pair[0] && z < pair[1] {
                        vox[i + n[0] * (j + n[1] * k)] = true;
                    }
                }
            }
        }
    }
    // greedy merge into boxes: x runs, then extend in y, then in z
    let idx = |i: usize, j: usize, k: usize| i + n[0] * (j + n[1] * k);
    let mut used = vec![false; vox.len()];
    let mut out = Vec::new();
    for k in 0..n[2] {
        for j in 0..n[1] {
            let mut i = 0;
            while i < n[0] {
                let c0 = idx(i, j, k);
                if !vox[c0] || used[c0] {
                    i += 1;
                    continue;
                }
                let mut i1 = i;
                while i1 + 1 < n[0] && vox[idx(i1 + 1, j, k)] && !used[idx(i1 + 1, j, k)] {
                    i1 += 1;
                }
                let row_ok = |jj: usize, kk: usize, used: &Vec<bool>| {
                    (i..=i1).all(|ii| vox[idx(ii, jj, kk)] && !used[idx(ii, jj, kk)])
                };
                let mut j1 = j;
                while j1 + 1 < n[1] && row_ok(j1 + 1, k, &used) {
                    j1 += 1;
                }
                let mut k1 = k;
                while k1 + 1 < n[2] && (j..=j1).all(|jj| row_ok(jj, k1 + 1, &used)) {
                    k1 += 1;
                }
                for kk in k..=k1 {
                    for jj in j..=j1 {
                        for ii in i..=i1 {
                            used[idx(ii, jj, kk)] = true;
                        }
                    }
                }
                out.push((
                    [
                        b.min[0] + i as f64 * h[0],
                        b.min[1] + j as f64 * h[1],
                        b.min[2] + k as f64 * h[2],
                    ],
                    [
                        b.min[0] + (i1 + 1) as f64 * h[0],
                        b.min[1] + (j1 + 1) as f64 * h[1],
                        b.min[2] + (k1 + 1) as f64 * h[2],
                    ],
                ));
                i = i1 + 1;
            }
        }
    }
    if out.is_empty() {
        out.push((b.min, b.max));
    }
    out
}

/// Write blocks as a STEP AP214 file of boxes (one product per block).
pub fn write_boxes(name: &str, boxes: &[(String, V3, V3)]) -> String {
    let mut s = String::new();
    s += "ISO-10303-21;\nHEADER;\n";
    s += "FILE_DESCRIPTION(('FerroTherm model geometry'),'2;1');\n";
    s += &format!("FILE_NAME('{}','2026-01-01T00:00:00',('Roany Romano'),(''),'FerroTherm','FerroTherm','');\n", name.replace('\'', "''"));
    s += "FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\nENDSEC;\nDATA;\n";
    let mut id = 1u64;
    let mut ent = |s: &mut String, body: String| -> u64 {
        let i = id;
        s.push_str(&format!("#{i}={body};\n"));
        id += 1;
        i
    };
    let app = ent(&mut s, "APPLICATION_CONTEXT('automotive design')".into());
    ent(&mut s, format!("APPLICATION_PROTOCOL_DEFINITION('international standard','automotive_design',2000,#{app})"));
    let pctx = ent(&mut s, format!("PRODUCT_CONTEXT('',#{app},'mechanical')"));
    let pdctx = ent(
        &mut s,
        format!("PRODUCT_DEFINITION_CONTEXT('part definition',#{app},'design')"),
    );
    let mm = ent(
        &mut s,
        "(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.))".into(),
    );
    let rad = ent(
        &mut s,
        "(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.))".into(),
    );
    let sr = ent(
        &mut s,
        "(NAMED_UNIT(*)SI_UNIT($,.STERADIAN.)SOLID_ANGLE_UNIT())".into(),
    );
    let unc = ent(&mut s, format!("UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-06),#{mm},'distance_accuracy_value','')"));
    let ctx = ent(
        &mut s,
        format!("(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{unc}))GLOBAL_UNIT_ASSIGNED_CONTEXT((#{mm},#{rad},#{sr}))REPRESENTATION_CONTEXT('',''))"),
    );
    let o = ent(&mut s, "CARTESIAN_POINT('',(0.,0.,0.))".into());
    let dz = ent(&mut s, "DIRECTION('',(0.,0.,1.))".into());
    let dx = ent(&mut s, "DIRECTION('',(1.,0.,0.))".into());
    let ax = ent(&mut s, format!("AXIS2_PLACEMENT_3D('',#{o},#{dz},#{dx})"));
    let f = |v: f64| format!("{v:.6}");
    for (bn, lo, hi) in boxes {
        let bn = bn.replace('\'', "''");
        // 8 vertices
        let corners = [
            [lo[0], lo[1], lo[2]],
            [hi[0], lo[1], lo[2]],
            [hi[0], hi[1], lo[2]],
            [lo[0], hi[1], lo[2]],
            [lo[0], lo[1], hi[2]],
            [hi[0], lo[1], hi[2]],
            [hi[0], hi[1], hi[2]],
            [lo[0], hi[1], hi[2]],
        ];
        let mut vp = Vec::new();
        let mut cp = Vec::new();
        for c in corners {
            let p = ent(
                &mut s,
                format!("CARTESIAN_POINT('',({},{},{}))", f(c[0]), f(c[1]), f(c[2])),
            );
            cp.push(p);
            vp.push(ent(&mut s, format!("VERTEX_POINT('',#{p})")));
        }
        // edges (a, b)
        let edges = [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];
        let mut ec = Vec::new();
        for (a, b) in edges {
            let pa = corners[a];
            let pb = corners[b];
            let d = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let dir = ent(
                &mut s,
                format!(
                    "DIRECTION('',({},{},{}))",
                    f(d[0] / l),
                    f(d[1] / l),
                    f(d[2] / l)
                ),
            );
            let vec = ent(&mut s, format!("VECTOR('',#{dir},{})", f(l)));
            let line = ent(&mut s, format!("LINE('',#{},#{vec})", cp[a]));
            ec.push(ent(
                &mut s,
                format!("EDGE_CURVE('',#{},#{},#{line},.T.)", vp[a], vp[b]),
            ));
        }
        // faces: (vertex loop, edge list with orientation), outward normal, plane origin corner
        let face_def: [([usize; 4], V3, usize); 6] = [
            ([0, 3, 2, 1], [0.0, 0.0, -1.0], 0),
            ([4, 5, 6, 7], [0.0, 0.0, 1.0], 4),
            ([0, 1, 5, 4], [0.0, -1.0, 0.0], 0),
            ([1, 2, 6, 5], [1.0, 0.0, 0.0], 1),
            ([2, 3, 7, 6], [0.0, 1.0, 0.0], 2),
            ([3, 0, 4, 7], [-1.0, 0.0, 0.0], 3),
        ];
        let mut faces = Vec::new();
        for (lp, nrm, org) in face_def {
            let mut oes = Vec::new();
            for k in 0..4 {
                let (a, b) = (lp[k], lp[(k + 1) % 4]);
                let (ei, same) = edges
                    .iter()
                    .position(|e| *e == (a, b))
                    .map(|p| (p, true))
                    .or_else(|| edges.iter().position(|e| *e == (b, a)).map(|p| (p, false)))
                    .unwrap();
                oes.push(ent(
                    &mut s,
                    format!(
                        "ORIENTED_EDGE('',*,*,#{},{})",
                        ec[ei],
                        if same { ".T." } else { ".F." }
                    ),
                ));
            }
            let lst = oes
                .iter()
                .map(|e| format!("#{e}"))
                .collect::<Vec<_>>()
                .join(",");
            let el = ent(&mut s, format!("EDGE_LOOP('',({lst}))"));
            let fb = ent(&mut s, format!("FACE_OUTER_BOUND('',#{el},.T.)"));
            let nd = ent(
                &mut s,
                format!("DIRECTION('',({},{},{}))", f(nrm[0]), f(nrm[1]), f(nrm[2])),
            );
            let rd = if nrm[0].abs() > 0.5 {
                "(0.,1.,0.)"
            } else {
                "(1.,0.,0.)"
            };
            let rdi = ent(&mut s, format!("DIRECTION('',{rd})"));
            let pax = ent(
                &mut s,
                format!("AXIS2_PLACEMENT_3D('',#{},#{nd},#{rdi})", cp[org]),
            );
            let pl = ent(&mut s, format!("PLANE('',#{pax})"));
            faces.push(ent(&mut s, format!("ADVANCED_FACE('',(#{fb}),#{pl},.T.)")));
        }
        let fl = faces
            .iter()
            .map(|e| format!("#{e}"))
            .collect::<Vec<_>>()
            .join(",");
        let shell = ent(&mut s, format!("CLOSED_SHELL('',({fl}))"));
        let brep = ent(&mut s, format!("MANIFOLD_SOLID_BREP('{bn}',#{shell})"));
        let rep = ent(
            &mut s,
            format!("ADVANCED_BREP_SHAPE_REPRESENTATION('{bn}',(#{ax},#{brep}),#{ctx})"),
        );
        let prod = ent(&mut s, format!("PRODUCT('{bn}','{bn}','',(#{pctx}))"));
        let pdf = ent(
            &mut s,
            format!("PRODUCT_DEFINITION_FORMATION('','',#{prod})"),
        );
        let pd = ent(
            &mut s,
            format!("PRODUCT_DEFINITION('design','',#{pdf},#{pdctx})"),
        );
        let pds = ent(&mut s, format!("PRODUCT_DEFINITION_SHAPE('','',#{pd})"));
        ent(
            &mut s,
            format!("SHAPE_DEFINITION_REPRESENTATION(#{pds},#{rep})"),
        );
    }
    s += "ENDSEC;\nEND-ISO-10303-21;\n";
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_round_trip() {
        let boxes = vec![
            ("base".to_string(), [0.0, 0.0, 0.0], [100.0, 50.0, 4.0]),
            ("fin".to_string(), [10.0, 5.0, 4.0], [12.0, 45.0, 30.0]),
        ];
        let text = write_boxes("rt", &boxes);
        let m = read(&text, 0.01).unwrap();
        assert_eq!(m.bodies.len(), 2, "{:?}", m.warnings);
        let base = m.bodies.iter().find(|b| b.name == "base").unwrap();
        assert!(!base.bbox_only);
        assert!(
            (base.volume() - 100.0 * 50.0 * 4.0).abs() < 1e-5 * 20000.0,
            "{}",
            base.volume()
        );
        for a in 0..3 {
            assert!(
                (base.min[a] - boxes[0].1[a]).abs() < 1e-4
                    && (base.max[a] - boxes[0].2[a]).abs() < 1e-4
            );
        }
        let fin = m.bodies.iter().find(|b| b.name == "fin").unwrap();
        let bx = body_boxes(fin, BlockMode::Voxels(1.0));
        let vol: f64 = bx
            .iter()
            .map(|(l, h)| (h[0] - l[0]) * (h[1] - l[1]) * (h[2] - l[2]))
            .sum();
        assert!((vol - 2.0 * 40.0 * 26.0).abs() < 1e-3, "{vol}");
        assert_eq!(bx.len(), 1, "a box voxelises to a single merged box");
    }

    #[test]
    fn transforms_compose() {
        let a = Xf {
            r: [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            t: [10.0, 0.0, 0.0],
        };
        let p = a.apply([1.0, 0.0, 0.0]);
        assert!((p[0] - 10.0).abs() < 1e-12 && (p[1] - 1.0).abs() < 1e-12);
        let q = a.inverse().apply(p);
        assert!((q[0] - 1.0).abs() < 1e-12 && q[1].abs() < 1e-12);
    }
}
