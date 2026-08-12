//! The `lsc` executable (SPEC §4).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(lc::main(args));
}
