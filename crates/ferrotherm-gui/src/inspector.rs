// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Property editors for every item of a case.

use eframe::egui::{self, Color32, ComboBox, DragValue, Grid, RichText, Ui};
use ferrotherm_core::case::{
    Block, Boundary, BoundaryKind, Component, Domain, EnvironmentRef, EnvironmentUse, Face,
    GridSettings, MaybeVec3, Meta, Mode, Region2, SolverSettings, TransientSettings,
};
use ferrotherm_core::library::{CaseFace, Library};
use ferrotherm_core::pcb::{CopperLayer, PcbStack};
use ferrotherm_core::report::ComponentResult;

pub const AXES: [&str; 3] = ["x", "y", "z"];

// ------------------------------------------------------------------ widgets

pub fn num(ui: &mut Ui, v: &mut f64, speed: f64, suffix: &str) -> egui::Response {
    ui.add(
        DragValue::new(v)
            .speed(speed)
            .suffix(suffix)
            .max_decimals(4),
    )
}

fn num_min(ui: &mut Ui, v: &mut f64, speed: f64, min: f64, suffix: &str) -> egui::Response {
    ui.add(
        DragValue::new(v)
            .speed(speed)
            .range(min..=f64::INFINITY)
            .suffix(suffix)
            .max_decimals(4),
    )
}

fn sci(ui: &mut Ui, v: &mut f64) -> egui::Response {
    ui.add(
        DragValue::new(v)
            .speed(0.0)
            .range(1e-15..=1.0)
            .custom_formatter(|x, _| format!("{x:.1e}"))
            .custom_parser(|s| s.trim().parse::<f64>().ok()),
    )
    .on_hover_text("Type a value, e.g. 1e-9")
}

fn vec3(ui: &mut Ui, v: &mut [f64; 3], suffix: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        for k in 0..3 {
            ui.label(RichText::new(AXES[k]).weak());
            changed |= num(ui, &mut v[k], 0.1, suffix).changed();
        }
    });
    changed
}

/// Optional number: checkbox + value; `none_text` describes what happens when unset.
fn opt_num(
    ui: &mut Ui,
    v: &mut Option<f64>,
    default: f64,
    speed: f64,
    suffix: &str,
    none_text: &str,
) {
    ui.horizontal(|ui| {
        let mut on = v.is_some();
        if ui.checkbox(&mut on, "").changed() {
            *v = if on { Some(default) } else { None };
        }
        match v {
            Some(x) => {
                num(ui, x, speed, suffix);
            }
            None => {
                ui.label(RichText::new(none_text).weak().italics());
            }
        }
    });
}

fn axis_combo(ui: &mut Ui, salt: &str, axis: &mut String) {
    ComboBox::from_id_salt(salt)
        .width(60.0)
        .truncate()
        .selected_text(axis.as_str())
        .show_ui(ui, |ui| {
            for a in AXES {
                ui.selectable_value(axis, a.to_string(), a);
            }
        });
}

pub fn material_info(lib: &Library, id: &str) -> String {
    match lib.material(id) {
        Ok(m) => {
            let k = m.k.as_vec();
            let ks = if k[0] == k[1] && k[1] == k[2] {
                format!("{:.4} W/(m·K)", k[0])
            } else {
                format!("{:.3} / {:.3} / {:.3} W/(m·K)", k[0], k[1], k[2])
            };
            let mut s = format!(
                "{}\nk = {ks}\nρ = {:.0} kg/m³, cp = {:.0} J/(kg·K)",
                m.name, m.density, m.cp
            );
            if !m.specs.is_empty() {
                s += &format!("\nSpecs: {}", m.specs.join(", "));
            }
            if !m.source.is_empty() {
                s += &format!("\nSource: {}", m.source);
            }
            if !m.notes.is_empty() {
                s += &format!("\nNote: {}", m.notes);
            }
            s
        }
        Err(_) => format!("Unknown material '{id}'"),
    }
}

pub fn material_combo(ui: &mut Ui, salt: &str, value: &mut String, lib: &Library) {
    let text = lib
        .material(value)
        .map(|m| m.name.clone())
        .unwrap_or_else(|_| format!("unknown: {value}"));
    let r = ComboBox::from_id_salt(salt)
        .width(230.0)
        .truncate()
        .height(460.0)
        .selected_text(text)
        .show_ui(ui, |ui| {
            let mut cats: Vec<&str> = lib.materials.iter().map(|m| m.category.as_str()).collect();
            cats.dedup();
            let mut seen = Vec::new();
            for c in cats {
                if seen.contains(&c) || c == "internal" {
                    continue;
                }
                seen.push(c);
                ui.label(RichText::new(c.to_uppercase()).small().strong());
                for m in lib.materials.iter().filter(|m| m.category == c) {
                    ui.selectable_value(value, m.id.clone(), &m.name)
                        .on_hover_text(material_info(lib, &m.id));
                }
            }
        });
    r.response.on_hover_text(material_info(lib, value));
}

fn package_combo(ui: &mut Ui, salt: &str, value: &mut String, lib: &Library) {
    let text = lib
        .package(value)
        .map(|p| p.name.clone())
        .unwrap_or_else(|_| format!("unknown: {value}"));
    ComboBox::from_id_salt(salt)
        .width(230.0)
        .truncate()
        .height(460.0)
        .selected_text(text)
        .show_ui(ui, |ui| {
            let mut fams: Vec<&str> = Vec::new();
            for p in &lib.packages {
                if !fams.contains(&p.family.as_str()) {
                    fams.push(&p.family);
                }
            }
            for f in fams {
                ui.label(RichText::new(f).small().strong());
                for p in lib.packages.iter().filter(|p| p.family == f) {
                    let tip = format!(
                        "{}\nRjc: {}\n{}\n{}",
                        p.name,
                        p.r_jc
                            .map(|r| format!("{r} K/W"))
                            .unwrap_or_else(|| "from datasheet".into()),
                        p.standard,
                        p.notes
                    );
                    ui.selectable_value(value, p.id.clone(), &p.name)
                        .on_hover_text(tip);
                }
            }
        });
}

fn finish_combo(ui: &mut Ui, salt: &str, value: &mut String, lib: &Library) {
    let text = lib
        .finish(value)
        .map(|f| format!("{} (ε {})", f.name, f.emissivity))
        .unwrap_or_else(|_| format!("unknown: {value}"));
    ComboBox::from_id_salt(salt)
        .width(230.0)
        .truncate()
        .selected_text(text)
        .show_ui(ui, |ui| {
            for f in &lib.finishes {
                ui.selectable_value(
                    value,
                    f.id.clone(),
                    format!("{}  (ε {})", f.name, f.emissivity),
                )
                .on_hover_text(format!("{}\n{}", f.specs.join(", "), f.source));
            }
        });
}

// ------------------------------------------------------------------ editors

fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    Grid::new(id)
        .num_columns(2)
        .spacing([10.0, 6.0])
        .striped(false)
        .show(ui, add);
}

fn heading(ui: &mut Ui, kind: &str, name: &str) {
    ui.label(RichText::new(kind).small().weak());
    ui.heading(name);
    ui.separator();
}

pub fn meta(ui: &mut Ui, m: &mut Meta) {
    heading(ui, "MODEL", "Project information");
    grid(ui, "meta", |ui| {
        ui.label("Title");
        ui.text_edit_singleline(&mut m.title);
        ui.end_row();
        ui.label("Author");
        ui.text_edit_singleline(&mut m.author);
        ui.end_row();
        ui.label("Description");
        ui.text_edit_multiline(&mut m.description);
        ui.end_row();
    });
}

pub fn domain(ui: &mut Ui, d: &mut Domain, lib: &Library) {
    heading(ui, "MODEL", "Domain");
    grid(ui, "domain", |ui| {
        ui.label("Min");
        vec3(ui, &mut d.min, " mm");
        ui.end_row();
        ui.label("Max");
        vec3(ui, &mut d.max, " mm");
        ui.end_row();
        ui.label("Fill material");
        material_combo(ui, "domain_mat", &mut d.material, lib);
        ui.end_row();
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Everything not covered by a block is filled with this material. \
             Phase 0 has no fluid flow: internal air conducts only.",
        )
        .weak(),
    );
}

pub fn grid_settings(ui: &mut Ui, g: &mut GridSettings, info: Option<&str>) {
    heading(ui, "MODEL", "Grid");
    let mut per_axis = matches!(g.max_cell_mm, MaybeVec3::Vec3(_));
    grid(ui, "grid", |ui| {
        ui.label("Per-axis size");
        if ui.checkbox(&mut per_axis, "").changed() {
            g.max_cell_mm = if per_axis {
                MaybeVec3::Vec3(g.max_cell_mm.as_vec())
            } else {
                MaybeVec3::Scalar(g.max_cell_mm.as_vec()[0])
            };
        }
        ui.end_row();
        ui.label("Max cell size");
        match &mut g.max_cell_mm {
            MaybeVec3::Scalar(v) => {
                ui.add(
                    DragValue::new(v)
                        .speed(0.05)
                        .range(0.01..=1000.0)
                        .suffix(" mm"),
                );
            }
            MaybeVec3::Vec3(v) => {
                ui.horizontal(|ui| {
                    for k in 0..3 {
                        ui.label(RichText::new(AXES[k]).weak());
                        ui.add(
                            DragValue::new(&mut v[k])
                                .speed(0.05)
                                .range(0.01..=1000.0)
                                .suffix(" mm"),
                        );
                    }
                });
            }
        }
        ui.end_row();
        ui.label("Min cells per interval");
        ui.add(DragValue::new(&mut g.min_cells_per_interval).range(1..=20));
        ui.end_row();
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "The grid always contains every block, component and boundary-region plane; \
             boards get at least 4 cells through their thickness. Start coarse (3-4 mm), \
             then refine and check that junction temperatures stop changing.",
        )
        .weak(),
    );
    if let Some(i) = info {
        ui.add_space(6.0);
        ui.label(i);
    }
}

pub fn environment(ui: &mut Ui, env: &mut Option<EnvironmentRef>, lib: &Library) {
    heading(ui, "MODEL", "Environment");
    let mut on = env.is_some();
    if ui
        .checkbox(&mut on, "Use an environment preset for the ambient")
        .changed()
    {
        *env = if on {
            Some(EnvironmentRef {
                preset: lib
                    .environments
                    .first()
                    .map(|e| e.id.clone())
                    .unwrap_or_default(),
                use_: EnvironmentUse::AmbientMax,
            })
        } else {
            None
        };
    }
    let Some(e) = env else {
        ui.label(
            RichText::new("Without a preset, every convection boundary needs its own ambient.")
                .weak(),
        );
        return;
    };
    grid(ui, "env", |ui| {
        ui.label("Preset");
        let text = lib
            .environment(&e.preset)
            .map(|x| x.name.clone())
            .unwrap_or_else(|_| format!("unknown: {}", e.preset));
        ComboBox::from_id_salt("env_preset")
            .width(260.0)
            .truncate()
            .selected_text(text)
            .show_ui(ui, |ui| {
                for x in &lib.environments {
                    ui.selectable_value(&mut e.preset, x.id.clone(), &x.name);
                }
            });
        ui.end_row();
        ui.label("Temperature used");
        let names = [
            (EnvironmentUse::AmbientMax, "Ambient, max"),
            (EnvironmentUse::AmbientMin, "Ambient, min"),
            (EnvironmentUse::InducedMax, "Induced (storage), max"),
            (EnvironmentUse::InducedMin, "Induced (storage), min"),
        ];
        let cur = names
            .iter()
            .find(|n| n.0 == e.use_)
            .map(|n| n.1)
            .unwrap_or("");
        ComboBox::from_id_salt("env_use")
            .width(260.0)
            .truncate()
            .selected_text(cur)
            .show_ui(ui, |ui| {
                for (v, n) in names {
                    ui.selectable_value(&mut e.use_, v, n);
                }
            });
        ui.end_row();
    });
    if let Ok(x) = lib.environment(&e.preset) {
        ui.add_space(6.0);
        ui.label(format!(
            "Ambient {:.1} … {:.1} °C · induced {:.1} … {:.1} °C",
            x.ambient_min_c, x.ambient_max_c, x.induced_min_c, x.induced_max_c
        ));
        ui.label(RichText::new(&x.source).weak());
        if !x.notes.is_empty() {
            ui.label(RichText::new(&x.notes).weak().italics());
        }
    }
}

pub fn solver(ui: &mut Ui, s: &mut SolverSettings) {
    heading(ui, "MODEL", "Solver");
    grid(ui, "solver", |ui| {
        ui.label("Analysis");
        ComboBox::from_id_salt("mode")
            .selected_text(match s.mode {
                Mode::Steady => "Steady state",
                Mode::Transient => "Transient",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut s.mode, Mode::Steady, "Steady state");
                ui.selectable_value(&mut s.mode, Mode::Transient, "Transient");
            });
        ui.end_row();
        if s.mode == Mode::Transient {
            let tr = s.transient.get_or_insert(TransientSettings {
                dt_s: 10.0,
                end_s: 600.0,
                output_every_s: None,
            });
            ui.label("Time step");
            num_min(ui, &mut tr.dt_s, 0.1, 1e-6, " s");
            ui.end_row();
            ui.label("End time");
            num_min(ui, &mut tr.end_s, 1.0, 1e-6, " s");
            ui.end_row();
            ui.label("Save field every");
            let d = tr.end_s / 10.0;
            opt_num(
                ui,
                &mut tr.output_every_s,
                d,
                1.0,
                " s",
                "start and end only",
            );
            ui.end_row();
        }
        ui.label("Initial temperature");
        opt_num(ui, &mut s.initial_c, 25.0, 0.5, " °C", "ambient");
        ui.end_row();
        ui.label("Linear tolerance");
        sci(ui, &mut s.tolerance);
        ui.end_row();
        ui.label("Max iterations");
        ui.add(DragValue::new(&mut s.max_iterations).range(10..=10_000_000));
        ui.end_row();
        ui.label("Radiation tolerance");
        ui.add(
            DragValue::new(&mut s.nonlinear_tolerance_k)
                .speed(0.0001)
                .range(1e-8..=10.0)
                .suffix(" K"),
        );
        ui.end_row();
    });
}

fn size_rows(ui: &mut Ui, min: &mut [f64; 3], max: &mut [f64; 3]) {
    ui.label("Min");
    vec3(ui, min, " mm");
    ui.end_row();
    ui.label("Size");
    let mut size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    if vec3(ui, &mut size, " mm") {
        for k in 0..3 {
            max[k] = min[k] + size[k];
        }
    }
    ui.end_row();
    ui.label("Max");
    vec3(ui, max, " mm");
    ui.end_row();
}

fn default_pcb(thickness: f64) -> PcbStack {
    PcbStack {
        thickness_mm: if thickness > 0.2 { thickness } else { 1.6 },
        dielectric: "fr4".into(),
        copper: "cu-c11000".into(),
        copper_layers: vec![],
        layers: Some(4),
        copper_oz: Some(1.0),
        coverage: Some(0.5),
        via_fraction: 0.0,
        normal: "z".into(),
    }
}

pub fn block(ui: &mut Ui, b: &mut Block, lib: &Library, idx: usize) {
    heading(ui, "BLOCK", &b.name.clone());
    let is_pcb = b.pcb.is_some();
    grid(ui, "block", |ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut b.name);
        ui.end_row();
        ui.label("Kind");
        ui.horizontal(|ui| {
            if ui.selectable_label(!is_pcb, "Material").clicked() && is_pcb {
                b.pcb = None;
                b.material = Some("al-6061-t6".into());
            }
            if ui.selectable_label(is_pcb, "PCB stack-up").clicked() && !is_pcb {
                let t = b.max[2] - b.min[2];
                b.pcb = Some(default_pcb(t));
                b.material = None;
            }
        });
        ui.end_row();
        if let Some(m) = &mut b.material {
            ui.label("Material");
            material_combo(ui, &format!("bmat{idx}"), m, lib);
            ui.end_row();
        }
        size_rows(ui, &mut b.min, &mut b.max);
        ui.label("Power");
        num_min(ui, &mut b.power_w, 0.01, 0.0, " W")
            .on_hover_text("Uniform heat load of the whole block");
        ui.end_row();
    });
    if let Some(stack) = &mut b.pcb {
        ui.add_space(8.0);
        ui.label(RichText::new("PCB stack-up").strong());
        pcb_editor(ui, stack, lib, idx);
        let thick = b.max[2] - b.min[2];
        if stack.normal == "z" && (thick - stack.thickness_mm).abs() > 1e-6 {
            ui.horizontal(|ui| {
                ui.colored_label(
                    Color32::from_rgb(230, 170, 60),
                    format!("Block height {thick:.3} mm differs from the board thickness"),
                );
                if ui.small_button("Match block").clicked() {
                    b.max[2] = b.min[2] + stack.thickness_mm;
                }
            });
        }
    } else if let Some(m) = &b.material {
        ui.add_space(8.0);
        ui.label(RichText::new(material_info(lib, m)).weak());
    }
}

fn pcb_editor(ui: &mut Ui, s: &mut PcbStack, lib: &Library, idx: usize) {
    let per_layer = !s.copper_layers.is_empty();
    grid(ui, "pcb", |ui| {
        ui.label("Thickness");
        num_min(ui, &mut s.thickness_mm, 0.01, 0.01, " mm");
        ui.end_row();
        ui.label("Dielectric");
        material_combo(ui, &format!("pcbdiel{idx}"), &mut s.dielectric, lib);
        ui.end_row();
        ui.label("Board normal");
        axis_combo(ui, &format!("pcbn{idx}"), &mut s.normal);
        ui.end_row();
        ui.label("Copper");
        ui.horizontal(|ui| {
            if ui.selectable_label(!per_layer, "Uniform").clicked() && per_layer {
                let first = s.copper_layers[0].clone();
                s.layers = Some(s.copper_layers.len());
                s.copper_oz = Some(first.oz);
                s.coverage = Some(first.coverage);
                s.copper_layers.clear();
            }
            if ui.selectable_label(per_layer, "Per layer").clicked() && !per_layer {
                let n = s.layers.unwrap_or(4).max(1);
                s.copper_layers = vec![
                    CopperLayer {
                        oz: s.copper_oz.unwrap_or(1.0),
                        coverage: s.coverage.unwrap_or(0.5),
                    };
                    n
                ];
            }
        });
        ui.end_row();
        if !per_layer {
            let mut n = s.layers.unwrap_or(4);
            ui.label("Layers");
            if ui.add(DragValue::new(&mut n).range(1..=64)).changed() {
                s.layers = Some(n);
            }
            ui.end_row();
            let mut oz = s.copper_oz.unwrap_or(1.0);
            ui.label("Copper weight");
            if ui
                .add(
                    DragValue::new(&mut oz)
                        .speed(0.05)
                        .range(0.1..=10.0)
                        .suffix(" oz"),
                )
                .on_hover_text("1 oz ≈ 35 µm")
                .changed()
            {
                s.copper_oz = Some(oz);
            }
            ui.end_row();
            let mut cov = s.coverage.unwrap_or(0.5) * 100.0;
            ui.label("Coverage");
            if ui
                .add(
                    DragValue::new(&mut cov)
                        .speed(0.5)
                        .range(0.0..=100.0)
                        .suffix(" %"),
                )
                .changed()
            {
                s.coverage = Some(cov / 100.0);
            }
            ui.end_row();
        }
        let mut vias = s.via_fraction * 100.0;
        ui.label("Thermal vias");
        if ui
            .add(
                DragValue::new(&mut vias)
                    .speed(0.05)
                    .range(0.0..=50.0)
                    .suffix(" %"),
            )
            .on_hover_text("Copper area fraction of plated via barrels over the board area")
            .changed()
        {
            s.via_fraction = vias / 100.0;
        }
        ui.end_row();
    });
    if !s.copper_layers.is_empty() {
        let mut remove = None;
        Grid::new("pcb_layers")
            .num_columns(4)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                for (i, l) in s.copper_layers.iter_mut().enumerate() {
                    ui.label(format!("L{}", i + 1));
                    ui.add(
                        DragValue::new(&mut l.oz)
                            .speed(0.05)
                            .range(0.1..=10.0)
                            .suffix(" oz"),
                    );
                    let mut cov = l.coverage * 100.0;
                    if ui
                        .add(
                            DragValue::new(&mut cov)
                                .speed(0.5)
                                .range(0.0..=100.0)
                                .suffix(" %"),
                        )
                        .changed()
                    {
                        l.coverage = cov / 100.0;
                    }
                    if ui.small_button("x").on_hover_text("Remove layer").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        if let Some(i) = remove {
            if s.copper_layers.len() > 1 {
                s.copper_layers.remove(i);
            }
        }
        if ui.small_button("+ layer").clicked() {
            let l = s.copper_layers.last().cloned().unwrap_or(CopperLayer {
                oz: 1.0,
                coverage: 0.5,
            });
            s.copper_layers.push(l);
        }
    }
    ui.add_space(4.0);
    match s.effective(lib) {
        Ok(e) => {
            ui.label(format!(
                "Effective k: {:.2} in-plane, {:.3} through-plane W/(m·K) · copper {:.0} µm",
                e.k_in_plane,
                e.k_through,
                e.copper_thickness_m * 1e6
            ));
        }
        Err(e) => {
            ui.colored_label(Color32::from_rgb(230, 90, 90), e.to_string());
        }
    }
}

pub fn component(
    ui: &mut Ui,
    c: &mut Component,
    lib: &Library,
    idx: usize,
    result: Option<&ComponentResult>,
) {
    heading(ui, "COMPONENT", &c.name.clone());
    if let Some(r) = result {
        let (col, verdict) = match r.pass {
            Some(true) => (Color32::from_rgb(80, 190, 110), "PASS"),
            Some(false) => (Color32::from_rgb(235, 85, 85), "FAIL"),
            None => (ui.visuals().text_color(), "no limit"),
        };
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Tj {:.1} °C", r.tj_c))
                    .strong()
                    .size(16.0),
            );
            ui.label(RichText::new(verdict).color(col).strong());
            if let Some(m) = r.margin_k {
                ui.label(format!("margin {m:+.1} K"));
            }
        });
        ui.separator();
    }
    let pkg = lib.package(&c.package).ok().cloned();
    grid(ui, "comp", |ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut c.name);
        ui.end_row();
        ui.label("Package");
        package_combo(ui, &format!("pkg{idx}"), &mut c.package, lib);
        ui.end_row();
        ui.label("Power");
        num_min(ui, &mut c.power_w, 0.01, 0.0, " W");
        ui.end_row();
        ui.label("Position (min corner)");
        vec3(ui, &mut c.position, " mm");
        ui.end_row();
        ui.label("Mounted on (normal)");
        axis_combo(ui, &format!("cax{idx}"), &mut c.mount_axis);
        ui.end_row();
        ui.label("Body size L×W×H");
        let nominal = pkg.as_ref().and_then(|p| p.size_mm);
        ui.vertical(|ui| {
            let mut custom = c.size.is_some();
            if ui
                .checkbox(&mut custom, "custom size")
                .on_hover_text("H is measured along the mounting normal")
                .changed()
            {
                c.size = if custom {
                    Some(nominal.unwrap_or([10.0, 10.0, 2.0]))
                } else {
                    None
                };
            }
            match &mut c.size {
                Some(s) => {
                    ui.horizontal(|ui| {
                        for (k, l) in ["L", "W", "H"].iter().enumerate() {
                            ui.label(RichText::new(*l).weak());
                            ui.add(
                                DragValue::new(&mut s[k])
                                    .speed(0.05)
                                    .range(0.01..=1000.0)
                                    .suffix(" mm"),
                            );
                        }
                    });
                }
                None => {
                    ui.label(
                        RichText::new(match nominal {
                            Some(n) => format!("package nominal {} × {} × {} mm", n[0], n[1], n[2]),
                            None => "package has no nominal size: set a custom size".into(),
                        })
                        .weak()
                        .italics(),
                    );
                }
            }
        });
        ui.end_row();

        let def_jc = pkg.as_ref().and_then(|p| p.r_jc);
        let def_jb = pkg.as_ref().and_then(|p| p.r_jb);
        ui.label("θJC (junction-case)");
        opt_num(
            ui,
            &mut c.r_jc,
            def_jc.unwrap_or(10.0),
            0.05,
            " K/W",
            &match def_jc {
                Some(r) => format!("package default {r} K/W"),
                None => "not set".into(),
            },
        );
        ui.end_row();
        ui.label("θJB (junction-board)");
        opt_num(
            ui,
            &mut c.r_jb,
            def_jb.unwrap_or(20.0),
            0.05,
            " K/W",
            &match def_jb {
                Some(r) => format!("package default {r} K/W"),
                None => "not modelled (face adiabatic)".into(),
            },
        );
        ui.end_row();
        ui.label("Case face");
        let def_face = pkg
            .as_ref()
            .map(|p| p.case_face)
            .unwrap_or(CaseFace::Bottom);
        let face_name = |f: CaseFace| match f {
            CaseFace::Top => "top",
            CaseFace::Bottom => "bottom",
        };
        ComboBox::from_id_salt(format!("cf{idx}"))
            .selected_text(match c.case_face {
                Some(f) => face_name(f).to_string(),
                None => format!("package default ({})", face_name(def_face)),
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut c.case_face,
                    None,
                    format!("package default ({})", face_name(def_face)),
                );
                ui.selectable_value(&mut c.case_face, Some(CaseFace::Top), "top");
                ui.selectable_value(&mut c.case_face, Some(CaseFace::Bottom), "bottom");
            })
            .response
            .on_hover_text(
                "The face connected through θJC; the opposite face uses θJB. \
                 'bottom' = towards the board / along -normal.",
            );
        ui.end_row();

        ui.label("Tj limit");
        let current = if let Some(t) = c.tj_max_c {
            format!("custom {t:.1} °C")
        } else if let Some(d) = &c.derating {
            lib.derating(d)
                .map(|x| format!("{} °C · {}", x.tj_max_c, x.name))
                .unwrap_or_else(|_| format!("unknown: {d}"))
        } else {
            "none".into()
        };
        ComboBox::from_id_salt(format!("lim{idx}"))
            .width(260.0)
            .truncate()
            .selected_text(current)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(c.tj_max_c.is_none() && c.derating.is_none(), "none")
                    .clicked()
                {
                    c.tj_max_c = None;
                    c.derating = None;
                }
                for d in &lib.derating {
                    let sel = c.tj_max_c.is_none() && c.derating.as_deref() == Some(&d.id);
                    if ui
                        .selectable_label(sel, format!("{} °C · {}", d.tj_max_c, d.name))
                        .on_hover_text(&d.source)
                        .clicked()
                    {
                        c.tj_max_c = None;
                        c.derating = Some(d.id.clone());
                    }
                }
                if ui
                    .selectable_label(c.tj_max_c.is_some(), "custom value…")
                    .clicked()
                    && c.tj_max_c.is_none()
                {
                    c.tj_max_c = Some(125.0);
                }
            });
        ui.end_row();
        if let Some(t) = &mut c.tj_max_c {
            ui.label("");
            num(ui, t, 0.5, " °C");
            ui.end_row();
        }
    });
    if c.r_jc.is_none() && pkg.as_ref().and_then(|p| p.r_jc).is_none() && c.r_jb.is_none() {
        ui.colored_label(
            Color32::from_rgb(230, 170, 60),
            "Set θJC (and ideally θJB) from the device datasheet.",
        );
    }
    if let Some(p) = &pkg {
        ui.add_space(6.0);
        ui.label(RichText::new(format!("{} · {}", p.family, p.standard)).weak());
        if !p.notes.is_empty() {
            ui.label(RichText::new(&p.notes).weak().italics());
        }
    }
}

fn face_label(f: Face) -> &'static str {
    match f {
        Face::Xmin => "x min",
        Face::Xmax => "x max",
        Face::Ymin => "y min",
        Face::Ymax => "y max",
        Face::Zmin => "z min (bottom)",
        Face::Zmax => "z max (top)",
    }
}

pub fn boundary(
    ui: &mut Ui,
    b: &mut Boundary,
    lib: &Library,
    idx: usize,
    ambient: Option<f64>,
    heat_out: Option<f64>,
) {
    let title = if b.name.is_empty() {
        face_label(b.face).to_string()
    } else {
        b.name.clone()
    };
    heading(ui, "BOUNDARY", &title);
    if let Some(q) = heat_out {
        ui.label(RichText::new(format!("Heat out: {q:.3} W")).strong());
        ui.separator();
    }
    grid(ui, "bnd", |ui| {
        ui.label("Name");
        ui.text_edit_singleline(&mut b.name);
        ui.end_row();
        ui.label("Face");
        ComboBox::from_id_salt(format!("bf{idx}"))
            .selected_text(face_label(b.face))
            .show_ui(ui, |ui| {
                for f in Face::ALL {
                    ui.selectable_value(&mut b.face, f, face_label(f));
                }
            });
        ui.end_row();
        ui.label("Type");
        let kname = |k: BoundaryKind| match k {
            BoundaryKind::Adiabatic => "Adiabatic",
            BoundaryKind::Fixed => "Fixed temperature",
            BoundaryKind::Convection => "Convection (+ radiation)",
            BoundaryKind::Radiation => "Radiation only",
        };
        ComboBox::from_id_salt(format!("bk{idx}"))
            .width(200.0)
            .truncate()
            .selected_text(kname(b.kind))
            .show_ui(ui, |ui| {
                for k in [
                    BoundaryKind::Convection,
                    BoundaryKind::Radiation,
                    BoundaryKind::Fixed,
                    BoundaryKind::Adiabatic,
                ] {
                    ui.selectable_value(&mut b.kind, k, kname(k));
                }
            });
        ui.end_row();
        let amb_text = match ambient {
            Some(a) => format!("environment ambient ({a:.1} °C)"),
            None => "no environment: set a value".into(),
        };
        match b.kind {
            BoundaryKind::Fixed => {
                ui.label("Temperature");
                let mut t = b.t_c.unwrap_or(ambient.unwrap_or(25.0));
                if num(ui, &mut t, 0.5, " °C").changed() || b.t_c.is_none() {
                    b.t_c = Some(t);
                }
                ui.end_row();
            }
            BoundaryKind::Convection => {
                ui.label("h");
                let mut h = b.h.unwrap_or(10.0);
                if num_min(ui, &mut h, 0.1, 0.0, " W/(m²·K)")
                    .on_hover_text(
                        "Typical: natural convection 3–10, forced air 20–100, \
                         liquid cold plate 500–5000",
                    )
                    .changed()
                    || b.h.is_none()
                {
                    b.h = Some(h);
                }
                ui.end_row();
                ui.label("Fluid temperature");
                opt_num(
                    ui,
                    &mut b.t_ambient_c,
                    ambient.unwrap_or(25.0),
                    0.5,
                    " °C",
                    &amb_text,
                );
                ui.end_row();
            }
            _ => {}
        }
        if matches!(b.kind, BoundaryKind::Convection | BoundaryKind::Radiation) {
            ui.label("Emissivity");
            #[derive(PartialEq, Clone, Copy)]
            enum Src {
                None,
                Value,
                Finish,
            }
            let cur = if b.emissivity.is_some() {
                Src::Value
            } else if b.finish.is_some() {
                Src::Finish
            } else {
                Src::None
            };
            let mut sel = cur;
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if b.kind == BoundaryKind::Convection {
                        ui.selectable_value(&mut sel, Src::None, "none");
                    }
                    ui.selectable_value(&mut sel, Src::Finish, "finish");
                    ui.selectable_value(&mut sel, Src::Value, "value");
                });
                if sel != cur {
                    match sel {
                        Src::None => {
                            b.emissivity = None;
                            b.finish = None;
                        }
                        Src::Value => {
                            let e = b
                                .finish
                                .as_ref()
                                .and_then(|f| lib.finish(f).ok())
                                .map(|f| f.emissivity)
                                .unwrap_or(0.85);
                            b.emissivity = Some(e);
                            b.finish = None;
                        }
                        Src::Finish => {
                            b.emissivity = None;
                            b.finish = Some(
                                lib.finishes
                                    .first()
                                    .map(|f| f.id.clone())
                                    .unwrap_or_default(),
                            );
                        }
                    }
                }
                if b.kind == BoundaryKind::Radiation && b.emissivity.is_none() && b.finish.is_none()
                {
                    b.emissivity = Some(0.85);
                }
                if let Some(e) = &mut b.emissivity {
                    ui.add(DragValue::new(e).speed(0.01).range(0.0..=1.0));
                }
                if let Some(f) = &mut b.finish {
                    finish_combo(ui, &format!("bfin{idx}"), f, lib);
                }
            });
            ui.end_row();
            if b.emissivity.is_some() || b.finish.is_some() {
                ui.label("Radiation sink");
                let fallback = if b.kind == BoundaryKind::Convection {
                    "same as fluid temperature"
                } else {
                    &amb_text
                };
                opt_num(
                    ui,
                    &mut b.t_radiation_c,
                    ambient.unwrap_or(25.0),
                    0.5,
                    " °C",
                    fallback,
                );
                ui.end_row();
            }
        }
        ui.label("Region");
        let t = b.face.tangential();
        ui.vertical(|ui| {
            let mut on = b.region.is_some();
            if ui.checkbox(&mut on, "limit to a rectangle").changed() {
                b.region = if on {
                    Some(Region2 {
                        min: [0.0, 0.0],
                        max: [10.0, 10.0],
                    })
                } else {
                    None
                };
            }
            if let Some(r) = &mut b.region {
                for (label, v) in [("min", &mut r.min), ("max", &mut r.max)] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(label).weak());
                        for q in 0..2 {
                            ui.label(RichText::new(AXES[t[q]]).weak());
                            num(ui, &mut v[q], 0.1, " mm");
                        }
                    });
                }
            }
        });
        ui.end_row();
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Faces without a boundary are adiabatic. Where boundaries overlap, the last \
             one in the list wins (an adiabatic region can cut a hole in a convection face).",
        )
        .weak(),
    );
}
