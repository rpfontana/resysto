# Authentication and Authorization

Every request has to answer two questions: "who is making this request?" and "are they allowed to do what they're asking?" The first is authentication, the second is authorization. Resysto manages its customers' security and compliance data, so it needs both.

Axum's extractor system does most of the work here. Custom extractors validate credentials and check permissions, and adding auth to a handler is just a matter of putting the right extractor in its argument list.

## Authentication strategies

The right approach depends on who the client is, and resysto has two kinds.

**The Svelte UI** is a first-party browser application, so it uses server-side sessions with cookies. Session IDs live in `HttpOnly`, `Secure`, `SameSite=Strict` cookies, which means JavaScript can't access them. This materially reduces the risk of cookie theft through XSS. `HttpOnly` doesn't prevent XSS itself, and an injected script can still act on the user's behalf or exfiltrate other in-page data. But it does prevent the most common attack vector of stealing the session token directly, which is why OWASP and MDN both recommend it. The server stores session state (user ID, expiry) in SurrealDB, so sessions can be revoked immediately. The `tower-sessions` crate provides session middleware for Axum, and `axum-login` layers identification, authentication, and authorization on top. Cookie-based sessions also need CSRF protection.

**External clients**, such as service-to-service calls and third-party integrations, use stateless JWTs. The client includes a token in the `Authorization: Bearer` header, and the server validates it without hitting a database. This is simpler for machine clients that don't have a browser cookie jar. The tradeoff is that JWTs can't be revoked before expiry without additional infrastructure (a deny-list or short lifetimes with refresh tokens).

The rest of this chapter shows the JWT approach, because it shows off Axum's extractor pattern nicely and the pattern is the same for the UI's sessions: the extractor reads the session instead of the bearer token.

## JWT authentication with a custom extractor

The flow is straightforward: the client authenticates, gets back a JWT, and then includes it in the `Authorization` header on every subsequent request. The server validates the token each time without needing to look anything up in a database.

The key to making this work cleanly in Axum is a custom extractor that implements `FromRequestParts`. When this extractor is in a handler's signature, Axum runs the validation logic automatically before the handler body ever executes:

```rust
use axum::{
    extract::FromRequestParts,
    http::request::Parts,
};
use axum_extra::{
    headers::{Authorization, authorization::Bearer},
    TypedHeader,
};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

/// Represents an authenticated user. Including this in a handler's
/// signature automatically requires and validates a JWT.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: UserId,
    pub email: String,
    pub role: Role,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Claims {
    pub sub: UserId,     // subject (user ID)
    pub email: String,
    pub role: Role,
    pub iss: String,     // issuer
    pub aud: String,     // audience
    pub exp: u64,        // expiration time
    pub iat: u64,        // issued at
}

impl<S> FromRequestParts<S> for AuthUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);

        // Extract the Authorization: Bearer <token> header
        let TypedHeader(Authorization(bearer)) = parts
            .extract::<TypedHeader<Authorization<Bearer>>>()
            .await
            .map_err(|_| AppError::Unauthorized)?;

        // Decode and validate the JWT with explicit validation rules.
        // Validation::default() uses HS256 and checks exp, but we are
        // explicit about what we accept.
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&["resysto"]);
        validation.set_audience(&["resysto-api"]);
        validation.leeway = 30; // 30 seconds of clock skew tolerance

        let token_data = decode::<Claims>(
            bearer.token(),
            &DecodingKey::from_secret(
                app_state.config.jwt_secret.expose_secret().as_bytes()
            ),
            &validation,
        )
        .map_err(|_| AppError::Unauthorized)?;

        Ok(AuthUser {
            user_id: token_data.claims.sub,
            email: token_data.claims.email,
            role: token_data.claims.role,
        })
    }
}
```

Using it in a handler is as simple as adding the parameter to the function signature:

```rust
async fn get_my_profile(
    user: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<ProfileResponse>> {
    let profile = state.user_service
        .get_by_id(user.user_id)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(profile.into()))
}
```

If the JWT is missing, expired, or invalid, the extractor returns `AppError::Unauthorized` and the handler body never runs. There's no explicit auth-checking code in the handler itself, and that's the separation we want.

### Optional authentication with MaybeAuthUser

Some endpoints behave differently depending on whether the caller is logged in, without actually requiring authentication. Think of a feed endpoint that shows personalized results for logged-in users and generic results for anonymous visitors.

`Option<AuthUser>` looks like the obvious choice here, but it loses an important distinction. `None` could mean "no Authorization header was sent" (the user is anonymous) or "an Authorization header was sent but the token was garbage" (which should be a 401, not a silent fallback to anonymous behavior). Those are very different situations.

A dedicated `MaybeAuthUser` extractor tells them apart:

```rust
pub struct MaybeAuthUser(pub Option<AuthUser>);

impl<S> FromRequestParts<S> for MaybeAuthUser
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        // If there is no Authorization header at all, that is fine.
        let Some(auth_header) = parts.headers.get(header::AUTHORIZATION) else {
            return Ok(Self(None));
        };

        // But if a header IS present, it must be valid.
        // An invalid token is an error, not anonymous access.
        let token = auth_header
            .to_str()
            .ok()
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(AppError::Unauthorized)?;

        let app_state = AppState::from_ref(state);
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&["resysto"]);
        validation.set_audience(&["resysto-api"]);

        let token_data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(
                app_state.config.jwt_secret.expose_secret().as_bytes()
            ),
            &validation,
        )
        .map_err(|_| AppError::Unauthorized)?;

        Ok(Self(Some(AuthUser {
            user_id: token_data.claims.sub,
            email: token_data.claims.email,
            role: token_data.claims.role,
        })))
    }
}
```

It's a small detail, but it prevents a whole category of subtle auth bugs where a client sends a malformed token and silently gets anonymous behavior instead of a clear error.

## Issuing tokens

The other side of the coin is how tokens get created. The login endpoint validates credentials and issues a JWT:

```rust
use std::time::{SystemTime, UNIX_EPOCH};
use jsonwebtoken::{encode, EncodingKey, Header};

async fn login(
    State(state): State<AppState>,
    Json(credentials): Json<LoginDto>,
) -> AppResult<Json<TokenResponse>> {
    let user = state.user_service
        .authenticate(&credentials.email, &credentials.password)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let claims = Claims {
        sub: user.id(),
        email: user.email().as_str().to_string(),
        role: user.role().clone(),
        iss: "resysto".to_string(),
        aud: "resysto-api".to_string(),
        iat: now,
        // Short-lived access tokens limit damage if compromised.
        // For longer access, use a refresh token mechanism.
        exp: now + 15 * 60,
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(
            state.config.jwt_secret.expose_secret().as_bytes()
        ),
    )
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok(Json(TokenResponse { token }))
}
```

There are a few production considerations here that are easy to overlook.

**Token lifetime.** Access tokens live 15 minutes. Short lifetimes limit the window of exposure if a token gets stolen. A client that needs longer access gets a separate refresh token, which the server can revoke.

**Algorithm pinning.** Always specify the expected algorithm explicitly (here, `Algorithm::HS256`) rather than trusting the `alg` header in the token itself. Accepting the token's self-declared algorithm is a well-known attack vector.

**Issuer and audience.** Setting `iss` and `aud` claims, and validating them on decode, prevents tokens issued for one service from being accepted by another. This matters as soon as more than one service shares a secret or uses the same identity provider.

**Role claims and revocation.** Embedding roles in the JWT means that permission changes (revoking admin access, disabling an account) don't take effect until the token expires and a new one is issued. Where immediate revocation matters, we need either very short token lifetimes, a token version check against the database, or a deny-list.

## Password hashing

Passwords are never stored in plain text, and the choice of hashing algorithm matters too. We use Argon2. It won the Password Hashing Competition in 2015 and it's still the best option we have. Don't use SHA-256, MD5, or even bcrypt.

```rust
use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    // Use Argon2id, which is the variant recommended by OWASP.
    // The default() configuration uses Argon2id with reasonable parameters,
    // but for production tune memory cost and iterations based on the
    // hardware and OWASP's current minimum recommendations.
    let hasher = Argon2::default(); // Argon2id with default params
    let hash = hasher
        .hash_password(password.as_bytes())
        .map_err(|e| anyhow::anyhow!("failed to hash password: {}", e))?
        .to_string();
    Ok(hash)
}

pub fn verify_password(password: &str, hash: &str) -> anyhow::Result<bool> {
    let parsed_hash = PasswordHash::new(hash)
        .map_err(|e| anyhow::anyhow!("failed to parse password hash: {}", e))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok())
}
```

Password hashing is deliberately slow. That's the whole point, making brute-force attacks impractical. But in an async context, this means it runs on a blocking thread so it doesn't tie up the async runtime:

```rust
let hash = tokio::task::spawn_blocking(move || hash_password(&password))
    .await
    .context("password hashing task failed")??;
```

## Role-based authorization

Once authentication works, authorization is the natural next step. The simplest approach is checking the user's role inside the handler:

```rust
async fn admin_only_endpoint(
    user: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<AdminData>> {
    if user.role != Role::Admin {
        return Err(AppError::Forbidden);
    }
    // ... admin logic
}
```

That works, but when the same role check repeats across multiple handlers, an extractor bakes the role requirement right in:

```rust
pub struct RequireAdmin(pub AuthUser);

impl<S> FromRequestParts<S> for RequireAdmin
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;

        if user.role != Role::Admin {
            return Err(AppError::Forbidden);
        }

        Ok(Self(user))
    }
}

// Usage: just include it in the handler signature
async fn admin_endpoint(RequireAdmin(user): RequireAdmin) -> AppResult<Json<AdminData>> {
    // If we get here, the user is authenticated AND has the admin role
    // ...
}
```

## Authorization in the database

Authorization doesn't stop at the handler. The app's queries run in the user's SurrealDB record session, never as the system user, so the `PERMISSIONS` on each table decide which records the user can read and write. A check that a handler forgets is still enforced by the database.

## Middleware-based authentication

Instead of putting the extractor on each handler, authentication can also be applied as middleware to a whole group of routes. This is handy for a block of routes that all require authentication, without repeating `AuthUser` in every handler signature.

```rust
let public_routes = Router::new()
    .route("/health", get(health_check))
    .route("/api/v1/auth/login", post(login))
    .route("/api/v1/auth/register", post(register));

let protected_routes = Router::new()
    .nest("/api/v1/users", user_routes())
    .nest("/api/v1/posts", post_routes())
    .route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth_middleware,
    ));

let app = Router::new()
    .merge(public_routes)
    .merge(protected_routes)
    .with_state(state);
```

Extractors are more flexible, because individual handlers can opt in or out and the handler gets direct access to the `AuthUser` value. Middleware is more convenient when entire route groups all need the same authentication requirement. The two combine: middleware for the baseline and extractors for finer-grained checks within that group.

## Ecosystem crates

The ecosystem offers ready-made pieces:

- **axum-login** provides session-based authentication with pluggable backends
- **axum-gate** combines JWT validation with role-based authorization
- **axum-session** handles database-persisted sessions
- **jwt-authorizer** provides JWT validation with OIDC discovery support
- **axum-csrf-sync-pattern** implements CSRF protection for session-based authentication

For bearer tokens, the custom `AuthUser` extractor we built above gives the right balance of simplicity and control. The UI's sessions start from `tower-sessions` and `axum-login`.
