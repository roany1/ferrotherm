// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! CAD import dialog (STEP, STL, IDF, KiCad, Gerber, IPC-2581, ODB++) and
//! geometry export.

use crate::inspector::{material_combo, num};
use eframe::egui::{
    self, Color32, ComboBox, DragValue, Grid, Pos2, RichText, Sense, Stroke, Ui, Vec2,
};
use ferrotherm_core::library::Library;
use ferrotherm_io::ecad::{self, Board, BoardSummary, ComponentMode, Fragment, ToCaseOptions};
use ferrotherm_io::import::{self, Imported, McadOptions, Source};
use ferrotherm_io::step::{BlockMode, StepModel};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

const WARN: Color32 = Color32::from_rgb(230, 170, 60);
const ERR: Color32 = Color32::from_rgb(235, 85, 85);

enum State {
    Loading(Receiver<Result<(Source, Imported), String>>),
    Board(Source, Box<Board>, BoardSummary),
    Mcad(Source, Box<StepModel>),
    Project(Box<ferrotherm_io::celsius::CelsiusModel>),
    Failed(String),
}

pub enum Action {
    /// Add to the case; `bool` = grow the domain to fit.
    Add(Fragment, bool),
    /// Replace the current model with this one.
    Open(Box<ferrotherm_core::Case>),
    Close,
}

pub struct ImportDialog {
    pub path: PathBuf,
    state: State,
    first_occt: bool,
    // ECAD options
    tile_mm: f64,
    origin: [f64; 3],
    components: ComponentMode,
    min_area: f64,
    powered_only: bool,
    default_height: Option<f64>,
    power_note: Option<String>,
    // MCAD options
    voxel: bool,
    voxel_mm: f64,
    material: String,
    offset: [f64; 3],
    min_volume: f64,
    detect_pcb: bool,
    // common
    prefix: String,
    fit: bool,
}

pub fn file_filters(d: rfd::FileDialog) -> rfd::FileDialog {
    let mut d = d;
    for (name, ext) in import::FILTERS {
        d = d.add_filter(*name, ext);
    }
    d
}

/// True for a path the import dialog can read.
pub fn is_cad(p: &Path) -> bool {
    if p.is_dir() {
        return true;
    }
    let e = p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    import::FILTERS[0].1.contains(&e.as_str())
        || matches!(e.as_str(), "emp" | "drl" | "gts" | "gbs" | "gto" | "gbo")
}

impl ImportDialog {
    pub fn open(path: PathBuf, ctx: &egui::Context) -> ImportDialog {
        let (tx, rx) = channel();
        let p = path.clone();
        let ctx2 = ctx.clone();
        let lower = p.to_string_lossy().to_ascii_lowercase();
        let first_occt =
            (lower.ends_with(".step") || lower.ends_with(".stp") || lower.ends_with(".p21"))
                && !ferrotherm_io::occt::is_warm();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(|| import::import_path(&p, 0.1))
                .unwrap_or_else(|_| Err("the importer crashed on this file".into()));
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        ImportDialog {
            path,
            state: State::Loading(rx),
            first_occt,
            tile_mm: 10.0,
            origin: [0.0; 3],
            components: ComponentMode::Compact,
            min_area: 4.0,
            powered_only: false,
            default_height: None,
            power_note: None,
            voxel: false,
            voxel_mm: 1.0,
            material: "al-6061-t6".into(),
            offset: [0.0; 3],
            min_volume: 0.0,
            detect_pcb: true,
            prefix: String::new(),
            fit: true,
        }
    }

    fn poll(&mut self) {
        if let State::Loading(rx) = &self.state {
            if let Ok(r) = rx.try_recv() {
                self.state = match r {
                    Ok((src, Imported::Board(mut b))) => {
                        let s = b.summary();
                        State::Board(src, Box::new(b), s)
                    }
                    Ok((src, Imported::Mcad(m))) => State::Mcad(src, Box::new(m)),
                    Ok((_, Imported::Project(m))) => {
                        if let BlockMode::Voxels(v) = m.default_cad_mode() {
                            self.voxel = true;
                            self.voxel_mm = v;
                        }
                        State::Project(m)
                    }
                    Err(e) => State::Failed(e),
                };
            }
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        lib: &Library,
        domain_min: [f64; 3],
    ) -> Option<Action> {
        self.poll();
        let mut action = None;
        let mut open = true;
        let title = format!(
            "Import — {}",
            self.path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        );
        egui::Window::new(title)
            .id(egui::Id::new("import_dialog"))
            .open(&mut open)
            .default_width(560.0)
            .collapsible(false)
            .show(ctx, |ui| {
                action = self.body(ui, lib, domain_min);
            });
        if !open {
            return Some(Action::Close);
        }
        action
    }

    fn body(&mut self, ui: &mut Ui, lib: &Library, domain_min: [f64; 3]) -> Option<Action> {
        match &mut self.state {
            State::Loading(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading the file…");
                });
                if self.first_occt {
                    ui.label(
                        RichText::new(
                            "First STEP import: preparing the OpenCascade kernel (about 30 s, only once; \
                             it is cached for the next runs).",
                        )
                        .weak(),
                    );
                }
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(200));
                None
            }
            State::Failed(e) => {
                ui.colored_label(ERR, e.as_str());
                ui.label(
                    RichText::new(
                        "Supported: STEP (.step/.stp), STL, IDF 3.0 (.emn + .emp), KiCad (.kicad_pcb), \
                         Gerber X2/RS-274X + Excellon (folder, .zip or .gbrjob), IPC-2581 (.xml/.cvg), \
                         ODB++ (folder or .tgz/.zip).",
                    )
                    .weak(),
                );
                if ui.button("Close").clicked() {
                    return Some(Action::Close);
                }
                None
            }
            State::Board(..) => self.board_ui(ui, domain_min),
            State::Mcad(..) => self.mcad_ui(ui, lib, domain_min),
            State::Project(..) => self.project_ui(ui),
        }
    }

    fn board_ui(&mut self, ui: &mut Ui, domain_min: [f64; 3]) -> Option<Action> {
        let State::Board(src, board, s) = &mut self.state else {
            return None;
        };
        ui.label(
            RichText::new(format!(
                "{} board{}",
                src.label(),
                if s.name.is_empty() {
                    String::new()
                } else {
                    format!(" '{}'", s.name)
                }
            ))
            .strong(),
        );
        ui.label(format!(
            "{:.1} × {:.1} mm · {:.2} mm thick · {} parts · {} drills",
            s.size_mm[0], s.size_mm[1], s.thickness_mm, s.components, s.drills
        ));
        if !s.copper_layers.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for (n, t, c) in &s.copper_layers {
                    let th = if t.is_finite() {
                        format!("{:.0} µm", t * 1000.0)
                    } else {
                        "? µm".into()
                    };
                    let cv = if c.is_finite() {
                        format!("{:.0} %", c * 100.0)
                    } else {
                        "no data".into()
                    };
                    ui.label(RichText::new(format!("{n}: {th}, {cv}")).small());
                    ui.separator();
                }
            });
        }
        for w in s.warnings.iter().take(6) {
            ui.colored_label(WARN, RichText::new(w).small());
        }
        board_preview(ui, board);
        ui.separator();
        let board: &mut Board = board;
        let mut load_power = false;
        Grid::new("imp_board").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Board corner at");
            ui.horizontal(|ui| {
                for k in 0..3 {
                    num(ui, &mut self.origin[k], 0.5, " mm");
                }
                if ui.small_button("domain min").clicked() {
                    self.origin = domain_min;
                }
            });
            ui.end_row();
            ui.label("PCB map tile");
            ui.add(DragValue::new(&mut self.tile_mm).speed(0.5).range(0.0..=500.0).suffix(" mm"))
                .on_hover_text("Each tile gets its own copper coverage per layer and via fraction (0 = one block)");
            ui.end_row();
            ui.label("Components");
            ComboBox::from_id_salt("imp_cm")
                .selected_text(match self.components {
                    ComponentMode::Compact => "2R compact (when θJC known), else blocks",
                    ComponentMode::Blocks => "Solid blocks",
                    ComponentMode::None => "None",
                })
                .width(280.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.components, ComponentMode::Compact, "2R compact (when θJC known), else blocks");
                    ui.selectable_value(&mut self.components, ComponentMode::Blocks, "Solid blocks");
                    ui.selectable_value(&mut self.components, ComponentMode::None, "None");
                });
            ui.end_row();
            ui.label("Skip parts smaller than");
            ui.add(DragValue::new(&mut self.min_area).speed(0.5).range(0.0..=10_000.0).suffix(" mm²"));
            ui.end_row();
            ui.label("Only powered parts");
            ui.checkbox(&mut self.powered_only, "");
            ui.end_row();
            ui.label("Height when unknown");
            ui.horizontal(|ui| {
                let mut on = self.default_height.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    self.default_height = on.then_some(2.0);
                }
                match &mut self.default_height {
                    Some(h) => {
                        ui.add(DragValue::new(h).speed(0.05).range(0.05..=100.0).suffix(" mm"));
                    }
                    None => {
                        ui.label(RichText::new("guess from the package name").weak().italics());
                    }
                }
            });
            ui.end_row();
            ui.label("Power list");
            ui.horizontal(|ui| {
                if ui.button("Load CSV…").on_hover_text("refdes, power_W [, θJC, θJB, Tj max]").clicked() {
                    load_power = true;
                }
                if let Some(n) = &self.power_note {
                    ui.label(RichText::new(n).weak());
                }
            });
            ui.end_row();
            common(ui, &mut self.prefix, &mut self.fit);
        });
        if load_power {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("CSV", &["csv", "txt"])
                .pick_file()
            {
                self.power_note = Some(match std::fs::read_to_string(&p) {
                    Ok(t) => match ecad::apply_power_csv(board, &t) {
                        Ok(n) => format!("{n} parts with power"),
                        Err(e) => e,
                    },
                    Err(e) => e.to_string(),
                });
            }
        }
        let total: f64 = board.components.iter().filter_map(|c| c.power_w).sum();
        if total > 0.0 {
            ui.label(format!("Total power: {total:.3} W"));
        }
        ui.add_space(6.0);
        let mut out = None;
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Add to model").strong()).clicked() {
                let opt = ToCaseOptions {
                    tile_mm: self.tile_mm,
                    origin: self.origin,
                    components: self.components,
                    default_height_mm: self.default_height,
                    min_area_mm2: self.min_area,
                    powered_only: self.powered_only,
                    dielectric: "fr4".into(),
                    prefix: self.prefix.clone(),
                };
                out = Some(Action::Add(import::board_fragment(board, &opt), self.fit));
            }
            if ui.button("Cancel").clicked() {
                out = Some(Action::Close);
            }
        });
        out
    }

    fn project_ui(&mut self, ui: &mut Ui) -> Option<Action> {
        let State::Project(m) = &self.state else {
            return None;
        };
        let c = &m.case;
        ui.label(RichText::new(m.summary()).strong());
        ui.label(format!(
            "Domain {:.1} × {:.1} × {:.1} mm · {} blocks · {} components · {} materials · air flow {}",
            c.domain.max[0] - c.domain.min[0],
            c.domain.max[1] - c.domain.min[1],
            c.domain.max[2] - c.domain.min[2],
            c.blocks.len(),
            c.components.len(),
            c.materials.len(),
            if c.flow.is_some() { "on" } else { "off" }
        ));
        let p: f64 = c.blocks.iter().map(|b| b.power_w).sum::<f64>()
            + c.components.iter().map(|x| x.power_w).sum::<f64>()
            + m.cad.iter().map(|x| x.power_w).sum::<f64>();
        ui.label(format!("Total power {p:.3} W"));
        egui::ScrollArea::vertical()
            .max_height(140.0)
            .show(ui, |ui| {
                for w in &m.warnings {
                    ui.colored_label(WARN, RichText::new(w).small());
                }
            });
        if !m.cad.is_empty() {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(format!("{} CAD solids:", m.cad.len()));
                ui.selectable_value(&mut self.voxel, false, "Bounding box");
                ui.selectable_value(&mut self.voxel, true, "Voxelised");
                if self.voxel {
                    ui.add(
                        DragValue::new(&mut self.voxel_mm)
                            .speed(0.05)
                            .range(0.2..=50.0)
                            .suffix(" mm"),
                    )
                    .on_hover_text("Smaller voxels follow the shape better but make a finer grid");
                }
            });
        }
        ui.label(
            RichText::new(
                "Values come straight from the Celsius model (materials, θJC/θJB, power, environment). \
                 Compare a few positions with Celsius before trusting the import.",
            )
            .weak()
            .small(),
        );
        let mode = if self.voxel {
            BlockMode::Voxels(self.voxel_mm)
        } else {
            BlockMode::BoundingBox
        };
        let mut out = None;
        ui.horizontal(|ui| {
            if ui
                .button(RichText::new("Open as new model").strong())
                .on_hover_text(
                    "Replaces the current model (domain, boundaries, flow settings, objects)",
                )
                .clicked()
            {
                out = Some(Action::Open(Box::new(m.to_case(mode))));
            }
            if ui
                .button("Add to current model")
                .on_hover_text("Adds only the objects and materials")
                .clicked()
            {
                out = Some(Action::Add(
                    ferrotherm_io::import::project_fragment(m, mode, &self.prefix),
                    self.fit,
                ));
            }
            if ui.button("Cancel").clicked() {
                out = Some(Action::Close);
            }
        });
        out
    }

    fn mcad_ui(&mut self, ui: &mut Ui, lib: &Library, domain_min: [f64; 3]) -> Option<Action> {
        let State::Mcad(src, m) = &self.state else {
            return None;
        };
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for b in &m.bodies {
            for a in 0..3 {
                lo[a] = lo[a].min(b.min[a]);
                hi[a] = hi[a].max(b.max[a]);
            }
        }
        ui.label(
            RichText::new(format!("{} model: {} bodies", src.label(), m.bodies.len())).strong(),
        );
        if lo[0].is_finite() {
            ui.label(format!(
                "Extent [{:.1}, {:.1}, {:.1}] … [{:.1}, {:.1}, {:.1}] mm",
                lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]
            ));
        }
        for w in m.warnings.iter().take(6) {
            ui.colored_label(WARN, RichText::new(w).small());
        }
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .show(ui, |ui| {
                Grid::new("imp_bodies")
                    .striped(true)
                    .spacing([12.0, 2.0])
                    .show(ui, |ui| {
                        for h in ["Body", "Size [mm]", "Triangles"] {
                            ui.label(RichText::new(h).weak().small());
                        }
                        ui.end_row();
                        for b in m.bodies.iter().take(400) {
                            ui.label(RichText::new(&b.name).small())
                                .on_hover_text(&b.path);
                            ui.label(
                                RichText::new(format!(
                                    "{:.2} × {:.2} × {:.2}",
                                    b.max[0] - b.min[0],
                                    b.max[1] - b.min[1],
                                    b.max[2] - b.min[2]
                                ))
                                .small(),
                            );
                            ui.label(
                                RichText::new(if b.bbox_only {
                                    "box only".to_string()
                                } else {
                                    b.tris.len().to_string()
                                })
                                .small(),
                            );
                            ui.end_row();
                        }
                    });
            });
        ui.separator();
        Grid::new("imp_mcad")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label("Shape");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.voxel, false, "Bounding box per body");
                    ui.selectable_value(&mut self.voxel, true, "Voxelised shape");
                    if self.voxel {
                        ui.add(
                            DragValue::new(&mut self.voxel_mm)
                                .speed(0.05)
                                .range(0.05..=100.0)
                                .suffix(" mm"),
                        );
                    }
                });
                ui.end_row();
                ui.label("Material");
                material_combo(ui, "imp_mat", &mut self.material, lib);
                ui.end_row();
                ui.label("Move by");
                ui.horizontal(|ui| {
                    for k in 0..3 {
                        num(ui, &mut self.offset[k], 0.5, " mm");
                    }
                    if lo[0].is_finite() && ui.small_button("align min to domain min").clicked() {
                        self.offset = [0, 1, 2].map(|a| domain_min[a] - lo[a]);
                    }
                });
                ui.end_row();
                ui.label("Skip bodies below");
                ui.add(
                    DragValue::new(&mut self.min_volume)
                        .speed(1.0)
                        .range(0.0..=1e9)
                        .suffix(" mm³"),
                );
                ui.end_row();
                ui.label("Boards");
                ui.checkbox(
                    &mut self.detect_pcb,
                    "bodies named PCB/board become a PCB stack-up",
                );
                ui.end_row();
                common(ui, &mut self.prefix, &mut self.fit);
            });
        ui.label(
            RichText::new(
                "Blocks are axis-aligned: a voxel size of ~¼ of the smallest feature keeps the shape; \
                 import the ECAD data (IDF/KiCad/ODB++…) for boards and parts with power.",
            )
            .weak()
            .small(),
        );
        let mut out = None;
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Add to model").strong()).clicked() {
                let opt = McadOptions {
                    mode: if self.voxel {
                        BlockMode::Voxels(self.voxel_mm)
                    } else {
                        BlockMode::BoundingBox
                    },
                    material: self.material.clone(),
                    offset: self.offset,
                    prefix: self.prefix.clone(),
                    min_volume_mm3: self.min_volume,
                    detect_pcb: self.detect_pcb,
                };
                out = Some(Action::Add(import::mcad_to_fragment(m, &opt), self.fit));
            }
            if ui.button("Cancel").clicked() {
                out = Some(Action::Close);
            }
        });
        out
    }
}

fn common(ui: &mut Ui, prefix: &mut String, fit: &mut bool) {
    ui.label("Name prefix");
    ui.text_edit_singleline(prefix)
        .on_hover_text("e.g. 'cpu_board/' to group the imported objects");
    ui.end_row();
    ui.label("Domain");
    ui.checkbox(fit, "grow the domain to fit (10 mm margin)");
    ui.end_row();
}

/// Top view of the board outline and part footprints.
fn board_preview(ui: &mut Ui, b: &Board) {
    let Some((lo, hi)) = b.bbox() else { return };
    let w = (hi[0] - lo[0]).max(1e-6);
    let h = (hi[1] - lo[1]).max(1e-6);
    let width = ui.available_width().min(520.0);
    let height = (width * (h / w) as f32).clamp(60.0, 260.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let s = ((rect.width() - 8.0) / w as f32).min((rect.height() - 8.0) / h as f32);
    let ox = rect.center().x - 0.5 * w as f32 * s;
    let oy = rect.center().y + 0.5 * h as f32 * s;
    let to = |p: [f64; 2]| {
        Pos2::new(
            ox + (p[0] - lo[0]) as f32 * s,
            oy - (p[1] - lo[1]) as f32 * s,
        )
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_rgb(24, 40, 30));
    for poly in &b.outline {
        let pts: Vec<Pos2> = poly.iter().chain(poly.first()).map(|p| to(*p)).collect();
        painter.add(egui::Shape::line(
            pts,
            Stroke::new(1.5, Color32::from_rgb(120, 200, 140)),
        ));
    }
    for c in &b.components {
        let Some((a, z)) = c.footprint() else {
            continue;
        };
        let r = egui::Rect::from_two_pos(to(a), to(z));
        let col = if c.power_w.unwrap_or(0.0) > 0.0 {
            Color32::from_rgb(235, 120, 60)
        } else if c.side == ecad::Side::Top {
            Color32::from_rgb(190, 190, 200)
        } else {
            Color32::from_rgb(110, 130, 190)
        };
        painter.rect_stroke(r, 0.0, Stroke::new(1.0, col), egui::StrokeKind::Inside);
    }
    painter.text(
        rect.left_bottom() + egui::vec2(6.0, -4.0),
        egui::Align2::LEFT_BOTTOM,
        "top view · grey top side, blue bottom side, orange powered",
        egui::FontId::proportional(10.0),
        Color32::from_gray(170),
    );
}
