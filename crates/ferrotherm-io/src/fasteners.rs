// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Find fasteners (screws, washers, nuts, rivets, pins, inserts, standoffs)
//! in a STEP assembly and write the same file without them.
//!
//! **Recognition** is a rule-based classifier, not a trained model. Each leaf
//! part gets a score from
//! * its names (product id / name / description, instance names): fastener
//!   words in Italian, English, German, French and Spanish, fastener standard
//!   numbers (ISO, DIN, UNI, EN) and thread designations (M3x8, #4-40);
//! * its own geometry in its local frame: washer (thin round disc), nut
//!   (hexagon, across-corners / across-flats ≈ 1.155), screw (round and
//!   elongated), small size;
//! * how often it is used.
//!
//! Names of PCBs, boards and reference designators (R12, C3, U1…) block a
//! geometry-only match, so round capacitors, LEDs or connector pins are not
//! taken for screws. Names alone decide an automatic selection; geometry
//! alone only proposes ("possible").
//!
//! **Removal** edits the Part 21 text instead of rewriting the model: the
//! records of the removed parts (product, definitions, shape representations
//! and their geometry, assembly occurrences with their placements, styles and
//! layer entries) are deleted, references to them are dropped from the lists
//! that held them, and every other record is copied byte for byte. Header,
//! units, names, colours, PCBs and components are therefore untouched, and
//! the result is checked for dangling references before it is returned.

use crate::step::{context_length_unit_mm, length_unit_mm, parse_part21, Entity, Param};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

// ------------------------------------------------------------ recognition

/// Kind of fastener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Kind {
    Screw,
    Washer,
    Nut,
    Rivet,
    Pin,
    Insert,
    Standoff,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Screw => "screw",
            Kind::Washer => "washer",
            Kind::Nut => "nut",
            Kind::Rivet => "rivet",
            Kind::Pin => "pin",
            Kind::Insert => "insert",
            Kind::Standoff => "standoff",
        }
    }
}

/// One part that may be a fastener.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    /// PRODUCT_DEFINITION entity id (what is removed).
    pub id: u64,
    pub name: String,
    pub kind: Kind,
    /// 0 … 1.
    pub score: f64,
    /// Selected for removal by default.
    pub suggested: bool,
    /// Instances in the whole assembly.
    pub instances: usize,
    /// Local bounding box, sorted extents, mm (None if no geometry was found).
    pub size_mm: Option<[f64; 3]>,
    /// Why it was classified this way.
    pub reasons: Vec<String>,
    /// Recognised from its name (not only its shape).
    pub by_name: bool,
}

/// Result of a scan.
#[derive(Debug, Clone, Serialize)]
pub struct Scan {
    pub parts: usize,
    pub assemblies: usize,
    pub unit_mm: f64,
    /// Sorted by score, highest first.
    pub candidates: Vec<Candidate>,
}

/// Score from which a part is selected by default.
pub const SUGGEST: f64 = 0.6;
/// Lowest score listed.
pub const LIST: f64 = 0.3;

/// Words that name a fastener kind (upper case, whole words).
const WORDS: &[(Kind, &[&str])] = &[
    (
        Kind::Screw,
        &[
            "VITE",
            "VITI",
            "SCREW",
            "SCREWS",
            "BOLT",
            "BOLTS",
            "BULLONE",
            "BULLONI",
            "TCEI",
            "TBEI",
            "TSPEI",
            "TCCE",
            "TPSEI",
            "SHCS",
            "BHCS",
            "FHCS",
            "SHSS",
            "PHMS",
            "FHMS",
            "SCHRAUBE",
            "SCHRAUBEN",
            "TORNILLO",
            "TORNILLOS",
            "VIS",
            "GRANO",
            "GRUB",
            "CAPSCREW",
            "SETSCREW",
            "THUMBSCREW",
            "PROLUNGA",
        ],
    ),
    (
        Kind::Washer,
        &[
            "RONDELLA",
            "RONDELLE",
            "ROSETTA",
            "ROSETTE",
            "WASHER",
            "WASHERS",
            "GROWER",
            "SCHEIBE",
            "ARANDELA",
            "SPRINGWASHER",
            "LOCKWASHER",
            "BELLEVILLE",
        ],
    ),
    (
        Kind::Nut,
        &[
            "DADO", "DADI", "NUT", "NUTS", "MUTTER", "ECROU", "TUERCA", "HEXNUT", "LOCKNUT",
        ],
    ),
    (
        Kind::Rivet,
        &[
            "RIVET", "RIVETS", "RIVETTO", "RIVETTI", "NIET", "NIETE", "REMACHE",
        ],
    ),
    (
        Kind::Pin,
        &[
            "SPINA",
            "SPINE",
            "DOWEL",
            "COPIGLIA",
            "ROLLPIN",
            "SPLITPIN",
            "SPANNSTIFT",
            "ZYLINDERSTIFT",
        ],
    ),
    (
        Kind::Insert,
        &[
            "PEM",
            "HELICOIL",
            "INSERTO",
            "INSERTI",
            "CLINCH",
            "RIVNUT",
            "GEWINDEEINSATZ",
        ],
    ),
    (
        Kind::Standoff,
        &[
            "STANDOFF",
            "STANDOFFS",
            "SPACER",
            "SPACERS",
            "DISTANZIALE",
            "DISTANZIALI",
            "COLONNETTA",
            "COLONNETTE",
            "ABSTANDSBOLZEN",
            "ENTRETOISE",
            "SEPARADOR",
        ],
    ),
];

/// Fastener standards by number (ISO / DIN / UNI / EN ISO).
const STANDARDS: &[(Kind, &[u32])] = &[
    (
        Kind::Screw,
        &[
            4762, 4014, 4017, 7380, 7045, 7046, 7047, 1207, 1580, 10642, 2009, 2010, 4026, 4027,
            4028, 4029, 14579, 14580, 14583, 14584, 7049, 7050, 7051, 912, 931, 933, 965, 7985,
            7991, 84, 85, 963, 964, 916, 913, 914, 7984, 6912, 5931, 5739, 5737, 7687, 6107, 6109,
            5933, 6954, 6955, 7500, 7513,
        ],
    ),
    (
        Kind::Washer,
        &[
            7089, 7090, 7091, 7092, 7093, 10673, 125, 127, 433, 6798, 6796, 9021, 7980, 6592, 1751,
            6593, 8842,
        ],
    ),
    (
        Kind::Nut,
        &[
            4032, 4033, 4034, 4035, 7040, 10511, 10512, 934, 985, 439, 982, 1587, 6923, 562, 5588,
            5589, 7473, 7474,
        ],
    ),
    (Kind::Rivet, &[15977, 15983, 7337, 660, 661]),
    (Kind::Pin, &[2338, 8734, 8752, 1234, 8735, 6325, 1481, 94]),
];

/// Names that block a geometry-only match.
const BLOCK_WORDS: &[&str] = &[
    "PCB",
    "PWB",
    "BOARD",
    "SCHEDA",
    "CIRCUITO",
    "LAYOUT",
    "CAP",
    "CAPACITOR",
    "CONDENSATORE",
    "RESISTOR",
    "RESISTENZA",
    "LED",
    "DIODE",
    "DIODO",
    "CONNECTOR",
    "CONNETTORE",
    "INDUCTOR",
    "CRYSTAL",
    "QUARZO",
    "FUSE",
    "FUSIBILE",
    "RELAY",
    "RELE",
    "HEATSINK",
    "DISSIPATORE",
];

fn words(s: &str) -> Vec<String> {
    s.to_ascii_uppercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Thread designation: M3, M2.5x6, M3X8, #4-40, 1/4-20.
fn thread(name: &str) -> Option<(String, bool)> {
    let u = name.to_ascii_uppercase();
    let b = u.as_bytes();
    let n = b.len();
    for i in 0..n {
        let prev_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if b[i] == b'M' && prev_ok && i + 1 < n && b[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < n && (b[j].is_ascii_digit() || b[j] == b'.' || b[j] == b',') {
                j += 1;
            }
            let dia: f64 = u[i + 1..j].replace(',', ".").parse().unwrap_or(0.0);
            if !(1.0..=24.0).contains(&dia) {
                continue;
            }
            // optional length: x8, X 8, ×8
            let mut k = j;
            while k < n && b[k] == b' ' {
                k += 1;
            }
            let has_len = k < n
                && (b[k] == b'X' || u[k..].starts_with('×'))
                && u[k..]
                    .chars()
                    .skip(1)
                    .find(|c| *c != ' ')
                    .is_some_and(|c| c.is_ascii_digit());
            let end_ok = j == n || !b[j].is_ascii_alphabetic() || b[j] == b'X';
            if end_ok {
                return Some((u[i..j].to_string(), has_len));
            }
        }
        if b[i] == b'#' && i + 3 < n && b[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < n && b[j].is_ascii_digit() {
                j += 1;
            }
            if j + 2 < n && b[j] == b'-' && b[j + 1].is_ascii_digit() && b[j + 2].is_ascii_digit() {
                return Some((u[i..j + 3].to_string(), true));
            }
        }
    }
    None
}

/// Standard designations: ISO 4762, DIN912, UNI 5931, EN ISO 7089.
fn standards(name: &str) -> Vec<(String, u32)> {
    let w = words(name);
    let mut out = Vec::new();
    for (i, t) in w.iter().enumerate() {
        for org in ["ISO", "DIN", "UNI", "EN"] {
            let num = if t == org {
                // "ISO 4762", "EN ISO 7089"
                w.get(i + 1)
                    .filter(|n| n.chars().all(|c| c.is_ascii_digit()))
                    .cloned()
            } else if let Some(rest) = t.strip_prefix(org) {
                // "DIN912" (digits only: ISO6541FDBQR is a chip, not ISO 6541)
                rest.chars()
                    .all(|c| c.is_ascii_digit())
                    .then(|| rest.to_string())
                    .filter(|r| !r.is_empty())
            } else {
                None
            };
            if let Some(n) = num.and_then(|n| n.parse::<u32>().ok()) {
                out.push((org.to_string(), n));
            }
        }
    }
    out
}

/// Score and kind from names and local geometry.
/// `geo`: sorted local extents and the largest circle diameter, mm.
fn classify(
    name: &str,
    geo: Option<([f64; 3], f64)>,
    instances: usize,
) -> Option<(Kind, f64, Vec<String>, bool)> {
    let w = words(name);
    let joined: String = w.concat();
    let mut score: HashMap<Kind, f64> = HashMap::new();
    let mut reasons = Vec::new();
    let mut by_name = false;
    for (k, list) in WORDS {
        for &kw in *list {
            // whole word, or glued words such as "LOCKWASHER", "VITETCEI"
            let hit = w.iter().any(|x| x == kw) || (kw.len() >= 6 && joined.contains(kw));
            if hit {
                *score.entry(*k).or_default() += 0.7;
                reasons.push(format!("name: {}", kw.to_lowercase()));
                by_name = true;
                break;
            }
        }
    }
    for (org, n) in standards(name) {
        if let Some((k, _)) = STANDARDS.iter().find(|(_, nums)| nums.contains(&n)) {
            *score.entry(*k).or_default() += 0.7;
            reasons.push(format!("standard: {org} {n}"));
            by_name = true;
        }
    }
    let thr = thread(name);
    if let Some((t, with_len)) = &thr {
        let e = score.entry(Kind::Screw).or_default();
        if *with_len {
            *e += 0.5;
            reasons.push(format!("thread: {t} × length"));
            by_name = true;
        } else {
            *e += 0.2;
            reasons.push(format!("thread: {t}"));
        }
    }
    let blocked = !by_name
        && (w.iter().any(|x| BLOCK_WORDS.contains(&x.as_str()))
            || w.first().is_some_and(|f| {
                // reference designators: R12, C3, U1, J5, TP2, LED4
                let letters = f.chars().take_while(|c| c.is_ascii_alphabetic()).count();
                (1..=3).contains(&letters)
                    && f.len() > letters
                    && f[letters..].chars().all(|c| c.is_ascii_digit())
            }));
    if blocked {
        return None;
    }
    if let Some(([a, b, c], circle)) = geo {
        if (2.0..=80.0).contains(&c) && a >= 0.2 {
            let close = |x: f64, y: f64| (x - y).abs() <= 0.06 * y.max(1e-9);
            // really round: a circle as large as the outline (a square chip
            // has equal sides too)
            let round_d = |d: f64| circle > 0.0 && (circle - d).abs() <= 0.1 * d;
            // geometry evidence is capped below the suggestion threshold
            let (k, g, why) = if a <= 0.3 * b && close(b, c) && round_d(c) && c <= 40.0 {
                (Kind::Washer, 0.4, "thin round disc")
            } else if (1.10..=1.20).contains(&(c / b))
                && a >= 0.3 * b
                && a <= 1.2 * c
                && c <= 40.0
                && circle > 0.0
                && circle < b
            {
                (Kind::Nut, 0.35, "hexagonal outline with a hole")
            } else if close(a, b) && round_d(b) && c >= 1.3 * b && b <= 16.0 {
                (Kind::Screw, 0.3, "round and elongated")
            } else {
                (Kind::Screw, 0.0, "")
            };
            if g > 0.0 {
                let e = score.entry(k).or_default();
                if by_name {
                    *e += 0.15;
                } else {
                    *e += g;
                }
                reasons.push(format!("shape: {why} ({a:.1} × {b:.1} × {c:.1} mm)"));
            }
            if by_name && c > 80.0 {
                reasons.push("large for a fastener".into());
            }
        } else if by_name && c > 80.0 {
            // a 200 mm "screw" is more likely a lead screw or a wrongly named part
            for v in score.values_mut() {
                *v -= 0.3;
            }
            reasons.push(format!("large for a fastener ({c:.0} mm)"));
        }
    }
    if instances >= 4 && !score.is_empty() {
        for v in score.values_mut() {
            *v += 0.05;
        }
        reasons.push(format!("{instances} instances"));
    }
    let (k, s) = score
        .into_iter()
        .max_by(|a, b| a.1.total_cmp(&b.1).then((b.0 as u8).cmp(&(a.0 as u8))))?;
    Some((k, s.min(1.0), reasons, by_name))
}

/// Leading part-number digits of a name ("2146-0514_ROSETTA" → "2146"), at
/// least four.
fn family(name: &str) -> Option<&str> {
    let n = name.trim();
    let d = n.bytes().take_while(u8::is_ascii_digit).count();
    (d >= 4).then(|| &n[..4])
}

// ------------------------------------------------------------ structure

/// The product structure needed here, from the parsed entities.
struct Structure {
    /// PD → (display name, all names joined for matching).
    names: HashMap<u64, (String, String)>,
    /// parent PD → child PDs (one entry per occurrence).
    children: HashMap<u64, Vec<u64>>,
    /// child PD → instance names.
    inst_names: HashMap<u64, Vec<String>>,
    /// PD → shape representations (with those linked by plain relationships).
    reps: HashMap<u64, Vec<u64>>,
    pds: Vec<u64>,
}

fn rec<'a>(e: &'a HashMap<u64, Entity>, id: u64, name: &str) -> Option<&'a [Param]> {
    e.get(&id)?
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, p)| p.as_slice())
}

fn has(e: &HashMap<u64, Entity>, id: u64, name: &str) -> bool {
    e.get(&id).is_some_and(|x| x.iter().any(|(n, _)| n == name))
}

fn structure(e: &HashMap<u64, Entity>) -> Structure {
    let mut names = HashMap::new();
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut inst_names: HashMap<u64, Vec<String>> = HashMap::new();
    let mut pds = Vec::new();
    let mut pds_of: HashMap<u64, u64> = HashMap::new(); // PDS → PD
    let mut sr_of_pds: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut srr: Vec<(u64, u64)> = Vec::new();
    for (&id, ent) in e {
        for (n, p) in ent {
            match n.as_str() {
                "PRODUCT_DEFINITION" | "PRODUCT_DEFINITION_WITH_ASSOCIATED_DOCUMENTS" => {
                    let pdf = p.get(2).and_then(Param::as_ref);
                    let prod = pdf
                        .and_then(|f| {
                            e.get(&f)?
                                .iter()
                                .find(|(n, _)| n.starts_with("PRODUCT_DEFINITION_FORMATION"))
                                .and_then(|(_, q)| q.get(2)?.as_ref())
                        })
                        .and_then(|pr| rec(e, pr, "PRODUCT"));
                    let (id_s, nm, desc) = prod
                        .map(|q| {
                            (
                                q.first().map(|x| x.as_str()).unwrap_or(""),
                                q.get(1).map(|x| x.as_str()).unwrap_or(""),
                                q.get(2).map(|x| x.as_str()).unwrap_or(""),
                            )
                        })
                        .unwrap_or(("", "", ""));
                    let disp = if !nm.is_empty() { nm } else { id_s };
                    let pd_desc = p.get(1).map(|x| x.as_str()).unwrap_or("");
                    let all = format!("{id_s} {nm} {desc} {pd_desc}");
                    names.insert(id, (disp.to_string(), all));
                    pds.push(id);
                }
                "NEXT_ASSEMBLY_USAGE_OCCURRENCE" => {
                    if let (Some(a), Some(b)) = (
                        p.get(3).and_then(Param::as_ref),
                        p.get(4).and_then(Param::as_ref),
                    ) {
                        children.entry(a).or_default().push(b);
                        let iname = format!(
                            "{} {}",
                            p.get(1).map(|x| x.as_str()).unwrap_or(""),
                            p.get(2).map(|x| x.as_str()).unwrap_or("")
                        );
                        inst_names.entry(b).or_default().push(iname);
                    }
                }
                "PRODUCT_DEFINITION_SHAPE" => {
                    if let Some(d) = p.get(2).and_then(Param::as_ref) {
                        pds_of.insert(id, d);
                    }
                }
                "SHAPE_DEFINITION_REPRESENTATION" => {
                    if let (Some(d), Some(r)) = (
                        p.first().and_then(Param::as_ref),
                        p.get(1).and_then(Param::as_ref),
                    ) {
                        sr_of_pds.entry(d).or_default().push(r);
                    }
                }
                // plain relationship (no transformation): same part, more geometry
                "SHAPE_REPRESENTATION_RELATIONSHIP"
                    if !ent.iter().any(|(m, _)| m.contains("WITH_TRANSFORMATION")) =>
                {
                    if let (Some(a), Some(b)) = (
                        p.get(2).and_then(Param::as_ref),
                        p.get(3).and_then(Param::as_ref),
                    ) {
                        srr.push((a, b));
                    }
                }
                _ => {}
            }
        }
    }
    let mut reps: HashMap<u64, Vec<u64>> = HashMap::new();
    for (pds_id, srs) in &sr_of_pds {
        if let Some(pd) = pds_of.get(pds_id) {
            // only shapes of a product definition (not of an occurrence)
            if names.contains_key(pd) {
                let mut v = srs.clone();
                let mut k = 0;
                while k < v.len() {
                    let cur = v[k];
                    for &(a, b) in &srr {
                        for (x, y) in [(a, b), (b, a)] {
                            if x == cur && !v.contains(&y) {
                                v.push(y);
                            }
                        }
                    }
                    k += 1;
                }
                reps.entry(*pd).or_default().extend(v);
            }
        }
    }
    pds.sort_unstable();
    Structure {
        names,
        children,
        inst_names,
        reps,
        pds,
    }
}

/// Local bounding box of a part's own geometry, sorted extents in mm.
fn part_size(e: &HashMap<u64, Entity>, reps: &[u64], unit_global: f64) -> Option<([f64; 3], f64)> {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    let mut max_r = 0.0f64;
    let mut seen = HashSet::new();
    let mut stack: Vec<u64> = Vec::new();
    let mut unit = unit_global;
    for &r in reps {
        if let Some((_, p)) = e.get(&r).and_then(|x| x.first()) {
            if let Some(u) = p
                .get(2)
                .and_then(Param::as_ref)
                .and_then(|c| context_length_unit_mm(e, c))
            {
                unit = u;
            }
            for it in p.get(1).map(|x| x.as_list()).unwrap_or(&[]) {
                if let Some(id) = it.as_ref() {
                    stack.push(id);
                }
            }
        }
    }
    let point = |id: u64| -> Option<[f64; 3]> {
        let p = rec(e, id, "CARTESIAN_POINT")?;
        let c = p.get(1)?.as_list();
        Some([0, 1, 2].map(|k| c.get(k).and_then(Param::as_num).unwrap_or(0.0)))
    };
    let dir = |id: Option<u64>| -> Option<[f64; 3]> {
        let p = rec(e, id?, "DIRECTION")?;
        let c = p.get(1)?.as_list();
        let v = [0, 1, 2].map(|k| c.get(k).and_then(Param::as_num).unwrap_or(0.0));
        let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        (n > 0.0).then(|| v.map(|x| x / n))
    };
    let mut add = |p: [f64; 3], r: [f64; 3]| {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k] - r[k]);
            hi[k] = hi[k].max(p[k] + r[k]);
        }
    };
    let mut steps = 0usize;
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        steps += 1;
        if steps > 2_000_000 {
            break;
        }
        let Some(ent) = e.get(&id) else { continue };
        for (n, p) in ent {
            match n.as_str() {
                // do not walk into other parts or placements
                "MAPPED_ITEM"
                | "REPRESENTATION_MAP"
                | "AXIS2_PLACEMENT_3D"
                | "AXIS2_PLACEMENT_2D"
                | "STYLED_ITEM"
                | "PRESENTATION_STYLE_ASSIGNMENT" => continue,
                "VERTEX_POINT" => {
                    if let Some(pt) = p.get(1).and_then(Param::as_ref).and_then(point) {
                        add(pt, [0.0; 3]);
                    }
                    continue;
                }
                "CIRCLE" | "ELLIPSE" => {
                    let ax = p.get(1).and_then(Param::as_ref);
                    let r = p
                        .get(2)
                        .and_then(Param::as_num)
                        .unwrap_or(0.0)
                        .max(p.get(3).and_then(Param::as_num).unwrap_or(0.0));
                    if let Some(a) = ax.and_then(|a| rec(e, a, "AXIS2_PLACEMENT_3D")) {
                        if let Some(o) = a.get(1).and_then(Param::as_ref).and_then(point) {
                            let z =
                                dir(a.get(2).and_then(Param::as_ref)).unwrap_or([0.0, 0.0, 1.0]);
                            add(o, z.map(|zk| r * (1.0 - zk * zk).max(0.0).sqrt()));
                            max_r = max_r.max(r);
                        }
                    }
                    continue;
                }
                n if n.starts_with("B_SPLINE") || n == "POLYLINE" || n == "POLY_LOOP" => {
                    for q in p {
                        for c in q.as_list() {
                            for id2 in std::iter::once(c).chain(c.as_list().iter()) {
                                if let Some(pt) = id2.as_ref().and_then(point) {
                                    add(pt, [0.0; 3]);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            fn refs(p: &Param, out: &mut Vec<u64>) {
                match p {
                    Param::Ref(r) => out.push(*r),
                    Param::List(v) => v.iter().for_each(|x| refs(x, out)),
                    Param::Typed(_, b) => refs(b, out),
                    _ => {}
                }
            }
            for q in p {
                refs(q, &mut stack);
            }
        }
    }
    if !lo[0].is_finite() {
        return None;
    }
    let mut s = [0, 1, 2].map(|k| (hi[k] - lo[k]) * unit);
    s.sort_by(f64::total_cmp);
    Some((s, 2.0 * max_r * unit))
}

/// Find the fasteners of a STEP file.
pub fn scan(text: &str) -> Result<Scan, String> {
    let e = parse_part21(text)?;
    let unit = length_unit_mm(&e);
    let st = structure(&e);
    // instances: product of occurrence counts from the roots down
    let mut parents: HashMap<u64, usize> = HashMap::new();
    for kids in st.children.values() {
        for k in kids {
            *parents.entry(*k).or_default() += 1;
        }
    }
    let mut count: HashMap<u64, usize> = HashMap::new();
    let mut stack: Vec<(u64, usize)> = st
        .pds
        .iter()
        .filter(|p| !parents.contains_key(p))
        .map(|&p| (p, 1))
        .collect();
    let mut guard = 0usize;
    while let Some((p, n)) = stack.pop() {
        *count.entry(p).or_default() += n;
        guard += 1;
        if guard > 5_000_000 {
            break;
        }
        if let Some(kids) = st.children.get(&p) {
            for &k in kids {
                stack.push((k, n));
            }
        }
    }
    let mut out = Vec::new();
    let mut parts = 0;
    for &pd in &st.pds {
        if st.children.get(&pd).is_some_and(|k| !k.is_empty()) {
            continue; // assemblies are not classified themselves
        }
        parts += 1;
        let (disp, all) = st.names.get(&pd).cloned().unwrap_or_default();
        let inst = st
            .inst_names
            .get(&pd)
            .map(|v| v.join(" "))
            .unwrap_or_default();
        let geo = st.reps.get(&pd).and_then(|r| part_size(&e, r, unit));
        let size = geo.map(|g| g.0);
        let n = count.get(&pd).copied().unwrap_or(1);
        if let Some((kind, score, reasons, by_name)) = classify(&format!("{all} {inst}"), geo, n) {
            if score >= LIST {
                out.push(Candidate {
                    id: pd,
                    name: if disp.is_empty() {
                        format!("#{pd}")
                    } else {
                        disp
                    },
                    kind,
                    score,
                    suggested: false,
                    instances: n,
                    size_mm: size,
                    reasons,
                    by_name,
                });
            }
        }
    }
    // parts numbered in the same family as a fastener recognised by name
    // (company codes such as 2146…) are likely fasteners too
    let families: HashMap<String, String> = out
        .iter()
        .filter(|c| c.by_name && c.score >= SUGGEST)
        .filter_map(|c| family(&c.name).map(|f| (f.to_string(), c.name.clone())))
        .collect();
    for c in out.iter_mut().filter(|c| !c.by_name) {
        if let Some(src) = family(&c.name).and_then(|f| families.get(f)) {
            c.score = (c.score + 0.25).min(1.0);
            c.reasons.push(format!("same part-number family as {src}"));
        }
    }
    for c in &mut out {
        c.suggested = c.score >= SUGGEST && c.kind != Kind::Standoff;
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.name.cmp(&b.name)));
    Ok(Scan {
        parts,
        assemblies: st.pds.len() - parts,
        unit_mm: unit,
        candidates: out,
    })
}

// ------------------------------------------------------------ Part 21 surgery

/// A parenthesised list inside a record.
#[derive(Debug, Clone)]
struct Group {
    open: usize,
    close: usize,
    depth: u32,
    /// Element spans (absolute, between separators).
    elems: Vec<(usize, usize)>,
}

#[derive(Debug, Clone)]
struct RefTok {
    id: u64,
    start: usize,
    end: usize,
    group: usize,
}

#[derive(Debug, Clone)]
struct Rec {
    id: u64,
    start: usize,
    /// Exclusive, including ';' and one line break.
    end: usize,
    complex: bool,
    groups: Vec<Group>,
    refs: Vec<RefTok>,
}

/// Split the DATA section into records with their references.
fn scan_records(text: &str) -> Result<(usize, Vec<Rec>, usize), String> {
    let b = text.as_bytes();
    let data = text
        .find("DATA;")
        .ok_or("not a STEP file (no DATA section)")?
        + 5;
    let mut i = data;
    let mut recs = Vec::new();
    let n = b.len();
    loop {
        // whitespace and comments
        loop {
            while i < n && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if b[i..].starts_with(b"/*") {
                i = text[i..].find("*/").map(|k| i + k + 2).unwrap_or(n);
            } else {
                break;
            }
        }
        if i >= n || b[i..].starts_with(b"ENDSEC") {
            break;
        }
        if b[i] != b'#' {
            return Err(format!("unexpected text at byte {i} in DATA"));
        }
        let start = i;
        i += 1;
        let ds = i;
        while i < n && b[i].is_ascii_digit() {
            i += 1;
        }
        let id: u64 = text[ds..i]
            .parse()
            .map_err(|_| format!("bad id at byte {start}"))?;
        while i < n && b[i] != b'=' {
            i += 1;
        }
        i += 1;
        while i < n && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let complex = b.get(i) == Some(&b'(');
        let mut groups: Vec<Group> = Vec::new();
        let mut stack: Vec<(usize, usize)> = Vec::new(); // (group index, elem start)
        let mut refs = Vec::new();
        loop {
            if i >= n {
                return Err(format!("record #{id} not terminated"));
            }
            match b[i] {
                b'\'' => {
                    i += 1;
                    while i < n {
                        if b[i] == b'\'' {
                            if b.get(i + 1) == Some(&b'\'') {
                                i += 2;
                                continue;
                            }
                            break;
                        }
                        i += 1;
                    }
                    i += 1;
                }
                b'(' => {
                    groups.push(Group {
                        open: i,
                        close: i,
                        depth: stack.len() as u32 + 1,
                        elems: Vec::new(),
                    });
                    stack.push((groups.len() - 1, i + 1));
                    i += 1;
                }
                b',' => {
                    if let Some((g, es)) = stack.last_mut() {
                        groups[*g].elems.push((*es, i));
                        *es = i + 1;
                    }
                    i += 1;
                }
                b')' => {
                    if let Some((g, es)) = stack.pop() {
                        if !text[es..i].trim().is_empty() || !groups[g].elems.is_empty() {
                            groups[g].elems.push((es, i));
                        }
                        groups[g].close = i;
                    }
                    i += 1;
                }
                b'#' => {
                    let s = i;
                    i += 1;
                    let d = i;
                    while i < n && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    if let (Ok(r), Some((g, _))) = (text[d..i].parse::<u64>(), stack.last()) {
                        refs.push(RefTok {
                            id: r,
                            start: s,
                            end: i,
                            group: *g,
                        });
                    }
                }
                b';' if stack.is_empty() => {
                    i += 1;
                    if b[i..].starts_with(b"\r\n") {
                        i += 2;
                    } else if b.get(i) == Some(&b'\n') {
                        i += 1;
                    }
                    break;
                }
                _ => i += 1,
            }
        }
        recs.push(Rec {
            id,
            start,
            end: i,
            complex,
            groups,
            refs,
        });
    }
    Ok((data, recs, i))
}

/// What a removal did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Removal {
    /// Names of the removed parts (and assemblies left empty).
    pub parts: Vec<String>,
    /// Assembly occurrences removed.
    pub occurrences: usize,
    /// Records deleted / edited (a list entry dropped).
    pub deleted_records: usize,
    pub edited_records: usize,
    pub bytes_before: usize,
    pub bytes_after: usize,
}

/// Write the file without the given part definitions (PRODUCT_DEFINITION ids).
/// Assemblies left without children are removed too.
pub fn remove(text: &str, part_ids: &[u64]) -> Result<(String, Removal), String> {
    let e = parse_part21(text)?;
    let st = structure(&e);
    let (_, recs, tail_start) = scan_records(text)?;
    let index: HashMap<u64, usize> = recs.iter().enumerate().map(|(k, r)| (r.id, k)).collect();
    let mut referrers: HashMap<u64, Vec<usize>> = HashMap::new();
    for (k, r) in recs.iter().enumerate() {
        for t in &r.refs {
            referrers.entry(t.id).or_default().push(k);
        }
    }
    let mut selected: HashSet<u64> = part_ids.iter().copied().collect();
    for p in &selected {
        if !st.names.contains_key(p) {
            return Err(format!("#{p} is not a part definition of this file"));
        }
    }
    // assemblies whose children are all removed go too (repeat upwards)
    loop {
        let mut more = Vec::new();
        for (parent, kids) in &st.children {
            if !selected.contains(parent)
                && !kids.is_empty()
                && kids.iter().all(|k| selected.contains(k))
            {
                more.push(*parent);
            }
        }
        let roots: Vec<u64> = st
            .pds
            .iter()
            .copied()
            .filter(|p| !st.children.values().any(|v| v.contains(p)))
            .collect();
        more.retain(|p| !roots.contains(p)); // never the top assembly
        if more.is_empty() {
            break;
        }
        selected.extend(more);
    }

    // seeds: the part definitions, their formation and product when unused
    // elsewhere, and their shape representations
    let mut deleted: HashSet<u64> = HashSet::new();
    let mut queue: Vec<u64> = Vec::new();
    let del = |id: u64, deleted: &mut HashSet<u64>, queue: &mut Vec<u64>| {
        if index.contains_key(&id) && deleted.insert(id) {
            queue.push(id);
        }
    };
    for &pd in &selected {
        del(pd, &mut deleted, &mut queue);
        if let Some(reps) = st.reps.get(&pd) {
            for &r in reps {
                // a representation shared with a kept part stays
                let shared = st
                    .reps
                    .iter()
                    .any(|(q, rs)| !selected.contains(q) && rs.contains(&r));
                if !shared {
                    del(r, &mut deleted, &mut queue);
                }
            }
        }
        let pdf = e
            .get(&pd)
            .and_then(|x| x.first())
            .and_then(|(_, p)| p.get(2)?.as_ref());
        if let Some(f) = pdf {
            let used = st.pds.iter().any(|q| {
                !selected.contains(q)
                    && e.get(q)
                        .and_then(|x| x.first())
                        .and_then(|(_, p)| p.get(2)?.as_ref())
                        == Some(f)
            });
            if !used {
                del(f, &mut deleted, &mut queue);
                let prod = e
                    .get(&f)
                    .and_then(|x| x.first())
                    .and_then(|(_, p)| p.get(2)?.as_ref());
                if let Some(pr) = prod {
                    let other_pdf = referrers.get(&pr).is_some_and(|v| {
                        v.iter().any(|&k| {
                            let id = recs[k].id;
                            id != f && has(&e, id, "PRODUCT_DEFINITION_FORMATION")
                                || id != f
                                    && has(
                                        &e,
                                        id,
                                        "PRODUCT_DEFINITION_FORMATION_WITH_SPECIFIED_SOURCE",
                                    )
                        })
                    });
                    if !other_pdf {
                        del(pr, &mut deleted, &mut queue);
                    }
                }
            }
        }
    }

    // cascade: records pointing at deleted ones lose that entry or go too;
    // records only used by deleted ones go too
    let original_referred: HashSet<u64> = referrers.keys().copied().collect();
    let mut drop_elems: HashMap<usize, HashSet<(usize, usize)>> = HashMap::new(); // rec → (group, elem)
    let mut qi = 0;
    while qi < queue.len() {
        let d = queue[qi];
        qi += 1;
        // referrers
        if let Some(rs) = referrers.get(&d) {
            for &k in rs {
                let r = &recs[k];
                if deleted.contains(&r.id) {
                    continue;
                }
                let base = if r.complex { 2 } else { 1 };
                let mut kill = false;
                for t in r.refs.iter().filter(|t| t.id == d) {
                    let g = &r.groups[t.group];
                    if g.depth <= base {
                        kill = true;
                        break;
                    }
                    let el = g
                        .elems
                        .iter()
                        .position(|&(s, en)| s <= t.start && t.end <= en)
                        .unwrap_or(usize::MAX);
                    let bare = el != usize::MAX && {
                        let (s, en) = g.elems[el];
                        text[s..en].trim() == &text[t.start..t.end]
                    };
                    if !bare {
                        kill = true;
                        break;
                    }
                    drop_elems.entry(k).or_default().insert((t.group, el));
                }
                if !kill {
                    // a list emptied completely: the record cannot stay
                    let drops = drop_elems.get(&k).cloned().unwrap_or_default();
                    for (gi, g) in r.groups.iter().enumerate() {
                        let n_drop = drops.iter().filter(|(x, _)| *x == gi).count();
                        if n_drop > 0 && n_drop >= g.elems.len() {
                            kill = true;
                        }
                    }
                }
                if kill {
                    del(r.id, &mut deleted, &mut queue);
                }
            }
        }
        // referents that only deleted records used
        if let Some(&k) = index.get(&d) {
            for t in &recs[k].refs {
                if deleted.contains(&t.id) || !original_referred.contains(&t.id) {
                    continue;
                }
                // presentation records (colours, layers, visibility) do not
                // keep geometry alive: they lose the entry or go with it
                let all_dead = referrers.get(&t.id).is_some_and(|v| {
                    v.iter()
                        .all(|&j| deleted.contains(&recs[j].id) || presentation(&e, recs[j].id))
                });
                if all_dead {
                    del(t.id, &mut deleted, &mut queue);
                }
            }
        }
    }
    // safety: only the selected product definitions may disappear
    for &id in &deleted {
        if (has(&e, id, "PRODUCT_DEFINITION")
            || has(&e, id, "PRODUCT_DEFINITION_WITH_ASSOCIATED_DOCUMENTS"))
            && !selected.contains(&id)
        {
            let nm = st.names.get(&id).map(|x| x.0.clone()).unwrap_or_default();
            return Err(format!(
                "removing the selection would also remove part '{nm}' (#{id}); nothing was written"
            ));
        }
    }

    // write
    let mut out = String::with_capacity(text.len());
    let first = recs.first().map(|r| r.start).unwrap_or(tail_start);
    out.push_str(&text[..first]);
    let mut rep = Removal {
        bytes_before: text.len(),
        ..Default::default()
    };
    let mut prev_end = first;
    for (k, r) in recs.iter().enumerate() {
        // text between records (comments, blank lines) stays
        out.push_str(&text[prev_end..r.start]);
        prev_end = r.end;
        if deleted.contains(&r.id) {
            rep.deleted_records += 1;
            if has(&e, r.id, "NEXT_ASSEMBLY_USAGE_OCCURRENCE") {
                rep.occurrences += 1;
            }
            continue;
        }
        match drop_elems.get(&k) {
            Some(drops) if !drops.is_empty() => {
                rep.edited_records += 1;
                out.push_str(&edit_record(text, r, drops));
            }
            _ => out.push_str(&text[r.start..r.end]),
        }
    }
    out.push_str(&text[prev_end..]);
    rep.bytes_after = out.len();
    let mut names: Vec<String> = selected
        .iter()
        .map(|p| {
            st.names
                .get(p)
                .map(|x| x.0.clone())
                .unwrap_or_else(|| format!("#{p}"))
        })
        .collect();
    names.sort();
    rep.parts = names;
    check_references(&out)?;
    Ok((out, rep))
}

/// Records that only decorate others (styles, layers, visibility).
fn presentation(e: &HashMap<u64, Entity>, id: u64) -> bool {
    e.get(&id).is_some_and(|x| {
        x.iter().any(|(n, _)| {
            matches!(
                n.as_str(),
                "STYLED_ITEM"
                    | "OVER_RIDING_STYLED_ITEM"
                    | "CONTEXT_DEPENDENT_OVER_RIDING_STYLED_ITEM"
                    | "ANNOTATION_OCCURRENCE"
                    | "PRESENTATION_LAYER_ASSIGNMENT"
                    | "INVISIBILITY"
                    | "CONTEXT_DEPENDENT_INVISIBILITY"
            )
        })
    })
}

/// The record text with some list elements dropped.
fn edit_record(text: &str, r: &Rec, drops: &HashSet<(usize, usize)>) -> String {
    // rebuild each touched group from its kept elements; groups are handled
    // from the innermost / rightmost so outer spans stay valid
    let mut s = text[r.start..r.end].to_string();
    let mut touched: Vec<usize> = drops.iter().map(|(g, _)| *g).collect();
    touched.sort_unstable();
    touched.dedup();
    touched.sort_by_key(|&g| std::cmp::Reverse(r.groups[g].open));
    for gi in touched {
        let g = &r.groups[gi];
        let kept: Vec<&str> = g
            .elems
            .iter()
            .enumerate()
            .filter(|(ei, _)| !drops.contains(&(gi, *ei)))
            .map(|(_, &(a, b))| &text[a..b])
            .collect();
        let (a, b) = (g.open + 1 - r.start, g.close - r.start);
        // keep the original separator style (",", ", ")
        let sep = if text[g.open..g.close].contains(", ") {
            ", "
        } else {
            ","
        };
        let body: Vec<&str> = kept.iter().map(|x| x.trim()).collect();
        s.replace_range(a..b, &body.join(sep));
    }
    s
}

/// Every #reference points at an existing record.
pub fn check_references(text: &str) -> Result<(), String> {
    let (_, recs, _) = scan_records(text)?;
    let ids: HashSet<u64> = recs.iter().map(|r| r.id).collect();
    for r in &recs {
        for t in &r.refs {
            if !ids.contains(&t.id) {
                return Err(format!("#{} refers to missing #{}", r.id, t.id));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_classified() {
        let c = |n: &str| classify(n, None, 1).map(|x| (x.0, x.1 >= SUGGEST));
        assert_eq!(family("2146-0514_ROSETTA"), Some("2146"));
        assert_eq!(family("214604502"), Some("2146"));
        assert_eq!(family("21x"), None);
        assert_eq!(c("VITE TCEI M3X8 UNI 5931"), Some((Kind::Screw, true)));
        assert_eq!(c("ISO 4762 M4 x 12"), Some((Kind::Screw, true)));
        assert_eq!(c("DIN912-M3x10"), Some((Kind::Screw, true)));
        assert_eq!(c("Rondella piana M3 UNI 6592"), Some((Kind::Washer, true)));
        assert_eq!(c("ISO 7089 - 3"), Some((Kind::Washer, true)));
        assert_eq!(c("Hex Nut M3 ISO 4032"), Some((Kind::Nut, true)));
        assert_eq!(c("Distanziale M3x10"), Some((Kind::Standoff, true)));
        assert_eq!(c("lock_washer_m2"), Some((Kind::Washer, true)));
        // chips and components are not fasteners
        assert_eq!(c("ISO6541FDBQR_3"), None);
        assert_eq!(c("C12"), None);
        assert_eq!(c("SCHEDA_LOGICA"), None);
        assert_eq!(c("PIN1_AREA"), None);
        // geometry alone only proposes, and never for designators
        let g = |n: &str, s| classify(n, Some(s), 8);
        let w = g("SOLID-123", ([0.5, 7.0, 7.0], 7.0)).unwrap();
        assert_eq!(w.0, Kind::Washer);
        assert!(w.1 < SUGGEST && w.1 >= LIST);
        assert!(g("C12", ([5.0, 5.0, 11.0], 5.0)).is_none());
        assert!(g("ELKO 10uF CAPACITOR", ([5.0, 5.0, 11.0], 5.0)).is_none());
        // a square BGA is not a washer, a flat footprint is nothing
        assert!(g("LFMXO5-25-9BBG400I", ([1.5, 17.0, 17.0], 0.4)).is_none());
        assert!(g("SOLID-9", ([0.0, 0.8, 0.8], 0.8)).is_none());
        assert_eq!(
            classify("2146-0514_ROSETTA_PER_PERNI_GR", None, 6).map(|x| x.0),
            Some(Kind::Washer)
        );
    }

    /// A small AP214-style assembly written by hand: a chassis, a PCB with a
    /// component, two screws and a washer in a sub-assembly, with colours,
    /// a layer and a product category listing every product.
    pub(crate) fn sample() -> String {
        let mut s = String::from(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('test'),'2;1');\nFILE_NAME('t.stp','2026-01-01T00:00:00',('me'),(''),'x','y','');\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\nENDSEC;\nDATA;\n",
        );
        let mut id = 1u64;
        let mut next = || {
            let v = id;
            id += 1;
            v
        };
        let l = |s: &mut String, line: String| {
            s.push_str(&line);
            s.push('\n');
        };
        let ctx = next();
        l(
            &mut s,
            format!("#{ctx}=APPLICATION_CONTEXT('automotive design');"),
        );
        let pctx = next();
        l(
            &mut s,
            format!("#{pctx}=PRODUCT_CONTEXT('',#{ctx},'mechanical');"),
        );
        let dctx = next();
        l(
            &mut s,
            format!("#{dctx}=PRODUCT_DEFINITION_CONTEXT('part definition',#{ctx},'design');"),
        );
        let mm = next();
        l(
            &mut s,
            format!("#{mm}=(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.));"),
        );
        let gctx = next();
        l(&mut s, format!("#{gctx}=(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNIT_ASSIGNED_CONTEXT((#{mm}))REPRESENTATION_CONTEXT('',''));"));
        let o = next();
        l(&mut s, format!("#{o}=CARTESIAN_POINT('',(0.,0.,0.));"));
        let ax = next();
        l(&mut s, format!("#{ax}=AXIS2_PLACEMENT_3D('',#{o},$,$);"));
        let mut prods = Vec::new();
        let mut styled = Vec::new();
        // part with a box of vertices (lo..hi) or a cylinder (r, h)
        let mut part = |s: &mut String, name: &str, geo: (&str, [f64; 6])| -> (u64, u64) {
            let pr = next();
            l(s, format!("#{pr}=PRODUCT('{name}','{name}','',(#{pctx}));"));
            prods.push(pr);
            let f = next();
            l(
                s,
                format!("#{f}=PRODUCT_DEFINITION_FORMATION('','',#{pr});"),
            );
            let pd = next();
            l(
                s,
                format!("#{pd}=PRODUCT_DEFINITION('design','',#{f},#{dctx});"),
            );
            let pds = next();
            l(s, format!("#{pds}=PRODUCT_DEFINITION_SHAPE('','',#{pd});"));
            let mut items = vec![ax];
            let g = geo.1;
            let solid;
            if geo.0 == "box" {
                let mut vps = Vec::new();
                for &x in &[g[0], g[3]] {
                    for &y in &[g[1], g[4]] {
                        for &z in &[g[2], g[5]] {
                            let p = next();
                            l(
                                s,
                                format!("#{p}=CARTESIAN_POINT('',({x:.1},{y:.1},{z:.1}));"),
                            );
                            let v = next();
                            l(s, format!("#{v}=VERTEX_POINT('',#{p});"));
                            vps.push(v);
                        }
                    }
                }
                solid = next();
                let list: Vec<String> = vps.iter().map(|v| format!("#{v}")).collect();
                l(
                    s,
                    format!("#{solid}=MANIFOLD_SOLID_BREP('{name}',#{});", vps[0]),
                );
                let _ = list;
            } else {
                // cylinder radius g[0], height g[1] along z
                let c0 = next();
                l(s, format!("#{c0}=CARTESIAN_POINT('',(0.,0.,0.));"));
                let z = next();
                l(s, format!("#{z}=DIRECTION('',(0.,0.,1.));"));
                let a0 = next();
                l(s, format!("#{a0}=AXIS2_PLACEMENT_3D('',#{c0},#{z},$);"));
                let ci0 = next();
                l(s, format!("#{ci0}=CIRCLE('',#{a0},{:.2});", g[0]));
                let c1 = next();
                l(s, format!("#{c1}=CARTESIAN_POINT('',(0.,0.,{:.2}));", g[1]));
                let a1 = next();
                l(s, format!("#{a1}=AXIS2_PLACEMENT_3D('',#{c1},#{z},$);"));
                let ci1 = next();
                l(s, format!("#{ci1}=CIRCLE('',#{a1},{:.2});", g[0]));
                solid = next();
                l(
                    s,
                    format!("#{solid}=MANIFOLD_SOLID_BREP('{name}',(#{ci0},#{ci1}));"),
                );
            }
            items.push(solid);
            let sr = next();
            let li: Vec<String> = items.iter().map(|v| format!("#{v}")).collect();
            l(
                s,
                format!(
                    "#{sr}=ADVANCED_BREP_SHAPE_REPRESENTATION('{name}',({}),#{gctx});",
                    li.join(",")
                ),
            );
            let sdr = next();
            l(
                s,
                format!("#{sdr}=SHAPE_DEFINITION_REPRESENTATION(#{pds},#{sr});"),
            );
            let sty = next();
            l(s, format!("#{sty}=STYLED_ITEM('color',(#{o}),#{solid});"));
            styled.push(sty);
            (pd, sr)
        };
        let (top, top_sr) = part(&mut s, "TOP_ASM", ("box", [0.0; 6]));
        let (chassis, _) = part(
            &mut s,
            "CHASSIS",
            ("box", [0.0, 0.0, 0.0, 200.0, 150.0, 40.0]),
        );
        let (pcb, _) = part(
            &mut s,
            "PCB_MAIN",
            ("box", [0.0, 0.0, 0.0, 160.0, 100.0, 1.6]),
        );
        let (u1, _) = part(&mut s, "U1", ("box", [0.0, 0.0, 0.0, 10.0, 10.0, 1.2]));
        let (cap, _) = part(&mut s, "C12", ("cyl", [2.5, 11.0, 0.0, 0.0, 0.0, 0.0]));
        let (sub, sub_sr) = part(&mut s, "FIXING_KIT", ("box", [0.0; 6]));
        let (screw, _) = part(
            &mut s,
            "VITE TCEI M3X8 UNI 5931",
            ("cyl", [2.75, 11.0, 0.0, 0.0, 0.0, 0.0]),
        );
        let (washer, _) = part(
            &mut s,
            "RONDELLA M3 UNI 6592",
            ("cyl", [3.5, 0.5, 0.0, 0.0, 0.0, 0.0]),
        );
        let mut occ = |s: &mut String,
                       parent: (u64, u64),
                       child: u64,
                       child_sr: Option<u64>,
                       tag: &str| {
            let n = next();
            l(
                s,
                format!(
                    "#{n}=NEXT_ASSEMBLY_USAGE_OCCURRENCE('{tag}','{tag}','',#{},#{child},$);",
                    parent.0
                ),
            );
            let pds = next();
            l(s, format!("#{pds}=PRODUCT_DEFINITION_SHAPE('','',#{n});"));
            let t = next();
            l(
                s,
                format!("#{t}=ITEM_DEFINED_TRANSFORMATION('','',#{ax},#{ax});"),
            );
            let rr = next();
            let csr = child_sr.unwrap_or(parent.1);
            l(s, format!("#{rr}=(REPRESENTATION_RELATIONSHIP('','',#{csr},#{})REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#{t})SHAPE_REPRESENTATION_RELATIONSHIP());", parent.1));
            let c = next();
            l(
                s,
                format!("#{c}=CONTEXT_DEPENDENT_SHAPE_REPRESENTATION(#{rr},#{pds});"),
            );
        };
        // the child shape representation ids follow the part order: look them up by name
        let _ = (chassis, pcb, u1, cap, screw, washer);
        let srs: HashMap<u64, u64> = {
            let e = parse_part21(&format!("{s}ENDSEC;\nEND-ISO-10303-21;\n")).unwrap();
            let st = structure(&e);
            st.reps.iter().map(|(k, v)| (*k, v[0])).collect()
        };
        occ(
            &mut s,
            (top, top_sr),
            chassis,
            srs.get(&chassis).copied(),
            "CHASSIS:1",
        );
        occ(&mut s, (top, top_sr), pcb, srs.get(&pcb).copied(), "PCB:1");
        occ(&mut s, (pcb, srs[&pcb]), u1, srs.get(&u1).copied(), "U1");
        occ(&mut s, (pcb, srs[&pcb]), cap, srs.get(&cap).copied(), "C12");
        occ(&mut s, (top, top_sr), sub, Some(sub_sr), "KIT:1");
        for k in 0..2 {
            occ(
                &mut s,
                (sub, sub_sr),
                screw,
                srs.get(&screw).copied(),
                &format!("VITE:{k}"),
            );
        }
        occ(
            &mut s,
            (sub, sub_sr),
            washer,
            srs.get(&washer).copied(),
            "RONDELLA:1",
        );
        let cat = next();
        let pl: Vec<String> = prods.iter().map(|p| format!("#{p}")).collect();
        l(
            &mut s,
            format!(
                "#{cat}=PRODUCT_RELATED_PRODUCT_CATEGORY('part',$,({}));",
                pl.join(",")
            ),
        );
        let mdg = next();
        let sl: Vec<String> = styled.iter().map(|p| format!("#{p}")).collect();
        l(
            &mut s,
            format!(
                "#{mdg}=MECHANICAL_DESIGN_GEOMETRIC_PRESENTATION_REPRESENTATION('',({}),#{gctx});",
                sl.join(",")
            ),
        );
        s.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
        s
    }

    #[test]
    fn screws_and_washer_are_found_and_removed_the_rest_is_byte_identical() {
        let text = sample();
        check_references(&text).unwrap();
        let sc = scan(&text).unwrap();
        let sugg: Vec<&str> = sc
            .candidates
            .iter()
            .filter(|c| c.suggested)
            .map(|c| c.name.as_str())
            .collect();
        assert!(sugg.contains(&"VITE TCEI M3X8 UNI 5931"), "{sugg:?}");
        assert!(sugg.contains(&"RONDELLA M3 UNI 6592"), "{sugg:?}");
        assert!(!sc
            .candidates
            .iter()
            .any(|c| c.name == "C12" || c.name == "PCB_MAIN" || c.name == "U1"));
        let screw = sc
            .candidates
            .iter()
            .find(|c| c.kind == Kind::Screw)
            .unwrap();
        assert_eq!(screw.instances, 2);
        let s = screw.size_mm.unwrap();
        assert!(
            (s[0] - 5.5).abs() < 1e-6 && (s[2] - 11.0).abs() < 1e-6,
            "{s:?}"
        );

        let ids: Vec<u64> = sc
            .candidates
            .iter()
            .filter(|c| c.suggested)
            .map(|c| c.id)
            .collect();
        let (out, rep) = remove(&text, &ids).unwrap();
        // the empty fixing kit goes too
        assert!(
            rep.parts.contains(&"FIXING_KIT".to_string()),
            "{:?}",
            rep.parts
        );
        assert_eq!(rep.occurrences, 4);
        check_references(&out).unwrap();
        // names of kept parts are all there, removed ones are gone
        for keep in ["TOP_ASM", "CHASSIS", "PCB_MAIN", "U1", "C12"] {
            assert!(out.contains(&format!("PRODUCT('{keep}'")), "{keep} missing");
        }
        for gone in ["VITE TCEI", "RONDELLA", "FIXING_KIT"] {
            let left: Vec<&str> = out.lines().filter(|l| l.contains(gone)).collect();
            assert!(left.is_empty(), "{gone} still there: {left:?}");
        }
        // every kept line is unchanged except the lists that named removed items
        let before: HashSet<&str> = text.lines().collect();
        let changed: Vec<&str> = out.lines().filter(|l| !before.contains(l)).collect();
        assert_eq!(changed.len(), rep.edited_records, "{changed:?}");
        assert!(
            changed
                .iter()
                .all(|l| l.contains("PRODUCT_RELATED_PRODUCT_CATEGORY")
                    || l.contains("MECHANICAL_DESIGN_GEOMETRIC_PRESENTATION")
                    || l.contains("SHAPE_REPRESENTATION(")),
            "{changed:?}"
        );
        // the header is untouched
        assert_eq!(text.split("DATA;").next(), out.split("DATA;").next());
        // the result reads again and still has the PCB and its component
        let sc2 = scan(&out).unwrap();
        assert!(sc2.candidates.iter().all(|c| !c.suggested));
        let st = structure(&parse_part21(&out).unwrap());
        let names: Vec<&str> = st.names.values().map(|x| x.0.as_str()).collect();
        assert_eq!(names.len(), 5, "{names:?}");
    }

    #[test]
    fn removing_nothing_gives_the_same_file() {
        let text = sample();
        let (out, rep) = remove(&text, &[]).unwrap();
        assert_eq!(out, text);
        assert_eq!(rep.deleted_records, 0);
    }

    /// Round trip through OpenCascade: a STEP file written by OCCT, cleaned
    /// here, read back by OCCT with the same bodies minus the fasteners.
    #[cfg(feature = "occt")]
    #[test]
    fn occt_written_file_round_trip() {
        let mut k = crate::occt::kernel().unwrap();
        let doc = k.xcaf_new_document().unwrap();
        let mut add = |k: &mut occt_wasm::OcctKernel, s, name: &str| {
            let l = k.xcaf_add_shape(doc, s).unwrap();
            k.xcaf_set_name(doc, l, name).unwrap();
        };
        let chassis = k.make_box(200.0, 150.0, 40.0).unwrap();
        add(&mut k, chassis, "CHASSIS");
        let pcb = k.make_box(160.0, 100.0, 1.6).unwrap();
        let pcb = k.translate(pcb, 20.0, 25.0, 10.0).unwrap();
        add(&mut k, pcb, "PCB_MAIN");
        let cap = k.make_cylinder(2.5, 11.0).unwrap();
        let cap = k.translate(cap, 60.0, 60.0, 11.6).unwrap();
        add(&mut k, cap, "C12");
        for (i, x) in [30.0, 170.0].into_iter().enumerate() {
            // M3 socket head: head d 5.5 × 3, shank d 3 × 8
            let head = k.make_cylinder(2.75, 3.0).unwrap();
            let shank = k.make_cylinder(1.5, 8.0).unwrap();
            let shank = k.translate(shank, 0.0, 0.0, -8.0).unwrap();
            let screw = k.fuse(head, shank).unwrap();
            let screw = k.translate(screw, x, 35.0, 12.1).unwrap();
            add(&mut k, screw, &format!("ISO 4762 M3x8 {i}"));
            let w = k.make_cylinder(3.5, 0.5).unwrap();
            let hole = k.make_cylinder(1.6, 0.5).unwrap();
            let w = k.cut(w, hole).unwrap();
            let w = k.translate(w, x, 35.0, 11.6).unwrap();
            add(&mut k, w, &format!("Rondella M3 UNI 6592 {i}"));
        }
        let text = k.xcaf_export_step(doc).unwrap();
        let sc = scan(&text).unwrap();
        let ids: Vec<u64> = sc.candidates.iter().filter(|c| c.suggested).map(|c| c.id).collect();
        assert_eq!(ids.len(), 4, "{:?}", sc.candidates);
        let (out, _) = remove(&text, &ids).unwrap();
        let before = crate::step::read(&text, 0.1).unwrap();
        let after = crate::step::read(&out, 0.1).unwrap();
        let names = |m: &crate::step::StepModel| {
            let mut v: Vec<(String, String)> = m
                .bodies
                .iter()
                .map(|b| (b.name.clone(), format!("{:.3?}{:.3?}", b.min, b.max)))
                .collect();
            v.sort();
            v
        };
        let kept: Vec<_> = names(&before)
            .into_iter()
            .filter(|(n, _)| !n.contains("M3"))
            .collect();
        assert_eq!(names(&after), kept);
        assert_eq!(kept.len(), 3);
    }
}
