// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Application state and layout.

use crate::colormap::Palette;
use crate::flow_ui;
use crate::import_ui::{self, ImportDialog};
use crate::inspector;
use crate::results::{self, FAIL, PASS, WARN};
use crate::view3d::{self, box_quads, shade, Clip, Layer, Preset, Quad, Tag, Viewport};
use eframe::egui::{
    self, Align2, Color32, ComboBox, FontId, Key, KeyboardShortcut, Modifiers, RichText, Stroke,
    Ui, ViewportCommand,
};
use ferrotherm_core::case::{
    Block, Boundary, BoundaryKind, Component, Face, Fan, HeatSink, Loss, Monitor, Resistance,
};
use ferrotherm_core::model::{component_box, BlockOrigin, Model, Owner};
use ferrotherm_core::report::Report;
use ferrotherm_core::simulate::{Event, RunResult};
use ferrotherm_core::{Case, Library, Simulation};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const EXAMPLE_CASE: &str = include_str!("../../../examples/mil_enclosure.toml");

const NEW_CASE: &str = r#"
[meta]
title = "New model"

[domain]
min = [0, 0, 0]
max = [100, 80, 20]
material = "air-still"

[grid]
max_cell_mm = 2.0

[environment]
preset = "mil-std-810h-a2-basic-hot"
use = "ambient_max"

[[blocks]]
name = "plate"
material = "al-6061-t6"
min = [0, 0, 0]
max = [100, 80, 4]

[[components]]
name = "U1"
package = "lcc-28"
position = [44.285, 34.285, 4]
power_w = 2.0
r_jc = 20.0
case_face = "bottom"
derating = "sd18-hermetic-normal"

[[boundaries]]
name = "bottom"
face = "zmin"
type = "convection"
h = 10.0
finish = "al-anodize-black"
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sel {
    None,
    Meta,
    Domain,
    Grid,
    Environment,
    Solver,
    Flow,
    Radiation,
    Block(usize),
    Component(usize),
    Boundary(usize),
    Fan(usize),
    Resistance(usize),
    HeatSink(usize),
    Monitor(usize),
}

/// Field shown in the results view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Temperature,
    Speed,
    Pressure,
}

/// A finished solution, independent of later edits to the case.
pub struct Solved {
    pub id: u64,
    pub case: Case,
    pub model: Model,
    pub result: RunResult,
    pub report: Report,
}

enum Msg {
    Built {
        cells: usize,
        dims: [usize; 3],
        power: f64,
        warnings: Vec<String>,
    },
    Status(String),
    Iter {
        residual: f64,
        tolerance: f64,
    },
    Outer(usize),
    Step {
        step: usize,
        steps: usize,
    },
    Flow {
        iteration: usize,
        max_iterations: usize,
        residuals: [f64; 3],
    },
    Solved(Box<Solved>),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Check,
    Solve,
}

struct Job {
    kind: JobKind,
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    status: String,
    fraction: Option<f32>,
    outer: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Src {
    Geometry,
    Results,
    Boundary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Geometry,
    Results,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BottomTab {
    Results,
    Convergence,
    Messages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LibTab {
    Materials,
    Finishes,
    Packages,
    Environments,
    Derating,
}

#[derive(Debug, Clone, PartialEq)]
enum Pending {
    New,
    Open,
    OpenPath(PathBuf),
    Example,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Info,
    Warn,
    Error,
}

struct LogLine {
    level: Level,
    text: String,
}

#[derive(PartialEq)]
struct CacheKey {
    solve: u64,
    snapshot: usize,
    clip: (bool, usize, u64, bool),
    hidden: Vec<bool>,
    show_air: bool,
    field: Field,
}

struct ResultCache {
    key: CacheKey,
    gen: u64,
    quads: Vec<Quad>,
    vmin: f32,
    vmax: f32,
}

pub struct FerroApp {
    case: Case,
    library: Library,
    lib_key: Vec<String>,
    path: Option<PathBuf>,
    sel: Sel,
    hidden: Vec<bool>,
    undo: Vec<String>,
    redo: Vec<String>,
    last_json: String,
    dirty: bool,
    job: Option<Job>,
    solved: Option<Solved>,
    next_id: u64,
    stale: bool,
    check_info: Option<String>,
    log: Vec<LogLine>,
    vp: Viewport,
    mode: ViewMode,
    clip: Clip,
    xray: bool,
    show_air: bool,
    show_bcs: bool,
    palette: Palette,
    auto_range: bool,
    range: (f32, f32),
    snapshot: usize,
    cache: Option<ResultCache>,
    painter_geometry: bool,
    show_library: bool,
    lib_tab: LibTab,
    show_about: bool,
    show_help: bool,
    pending: Option<Pending>,
    allow_close: bool,
    bottom: BottomTab,
    title: String,
    field: Field,
    vectors: bool,
    vector_density: usize,
    /// Live flow residuals of the running solve: (iteration, [mass, momentum, energy]).
    live_residuals: Vec<(usize, [f64; 3])>,
    import: Option<ImportDialog>,
}

/// Axis-aligned box (min, max) in mm.
type BoxMm = ([f64; 3], [f64; 3]);

/// Index of the cell containing `x` (grid node coordinates, m).
fn locate(nodes: &[f64], x: f64) -> usize {
    nodes.partition_point(|&v| v <= x).saturating_sub(1)
}

fn dir_of(path: &Option<PathBuf>) -> PathBuf {
    path.as_ref()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

impl FerroApp {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 5.0);
        cc.egui_ctx.set_global_style(style);
        let case = Case::from_toml(EXAMPLE_CASE).expect("example case parses");
        let mut app = FerroApp {
            last_json: serde_json::to_string(&case).unwrap_or_default(),
            library: Library::builtin(),
            lib_key: vec![],
            hidden: vec![false; case.blocks.len()],
            case,
            path: None,
            sel: Sel::None,
            undo: vec![],
            redo: vec![],
            dirty: false,
            job: None,
            solved: None,
            next_id: 1,
            stale: false,
            check_info: None,
            log: vec![],
            vp: Viewport::default(),
            mode: ViewMode::Geometry,
            clip: Clip {
                enabled: false,
                axis: 2,
                pos: 0.0,
                flip: false,
            },
            xray: false,
            show_air: false,
            show_bcs: false,
            palette: Palette::Turbo,
            auto_range: true,
            range: (20.0, 100.0),
            snapshot: 0,
            cache: None,
            painter_geometry: false,
            show_library: false,
            lib_tab: LibTab::Materials,
            show_about: false,
            show_help: false,
            pending: None,
            allow_close: false,
            bottom: BottomTab::Messages,
            title: String::new(),
            field: Field::Temperature,
            vectors: true,
            vector_density: 30,
            live_residuals: Vec::new(),
            import: None,
        };
        app.info(format!(
            "FerroTherm {} — author Roany Romano. Loaded the example model; F5 solves.",
            env!("CARGO_PKG_VERSION")
        ));
        if let Some(p) = path {
            let is_toml = p
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("toml"));
            if !is_toml && import_ui::is_cad(&p) {
                // `ferrotherm-gui board.kicad_pcb`: start a new model and import
                let c = Case::from_toml(NEW_CASE).expect("new case parses");
                app.set_case(c, None);
                app.case.blocks.clear();
                app.case.components.clear();
                app.hidden.clear();
                app.last_json = serde_json::to_string(&app.case).unwrap_or_default();
                app.open_import(p, &cc.egui_ctx);
            } else {
                app.open_path(&p);
            }
        }
        app.reset_clip_pos();
        app
    }

    // ------------------------------------------------------------ logging

    fn push_log(&mut self, level: Level, text: String) {
        self.log.push(LogLine { level, text });
        if self.log.len() > 500 {
            self.log.remove(0);
        }
    }
    fn info(&mut self, t: impl Into<String>) {
        self.push_log(Level::Info, t.into());
    }
    fn warn(&mut self, t: impl Into<String>) {
        self.push_log(Level::Warn, t.into());
    }
    fn error(&mut self, t: impl Into<String>) {
        self.push_log(Level::Error, t.into());
        self.bottom = BottomTab::Messages;
    }

    // ------------------------------------------------------------ case state

    fn set_case(&mut self, case: Case, path: Option<PathBuf>) {
        self.case = case;
        self.path = path;
        self.last_json = serde_json::to_string(&self.case).unwrap_or_default();
        self.undo.clear();
        self.redo.clear();
        self.dirty = false;
        self.sel = Sel::None;
        self.hidden = vec![false; self.case.blocks.len()];
        self.solved = None;
        self.cache = None;
        self.stale = false;
        self.check_info = None;
        self.mode = ViewMode::Geometry;
        self.lib_key = vec!["<force reload>".into()];
        self.reload_library();
        self.vp.needs_fit = true;
        self.reset_clip_pos();
    }

    fn reset_clip_pos(&mut self) {
        let a = self.clip.axis;
        self.clip.pos = 0.5 * (self.case.domain.min[a] + self.case.domain.max[a]);
    }

    fn reload_library(&mut self) {
        if self.lib_key == self.case.libraries {
            return;
        }
        self.lib_key = self.case.libraries.clone();
        match ferrotherm_core::model::load_library(&self.case, &dir_of(&self.path)) {
            Ok(l) => self.library = l,
            Err(e) => {
                self.library = Library::builtin();
                self.error(format!("Library: {e}"));
            }
        }
    }

    /// Detect edits once per frame (not while dragging) and record undo steps.
    fn track_changes(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.pointer.any_down()) {
            return;
        }
        let json = serde_json::to_string(&self.case).unwrap_or_default();
        if json != self.last_json {
            self.undo.push(std::mem::replace(&mut self.last_json, json));
            if self.undo.len() > 300 {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.after_edit();
        }
    }

    fn after_edit(&mut self) {
        self.dirty = true;
        if self.solved.is_some() {
            self.stale = true;
        }
        self.check_info = None;
        self.hidden.resize(self.case.blocks.len(), false);
        self.reload_library();
        self.clamp_sel();
    }

    fn clamp_sel(&mut self) {
        let ok = match self.sel {
            Sel::Block(i) => i < self.case.blocks.len(),
            Sel::Component(i) => i < self.case.components.len(),
            Sel::Boundary(i) => i < self.case.boundaries.len(),
            Sel::Fan(i) => i < self.case.fans.len(),
            Sel::Resistance(i) => i < self.case.resistances.len(),
            Sel::HeatSink(i) => i < self.case.heatsinks.len(),
            Sel::Monitor(i) => i < self.case.monitors.len(),
            _ => true,
        };
        if !ok {
            self.sel = Sel::None;
        }
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            if let Ok(c) = serde_json::from_str::<Case>(&prev) {
                self.redo.push(std::mem::replace(&mut self.last_json, prev));
                self.case = c;
                self.after_edit();
            }
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            if let Ok(c) = serde_json::from_str::<Case>(&next) {
                self.undo.push(std::mem::replace(&mut self.last_json, next));
                self.case = c;
                self.after_edit();
            }
        }
    }

    // ------------------------------------------------------------ files

    fn request(&mut self, p: Pending) {
        if self.dirty {
            self.pending = Some(p);
        } else {
            self.perform(p);
        }
    }

    fn perform(&mut self, p: Pending) {
        match p {
            Pending::New => {
                let c = Case::from_toml(NEW_CASE).expect("new case parses");
                self.set_case(c, None);
                self.info("New model");
            }
            Pending::Example => {
                let c = Case::from_toml(EXAMPLE_CASE).expect("example parses");
                self.set_case(c, None);
                self.info("Loaded the MIL enclosure example");
            }
            Pending::Open => {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("FerroTherm case", &["toml"])
                    .set_directory(dir_of(&self.path))
                    .pick_file()
                {
                    self.open_path(&p);
                }
            }
            Pending::OpenPath(p) => self.open_path(&p),
            Pending::Quit => {
                self.allow_close = true;
            }
        }
    }

    fn open_path(&mut self, p: &Path) {
        match Case::from_file(p) {
            Ok(c) => {
                self.set_case(c, Some(p.to_path_buf()));
                self.info(format!("Opened {}", p.display()));
            }
            Err(e) => self.error(format!("Cannot open {}: {e}", p.display())),
        }
    }

    fn save(&mut self, save_as: bool) -> bool {
        let path = match (&self.path, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => {
                let name = self
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "model.toml".into());
                rfd::FileDialog::new()
                    .add_filter("FerroTherm case", &["toml"])
                    .set_directory(dir_of(&self.path))
                    .set_file_name(name)
                    .save_file()
            }
        };
        let Some(mut path) = path else {
            return false;
        };
        if path.extension().is_none() {
            path.set_extension("toml");
        }
        match self.case.to_toml() {
            Ok(text) => {
                let header = format!(
                    "# FerroTherm case — saved by FerroTherm {}\n# Units: mm, °C, W, W/(m² K), s\n\n",
                    env!("CARGO_PKG_VERSION")
                );
                match std::fs::write(&path, header + &text) {
                    Ok(()) => {
                        self.info(format!("Saved {}", path.display()));
                        self.path = Some(path);
                        self.dirty = false;
                        true
                    }
                    Err(e) => {
                        self.error(format!("Cannot save {}: {e}", path.display()));
                        false
                    }
                }
            }
            Err(e) => {
                self.error(format!("Cannot serialise the case: {e}"));
                false
            }
        }
    }

    fn export_results(&mut self) {
        let Some(s) = &self.solved else { return };
        let Some(dir) = rfd::FileDialog::new()
            .set_directory(dir_of(&self.path))
            .pick_folder()
        else {
            return;
        };
        let stem = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "ferrotherm".into());
        match ferrotherm_core::export::write_results(&dir, &stem, &s.model, &s.result, &s.report) {
            Ok(files) => {
                let msg = format!(
                    "Exported {} files to {} (open the .vtr/.pvd in ParaView)",
                    files.len(),
                    dir.display()
                );
                self.info(msg);
            }
            Err(e) => self.error(format!("Export failed: {e}")),
        }
    }

    fn import_file(&mut self, ctx: &egui::Context, folder: bool) {
        let d = rfd::FileDialog::new().set_directory(dir_of(&self.path));
        let p = if folder {
            d.pick_folder()
        } else {
            import_ui::file_filters(d).pick_file()
        };
        if let Some(p) = p {
            self.open_import(p, ctx);
        }
    }

    fn open_import(&mut self, p: PathBuf, ctx: &egui::Context) {
        self.info(format!("Importing {}", p.display()));
        self.import = Some(ImportDialog::open(p, ctx));
    }

    fn export_geometry(&mut self, ext: &str) {
        let (label, exts): (&str, &[&str]) = match ext {
            "step" => ("STEP", &["step", "stp"]),
            "stl" => ("STL", &["stl"]),
            _ => ("IDF board", &["emn"]),
        };
        let stem = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "ferrotherm".into());
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter(label, exts)
            .set_directory(dir_of(&self.path))
            .set_file_name(format!("{stem}.{ext}"))
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension(ext);
        }
        let lib = &self.library;
        let res: Result<Vec<PathBuf>, String> = match ext {
            "step" => ferrotherm_io::export::case_to_step(&self.case, lib)
                .and_then(|t| std::fs::write(&path, t).map_err(|e| e.to_string()))
                .map(|_| vec![path.clone()]),
            "stl" => ferrotherm_io::export::case_to_stl(&self.case, lib)
                .and_then(|t| std::fs::write(&path, t).map_err(|e| e.to_string()))
                .map(|_| vec![path.clone()]),
            _ => ferrotherm_io::export::case_to_idf(&self.case, lib).and_then(|(emn, emp)| {
                let a = path.with_extension("emn");
                let b = path.with_extension("emp");
                std::fs::write(&a, emn).map_err(|e| e.to_string())?;
                std::fs::write(&b, emp).map_err(|e| e.to_string())?;
                Ok(vec![a, b])
            }),
        };
        match res {
            Ok(files) => {
                let names: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
                self.info(format!("Exported {}", names.join(", ")));
            }
            Err(e) => self.error(format!("Export failed: {e}")),
        }
    }

    fn add_library_file(&mut self) {
        let Some(p) = rfd::FileDialog::new()
            .add_filter("FerroTherm library", &["json"])
            .set_directory(dir_of(&self.path))
            .pick_file()
        else {
            return;
        };
        let base = dir_of(&self.path);
        let rel = p
            .strip_prefix(&base)
            .map(Path::to_path_buf)
            .unwrap_or(p.clone());
        let s = rel.to_string_lossy().replace('\\', "/");
        if !self.case.libraries.contains(&s) {
            self.case.libraries.push(s);
            self.info(format!("Added library {}", p.display()));
        }
    }

    // ------------------------------------------------------------ solver

    fn start(&mut self, kind: JobKind, ctx: &egui::Context) {
        if self.job.is_some() {
            return;
        }
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let case = self.case.clone();
        let lib = self.library.clone();
        let flag = cancel.clone();
        let ctx2 = ctx.clone();
        let id = self.next_id;
        self.next_id += 1;
        std::thread::spawn(move || {
            let send = |m: Msg| {
                let _ = tx.send(m);
                ctx2.request_repaint();
            };
            let sim = match Simulation::with_library(case.clone(), lib) {
                Ok(s) => s,
                Err(e) => {
                    send(Msg::Failed(e.to_string()));
                    return;
                }
            };
            send(Msg::Built {
                cells: sim.model.ncells,
                dims: sim.model.grid.n(),
                power: sim.model.total_power(),
                warnings: sim.model.warnings.clone(),
            });
            if kind == JobKind::Check {
                return;
            }
            let mut last = Instant::now() - Duration::from_secs(1);
            let out = sim.run_observed(&mut |e| {
                if flag.load(Ordering::Relaxed) {
                    return false;
                }
                let due = last.elapsed() > Duration::from_millis(60);
                match e {
                    Event::Message(s) => send(Msg::Status(s.to_string())),
                    Event::Outer(k) => send(Msg::Outer(k)),
                    Event::Iteration {
                        residual,
                        tolerance,
                        ..
                    } if due => {
                        last = Instant::now();
                        send(Msg::Iter {
                            residual,
                            tolerance,
                        });
                    }
                    Event::Step { step, steps, .. } if due || step == steps => {
                        last = Instant::now();
                        send(Msg::Step { step, steps });
                    }
                    Event::FlowIteration {
                        iteration,
                        max_iterations,
                        residuals,
                    } => send(Msg::Flow {
                        iteration,
                        max_iterations,
                        residuals,
                    }),
                    _ => {}
                }
                true
            });
            match out {
                Ok(o) => send(Msg::Solved(Box::new(Solved {
                    id,
                    case,
                    model: sim.model,
                    result: o.result,
                    report: o.report,
                }))),
                Err(e) => send(Msg::Failed(e.to_string())),
            }
        });
        self.live_residuals.clear();
        if kind == JobKind::Solve && self.case.flow.as_ref().is_some_and(|f| f.enabled) {
            self.bottom = BottomTab::Convergence;
        }
        self.job = Some(Job {
            kind,
            rx,
            cancel,
            started: Instant::now(),
            status: match kind {
                JobKind::Check => "Building the grid…".into(),
                JobKind::Solve => "Building the model…".into(),
            },
            fraction: None,
            outer: 0,
        });
    }

    fn poll_job(&mut self) {
        let Some(job) = &mut self.job else { return };
        let mut finished: Option<Result<Option<Box<Solved>>, String>> = None;
        let mut built = None;
        while let Ok(m) = job.rx.try_recv() {
            match m {
                Msg::Built {
                    cells,
                    dims,
                    power,
                    warnings,
                } => {
                    built = Some((cells, dims, power, warnings));
                    if job.kind == JobKind::Check {
                        finished = Some(Ok(None));
                    } else {
                        job.status = format!("Solving {cells} cells…");
                    }
                }
                Msg::Status(s) => job.status = s,
                Msg::Outer(k) => job.outer = k,
                Msg::Iter {
                    residual,
                    tolerance,
                } => {
                    let f = (residual.max(1e-300).ln() / tolerance.ln()).clamp(0.0, 1.0) as f32;
                    job.fraction = Some(f);
                    job.status = if job.outer > 0 {
                        format!(
                            "Radiation iteration {} · residual {residual:.1e}",
                            job.outer
                        )
                    } else {
                        format!("Residual {residual:.1e} (target {tolerance:.0e})")
                    };
                }
                Msg::Step { step, steps } => {
                    job.fraction = Some(step as f32 / steps as f32);
                    job.status = format!("Time step {step} / {steps}");
                }
                Msg::Flow {
                    iteration,
                    max_iterations,
                    residuals,
                } => {
                    self.live_residuals.push((iteration, residuals));
                    if self.case.solver.mode == ferrotherm_core::case::Mode::Steady {
                        job.fraction = Some(iteration as f32 / max_iterations.max(1) as f32);
                    }
                    job.status = format!(
                        "Flow iteration {iteration} · mass {:.1e} · momentum {:.1e} · energy {:.1e}",
                        residuals[0], residuals[1], residuals[2]
                    );
                }
                Msg::Solved(s) => finished = Some(Ok(Some(s))),
                Msg::Failed(e) => finished = Some(Err(e)),
            }
        }
        if let Some((cells, dims, power, warnings)) = built {
            let info = format!(
                "Grid {} × {} × {} = {} cells · total power {:.2} W",
                dims[0], dims[1], dims[2], cells, power
            );
            self.check_info = Some(info.clone());
            self.info(info);
            for w in warnings {
                self.warn(w);
            }
        }
        if let Some(f) = finished {
            let job = self.job.take().expect("job");
            let secs = job.started.elapsed().as_secs_f64();
            match f {
                Ok(Some(s)) => {
                    let fails = s
                        .report
                        .components
                        .iter()
                        .filter(|c| c.pass == Some(false))
                        .count();
                    self.info(format!(
                        "Solved in {secs:.1} s · T max {:.1} °C · energy imbalance {:.1e} %{}",
                        s.report.t_max_c,
                        s.report.energy_imbalance_pct,
                        if fails > 0 {
                            format!(" · {fails} component(s) over limit")
                        } else {
                            String::new()
                        }
                    ));
                    self.snapshot = s.result.snapshots.len() - 1;
                    self.solved = Some(*s);
                    self.stale = false;
                    self.cache = None;
                    self.mode = ViewMode::Results;
                    self.bottom = BottomTab::Results;
                }
                Ok(None) => {}
                Err(e) => {
                    if e.contains("cancelled") {
                        self.warn("Solve cancelled");
                    } else {
                        self.error(e);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ editing helpers

    fn add_block(&mut self) {
        let d = &self.case.domain;
        let c: Vec<f64> = (0..3).map(|a| 0.5 * (d.min[a] + d.max[a])).collect();
        let h: Vec<f64> = (0..3).map(|a| 0.1 * (d.max[a] - d.min[a])).collect();
        let n = self.case.blocks.len() + 1;
        self.case.blocks.push(Block {
            name: format!("block_{n}"),
            material: Some("al-6061-t6".into()),
            pcb: None,
            min: [c[0] - h[0], c[1] - h[1], c[2] - h[2]],
            max: [c[0] + h[0], c[1] + h[1], c[2] + h[2]],
            power_w: 0.0,
            emissivity: None,
            finish: None,
        });
        self.hidden.push(false);
        self.sel = Sel::Block(self.case.blocks.len() - 1);
    }

    fn add_component(&mut self) {
        let d = &self.case.domain;
        let n = self.case.components.len() + 1;
        self.case.components.push(Component {
            name: format!("U{n}"),
            package: "lcc-28".into(),
            position: [
                0.5 * (d.min[0] + d.max[0]),
                0.5 * (d.min[1] + d.max[1]),
                0.5 * (d.min[2] + d.max[2]),
            ],
            size: None,
            power_w: 1.0,
            r_jc: Some(20.0),
            r_jb: None,
            case_face: None,
            mount_axis: "z".into(),
            derating: Some("sd18-hermetic-normal".into()),
            tj_max_c: None,
            network: None,
        });
        self.sel = Sel::Component(self.case.components.len() - 1);
    }

    fn add_boundary(&mut self) {
        let has_env = self.case.environment.is_some();
        self.case.boundaries.push(Boundary {
            name: String::new(),
            face: Face::Zmax,
            region: None,
            kind: BoundaryKind::Convection,
            t_c: None,
            h: Some(10.0),
            t_ambient_c: if has_env { None } else { Some(25.0) },
            emissivity: None,
            finish: None,
            t_radiation_c: None,
            heat_flux_w_m2: None,
            solar_w_m2: None,
            absorptivity: None,
            velocity_m_s: None,
            flow_m3_s: None,
            loss: None,
        });
        self.sel = Sel::Boundary(self.case.boundaries.len() - 1);
    }

    fn domain_center(&self) -> [f64; 3] {
        let d = &self.case.domain;
        [0, 1, 2].map(|a| 0.5 * (d.min[a] + d.max[a]))
    }

    fn add_fan(&mut self) {
        let c = self.domain_center();
        let d = &self.case.domain;
        let n = self.case.fans.len() + 1;
        let size = (0.3 * (d.max[1] - d.min[1]).min(d.max[2] - d.min[2])).clamp(10.0, 120.0);
        self.case.fans.push(Fan {
            name: format!("fan_{n}"),
            center: [d.min[0], c[1], c[2]],
            direction: "+x".into(),
            diameter: Some(size.round()),
            hub_diameter: Some((0.4 * size).round()),
            size: None,
            // generic 40 mm-class curve: replace with the datasheet
            curve: vec![[0.0, 30.0], [0.0025, 20.0], [0.005, 8.0], [0.0065, 0.0]],
            flow_m3_s: None,
        });
        self.sel = Sel::Fan(self.case.fans.len() - 1);
        self.ensure_flow_on();
    }

    fn add_resistance(&mut self) {
        let c = self.domain_center();
        let d = &self.case.domain;
        let n = self.case.resistances.len() + 1;
        self.case.resistances.push(Resistance {
            name: format!("grille_{n}"),
            center: c,
            axis: "x".into(),
            size: [0.5 * (d.max[1] - d.min[1]), 0.5 * (d.max[2] - d.min[2])],
            loss: Loss {
                zeta: 2.0,
                free_area: Some(0.5),
                hole_diameter_mm: Some(3.0),
            },
        });
        self.sel = Sel::Resistance(self.case.resistances.len() - 1);
        self.ensure_flow_on();
    }

    fn add_heatsink(&mut self) {
        let c = self.domain_center();
        let n = self.case.heatsinks.len() + 1;
        self.case.heatsinks.push(HeatSink {
            name: format!("heatsink_{n}"),
            material: "al-6061-t6".into(),
            position: [c[0] - 20.0, c[1] - 20.0, c[2]],
            base: [40.0, 40.0, 3.0],
            normal: "z".into(),
            fin_axis: "x".into(),
            fin_count: 8,
            fin_thickness: 1.2,
            fin_height: 15.0,
            emissivity: None,
        });
        self.sel = Sel::HeatSink(self.case.heatsinks.len() - 1);
    }

    fn add_monitor(&mut self) {
        let n = self.case.monitors.len() + 1;
        self.case.monitors.push(Monitor {
            name: format!("probe_{n}"),
            position: self.domain_center(),
        });
        self.sel = Sel::Monitor(self.case.monitors.len() - 1);
    }

    fn ensure_flow_on(&mut self) {
        if !self.case.flow.as_ref().is_some_and(|f| f.enabled) {
            self.info("Fans and grilles act only with the air flow on: Model > Air flow.");
        }
    }

    fn delete_selected(&mut self) {
        match self.sel {
            Sel::Block(i) if i < self.case.blocks.len() => {
                self.case.blocks.remove(i);
                if i < self.hidden.len() {
                    self.hidden.remove(i);
                }
            }
            Sel::Component(i) if i < self.case.components.len() => {
                self.case.components.remove(i);
            }
            Sel::Boundary(i) if i < self.case.boundaries.len() => {
                self.case.boundaries.remove(i);
            }
            Sel::Fan(i) if i < self.case.fans.len() => {
                self.case.fans.remove(i);
            }
            Sel::Resistance(i) if i < self.case.resistances.len() => {
                self.case.resistances.remove(i);
            }
            Sel::HeatSink(i) if i < self.case.heatsinks.len() => {
                self.case.heatsinks.remove(i);
            }
            Sel::Monitor(i) if i < self.case.monitors.len() => {
                self.case.monitors.remove(i);
            }
            _ => return,
        }
        self.sel = Sel::None;
    }

    fn duplicate_selected(&mut self) {
        match self.sel {
            Sel::Block(i) => {
                let mut b = self.case.blocks[i].clone();
                b.name = format!("{}_copy", b.name);
                self.case.blocks.insert(i + 1, b);
                self.hidden.insert(i + 1, false);
                self.sel = Sel::Block(i + 1);
            }
            Sel::Component(i) => {
                let mut c = self.case.components[i].clone();
                c.name = format!("{}_copy", c.name);
                self.case.components.insert(i + 1, c);
                self.sel = Sel::Component(i + 1);
            }
            Sel::Boundary(i) => {
                let b = self.case.boundaries[i].clone();
                self.case.boundaries.insert(i + 1, b);
                self.sel = Sel::Boundary(i + 1);
            }
            Sel::Fan(i) => {
                let mut f = self.case.fans[i].clone();
                f.name = format!("{}_copy", f.name);
                self.case.fans.insert(i + 1, f);
                self.sel = Sel::Fan(i + 1);
            }
            Sel::Resistance(i) => {
                let mut r = self.case.resistances[i].clone();
                r.name = format!("{}_copy", r.name);
                self.case.resistances.insert(i + 1, r);
                self.sel = Sel::Resistance(i + 1);
            }
            Sel::HeatSink(i) => {
                let mut h = self.case.heatsinks[i].clone();
                h.name = format!("{}_copy", h.name);
                self.case.heatsinks.insert(i + 1, h);
                self.sel = Sel::HeatSink(i + 1);
            }
            Sel::Monitor(i) => {
                let mut m = self.case.monitors[i].clone();
                m.name = format!("{}_copy", m.name);
                self.case.monitors.insert(i + 1, m);
                self.sel = Sel::Monitor(i + 1);
            }
            _ => {}
        }
    }

    fn move_selected(&mut self, up: bool) {
        fn mv<T>(v: &mut [T], i: usize, up: bool) -> Option<usize> {
            let j = if up { i.checked_sub(1)? } else { i + 1 };
            if j >= v.len() {
                return None;
            }
            v.swap(i, j);
            Some(j)
        }
        match self.sel {
            Sel::Block(i) => {
                if let Some(j) = mv(&mut self.case.blocks, i, up) {
                    self.hidden.swap(i, j);
                    self.sel = Sel::Block(j);
                }
            }
            Sel::Component(i) => {
                if let Some(j) = mv(&mut self.case.components, i, up) {
                    self.sel = Sel::Component(j);
                }
            }
            Sel::Boundary(i) => {
                if let Some(j) = mv(&mut self.case.boundaries, i, up) {
                    self.sel = Sel::Boundary(j);
                }
            }
            Sel::Fan(i) => {
                if let Some(j) = mv(&mut self.case.fans, i, up) {
                    self.sel = Sel::Fan(j);
                }
            }
            Sel::Resistance(i) => {
                if let Some(j) = mv(&mut self.case.resistances, i, up) {
                    self.sel = Sel::Resistance(j);
                }
            }
            Sel::HeatSink(i) => {
                if let Some(j) = mv(&mut self.case.heatsinks, i, up) {
                    self.sel = Sel::HeatSink(j);
                }
            }
            Sel::Monitor(i) => {
                if let Some(j) = mv(&mut self.case.monitors, i, up) {
                    self.sel = Sel::Monitor(j);
                }
            }
            _ => {}
        }
    }

    fn ambient(&self) -> Option<f64> {
        let e = self.case.environment.as_ref()?;
        let env = self.library.environment(&e.preset).ok()?;
        use ferrotherm_core::case::EnvironmentUse::*;
        Some(match e.use_ {
            AmbientMax => env.ambient_max_c,
            AmbientMin => env.ambient_min_c,
            InducedMax => env.induced_max_c,
            InducedMin => env.induced_min_c,
        })
    }

    fn results_valid(&self) -> Option<&Solved> {
        self.solved.as_ref().filter(|_| !self.stale)
    }

    // ------------------------------------------------------------ UI: menus

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let sc = |m: Modifiers, k: Key| KeyboardShortcut::new(m, k);
        let (new, open, save, save_as, undo, redo, redo2, solve) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&sc(cmd, Key::N)),
                i.consume_shortcut(&sc(cmd, Key::O)),
                i.consume_shortcut(&sc(cmd | Modifiers::SHIFT, Key::S)),
                false,
                i.consume_shortcut(&sc(cmd, Key::Z)),
                i.consume_shortcut(&sc(cmd | Modifiers::SHIFT, Key::Z)),
                i.consume_shortcut(&sc(cmd, Key::Y)),
                i.consume_shortcut(&sc(Modifiers::NONE, Key::F5)),
            )
        });
        // Cmd+S after Cmd+Shift+S so the shift variant wins
        let save_plain = ctx.input_mut(|i| i.consume_shortcut(&sc(cmd, Key::S)));
        if new {
            self.request(Pending::New);
        }
        if open {
            self.request(Pending::Open);
        }
        if save || save_as {
            self.save(true);
        } else if save_plain {
            self.save(false);
        }
        if redo || redo2 {
            self.redo();
        } else if undo {
            self.undo();
        }
        if solve {
            self.start(JobKind::Solve, ctx);
        }
        let no_focus = ctx.memory(|m| m.focused().is_none());
        if no_focus {
            let (del, esc, dup) = ctx.input_mut(|i| {
                (
                    i.consume_shortcut(&sc(Modifiers::NONE, Key::Delete))
                        | i.consume_shortcut(&sc(Modifiers::NONE, Key::Backspace)),
                    i.consume_shortcut(&sc(Modifiers::NONE, Key::Escape)),
                    i.consume_shortcut(&sc(cmd, Key::D)),
                )
            });
            if del {
                self.delete_selected();
            }
            if esc {
                self.sel = Sel::None;
            }
            if dup {
                self.duplicate_selected();
            }
        }
    }

    fn menu(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mac = cfg!(target_os = "macos");
        let k = |s: &str| {
            if mac {
                s.replace("Ctrl+", "Cmd+")
            } else {
                s.to_string()
            }
        };
        let item = |label: &str, keys: &str| format!("{label:<15}{}", k(keys));
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button(item("New", "Ctrl+N")).clicked() {
                    self.request(Pending::New);
                }
                if ui.button(item("Open…", "Ctrl+O")).clicked() {
                    self.request(Pending::Open);
                }
                if ui.button("Open example").clicked() {
                    self.request(Pending::Example);
                }
                ui.separator();
                if ui.button(item("Save", "Ctrl+S")).clicked() {
                    self.save(false);
                }
                if ui.button(item("Save as…", "Ctrl+Shift+S")).clicked() {
                    self.save(true);
                }
                ui.separator();
                if ui
                    .button("Import CAD…")
                    .on_hover_text("STEP, STL, IDF, KiCad, Gerber/Excellon, IPC-2581, ODB++")
                    .clicked()
                {
                    self.import_file(&ctx, false);
                }
                if ui
                    .button("Import CAD folder…")
                    .on_hover_text("Gerber/Excellon set or ODB++ job folder")
                    .clicked()
                {
                    self.import_file(&ctx, true);
                }
                ui.menu_button("Export geometry", |ui| {
                    if ui.button("STEP (AP214 solids)…").clicked() {
                        self.export_geometry("step");
                    }
                    if ui.button("STL…").clicked() {
                        self.export_geometry("stl");
                    }
                    if ui.button("IDF 3.0 (.emn + .emp)…").clicked() {
                        self.export_geometry("emn");
                    }
                });
                if ui
                    .add_enabled(self.solved.is_some(), egui::Button::new("Export results…"))
                    .on_hover_text(
                        "VTK for ParaView (temperature, velocity, pressure), report.md/json, \
                         components.csv, blocks.csv, junctions.csv",
                    )
                    .clicked()
                {
                    self.export_results();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(
                        !self.undo.is_empty(),
                        egui::Button::new(item("Undo", "Ctrl+Z")),
                    )
                    .clicked()
                {
                    self.undo();
                }
                if ui
                    .add_enabled(
                        !self.redo.is_empty(),
                        egui::Button::new(item("Redo", "Ctrl+Y")),
                    )
                    .clicked()
                {
                    self.redo();
                }
                ui.separator();
                if ui.button("Add block").clicked() {
                    self.add_block();
                }
                if ui.button("Add component").clicked() {
                    self.add_component();
                }
                if ui.button("Add boundary").clicked() {
                    self.add_boundary();
                }
                if ui.button("Add fan").clicked() {
                    self.add_fan();
                }
                if ui.button("Add grille / resistance").clicked() {
                    self.add_resistance();
                }
                if ui.button("Add heat sink").clicked() {
                    self.add_heatsink();
                }
                if ui.button("Add monitor point").clicked() {
                    self.add_monitor();
                }
                ui.separator();
                let has_item = self.sel_is_item();
                if ui
                    .add_enabled(has_item, egui::Button::new(item("Duplicate", "Ctrl+D")))
                    .clicked()
                {
                    self.duplicate_selected();
                }
                if ui
                    .add_enabled(has_item, egui::Button::new(item("Delete", "Del")))
                    .clicked()
                {
                    self.delete_selected();
                }
            });
            ui.menu_button("Solve", |ui| {
                let idle = self.job.is_none();
                if ui
                    .add_enabled(idle, egui::Button::new("Check model"))
                    .clicked()
                {
                    self.start(JobKind::Check, &ctx);
                }
                if ui
                    .add_enabled(idle, egui::Button::new(item("Solve", "F5")))
                    .clicked()
                {
                    self.start(JobKind::Solve, &ctx);
                }
            });
            ui.menu_button("Libraries", |ui| {
                if ui.button("Browse libraries…").clicked() {
                    self.show_library = true;
                }
                if ui
                    .button("Add a library file to this model…")
                    .on_hover_text("JSON file with your own materials, packages, …")
                    .clicked()
                {
                    self.add_library_file();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Quick guide").clicked() {
                    self.show_help = true;
                }
                if ui.button("About FerroTherm").clicked() {
                    self.show_about = true;
                }
            });
            ui.add_space(16.0);
            let busy = self.job.is_some();
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(RichText::new("▶  Solve").strong())
                        .fill(Color32::from_rgb(40, 110, 70)),
                )
                .on_hover_text("F5")
                .clicked()
            {
                self.start(JobKind::Solve, &ctx);
            }
        });
    }

    // ------------------------------------------------------------ UI: tree

    fn tree(&mut self, ui: &mut Ui) {
        let sel = self.sel;
        let item = |ui: &mut Ui, s: Sel, text: &str| -> bool {
            ui.selectable_label(sel == s, text).clicked()
        };
        let mut new_sel = None;
        ui.add_space(4.0);
        ui.label(RichText::new("MODEL").small().weak());
        let title = if self.case.meta.title.is_empty() {
            "Project".to_string()
        } else {
            self.case.meta.title.clone()
        };
        for (s, t) in [
            (Sel::Meta, title.as_str()),
            (Sel::Domain, "Domain"),
            (Sel::Grid, "Grid"),
            (Sel::Environment, "Environment"),
            (Sel::Solver, "Solver"),
            (
                Sel::Flow,
                if self.case.flow.as_ref().is_some_and(|f| f.enabled) {
                    "Air flow · on"
                } else {
                    "Air flow · off"
                },
            ),
            (
                Sel::Radiation,
                if self.case.radiation.as_ref().is_some_and(|r| r.enabled) {
                    "Radiation · on"
                } else {
                    "Radiation · off"
                },
            ),
        ] {
            if item(ui, s, t) {
                new_sel = Some(s);
            }
        }
        let header = |ui: &mut Ui, label: &str, n: usize| -> bool {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{label} ({n})")).small().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.small_button("+").on_hover_text("Add").clicked()
                })
                .inner
            })
            .inner
        };
        if header(ui, "BLOCKS", self.case.blocks.len()) {
            self.add_block();
        }
        // (name, Tj, pass) per component of a valid solution, computed up front
        let verdicts: Vec<(String, f64, Option<bool>)> = self
            .results_valid()
            .map(|s| {
                s.report
                    .components
                    .iter()
                    .map(|c| (c.name.clone(), c.tj_c, c.pass))
                    .collect()
            })
            .unwrap_or_default();
        let mut toggle_hidden = None;
        for (i, b) in self.case.blocks.iter().enumerate() {
            ui.horizontal(|ui| {
                let mut vis = !self.hidden.get(i).copied().unwrap_or(false);
                if ui
                    .checkbox(&mut vis, "")
                    .on_hover_text("Show in the 3D view")
                    .changed()
                {
                    toggle_hidden = Some((i, !vis));
                }
                let mut text = RichText::new(&b.name);
                if b.power_w > 0.0 {
                    text = RichText::new(format!("{}  ·  {:.1} W", b.name, b.power_w));
                }
                if ui.selectable_label(sel == Sel::Block(i), text).clicked() {
                    new_sel = Some(Sel::Block(i));
                }
            });
        }
        if let Some((i, h)) = toggle_hidden {
            if let Some(x) = self.hidden.get_mut(i) {
                *x = h;
            }
        }
        if header(ui, "COMPONENTS", self.case.components.len()) {
            self.add_component();
        }
        for (i, c) in self.case.components.iter().enumerate() {
            let verdict = verdicts.get(i).filter(|r| r.0 == c.name);
            let mut text = RichText::new(format!("{}  ·  {:.1} W", c.name, c.power_w));
            if let Some((_, tj, pass)) = verdict {
                text = RichText::new(format!("{}  ·  Tj {:.0} °C", c.name, tj));
                if *pass == Some(false) {
                    text = text.color(FAIL);
                } else if *pass == Some(true) {
                    text = text.color(PASS);
                }
            }
            if ui
                .selectable_label(sel == Sel::Component(i), text)
                .clicked()
            {
                new_sel = Some(Sel::Component(i));
            }
        }
        if header(ui, "BOUNDARIES", self.case.boundaries.len()) {
            self.add_boundary();
        }
        for (i, b) in self.case.boundaries.iter().enumerate() {
            let kind = match b.kind {
                BoundaryKind::Adiabatic => "adiabatic",
                BoundaryKind::Fixed => "fixed T",
                BoundaryKind::Convection => "convection",
                BoundaryKind::Radiation => "radiation",
                BoundaryKind::Opening => "opening",
                BoundaryKind::Inlet => "inlet",
                BoundaryKind::Symmetry => "symmetry",
            };
            let name = if b.name.is_empty() {
                b.face.name().to_string()
            } else {
                b.name.clone()
            };
            if ui
                .selectable_label(sel == Sel::Boundary(i), format!("{name}  ·  {kind}"))
                .clicked()
            {
                new_sel = Some(Sel::Boundary(i));
            }
        }
        let fan_results: Vec<(String, f64)> = self
            .results_valid()
            .and_then(|s| s.result.flow.as_ref())
            .map(|f| {
                f.fans
                    .iter()
                    .map(|r| (r.name.clone(), r.flow_m3_s))
                    .collect()
            })
            .unwrap_or_default();
        if header(ui, "FANS", self.case.fans.len()) {
            self.add_fan();
        }
        for (i, f) in self.case.fans.iter().enumerate() {
            let text = match fan_results.iter().find(|r| r.0 == f.name) {
                Some((_, q)) => format!("{}  ·  {:.1} CFM", f.name, q / flow_ui::CFM),
                None => f.name.clone(),
            };
            if ui.selectable_label(sel == Sel::Fan(i), text).clicked() {
                new_sel = Some(Sel::Fan(i));
            }
        }
        if header(ui, "GRILLES", self.case.resistances.len()) {
            self.add_resistance();
        }
        for (i, r) in self.case.resistances.iter().enumerate() {
            if ui
                .selectable_label(
                    sel == Sel::Resistance(i),
                    format!("{}  ·  ζ {:.2}", r.name, r.loss.zeta),
                )
                .clicked()
            {
                new_sel = Some(Sel::Resistance(i));
            }
        }
        if header(ui, "HEAT SINKS", self.case.heatsinks.len()) {
            self.add_heatsink();
        }
        for (i, h) in self.case.heatsinks.iter().enumerate() {
            if ui
                .selectable_label(
                    sel == Sel::HeatSink(i),
                    format!("{}  ·  {} fins", h.name, h.fin_count),
                )
                .clicked()
            {
                new_sel = Some(Sel::HeatSink(i));
            }
        }
        if header(ui, "MONITORS", self.case.monitors.len()) {
            self.add_monitor();
        }
        for (i, m) in self.case.monitors.iter().enumerate() {
            if ui
                .selectable_label(sel == Sel::Monitor(i), &m.name)
                .clicked()
            {
                new_sel = Some(Sel::Monitor(i));
            }
        }
        if let Some(s) = new_sel {
            self.sel = s;
        }
        if self.sel_is_item() {
            ui.add_space(10.0);
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                if ui.small_button("Duplicate").clicked() {
                    self.duplicate_selected();
                }
                if ui.small_button("Up").on_hover_text("Move up").clicked() {
                    self.move_selected(true);
                }
                if ui.small_button("Down").on_hover_text("Move down").clicked() {
                    self.move_selected(false);
                }
                if ui
                    .small_button(RichText::new("Delete").color(FAIL))
                    .clicked()
                {
                    self.delete_selected();
                }
            });
            ui.label(
                RichText::new("Later items overwrite earlier ones where they overlap.")
                    .small()
                    .weak(),
            );
        }
    }

    // ------------------------------------------------------------ UI: inspector

    fn inspector(&mut self, ui: &mut Ui) {
        let lib = &self.library;
        let amb = self.ambient();
        let valid = self.solved.as_ref().filter(|_| !self.stale);
        match self.sel {
            Sel::None => {
                ui.add_space(8.0);
                ui.heading("FerroTherm");
                ui.label(
                    RichText::new(
                        "Select an item in the tree or click it in the 3D view to edit it. \
                         Use + to add blocks, components and boundaries, then press Solve (F5).",
                    )
                    .weak(),
                );
            }
            Sel::Meta => inspector::meta(ui, &mut self.case.meta),
            Sel::Domain => inspector::domain(ui, &mut self.case.domain, lib),
            Sel::Grid => {
                inspector::grid_settings(ui, &mut self.case.grid, self.check_info.as_deref())
            }
            Sel::Environment => inspector::environment(ui, &mut self.case.environment, lib),
            Sel::Solver => inspector::solver(ui, &mut self.case.solver),
            Sel::Flow => {
                flow_ui::flow_settings(ui, &mut self.case.flow, &mut self.case.domain.material, lib)
            }
            Sel::Radiation => flow_ui::radiation_settings(ui, &mut self.case.radiation),
            Sel::Fan(i) => {
                let r = valid.and_then(|s| s.result.flow.as_ref()).and_then(|f| {
                    let name = &self.case.fans.get(i)?.name;
                    f.fans
                        .iter()
                        .find(|x| &x.name == name)
                        .map(|x| (x.flow_m3_s, x.pressure_pa))
                });
                if let Some(f) = self.case.fans.get_mut(i) {
                    flow_ui::fan(ui, f, i, r);
                }
            }
            Sel::Resistance(i) => {
                if let Some(r) = self.case.resistances.get_mut(i) {
                    flow_ui::resistance(ui, r, i);
                }
            }
            Sel::HeatSink(i) => {
                if let Some(h) = self.case.heatsinks.get_mut(i) {
                    flow_ui::heatsink(ui, h, i, lib);
                    if let Some(s) = valid {
                        let prefix = format!("{}/", h.name);
                        let parts: Vec<_> = s
                            .report
                            .blocks
                            .iter()
                            .filter(|b| b.name.starts_with(&prefix))
                            .collect();
                        if !parts.is_empty() {
                            let tmax = parts.iter().map(|b| b.t_max_c).fold(f64::MIN, f64::max);
                            let tmin = parts.iter().map(|b| b.t_min_c).fold(f64::MAX, f64::min);
                            ui.add_space(6.0);
                            ui.label(
                                RichText::new(format!("Result: {tmin:.1} … {tmax:.1} °C")).strong(),
                            );
                        }
                    }
                }
            }
            Sel::Monitor(i) => {
                let r = valid.and_then(|s| s.result.flow.as_ref()).and_then(|f| {
                    let name = &self.case.monitors.get(i)?.name;
                    f.monitors
                        .iter()
                        .find(|x| &x.name == name)
                        .map(|x| (x.t_c, x.speed_m_s))
                });
                if let Some(m) = self.case.monitors.get_mut(i) {
                    flow_ui::monitor(ui, m, r);
                }
            }
            Sel::Block(i) => {
                if let Some(b) = self.case.blocks.get_mut(i) {
                    inspector::block(ui, b, lib, i);
                    if let Some(s) = valid {
                        if let Some(r) = s.report.blocks.iter().find(|r| r.name == b.name) {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(format!(
                                    "Result: {:.1} / {:.1} / {:.1} °C (min / avg / max)",
                                    r.t_min_c, r.t_avg_c, r.t_max_c
                                ))
                                .strong(),
                            );
                        }
                    }
                }
            }
            Sel::Component(i) => {
                if let Some(c) = self.case.components.get_mut(i) {
                    let r = valid
                        .and_then(|s| s.report.components.get(i))
                        .filter(|r| r.name == c.name);
                    inspector::component(ui, c, lib, i, r);
                    flow_ui::network_editor(ui, &mut c.network, i);
                    if let Some(r) = r {
                        if !r.surfaces.is_empty() {
                            ui.add_space(6.0);
                            ui.label(RichText::new("Heat leaving each surface").strong());
                            for (n, q) in &r.surfaces {
                                ui.label(format!("{n}: {q:.3} W"));
                            }
                        }
                    }
                }
            }
            Sel::Boundary(i) => {
                if let Some(b) = self.case.boundaries.get_mut(i) {
                    let q = valid
                        .and_then(|s| s.report.boundaries.get(i))
                        .map(|r| r.heat_out_w);
                    inspector::boundary(ui, b, lib, i, amb, q);
                }
            }
        }
    }

    // ------------------------------------------------------------ UI: 3D view

    fn view_toolbar(&mut self, ui: &mut Ui) {
        ui.spacing_mut().slider_width = 110.0;
        ui.horizontal_wrapped(|ui| {
            let has = self.solved.is_some();
            ui.selectable_value(&mut self.mode, ViewMode::Geometry, "Geometry");
            let has_flow = self
                .solved
                .as_ref()
                .is_some_and(|s| s.result.snapshots.last().is_some_and(|x| x.vel.is_some()));
            ui.add_enabled_ui(has, |ui| {
                for (f, label) in [
                    (Field::Temperature, "Temperature"),
                    (Field::Speed, "Speed"),
                    (Field::Pressure, "Pressure"),
                ] {
                    if f != Field::Temperature && !has_flow {
                        continue;
                    }
                    let on = self.mode == ViewMode::Results && self.field == f;
                    if ui
                        .selectable_label(on, label)
                        .on_disabled_hover_text("Solve first (F5)")
                        .clicked()
                    {
                        self.mode = ViewMode::Results;
                        if f != Field::Temperature
                            && self.field == Field::Temperature
                            && !self.clip.enabled
                        {
                            // air fields are read on a section plane
                            self.clip.enabled = true;
                            self.clip.axis = 1;
                            self.reset_clip_pos();
                        }
                        self.field = f;
                    }
                }
            });
            ui.separator();
            for (p, t, tip) in [
                (Preset::Iso, "Iso", "Isometric"),
                (Preset::Top, "Top", "From +z"),
                (Preset::Front, "Front", "From -y"),
                (Preset::Right, "Right", "From +x"),
                (Preset::Back, "Back", "From +y"),
                (Preset::Left, "Left", "From -x"),
                (Preset::Bottom, "Bottom", "From -z"),
            ] {
                if ui.small_button(t).on_hover_text(tip).clicked() {
                    self.vp.preset(p);
                }
            }
            if ui
                .small_button("Fit")
                .on_hover_text("Fit to view (double-click)")
                .clicked()
            {
                self.vp.needs_fit = true;
            }
            ui.separator();
            ui.checkbox(&mut self.clip.enabled, "Section");
            if self.clip.enabled {
                let before = self.clip.axis;
                ComboBox::from_id_salt("clip_axis")
                    .width(40.0)
                    .truncate()
                    .selected_text(inspector::AXES[self.clip.axis])
                    .show_ui(ui, |ui| {
                        for a in 0..3 {
                            ui.selectable_value(&mut self.clip.axis, a, inspector::AXES[a]);
                        }
                    });
                if before != self.clip.axis {
                    self.reset_clip_pos();
                }
                let a = self.clip.axis;
                let (lo, hi) = (self.case.domain.min[a], self.case.domain.max[a]);
                ui.add(egui::Slider::new(&mut self.clip.pos, lo..=hi).show_value(false));
                ui.add(
                    egui::DragValue::new(&mut self.clip.pos)
                        .range(lo..=hi)
                        .speed(0.2)
                        .suffix(" mm")
                        .max_decimals(1),
                );
                ui.checkbox(&mut self.clip.flip, "flip");
            }
            ui.separator();
            match self.mode {
                ViewMode::Geometry => {
                    ui.checkbox(&mut self.xray, "X-ray");
                    ui.checkbox(&mut self.show_bcs, "Boundaries");
                }
                ViewMode::Results => {
                    if self.field == Field::Temperature {
                        ui.checkbox(&mut self.show_air, "Air")
                            .on_hover_text("Include the air cells (use with Section)");
                    }
                    let has_vel = self.solved.as_ref().is_some_and(|s| {
                        s.result.snapshots.last().is_some_and(|x| x.vel.is_some())
                    });
                    if has_vel {
                        ui.checkbox(&mut self.vectors, "Vectors").on_hover_text(
                            "Velocity arrows on the section plane (turn on Section)",
                        );
                        if self.vectors {
                            ui.add(
                                egui::DragValue::new(&mut self.vector_density)
                                    .range(5..=120)
                                    .prefix("n "),
                            )
                            .on_hover_text("Arrows per side");
                        }
                    }
                    ComboBox::from_id_salt("palette")
                        .width(80.0)
                        .truncate()
                        .selected_text(self.palette.name())
                        .show_ui(ui, |ui| {
                            for p in Palette::ALL {
                                ui.selectable_value(&mut self.palette, p, p.name());
                            }
                        });
                    ui.checkbox(&mut self.auto_range, "Auto range");
                    if !self.auto_range {
                        let unit = self.field_unit();
                        ui.add(
                            egui::DragValue::new(&mut self.range.0)
                                .speed(0.02)
                                .suffix(unit),
                        );
                        ui.label("…");
                        ui.add(
                            egui::DragValue::new(&mut self.range.1)
                                .speed(0.02)
                                .suffix(unit),
                        );
                    }
                    if let Some(s) = &self.solved {
                        let n = s.result.snapshots.len();
                        if n > 1 {
                            ui.separator();
                            ui.label("Time");
                            let t = s.result.snapshots[self.snapshot.min(n - 1)].time_s;
                            ui.add(
                                egui::Slider::new(&mut self.snapshot, 0..=n - 1).show_value(false),
                            );
                            ui.label(format!("{t:.0} s"));
                        }
                    }
                }
            }
        });
    }

    fn field_unit(&self) -> &'static str {
        match self.field {
            Field::Temperature => " °C",
            Field::Speed => " m/s",
            Field::Pressure => " Pa",
        }
    }

    /// Velocity arrows of the cells next to the section plane.
    fn draw_vectors(&self, ui: &Ui, rect: egui::Rect) {
        let Some(s) = &self.solved else { return };
        let snap = &s.result.snapshots[self.snapshot.min(s.result.snapshots.len() - 1)];
        let Some(vel) = &snap.vel else { return };
        let m = &s.model;
        let g = &m.grid;
        let n = g.n();
        let d = &self.case.domain;
        // plane: the section plane, or mid-y when no section is active
        let (axis, at) = (self.clip.axis, self.clip.pos);
        let k = (0..n[axis])
            .min_by(|&a, &b| {
                (g.center(axis, a) * 1e3 - at)
                    .abs()
                    .total_cmp(&(g.center(axis, b) * 1e3 - at).abs())
            })
            .unwrap_or(0);
        let t = ferrotherm_core::model::tangential(axis);
        let side = [d.max[t[0]] - d.min[t[0]], d.max[t[1]] - d.min[t[1]]];
        let step = side[0].max(side[1]) / self.vector_density.max(2) as f64;
        let vmax = vel
            .iter()
            .map(|u| (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt())
            .fold(0.0f32, f32::max)
            .max(1e-9);
        let len_mm = 0.9 * step as f32;
        let painter = ui.painter_at(rect);
        let mut a = d.min[t[0]] + 0.5 * step;
        while a < d.max[t[0]] {
            let mut b = d.min[t[1]] + 0.5 * step;
            while b < d.max[t[1]] {
                let mut p = [0.0; 3];
                p[axis] = g.center(axis, k) * 1e3;
                p[t[0]] = a;
                p[t[1]] = b;
                let idx = [0, 1, 2].map(|q| locate(&g.nodes[q], p[q] * 1e-3).min(n[q] - 1));
                let c = g.idx(idx[0], idx[1], idx[2]);
                if m.owner[c] == Owner::Fill {
                    let u = vel[c];
                    let sp = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
                    if sp > 0.02 * vmax {
                        let f = len_mm * (sp / vmax).sqrt() / sp;
                        let p0 = p.map(|x| x as f32);
                        let p1 = [p0[0] + u[0] * f, p0[1] + u[1] * f, p0[2] + u[2] * f];
                        let s0 = self.vp.project(rect, p0);
                        let s1 = self.vp.project(rect, p1);
                        let col = self.palette.map(sp / vmax);
                        let stroke = Stroke::new(1.3, col);
                        painter.line_segment([s0, s1], stroke);
                        let dir = s1 - s0;
                        let l = dir.length();
                        if l > 3.0 {
                            let u2 = dir / l;
                            let nrm = egui::vec2(-u2.y, u2.x);
                            let h = (0.35 * l).min(6.0);
                            painter.line_segment([s1, s1 - u2 * h + nrm * 0.5 * h], stroke);
                            painter.line_segment([s1, s1 - u2 * h - nrm * 0.5 * h], stroke);
                        }
                    }
                }
                b += step;
            }
            a += step;
        }
    }

    fn material_color(&self, id: &str) -> Color32 {
        let cat = self
            .library
            .material(id)
            .map(|m| m.category.as_str())
            .unwrap_or("");
        if id.starts_with("cu-") || id.starts_with("brass") || id.starts_with("cuw") {
            return Color32::from_rgb(205, 125, 75);
        }
        match cat {
            "metal" => Color32::from_rgb(168, 174, 184),
            "composite" => Color32::from_rgb(140, 145, 160),
            "pcb" => Color32::from_rgb(46, 125, 70),
            "ceramic" => Color32::from_rgb(232, 226, 210),
            "semiconductor" => Color32::from_rgb(95, 95, 120),
            "joining" => Color32::from_rgb(190, 190, 170),
            "tim" => Color32::from_rgb(225, 120, 165),
            "coating" => Color32::from_rgb(205, 175, 95),
            "fluid" => Color32::from_rgb(120, 170, 225),
            _ => Color32::from_rgb(150, 150, 150),
        }
    }

    fn geometry_quads(&self) -> Vec<Quad> {
        let mut q = Vec::new();
        let alpha = if self.xray { 70 } else { 255 };
        let f = |v: [f64; 3]| v.map(|x| x as f32);
        for (i, b) in self.case.blocks.iter().enumerate() {
            if self.hidden.get(i).copied().unwrap_or(false) {
                continue;
            }
            let Some((lo, hi)) = self.clip.apply(b.min, b.max) else {
                continue;
            };
            let base = match (&b.material, &b.pcb) {
                (Some(m), _) => self.material_color(m),
                (None, Some(_)) => Color32::from_rgb(46, 125, 70),
                _ => Color32::GRAY,
            };
            let c = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha);
            box_quads(f(lo), f(hi), c, Tag::Block(i), &mut q);
        }
        for (i, c) in self.case.components.iter().enumerate() {
            let Ok((lo, hi)) = component_box(c, &self.library) else {
                continue;
            };
            let Some((lo, hi)) = self.clip.apply(lo, hi) else {
                continue;
            };
            box_quads(
                f(lo),
                f(hi),
                Color32::from_rgba_unmultiplied(55, 58, 66, alpha.max(160)),
                Tag::Component(i),
                &mut q,
            );
        }
        for (i, h) in self.case.heatsinks.iter().enumerate() {
            let Ok(blocks) = h.blocks() else { continue };
            let base = self.material_color(&h.material);
            let c = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha);
            for b in blocks {
                if let Some((lo, hi)) = self.clip.apply(b.min, b.max) {
                    box_quads(f(lo), f(hi), c, Tag::HeatSink(i), &mut q);
                }
            }
        }
        for (i, (lo, hi)) in self.fan_boxes() {
            if let Some((lo, hi)) = self.clip.apply(lo, hi) {
                box_quads(
                    f(lo),
                    f(hi),
                    Color32::from_rgba_unmultiplied(70, 110, 230, alpha.max(200)),
                    Tag::Fan(i),
                    &mut q,
                );
            }
        }
        for (i, (lo, hi)) in self.resistance_boxes() {
            if let Some((lo, hi)) = self.clip.apply(lo, hi) {
                box_quads(
                    f(lo),
                    f(hi),
                    Color32::from_rgba_unmultiplied(215, 190, 90, alpha.min(150)),
                    Tag::Resistance(i),
                    &mut q,
                );
            }
        }
        for (i, (lo, hi)) in self.monitor_boxes() {
            if let Some((lo, hi)) = self.clip.apply(lo, hi) {
                box_quads(
                    f(lo),
                    f(hi),
                    Color32::from_rgb(235, 70, 70),
                    Tag::Monitor(i),
                    &mut q,
                );
            }
        }
        q
    }

    fn thin(&self) -> f64 {
        let d = &self.case.domain;
        let diag = (0..3)
            .map(|a| (d.max[a] - d.min[a]).powi(2))
            .sum::<f64>()
            .sqrt();
        (0.004 * diag).max(0.2)
    }

    fn fan_boxes(&self) -> Vec<(usize, BoxMm)> {
        let t = self.thin();
        self.case
            .fans
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let (a, _) = ferrotherm_core::model::direction_of(&f.direction).ok()?;
                let ext = ferrotherm_core::model::fan_extent(f).ok()?;
                let tg = ferrotherm_core::model::tangential(a);
                let mut lo = f.center;
                let mut hi = f.center;
                lo[tg[0]] -= 0.5 * ext[0];
                hi[tg[0]] += 0.5 * ext[0];
                lo[tg[1]] -= 0.5 * ext[1];
                hi[tg[1]] += 0.5 * ext[1];
                lo[a] -= t;
                hi[a] += t;
                Some((i, (lo, hi)))
            })
            .collect()
    }

    fn resistance_boxes(&self) -> Vec<(usize, BoxMm)> {
        let t = 0.5 * self.thin();
        self.case
            .resistances
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let a = ferrotherm_core::model::axis_of(&r.axis).ok()?;
                let tg = ferrotherm_core::model::tangential(a);
                let mut lo = r.center;
                let mut hi = r.center;
                for k in 0..2 {
                    lo[tg[k]] -= 0.5 * r.size[k];
                    hi[tg[k]] += 0.5 * r.size[k];
                }
                lo[a] -= t;
                hi[a] += t;
                Some((i, (lo, hi)))
            })
            .collect()
    }

    fn monitor_boxes(&self) -> Vec<(usize, BoxMm)> {
        let h = 1.5 * self.thin();
        self.case
            .monitors
            .iter()
            .enumerate()
            .map(|(i, m)| (i, (m.position.map(|x| x - h), m.position.map(|x| x + h))))
            .collect()
    }

    fn sel_is_item(&self) -> bool {
        matches!(
            self.sel,
            Sel::Block(_)
                | Sel::Component(_)
                | Sel::Boundary(_)
                | Sel::Fan(_)
                | Sel::Resistance(_)
                | Sel::HeatSink(_)
                | Sel::Monitor(_)
        )
    }

    /// Hidden flag per model block (case blocks first, then heat-sink parts).
    fn hidden_model_blocks(&self, m: &Model) -> Vec<bool> {
        m.blocks
            .iter()
            .map(|b| match b.origin {
                BlockOrigin::Case(j) => self.hidden.get(j).copied().unwrap_or(false),
                BlockOrigin::HeatSink(_) => false,
            })
            .collect()
    }

    fn boundary_quads(&self) -> Vec<Quad> {
        let mut out = Vec::new();
        let d = &self.case.domain;
        for (i, b) in self.case.boundaries.iter().enumerate() {
            if !(self.show_bcs || self.sel == Sel::Boundary(i)) {
                continue;
            }
            let a = b.face.axis();
            let t = b.face.tangential();
            let mut lo = d.min.map(|x| x as f32);
            let mut hi = d.max.map(|x| x as f32);
            if let Some(r) = &b.region {
                for q in 0..2 {
                    lo[t[q]] = r.min[q].max(d.min[t[q]]) as f32;
                    hi[t[q]] = r.max[q].min(d.max[t[q]]) as f32;
                }
            }
            let x = if b.face.is_max() { d.max[a] } else { d.min[a] } as f32;
            lo[a] = x;
            hi[a] = x;
            let col = match b.kind {
                BoundaryKind::Fixed => Color32::from_rgb(70, 120, 255),
                BoundaryKind::Convection => Color32::from_rgb(40, 200, 220),
                BoundaryKind::Radiation => Color32::from_rgb(255, 150, 40),
                BoundaryKind::Adiabatic => Color32::from_rgb(160, 160, 160),
                BoundaryKind::Opening => Color32::from_rgb(120, 230, 120),
                BoundaryKind::Inlet => Color32::from_rgb(60, 160, 255),
                BoundaryKind::Symmetry => Color32::from_rgb(200, 120, 230),
            };
            let alpha = if self.sel == Sel::Boundary(i) {
                120
            } else {
                60
            };
            let mut tmp = Vec::new();
            box_quads(
                lo,
                hi,
                Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha),
                Tag::Boundary(i),
                &mut tmp,
            );
            // keep only the outward-facing quad of this face
            out.extend(
                tmp.into_iter()
                    .filter(|q| q.axis as usize == a && q.positive == b.face.is_max()),
            );
        }
        out
    }

    fn selected_box(&self) -> Option<([f32; 3], [f32; 3])> {
        let f = |v: [f64; 3]| v.map(|x| x as f32);
        match self.sel {
            Sel::Block(i) => self.case.blocks.get(i).map(|b| (f(b.min), f(b.max))),
            Sel::Component(i) => self
                .case
                .components
                .get(i)
                .and_then(|c| component_box(c, &self.library).ok())
                .map(|(lo, hi)| (f(lo), f(hi))),
            Sel::Domain => Some((f(self.case.domain.min), f(self.case.domain.max))),
            Sel::HeatSink(i) => {
                let blocks = self.case.heatsinks.get(i)?.blocks().ok()?;
                let mut lo = [f64::INFINITY; 3];
                let mut hi = [f64::NEG_INFINITY; 3];
                for b in blocks {
                    for a in 0..3 {
                        lo[a] = lo[a].min(b.min[a]);
                        hi[a] = hi[a].max(b.max[a]);
                    }
                }
                Some((f(lo), f(hi)))
            }
            Sel::Fan(i) => self
                .fan_boxes()
                .into_iter()
                .find(|x| x.0 == i)
                .map(|(_, (a, b))| (f(a), f(b))),
            Sel::Resistance(i) => self
                .resistance_boxes()
                .into_iter()
                .find(|x| x.0 == i)
                .map(|(_, (a, b))| (f(a), f(b))),
            Sel::Monitor(i) => self
                .monitor_boxes()
                .into_iter()
                .find(|x| x.0 == i)
                .map(|(_, (a, b))| (f(a), f(b))),
            _ => None,
        }
    }

    fn ensure_cache(&mut self) {
        let Some(s) = &self.solved else {
            self.cache = None;
            return;
        };
        let snapshot = self.snapshot.min(s.result.snapshots.len() - 1);
        let key = CacheKey {
            solve: s.id,
            snapshot,
            clip: (
                self.clip.enabled,
                self.clip.axis,
                self.clip.pos.to_bits(),
                self.clip.flip,
            ),
            hidden: self.hidden.clone(),
            show_air: self.show_air,
            field: self.field,
        };
        if self.cache.as_ref().map(|c| c.key == key).unwrap_or(false) {
            return;
        }
        let snap = &s.result.snapshots[snapshot];
        let m = &s.model;
        let hidden = self.hidden_model_blocks(m);
        let field: Box<dyn Fn(usize) -> Option<f32>> = match self.field {
            Field::Temperature => Box::new(|c| {
                Some(match m.owner[c] {
                    Owner::Component(ci) => snap.t[m.components[ci].node],
                    _ => snap.t[c],
                } as f32)
            }),
            Field::Speed => Box::new(|c| {
                let v = snap.vel.as_ref()?;
                (m.owner[c] == Owner::Fill).then(|| {
                    let u = v[c];
                    (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt()
                })
            }),
            Field::Pressure => Box::new(|c| {
                let p = snap.p.as_ref()?;
                (m.owner[c] == Owner::Fill).then(|| p[c])
            }),
        };
        let show_air = self.show_air || self.field != Field::Temperature;
        let (quads, vmin, vmax) = view3d::result_quads(m, &*field, &hidden, show_air, &self.clip);
        self.next_id += 1;
        self.cache = Some(ResultCache {
            key,
            gen: self.next_id,
            quads,
            vmin,
            vmax,
        });
    }

    fn viewport(&mut self, ui: &mut Ui) {
        let d = &self.case.domain;
        let bounds = (d.min.map(|x| x as f32), d.max.map(|x| x as f32));
        let dark = ui.visuals().dark_mode;
        let bg = if dark {
            Color32::from_rgb(28, 31, 38)
        } else {
            Color32::from_rgb(232, 236, 242)
        };
        let text = ui.visuals().text_color();
        let frame = self.vp.begin(ui, bounds);
        let rect = frame.rect;
        let edge = if dark {
            Color32::from_gray(105)
        } else {
            Color32::from_gray(140)
        };
        let mut lines = Vec::new();
        view3d::box_edges(bounds.0, bounds.1, edge, &mut lines);

        let results = self.mode == ViewMode::Results && self.solved.is_some();
        let painter_geometry = !results && self.xray;
        self.painter_geometry = painter_geometry;
        let mut range = (0.0f32, 1.0f32);
        let bq = self.boundary_quads();
        let bcolor = |q: &Quad| q.color;
        let bstroke = Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.6));
        if results {
            self.ensure_cache();
            let c = self.cache.as_ref().expect("cache");
            range = if self.auto_range {
                (c.vmin, c.vmax)
            } else {
                self.range
            };
            let span = (range.1 - range.0).max(1e-6);
            let pal = self.palette;
            let color = move |q: &Quad| shade(pal.map((q.value - range.0) / span), q.axis);
            let model = &self.solved.as_ref().expect("solved").model;
            let group = |q: &Quad| match q.tag {
                Tag::Cell(cell) => match model.owner[cell] {
                    Owner::Fill => 1,
                    Owner::Block(b) => 2 + b as u32,
                    Owner::Component(ci) => 1_000_000 + ci as u32,
                },
                _ => 0,
            };
            let mut h = std::collections::hash_map::DefaultHasher::new();
            use std::hash::{Hash, Hasher};
            (c.gen, pal as u8, range.0.to_bits(), range.1.to_bits()).hash(&mut h);
            self.vp.render_opaque(
                ui,
                &frame,
                &c.quads,
                &color,
                &group,
                &lines,
                h.finish(),
                bg,
                0.45,
            );
            let auto = (c.vmin, c.vmax);
            if self.auto_range {
                self.range = auto;
            }
            self.vp.draw(
                ui,
                &frame,
                &[Layer {
                    quads: &bq,
                    two_sided: false,
                    outline: Some(bstroke),
                    color: &bcolor,
                    pickable: true,
                }],
            );
        } else {
            let gq = self.geometry_quads();
            let gcolor = |q: &Quad| shade(q.color, q.axis);
            if painter_geometry {
                ui.painter_at(rect).rect_filled(rect, 0.0, bg);
                self.vp
                    .draw_box_edges(ui, rect, bounds.0, bounds.1, Stroke::new(1.0, edge));
                let outline = Stroke::new(1.0, Color32::from_white_alpha(70));
                self.vp.draw(
                    ui,
                    &frame,
                    &[
                        Layer {
                            quads: &gq,
                            two_sided: true,
                            outline: Some(outline),
                            color: &gcolor,
                            pickable: true,
                        },
                        Layer {
                            quads: &bq,
                            two_sided: false,
                            outline: Some(bstroke),
                            color: &bcolor,
                            pickable: true,
                        },
                    ],
                );
            } else {
                // every face is its own outline group: box edges are drawn
                let group = |q: &Quad| match q.tag {
                    Tag::Block(i) => 1 + 8 * i as u32 + q.axis as u32 * 2 + q.positive as u32,
                    Tag::Component(i) => {
                        1_000_000 + 8 * i as u32 + q.axis as u32 * 2 + q.positive as u32
                    }
                    Tag::HeatSink(i) => {
                        2_000_000 + 8 * i as u32 + q.axis as u32 * 2 + q.positive as u32
                    }
                    Tag::Fan(i) => 3_000_000 + 8 * i as u32 + q.axis as u32 * 2 + q.positive as u32,
                    Tag::Resistance(i) => {
                        4_000_000 + 8 * i as u32 + q.axis as u32 * 2 + q.positive as u32
                    }
                    Tag::Monitor(i) => {
                        5_000_000 + 8 * i as u32 + q.axis as u32 * 2 + q.positive as u32
                    }
                    _ => 0,
                };
                let hash = view3d::hash_quads(&gq);
                self.vp
                    .render_opaque(ui, &frame, &gq, &gcolor, &group, &lines, hash, bg, 0.35);
                self.vp.draw(
                    ui,
                    &frame,
                    &[Layer {
                        quads: &bq,
                        two_sided: false,
                        outline: Some(bstroke),
                        color: &bcolor,
                        pickable: true,
                    }],
                );
            }
        }

        // clip plane outline
        if self.clip.enabled {
            let a = self.clip.axis;
            let mut lo = bounds.0;
            let mut hi = bounds.1;
            lo[a] = self.clip.pos as f32;
            hi[a] = self.clip.pos as f32;
            self.vp.draw_box_edges(
                ui,
                rect,
                lo,
                hi,
                Stroke::new(1.0, Color32::from_rgb(255, 210, 0).gamma_multiply(0.7)),
            );
        }
        // selection highlight on top of everything
        if let Some((lo, hi)) = self.selected_box() {
            self.vp.draw_box_edges(
                ui,
                rect,
                lo,
                hi,
                Stroke::new(2.0, Color32::from_rgb(255, 165, 0)),
            );
        }
        self.vp.draw_axes(ui, rect);
        if results && self.vectors && self.clip.enabled && self.field != Field::Pressure {
            self.draw_vectors(ui, rect);
        }
        if results {
            let unit = self.field_unit().trim();
            view3d::draw_colorbar(ui, rect, self.palette, range.0, range.1, unit, text);
        }

        let painter = ui.painter_at(rect);
        let fname = match self.field {
            Field::Temperature => "Temperature",
            Field::Speed => "Air speed",
            Field::Pressure => "Pressure (relative to ambient)",
        };
        let label = if results {
            if self.stale {
                format!("{fname} — results out of date")
            } else {
                fname.to_string()
            }
        } else {
            "Geometry".to_string()
        };
        painter.text(
            rect.left_top() + egui::vec2(10.0, 8.0),
            Align2::LEFT_TOP,
            label,
            FontId::proportional(13.0),
            if self.stale && results { WARN } else { text },
        );
        painter.text(
            rect.left_bottom() + egui::vec2(80.0, -10.0),
            Align2::LEFT_BOTTOM,
            "drag: rotate · right-drag/shift: pan · wheel: zoom · double-click: fit",
            FontId::proportional(11.0),
            ui.visuals().weak_text_color(),
        );

        // hover probe and click selection
        let hover = frame.response.hover_pos();
        let hit = hover.and_then(|p| self.hit(rect, p, results));
        if let (Some(pos), Some(h)) = (hover, hit) {
            if !frame.response.dragged() {
                if let Some(tip) = self.probe_text(h) {
                    let galley =
                        painter.layout_no_wrap(tip, FontId::proportional(12.0), Color32::WHITE);
                    let at = pos + egui::vec2(14.0, 14.0);
                    let r = egui::Rect::from_min_size(at, galley.size()).expand(5.0);
                    painter.rect_filled(r, 4.0, Color32::from_black_alpha(200));
                    painter.galley(at, galley, Color32::WHITE);
                }
            }
        }
        if frame.response.clicked() {
            if let Some(h) = hit {
                if let Some(s) = self.tag_to_sel(h) {
                    self.sel = s;
                }
            }
        }
    }

    /// What is under the pointer: overlays first, then the opaque raster.
    fn hit(&self, rect: egui::Rect, p: egui::Pos2, results: bool) -> Option<(Src, usize)> {
        if let Some((layer, qi)) = self.vp.pick(p) {
            let src = if self.painter_geometry && layer == 0 {
                Src::Geometry
            } else {
                Src::Boundary
            };
            return Some((src, qi));
        }
        if self.painter_geometry {
            return None;
        }
        self.vp
            .pick_opaque(rect, p)
            .map(|qi| (if results { Src::Results } else { Src::Geometry }, qi))
    }

    fn quad_tag(&self, (src, qi): (Src, usize)) -> Option<Tag> {
        match src {
            Src::Results => self
                .cache
                .as_ref()
                .and_then(|c| c.quads.get(qi))
                .map(|q| q.tag),
            Src::Geometry => self.geometry_quads().get(qi).map(|q| q.tag),
            Src::Boundary => self.boundary_quads().get(qi).map(|q| q.tag),
        }
    }

    fn tag_to_sel(&self, h: (Src, usize)) -> Option<Sel> {
        match self.quad_tag(h)? {
            Tag::Block(i) => Some(Sel::Block(i)),
            Tag::Component(i) => Some(Sel::Component(i)),
            Tag::Boundary(i) => Some(Sel::Boundary(i)),
            Tag::Fan(i) => Some(Sel::Fan(i)),
            Tag::Resistance(i) => Some(Sel::Resistance(i)),
            Tag::HeatSink(i) => Some(Sel::HeatSink(i)),
            Tag::Monitor(i) => Some(Sel::Monitor(i)),
            Tag::Cell(c) => {
                let s = self.solved.as_ref()?;
                match s.model.owner[c] {
                    Owner::Block(b) => match s.model.blocks.get(b)?.origin {
                        BlockOrigin::Case(j) => {
                            let name = &s.case.blocks.get(j)?.name;
                            self.case
                                .blocks
                                .iter()
                                .position(|x| &x.name == name)
                                .map(Sel::Block)
                        }
                        BlockOrigin::HeatSink(j) => {
                            let name = &s.case.heatsinks.get(j)?.name;
                            self.case
                                .heatsinks
                                .iter()
                                .position(|x| &x.name == name)
                                .map(Sel::HeatSink)
                        }
                    },
                    Owner::Component(ci) => {
                        let name = &s.case.components.get(ci)?.name;
                        self.case
                            .components
                            .iter()
                            .position(|x| &x.name == name)
                            .map(Sel::Component)
                    }
                    Owner::Fill => None,
                }
            }
        }
    }

    fn probe_text(&self, h: (Src, usize)) -> Option<String> {
        match self.quad_tag(h)? {
            Tag::Block(i) => {
                let b = self.case.blocks.get(i)?;
                let what = match (&b.material, &b.pcb) {
                    (Some(m), _) => self
                        .library
                        .material(m)
                        .map(|x| x.name.clone())
                        .unwrap_or(m.clone()),
                    _ => "PCB stack-up".into(),
                };
                Some(format!("{}\n{}", b.name, what))
            }
            Tag::Component(i) => {
                let c = self.case.components.get(i)?;
                Some(format!("{}\n{} · {:.2} W", c.name, c.package, c.power_w))
            }
            Tag::Fan(i) => {
                let f = self.case.fans.get(i)?;
                Some(format!("{}\nfan blowing {}", f.name, f.direction))
            }
            Tag::Resistance(i) => {
                let r = self.case.resistances.get(i)?;
                Some(format!("{}\ngrille ζ = {:.2}", r.name, r.loss.zeta))
            }
            Tag::HeatSink(i) => {
                let h = self.case.heatsinks.get(i)?;
                Some(format!("{}\nheat sink, {} fins", h.name, h.fin_count))
            }
            Tag::Monitor(i) => {
                let m = self.case.monitors.get(i)?;
                Some(format!("{}\nmonitor point", m.name))
            }
            Tag::Boundary(i) => {
                let b = self.case.boundaries.get(i)?;
                Some(format!(
                    "{}\n{:?}",
                    if b.name.is_empty() {
                        b.face.name()
                    } else {
                        &b.name
                    },
                    b.kind
                ))
            }
            Tag::Cell(c) => {
                let s = self.solved.as_ref()?;
                let snap = &s.result.snapshots[self.snapshot.min(s.result.snapshots.len() - 1)];
                let g = &s.model.grid;
                let p = g.ijk(c);
                let pos = [0, 1, 2].map(|a| g.center(a, p[a]) * 1e3);
                let (name, t) = match s.model.owner[c] {
                    Owner::Block(b) => (s.model.blocks[b].name.clone(), snap.t[c]),
                    Owner::Component(ci) => (
                        format!("{} (junction)", s.model.components[ci].name),
                        snap.t[s.model.components[ci].node],
                    ),
                    Owner::Fill => (s.case.domain.material.clone(), snap.t[c]),
                };
                let mut extra = String::new();
                if s.model.owner[c] == Owner::Fill {
                    if let Some(v) = &snap.vel {
                        let u = v[c];
                        extra += &format!(
                            "\n{:.3} m/s ({:+.3}, {:+.3}, {:+.3})",
                            (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt(),
                            u[0],
                            u[1],
                            u[2]
                        );
                    }
                    if let Some(p) = &snap.p {
                        extra += &format!("\n{:+.3} Pa", p[c]);
                    }
                }
                Some(format!(
                    "{t:.2} °C{extra}\n{name}\n({:.1}, {:.1}, {:.1}) mm",
                    pos[0], pos[1], pos[2]
                ))
            }
        }
    }

    // ------------------------------------------------------------ UI: bottom

    fn bottom_panel(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.bottom, BottomTab::Results, "Results");
            ui.selectable_value(&mut self.bottom, BottomTab::Convergence, "Convergence");
            let errs = self.log.iter().filter(|l| l.level == Level::Error).count();
            let label = if errs > 0 {
                RichText::new(format!("Messages ({errs} errors)")).color(FAIL)
            } else {
                RichText::new("Messages")
            };
            ui.selectable_value(&mut self.bottom, BottomTab::Messages, label);
            if self.bottom == BottomTab::Results && self.solved.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Export results…").clicked() {
                        self.export_results();
                    }
                });
            }
        });
        ui.separator();
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .stick_to_bottom(self.bottom == BottomTab::Messages)
            .show(ui, |ui| match self.bottom {
                BottomTab::Results => match &self.solved {
                    Some(s) => {
                        results::summary(ui, s, self.stale);
                        ui.add_space(6.0);
                        let mut sel = self.sel;
                        results::tables(ui, s, &mut sel);
                        self.sel = sel;
                    }
                    None => {
                        ui.label(RichText::new("No results yet: press Solve (F5).").weak());
                    }
                },
                BottomTab::Convergence => {
                    let live = self.job.is_some();
                    let hist: Vec<(usize, [f64; 3])> = if live || self.solved.is_none() {
                        self.live_residuals.clone()
                    } else {
                        self.solved
                            .as_ref()
                            .and_then(|s| s.result.flow.as_ref())
                            .map(|f| {
                                f.residuals
                                    .iter()
                                    .enumerate()
                                    .map(|(i, r)| (i + 1, *r))
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                    if hist.is_empty() {
                        ui.label(
                            RichText::new(
                                "Residuals appear here while a flow (CFD) case is solving.",
                            )
                            .weak(),
                        );
                    } else {
                        let tol = self.case.flow.as_ref().map(|f| f.tolerance).unwrap_or(1e-3);
                        results::residual_chart(ui, &hist, tol);
                    }
                    if let Some(f) = self.results_valid().and_then(|s| s.result.flow.as_ref()) {
                        results::flow_tables(ui, f);
                    }
                }
                BottomTab::Messages => {
                    for l in &self.log {
                        let col = match l.level {
                            Level::Info => ui.visuals().text_color(),
                            Level::Warn => WARN,
                            Level::Error => FAIL,
                        };
                        ui.label(RichText::new(&l.text).color(col).monospace());
                    }
                }
            });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(job) = &self.job {
                ui.spinner();
                ui.label(&job.status);
                if let Some(f) = job.fraction {
                    ui.add(egui::ProgressBar::new(f).desired_width(160.0));
                }
                ui.label(format!("{:.0} s", job.started.elapsed().as_secs_f64()));
                if ui.button("Cancel").clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            } else if let Some(i) = &self.check_info {
                ui.label(RichText::new(i).weak());
            } else {
                ui.label(RichText::new("Ready").weak());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "FerroTherm {} · Roany Romano",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .weak()
                    .small(),
                );
            });
        });
    }

    // ------------------------------------------------------------ UI: windows

    fn windows(&mut self, ctx: &egui::Context) {
        if let Some(dlg) = &mut self.import {
            match dlg.show(ctx, &self.library, self.case.domain.min) {
                Some(import_ui::Action::Add(fr, fit)) => {
                    let (nb, nc) = (fr.blocks.len(), fr.components.len());
                    let notes =
                        ferrotherm_io::import::merge_into_case(&mut self.case, fr, fit, 10.0);
                    let name = dlg.path.display().to_string();
                    self.import = None;
                    self.hidden.resize(self.case.blocks.len(), false);
                    self.info(format!("Imported {name}: {nb} blocks, {nc} components"));
                    for n in notes {
                        self.warn(n);
                    }
                    self.vp.needs_fit = true;
                }
                Some(import_ui::Action::Close) => self.import = None,
                None => {}
            }
        }
        if let Some(p) = self.pending.clone() {
            let mut close = false;
            egui::Modal::new(egui::Id::new("unsaved")).show(ctx, |ui| {
                ui.set_width(340.0);
                ui.heading("Unsaved changes");
                ui.label("The model has changes that are not saved.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() && self.save(false) {
                        close = true;
                        self.perform(p.clone());
                    }
                    if ui.button("Discard").clicked() {
                        close = true;
                        self.perform(p.clone());
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.pending = None;
                if p == Pending::Quit && self.allow_close {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
        if self.show_about {
            let mut open = true;
            egui::Window::new("About FerroTherm")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.heading(format!("FerroTherm {}", env!("CARGO_PKG_VERSION")));
                    ui.label("Open-source thermal solver for electronics enclosures.");
                    ui.label("Author: Roany Romano");
                    ui.label("License: MIT OR Apache-2.0");
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(
                            "3D conjugate heat transfer: conduction, air flow (SIMPLEC, Boussinesq, \
                             LVEL turbulence), fans, grilles, heat sinks, internal radiation, \
                             2R and DELPHI compact models, MIL libraries; CAD import from STEP \
                             (OpenCascade), IDF, KiCad, Gerber, IPC-2581 and ODB++.",
                        )
                        .weak(),
                    );
                });
            self.show_about = open;
        }
        if self.show_help {
            let mut open = true;
            egui::Window::new("Quick guide")
                .open(&mut open)
                .default_width(460.0)
                .show(ctx, |ui| {
                    for (h, t) in [
                        ("1. Geometry", "Domain = bounding box, filled with air. Add blocks (chassis, pedestals, gap pads, boards) with + in the tree. Later blocks overwrite earlier ones where they overlap. Units: mm."),
                        ("2. Boards", "Set a block's kind to 'PCB stack-up' and give layers, copper weight and coverage: FerroTherm computes the orthotropic conductivity."),
                        ("3. Components", "Pick a package, position (min corner) and power. θJC/θJB come from the datasheet; MIL-HDBK-217F defaults exist for ceramic packages. Choose a Tj limit (SD-18 derating or custom)."),
                        ("4. Boundaries", "Faces without a boundary are adiabatic. Convection needs h; add a finish for radiation. The environment preset gives the default ambient (e.g. MIL-STD-810H 49 °C)."),
                        ("5. Air flow", "Model > Air flow turns on the CFD solver (fill the domain with 'air'). Add openings (vents) on faces, fans, grilles and heat sinks; monitor points track convergence."),
                        ("6. CAD", "File > Import CAD reads STEP/STL (mechanics) and IDF, KiCad, Gerber+Excellon, IPC-2581, ODB++ (boards: copper map per tile, parts, power list CSV). Drag files onto the window. File > Export geometry writes STEP, STL or IDF."),
                        ("7. Solve", "F5. Results: temperature, air speed and pressure on section planes with velocity arrows; Convergence tab; tables with PASS/FAIL; export VTK for ParaView and CSV."),
                        ("Tips", "Start with 3–4 mm cells, then refine and check that Tj stops changing. Ctrl+Z undoes any edit. Hover any value for details and sources."),
                    ] {
                        ui.label(RichText::new(h).strong());
                        ui.label(t);
                        ui.add_space(4.0);
                    }
                });
            self.show_help = open;
        }
        if self.show_library {
            let mut open = true;
            egui::Window::new("Libraries")
                .open(&mut open)
                .default_size([760.0, 480.0])
                .show(ctx, |ui| self.library_window(ui));
            self.show_library = open;
        }
    }

    fn library_window(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            for (t, n) in [
                (LibTab::Materials, "Materials"),
                (LibTab::Finishes, "Finishes"),
                (LibTab::Packages, "Packages"),
                (LibTab::Environments, "Environments"),
                (LibTab::Derating, "Tj derating"),
            ] {
                ui.selectable_value(&mut self.lib_tab, t, n);
            }
        });
        ui.label(
            RichText::new(
                "Typical handbook values with their sources. Hover a row for details. \
                 Add your own entries with Libraries > Add a library file.",
            )
            .weak()
            .small(),
        );
        ui.separator();
        let lib = &self.library;
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let g = |id: &str| egui::Grid::new(id).striped(true).spacing([14.0, 4.0]);
                match self.lib_tab {
                    LibTab::Materials => {
                        g("lm").show(ui, |ui| {
                            for h in ["Name", "Category", "k [W/(m·K)]", "ρ", "cp", "Specs"] {
                                ui.label(RichText::new(h).weak());
                            }
                            ui.end_row();
                            for m in lib.materials.iter().filter(|m| m.category != "internal") {
                                ui.label(&m.name)
                                    .on_hover_text(inspector::material_info(lib, &m.id));
                                ui.label(&m.category);
                                let k = m.k.as_vec();
                                ui.label(if k[0] == k[1] && k[1] == k[2] {
                                    format!("{}", k[0])
                                } else {
                                    format!("{} / {} / {}", k[0], k[1], k[2])
                                });
                                ui.label(format!("{:.0}", m.density));
                                ui.label(format!("{:.0}", m.cp));
                                ui.label(m.specs.join(", "));
                                ui.end_row();
                            }
                        });
                    }
                    LibTab::Finishes => {
                        g("lf").show(ui, |ui| {
                            for h in ["Name", "ε", "Specs", "Source"] {
                                ui.label(RichText::new(h).weak());
                            }
                            ui.end_row();
                            for f in &lib.finishes {
                                ui.label(&f.name);
                                ui.label(format!("{}", f.emissivity));
                                ui.label(f.specs.join(", "));
                                ui.label(RichText::new(&f.source).weak());
                                ui.end_row();
                            }
                        });
                    }
                    LibTab::Packages => {
                        g("lp").show(ui, |ui| {
                            for h in ["Name", "θJC", "θJB", "Size [mm]", "Standard"] {
                                ui.label(RichText::new(h).weak());
                            }
                            ui.end_row();
                            for p in &lib.packages {
                                ui.label(&p.name).on_hover_text(&p.notes);
                                ui.label(
                                    p.r_jc.map(|r| format!("{r}")).unwrap_or("datasheet".into()),
                                );
                                ui.label(p.r_jb.map(|r| format!("{r}")).unwrap_or("–".into()));
                                ui.label(
                                    p.size_mm
                                        .map(|s| format!("{} × {} × {}", s[0], s[1], s[2]))
                                        .unwrap_or("user".into()),
                                );
                                ui.label(RichText::new(&p.standard).weak());
                                ui.end_row();
                            }
                        });
                    }
                    LibTab::Environments => {
                        g("le").show(ui, |ui| {
                            for h in ["Name", "Ambient [°C]", "Induced [°C]", "Source"] {
                                ui.label(RichText::new(h).weak());
                            }
                            ui.end_row();
                            for e in &lib.environments {
                                ui.label(&e.name).on_hover_text(&e.notes);
                                ui.label(format!("{} … {}", e.ambient_min_c, e.ambient_max_c));
                                ui.label(format!("{} … {}", e.induced_min_c, e.induced_max_c));
                                ui.label(RichText::new(&e.source).weak());
                                ui.end_row();
                            }
                        });
                    }
                    LibTab::Derating => {
                        g("ld").show(ui, |ui| {
                            for h in ["Rule", "Tj max [°C]", "Source"] {
                                ui.label(RichText::new(h).weak());
                            }
                            ui.end_row();
                            for d in &lib.derating {
                                ui.label(&d.name);
                                ui.label(format!("{}", d.tj_max_c));
                                ui.label(RichText::new(&d.source).weak());
                                ui.end_row();
                            }
                        });
                    }
                }
            });
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled".into());
        let t = format!(
            "{}{} — FerroTherm",
            name,
            if self.dirty { " •" } else { "" }
        );
        if t != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(t.clone()));
            self.title = t;
        }
    }
}

impl eframe::App for FerroApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_job();
        self.shortcuts(&ctx);

        // dropped files
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for p in dropped {
            let is_toml = p
                .extension()
                .map(|e| e.eq_ignore_ascii_case("toml"))
                .unwrap_or(false);
            if is_toml {
                self.request(Pending::OpenPath(p));
                break;
            } else if import_ui::is_cad(&p) && self.import.is_none() {
                self.open_import(p, &ctx);
                break;
            }
        }

        // window close with unsaved changes
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty && !self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
        }

        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::bottom("results")
            .resizable(true)
            .default_size(230.0)
            .min_size(80.0)
            .show(ui, |ui| self.bottom_panel(ui));
        egui::Panel::left("tree")
            .resizable(true)
            .default_size(230.0)
            .min_size(160.0)
            .max_size(380.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.tree(ui));
            });
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(380.0)
            .min_size(280.0)
            .max_size(560.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.inspector(ui));
            });
        egui::CentralPanel::default().show(ui, |ui| {
            self.view_toolbar(ui);
            self.viewport(ui);
        });
        self.windows(&ctx);
        self.track_changes(&ctx);
        self.update_title(&ctx);
        if self.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_case_template_parses_and_builds() {
        let c = Case::from_toml(NEW_CASE).unwrap();
        let lib = Library::builtin();
        let sim = Simulation::with_library(c, lib).unwrap();
        let out = sim.run(|_| {}).unwrap();
        assert!(out.report.energy_imbalance_pct.abs() < 1e-3);
    }
}
