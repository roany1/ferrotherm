// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Verification of the flow solver and the Phase 2/3 models against exact
//! solutions and published benchmarks. Each test states its reference.

use ferrotherm_core::simulate::SIGMA;
use ferrotherm_core::{Case, Library, Simulation};

fn sim(toml: &str) -> Simulation {
    let case = Case::from_toml(toml).unwrap();
    Simulation::with_library(case, Library::builtin()).unwrap()
}

const AIR_RHO: f64 = 1.1614;
const AIR_MU: f64 = 1.846e-5;
const AIR_K: f64 = 0.0263;
const AIR_CP: f64 = 1007.0;

/// Plane Poiseuille flow between parallel plates (h = 10 mm), laminar.
/// Exact: dp/dx = −12 μ U / h², u_max = 1.5 U.
#[test]
fn poiseuille_channel() {
    let s = sim(r#"
        [domain]
        min = [0, 0, 0]
        max = [200, 1, 10]
        material = "air"
        [grid]
        max_cell_mm = [2.0, 1.0, 0.5]
        [flow]
        turbulence = "laminar"
        gravity = [0, 0, 0]
        max_iterations = 600
        tolerance = 1e-5
        relax_velocity = 0.7
        [[boundaries]]
        face = "xmin"
        type = "inlet"
        velocity_m_s = 0.05
        t_ambient_c = 20
        [[boundaries]]
        face = "xmax"
        type = "opening"
        t_ambient_c = 20
        [[boundaries]]
        face = "ymin"
        type = "symmetry"
        [[boundaries]]
        face = "ymax"
        type = "symmetry"
    "#);
    let out = s.run(|_| {}).unwrap();
    let f = out.result.flow.as_ref().unwrap();
    assert!(f.converged, "not converged: {:?}", f.residuals.last());
    let snap = out.result.last();
    let p = snap.p.as_ref().unwrap();
    let vel = snap.vel.as_ref().unwrap();
    let g = &s.model.grid;
    let n = g.n();
    // pressure gradient between x = 100 and 180 mm, averaged over z
    let col = |x: f64| -> usize { (0..n[0]).find(|&i| g.nodes[0][i + 1] > x * 1e-3).unwrap() };
    let (i1, i2) = (col(100.0), col(180.0));
    let pavg = |i: usize| (0..n[2]).map(|k| p[g.idx(i, 0, k)] as f64).sum::<f64>() / n[2] as f64;
    let dpdx = (pavg(i2) - pavg(i1)) / (g.center(0, i2) - g.center(0, i1));
    let exact = -12.0 * AIR_MU * 0.05 / (0.01f64).powi(2);
    eprintln!("POISEUILLE dp/dx {dpdx:.5} exact {exact:.5}");
    assert!((dpdx / exact - 1.0).abs() < 0.02, "dp/dx {dpdx} vs {exact}");
    // centreline velocity (two middle cells straddle the centre)
    let umid = 0.5 * (vel[g.idx(i2, 0, n[2] / 2 - 1)][0] + vel[g.idx(i2, 0, n[2] / 2)][0]) as f64;
    // exact average of u over the two centre cells (each 0.5 mm wide)
    let y = |z: f64| 6.0 * 0.05 * (z / 0.01) * (1.0 - z / 0.01);
    let cell_avg = |z0: f64, z1: f64| {
        let m = 200;
        (0..m).map(|k| y(z0 + (z1 - z0) * (k as f64 + 0.5) / m as f64)).sum::<f64>() / m as f64
    };
    let expect = cell_avg(0.0045, 0.0055);
    eprintln!("POISEUILLE u_centre {umid:.5} expect {expect:.5}");
    assert!((umid / expect - 1.0).abs() < 0.02);
}

/// Natural convection in a differentially heated square cavity, Pr ≈ 0.71.
/// Reference: de Vahl Davis (1983) benchmark, mean Nusselt number on the hot
/// wall: Ra = 1e4 → 2.243, Ra = 1e5 → 4.519.
fn cavity(ra: f64, cells: usize) -> f64 {
    let l = 0.05;
    let nu = AIR_MU / AIR_RHO;
    let alpha = AIR_K / (AIR_RHO * AIR_CP);
    let beta = 0.003333;
    let dt = ra * nu * alpha / (9.81 * beta * l * l * l);
    let h = 50.0 / cells as f64;
    let s = sim(&format!(
        r#"
        [domain]
        min = [0, 0, 0]
        max = [50, 1, 50]
        material = "air"
        [grid]
        max_cell_mm = [{h}, 1.0, {h}]
        [flow]
        turbulence = "laminar"
        reference_temperature_c = 20
        max_iterations = 4000
        tolerance = 1e-5
        relax_velocity = 0.7
        relax_temperature = 0.9
        [[boundaries]]
        name = "hot"
        face = "xmin"
        type = "fixed"
        t_c = {th}
        [[boundaries]]
        name = "cold"
        face = "xmax"
        type = "fixed"
        t_c = {tc}
        [[boundaries]]
        face = "ymin"
        type = "symmetry"
        [[boundaries]]
        face = "ymax"
        type = "symmetry"
        [solver]
        initial_c = 20
    "#,
        th = 20.0 + 0.5 * dt,
        tc = 20.0 - 0.5 * dt
    ));
    let out = s.run(|_| {}).unwrap();
    let f = out.result.flow.as_ref().unwrap();
    assert!(f.converged, "Ra {ra}: not converged {:?}", f.residuals.last());
    let q_hot = -out.report.boundaries[0].heat_out_w; // heat entering at the hot wall
    let area = l * 1e-3;
    let nu_num = q_hot / (AIR_K * dt / l * area);
    eprintln!(
        "CAVITY Ra {ra:.0e} cells {cells}: Nu {nu_num:.4} ({} it, imbalance {:.2e} %)",
        f.iterations, out.report.energy_imbalance_pct
    );
    nu_num
}

#[test]
fn de_vahl_davis_ra1e4() {
    let nu = cavity(1e4, 40);
    assert!((nu / 2.243 - 1.0).abs() < 0.02, "Nu {nu}");
}

#[test]
fn de_vahl_davis_ra1e5() {
    let nu = cavity(1e5, 64);
    assert!((nu / 4.519 - 1.0).abs() < 0.02, "Nu {nu}");
}

/// Forced convection over a heated block: all the power must leave with the
/// air (adiabatic walls) — global energy conservation of the coupled solver.
#[test]
fn forced_convection_energy_balance() {
    let s = sim(r#"
        [domain]
        min = [0, 0, 0]
        max = [150, 40, 20]
        material = "air"
        [grid]
        max_cell_mm = 2.0
        [flow]
        turbulence = "lvel"
        max_iterations = 800
        tolerance = 1e-4
        [[blocks]]
        name = "heater"
        material = "al-6061-t6"
        min = [50, 10, 0]
        max = [80, 30, 8]
        power_w = 5.0
        [[boundaries]]
        face = "xmin"
        type = "inlet"
        velocity_m_s = 1.0
        t_ambient_c = 25
        [[boundaries]]
        face = "xmax"
        type = "opening"
        t_ambient_c = 25
    "#);
    let out = s.run(|_| {}).unwrap();
    let f = out.result.flow.as_ref().unwrap();
    eprintln!(
        "FORCED: converged {} it {} imbalance {:.3e} % advected {:.4} W, heater max {:.2}",
        f.converged, f.iterations, out.report.energy_imbalance_pct, out.report.advected_out_w,
        out.report.blocks[0].t_max_c
    );
    assert!(f.converged);
    assert!(out.report.energy_imbalance_pct.abs() < 0.5);
    // outlet air temperature rise from the energy balance
    let q = 1.0 * 0.04 * 0.02;
    let dt_air = 5.0 / (AIR_RHO * AIR_CP * q);
    assert!(out.report.blocks[0].t_max_c > 25.0 + dt_air);
}

/// Fan with a linear curve in a frictionless duct (symmetry walls) with a
/// grille: the operating point must satisfy
/// P0 (1 − Q/Qmax) = ½ρ (1 + ζ + ζ_exit) (Q/A)², with ζ_exit = 0.01 (solver floor).
#[test]
fn fan_operating_point() {
    let s = sim(r#"
        [domain]
        min = [0, 0, 0]
        max = [300, 50, 50]
        material = "air"
        [grid]
        max_cell_mm = 5.0
        [flow]
        turbulence = "laminar"
        gravity = [0, 0, 0]
        max_iterations = 800
        tolerance = 1e-5
        [[fans]]
        name = "fan"
        center = [100, 25, 25]
        direction = "+x"
        size = [50, 50]
        curve = [[0.0, 20.0], [0.02, 0.0]]
        [[resistances]]
        name = "grille"
        center = [200, 25, 25]
        axis = "x"
        size = [50, 50]
        loss = { zeta = 3.0 }
        [[boundaries]]
        face = "xmin"
        type = "opening"
        t_ambient_c = 20
        [[boundaries]]
        face = "xmax"
        type = "opening"
        t_ambient_c = 20
        [[boundaries]]
        face = "ymin"
        type = "symmetry"
        [[boundaries]]
        face = "ymax"
        type = "symmetry"
        [[boundaries]]
        face = "zmin"
        type = "symmetry"
        [[boundaries]]
        face = "zmax"
        type = "symmetry"
    "#);
    let out = s.run(|_| {}).unwrap();
    let f = out.result.flow.as_ref().unwrap();
    assert!(f.converged, "{:?}", f.residuals.last());
    let q = f.fans[0].flow_m3_s;
    // solve the 1D operating point
    let a = 0.05 * 0.05;
    let k = 0.5 * AIR_RHO * (1.0 + 3.0 + 0.01) / (a * a);
    // 20 (1 − Q/0.02) = k Q²  →  k Q² + 1000 Q − 20 = 0
    let qx = (-1000.0 + (1000.0f64 * 1000.0 + 4.0 * k * 20.0).sqrt()) / (2.0 * k);
    eprintln!("FAN: Q {q:.6} expected {qx:.6}, dp {:.3} Pa", f.fans[0].pressure_pa);
    assert!((q / qx - 1.0).abs() < 0.02);
}

/// Grey radiation between two parallel plates (gap 5 mm, 100 × 100 mm),
/// closed by re-radiating sides, plus conduction through still air.
/// Reference: q_rad = σ(T1⁴ − T2⁴)/((1−ε1)/ε1 + 1/F_eff + (1−ε2)/ε2), with
/// F_eff = F12 + (1 − F12)/2 for symmetric re-radiating sides and F12 from
/// the aligned-parallel-rectangles formula (Incropera, Table 13.2).
#[test]
fn radiation_parallel_plates() {
    let s = sim(r#"
        [domain]
        min = [0, 0, 0]
        max = [100, 100, 15]
        material = "air-still"
        [grid]
        max_cell_mm = [5, 5, 1]
        [radiation]
        rays_per_patch = 4000
        patch_size_mm = 20
        [[blocks]]
        name = "hot"
        material = "cu-c11000"
        min = [0, 0, 0]
        max = [100, 100, 5]
        emissivity = 0.8
        [[blocks]]
        name = "cold"
        material = "cu-c11000"
        min = [0, 0, 10]
        max = [100, 100, 15]
        emissivity = 0.6
        [[boundaries]]
        face = "zmin"
        type = "fixed"
        t_c = 100
        [[boundaries]]
        face = "zmax"
        type = "fixed"
        t_c = 20
    "#);
    let out = s.run(|_| {}).unwrap();
    let q = -out.report.boundaries[0].heat_out_w;
    // view factor of aligned parallel rectangles X = Y = 100/5 = 20
    let (x, y): (f64, f64) = (20.0, 20.0);
    let f12 = 2.0 / (std::f64::consts::PI * x * y)
        * (((1.0 + x * x) * (1.0 + y * y) / (1.0 + x * x + y * y)).sqrt().ln()
            + x * (1.0 + y * y).sqrt() * (x / (1.0 + y * y).sqrt()).atan()
            + y * (1.0 + x * x).sqrt() * (y / (1.0 + x * x).sqrt()).atan()
            - x * x.atan()
            - y * y.atan());
    let feff = f12 + 0.5 * (1.0 - f12);
    // copper plates: nearly isothermal at the fixed temperatures
    let t1: f64 = 100.0 + 273.15;
    let t2: f64 = 20.0 + 273.15;
    let a = 0.1 * 0.1;
    let qrad = a * SIGMA * (t1.powi(4) - t2.powi(4))
        / ((1.0 - 0.8) / 0.8 + 1.0 / feff + (1.0 - 0.6) / 0.6);
    let qcond = AIR_K * a * 80.0 / 0.005;
    eprintln!("RAD: q {q:.4} W, expected {:.4} (rad {qrad:.4} + cond {qcond:.4}), F12 {f12:.4}", qrad + qcond);
    assert!((q / (qrad + qcond) - 1.0).abs() < 0.03);
}

/// Temperature-dependent conductivity k = k0 (1 + b T): the heat flux through
/// a slab is q = (k0/L)[(T1 − T2) + b/2 (T1² − T2²)] (Kirchhoff transform).
#[test]
fn k_of_t_slab() {
    let lib_json = r#"{"materials":[{"id":"kt","name":"k(T)","k":10.0,"density":1000,"cp":1000,
        "k_table":[[0.0,10.0],[200.0,30.0]]}]}"#;
    let mut lib = Library::builtin();
    lib.merge_json(lib_json, "t").unwrap();
    let case = Case::from_toml(
        r#"
        [domain]
        min = [0, 0, 0]
        max = [20, 5, 5]
        material = "kt"
        [grid]
        max_cell_mm = [0.25, 5, 5]
        [[boundaries]]
        face = "xmin"
        type = "fixed"
        t_c = 150
        [[boundaries]]
        face = "xmax"
        type = "fixed"
        t_c = 10
        [solver]
        nonlinear_tolerance_k = 1e-6
    "#,
    )
    .unwrap();
    let s = Simulation::with_library(case, lib).unwrap();
    let out = s.run(|_| {}).unwrap();
    let q = -out.report.boundaries[0].heat_out_w;
    // k = 10 + 0.1 T
    let (t1, t2) = (150.0f64, 10.0f64);
    let exact = (5e-3 * 5e-3) / 0.02 * (10.0 * (t1 - t2) + 0.05 * (t1 * t1 - t2 * t2));
    eprintln!("K(T): q {q:.5} exact {exact:.5}");
    assert!((q / exact - 1.0).abs() < 1e-3);
}

/// Absorbed solar flux on an insulated plate with convection: T = T∞ + αG/h.
#[test]
fn solar_flux_plate() {
    let s = sim(r#"
        [domain]
        min = [0, 0, 0]
        max = [50, 50, 3]
        material = "al-6061-t6"
        [grid]
        max_cell_mm = 5
        [[boundaries]]
        face = "zmax"
        type = "convection"
        h = 10
        t_ambient_c = 30
        solar_w_m2 = 1000
        absorptivity = 0.4
    "#);
    let out = s.run(|_| {}).unwrap();
    let t = out.report.blocks.first().map(|b| b.t_avg_c).unwrap_or(out.report.t_max_c);
    eprintln!("SOLAR: T {t:.4} expected {:.4}", 30.0 + 0.4 * 1000.0 / 10.0);
    assert!((out.report.t_max_c - 70.0).abs() < 0.01);
    assert!(out.report.energy_imbalance_pct.abs() < 1e-6);
}

/// DELPHI-style network: a two-resistor star network must agree with the
/// equivalent 2R model.
#[test]
fn network_matches_2r() {
    let base = |comp: &str| {
        format!(
            r#"
        [domain]
        min = [0, 0, 0]
        max = [40, 40, 10]
        material = "air-still"
        [grid]
        max_cell_mm = 1.0
        [[blocks]]
        name = "board"
        material = "al-6061-t6"
        min = [0, 0, 0]
        max = [40, 40, 3]
        [[boundaries]]
        face = "zmin"
        type = "fixed"
        t_c = 40
        [[boundaries]]
        face = "zmax"
        type = "convection"
        h = 20
        t_ambient_c = 40
        {comp}
    "#
        )
    };
    let two_r = sim(&base(
        r#"[[components]]
        name = "U1"
        package = "qfn-5x5"
        position = [17.5, 17.5, 3]
        r_jc = 5.0
        r_jb = 30.0
        case_face = "bottom"
        power_w = 2.0"#,
    ));
    let net = sim(&base(
        r#"[[components]]
        name = "U1"
        package = "qfn-5x5"
        position = [17.5, 17.5, 3]
        power_w = 2.0
        [components.network]
        nodes = [{ name = "junction" }, { name = "bottom", surface = "bottom" }, { name = "top", surface = "top" }]
        links = [{ a = "junction", b = "bottom", r = 5.0 }, { a = "junction", b = "top", r = 30.0 }]"#,
    ));
    let a = two_r.run(|_| {}).unwrap().report.components[0].tj_c;
    let b = net.run(|_| {}).unwrap();
    let tj = b.report.components[0].tj_c;
    eprintln!("NETWORK: 2R Tj {a:.4}, network Tj {tj:.4}; nodes {:?}", b.report.components[0].nodes);
    assert!((a - tj).abs() / (a - 40.0) < 0.02);
    assert!(b.report.energy_imbalance_pct.abs() < 1e-6);
}
