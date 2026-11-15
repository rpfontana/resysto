use axum::extract::{FromRef, FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use resysto_surrealdb::{Db, MASTER_DB};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
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

/// A session on the database of the tenant the `{slug}` of the path names
pub struct TenantDb(pub Surreal<Any>);

#[derive(Deserialize)]
struct TenantPath {
    slug: String,
}

impl<S> FromRequestParts<S> for TenantDb
where
    Db: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Path(TenantPath { slug }) =
            Path::<TenantPath>::from_request_parts(parts, state).await?;
        let server = Db::from_ref(state);
        let master = server.system_session(MASTER_DB).await.map_err(internal)?;
        let database = tenant_database(&master, &slug)
            .await
            .map_err(internal)?
            .ok_or(AppError::NotFound)?;
        let db = server.system_session(&database).await.map_err(internal)?;
        Ok(Self(db))
    }
}

// only a ready tenant is reachable: a suspended or deleted one reads as missing
async fn tenant_database(
    master: &Surreal<Any>,
    slug: &str,
) -> Result<Option<String>, surrealdb::Error> {
    let database: Option<String> = master
        .query(
            "SELECT VALUE db_name FROM ONLY tenant WHERE slug = $slug AND state = 'ready' LIMIT 1",
        )
        .bind(("slug", slug.to_owned()))
        .await?
        .check()?
        .take(0)?;
    Ok(database)
}

fn internal(error: impl std::error::Error + Send + Sync + 'static) -> AppError {
    AppError::Internal(error.into())
}

#[cfg(test)]
mod tests {
    use resysto_surrealdb::Config;
    use secrecy::SecretString;
    use surrealdb::types::Uuid;

    use super::*;

    #[derive(Deserialize)]
    struct Settings {
        db: DbSettings,
        auth: AuthSettings,
    }

    #[derive(Deserialize)]
    struct DbSettings {
        endpoint: String,
        namespace: String,
        migrator_user: String,
        migrator_password: SecretString,
    }

    #[derive(Deserialize)]
    struct AuthSettings {
        signing_key: SecretString,
        verifying_key: String,
    }

    // a database standing in for the master, with its own tenant rows
    async fn master() -> Surreal<Any> {
        let Settings { db: settings, auth } = resysto_config::load().unwrap();
        let server = Db::connect(Config {
            endpoint: settings.endpoint,
            namespace: settings.namespace,
            username: settings.migrator_user,
            password: settings.migrator_password,
            signing_key: auth.signing_key,
            verifying_key: auth.verifying_key,
        })
        .await
        .unwrap();
        let name = format!("t_test_{}", Uuid::new_v4().simple());
        server.define_database(&name).await.unwrap();
        server.system_session(&name).await.unwrap()
    }

    #[tokio::test]
    async fn a_slug_reaches_only_a_ready_tenant() {
        let master = master().await;
        master
            .query(
                "CREATE tenant SET slug = 'acme', db_name = 't_acme', state = 'ready';
                 CREATE tenant SET slug = 'paused', db_name = 't_paused', state = 'suspended';
                 CREATE tenant SET slug = 'gone', db_name = 't_gone', state = 'deleted';",
            )
            .await
            .unwrap()
            .check()
            .unwrap();

        let cases = [
            ("acme", Some("t_acme")),
            ("paused", None),
            ("gone", None),
            ("nobody", None),
        ];
        for (slug, database) in cases {
            let found = tenant_database(&master, slug).await.unwrap();
            assert_eq!(found.as_deref(), database, "slug {slug}");
        }
    }
}
