use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use resysto_http::{ApiResponse, AppError, AppResult, Path, Query, TenantDb, ValidatedJson};
use resysto_model::OrganizationId;
use resysto_surrealdb::{Db, Page, PageRequest};
use serde::{Deserialize, Serialize};
use validator::Validate;

use super::model::{
    self, NewOrganization, OrganizationError, OrganizationPatch, OrganizationRecord,
};

/// The routes of `/t/{slug}/api/v1/organizations`
pub fn router() -> Router<Db> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", get(read).patch(update).delete(delete))
        .route("/{id}/reassign", post(reassign))
}

// the path also carries the tenant slug, so the id is read by name
#[derive(Debug, Deserialize)]
struct ById {
    id: OrganizationId,
}

#[derive(Debug, Deserialize, Validate)]
struct Reassign {
    to: OrganizationId,
}

#[derive(Debug, Serialize)]
struct Reassigned {
    reassigned: u64,
}

async fn list(
    TenantDb(db): TenantDb,
    Query(request): Query<PageRequest>,
) -> AppResult<Json<Page<OrganizationRecord>>> {
    Ok(Json(model::list(&db, request).await?))
}

async fn create(
    TenantDb(db): TenantDb,
    ValidatedJson(input): ValidatedJson<NewOrganization>,
) -> AppResult<(StatusCode, Json<ApiResponse<OrganizationRecord>>)> {
    let data = model::create(&db, input).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse { data })))
}

async fn read(
    TenantDb(db): TenantDb,
    Path(ById { id }): Path<ById>,
) -> AppResult<Json<ApiResponse<OrganizationRecord>>> {
    let data = model::get(&db, id).await?;
    Ok(Json(ApiResponse { data }))
}

async fn update(
    TenantDb(db): TenantDb,
    Path(ById { id }): Path<ById>,
    ValidatedJson(patch): ValidatedJson<OrganizationPatch>,
) -> AppResult<Json<ApiResponse<OrganizationRecord>>> {
    let data = model::update(&db, id, patch).await?;
    Ok(Json(ApiResponse { data }))
}

async fn reassign(
    TenantDb(db): TenantDb,
    Path(ById { id }): Path<ById>,
    ValidatedJson(body): ValidatedJson<Reassign>,
) -> AppResult<Json<ApiResponse<Reassigned>>> {
    let reassigned = model::reassign(&db, id, body.to).await?;
    Ok(Json(ApiResponse {
        data: Reassigned { reassigned },
    }))
}

async fn delete(TenantDb(db): TenantDb, Path(ById { id }): Path<ById>) -> AppResult<StatusCode> {
    model::delete(&db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

impl From<OrganizationError> for AppError {
    fn from(error: OrganizationError) -> Self {
        match error {
            OrganizationError::NotFound => Self::NotFound,
            OrganizationError::Forbidden => Self::Forbidden,
            OrganizationError::HasChildren | OrganizationError::InUse(_) => {
                Self::Conflict(error.to_string())
            }
            OrganizationError::ParentNotFound
            | OrganizationError::OwnParent
            | OrganizationError::Cycle
            | OrganizationError::TooDeep
            | OrganizationError::SameTarget
            | OrganizationError::TargetNotFound => Self::Unprocessable(error.to_string()),
            OrganizationError::Db(error) => Self::Internal(error.into()),
        }
    }
}
