//! Tests on real files exported by KiCad (set FT_TESTDATA to the folder).
use ferrotherm_io::gerber;
use std::path::PathBuf;

fn data() -> Option<PathBuf> {
    std::env::var_os("FT_TESTDATA").map(PathBuf::from)
}

fn read_dir(p: &std::path::Path) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(p).unwrap() {
        let e = e.unwrap();
        if let Ok(t) = std::fs::read_to_string(e.path()) {
            v.push((e.file_name().to_string_lossy().to_string(), t));
        }
    }
    v
}

#[test]
fn kicad_gerber_set() {
    let Some(d) = data() else { return };
    for set in ["pic_gerber", "video_gerber"] {
        let files = read_dir(&d.join(set));
        let t0 = std::time::Instant::now();
        let mut b = gerber::read_set(&files).unwrap();
        let s = b.summary();
        eprintln!("{set}: {:?} in {:?}", s, t0.elapsed());
        assert!(s.copper_layers.len() >= 2);
        assert!(s.drills > 10);
        assert!(s.size_mm[0] > 10.0 && s.size_mm[1] > 10.0);
    }
}

#[test]
fn kicad_board_vs_gerber() {
    let Some(d) = data() else { return };
    for (pcb, set) in [
        ("pic_programmer_v7.kicad_pcb", "pic_gerber"),
        ("video_v7.kicad_pcb", "video_gerber"),
    ] {
        let t0 = std::time::Instant::now();
        let mut b =
            ferrotherm_io::kicad::read(&std::fs::read_to_string(d.join(pcb)).unwrap()).unwrap();
        let s = b.summary();
        let t1 = t0.elapsed();
        let mut g = gerber::read_set(&read_dir(&d.join(set))).unwrap();
        let gs = g.summary();
        eprintln!(
            "{pcb}: {} parts, {} drills, size {:?}, layers {:?} ({t1:?})",
            s.components, s.drills, s.size_mm, s.copper_layers
        );
        eprintln!("   gerber layers {:?}", gs.copper_layers);
        assert_eq!(s.copper_layers.len(), gs.copper_layers.len());
        for (a, b) in s.copper_layers.iter().zip(&gs.copper_layers) {
            assert!(
                (a.2 - b.2).abs() < 0.02,
                "coverage {} vs gerber {}",
                a.2,
                b.2
            );
        }
        assert!((s.size_mm[0] - gs.size_mm[0]).abs() < 0.5);
    }
    // KiCad 9 format of the same demos
    for pcb in ["pic_programmer.kicad_pcb", "video.kicad_pcb"] {
        let mut b =
            ferrotherm_io::kicad::read(&std::fs::read_to_string(d.join(pcb)).unwrap()).unwrap();
        let s = b.summary();
        eprintln!(
            "{pcb} (v9): {} parts size {:?} layers {:?}",
            s.components, s.size_mm, s.copper_layers
        );
        assert!(s.components > 20);
    }
}

#[test]
fn step_real_files() {
    let Some(d) = data() else { return };
    use ferrotherm_io::step;
    for f in ["LQFP-48_7x7mm_P0.5mm.step", "TO-220-3_Vertical.step"] {
        let t0 = std::time::Instant::now();
        let m = step::read(&std::fs::read_to_string(d.join(f)).unwrap(), 0.05).unwrap();
        eprintln!(
            "{f}: {} bodies ({:?}) {:?}",
            m.bodies.len(),
            t0.elapsed(),
            m.warnings
        );
        for b in &m.bodies {
            eprintln!(
                "   {} [{}] {:?}..{:?} vol {:.2} tris {} bbox_only {}",
                b.name,
                b.path,
                b.min,
                b.max,
                b.volume(),
                b.tris.len(),
                b.bbox_only
            );
        }
        assert!(!m.bodies.is_empty());
    }
    // LQFP-48 7x7: body 7x7 mm, leads to 9x9 mm, height <= 1.6 mm
    let m = step::read(
        &std::fs::read_to_string(d.join("LQFP-48_7x7mm_P0.5mm.step")).unwrap(),
        0.05,
    )
    .unwrap();
    let (mut mn, mut mx) = ([f64::MAX; 3], [f64::MIN; 3]);
    for b in &m.bodies {
        for a in 0..3 {
            mn[a] = mn[a].min(b.min[a]);
            mx[a] = mx[a].max(b.max[a]);
        }
    }
    assert!(
        (mx[0] - mn[0] - 9.0).abs() < 0.3 && (mx[1] - mn[1] - 9.0).abs() < 0.3,
        "{mn:?} {mx:?}"
    );
    assert!(mx[2] - mn[2] <= 1.65, "{mn:?} {mx:?}");

    // real KiCad assembly: every placed part found at its pick-and-place position
    let t0 = std::time::Instant::now();
    let m = step::read(
        &std::fs::read_to_string(d.join("pic_assembly.step")).unwrap(),
        0.1,
    )
    .unwrap();
    eprintln!(
        "assembly: {} bodies, {} products, {} occurrences, warnings {:?} ({:?})",
        m.bodies.len(),
        m.products,
        m.occurrences,
        m.warnings,
        t0.elapsed()
    );
    let pos = std::fs::read_to_string(d.join("pic_pos.csv")).unwrap();
    let mut checked = 0;
    for line in pos.lines().skip(1) {
        let f: Vec<&str> = line.split(',').map(|v| v.trim_matches('"')).collect();
        let n = f.len(); // value fields may contain commas: positions are counted from the end
        let (r, x, y) = (
            f[0],
            f[n - 4].parse::<f64>().unwrap(),
            f[n - 3].parse::<f64>().unwrap(),
        );
        let Some(b) = m.bodies.iter().find(|b| b.name == r) else {
            continue;
        };
        assert!(!b.bbox_only);
        assert!(
            x > b.min[0] - 1.5 && x < b.max[0] + 1.5 && y > b.min[1] - 1.5 && y < b.max[1] + 1.5,
            "{r} at ({x},{y}) outside {:?}..{:?}",
            b.min,
            b.max
        );
        checked += 1;
    }
    eprintln!("{checked} parts matched the position file");
    assert!(checked >= 50);
    // the board itself: 1.6 mm thick, z from 0 to 1.6
    let pcb = m
        .bodies
        .iter()
        .find(|b| b.name.contains("PCB"))
        .expect("board body");
    assert!(
        (pcb.max[2] - pcb.min[2] - 1.6).abs() < 0.06,
        "{:?} {:?}",
        pcb.min,
        pcb.max
    );
}

/// STEP written by FerroTherm must be readable by OpenCascade with the same boxes.
#[cfg(feature = "occt")]
#[test]
fn step_writer_read_by_occt() {
    use ferrotherm_io::step;
    let boxes = vec![
        ("board".to_string(), [0.0, 0.0, 0.0], [100.0, 80.0, 1.6]),
        ("U1".to_string(), [10.0, 20.0, 1.6], [24.0, 34.0, 3.2]),
        (
            "heatsink base".to_string(),
            [40.0, 10.0, 1.6],
            [70.0, 40.0, 6.6],
        ),
    ];
    let text = step::write_boxes("ft-test", &boxes);
    let m = ferrotherm_io::occt::read_step(&text, 0.01).unwrap();
    assert_eq!(m.bodies.len(), boxes.len(), "{:?}", m.warnings);
    for (n, a, b) in &boxes {
        let body = m.bodies.iter().find(|x| &x.name == n).unwrap_or_else(|| {
            panic!(
                "{n} missing: {:?}",
                m.bodies.iter().map(|b| &b.name).collect::<Vec<_>>()
            )
        });
        for k in 0..3 {
            assert!(
                (body.min[k] - a[k]).abs() < 1e-4 && (body.max[k] - b[k]).abs() < 1e-4,
                "{n}"
            );
        }
        let v = (b[0] - a[0]) * (b[1] - a[1]) * (b[2] - a[2]);
        assert!(
            (body.volume() - v).abs() < 1e-5 * v,
            "{n} volume {}",
            body.volume()
        );
    }
}
