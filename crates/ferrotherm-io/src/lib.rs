// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Import and export for FerroTherm.

// Geometry code indexes axes and matrix entries explicitly.
#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

pub mod archive;
pub mod ecad;
pub mod export;
pub mod geom;
pub mod gerber;
pub mod idf;
pub mod import;
pub mod ipc2581;
pub mod kicad;
#[cfg(feature = "occt")]
pub mod occt;
pub mod odbpp;
pub mod step;
pub mod stl;
pub mod xml;
