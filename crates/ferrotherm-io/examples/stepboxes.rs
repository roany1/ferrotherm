// STEP bodies as "path | name | min | max" lines, built-in reader (debug tool).
fn main() {
    let p = std::env::args().nth(1).expect("usage: stepboxes FILE.stp");
    let m = ferrotherm_io::step::read_structure(&std::fs::read_to_string(&p).unwrap()).unwrap();
    for b in &m.bodies {
        let r = |v: [f64; 3]| v.map(|x| (x * 1000.0).round() / 1000.0);
        println!("{} | {} | {:?} | {:?}", b.path, b.name, r(b.min), r(b.max));
    }
    eprintln!("{} bodies", m.bodies.len());
}
