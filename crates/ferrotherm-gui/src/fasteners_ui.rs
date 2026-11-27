//! Window that cleans a STEP assembly: finds the fasteners, lets the user
//! pick any other part to remove, optionally closes the holes the fasteners
//! went through, and saves the same STEP file without the removed parts.

use eframe::egui::{self, Color32, RichText};
use ferrotherm_io::fasteners::{self, Filled, Kind, Removal, Scan};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

type SaveResult = Result<(PathBuf, Removal, Vec<Filled>), String>;

enum Job {
    Scanning(Receiver<Result<(Arc<String>, Scan), String>>),
    Saving(Receiver<SaveResult>),
}

/// What the app should do after a frame of the window.
pub enum Action {
    Close,
    /// Import this (cleaned) file.
    Import(PathBuf),
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Fasteners,
    AllParts,
}

pub struct FastenerTool {
    pub path: PathBuf,
    text: Option<Arc<String>>,
    scan: Option<Scan>,
    /// Part definitions to remove.
    selected: HashSet<u64>,
    job: Option<Job>,
    status: Option<(bool, String)>,
    filled: Vec<Filled>,
    filter: String,
    tab: Tab,
    fill_holes: bool,
    max_hole_mm: f64,
    small_mm: f64,
    import_after: bool,
    saved: Option<PathBuf>,
    /// The saved file was already handed over for import.
    handed: bool,
}

fn size_text(s: Option<[f64; 3]>) -> String {
    s.map(|s| format!("{:.1} × {:.1} × {:.1}", s[0], s[1], s[2]))
        .unwrap_or_else(|| "—".into())
}

impl FastenerTool {
    /// Start scanning a STEP file in the background.
    pub fn open(path: PathBuf, ctx: &egui::Context) -> Self {
        let (tx, rx) = channel();
        let p = path.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let r = std::fs::read_to_string(&p)
                .map_err(|e| format!("{}: {e}", p.display()))
                .and_then(|t| {
                    let s = fasteners::scan(&t)?;
                    Ok((Arc::new(t), s))
                });
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        FastenerTool {
            path,
            text: None,
            scan: None,
            selected: HashSet::new(),
            job: Some(Job::Scanning(rx)),
            status: None,
            filled: Vec::new(),
            filter: String::new(),
            tab: Tab::Fasteners,
            fill_holes: true,
            max_hole_mm: 12.0,
            small_mm: 3.0,
            import_after: true,
            saved: None,
            handed: false,
        }
    }

    fn poll(&mut self) {
        match &self.job {
            Some(Job::Scanning(rx)) => {
                if let Ok(r) = rx.try_recv() {
                    self.job = None;
                    match r {
                        Ok((t, s)) => {
                            self.selected = s
                                .candidates
                                .iter()
                                .filter(|c| c.suggested)
                                .map(|c| c.id)
                                .collect();
                            self.text = Some(t);
                            self.scan = Some(s);
                        }
                        Err(e) => self.status = Some((false, e)),
                    }
                }
            }
            Some(Job::Saving(rx)) => {
                if let Ok(r) = rx.try_recv() {
                    self.job = None;
                    match r {
                        Ok((p, rep, filled)) => {
                            let holes: usize = filled.iter().map(|f| f.holes).sum();
                            let parts = filled.iter().filter(|f| f.holes > 0).count();
                            self.status = Some((
                                true,
                                format!(
                                    "Saved {}: {} parts removed ({} instances){}; everything else is unchanged.",
                                    p.display(),
                                    rep.parts.len(),
                                    rep.occurrences,
                                    if self.fill_holes {
                                        format!(", {holes} holes closed in {parts} parts")
                                    } else {
                                        String::new()
                                    }
                                ),
                            ));
                            self.filled = filled;
                            self.saved = Some(p);
                            self.handed = false;
                        }
                        Err(e) => self.status = Some((false, e)),
                    }
                }
            }
            None => {}
        }
    }

    fn save(&mut self, ctx: &egui::Context) {
        let (Some(text), Some(scan)) = (self.text.clone(), self.scan.as_ref()) else {
            return;
        };
        let ids: Vec<u64> = self.selected.iter().copied().collect();
        // holes are closed for the removed fasteners, not for other parts
        let fill: Vec<u64> = scan
            .candidates
            .iter()
            .filter(|c| self.selected.contains(&c.id))
            .map(|c| c.id)
            .collect();
        let stem = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "model".into());
        let ext = self
            .path
            .extension()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "stp".into());
        let mut d = rfd::FileDialog::new()
            .set_file_name(format!("{stem}_clean.{ext}"))
            .add_filter("STEP", &["stp", "step"]);
        if let Some(dir) = self.path.parent() {
            d = d.set_directory(dir);
        }
        let Some(out) = d.save_file() else { return };
        if out == self.path {
            self.status = Some((
                false,
                "Choose a new file: the original is kept as it is.".into(),
            ));
            return;
        }
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        let (fill_holes, max_hole) = (self.fill_holes, self.max_hole_mm);
        std::thread::spawn(move || {
            let r = if fill_holes && !fill.is_empty() {
                fasteners::remove_and_fill(&text, &ids, &fill, max_hole)
            } else {
                fasteners::remove(&text, &ids).map(|(t, r)| (t, r, Vec::new()))
            }
            .and_then(|(clean, rep, filled)| {
                std::fs::write(&out, clean).map_err(|e| format!("{}: {e}", out.display()))?;
                Ok((out, rep, filled))
            });
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        self.job = Some(Job::Saving(rx));
        self.status = None;
        self.filled.clear();
    }

    fn fasteners_tab(&mut self, ui: &mut egui::Ui, scan: &Scan) {
        ui.horizontal(|ui| {
            if ui.button("Suggested").clicked() {
                for c in &scan.candidates {
                    if c.suggested {
                        self.selected.insert(c.id);
                    } else {
                        self.selected.remove(&c.id);
                    }
                }
            }
            if ui.button("All").clicked() {
                self.selected.extend(scan.candidates.iter().map(|c| c.id));
            }
            if ui.button("None").clicked() {
                for c in &scan.candidates {
                    self.selected.remove(&c.id);
                }
            }
        });
        let f = self.filter.to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("fast-scroll")
            .max_height(ui.available_height() - 120.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("fast-grid")
                    .striped(true)
                    .num_columns(7)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for h in [
                            "",
                            "Kind",
                            "Confidence",
                            "Instances",
                            "Size (mm)",
                            "Name",
                            "Why",
                        ] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        for c in &scan.candidates {
                            if !f.is_empty() && !c.name.to_lowercase().contains(&f) {
                                continue;
                            }
                            let mut on = self.selected.contains(&c.id);
                            if ui.checkbox(&mut on, "").changed() {
                                if on {
                                    self.selected.insert(c.id);
                                } else {
                                    self.selected.remove(&c.id);
                                }
                            }
                            let col = match c.kind {
                                Kind::Screw => Color32::from_rgb(120, 170, 255),
                                Kind::Washer => Color32::from_rgb(140, 210, 140),
                                Kind::Nut => Color32::from_rgb(230, 190, 90),
                                Kind::Standoff => Color32::from_rgb(200, 140, 220),
                                _ => Color32::from_rgb(200, 200, 200),
                            };
                            ui.colored_label(col, c.kind.label());
                            let pct = (c.score * 100.0).round();
                            let src = if c.by_name { "name" } else { "shape" };
                            ui.add(
                                egui::ProgressBar::new(c.score as f32)
                                    .desired_width(110.0)
                                    .text(format!("{pct:.0}%  {src}")),
                            );
                            ui.label(format!("×{}", c.instances));
                            ui.label(size_text(c.size_mm));
                            ui.label(&c.name);
                            let why = c.reasons.join("; ");
                            ui.add(egui::Label::new(RichText::new(&why).small().weak()).truncate())
                                .on_hover_text(&why);
                            ui.end_row();
                        }
                    });
                if scan.candidates.is_empty() {
                    ui.label("No fasteners found. Use the All parts tab to pick parts by hand.");
                }
            });
    }

    fn all_parts_tab(&mut self, ui: &mut egui::Ui, scan: &Scan) {
        ui.horizontal(|ui| {
            ui.label("Select parts smaller than");
            ui.add(
                egui::DragValue::new(&mut self.small_mm)
                    .range(0.1..=500.0)
                    .speed(0.1)
                    .suffix(" mm"),
            );
            if ui
                .button("Select")
                .on_hover_text("Ticks every part whose largest side is below this size")
                .clicked()
            {
                for p in &scan.all_parts {
                    if p.size_mm.is_some_and(|s| s[2] < self.small_mm) {
                        self.selected.insert(p.id);
                    }
                }
            }
            if ui.button("Clear all").clicked() {
                self.selected.clear();
            }
        });
        let f = self.filter.to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("parts-scroll")
            .max_height(ui.available_height() - 120.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("parts-grid")
                    .striped(true)
                    .num_columns(5)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for h in ["", "Name", "In", "Instances", "Size (mm)"] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        for p in &scan.all_parts {
                            if !f.is_empty()
                                && !p.name.to_lowercase().contains(&f)
                                && !p.parent.to_lowercase().contains(&f)
                            {
                                continue;
                            }
                            let mut on = self.selected.contains(&p.id);
                            if ui.checkbox(&mut on, "").changed() {
                                if on {
                                    self.selected.insert(p.id);
                                } else {
                                    self.selected.remove(&p.id);
                                }
                            }
                            let name = if p.assembly {
                                RichText::new(format!("{} (assembly)", p.name)).strong()
                            } else {
                                RichText::new(&p.name)
                            };
                            ui.label(name).on_hover_text(if p.assembly {
                                "Sub-assembly: removing it removes the parts used only inside it"
                            } else {
                                "Part"
                            });
                            ui.label(RichText::new(&p.parent).weak());
                            ui.label(format!("×{}", p.instances));
                            ui.label(size_text(p.size_mm));
                            ui.end_row();
                        }
                    });
            });
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<Action> {
        self.poll();
        let mut action = None;
        let mut open = true;
        let title = format!(
            "Clean STEP — {}",
            self.path
                .file_name()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        );
        egui::Window::new(title)
            .id(egui::Id::new("fastener-tool"))
            .open(&mut open)
            .default_size([900.0, 560.0])
            .resizable(true)
            .show(ctx, |ui| {
                if let Some(Job::Scanning(_)) = &self.job {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Reading the assembly and classifying the parts…");
                    });
                    return;
                }
                let Some(scan) = self.scan.take() else {
                    if let Some((_, e)) = &self.status {
                        ui.colored_label(Color32::from_rgb(230, 90, 80), e);
                    }
                    return;
                };
                ui.label(format!(
                    "{} parts, {} assemblies, unit {} mm. Ticked parts are removed; the rest of the file \
                     is copied unchanged.",
                    scan.parts, scan.assemblies, scan.unit_mm
                ));
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.tab,
                        Tab::Fasteners,
                        format!("Fasteners ({})", scan.candidates.len()),
                    );
                    ui.selectable_value(
                        &mut self.tab,
                        Tab::AllParts,
                        format!("All parts ({})", scan.all_parts.len()),
                    );
                    ui.separator();
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("filter by name")
                            .desired_width(180.0),
                    );
                    let inst: usize = scan
                        .all_parts
                        .iter()
                        .filter(|p| self.selected.contains(&p.id))
                        .map(|p| p.instances)
                        .sum();
                    ui.label(
                        RichText::new(format!("{} parts, {inst} instances selected", self.selected.len()))
                            .strong(),
                    );
                });
                ui.separator();
                match self.tab {
                    Tab::Fasteners => self.fasteners_tab(ui, &scan),
                    Tab::AllParts => self.all_parts_tab(ui, &scan),
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.fill_holes, "Close the holes of the removed fasteners")
                        .on_hover_text(
                            "The holes the removed screws, washers and nuts went through are filled \
                             with the solid of the part they are in (same material, same thickness \
                             and outline). Holes OpenCascade cannot rebuild stay open and are listed. \
                             Can take a few minutes on large assemblies.",
                        );
                    ui.add_enabled(
                        self.fill_holes,
                        egui::DragValue::new(&mut self.max_hole_mm)
                            .range(1.0..=50.0)
                            .speed(0.1)
                            .prefix("up to Ø ")
                            .suffix(" mm"),
                    );
                });
                ui.horizontal(|ui| {
                    let busy = self.job.is_some();
                    if busy {
                        ui.spinner();
                        ui.label(if self.fill_holes { "Removing and closing holes…" } else { "Removing…" });
                    }
                    let any = !self.selected.is_empty();
                    if ui
                        .add_enabled(!busy && any, egui::Button::new("Save STEP without the selected parts…"))
                        .on_hover_text(
                            "Writes a new STEP file: the same header, units, names, colours, PCBs \
                             and components, without the ticked parts. The original is not changed.",
                        )
                        .clicked()
                    {
                        self.save(ctx);
                    }
                    ui.checkbox(&mut self.import_after, "then import it");
                    if let Some(p) = &self.saved {
                        if ui.button("Import the saved file").clicked() {
                            action = Some(Action::Import(p.clone()));
                        }
                    }
                });
                if let Some((ok, msg)) = &self.status {
                    let col = if *ok {
                        Color32::from_rgb(120, 200, 120)
                    } else {
                        Color32::from_rgb(230, 90, 80)
                    };
                    ui.colored_label(col, msg);
                }
                if !self.filled.is_empty() {
                    egui::CollapsingHeader::new("Holes closed, per part")
                        .id_salt("filled-list")
                        .show(ui, |ui| {
                            for f in &self.filled {
                                let mut t = format!(
                                    "{}: {} closed (+{:.1} mm³)",
                                    f.part, f.holes, f.volume_added_mm3
                                );
                                if f.skipped > 0 {
                                    t += &format!(", {} left open", f.skipped);
                                }
                                if !f.note.is_empty() {
                                    t += &format!(" — {}", f.note);
                                }
                                ui.label(RichText::new(t).small());
                            }
                        });
                }
                self.scan = Some(scan);
            });
        // import right after a successful save, if asked
        if self.import_after && !self.handed && action.is_none() {
            if let Some(p) = &self.saved {
                action = Some(Action::Import(p.clone()));
                self.handed = true;
            }
        }
        if !open {
            return Some(Action::Close);
        }
        action
    }
}
