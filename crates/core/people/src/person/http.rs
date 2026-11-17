use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use resysto_http::{ApiResponse, AppError, AppResult, Path, Query, TenantDb, ValidatedJson};
use resysto_model::PersonId;
use resysto_surrealdb::{Db, Page, PageRequest};
use serde::Deserialize;

use super::model::{self, NewPerson, PersonError, PersonPatch, PersonRecord};

/// The routes of `/t/{slug}/api/v1/people`
pub fn router() -> Router<Db> {
    Router::new()
        .route("/", get(list).post(create))
        .route("/{id}", get(read).patch(update).delete(delete))
}

// the path also carries the tenant slug, so the id is read by name
#[derive(Debug, Deserialize)]
struct ById {
    id: PersonId,
}

async fn list(
    TenantDb(db): TenantDb,
    Query(request): Query<PageRequest>,
) -> AppResult<Json<Page<PersonRecord>>> {
    Ok(Json(model::list(&db, request).await?))
}

async fn create(
    TenantDb(db): TenantDb,
    ValidatedJson(input): ValidatedJson<NewPerson>,
) -> AppResult<(StatusCode, Json<ApiResponse<PersonRecord>>)> {
    let data = model::create(&db, input).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse { data })))
}

async fn read(
    TenantDb(db): TenantDb,
    Path(ById { id }): Path<ById>,
) -> AppResult<Json<ApiResponse<PersonRecord>>> {
    let data = model::get(&db, id).await?;
    Ok(Json(ApiResponse { data }))
}

async fn update(
    TenantDb(db): TenantDb,
    Path(ById { id }): Path<ById>,
    ValidatedJson(patch): ValidatedJson<PersonPatch>,
) -> AppResult<Json<ApiResponse<PersonRecord>>> {
    let data = model::update(&db, id, patch).await?;
    Ok(Json(ApiResponse { data }))
}

async fn delete(TenantDb(db): TenantDb, Path(ById { id }): Path<ById>) -> AppResult<StatusCode> {
    model::delete(&db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

impl From<PersonError> for AppError {
    fn from(error: PersonError) -> Self {
        match error {
            PersonError::NotFound => Self::NotFound,
            PersonError::Forbidden => Self::Forbidden,
            PersonError::OrganizationsNotFound(_) => Self::Unprocessable(error.to_string()),
            PersonError::Db(error) => Self::Internal(error.into()),
        }
    }
}
