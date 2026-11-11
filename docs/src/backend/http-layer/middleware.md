# Middleware

Some logic doesn't belong in any single handler but needs to run on many (or all) requests: logging, authentication, compression, rate limiting, request IDs, timeouts. In Axum this is middleware, and because Axum's middleware system is built on Tower, we get access to a huge ecosystem of pre-built components plus a clean way to compose our own.

What trips people up most often isn't writing middleware. It's getting the ordering right, so we start there.

## The Tower layer model

Tower thinks of middleware as "layers" that wrap a service. When a request comes in, it passes through each layer in order, from the outermost to the innermost (which is the handler). The response then travels back out through the layers in reverse. So the first layer added is the first to see the request and the last to see the response.

```
Request → Compression → Tracing → Timeout → Auth → Handler
Response ← Compression ← Tracing ← Timeout ← Auth ← Handler
```

This ordering matters more than it looks. If tracing has to record how long the entire request took, including time spent in authentication, the tracing layer has to be outside the auth layer. Get it backwards and the timing data will be wrong in subtle ways that are annoying to debug.

## The middleware stack

The server's middleware stack, built with `ServiceBuilder`:

```rust
use axum::http::StatusCode;
use tower::ServiceBuilder;
use tower_http::{
    compression::CompressionLayer,
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, SetRequestIdLayer, PropagateRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use std::time::Duration;

let app = Router::new()
    .merge(api_routes())
    .layer(
        ServiceBuilder::new()
            // Layers execute top-to-bottom on the request path.
            .layer(CompressionLayer::new())
            .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
            .layer(TraceLayer::new_for_http())
            .layer(PropagateRequestIdLayer::x_request_id())
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                Duration::from_secs(30),
            ))
            .layer(RequestBodyLimitLayer::new(1024 * 1024)) // 1 MB
    )
    .with_state(state);
```

What each layer does and why it sits where it does:

**CompressionLayer** compresses response bodies using gzip (or brotli, or deflate, depending on what the client supports) and sets the `Content-Encoding` header. It sits at the outermost position so it compresses the final response body after all inner layers have finished producing it. Compression applies to the body only, not to HTTP headers.

**SetRequestIdLayer** generates a unique UUID and attaches it to the incoming request. It comes before `TraceLayer` so the request ID is already available when the tracing span gets created.

**TraceLayer** records structured log entries for each request, including the method, path, status code, and duration. Because it runs after the request ID is set, the tracing span can include the request ID, which is what you search for when digging through logs later.

**PropagateRequestIdLayer** copies the request ID onto the outgoing response. It sits after `TraceLayer` so the response header is set before tracing finalizes the response side of the span. This ordering (set, then trace, then propagate) follows tower-http's own documentation and ensures that request IDs show up consistently in both request logs and response headers.

**TimeoutLayer** cancels requests that take longer than the specified duration and answers `408 Request Timeout`. This protects against slow clients, runaway queries, and other situations where a request just hangs forever.

**RequestBodyLimitLayer** rejects request bodies that exceed a size limit. It's a basic defense against denial-of-service attacks where a client sends an enormous payload.

## CORS

A browser only makes a CORS request when a page calls an API on another origin, and ours never does. In production axum serves the UI and the API from the same origin, and in development Vite proxies API calls to axum, so the browser sees a single origin there too. The server has no `CorsLayer`.

If a browser on another origin ever needs to call the API, the CORS policy comes from runtime configuration, not from the build profile. Using `cfg!(debug_assertions)` would be wrong here: a release build deployed to a staging environment should still have relaxed CORS, and a debug build running against production data should still be restrictive. Allowing all origins in production is a real security hole.

## Writing custom middleware with `from_fn`

Most custom middleware isn't reusable across projects: it just needs to run on requests in this application. For that, Axum gives us `middleware::from_fn`, which lets you write middleware as a plain async function. It's much simpler than implementing the full Tower `Layer` and `Service` traits, and it covers the vast majority of custom middleware needs.

```rust
use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};

async fn timing_middleware(req: Request, next: Next) -> Response {
    let start = std::time::Instant::now();
    let method = req.method().clone();
    let uri = req.uri().clone();

    let response = next.run(req).await;

    let duration = start.elapsed();
    tracing::info!(
        method = %method,
        uri = %uri,
        status = %response.status(),
        duration_ms = %duration.as_millis(),
        "request completed"
    );

    response
}
```

Applying it to the router:

```rust
use axum::middleware;

let app = Router::new()
    .merge(api_routes())
    .layer(middleware::from_fn(timing_middleware))
    .with_state(state);
```

When the middleware needs access to the application state, say, to look up a JWT secret for authentication, use `from_fn_with_state`:

```rust
async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let token = req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?;

    let claims = decode_jwt(token, state.config.jwt_secret.expose_secret())
        .map_err(|_| AppError::Unauthorized)?;

    // Store the authenticated user in request extensions so handlers can access it
    req.extensions_mut().insert(AuthUser::from(claims));

    Ok(next.run(req).await)
}

// Apply to specific routes
let protected_routes = Router::new()
    .route("/profile", get(get_profile))
    .route("/settings", put(update_settings))
    .route_layer(middleware::from_fn_with_state(state.clone(), auth_middleware));
```

## `route_layer` vs. `layer`

This distinction trips up a lot of people, and it matters for correctness.

**`.layer()`** applies the middleware to all routes on the router, including fallback handlers for unmatched paths. An authentication layer here means even 404 responses for nonexistent paths require authentication. That's not what we want: unauthenticated users get a 401 instead of a 404 for paths that don't exist.

**`.route_layer()`** applies the middleware only to routes that actually matched. Unmatched paths pass through to the fallback handler without hitting the middleware at all. This is what we use for authentication and authorization.

```rust
let app = Router::new()
    // Public routes
    .route("/health", get(health_check))
    .route("/api/v1/auth/login", post(login))
    // Protected routes
    .nest("/api/v1", protected_routes)
    // Global middleware (applied to everything)
    .layer(TraceLayer::new_for_http())
    .with_state(state);

let protected_routes = Router::new()
    .nest("/users", user_routes())
    .nest("/posts", post_routes())
    // Auth middleware only applies to matched routes
    .route_layer(middleware::from_fn_with_state(state, auth_middleware));
```

## Available middleware crates

Before writing custom middleware, check whether the ecosystem already solves the problem. The Tower and tower-http ecosystems are rich:

- **tower-http** includes `CorsLayer`, `CompressionLayer`, `TraceLayer`, `TimeoutLayer`, `RequestBodyLimitLayer`, `SetRequestIdLayer`, and many more
- **tower-governor** provides rate limiting based on the governor algorithm
- **tower-sessions** handles server-side sessions
- **tower-cookies** provides cookie management
- **axum-csrf-sync-pattern** implements the OWASP CSRF Synchronizer Token Pattern

The Tower ecosystem covers most of what we need out of the box. It's one of the best reasons to use Axum in the first place.
