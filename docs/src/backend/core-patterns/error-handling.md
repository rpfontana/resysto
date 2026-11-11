# Error Handling

Every error in resysto has two audiences. The client, whether that's our Svelte UI or anyone else calling the API, needs clear feedback when something goes wrong: a proper HTTP status code and a message that says what to fix. The team needs the full picture: error chains, backtraces, and enough context to track down the bug. The patterns in this chapter serve both without leaking internal details to the outside world.

Axum requires that all handlers are infallible at the framework level, which means they always have to return a valid HTTP response, even when things go sideways. A handler does this by returning `Result<T, E>` where `E` implements `IntoResponse`. When the handler returns `Err(e)`, Axum calls `e.into_response()` to produce the error response. This gives us full control over how errors look to the client.

## The AppError pattern

The server has one central error enum, `AppError`, that covers all the error cases the API can produce. Every handler returns `Result<T, AppError>`, and the `IntoResponse` implementation on `AppError` maps each variant to the right HTTP status code and response body:

```rust
use std::collections::HashMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("resource not found")]
    NotFound,

    #[error("{0}")]
    Validation(String),

    #[error("validation failed")]
    ValidationFields(HashMap<String, Vec<String>>),

    #[error("authentication required")]
    Unauthorized,

    #[error("insufficient permissions")]
    Forbidden,

    #[error("{0}")]
    Conflict(String),

    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_type, message) = match &self {
            AppError::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                self.to_string(),
            ),
            AppError::Validation(msg) => (
                StatusCode::BAD_REQUEST,
                "validation_error",
                msg.clone(),
            ),
            AppError::ValidationFields(fields) => {
                let body = serde_json::json!({
                    "error": {
                        "type": "validation_error",
                        "message": "request validation failed",
                        "fields": fields,
                    }
                });
                return (StatusCode::BAD_REQUEST, Json(body)).into_response();
            }
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                self.to_string(),
            ),
            AppError::Forbidden => (
                StatusCode::FORBIDDEN,
                "forbidden",
                self.to_string(),
            ),
            AppError::Conflict(msg) => (
                StatusCode::CONFLICT,
                "conflict",
                msg.clone(),
            ),
            AppError::Internal(err) => {
                // Log the full error chain for debugging. This is the only
                // place where the real error details are visible.
                tracing::error!(error = ?err, "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "an internal error occurred".to_string(),
                )
            }
        };

        let body = serde_json::json!({
            "error": {
                "type": error_type,
                "message": message,
            }
        });

        (status, Json(body)).into_response()
    }
}
```

The most important piece here is the `Internal` variant. It wraps `anyhow::Error`, which can hold any error type along with a chain of context messages. When an internal error happens, we log the full details (the error chain and, if available, a backtrace) but return only a generic message to the client. This is how we prevent information leakage: database schema details, SurrealDB error messages and internal file paths never show up in API responses.

## A type alias for convenience

Handlers don't spell out `Result<T, AppError>`. A type alias keeps their signatures clean:

```rust
pub type AppResult<T> = Result<T, AppError>;
```

A handler looks like this:

```rust
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
```

The `?` operator just works here. That's because of the `From` implementations on `AppError`, which come next.

## The error mapping chain

In our layered architecture, errors start at the bottom (the infrastructure layer), pass through the domain layer, and eventually reach the API layer where they become HTTP responses. Each layer has its own error types, and `From` implementations convert between them.

Here's what the chain looks like for a "create user" operation:

```
surrealdb::Error
  → CreateUserError (domain)
      → AppError (api)
          → HTTP Response
```

The infrastructure layer catches the database error and converts it into something the domain understands. SurrealDB 4 has no typed error for a unique index violation: in a record session it comes back as a validation error whose message says the index "already contains a conflicting value". The repository is the only place that knows this, so it's the only place to change if SurrealDB changes it:

```rust
// In the repository implementation
.map_err(|e| {
    if e.is_validation() && e.message().contains("already contains") {
        CreateUserError::Duplicate { email: req.email.clone() }
    } else {
        CreateUserError::Unknown(e.into())
    }
})
```

Then the API layer converts that domain error into an `AppError`:

```rust
impl From<CreateUserError> for AppError {
    fn from(err: CreateUserError) -> Self {
        match err {
            CreateUserError::Duplicate { email } => {
                AppError::Conflict(
                    format!("a user with email {} already exists", email.as_str())
                )
            }
            CreateUserError::Unknown(inner) => AppError::Internal(inner),
        }
    }
}
```

With these `From` implementations in place, the handler just uses `?` and lets the compiler figure out the conversions. No manual mapping, no boilerplate:

```rust
async fn create_user(
    State(state): State<AppState>,
    Json(payload): Json<CreateUserDto>,
) -> AppResult<(StatusCode, Json<UserResponse>)> {
    let user = state.user_service.register(payload).await?;  // ? handles the full chain
    Ok((StatusCode::CREATED, Json(user.into())))
}
```

## thiserror vs. anyhow

We use two different error crates. They serve complementary purposes, and the backend uses both.

**thiserror** is for errors that the code needs to match on. It generates `Display` and `Error` trait implementations from the enum definitions. We use it for domain errors (`CreateUserError`, `AuthError`), for the `AppError` enum, and for any error type where callers need to distinguish between variants and react differently.

**anyhow** is for errors that the code just needs to propagate. It gives you a single `anyhow::Error` type that can hold any error, along with context messages that form a chain explaining what went wrong. We use it for the catch-all `Internal` variant of `AppError`, and in infrastructure code where we want to add context without defining a new error variant for every possible failure mode.

The rule: use `thiserror` at boundaries where callers need to make decisions based on the error variant, and use `anyhow` for the "everything else" case where the error just needs to be logged and reported.

## Adding context with anyhow

The `anyhow::Context` trait attaches human-readable context to errors as they propagate up the call stack. It seems like extra work when you're writing the code, but it's what you need when you're debugging. It tells you not just what failed, but why the code was trying to do that thing in the first place.

```rust
use anyhow::Context;
use resysto_surrealdb::{Config, Db};

pub async fn connect_db(config: Config) -> anyhow::Result<Db> {
    Db::connect(config)
        .await
        .context("failed to connect to SurrealDB")
}
```

When this error gets logged, the output will include both the original error ("connection refused") and the context ("failed to connect to SurrealDB"). That's the difference between staring at a cryptic error message and immediately knowing where to look.

## Rules

**No `unwrap()`, `expect()` or `panic!` outside tests.** Clippy denies them across the workspace, along with every other way to panic such as slice indexing and `todo!()`, and `clippy.toml` allows them only in tests. A panic in a handler doesn't just affect that one request. If a panic happens while holding a mutex or other shared resource, it can poison the resource and cascade to other requests. Errors always go through the `Result` type.

**Never expose internal error messages to clients.** Database errors, file paths, stack traces, and internal service names are all information that an attacker can use. Log them at the `error` level for the team, and return a generic "internal error" message to the client.

**Use `thiserror` for structured errors and `anyhow` for catch-all propagation.** Don't use `anyhow` as a handler's return type directly, because you lose the ability to map different errors to different HTTP status codes. Use it inside the `AppError::Internal` variant instead.

**Use `#[from]` for automatic conversions.** The `thiserror` `#[from]` attribute generates `From` implementations that let the `?` operator convert errors automatically. This keeps handler code clean and focused on the happy path.

**Add context with `.context()` liberally in infrastructure code.** The small cost of writing a context string pays off enormously when someone is debugging something in production at 2 AM and the error log says "failed to persist user registration" instead of just "connection reset by peer".
