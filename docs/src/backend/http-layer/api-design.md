# API Design

An API where every endpoint returns data in a slightly different shape makes its clients spend more time reading docs (or guessing) than building. Our API is predictable for everyone who calls it, starting with our own Svelte UI. This chapter covers the REST conventions, response patterns, pagination strategies, and documentation approaches that make it so.

## REST conventions

REST isn't a formal specification with strict rules. It's more a set of conventions that most API consumers have come to expect. These are the ones we follow.

**Use plural nouns for resources.** `/api/v1/users`, not `/api/v1/user`. The resource name represents a collection, and individual items within that collection get accessed by their ID.

**Use HTTP methods to express the operation.** `GET` retrieves data, `POST` creates new resources, `PUT` replaces a resource entirely, `PATCH` applies a partial update, and `DELETE` removes a resource. Not `POST` for everything.

**Use appropriate status codes.** This is one of the highest-leverage things for API usability. When a client creates a resource, return `201 Created`, not `200 OK`. When a delete succeeds, return `204 No Content`. When the client sends invalid input, return `400 Bad Request` with details about what went wrong. When the requested resource doesn't exist, return `404 Not Found`.

The status codes we reach for most often:

| Code | Meaning | When to use |
|------|---------|------------|
| 200 | OK | Successful read or update |
| 201 | Created | A new resource was created |
| 204 | No Content | Successful operation with no response body (delete) |
| 400 | Bad Request | Invalid input or validation failure |
| 401 | Unauthorized | Missing or invalid authentication |
| 403 | Forbidden | Authenticated but not authorized |
| 404 | Not Found | Resource does not exist |
| 409 | Conflict | Business rule violation (duplicate email, etc.) |
| 422 | Unprocessable Entity | Semantically invalid request |
| 500 | Internal Server Error | Unexpected server failure |

## API versioning

The API is versioned from the start, with a path prefix. The UI ships in lockstep with the server, in the same binary, so it doesn't need the prefix; external clients do, because they need stability guarantees that we can't provide through coordination alone. The cost of stamping `/v1` on the routes from day one is basically nothing, while adding versioning later means either breaking changes or awkward workarounds.

A version prefix doesn't replace disciplined schema evolution: additive changes, deprecation windows, contract tests.

Path-based versioning looks like this:

```rust
fn api_routes() -> Router<AppState> {
    Router::new()
        .nest("/api/v1", v1_routes())
}

fn v1_routes() -> Router<AppState> {
    Router::new()
        .nest("/users", user_routes())
        .nest("/posts", post_routes())
}
```

When a particular endpoint eventually needs a v2, it goes alongside v1 without disturbing existing consumers:

```rust
fn api_routes() -> Router<AppState> {
    Router::new()
        .nest("/api/v1", v1_routes())
        .nest("/api/v2", v2_routes())
}
```

Handler implementations stay decoupled from the version prefix, so that v1 and v2 routes can share the same underlying service logic where the behavior hasn't changed.

## Consistent response shapes

Every response is wrapped in a standard type. When every endpoint returns data in a predictable structure, clients can write generic parsing logic instead of special-casing each endpoint.

```rust
#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub data: T,
}

// The shape of every list
#[derive(Serialize)]
pub struct Page<T> {
    pub data: Vec<T>,
    /// Cursor for the next page; `None` on the last page.
    pub next_cursor: Option<Cursor>,
}
```

Error responses use the format described in the [Error Handling](../core-patterns/error-handling.md) chapter. That way, clients can always check for an `error` field to determine whether the request succeeded.

## Pagination

Every endpoint that returns a list is paginated, from the first version. A list without pagination works until one table grows to a few hundred thousand rows; then it times out or runs out of memory, and adding pagination at that point breaks every client.

We paginate with a cursor (keyset pagination). A page ends with `next_cursor`, and the client sends it back as `after` to get the next one, until `next_cursor` is `null`. `limit` defaults to 20 and is capped at 200.

```
GET /api/v1/users?limit=20
-> { "data": [ ...20 users... ], "next_cursor": "0193a2f4-..." }

GET /api/v1/users?limit=20&after=0193a2f4-...
-> { "data": [ ...7 users... ], "next_cursor": null }
```

The cursor is the id of the last row on the page. Ids are time-ordered UUID v7, so rows come in creation order and the next page starts right after the cursor: the database jumps to it instead of reading and skipping the rows before. Clients treat the cursor as opaque: they pass it back unchanged and never build one.

List queries don't handle cursors by hand: the database crate has a helper that applies `after` and `limit` and returns `data` with `next_cursor`.

### Why not offset

Offset-based pagination (`LIMIT $n START $offset`) is the simplest approach, and it's what lets a UI jump to page 37 and show "page 37 of 120". It doesn't work with the amount of data resysto reads:

- **Every page costs more than the one before.** The database reads the skipped rows and throws them away. Page 5,000 of 200 rows reads a million rows, and walking a whole table page by page is quadratic.
- **The total is a full read.** "Page X of Y" needs `count()` over every row that matches the filter, on every request.
- **Concurrent writes move the pages.** A row inserted or deleted between two requests shifts every later page, so the client skips rows or sees them twice at the boundary.

So lists have no page numbers and no exact totals: the UI loads the next page as the user scrolls or asks for more.

## Filtering and sorting

For list endpoints that need filtering, we accept filter parameters as query strings:

```rust
#[derive(Debug, Deserialize)]
pub struct UserListParams {
    pub after: Option<Cursor>,
    pub limit: Option<u32>,
    pub role: Option<Role>,
    pub search: Option<String>,
    #[serde(default)]
    pub sort: UserSort,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserSort {
    #[default]
    Created,
    Name,
}
```

Sort orders are an enum, never a free field name, because each one needs its own index. The index ends with `id`, so the order is unique (`FIELDS name, id` for `Name`), and the cursor carries the values of the last row for every field in it.

A filter is applied while the range scan reads rows, until `$limit` of them pass it. That's fine for a filter that discards few rows, and slow for one that discards almost all of them: that one needs an index on the filtered field, checked with `EXPLAIN`.

## OpenAPI documentation

API docs written by hand are accurate the day someone writes them and slowly drift out of date after that. We generate the documentation directly from the code. The `utoipa` crate produces OpenAPI specifications from Rust types and handler annotations. Since the docs come from the actual code, they can't go stale.

The same spec is the contract with the UI: `openapi-typescript` generates the Svelte client's types from it, so a change to a Rust type that breaks the UI shows up in the UI's type check.

```rust
use utoipa::{OpenApi, ToSchema};

#[derive(Serialize, ToSchema)]
pub struct UserResponse {
    pub id: Uuid,
    pub name: String,
    pub email: String,
    pub created_at: DateTime<Utc>,
}

#[utoipa::path(
    post,
    path = "/api/v1/users",
    request_body = CreateUserDto,
    responses(
        (status = 201, description = "User created successfully", body = UserResponse),
        (status = 400, description = "Validation error", body = ErrorResponse),
        (status = 409, description = "User with this email already exists", body = ErrorResponse),
    ),
    tag = "users"
)]
async fn create_user(
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<CreateUserDto>,
) -> AppResult<(StatusCode, Json<UserResponse>)> {
    // ...
}
```

To serve the Swagger UI alongside the API:

```rust
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[derive(OpenApi)]
#[openapi(
    paths(create_user, get_user, list_users, update_user, delete_user),
    components(schemas(UserResponse, CreateUserDto, UpdateUserDto, ErrorResponse)),
    tags((name = "users", description = "User management endpoints"))
)]
struct ApiDoc;

let app = Router::new()
    .merge(api_routes())
    .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
    .with_state(state);
```

Developers can browse the API documentation at `/swagger-ui` and try out requests directly from the browser. The specification is generated from the actual types and handlers, so it can't fall out of sync with the implementation.

## Health check endpoints

The server has health check endpoints. The load balancer and container orchestrator need a way to know whether the service is alive and ready to accept traffic.

```rust
/// Liveness probe: is the process running and able to handle requests?
async fn health_live() -> StatusCode {
    StatusCode::OK
}

/// Readiness probe: is the application ready to serve traffic?
/// Checks that the dependencies the instance can't serve without are reachable.
async fn health_ready(State(state): State<AppState>) -> StatusCode {
    match state.db.health().await {
        Ok(()) => StatusCode::OK,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
```

They're mounted outside the versioned API routes so they remain stable even when the API evolves:

```rust
let app = Router::new()
    .route("/health", get(health_live))
    .route("/health/ready", get(health_ready))
    .merge(api_routes())  // api_routes() already nests under /api/v1
    .with_state(state);
```

The liveness probe is fast and unconditional. All it tells the orchestrator is "yes, the process is alive." The readiness probe is the one that checks whether the dependencies (SurrealDB, and whatever else the instance can't serve without) are healthy, so the orchestrator knows it's safe to route traffic to this instance. Getting these two confused is a common mistake, and it can lead to the orchestrator restarting healthy containers just because the database had a brief hiccup.

With the API designed this way, we have a solid foundation: consistent URLs, predictable response shapes, pagination that won't fall over at scale, docs that stay accurate, and health checks that keep our infrastructure informed.
