//! Window that finds the fasteners of a STEP assembly, lets the user review
//! the list, and saves the same STEP file without the selected parts.

use eframe::egui::{self, Color32, RichText};
use ferrotherm_io::fasteners::{self, Kind, Removal, Scan};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

enum Job {
    Scanning(Receiver<Result<(Arc<String>, Scan), String>>),
    Saving(Receiver<Result<(PathBuf, Removal), String>>),
}

/// What the app should do after a frame of the window.
pub enum Action {
    Close,
    /// Import this (cleaned) file.
    Import(PathBuf),
}

pub struct FastenerTool {
    pub path: PathBuf,
    text: Option<Arc<String>>,
    scan: Option<Scan>,
    selected: Vec<bool>,
    job: Option<Job>,
    status: Option<(bool, String)>,
    filter: String,
    import_after: bool,
    saved: Option<PathBuf>,
    /// The saved file was already handed over for import.
    handed: bool,
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
            selected: Vec::new(),
            job: Some(Job::Scanning(rx)),
            status: None,
            filter: String::new(),
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
                            self.selected = s.candidates.iter().map(|c| c.suggested).collect();
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
                        Ok((p, rep)) => {
                            self.status = Some((
                                true,
                                format!(
                                    "Saved {}: {} parts removed ({} instances), {} records \
                                     deleted, {} lists edited; everything else is unchanged.",
                                    p.display(),
                                    rep.parts.len(),
                                    rep.occurrences,
                                    rep.deleted_records,
                                    rep.edited_records
                                ),
                            ));
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
        let ids: Vec<u64> = scan
            .candidates
            .iter()
            .zip(&self.selected)
            .filter(|(_, s)| **s)
            .map(|(c, _)| c.id)
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
            .set_file_name(format!("{stem}_no_fasteners.{ext}"))
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
        std::thread::spawn(move || {
            let r = fasteners::remove(&text, &ids).and_then(|(clean, rep)| {
                std::fs::write(&out, clean).map_err(|e| format!("{}: {e}", out.display()))?;
                Ok((out, rep))
            });
            let _ = tx.send(r);
            ctx2.request_repaint();
        });
        self.job = Some(Job::Saving(rx));
        self.status = None;
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<Action> {
        self.poll();
        let mut action = None;
        let mut open = true;
        let title = format!(
            "Fasteners — {}",
            self.path
                .file_name()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        );
        egui::Window::new(title)
            .id(egui::Id::new("fastener-tool"))
            .open(&mut open)
            .default_size([880.0, 520.0])
            .resizable(true)
            .show(ctx, |ui| {
                if let Some(Job::Scanning(_)) = &self.job {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Reading the assembly and classifying the parts…");
                    });
                    return;
                }
                let Some(scan) = &self.scan else {
                    if let Some((_, e)) = &self.status {
                        ui.colored_label(Color32::from_rgb(230, 90, 80), e);
                    }
                    return;
                };
                ui.label(format!(
                    "{} parts, {} assemblies, unit {} mm. {} possible fasteners; the ticked ones \
                     are removed. Names decide a selection; shape alone only proposes.",
                    scan.parts,
                    scan.assemblies,
                    scan.unit_mm,
                    scan.candidates.len()
                ));
                ui.horizontal(|ui| {
                    if ui.button("Suggested").clicked() {
                        for (s, c) in self.selected.iter_mut().zip(&scan.candidates) {
                            *s = c.suggested;
                        }
                    }
                    if ui.button("All").clicked() {
                        self.selected.iter_mut().for_each(|s| *s = true);
                    }
                    if ui.button("None").clicked() {
                        self.selected.iter_mut().for_each(|s| *s = false);
                    }
                    ui.separator();
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("filter by name")
                            .desired_width(180.0),
                    );
                    let n = self.selected.iter().filter(|s| **s).count();
                    let inst: usize = scan
                        .candidates
                        .iter()
                        .zip(&self.selected)
                        .filter(|(_, s)| **s)
                        .map(|(c, _)| c.instances)
                        .sum();
                    ui.label(
                        RichText::new(format!("{n} parts, {inst} instances selected")).strong(),
                    );
                });
                ui.separator();
                let f = self.filter.to_lowercase();
                egui::ScrollArea::vertical()
                    .max_height(ui.available_height() - 90.0)
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
                                for (k, c) in scan.candidates.iter().enumerate() {
                                    if !f.is_empty() && !c.name.to_lowercase().contains(&f) {
                                        continue;
                                    }
                                    ui.checkbox(&mut self.selected[k], "");
                                    let col = match c.kind {
                                        Kind::Screw => Color32::from_rgb(120, 170, 255),
                                        Kind::Washer => Color32::from_rgb(140, 210, 140),
                                        Kind::Nut => Color32::from_rgb(230, 190, 90),
                                        Kind::Standoff => Color32::from_rgb(200, 140, 220),
                                        _ => Color32::from_rgb(200, 200, 200),
                                    };
                                    ui.colored_label(col, c.kind.label());
                                    let pct = (c.score * 100.0).round();
                                    let txt = if c.by_name {
                                        format!("{pct:.0}%  name")
                                    } else {
                                        format!("{pct:.0}%  shape")
                                    };
                                    ui.add(
                                        egui::ProgressBar::new(c.score as f32)
                                            .desired_width(110.0)
                                            .text(txt),
                                    );
                                    ui.label(format!("×{}", c.instances));
                                    ui.label(
                                        c.size_mm
                                            .map(|s| {
                                                format!("{:.1} × {:.1} × {:.1}", s[0], s[1], s[2])
                                            })
                                            .unwrap_or_else(|| "—".into()),
                                    );
                                    ui.label(&c.name);
                                    let why = c.reasons.join("; ");
                                    ui.add(
                                        egui::Label::new(RichText::new(&why).small().weak())
                                            .truncate(),
                                    )
                                    .on_hover_text(&why);
                                    ui.end_row();
                                }
                            });
                        if scan.candidates.is_empty() {
                            ui.label("No fasteners found.");
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    let busy = self.job.is_some();
                    if busy {
                        ui.spinner();
                    }
                    let any = self.selected.iter().any(|s| *s);
                    if ui
                        .add_enabled(
                            !busy && any,
                            egui::Button::new("Save STEP without the selected parts…"),
                        )
                        .on_hover_text(
                            "Writes a new STEP file: the same header, units, names, colours, \
                             PCBs and components, without the ticked parts. The original file \
                             is not changed.",
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
