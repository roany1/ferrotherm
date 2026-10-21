// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Editors for the CFD objects: flow and radiation settings, fans, grilles
//! (planar resistances), heat sinks, monitor points and DELPHI networks.

use crate::inspector::{material_combo, num, AXES};
use eframe::egui::{self, Color32, ComboBox, DragValue, Grid, RichText, Ui};
use ferrotherm_core::case::{
    Fan, FlowSettings, HeatSink, Loss, Monitor, NetLink, NetNode, Network, RadiationSettings,
    Resistance, Scheme, SurfaceKind, Turbulence,
};
use ferrotherm_core::library::Library;

const WARN: Color32 = Color32::from_rgb(230, 170, 60);
/// 1 CFM in m³/s.
pub const CFM: f64 = 4.719_474_432e-4;

fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    Grid::new(id)
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, add);
}

fn heading(ui: &mut Ui, kind: &str, name: &str) {
    ui.label(RichText::new(kind).small().weak());
    ui.heading(name);
    ui.separator();
}

fn vec3(ui: &mut Ui, v: &mut [f64; 3], suffix: &str, speed: f64) {
    ui.horizontal(|ui| {
        for k in 0..3 {
            ui.label(RichText::new(AXES[k]).weak());
            num(ui, &mut v[k], speed, suffix);
        }
    });
}

fn pos(ui: &mut Ui, v: &mut f64, speed: f64, suffix: &str) -> egui::Response {
    ui.add(
        DragValue::new(v)
            .speed(speed)
            .range(0.0..=f64::INFINITY)
            .suffix(suffix)
            .max_decimals(5),
    )
}

fn opt_pos(ui: &mut Ui, v: &mut Option<f64>, default: f64, speed: f64, suffix: &str, none: &str) {
    ui.horizontal(|ui| {
        let mut on = v.is_some();
        if ui.checkbox(&mut on, "").changed() {
            *v = on.then_some(default);
        }
        match v {
            Some(x) => {
                pos(ui, x, speed, suffix);
            }
            None => {
                ui.label(RichText::new(none).weak().italics());
            }
        }
    });
}

fn axis_combo(ui: &mut Ui, salt: &str, axis: &mut String) {
    ComboBox::from_id_salt(salt)
        .width(60.0)
        .selected_text(axis.as_str())
        .show_ui(ui, |ui| {
            for a in AXES {
                ui.selectable_value(axis, a.to_string(), a);
            }
        });
}

// ------------------------------------------------------------------ settings

pub fn flow_settings(ui: &mut Ui, f: &mut Option<FlowSettings>, fill: &mut String, lib: &Library) {
    heading(ui, "MODEL", "Air flow (CFD)");
    let mut on = f.as_ref().is_some_and(|f| f.enabled);
    if ui
        .checkbox(
            &mut on,
            "Solve the air flow (natural and forced convection)",
        )
        .changed()
    {
        match f {
            Some(s) => s.enabled = on,
            None if on => *f = Some(FlowSettings::default()),
            None => {}
        }
    }
    let fluid_ok = lib
        .material(fill)
        .map(|m| m.viscosity.is_some())
        .unwrap_or(false);
    if on && !fluid_ok {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(
                WARN,
                format!("The domain fill '{fill}' has no viscosity: flow needs a fluid."),
            );
            if ui.small_button("Use 'air'").clicked() {
                *fill = "air".into();
            }
            if ui.small_button("Use 'water'").clicked() {
                *fill = "water".into();
            }
        });
    }
    ui.label(
        RichText::new(
            "Finite-volume Navier–Stokes (SIMPLEC, staggered grid) with Boussinesq buoyancy and \
             conjugate heat transfer: the fluid is the domain fill, blocks are solids. Turbulence: \
             LVEL (algebraic, for electronics) or laminar.",
        )
        .weak(),
    );
    let Some(s) = f else { return };
    if !s.enabled {
        return;
    }
    ui.add_space(6.0);
    grid(ui, "flow", |ui| {
        ui.label("Turbulence");
        ComboBox::from_id_salt("turb")
            .selected_text(match s.turbulence {
                Turbulence::Lvel => "LVEL",
                Turbulence::Laminar => "Laminar",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut s.turbulence, Turbulence::Lvel, "LVEL")
                    .on_hover_text(
                        "Algebraic model with wall distance and Spalding's law of the wall",
                    );
                ui.selectable_value(&mut s.turbulence, Turbulence::Laminar, "Laminar");
            });
        ui.end_row();
        ui.label("Convection scheme");
        ComboBox::from_id_salt("scheme")
            .selected_text(match s.scheme {
                Scheme::PowerLaw => "Power law",
                Scheme::Upwind => "Upwind",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut s.scheme, Scheme::PowerLaw, "Power law");
                ui.selectable_value(&mut s.scheme, Scheme::Upwind, "Upwind");
            });
        ui.end_row();
        ui.label("Gravity");
        vec3(ui, &mut s.gravity, " m/s²", 0.01);
        ui.end_row();
        ui.label("Reference temperature");
        ui.horizontal(|ui| {
            let mut on = s.reference_temperature_c.is_some();
            if ui.checkbox(&mut on, "").changed() {
                s.reference_temperature_c = on.then_some(25.0);
            }
            match &mut s.reference_temperature_c {
                Some(t) => {
                    num(ui, t, 0.5, " °C");
                }
                None => {
                    ui.label(RichText::new("ambient").weak().italics());
                }
            }
        });
        ui.end_row();
        ui.label("Max iterations");
        ui.add(DragValue::new(&mut s.max_iterations).range(10..=100_000));
        ui.end_row();
        ui.label("Residual target");
        ui.add(
            DragValue::new(&mut s.tolerance)
                .speed(0.0)
                .range(1e-8..=0.1)
                .custom_formatter(|x, _| format!("{x:.0e}"))
                .custom_parser(|t| t.trim().parse().ok()),
        )
        .on_hover_text(
            "Normalised mass, momentum and energy residuals. The run also stops when the \
             junction and monitor temperatures stop changing.",
        );
        ui.end_row();
        ui.label("Relaxation u / p / T");
        ui.horizontal(|ui| {
            for v in [
                &mut s.relax_velocity,
                &mut s.relax_pressure,
                &mut s.relax_temperature,
            ] {
                ui.add(DragValue::new(v).speed(0.01).range(0.05..=1.0));
            }
        });
        ui.end_row();
        ui.label("Iterations per time step");
        ui.add(DragValue::new(&mut s.iterations_per_step).range(1..=1000))
            .on_hover_text("Transient runs only");
        ui.end_row();
    });
}

pub fn radiation_settings(ui: &mut Ui, r: &mut Option<RadiationSettings>) {
    heading(ui, "MODEL", "Internal radiation");
    let mut on = r.as_ref().is_some_and(|r| r.enabled);
    if ui
        .checkbox(&mut on, "Surface-to-surface radiation inside the domain")
        .changed()
    {
        match r {
            Some(s) => s.enabled = on,
            None if on => *r = Some(RadiationSettings::default()),
            None => {}
        }
    }
    ui.label(
        RichText::new(
            "View factors by Monte Carlo ray tracing through the grid, radiosity network between \
             surface patches of blocks and components; openings radiate to the ambient. \
             Air is transparent.",
        )
        .weak(),
    );
    let Some(s) = r else { return };
    if !s.enabled {
        return;
    }
    grid(ui, "rad", |ui| {
        ui.label("Patch size");
        ui.add(
            DragValue::new(&mut s.patch_size_mm)
                .speed(0.2)
                .range(0.5..=500.0)
                .suffix(" mm"),
        )
        .on_hover_text("Surfaces are grouped into patches of about this size");
        ui.end_row();
        ui.label("Rays per patch");
        ui.add(DragValue::new(&mut s.rays_per_patch).range(16..=100_000));
        ui.end_row();
        ui.label("Default emissivity");
        ui.add(
            DragValue::new(&mut s.default_emissivity)
                .speed(0.01)
                .range(0.0..=1.0),
        );
        ui.end_row();
    });
}

// ------------------------------------------------------------------ objects

const DIRS: [&str; 6] = ["+x", "-x", "+y", "-y", "+z", "-z"];

pub fn fan(ui: &mut Ui, f: &mut Fan, idx: usize, result: Option<(f64, f64)>) {
    heading(ui, "FAN", &f.name.clone());
    if let Some((q, p)) = result {
        ui.label(
            RichText::new(format!(
                "Operating point: {:.5} m³/s ({:.2} CFM) at {:.1} Pa",
                q,
                q / CFM,
                p
            ))
            .strong(),
        );
        ui.separator();
    }
    grid(ui, "fan", |ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut f.name);
        ui.end_row();
        ui.label("Centre");
        vec3(ui, &mut f.center, " mm", 0.1);
        ui.end_row();
        ui.label("Blows towards");
        ComboBox::from_id_salt(format!("fdir{idx}"))
            .width(60.0)
            .selected_text(f.direction.as_str())
            .show_ui(ui, |ui| {
                for d in DIRS {
                    ui.selectable_value(&mut f.direction, d.to_string(), d);
                }
            });
        ui.end_row();
        let round = f.size.is_none();
        ui.label("Shape");
        ui.horizontal(|ui| {
            if ui.selectable_label(round, "Round").clicked() && !round {
                let s = f.size.take().unwrap_or([40.0, 40.0]);
                f.diameter = Some(s[0].min(s[1]));
            }
            if ui.selectable_label(!round, "Rectangular").clicked() && round {
                let d = f.diameter.unwrap_or(40.0);
                f.size = Some([d, d]);
                f.diameter = None;
                f.hub_diameter = None;
            }
        });
        ui.end_row();
        if round {
            ui.label("Diameter");
            let mut d = f.diameter.unwrap_or(40.0);
            if pos(ui, &mut d, 0.1, " mm").changed() || f.diameter.is_none() {
                f.diameter = Some(d);
            }
            ui.end_row();
            ui.label("Hub diameter");
            opt_pos(ui, &mut f.hub_diameter, 0.4 * d, 0.1, " mm", "no hub");
            ui.end_row();
        } else if let Some(s) = &mut f.size {
            ui.label("Size");
            ui.horizontal(|ui| {
                pos(ui, &mut s[0], 0.1, " mm");
                ui.label("×");
                pos(ui, &mut s[1], 0.1, " mm");
            });
            ui.end_row();
        }
        let fixed = f.flow_m3_s.is_some();
        ui.label("Model");
        ui.horizontal(|ui| {
            if ui.selectable_label(!fixed, "Fan curve").clicked() && fixed {
                f.flow_m3_s = None;
                if f.curve.is_empty() {
                    f.curve = vec![[0.0, 30.0], [0.005, 15.0], [0.01, 0.0]];
                }
            }
            if ui.selectable_label(fixed, "Fixed flow").clicked() && !fixed {
                f.flow_m3_s = Some(0.005);
            }
        });
        ui.end_row();
        if let Some(q) = &mut f.flow_m3_s {
            ui.label("Volume flow");
            ui.horizontal(|ui| {
                pos(ui, q, 0.0001, " m³/s");
                ui.label(RichText::new(format!("= {:.2} CFM", *q / CFM)).weak());
            });
            ui.end_row();
        }
    });
    if f.flow_m3_s.is_none() {
        ui.add_space(6.0);
        ui.label(RichText::new("Fan curve (static pressure vs flow, from the datasheet)").strong());
        let mut remove = None;
        Grid::new(format!("curve{idx}"))
            .num_columns(4)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Q [m³/s]").weak());
                ui.label(RichText::new("CFM").weak());
                ui.label(RichText::new("Δp [Pa]").weak());
                ui.label("");
                ui.end_row();
                for (i, pt) in f.curve.iter_mut().enumerate() {
                    pos(ui, &mut pt[0], 0.0001, "");
                    let mut cfm = pt[0] / CFM;
                    if pos(ui, &mut cfm, 0.05, "").changed() {
                        pt[0] = cfm * CFM;
                    }
                    pos(ui, &mut pt[1], 0.1, "");
                    if ui.small_button("x").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        if let Some(i) = remove {
            f.curve.remove(i);
        }
        ui.horizontal(|ui| {
            if ui.small_button("+ point").clicked() {
                let last = f.curve.last().copied().unwrap_or([0.0, 20.0]);
                f.curve.push([last[0] + 0.001, (last[1] - 5.0).max(0.0)]);
            }
            if ui.small_button("Sort").clicked() {
                f.curve.sort_by(|a, b| a[0].total_cmp(&b[0]));
            }
        });
        let increasing = f.curve.windows(2).all(|w| w[1][0] > w[0][0]);
        if f.curve.len() < 2 || !increasing {
            ui.colored_label(
                WARN,
                "The curve needs at least two points with increasing flow.",
            );
        }
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "A fan on the domain boundary draws from / blows to the ambient; inside the domain \
             it is a pressure-jump plane. The operating point comes from the curve and the \
             system resistance.",
        )
        .weak(),
    );
}

pub fn loss_editor(ui: &mut Ui, l: &mut Loss, salt: &str) {
    Grid::new(salt)
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            ui.label("Loss coefficient ζ");
            pos(ui, &mut l.zeta, 0.05, "").on_hover_text(
                "Δp = ½ ρ ζ u² on the approach velocity u. From Idelchik (diagram 8-1 for thin \
                 perforated plates), the grille datasheet or a test.",
            );
            ui.end_row();
            ui.label("Free-area ratio");
            opt_pos(ui, &mut l.free_area, 0.5, 0.01, "", "not set");
            ui.end_row();
            ui.label("Hole diameter");
            opt_pos(ui, &mut l.hole_diameter_mm, 3.0, 0.05, " mm", "not set");
            ui.end_row();
        });
    if l.free_area.is_some() && l.hole_diameter_mm.is_some() {
        ui.label(
            RichText::new(
                "Laminar term 30/(f²·Re) added (Idelchik, via Electronics Cooling, May 1998).",
            )
            .weak(),
        );
    }
}

pub fn resistance(ui: &mut Ui, r: &mut Resistance, idx: usize) {
    heading(ui, "GRILLE / RESISTANCE", &r.name.clone());
    grid(ui, "res", |ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut r.name);
        ui.end_row();
        ui.label("Centre");
        vec3(ui, &mut r.center, " mm", 0.1);
        ui.end_row();
        ui.label("Normal axis");
        axis_combo(ui, &format!("rax{idx}"), &mut r.axis);
        ui.end_row();
        ui.label("Size");
        ui.horizontal(|ui| {
            pos(ui, &mut r.size[0], 0.1, " mm");
            ui.label("×");
            pos(ui, &mut r.size[1], 0.1, " mm");
        });
        ui.end_row();
    });
    ui.add_space(4.0);
    loss_editor(ui, &mut r.loss, &format!("rloss{idx}"));
    ui.label(
        RichText::new("A thin plane inside the domain: filters, perforated plates, card guides.")
            .weak(),
    );
}

pub fn heatsink(ui: &mut Ui, h: &mut HeatSink, idx: usize, lib: &Library) {
    heading(ui, "HEAT SINK", &h.name.clone());
    grid(ui, "hs", |ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut h.name);
        ui.end_row();
        ui.label("Material");
        material_combo(ui, &format!("hsmat{idx}"), &mut h.material, lib);
        ui.end_row();
        ui.label("Base min corner");
        vec3(ui, &mut h.position, " mm", 0.1);
        ui.end_row();
        ui.label("Base normal");
        axis_combo(ui, &format!("hsn{idx}"), &mut h.normal);
        ui.end_row();
        ui.label("Fins run along");
        axis_combo(ui, &format!("hsf{idx}"), &mut h.fin_axis);
        ui.end_row();
        ui.label("Base L × W × t");
        ui.horizontal(|ui| {
            for k in 0..3 {
                pos(ui, &mut h.base[k], 0.1, " mm");
            }
        });
        ui.end_row();
        ui.label("Fins");
        ui.add(DragValue::new(&mut h.fin_count).range(1..=500));
        ui.end_row();
        ui.label("Fin thickness");
        pos(ui, &mut h.fin_thickness, 0.05, " mm");
        ui.end_row();
        ui.label("Fin height");
        pos(ui, &mut h.fin_height, 0.1, " mm");
        ui.end_row();
        ui.label("Emissivity");
        opt_pos(ui, &mut h.emissivity, 0.85, 0.01, "", "radiation default");
        ui.end_row();
    });
    match h.blocks() {
        Ok(b) => {
            let gap = if h.fin_count > 1 {
                (h.base[1] - h.fin_count as f64 * h.fin_thickness) / (h.fin_count - 1) as f64
            } else {
                0.0
            };
            ui.label(format!("{} blocks · fin gap {:.2} mm", b.len(), gap));
            if gap > 0.0 {
                ui.label(
                    RichText::new(
                        "Resolve each gap with at least 2–3 cells (Grid) for the flow between fins.",
                    )
                    .weak(),
                );
            }
        }
        Err(e) => {
            ui.colored_label(WARN, e.to_string());
        }
    }
}

pub fn monitor(ui: &mut Ui, m: &mut Monitor, result: Option<(f64, f64)>) {
    heading(ui, "MONITOR POINT", &m.name.clone());
    if let Some((t, v)) = result {
        ui.label(RichText::new(format!("{t:.2} °C · {v:.3} m/s")).strong());
        ui.separator();
    }
    grid(ui, "mon", |ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut m.name);
        ui.end_row();
        ui.label("Position");
        vec3(ui, &mut m.position, " mm", 0.1);
        ui.end_row();
    });
    ui.label(
        RichText::new(
            "Temperature and speed are recorded every iteration; flow runs stop when monitors \
             and junctions settle.",
        )
        .weak(),
    );
}

// ------------------------------------------------------------------ network

const SURFACES: [(Option<SurfaceKind>, &str); 9] = [
    (None, "internal"),
    (Some(SurfaceKind::Top), "top"),
    (Some(SurfaceKind::Bottom), "bottom"),
    (Some(SurfaceKind::TopInner), "top inner"),
    (Some(SurfaceKind::TopOuter), "top outer"),
    (Some(SurfaceKind::BottomInner), "bottom inner"),
    (Some(SurfaceKind::BottomOuter), "bottom outer"),
    (Some(SurfaceKind::Sides), "sides"),
    (Some(SurfaceKind::Leads), "leads"),
];

fn surf_name(s: Option<SurfaceKind>) -> &'static str {
    SURFACES
        .iter()
        .find(|x| x.0 == s)
        .map(|x| x.1)
        .unwrap_or("?")
}

/// JEDEC/DELPHI-style default topology; resistances are placeholders the
/// user must replace with the supplier's values.
pub fn delphi_template() -> Network {
    let n = |name: &str, s: Option<SurfaceKind>| NetNode {
        name: name.into(),
        surface: s,
    };
    let l = |a: &str, b: &str| NetLink {
        a: a.into(),
        b: b.into(),
        r: 10.0,
    };
    Network {
        junction: "junction".into(),
        nodes: vec![
            n("junction", None),
            n("top_inner", Some(SurfaceKind::TopInner)),
            n("top_outer", Some(SurfaceKind::TopOuter)),
            n("bottom_inner", Some(SurfaceKind::BottomInner)),
            n("bottom_outer", Some(SurfaceKind::BottomOuter)),
            n("sides", Some(SurfaceKind::Sides)),
            n("leads", Some(SurfaceKind::Leads)),
        ],
        links: vec![
            l("junction", "top_inner"),
            l("junction", "top_outer"),
            l("junction", "bottom_inner"),
            l("junction", "bottom_outer"),
            l("junction", "sides"),
            l("junction", "leads"),
        ],
        inner_size_mm: None,
        lead_ring_mm: 1.0,
    }
}

pub fn network_editor(ui: &mut Ui, net: &mut Option<Network>, idx: usize) {
    ui.add_space(8.0);
    ui.label(RichText::new("Compact model").strong());
    ui.horizontal(|ui| {
        if ui
            .selectable_label(net.is_none(), "2R (θJC, θJB)")
            .clicked()
        {
            *net = None;
        }
        if ui
            .selectable_label(net.is_some(), "Multi-node (DELPHI)")
            .on_hover_text("JESD15-4 style network from the part supplier")
            .clicked()
            && net.is_none()
        {
            *net = Some(delphi_template());
        }
    });
    let Some(n) = net else { return };
    ui.colored_label(
        WARN,
        "Template resistances are 10 K/W placeholders: enter the supplier's DELPHI values.",
    );
    let names: Vec<String> = n.nodes.iter().map(|x| x.name.clone()).collect();
    Grid::new(format!("netn{idx}"))
        .num_columns(3)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label(RichText::new("Node").weak());
            ui.label(RichText::new("Attached to").weak());
            ui.label("");
            ui.end_row();
            let mut remove = None;
            for (i, node) in n.nodes.iter_mut().enumerate() {
                ui.add(egui::TextEdit::singleline(&mut node.name).desired_width(110.0));
                ComboBox::from_id_salt(format!("ns{idx}_{i}"))
                    .width(110.0)
                    .selected_text(surf_name(node.surface))
                    .show_ui(ui, |ui| {
                        for (s, label) in SURFACES {
                            ui.selectable_value(&mut node.surface, s, label);
                        }
                    });
                if ui.small_button("x").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
            if let Some(i) = remove {
                n.nodes.remove(i);
            }
        });
    if ui.small_button("+ node").clicked() {
        n.nodes.push(NetNode {
            name: format!("n{}", n.nodes.len() + 1),
            surface: None,
        });
    }
    ui.add_space(4.0);
    Grid::new(format!("netl{idx}"))
        .num_columns(4)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label(RichText::new("From").weak());
            ui.label(RichText::new("To").weak());
            ui.label(RichText::new("R [K/W]").weak());
            ui.label("");
            ui.end_row();
            let mut remove = None;
            for (i, l) in n.links.iter_mut().enumerate() {
                for (k, end) in [&mut l.a, &mut l.b].into_iter().enumerate() {
                    ComboBox::from_id_salt(format!("nl{idx}_{i}_{k}"))
                        .width(110.0)
                        .selected_text(end.as_str())
                        .show_ui(ui, |ui| {
                            for nm in &names {
                                ui.selectable_value(end, nm.clone(), nm);
                            }
                        });
                }
                ui.add(DragValue::new(&mut l.r).speed(0.05).range(1e-4..=1e6));
                if ui.small_button("x").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
            if let Some(i) = remove {
                n.links.remove(i);
            }
        });
    if ui.small_button("+ link").clicked() {
        let a = names.first().cloned().unwrap_or_default();
        let b = names.get(1).cloned().unwrap_or_default();
        n.links.push(NetLink { a, b, r: 10.0 });
    }
    grid(ui, &format!("netg{idx}"), |ui| {
        ui.label("Power node");
        ComboBox::from_id_salt(format!("nj{idx}"))
            .width(110.0)
            .selected_text(n.junction.as_str())
            .show_ui(ui, |ui| {
                for nm in &names {
                    ui.selectable_value(&mut n.junction, nm.clone(), nm);
                }
            });
        ui.end_row();
        ui.label("Lead ring width");
        pos(ui, &mut n.lead_ring_mm, 0.05, " mm");
        ui.end_row();
        ui.label("Inner region L × W");
        ui.horizontal(|ui| {
            let mut on = n.inner_size_mm.is_some();
            if ui.checkbox(&mut on, "").changed() {
                n.inner_size_mm = on.then_some([5.0, 5.0]);
            }
            match &mut n.inner_size_mm {
                Some(s) => {
                    pos(ui, &mut s[0], 0.05, " mm");
                    pos(ui, &mut s[1], 0.05, " mm");
                }
                None => {
                    ui.label(RichText::new("half the body").weak().italics());
                }
            }
        });
        ui.end_row();
    });
}
