mod error;
mod extract;

pub use error::{AppError, AppResult};
pub use extract::{IfMatch, Path, Query, TenantAccess, TenantDb, ValidatedJson};

use axum::Json;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    pub data: T,
}

/// One record answered as `{ "data": … }`, with its version as the `ETag`
#[derive(Debug)]
pub struct Tagged<T> {
    pub version: i64,
    pub data: T,
}

impl<T: Serialize> IntoResponse for Tagged<T> {
    fn into_response(self) -> Response {
        let mut response = Json(ApiResponse { data: self.data }).into_response();
        if let Ok(etag) = HeaderValue::try_from(format!("\"{}\"", self.version)) {
            response.headers_mut().insert(header::ETAG, etag);
        }
        response
    }
}
