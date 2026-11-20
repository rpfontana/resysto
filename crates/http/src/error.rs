use std::collections::BTreeMap;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;
use validator::ValidationErrors;

/// The body of every error answer
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    #[serde(rename = "type")]
    pub kind: ErrorKind,
    /// In English, for people and agents; the SPA shows its own text for `type`
    pub message: String,
    /// The messages of each invalid field, for `validation_error`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<BTreeMap<String, Vec<String>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    NotFound,
    ValidationError,
    Unauthorized,
    Forbidden,
    MfaRequired,
    Conflict,
    PreconditionRequired,
    PreconditionFailed,
    Unprocessable,
    TooManyRequests,
    InternalError,
}

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
    #[error("forbidden")]
    Forbidden,
    /// The session has to prove a second factor first
    #[error("a second factor is required")]
    MfaRequired,
    #[error("{0}")]
    Conflict(String),
    /// A write without the `If-Match` it needs
    #[error("If-Match is required: send the ETag of the record, from its last read")]
    PreconditionRequired,
    /// `If-Match` names a version the record no longer has
    #[error("{0}")]
    PreconditionFailed(String),
    #[error("{0}")]
    Unprocessable(String),
    /// `retry_after` in seconds, when the wait is known
    #[error("{message}")]
    TooManyRequests {
        message: String,
        retry_after: Option<u64>,
    },
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, kind) = match &self {
            Self::NotFound => (StatusCode::NOT_FOUND, ErrorKind::NotFound),
            Self::Validation(_) | Self::ValidationFields(_) => {
                (StatusCode::BAD_REQUEST, ErrorKind::ValidationError)
            }
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, ErrorKind::Unauthorized),
            Self::Forbidden => (StatusCode::FORBIDDEN, ErrorKind::Forbidden),
            Self::MfaRequired => (StatusCode::FORBIDDEN, ErrorKind::MfaRequired),
            Self::Conflict(_) => (StatusCode::CONFLICT, ErrorKind::Conflict),
            Self::PreconditionRequired => (
                StatusCode::PRECONDITION_REQUIRED,
                ErrorKind::PreconditionRequired,
            ),
            Self::PreconditionFailed(_) => (
                StatusCode::PRECONDITION_FAILED,
                ErrorKind::PreconditionFailed,
            ),
            Self::Unprocessable(_) => (StatusCode::UNPROCESSABLE_ENTITY, ErrorKind::Unprocessable),
            Self::TooManyRequests { .. } => {
                (StatusCode::TOO_MANY_REQUESTS, ErrorKind::TooManyRequests)
            }
            Self::Internal(error) => {
                // the only place the details are visible
                tracing::error!(error = ?error, "internal server error");
                (StatusCode::INTERNAL_SERVER_ERROR, ErrorKind::InternalError)
            }
        };
        let message = match &self {
            Self::Internal(_) => "an internal error occurred".to_owned(),
            other => other.to_string(),
        };
        let fields = match &self {
            Self::ValidationFields(fields) => Some(fields.clone()),
            _ => None,
        };
        let body = ErrorResponse {
            error: ErrorBody {
                kind,
                message,
                fields,
            },
        };
        let mut response = (status, Json(body)).into_response();
        if let Self::TooManyRequests {
            retry_after: Some(seconds),
            ..
        } = self
        {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
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
