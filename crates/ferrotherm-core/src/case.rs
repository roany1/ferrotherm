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
    /// Extra JSON library files, relative to the case file.
    #[serde(default)]
    pub libraries: Vec<String>,
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
