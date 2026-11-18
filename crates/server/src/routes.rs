use axum::http::{HeaderValue, header};
use axum::{Router, middleware};
use resysto_http::AppError;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::state::AppState;
use crate::{scope, site};

pub(crate) fn router(state: AppState, site_dir: &str) -> Router {
    Router::new()
        .nest("/api/v1", api(account_routes()))
        .nest("/t/{slug}/api/v1", api(tenant_routes(&state)))
        .with_state(state)
        .merge(site::router(site_dir))
}

fn account_routes() -> Router<AppState> {
    Router::new().nest("/auth", resysto_auth::router())
}

fn tenant_routes(state: &AppState) -> Router<AppState> {
    Router::new()
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
    routes
        .fallback(async || AppError::NotFound)
        .layer(middleware::from_fn(resysto_auth::same_origin))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
}
