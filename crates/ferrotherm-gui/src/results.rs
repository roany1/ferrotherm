// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Result tables and the transient junction-temperature chart.

use crate::app::{Sel, Solved};
use eframe::egui::{self, Align2, Color32, FontId, Grid, Pos2, RichText, Sense, Stroke, Ui, Vec2};

pub const PASS: Color32 = Color32::from_rgb(80, 190, 110);
pub const FAIL: Color32 = Color32::from_rgb(235, 85, 85);
pub const WARN: Color32 = Color32::from_rgb(230, 170, 60);

fn opt(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}")).unwrap_or_else(|| "–".into())
}

pub fn summary(ui: &mut Ui, s: &Solved, stale: bool) {
    let r = &s.report;
    ui.horizontal_wrapped(|ui| {
        if stale {
            ui.colored_label(
                WARN,
                "Model changed since this solution — solve again (F5).",
            );
            ui.separator();
        }
        let fails = r
            .components
            .iter()
            .filter(|c| c.pass == Some(false))
            .count();
        let checked = r.components.iter().filter(|c| c.pass.is_some()).count();
        if checked > 0 {
            if fails == 0 {
                ui.label(
                    RichText::new(format!("All {checked} limits met"))
                        .color(PASS)
                        .strong(),
                );
            } else {
                ui.label(
                    RichText::new(format!("{fails} of {checked} components over limit"))
                        .color(FAIL)
                        .strong(),
                );
            }
            ui.separator();
        }
        ui.label(format!("T max {:.1} °C", r.t_max_c));
        ui.separator();
        ui.label(format!(
            "Power {:.2} W · out {:.2} W{} · imbalance {:.1e} %",
            r.power_in_w,
            r.heat_out_w,
            if r.storage_rate_w.abs() > 1e-9 {
                format!(" · stored {:.2} W", r.storage_rate_w)
            } else {
                String::new()
            },
            r.energy_imbalance_pct
        ))
        .on_hover_text("Global energy balance: (in - out - storage) / in");
        ui.separator();
        ui.label(format!(
            "{} cells · {:.1} s · {} PCG it.",
            r.cells, r.elapsed_s, r.linear_iterations
        ));
        if r.time_s > 0.0 {
            ui.separator();
            ui.label(format!("t = {:.1} s", r.time_s));
        }
    });
}

pub fn tables(ui: &mut Ui, s: &Solved, sel: &mut Sel) {
    let r = &s.report;
    if !r.warnings.is_empty() {
        for w in &r.warnings {
            ui.colored_label(WARN, format!("Warning: {w}"));
        }
        ui.add_space(4.0);
    }
    ui.horizontal_top(|ui| {
        if !r.components.is_empty() {
            ui.vertical(|ui| {
                ui.label(RichText::new("Components").strong());
                Grid::new("res_comp")
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        for h in [
                            "Name",
                            "P [W]",
                            "Tj [°C]",
                            "Tcase",
                            "Tboard",
                            "Limit",
                            "Margin [K]",
                            "",
                        ] {
                            ui.label(RichText::new(h).weak());
                        }
                        ui.end_row();
                        for (i, c) in r.components.iter().enumerate() {
                            let resp = ui.selectable_label(*sel == Sel::Component(i), &c.name);
                            if resp.clicked() {
                                *sel = Sel::Component(i);
                            }
                            resp.on_hover_text(format!(
                                "{}\nheat via case {:.2} W, via board {:.2} W{}",
                                c.package,
                                c.q_case_w,
                                c.q_board_w,
                                c.limit_source
                                    .as_ref()
                                    .map(|s| format!("\nlimit: {s}"))
                                    .unwrap_or_default()
                            ));
                            ui.label(format!("{:.2}", c.power_w));
                            ui.label(RichText::new(format!("{:.1}", c.tj_c)).strong());
                            ui.label(opt(c.t_case_c));
                            ui.label(opt(c.t_board_c));
                            ui.label(opt(c.tj_limit_c));
                            ui.label(c.margin_k.map(|m| format!("{m:+.1}")).unwrap_or("–".into()));
                            match c.pass {
                                Some(true) => ui.colored_label(PASS, "PASS"),
                                Some(false) => {
                                    ui.colored_label(FAIL, RichText::new("FAIL").strong())
                                }
                                None => ui.label(""),
                            };
                            ui.end_row();
                        }
                    });
            });
            ui.add_space(24.0);
        }
        if !r.blocks.is_empty() {
            ui.vertical(|ui| {
                ui.label(RichText::new("Blocks").strong());
                Grid::new("res_blocks")
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        for h in ["Name", "Tmin", "Tavg", "Tmax [°C]"] {
                            ui.label(RichText::new(h).weak());
                        }
                        ui.end_row();
                        for b in &r.blocks {
                            let idx = s.case.blocks.iter().position(|x| x.name == b.name);
                            let selected = idx.map(|i| *sel == Sel::Block(i)).unwrap_or(false);
                            if ui.selectable_label(selected, &b.name).clicked() {
                                if let Some(i) = idx {
                                    *sel = Sel::Block(i);
                                }
                            }
                            ui.label(format!("{:.1}", b.t_min_c));
                            ui.label(format!("{:.1}", b.t_avg_c));
                            ui.label(RichText::new(format!("{:.1}", b.t_max_c)).strong());
                            ui.end_row();
                        }
                    });
            });
            ui.add_space(24.0);
        }
        ui.vertical(|ui| {
            ui.label(RichText::new("Boundaries").strong());
            Grid::new("res_bnd")
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Name").weak());
                    ui.label(RichText::new("Heat out [W]").weak());
                    ui.label(RichText::new("Share").weak());
                    ui.end_row();
                    let total: f64 = r.boundaries.iter().map(|b| b.heat_out_w.max(0.0)).sum();
                    for (i, b) in r.boundaries.iter().enumerate() {
                        if ui
                            .selectable_label(*sel == Sel::Boundary(i), &b.name)
                            .clicked()
                        {
                            *sel = Sel::Boundary(i);
                        }
                        ui.label(format!("{:.3}", b.heat_out_w));
                        ui.label(if total > 0.0 {
                            format!("{:.0} %", 100.0 * b.heat_out_w.max(0.0) / total)
                        } else {
                            "–".into()
                        });
                        ui.end_row();
                    }
                });
        });
    });
    if s.result.junction_history.len() > 2 && !r.components.is_empty() {
        ui.add_space(10.0);
        ui.label(RichText::new("Junction temperature vs time").strong());
        history_chart(ui, s);
    }
}

const SERIES: [Color32; 8] = [
    Color32::from_rgb(66, 135, 245),
    Color32::from_rgb(235, 110, 60),
    Color32::from_rgb(80, 190, 110),
    Color32::from_rgb(200, 90, 200),
    Color32::from_rgb(230, 190, 50),
    Color32::from_rgb(60, 200, 210),
    Color32::from_rgb(235, 85, 120),
    Color32::from_rgb(150, 150, 150),
];

fn nice_step(range: f64, target: usize) -> f64 {
    let raw = range / target.max(1) as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let f = raw / mag;
    let n = if f < 1.5 {
        1.0
    } else if f < 3.5 {
        2.0
    } else if f < 7.5 {
        5.0
    } else {
        10.0
    };
    n * mag
}

fn history_chart(ui: &mut Ui, s: &Solved) {
    let h = &s.result.junction_history;
    let names: Vec<&str> = s
        .report
        .components
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let t0 = h.first().map(|x| x.0).unwrap_or(0.0);
    let t1 = h.last().map(|x| x.0).unwrap_or(1.0).max(t0 + 1e-9);
    let (mut y0, mut y1) = (f64::INFINITY, f64::NEG_INFINITY);
    for (_, v) in h {
        for x in v {
            y0 = y0.min(*x);
            y1 = y1.max(*x);
        }
    }
    for c in &s.report.components {
        if let Some(l) = c.tj_limit_c {
            y1 = y1.max(l);
        }
    }
    let pad = ((y1 - y0) * 0.08).max(0.5);
    y0 -= pad;
    y1 += pad;
    let size = Vec2::new(ui.available_width().min(900.0), 220.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let text = ui.visuals().text_color();
    let weak = ui.visuals().weak_text_color();
    let plot = egui::Rect::from_min_max(
        Pos2::new(rect.left() + 46.0, rect.top() + 8.0),
        Pos2::new(rect.right() - 140.0, rect.bottom() - 24.0),
    );
    let px = |t: f64| plot.left() + ((t - t0) / (t1 - t0)) as f32 * plot.width();
    let py = |y: f64| plot.bottom() - ((y - y0) / (y1 - y0)) as f32 * plot.height();
    let grid = Stroke::new(1.0, weak.gamma_multiply(0.35));
    let ys = nice_step(y1 - y0, 5);
    let mut y = (y0 / ys).ceil() * ys;
    while y <= y1 {
        painter.line_segment(
            [
                Pos2::new(plot.left(), py(y)),
                Pos2::new(plot.right(), py(y)),
            ],
            grid,
        );
        painter.text(
            Pos2::new(plot.left() - 6.0, py(y)),
            Align2::RIGHT_CENTER,
            format!("{y:.0}"),
            FontId::proportional(11.0),
            weak,
        );
        y += ys;
    }
    let xs = nice_step(t1 - t0, 6);
    let mut x = (t0 / xs).ceil() * xs;
    while x <= t1 + 1e-9 {
        painter.line_segment(
            [
                Pos2::new(px(x), plot.top()),
                Pos2::new(px(x), plot.bottom()),
            ],
            grid,
        );
        painter.text(
            Pos2::new(px(x), plot.bottom() + 4.0),
            Align2::CENTER_TOP,
            format!("{x:.0}"),
            FontId::proportional(11.0),
            weak,
        );
        x += xs;
    }
    painter.text(
        Pos2::new(plot.right() + 8.0, plot.bottom()),
        Align2::LEFT_CENTER,
        "t [s]",
        FontId::proportional(11.0),
        weak,
    );
    painter.text(
        Pos2::new(rect.left() + 2.0, plot.top()),
        Align2::LEFT_TOP,
        "°C",
        FontId::proportional(11.0),
        weak,
    );
    for (k, name) in names.iter().enumerate() {
        let col = SERIES[k % SERIES.len()];
        let pts: Vec<Pos2> = h.iter().map(|(t, v)| Pos2::new(px(*t), py(v[k]))).collect();
        painter.add(egui::Shape::line(pts, Stroke::new(1.8, col)));
        if let Some(l) = s.report.components[k].tj_limit_c {
            let yl = py(l);
            let mut xx = plot.left();
            while xx < plot.right() {
                painter.line_segment(
                    [
                        Pos2::new(xx, yl),
                        Pos2::new((xx + 6.0).min(plot.right()), yl),
                    ],
                    Stroke::new(1.0, col.gamma_multiply(0.7)),
                );
                xx += 10.0;
            }
        }
        let ly = plot.top() + 6.0 + k as f32 * 16.0;
        painter.line_segment(
            [
                Pos2::new(plot.right() + 12.0, ly),
                Pos2::new(plot.right() + 28.0, ly),
            ],
            Stroke::new(2.5, col),
        );
        painter.text(
            Pos2::new(plot.right() + 34.0, ly),
            Align2::LEFT_CENTER,
            *name,
            FontId::proportional(11.0),
            text,
        );
    }
    painter.rect_stroke(
        plot,
        0.0,
        Stroke::new(1.0, weak.gamma_multiply(0.6)),
        egui::StrokeKind::Inside,
    );
}

/// Normalised flow residuals vs iteration (log scale).
pub fn residual_chart(ui: &mut Ui, hist: &[(usize, [f64; 3])], tol: f64) {
    let size = Vec2::new(ui.available_width().min(900.0), 200.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let weak = ui.visuals().weak_text_color();
    let text = ui.visuals().text_color();
    let plot = egui::Rect::from_min_max(
        Pos2::new(rect.left() + 46.0, rect.top() + 8.0),
        Pos2::new(rect.right() - 120.0, rect.bottom() - 22.0),
    );
    let n0 = hist.first().map(|h| h.0).unwrap_or(0) as f64;
    let n1 = (hist.last().map(|h| h.0).unwrap_or(1) as f64).max(n0 + 1.0);
    let mut lo = tol.log10().floor() - 0.5;
    let mut hi = lo + 1.0;
    // the first iterations start from rest and can be huge: keep them out
    // of the scale (they are clipped at the top)
    for (_, r) in hist.iter().skip(if hist.len() > 5 { 3 } else { 0 }) {
        for v in r {
            if *v > 0.0 && v.is_finite() {
                lo = lo.min(v.log10());
                hi = hi.max(v.log10());
            }
        }
    }
    lo = lo.floor().max(tol.log10() - 4.0);
    hi = hi.ceil().clamp(lo + 1.0, tol.log10() + 5.0).max(lo + 1.0);
    let px = |x: f64| plot.left() + ((x - n0) / (n1 - n0)) as f32 * plot.width();
    let py = |v: f64| {
        let l = v.max(1e-300).log10().clamp(lo, hi);
        plot.bottom() - ((l - lo) / (hi - lo)) as f32 * plot.height()
    };
    let grid = Stroke::new(1.0, weak.gamma_multiply(0.35));
    let mut e = lo;
    while e <= hi + 1e-9 {
        let y = py(10f64.powf(e));
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            grid,
        );
        painter.text(
            Pos2::new(plot.left() - 6.0, y),
            Align2::RIGHT_CENTER,
            format!("1e{e:.0}"),
            FontId::proportional(10.0),
            weak,
        );
        e += 1.0;
    }
    let xs = nice_step(n1 - n0, 6);
    let mut x = (n0 / xs).ceil() * xs;
    while x <= n1 + 1e-9 {
        painter.text(
            Pos2::new(px(x), plot.bottom() + 3.0),
            Align2::CENTER_TOP,
            format!("{x:.0}"),
            FontId::proportional(10.0),
            weak,
        );
        x += xs;
    }
    // tolerance line
    let yt = py(tol);
    let mut xx = plot.left();
    while xx < plot.right() {
        painter.line_segment(
            [
                Pos2::new(xx, yt),
                Pos2::new((xx + 6.0).min(plot.right()), yt),
            ],
            Stroke::new(1.0, WARN),
        );
        xx += 10.0;
    }
    for (k, name) in ["mass", "momentum", "energy"].iter().enumerate() {
        let col = SERIES[k];
        let pts: Vec<Pos2> = hist
            .iter()
            .map(|(i, r)| Pos2::new(px(*i as f64), py(r[k])))
            .collect();
        painter.add(egui::Shape::line(pts, Stroke::new(1.6, col)));
        let ly = plot.top() + 6.0 + k as f32 * 16.0;
        painter.line_segment(
            [
                Pos2::new(plot.right() + 12.0, ly),
                Pos2::new(plot.right() + 28.0, ly),
            ],
            Stroke::new(2.5, col),
        );
        painter.text(
            Pos2::new(plot.right() + 34.0, ly),
            Align2::LEFT_CENTER,
            *name,
            FontId::proportional(11.0),
            text,
        );
    }
    painter.text(
        Pos2::new(plot.right() + 12.0, plot.top() + 6.0 + 3.0 * 16.0),
        Align2::LEFT_CENTER,
        "- - target",
        FontId::proportional(11.0),
        WARN,
    );
    painter.rect_stroke(
        plot,
        0.0,
        Stroke::new(1.0, weak.gamma_multiply(0.6)),
        egui::StrokeKind::Inside,
    );
}

/// Fans, openings and monitor points of a flow solution.
pub fn flow_tables(ui: &mut Ui, f: &ferrotherm_core::flow::FlowResult) {
    ui.add_space(6.0);
    ui.label(format!(
        "{} iterations · {} · max air speed {:.3} m/s · {} / {}",
        f.iterations,
        if f.converged {
            if f.converged_by_monitors {
                "converged (temperatures settled)"
            } else {
                "converged (residuals)"
            }
        } else {
            "NOT converged: raise max iterations or check the model"
        },
        f.max_speed_m_s,
        f.fluid,
        f.turbulence
    ));
    if f.averaged_iterations > 0 {
        ui.colored_label(
            WARN,
            format!(
                "The steady iteration oscillates (key temperatures ±{:.2} K): fields averaged over the last {} \
                 iterations. Lower the velocity/pressure relaxation or run a transient.",
                0.5 * f.oscillation_k,
                f.averaged_iterations
            ),
        );
    }
    ui.horizontal_top(|ui| {
        if !f.fans.is_empty() {
            ui.vertical(|ui| {
                ui.label(RichText::new("Fans").strong());
                Grid::new("res_fans")
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        for h in ["Name", "Q [m³/s]", "CFM", "Δp [Pa]"] {
                            ui.label(RichText::new(h).weak());
                        }
                        ui.end_row();
                        for x in &f.fans {
                            ui.label(&x.name);
                            ui.label(format!("{:.5}", x.flow_m3_s));
                            ui.label(format!("{:.2}", x.flow_m3_s / crate::flow_ui::CFM));
                            ui.label(format!("{:.1}", x.pressure_pa));
                            ui.end_row();
                        }
                    });
            });
            ui.add_space(24.0);
        }
        if !f.openings.is_empty() {
            ui.vertical(|ui| {
                ui.label(RichText::new("Openings").strong());
                Grid::new("res_open")
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        for h in ["Name", "In [m³/s]", "Out [m³/s]", "Heat out [W]"] {
                            ui.label(RichText::new(h).weak());
                        }
                        ui.end_row();
                        for x in &f.openings {
                            ui.label(&x.name);
                            ui.label(format!("{:.5}", x.inflow_m3_s));
                            ui.label(format!("{:.5}", x.outflow_m3_s));
                            ui.label(format!("{:.3}", x.heat_out_w));
                            ui.end_row();
                        }
                    });
            });
            ui.add_space(24.0);
        }
        if !f.monitors.is_empty() {
            ui.vertical(|ui| {
                ui.label(RichText::new("Monitor points").strong());
                Grid::new("res_mon")
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        for h in ["Name", "T [°C]", "Speed [m/s]"] {
                            ui.label(RichText::new(h).weak());
                        }
                        ui.end_row();
                        for x in &f.monitors {
                            ui.label(&x.name);
                            ui.label(format!("{:.2}", x.t_c));
                            ui.label(format!("{:.3}", x.speed_m_s));
                            ui.end_row();
                        }
                    });
            });
        }
    });
}
