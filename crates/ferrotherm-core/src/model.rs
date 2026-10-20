// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Turns a [`Case`] into a discrete finite-volume model: grid, per-cell
//! properties, inter-cell conductances, boundary elements and compact
//! component networks (2R or DELPHI-style multi-node).

use crate::case::{
    Block, Boundary, BoundaryKind, Case, Component, EnvironmentUse, Face, Network, SurfaceKind,
};
use crate::error::{Error, Result};
use crate::library::{CaseFace, Library, Material};
use crate::mesh::Grid;
use std::collections::HashSet;
use std::path::Path;

const MM: f64 = 1e-3;
/// Minimum number of cells through the thickness of a PCB block.
pub const PCB_MIN_CELLS: usize = 4;
/// Internal resistance used when a surface node sits directly on the domain edge.
const EDGE_R: f64 = 1e-6;

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
    /// Unknown (cell or network node) behind the surface.
    pub node: usize,
    pub area: f64,
    /// Internal resistance from the node to the surface, K/W.
    pub r_int: f64,
    pub bc: SurfaceBc,
    /// Index of the case boundary that produced it.
    pub source: usize,
    /// Heat absorbed at the surface (imposed flux, solar), W.
    pub q_abs: f64,
    /// Axis of the half cell between the node and the surface (domain-face
    /// cells), used to update `r_int` for k(T) materials.
    pub axis: Option<u8>,
}

/// Extra conductance between two unknowns.
#[derive(Debug, Clone, Copy)]
pub struct Link {
    pub a: usize,
    pub b: usize,
    pub g: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockOrigin {
    /// Index into `case.blocks`.
    Case(usize),
    /// Index into `case.heatsinks`.
    HeatSink(usize),
}

#[derive(Debug, Clone)]
pub struct BlockInfo {
    pub name: String,
    pub material: String,
    pub power_w: f64,
    pub ncells: usize,
    pub emissivity: Option<f64>,
    pub origin: BlockOrigin,
}

/// Compatibility alias for 2R surfaces (surface 0 = case, 1 = board).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Case,
    Board,
}

#[derive(Debug, Clone)]
pub struct ComponentInfo {
    pub name: String,
    pub package: String,
    /// Junction (power) node.
    pub node: usize,
    pub power_w: f64,
    pub r_jc: Option<f64>,
    pub r_jb: Option<f64>,
    pub case_face: CaseFace,
    pub tj_limit: Option<(f64, String)>,
    /// True for a multi-node network model.
    pub network: bool,
    /// Every model node of the component: (name, node index); junction first.
    pub nodes: Vec<(String, usize)>,
    /// Surface names; index used by `links` and `boundary`.
    pub surfaces: Vec<String>,
    /// (link index, surface index): heat flowing from the component to the cell.
    pub links: Vec<(usize, u16)>,
    /// (boundary element index, surface index)
    pub boundary: Vec<(usize, u16)>,
}

#[derive(Debug, Clone)]
pub struct Model {
    pub grid: Grid,
    pub ncells: usize,
    pub nnodes: usize,
    pub materials: Vec<Material>,
    pub cell_material: Vec<u32>,
    pub owner: Vec<Owner>,
    /// k per cell at the reference temperature, W/(m K)
    pub k: Vec<[f64; 3]>,
    /// Heat capacity per node, J/K (network nodes: 0).
    pub capacity: Vec<f64>,
    /// Heat source per node, W.
    pub source: Vec<f64>,
    /// Conductance to the +x/+y/+z neighbour, W/K (0 at the domain edge).
    pub g: [Vec<f64>; 3],
    pub links: Vec<Link>,
    pub boundary: Vec<BoundaryElement>,
    pub boundary_names: Vec<String>,
    pub boundary_kinds: Vec<BoundaryKind>,
    pub blocks: Vec<BlockInfo>,
    pub components: Vec<ComponentInfo>,
    /// Per domain face (in `Face::ALL` order): case-boundary index of each
    /// surface cell (`u + n[t0]·v`), −1 = none (adiabatic wall).
    pub face_bc: [Vec<i32>; 6],
    /// Some material has a k(T) table.
    pub has_kt: bool,
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

pub fn axis_of(s: &str) -> Result<usize> {
    match s.trim_start_matches(['+', '-']) {
        "x" => Ok(0),
        "y" => Ok(1),
        "z" => Ok(2),
        o => Err(Error::Invalid(format!("axis must be x, y or z, got '{o}'"))),
    }
}

/// "+x" → (0, +1.0), "-z" → (2, −1.0).
pub fn direction_of(s: &str) -> Result<(usize, f64)> {
    let a = axis_of(s)?;
    let sign = if s.starts_with('-') { -1.0 } else { 1.0 };
    Ok((a, sign))
}

pub fn tangential(a: usize) -> [usize; 2] {
    match a {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
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

/// Every block of the case, heat sinks expanded, with its origin.
pub fn all_blocks(case: &Case) -> Result<Vec<(Block, BlockOrigin)>> {
    let mut out: Vec<(Block, BlockOrigin)> = case
        .blocks
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, b)| (b, BlockOrigin::Case(i)))
        .collect();
    for (i, h) in case.heatsinks.iter().enumerate() {
        for b in h.blocks()? {
            out.push((b, BlockOrigin::HeatSink(i)));
        }
    }
    Ok(out)
}

/// In-plane rectangle of a fan or resistance, mm: (axis, lo[2], hi[2], plane coordinate).
pub fn plane_rect(center: [f64; 3], axis: usize, size: [f64; 2]) -> ([f64; 2], [f64; 2], f64) {
    let t = tangential(axis);
    (
        [center[t[0]] - 0.5 * size[0], center[t[1]] - 0.5 * size[1]],
        [center[t[0]] + 0.5 * size[0], center[t[1]] + 0.5 * size[1]],
        center[axis],
    )
}

struct CompDef {
    lo: [f64; 3],
    hi: [f64; 3],
    normal: usize,
    r_jc: Option<f64>,
    r_jb: Option<f64>,
    case_face: CaseFace,
    network: Option<Network>,
}

impl Model {
    pub fn build(case: &Case, lib: &Library) -> Result<Model> {
        let mut warnings = Vec::new();
        let d = &case.domain;
        check_box("domain", d.min, d.max)?;
        let lo = d.min.map(|v| v * MM);
        let hi = d.max.map(|v| v * MM);
        let flow_on = case.flow.as_ref().map(|f| f.enabled).unwrap_or(false);

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

        // ---- blocks (heat sinks expanded) -----------------------------------
        struct BoxDef {
            lo: [f64; 3],
            hi: [f64; 3],
            mat: u32,
            refine: Option<(usize, usize)>,
        }
        let blocks_all = all_blocks(case)?;
        let mut block_boxes = Vec::new();
        for (b, _) in &blocks_all {
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
            if let Some(tab) = &m.k_table {
                let k = m.k.as_vec();
                if !(k[0] == k[1] && k[1] == k[2]) || tab.is_empty() {
                    return Err(Error::Invalid(format!(
                        "material '{}': k_table is only supported for isotropic materials",
                        m.id
                    )));
                }
            }
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
            // Boards are thin and strongly orthotropic: resolve the thickness.
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
            if !(c.power_w >= 0.0) {
                return Err(Error::Invalid(format!(
                    "component '{}': power must be >= 0",
                    c.name
                )));
            }
            let (r_jc, r_jb) = if c.network.is_some() {
                (None, None)
            } else {
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
                (r_jc, r_jb)
            };
            if let Some(net) = &c.network {
                validate_network(&c.name, net)?;
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
                network: c.network.clone(),
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
                network: c.network.is_some(),
                nodes: vec![],
                surfaces: vec![],
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
            if let Some(net) = &c.network {
                // inner/outer split and lead ring need their own planes
                let t = tangential(c.normal);
                let inner = net.inner_size_mm.map(|s| [s[0] * MM, s[1] * MM]).unwrap_or([
                    0.5 * (c.hi[t[0]] - c.lo[t[0]]),
                    0.5 * (c.hi[t[1]] - c.lo[t[1]]),
                ]);
                for q in 0..2 {
                    let mid = 0.5 * (c.lo[t[q]] + c.hi[t[q]]);
                    planes[t[q]].push(mid - 0.5 * inner[q]);
                    planes[t[q]].push(mid + 0.5 * inner[q]);
                    if net.nodes.iter().any(|n| n.surface == Some(SurfaceKind::Leads)) {
                        planes[t[q]].push(c.lo[t[q]] - net.lead_ring_mm * MM);
                        planes[t[q]].push(c.hi[t[q]] + net.lead_ring_mm * MM);
                    }
                }
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
        for f in &case.fans {
            let (a, _) = direction_of(&f.direction)?;
            let size = fan_extent(f)?;
            let (rlo, rhi, x) = plane_rect(f.center, a, size);
            planes[a].push(x * MM);
            let t = tangential(a);
            for q in 0..2 {
                planes[t[q]].push(rlo[q] * MM);
                planes[t[q]].push(rhi[q] * MM);
            }
        }
        for r in &case.resistances {
            let a = axis_of(&r.axis)?;
            let (rlo, rhi, x) = plane_rect(r.center, a, r.size);
            planes[a].push(x * MM);
            let t = tangential(a);
            for q in 0..2 {
                planes[t[q]].push(rlo[q] * MM);
                planes[t[q]].push(rhi[q] * MM);
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

        // ---- network node count -------------------------------------------
        let mut nnodes = ncells;
        for (ci, cd) in comp_defs.iter().enumerate() {
            match &cd.network {
                None => {
                    comp_infos[ci].node = nnodes;
                    comp_infos[ci].nodes = vec![("junction".into(), nnodes)];
                    comp_infos[ci].surfaces = vec!["case".into(), "board".into()];
                    nnodes += 1;
                }
                Some(net) => {
                    let mut names: Vec<String> = net.nodes.iter().map(|n| n.name.clone()).collect();
                    if !names.contains(&net.junction) {
                        names.insert(0, net.junction.clone());
                    } else {
                        let p = names.iter().position(|n| *n == net.junction).unwrap();
                        let j = names.remove(p);
                        names.insert(0, j);
                    }
                    comp_infos[ci].node = nnodes;
                    for n in names {
                        comp_infos[ci].nodes.push((n, nnodes));
                        nnodes += 1;
                    }
                }
            }
        }

        // ---- per-cell properties & block sources -------------------------
        let has_kt = materials.iter().any(|m| m.k_table.is_some());
        let t_ref = case.solver.initial_c.unwrap_or(25.0);
        let mut k = vec![[0.0; 3]; ncells];
        let mut capacity = vec![0.0; nnodes];
        let mut source = vec![0.0; nnodes];
        let mut block_vol = vec![0.0; block_boxes.len()];
        let mut block_cells = vec![0usize; block_boxes.len()];
        for c in 0..ncells {
            let m = &materials[cell_material[c] as usize];
            k[c] = m.k_at(t_ref);
            let v = grid.volume(c);
            capacity[c] = m.density * m.cp * v;
            if let Owner::Block(b) = owner[c] {
                block_vol[b] += v;
                block_cells[b] += 1;
            }
        }
        let mut blocks = Vec::new();
        for (bi, (b, origin)) in blocks_all.iter().enumerate() {
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
            let emissivity = match (b.emissivity, &b.finish) {
                (Some(e), _) => Some(e),
                (None, Some(f)) => Some(lib.finish(f)?.emissivity),
                _ => None,
            };
            blocks.push(BlockInfo {
                name: b.name.clone(),
                material: materials[block_boxes[bi].mat as usize].id.clone(),
                power_w: b.power_w,
                ncells: block_cells[bi],
                emissivity,
                origin: *origin,
            });
        }
        for c in 0..ncells {
            if let Owner::Block(b) = owner[c] {
                if blocks_all[b].0.power_w != 0.0 {
                    source[c] = blocks_all[b].0.power_w * grid.volume(c) / block_vol[b];
                }
            }
        }

        // ---- inter-cell conductances -------------------------------------
        let n = grid.n();
        let stride = [1, n[0], n[0] * n[1]];
        let g = conductances(&grid, &k);

        // ---- ambient and boundaries ----------------------------------------
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
        let bcs: Vec<Option<SurfaceBc>> = case
            .boundaries
            .iter()
            .map(|b| resolve_bc(b, lib, ambient_c))
            .collect::<Result<_>>()?;
        let q_flux: Vec<f64> = case
            .boundaries
            .iter()
            .map(|b| {
                b.heat_flux_w_m2.unwrap_or(0.0)
                    + b.solar_w_m2.unwrap_or(0.0) * b.absorptivity.unwrap_or(1.0)
            })
            .collect();
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
        let boundary_kinds: Vec<BoundaryKind> = case.boundaries.iter().map(|b| b.kind).collect();
        if !flow_on {
            for (i, b) in case.boundaries.iter().enumerate() {
                if matches!(b.kind, BoundaryKind::Opening | BoundaryKind::Inlet) {
                    warnings.push(format!(
                        "boundary '{}' is a flow boundary but [flow] is off: treated as adiabatic",
                        boundary_names[i]
                    ));
                }
            }
            if !case.fans.is_empty() || !case.resistances.is_empty() {
                warnings.push("fans and resistances have no effect without [flow]".into());
            }
        }

        // case boundary at a point of a domain face (last one wins)
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
        let mut face_bc: [Vec<i32>; 6] = Default::default();
        for (fi, face) in Face::ALL.iter().enumerate() {
            let t = face.tangential();
            let mut v = vec![-1i32; n[t[0]] * n[t[1]]];
            for vv in 0..n[t[1]] {
                for uu in 0..n[t[0]] {
                    if let Some(b) = find_bc(*face, grid.center(t[0], uu), grid.center(t[1], vv)) {
                        v[uu + n[t[0]] * vv] = b as i32;
                    }
                }
            }
            face_bc[fi] = v;
        }
        let face_index = |f: Face| Face::ALL.iter().position(|x| *x == f).unwrap();
        let bc_at = |face: Face, p: [usize; 3]| -> Option<usize> {
            let t = face.tangential();
            let v = face_bc[face_index(face)][p[t[0]] + n[t[0]] * p[t[1]]];
            if v >= 0 {
                Some(v as usize)
            } else {
                None
            }
        };
        let face_of = |a: usize, top: bool| match (a, top) {
            (0, false) => Face::Xmin,
            (0, true) => Face::Xmax,
            (1, false) => Face::Ymin,
            (1, true) => Face::Ymax,
            (_, false) => Face::Zmin,
            (_, true) => Face::Zmax,
        };

        // ---- compact components ---------------------------------------
        let mut links = Vec::new();
        let mut boundary = Vec::new();
        let mut skip: HashSet<(usize, Face)> = HashSet::new();
        for (ci, cd) in comp_defs.iter().enumerate() {
            let jn = comp_infos[ci].node;
            source[jn] = comp_infos[ci].power_w;
            let a = cd.normal;
            let t = tangential(a);
            let (a0, a1) = grid.range(a, cd.lo[a], cd.hi[a]);
            let (u0, u1) = grid.range(t[0], cd.lo[t[0]], cd.hi[t[0]]);
            let (v0, v1) = grid.range(t[1], cd.lo[t[1]], cd.hi[t[1]]);
            if a0 == a1 || u0 == u1 || v0 == v1 {
                return Err(Error::Invalid(format!(
                    "component '{}' has no cells",
                    comp_infos[ci].name
                )));
            }
            // cells of the top or bottom layer, optionally limited to the inner/outer part
            let layer_cells = |top: bool, part: Option<(bool, [f64; 2])>| -> Vec<(usize, [usize; 3])> {
                let layer = if top { a1 - 1 } else { a0 };
                let mut cells = Vec::new();
                for vv in v0..v1 {
                    for uu in u0..u1 {
                        let mut p = [0usize; 3];
                        p[a] = layer;
                        p[t[0]] = uu;
                        p[t[1]] = vv;
                        let c = grid.idx(p[0], p[1], p[2]);
                        if owner[c] != Owner::Component(ci) {
                            continue;
                        }
                        if let Some((inner, size)) = part {
                            let mid0 = 0.5 * (cd.lo[t[0]] + cd.hi[t[0]]);
                            let mid1 = 0.5 * (cd.lo[t[1]] + cd.hi[t[1]]);
                            let inside = (grid.center(t[0], uu) - mid0).abs() < 0.5 * size[0]
                                && (grid.center(t[1], vv) - mid1).abs() < 0.5 * size[1];
                            if inside != inner {
                                continue;
                            }
                        }
                        cells.push((c, p));
                    }
                }
                cells
            };
            // connect `node` to the outside neighbours of `cells` across axis `ax`
            // in direction `dir` (+1/−1); `r_spread` = resistance distributed by area
            let mut attach = |node: usize,
                              cells: &[(usize, [usize; 3])],
                              ax: usize,
                              dir: i32,
                              r_spread: Option<f64>,
                              surf: u16,
                              links: &mut Vec<Link>,
                              boundary: &mut Vec<BoundaryElement>,
                              info: &mut ComponentInfo|
             -> Result<()> {
                let a_tot: f64 = cells.iter().map(|(c, _)| grid.face_area(*c, ax)).sum();
                if a_tot <= 0.0 {
                    return Err(Error::Invalid(format!(
                        "component '{}': a model surface has no free cells (covered by another component?)",
                        info.name
                    )));
                }
                for (c, p) in cells {
                    let ai = grid.face_area(*c, ax);
                    let r_i = r_spread.map(|r| r * a_tot / ai).unwrap_or(0.0);
                    let at_edge = if dir > 0 {
                        p[ax] + 1 == n[ax]
                    } else {
                        p[ax] == 0
                    };
                    if at_edge {
                        let face = face_of(ax, dir > 0);
                        skip.insert((*c, face));
                        if let Some(bi) = bc_at(face, *p) {
                            if let Some(bc) = bcs[bi] {
                                info.boundary.push((boundary.len(), surf));
                                boundary.push(BoundaryElement {
                                    node,
                                    area: ai,
                                    r_int: r_i.max(EDGE_R),
                                    bc,
                                    source: bi,
                                    q_abs: q_flux[bi] * ai,
                                    axis: None,
                                });
                            }
                        }
                    } else {
                        let nb = if dir > 0 { c + stride[ax] } else { c - stride[ax] };
                        let pn = grid.ijk(nb);
                        let d_nb = grid.width(ax, pn[ax]);
                        let gl = 1.0 / (r_i + 0.5 * d_nb / (k[nb][ax] * ai));
                        info.links.push((links.len(), surf));
                        links.push(Link {
                            a: node,
                            b: nb,
                            g: gl,
                        });
                    }
                }
                Ok(())
            };
            let mut info = std::mem::replace(
                &mut comp_infos[ci],
                ComponentInfo {
                    name: String::new(),
                    package: String::new(),
                    node: 0,
                    power_w: 0.0,
                    r_jc: None,
                    r_jb: None,
                    case_face: CaseFace::Top,
                    tj_limit: None,
                    network: false,
                    nodes: vec![],
                    surfaces: vec![],
                    links: vec![],
                    boundary: vec![],
                },
            );
            match &cd.network {
                None => {
                    // 2R: surface 0 = case, 1 = board
                    let (r_top, r_bot, s_top, s_bot) = match cd.case_face {
                        CaseFace::Top => (cd.r_jc, cd.r_jb, 0u16, 1u16),
                        CaseFace::Bottom => (cd.r_jb, cd.r_jc, 1u16, 0u16),
                    };
                    for (r, surf, top) in [(r_top, s_top, true), (r_bot, s_bot, false)] {
                        let Some(r) = r else { continue };
                        let cells = layer_cells(top, None);
                        attach(
                            jn,
                            &cells,
                            a,
                            if top { 1 } else { -1 },
                            Some(r),
                            surf,
                            &mut links,
                            &mut boundary,
                            &mut info,
                        )?;
                    }
                }
                Some(net) => {
                    let node_list = info.nodes.clone();
                    let node_of = |name: &str| -> usize {
                        node_list.iter().find(|x| x.0 == name).map(|x| x.1).unwrap()
                    };
                    for l in &net.links {
                        links.push(Link {
                            a: node_of(&l.a),
                            b: node_of(&l.b),
                            g: 1.0 / l.r,
                        });
                    }
                    let inner = net.inner_size_mm.map(|s| [s[0] * MM, s[1] * MM]).unwrap_or([
                        0.5 * (cd.hi[t[0]] - cd.lo[t[0]]),
                        0.5 * (cd.hi[t[1]] - cd.lo[t[1]]),
                    ]);
                    for nn in &net.nodes {
                        let Some(kind) = nn.surface else { continue };
                        let node = node_of(&nn.name);
                        let surf = info.surfaces.len() as u16;
                        info.surfaces.push(nn.name.clone());
                        match kind {
                            SurfaceKind::Top | SurfaceKind::Bottom => {
                                let top = kind == SurfaceKind::Top;
                                let cells = layer_cells(top, None);
                                attach(node, &cells, a, if top { 1 } else { -1 }, None, surf, &mut links, &mut boundary, &mut info)?;
                            }
                            SurfaceKind::TopInner
                            | SurfaceKind::TopOuter
                            | SurfaceKind::BottomInner
                            | SurfaceKind::BottomOuter => {
                                let top = matches!(kind, SurfaceKind::TopInner | SurfaceKind::TopOuter);
                                let is_inner = matches!(kind, SurfaceKind::TopInner | SurfaceKind::BottomInner);
                                let mut cells = layer_cells(top, Some((is_inner, inner)));
                                if cells.is_empty() && is_inner {
                                    // inner region smaller than one cell: use the centre cell
                                    let all = layer_cells(top, None);
                                    let mid = [0.5 * (cd.lo[t[0]] + cd.hi[t[0]]), 0.5 * (cd.lo[t[1]] + cd.hi[t[1]])];
                                    if let Some(best) = all.iter().min_by(|x, y| {
                                        let dx = |q: &(usize, [usize; 3])| {
                                            (grid.center(t[0], q.1[t[0]]) - mid[0]).powi(2)
                                                + (grid.center(t[1], q.1[t[1]]) - mid[1]).powi(2)
                                        };
                                        dx(x).partial_cmp(&dx(y)).unwrap()
                                    }) {
                                        cells.push(*best);
                                    }
                                }
                                attach(node, &cells, a, if top { 1 } else { -1 }, None, surf, &mut links, &mut boundary, &mut info)?;
                            }
                            SurfaceKind::Sides => {
                                for ax in t {
                                    for dir in [-1i32, 1] {
                                        let mut cells = Vec::new();
                                        let (r0, r1) = if ax == t[0] { (u0, u1) } else { (v0, v1) };
                                        let layer = if dir > 0 { r1 - 1 } else { r0 };
                                        let other = if ax == t[0] { t[1] } else { t[0] };
                                        let (o0, o1) = if other == t[0] { (u0, u1) } else { (v0, v1) };
                                        for aa in a0..a1 {
                                            for oo in o0..o1 {
                                                let mut p = [0usize; 3];
                                                p[a] = aa;
                                                p[ax] = layer;
                                                p[other] = oo;
                                                let c = grid.idx(p[0], p[1], p[2]);
                                                if owner[c] == Owner::Component(ci) {
                                                    cells.push((c, p));
                                                }
                                            }
                                        }
                                        if !cells.is_empty() {
                                            attach(node, &cells, ax, dir, None, surf, &mut links, &mut boundary, &mut info)?;
                                        }
                                    }
                                }
                            }
                            SurfaceKind::Leads => {
                                if a0 == 0 {
                                    warnings.push(format!(
                                        "component '{}': no board below the body for the leads node",
                                        info.name
                                    ));
                                    continue;
                                }
                                let ring = net.lead_ring_mm * MM;
                                let (r00, r01) = grid.range(t[0], cd.lo[t[0]] - ring, cd.hi[t[0]] + ring);
                                let (r10, r11) = grid.range(t[1], cd.lo[t[1]] - ring, cd.hi[t[1]] + ring);
                                let layer = a0 - 1;
                                let mut a_tot = 0.0;
                                let mut cells = Vec::new();
                                for vv in r10..r11 {
                                    for uu in r00..r01 {
                                        if (u0..u1).contains(&uu) && (v0..v1).contains(&vv) {
                                            continue;
                                        }
                                        let mut p = [0usize; 3];
                                        p[a] = layer;
                                        p[t[0]] = uu;
                                        p[t[1]] = vv;
                                        let c = grid.idx(p[0], p[1], p[2]);
                                        a_tot += grid.face_area(c, a);
                                        cells.push(c);
                                    }
                                }
                                if a_tot <= 0.0 {
                                    return Err(Error::Invalid(format!(
                                        "component '{}': lead ring has no cells (increase lead_ring_mm)",
                                        info.name
                                    )));
                                }
                                for c in cells {
                                    let ai = grid.face_area(c, a);
                                    let pn = grid.ijk(c);
                                    let gl = k[c][a] * ai / (0.5 * grid.width(a, pn[a]));
                                    info.links.push((links.len(), surf));
                                    links.push(Link { a: node, b: c, g: gl });
                                }
                            }
                        }
                    }
                }
            }
            comp_infos[ci] = info;
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
                    if let Some(bi) = bc_at(face, p) {
                        if let Some(bc) = bcs[bi] {
                            let area = grid.face_area(c, a);
                            let r_int = 0.5 * grid.width(a, p[a]) / (k[c][a] * area);
                            boundary.push(BoundaryElement {
                                node: c,
                                area,
                                r_int,
                                bc,
                                source: bi,
                                q_abs: q_flux[bi] * area,
                                axis: Some(a as u8),
                            });
                        }
                    }
                }
            }
        }

        let has_open = flow_on
            && case
                .boundaries
                .iter()
                .any(|b| matches!(b.kind, BoundaryKind::Opening | BoundaryKind::Inlet));
        let has_sink = has_open
            || boundary.iter().any(|e| match e.bc {
                SurfaceBc::Fixed { .. } => true,
                SurfaceBc::Exchange { h, emissivity, .. } => h > 0.0 || emissivity > 0.0,
            });
        if !has_sink && case.solver.mode == crate::case::Mode::Steady {
            return Err(Error::Invalid(
                "steady case without any heat sink: add at least one fixed, convection, radiation or opening boundary".into(),
            ));
        }
        for (bi, b) in case.boundaries.iter().enumerate() {
            let touches = face_bc[face_index(b.face)].iter().any(|v| *v == bi as i32);
            if !touches && !matches!(b.kind, BoundaryKind::Adiabatic | BoundaryKind::Symmetry) {
                warnings.push(format!(
                    "boundary '{}' does not touch any cell (covered by a later boundary?)",
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
            boundary_kinds,
            blocks,
            components: comp_infos,
            face_bc,
            has_kt,
            ambient_c,
            initial_c,
            warnings,
        })
    }

    /// Imposed power: sources plus absorbed surface fluxes, W.
    pub fn total_power(&self) -> f64 {
        self.source.iter().sum::<f64>() + self.boundary.iter().map(|e| e.q_abs).sum::<f64>()
    }

    /// Material of a cell.
    pub fn material(&self, c: usize) -> &Material {
        &self.materials[self.cell_material[c] as usize]
    }

    /// Per-cell conductivity at the cell temperatures (k(T) materials).
    pub fn k_at(&self, t: &[f64]) -> Vec<[f64; 3]> {
        (0..self.ncells)
            .map(|c| {
                let m = self.material(c);
                if m.k_table.is_some() {
                    m.k_at(t[c])
                } else {
                    self.k[c]
                }
            })
            .collect()
    }

    /// Boundary elements with the half-cell resistance updated for the
    /// current conductivity (k(T) materials); borrowed when unchanged.
    pub fn boundary_at<'s>(&'s self, k_now: &[[f64; 3]]) -> std::borrow::Cow<'s, [BoundaryElement]> {
        if !self.has_kt {
            return std::borrow::Cow::Borrowed(&self.boundary);
        }
        let mut v = self.boundary.clone();
        for e in v.iter_mut() {
            if let Some(a) = e.axis {
                if e.node < self.ncells {
                    let a = a as usize;
                    e.r_int *= self.k[e.node][a] / k_now[e.node][a];
                }
            }
        }
        std::borrow::Cow::Owned(v)
    }

    /// Case-boundary index of the domain-face cell `p` on `face`.
    pub fn bc_at(&self, face: Face, p: [usize; 3]) -> Option<usize> {
        let n = self.grid.n();
        let t = face.tangential();
        let fi = Face::ALL.iter().position(|x| *x == face).unwrap();
        let v = self.face_bc[fi][p[t[0]] + n[t[0]] * p[t[1]]];
        (v >= 0).then_some(v as usize)
    }
}

/// Harmonic-mean conductances to the +axis neighbour.
pub fn conductances(grid: &Grid, k: &[[f64; 3]]) -> [Vec<f64>; 3] {
    let n = grid.n();
    let nc = grid.ncells();
    let stride = [1, n[0], n[0] * n[1]];
    let mut g: [Vec<f64>; 3] = [vec![0.0; nc], vec![0.0; nc], vec![0.0; nc]];
    for c in 0..nc {
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
    g
}

fn validate_network(name: &str, net: &Network) -> Result<()> {
    let mut names: Vec<&str> = net.nodes.iter().map(|n| n.name.as_str()).collect();
    if !names.contains(&net.junction.as_str()) {
        names.push(&net.junction);
    }
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != names.len() {
        return Err(Error::Invalid(format!("component '{name}': duplicate network node names")));
    }
    for l in &net.links {
        if !names.contains(&l.a.as_str()) || !names.contains(&l.b.as_str()) {
            return Err(Error::Invalid(format!(
                "component '{name}': network link {}–{} uses an unknown node",
                l.a, l.b
            )));
        }
        if !(l.r > 0.0) {
            return Err(Error::Invalid(format!(
                "component '{name}': network resistances must be > 0"
            )));
        }
    }
    if !net.nodes.iter().any(|n| n.surface.is_some()) {
        return Err(Error::Invalid(format!(
            "component '{name}': the network needs at least one surface node"
        )));
    }
    Ok(())
}

/// In-plane size of a fan for grid planes, mm.
pub fn fan_extent(f: &crate::case::Fan) -> Result<[f64; 2]> {
    match (f.size, f.diameter) {
        (Some(s), _) => Ok(s),
        (None, Some(d)) => Ok([d, d]),
        _ => Err(Error::Invalid(format!(
            "fan '{}': give `diameter` or `size`",
            f.name
        ))),
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
    let tang = tangential(normal);
    let mut ext = [0.0; 3];
    ext[tang[0]] = size[0];
    ext[tang[1]] = size[1];
    ext[normal] = size[2];
    let lo = c.position;
    let hi = [lo[0] + ext[0], lo[1] + ext[1], lo[2] + ext[2]];
    Ok((lo, hi))
}

fn resolve_bc(b: &Boundary, lib: &Library, ambient: Option<f64>) -> Result<Option<SurfaceBc>> {
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
    if let Some(a) = b.absorptivity {
        if !(0.0..=1.0).contains(&a) {
            return Err(Error::Invalid(format!(
                "boundary '{name}': absorptivity must be in [0,1]"
            )));
        }
    }
    Ok(Some(match b.kind {
        BoundaryKind::Symmetry => return Ok(None),
        BoundaryKind::Adiabatic => SurfaceBc::Exchange {
            h: 0.0,
            t_amb: 0.0,
            emissivity: 0.0,
            t_rad: 0.0,
        },
        BoundaryKind::Fixed => {
            let t = b
                .t_c
                .ok_or_else(|| Error::Invalid(format!("boundary '{name}': fixed needs `t_c`")))?;
            SurfaceBc::Fixed { t }
        }
        BoundaryKind::Convection => {
            let h = b.h.ok_or_else(|| {
                Error::Invalid(format!("boundary '{name}': convection needs `h`"))
            })?;
            if !(h >= 0.0) {
                return Err(Error::Invalid(format!("boundary '{name}': h must be >= 0")));
            }
            let t_amb = need_amb(b.t_ambient_c)?;
            SurfaceBc::Exchange {
                h,
                t_amb,
                emissivity: eps.unwrap_or(0.0),
                t_rad: b.t_radiation_c.unwrap_or(t_amb),
            }
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
            SurfaceBc::Exchange {
                h: 0.0,
                t_amb: t_rad,
                emissivity: e,
                t_rad,
            }
        }
        BoundaryKind::Opening | BoundaryKind::Inlet => {
            // validated by the flow solver; thermally the face is handled by
            // advection (or adiabatic when the cell is solid)
            if b.kind == BoundaryKind::Inlet && b.velocity_m_s.is_none() && b.flow_m3_s.is_none() {
                return Err(Error::Invalid(format!(
                    "boundary '{name}': inlet needs `velocity_m_s` or `flow_m3_s`"
                )));
            }
            need_amb(b.t_ambient_c)?;
            return Ok(None);
        }
    }))
}
