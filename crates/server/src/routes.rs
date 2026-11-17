use axum::http::{HeaderValue, header};
use axum::{Router, middleware};
use resysto_http::AppError;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::state::AppState;
use crate::{scope, site};

pub(crate) fn router(state: AppState, site_dir: &str) -> Router {
    Router::new()
        .nest("/t/{slug}/api/v1", tenant_api(state.clone()))
        .nest(
            "/api/v1",
            api(Router::new().nest("/auth", resysto_auth::router())),
        )
        .with_state(state)
        .merge(site::router(site_dir))
}

// each bounded context gets the database alone, so its handlers cannot reach the auth's keys
fn tenant_api(state: AppState) -> Router<AppState> {
    let db = state.db.clone();
    api(Router::new()
        .nest(
            "/organizations",
            resysto_org::organization::router().with_state(db.clone()),
        )
        .nest("/people", resysto_people::person::router().with_state(db))
        // route layers run from the last added: the member first, then its scope
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            scope::widen_to_branches,
        ))
        .route_layer(middleware::from_fn_with_state(
            state,
            resysto_auth::require_member,
        )))
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
