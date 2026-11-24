use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("External process/API error: {0}")]
    External(String),

    #[error("{0}")]
    Validation(String),

    #[error("{0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
