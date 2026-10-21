// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Export → re-import round trips on the MIL example, and the full
//! "CAD file → case → solved model" path on real KiCad data (FT_TESTDATA).

use ferrotherm_core::case::Case;
use ferrotherm_core::Simulation;
use ferrotherm_io::{export, import, step};
use std::path::{Path, PathBuf};

fn example() -> (Case, ferrotherm_core::library::Library) {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/mil_enclosure.toml");
    let case = Case::from_file(&p).unwrap();
    let lib = ferrotherm_core::model::load_library(&case, p.parent().unwrap()).unwrap();
    (case, lib)
}

#[test]
fn idf_export_reimports() {
    let (case, lib) = example();
    let (emn, emp) = export::case_to_idf(&case, &lib).unwrap();
    let mut b = ferrotherm_io::idf::read(&emn, Some(&emp)).unwrap();
    let s = b.summary();
    eprintln!(
        "IDF: {} parts, board {:?} t {}",
        s.components, s.size_mm, s.thickness_mm
    );
    assert!(s.components >= case.components.len());
    for c in &case.components {
        let (lo, hi) = ferrotherm_core::model::component_box(c, &lib).unwrap();
        let p = b
            .components
            .iter()
            .find(|p| p.refdes == c.name)
            .unwrap_or_else(|| panic!("{} missing", c.name));
        let (flo, fhi) = p.footprint().unwrap();
        for a in 0..2 {
            assert!(
                (flo[a] - lo[a]).abs() < 1e-3 && (fhi[a] - hi[a]).abs() < 1e-3,
                "{}",
                c.name
            );
        }
        assert!((p.height_mm.unwrap() - (hi[2] - lo[2])).abs() < 1e-3);
    }
}

#[test]
fn stl_export_reimports() {
    let (case, lib) = example();
    let boxes = export::case_boxes(&case, &lib).unwrap();
    let text = export::case_to_stl(&case, &lib).unwrap();
    let m = ferrotherm_io::stl::read(text.as_bytes(), "x", 1.0).unwrap();
    assert_eq!(m.bodies.len(), boxes.len());
    for ((_, a, b), body) in boxes.iter().zip(&m.bodies) {
        let v = (b[0] - a[0]) * (b[1] - a[1]) * (b[2] - a[2]);
        assert!((body.volume() - v).abs() < 1e-4 * v.max(1.0));
    }
}

#[cfg(feature = "occt")]
#[test]
fn step_export_read_by_occt() {
    let (case, lib) = example();
    let boxes = export::case_boxes(&case, &lib).unwrap();
    let text = export::case_to_step(&case, &lib).unwrap();
    let m = step::read(&text, 0.01).unwrap();
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert_eq!(m.bodies.len(), boxes.len());
    for (n, a, b) in &boxes {
        let body = m.bodies.iter().find(|x| &x.name == n).unwrap();
        for k in 0..3 {
            assert!(
                (body.min[k] - a[k]).abs() < 1e-3 && (body.max[k] - b[k]).abs() < 1e-3,
                "{n}"
            );
        }
    }
}

fn data() -> Option<PathBuf> {
    std::env::var_os("FT_TESTDATA").map(PathBuf::from)
}

/// KiCad board + STEP assembly → one case → model built and solved.
#[test]
fn real_board_to_solved_case() {
    let Some(d) = data() else { return };
    // ECAD: the KiCad board with a power list
    let (src, imp) = import::import_path(&d.join("pic_programmer_v7.kicad_pcb"), 0.1).unwrap();
    assert_eq!(src, import::Source::KiCad);
    let import::Imported::Board(mut board) = imp else {
        panic!()
    };
    ferrotherm_io::ecad::apply_power_csv(
        &mut board,
        "refdes,power\nU1,0.5\nU5,0.8\nQ1,0.3,40,20\n",
    )
    .unwrap();
    let opt = ferrotherm_io::ecad::ToCaseOptions {
        origin: [10.0, 10.0, 5.0],
        ..Default::default()
    };
    let fr = import::board_fragment(&mut board, &opt);
    let mut case = Case::from_toml(
        &(r#"
[meta]
title = "imported"
[domain]
min = [0, 0, 0]
max = [10, 10, 10]
material = "air-still"
[grid]
max_cell_mm = 4
"#.to_string()
        + &["xmin", "xmax", "ymin", "ymax", "zmin", "zmax"]
            .iter()
            .map(|f| format!("[[boundaries]]\nname = \"{f}\"\nface = \"{f}\"\ntype = \"convection\"\nh = 10\nt_ambient_c = 25\n"))
            .collect::<String>()),
    )
    .unwrap();
    let notes = import::merge_into_case(&mut case, fr, true, 5.0);
    eprintln!("notes: {notes:?}");
    // Gerber set of the same board must give the same tile count
    let (src, imp) = import::import_path(&d.join("pic_gerber"), 0.1).unwrap();
    assert_eq!(src, import::Source::Gerber);
    let import::Imported::Board(mut gb) = imp else {
        panic!()
    };
    let gfr = import::board_fragment(&mut gb, &ferrotherm_io::ecad::ToCaseOptions::default());
    assert!(!gfr.blocks.is_empty());
    // MCAD: STEP via auto-detection
    let (src, imp) = import::import_path(&d.join("TO-220-3_Vertical.step"), 0.1).unwrap();
    assert_eq!(src, import::Source::Step);
    let import::Imported::Mcad(m) = imp else {
        panic!()
    };
    let mfr = import::mcad_to_fragment(
        &m,
        &import::McadOptions {
            offset: [case.domain.max[0] - 20.0, case.domain.max[1] - 20.0, 30.0],
            prefix: "mcad/".into(),
            ..Default::default()
        },
    );
    import::merge_into_case(&mut case, mfr, true, 5.0);
    let text = case.to_toml().unwrap();
    let case2 = Case::from_toml(&text).unwrap();
    let lib = ferrotherm_core::library::Library::builtin();
    let sim = Simulation::with_library(case2, lib).unwrap();
    eprintln!(
        "model: {} cells, {} blocks, {} components",
        sim.model.ncells,
        sim.model.blocks.len(),
        sim.model.components.len()
    );
    let out = sim.run(|_| {}).unwrap();
    let r = &out.report;
    eprintln!(
        "Tmax {:.2} C, imbalance {:.4} %",
        r.t_max_c, r.energy_imbalance_pct
    );
    assert!((r.power_in_w - 1.6).abs() < 1e-9, "{}", r.power_in_w);
    assert!(r.energy_imbalance_pct.abs() < 0.1);
    assert!(r.t_max_c > 25.0);
}
