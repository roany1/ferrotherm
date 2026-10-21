// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! STL (ASCII and binary) import and ASCII export. STL has no units: values
//! are taken as mm (multiply with `scale_to_mm` otherwise).

use crate::step::{StepBody, StepModel, V3};
use std::fmt::Write as _;

fn body(name: String, tris: Vec<[V3; 3]>) -> Option<StepBody> {
    if tris.is_empty() {
        return None;
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
    Some(StepBody {
        path: name.clone(),
        name,
        tris,
        min: mn,
        max: mx,
        bbox_only: false,
    })
}

/// Read an STL file. ASCII files may hold several `solid` sections, each
/// becoming one body; a binary file is one body named `default_name`.
pub fn read(data: &[u8], default_name: &str, scale_to_mm: f64) -> Result<StepModel, String> {
    let mut m = StepModel {
        unit_mm: scale_to_mm,
        ..Default::default()
    };
    let binary_len = |d: &[u8]| -> Option<usize> {
        if d.len() < 84 {
            return None;
        }
        let n = u32::from_le_bytes([d[80], d[81], d[82], d[83]]) as usize;
        (d.len() == 84 + 50 * n).then_some(n)
    };
    let s = scale_to_mm;
    if let Some(n) = binary_len(data) {
        let f = |o: usize| {
            f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]) as f64 * s
        };
        let mut tris = Vec::with_capacity(n);
        for i in 0..n {
            let o = 84 + 50 * i + 12;
            let p = |k: usize| [f(o + 12 * k), f(o + 12 * k + 4), f(o + 12 * k + 8)];
            tris.push([p(0), p(1), p(2)]);
        }
        m.bodies.extend(body(default_name.to_string(), tris));
    } else {
        let text = String::from_utf8_lossy(data);
        if !text.trim_start().to_ascii_lowercase().starts_with("solid") {
            return Err("not an STL file".into());
        }
        let mut name = default_name.to_string();
        let mut tris = Vec::new();
        let mut cur: Vec<V3> = Vec::new();
        for (ln, line) in text.lines().enumerate() {
            let mut it = line.split_whitespace();
            match it.next().map(|w| w.to_ascii_lowercase()) {
                Some(w) if w == "solid" => {
                    let n: Vec<&str> = it.collect();
                    name = if n.is_empty() {
                        default_name.to_string()
                    } else {
                        n.join(" ")
                    };
                }
                Some(w) if w == "vertex" => {
                    let v: Vec<f64> = it.filter_map(|x| x.parse().ok()).collect();
                    if v.len() != 3 {
                        return Err(format!("STL line {}: bad vertex", ln + 1));
                    }
                    cur.push([v[0] * s, v[1] * s, v[2] * s]);
                }
                Some(w) if w == "endloop" => {
                    for k in 1..cur.len().saturating_sub(1) {
                        tris.push([cur[0], cur[k], cur[k + 1]]);
                    }
                    cur.clear();
                }
                Some(w) if w == "endsolid" => {
                    m.bodies
                        .extend(body(std::mem::take(&mut name), std::mem::take(&mut tris)));
                    name = default_name.to_string();
                }
                _ => {}
            }
        }
        m.bodies.extend(body(name, tris));
    }
    m.products = m.bodies.len();
    m.occurrences = m.bodies.len();
    if m.bodies.is_empty() {
        m.warnings.push("the STL file has no triangles".into());
    }
    Ok(m)
}

/// ASCII STL of axis-aligned boxes (mm), one `solid` per box.
pub fn write_boxes(boxes: &[(String, V3, V3)]) -> String {
    let mut out = String::new();
    for (name, a, b) in boxes {
        let n = name.replace(char::is_whitespace, "_");
        let _ = writeln!(out, "solid {n}");
        let c = |i: usize| -> V3 {
            [
                if i & 1 == 0 { a[0] } else { b[0] },
                if i & 2 == 0 { a[1] } else { b[1] },
                if i & 4 == 0 { a[2] } else { b[2] },
            ]
        };
        // faces as corner indices, counter-clockwise seen from outside
        let faces: [([usize; 4], V3); 6] = [
            ([0, 4, 6, 2], [-1.0, 0.0, 0.0]),
            ([1, 3, 7, 5], [1.0, 0.0, 0.0]),
            ([0, 1, 5, 4], [0.0, -1.0, 0.0]),
            ([2, 6, 7, 3], [0.0, 1.0, 0.0]),
            ([0, 2, 3, 1], [0.0, 0.0, -1.0]),
            ([4, 5, 7, 6], [0.0, 0.0, 1.0]),
        ];
        for (q, nv) in faces {
            for t in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
                let _ = writeln!(
                    out,
                    "  facet normal {} {} {}\n    outer loop",
                    nv[0], nv[1], nv[2]
                );
                for k in t {
                    let p = c(k);
                    let _ = writeln!(out, "      vertex {:.6} {:.6} {:.6}", p[0], p[1], p[2]);
                }
                let _ = writeln!(out, "    endloop\n  endfacet");
            }
        }
        let _ = writeln!(out, "endsolid {n}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_round_trip_and_binary() {
        let boxes = vec![
            ("a".to_string(), [0.0, 0.0, 0.0], [10.0, 20.0, 30.0]),
            ("b part".to_string(), [5.0, 5.0, 30.0], [6.0, 7.0, 33.0]),
        ];
        let text = write_boxes(&boxes);
        let m = read(text.as_bytes(), "x", 1.0).unwrap();
        assert_eq!(m.bodies.len(), 2);
        assert_eq!(m.bodies[1].name, "b_part");
        assert!((m.bodies[0].volume() - 6000.0).abs() < 1e-6);
        assert!((m.bodies[1].volume() - 6.0).abs() < 1e-6);
        // the signed volume must be positive (outward normals)
        let mut v = 0.0;
        for t in &m.bodies[0].tris {
            let (a, b, c) = (t[0], t[1], t[2]);
            v += a[0] * (b[1] * c[2] - c[1] * b[2]) - b[0] * (a[1] * c[2] - c[1] * a[2])
                + c[0] * (a[1] * b[2] - b[1] * a[2]);
        }
        assert!(v > 0.0);
        // binary version of the first box
        let mut bin = vec![0u8; 80];
        let tris = &m.bodies[0].tris;
        bin.extend((tris.len() as u32).to_le_bytes());
        for t in tris {
            bin.extend([0u8; 12]);
            for p in t {
                for c in p {
                    bin.extend((*c as f32).to_le_bytes());
                }
            }
            bin.extend([0u8; 2]);
        }
        let mb = read(&bin, "bin", 1.0).unwrap();
        assert_eq!(mb.bodies.len(), 1);
        assert!((mb.bodies[0].volume() - 6000.0).abs() < 1e-3);
    }
}
