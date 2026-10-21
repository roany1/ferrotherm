// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Format-independent board model shared by every ECAD importer, and its
//! conversion into FerroTherm blocks and components.
//!
//! The board becomes a set of PCB tiles: each tile is a block with its own
//! layered stack-up whose copper coverage per layer and thermal-via fraction
//! are measured from the rasterised copper of that tile ("detailed PCB"
//! conductivity map).

use crate::geom::{bbox, rotate, Poly, Raster, P2};
use ferrotherm_core::case::{Block, Component};
use ferrotherm_core::pcb::{CopperLayer, PcbStack};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LayerKind {
    Copper,
    Dielectric,
}

#[derive(Clone)]
pub struct Layer {
    pub name: String,
    pub kind: LayerKind,
    pub thickness_mm: Option<f64>,
    pub material: Option<String>,
    /// Rasterised copper (copper layers), in the board raster frame.
    pub copper: Option<Raster>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Side {
    Top,
    Bottom,
}

#[derive(Debug, Clone, Serialize)]
pub struct Placement {
    pub refdes: String,
    pub package: String,
    pub part: String,
    /// Placement origin, mm (board coordinates, y up).
    pub x: f64,
    pub y: f64,
    pub rotation_deg: f64,
    pub side: Side,
    /// Body outline in the package's local frame (before rotation), mm.
    pub local_bbox: Option<(P2, P2)>,
    pub height_mm: Option<f64>,
    pub power_w: Option<f64>,
    pub r_jc: Option<f64>,
    pub r_jb: Option<f64>,
    pub tj_max_c: Option<f64>,
}

impl Placement {
    pub fn new(refdes: &str, package: &str, x: f64, y: f64, rot: f64, side: Side) -> Placement {
        Placement {
            refdes: refdes.into(),
            package: package.into(),
            part: String::new(),
            x,
            y,
            rotation_deg: rot,
            side,
            local_bbox: None,
            height_mm: None,
            power_w: None,
            r_jc: None,
            r_jb: None,
            tj_max_c: None,
        }
    }

    /// Axis-aligned footprint on the board (after rotation), mm.
    pub fn footprint(&self) -> Option<(P2, P2)> {
        let (lo, hi) = self.local_bbox?;
        let corners = [
            [lo[0], lo[1]],
            [hi[0], lo[1]],
            [hi[0], hi[1]],
            [lo[0], hi[1]],
        ];
        let pts: Vec<P2> = corners
            .iter()
            .map(|c| {
                // bottom-side parts are mirrored in x before rotation
                let c = if self.side == Side::Bottom {
                    [-c[0], c[1]]
                } else {
                    *c
                };
                let r = rotate(c, self.rotation_deg);
                [r[0] + self.x, r[1] + self.y]
            })
            .collect();
        bbox(&pts)
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Drill {
    pub x: f64,
    pub y: f64,
    pub d_mm: f64,
    pub plated: bool,
}

/// A board read from any ECAD format.
#[derive(Clone, Default)]
pub struct Board {
    pub format: String,
    pub name: String,
    /// Outline contours (even–odd: outer boundary plus cut-outs), mm.
    pub outline: Vec<Poly>,
    pub thickness_mm: Option<f64>,
    /// Stack-up, top to bottom.
    pub layers: Vec<Layer>,
    pub components: Vec<Placement>,
    pub drills: Vec<Drill>,
    /// Raster frame shared by every copper raster.
    pub frame: Option<(P2, P2, f64)>,
    pub warnings: Vec<String>,
}

/// Summary for reports and the GUI.
#[derive(Debug, Clone, Serialize)]
pub struct BoardSummary {
    pub format: String,
    pub name: String,
    pub size_mm: [f64; 2],
    pub thickness_mm: f64,
    pub copper_layers: Vec<(String, f64, f64)>,
    pub components: usize,
    pub drills: usize,
    pub warnings: Vec<String>,
}

pub const COPPER_OZ_MM: f64 = 0.035;
/// Typical plating thickness of a via barrel, mm (IPC-6012 class 2 minimum average is 20 µm; 25 µm used here).
pub const VIA_PLATING_MM: f64 = 0.025;

impl Board {
    pub fn bbox(&self) -> Option<(P2, P2)> {
        let pts: Vec<P2> = self.outline.iter().flatten().copied().collect();
        if let Some(b) = bbox(&pts) {
            return Some(b);
        }
        // no outline: use the components
        let mut pts = Vec::new();
        for c in &self.components {
            if let Some((lo, hi)) = c.footprint() {
                pts.push(lo);
                pts.push(hi);
            } else {
                pts.push([c.x, c.y]);
            }
        }
        bbox(&pts)
    }

    /// Create the shared raster frame (resolution adapted to keep ≤ ~8 Mpx).
    pub fn ensure_frame(&mut self, res_mm: f64) -> (P2, P2, f64) {
        if let Some(f) = self.frame {
            return f;
        }
        let (lo, hi) = self.bbox().unwrap_or(([0.0, 0.0], [100.0, 100.0]));
        let area = (hi[0] - lo[0]) * (hi[1] - lo[1]);
        let res = res_mm.max((area / 8.0e6).sqrt());
        let f = ([lo[0] - res, lo[1] - res], [hi[0] + res, hi[1] + res], res);
        self.frame = Some(f);
        f
    }

    pub fn new_raster(&mut self) -> Raster {
        let (lo, hi, res) = self.ensure_frame(0.1);
        Raster::new(lo, hi, res)
    }

    pub fn total_thickness(&self) -> f64 {
        if let Some(t) = self.thickness_mm {
            return t;
        }
        let s: f64 = self.layers.iter().filter_map(|l| l.thickness_mm).sum();
        if s > 0.0 {
            s
        } else {
            1.6
        }
    }

    pub fn copper_layers(&self) -> impl Iterator<Item = &Layer> {
        self.layers.iter().filter(|l| l.kind == LayerKind::Copper)
    }

    fn outline_raster(&mut self) -> Option<Raster> {
        if self.outline.is_empty() {
            return None;
        }
        let mut r = self.new_raster();
        let polys: Vec<&[P2]> = self.outline.iter().map(|p| p.as_slice()).collect();
        r.fill_polys(&polys, true);
        Some(r)
    }

    pub fn summary(&mut self) -> BoardSummary {
        let (lo, hi) = self.bbox().unwrap_or(([0.0; 2], [0.0; 2]));
        let outline = self.outline_raster();
        let cl = self
            .layers
            .iter()
            .filter(|l| l.kind == LayerKind::Copper)
            .map(|l| {
                let cov = match (&l.copper, &outline) {
                    (Some(c), Some(o)) => {
                        let mut c = c.clone();
                        c.and(o);
                        c.total_coverage() / o.total_coverage().max(1e-12)
                    }
                    (Some(c), None) => c.total_coverage(),
                    _ => f64::NAN,
                };
                (l.name.clone(), l.thickness_mm.unwrap_or(f64::NAN), cov)
            })
            .collect();
        BoardSummary {
            format: self.format.clone(),
            name: self.name.clone(),
            size_mm: [hi[0] - lo[0], hi[1] - lo[1]],
            thickness_mm: self.total_thickness(),
            copper_layers: cl,
            components: self.components.len(),
            drills: self.drills.len(),
            warnings: self.warnings.clone(),
        }
    }
}

/// How components are created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentMode {
    /// Solid blocks of mould compound with their power spread in the volume.
    Blocks,
    /// 2R compact models when θJC is known, blocks otherwise.
    Compact,
    /// Skip components.
    None,
}

#[derive(Debug, Clone)]
pub struct ToCaseOptions {
    /// Tile size for the PCB conductivity map, mm (0 = one block).
    pub tile_mm: f64,
    /// Position of the board's (min x, min y, bottom) corner in the case, mm.
    pub origin: [f64; 3],
    pub components: ComponentMode,
    /// Height for parts without one, mm (None = guess from the package name).
    pub default_height_mm: Option<f64>,
    /// Ignore parts smaller than this footprint area, mm².
    pub min_area_mm2: f64,
    /// Only parts with power (skip unpowered passives).
    pub powered_only: bool,
    pub dielectric: String,
    pub prefix: String,
}

impl Default for ToCaseOptions {
    fn default() -> Self {
        ToCaseOptions {
            tile_mm: 10.0,
            origin: [0.0, 0.0, 0.0],
            components: ComponentMode::Compact,
            default_height_mm: None,
            min_area_mm2: 4.0,
            powered_only: false,
            dielectric: "fr4".into(),
            prefix: String::new(),
        }
    }
}

/// Blocks and components created from a board, in case coordinates (mm).
#[derive(Debug, Clone, Default)]
pub struct Fragment {
    pub blocks: Vec<Block>,
    pub components: Vec<Component>,
    /// Bounding box of everything created.
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub notes: Vec<String>,
}

/// Rough package height from the package/footprint name, mm. Used only when
/// the ECAD data has no height; always reported so the user can correct it.
pub fn guess_height(package: &str) -> f64 {
    let p = package.to_ascii_lowercase();
    let has = |s: &str| p.contains(s);
    if has("bga") {
        2.0
    } else if has("qfn") || has("dfn") || has("son") || has("lga") {
        0.9
    } else if has("qfp") || has("tqfp") || has("lqfp") {
        1.6
    } else if has("soic") || has("so-") || has("so8") || has("ssop") || has("tssop") || has("msop")
    {
        1.5
    } else if has("sot-23") || has("sot23") || has("sc-70") || has("sot-3") {
        1.1
    } else if has("to-220") || has("to220") {
        4.5
    } else if has("to-263") || has("d2pak") || has("to-252") || has("dpak") {
        2.4
    } else if has("dip") {
        4.5
    } else if has("0201") || has("0402") {
        0.4
    } else if has("0603") || has("0805") || has("1206") || has("r_") || has("c_") {
        0.8
    } else if has("cp_") || has("elec") || has("radial") {
        10.0
    } else if has("conn") || has("header") || has("dsub") || has("usb") {
        8.0
    } else {
        2.0
    }
}

pub fn to_fragment(board: &mut Board, opt: &ToCaseOptions) -> Fragment {
    let mut fr = Fragment::default();
    let Some((lo, hi)) = board.bbox() else {
        fr.notes
            .push("the board has no outline and no components".into());
        return fr;
    };
    let t_board = board.total_thickness();
    let off = [opt.origin[0] - lo[0], opt.origin[1] - lo[1]];
    let z0 = opt.origin[2];
    let outline = board.outline_raster();
    // copper layers with their rasters clipped to the outline
    let mut coppers: Vec<(f64, Option<Raster>)> = board
        .layers
        .iter()
        .filter(|l| l.kind == LayerKind::Copper)
        .map(|l| {
            let t = l.thickness_mm.unwrap_or(COPPER_OZ_MM);
            let r = l.copper.clone().map(|mut r| {
                if let Some(o) = &outline {
                    r.and(o);
                }
                r
            });
            (t, r)
        })
        .collect();
    if coppers.is_empty() {
        fr.notes
            .push("no copper layer information: 4 layers × 1 oz, 50 % coverage assumed".into());
        coppers = vec![(COPPER_OZ_MM, None); 4];
    }
    let cu_total: f64 = coppers.iter().map(|c| c.0).sum();
    if cu_total >= t_board {
        fr.notes
            .push("copper thicker than the board: thickness data inconsistent".into());
    }

    // ---- PCB tiles ----
    let (nx, ny) = if opt.tile_mm > 0.0 {
        (
            (((hi[0] - lo[0]) / opt.tile_mm).ceil() as usize).max(1),
            (((hi[1] - lo[1]) / opt.tile_mm).ceil() as usize).max(1),
        )
    } else {
        (1, 1)
    };
    let tx = (hi[0] - lo[0]) / nx as f64;
    let ty = (hi[1] - lo[1]) / ny as f64;
    for j in 0..ny {
        for i in 0..nx {
            let tlo = [lo[0] + i as f64 * tx, lo[1] + j as f64 * ty];
            let thi = [tlo[0] + tx, tlo[1] + ty];
            let inside = match &outline {
                Some(o) => o.coverage(tlo, thi),
                None => 1.0,
            };
            if inside < 0.5 {
                continue;
            }
            let layers: Vec<CopperLayer> = coppers
                .iter()
                .map(|(t, r)| CopperLayer {
                    oz: t / COPPER_OZ_MM,
                    coverage: match r {
                        Some(r) => (r.coverage(tlo, thi) / inside).clamp(0.0, 1.0),
                        None => 0.5,
                    },
                })
                .collect();
            // plated vias inside the tile: barrel copper area fraction
            let tile_area = tx * ty * inside;
            let via_area: f64 = board
                .drills
                .iter()
                .filter(|d| {
                    d.plated && d.x >= tlo[0] && d.x < thi[0] && d.y >= tlo[1] && d.y < thi[1]
                })
                .map(|d| {
                    let ro = 0.5 * d.d_mm;
                    let ri = (ro - VIA_PLATING_MM).max(0.0);
                    std::f64::consts::PI * (ro * ro - ri * ri)
                })
                .sum();
            let via_fraction = (via_area / tile_area).clamp(0.0, 0.5);
            let name = if nx * ny == 1 {
                format!("{}pcb", opt.prefix)
            } else {
                format!("{}pcb/r{}c{}", opt.prefix, j + 1, i + 1)
            };
            fr.blocks.push(Block {
                name,
                material: None,
                pcb: Some(PcbStack {
                    thickness_mm: t_board,
                    dielectric: opt.dielectric.clone(),
                    copper: "cu-c11000".into(),
                    copper_layers: layers,
                    layers: None,
                    copper_oz: None,
                    coverage: None,
                    via_fraction,
                    normal: "z".into(),
                }),
                min: [tlo[0] + off[0], tlo[1] + off[1], z0],
                max: [thi[0] + off[0], thi[1] + off[1], z0 + t_board],
                power_w: 0.0,
                emissivity: None,
                finish: Some("pcb-soldermask".into()),
            });
        }
    }

    // ---- components ----
    let mut guessed = 0;
    if opt.components != ComponentMode::None {
        for p in &board.components {
            let Some((flo, fhi)) = p.footprint() else {
                continue;
            };
            let area = (fhi[0] - flo[0]) * (fhi[1] - flo[1]);
            if area < opt.min_area_mm2 {
                continue;
            }
            if opt.powered_only && p.power_w.unwrap_or(0.0) <= 0.0 {
                continue;
            }
            let h = match (p.height_mm, opt.default_height_mm) {
                (Some(h), _) if h > 0.0 => h,
                (_, Some(d)) => d,
                _ => {
                    guessed += 1;
                    guess_height(&p.package)
                }
            };
            let (zlo, zhi) = match p.side {
                Side::Top => (z0 + t_board, z0 + t_board + h),
                Side::Bottom => (z0 - h, z0),
            };
            let min = [flo[0] + off[0], flo[1] + off[1], zlo];
            let max = [fhi[0] + off[0], fhi[1] + off[1], zhi];
            let name = format!("{}{}", opt.prefix, p.refdes);
            let power = p.power_w.unwrap_or(0.0);
            if opt.components == ComponentMode::Compact && (p.r_jc.is_some() || p.r_jb.is_some()) {
                fr.components.push(Component {
                    name,
                    package: "generic".into(),
                    position: min,
                    size: Some([max[0] - min[0], max[1] - min[1], max[2] - min[2]]),
                    power_w: power,
                    r_jc: p.r_jc,
                    r_jb: p.r_jb,
                    // case = the face away from the board
                    case_face: Some(match p.side {
                        Side::Top => ferrotherm_core::library::CaseFace::Top,
                        Side::Bottom => ferrotherm_core::library::CaseFace::Bottom,
                    }),
                    mount_axis: "z".into(),
                    derating: None,
                    tj_max_c: p.tj_max_c,
                    network: None,
                });
            } else {
                fr.blocks.push(Block {
                    name,
                    material: Some("mold-compound".into()),
                    pcb: None,
                    min,
                    max,
                    power_w: power,
                    emissivity: Some(0.9),
                    finish: None,
                });
            }
        }
    }
    if guessed > 0 {
        fr.notes.push(format!(
            "{guessed} component heights were guessed from the package name: check them"
        ));
    }
    // extent
    let mut mn = [f64::INFINITY; 3];
    let mut mx = [f64::NEG_INFINITY; 3];
    for b in &fr.blocks {
        for a in 0..3 {
            mn[a] = mn[a].min(b.min[a]);
            mx[a] = mx[a].max(b.max[a]);
        }
    }
    for c in &fr.components {
        let s = c.size.unwrap();
        for a in 0..3 {
            mn[a] = mn[a].min(c.position[a]);
            mx[a] = mx[a].max(c.position[a] + s[a]);
        }
    }
    fr.min = mn;
    fr.max = mx;
    fr
}

/// Power list (CSV): `refdes, power_w [, r_jc, r_jb, tj_max_c]`, header optional.
pub fn apply_power_csv(board: &mut Board, text: &str) -> Result<usize, String> {
    let mut n = 0;
    for (ln, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let sep = if line.contains(';') { ';' } else { ',' };
        let f: Vec<&str> = line
            .split(sep)
            .map(|s| s.trim().trim_matches('"'))
            .collect();
        if f.len() < 2 {
            continue;
        }
        let Ok(p) = f[1].parse::<f64>() else {
            if ln == 0 {
                continue; // header
            }
            return Err(format!("line {}: power '{}' is not a number", ln + 1, f[1]));
        };
        let num = |i: usize| f.get(i).and_then(|s| s.parse::<f64>().ok());
        match board
            .components
            .iter_mut()
            .find(|c| c.refdes.eq_ignore_ascii_case(f[0]))
        {
            Some(c) => {
                c.power_w = Some(p);
                c.r_jc = num(2).or(c.r_jc);
                c.r_jb = num(3).or(c.r_jb);
                c.tj_max_c = num(4).or(c.tj_max_c);
                n += 1;
            }
            None => board
                .warnings
                .push(format!("power list: no component '{}' on the board", f[0])),
        }
    }
    Ok(n)
}
