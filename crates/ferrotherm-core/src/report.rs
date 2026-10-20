// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Post-processing: block statistics, junction temperatures with derating
//! checks, and the global energy balance.

use crate::flow::{FanResult, MonitorResult, OpeningResult};
use crate::model::{Model, Owner};
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
    /// Temperature of every network node (multi-node models).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<(String, f64)>,
    /// Heat leaving through each model surface, W.
    pub surfaces: Vec<(String, f64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowSummary {
    pub fluid: String,
    pub turbulence: String,
    pub iterations: usize,
    pub converged: bool,
    pub converged_by_monitors: bool,
    /// Final normalised [mass, momentum, energy] residuals.
    pub final_residuals: [f64; 3],
    pub max_speed_m_s: f64,
    pub fans: Vec<FanResult>,
    pub openings: Vec<OpeningResult>,
    pub monitors: Vec<MonitorResult>,
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
    /// Heat carried out by the air through openings, W (part of heat_out_w).
    pub advected_out_w: f64,
    /// Heat radiated out through openings, W (part of heat_out_w).
    pub radiated_out_w: f64,
    pub blocks: Vec<BlockStats>,
    pub components: Vec<ComponentResult>,
    pub boundaries: Vec<BoundaryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow: Option<FlowSummary>,
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
        let mut qs = vec![0.0; c.surfaces.len()];
        for &(li, surf) in &c.links {
            let l = m.links[li];
            qs[surf as usize] += l.g * (t[l.a] - t[l.b]);
        }
        for &(bi, surf) in &c.boundary {
            let e = &m.boundary[bi];
            qs[surf as usize] += (t[e.node] - r.surface_t[bi]) / e.r_int;
        }
        let (qc, qb) = if c.network {
            (0.0, 0.0)
        } else {
            (qs[0], qs[1])
        };
        let t_case = if c.network { None } else { c.r_jc.map(|r| tj - qc * r) };
        let t_board = if c.network { None } else { c.r_jb.map(|r| tj - qb * r) };
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
            nodes: if c.network {
                c.nodes.iter().map(|(n, i)| (n.clone(), t[*i])).collect()
            } else {
                vec![]
            },
            surfaces: c.surfaces.iter().cloned().zip(qs).collect(),
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
        advected_out_w: h.advected_out,
        radiated_out_w: h.radiation_to_openings,
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
        flow: r.flow.as_ref().map(|f| FlowSummary {
            fluid: f.fluid.clone(),
            turbulence: f.turbulence.clone(),
            iterations: f.iterations,
            converged: f.converged,
            converged_by_monitors: f.converged_by_monitors,
            final_residuals: f.residuals.last().copied().unwrap_or([0.0; 3]),
            max_speed_m_s: f.max_speed_m_s,
            fans: f.fans.clone(),
            openings: f.openings.clone(),
            monitors: f
                .monitors
                .iter()
                .map(|mo| MonitorResult {
                    history: vec![],
                    ..mo.clone()
                })
                .collect(),
        }),
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
            s += "## Components (compact models)\n\n";
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
            for c in self.components.iter().filter(|c| !c.nodes.is_empty()) {
                s += &format!("\n{} network: ", c.name);
                s += &c
                    .nodes
                    .iter()
                    .map(|(n, t)| format!("{n} {t:.2} °C"))
                    .collect::<Vec<_>>()
                    .join(", ");
                s += " · heat by surface: ";
                s += &c
                    .surfaces
                    .iter()
                    .map(|(n, q)| format!("{n} {q:.3} W"))
                    .collect::<Vec<_>>()
                    .join(", ");
                s += "\n";
            }
            s += "\n";
        }
        if let Some(f) = &self.flow {
            s += "## Flow\n\n";
            s += &format!(
                "Fluid: {} · turbulence: {} · {} iterations, {} (residuals mass {:.1e}, momentum {:.1e}, energy {:.1e}) · max speed {:.3} m/s\n\n",
                f.fluid,
                f.turbulence,
                f.iterations,
                if f.converged_by_monitors {
                    "converged (key temperatures steady within 0.05 K or 0.2 % of their rise over 50 iterations)"
                } else if f.converged {
                    "converged"
                } else {
                    "**NOT converged**"
                },
                f.final_residuals[0],
                f.final_residuals[1],
                f.final_residuals[2],
                f.max_speed_m_s
            );
            if !f.fans.is_empty() {
                s += "| Fan | Flow [m³/s] | Flow [CFM] | Static pressure [Pa] |\n|---|---:|---:|---:|\n";
                for fan in &f.fans {
                    s += &format!(
                        "| {} | {:.5} | {:.2} | {:.2} |\n",
                        fan.name,
                        fan.flow_m3_s,
                        fan.flow_m3_s / 4.719_474e-4,
                        fan.pressure_pa
                    );
                }
                s += "\n";
            }
            if !f.openings.is_empty() {
                s += "| Opening | In [m³/s] | Out [m³/s] | Heat carried out [W] |\n|---|---:|---:|---:|\n";
                for o in &f.openings {
                    s += &format!(
                        "| {} | {:.5} | {:.5} | {:.3} |\n",
                        o.name, o.inflow_m3_s, o.outflow_m3_s, o.heat_out_w
                    );
                }
                s += "\n";
            }
            if !f.monitors.is_empty() {
                s += "| Monitor | Position [mm] | T [°C] | Speed [m/s] |\n|---|---|---:|---:|\n";
                for mo in &f.monitors {
                    s += &format!(
                        "| {} | ({:.1}, {:.1}, {:.1}) | {:.2} | {:.4} |\n",
                        mo.name,
                        mo.position_mm[0],
                        mo.position_mm[1],
                        mo.position_mm[2],
                        mo.t_c,
                        mo.speed_m_s
                    );
                }
                s += "\n";
            }
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
        if self.advected_out_w != 0.0 || self.radiated_out_w != 0.0 {
            s += &format!(
                "\nOf the heat out: {:.3} W carried by the air through openings, {:.3} W radiated through openings.\n",
                self.advected_out_w, self.radiated_out_w
            );
        }
        if self.flow.is_none() {
            s += "\n_Conduction model: the air does not move (no [flow] section); valid and conservative for sealed natural-convection cavities only._\n";
        }
        s
    }
}
