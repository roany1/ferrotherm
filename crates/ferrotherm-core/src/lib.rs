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
pub mod flow;
pub mod library;
pub mod linalg;
pub mod mesh;
pub mod mg;
pub mod model;
pub mod pcb;
pub mod radiation;
pub mod report;
pub mod simulate;
pub mod stencil;
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
    /// Internal radiation (view factors), when `[radiation]` is enabled.
    pub s2s: Option<radiation::S2s>,
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

    pub fn with_library(case: Case, mut library: Library) -> Result<Simulation> {
        library.add_materials(&case.materials)?;
        let model = Model::build(&case, &library)?;
        let s2s = case
            .radiation
            .as_ref()
            .and_then(|cfg| radiation::S2s::build(&model, &case, cfg));
        Ok(Simulation {
            case,
            library,
            model,
            s2s,
        })
    }

    /// True when the air (or liquid) flow is solved.
    pub fn has_flow(&self) -> bool {
        self.case.flow.as_ref().map(|f| f.enabled).unwrap_or(false)
    }

    pub fn run(&self, mut progress: impl FnMut(&str)) -> Result<Output> {
        self.run_observed(&mut |e| {
            match e {
                simulate::Event::Message(s) => progress(s),
                simulate::Event::FlowIteration {
                    iteration,
                    residuals,
                    ..
                } if iteration % 50 == 0 => progress(&format!(
                    "iteration {iteration}: mass {:.1e} momentum {:.1e} energy {:.1e}",
                    residuals[0], residuals[1], residuals[2]
                )),
                simulate::Event::Step {
                    step,
                    steps,
                    time_s,
                } if step % (steps / 20).max(1) == 0 => {
                    progress(&format!("t = {time_s:.3} s ({step}/{steps})"))
                }
                _ => {}
            }
            true
        })
    }

    /// Run with an observer that receives every [`simulate::Event`] and can
    /// cancel by returning `false`.
    pub fn run_observed(
        &self,
        observer: &mut dyn FnMut(simulate::Event) -> bool,
    ) -> Result<Output> {
        let result = if self.has_flow() {
            flow::run(&self.model, &self.case, self.s2s.as_ref(), observer)?
        } else {
            simulate::run_observed(&self.model, &self.case.solver, self.s2s.as_ref(), observer)?
        };
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
