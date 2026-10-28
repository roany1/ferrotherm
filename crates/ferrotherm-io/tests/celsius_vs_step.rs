//! A Celsius EC Solver model checked against the STEP export of the same
//! model made by Celsius itself (set FT_TESTDATA to the folder holding
//! `test_paper2.equipment` and `test_paper2.stp`).
//!
//! The STEP file is in Celsius coordinates (X, Y up, Z depth); every board,
//! component and CAD obstruction imported by FerroTherm must coincide with
//! the solid of the same name.
use ferrotherm_io::{celsius, step};
use std::path::PathBuf;

type V3 = [f64; 3];

fn data() -> Option<PathBuf> {
    std::env::var_os("FT_TESTDATA").map(PathBuf::from)
}

#[test]
fn test_paper2_matches_its_step_export() {
    let Some(d) = data() else { return };
    let (eq, stp) = (d.join("test_paper2.equipment"), d.join("test_paper2.stp"));
    if !eq.exists() || !stp.exists() {
        eprintln!("test_paper2 files not in FT_TESTDATA: skipped");
        return;
    }
    let m = celsius::read(&std::fs::read(eq).unwrap()).unwrap();
    let text = std::fs::read_to_string(stp).unwrap();
    let s = step::read_structure(&text).unwrap();
    let depth = m.case.domain.max[1];
    // FerroTherm box → Celsius box
    let cel = |lo: V3, hi: V3| -> (V3, V3) {
        ([lo[0], lo[2], depth - hi[1]], [hi[0], hi[2], depth - lo[1]])
    };
    // imported objects (name → Celsius box)
    let mut ours: Vec<(String, V3, V3)> = Vec::new();
    for b in &m.case.blocks {
        let (a, z) = cel(b.min, b.max);
        ours.push((b.name.clone(), a, z));
    }
    for c in &m.case.components {
        // size is [L, W, H]: in-plane extents in increasing axis order, H along the mount axis
        let sz = c.size.unwrap();
        let n = ["x", "y", "z"]
            .iter()
            .position(|a| *a == c.mount_axis)
            .unwrap();
        let t: Vec<usize> = (0..3).filter(|&k| k != n).collect();
        let mut e = [0.0; 3];
        e[t[0]] = sz[0];
        e[t[1]] = sz[1];
        e[n] = sz[2];
        let p = c.position;
        let (a, z) = cel(p, [p[0] + e[0], p[1] + e[1], p[2] + e[2]]);
        ours.push((c.name.clone(), a, z));
    }
    for c in &m.cad {
        let (a, z) = cel(c.body.min, c.body.max);
        ours.push((c.body.name.clone(), a, z));
    }
    // STEP solids are named "U1 (FCBGA35, FPGA_9W)", "BOARD1_POWER (PCB)", …;
    // package internals (balls, die, …) have no counterpart and are skipped.
    // A designator repeated on two boards takes the `~2` suffix in FerroTherm,
    // so each solid must match one of the objects with its designator.
    let tol = 1e-3;
    let same = |a: &V3, z: &V3, lo: &V3, hi: &V3| {
        (0..3).all(|i| (a[i] - lo[i]).abs() < tol && (z[i] - hi[i]).abs() < tol)
    };
    let mut checked = 0;
    for b in &s.bodies {
        let base = b.name.split(" (").next().unwrap().trim().to_string();
        let cands: Vec<_> = ours
            .iter()
            .filter(|o| o.0 == base || o.0.starts_with(&format!("{base}~")))
            .collect();
        if cands.is_empty() {
            continue;
        }
        assert!(
            cands.iter().any(|o| same(&o.1, &o.2, &b.min, &b.max)),
            "{base}: STEP {:?}..{:?}, imported {:?}",
            b.min,
            b.max,
            cands
        );
        checked += 1;
    }
    eprintln!("{checked} objects match the STEP export");
    // 3 boards, 13 parts with a body in the STEP file, 1 CAD obstruction
    assert!(checked >= 17, "only {checked} objects compared");
}
