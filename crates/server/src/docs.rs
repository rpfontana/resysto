use std::sync::Arc;

use axum::http::{HeaderValue, header};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router, middleware};
use tower_http::set_header::SetResponseHeaderLayer;
use utoipa::openapi::path::{Operation, ParameterBuilder, ParameterIn};
use utoipa::openapi::schema::{Object, Type};
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityRequirement, SecurityScheme};
use utoipa::openapi::{
    ContentBuilder, InfoBuilder, OpenApi, Ref, RefOr, Required, ResponseBuilder,
};

use crate::state::AppState;

/// The cookie of a signed-in session
const SESSION_COOKIE: &str = "__Host-session";

// Scalar's bundle pinned by version and hash, with every call it could make to Scalar's own
// services turned off: proxy, telemetry, agent, MCP, fonts
const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Resysto API</title>
</head>
<body>
<script id="api-reference" type="application/json" data-url="/api/openapi.json" data-configuration='{"proxyUrl":"","telemetry":false,"withDefaultFonts":false,"agent":{"disabled":true},"mcp":{"disabled":true},"showDeveloperTools":"never"}'></script>
<script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference@1.77.0/dist/browser/standalone.js" integrity="sha384-+7ESN8b9Ljg46TCYKedZUlhF/wfZbOt78aV/DtoU3anEElh8Q2eXsyhQ9W5lAZD4" crossorigin="anonymous"></script>
</body>
</html>
"#;

// whatever the page's configuration says, the browser lets it talk only to this server
const PAGE_POLICY: &str = "default-src 'self'; script-src https://cdn.jsdelivr.net; \
     style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; \
     connect-src 'self'; worker-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; \
     form-action 'none'";

/// `/api/openapi.json` and the page that shows it, `/api/docs`, for signed-in accounts only
pub(crate) fn router(state: &AppState, openapi: OpenApi) -> Router<AppState> {
    let openapi = Arc::new(openapi);
    let spec = move || {
        let openapi = Arc::clone(&openapi);
        async move { Json(&*openapi).into_response() }
    };
    let page = async || {
        (
            [(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(PAGE_POLICY),
            )],
            Html(PAGE),
        )
    };
    Router::new()
        .route("/api/openapi.json", get(spec))
        .route("/api/docs", get(page))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            resysto_auth::require_session,
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
}

/// Completes what the handlers' annotations can't say: who the API is, the session cookie every
/// route but the login needs, and the slug and the refusals every tenant route shares
pub(crate) fn document(mut openapi: OpenApi) -> OpenApi {
    openapi.info = InfoBuilder::new()
        .title("Resysto API")
        .version(env!("CARGO_PKG_VERSION"))
        .description(Some(
            "The REST API of resysto. Errors answer `{ \"error\": { \"type\", \"message\" } }`; \
             the routes of a tenant live under `/t/{slug}/api/v1`.",
        ))
        .build();
    openapi
        .components
        .get_or_insert_with(Default::default)
        .add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(SESSION_COOKIE))),
        );
    for (path, item) in &mut openapi.paths.paths {
        let in_tenant = path.starts_with("/t/{slug}/");
        let operations = [
            &mut item.get,
            &mut item.post,
            &mut item.patch,
            &mut item.put,
            &mut item.delete,
        ];
        for operation in operations.into_iter().flatten() {
            secure(operation);
            if in_tenant {
                enter_tenant(operation);
            }
        }
    }
    openapi
}

// `security(())` on a handler marks a route open without a session
fn secure(operation: &mut Operation) {
    let open = operation
        .security
        .as_ref()
        .is_some_and(|requirements| requirements.contains(&SecurityRequirement::default()));
    if open {
        operation.security = None;
        return;
    }
    operation.security = Some(vec![SecurityRequirement::new(
        "session",
        Vec::<String>::new(),
    )]);
    refusal(operation, "401", "no live session");
}

fn enter_tenant(operation: &mut Operation) {
    let slug = ParameterBuilder::new()
        .name("slug")
        .parameter_in(ParameterIn::Path)
        .required(Required::True)
        .description(Some("The tenant's slug"))
        .schema(Some(Object::with_type(Type::String)))
        .build();
    operation
        .parameters
        .get_or_insert_with(Vec::new)
        .insert(0, RefOr::T(slug));
    refusal(
        operation,
        "403",
        "`forbidden` by the member's roles, or `mfa_required` by the tenant",
    );
    refusal(
        operation,
        "404",
        "no such tenant, or the account is not a member",
    );
}

// a response of the error type, unless the handler already describes that status
fn refusal(operation: &mut Operation, status: &str, description: &str) {
    operation
        .responses
        .responses
        .entry(status.to_owned())
        .or_insert_with(|| {
            let content = ContentBuilder::new()
                .schema(Some(Ref::from_schema_name("ErrorResponse")))
                .build();
            RefOr::T(
                ResponseBuilder::new()
                    .description(description)
                    .content("application/json", content)
                    .build(),
            )
        });
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum_client_ip::ClientIpSource;
    use resysto_auth::{Cipher, MasterName, Passwords, Policies, PublicUrl, create_account};
    use secrecy::SecretString;
    use serde_json::{Value, json};
    use surrealdb::types::Uuid;
    use tower::ServiceExt;

    use super::*;
    use crate::routes::{rest, router};
    use crate::testing;

    // a changed handler or type changes the contract the SPA builds its types from; after
    // reviewing the change, `UPDATE_OPENAPI=1` rewrites the file
    #[test]
    fn the_openapi_document_is_the_one_the_spa_was_built_against() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../openapi.json");
        let openapi = document(rest(|tenant| tenant).into_openapi());
        let generated = format!("{}\n", serde_json::to_string_pretty(&openapi).unwrap());
        if std::env::var_os("UPDATE_OPENAPI").is_some() {
            std::fs::write(path, &generated).unwrap();
            return;
        }
        // git may check the file out with CRLF line endings
        let kept = std::fs::read_to_string(path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        let first_difference = generated
            .lines()
            .zip(kept.lines().chain(std::iter::repeat("")))
            .position(|(new, old)| new != old);
        if let Some(line) = first_difference.or_else(|| (generated != kept).then_some(0)) {
            panic!(
                "openapi.json differs from the handlers from line {}: review the change, then \
                 run the test again with UPDATE_OPENAPI=1",
                line + 1
            );
        }
    }

    #[tokio::test]
    async fn the_api_docs_open_only_to_a_signed_in_account() {
        let server = testing::server().await;
        let master_name = format!("t_test_{}_master", Uuid::new_v4().simple());
        resysto_migrator::provision_master(&server, &master_name)
            .await
            .unwrap();
        let master = server.system_session(&master_name).await.unwrap();
        let passwords = Passwords::new(SecretString::from("pepper")).unwrap();
        let password = "a long enough password";
        let email = "ada@acme.it";
        create_account(&master, &passwords, email.parse().unwrap(), password.into())
            .await
            .unwrap();
        let state = AppState {
            db: server,
            master: MasterName(master_name.into()),
            passwords,
            cipher: Cipher::new(&SecretString::from("7".repeat(64))).unwrap(),
            policies: Arc::new(Policies::load().unwrap()),
            public: PublicUrl("http://localhost:3000".into()),
        };
        let app = router(state, "no-site", ClientIpSource::ConnectInfo);
        let get = async |path: &str, cookie: Option<&str>| {
            let mut request = Request::get(path);
            if let Some(cookie) = cookie {
                request = request.header(header::COOKIE, cookie);
            }
            app.clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap()
        };

        for path in ["/api/docs", "/api/openapi.json"] {
            assert_eq!(
                get(path, None).await.status(),
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
        let login = Request::post("/api/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "email": email, "password": password }).to_string(),
            ))
            .unwrap();
        let signed_in = app.clone().oneshot(login).await.unwrap();
        let cookie = signed_in.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();

        let page = get("/api/docs", Some(&cookie)).await;
        assert_eq!(page.status(), StatusCode::OK);
        let policy = page.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        assert!(policy.contains("connect-src 'self'"), "{policy}");
        let spec = get("/api/openapi.json", Some(&cookie)).await;
        assert_eq!(spec.status(), StatusCode::OK);
        let body = to_bytes(spec.into_body(), usize::MAX).await.unwrap();
        let spec: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            spec["paths"]["/t/{slug}/api/v1/people"].is_object(),
            "{spec}"
        );
    }
}
