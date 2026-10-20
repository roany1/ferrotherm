// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Effective orthotropic properties of a printed circuit board stack-up.
//!
//! Standard layered model: in-plane conductivity is the thickness-weighted
//! average (layers in parallel), through-plane conductivity is the series
//! combination. Each copper layer is a copper/dielectric mix weighted by its
//! coverage fraction. This is the usual "effective conductivity" board model;
//! it does not resolve individual traces or thermal vias (vias can be added
//! through `via_fraction`, which adds copper columns in the thickness direction).

use crate::error::{Error, Result};
use crate::library::{Library, Material};
use serde::{Deserialize, Serialize};

/// Thickness of 1 oz/ft^2 copper, metres (35 µm, the industry convention).
pub const OZ_TO_M: f64 = 35.0e-6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopperLayer {
    /// Copper weight in oz/ft^2 (1 oz ≈ 35 µm).
    pub oz: f64,
    /// Fraction of the layer area covered by copper, 0..1.
    pub coverage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PcbStack {
    /// Total board thickness, mm.
    pub thickness_mm: f64,
    /// Dielectric material id (default `fr4`).
    #[serde(default = "default_dielectric")]
    pub dielectric: String,
    /// Copper material id (default `cu-c11000`).
    #[serde(default = "default_copper")]
    pub copper: String,
    /// Explicit list of copper layers...
    #[serde(default)]
    pub copper_layers: Vec<CopperLayer>,
    /// ...or a uniform shorthand: number of layers, oz and coverage.
    pub layers: Option<usize>,
    pub copper_oz: Option<f64>,
    pub coverage: Option<f64>,
    /// Area fraction of plated thermal vias (copper barrel cross-section / board area).
    #[serde(default)]
    pub via_fraction: f64,
    /// Axis normal to the board: "x", "y" or "z" (default "z").
    #[serde(default = "default_normal")]
    pub normal: String,
}

fn default_dielectric() -> String {
    "fr4".into()
}
fn default_copper() -> String {
    "cu-c11000".into()
}
fn default_normal() -> String {
    "z".into()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveBoard {
    pub k_in_plane: f64,
    pub k_through: f64,
    pub density: f64,
    pub cp: f64,
    pub copper_thickness_m: f64,
}

impl PcbStack {
    fn layers_resolved(&self) -> Result<Vec<CopperLayer>> {
        if !self.copper_layers.is_empty() {
            return Ok(self.copper_layers.clone());
        }
        match (self.layers, self.copper_oz, self.coverage) {
            (Some(n), Some(oz), Some(cov)) => Ok(vec![CopperLayer { oz, coverage: cov }; n]),
            _ => Err(Error::Invalid(
                "pcb: give either `copper_layers` or all of `layers`, `copper_oz`, `coverage`"
                    .into(),
            )),
        }
    }

    pub fn normal_axis(&self) -> Result<usize> {
        match self.normal.as_str() {
            "x" => Ok(0),
            "y" => Ok(1),
            "z" => Ok(2),
            o => Err(Error::Invalid(format!(
                "pcb normal must be x, y or z, got '{o}'"
            ))),
        }
    }

    pub fn effective(&self, lib: &Library) -> Result<EffectiveBoard> {
        let diel = lib.material(&self.dielectric)?;
        let cu = lib.material(&self.copper)?;
        let layers = self.layers_resolved()?;
        let t_total = self.thickness_mm * 1e-3;
        if !(t_total > 0.0) {
            return Err(Error::Invalid("pcb thickness must be > 0".into()));
        }
        if !(0.0..1.0).contains(&self.via_fraction) {
            return Err(Error::Invalid("pcb via_fraction must be in [0, 1)".into()));
        }
        let kd = diel.k.as_vec();
        let (kd_in, kd_th) = (0.5 * (kd[0] + kd[1]), kd[2]);
        let kc = cu.k.as_vec()[0];

        let mut t_cu = 0.0;
        let mut sum_tk_in = 0.0; // parallel (in-plane)
        let mut sum_t_over_k = 0.0; // series (through-plane)
        let mut sum_rho_t = 0.0;
        let mut sum_rhocp_t = 0.0;
        for l in &layers {
            if !(0.0..=1.0).contains(&l.coverage) || !(l.oz > 0.0) {
                return Err(Error::Invalid(
                    "pcb copper layer: oz > 0 and coverage in [0,1]".into(),
                ));
            }
            let t = l.oz * OZ_TO_M;
            t_cu += t;
            let k_in = l.coverage * kc + (1.0 - l.coverage) * kd_in;
            let k_th = l.coverage * kc + (1.0 - l.coverage) * kd_th;
            sum_tk_in += t * k_in;
            sum_t_over_k += t / k_th;
            let rho = l.coverage * cu.density + (1.0 - l.coverage) * diel.density;
            let rhocp =
                l.coverage * cu.density * cu.cp + (1.0 - l.coverage) * diel.density * diel.cp;
            sum_rho_t += rho * t;
            sum_rhocp_t += rhocp * t;
        }
        let t_d = t_total - t_cu;
        if t_d <= 0.0 {
            return Err(Error::Invalid(format!(
                "pcb: copper ({:.3} mm) is thicker than the board ({:.3} mm)",
                t_cu * 1e3,
                self.thickness_mm
            )));
        }
        sum_tk_in += t_d * kd_in;
        sum_t_over_k += t_d / kd_th;
        sum_rho_t += t_d * diel.density;
        sum_rhocp_t += t_d * diel.density * diel.cp;

        let mut k_in = sum_tk_in / t_total;
        let mut k_th = t_total / sum_t_over_k;
        // Thermal vias: copper columns in parallel with the laminate, through-plane.
        let f = self.via_fraction;
        k_th = f * kc + (1.0 - f) * k_th;
        k_in = f * kc + (1.0 - f) * k_in;
        let density = sum_rho_t / t_total;
        let cp = sum_rhocp_t / t_total / density;
        Ok(EffectiveBoard {
            k_in_plane: k_in,
            k_through: k_th,
            density,
            cp,
            copper_thickness_m: t_cu,
        })
    }

    /// Effective board as a (temporary) orthotropic material.
    pub fn to_material(&self, lib: &Library, id: &str) -> Result<Material> {
        let e = self.effective(lib)?;
        let mut k = [e.k_in_plane; 3];
        k[self.normal_axis()?] = e.k_through;
        Ok(Material {
            id: id.to_string(),
            name: format!("PCB stack ({id})"),
            category: "pcb".into(),
            k: crate::library::Conductivity::Orthotropic(k),
            density: e.density,
            cp: e.cp,
            viscosity: None,
            beta: None,
            k_table: None,
            specs: vec![],
            source: "FerroTherm layered effective-conductivity model".into(),
            notes: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_laminate_matches_dielectric() {
        let lib = Library::builtin();
        let s = PcbStack {
            thickness_mm: 1.6,
            dielectric: "fr4".into(),
            copper: "cu-c11000".into(),
            copper_layers: vec![CopperLayer {
                oz: 1e-9,
                coverage: 0.0,
            }],
            layers: None,
            copper_oz: None,
            coverage: None,
            via_fraction: 0.0,
            normal: "z".into(),
        };
        let e = s.effective(&lib).unwrap();
        assert!((e.k_in_plane - 0.81).abs() < 1e-6);
        assert!((e.k_through - 0.29).abs() < 1e-6);
    }

    #[test]
    fn hand_calculation_four_layers() {
        // 4 x 1 oz, full coverage, 1.6 mm: t_cu = 140 µm.
        // k_in = (140e-6*388 + 1.46e-3*0.81)/1.6e-3 = 34.69 W/mK
        let lib = Library::builtin();
        let s = PcbStack {
            thickness_mm: 1.6,
            dielectric: "fr4".into(),
            copper: "cu-c11000".into(),
            copper_layers: vec![],
            layers: Some(4),
            copper_oz: Some(1.0),
            coverage: Some(1.0),
            via_fraction: 0.0,
            normal: "z".into(),
        };
        let e = s.effective(&lib).unwrap();
        let k_in = (140e-6 * 388.0 + 1.46e-3 * 0.81) / 1.6e-3;
        let k_th = 1.6e-3 / (140e-6 / 388.0 + 1.46e-3 / 0.29);
        assert!((e.k_in_plane - k_in).abs() < 1e-9);
        assert!((e.k_through - k_th).abs() < 1e-9);
    }
}
