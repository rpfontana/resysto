use axum::Json;
use axum::extract::FromRef;
use axum::http::StatusCode;
use resysto_http::{
    ApiResponse, AppError, AppResult, ErrorResponse, IfMatch, Path, Query, Tagged, TenantDb,
    ValidatedJson,
};
use resysto_model::OrganizationId;
use resysto_surrealdb::{Db, Page, PageRequest};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use validator::Validate;

use super::model::{NewOrganization, OrganizationPatch, OrganizationRecord};
use super::ops::{self, OrganizationError};

/// The routes of `/t/{slug}/api/v1/organizations`
pub fn router<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Db: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(read, update, delete))
        .routes(routes!(reassign))
}

// the path also carries the tenant slug, so the id is read by name
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
struct ById {
    id: OrganizationId,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
#[serde(deny_unknown_fields)]
struct Reassign {
    /// The organization that takes the records over
    to: OrganizationId,
}

#[derive(Debug, Serialize, ToSchema)]
struct Reassigned {
    /// How many records moved
    reassigned: u64,
}

/// List organizations
///
/// The ones the member reaches, newest first
#[utoipa::path(
    get,
    operation_id = "organizations_list",
    path = "/",
    tag = "organizations",
    params(PageRequest),
    responses((status = 200, body = Page<OrganizationRecord>)),
)]
async fn list(
    TenantDb(db): TenantDb,
    Query(request): Query<PageRequest>,
) -> AppResult<Json<Page<OrganizationRecord>>> {
    Ok(Json(ops::list(&db, request).await?))
}

/// Create an organization
///
/// At the root, or below `parent`
#[utoipa::path(
    post,
    operation_id = "organizations_create",
    path = "/",
    tag = "organizations",
    request_body = NewOrganization,
    responses(
        (status = 201, body = ApiResponse<OrganizationRecord>, headers(("ETag" = String))),
        (status = 400, body = ErrorResponse),
        (status = 422, body = ErrorResponse, description = "the parent is missing or too deep"),
    ),
)]
async fn create(
    TenantDb(db): TenantDb,
    ValidatedJson(input): ValidatedJson<NewOrganization>,
) -> AppResult<(StatusCode, Tagged<OrganizationRecord>)> {
    let data = ops::create(&db, input).await?;
    Ok((
        StatusCode::CREATED,
        Tagged {
            version: data.version,
            data,
        },
    ))
}

/// Get an organization
#[utoipa::path(
    get,
    operation_id = "organizations_get",
    path = "/{id}",
    tag = "organizations",
    params(ById),
    responses(
        (status = 200, body = ApiResponse<OrganizationRecord>, headers(("ETag" = String))),
        (status = 404, body = ErrorResponse),
    ),
)]
async fn read(
    TenantDb(db): TenantDb,
    Path(ById { id }): Path<ById>,
) -> AppResult<Tagged<OrganizationRecord>> {
    let data = ops::get(&db, id).await?;
    Ok(Tagged {
        version: data.version,
        data,
    })
}

/// Update an organization
///
/// A JSON Merge Patch; a new `parent` moves the whole branch
#[utoipa::path(
    patch,
    operation_id = "organizations_update",
    path = "/{id}",
    tag = "organizations",
    params(ById, IfMatch),
    request_body = OrganizationPatch,
    responses(
        (status = 200, body = ApiResponse<OrganizationRecord>, headers(("ETag" = String))),
        (status = 400, body = ErrorResponse),
        (status = 404, body = ErrorResponse),
        (status = 412, body = ErrorResponse, description = "the organization changed since its read"),
        (status = 422, body = ErrorResponse, description = "the move would make a cycle or a tree too deep"),
        (status = 428, body = ErrorResponse, description = "If-Match is missing"),
    ),
)]
async fn update(
    TenantDb(db): TenantDb,
    Path(ById { id }): Path<ById>,
    IfMatch(version): IfMatch,
    ValidatedJson(patch): ValidatedJson<OrganizationPatch>,
) -> AppResult<Tagged<OrganizationRecord>> {
    let data = ops::update(&db, id, version, patch).await?;
    Ok(Tagged {
        version: data.version,
        data,
    })
}

/// Reassign an organization's records
///
/// Moves every record of the organization to another one, so that it can be deleted
#[utoipa::path(
    post,
    operation_id = "organizations_reassign",
    path = "/{id}/reassign",
    tag = "organizations",
    params(ById),
    request_body = Reassign,
    responses(
        (status = 200, body = ApiResponse<Reassigned>),
        (status = 404, body = ErrorResponse),
        (status = 422, body = ErrorResponse, description = "the target is missing or the same"),
    ),
)]
async fn reassign(
    TenantDb(db): TenantDb,
    Path(ById { id }): Path<ById>,
    ValidatedJson(body): ValidatedJson<Reassign>,
) -> AppResult<Json<ApiResponse<Reassigned>>> {
    let reassigned = ops::reassign(&db, id, body.to).await?;
    Ok(Json(ApiResponse {
        data: Reassigned { reassigned },
    }))
}

/// Delete an organization
///
/// Only one without branches that no record carries
#[utoipa::path(
    delete,
    operation_id = "organizations_delete",
    path = "/{id}",
    tag = "organizations",
    params(ById, IfMatch),
    responses(
        (status = 204),
        (status = 404, body = ErrorResponse),
        (status = 409, body = ErrorResponse, description = "it has branches, or records carry it"),
        (status = 412, body = ErrorResponse, description = "the organization changed since its read"),
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

impl From<OrganizationError> for AppError {
    fn from(error: OrganizationError) -> Self {
        match error {
            OrganizationError::NotFound => Self::NotFound,
            OrganizationError::Stale(_) => Self::PreconditionFailed(error.to_string()),
            OrganizationError::NothingToChange => Self::Validation(error.to_string()),
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
