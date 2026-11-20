use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, header};
use axum::{Router, middleware};
use axum_client_ip::ClientIpSource;
use resysto_http::AppError;
use tower_http::request_id::PropagateRequestIdLayer;
use tower_http::set_header::{SetRequestHeaderLayer, SetResponseHeaderLayer};
use uuid::Uuid;

use crate::state::AppState;
use crate::{scope, site};

pub(crate) fn router(state: AppState, site_dir: &str, ip_source: ClientIpSource) -> Router {
    Router::new()
        .nest("/api/v1", api(account_routes()))
        .nest("/t/{slug}/api/v1", api(tenant_routes(&state)))
        .with_state(state)
        .merge(site::router(site_dir))
        .layer(ip_source.into_extension())
}

fn account_routes() -> Router<AppState> {
    Router::new().nest("/auth", resysto_auth::router())
}

fn tenant_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .nest("/audit", resysto_auth::audit_router())
        .nest("/organizations", resysto_org::organization::router())
        .nest("/people", resysto_people::person::router())
        // route layers run from the last added: the member first, then its scope
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            scope::widen_to_branches,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            resysto_auth::require_member,
        ))
}

// without its own fallback an unknown API path would get the SPA's page;
// API answers must stay out of browser and proxy caches
fn api(routes: Router<AppState>) -> Router<AppState> {
    let routes = routes
        .fallback(async || AppError::NotFound)
        .layer(middleware::from_fn(resysto_auth::same_origin))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));
    with_request_ids(routes)
}

// every request gets an x-request-id from the server, replacing any the client sent; the audit
// records it and the answer carries it back
fn with_request_ids<S: Clone + Send + Sync + 'static>(routes: Router<S>) -> Router<S> {
    routes
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestHeaderLayer::overriding(
            HeaderName::from_static("x-request-id"),
            |_: &Request| HeaderValue::try_from(Uuid::now_v7().to_string()).ok(),
        ))
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::HeaderMap;
    use axum::routing::get;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn the_server_makes_every_request_id_and_answers_with_it() {
        // answers with the id it received
        let echo = async |headers: HeaderMap| {
            headers
                .get("x-request-id")
                .and_then(|id| id.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        let app = with_request_ids(Router::new().route("/", get(echo)));
        let request = Request::get("/")
            .header("x-request-id", "chosen-by-the-client")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        let answered = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let seen = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(seen, answered);
        let id = Uuid::try_parse(&seen).unwrap();
        assert_eq!(id.get_version_num(), 7, "{seen}");
    }
}
