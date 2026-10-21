// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

// No console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
// Index loops mirror the geometry maths (corner k of a quad, axis a, ...).
#![allow(clippy::needless_range_loop)]

mod app;
mod colormap;
mod flow_ui;
mod import_ui;
mod inspector;
mod raster;
mod results;
mod view3d;

use eframe::egui;
use std::path::PathBuf;

fn options(renderer: eframe::Renderer) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("FerroTherm")
            .with_app_id("ferrotherm")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 620.0]),
        renderer,
        ..Default::default()
    }
}

fn run(renderer: eframe::Renderer, path: Option<PathBuf>) -> eframe::Result {
    eframe::run_native(
        "FerroTherm",
        options(renderer),
        Box::new(move |cc| Ok(Box::new(app::FerroApp::new(cc, path)))),
    )
}

fn main() -> eframe::Result {
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    // FERROTHERM_RENDERER=glow forces OpenGL (e.g. old drivers, remote desktop).
    let forced = std::env::var("FERROTHERM_RENDERER").ok();
    if forced.as_deref() == Some("glow") {
        return run(eframe::Renderer::Glow, path);
    }
    match run(eframe::Renderer::Wgpu, path.clone()) {
        Ok(()) => Ok(()),
        Err(e) if forced.is_none() => {
            eprintln!("wgpu renderer failed ({e}); falling back to OpenGL");
            run(eframe::Renderer::Glow, path)
        }
        Err(e) => Err(e),
    }
}
