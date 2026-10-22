// Summary of a Celsius EC Solver model as FerroTherm imports it (debug tool).
fn main() {
    let p = std::env::args()
        .nth(1)
        .expect("usage: celsiusinfo FILE.equipment [out.toml]");
    let t0 = std::time::Instant::now();
    let m = ferrotherm_io::celsius::read(&std::fs::read(&p).unwrap()).unwrap();
    println!("{} ({:?})", m.summary(), t0.elapsed());
    for w in &m.warnings {
        println!("warning: {w}");
    }
    let c = m.to_case(ferrotherm_io::step::BlockMode::Voxels(2.0));
    println!(
        "domain {:?} .. {:?}, fill {}",
        c.domain.min, c.domain.max, c.domain.material
    );
    for b in c.blocks.iter().take(30) {
        println!(
            "block {:<28} {:?} .. {:?} {:?} P={}",
            b.name, b.min, b.max, b.material, b.power_w
        );
    }
    println!("{} blocks", c.blocks.len());
    for k in &c.components {
        println!(
            "comp {:<12} pos {:?} size {:?} P={} rjc {:?} rjb {:?} {:?}",
            k.name, k.position, k.size, k.power_w, k.r_jc, k.r_jb, k.case_face
        );
    }
    if let Some(out) = std::env::args().nth(2) {
        std::fs::write(out, c.to_toml().unwrap()).unwrap();
    }
}
