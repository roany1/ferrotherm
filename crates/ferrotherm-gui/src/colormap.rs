// FerroTherm — Copyright (c) 2026 Roany Romano
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Colour maps (sampled from matplotlib's Turbo and Inferno, 33 points each).

use eframe::egui::Color32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    Turbo,
    Inferno,
}

impl Palette {
    pub const ALL: [Palette; 2] = [Palette::Turbo, Palette::Inferno];

    pub fn name(self) -> &'static str {
        match self {
            Palette::Turbo => "Turbo",
            Palette::Inferno => "Inferno",
        }
    }

    /// Colour for `x` in [0, 1] (clamped).
    pub fn map(self, x: f32) -> Color32 {
        let lut = match self {
            Palette::Turbo => &TURBO,
            Palette::Inferno => &INFERNO,
        };
        let x = if x.is_finite() {
            x.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let f = x * (lut.len() - 1) as f32;
        let i = (f.floor() as usize).min(lut.len() - 2);
        let t = f - i as f32;
        let a = lut[i];
        let b = lut[i + 1];
        let mix = |p: u8, q: u8| (p as f32 + (q as f32 - p as f32) * t).round() as u8;
        Color32::from_rgb(mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2]))
    }
}

pub const TURBO: [[u8; 3]; 33] = [
    [48, 18, 59],
    [57, 42, 115],
    [64, 64, 162],
    [68, 86, 199],
    [70, 107, 227],
    [70, 128, 246],
    [66, 148, 255],
    [55, 168, 250],
    [40, 188, 235],
    [28, 205, 216],
    [24, 221, 194],
    [31, 233, 175],
    [50, 242, 152],
    [78, 249, 125],
    [109, 254, 98],
    [139, 255, 75],
    [164, 252, 60],
    [185, 246, 53],
    [205, 236, 52],
    [223, 223, 55],
    [238, 207, 58],
    [248, 190, 57],
    [253, 172, 52],
    [254, 150, 43],
    [251, 126, 33],
    [244, 102, 23],
    [235, 80, 14],
    [223, 63, 8],
    [208, 47, 5],
    [190, 33, 2],
    [169, 22, 1],
    [146, 11, 1],
    [122, 4, 3],
];
pub const INFERNO: [[u8; 3]; 33] = [
    [0, 0, 4],
    [4, 3, 18],
    [11, 7, 36],
    [21, 11, 55],
    [33, 12, 74],
    [47, 10, 91],
    [61, 9, 101],
    [74, 12, 107],
    [87, 16, 110],
    [100, 21, 110],
    [113, 25, 110],
    [125, 30, 109],
    [138, 34, 106],
    [151, 39, 102],
    [163, 44, 97],
    [176, 49, 91],
    [188, 55, 84],
    [199, 62, 76],
    [210, 70, 68],
    [219, 80, 59],
    [228, 90, 49],
    [235, 102, 40],
    [241, 115, 29],
    [246, 128, 19],
    [249, 142, 9],
    [251, 157, 7],
    [252, 172, 17],
    [251, 188, 33],
    [249, 203, 53],
    [245, 219, 76],
    [242, 234, 105],
    [243, 246, 138],
    [252, 255, 164],
];
