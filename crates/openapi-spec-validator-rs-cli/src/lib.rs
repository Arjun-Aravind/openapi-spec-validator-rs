//! Command-line interface compatible with `openapi-spec-validator`.
//!
//! Output and exit codes follow upstream: `<file>: OK` per valid file; on the first
//! failing file it prints the error and exits with 1 (read or validation error) or
//! 2 (any other error), without looking at the remaining files. Files are validated
//! in parallel, but reported in order.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use clap::{Parser, ValueEnum};
use openapi_spec_validator_rs::{
    ContextHandle, ErrorMode, LoadError, Options, Outcome, Retrieve, SpecVersion, TextErrors,
    ValidationError, best_match, load_text, read_text_until_unreadable, validate, validate_path,
};
use rayon::prelude::*;

/// Stack size of the validation threads (address space only; pages are committed on use).
const WORKER_STACK_SIZE: usize = 256 << 20;

/// Validate OpenAPI / Swagger specification files.
#[derive(Debug, Parser)]
#[command(name = "openapi-spec-validator-rs", version)]
pub struct Args {
    /// Validate specified file(s).
    #[arg(value_name = "file", required = true, num_args = 1..)]
    pub files: Vec<String>,

    /// Control subschema error details. Defaults to "best-match", use "all" to get
    /// all subschema errors.
    #[arg(long, value_enum, value_name = "{best-match,all}")]
    pub subschema_errors: Option<SubschemaErrors>,

    /// Control validation errors count. Defaults to "first", use "all" to get all
    /// validation errors.
    #[arg(long, value_enum, default_value_t = ValidationErrors::First, value_name = "{first,all}")]
    pub validation_errors: ValidationErrors,

    /// Deprecated alias for --subschema-errors.
    #[arg(
        long = "errors",
        visible_alias = "error",
        value_enum,
        value_name = "{best-match,all}"
    )]
    pub deprecated_subschema_errors: Option<SubschemaErrors>,

    /// OpenAPI schema version (default: detect).
    #[arg(long, default_value = "detect", value_name = "{detect,2.0,3.0,3.1,3.2}",
          value_parser = ["detect", "2.0", "3.0", "3.1", "3.2", "3.0.0", "3.1.0", "3.2.0"])]
    pub schema: String,
}

/// How much detail to print about subschema (e.g. `oneOf` branch) errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SubschemaErrors {
    BestMatch,
    All,
}

/// Whether to stop at the first validation error of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ValidationErrors {
    First,
    All,
}

/// Runs the CLI with `args` (excluding the program name), writing to `out` and
/// `err`, and returns the process exit code. `http(s)://` `$ref`s are fetched with the
/// library's default retriever (the built-in client when the `http` feature is on).
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let retriever = Options::default().retriever;
    run_with_stdin(args, &mut std::io::stdin(), retriever, out, err)
}

/// Like [`run`], reading the `-` file from `stdin` instead of the process's standard
/// input and fetching `http(s)://` `$ref`s with `retriever` (`None` leaves them
/// unresolvable). The Python package passes a replaced `sys.stdin` and its own
/// retriever this way.
pub fn run_with_stdin<I, T>(
    args: I,
    stdin: &mut (dyn Read + Send),
    retriever: Option<Arc<dyn Retrieve>>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let argv = std::iter::once(OsString::from("openapi-spec-validator-rs"))
        .chain(args.into_iter().map(Into::into));
    let args = match Args::try_parse_from(argv) {
        Ok(args) => args,
        Err(error) => {
            let rendered = error.render().to_string();
            let target: &mut dyn Write = if error.use_stderr() { err } else { out };
            let _ = target.write_all(rendered.as_bytes());
            return error.exit_code();
        }
    };
    // An I/O error here is a broken pipe or similar: nothing sensible left to report.
    execute(&args, &Mutex::new(stdin), retriever, out, err).unwrap_or(1)
}

fn execute(
    args: &Args,
    stdin: &Mutex<&mut (dyn Read + Send)>,
    retriever: Option<Arc<dyn Retrieve>>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> std::io::Result<i32> {
    let subschema_errors = subschema_mode(args, err)?;
    let options = Options {
        version: SpecVersion::from_cli(&args.schema),
        errors: match args.validation_errors {
            ValidationErrors::First => ErrorMode::First,
            ValidationErrors::All => ErrorMode::All,
        },
        base_uri: String::new(),
        retriever,
    };

    // Loading and validating recurse with the document's nesting, which the loader allows
    // up to Python's limits; run on threads whose stacks fit that in any build.
    let results: Vec<FileResult> = match rayon::ThreadPoolBuilder::new()
        .stack_size(WORKER_STACK_SIZE)
        .build()
    {
        Ok(pool) => {
            // The caller's validation settings (cancellation) apply in the pool too.
            let context = ContextHandle::capture();
            pool.install(|| {
                args.files
                    .par_iter()
                    .map(|file| context.install(|| validate_file(file, &options, stdin)))
                    .collect()
            })
        }
        // No thread can be started (a process limit is reached): validate the files one
        // after another on this thread, never through rayon's global pool, which could not
        // start its threads either.
        Err(_) => args
            .files
            .iter()
            .map(|file| validate_file(file, &options, stdin))
            .collect(),
    };

    for result in results {
        let FileResult { name, result } = result;
        match result {
            Err(error) => {
                writeln!(out, "{error}")?;
                return Ok(1);
            }
            Ok(Outcome::Valid) => writeln!(out, "{name}: OK")?,
            Ok(Outcome::Error(message)) => {
                writeln!(out, "{name}: Error: {message}")?;
                return Ok(2);
            }
            Ok(Outcome::Invalid(errors)) => {
                if args.validation_errors == ValidationErrors::All {
                    for (index, error) in errors.iter().enumerate() {
                        print_validation_error(
                            out,
                            &name,
                            error,
                            subschema_errors,
                            Some(index + 1),
                        )?;
                    }
                    writeln!(out, "{name}: {} validation errors found", errors.len())?;
                } else if let Some(error) = errors.first() {
                    print_validation_error(out, &name, error, subschema_errors, None)?;
                }
                return Ok(1);
            }
        }
    }
    Ok(0)
}

/// Resolves `--subschema-errors` and its deprecated alias, warning like upstream.
fn subschema_mode(args: &Args, err: &mut dyn Write) -> std::io::Result<SubschemaErrors> {
    let warn = std::env::var("OPENAPI_SPEC_VALIDATOR_WARN_DEPRECATED").as_deref() != Ok("0");
    let mut mode = args.subschema_errors;
    if let Some(deprecated) = args.deprecated_subschema_errors {
        let message = if mode.is_none() {
            mode = Some(deprecated);
            "--errors/--error is deprecated. Use --subschema-errors instead."
        } else {
            "--errors/--error is deprecated and ignored when --subschema-errors is provided."
        };
        if warn {
            writeln!(err, "DeprecationWarning: {message}")?;
        }
    }
    Ok(mode.unwrap_or(SubschemaErrors::BestMatch))
}

struct FileResult {
    /// The name upstream prints: the argument as given, or `stdin`.
    name: String,
    result: Result<Outcome, LoadError>,
}

fn validate_file(
    file: &str,
    options: &Options,
    stdin: &Mutex<&mut (dyn Read + Send)>,
) -> FileResult {
    if file == "-" || file == "/-" {
        let mut bytes = Vec::new();
        let mut stdin = stdin
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = read_text_until_unreadable(&mut **stdin, &mut bytes)
            .map_err(|error| LoadError {
                message: error.to_string(),
            })
            .and_then(|_| load_text(&bytes, "<stdin>", stdin_errors()))
            .map(|spec| validate(&spec, options));
        return FileResult {
            name: "stdin".to_owned(),
            result,
        };
    }
    FileResult {
        name: file.to_owned(),
        result: validate_path(Path::new(file), options),
    }
}

/// The error handler of Python's `sys.stdin`: `surrogateescape` in UTF-8 mode or under the
/// C/POSIX locale, `strict` otherwise. It decides how undecodable input is reported.
fn stdin_errors() -> TextErrors {
    let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    if env("PYTHONUTF8").as_deref() == Some("1") {
        return TextErrors::SurrogateEscape;
    }
    // Windows has no C locale: Python decodes stdin strictly there.
    if cfg!(windows) {
        return TextErrors::Strict;
    }
    let locale = env("LC_ALL")
        .or_else(|| env("LC_CTYPE"))
        .or_else(|| env("LANG"))
        .unwrap_or_else(|| "C".to_owned());
    if locale == "C" || locale == "POSIX" || locale.starts_with("C.") {
        TextErrors::SurrogateEscape
    } else {
        TextErrors::Strict
    }
}

/// Upstream's `print_validationerror`.
fn print_validation_error(
    out: &mut dyn Write,
    name: &str,
    error: &ValidationError,
    subschema_errors: SubschemaErrors,
    index: Option<usize>,
) -> std::io::Result<()> {
    match index {
        None => writeln!(out, "{name}: Validation Error: {error}")?,
        Some(index) => writeln!(out, "{name}: Validation Error: [{index}] {error}")?,
    }
    if let Some(cause) = error.schema_error.as_ref().and_then(|d| d.cause.as_ref()) {
        writeln!(out, "\n# Cause\n")?;
        writeln!(out, "{cause}")?;
    }
    let context = error
        .schema_error
        .as_ref()
        .map(|details| details.context.as_slice())
        .unwrap_or_default();
    if context.is_empty() {
        return Ok(());
    }
    match subschema_errors {
        SubschemaErrors::All => {
            writeln!(out, "\n\n# Due to one of those errors\n")?;
            let rendered: Vec<String> = context.iter().map(|e| format!("## {e}")).collect();
            writeln!(out, "{}", rendered.join("\n\n\n"))?;
        }
        SubschemaErrors::BestMatch => {
            writeln!(out, "\n\n# Probably due to this subschema error\n")?;
            if let Some(best) = best_match(context) {
                writeln!(out, "## {best}")?;
            }
            if context.len() > 1 {
                writeln!(
                    out,
                    "\n({} more subschemas errors, use --subschema-errors=all to see them.)",
                    context.len() - 1
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(
            std::iter::once("openapi-spec-validator-rs").chain(args.iter().copied()),
        )
    }

    #[test]
    fn defaults_match_upstream() {
        let args = parse(&["spec.yaml"]).unwrap();
        assert_eq!(args.files, ["spec.yaml"]);
        assert_eq!(args.validation_errors, ValidationErrors::First);
        assert_eq!(args.subschema_errors, None);
        assert_eq!(args.schema, "detect");
    }

    #[test]
    fn accepts_upstream_options() {
        let args = parse(&[
            "a.yaml",
            "b.json",
            "--validation-errors",
            "all",
            "--subschema-errors",
            "all",
            "--schema",
            "3.0.0",
        ])
        .unwrap();
        assert_eq!(args.files, ["a.yaml", "b.json"]);
        assert_eq!(args.validation_errors, ValidationErrors::All);
        assert_eq!(args.subschema_errors, Some(SubschemaErrors::All));
        assert_eq!(args.schema, "3.0.0");

        let args = parse(&["a.yaml", "--error", "best-match"]).unwrap();
        assert_eq!(
            args.deprecated_subschema_errors,
            Some(SubschemaErrors::BestMatch)
        );
    }

    #[test]
    fn rejects_bad_usage_with_exit_code_2() {
        assert_eq!(parse(&[]).unwrap_err().exit_code(), 2);
        assert_eq!(
            parse(&["a.yaml", "--schema", "4.0"])
                .unwrap_err()
                .exit_code(),
            2
        );
    }

    #[test]
    fn missing_files_are_read_errors() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(["does-not-exist.yaml"], &mut out, &mut err);
        assert_eq!(code, 1);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "No such file: does-not-exist.yaml\n"
        );
    }
}
