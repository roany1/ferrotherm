// Dump the decoded XML of a Celsius model (debug tool).
fn main() {
    let p = std::env::args()
        .nth(1)
        .expect("usage: celsiusxml FILE.equipment OUT.xml");
    let (app, ver, text) = ferrotherm_io::celsius::extract(&std::fs::read(&p).unwrap()).unwrap();
    eprintln!("{app} {ver}: {} bytes", text.len());
    std::fs::write(std::env::args().nth(2).unwrap(), text).unwrap();
}
