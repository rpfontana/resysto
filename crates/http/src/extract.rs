use axum::extract::{FromRef, FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use resysto_surrealdb::{Db, Grant};
use serde::de::DeserializeOwned;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::RecordId;
use validator::Validate;

use crate::AppError;

/// `axum::extract::Path` that rejects in the API error format
#[derive(Debug, FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(AppError))]
pub struct Path<T>(pub T);

/// `axum::extract::Query` that rejects in the API error format
#[derive(Debug, FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(AppError))]
pub struct Query<T>(pub T);

/// A JSON body that deserialized and passed its `Validate` rules
#[derive(Debug)]
pub struct ValidatedJson<T>(pub T);

impl<T, S> FromRequest<S> for ValidatedJson<T>
where
    T: DeserializeOwned + Validate,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let axum::Json(value) = axum::Json::<T>::from_request(request, state).await?;
        value.validate()?;
        Ok(Self(value))
    }
}

/// Where an authenticated and authorized request may go in its tenant, which the server's
/// middleware puts in the request extensions
#[derive(Debug, Clone)]
pub struct TenantAccess {
    pub database: String,
    pub account: RecordId,
    pub grant: Grant,
}

/// A record session on the tenant's database, limited by the grant of the request
pub struct TenantDb(pub Surreal<Any>);

impl<S> FromRequestParts<S> for TenantDb
where
    Db: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let access = parts
            .extensions
            .get::<TenantAccess>()
            .ok_or(AppError::Unauthorized)?;
        let db = Db::from_ref(state)
            .record_session(&access.database, access.account.clone(), &access.grant)
            .await
            .map_err(|error| AppError::Internal(error.into()))?;
        Ok(Self(db))
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Request;

    use super::*;

    struct Unreachable;

    impl FromRef<Unreachable> for Db {
        fn from_ref(_: &Unreachable) -> Self {
            panic!("a request without authentication reached the database");
        }
    }

    #[tokio::test]
    async fn a_request_without_authentication_never_reaches_a_tenant() {
        let (mut parts, ()) = Request::new(()).into_parts();

        let refused = TenantDb::from_request_parts(&mut parts, &Unreachable).await;

        assert!(
            matches!(refused, Err(AppError::Unauthorized)),
            "{:?}",
            refused.err()
        );
    }
}
