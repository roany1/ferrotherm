// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Gerber (RS-274X / X2), Excellon drill and Gerber job file readers.
//!
//! Gerber files are interpreted into a display list of filled primitives
//! (discs, round-capped segments, polygons) that is rasterised into the board
//! frame to measure copper coverage. Supported: FS/MO, standard apertures
//! C/R/O/P, aperture macros (primitives 1, 4, 5, 7, 20, 21, 22 with
//! arithmetic expressions), D01/D02/D03, G01/G02/G03 (multi-quadrant),
//! regions G36/G37, polarity LPD/LPC, X2 file attributes. Exposure-off macro
//! primitives and aperture holes are ignored (slight copper over-estimate).

use crate::ecad::{Board, Drill, Layer, LayerKind};
use crate::geom::{arc_points, bbox, Poly, Raster, P2};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Prim {
    Disc { c: P2, r: f64, dark: bool },
    Seg { a: P2, b: P2, w: f64, dark: bool },
    Poly { pts: Vec<P2>, dark: bool },
}

impl Prim {
    fn points(&self, out: &mut Vec<P2>) {
        match self {
            Prim::Disc { c, r, .. } => {
                out.push([c[0] - r, c[1] - r]);
                out.push([c[0] + r, c[1] + r]);
            }
            Prim::Seg { a, b, w, .. } => {
                let r = 0.5 * w;
                out.push([a[0].min(b[0]) - r, a[1].min(b[1]) - r]);
                out.push([a[0].max(b[0]) + r, a[1].max(b[1]) + r]);
            }
            Prim::Poly { pts, .. } => out.extend_from_slice(pts),
        }
    }
    pub fn draw(&self, r: &mut Raster) {
        match self {
            Prim::Disc { c, r: rad, dark } => r.disc(*c, *rad, *dark),
            Prim::Seg { a, b, w, dark } => r.segment(*a, *b, *w, *dark),
            Prim::Poly { pts, dark } => r.fill_poly(pts, *dark),
        }
    }
}

#[derive(Debug, Clone)]
enum Aperture {
    Circle(f64),
    Rect(f64, f64),
    Obround(f64, f64),
    Polygon(f64, usize, f64),
    Macro(String, Vec<f64>),
}

#[derive(Debug, Clone, Default)]
pub struct GerberLayer {
    pub file_function: Option<String>,
    pub prims: Vec<Prim>,
    /// Draws without area (profile/outline lines), as polylines.
    pub lines: Vec<Vec<P2>>,
    pub warnings: Vec<String>,
}

impl GerberLayer {
    pub fn bbox(&self) -> Option<(P2, P2)> {
        let mut pts = Vec::new();
        for p in &self.prims {
            p.points(&mut pts);
        }
        for l in &self.lines {
            pts.extend_from_slice(l);
        }
        bbox(&pts)
    }

    pub fn rasterize(&self, r: &mut Raster) {
        for p in &self.prims {
            p.draw(r);
        }
    }

    /// Closed outline contours from the drawn lines (profile layers).
    pub fn contours(&self) -> Vec<Poly> {
        chain_segments(&self.lines, 0.01)
    }
}

/// Evaluate an aperture-macro arithmetic expression.
fn eval(expr: &str, vars: &HashMap<usize, f64>) -> f64 {
    struct P<'a> {
        s: &'a [u8],
        i: usize,
        v: &'a HashMap<usize, f64>,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && self.s[self.i] == b' ' {
                self.i += 1;
            }
        }
        fn expr(&mut self) -> f64 {
            let mut a = self.term();
            loop {
                self.ws();
                match self.s.get(self.i) {
                    Some(b'+') => {
                        self.i += 1;
                        a += self.term();
                    }
                    Some(b'-') => {
                        self.i += 1;
                        a -= self.term();
                    }
                    _ => return a,
                }
            }
        }
        fn term(&mut self) -> f64 {
            let mut a = self.factor();
            loop {
                self.ws();
                match self.s.get(self.i) {
                    Some(b'x') | Some(b'X') => {
                        self.i += 1;
                        a *= self.factor();
                    }
                    Some(b'/') => {
                        self.i += 1;
                        let d = self.factor();
                        a = if d != 0.0 { a / d } else { 0.0 };
                    }
                    _ => return a,
                }
            }
        }
        fn factor(&mut self) -> f64 {
            self.ws();
            match self.s.get(self.i) {
                Some(b'(') => {
                    self.i += 1;
                    let v = self.expr();
                    self.ws();
                    if self.s.get(self.i) == Some(&b')') {
                        self.i += 1;
                    }
                    v
                }
                Some(b'-') => {
                    self.i += 1;
                    -self.factor()
                }
                Some(b'+') => {
                    self.i += 1;
                    self.factor()
                }
                Some(b'$') => {
                    self.i += 1;
                    let st = self.i;
                    while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                        self.i += 1;
                    }
                    let k: usize = std::str::from_utf8(&self.s[st..self.i])
                        .unwrap()
                        .parse()
                        .unwrap_or(0);
                    *self.v.get(&k).unwrap_or(&0.0)
                }
                _ => {
                    let st = self.i;
                    while self.i < self.s.len()
                        && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.')
                    {
                        self.i += 1;
                    }
                    std::str::from_utf8(&self.s[st..self.i])
                        .unwrap()
                        .parse()
                        .unwrap_or(0.0)
                }
            }
        }
    }
    let mut p = P {
        s: expr.as_bytes(),
        i: 0,
        v: vars,
    };
    p.expr()
}

fn rot(p: P2, deg: f64) -> P2 {
    crate::geom::rotate(p, deg)
}

/// Primitives of a flashed macro at `c`.
fn flash_macro(
    body: &[String],
    params: &[f64],
    c: P2,
    dark: bool,
    out: &mut Vec<Prim>,
    warn: &mut Vec<String>,
) {
    let mut vars: HashMap<usize, f64> = HashMap::new();
    for (i, p) in params.iter().enumerate() {
        vars.insert(i + 1, *p);
    }
    for stmt in body {
        let s = stmt.trim();
        if s.is_empty() || s.starts_with('0') && (s.len() == 1 || s.as_bytes()[1] == b' ') {
            continue; // comment
        }
        if let Some(rest) = s.strip_prefix('$') {
            if let Some((k, e)) = rest.split_once('=') {
                if let Ok(k) = k.trim().parse::<usize>() {
                    let v = eval(e, &vars);
                    vars.insert(k, v);
                }
            }
            continue;
        }
        let f: Vec<f64> = s.split(',').map(|x| eval(x, &vars)).collect();
        if f.is_empty() {
            continue;
        }
        let code = f[0] as i32;
        let exp_on = |e: f64| e != 0.0;
        let tr = |x: f64, y: f64, r: f64| {
            let q = rot([x, y], r);
            [q[0] + c[0], q[1] + c[1]]
        };
        match code {
            1 if f.len() >= 5 => {
                if exp_on(f[1]) {
                    let r = f.get(5).copied().unwrap_or(0.0);
                    out.push(Prim::Disc {
                        c: tr(f[3], f[4], r),
                        r: 0.5 * f[2],
                        dark,
                    });
                }
            }
            20 | 2 if f.len() >= 8 => {
                if exp_on(f[1]) {
                    let (w, r) = (f[2], f[7]);
                    let (a, b) = ([f[3], f[4]], [f[5], f[6]]);
                    let d = [b[0] - a[0], b[1] - a[1]];
                    let l = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-12);
                    let nrm = [-d[1] / l * 0.5 * w, d[0] / l * 0.5 * w];
                    out.push(Prim::Poly {
                        pts: vec![
                            tr(a[0] + nrm[0], a[1] + nrm[1], r),
                            tr(b[0] + nrm[0], b[1] + nrm[1], r),
                            tr(b[0] - nrm[0], b[1] - nrm[1], r),
                            tr(a[0] - nrm[0], a[1] - nrm[1], r),
                        ],
                        dark,
                    });
                }
            }
            21 if f.len() >= 7 => {
                if exp_on(f[1]) {
                    let (w, h, cx, cy, r) = (f[2], f[3], f[4], f[5], f[6]);
                    out.push(Prim::Poly {
                        pts: vec![
                            tr(cx - 0.5 * w, cy - 0.5 * h, r),
                            tr(cx + 0.5 * w, cy - 0.5 * h, r),
                            tr(cx + 0.5 * w, cy + 0.5 * h, r),
                            tr(cx - 0.5 * w, cy + 0.5 * h, r),
                        ],
                        dark,
                    });
                }
            }
            22 if f.len() >= 7 => {
                if exp_on(f[1]) {
                    let (w, h, x, y, r) = (f[2], f[3], f[4], f[5], f[6]);
                    out.push(Prim::Poly {
                        pts: vec![
                            tr(x, y, r),
                            tr(x + w, y, r),
                            tr(x + w, y + h, r),
                            tr(x, y + h, r),
                        ],
                        dark,
                    });
                }
            }
            4 if f.len() >= 3 => {
                let n = f[2] as usize;
                if f.len() > 3 + 2 * (n + 1) && exp_on(f[1]) {
                    let r = f[3 + 2 * (n + 1)];
                    let pts = (0..=n).map(|k| tr(f[3 + 2 * k], f[4 + 2 * k], r)).collect();
                    out.push(Prim::Poly { pts, dark });
                }
            }
            5 if f.len() >= 6 => {
                if exp_on(f[1]) {
                    let n = (f[2] as usize).max(3);
                    let (cx, cy, d) = (f[3], f[4], f[5]);
                    let r = f.get(6).copied().unwrap_or(0.0);
                    let pts = (0..n)
                        .map(|k| {
                            let a = (k as f64) * 360.0 / n as f64;
                            let q = rot([0.5 * d, 0.0], a);
                            tr(cx + q[0], cy + q[1], r)
                        })
                        .collect();
                    out.push(Prim::Poly { pts, dark });
                }
            }
            7 if f.len() >= 6 => {
                // thermal: approximate as a full ring (gaps ignored)
                let (cx, cy, od, id) = (f[1], f[2], f[3], f[4]);
                let r = f.get(6).copied().unwrap_or(0.0);
                let ccen = tr(cx, cy, r);
                let mut ring = Vec::new();
                let rm = 0.25 * (od + id);
                for k in 0..=36 {
                    let a = (k as f64 * 10.0).to_radians();
                    ring.push([ccen[0] + rm * a.cos(), ccen[1] + rm * a.sin()]);
                }
                for w in ring.windows(2) {
                    out.push(Prim::Seg {
                        a: w[0],
                        b: w[1],
                        w: 0.5 * (od - id),
                        dark,
                    });
                }
            }
            _ => {
                if !warn.iter().any(|w| w.contains("macro primitive")) {
                    warn.push(format!(
                        "unsupported aperture macro primitive {code} ignored"
                    ));
                }
            }
        }
    }
}

/// Parse a Gerber RS-274X file into primitives (mm).
pub fn parse_gerber(text: &str) -> Result<GerberLayer, String> {
    let mut out = GerberLayer::default();
    let mut scale = 1.0; // file unit → mm
    let mut fmt = (2u32, 6u32); // x int, dec
    let mut apertures: HashMap<u32, Aperture> = HashMap::new();
    let mut macros: HashMap<String, Vec<String>> = HashMap::new();
    let mut cur_ap: Option<u32> = None;
    let mut pos: P2 = [0.0, 0.0];
    let mut interp = 1; // 1 linear, 2 cw, 3 ccw
    let mut dark = true;
    let mut in_region = false;
    let mut region: Vec<P2> = Vec::new();
    let mut regions: Vec<Vec<P2>> = Vec::new();
    let mut cur_line: Vec<P2> = Vec::new();

    let bytes = text.as_bytes();
    let mut i = 0;
    let parse_coord = |s: &str, fmt: (u32, u32), scale: f64| -> f64 {
        if s.contains('.') {
            return s.parse::<f64>().unwrap_or(0.0) * scale;
        }
        let v: f64 = s.parse::<i64>().unwrap_or(0) as f64;
        v / 10f64.powi(fmt.1 as i32) * scale
    };
    let flush_line = |cur_line: &mut Vec<P2>, out: &mut GerberLayer| {
        if cur_line.len() >= 2 {
            out.lines.push(std::mem::take(cur_line));
        } else {
            cur_line.clear();
        }
    };
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'%' {
            // extended command block
            let end = text[i + 1..]
                .find('%')
                .map(|e| i + 1 + e)
                .ok_or("unterminated % block")?;
            let block = &text[i + 1..end];
            i = end + 1;
            let words: Vec<&str> = block
                .split('*')
                .map(|w| w.trim())
                .filter(|w| !w.is_empty())
                .collect();
            let Some(first) = words.first() else { continue };
            if let Some(rest) = first.strip_prefix("FS") {
                // FSLAX46Y46
                if let Some(xp) = rest.find('X') {
                    let d = &rest[xp + 1..];
                    if d.len() >= 2 {
                        let a = d.as_bytes()[0] - b'0';
                        let b = d.as_bytes()[1] - b'0';
                        fmt = (a as u32, b as u32);
                    }
                }
            } else if first.starts_with("MOMM") {
                scale = 1.0;
            } else if first.starts_with("MOIN") {
                scale = 25.4;
            } else if let Some(rest) = first.strip_prefix("AD") {
                // ADD10C,0.5X0.2
                let rest = rest.trim_start_matches('D');
                let n_end = rest
                    .find(|ch: char| !ch.is_ascii_digit())
                    .unwrap_or(rest.len());
                let code: u32 = rest[..n_end].parse().map_err(|_| "bad aperture code")?;
                let def = &rest[n_end..];
                let (name, params) = match def.split_once(',') {
                    Some((n, p)) => (
                        n,
                        p.split('X')
                            .filter_map(|v| v.trim().parse::<f64>().ok())
                            .collect::<Vec<_>>(),
                    ),
                    None => (def, vec![]),
                };
                let s = |k: usize| params.get(k).copied().unwrap_or(0.0) * scale;
                let ap = match name {
                    "C" => Aperture::Circle(s(0)),
                    "R" => Aperture::Rect(s(0), s(1)),
                    "O" => Aperture::Obround(s(0), s(1)),
                    "P" => Aperture::Polygon(
                        s(0),
                        params.get(1).copied().unwrap_or(3.0) as usize,
                        params.get(2).copied().unwrap_or(0.0),
                    ),
                    m => Aperture::Macro(m.to_string(), params.clone()),
                };
                apertures.insert(code, ap);
            } else if let Some(name) = first.strip_prefix("AM") {
                macros.insert(
                    name.to_string(),
                    words[1..].iter().map(|w| w.to_string()).collect(),
                );
            } else if first.starts_with("LPD") {
                dark = true;
            } else if first.starts_with("LPC") {
                dark = false;
            } else if let Some(rest) = first.strip_prefix("TF.FileFunction,") {
                out.file_function = Some(rest.to_string());
            } else if first.starts_with("SR")
                && first.len() > 2
                && !out.warnings.iter().any(|w| w.contains("step"))
            {
                out.warnings
                    .push("step-and-repeat (SR) blocks are not replicated".into());
            }
            continue;
        }
        // normal data block up to '*'
        let end = match text[i..].find('*') {
            Some(e) => i + e,
            None => break,
        };
        let blk = text[i..end].trim();
        i = end + 1;
        if blk.is_empty() || blk.starts_with("G04") {
            continue;
        }
        let mut s = blk;
        // G codes
        while let Some(rest) = s.strip_prefix('G') {
            let n_end = rest
                .find(|ch: char| !ch.is_ascii_digit())
                .unwrap_or(rest.len());
            let g: u32 = rest[..n_end].parse().unwrap_or(0);
            match g {
                1 => interp = 1,
                2 => interp = 2,
                3 => interp = 3,
                36 => {
                    in_region = true;
                    region.clear();
                    regions.clear();
                }
                37 => {
                    if region.len() >= 3 {
                        regions.push(std::mem::take(&mut region));
                    }
                    for r in regions.drain(..) {
                        out.prims.push(Prim::Poly { pts: r, dark });
                    }
                    in_region = false;
                }
                _ => {}
            }
            s = &rest[n_end..];
        }
        if s.starts_with("M02") || s.starts_with("M00") || s.starts_with("M01") {
            break;
        }
        // aperture select
        if let Some(rest) = s.strip_prefix('D') {
            if let Ok(d) = rest.parse::<u32>() {
                if d >= 10 {
                    flush_line(&mut cur_line, &mut out);
                    cur_ap = Some(d);
                    continue;
                }
            }
        }
        // coordinates
        let mut x = None;
        let mut y = None;
        let mut ij = [0.0, 0.0];
        let mut op = None;
        let b = s.as_bytes();
        let mut k = 0;
        while k < b.len() {
            let ch = b[k];
            k += 1;
            let st = k;
            while k < b.len()
                && (b[k].is_ascii_digit() || b[k] == b'-' || b[k] == b'+' || b[k] == b'.')
            {
                k += 1;
            }
            let num = &s[st..k];
            match ch {
                b'X' => x = Some(parse_coord(num, fmt, scale)),
                b'Y' => y = Some(parse_coord(num, fmt, scale)),
                b'I' => ij[0] = parse_coord(num, fmt, scale),
                b'J' => ij[1] = parse_coord(num, fmt, scale),
                b'D' => op = num.parse::<u32>().ok(),
                _ => {}
            }
        }
        let new = [x.unwrap_or(pos[0]), y.unwrap_or(pos[1])];
        let op = match op {
            Some(o) => o,
            None if x.is_some() || y.is_some() => 1, // deprecated modal D01
            None => continue,
        };
        match op {
            1 => {
                // draw from pos to new
                let mut path = vec![pos];
                if interp == 1 {
                    path.push(new);
                } else {
                    let c = [pos[0] + ij[0], pos[1] + ij[1]];
                    let a0 = (pos[1] - c[1]).atan2(pos[0] - c[0]);
                    let a1 = (new[1] - c[1]).atan2(new[0] - c[0]);
                    let mut sw = (a1 - a0).to_degrees();
                    if interp == 3 {
                        if sw <= 1e-9 {
                            sw += 360.0;
                        }
                    } else if sw >= -1e-9 {
                        sw -= 360.0;
                    }
                    arc_points(c, pos, sw, &mut path);
                    if let Some(l) = path.last_mut() {
                        *l = new;
                    }
                }
                if in_region {
                    if region.is_empty() {
                        region.push(pos);
                    }
                    region.extend_from_slice(&path[1..]);
                } else {
                    let w = match cur_ap.and_then(|a| apertures.get(&a)) {
                        Some(Aperture::Circle(d)) => *d,
                        Some(Aperture::Rect(a, b)) | Some(Aperture::Obround(a, b)) => a.min(*b),
                        Some(Aperture::Polygon(d, _, _)) => *d,
                        _ => 0.0,
                    };
                    if w > 0.0 {
                        for seg in path.windows(2) {
                            out.prims.push(Prim::Seg {
                                a: seg[0],
                                b: seg[1],
                                w,
                                dark,
                            });
                        }
                    }
                    // keep the centre line for outline extraction
                    if cur_line
                        .last()
                        .map(|l| (l[0] - pos[0]).abs() + (l[1] - pos[1]).abs() > 1e-9)
                        .unwrap_or(true)
                    {
                        flush_line(&mut cur_line, &mut out);
                        cur_line.push(pos);
                    }
                    cur_line.extend_from_slice(&path[1..]);
                }
            }
            2 => {
                if in_region && region.len() >= 3 {
                    regions.push(std::mem::take(&mut region));
                } else if in_region {
                    region.clear();
                }
                flush_line(&mut cur_line, &mut out);
            }
            3 => {
                flush_line(&mut cur_line, &mut out);
                match cur_ap.and_then(|a| apertures.get(&a)).cloned() {
                    Some(Aperture::Circle(d)) => out.prims.push(Prim::Disc {
                        c: new,
                        r: 0.5 * d,
                        dark,
                    }),
                    Some(Aperture::Rect(w, h)) => out.prims.push(Prim::Poly {
                        pts: vec![
                            [new[0] - 0.5 * w, new[1] - 0.5 * h],
                            [new[0] + 0.5 * w, new[1] - 0.5 * h],
                            [new[0] + 0.5 * w, new[1] + 0.5 * h],
                            [new[0] - 0.5 * w, new[1] + 0.5 * h],
                        ],
                        dark,
                    }),
                    Some(Aperture::Obround(w, h)) => {
                        if (w - h).abs() < 1e-12 {
                            out.prims.push(Prim::Disc {
                                c: new,
                                r: 0.5 * w,
                                dark,
                            });
                        } else if w > h {
                            out.prims.push(Prim::Seg {
                                a: [new[0] - 0.5 * (w - h), new[1]],
                                b: [new[0] + 0.5 * (w - h), new[1]],
                                w: h,
                                dark,
                            });
                        } else {
                            out.prims.push(Prim::Seg {
                                a: [new[0], new[1] - 0.5 * (h - w)],
                                b: [new[0], new[1] + 0.5 * (h - w)],
                                w,
                                dark,
                            });
                        }
                    }
                    Some(Aperture::Polygon(d, n, r)) => {
                        let n = n.max(3);
                        let pts = (0..n)
                            .map(|k| {
                                let q = rot([0.5 * d, 0.0], r + k as f64 * 360.0 / n as f64);
                                [new[0] + q[0], new[1] + q[1]]
                            })
                            .collect();
                        out.prims.push(Prim::Poly { pts, dark });
                    }
                    Some(Aperture::Macro(name, params)) => match macros.get(&name) {
                        Some(body) => {
                            // macro coordinates are in file units
                            let mut tmp = Vec::new();
                            let mut warn = Vec::new();
                            flash_macro(body, &params, [0.0, 0.0], dark, &mut tmp, &mut warn);
                            for w in warn {
                                if !out.warnings.contains(&w) {
                                    out.warnings.push(w);
                                }
                            }
                            for p in tmp {
                                out.prims.push(match p {
                                    Prim::Disc { c, r, dark } => Prim::Disc {
                                        c: [new[0] + c[0] * scale, new[1] + c[1] * scale],
                                        r: r * scale,
                                        dark,
                                    },
                                    Prim::Seg { a, b, w, dark } => Prim::Seg {
                                        a: [new[0] + a[0] * scale, new[1] + a[1] * scale],
                                        b: [new[0] + b[0] * scale, new[1] + b[1] * scale],
                                        w: w * scale,
                                        dark,
                                    },
                                    Prim::Poly { pts, dark } => Prim::Poly {
                                        pts: pts
                                            .iter()
                                            .map(|q| [new[0] + q[0] * scale, new[1] + q[1] * scale])
                                            .collect(),
                                        dark,
                                    },
                                });
                            }
                        }
                        None => out
                            .warnings
                            .push(format!("undefined aperture macro '{name}'")),
                    },
                    None => {}
                }
            }
            _ => {}
        }
        pos = new;
    }
    flush_line(&mut cur_line, &mut out);
    Ok(out)
}

/// Join polylines into closed loops by matching end points within `tol` mm.
pub fn chain_segments(lines: &[Vec<P2>], tol: f64) -> Vec<Poly> {
    let mut pool: Vec<Vec<P2>> = lines.iter().filter(|l| l.len() >= 2).cloned().collect();
    let close = |a: P2, b: P2| (a[0] - b[0]).abs() <= tol && (a[1] - b[1]).abs() <= tol;
    let mut loops = Vec::new();
    while let Some(mut cur) = pool.pop() {
        loop {
            let end = *cur.last().unwrap();
            if cur.len() > 2 && close(end, cur[0]) {
                cur.pop();
                loops.push(cur);
                break;
            }
            let mut found = None;
            for (k, l) in pool.iter().enumerate() {
                if close(l[0], end) {
                    found = Some((k, false));
                    break;
                }
                if close(*l.last().unwrap(), end) {
                    found = Some((k, true));
                    break;
                }
            }
            match found {
                Some((k, rev)) => {
                    let mut l = pool.swap_remove(k);
                    if rev {
                        l.reverse();
                    }
                    cur.extend_from_slice(&l[1..]);
                }
                None => break, // open chain: dropped
            }
        }
    }
    loops
}

/// Excellon drill file → drills (mm). `plated_default` for files without attributes.
pub fn parse_excellon(
    text: &str,
    plated_default: bool,
) -> Result<(Vec<Drill>, Vec<String>), String> {
    let mut tools: HashMap<u32, (f64, bool)> = HashMap::new();
    let mut metric = true;
    let mut fmt_dec = 3u32; // implied decimals
    let mut cur: Option<(f64, bool)> = None;
    let mut header = true;
    let mut drills = Vec::new();
    let mut warnings = Vec::new();
    let mut tool_plated = plated_default;
    let mut file_plated = plated_default;
    let (mut x, mut y) = (0.0, 0.0);
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with(';') {
            if line.contains("TF.FileFunction,NonPlated") {
                file_plated = false;
                tool_plated = false;
            } else if line.contains("TF.FileFunction,Plated") {
                file_plated = true;
                tool_plated = true;
            }
            if line.contains("TA.AperFunction,NonPlated") {
                tool_plated = false;
            } else if line.contains("TA.AperFunction,Plated") {
                tool_plated = true;
            }
            if let Some(f) = line.split("FILE_FORMAT=").nth(1) {
                if let Some((_, d)) = f.split_once(':') {
                    fmt_dec = d.trim().parse().unwrap_or(fmt_dec);
                }
            }
            continue;
        }
        if line.starts_with("METRIC") {
            metric = true;
            if fmt_dec == 3 && line.contains("0000.00") {
                fmt_dec = 2;
            }
            continue;
        }
        if line.starts_with("INCH") {
            metric = false;
            fmt_dec = 4;
            continue;
        }
        if line == "%" || line == "M95" {
            header = false;
            continue;
        }
        let unit = if metric { 1.0 } else { 25.4 };
        if let Some(rest) = line.strip_prefix('T') {
            let n_end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            let t: u32 = rest[..n_end].parse().unwrap_or(0);
            if let Some(cpos) = rest.find('C') {
                let d: f64 = rest[cpos + 1..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0.0);
                tools.insert(t, (d * unit, tool_plated));
                tool_plated = file_plated;
            } else if !header {
                cur = tools.get(&t).copied();
            }
            continue;
        }
        if line.starts_with('X') || line.starts_with('Y') {
            if line.contains("G85") {
                warnings.push("routed slots (G85) are treated as two holes".into());
            }
            let parse = |s: &str| -> Option<f64> {
                let num: String = s
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
                    .collect();
                if num.is_empty() {
                    return None;
                }
                if num.contains('.') {
                    num.parse::<f64>().ok().map(|v| v * unit)
                } else {
                    num.parse::<i64>()
                        .ok()
                        .map(|v| v as f64 / 10f64.powi(fmt_dec as i32) * unit)
                }
            };
            for part in line.split('G').next().into_iter() {
                if let Some(p) = part.find('X') {
                    if let Some(v) = parse(&part[p + 1..]) {
                        x = v;
                    }
                }
                if let Some(p) = part.find('Y') {
                    if let Some(v) = parse(&part[p + 1..]) {
                        y = v;
                    }
                }
            }
            if let Some((d, plated)) = cur {
                drills.push(Drill {
                    x,
                    y,
                    d_mm: d,
                    plated,
                });
            }
        }
    }
    Ok((drills, warnings))
}

/// Gerber job file (JSON): stack-up and file functions.
#[derive(Debug, Clone, Default)]
pub struct Job {
    pub thickness_mm: Option<f64>,
    /// (name, kind, thickness) top to bottom.
    pub stackup: Vec<(String, LayerKind, f64, Option<String>)>,
    /// file path → file function
    pub files: Vec<(String, String)>,
}

pub fn parse_job(text: &str) -> Result<Job, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut job = Job {
        thickness_mm: v["GeneralSpecs"]["BoardThickness"].as_f64(),
        ..Default::default()
    };
    if let Some(arr) = v["MaterialStackup"].as_array() {
        for l in arr {
            let t = l["Type"].as_str().unwrap_or("");
            let kind = match t {
                "Copper" => LayerKind::Copper,
                "Dielectric" => LayerKind::Dielectric,
                _ => continue,
            };
            job.stackup.push((
                l["Name"].as_str().unwrap_or("").to_string(),
                kind,
                l["Thickness"].as_f64().unwrap_or(0.0),
                l["Material"].as_str().map(|s| s.to_string()),
            ));
        }
    }
    if let Some(arr) = v["FilesAttributes"].as_array() {
        for f in arr {
            if let (Some(p), Some(fun)) = (f["Path"].as_str(), f["FileFunction"].as_str()) {
                job.files.push((p.to_string(), fun.to_string()));
            }
        }
    }
    Ok(job)
}

/// Copper layer index from a file function such as "Copper,L2,Inr".
fn copper_index(ff: &str) -> Option<u32> {
    let mut it = ff.split(',');
    if it.next()? != "Copper" {
        return None;
    }
    it.next()?.strip_prefix('L')?.parse().ok()
}

/// Read a set of Gerber/Excellon/job files (name, contents) into a board.
pub fn read_set(files: &[(String, String)]) -> Result<Board, String> {
    let mut board = Board {
        format: "Gerber/Excellon".into(),
        ..Default::default()
    };
    let job = files
        .iter()
        .find(|(n, _)| n.to_ascii_lowercase().ends_with(".gbrjob"))
        .map(|(_, t)| parse_job(t))
        .transpose()?;
    let mut copper: Vec<(u32, String, GerberLayer)> = Vec::new();
    let mut profile: Option<GerberLayer> = None;
    for (name, text) in files {
        let lower = name.to_ascii_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or("");
        let is_drill = matches!(ext, "drl" | "xln" | "exc" | "txt" | "drd")
            || text.trim_start().starts_with("M48");
        if lower.ends_with(".gbrjob") {
            continue;
        }
        if is_drill {
            let (d, w) = parse_excellon(text, !lower.contains("npth"))?;
            board.drills.extend(d);
            board.warnings.extend(w);
            continue;
        }
        if !text.contains("%FS") {
            continue; // not a Gerber file
        }
        let g = parse_gerber(text).map_err(|e| format!("{name}: {e}"))?;
        // the job file's function wins, then X2 attributes, then extensions
        let base = std::path::Path::new(name)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let ff = job
            .as_ref()
            .and_then(|j| {
                j.files
                    .iter()
                    .find(|(p, _)| p.eq_ignore_ascii_case(&base))
                    .map(|x| x.1.clone())
            })
            .or_else(|| g.file_function.clone())
            .or_else(|| match ext {
                "gtl" | "cmp" | "top" => Some("Copper,L1,Top".into()),
                "gbl" | "sol" | "bot" => Some("Copper,L99,Bot".into()),
                "gko" | "gm1" | "gml" | "oln" => Some("Profile,NP".into()),
                e if e.starts_with('g') && e[1..].parse::<u32>().is_ok() => Some(format!(
                    "Copper,L{},Inr",
                    e[1..].parse::<u32>().unwrap() + 1
                )),
                _ => None,
            });
        for w in &g.warnings {
            board.warnings.push(format!("{base}: {w}"));
        }
        match ff {
            Some(f) if f.starts_with("Profile") => profile = Some(g),
            Some(f) => {
                if let Some(ix) = copper_index(&f) {
                    copper.push((ix, base, g));
                }
            }
            None => {}
        }
    }
    copper.sort_by_key(|c| c.0);
    if let Some(p) = &profile {
        board.outline = p.contours();
        if board.outline.is_empty() {
            board
                .warnings
                .push("the profile layer has no closed contour; using the copper extent".into());
        }
    } else {
        board
            .warnings
            .push("no board profile (outline) layer found; using the copper extent".into());
    }
    if board.outline.is_empty() {
        let mut pts = Vec::new();
        for (_, _, g) in &copper {
            if let Some((lo, hi)) = g.bbox() {
                pts.push(lo);
                pts.push(hi);
            }
        }
        if let Some((lo, hi)) = bbox(&pts) {
            board.outline = vec![vec![lo, [hi[0], lo[1]], hi, [lo[0], hi[1]]]];
        }
    }
    board.thickness_mm = job.as_ref().and_then(|j| j.thickness_mm);
    // stack-up: from the job when present, else copper layers only
    let cu_thick: Vec<f64> = job
        .as_ref()
        .map(|j| {
            j.stackup
                .iter()
                .filter(|l| l.1 == LayerKind::Copper)
                .map(|l| l.2)
                .collect()
        })
        .unwrap_or_default();
    if let Some(j) = &job {
        let mut ci = 0;
        for (name, kind, t, mat) in &j.stackup {
            let mut l = Layer {
                name: name.clone(),
                kind: *kind,
                thickness_mm: Some(*t),
                material: mat.clone(),
                copper: None,
            };
            if *kind == LayerKind::Copper {
                if let Some((_, _, g)) = copper.get(ci) {
                    let mut r = board.new_raster();
                    g.rasterize(&mut r);
                    l.copper = Some(r);
                }
                ci += 1;
            }
            board.layers.push(l);
        }
    } else {
        for (k, (_, name, g)) in copper.iter().enumerate() {
            let mut r = board.new_raster();
            g.rasterize(&mut r);
            board.layers.push(Layer {
                name: name.clone(),
                kind: LayerKind::Copper,
                thickness_mm: cu_thick.get(k).copied(),
                material: None,
                copper: Some(r),
            });
        }
    }
    Ok(board)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macro_expressions() {
        let mut v = HashMap::new();
        v.insert(1, 2.0);
        v.insert(2, 3.0);
        assert!((eval("$1x$2+1", &v) - 7.0).abs() < 1e-12);
        assert!((eval("($1+$2)/2", &v) - 2.5).abs() < 1e-12);
        assert!((eval("-$1", &v) + 2.0).abs() < 1e-12);
    }

    #[test]
    fn simple_gerber_area() {
        // 10 x 10 mm square region and a 1 mm track 10 mm long
        let g = "%FSLAX46Y46*%\n%MOMM*%\n%ADD10C,1.000000*%\nG36*\nX0Y0D02*\nX10000000Y0D01*\nX10000000Y10000000D01*\nX0Y10000000D01*\nX0Y0D01*\nG37*\nD10*\nX20000000Y5000000D02*\nX30000000Y5000000D01*\nM02*\n";
        let l = parse_gerber(g).unwrap();
        let mut r = Raster::new([-1.0, -1.0], [32.0, 12.0], 0.02);
        l.rasterize(&mut r);
        let area = r.total_coverage() * 33.0 * 13.0;
        let exact = 100.0 + 10.0 + std::f64::consts::PI * 0.25;
        assert!((area - exact).abs() < 0.3, "{area} vs {exact}");
    }
}
