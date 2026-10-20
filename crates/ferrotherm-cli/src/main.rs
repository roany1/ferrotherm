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
