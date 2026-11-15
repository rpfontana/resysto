use std::collections::BTreeMap;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use validator::ValidationErrors;

/// An API error, answered as `{ "error": { "type", "message" } }`
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("resource not found")]
    NotFound,
    #[error("{0}")]
    Validation(String),
    #[error("request validation failed")]
    ValidationFields(BTreeMap<String, Vec<String>>),
    #[error("authentication required")]
    Unauthorized,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unprocessable(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, kind) = match &self {
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            Self::Validation(_) | Self::ValidationFields(_) => {
                (StatusCode::BAD_REQUEST, "validation_error")
            }
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::Unprocessable(_) => (StatusCode::UNPROCESSABLE_ENTITY, "unprocessable"),
            Self::Internal(error) => {
                // the only place the details are visible
                tracing::error!(error = ?error, "internal server error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        };
        let body = match &self {
            Self::ValidationFields(fields) => json!({
                "error": { "type": kind, "message": self.to_string(), "fields": fields }
            }),
            Self::Internal(_) => json!({
                "error": { "type": kind, "message": "an internal error occurred" }
            }),
            _ => json!({ "error": { "type": kind, "message": self.to_string() } }),
        };
        (status, Json(body)).into_response()
    }
}

impl From<JsonRejection> for AppError {
    fn from(rejection: JsonRejection) -> Self {
        Self::Validation(rejection.body_text())
    }
}

impl From<PathRejection> for AppError {
    fn from(rejection: PathRejection) -> Self {
        Self::Validation(rejection.body_text())
    }
}

impl From<QueryRejection> for AppError {
    fn from(rejection: QueryRejection) -> Self {
        Self::Validation(rejection.body_text())
    }
}

impl From<ValidationErrors> for AppError {
    fn from(errors: ValidationErrors) -> Self {
        let fields = errors
            .field_errors()
            .into_iter()
            .map(|(field, errors)| {
                let messages = errors
                    .iter()
                    .map(|error| match &error.message {
                        Some(message) => message.to_string(),
                        None => format!("{field} is invalid"),
                    })
                    .collect();
                (field.to_string(), messages)
            })
            .collect();
        Self::ValidationFields(fields)
    }
}
