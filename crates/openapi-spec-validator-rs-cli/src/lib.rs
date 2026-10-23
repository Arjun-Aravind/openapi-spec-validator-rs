//! Command-line interface compatible with `openapi-spec-validator`.

use std::ffi::OsString;
use std::io::Write;

use clap::{Parser, ValueEnum};

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
/// `err`, and returns the process exit code.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> i32
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
    let _ = writeln!(err, "validation is not implemented yet: {:?}", args.files);
    2
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
}
