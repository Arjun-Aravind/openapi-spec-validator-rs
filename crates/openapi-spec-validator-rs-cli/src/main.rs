use std::io::{self, Write};
use std::process::ExitCode;

// Parsing a large spec makes millions of small allocations.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut err = stderr.lock();
    let code = openapi_spec_validator_rs_cli::run(std::env::args_os().skip(1), &mut out, &mut err);
    let _ = out.flush();
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
