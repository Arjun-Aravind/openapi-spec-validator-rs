//! Python `re` compatibility (temporary minimal version).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyRegexError {
    pub msg: String,
    pub pos: Option<usize>,
}

pub struct PyRegex {
    regex: fancy_regex::Regex,
}

impl PyRegex {
    pub fn compile(pattern: &str) -> Result<PyRegex, PyRegexError> {
        if pattern.contains("(?<") && !pattern.contains("(?<=") && !pattern.contains("(?<!") {
            return Err(PyRegexError {
                msg: "unknown extension ?<".to_owned(),
                pos: None,
            });
        }
        fancy_regex::Regex::new(pattern)
            .map(|regex| PyRegex { regex })
            .map_err(|e| PyRegexError {
                msg: e.to_string(),
                pos: None,
            })
    }

    pub fn search(&self, text: &str) -> Result<bool, String> {
        self.regex.is_match(text).map_err(|e| e.to_string())
    }
}

pub fn is_valid(pattern: &str) -> bool {
    PyRegex::compile(pattern).is_ok()
}
