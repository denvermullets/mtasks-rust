use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("missing API token — configure one in Settings")]
    MissingToken,

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("validation failed: {}", .errors.join("; "))]
    Validation { errors: Vec<String> },

    #[error("server error ({status}): {message}")]
    Server { status: u16, message: String },

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("decode error: {0}")]
    Decode(String),

    #[error("enum mapping error: {0}")]
    EnumMapping(String),
}
