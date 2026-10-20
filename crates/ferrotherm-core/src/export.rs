// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Write a complete result set to a directory: VTK field(s), reports and the
//! junction-temperature history.

use crate::error::{Error, Result};
use crate::model::Model;
use crate::report::Report;
use crate::simulate::RunResult;
use crate::vtk;
use std::path::{Path, PathBuf};

/// Writes `<stem>.vtr` (steady) or `<stem>_NNNNN.vtr` + `<stem>.pvd`
/// (transient), `report.md`, `report.json` and, when there are components,
/// `junctions.csv`. Returns the paths written.
pub fn write_results(
    dir: &Path,
    stem: &str,
    model: &Model,
    result: &RunResult,
    report: &Report,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir).map_err(|source| Error::Io {
        path: dir.display().to_string(),
        source,
    })?;
    let mut written = Vec::new();
    let mut write = |name: &str, text: &str| -> Result<()> {
        let p = dir.join(name);
        std::fs::write(&p, text).map_err(|source| Error::Io {
            path: p.display().to_string(),
            source,
        })?;
        written.push(p);
        Ok(())
    };
    if result.snapshots.len() == 1 {
        write(&format!("{stem}.vtr"), &vtk::vtr(model, result.last()))?;
    } else {
        let mut entries = Vec::new();
        for (i, s) in result.snapshots.iter().enumerate() {
            let f = format!("{stem}_{i:05}.vtr");
            write(&f, &vtk::vtr(model, s))?;
            entries.push((s.time_s, f));
        }
        write(&format!("{stem}.pvd"), &vtk::pvd(&entries))?;
    }
    write("report.md", &report.to_markdown())?;
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| Error::Invalid(format!("cannot serialise report: {e}")))?;
    write("report.json", &json)?;
    if !model.components.is_empty() {
        write(
            "junctions.csv",
            &vtk::junction_csv(model, &result.junction_history),
        )?;
    }
    Ok(written)
}
