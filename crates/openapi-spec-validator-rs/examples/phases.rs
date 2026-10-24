//! Times the validation phases on a spec file: `cargo run --release --example phases -- FILE`.

use std::time::Instant;

use openapi_spec_validator_rs::{Options, load, validate};

fn main() {
    let path = std::env::args().nth(1).expect("usage: phases FILE");
    let start = Instant::now();
    let bytes = std::fs::read(&path).expect("readable file");
    let read = start.elapsed();
    let start = Instant::now();
    let spec = load(&bytes, "<urllib response>").expect("loadable spec");
    let parsed = start.elapsed();
    let start = Instant::now();
    let outcome = validate(&spec, &Options::default());
    let validated = start.elapsed();
    let start = Instant::now();
    drop(spec);
    let dropped = start.elapsed();
    println!(
        "read {read:?}, parse {parsed:?}, validate {validated:?}, drop {dropped:?}, valid={}",
        outcome.is_valid()
    );
}
