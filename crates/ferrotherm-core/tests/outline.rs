// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Polygon outlines of blocks (non-rectangular boards, cut-outs).

use ferrotherm_core::{Case, Library, Simulation};

fn sim(toml: &str) -> Simulation {
    let case = Case::from_toml(toml).unwrap();
    Simulation::with_library(case, Library::builtin()).unwrap()
}

/// L-shaped 1.6 mm board (100 × 80 mm with a 50 × 40 mm notch) and a
/// 10 × 10 mm hole: the board must own exactly the cells inside the outline
/// (area 6000 − 100 = 5900 mm² on a 1 mm grid that matches the vertices),
/// carry all its power and leave the notch and the hole to the air.
#[test]
fn l_shaped_board_with_hole() {
    let s = sim(r#"
        [domain]
        min = [0, 0, 0]
        max = [100, 80, 10]
        material = "air-still"
        [grid]
        max_cell_mm = [1.0, 1.0, 2.0]
        [[blocks]]
        name = "board"
        min = [0, 0, 4]
        max = [100, 80, 5.6]
        power_w = 5.9
        outline = [
            [[0, 0], [100, 0], [100, 40], [50, 40], [50, 80], [0, 80]],
            [[20, 20], [30, 20], [30, 30], [20, 30]],
        ]
        [blocks.pcb]
        thickness_mm = 1.6
        layers = 4
        copper_oz = 1.0
        coverage = 0.5
        [[boundaries]]
        face = "zmin"
        type = "fixed"
        t_c = 25
    "#);
    let m = &s.model;
    let g = &m.grid;
    let n = g.n();
    let board = m.blocks.iter().position(|b| b.name == "board").unwrap();
    let bmat = m
        .materials
        .iter()
        .position(|x| x.id == m.blocks[board].material)
        .unwrap() as u32;
    // board cells of one layer of the board thickness
    let k = (0..n[2])
        .find(|&k| g.center(2, k) > 4.0e-3 && g.center(2, k) < 5.6e-3)
        .unwrap();
    let mut area = 0.0;
    for j in 0..n[1] {
        for i in 0..n[0] {
            if m.cell_material[g.idx(i, j, k)] == bmat {
                area += g.width(0, i) * g.width(1, j) * 1e6;
            }
        }
    }
    eprintln!("OUTLINE: board area {area:.1} mm² (exact 5900)");
    assert!((area - 5900.0).abs() < 1e-6);
    let at = |x: f64, y: f64| {
        let i = (0..n[0]).find(|&i| g.nodes[0][i + 1] > x * 1e-3).unwrap();
        let j = (0..n[1]).find(|&j| g.nodes[1][j + 1] > y * 1e-3).unwrap();
        m.cell_material[g.idx(i, j, k)]
    };
    assert_ne!(at(75.0, 60.0), bmat, "notch must be air");
    assert_ne!(at(25.0, 25.0), bmat, "hole must be air");
    assert_eq!(at(75.0, 20.0), bmat);
    assert_eq!(at(25.0, 60.0), bmat);
    // all the power in the board cells
    let p: f64 = (0..m.ncells)
        .filter(|&c| m.cell_material[c] == bmat)
        .map(|c| m.source[c])
        .sum();
    assert!((p - 5.9).abs() < 1e-9, "power {p}");
    let out = s.run(|_| {}).unwrap();
    assert!(out.report.energy_imbalance_pct.abs() < 1e-3);
}

/// A point-in-polygon sanity check of the even–odd rule.
#[test]
fn even_odd_rings() {
    use ferrotherm_core::case::point_in_rings;
    let rings = vec![
        vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
        vec![[3.0, 3.0], [7.0, 3.0], [7.0, 7.0], [3.0, 7.0]],
    ];
    assert!(point_in_rings(&rings, 1.0, 1.0));
    assert!(!point_in_rings(&rings, 5.0, 5.0));
    assert!(!point_in_rings(&rings, 11.0, 5.0));
}

/// `min_cells` puts at least that many cells across the block, whatever
/// `max_cell_mm` says (the die of a package on a coarse board grid).
#[test]
fn min_cells_refines_small_blocks() {
    let toml = |mc: &str| {
        format!(
            r#"
        [domain]
        min = [0, 0, 0]
        max = [60, 60, 10]
        material = "air-still"
        [grid]
        max_cell_mm = 5.0
        [[blocks]]
        name = "die"
        material = "si"
        min = [28, 28, 4]
        max = [31, 31, 5]
        power_w = 1.0
        {mc}
        [[boundaries]]
        face = "zmin"
        type = "fixed"
        t_c = 25
    "#
        )
    };
    let count = |s: &Simulation, a: usize| {
        let g = &s.model.grid;
        (0..g.n()[a])
            .filter(|&i| g.center(a, i) > 28e-3 && g.center(a, i) < 31e-3)
            .count()
    };
    let coarse = sim(&toml(""));
    assert_eq!(count(&coarse, 0), 1);
    let fine = sim(&toml("min_cells = [5, 4, 0]"));
    assert_eq!(count(&fine, 0), 5);
    assert_eq!(count(&fine, 1), 4);
    let back = Case::from_toml(&fine.case.to_toml().unwrap()).unwrap();
    assert_eq!(back.blocks[0].min_cells, Some([5, 4, 0]));
}
