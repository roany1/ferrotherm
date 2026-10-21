// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! One entry point for every supported CAD file: format detection, reading,
//! and conversion into case blocks/components.
//!
//! | Source | Accepted input |
//! |---|---|
//! | STEP AP203/214/242 | `.step` `.stp` `.p21` |
//! | STL | `.stl` (ASCII/binary, mm) |
//! | IDF 3.0 | `.emn`/`.brd`/`.bdf`/`.idb` (+ `.emp`/`.lib`/`.ldf`/`.idl` beside it) |
//! | KiCad | `.kicad_pcb` (v5 … v9) |
//! | Gerber X2/RS-274X + Excellon | folder, `.zip`, `.gbrjob` or any Gerber file of the set |
//! | IPC-2581 | `.xml` `.cvg` |
//! | ODB++ | folder, `.tgz`/`.tar.gz`/`.tar`/`.zip` |

use crate::archive::{self, Files};
use crate::ecad::{self, Board, Fragment};
use crate::step::{self, BlockMode, StepModel};
use ferrotherm_core::case::{Block, Case};
use ferrotherm_core::pcb::PcbStack;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Step,
    Stl,
    Idf,
    KiCad,
    Gerber,
    Ipc2581,
    Odbpp,
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::Step => "STEP",
            Source::Stl => "STL",
            Source::Idf => "IDF 3.0",
            Source::KiCad => "KiCad",
            Source::Gerber => "Gerber/Excellon",
            Source::Ipc2581 => "IPC-2581",
            Source::Odbpp => "ODB++",
        }
    }
    /// True for ECAD formats (board + placements).
    pub fn is_ecad(&self) -> bool {
        !matches!(self, Source::Step | Source::Stl)
    }
}

/// File-dialog filters: (label, extensions).
pub const FILTERS: &[(&str, &[&str])] = &[
    (
        "All supported",
        &[
            "step",
            "stp",
            "p21",
            "stl",
            "emn",
            "brd",
            "bdf",
            "idb",
            "kicad_pcb",
            "gbrjob",
            "gbr",
            "gtl",
            "gbl",
            "gko",
            "gm1",
            "xml",
            "cvg",
            "tgz",
            "gz",
            "tar",
            "zip",
        ],
    ),
    ("STEP (MCAD)", &["step", "stp", "p21"]),
    ("STL", &["stl"]),
    ("IDF 3.0", &["emn", "brd", "bdf", "idb"]),
    ("KiCad board", &["kicad_pcb"]),
    (
        "Gerber job / Gerber file",
        &["gbrjob", "gbr", "gtl", "gbl", "gko", "gm1"],
    ),
    ("IPC-2581", &["xml", "cvg"]),
    ("ODB++ / Gerber archive", &["tgz", "gz", "tar", "zip"]),
];

pub enum Imported {
    Board(Board),
    Mcad(StepModel),
}

fn lower(p: &Path) -> String {
    p.to_string_lossy().to_ascii_lowercase()
}

fn ext_of(name: &str) -> String {
    name.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}

const GERBER_EXT: &[&str] = &[
    "gbr", "gtl", "gbl", "gts", "gbs", "gto", "gbo", "gtp", "gbp", "gko", "gm1", "gml", "g1", "g2",
    "g3", "g4", "drl", "xln", "gbrjob", "pho", "art", "ger",
];

fn is_ipc(text: &str) -> bool {
    let head: String = text.chars().take(4096).collect();
    head.contains("<IPC-2581")
}

/// Which format a set of files (folder or archive) holds.
pub fn detect_files(files: &Files) -> Option<Source> {
    let names: Vec<String> = files.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
    if names.iter().any(|n| n.ends_with("matrix/matrix")) {
        return Some(Source::Odbpp);
    }
    if names.iter().any(|n| n.ends_with(".kicad_pcb")) {
        return Some(Source::KiCad);
    }
    for (n, d) in files {
        let e = ext_of(n);
        if (e == "xml" || e == "cvg") && is_ipc(&String::from_utf8_lossy(&d[..d.len().min(8192)])) {
            return Some(Source::Ipc2581);
        }
    }
    if names
        .iter()
        .any(|n| matches!(ext_of(n).as_str(), "emn" | "bdf" | "idb"))
    {
        return Some(Source::Idf);
    }
    if names
        .iter()
        .any(|n| GERBER_EXT.contains(&ext_of(n).as_str()))
    {
        return Some(Source::Gerber);
    }
    if names
        .iter()
        .any(|n| matches!(ext_of(n).as_str(), "step" | "stp" | "p21"))
    {
        return Some(Source::Step);
    }
    if names.iter().any(|n| ext_of(n) == "stl") {
        return Some(Source::Stl);
    }
    // Gerber files without a known extension
    if files
        .iter()
        .any(|(_, d)| String::from_utf8_lossy(&d[..d.len().min(4096)]).contains("%FS"))
    {
        return Some(Source::Gerber);
    }
    None
}

fn text(d: &[u8]) -> String {
    String::from_utf8_lossy(d).to_string()
}

fn read_idf_files(files: &Files) -> Result<Board, String> {
    let (bn, bd) = files
        .iter()
        .find(|(n, _)| matches!(ext_of(n).as_str(), "emn" | "bdf" | "idb" | "brd"))
        .ok_or("no IDF board file")?;
    let stem = bn
        .rsplit_once('.')
        .map(|s| s.0)
        .unwrap_or(bn)
        .to_ascii_lowercase();
    let lib = files
        .iter()
        .filter(|(n, _)| matches!(ext_of(n).as_str(), "emp" | "lib" | "ldf" | "idl"))
        .min_by_key(|(n, _)| {
            if n.to_ascii_lowercase().starts_with(&stem) {
                0
            } else {
                1
            }
        })
        .map(|(_, d)| text(d));
    let mut b = crate::idf::read(&text(bd), lib.as_deref())?;
    if lib.is_none() {
        b.warnings
            .push("no IDF library file (.emp) found: part outlines and heights unknown".into());
    }
    Ok(b)
}

/// Read a set of files whose format is known.
pub fn read_files(src: Source, files: Files, tolerance_mm: f64) -> Result<Imported, String> {
    match src {
        Source::Odbpp => crate::odbpp::read(files).map(Imported::Board),
        Source::KiCad => {
            let (_, d) = files
                .iter()
                .find(|(n, _)| n.to_ascii_lowercase().ends_with(".kicad_pcb"))
                .ok_or("no .kicad_pcb file")?;
            crate::kicad::read(&text(d)).map(Imported::Board)
        }
        Source::Ipc2581 => {
            let (_, d) = files
                .iter()
                .find(|(n, d)| {
                    matches!(ext_of(n).as_str(), "xml" | "cvg")
                        && is_ipc(&text(&d[..d.len().min(8192)]))
                })
                .ok_or("no IPC-2581 file")?;
            crate::ipc2581::read(&text(d)).map(Imported::Board)
        }
        Source::Idf => read_idf_files(&files).map(Imported::Board),
        Source::Gerber => {
            let t: Vec<(String, String)> = files
                .iter()
                .filter(|(n, _)| {
                    let e = ext_of(n);
                    !matches!(
                        e.as_str(),
                        "pdf" | "png" | "jpg" | "step" | "stp" | "kicad_pcb" | "zip"
                    )
                })
                .map(|(n, d)| (n.clone(), archive::text_of(n, d)))
                .collect();
            crate::gerber::read_set(&t).map(Imported::Board)
        }
        Source::Step => {
            let (_, d) = files
                .iter()
                .find(|(n, _)| matches!(ext_of(n).as_str(), "step" | "stp" | "p21"))
                .ok_or("no STEP file")?;
            step::read(&text(d), tolerance_mm).map(Imported::Mcad)
        }
        Source::Stl => {
            let (n, d) = files
                .iter()
                .find(|(n, _)| ext_of(n) == "stl")
                .ok_or("no STL file")?;
            let stem = n.rsplit('/').next().unwrap_or(n);
            let stem = stem.rsplit_once('.').map(|s| s.0).unwrap_or(stem);
            crate::stl::read(d, stem, 1.0).map(Imported::Mcad)
        }
    }
}

/// Read any supported file or folder. `tolerance_mm`: STEP tessellation.
pub fn import_path(path: &Path, tolerance_mm: f64) -> Result<(Source, Imported), String> {
    let (src, mut imp) = import_path_raw(path, tolerance_mm)?;
    // give unnamed boards the file name
    if let Imported::Board(b) = &mut imp {
        let generic = b.name.trim().is_empty()
            || b.name == "kicad_pcb"
            || b.name.eq_ignore_ascii_case("unnamed");
        if generic {
            if let Some(stem) = path.file_stem() {
                b.name = stem.to_string_lossy().trim_end_matches(".tar").to_string();
            }
        }
    }
    Ok((src, imp))
}

fn import_path_raw(path: &Path, tolerance_mm: f64) -> Result<(Source, Imported), String> {
    let l = lower(path);
    let e = ext_of(&l);
    let single = |src: Source| -> Result<(Source, Imported), String> {
        let d = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok((src, read_files(src, vec![(name, d)], tolerance_mm)?))
    };
    if path.is_file() {
        match e.as_str() {
            "step" | "stp" | "p21" => return single(Source::Step),
            "stl" => return single(Source::Stl),
            "kicad_pcb" => return single(Source::KiCad),
            "xml" | "cvg" => return single(Source::Ipc2581),
            "emn" | "brd" | "bdf" | "idb" => {
                // board file + library beside it
                let mut files = vec![(
                    path.file_name().unwrap().to_string_lossy().to_string(),
                    std::fs::read(path).map_err(|e| e.to_string())?,
                )];
                for le in ["emp", "lib", "ldf", "idl", "EMP", "LIB", "LDF", "IDL"] {
                    let p = path.with_extension(le);
                    if let Ok(d) = std::fs::read(&p) {
                        files.push((p.file_name().unwrap().to_string_lossy().to_string(), d));
                        break;
                    }
                }
                return Ok((Source::Idf, read_files(Source::Idf, files, tolerance_mm)?));
            }
            _ => {}
        }
        let archive = l.ends_with(".zip")
            || l.ends_with(".tgz")
            || l.ends_with(".tar.gz")
            || l.ends_with(".tar");
        if !archive {
            // a file of a Gerber set: read its folder
            if GERBER_EXT.contains(&e.as_str()) {
                if let Some(dir) = path.parent() {
                    let files = archive::load(dir)?;
                    return Ok((
                        Source::Gerber,
                        read_files(Source::Gerber, files, tolerance_mm)?,
                    ));
                }
            }
            return Err(format!("{}: unknown file type", path.display()));
        }
    }
    let files = archive::load(path)?;
    let src = detect_files(&files)
        .ok_or_else(|| format!("{}: no supported CAD data found", path.display()))?;
    Ok((src, read_files(src, files, tolerance_mm)?))
}

// ------------------------------------------------------------------ MCAD → case

#[derive(Debug, Clone)]
pub struct McadOptions {
    pub mode: BlockMode,
    /// Material of the created blocks.
    pub material: String,
    /// Translation added to every coordinate, mm.
    pub offset: [f64; 3],
    pub prefix: String,
    /// Skip bodies smaller than this (bounding-box volume, mm³).
    pub min_volume_mm3: f64,
    /// Bodies named like a PCB become a PCB stack (4 × 1 oz, 30 % coverage
    /// assumed, reported in the notes) instead of a solid material.
    pub detect_pcb: bool,
}

impl Default for McadOptions {
    fn default() -> Self {
        McadOptions {
            mode: BlockMode::BoundingBox,
            material: "al-6061-t6".into(),
            offset: [0.0; 3],
            prefix: String::new(),
            min_volume_mm3: 0.0,
            detect_pcb: true,
        }
    }
}

/// Convert MCAD bodies into blocks.
pub fn mcad_to_fragment(m: &StepModel, opt: &McadOptions) -> Fragment {
    let mut fr = Fragment::default();
    let mut skipped = 0;
    let mut pcbs = 0;
    for b in &m.bodies {
        let bv = (0..3)
            .map(|a| (b.max[a] - b.min[a]).max(0.0))
            .product::<f64>();
        if bv < opt.min_volume_mm3 {
            skipped += 1;
            continue;
        }
        let ext = [
            b.max[0] - b.min[0],
            b.max[1] - b.min[1],
            b.max[2] - b.min[2],
        ];
        let thin = ext[2] < 3.5 && ext[2] > 0.2 && ext[0].min(ext[1]) > 10.0 * ext[2];
        let is_pcb = opt.detect_pcb && thin && {
            let n = b.name.to_ascii_lowercase();
            n.contains("pcb") || n.contains("board")
        };
        let boxes = if is_pcb {
            vec![(b.min, b.max)]
        } else {
            step::body_boxes(b, opt.mode)
        };
        let multi = boxes.len() > 1;
        for (k, (lo, hi)) in boxes.into_iter().enumerate() {
            let name = if multi {
                format!("{}{}#{}", opt.prefix, b.name, k + 1)
            } else {
                format!("{}{}", opt.prefix, b.name)
            };
            let min = [
                lo[0] + opt.offset[0],
                lo[1] + opt.offset[1],
                lo[2] + opt.offset[2],
            ];
            let max = [
                hi[0] + opt.offset[0],
                hi[1] + opt.offset[1],
                hi[2] + opt.offset[2],
            ];
            let block = if is_pcb {
                pcbs += 1;
                Block {
                    name,
                    material: None,
                    pcb: Some(PcbStack {
                        thickness_mm: max[2] - min[2],
                        dielectric: "fr4".into(),
                        copper: "cu-c11000".into(),
                        copper_layers: Vec::new(),
                        layers: Some(4),
                        copper_oz: Some(1.0),
                        coverage: Some(0.3),
                        via_fraction: 0.0,
                        normal: "z".into(),
                    }),
                    min,
                    max,
                    power_w: 0.0,
                    emissivity: None,
                    finish: Some("pcb-soldermask".into()),
                }
            } else {
                Block {
                    name,
                    material: Some(opt.material.clone()),
                    pcb: None,
                    min,
                    max,
                    power_w: 0.0,
                    emissivity: None,
                    finish: None,
                }
            };
            fr.blocks.push(block);
        }
    }
    if skipped > 0 {
        fr.notes.push(format!("{skipped} small bodies skipped"));
    }
    if pcbs > 0 {
        fr.notes.push(format!(
            "{pcbs} board body(ies) modelled as a PCB stack with 4 × 1 oz copper at 30 % coverage (assumed: import the ECAD data for the real copper)"
        ));
    }
    if m.bodies.iter().any(|b| b.bbox_only) {
        fr.notes
            .push("some bodies have only a bounding box (not tessellated)".into());
    }
    extent(&mut fr);
    fr
}

/// Recompute the fragment's bounding box.
pub fn extent(fr: &mut Fragment) {
    let mut mn = [f64::INFINITY; 3];
    let mut mx = [f64::NEG_INFINITY; 3];
    for b in &fr.blocks {
        for a in 0..3 {
            mn[a] = mn[a].min(b.min[a]);
            mx[a] = mx[a].max(b.max[a]);
        }
    }
    for c in &fr.components {
        if let Some(s) = c.size {
            for a in 0..3 {
                mn[a] = mn[a].min(c.position[a]);
                mx[a] = mx[a].max(c.position[a] + s[a]);
            }
        }
    }
    fr.min = mn;
    fr.max = mx;
}

fn unique(name: &str, taken: &mut std::collections::HashSet<String>) -> String {
    if taken.insert(name.to_string()) {
        return name.to_string();
    }
    for i in 2.. {
        let n = format!("{name}~{i}");
        if taken.insert(n.clone()) {
            return n;
        }
    }
    unreachable!()
}

/// Add a fragment to a case. Names that clash get a `~N` suffix. With
/// `fit_domain`, the domain grows to contain the fragment plus `margin_mm`
/// (or is set to it when the case had no geometry yet).
/// Returns notes for the user.
pub fn merge_into_case(
    case: &mut Case,
    fr: Fragment,
    fit_domain: bool,
    margin_mm: f64,
) -> Vec<String> {
    let mut notes = fr.notes.clone();
    let was_empty =
        case.blocks.is_empty() && case.components.is_empty() && case.heatsinks.is_empty();
    let mut taken: std::collections::HashSet<String> = case
        .blocks
        .iter()
        .map(|b| b.name.clone())
        .chain(case.components.iter().map(|c| c.name.clone()))
        .collect();
    let mut renamed = 0;
    for mut b in fr.blocks {
        let n = unique(&b.name, &mut taken);
        renamed += (n != b.name) as usize;
        b.name = n;
        case.blocks.push(b);
    }
    for mut c in fr.components {
        let n = unique(&c.name, &mut taken);
        renamed += (n != c.name) as usize;
        c.name = n;
        case.components.push(c);
    }
    if renamed > 0 {
        notes.push(format!(
            "{renamed} names already in use were given a ~N suffix"
        ));
    }
    if fr.min[0].is_finite() {
        let inside = (0..3).all(|a| {
            fr.min[a] >= case.domain.min[a] - 1e-9 && fr.max[a] <= case.domain.max[a] + 1e-9
        });
        if fit_domain && was_empty {
            // nothing else in the model: the domain wraps the import
            for a in 0..3 {
                case.domain.min[a] = fr.min[a] - margin_mm;
                case.domain.max[a] = fr.max[a] + margin_mm;
            }
        } else if fit_domain {
            for a in 0..3 {
                case.domain.min[a] = case.domain.min[a].min(fr.min[a] - margin_mm);
                case.domain.max[a] = case.domain.max[a].max(fr.max[a] + margin_mm);
            }
        } else if !inside {
            notes.push("the imported geometry extends outside the domain".into());
        }
    }
    notes
}

/// Import a board (ECAD) as PCB tiles + parts.
pub fn board_fragment(board: &mut Board, opt: &ecad::ToCaseOptions) -> Fragment {
    let mut fr = ecad::to_fragment(board, opt);
    fr.notes.extend(board.warnings.iter().cloned());
    fr
}
