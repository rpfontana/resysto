use std::sync::Arc;

use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, header};
use axum::{Router, middleware};
use axum_client_ip::ClientIpSource;
use resysto_http::AppError;
use tower_http::request_id::PropagateRequestIdLayer;
use tower_http::set_header::{SetRequestHeaderLayer, SetResponseHeaderLayer};
use uuid::Uuid;

use crate::mcp::{Mcp, Mounted};
use crate::state::AppState;
use crate::{scope, site};

pub(crate) fn router(state: AppState, site_dir: &str, ip_source: ClientIpSource) -> Router {
    Router::new()
        .nest("/api/v1", api(account_routes()))
        .nest("/t/{slug}/api/v1", api(tenant_routes(&state)))
        .nest("/mcp", api(mcp_routes(&state, tools())))
        .merge(oauth_routes())
        .with_state(state)
        .merge(site::router(site_dir))
        .layer(ip_source.into_extension())
}

// no cookie authenticates these routes, so they need no same-origin check
fn oauth_routes() -> Router<AppState> {
    with_request_ids(resysto_auth::oauth_router())
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

// the tools of each bounded context, under the REST resource the policies decide them on
fn tools() -> Vec<Mounted> {
    let mounted = [
        ("organizations", resysto_org::organization::tools()),
        ("people", resysto_people::person::tools()),
    ];
    mounted
        .into_iter()
        .flat_map(|(resource, tools)| {
            tools
                .into_iter()
                .map(move |tool| Mounted { resource, tool })
        })
        .collect()
}

// the tenant and its scope come with each call, not with the request
pub(crate) fn mcp_routes(state: &AppState, tools: Vec<Mounted>) -> Router<AppState> {
    let mcp = Mcp::new(state.db.clone(), Arc::clone(&state.policies), tools);
    Router::new()
        .route_service("/", mcp.service(&state.public))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            resysto_auth::require_agent,
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

    // the MCP server takes `tenant` out of every call before the tool sees it
    #[test]
    fn no_tool_takes_a_parameter_named_tenant() {
        for Mounted { tool, .. } in tools() {
            let takes = tool.input_schema["properties"].get("tenant").is_some();
            assert!(!takes, "{} takes a tenant parameter", tool.name);
        }
    }
}
