use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use resysto_http::{AppError, AppResult, IfMatch, Path, Query, Tagged, TenantDb, ValidatedJson};
use resysto_model::PersonId;
use resysto_surrealdb::{Db, Page, PageRequest};
use serde::Deserialize;

use super::model::{NewPerson, PersonPatch, PersonRecord};
use super::ops::{self, PersonError};

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
    Ok(Json(ops::list(&db, request).await?))
}

async fn create(
    TenantDb(db): TenantDb,
    ValidatedJson(input): ValidatedJson<NewPerson>,
) -> AppResult<(StatusCode, Tagged<PersonRecord>)> {
    let data = ops::create(&db, input).await?;
    Ok((
        StatusCode::CREATED,
        Tagged {
            version: data.version,
            data,
        },
    ))
}

async fn read(
    TenantDb(db): TenantDb,
    Path(ById { id }): Path<ById>,
) -> AppResult<Tagged<PersonRecord>> {
    let data = ops::get(&db, id).await?;
    Ok(Tagged {
        version: data.version,
        data,
    })
}

async fn update(
    TenantDb(db): TenantDb,
    Path(ById { id }): Path<ById>,
    IfMatch(version): IfMatch,
    ValidatedJson(patch): ValidatedJson<PersonPatch>,
) -> AppResult<Tagged<PersonRecord>> {
    let data = ops::update(&db, id, version, patch).await?;
    Ok(Tagged {
        version: data.version,
        data,
    })
}

async fn delete(
    TenantDb(db): TenantDb,
    Path(ById { id }): Path<ById>,
    IfMatch(version): IfMatch,
) -> AppResult<StatusCode> {
    ops::delete(&db, id, version).await?;
    Ok(StatusCode::NO_CONTENT)
}

impl From<PersonError> for AppError {
    fn from(error: PersonError) -> Self {
        match error {
            PersonError::NotFound => Self::NotFound,
            PersonError::Stale(_) => Self::PreconditionFailed(error.to_string()),
            PersonError::NothingToChange => Self::Validation(error.to_string()),
            PersonError::Forbidden => Self::Forbidden,
            PersonError::OrganizationsNotFound(_) => Self::Unprocessable(error.to_string()),
            PersonError::Db(error) => Self::Internal(error.into()),
        }
    }
}
