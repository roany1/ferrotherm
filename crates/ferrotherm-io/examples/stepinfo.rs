// Summary of a STEP file as FerroTherm sees it (debug tool).
fn main() {
    let p = std::env::args().nth(1).expect("usage: stepinfo FILE.step [tolerance_mm]");
    let tol: f64 = std::env::args().nth(2).and_then(|v| v.parse().ok()).unwrap_or(0.1);
    let text = std::fs::read_to_string(&p).unwrap();
    let t0 = std::time::Instant::now();
    let m = ferrotherm_io::step::read(&text, tol).unwrap();
    println!("{} bodies, {} products, {} occurrences in {:?}", m.bodies.len(), m.products, m.occurrences, t0.elapsed());
    for w in &m.warnings {
        println!("warning: {w}");
    }
    for b in &m.bodies {
        println!(
            "{:<12} {:>7} tris  min [{:8.3} {:8.3} {:7.3}]  max [{:8.3} {:8.3} {:7.3}]  vol {:9.2} mm3  {}",
            b.name, b.tris.len(), b.min[0], b.min[1], b.min[2], b.max[0], b.max[1], b.max[2], b.volume(), b.path
        );
    }
}
