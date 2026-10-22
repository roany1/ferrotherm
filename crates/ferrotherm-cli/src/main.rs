// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ferrotherm_core::pcb::PcbStack;
use ferrotherm_core::{Case, Library, Simulation};
use std::path::{Path, PathBuf};

const EXAMPLE_CASE: &str = include_str!("../../../examples/mil_enclosure.toml");

#[derive(Parser)]
#[command(
    name = "ferrotherm",
    version,
    author = "Roany Romano",
    about = "FerroTherm — open-source thermal solver for electronics enclosures"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Extra JSON library file(s) to load on top of the built-in libraries.
    #[arg(long = "library", global = true)]
    libraries: Vec<PathBuf>,
}

#[derive(Subcommand)]
// parsed once at start-up: the size of the Import variant does not matter
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// Solve a case and write results (VTK for ParaView, Markdown/JSON report, junction CSV).
    Run {
        case: PathBuf,
        /// Output directory (default: <case name>_results next to the case).
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(short, long)]
        quiet: bool,
    },
    /// Validate a case and show the grid without solving.
    Check { case: PathBuf },
    /// Write the example MIL enclosure case to a file.
    Init {
        #[arg(default_value = "enclosure.toml")]
        file: PathBuf,
    },
    /// List materials (optionally filtered by category, e.g. metal, pcb, ceramic).
    Materials {
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List component packages and compact-model defaults.
    Packages {
        #[arg(long)]
        json: bool,
    },
    /// List surface finishes (emissivity).
    Finishes {
        #[arg(long)]
        json: bool,
    },
    /// List environment presets (MIL-STD-810H etc.).
    Environments {
        #[arg(long)]
        json: bool,
    },
    /// List junction-temperature derating rules.
    Deratings {
        #[arg(long)]
        json: bool,
    },
    /// Show what a CAD file contains (STEP, STL, IDF, KiCad, Gerber, IPC-2581, ODB++).
    CadInfo {
        /// File, folder or archive
        file: PathBuf,
    },
    /// Import CAD data into a case: a new one, or added to `--into`.
    Import {
        /// File, folder or archive (STEP, STL, IDF, KiCad, Gerber/Excellon, IPC-2581, ODB++)
        file: PathBuf,
        /// Existing case to add the geometry to
        #[arg(long)]
        into: Option<PathBuf>,
        /// Output case (default: the --into case, or <file name>.toml)
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Where the imported geometry goes: board corner (ECAD) or offset (MCAD), mm "x,y,z"
        #[arg(long, value_delimiter = ',', allow_hyphen_values = true)]
        origin: Option<Vec<f64>>,
        /// ECAD: PCB conductivity-map tile size, mm (0 = single block)
        #[arg(long, default_value_t = 10.0)]
        tile: f64,
        /// ECAD: power list CSV (refdes, power_W [, Rjc, Rjb, Tj_max])
        #[arg(long)]
        power: Option<PathBuf>,
        /// ECAD: components as "compact" (2R when Rjc known), "blocks" or "none"
        #[arg(long, default_value = "compact")]
        components: String,
        /// ECAD: ignore parts with a smaller footprint, mm²
        #[arg(long, default_value_t = 4.0)]
        min_area: f64,
        /// MCAD: "box" (bounding box per body) or "voxel:<mm>" (voxelised shape)
        #[arg(long, default_value = "box")]
        shape: String,
        /// MCAD: material of the imported bodies
        #[arg(long, default_value = "al-6061-t6")]
        material: String,
        /// STEP chordal tolerance, mm
        #[arg(long, default_value_t = 0.1)]
        tolerance: f64,
        /// Name prefix for the created objects
        #[arg(long, default_value = "")]
        prefix: String,
        /// Do not grow the domain to fit the imported geometry
        #[arg(long)]
        no_fit: bool,
    },
    /// Export the case geometry: STEP (AP214 solids), STL or IDF 3.0 (.emn + .emp).
    Export {
        case: PathBuf,
        /// Output file; format from the extension (.step/.stp, .stl, .emn)
        out: PathBuf,
    },
    /// Effective conductivity of a PCB stack-up.
    Pcb {
        /// Board thickness, mm
        #[arg(long, default_value_t = 1.6)]
        thickness: f64,
        /// Number of copper layers
        #[arg(long, default_value_t = 4)]
        layers: usize,
        /// Copper weight per layer, oz
        #[arg(long, default_value_t = 1.0)]
        oz: f64,
        /// Copper coverage per layer, 0..1
        #[arg(long, default_value_t = 0.5)]
        coverage: f64,
        /// Thermal-via copper area fraction, 0..1
        #[arg(long, default_value_t = 0.0)]
        vias: f64,
        #[arg(long, default_value = "fr4")]
        dielectric: String,
    },
}

fn library(extra: &[PathBuf]) -> Result<Library> {
    let mut lib = Library::builtin();
    for p in extra {
        lib.merge_file(p)
            .with_context(|| format!("loading library {}", p.display()))?;
    }
    Ok(lib)
}

fn load(case_path: &Path, extra: &[PathBuf]) -> Result<Simulation> {
    let case = Case::from_file(case_path)?;
    let base = case_path.parent().unwrap_or(Path::new("."));
    let mut lib = ferrotherm_core::model::load_library(&case, base)?;
    for p in extra {
        lib.merge_file(p)?;
    }
    Ok(Simulation::with_library(case, lib)?)
}

fn parse_shape(s: &str) -> Result<ferrotherm_io::step::BlockMode> {
    use ferrotherm_io::step::BlockMode;
    if s == "box" {
        return Ok(BlockMode::BoundingBox);
    }
    if let Some(v) = s.strip_prefix("voxel:") {
        return Ok(BlockMode::Voxels(v.parse().context("--shape voxel:<mm>")?));
    }
    anyhow::bail!("--shape: use \"box\" or \"voxel:<mm>\"")
}

/// Minimal case around imported geometry: still air, natural-convection
/// walls (h = 10 W/m²K at 25 °C) — to be edited by the user.
fn new_case_for(file: &Path, fr: &ferrotherm_io::ecad::Fragment) -> Case {
    let title = file
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "imported".into());
    let (lo, hi) = if fr.min[0].is_finite() {
        (fr.min, fr.max)
    } else {
        ([0.0; 3], [100.0; 3])
    };
    let mut text = format!(
        "[meta]\ntitle = \"{}\"\nauthor = \"Roany Romano\"\ndescription = \"imported from {}\"\n\n[domain]\nmin = [{}, {}, {}]\nmax = [{}, {}, {}]\nmaterial = \"air-still\"\n\n[grid]\nmax_cell_mm = 2.0\n",
        title.replace('"', "'"),
        file.file_name().map(|s| s.to_string_lossy().replace('"', "'")).unwrap_or_default(),
        lo[0] - 10.0,
        lo[1] - 10.0,
        lo[2] - 10.0,
        hi[0] + 10.0,
        hi[1] + 10.0,
        hi[2] + 10.0
    );
    for f in ["xmin", "xmax", "ymin", "ymax", "zmin", "zmax"] {
        text += &format!("\n[[boundaries]]\nname = \"{f}\"\nface = \"{f}\"\ntype = \"convection\"\nh = 10.0\nt_ambient_c = 25.0\n");
    }
    Case::from_toml(&text).expect("template case")
}

fn cad_info(file: &Path) -> Result<()> {
    use ferrotherm_io::import::{self, Imported};
    let (src, imp) = import::import_path(file, 0.1).map_err(anyhow::Error::msg)?;
    println!("format: {}", src.label());
    match imp {
        Imported::Board(mut b) => {
            let s = b.summary();
            println!("board: {} ({})", s.name, s.format);
            println!(
                "size: {:.2} × {:.2} mm, thickness {:.3} mm",
                s.size_mm[0], s.size_mm[1], s.thickness_mm
            );
            for (n, t, c) in &s.copper_layers {
                println!(
                    "  copper {n:<20} {:.0} µm  coverage {:.1} %",
                    t * 1000.0,
                    c * 100.0
                );
            }
            println!("components: {}, drills: {}", s.components, s.drills);
            for w in &s.warnings {
                println!("warning: {w}");
            }
        }
        Imported::Mcad(m) => {
            println!(
                "{} bodies, {} products, {} occurrences",
                m.bodies.len(),
                m.products,
                m.occurrences
            );
            for b in &m.bodies {
                println!(
                    "  {:<24} [{:.2}, {:.2}, {:.2}] .. [{:.2}, {:.2}, {:.2}] mm  {} triangles",
                    b.name,
                    b.min[0],
                    b.min[1],
                    b.min[2],
                    b.max[0],
                    b.max[1],
                    b.max[2],
                    b.tris.len()
                );
            }
            for w in &m.warnings {
                println!("warning: {w}");
            }
        }
    }
    Ok(())
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run { case, out, quiet } => {
            let sim = load(&case, &cli.libraries)?;
            let n = sim.model.grid.n();
            if !quiet {
                eprintln!(
                    "FerroTherm {} — {} × {} × {} = {} cells, {} components, {:.3} W",
                    env!("CARGO_PKG_VERSION"),
                    n[0],
                    n[1],
                    n[2],
                    sim.model.ncells,
                    sim.model.components.len(),
                    sim.model.total_power()
                );
                for w in &sim.model.warnings {
                    eprintln!("warning: {w}");
                }
            }
            let res = sim.run(|m| {
                if !quiet {
                    eprintln!("  {m}");
                }
            })?;
            let stem = case
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "case".into());
            let dir = out.unwrap_or_else(|| case.with_file_name(format!("{stem}_results")));
            ferrotherm_core::export::write_results(
                &dir,
                &stem,
                &sim.model,
                &res.result,
                &res.report,
            )?;
            println!("{}", res.report.to_markdown());
            if !quiet {
                eprintln!("results written to {}", dir.display());
            }
            if res.report.components.iter().any(|c| c.pass == Some(false)) {
                std::process::exit(2);
            }
        }
        Cmd::Check { case } => {
            let sim = load(&case, &cli.libraries)?;
            let m = &sim.model;
            let n = m.grid.n();
            println!("case OK: {}", sim.case.meta.title);
            println!("grid: {} × {} × {} = {} cells", n[0], n[1], n[2], m.ncells);
            println!(
                "blocks: {}, components: {}, boundary patches: {}",
                m.blocks.len(),
                m.components.len(),
                m.boundary.len()
            );
            println!("total power: {:.4} W", m.total_power());
            if let Some(a) = m.ambient_c {
                println!("default ambient: {a:.1} °C");
            }
            for w in &m.warnings {
                println!("warning: {w}");
            }
        }
        Cmd::Init { file } => {
            if file.exists() {
                anyhow::bail!("{} already exists", file.display());
            }
            std::fs::write(&file, EXAMPLE_CASE)?;
            println!("wrote {}", file.display());
        }
        Cmd::Materials { category, json } => {
            let lib = library(&cli.libraries)?;
            let list: Vec<_> = lib
                .materials
                .iter()
                .filter(|m| category.as_deref().map(|c| m.category == c).unwrap_or(true))
                .collect();
            if json {
                return print_json(&list);
            }
            println!(
                "{:<20} {:<14} {:>22} {:>8} {:>7}  specs",
                "id", "category", "k [W/mK] (x,y,z)", "rho", "cp"
            );
            for m in list {
                let k = m.k.as_vec();
                let ks = if k[0] == k[1] && k[1] == k[2] {
                    format!("{:.4}", k[0])
                } else {
                    format!("{:.3},{:.3},{:.3}", k[0], k[1], k[2])
                };
                println!(
                    "{:<20} {:<14} {:>22} {:>8.0} {:>7.0}  {}",
                    m.id,
                    m.category,
                    ks,
                    m.density,
                    m.cp,
                    m.specs.join(", ")
                );
            }
        }
        Cmd::Packages { json } => {
            let lib = library(&cli.libraries)?;
            if json {
                return print_json(&lib.packages);
            }
            println!(
                "{:<30} {:>7} {:>7} {:>7}  {:<22} standard",
                "id", "Rjc", "Rjb", "case", "size [mm]"
            );
            for p in &lib.packages {
                // r_jc missing = must come from the datasheet; r_jb missing = not modelled
                let f = |v: Option<f64>, missing: &str| {
                    v.map(|x| format!("{x:.1}"))
                        .unwrap_or_else(|| missing.into())
                };
                let size = p
                    .size_mm
                    .map(|s| format!("{}x{}x{}", s[0], s[1], s[2]))
                    .unwrap_or_else(|| "user".into());
                println!(
                    "{:<30} {:>7} {:>7} {:>7}  {:<22} {}",
                    p.id,
                    f(p.r_jc, "datash."),
                    f(p.r_jb, "-"),
                    format!("{:?}", p.case_face).to_lowercase(),
                    size,
                    p.standard
                );
            }
        }
        Cmd::Finishes { json } => {
            let lib = library(&cli.libraries)?;
            if json {
                return print_json(&lib.finishes);
            }
            for f in &lib.finishes {
                println!(
                    "{:<20} ε = {:<5} {:<40} {}",
                    f.id,
                    f.emissivity,
                    f.name,
                    f.specs.join(", ")
                );
            }
        }
        Cmd::Environments { json } => {
            let lib = library(&cli.libraries)?;
            if json {
                return print_json(&lib.environments);
            }
            for e in &lib.environments {
                println!(
                    "{:<26} ambient {:>6.1}..{:<6.1} induced {:>6.1}..{:<6.1} °C  {}",
                    e.id,
                    e.ambient_min_c,
                    e.ambient_max_c,
                    e.induced_min_c,
                    e.induced_max_c,
                    e.source
                );
            }
        }
        Cmd::Deratings { json } => {
            let lib = library(&cli.libraries)?;
            if json {
                return print_json(&lib.derating);
            }
            for d in &lib.derating {
                println!("{:<28} Tj ≤ {:>5.1} °C  {}", d.id, d.tj_max_c, d.source);
            }
        }
        Cmd::CadInfo { file } => cad_info(&file)?,
        Cmd::Import {
            file,
            into,
            out,
            origin,
            tile,
            power,
            components,
            min_area,
            shape,
            material,
            tolerance,
            prefix,
            no_fit,
        } => {
            use ferrotherm_io::import::{self, Imported};
            let (src, imp) = import::import_path(&file, tolerance).map_err(anyhow::Error::msg)?;
            let origin = match origin {
                Some(o) if o.len() == 3 => Some([o[0], o[1], o[2]]),
                Some(_) => anyhow::bail!("--origin needs three values: x,y,z"),
                None => None,
            };
            let fr = match imp {
                Imported::Board(mut b) => {
                    if let Some(p) = &power {
                        let n = ferrotherm_io::ecad::apply_power_csv(
                            &mut b,
                            &std::fs::read_to_string(p)?,
                        )
                        .map_err(anyhow::Error::msg)?;
                        eprintln!("power list: {n} parts");
                    }
                    let opt = ferrotherm_io::ecad::ToCaseOptions {
                        tile_mm: tile,
                        origin: origin.unwrap_or([0.0; 3]),
                        components: match components.as_str() {
                            "blocks" => ferrotherm_io::ecad::ComponentMode::Blocks,
                            "none" => ferrotherm_io::ecad::ComponentMode::None,
                            "compact" => ferrotherm_io::ecad::ComponentMode::Compact,
                            o => anyhow::bail!("--components: unknown mode '{o}'"),
                        },
                        min_area_mm2: min_area,
                        prefix: prefix.clone(),
                        ..Default::default()
                    };
                    import::board_fragment(&mut b, &opt)
                }
                Imported::Mcad(m) => {
                    let mode = parse_shape(&shape)?;
                    for w in &m.warnings {
                        eprintln!("warning: {w}");
                    }
                    import::mcad_to_fragment(
                        &m,
                        &import::McadOptions {
                            mode,
                            material: material.clone(),
                            offset: origin.unwrap_or([0.0; 3]),
                            prefix: prefix.clone(),
                            ..Default::default()
                        },
                    )
                }
            };
            let (nb, nc) = (fr.blocks.len(), fr.components.len());
            let mut case = match &into {
                Some(p) => Case::from_file(p)?,
                None => new_case_for(&file, &fr),
            };
            let notes = import::merge_into_case(&mut case, fr, !no_fit, 10.0);
            let explicit = out.is_some() || into.is_some();
            let out = out.or(into).unwrap_or_else(|| file.with_extension("toml"));
            if !explicit && out.exists() {
                anyhow::bail!(
                    "{} already exists: choose the output with -o",
                    out.display()
                );
            }
            std::fs::write(&out, case.to_toml()?)?;
            println!(
                "{}: {nb} blocks, {nc} components -> {}",
                src.label(),
                out.display()
            );
            for n in notes {
                println!("note: {n}");
            }
        }
        Cmd::Export { case, out } => {
            let sim = load(&case, &cli.libraries)?;
            let ext = out
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            match ext.as_str() {
                "step" | "stp" => std::fs::write(
                    &out,
                    ferrotherm_io::export::case_to_step(&sim.case, &sim.library)
                        .map_err(anyhow::Error::msg)?,
                )?,
                "stl" => std::fs::write(
                    &out,
                    ferrotherm_io::export::case_to_stl(&sim.case, &sim.library)
                        .map_err(anyhow::Error::msg)?,
                )?,
                "emn" | "idf" => {
                    let (emn, emp) = ferrotherm_io::export::case_to_idf(&sim.case, &sim.library)
                        .map_err(anyhow::Error::msg)?;
                    std::fs::write(out.with_extension("emn"), emn)?;
                    std::fs::write(out.with_extension("emp"), emp)?;
                }
                _ => anyhow::bail!("unknown export format '.{ext}' (use .step, .stl or .emn)"),
            }
            println!("wrote {}", out.display());
        }
        Cmd::Pcb {
            thickness,
            layers,
            oz,
            coverage,
            vias,
            dielectric,
        } => {
            let lib = library(&cli.libraries)?;
            let s = PcbStack {
                thickness_mm: thickness,
                dielectric,
                copper: "cu-c11000".into(),
                copper_layers: vec![],
                layers: Some(layers),
                copper_oz: Some(oz),
                coverage: Some(coverage),
                via_fraction: vias,
                normal: "z".into(),
            };
            let e = s.effective(&lib)?;
            println!("in-plane k     : {:.3} W/(m K)", e.k_in_plane);
            println!("through-plane k: {:.3} W/(m K)", e.k_through);
            println!("density        : {:.0} kg/m³", e.density);
            println!("cp             : {:.0} J/(kg K)", e.cp);
            println!("copper total   : {:.0} µm", e.copper_thickness_m * 1e6);
        }
    }
    Ok(())
}
