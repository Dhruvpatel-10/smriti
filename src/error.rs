use thiserror::Error;

#[derive(Error, Debug)]
pub enum KbError {
    #[error("database error: {context} — {source}")]
    Database {
        context: String,
        source: Box<surrealdb::Error>,
    },

    #[error("memory not found: {0}")]
    NotFound(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("queue error: {0}")]
    Queue(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("llm error: {0}")]
    Llm(String),

    #[error("embedding error: {0}")]
    Embedding(String),

    #[error("data corruption: {0}")]
    DataCorruption(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl KbError {
    pub fn db(context: impl Into<String>, source: surrealdb::Error) -> Self {
        Self::Database {
            context: context.into(),
            source: Box::new(source),
        }
    }

    /// Sanitized error message safe for MCP client consumption.
    /// Strips internal paths and implementation details.
    pub fn client_message(&self) -> String {
        match self {
            Self::InvalidInput(msg) => format!("invalid input: {msg}"),
            Self::NotFound(id) => format!("not found: {id}"),
            Self::Database { .. } => "database error (check server logs)".to_string(),
            Self::Embedding(_) => "embedding error (check server logs)".to_string(),
            Self::DataCorruption(_) => "data corruption (check server logs)".to_string(),
            Self::Io(_) => "i/o error (check server logs)".to_string(),
            Self::Llm(_) => "llm error (check server logs)".to_string(),
            Self::Config(msg) => format!("config error: {msg}"),
            Self::Json(_) => "serialization error (check server logs)".to_string(),
            Self::Queue(msg) => format!("queue error: {msg}"),
        }
    }
}

pub type Result<T> = std::result::Result<T, KbError>;
