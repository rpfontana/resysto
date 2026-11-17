mod error;
mod extract;

pub use error::{AppError, AppResult};
pub use extract::{Path, Query, TenantAccess, TenantDb, ValidatedJson};

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ApiResponse<T> {
    pub data: T,
}
