// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Export of the case geometry to MCAD/ECAD exchange formats: STEP (boxes as
//! B-rep solids), STL and IDF 3.0 (board outline + component placements).

use crate::ecad::Side;
use crate::idf::IdfPart;
use crate::step::V3;
use ferrotherm_core::case::Case;
use ferrotherm_core::library::Library;
use ferrotherm_core::model::{all_blocks, component_box};

/// Every solid of the case as a named box (mm): blocks, heat-sink parts and
/// component bodies.
pub fn case_boxes(case: &Case, lib: &Library) -> Result<Vec<(String, V3, V3)>, String> {
    let mut out = Vec::new();
    for (b, _) in all_blocks(case).map_err(|e| e.to_string())? {
        out.push((b.name.clone(), b.min, b.max));
    }
    for c in &case.components {
        let (lo, hi) = component_box(c, lib).map_err(|e| e.to_string())?;
        out.push((c.name.clone(), lo, hi));
    }
    Ok(out)
}

pub fn case_to_step(case: &Case, lib: &Library) -> Result<String, String> {
    let name = if case.meta.title.is_empty() {
        "ferrotherm"
    } else {
        &case.meta.title
    };
    Ok(crate::step::write_boxes(name, &case_boxes(case, lib)?))
}

pub fn case_to_stl(case: &Case, lib: &Library) -> Result<String, String> {
    Ok(crate::stl::write_boxes(&case_boxes(case, lib)?))
}

/// IDF 3.0 board (.emn) and library (.emp). The board is the union bounding
/// box of the PCB blocks (normal z); parts are the components and the
/// powered or non-PCB blocks resting on its top or bottom face.
pub fn case_to_idf(case: &Case, lib: &Library) -> Result<(String, String), String> {
    let blocks = all_blocks(case).map_err(|e| e.to_string())?;
    let pcbs: Vec<_> = blocks
        .iter()
        .filter(|(b, _)| {
            b.pcb
                .as_ref()
                .is_some_and(|p| p.normal.trim_start_matches(['+', '-']) == "z")
        })
        .collect();
    if pcbs.is_empty() {
        return Err("the case has no PCB block (normal z) to export as an IDF board".into());
    }
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for (b, _) in &pcbs {
        for a in 0..3 {
            lo[a] = lo[a].min(b.min[a]);
            hi[a] = hi[a].max(b.max[a]);
        }
    }
    let thickness = hi[2] - lo[2];
    let outline = vec![
        [lo[0], lo[1]],
        [hi[0], lo[1]],
        [hi[0], hi[1]],
        [lo[0], hi[1]],
    ];
    let tol = 1e-6;
    let mut parts = Vec::new();
    let mut push = |name: &str, package: &str, a: V3, b: V3| {
        let cx = 0.5 * (a[0] + b[0]);
        let cy = 0.5 * (a[1] + b[1]);
        if cx < lo[0] || cx > hi[0] || cy < lo[1] || cy > hi[1] {
            return;
        }
        let side = if (a[2] - hi[2]).abs() < tol || a[2] >= hi[2] - tol {
            Side::Top
        } else if (b[2] - lo[2]).abs() < tol || b[2] <= lo[2] + tol {
            Side::Bottom
        } else {
            return;
        };
        parts.push(IdfPart {
            refdes: name.to_string(),
            package: package.to_string(),
            center: [cx, cy],
            size: [b[0] - a[0], b[1] - a[1]],
            height: b[2] - a[2],
            side,
        });
    };
    for c in &case.components {
        let (a, b) = component_box(c, lib).map_err(|e| e.to_string())?;
        push(&c.name, &c.package, a, b);
    }
    for (b, _) in &blocks {
        if b.pcb.is_none() {
            push(
                &b.name,
                b.material.as_deref().unwrap_or("block"),
                b.min,
                b.max,
            );
        }
    }
    let name = if case.meta.title.is_empty() {
        "ferrotherm"
    } else {
        &case.meta.title
    };
    Ok(crate::idf::write(name, &outline, thickness, &parts))
}
