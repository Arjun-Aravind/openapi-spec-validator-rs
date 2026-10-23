//! Python bindings: `openapi_spec_validator_rs._core`.

use pyo3::prelude::*;

/// Runs the command-line interface and returns `(exit_code, stdout, stderr)`.
///
/// Output is returned rather than written to the process streams so that Python
/// code (and pytest's `capsys`) sees it through `sys.stdout` / `sys.stderr`.
#[pyfunction]
fn cli_main(py: Python<'_>, args: Vec<String>) -> (i32, String, String) {
    py.detach(|| {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = openapi_spec_validator_rs_cli::run(args, &mut out, &mut err);
        (
            code,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    })
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(cli_main, m)?)?;
    Ok(())
}
