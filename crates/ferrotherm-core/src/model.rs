// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Turns a [`Case`] into a discrete finite-volume model: grid, per-cell
//! properties, inter-cell conductances, boundary elements and compact
//! (2-resistor) component nodes.

use crate::case::{Boundary, BoundaryKind, Case, Component, EnvironmentUse, Face};
use crate::error::{Error, Result};
use crate::library::{CaseFace, Library, Material};
use crate::mesh::Grid;
use std::collections::HashSet;
use std::path::Path;

const MM: f64 = 1e-3;
/// Minimum number of cells through the thickness of a PCB block.
pub const PCB_MIN_CELLS: usize = 4;

/// Owner of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Fill,
    Block(usize),
    Component(usize),
}

/// Surface condition after resolving defaults, emissivity and ambient.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SurfaceBc {
    /// Fixed surface temperature, °C.
    Fixed { t: f64 },
    /// Convection (h, T_amb) and/or radiation (emissivity, T_rad) to the surroundings.
    Exchange {
        h: f64,
        t_amb: f64,
        emissivity: f64,
        t_rad: f64,
    },
}

/// A boundary patch attached to one unknown.
#[derive(Debug, Clone)]
pub struct BoundaryElement {
    /// Unknown (cell or junction node) behind the surface.
    pub node: usize,
    pub area: f64,
    /// Internal resistance from the node to the surface, K/W.
    pub r_int: f64,
    pub bc: SurfaceBc,
    /// Index of the case boundary that produced it.
    pub source: usize,
}

/// Extra conductance between two unknowns (junction ↔ cell).
#[derive(Debug, Clone, Copy)]
pub struct Link {
    pub a: usize,
    pub b: usize,
    pub g: f64,
}

#[derive(Debug, Clone)]
pub struct BlockInfo {
    pub name: String,
    pub material: String,
    pub power_w: f64,
    pub ncells: usize,
}

/// Which side of a compact component a link or boundary element belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Case,
    Board,
}

#[derive(Debug, Clone)]
pub struct ComponentInfo {
    pub name: String,
    pub package: String,
    pub node: usize,
    pub power_w: f64,
    pub r_jc: Option<f64>,
    pub r_jb: Option<f64>,
    pub case_face: CaseFace,
    pub tj_limit: Option<(f64, String)>,
    /// (link index, side)
    pub links: Vec<(usize, Side)>,
    /// (boundary element index, side)
    pub boundary: Vec<(usize, Side)>,
}

#[derive(Debug, Clone)]
pub struct Model {
    pub grid: Grid,
    pub ncells: usize,
    pub nnodes: usize,
    pub materials: Vec<Material>,
    pub cell_material: Vec<u32>,
    pub owner: Vec<Owner>,
    /// k per cell, W/(m K)
    pub k: Vec<[f64; 3]>,
    /// Heat capacity per node, J/K (junction nodes: 0).
    pub capacity: Vec<f64>,
    /// Heat source per node, W.
    pub source: Vec<f64>,
    /// Conductance to the +x/+y/+z neighbour, W/K (0 at the domain edge).
    pub g: [Vec<f64>; 3],
    pub links: Vec<Link>,
    pub boundary: Vec<BoundaryElement>,
    pub boundary_names: Vec<String>,
    pub blocks: Vec<BlockInfo>,
    pub components: Vec<ComponentInfo>,
    pub ambient_c: Option<f64>,
    pub initial_c: f64,
    pub warnings: Vec<String>,
}

/// Built-in library plus the case's extra library files (relative to `base_dir`).
pub fn load_library(case: &Case, base_dir: &Path) -> Result<Library> {
    let mut lib = Library::builtin();
    for f in &case.libraries {
        lib.merge_file(&base_dir.join(f))?;
    }
    Ok(lib)
}

fn axis_of(s: &str) -> Result<usize> {
    match s {
        "x" => Ok(0),
        "y" => Ok(1),
        "z" => Ok(2),
        o => Err(Error::Invalid(format!("axis must be x, y or z, got '{o}'"))),
    }
}

fn check_box(name: &str, lo: [f64; 3], hi: [f64; 3]) -> Result<()> {
    for a in 0..3 {
        if !(lo[a].is_finite() && hi[a].is_finite() && hi[a] > lo[a]) {
            return Err(Error::Invalid(format!(
                "'{name}': max must be > min on every axis"
            )));
        }
    }
    Ok(())
}

impl Model {
    pub fn build(case: &Case, lib: &Library) -> Result<Model> {
        let mut warnings = Vec::new();
        let d = &case.domain;
        check_box("domain", d.min, d.max)?;
        let lo = d.min.map(|v| v * MM);
        let hi = d.max.map(|v| v * MM);

        // ---- materials -------------------------------------------------
        let mut materials: Vec<Material> = vec![lib.material(&d.material)?.clone()];
        let mat_index = |m: Material, mats: &mut Vec<Material>| -> u32 {
            if let Some(p) = mats.iter().position(|x| x.id == m.id) {
                p as u32
            } else {
                mats.push(m);
                (mats.len() - 1) as u32
            }
        };

        // ---- blocks ----------------------------------------------------
        struct BoxDef {
            lo: [f64; 3],
            hi: [f64; 3],
            mat: u32,
            /// (axis, n): force at least n cells across the block along axis.
            refine: Option<(usize, usize)>,
        }
        let mut block_boxes = Vec::new();
        for (bi, b) in case.blocks.iter().enumerate() {
            check_box(&b.name, b.min, b.max)?;
            let m = match (&b.material, &b.pcb) {
                (Some(id), None) => lib.material(id)?.clone(),
                (None, Some(stack)) => stack.to_material(lib, &format!("pcb:{}", b.name))?,
                _ => {
                    return Err(Error::Invalid(format!(
                        "block '{}': give exactly one of `material` or `pcb`",
                        b.name
                    )))
                }
            };
            let mi = mat_index(m, &mut materials);
            let blo = b.min.map(|v| v * MM);
            let bhi = b.max.map(|v| v * MM);
            for a in 0..3 {
                if blo[a] < lo[a] - 1e-12 || bhi[a] > hi[a] + 1e-12 {
                    warnings.push(format!(
                        "block '{}' extends outside the domain and is clipped",
                        b.name
                    ));
                    break;
                }
            }
            let _ = bi;
            // Boards are thin and strongly orthotropic: resolve the thickness with
            // several cells so the through-plane half-cell resistance at the
            // component footprint does not depend on the grid size.
            let refine = match &b.pcb {
                Some(stack) => Some((stack.normal_axis()?, PCB_MIN_CELLS)),
                None => None,
            };
            block_boxes.push(BoxDef {
                lo: blo,
                hi: bhi,
                mat: mi,
                refine,
            });
        }

        // ---- components ------------------------------------------------
        let body = mat_index(lib.material("compact-body")?.clone(), &mut materials);
        struct CompDef {
            lo: [f64; 3],
            hi: [f64; 3],
            normal: usize,
            r_jc: Option<f64>,
            r_jb: Option<f64>,
            case_face: CaseFace,
        }
        let mut comp_defs = Vec::new();
        let mut comp_infos = Vec::new();
        for c in &case.components {
            let p = lib.package(&c.package)?;
            let normal = axis_of(&c.mount_axis)?;
            let (clo_mm, chi_mm) = component_box(c, lib)?;
            let clo = clo_mm.map(|v| v * MM);
            let chi = chi_mm.map(|v| v * MM);
            check_box(&c.name, clo, chi)?;
            for a in 0..3 {
                if clo[a] < lo[a] - 1e-12 || chi[a] > hi[a] + 1e-12 {
                    return Err(Error::Invalid(format!(
                        "component '{}' is outside the domain",
                        c.name
                    )));
                }
            }
            let r_jc = c.r_jc.or(p.r_jc);
            let r_jb = c.r_jb.or(p.r_jb);
            if r_jc.is_none() && r_jb.is_none() {
                return Err(Error::Invalid(format!(
                    "component '{}': package '{}' has no default r_jc; give `r_jc` (and ideally `r_jb`) from the datasheet",
                    c.name, p.id
                )));
            }
            for r in [r_jc, r_jb].into_iter().flatten() {
                if !(r > 0.0) {
                    return Err(Error::Invalid(format!(
                        "component '{}': resistances must be > 0",
                        c.name
                    )));
                }
            }
            if !(c.power_w >= 0.0) {
                return Err(Error::Invalid(format!(
                    "component '{}': power must be >= 0",
                    c.name
                )));
            }
            let tj_limit = match (c.tj_max_c, &c.derating) {
                (Some(t), _) => Some((t, "user limit".to_string())),
                (None, Some(id)) => {
                    let dr = lib.derating(id)?;
                    Some((dr.tj_max_c, format!("{} ({})", dr.name, dr.source)))
                }
                _ => None,
            };
            let case_face = c.case_face.unwrap_or(p.case_face);
            comp_defs.push(CompDef {
                lo: clo,
                hi: chi,
                normal,
                r_jc,
                r_jb,
                case_face,
            });
            comp_infos.push(ComponentInfo {
                name: c.name.clone(),
                package: p.id.clone(),
                node: 0,
                power_w: c.power_w,
                r_jc,
                r_jb,
                case_face,
                tj_limit,
                links: vec![],
                boundary: vec![],
            });
        }

        // ---- grid ------------------------------------------------------
        let mut planes: [Vec<f64>; 3] = Default::default();
        for b in &block_boxes {
            for a in 0..3 {
                planes[a].push(b.lo[a]);
                planes[a].push(b.hi[a]);
            }
            if let Some((a, nmin)) = b.refine {
                for q in 1..nmin {
                    planes[a].push(b.lo[a] + (b.hi[a] - b.lo[a]) * q as f64 / nmin as f64);
                }
            }
        }
        for c in &comp_defs {
            for a in 0..3 {
                planes[a].push(c.lo[a]);
                planes[a].push(c.hi[a]);
            }
        }
        for bnd in &case.boundaries {
            if let Some(r) = &bnd.region {
                let t = bnd.face.tangential();
                for q in 0..2 {
                    planes[t[q]].push(r.min[q] * MM);
                    planes[t[q]].push(r.max[q] * MM);
                }
            }
        }
        let maxc = case.grid.max_cell_mm.as_vec().map(|v| v * MM);
        if maxc.iter().any(|v| !(*v > 0.0)) {
            return Err(Error::Invalid("grid.max_cell_mm must be > 0".into()));
        }
        let grid = Grid::conforming(lo, hi, planes, maxc, case.grid.min_cells_per_interval);
        let ncells = grid.ncells();
        if ncells > 50_000_000 {
            return Err(Error::Invalid(format!(
                "grid has {ncells} cells; increase grid.max_cell_mm"
            )));
        }

        // ---- ownership -------------------------------------------------
        let mut owner = vec![Owner::Fill; ncells];
        let mut cell_material = vec![0u32; ncells];
        let paint = |lo: [f64; 3], hi: [f64; 3], f: &mut dyn FnMut(usize)| {
            let (i0, i1) = grid.range(0, lo[0], hi[0]);
            let (j0, j1) = grid.range(1, lo[1], hi[1]);
            let (k0, k1) = grid.range(2, lo[2], hi[2]);
            for k in k0..k1 {
                for j in j0..j1 {
                    for i in i0..i1 {
                        f(grid.idx(i, j, k));
                    }
                }
            }
        };
        for (bi, b) in block_boxes.iter().enumerate() {
            paint(b.lo, b.hi, &mut |c| {
                owner[c] = Owner::Block(bi);
                cell_material[c] = b.mat;
            });
        }
        for (ci, c) in comp_defs.iter().enumerate() {
            paint(c.lo, c.hi, &mut |cell| {
                owner[cell] = Owner::Component(ci);
                cell_material[cell] = body;
            });
        }

        // ---- per-cell properties & block sources -------------------------
        let njun = comp_defs.len();
        let nnodes = ncells + njun;
        let mut k = vec![[0.0; 3]; ncells];
        let mut capacity = vec![0.0; nnodes];
        let mut source = vec![0.0; nnodes];
        let mut block_vol = vec![0.0; block_boxes.len()];
        let mut block_cells = vec![0usize; block_boxes.len()];
        for c in 0..ncells {
            let m = &materials[cell_material[c] as usize];
            k[c] = m.k.as_vec();
            let v = grid.volume(c);
            capacity[c] = m.density * m.cp * v;
            if let Owner::Block(b) = owner[c] {
                block_vol[b] += v;
                block_cells[b] += 1;
            }
        }
        let mut blocks = Vec::new();
        for (bi, b) in case.blocks.iter().enumerate() {
            if block_cells[bi] == 0 {
                let msg = format!(
                    "block '{}' owns no cells (covered by later blocks/components or thinner than the grid)",
                    b.name
                );
                if b.power_w != 0.0 {
                    return Err(Error::Invalid(msg));
                }
                warnings.push(msg);
            }
            blocks.push(BlockInfo {
                name: b.name.clone(),
                material: materials[block_boxes[bi].mat as usize].id.clone(),
                power_w: b.power_w,
                ncells: block_cells[bi],
            });
        }
        for c in 0..ncells {
            if let Owner::Block(b) = owner[c] {
                if case.blocks[b].power_w != 0.0 {
                    source[c] = case.blocks[b].power_w * grid.volume(c) / block_vol[b];
                }
            }
        }

        // ---- inter-cell conductances -------------------------------------
        let n = grid.n();
        let stride = [1, n[0], n[0] * n[1]];
        let mut g: [Vec<f64>; 3] = [vec![0.0; ncells], vec![0.0; ncells], vec![0.0; ncells]];
        for c in 0..ncells {
            let p = grid.ijk(c);
            for a in 0..3 {
                if p[a] + 1 < n[a] {
                    let c2 = c + stride[a];
                    let d1 = grid.width(a, p[a]);
                    let d2 = grid.width(a, p[a] + 1);
                    let area = grid.face_area(c, a);
                    g[a][c] = area / (0.5 * d1 / k[c][a] + 0.5 * d2 / k[c2][a]);
                }
            }
        }

        // ---- ambient ---------------------------------------------------
        let ambient_c = match &case.environment {
            Some(e) => {
                let env = lib.environment(&e.preset)?;
                Some(match e.use_ {
                    EnvironmentUse::AmbientMax => env.ambient_max_c,
                    EnvironmentUse::AmbientMin => env.ambient_min_c,
                    EnvironmentUse::InducedMax => env.induced_max_c,
                    EnvironmentUse::InducedMin => env.induced_min_c,
                })
            }
            None => None,
        };
        let bcs: Vec<SurfaceBc> = case
            .boundaries
            .iter()
            .map(|b| resolve_bc(b, lib, ambient_c))
            .collect::<Result<_>>()?;
        let boundary_names: Vec<String> = case
            .boundaries
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if b.name.is_empty() {
                    format!("{}#{}", b.face.name(), i)
                } else {
                    b.name.clone()
                }
            })
            .collect();

        // BC lookup for a point on a domain face.
        let find_bc = |face: Face, u: f64, v: f64| -> Option<usize> {
            let mut found = None;
            for (i, b) in case.boundaries.iter().enumerate() {
                if b.face != face {
                    continue;
                }
                let inside = match &b.region {
                    None => true,
                    Some(r) => {
                        u > r.min[0] * MM
                            && u < r.max[0] * MM
                            && v > r.min[1] * MM
                            && v < r.max[1] * MM
                    }
                };
                if inside {
                    found = Some(i);
                }
            }
            found
        };

        // ---- compact components: junction nodes --------------------------
        let mut links = Vec::new();
        let mut boundary = Vec::new();
        let mut skip: HashSet<(usize, Face)> = HashSet::new();
        for (ci, cd) in comp_defs.iter().enumerate() {
            let node = ncells + ci;
            comp_infos[ci].node = node;
            source[node] = comp_infos[ci].power_w;
            let a = cd.normal;
            let t = match a {
                0 => [1, 2],
                1 => [0, 2],
                _ => [0, 1],
            };
            let (r_top, r_bot, side_top, side_bot) = match cd.case_face {
                CaseFace::Top => (cd.r_jc, cd.r_jb, Side::Case, Side::Board),
                CaseFace::Bottom => (cd.r_jb, cd.r_jc, Side::Board, Side::Case),
            };
            let (a0, a1) = grid.range(a, cd.lo[a], cd.hi[a]);
            let (u0, u1) = grid.range(t[0], cd.lo[t[0]], cd.hi[t[0]]);
            let (v0, v1) = grid.range(t[1], cd.lo[t[1]], cd.hi[t[1]]);
            if a0 == a1 || u0 == u1 || v0 == v1 {
                return Err(Error::Invalid(format!(
                    "component '{}' has no cells",
                    comp_infos[ci].name
                )));
            }
            for (r, side, top) in [(r_top, side_top, true), (r_bot, side_bot, false)] {
                let Some(r) = r else { continue };
                let layer = if top { a1 - 1 } else { a0 };
                let mut cells = Vec::new();
                for vv in v0..v1 {
                    for uu in u0..u1 {
                        let mut p = [0usize; 3];
                        p[a] = layer;
                        p[t[0]] = uu;
                        p[t[1]] = vv;
                        let c = grid.idx(p[0], p[1], p[2]);
                        // only cells still owned by this component take part
                        if owner[c] == Owner::Component(ci) {
                            cells.push((c, p));
                        }
                    }
                }
                let a_tot: f64 = cells.iter().map(|(c, _)| grid.face_area(*c, a)).sum();
                if a_tot <= 0.0 {
                    return Err(Error::Invalid(format!(
                        "component '{}': face fully covered by another component",
                        comp_infos[ci].name
                    )));
                }
                for (c, p) in cells {
                    let ai = grid.face_area(c, a);
                    let r_i = r * a_tot / ai;
                    let at_edge = if top { p[a] + 1 == n[a] } else { p[a] == 0 };
                    if at_edge {
                        let face = match (a, top) {
                            (0, false) => Face::Xmin,
                            (0, true) => Face::Xmax,
                            (1, false) => Face::Ymin,
                            (1, true) => Face::Ymax,
                            (_, false) => Face::Zmin,
                            (_, true) => Face::Zmax,
                        };
                        skip.insert((c, face));
                        let ft = face.tangential();
                        if let Some(bi) = find_bc(
                            face,
                            grid.center(ft[0], p[ft[0]]),
                            grid.center(ft[1], p[ft[1]]),
                        ) {
                            comp_infos[ci].boundary.push((boundary.len(), side));
                            boundary.push(BoundaryElement {
                                node,
                                area: ai,
                                r_int: r_i,
                                bc: bcs[bi],
                                source: bi,
                            });
                        }
                    } else {
                        let nb = if top { c + stride[a] } else { c - stride[a] };
                        let pn = grid.ijk(nb);
                        let d_nb = grid.width(a, pn[a]);
                        let gl = 1.0 / (r_i + 0.5 * d_nb / (k[nb][a] * ai));
                        comp_infos[ci].links.push((links.len(), side));
                        links.push(Link {
                            a: node,
                            b: nb,
                            g: gl,
                        });
                    }
                }
            }
        }

        // ---- boundary elements on the domain faces -----------------------
        for face in Face::ALL {
            let a = face.axis();
            let layer = if face.is_max() { n[a] - 1 } else { 0 };
            let t = face.tangential();
            for vv in 0..n[t[1]] {
                for uu in 0..n[t[0]] {
                    let mut p = [0usize; 3];
                    p[a] = layer;
                    p[t[0]] = uu;
                    p[t[1]] = vv;
                    let c = grid.idx(p[0], p[1], p[2]);
                    if skip.contains(&(c, face)) {
                        continue;
                    }
                    if let Some(bi) = find_bc(face, grid.center(t[0], uu), grid.center(t[1], vv)) {
                        let area = grid.face_area(c, a);
                        let r_int = 0.5 * grid.width(a, p[a]) / (k[c][a] * area);
                        boundary.push(BoundaryElement {
                            node: c,
                            area,
                            r_int,
                            bc: bcs[bi],
                            source: bi,
                        });
                    }
                }
            }
        }

        let has_sink = boundary.iter().any(|e| match e.bc {
            SurfaceBc::Fixed { .. } => true,
            SurfaceBc::Exchange { h, emissivity, .. } => h > 0.0 || emissivity > 0.0,
        });
        if !has_sink && case.solver.mode == crate::case::Mode::Steady {
            return Err(Error::Invalid(
                "steady case without any heat sink: add at least one fixed, convection or radiation boundary".into(),
            ));
        }
        for (bi, b) in case.boundaries.iter().enumerate() {
            if !boundary.iter().any(|e| e.source == bi) && b.kind != BoundaryKind::Adiabatic {
                warnings.push(format!(
                    "boundary '{}' does not touch any cell",
                    boundary_names[bi]
                ));
            }
        }

        let initial_c = case.solver.initial_c.or(ambient_c).unwrap_or(25.0);

        Ok(Model {
            grid,
            ncells,
            nnodes,
            materials,
            cell_material,
            owner,
            k,
            capacity,
            source,
            g,
            links,
            boundary,
            boundary_names,
            blocks,
            components: comp_infos,
            ambient_c,
            initial_c,
            warnings,
        })
    }

    pub fn total_power(&self) -> f64 {
        self.source.iter().sum()
    }
}

/// Bounding box of a component body in case units (mm): `(min, max)`.
pub fn component_box(c: &Component, lib: &Library) -> Result<([f64; 3], [f64; 3])> {
    let p = lib.package(&c.package)?;
    let size = c.size.or(p.size_mm).ok_or_else(|| {
        Error::Invalid(format!(
            "component '{}': package '{}' has no nominal size, give `size`",
            c.name, p.id
        ))
    })?;
    let normal = axis_of(&c.mount_axis)?;
    let tang: [usize; 2] = match normal {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };
    let mut ext = [0.0; 3];
    ext[tang[0]] = size[0];
    ext[tang[1]] = size[1];
    ext[normal] = size[2];
    let lo = c.position;
    let hi = [lo[0] + ext[0], lo[1] + ext[1], lo[2] + ext[2]];
    Ok((lo, hi))
}

fn resolve_bc(b: &Boundary, lib: &Library, ambient: Option<f64>) -> Result<SurfaceBc> {
    let name = if b.name.is_empty() {
        b.face.name().to_string()
    } else {
        b.name.clone()
    };
    let need_amb = |v: Option<f64>| -> Result<f64> {
        v.or(ambient).ok_or_else(|| {
            Error::Invalid(format!(
                "boundary '{name}': no ambient temperature (set `t_ambient_c` or an [environment] preset)"
            ))
        })
    };
    let eps = match (b.emissivity, &b.finish) {
        (Some(e), _) => Some(e),
        (None, Some(f)) => Some(lib.finish(f)?.emissivity),
        _ => None,
    };
    if let Some(e) = eps {
        if !(0.0..=1.0).contains(&e) {
            return Err(Error::Invalid(format!(
                "boundary '{name}': emissivity must be in [0,1]"
            )));
        }
    }
    match b.kind {
        BoundaryKind::Adiabatic => Ok(SurfaceBc::Exchange {
            h: 0.0,
            t_amb: 0.0,
            emissivity: 0.0,
            t_rad: 0.0,
        }),
        BoundaryKind::Fixed => {
            let t = b
                .t_c
                .ok_or_else(|| Error::Invalid(format!("boundary '{name}': fixed needs `t_c`")))?;
            Ok(SurfaceBc::Fixed { t })
        }
        BoundaryKind::Convection => {
            let h = b.h.ok_or_else(|| {
                Error::Invalid(format!("boundary '{name}': convection needs `h`"))
            })?;
            if !(h >= 0.0) {
                return Err(Error::Invalid(format!("boundary '{name}': h must be >= 0")));
            }
            let t_amb = need_amb(b.t_ambient_c)?;
            Ok(SurfaceBc::Exchange {
                h,
                t_amb,
                emissivity: eps.unwrap_or(0.0),
                t_rad: b.t_radiation_c.unwrap_or(t_amb),
            })
        }
        BoundaryKind::Radiation => {
            let e = eps.ok_or_else(|| {
                Error::Invalid(format!(
                    "boundary '{name}': radiation needs `emissivity` or `finish`"
                ))
            })?;
            let t_rad = match b.t_radiation_c {
                Some(t) => t,
                None => need_amb(b.t_ambient_c)?,
            };
            Ok(SurfaceBc::Exchange {
                h: 0.0,
                t_amb: t_rad,
                emissivity: e,
                t_rad,
            })
        }
    }
}
