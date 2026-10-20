// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

use thiserror::Error;

/// Every error FerroTherm can return.
#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error on '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot parse case file: {0}")]
    CaseParse(String),
    #[error("cannot parse library file: {0}")]
    LibraryParse(String),
    #[error("unknown {kind} '{id}' (run `ferrotherm {kind}s` to list the available ids)")]
    UnknownId { kind: &'static str, id: String },
    #[error("invalid case: {0}")]
    Invalid(String),
    #[error("cancelled by the user")]
    Cancelled,
    #[error("linear solver did not converge: residual {residual:.3e} after {iterations} iterations (tolerance {tolerance:.1e})")]
    NoConvergence {
        residual: f64,
        iterations: usize,
        tolerance: f64,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
