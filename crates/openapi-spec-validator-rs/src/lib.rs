//! A fast, drop-in Rust companion to [openapi-spec-validator].
//!
//! Validates Swagger 2.0 and OpenAPI 3.0, 3.1 and 3.2 documents with the same
//! rules (and the same verdicts) as the Python implementation.
//!
//! [openapi-spec-validator]: https://github.com/python-openapi/openapi-spec-validator

pub mod version;

pub use version::{SpecVersion, detect_version};
