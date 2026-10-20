// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Built-in material, surface-finish, package, environment and derating libraries.
//!
//! The libraries ship inside the binary (JSON files under `data/`) and can be
//! extended or overridden with user JSON files of the same shape: an entry
//! whose `id` already exists replaces the built-in one.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

const MATERIALS_JSON: &str = include_str!("../data/materials.json");
const FINISHES_JSON: &str = include_str!("../data/finishes.json");
const PACKAGES_JSON: &str = include_str!("../data/packages.json");
const STANDARDS_JSON: &str = include_str!("../data/standards.json");

/// Thermal conductivity: isotropic scalar or orthotropic `[kx, ky, kz]`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Conductivity {
    Isotropic(f64),
    Orthotropic([f64; 3]),
}

impl Conductivity {
    pub fn as_vec(&self) -> [f64; 3] {
        match *self {
            Conductivity::Isotropic(k) => [k, k, k],
            Conductivity::Orthotropic(k) => k,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Material {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub category: String,
    /// W/(m K)
    pub k: Conductivity,
    /// kg/m^3
    pub density: f64,
    /// J/(kg K)
    pub cp: f64,
    #[serde(default)]
    pub specs: Vec<String>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finish {
    pub id: String,
    pub name: String,
    pub emissivity: f64,
    #[serde(default)]
    pub specs: Vec<String>,
    #[serde(default)]
    pub source: String,
}

/// Which package face is the "case" in the 2R compact model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CaseFace {
    Top,
    Bottom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub family: String,
    /// Junction-to-case resistance, K/W. `None` = must be supplied by the component.
    pub r_jc: Option<f64>,
    /// Junction-to-board resistance, K/W.
    pub r_jb: Option<f64>,
    pub case_face: CaseFace,
    /// Nominal body size L x W x H in mm.
    pub size_mm: Option<[f64; 3]>,
    #[serde(default)]
    pub standard: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub name: String,
    pub ambient_min_c: f64,
    pub ambient_max_c: f64,
    pub induced_min_c: f64,
    pub induced_max_c: f64,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Derating {
    pub id: String,
    pub name: String,
    pub tj_max_c: f64,
    #[serde(default)]
    pub source: String,
}

#[derive(Deserialize, Default)]
struct MaterialsFile {
    #[serde(default)]
    materials: Vec<Material>,
}
#[derive(Deserialize, Default)]
struct FinishesFile {
    #[serde(default)]
    finishes: Vec<Finish>,
}
#[derive(Deserialize, Default)]
struct PackagesFile {
    #[serde(default)]
    packages: Vec<Package>,
}
#[derive(Deserialize, Default)]
struct StandardsFile {
    #[serde(default)]
    environments: Vec<Environment>,
    #[serde(default)]
    derating: Vec<Derating>,
}

/// A user library file may contain any subset of the sections.
#[derive(Deserialize, Default)]
struct UserLibraryFile {
    #[serde(default)]
    materials: Vec<Material>,
    #[serde(default)]
    finishes: Vec<Finish>,
    #[serde(default)]
    packages: Vec<Package>,
    #[serde(default)]
    environments: Vec<Environment>,
    #[serde(default)]
    derating: Vec<Derating>,
}

/// All lookup tables used by a simulation.
#[derive(Debug, Clone, Default)]
pub struct Library {
    pub materials: Vec<Material>,
    pub finishes: Vec<Finish>,
    pub packages: Vec<Package>,
    pub environments: Vec<Environment>,
    pub derating: Vec<Derating>,
}

fn parse<T: for<'de> Deserialize<'de>>(text: &str, what: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|e| Error::LibraryParse(format!("{what}: {e}")))
}

fn upsert<T: Clone>(list: &mut Vec<T>, items: Vec<T>, id: impl Fn(&T) -> &str) {
    for it in items {
        if let Some(pos) = list.iter().position(|x| id(x) == id(&it)) {
            list[pos] = it;
        } else {
            list.push(it);
        }
    }
}

impl Library {
    /// The libraries compiled into FerroTherm.
    pub fn builtin() -> Self {
        // The embedded JSON is tested in CI, so a parse failure here is a build bug.
        let m: MaterialsFile = parse(MATERIALS_JSON, "materials.json").expect("builtin materials");
        let f: FinishesFile = parse(FINISHES_JSON, "finishes.json").expect("builtin finishes");
        let p: PackagesFile = parse(PACKAGES_JSON, "packages.json").expect("builtin packages");
        let s: StandardsFile = parse(STANDARDS_JSON, "standards.json").expect("builtin standards");
        let lib = Library {
            materials: m.materials,
            finishes: f.finishes,
            packages: p.packages,
            environments: s.environments,
            derating: s.derating,
        };
        lib.validate().expect("builtin library is valid");
        lib
    }

    /// Merge a user JSON library (same schema; any subset of sections).
    pub fn merge_json(&mut self, text: &str, origin: &str) -> Result<()> {
        let u: UserLibraryFile = parse(text, origin)?;
        upsert(&mut self.materials, u.materials, |x| &x.id);
        upsert(&mut self.finishes, u.finishes, |x| &x.id);
        upsert(&mut self.packages, u.packages, |x| &x.id);
        upsert(&mut self.environments, u.environments, |x| &x.id);
        upsert(&mut self.derating, u.derating, |x| &x.id);
        self.validate()
    }

    pub fn merge_file(&mut self, path: &Path) -> Result<()> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
        self.merge_json(&text, &path.display().to_string())
    }

    /// Physical sanity checks on every entry.
    pub fn validate(&self) -> Result<()> {
        for m in &self.materials {
            let k = m.k.as_vec();
            if k.iter().any(|v| !(v.is_finite() && *v > 0.0)) || !(m.density > 0.0) || !(m.cp > 0.0)
            {
                return Err(Error::LibraryParse(format!(
                    "material '{}' needs k, density and cp > 0",
                    m.id
                )));
            }
        }
        for f in &self.finishes {
            if !(0.0..=1.0).contains(&f.emissivity) {
                return Err(Error::LibraryParse(format!(
                    "finish '{}': emissivity must be in [0, 1]",
                    f.id
                )));
            }
        }
        for p in &self.packages {
            for r in [p.r_jc, p.r_jb].into_iter().flatten() {
                if !(r > 0.0) {
                    return Err(Error::LibraryParse(format!(
                        "package '{}': thermal resistances must be > 0",
                        p.id
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn material(&self, id: &str) -> Result<&Material> {
        self.materials
            .iter()
            .find(|m| m.id == id)
            .ok_or(Error::UnknownId {
                kind: "material",
                id: id.into(),
            })
    }
    pub fn finish(&self, id: &str) -> Result<&Finish> {
        self.finishes
            .iter()
            .find(|m| m.id == id)
            .ok_or(Error::UnknownId {
                kind: "finish",
                id: id.into(),
            })
    }
    pub fn package(&self, id: &str) -> Result<&Package> {
        self.packages
            .iter()
            .find(|m| m.id == id)
            .ok_or(Error::UnknownId {
                kind: "package",
                id: id.into(),
            })
    }
    pub fn environment(&self, id: &str) -> Result<&Environment> {
        self.environments
            .iter()
            .find(|m| m.id == id)
            .ok_or(Error::UnknownId {
                kind: "environment",
                id: id.into(),
            })
    }
    pub fn derating(&self, id: &str) -> Result<&Derating> {
        self.derating
            .iter()
            .find(|m| m.id == id)
            .ok_or(Error::UnknownId {
                kind: "derating",
                id: id.into(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_loads_and_has_unique_ids() {
        let lib = Library::builtin();
        assert!(lib.materials.len() > 30);
        let mut ids: Vec<&str> = lib.materials.iter().map(|m| m.id.as_str()).collect();
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(n, ids.len(), "duplicate material id");
        assert!(lib.package("mil217-chip-carrier-small-die").unwrap().r_jc == Some(20.0));
        assert_eq!(
            lib.derating("sd18-hermetic-normal").unwrap().tj_max_c,
            110.0
        );
    }

    #[test]
    fn user_library_overrides() {
        let mut lib = Library::builtin();
        lib.merge_json(
            r#"{"materials":[{"id":"fr4","name":"my FR-4","k":0.3,"density":1900,"cp":1150}]}"#,
            "test",
        )
        .unwrap();
        assert_eq!(lib.material("fr4").unwrap().k.as_vec(), [0.3; 3]);
    }
}
