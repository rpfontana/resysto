use axum::Json;
use axum::extract::FromRef;
use axum::http::StatusCode;
use resysto_http::{
    ApiResponse, AppError, AppResult, ErrorResponse, IfMatch, Path, Query, Tagged, TenantDb,
    ValidatedJson,
};
use resysto_model::PersonId;
use resysto_surrealdb::{Db, Page, PageRequest};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::model::{NewPerson, PersonPatch, PersonRecord};
use super::ops::{self, PersonError};

/// The routes of `/t/{slug}/api/v1/people`
pub fn router<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Db: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(read, update, delete))
}

// the path also carries the tenant slug, so the id is read by name
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
struct ById {
    id: PersonId,
}

/// List people
///
/// The people of the organizations the member reaches, newest first
#[utoipa::path(
    get,
    operation_id = "people_list",
    path = "/",
    tag = "people",
    params(PageRequest),
    responses((status = 200, body = Page<PersonRecord>)),
)]
async fn list(
    TenantDb(db): TenantDb,
    Query(request): Query<PageRequest>,
) -> AppResult<Json<Page<PersonRecord>>> {
    Ok(Json(ops::list(&db, request).await?))
}

/// Create a person
#[utoipa::path(
    post,
    operation_id = "people_create",
    path = "/",
    tag = "people",
    request_body = NewPerson,
    responses(
        (status = 201, body = ApiResponse<PersonRecord>, headers(("ETag" = String))),
        (status = 400, body = ErrorResponse),
        (status = 422, body = ErrorResponse, description = "an organization is missing"),
    ),
)]
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

/// Get a person
#[utoipa::path(
    get,
    operation_id = "people_get",
    path = "/{id}",
    tag = "people",
    params(ById),
    responses(
        (status = 200, body = ApiResponse<PersonRecord>, headers(("ETag" = String))),
        (status = 404, body = ErrorResponse),
    ),
)]
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

/// Update a person
///
/// A JSON Merge Patch
#[utoipa::path(
    patch,
    operation_id = "people_update",
    path = "/{id}",
    tag = "people",
    params(ById, IfMatch),
    request_body = PersonPatch,
    responses(
        (status = 200, body = ApiResponse<PersonRecord>, headers(("ETag" = String))),
        (status = 400, body = ErrorResponse),
        (status = 404, body = ErrorResponse),
        (status = 412, body = ErrorResponse, description = "the person changed since its read"),
        (status = 422, body = ErrorResponse, description = "an organization is missing"),
        (status = 428, body = ErrorResponse, description = "If-Match is missing"),
    ),
)]
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

/// Delete a person
#[utoipa::path(
    delete,
    operation_id = "people_delete",
    path = "/{id}",
    tag = "people",
    params(ById, IfMatch),
    responses(
        (status = 204),
        (status = 404, body = ErrorResponse),
        (status = 412, body = ErrorResponse, description = "the person changed since its read"),
        (status = 428, body = ErrorResponse, description = "If-Match is missing"),
    ),
)]
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
