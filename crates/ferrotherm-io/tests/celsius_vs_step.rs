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

/// The Celsius "Full Enclosure" tutorial (rack with chassis bays, blade
/// servers, a card bay, a power supply, fans and vents) against its STEP
/// export (`Finished_Tutorial_Full_Enclosure.equipment` and `.stp` in
/// FT_TESTDATA). Every solid part of the export must coincide with an
/// imported block, every fan housing with an imported fan and every vent
/// with an opening or a planar resistance.
#[test]
fn full_enclosure_tutorial_matches_its_step_export() {
    let Some(d) = data() else { return };
    let (eq, stp) = (
        d.join("Finished_Tutorial_Full_Enclosure.equipment"),
        d.join("Finished_Tutorial_Full_Enclosure.stp"),
    );
    if !eq.exists() || !stp.exists() {
        eprintln!("tutorial files not in FT_TESTDATA: skipped");
        return;
    }
    let m = celsius::read(&std::fs::read(eq).unwrap()).unwrap();
    let s = step::read_structure(&std::fs::read_to_string(stp).unwrap()).unwrap();
    let c = &m.case;
    let depth = c.domain.max[1];
    let cel = |lo: V3, hi: V3| -> (V3, V3) {
        ([lo[0], lo[2], depth - hi[1]], [hi[0], hi[2], depth - lo[1]])
    };
    let blocks: Vec<(V3, V3)> = c.blocks.iter().map(|b| cel(b.min, b.max)).collect();
    let tol = 2e-3;
    let near = |a: f64, b: f64| (a - b).abs() < tol;
    let leaf = |n: &str| -> String {
        let n = n.trim_end_matches(|ch: char| ch == '_' || ch.is_ascii_digit());
        // digit runs → '#'
        let mut out = String::new();
        for ch in n.chars() {
            if ch.is_ascii_digit() {
                if !out.ends_with('#') {
                    out.push('#');
                }
            } else {
                out.push(ch);
            }
        }
        out
    };
    let solid_kinds = [
        "U# (Chip)",
        "J# (Chip Socket)",
        "J# (Daughter Board Socket)",
        "PCB# (PCB)",
        "D# (Drive)",
        "HS# (Heatsink)",
        "Chassis Top Side",
        "Chassis Bottom Side",
        "Chassis Left Side",
        "Chassis Right Side",
        "Power Obstruction",
    ];
    let mut counts = std::collections::BTreeMap::new();
    for b in &s.bodies {
        let k = leaf(&b.name);
        let ext: Vec<f64> = (0..3).map(|i| b.max[i] - b.min[i]).collect();
        let thin = ext.iter().cloned().fold(f64::INFINITY, f64::min) < 0.01;
        if solid_kinds.contains(&k.as_str()) && !thin {
            let ok = blocks
                .iter()
                .any(|(a, z)| (0..3).all(|i| near(a[i], b.min[i]) && near(z[i], b.max[i])));
            assert!(ok, "{} {:?} … {:?} not imported", b.path, b.min, b.max);
            *counts.entry(k).or_insert(0) += 1;
        } else if k.starts_with("F# (") && !thin {
            // fan housing: centre in its plane, fan plane on one face
            let ax = (0..3).min_by(|&i, &j| ext[i].total_cmp(&ext[j])).unwrap();
            let ok = c.fans.iter().any(|f| {
                let p = [f.center[0], f.center[2], depth - f.center[1]];
                (0..3).all(|i| {
                    if i == ax {
                        near(p[i], b.min[i]) || near(p[i], b.max[i])
                    } else {
                        near(p[i], 0.5 * (b.min[i] + b.max[i]))
                    }
                })
            });
            assert!(ok, "fan {} {:?} … {:?} not imported", b.path, b.min, b.max);
            *counts.entry("fans".to_string()).or_insert(0) += 1;
        } else if (k.starts_with("Vent") || k.starts_with("Interconnect")) && thin {
            let ax = (0..3).find(|&i| ext[i] < 0.01).unwrap();
            let t: Vec<usize> = (0..3).filter(|&i| i != ax).collect();
            let rect_ok = |lo: V3, hi: V3| {
                t.iter()
                    .all(|&i| near(lo[i], b.min[i]) && near(hi[i], b.max[i]))
                    && (near(lo[ax], b.min[ax]) || near(lo[ax], b.max[ax]))
            };
            let opening = c.boundaries.iter().any(|o| {
                let Some(r) = &o.region else { return false };
                let ft = o.face.tangential();
                let fa = o.face.axis();
                let v = if o.face.is_max() {
                    c.domain.max[fa]
                } else {
                    c.domain.min[fa]
                };
                let mut lo = [0.0; 3];
                let mut hi = [0.0; 3];
                lo[fa] = v;
                hi[fa] = v;
                lo[ft[0]] = r.min[0];
                lo[ft[1]] = r.min[1];
                hi[ft[0]] = r.max[0];
                hi[ft[1]] = r.max[1];
                let (a, z) = cel(lo, hi);
                rect_ok(a, z)
            });
            let resistance = c.resistances.iter().any(|r| {
                let fa = ["x", "y", "z"].iter().position(|a| *a == r.axis).unwrap();
                let ft = [[1, 2], [0, 2], [0, 1]][fa];
                let mut lo = r.center;
                let mut hi = r.center;
                for q in 0..2 {
                    lo[ft[q]] -= 0.5 * r.size[q];
                    hi[ft[q]] += 0.5 * r.size[q];
                }
                let (a, z) = cel(lo, hi);
                rect_ok(a, z)
            });
            assert!(
                opening || resistance,
                "vent {} {:?} … {:?} not imported",
                b.path,
                b.min,
                b.max
            );
            *counts.entry("vents".to_string()).or_insert(0) += 1;
        }
    }
    eprintln!("tutorial vs STEP: {counts:?}");
    assert_eq!(counts.get("U# (Chip)"), Some(&16));
    assert_eq!(counts.get("fans"), Some(&16));
    assert!(counts.get("vents").copied().unwrap_or(0) >= 36);
    assert_eq!(counts.get("HS# (Heatsink)"), Some(&880));
}
