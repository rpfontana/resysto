# Routing and Handlers

Every HTTP request that hits the server needs a place to land, and in Axum that place is a handler. A handler is just an async function that takes zero or more extractors as arguments and returns something that implements `IntoResponse`. Axum handles the plumbing: it deserializes request data into the extractor types and serializes the return value back into an HTTP response.

Handlers are thin. Their job is to pull data out of the request, hand it off to a service or repository, and turn the result into a response. Business logic, database queries, or complex branching inside a handler belong somewhere else. Thin handlers, always.

## Defining routes

Axum's router uses method chaining. You define individual routes with `.route()`, group related routes under a common prefix with `.nest()`, and combine separate route groups with `.merge()`.

```rust
use axum::{routing::get, Router};
use tower_http::services::{ServeDir, ServeFile};

pub fn router(state: AppState, site: &str) -> Router {
    let spa = ServeDir::new(site).fallback(ServeFile::new(format!("{site}/200.html")));

    Router::new()
        .nest("/api/v1", api_routes())
        .route("/health", get(health_check))
        .route("/health/ready", get(readiness_check))
        .fallback_service(spa)
        .with_state(state)
}

fn api_routes() -> Router<AppState> {
    Router::new()
        .nest("/users", user_routes())
        .nest("/posts", post_routes())
        .fallback(|| async { AppError::NotFound })
}

fn user_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_users).post(create_user))
        .route("/{id}", get(get_user).patch(update_user).delete(delete_user))
}

fn post_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_posts).post(create_post))
        .route("/{id}", get(get_post).patch(update_post).delete(delete_post))
}
```

Routes are split into separate functions (or even separate files) to keep the router definition readable as the application grows. Each function returns a `Router<AppState>`, and the main router composes them together. This is also where route-specific middleware goes, which the [Middleware](./middleware.md) chapter covers.

The same router serves the Svelte UI. Any path that no route matches goes to `fallback_service`, which serves the matching file from the UI build, or `200.html` when there is none, so the client-side router can take over. A nested router inherits that fallback, so without one of its own an unknown `/api/v1/...` path would answer 200 with the UI's HTML. `api_routes()` sets its own fallback, and unknown API paths get a proper 404 in the API's error format.

## Writing handlers

Well-structured handlers all follow the same rhythm: extract, delegate, respond.

```rust
use axum::{extract::{State, Path}, http::StatusCode, Json};

async fn create_user(
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<CreateUserDto>,
) -> AppResult<(StatusCode, Json<UserResponse>)> {
    let name = UserName::parse(&payload.name)
        .map_err(|e| AppError::Validation(e.to_string()))?;
    let email = Email::parse(&payload.email)
        .map_err(|e| AppError::Validation(e.to_string()))?;

    let user = state.user_service
        .register(name, email, &payload.password)
        .await?;

    Ok((StatusCode::CREATED, Json(user.into())))
}

async fn get_user(
    State(state): State<AppState>,
    Path(id): Path<UserId>,
) -> AppResult<Json<UserResponse>> {
    let user = state.user_service
        .get_by_id(id)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(user.into()))
}

async fn list_users(
    State(state): State<AppState>,
    Query(pagination): Query<PaginationParams>,
) -> AppResult<Json<PaginatedResponse<UserResponse>>> {
    let (users, next_cursor) = state.user_service
        .list(pagination.after.as_deref(), pagination.limit.clamp(1, 200))
        .await?;

    let response = PaginatedResponse {
        data: users.into_iter().map(Into::into).collect(),
        next_cursor,
    };

    Ok(Json(response))
}

async fn delete_user(
    State(state): State<AppState>,
    Path(id): Path<UserId>,
) -> AppResult<StatusCode> {
    state.user_service
        .delete(id)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}
```

The conventions: when we successfully create something, we return `StatusCode::CREATED` (201) with the created resource in the body. A successful deletion returns `StatusCode::NO_CONTENT` (204) with no body, because there's nothing left to show. And when a lookup finds nothing, we return `AppError::NotFound`, which the `IntoResponse` implementation on `AppError` turns into a proper 404 response. Each handler does very little work. That's the goal.

## Extractors

Extractors are how Axum pulls data out of an incoming request. Under the hood, they implement either `FromRequest` (if they need to consume the request body) or `FromRequestParts` (if they only need headers, query parameters, path segments, or other metadata without touching the body).

The built-in extractors cover most of what a handler needs:

```rust
use axum::extract::{State, Path, Query, Json};

// Path parameters: /users/{id}
async fn get_user(Path(id): Path<UserId>) -> ... { }

// Query parameters: /users?after=<cursor>&limit=50
async fn list_users(Query(params): Query<PaginationParams>) -> ... { }

// JSON body
async fn create_user(Json(body): Json<CreateUserDto>) -> ... { }

// Application state
async fn handler(State(state): State<AppState>) -> ... { }
```

A handler can use multiple extractors. The one constraint is that at most one extractor can consume the request body (like `Json`), and it has to come last in the argument list. If the order is wrong, the compiler says so.

## The `#[debug_handler]` macro

When a handler fails to compile because of trait bound errors, the compiler's messages can be really opaque. Axum provides a `#[debug_handler]` macro that makes things much clearer:

```rust
#[axum::debug_handler]
async fn my_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateUserDto>,
) -> AppResult<Json<UserResponse>> {
    // ...
}
```

It adds extra type checking that produces human-readable errors like "argument #2 must implement FromRequest" instead of a wall of trait bound failures. It has no runtime cost, so there's no need to remove it once the handler compiles. It saves real time when someone comes back to change a handler six months later.

## Response types

Axum is flexible about what a handler can return. Anything that implements `IntoResponse` works, and there are quite a few built-in implementations. The patterns we use most often:

```rust
// Just a status code
async fn health_check() -> StatusCode {
    StatusCode::OK
}

// A tuple of status code and body
async fn create_resource() -> (StatusCode, Json<Resource>) {
    (StatusCode::CREATED, Json(resource))
}

// A Result for fallible operations
async fn get_resource() -> Result<Json<Resource>, AppError> {
    Ok(Json(resource))
}

// Headers and body together
async fn with_headers() -> (StatusCode, [(HeaderName, &'static str); 1], Json<Data>) {
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "max-age=3600")],
        Json(data),
    )
}
```

Most handlers return the `Result` type, because most handlers can fail in one way or another. `Result<T, AppError>` gives the error mapping from the [Error Handling](../core-patterns/error-handling.md) chapter, and it keeps handler code focused on the happy path.

## Keeping handlers thin

This is worth repeating, because it goes wrong across every language and framework. When a handler starts growing beyond 15 or 20 lines, that's usually a sign that logic has crept into the wrong place.

What lives in a handler:
- Extracting data from the request (via Axum extractors)
- Parsing raw input into domain types (calling `UserName::parse()`, `Email::parse()`, etc.)
- Calling a single service method
- Converting the result to an HTTP response

What doesn't live in a handler:
- Database queries
- Business rule enforcement
- Calls to multiple services that need to be coordinated
- Complex conditional logic
- Sending emails, starting Restate jobs, or other side effects

If a handler needs to do several things in coordination, that coordination logic belongs in a service method.
