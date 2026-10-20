// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Case file (TOML) data model.
//!
//! Units in the case file: lengths in **mm**, temperatures in **°C**, power in
//! **W**, heat transfer coefficients in **W/(m² K)**, time in **s**.

use crate::error::{Error, Result};
use crate::library::CaseFace;
use crate::pcb::PcbStack;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    #[serde(default)]
    pub meta: Meta,
    pub domain: Domain,
    #[serde(default)]
    pub grid: GridSettings,
    pub environment: Option<EnvironmentRef>,
    #[serde(default)]
    pub blocks: Vec<Block>,
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub boundaries: Vec<Boundary>,
    #[serde(default)]
    pub solver: SolverSettings,
    /// Air/liquid flow (CFD). Absent or `enabled = false`: conduction only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<FlowSettings>,
    /// Internal surface-to-surface radiation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radiation: Option<RadiationSettings>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fans: Vec<Fan>,
    /// Planar flow resistances: grilles, perforated plates, filters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resistances: Vec<Resistance>,
    /// Parametric plate-fin heat sinks (expanded into blocks).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heatsinks: Vec<HeatSink>,
    /// Monitor points (temperature and velocity history).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub monitors: Vec<Monitor>,
    /// Extra JSON library files, relative to the case file.
    #[serde(default)]
    pub libraries: Vec<String>,
}

// ------------------------------------------------------------------ flow

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Turbulence {
    Laminar,
    /// LVEL algebraic model (Agonafer, Gan-Li & Spalding 1996): wall distance
    /// from a Poisson equation + Spalding's law of the wall.
    #[default]
    Lvel,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    Upwind,
    /// Patankar power-law scheme (default).
    #[default]
    PowerLaw,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowSettings {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Gravity vector, m/s² (default [0, 0, −9.81]: z up).
    #[serde(default = "default_gravity")]
    pub gravity: [f64; 3],
    #[serde(default)]
    pub turbulence: Turbulence,
    #[serde(default)]
    pub scheme: Scheme,
    /// Boussinesq reference temperature, °C (default: ambient).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_temperature_c: Option<f64>,
    /// Outer (SIMPLEC) iterations, steady state or per time step ×10.
    #[serde(default = "default_flow_iters")]
    pub max_iterations: usize,
    /// Normalised residual target (mass, momentum, energy).
    #[serde(default = "default_flow_tol")]
    pub tolerance: f64,
    #[serde(default = "default_relax_u")]
    pub relax_velocity: f64,
    #[serde(default = "default_relax_p")]
    pub relax_pressure: f64,
    #[serde(default = "default_relax_t")]
    pub relax_temperature: f64,
    /// Outer iterations per time step (transient).
    #[serde(default = "default_inner")]
    pub iterations_per_step: usize,
}

impl Default for FlowSettings {
    fn default() -> Self {
        FlowSettings {
            enabled: true,
            gravity: default_gravity(),
            turbulence: Turbulence::default(),
            scheme: Scheme::default(),
            reference_temperature_c: None,
            max_iterations: default_flow_iters(),
            tolerance: default_flow_tol(),
            relax_velocity: default_relax_u(),
            relax_pressure: default_relax_p(),
            relax_temperature: default_relax_t(),
            iterations_per_step: default_inner(),
        }
    }
}

fn yes() -> bool {
    true
}
fn default_gravity() -> [f64; 3] {
    [0.0, 0.0, -9.81]
}
fn default_flow_iters() -> usize {
    500
}
fn default_flow_tol() -> f64 {
    1e-3
}
fn default_relax_u() -> f64 {
    0.6
}
fn default_relax_p() -> f64 {
    0.8
}
fn default_relax_t() -> f64 {
    1.0
}
fn default_inner() -> usize {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadiationSettings {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Rays per surface patch for the view factors (Monte Carlo).
    #[serde(default = "default_rays")]
    pub rays_per_patch: usize,
    /// Surface patch size, mm (coarser = faster).
    #[serde(default = "default_patch")]
    pub patch_size_mm: f64,
    /// Emissivity of surfaces without their own value.
    #[serde(default = "default_eps")]
    pub default_emissivity: f64,
}

impl Default for RadiationSettings {
    fn default() -> Self {
        RadiationSettings {
            enabled: true,
            rays_per_patch: default_rays(),
            patch_size_mm: default_patch(),
            default_emissivity: default_eps(),
        }
    }
}

fn default_rays() -> usize {
    256
}
fn default_patch() -> f64 {
    10.0
}
fn default_eps() -> f64 {
    0.9
}

/// Pressure-loss specification of a grille, perforated plate or filter.
///
/// Δp = ½ ρ ζ u² + 15 μ u / (f d) where u is the approach (face) velocity.
/// The linear term is the laminar thin-plate law ζ = 30/(f² Re), Re = u d/(f ν)
/// (Idelchik, Flow Resistance, 1988, as summarised in Electronics Cooling,
/// May 1998); it is added only when `free_area` and `hole_diameter_mm` are set.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Loss {
    /// Loss coefficient on the approach velocity (from Idelchik, a datasheet or a test).
    #[serde(default)]
    pub zeta: f64,
    /// Open-area ratio f (0..1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_area: Option<f64>,
    /// Hole diameter, mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hole_diameter_mm: Option<f64>,
}

impl Loss {
    /// (linear coefficient Pa·s/m, quadratic coefficient Pa·s²/m²).
    pub fn coefficients(&self, rho: f64, mu: f64) -> (f64, f64) {
        let lin = match (self.free_area, self.hole_diameter_mm) {
            (Some(f), Some(d)) if f > 0.0 && d > 0.0 => 15.0 * mu / (f * d * 1e-3),
            _ => 0.0,
        };
        (lin, 0.5 * rho * self.zeta.max(0.0))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fan {
    pub name: String,
    /// Centre of the fan face, mm. The fan is a plane normal to `direction`.
    pub center: [f64; 3],
    /// Flow direction: "+x", "-x", "+y", "-y", "+z", "-z".
    pub direction: String,
    /// Outer diameter, mm (round fan) …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diameter: Option<f64>,
    /// … with an optional hub that carries no flow, mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub_diameter: Option<f64>,
    /// … or a rectangular face [a, b] in the two in-plane axes, mm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f64; 2]>,
    /// Fan curve: [[volume flow m³/s, static pressure Pa], ...], flow increasing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curve: Vec<[f64; 2]>,
    /// Fixed volume flow, m³/s (instead of a curve).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_m3_s: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resistance {
    pub name: String,
    /// Centre, mm.
    pub center: [f64; 3],
    /// Normal axis of the plane: "x", "y" or "z".
    pub axis: String,
    /// Size in the two in-plane axes (increasing axis order), mm.
    pub size: [f64; 2],
    pub loss: Loss,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeatSink {
    pub name: String,
    #[serde(default = "default_hs_material")]
    pub material: String,
    /// Minimum corner of the base, mm.
    pub position: [f64; 3],
    /// Base size: length along `fin_axis`, width across the fins, thickness, mm.
    pub base: [f64; 3],
    /// Axis normal to the base; fins grow towards +normal (default "z").
    #[serde(default = "default_axis")]
    pub normal: String,
    /// Axis the fins run along (in the base plane), "x" or "y" for a z base.
    pub fin_axis: String,
    pub fin_count: usize,
    pub fin_thickness: f64,
    pub fin_height: f64,
    /// Surface emissivity for radiation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissivity: Option<f64>,
}

fn default_hs_material() -> String {
    "al-6061-t6".into()
}

impl HeatSink {
    /// The base and fins as blocks (geometry in mm).
    pub fn blocks(&self) -> Result<Vec<Block>> {
        let ax = |s: &str| match s {
            "x" => Ok(0usize),
            "y" => Ok(1),
            "z" => Ok(2),
            o => Err(Error::Invalid(format!("heat sink '{}': bad axis '{o}'", self.name))),
        };
        let n = ax(&self.normal)?;
        let f = ax(&self.fin_axis)?;
        if f == n {
            return Err(Error::Invalid(format!(
                "heat sink '{}': fin_axis must lie in the base plane",
                self.name
            )));
        }
        let w = 3 - n - f; // across the fins
        if self.fin_count == 0 || !(self.fin_thickness > 0.0) || !(self.fin_height > 0.0) {
            return Err(Error::Invalid(format!(
                "heat sink '{}': needs fins (count, thickness, height > 0)",
                self.name
            )));
        }
        let (len, width, thick) = (self.base[0], self.base[1], self.base[2]);
        if self.fin_count as f64 * self.fin_thickness > width + 1e-9 {
            return Err(Error::Invalid(format!(
                "heat sink '{}': fins wider than the base",
                self.name
            )));
        }
        let mut out = Vec::new();
        let mut lo = self.position;
        let mut hi = self.position;
        hi[f] += len;
        hi[w] += width;
        hi[n] += thick;
        out.push(Block {
            name: format!("{}/base", self.name),
            material: Some(self.material.clone()),
            pcb: None,
            min: lo,
            max: hi,
            power_w: 0.0,
            emissivity: self.emissivity,
            finish: None,
        });
        let pitch = if self.fin_count > 1 {
            (width - self.fin_thickness) / (self.fin_count - 1) as f64
        } else {
            0.0
        };
        for i in 0..self.fin_count {
            lo = self.position;
            hi = self.position;
            let off = if self.fin_count > 1 {
                i as f64 * pitch
            } else {
                0.5 * (width - self.fin_thickness)
            };
            lo[w] += off;
            hi[w] = lo[w] + self.fin_thickness;
            lo[n] += thick;
            hi[n] = lo[n] + self.fin_height;
            hi[f] += len;
            out.push(Block {
                name: format!("{}/fin{}", self.name, i + 1),
                material: Some(self.material.clone()),
                pcb: None,
                min: lo,
                max: hi,
                power_w: 0.0,
                emissivity: self.emissivity,
                finish: None,
            });
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Monitor {
    pub name: String,
    /// mm
    pub position: [f64; 3],
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Domain {
    pub min: [f64; 3],
    pub max: [f64; 3],
    /// Material filling everything not covered by a block (default `air-still`).
    #[serde(default = "default_fill")]
    pub material: String,
}

fn default_fill() -> String {
    "air-still".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GridSettings {
    /// Maximum cell size, mm (scalar or per axis).
    #[serde(default = "default_max_cell")]
    pub max_cell_mm: MaybeVec3,
    /// Minimum number of cells across every interval between geometry planes.
    #[serde(default = "one")]
    pub min_cells_per_interval: usize,
}

impl Default for GridSettings {
    fn default() -> Self {
        GridSettings {
            max_cell_mm: default_max_cell(),
            min_cells_per_interval: 1,
        }
    }
}

fn default_max_cell() -> MaybeVec3 {
    MaybeVec3::Scalar(2.0)
}
fn one() -> usize {
    1
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MaybeVec3 {
    Scalar(f64),
    Vec3([f64; 3]),
}
impl MaybeVec3 {
    pub fn as_vec(&self) -> [f64; 3] {
        match *self {
            MaybeVec3::Scalar(v) => [v; 3],
            MaybeVec3::Vec3(v) => v,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentUse {
    AmbientMax,
    AmbientMin,
    InducedMax,
    InducedMin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRef {
    /// Environment preset id from the library.
    pub preset: String,
    /// Which temperature of the preset becomes the default ambient.
    #[serde(rename = "use", default = "default_env_use")]
    pub use_: EnvironmentUse,
}

fn default_env_use() -> EnvironmentUse {
    EnvironmentUse::AmbientMax
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub name: String,
    /// Library material id (or use `pcb`).
    pub material: Option<String>,
    /// Effective PCB stack instead of a library material.
    pub pcb: Option<PcbStack>,
    pub min: [f64; 3],
    pub max: [f64; 3],
    /// Uniform volumetric heat load of the whole block, W.
    #[serde(default)]
    pub power_w: f64,
    /// Surface emissivity for internal radiation (or `finish`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissivity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub name: String,
    pub package: String,
    /// Minimum corner, mm.
    pub position: [f64; 3],
    /// Body size, mm (defaults to the package nominal size).
    pub size: Option<[f64; 3]>,
    pub power_w: f64,
    /// Overrides of the package compact model.
    pub r_jc: Option<f64>,
    pub r_jb: Option<f64>,
    pub case_face: Option<CaseFace>,
    /// Axis normal to the board the component sits on (default "z").
    #[serde(default = "default_axis")]
    pub mount_axis: String,
    /// Derating rule id for the Tj check.
    pub derating: Option<String>,
    /// Explicit Tj limit, °C (overrides `derating`).
    pub tj_max_c: Option<f64>,
    /// Multi-node compact model (DELPHI-style network) instead of 2R.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
}

/// Where a surface node of a compact network attaches to the body.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    Top,
    Bottom,
    TopInner,
    TopOuter,
    BottomInner,
    BottomOuter,
    Sides,
    /// Ring of board cells around the footprint (lead/ball joints).
    Leads,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NetNode {
    pub name: String,
    /// Surface the node is attached to; internal node if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<SurfaceKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NetLink {
    pub a: String,
    pub b: String,
    /// Thermal resistance, K/W.
    pub r: f64,
}

/// DELPHI-style multi-node compact model (JEDEC JESD15-4 topology or any
/// other network). Resistances come from the part supplier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// Node that dissipates the power (default "junction").
    #[serde(default = "default_junction")]
    pub junction: String,
    pub nodes: Vec<NetNode>,
    pub links: Vec<NetLink>,
    /// Size of the inner region of top/bottom faces [L, W], mm (default: half the body).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_size_mm: Option<[f64; 2]>,
    /// Width of the lead ring around the footprint, mm (default 1).
    #[serde(default = "default_ring")]
    pub lead_ring_mm: f64,
}

fn default_junction() -> String {
    "junction".into()
}
fn default_ring() -> f64 {
    1.0
}

fn default_axis() -> String {
    "z".into()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Face {
    Xmin,
    Xmax,
    Ymin,
    Ymax,
    Zmin,
    Zmax,
}

impl Face {
    pub const ALL: [Face; 6] = [
        Face::Xmin,
        Face::Xmax,
        Face::Ymin,
        Face::Ymax,
        Face::Zmin,
        Face::Zmax,
    ];
    pub fn axis(self) -> usize {
        match self {
            Face::Xmin | Face::Xmax => 0,
            Face::Ymin | Face::Ymax => 1,
            Face::Zmin | Face::Zmax => 2,
        }
    }
    pub fn is_max(self) -> bool {
        matches!(self, Face::Xmax | Face::Ymax | Face::Zmax)
    }
    pub fn name(self) -> &'static str {
        match self {
            Face::Xmin => "xmin",
            Face::Xmax => "xmax",
            Face::Ymin => "ymin",
            Face::Ymax => "ymax",
            Face::Zmin => "zmin",
            Face::Zmax => "zmax",
        }
    }
    /// The two in-plane axes, in increasing order.
    pub fn tangential(self) -> [usize; 2] {
        match self.axis() {
            0 => [1, 2],
            1 => [0, 2],
            _ => [0, 1],
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BoundaryKind {
    Adiabatic,
    Fixed,
    Convection,
    /// Radiation only (no convection).
    Radiation,
    /// Flow opening at ambient pressure (vent); air enters at ambient temperature.
    Opening,
    /// Fixed inflow (velocity or volume flow) at a given temperature.
    Inlet,
    /// Symmetry plane: no flow through it, no shear, no heat flow.
    Symmetry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Region2 {
    /// mm, in the two tangential coordinates of the face (x,y for z faces; x,z for y faces; y,z for x faces).
    pub min: [f64; 2],
    pub max: [f64; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boundary {
    #[serde(default)]
    pub name: String,
    pub face: Face,
    /// Restrict to a rectangle of the face; whole face if absent.
    pub region: Option<Region2>,
    #[serde(rename = "type")]
    pub kind: BoundaryKind,
    /// Fixed temperature, °C (type = "fixed").
    pub t_c: Option<f64>,
    /// Convection coefficient, W/(m² K).
    pub h: Option<f64>,
    /// Fluid temperature for convection, °C (default: environment ambient).
    pub t_ambient_c: Option<f64>,
    /// Emissivity (adds radiation to a convection boundary, or alone for type = "radiation").
    pub emissivity: Option<f64>,
    /// Finish id from the library, alternative to `emissivity`.
    pub finish: Option<String>,
    /// Radiation sink temperature, °C (default: the ambient).
    pub t_radiation_c: Option<f64>,
    /// Absorbed heat flux on the surface, W/m² (e.g. measured load).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heat_flux_w_m2: Option<f64>,
    /// Solar irradiance, W/m² (absorbed with `absorptivity`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solar_w_m2: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absorptivity: Option<f64>,
    /// Inlet: velocity into the domain, m/s …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_m_s: Option<f64>,
    /// … or volume flow, m³/s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_m3_s: Option<f64>,
    /// Opening: grille/perforated plate on the vent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss: Option<Loss>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Steady,
    Transient,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolverSettings {
    #[serde(default = "default_mode")]
    pub mode: Mode,
    /// Relative residual tolerance of the linear solver.
    #[serde(default = "default_tol")]
    pub tolerance: f64,
    #[serde(default = "default_maxit")]
    pub max_iterations: usize,
    /// Initial temperature, °C (default: ambient).
    pub initial_c: Option<f64>,
    /// Nonlinear (radiation) convergence threshold on temperature change, K.
    #[serde(default = "default_nl_tol")]
    pub nonlinear_tolerance_k: f64,
    #[serde(default = "default_nl_maxit")]
    pub max_nonlinear_iterations: usize,
    pub transient: Option<TransientSettings>,
}

impl Default for SolverSettings {
    fn default() -> Self {
        SolverSettings {
            mode: default_mode(),
            tolerance: default_tol(),
            max_iterations: default_maxit(),
            initial_c: None,
            nonlinear_tolerance_k: default_nl_tol(),
            max_nonlinear_iterations: default_nl_maxit(),
            transient: None,
        }
    }
}

fn default_mode() -> Mode {
    Mode::Steady
}
fn default_tol() -> f64 {
    1e-9
}
fn default_maxit() -> usize {
    20_000
}
fn default_nl_tol() -> f64 {
    1e-3
}
fn default_nl_maxit() -> usize {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransientSettings {
    pub dt_s: f64,
    pub end_s: f64,
    /// Write a full field every N seconds (default: only the end).
    pub output_every_s: Option<f64>,
}

impl Case {
    pub fn from_toml(text: &str) -> Result<Case> {
        toml::from_str(text).map_err(|e| Error::CaseParse(e.to_string()))
    }

    /// Serialise back to TOML (comments of the original file are not preserved).
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| Error::CaseParse(e.to_string()))
    }

    pub fn from_file(path: &Path) -> Result<Case> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_toml(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_round_trips_through_toml() {
        let text = include_str!("../../../examples/mil_enclosure.toml");
        let a = Case::from_toml(text).unwrap();
        let out = a.to_toml().unwrap();
        let b = Case::from_toml(&out).unwrap();
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }
}
