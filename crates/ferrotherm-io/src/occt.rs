// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! STEP import through OpenCascade (OCCT 8, compiled to WebAssembly and run by
//! wasmtime via the `occt-wasm` crate — no C++ toolchain or system library).
//!
//! The file is loaded into an XCAF document (names, assembly structure,
//! placements), tessellated by OCCT and exported as binary glTF in memory; the
//! glTF node tree is then walked to produce one [`StepBody`] per part
//! instance, in mm, global coordinates. Instance names are the assembly
//! component names (for PCB exports from KiCad/Altium these are the
//! reference designators).
//!
//! Compiling the 22 MB WASM module takes ~30 s, so the compiled module is
//! cached on disk (user cache folder) and later starts take well under a
//! second.

use crate::step::{StepBody, StepModel, Xf, V3};
use occt_wasm::wasmtime::{Engine, Module};
use occt_wasm::OcctKernel;
use std::path::PathBuf;
use std::sync::OnceLock;

static MODULE: OnceLock<Result<Module, String>> = OnceLock::new();

/// Folder used for the compiled-module cache.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("FERROTHERM_CACHE") {
        return Some(PathBuf::from(d));
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("FerroTherm"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|d| PathBuf::from(d).join("Library/Caches/FerroTherm"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|d| d.join("ferrotherm"))
    }
}

fn cache_file() -> Option<PathBuf> {
    // keyed by our version + target: the module bytes only change when the
    // dependency set (Cargo.lock) changes, which goes with a new release
    let key = format!(
        "occt-{}-{}-{}.cwasm",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    cache_dir().map(|d| d.join(key))
}

fn module() -> Result<&'static Module, String> {
    MODULE
        .get_or_init(|| {
            let engine = Engine::new(&OcctKernel::engine_config()).map_err(|e| e.to_string())?;
            let cache = cache_file();
            if let Some(f) = &cache {
                if f.is_file() {
                    // SAFETY: the file is written only by this function below
                    // (our own compiled module) in the user's cache folder;
                    // wasmtime additionally checks engine/version compatibility.
                    if let Ok(m) = unsafe { Module::deserialize_file(&engine, f) } {
                        return Ok(m);
                    }
                }
            }
            let m = OcctKernel::compile(&engine).map_err(|e| e.to_string())?;
            if let Some(f) = &cache {
                if let (Some(dir), Ok(bytes)) = (f.parent(), m.serialize()) {
                    let _ = std::fs::create_dir_all(dir);
                    let tmp = f.with_extension(format!("tmp{}", std::process::id()));
                    if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, f).is_err() {
                        let _ = std::fs::remove_file(&tmp);
                    }
                }
            }
            Ok(m)
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// True when the compiled kernel is already available (no 30 s first start).
pub fn is_warm() -> bool {
    MODULE.get().is_some() || cache_file().is_some_and(|f| f.is_file())
}

/// Compile (or load from cache) the kernel ahead of time, e.g. in a
/// background thread at GUI start-up.
pub fn warm_up() -> Result<(), String> {
    module().map(|_| ())
}

/// A fresh kernel instance (own memory, ~1 ms once the module is compiled).
pub fn kernel() -> Result<OcctKernel, String> {
    OcctKernel::from_module(module()?).map_err(|e| e.to_string())
}

/// Import a STEP file with OCCT. `tolerance_mm`: chordal deflection.
pub fn read_step(text: &str, tolerance_mm: f64) -> Result<StepModel, String> {
    let mut k = kernel()?;
    let doc = k.xcaf_import_step(text).map_err(|e| e.to_string())?;
    let glb = k
        .xcaf_export_glb(doc, tolerance_mm.max(1e-3), 0.5)
        .map_err(|e| e.to_string())?;
    let _ = k.xcaf_close(doc);
    drop(k);
    let mut m = from_glb(&glb)?;
    m.warnings.retain(|w| !w.is_empty());
    Ok(m)
}

/// Write a STEP file of boxes using OCCT (AP214, real B-rep solids) — used
/// to cross-check the built-in writer.
pub fn boxes_to_step(boxes: &[(String, V3, V3)]) -> Result<String, String> {
    let mut k = kernel()?;
    let doc = k.xcaf_new_document().map_err(|e| e.to_string())?;
    let mut shapes = Vec::new();
    for (_, a, b) in boxes {
        let s = k
            .make_box_from_corners(a[0], a[1], a[2], b[0], b[1], b[2])
            .map_err(|e| e.to_string())?;
        shapes.push(s);
    }
    for (s, (n, _, _)) in shapes.iter().zip(boxes) {
        let l = k.xcaf_add_shape(doc, *s).map_err(|e| e.to_string())?;
        let _ = k.xcaf_set_name(doc, l, n);
    }
    k.xcaf_export_step(doc).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ glTF

fn json_err<E: std::fmt::Display>(e: E) -> String {
    format!("glTF: {e}")
}

/// Convert an in-memory binary glTF (as written by OCCT, metres, Z up) into
/// bodies in mm.
pub fn from_glb(glb: &[u8]) -> Result<StepModel, String> {
    use serde_json::Value;
    if glb.len() < 20 || &glb[..4] != b"glTF" {
        return Err("not a binary glTF".into());
    }
    let u32_at = |o: usize| u32::from_le_bytes([glb[o], glb[o + 1], glb[o + 2], glb[o + 3]]) as usize;
    let mut off = 12;
    let mut json: Option<Value> = None;
    let mut bin: &[u8] = &[];
    while off + 8 <= glb.len() {
        let len = u32_at(off);
        let ty = u32_at(off + 4);
        let end = (off + 8 + len).min(glb.len());
        let chunk = &glb[off + 8..end];
        match ty {
            0x4E4F_534A => json = Some(serde_json::from_slice(chunk).map_err(json_err)?),
            0x004E_4942 => bin = chunk,
            _ => {}
        }
        off = end;
    }
    let j = json.ok_or("glTF: no JSON chunk")?;
    let arr = |k: &str| j.get(k).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let nodes = arr("nodes");
    let meshes = arr("meshes");
    let accessors = arr("accessors");
    let views = arr("bufferViews");

    let accessor = |i: usize| -> Result<(Vec<f64>, usize), String> {
        // returns flat values and component count
        let a = accessors.get(i).ok_or("glTF: bad accessor")?;
        let count = a["count"].as_u64().unwrap_or(0) as usize;
        let comps = match a["type"].as_str().unwrap_or("") {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            t => return Err(format!("glTF: accessor type {t}")),
        };
        let ct = a["componentType"].as_u64().unwrap_or(0);
        let csize = match ct {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            _ => return Err(format!("glTF: component type {ct}")),
        };
        let v = views
            .get(a["bufferView"].as_u64().ok_or("glTF: sparse accessor")? as usize)
            .ok_or("glTF: bad view")?;
        let base = v["byteOffset"].as_u64().unwrap_or(0) as usize + a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let stride = v["byteStride"].as_u64().map(|s| s as usize).unwrap_or(comps * csize);
        let mut out = Vec::with_capacity(count * comps);
        for e in 0..count {
            for c in 0..comps {
                let o = base + e * stride + c * csize;
                if o + csize > bin.len() {
                    return Err("glTF: accessor out of range".into());
                }
                let b = &bin[o..o + csize];
                out.push(match ct {
                    5120 => b[0] as i8 as f64,
                    5121 => b[0] as f64,
                    5122 => i16::from_le_bytes([b[0], b[1]]) as f64,
                    5123 => u16::from_le_bytes([b[0], b[1]]) as f64,
                    5125 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
                    _ => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
                });
            }
        }
        Ok((out, comps))
    };

    let local = |n: &Value| -> Xf {
        if let Some(m) = n["matrix"].as_array() {
            let g = |i: usize| m.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0);
            // column-major 4x4
            return Xf {
                r: [[g(0), g(4), g(8)], [g(1), g(5), g(9)], [g(2), g(6), g(10)]],
                t: [g(12), g(13), g(14)],
            };
        }
        let v3 = |k: &str, d: f64| -> V3 {
            let a = n[k].as_array();
            let g = |i| a.and_then(|a| a.get(i)).and_then(|v: &Value| v.as_f64()).unwrap_or(d);
            [g(0), g(1), g(2)]
        };
        let t = v3("translation", 0.0);
        let s = v3("scale", 1.0);
        let q = n["rotation"].as_array();
        let qg = |i: usize, d: f64| q.and_then(|a| a.get(i)).and_then(|v| v.as_f64()).unwrap_or(d);
        let (x, y, z, w) = (qg(0, 0.0), qg(1, 0.0), qg(2, 0.0), qg(3, 1.0));
        let r = [
            [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
            [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
            [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
        ];
        let mut rs = r;
        for row in rs.iter_mut() {
            for (c, v) in row.iter_mut().enumerate() {
                *v *= s[c];
            }
        }
        Xf { r: rs, t }
    };

    let to_mm = Xf {
        r: [[1000.0, 0.0, 0.0], [0.0, 1000.0, 0.0], [0.0, 0.0, 1000.0]],
        t: [0.0; 3],
    };
    let mut model = StepModel {
        unit_mm: 1.0,
        ..Default::default()
    };
    let roots: Vec<usize> = j["scenes"]
        .as_array()
        .and_then(|s| s.get(j["scene"].as_u64().unwrap_or(0) as usize))
        .and_then(|s| s["nodes"].as_array())
        .map(|a| a.iter().filter_map(|v| v.as_u64().map(|x| x as usize)).collect())
        .unwrap_or_else(|| (0..nodes.len()).collect());

    let mut products = std::collections::HashSet::new();
    // (node, transform, names on the path)
    let mut stack: Vec<(usize, Xf, Vec<String>)> = roots.iter().rev().map(|&r| (r, to_mm, Vec::new())).collect();
    let mut guard = 0usize;
    while let Some((ni, parent, mut names)) = stack.pop() {
        guard += 1;
        if guard > 5_000_000 {
            return Err("glTF: node graph too large or cyclic".into());
        }
        let n = nodes.get(ni).ok_or("glTF: bad node index")?;
        let xf = parent.then(&local(n));
        let nm = n["name"].as_str().unwrap_or("").to_string();
        names.push(nm);
        if let Some(mi) = n["mesh"].as_u64() {
            model.occurrences += 1;
            let mesh = meshes.get(mi as usize).ok_or("glTF: bad mesh index")?;
            let mname = mesh["name"].as_str().unwrap_or("").to_string();
            products.insert(mname.clone());
            let mut tris = Vec::new();
            for p in mesh["primitives"].as_array().map(|v| v.as_slice()).unwrap_or(&[]) {
                if p["mode"].as_u64().unwrap_or(4) != 4 {
                    continue;
                }
                let Some(pi) = p["attributes"]["POSITION"].as_u64() else { continue };
                let (pos, c) = accessor(pi as usize)?;
                if c != 3 {
                    continue;
                }
                let nv = pos.len() / 3;
                let pts: Vec<V3> = (0..nv).map(|v| xf.apply([pos[3 * v], pos[3 * v + 1], pos[3 * v + 2]])).collect();
                let idx: Vec<usize> = match p["indices"].as_u64() {
                    Some(ii) => accessor(ii as usize)?.0.into_iter().map(|x| x as usize).collect(),
                    None => (0..nv).collect(),
                };
                for t in idx.chunks_exact(3) {
                    if t.iter().all(|&i| i < nv) {
                        tris.push([pts[t[0]], pts[t[1]], pts[t[2]]]);
                    }
                }
            }
            if tris.is_empty() {
                continue;
            }
            let mut mn = [f64::INFINITY; 3];
            let mut mx = [f64::NEG_INFINITY; 3];
            for t in &tris {
                for p in t {
                    for a in 0..3 {
                        mn[a] = mn[a].min(p[a]);
                        mx[a] = mx[a].max(p[a]);
                    }
                }
            }
            // instance name: nearest meaningful name below the top node
            let meaningful = |s: &str| !s.is_empty() && !s.starts_with("=>");
            let name = names
                .iter()
                .skip(1)
                .rev()
                .find(|s| meaningful(s))
                .cloned()
                .or_else(|| meaningful(&mname).then(|| mname.clone()))
                .or_else(|| names.first().filter(|s| meaningful(s)).cloned())
                .unwrap_or_else(|| format!("body{}", model.bodies.len() + 1));
            let mut path: Vec<&str> = names.iter().map(|s| s.as_str()).filter(|s| meaningful(s)).collect();
            if meaningful(&mname) && path.last() != Some(&mname.as_str()) {
                path.push(&mname);
            }
            model.bodies.push(StepBody {
                name,
                path: path.join("/"),
                tris,
                min: mn,
                max: mx,
                bbox_only: false,
            });
        }
        if let Some(ch) = n["children"].as_array() {
            for c in ch.iter().rev() {
                if let Some(c) = c.as_u64() {
                    stack.push((c as usize, xf, names.clone()));
                }
            }
        }
    }
    model.products = products.len();
    if model.bodies.is_empty() {
        model.warnings.push("no solid bodies found in the STEP file".into());
    }
    Ok(model)
}
