// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Post-processing: block statistics, junction temperatures with derating
//! checks, and the global energy balance.

use crate::model::{Model, Owner, Side};
use crate::simulate::RunResult;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct BlockStats {
    pub name: String,
    pub material: String,
    pub power_w: f64,
    pub t_min_c: f64,
    pub t_avg_c: f64,
    pub t_max_c: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentResult {
    pub name: String,
    pub package: String,
    pub power_w: f64,
    pub tj_c: f64,
    pub t_case_c: Option<f64>,
    pub t_board_c: Option<f64>,
    pub q_case_w: f64,
    pub q_board_w: f64,
    pub tj_limit_c: Option<f64>,
    pub limit_source: Option<String>,
    pub margin_k: Option<f64>,
    pub pass: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BoundaryResult {
    pub name: String,
    pub heat_out_w: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub title: String,
    pub author: String,
    pub solver_version: String,
    pub cells: usize,
    pub grid: [usize; 3],
    pub time_s: f64,
    pub ambient_c: Option<f64>,
    pub t_max_c: f64,
    pub t_max_location_mm: [f64; 3],
    pub power_in_w: f64,
    pub heat_out_w: f64,
    pub storage_rate_w: f64,
    /// (in − out − storage) / in, %.
    pub energy_imbalance_pct: f64,
    pub linear_iterations: usize,
    pub nonlinear_iterations: usize,
    pub elapsed_s: f64,
    pub blocks: Vec<BlockStats>,
    pub components: Vec<ComponentResult>,
    pub boundaries: Vec<BoundaryResult>,
    pub warnings: Vec<String>,
}

pub fn build(m: &Model, r: &RunResult, title: &str, author: &str) -> Report {
    let snap = r.last();
    let t = &snap.t;
    let g = &m.grid;

    let mut blocks: Vec<(f64, f64, f64, f64)> =
        vec![(f64::INFINITY, 0.0, f64::NEG_INFINITY, 0.0); m.blocks.len()];
    let mut tmax = f64::NEG_INFINITY;
    let mut tmax_c = 0;
    for c in 0..m.ncells {
        if matches!(m.owner[c], Owner::Component(_)) {
            continue; // compact bodies are not physical temperatures
        }
        if t[c] > tmax {
            tmax = t[c];
            tmax_c = c;
        }
        if let Owner::Block(b) = m.owner[c] {
            let v = g.volume(c);
            let s = &mut blocks[b];
            s.0 = s.0.min(t[c]);
            s.1 += t[c] * v;
            s.2 = s.2.max(t[c]);
            s.3 += v;
        }
    }
    let p = g.ijk(tmax_c);
    let loc = [0, 1, 2].map(|a| g.center(a, p[a]) * 1e3);

    let block_stats = m
        .blocks
        .iter()
        .zip(&blocks)
        .filter(|(b, _)| b.ncells > 0)
        .map(|(b, s)| BlockStats {
            name: b.name.clone(),
            material: b.material.clone(),
            power_w: b.power_w,
            t_min_c: s.0,
            t_avg_c: s.1 / s.3,
            t_max_c: s.2,
        })
        .collect();

    let mut comps = Vec::new();
    for c in &m.components {
        let tj = t[c.node];
        let (mut qc, mut qb) = (0.0, 0.0);
        for &(li, side) in &c.links {
            let l = m.links[li];
            let q = l.g * (tj - t[l.b]);
            match side {
                Side::Case => qc += q,
                Side::Board => qb += q,
            }
        }
        for &(bi, side) in &c.boundary {
            let e = &m.boundary[bi];
            let q = (tj - r.surface_t[bi]) / e.r_int;
            match side {
                Side::Case => qc += q,
                Side::Board => qb += q,
            }
        }
        let t_case = c.r_jc.map(|r| tj - qc * r);
        let t_board = c.r_jb.map(|r| tj - qb * r);
        let (lim, src) = match &c.tj_limit {
            Some((l, s)) => (Some(*l), Some(s.clone())),
            None => (None, None),
        };
        comps.push(ComponentResult {
            name: c.name.clone(),
            package: c.package.clone(),
            power_w: c.power_w,
            tj_c: tj,
            t_case_c: t_case,
            t_board_c: t_board,
            q_case_w: qc,
            q_board_w: qb,
            tj_limit_c: lim,
            limit_source: src,
            margin_k: lim.map(|l| l - tj),
            pass: lim.map(|l| tj <= l),
        });
    }

    let h = &r.heat;
    let imb = if h.total_in.abs() > 0.0 {
        (h.total_in - h.total_out - h.storage_rate) / h.total_in * 100.0
    } else {
        0.0
    };
    Report {
        title: title.to_string(),
        author: author.to_string(),
        solver_version: format!("FerroTherm {}", env!("CARGO_PKG_VERSION")),
        cells: m.ncells,
        grid: g.n(),
        time_s: snap.time_s,
        ambient_c: m.ambient_c,
        t_max_c: tmax,
        t_max_location_mm: loc,
        power_in_w: h.total_in,
        heat_out_w: h.total_out,
        storage_rate_w: h.storage_rate,
        energy_imbalance_pct: imb,
        linear_iterations: r.linear_iterations,
        nonlinear_iterations: r.nonlinear_iterations,
        elapsed_s: r.elapsed_s,
        blocks: block_stats,
        components: comps,
        boundaries: m
            .boundary_names
            .iter()
            .zip(&h.per_boundary)
            .map(|(n, q)| BoundaryResult {
                name: n.clone(),
                heat_out_w: *q,
            })
            .collect(),
        warnings: m.warnings.clone(),
    }
}

fn opt(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "–".into())
}

impl Report {
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        s += &format!(
            "# {}\n\n",
            if self.title.is_empty() {
                "FerroTherm report"
            } else {
                &self.title
            }
        );
        if !self.author.is_empty() {
            s += &format!("Author: {}  \n", self.author);
        }
        s += &format!("Solver: {}  \n", self.solver_version);
        s += &format!(
            "Grid: {} × {} × {} = {} cells, solved in {:.2} s ({} PCG iterations, {} outer iterations)\n\n",
            self.grid[0], self.grid[1], self.grid[2], self.cells, self.elapsed_s, self.linear_iterations, self.nonlinear_iterations
        );
        if self.time_s > 0.0 {
            s += &format!("Results at t = {:.3} s\n\n", self.time_s);
        }
        if let Some(a) = self.ambient_c {
            s += &format!("Default ambient: {a:.1} °C\n\n");
        }
        s += "## Energy balance\n\n";
        s += &format!(
            "| Power in | Heat out | Storage rate | Imbalance |\n|---:|---:|---:|---:|\n| {:.4} W | {:.4} W | {:.4} W | {:.2e} % |\n\n",
            self.power_in_w, self.heat_out_w, self.storage_rate_w, self.energy_imbalance_pct
        );
        s += &format!(
            "Hottest cell: {:.2} °C at ({:.1}, {:.1}, {:.1}) mm\n\n",
            self.t_max_c,
            self.t_max_location_mm[0],
            self.t_max_location_mm[1],
            self.t_max_location_mm[2]
        );
        if !self.components.is_empty() {
            s += "## Components (2R compact models)\n\n";
            s += "| Component | Package | P [W] | Tj [°C] | Tcase [°C] | Tboard [°C] | Q case/board [W] | Tj limit [°C] | Margin [K] | Result |\n";
            s += "|---|---|---:|---:|---:|---:|---:|---:|---:|---|\n";
            for c in &self.components {
                let res = match c.pass {
                    Some(true) => "PASS",
                    Some(false) => "**FAIL**",
                    None => "–",
                };
                s += &format!(
                    "| {} | {} | {:.2} | {:.2} | {} | {} | {:.2} / {:.2} | {} | {} | {} |\n",
                    c.name,
                    c.package,
                    c.power_w,
                    c.tj_c,
                    opt(c.t_case_c),
                    opt(c.t_board_c),
                    c.q_case_w,
                    c.q_board_w,
                    opt(c.tj_limit_c),
                    opt(c.margin_k),
                    res
                );
            }
            let srcs: Vec<&String> = self
                .components
                .iter()
                .filter_map(|c| c.limit_source.as_ref())
                .collect();
            if !srcs.is_empty() {
                let mut u: Vec<&String> = srcs.clone();
                u.sort();
                u.dedup();
                s += "\nLimits: ";
                s += &u.iter().map(|x| x.as_str()).collect::<Vec<_>>().join("; ");
                s += "\n";
            }
            s += "\n";
        }
        if !self.blocks.is_empty() {
            s += "## Blocks\n\n| Block | Material | P [W] | Tmin [°C] | Tavg [°C] | Tmax [°C] |\n|---|---|---:|---:|---:|---:|\n";
            for b in &self.blocks {
                s += &format!(
                    "| {} | {} | {:.2} | {:.2} | {:.2} | {:.2} |\n",
                    b.name, b.material, b.power_w, b.t_min_c, b.t_avg_c, b.t_max_c
                );
            }
            s += "\n";
        }
        s += "## Boundaries\n\n| Boundary | Heat out [W] |\n|---|---:|\n";
        for b in &self.boundaries {
            s += &format!("| {} | {:.4} |\n", b.name, b.heat_out_w);
        }
        if !self.warnings.is_empty() {
            s += "\n## Warnings\n\n";
            for w in &self.warnings {
                s += &format!("- {w}\n");
            }
        }
        s += "\n_Phase 0 model: 3D conduction with convection/radiation boundary conditions; internal air conducts only (no flow), which is conservative for natural-convection cavities._\n";
        s
    }
}
