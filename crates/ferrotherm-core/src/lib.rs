// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! # FerroTherm core
//!
//! Open-source thermal solver for electronics enclosures.
//!
//! Phase 0 scope: steady and transient 3D heat conduction on a conforming
//! non-uniform Cartesian grid (finite volumes, harmonic-mean face
//! conductances, orthotropic materials), surface convection and grey-body
//! radiation boundary conditions, 2-resistor compact component models with
//! junction-temperature derating checks, and an effective-conductivity PCB
//! stack-up model.
//!
//! ```no_run
//! use ferrotherm_core::{Case, Simulation};
//! let case = Case::from_file("enclosure.toml".as_ref()).unwrap();
//! let sim = Simulation::new(case, ".".as_ref()).unwrap();
//! let out = sim.run(|msg| eprintln!("{msg}")).unwrap();
//! println!("{}", out.report.to_markdown());
//! ```

// NaN-safe `!(x > 0.0)` checks are intentional; index loops mirror the stencil maths.
#![allow(clippy::neg_cmp_op_on_partial_ord, clippy::needless_range_loop)]

pub mod case;
pub mod error;
pub mod export;
pub mod library;
pub mod linalg;
pub mod mesh;
pub mod model;
pub mod pcb;
pub mod report;
pub mod simulate;
pub mod vtk;

pub use case::Case;
pub use error::{Error, Result};
pub use library::Library;
pub use model::Model;
pub use report::Report;
pub use simulate::RunResult;

use std::path::Path;

/// A case bound to its library and discretised model.
pub struct Simulation {
    pub case: Case,
    pub library: Library,
    pub model: Model,
}

pub struct Output {
    pub result: RunResult,
    pub report: Report,
}

impl Simulation {
    /// `base_dir` is used to resolve relative library paths in the case.
    pub fn new(case: Case, base_dir: &Path) -> Result<Simulation> {
        let library = model::load_library(&case, base_dir)?;
        Self::with_library(case, library)
    }

    pub fn with_library(case: Case, library: Library) -> Result<Simulation> {
        let model = Model::build(&case, &library)?;
        Ok(Simulation {
            case,
            library,
            model,
        })
    }

    pub fn run(&self, progress: impl FnMut(&str)) -> Result<Output> {
        let result = simulate::run(&self.model, &self.case.solver, progress)?;
        Ok(self.finish(result))
    }

    /// Run with an observer that receives every [`simulate::Event`] and can
    /// cancel by returning `false`.
    pub fn run_observed(
        &self,
        observer: &mut dyn FnMut(simulate::Event) -> bool,
    ) -> Result<Output> {
        let result = simulate::run_observed(&self.model, &self.case.solver, observer)?;
        Ok(self.finish(result))
    }

    fn finish(&self, result: RunResult) -> Output {
        let report = report::build(
            &self.model,
            &result,
            &self.case.meta.title,
            &self.case.meta.author,
        );
        Output { result, report }
    }
}
