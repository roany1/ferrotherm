//! Object panel: a palette of the object types of an electronics-cooling
//! model, grouped by category, with search and favourites. Clicking a tile
//! adds that object to the model. Types FerroTherm cannot model yet are shown
//! greyed out so the palette reads as a complete map of what such a tool
//! offers and of what is still missing.

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};
use ferrotherm_core::library::Library;

/// Objects the panel can add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Add {
    Pcb,
    MezzanineCard,
    HeatSpreader,
    HeatSource,
    StandOff,
    Component,
    GapPad,
    HtcBoundary,
    HeatSink,
    Tim,
    Fan,
    FanTray,
    FlowDevice,
    Solid,
    Perforated,
    Face,
    Chassis,
    ChassisSide,
    TestChamber,
    Vent,
    VentOpening,
    Sensor,
    SensorArray,
}

struct Tile {
    name: &'static str,
    add: Option<Add>,
    tip: &'static str,
}

const fn t(name: &'static str, add: Add, tip: &'static str) -> Tile {
    Tile {
        name,
        add: Some(add),
        tip,
    }
}
const fn n(name: &'static str) -> Tile {
    Tile {
        name,
        add: None,
        tip: "",
    }
}

use Add::*;

const COMMON: &[Tile] = &[
    t(
        "PCB",
        Pcb,
        "Layered board block (copper layers smeared into k in-plane / through)",
    ),
    t(
        "Component",
        Component,
        "Package with a 2R (θJC, θJB) or DELPHI network",
    ),
    t("Heatsink", HeatSink, "Parametric plate-fin heat sink"),
    t("TIM", Tim, "Thin thermal-grease layer (0.2 mm block)"),
    t("Fan", Fan, "Axial fan with a P–Q curve"),
    t(
        "Solid Obstruction",
        Solid,
        "Solid block of a library material",
    ),
    t(
        "Chassis",
        Chassis,
        "Six 2 mm aluminium walls inside the domain",
    ),
    t(
        "Vent Opening",
        VentOpening,
        "Free opening at ambient pressure in a domain face",
    ),
];
const PCB: &[Tile] = &[
    t(
        "PCB",
        Pcb,
        "Layered board block (copper layers smeared into k in-plane / through)",
    ),
    n("PCB Hole"),
    n("Via"),
    n("Conductor Layer"),
    n("Daughter Board Socket"),
    t(
        "Mezzanine Card",
        MezzanineCard,
        "Second board 10 mm above the first",
    ),
    t("Heat Spreader", HeatSpreader, "1 mm copper plate"),
    t(
        "Heat Source",
        HeatSource,
        "Small copper block with a fixed power",
    ),
    t("Stand Off", StandOff, "5 × 5 mm aluminium post"),
    n("Board Keepout"),
    n("PCB Conductor"),
    n("PCB Pad"),
    n("Trace"),
    n("Trace Segment"),
    n("Trace Junction"),
    n("PCB Conductor Hole"),
    n("Electrical Boundary"),
    n("Board Connector"),
    n("Net"),
];
const PCB_COMPONENTS: &[Tile] = &[
    t(
        "Component",
        Component,
        "Package with a 2R (θJC, θJB) or DELPHI network",
    ),
    n("Chip Socket"),
    n("Inductor"),
    n("Resistor"),
    n("Capacitor"),
    n("Sub-Component"),
    n("Subcomponent Group"),
    n("Cable Socket"),
    n("Port"),
    n("Cable"),
    n("Cable Segment"),
    n("Cable Junction"),
    t("Gap Pad", GapPad, "1 mm gap pad (3 W/m·K)"),
    n("Substrate"),
    n("Substrate Hole"),
    n("Shielding Can"),
    n("Shielding Can Top"),
    n("Shielding Can Side"),
    n("Battery"),
    t(
        "HTC Boundary",
        HtcBoundary,
        "Heat-transfer-coefficient boundary on a domain face",
    ),
];
const SUBSTRATE: &[Tile] = &[
    n("Substrate"),
    n("Substrate Pad"),
    n("Substrate Hole"),
    n("Substrate Conductor"),
    n("Substrate Conductor Hole"),
    n("Substrate Trace"),
    n("Trace Segment"),
    n("Trace Junction"),
    n("Substrate Net"),
    n("Via"),
];
const HEATSINKS: &[Tile] = &[
    t("Heatsink", HeatSink, "Parametric plate-fin heat sink"),
    n("Heatpipe"),
    n("Heatsink Cutout"),
    n("Thermoelectric Cooler"),
    t("TIM", Tim, "Thin thermal-grease layer (0.2 mm block)"),
    n("TEC Hole"),
];
const FANS: &[Tile] = &[
    t("Fan", Fan, "Axial fan with a P–Q curve"),
    t("Fan Tray", FanTray, "Four fans in a 2 × 2 array"),
    n("Radial Fan"),
    n("Blower"),
    n("Blower Inlet"),
    n("Blower Outlet"),
    t(
        "Flow Device",
        FlowDevice,
        "Fixed volume flow through a plane",
    ),
    n("Controller"),
];
const OBSTRUCTIONS: &[Tile] = &[
    t(
        "Solid Obstruction",
        Solid,
        "Solid block of a library material",
    ),
    n("Obstruction Hole"),
    t(
        "Perforated Obstruction",
        Perforated,
        "Planar resistance (perforated plate, grille, filter)",
    ),
    n("Porous Obstruction"),
    t("Face", Face, "Thin wall: blocks the flow through a plane"),
    n("Polygon Obstruction"),
    n("Polyline Obstruction"),
];
const MULTI_FLUID: &[Tile] = &[
    n("Cooling Duct"),
    n("Cooling Duct Segment"),
    n("Cooling Duct Junction"),
    n("Cold Plate"),
    n("Pumped Return"),
    n("Pumped Supply"),
    n("Pump"),
    n("Damper"),
    t("Vent", Vent, "Opening with a grille loss in a domain face"),
];
const ENCLOSURES: &[Tile] = &[
    t(
        "Chassis",
        Chassis,
        "Six 2 mm aluminium walls inside the domain",
    ),
    t(
        "Test Chamber",
        TestChamber,
        "The domain (air box) around the model",
    ),
    t("Vent", Vent, "Opening with a grille loss in a domain face"),
    t(
        "Vent Opening",
        VentOpening,
        "Free opening at ambient pressure in a domain face",
    ),
    n("Power Supply"),
    t("Chassis Side", ChassisSide, "One 2 mm aluminium wall"),
    n("Test Chamber Side"),
    n("Chassis Bay"),
    n("Chassis Bay Side"),
    n("Chassis Slot"),
    n("Chassis Slot Side"),
    n("Card Bay"),
    n("Card Bay Side"),
    n("Card Bay Slot"),
    n("Card Slot Side"),
    n("Drive Bay"),
    n("Drive Bay Side"),
    n("Drive"),
    n("IT Equipment"),
    n("Heat Exchanger"),
    n("Subassembly"),
];
const RESULT_ANALYSIS: &[Tile] = &[
    t(
        "Sensor",
        Sensor,
        "Monitor point: temperature and speed every iteration",
    ),
    n("Streamline Plot"),
    n("Push Pin"),
    n("Push Pin Overlay Text Box"),
    n("Result Plane"),
    n("Result Face"),
    n("Result Cloud"),
    n("Isosurface"),
    n("Result Volume"),
    n("Result Particle Simulation"),
    n("Grid Control"),
    n("Max and Min"),
    n("Pressure Datum"),
    n("Flow Solution Region"),
    n("Profile Plot"),
    n("Clipping Cuboid"),
    n("Clipping Plane"),
    t("Sensor Array", SensorArray, "Five monitor points in a row"),
    n("Terminal"),
];

const CATEGORIES: &[(&str, &[Tile])] = &[
    ("Commonly Used", COMMON),
    ("PCB", PCB),
    ("PCB Components", PCB_COMPONENTS),
    ("Substrate", SUBSTRATE),
    ("Heatsinks", HEATSINKS),
    ("Fans", FANS),
    ("Obstructions", OBSTRUCTIONS),
    ("Multi-Fluid", MULTI_FLUID),
    ("Enclosures", ENCLOSURES),
    ("Result Analysis", RESULT_ANALYSIS),
];

fn find(name: &str) -> Option<&'static Tile> {
    CATEGORIES
        .iter()
        .flat_map(|(_, ts)| ts.iter())
        .find(|t| t.name == name)
}

/// What the user picked in the panel.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// A standard object type.
    Std(Add),
    /// A component with this library package id.
    Package(String),
    /// A solid block (a thin layer for TIMs) of this library material id.
    Material(String),
}

/// Material categories offered as library parts, with their folder names.
/// Fluids and internal materials are not parts you place.
const MATERIAL_FOLDERS: &[(&str, &str)] = &[
    ("metal", "Metals"),
    ("ceramic", "Ceramics"),
    ("tim", "Thermal interface materials"),
    ("pcb", "PCB laminates"),
    ("semiconductor", "Semiconductors"),
    ("composite", "Composites"),
    ("joining", "Solders and adhesives"),
    ("coating", "Coatings"),
];

/// Panel state kept by the app.
#[derive(Default)]
pub struct ObjectPanel {
    search: String,
    /// Tile names, in the order they were added.
    favourites: Vec<String>,
}

const TILE: Vec2 = Vec2::new(72.0, 72.0);

impl ObjectPanel {
    /// Draw the panel; returns the object to add when a tile was clicked.
    pub fn ui(&mut self, ui: &mut Ui, lib: &Library) -> Option<Pick> {
        let mut out = None;
        let mut pick = None;
        ui.spacing_mut().indent = 8.0;
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);
        let q = self.search.trim().to_lowercase();
        if !q.is_empty() {
            let mut seen: Vec<&str> = Vec::new();
            let hits: Vec<&Tile> = CATEGORIES
                .iter()
                .flat_map(|(_, ts)| ts.iter())
                .filter(|t| t.name.to_lowercase().contains(&q))
                .filter(|t| {
                    let new = !seen.contains(&t.name);
                    seen.push(t.name);
                    new
                })
                .collect();
            let lib_hits = library_rows(lib, &q);
            if hits.is_empty() && lib_hits.is_empty() {
                ui.label(RichText::new("No object matches.").weak());
            }
            self.grid(ui, "search", &hits, &mut out);
            if !lib_hits.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("FerroTherm Library").strong());
                for r in &lib_hits {
                    row(ui, r, &mut pick);
                }
            }
            return out.map(Pick::Std).or(pick);
        }
        egui::CollapsingHeader::new(RichText::new("Standard Objects").strong())
            .id_salt("std-objects")
            .default_open(true)
            .show(ui, |ui| {
                for (i, (cat, tiles)) in CATEGORIES.iter().enumerate() {
                    egui::CollapsingHeader::new(*cat)
                        .id_salt(("objcat", i))
                        .default_open(i == 0)
                        .show(ui, |ui| {
                            let v: Vec<&Tile> = tiles.iter().collect();
                            self.grid(ui, cat, &v, &mut out);
                        });
                }
            });
        egui::CollapsingHeader::new(RichText::new("Favourites").strong())
            .id_salt("fav-objects")
            .default_open(!self.favourites.is_empty())
            .show(ui, |ui| {
                let v: Vec<&Tile> = self.favourites.iter().filter_map(|f| find(f)).collect();
                if v.is_empty() {
                    ui.label(
                        RichText::new("Right-click an object to add it here.")
                            .weak()
                            .small(),
                    );
                }
                self.grid(ui, "fav", &v, &mut out);
            });
        egui::CollapsingHeader::new(RichText::new("FerroTherm Library").strong())
            .id_salt("lib-objects")
            .default_open(false)
            .show(ui, |ui| {
                let rows = library_rows(lib, "");
                egui::CollapsingHeader::new("Component packages")
                    .id_salt("lib-pkgs")
                    .show(ui, |ui| {
                        let mut fams: Vec<&str> = Vec::new();
                        for r in rows.iter().filter(|r| r.package) {
                            if !fams.contains(&r.folder.as_str()) {
                                fams.push(&r.folder);
                            }
                        }
                        for f in fams {
                            egui::CollapsingHeader::new(f)
                                .id_salt(("lib-fam", f))
                                .show(ui, |ui| {
                                    for r in rows.iter().filter(|r| r.package && r.folder == f) {
                                        row(ui, r, &mut pick);
                                    }
                                });
                        }
                    });
                egui::CollapsingHeader::new("Materials")
                    .id_salt("lib-mats")
                    .show(ui, |ui| {
                        for (_, folder) in MATERIAL_FOLDERS {
                            let any = rows.iter().any(|r| !r.package && r.folder == *folder);
                            if !any {
                                continue;
                            }
                            egui::CollapsingHeader::new(*folder)
                                .id_salt(("lib-cat", *folder))
                                .show(ui, |ui| {
                                    for r in
                                        rows.iter().filter(|r| !r.package && r.folder == *folder)
                                    {
                                        row(ui, r, &mut pick);
                                    }
                                });
                        }
                    });
                ui.label(
                    RichText::new(
                        "Built-in data plus any library files loaded from the Libraries menu. \
                         Click a package to add a component, a material to add a solid block.",
                    )
                    .weak()
                    .small(),
                );
            });
        out.map(Pick::Std).or(pick)
    }

    fn grid(&mut self, ui: &mut Ui, salt: &str, tiles: &[&Tile], out: &mut Option<Add>) {
        let cols = ((ui.available_width() / TILE.x).floor() as usize).max(1);
        let mut toggle_fav: Option<&'static str> = None;
        egui::Grid::new(("objgrid", salt))
            .spacing([0.0, 2.0])
            .show(ui, |ui| {
                for (i, t) in tiles.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(TILE, Sense::click());
                    let on = t.add.is_some();
                    let vis = ui.visuals();
                    if on && resp.hovered() {
                        ui.painter()
                            .rect_filled(rect, 4.0, vis.widgets.hovered.weak_bg_fill);
                    }
                    let fg = if on {
                        vis.text_color()
                    } else {
                        vis.weak_text_color().gamma_multiply(0.6)
                    };
                    let icon = Rect::from_center_size(
                        Pos2::new(rect.center().x, rect.top() + 22.0),
                        Vec2::splat(30.0),
                    );
                    draw_icon(ui.painter(), icon, t, fg);
                    let galley = ui.painter().layout(
                        t.name.to_string(),
                        egui::FontId::proportional(10.5),
                        fg,
                        TILE.x - 4.0,
                    );
                    let gpos =
                        Pos2::new(rect.center().x - galley.size().x / 2.0, rect.top() + 42.0);
                    ui.painter().galley(gpos, galley, fg);
                    let resp = if on {
                        resp.on_hover_text(t.tip)
                    } else {
                        resp.on_hover_text("Not available in FerroTherm yet")
                    };
                    if on && resp.clicked() {
                        *out = t.add;
                    }
                    resp.context_menu(|ui| {
                        let fav = self.favourites.iter().any(|f| f == t.name);
                        let label = if fav {
                            "Remove from favourites"
                        } else {
                            "Add to favourites"
                        };
                        if ui.add_enabled(on, egui::Button::new(label)).clicked() {
                            toggle_fav = Some(t.name);
                            ui.close();
                        }
                    });
                    if (i + 1) % cols == 0 {
                        ui.end_row();
                    }
                }
            });
        if let Some(name) = toggle_fav {
            if let Some(k) = self.favourites.iter().position(|f| f == name) {
                self.favourites.remove(k);
            } else {
                self.favourites.push(name.to_string());
            }
        }
    }
}

/// One library entry shown as a row.
struct LibRow {
    package: bool,
    id: String,
    name: String,
    folder: String,
    tip: String,
}

/// Library entries whose name or id contains `q` (all when `q` is empty).
fn library_rows(lib: &Library, q: &str) -> Vec<LibRow> {
    let hit = |id: &str, name: &str| {
        q.is_empty() || id.to_lowercase().contains(q) || name.to_lowercase().contains(q)
    };
    let mut v = Vec::new();
    for p in &lib.packages {
        if !hit(&p.id, &p.name) {
            continue;
        }
        let mut tip = format!("{}\n{}", p.id, p.name);
        if let Some(r) = p.r_jc {
            tip += &format!("\nθJC {r} K/W");
        }
        if let Some(r) = p.r_jb {
            tip += &format!("\nθJB {r} K/W");
        }
        if let Some(s) = p.size_mm {
            tip += &format!("\n{} × {} × {} mm", s[0], s[1], s[2]);
        }
        if !p.standard.is_empty() {
            tip += &format!("\nSource: {}", p.standard);
        }
        v.push(LibRow {
            package: true,
            id: p.id.clone(),
            name: p.name.clone(),
            folder: if p.family.is_empty() {
                "Other".into()
            } else {
                p.family.clone()
            },
            tip,
        });
    }
    for m in &lib.materials {
        let Some((_, folder)) = MATERIAL_FOLDERS.iter().find(|(c, _)| *c == m.category) else {
            continue;
        };
        if !hit(&m.id, &m.name) {
            continue;
        }
        let k = m.k.as_vec();
        let ks = if k[0] == k[1] && k[1] == k[2] {
            format!("{} W/m·K", k[0])
        } else {
            format!("{} / {} / {} W/m·K", k[0], k[1], k[2])
        };
        let mut tip = format!(
            "{}\n{}\nk {ks}, ρ {} kg/m³, cp {} J/kg·K",
            m.id, m.name, m.density, m.cp
        );
        if !m.source.is_empty() {
            tip += &format!("\nSource: {}", m.source);
        }
        v.push(LibRow {
            package: false,
            id: m.id.clone(),
            name: m.name.clone(),
            folder: folder.to_string(),
            tip,
        });
    }
    v
}

fn row(ui: &mut Ui, r: &LibRow, pick: &mut Option<Pick>) {
    let resp = ui
        .horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
            let fg = ui.visuals().text_color();
            let st = Stroke::new(1.0, fg);
            if r.package {
                ui.painter()
                    .rect_stroke(rect.shrink(3.0), 1.0, st, egui::StrokeKind::Middle);
                for k in 0..3 {
                    let x = rect.left() + 5.0 + 3.0 * k as f32;
                    ui.painter().line_segment(
                        [Pos2::new(x, rect.top()), Pos2::new(x, rect.top() + 3.0)],
                        st,
                    );
                    ui.painter().line_segment(
                        [
                            Pos2::new(x, rect.bottom() - 3.0),
                            Pos2::new(x, rect.bottom()),
                        ],
                        st,
                    );
                }
            } else {
                ui.painter()
                    .rect_filled(rect.shrink(3.0), 1.0, fg.gamma_multiply(0.5));
            }
            ui.add(egui::Label::new(&r.name).truncate().sense(Sense::click()))
        })
        .inner
        .on_hover_text(&r.tip);
    if resp.clicked() {
        *pick = Some(if r.package {
            Pick::Package(r.id.clone())
        } else {
            Pick::Material(r.id.clone())
        });
    }
}

/// Simple line icons drawn in FerroTherm's own style.
fn draw_icon(p: &egui::Painter, r: Rect, t: &Tile, fg: Color32) {
    let s = Stroke::new(1.4, fg);
    let thin = Stroke::new(1.0, fg);
    let c = r.center();
    let at = |x: f32, y: f32| Pos2::new(r.left() + x * r.width(), r.top() + y * r.height());
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_two_pos(at(x0, y0), at(x1, y1));
    let line =
        |a: (f32, f32), b: (f32, f32), st: Stroke| p.line_segment([at(a.0, a.1), at(b.0, b.1)], st);
    let ss = egui::StrokeKind::Middle;
    let Some(a) = t.add else {
        // generic placeholder for types not modelled yet
        p.rect_stroke(rect(0.15, 0.15, 0.85, 0.85), 3.0, thin, ss);
        line((0.3, 0.5), (0.7, 0.5), thin);
        return;
    };
    match a {
        Pcb | MezzanineCard => {
            if a == MezzanineCard {
                p.rect_stroke(rect(0.3, 0.05, 0.95, 0.6), 0.0, thin, ss);
            }
            p.rect_stroke(rect(0.05, 0.3, 0.75, 0.95), 0.0, s, ss);
            p.rect_filled(rect(0.15, 0.42, 0.32, 0.6), 0.0, fg);
            for k in 0..3 {
                let y = 0.45 + 0.1 * k as f32;
                line((0.5, y), (0.65, y), thin);
            }
        }
        Component => {
            p.rect_stroke(rect(0.12, 0.22, 0.88, 0.78), 3.0, s, ss);
            for k in 0..7 {
                let x = 0.2 + 0.1 * k as f32;
                line((x, 0.1), (x, 0.22), thin);
                line((x, 0.78), (x, 0.9), thin);
            }
        }
        HeatSink => {
            p.rect_filled(rect(0.1, 0.65, 0.9, 0.8), 0.0, fg);
            for k in 0..7 {
                let x = 0.14 + 0.12 * k as f32;
                line((x, 0.2), (x, 0.65), thin);
            }
        }
        Tim | GapPad => {
            p.rect_stroke(rect(0.1, 0.2, 0.9, 0.36), 0.0, thin, ss);
            p.rect_filled(rect(0.1, 0.44, 0.9, 0.56), 0.0, fg);
            p.rect_stroke(rect(0.1, 0.64, 0.9, 0.8), 0.0, thin, ss);
        }
        Fan | FlowDevice => {
            p.rect_stroke(r.shrink(2.0), 2.0, s, ss);
            p.circle_stroke(c, r.width() * 0.36, thin);
            p.circle_filled(c, r.width() * 0.12, fg);
            if a == FlowDevice {
                line((0.0, 0.5), (1.0, 0.5), s);
                line((0.85, 0.4), (1.0, 0.5), s);
                line((0.85, 0.6), (1.0, 0.5), s);
            }
        }
        FanTray => {
            for (x, y) in [(0.0, 0.0), (0.5, 0.0), (0.0, 0.5), (0.5, 0.5)] {
                let q = rect(x + 0.03, y + 0.03, x + 0.47, y + 0.47);
                p.rect_stroke(q, 1.0, thin, ss);
                p.circle_stroke(q.center(), q.width() * 0.33, thin);
            }
        }
        Solid | StandOff | HeatSpreader | ChassisSide => match a {
            Solid => {
                p.rect_stroke(r.shrink(3.0), 0.0, s, ss);
            }
            StandOff => {
                p.rect_filled(rect(0.2, 0.1, 0.8, 0.2), 0.0, fg);
                p.rect_filled(rect(0.42, 0.2, 0.58, 0.8), 0.0, fg);
                p.rect_filled(rect(0.2, 0.8, 0.8, 0.9), 0.0, fg);
            }
            HeatSpreader => {
                p.rect_stroke(rect(0.1, 0.35, 0.9, 0.65), 0.0, s, ss);
                for k in 0..6 {
                    let x = 0.15 + 0.13 * k as f32;
                    line((x, 0.62), (x + 0.1, 0.38), thin);
                }
            }
            _ => {
                p.rect_stroke(rect(0.38, 0.08, 0.62, 0.92), 0.0, s, ss);
            }
        },
        HeatSource => {
            let col = Color32::from_rgb(232, 110, 40);
            let pts = vec![
                at(0.5, 0.08),
                at(0.78, 0.55),
                at(0.66, 0.9),
                at(0.34, 0.9),
                at(0.22, 0.55),
            ];
            p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
            p.circle_filled(
                at(0.5, 0.72),
                r.width() * 0.12,
                Color32::from_rgb(250, 200, 60),
            );
        }
        HtcBoundary => {
            line((0.2, 0.1), (0.2, 0.9), s);
            let pts: Vec<Pos2> = (0..=16)
                .map(|k| {
                    let u = k as f32 / 16.0;
                    at(
                        0.3 + 0.55 * u,
                        0.5 - 0.15 * (u * std::f32::consts::TAU * 1.5).sin(),
                    )
                })
                .collect();
            p.add(egui::Shape::line(pts, thin));
            line((0.85, 0.5), (0.75, 0.4), thin);
            line((0.85, 0.5), (0.75, 0.6), thin);
        }
        Perforated => {
            p.rect_stroke(r.shrink(3.0), 0.0, s, ss);
            for i in 0..5 {
                for j in 0..5 {
                    p.circle_filled(at(0.22 + 0.14 * i as f32, 0.22 + 0.14 * j as f32), 1.1, fg);
                }
            }
        }
        Face => {
            let pts = vec![at(0.3, 0.2), at(0.9, 0.2), at(0.7, 0.8), at(0.1, 0.8)];
            p.add(egui::Shape::closed_line(pts, s));
        }
        Chassis | TestChamber => {
            let d = 0.22;
            let front = rect(0.08, 0.08 + d, 0.92 - d, 0.92);
            let back = front.translate(Vec2::new(d * r.width(), -d * r.height()));
            let st = if a == TestChamber { thin } else { s };
            p.rect_stroke(back, 0.0, thin, ss);
            p.rect_stroke(front, 0.0, st, ss);
            for (u, v) in [
                (front.left_top(), back.left_top()),
                (front.right_top(), back.right_top()),
                (front.right_bottom(), back.right_bottom()),
            ] {
                p.line_segment([u, v], thin);
            }
            if a == Chassis {
                for k in 0..3 {
                    let y = front.top() + front.height() * (0.3 + 0.2 * k as f32);
                    p.line_segment(
                        [
                            Pos2::new(front.left() + 4.0, y),
                            Pos2::new(front.center().x, y),
                        ],
                        thin,
                    );
                }
            }
        }
        Vent | VentOpening => {
            let q = r.shrink(3.0);
            p.rect_stroke(q, 0.0, s, ss);
            let inner = if a == VentOpening {
                q.shrink(4.0)
            } else {
                q.shrink(2.0)
            };
            if a == VentOpening {
                p.rect_stroke(inner, 0.0, thin, ss);
            }
            for k in 0..6 {
                let y = inner.top() + inner.height() * (k as f32 + 0.5) / 6.0;
                p.line_segment(
                    [
                        Pos2::new(inner.left() + 2.0, y),
                        Pos2::new(inner.right() - 2.0, y),
                    ],
                    thin,
                );
            }
        }
        Sensor | SensorArray => {
            let dots: &[(f32, f32)] = if a == Sensor {
                &[(0.3, 0.75)]
            } else {
                &[(0.2, 0.8), (0.5, 0.8), (0.8, 0.8)]
            };
            for &(x, y) in dots {
                p.circle_filled(at(x, y), 3.5, fg);
            }
            if a == Sensor {
                for k in 1..=2 {
                    let rad = r.width() * 0.25 * k as f32;
                    let pts: Vec<Pos2> = (0..=10)
                        .map(|i| {
                            let ang = -std::f32::consts::FRAC_PI_2 * i as f32 / 10.0;
                            at(0.3, 0.75) + Vec2::new(rad * ang.cos(), rad * ang.sin())
                        })
                        .collect();
                    p.add(egui::Shape::line(pts, s));
                }
            } else {
                line((0.1, 0.55), (0.9, 0.55), thin);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_addable_tile_is_reachable_and_names_are_unique_per_category() {
        for (cat, tiles) in CATEGORIES {
            let mut names: Vec<&str> = tiles.iter().map(|t| t.name).collect();
            names.sort();
            let len = names.len();
            names.dedup();
            assert_eq!(names.len(), len, "duplicate tile in {cat}");
        }
        let all = [
            Pcb,
            MezzanineCard,
            HeatSpreader,
            HeatSource,
            StandOff,
            Component,
            GapPad,
            HtcBoundary,
            HeatSink,
            Tim,
            Fan,
            FanTray,
            FlowDevice,
            Solid,
            Perforated,
            Face,
            Chassis,
            ChassisSide,
            TestChamber,
            Vent,
            VentOpening,
            Sensor,
            SensorArray,
        ];
        for a in all {
            assert!(
                CATEGORIES
                    .iter()
                    .flat_map(|(_, ts)| ts.iter())
                    .any(|t| t.add == Some(a)),
                "{a:?} has no tile"
            );
        }
    }
}
