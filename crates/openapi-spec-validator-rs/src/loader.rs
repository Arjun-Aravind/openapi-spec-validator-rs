//! Document loading (temporary JSON-only version).

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LoadError {
    pub message: String,
}

pub fn load(bytes: &[u8], _stream_name: &str) -> Result<serde_json::Value, LoadError> {
    serde_json::from_slice(bytes).map_err(|e| LoadError {
        message: e.to_string(),
    })
}
