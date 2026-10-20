// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Verification against closed-form solutions. Each test states the exact
//! solution it is compared with.

use ferrotherm_core::simulate::SIGMA;
use ferrotherm_core::{Case, Library, Simulation};

const TEST_LIB: &str = r#"{ "materials": [
  {"id":"k1","name":"k=1","k":1.0,"density":1000,"cp":1000},
  {"id":"k10","name":"k=10","k":10.0,"density":1000,"cp":1000},
  {"id":"k1e4","name":"k=1e4","k":1.0e4,"density":1000,"cp":1000}
]}"#;

fn sim(toml: &str) -> Simulation {
    let mut lib = Library::builtin();
    lib.merge_json(TEST_LIB, "test").unwrap();
    let case = Case::from_toml(toml).unwrap();
    Simulation::with_library(case, lib).unwrap()
}

/// Cell-centre temperatures along x for the single y/z column (j = k = 0).
fn line_x(s: &Simulation, t: &[f64]) -> Vec<(f64, f64)> {
    let g = &s.model.grid;
    (0..g.n()[0])
        .map(|i| (g.center(0, i), t[g.idx(i, 0, 0)]))
        .collect()
}

/// Two-layer wall, fixed temperature on one side and convection on the other.
/// Exact: piecewise linear profile, q'' = ΔT / (L1/k1 + L2/k2 + 1/h).
/// The finite-volume scheme with harmonic face conductances is exact here.
#[test]
fn composite_wall_with_convection_is_exact() {
    let s = sim(r#"
        [domain]
        min = [0, 0, 0]
        max = [30, 10, 10]
        material = "k1"
        [grid]
        max_cell_mm = [1.0, 10.0, 10.0]
        [[blocks]]
        name = "layer2"
        material = "k10"
        min = [10, 0, 0]
        max = [30, 10, 10]
        [[boundaries]]
        face = "xmin"
        type = "fixed"
        t_c = 100.0
        [[boundaries]]
        face = "xmax"
        type = "convection"
        h = 50.0
        t_ambient_c = 20.0
    "#);
    let out = s.run(|_| {}).unwrap();
    let t = &out.result.last().t;
    let (l1, l2, k1, k2, h) = (0.010, 0.020, 1.0, 10.0, 50.0);
    let q = 80.0 / (l1 / k1 + l2 / k2 + 1.0 / h);
    for (x, ti) in line_x(&s, t) {
        let exact = if x < l1 {
            100.0 - q * x / k1
        } else {
            100.0 - q * l1 / k1 - q * (x - l1) / k2
        };
        assert!((ti - exact).abs() < 1e-6, "x={x}: {ti} vs {exact}");
    }
    let area = 1e-4;
    assert!((out.report.heat_out_w - 0.0).abs() < 1e-9); // no internal source: in = out through the two faces
    let q_in = out.report.boundaries[0].heat_out_w; // negative = enters
    assert!((-q_in - q * area).abs() < 1e-9);
}

/// Slab with uniform heat generation and both faces at T0.
/// Exact: T = T0 + q'''/(2k) x (L − x). Second-order convergence expected.
#[test]
fn uniform_generation_converges_second_order() {
    let err = |cells: usize| {
        let dx = 20.0 / cells as f64;
        let s = sim(&format!(
            r#"
            [domain]
            min = [0, 0, 0]
            max = [20, 5, 5]
            material = "k1"
            [grid]
            max_cell_mm = [{dx}, 5.0, 5.0]
            [[blocks]]
            name = "src"
            material = "k1"
            min = [0, 0, 0]
            max = [20, 5, 5]
            power_w = 0.5
            [[boundaries]]
            face = "xmin"
            type = "fixed"
            t_c = 0.0
            [[boundaries]]
            face = "xmax"
            type = "fixed"
            t_c = 0.0
        "#
        ));
        let out = s.run(|_| {}).unwrap();
        let qv = 0.5 / (0.020 * 0.005 * 0.005);
        let l = 0.020;
        let e = line_x(&s, &out.result.last().t)
            .into_iter()
            .map(|(x, t)| (t - qv / 2.0 * x * (l - x)).abs())
            .fold(0.0, f64::max);
        assert!(out.report.energy_imbalance_pct.abs() < 1e-6);
        e
    };
    let e10 = err(10);
    let e20 = err(20);
    let e40 = err(40);
    let tmax = 0.5 / (0.020 * 0.005 * 0.005) / 8.0 * 0.020 * 0.020; // 2500 K peak with k = 1
    assert!(e40 / tmax < 2e-3, "relative error {}", e40 / tmax);
    let order = (e10 / e20).log2();
    let order2 = (e20 / e40).log2();
    eprintln!("GEN: e10={e10:.4e} e20={e20:.4e} e40={e40:.4e} order {order:.3} {order2:.3}");
    assert!(
        order > 1.9 && order2 > 1.9,
        "observed order {order}, {order2}"
    );
}

/// Plane wall, initially at Ti, face x=0 suddenly held at Ts, x=L adiabatic.
/// Exact (Fourier series): θ/θi = Σ 4/((2n+1)π) sin(λn x) exp(−λn² α t), λn=(2n+1)π/(2L).
#[test]
fn transient_plane_wall_matches_fourier_series() {
    let s = sim(r#"
        [domain]
        min = [0, 0, 0]
        max = [10, 1, 1]
        material = "k1"
        [grid]
        max_cell_mm = [0.125, 1.0, 1.0]
        [[boundaries]]
        face = "xmin"
        type = "fixed"
        t_c = 0.0
        [solver]
        mode = "transient"
        initial_c = 100.0
        [solver.transient]
        dt_s = 0.25
        end_s = 20.0
    "#);
    let out = s.run(|_| {}).unwrap();
    let alpha = 1.0 / (1000.0 * 1000.0);
    let (l, t) = (0.010, 20.0); // Fo = α t / L² = 0.2
    let exact = |x: f64| {
        let mut v = 0.0;
        for n in 0..200 {
            let m = (2 * n + 1) as f64;
            let lam = m * std::f64::consts::PI / (2.0 * l);
            v +=
                4.0 / (m * std::f64::consts::PI) * (lam * x).sin() * (-lam * lam * alpha * t).exp();
        }
        100.0 * v
    };
    let mut emax: f64 = 0.0;
    for (x, ti) in line_x(&s, &out.result.last().t) {
        emax = emax.max((ti - exact(x)).abs());
    }
    eprintln!("TRANSIENT: emax = {emax:.4} K");
    assert!(emax < 0.5, "max error {emax} K out of 100 K");
    // Energy: in a transient, losses + storage = 0 (no source).
    assert!((out.result.heat.total_out + out.result.heat.storage_rate).abs() < 1e-6 * 100.0);
}

/// Near-isothermal cube, radiating as a black body from all faces.
/// Exact: P = ε σ A (T⁴ − Tr⁴).
#[test]
fn radiation_only_cube() {
    let s = sim(r#"
        [domain]
        min = [0, 0, 0]
        max = [50, 50, 50]
        material = "k1e4"
        [grid]
        max_cell_mm = 10.0
        [[blocks]]
        name = "cube"
        material = "k1e4"
        min = [0, 0, 0]
        max = [50, 50, 50]
        power_w = 10.0
        [[boundaries]]
        face = "xmin"
        type = "radiation"
        emissivity = 1.0
        t_radiation_c = 20.0
        [[boundaries]]
        face = "xmax"
        type = "radiation"
        emissivity = 1.0
        t_radiation_c = 20.0
        [[boundaries]]
        face = "ymin"
        type = "radiation"
        emissivity = 1.0
        t_radiation_c = 20.0
        [[boundaries]]
        face = "ymax"
        type = "radiation"
        emissivity = 1.0
        t_radiation_c = 20.0
        [[boundaries]]
        face = "zmin"
        type = "radiation"
        emissivity = 1.0
        t_radiation_c = 20.0
        [[boundaries]]
        face = "zmax"
        type = "radiation"
        emissivity = 1.0
        t_radiation_c = 20.0
        [solver]
        nonlinear_tolerance_k = 1e-6
    "#);
    let out = s.run(|_| {}).unwrap();
    let area = 6.0 * 0.05 * 0.05;
    let tr = 293.15_f64;
    let t_exact = (10.0 / (SIGMA * area) + tr.powi(4)).powf(0.25) - 273.15;
    let t_avg = out.report.blocks[0].t_avg_c;
    eprintln!("RAD: {t_avg:.6} vs {t_exact:.6}");
    assert!((t_avg - t_exact).abs() < 1e-3, "{t_avg} vs {t_exact}");
    assert!(out.report.energy_imbalance_pct.abs() < 1e-4);
}

/// 2R compact component: Tj − Tcase = P·Rjc exactly, and all power accounted for.
#[test]
fn compact_component_resistances() {
    let s = sim(r#"
        [domain]
        min = [0, 0, 0]
        max = [40, 40, 10]
        material = "air-still"
        [grid]
        max_cell_mm = 1.0
        [[blocks]]
        name = "plate"
        material = "al-6061-t6"
        min = [0, 0, 0]
        max = [40, 40, 3]
        [[components]]
        name = "U1"
        package = "lcc-28"
        position = [14.285, 14.285, 3]
        r_jc = 20.0
        r_jb = 50.0
        case_face = "bottom"
        power_w = 2.0
        derating = "sd18-hermetic-normal"
        [[boundaries]]
        face = "zmin"
        type = "fixed"
        t_c = 40.0
    "#);
    let out = s.run(|_| {}).unwrap();
    let c = &out.report.components[0];
    // case = bottom: connected via r_jc to the plate; top (r_jb) faces still air.
    assert!((c.q_case_w + c.q_board_w - 2.0).abs() < 1e-8);
    let tcase = c.t_case_c.unwrap();
    assert!((c.tj_c - tcase - c.q_case_w * 20.0).abs() < 1e-9);
    // nearly all heat goes through the case into aluminium
    assert!(c.q_case_w > 1.95, "q_case {}", c.q_case_w);
    // plate is 3 mm aluminium on a 40 °C cold wall: Tcase just above 40
    assert!(tcase > 40.0 && tcase < 41.0, "Tcase {tcase}");
    eprintln!(
        "COMPACT: Tj {:.4} Tcase {:.4} qc {:.5} qb {:.5}",
        c.tj_c, tcase, c.q_case_w, c.q_board_w
    );
    assert_eq!(c.tj_limit_c, Some(110.0));
    assert_eq!(c.pass, Some(true));
    assert!(out.report.energy_imbalance_pct.abs() < 1e-6);
}

/// A boundary patch limited to a region of a face carries all the heat.
#[test]
fn patch_boundary_region() {
    let s = sim(r#"
        [domain]
        min = [0, 0, 0]
        max = [100, 60, 5]
        material = "al-6061-t6"
        [grid]
        max_cell_mm = 2.5
        [[blocks]]
        name = "heater"
        material = "al-6061-t6"
        min = [80, 20, 0]
        max = [100, 40, 5]
        power_w = 15.0
        [[boundaries]]
        name = "coldplate"
        face = "zmin"
        region = { min = [0, 0], max = [20, 60] }
        type = "convection"
        h = 2000.0
        t_ambient_c = 30.0
    "#);
    let out = s.run(|_| {}).unwrap();
    assert_eq!(out.report.boundaries.len(), 1);
    assert!((out.report.boundaries[0].heat_out_w - 15.0).abs() < 1e-8);
    // 1D estimate: conduction along the plate (L ≈ 70 mm, A = 60x5 mm²) + film under the cold plate.
    let r = 0.07 / (167.0 * 0.06 * 0.005) + 1.0 / (2000.0 * 0.02 * 0.06);
    let dt = out.report.blocks[0].t_avg_c - 30.0;
    eprintln!("PATCH: dT {dt:.3} vs 1D {:.3}", 15.0 * r);
    assert!(
        dt > 15.0 * r * 0.95 && dt < 15.0 * r * 1.2,
        "ΔT {dt} vs 1D estimate {}",
        15.0 * r
    );
}
