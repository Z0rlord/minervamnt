use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum MintError {
    #[error("quote not found: {0}")]
    QuoteNotFound(String),

    #[error("quote not paid: {0}")]
    QuoteNotPaid(String),

    #[error("quote already issued: {0}")]
    QuoteAlreadyIssued(String),

    #[error("token already spent")]
    TokenAlreadySpent,

    #[error("transaction unbalanced: inputs {inputs} != outputs {outputs}")]
    Unbalanced { inputs: u64, outputs: u64 },

    #[error("insufficient VTXO liquidity: needed {needed_msat} msat")]
    InsufficientLiquidity { needed_msat: u64 },

    #[error("token mapping not found: {0}")]
    MappingNotFound(String),

    #[error("keyset not found: {0}")]
    KeysetNotFound(String),

    #[error("ark client error: {0}")]
    Ark(String),

    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

impl MintError {
    fn status(&self) -> StatusCode {
        match self {
            MintError::QuoteNotFound(_)
            | MintError::MappingNotFound(_)
            | MintError::KeysetNotFound(_) => StatusCode::NOT_FOUND,
            MintError::Ark(_) | MintError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
            MintError::InsufficientLiquidity { .. } => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::BAD_REQUEST,
        }
    }

    /// Cashu error codes are integers per NUT error conventions; we use a
    /// coarse mapping for the scaffold.
    fn code(&self) -> u16 {
        match self {
            // 12001 = keyset not found per Cashu NUT error codes.
            MintError::KeysetNotFound(_) => 12001,
            MintError::TokenAlreadySpent => 11001,
            MintError::Unbalanced { .. } => 11002,
            MintError::QuoteNotPaid(_) => 20001,
            MintError::QuoteAlreadyIssued(_) => 20002,
            _ => 10000,
        }
    }
}

impl IntoResponse for MintError {
    fn into_response(self) -> Response {
        let body = json!({
            "detail": self.to_string(),
            "code": self.code(),
        });
        (self.status(), Json(body)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, MintError>;
