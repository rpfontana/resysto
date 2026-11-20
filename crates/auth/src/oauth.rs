use std::time::Duration;

use axum::extract::rejection::FormRejection;
use axum::extract::{FromRef, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use axum_extra::extract::CookieJar;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use resysto_http::{ApiResponse, AppError, AppResult, ErrorResponse, Path, Query, ValidatedJson};
use resysto_id::{Entity, Id};
use resysto_surrealdb::{Db, Executor, transaction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{RecordId, SurrealValue};
use url::Url;
use utoipa::{IntoParams, ToSchema};
use validator::Validate;

use super::account::AccountId;
use super::agent::PublicUrl;
use super::audit::{self, About, Client, Event};
use super::cimd::{self, ClientApp};
use super::error::AuthError;
use super::http::{MasterName, master, signed_in};
use super::membership::{self, needs_mfa};
use super::mfa::MfaRequiredFor;
use super::session::Session;
use super::token::{Token, key};

const REQUEST_LIFETIME: Duration = Duration::from_secs(10 * 60);
const CODE_LIFETIME: Duration = Duration::from_secs(5 * 60);
pub(crate) const ACCESS_TOKEN_LIFETIME: Duration = Duration::from_secs(60 * 60);
const REFRESH_TOKEN_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const GRANT_LIFETIME: Duration = Duration::from_secs(90 * 24 * 60 * 60);
const CONSENT_PAGE: &str = "/oauth/consent";

#[derive(Debug)]
pub enum OAuthGrant {}

impl Entity for OAuthGrant {
    const TABLE: &'static str = "oauth_grant";
}

/// A member's consent to an app
pub type GrantId = Id<OAuthGrant>;

/// The routes of the authorization server, at the server's root
pub fn oauth_router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    Db: FromRef<S>,
    MasterName: FromRef<S>,
    PublicUrl: FromRef<S>,
{
    Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(server_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(resource_metadata),
        )
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
}

pub(crate) fn mcp_resource(public: &PublicUrl) -> String {
    format!("{}/mcp", public.0)
}

/// Creates `account`'s consent to the app `client`; `code` is the key of the code it answers
pub async fn create_grant(
    master: &impl Executor,
    account: AccountId,
    tenants: Vec<RecordId>,
    client: &str,
    client_name: &str,
    mfa: bool,
    code: Option<String>,
) -> Result<GrantId, resysto_surrealdb::Error> {
    let grant = GrantId::new();
    master
        .query(
            "CREATE $grant SET account = $account, tenants = $tenants, client = $client,
                 client_name = $client_name, mfa = $mfa, code = $code,
                 expires_at = time::now() + $lifetime
             RETURN NONE",
        )
        .bind(("grant", grant))
        .bind(("account", account))
        .bind(("tenants", tenants))
        .bind(("client", client.to_owned()))
        .bind(("client_name", client_name.to_owned()))
        .bind(("mfa", mfa))
        .bind(("code", code))
        .bind(("lifetime", GRANT_LIFETIME))
        .await?
        .check()?;
    Ok(grant)
}

#[derive(SurrealValue)]
struct GrantRow {
    account: AccountId,
    client: String,
    tenants: Vec<RecordId>,
}

pub async fn issue_access_token(
    master: &Surreal<Any>,
    grant: GrantId,
) -> Result<String, AuthError> {
    let token = Token::new();
    let issued: Option<GrantRow> = master
        .query(
            "CREATE type::record('access_token', $key)
                 SET grant = $grant, expires_at = time::now() + $lifetime
                 RETURN NONE;
             SELECT account, client, tenants FROM ONLY $grant;",
        )
        .bind(("key", token.key()))
        .bind(("grant", grant))
        .bind(("lifetime", ACCESS_TOKEN_LIFETIME))
        .await?
        .check()?
        .take(1)?;
    if let Some(issued) = issued {
        let about = About {
            app: Some(issued.client),
            tenants: Some(issued.tenants),
            ..About::of(issued.account).in_session(token.key())
        };
        audit::record(master, Event::AccessTokenIssued, about).await?;
    }
    Ok(token.into_string())
}

// RFC 8414
async fn server_metadata(State(public): State<PublicUrl>) -> Json<Value> {
    let issuer = &*public.0;
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "revocation_endpoint": format!("{issuer}/oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "revocation_endpoint_auth_methods_supported": ["none"],
        "client_id_metadata_document_supported": true,
        "authorization_response_iss_parameter_supported": true,
    }))
}

// RFC 9728
async fn resource_metadata(State(public): State<PublicUrl>) -> Json<Value> {
    Json(json!({
        "resource": mcp_resource(&public),
        "authorization_servers": [&*public.0],
        "bearer_methods_supported": ["header"],
        "resource_name": "resysto",
    }))
}

#[derive(Debug, Deserialize)]
struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    resource: Option<String>,
}

#[derive(Debug)]
enum Stop {
    /// The client or its redirect URI can't be trusted, so the browser stays here
    Here(String),
    /// The browser goes back to the app with the error
    Back(String),
    Server(AppError),
}

impl From<AuthError> for Stop {
    fn from(error: AuthError) -> Self {
        Self::Server(error.into())
    }
}

impl From<AppError> for Stop {
    fn from(error: AppError) -> Self {
        Self::Server(error)
    }
}

async fn authorize(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(public): State<PublicUrl>,
    Query(query): Query<AuthorizeQuery>,
) -> Response {
    let begun = async {
        let master = master(&db, &name).await?;
        let client_id = query
            .client_id
            .as_deref()
            .ok_or_else(|| Stop::Here("client_id is missing".to_owned()))?;
        let app = cimd::fetch(client_id)
            .await
            .map_err(|error| Stop::Here(error.to_string()))?;
        begin(&master, &public, &app, query).await
    }
    .await;
    match begun {
        Ok(consent) => Redirect::to(&consent).into_response(),
        Err(Stop::Here(message)) => (StatusCode::BAD_REQUEST, message).into_response(),
        Err(Stop::Back(redirect)) => Redirect::to(&redirect).into_response(),
        Err(Stop::Server(error)) => error.into_response(),
    }
}

async fn begin(
    master: &Surreal<Any>,
    public: &PublicUrl,
    app: &ClientApp,
    query: AuthorizeQuery,
) -> Result<String, Stop> {
    let redirect_uri = query
        .redirect_uri
        .ok_or_else(|| Stop::Here("redirect_uri is missing".to_owned()))?;
    if !app.redirects_to(&redirect_uri) {
        return Err(Stop::Here(format!(
            "redirect_uri is not one of the redirect URIs of {}",
            app.id
        )));
    }
    let state = query.state;
    let back = |error: &str, description: &str| {
        Stop::Back(error_redirect(
            &redirect_uri,
            public,
            state.as_deref(),
            error,
            description,
        ))
    };
    if query.response_type.as_deref() != Some("code") {
        return Err(back(
            "unsupported_response_type",
            "only the code flow is supported",
        ));
    }
    let challenge = match (query.code_challenge, query.code_challenge_method.as_deref()) {
        (Some(challenge), Some("S256")) if pkce_value(&challenge) => challenge,
        _ => return Err(back("invalid_request", "PKCE with S256 is required")),
    };
    if query.resource != Some(mcp_resource(public)) {
        return Err(back(
            "invalid_target",
            "resource must be the MCP endpoint of this server",
        ));
    }
    let id = Token::new().into_string();
    master
        .query(
            "CREATE type::record('oauth_request', $id)
             SET client = $client, client_name = $client_name, redirect_uri = $redirect_uri,
                 code_challenge = $challenge, state = $state,
                 expires_at = time::now() + $lifetime
             RETURN NONE",
        )
        .bind(("id", id.clone()))
        .bind(("client", app.id.clone()))
        .bind(("client_name", app.name.clone()))
        .bind(("redirect_uri", redirect_uri.clone()))
        .bind(("challenge", challenge))
        .bind(("state", state.clone()))
        .bind(("lifetime", REQUEST_LIFETIME))
        .await
        .map_err(AuthError::from)?
        .check()
        .map_err(AuthError::from)?;
    Ok(format!("{}{CONSENT_PAGE}?request={id}", public.0))
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ConsentRequest {
    client_name: String,
    client_id: String,
    redirect_host: String,
    loopback: bool,
    tenants: Vec<ConsentTenant>,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ConsentTenant {
    slug: String,
    roles: Vec<String>,
    second_factor_needed: bool,
}

#[derive(SurrealValue)]
struct Pending {
    client: String,
    client_name: String,
    redirect_uri: String,
    state: Option<String>,
}

async fn pending(master: &Surreal<Any>, id: &str) -> Result<Option<Pending>, AuthError> {
    let found: Option<Pending> = master
        .query(
            "SELECT client, client_name, redirect_uri, state
             FROM type::record('oauth_request', $id)
             WHERE expires_at > time::now() AND code = NONE",
        )
        .bind(("id", id.to_owned()))
        .await?
        .check()?
        .take(0)?;
    Ok(found)
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Path)]
pub(crate) struct RequestPath {
    id: String,
}

/// Get an app's authorization request
///
/// What the consent page shows the member
#[utoipa::path(
    get,
    operation_id = "oauth_request_get",
    path = "/oauth/requests/{id}",
    tag = "oauth",
    params(RequestPath),
    responses(
        (status = 200, body = ApiResponse<ConsentRequest>),
        (status = 404, body = ErrorResponse, description = "unknown, expired or already answered"),
    ),
)]
pub(crate) async fn consent_request(
    State(db): State<Db>,
    State(name): State<MasterName>,
    jar: CookieJar,
    Path(RequestPath { id }): Path<RequestPath>,
) -> AppResult<Json<ApiResponse<ConsentRequest>>> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    let request = pending(&master, &id).await?.ok_or(AppError::NotFound)?;
    let tenants = membership::memberships(&master, session.account)
        .await?
        .into_iter()
        .map(|member| ConsentTenant {
            second_factor_needed: member.mfa_required && !session.mfa,
            slug: member.slug,
            roles: member.roles,
        })
        .collect();
    let redirect = Url::parse(&request.redirect_uri).ok();
    Ok(Json(ApiResponse {
        data: ConsentRequest {
            client_name: request.client_name,
            client_id: request.client,
            redirect_host: redirect
                .as_ref()
                .and_then(Url::host_str)
                .unwrap_or_default()
                .to_owned(),
            loopback: redirect.as_ref().is_some_and(|url| url.scheme() == "http"),
            tenants,
        },
    }))
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub(crate) struct Approval {
    #[validate(length(min = 1, message = "choose at least one tenant"))]
    tenants: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Next {
    redirect: String,
}

/// Allow an app
///
/// In the tenants chosen; the answer says where the browser goes back to the app
#[utoipa::path(
    post,
    operation_id = "oauth_request_approve",
    path = "/oauth/requests/{id}/approve",
    tag = "oauth",
    params(RequestPath),
    request_body = Approval,
    responses(
        (status = 200, body = ApiResponse<Next>),
        (status = 400, body = ErrorResponse, description = "no tenant chosen"),
        (status = 403, body = ErrorResponse, description = "`mfa_required`: a tenant chosen asks for a second factor"),
        (status = 404, body = ErrorResponse, description = "unknown, expired or already answered"),
        (status = 422, body = ErrorResponse, description = "a tenant chosen is not the account's"),
    ),
)]
pub(crate) async fn approve(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(public): State<PublicUrl>,
    jar: CookieJar,
    client: Client,
    Path(RequestPath { id }): Path<RequestPath>,
    ValidatedJson(approval): ValidatedJson<Approval>,
) -> AppResult<Json<ApiResponse<Next>>> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    let redirect = allow(&master, &public, &id, session, &approval.tenants, &client).await?;
    Ok(Json(ApiResponse {
        data: Next { redirect },
    }))
}

#[derive(SurrealValue)]
struct Chosen {
    tenant: RecordId,
    slug: String,
    roles: Vec<String>,
    mfa_required_for: MfaRequiredFor,
}

async fn chosen_tenants(
    master: &Surreal<Any>,
    session: Session,
    slugs: &[String],
) -> AppResult<Vec<RecordId>> {
    let chosen: Vec<Chosen> = master
        .query(
            "SELECT out AS tenant, out.slug AS slug, roles,
                 out.mfa_required_for AS mfa_required_for
             FROM member_of
             WHERE in = $account AND out.state = 'ready' AND out.slug IN $slugs",
        )
        .bind(("account", session.account))
        .bind(("slugs", slugs.to_vec()))
        .await
        .map_err(AuthError::from)?
        .check()
        .map_err(AuthError::from)?
        .take(0)
        .map_err(AuthError::from)?;
    if let Some(unknown) = slugs
        .iter()
        .find(|slug| !chosen.iter().any(|found| &found.slug == *slug))
    {
        return Err(AppError::Unprocessable(format!(
            "you are not a member of the tenant {unknown}"
        )));
    }
    if !session.mfa
        && chosen
            .iter()
            .any(|found| needs_mfa(found.mfa_required_for, &found.roles))
    {
        return Err(AppError::MfaRequired);
    }
    Ok(chosen.into_iter().map(|found| found.tenant).collect())
}

async fn allow(
    master: &Surreal<Any>,
    public: &PublicUrl,
    id: &str,
    session: Session,
    slugs: &[String],
    client: &Client,
) -> AppResult<String> {
    let request = pending(master, id).await?.ok_or(AppError::NotFound)?;
    let tenants = chosen_tenants(master, session, slugs).await?;
    let code = Token::new();
    let code_key = code.key();
    let allowed = transaction(master, |tx| {
        let (id, code_key) = (id.to_owned(), code_key.clone());
        let (tenants, app, app_name) = (
            tenants.clone(),
            request.client.clone(),
            request.client_name.clone(),
        );
        let client = client.clone();
        async move {
            let grant = create_grant(
                &tx,
                session.account,
                tenants.clone(),
                &app,
                &app_name,
                session.mfa,
                Some(code_key.clone()),
            )
            .await?;
            let taken: Vec<RecordId> = tx
                .query(
                    "UPDATE type::record('oauth_request', $id)
                     SET code = $code, grant = $grant, expires_at = time::now() + $wait
                     WHERE code = NONE AND expires_at > time::now()
                     RETURN VALUE id",
                )
                .bind(("id", id))
                .bind(("code", code_key.clone()))
                .bind(("grant", grant))
                .bind(("wait", CODE_LIFETIME))
                .await?
                .check()?
                .take(0)?;
            // another consent took the request first: its grant is the one that counts
            if taken.is_empty() {
                tx.query("DELETE $grant")
                    .bind(("grant", grant))
                    .await?
                    .check()?;
                return Ok(false);
            }
            let about = About {
                app: Some(app),
                tenants: Some(tenants),
                ..About::of(session.account).with_client(&client)
            };
            audit::record(&tx, Event::AppAllowed, about).await?;
            Ok(true)
        }
    })
    .await
    .map_err(AuthError::from)?;
    if !allowed {
        return Err(AppError::NotFound);
    }
    let mut redirect = answer(&request.redirect_uri, public, request.state.as_deref())?;
    redirect
        .query_pairs_mut()
        .append_pair("code", &code.into_string());
    Ok(redirect.into())
}

/// Refuse an app
#[utoipa::path(
    post,
    operation_id = "oauth_request_deny",
    path = "/oauth/requests/{id}/deny",
    tag = "oauth",
    params(RequestPath),
    responses(
        (status = 200, body = ApiResponse<Next>),
        (status = 404, body = ErrorResponse, description = "unknown, expired or already answered"),
    ),
)]
pub(crate) async fn deny(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(public): State<PublicUrl>,
    jar: CookieJar,
    client: Client,
    Path(RequestPath { id }): Path<RequestPath>,
) -> AppResult<Json<ApiResponse<Next>>> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    let request = pending(&master, &id).await?.ok_or(AppError::NotFound)?;
    master
        .query("DELETE type::record('oauth_request', $id)")
        .bind(("id", id))
        .await
        .map_err(AuthError::from)?
        .check()
        .map_err(AuthError::from)?;
    let about = About {
        app: Some(request.client),
        ..About::of(session.account).with_client(&client)
    };
    audit::record(&master, Event::AppRefused, about)
        .await
        .map_err(AuthError::from)?;
    let redirect = error_redirect(
        &request.redirect_uri,
        &public,
        request.state.as_deref(),
        "access_denied",
        "the member refused the app",
    );
    Ok(Json(ApiResponse {
        data: Next { redirect },
    }))
}

#[derive(Debug, Deserialize)]
struct TokenForm {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    resource: Option<String>,
}

#[derive(Debug, Serialize)]
struct Tokens {
    access_token: String,
    token_type: &'static str,
    expires_in: u64,
    refresh_token: String,
}

#[derive(Debug)]
struct OAuthError {
    error: &'static str,
    description: String,
}

impl OAuthError {
    fn new(error: &'static str, description: impl Into<String>) -> Self {
        Self {
            error,
            description: description.into(),
        }
    }
}

impl From<AuthError> for OAuthError {
    fn from(error: AuthError) -> Self {
        tracing::error!(%error, "token endpoint failed");
        Self::new("server_error", "the server failed, try again later")
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let status = if self.error == "server_error" {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            StatusCode::BAD_REQUEST
        };
        let body = json!({ "error": self.error, "error_description": self.description });
        no_store((status, Json(body)).into_response())
    }
}

async fn token(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(public): State<PublicUrl>,
    form: Result<Form<TokenForm>, FormRejection>,
) -> Response {
    let issued = async {
        let Form(form) =
            form.map_err(|error| OAuthError::new("invalid_request", error.body_text()))?;
        let master = db.system_session(&name.0).await.map_err(AuthError::from)?;
        match form.grant_type.as_deref() {
            Some("authorization_code") => exchange_code(&master, &public, form).await,
            Some("refresh_token") => refresh(&master, form).await,
            _ => Err(OAuthError::new(
                "unsupported_grant_type",
                "grant_type must be authorization_code or refresh_token",
            )),
        }
    }
    .await;
    match issued {
        Ok(tokens) => no_store(Json(tokens).into_response()),
        Err(error) => error.into_response(),
    }
}

#[derive(SurrealValue)]
struct Redeemed {
    client: String,
    redirect_uri: String,
    code_challenge: String,
    grant: Option<GrantId>,
    live: bool,
}

// a code used twice leaked: what the first use got is revoked
async fn exchange_code(
    master: &Surreal<Any>,
    public: &PublicUrl,
    form: TokenForm,
) -> Result<Tokens, OAuthError> {
    let missing = |field: &str| OAuthError::new("invalid_request", format!("{field} is missing"));
    let code = form.code.ok_or_else(|| missing("code"))?;
    let client_id = form.client_id.ok_or_else(|| missing("client_id"))?;
    let redirect_uri = form.redirect_uri.ok_or_else(|| missing("redirect_uri"))?;
    let verifier = form.code_verifier.ok_or_else(|| missing("code_verifier"))?;
    let code_key = key(&code);
    let redeemed: Option<Redeemed> = master
        .query(
            "DELETE oauth_request WHERE code = $code
             RETURN VALUE {
                 client: $before.client, redirect_uri: $before.redirect_uri,
                 code_challenge: $before.code_challenge, grant: $before.grant,
                 live: $before.expires_at > time::now()
             }",
        )
        .bind(("code", code_key.clone()))
        .await
        .map_err(AuthError::from)?
        .check()
        .map_err(AuthError::from)?
        .take(0)
        .map_err(AuthError::from)?;
    let invalid = |description: &str| OAuthError::new("invalid_grant", description);
    let Some(redeemed) = redeemed else {
        revoke_code_grant(master, &code_key).await?;
        return Err(invalid("the code is unknown, used or expired"));
    };
    let grant = redeemed
        .grant
        .ok_or_else(|| invalid("the code was never issued"))?;
    if !redeemed.live {
        return Err(invalid("the code expired"));
    }
    if redeemed.client != client_id || redeemed.redirect_uri != redirect_uri {
        return Err(invalid(
            "client_id or redirect_uri differ from the authorization's",
        ));
    }
    if !pkce_value(&verifier) || challenge_of(&verifier) != redeemed.code_challenge {
        return Err(invalid("code_verifier does not match the code_challenge"));
    }
    if form
        .resource
        .is_some_and(|asked| asked != mcp_resource(public))
    {
        return Err(OAuthError::new(
            "invalid_target",
            "resource differs from the authorization's",
        ));
    }
    Ok(issue_tokens(master, grant).await?)
}

async fn revoke_code_grant(master: &Surreal<Any>, code_key: &str) -> Result<(), AuthError> {
    let revoked: Vec<GrantRow> = master
        .query(
            "DELETE oauth_grant WHERE code = $code
             RETURN VALUE { account: $before.account, client: $before.client,
                 tenants: $before.tenants }",
        )
        .bind(("code", code_key.to_owned()))
        .await?
        .check()?
        .take(0)?;
    for grant in revoked {
        tracing::warn!(account = %grant.account, "authorization code used twice: grant revoked");
        record_grant(master, Event::RefreshTokenReused, grant).await?;
    }
    Ok(())
}

#[derive(SurrealValue)]
struct Refreshed {
    grant: GrantId,
    client: String,
    live: bool,
}

// a refresh token used twice leaked: its grant is revoked; memberships are checked on each call
async fn refresh(master: &Surreal<Any>, form: TokenForm) -> Result<Tokens, OAuthError> {
    let missing = |field: &str| OAuthError::new("invalid_request", format!("{field} is missing"));
    let token = form.refresh_token.ok_or_else(|| missing("refresh_token"))?;
    let client_id = form.client_id.ok_or_else(|| missing("client_id"))?;
    let invalid = |description: &str| OAuthError::new("invalid_grant", description);
    let token_key = key(&token);
    let used: Option<Refreshed> = master
        .query(
            "UPDATE type::record('refresh_token', $key) SET used_at = time::now()
             WHERE used_at = NONE AND expires_at > time::now()
             RETURN VALUE {
                 grant: grant, client: grant.client,
                 live: grant.expires_at > time::now() AND grant.account.active = true
             }",
        )
        .bind(("key", token_key.clone()))
        .await
        .map_err(AuthError::from)?
        .check()
        .map_err(AuthError::from)?
        .take(0)
        .map_err(AuthError::from)?;
    let Some(used) = used else {
        revoke_reused(master, &token_key).await?;
        return Err(invalid("the refresh token is unknown, used or expired"));
    };
    if used.client != client_id {
        return Err(invalid("client_id differs from the token's"));
    }
    if !used.live {
        delete_grant(master, used.grant).await?;
        return Err(invalid("the authorization ended: sign in again"));
    }
    Ok(issue_tokens(master, used.grant).await?)
}

async fn revoke_reused(master: &Surreal<Any>, token_key: &str) -> Result<(), AuthError> {
    let reused: Option<GrantId> = master
        .query("SELECT VALUE grant FROM type::record('refresh_token', $key) WHERE used_at != NONE")
        .bind(("key", token_key.to_owned()))
        .await?
        .check()?
        .take(0)?;
    if let Some(grant) = reused {
        tracing::warn!(%grant, "refresh token used twice: grant revoked");
        if let Some(row) = delete_grant(master, grant).await? {
            record_grant(master, Event::RefreshTokenReused, row).await?;
        }
    }
    Ok(())
}

async fn issue_tokens(master: &Surreal<Any>, grant: GrantId) -> Result<Tokens, AuthError> {
    let access_token = issue_access_token(master, grant).await?;
    let refresh = Token::new();
    master
        .query(
            "LET $end = time::now() + $lifetime;
             CREATE type::record('refresh_token', $key)
                 SET grant = $grant,
                     expires_at = IF $end < $grant.expires_at THEN $end ELSE $grant.expires_at END
                 RETURN NONE;",
        )
        .bind(("key", refresh.key()))
        .bind(("grant", grant))
        .bind(("lifetime", REFRESH_TOKEN_LIFETIME))
        .await?
        .check()?;
    Ok(Tokens {
        access_token,
        token_type: "Bearer",
        expires_in: ACCESS_TOKEN_LIFETIME.as_secs(),
        refresh_token: refresh.into_string(),
    })
}

#[derive(Debug, Deserialize)]
struct RevokeForm {
    token: Option<String>,
}

// RFC 7009: the answer is the same whether the token existed or not
async fn revoke(
    State(db): State<Db>,
    State(name): State<MasterName>,
    form: Result<Form<RevokeForm>, FormRejection>,
) -> Response {
    let Ok(Form(RevokeForm { token: Some(token) })) = form else {
        return OAuthError::new("invalid_request", "token is missing").into_response();
    };
    let revoked = async {
        let master = db.system_session(&name.0).await.map_err(AuthError::from)?;
        revoke_token(&master, &token).await
    }
    .await;
    match revoked {
        Ok(()) => StatusCode::OK.into_response(),
        Err(error) => OAuthError::from(error).into_response(),
    }
}

async fn revoke_token(master: &Surreal<Any>, token: &str) -> Result<(), AuthError> {
    let token_key = key(token);
    let grant: Option<GrantId> = master
        .query(
            "DELETE type::record('access_token', $key);
             SELECT VALUE grant FROM type::record('refresh_token', $key);",
        )
        .bind(("key", token_key))
        .await?
        .check()?
        .take(1)?;
    if let Some(grant) = grant
        && let Some(row) = delete_grant(master, grant).await?
    {
        record_grant(master, Event::GrantRevoked, row).await?;
    }
    Ok(())
}

async fn delete_grant(
    master: &Surreal<Any>,
    grant: GrantId,
) -> Result<Option<GrantRow>, AuthError> {
    let deleted: Option<GrantRow> = master
        .query(
            "DELETE ONLY $grant RETURN VALUE {
                 account: $before.account, client: $before.client, tenants: $before.tenants
             }",
        )
        .bind(("grant", grant))
        .await?
        .check()?
        .take(0)?;
    Ok(deleted)
}

async fn record_grant(
    master: &Surreal<Any>,
    event: Event,
    grant: GrantRow,
) -> Result<(), AuthError> {
    let about = About {
        app: Some(grant.client),
        tenants: Some(grant.tenants),
        ..About::of(grant.account)
    };
    audit::record(master, event, about).await?;
    Ok(())
}

// RFC 7636, for a verifier and an S256 challenge alike
fn pkce_value(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

// RFC 9207: every answer carries the issuer
fn answer(redirect_uri: &str, public: &PublicUrl, state: Option<&str>) -> AppResult<Url> {
    let mut url = Url::parse(redirect_uri).map_err(|error| AppError::Internal(error.into()))?;
    {
        let mut pairs = url.query_pairs_mut();
        if let Some(state) = state {
            pairs.append_pair("state", state);
        }
        pairs.append_pair("iss", &public.0);
    }
    Ok(url)
}

fn error_redirect(
    redirect_uri: &str,
    public: &PublicUrl,
    state: Option<&str>,
    error: &str,
    description: &str,
) -> String {
    match answer(redirect_uri, public, state) {
        Ok(mut url) => {
            url.query_pairs_mut()
                .append_pair("error", error)
                .append_pair("error_description", description);
            url.into()
        }
        Err(_) => redirect_uri.to_owned(),
    }
}

fn no_store(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::create_account;
    use crate::membership::add_member;
    use crate::testing::{PASSWORD, ada, new_master, new_tenant, passwords, server};

    const APP: &str = "https://claude.ai/oauth/claude-code-client-metadata";
    const REDIRECT: &str = "http://localhost:4242/callback";
    const RESOURCE: &str = "https://resysto.example/mcp";

    struct Setup {
        master: Surreal<Any>,
        public: PublicUrl,
        app: ClientApp,
        session: Session,
    }

    // Ada reads in acme and in beta, which asks everyone for a second factor; gamma is not hers
    async fn setup() -> Setup {
        let server = server().await;
        let (_, master) = new_master(&server).await;
        new_tenant(&server, &master, "acme").await;
        let account = create_account(&master, &passwords(), ada(), PASSWORD.into())
            .await
            .unwrap();
        add_member(
            &server,
            &master,
            account,
            "acme",
            &["tenant/reader".to_owned()],
        )
        .await
        .unwrap();
        master
            .query(
                "LET $beta = CREATE ONLY tenant SET slug = 'beta', db_name = 't_beta',
                     state = 'ready', mfa_required_for = 'everyone';
                 CREATE tenant SET slug = 'gamma', db_name = 't_gamma', state = 'ready';
                 RELATE $account->member_of->($beta.id) SET roles = ['tenant/reader'];",
            )
            .bind(("account", account))
            .await
            .unwrap()
            .check()
            .unwrap();
        let document = json!({
            "client_id": APP,
            "client_name": "Claude Code",
            "redirect_uris": ["http://localhost/callback"],
        });
        Setup {
            master,
            public: PublicUrl("https://resysto.example".into()),
            app: cimd::parse(document.to_string().as_bytes(), APP).unwrap(),
            session: Session {
                account,
                mfa: false,
            },
        }
    }

    fn verifier() -> String {
        "v".repeat(64)
    }

    fn query() -> AuthorizeQuery {
        AuthorizeQuery {
            response_type: Some("code".into()),
            client_id: Some(APP.into()),
            redirect_uri: Some(REDIRECT.into()),
            state: Some("s1".into()),
            code_challenge: Some(challenge_of(&verifier())),
            code_challenge_method: Some("S256".into()),
            resource: Some(RESOURCE.into()),
        }
    }

    fn query_param(url: &str, name: &str) -> Option<String> {
        Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }

    async fn requested(setup: &Setup) -> String {
        let consent = begin(&setup.master, &setup.public, &setup.app, query())
            .await
            .unwrap();
        query_param(&consent, "request").unwrap()
    }

    async fn consented(setup: &Setup) -> String {
        let id = requested(setup).await;
        let acme = ["acme".to_owned()];
        let back = allow(
            &setup.master,
            &setup.public,
            &id,
            setup.session,
            &acme,
            &Client::default(),
        )
        .await
        .unwrap();
        assert_eq!(query_param(&back, "state").as_deref(), Some("s1"));
        assert_eq!(
            query_param(&back, "iss").as_deref(),
            Some("https://resysto.example")
        );
        query_param(&back, "code").unwrap()
    }

    fn code_form(code: &str, verifier: &str) -> TokenForm {
        TokenForm {
            grant_type: Some("authorization_code".into()),
            code: Some(code.into()),
            redirect_uri: Some(REDIRECT.into()),
            client_id: Some(APP.into()),
            code_verifier: Some(verifier.into()),
            refresh_token: None,
            resource: Some(RESOURCE.into()),
        }
    }

    fn refresh_form(token: &str) -> TokenForm {
        TokenForm {
            grant_type: Some("refresh_token".into()),
            code: None,
            redirect_uri: None,
            client_id: Some(APP.into()),
            code_verifier: None,
            refresh_token: Some(token.into()),
            resource: None,
        }
    }

    async fn live(master: &Surreal<Any>, access_token: &str) -> bool {
        let found: Option<i64> = master
            .query("RETURN count(SELECT id FROM type::record('access_token', $key))")
            .bind(("key", key(access_token)))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        found == Some(1)
    }

    fn error_of<T: std::fmt::Debug>(result: Result<T, OAuthError>) -> &'static str {
        match result {
            Err(error) => error.error,
            Ok(other) => panic!("not refused: {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_app_gets_tokens_once_through_a_member_s_consent_and_its_pkce_verifier() {
        let setup = setup().await;
        let stops = [
            (
                AuthorizeQuery {
                    redirect_uri: Some("https://evil.example/callback".into()),
                    ..query()
                },
                "",
            ),
            (
                AuthorizeQuery {
                    code_challenge_method: Some("plain".into()),
                    ..query()
                },
                "invalid_request",
            ),
            (
                AuthorizeQuery {
                    resource: Some("https://resysto.example/t/acme/mcp".into()),
                    ..query()
                },
                "invalid_target",
            ),
        ];
        for (asked, expected) in stops {
            match begin(&setup.master, &setup.public, &setup.app, asked).await {
                Err(Stop::Here(_)) => assert_eq!(expected, ""),
                Err(Stop::Back(redirect)) => {
                    assert!(redirect.starts_with(REDIRECT), "{redirect}");
                    assert_eq!(query_param(&redirect, "error").as_deref(), Some(expected));
                    assert_eq!(query_param(&redirect, "state").as_deref(), Some("s1"));
                }
                other => panic!("{expected}: {other:?}"),
            }
        }

        let guessed = consented(&setup).await;
        let wrong = exchange_code(
            &setup.master,
            &setup.public,
            code_form(&guessed, &"w".repeat(64)),
        );
        assert_eq!(error_of(wrong.await), "invalid_grant");

        let code = consented(&setup).await;
        let tokens = exchange_code(&setup.master, &setup.public, code_form(&code, &verifier()))
            .await
            .unwrap();
        assert!(live(&setup.master, &tokens.access_token).await);
        let again = exchange_code(&setup.master, &setup.public, code_form(&code, &verifier()));
        assert_eq!(error_of(again.await), "invalid_grant");
        assert!(
            !live(&setup.master, &tokens.access_token).await,
            "a code used twice left its tokens working"
        );
    }

    #[tokio::test]
    async fn a_member_lets_an_app_only_into_their_tenants_with_the_factor_each_asks_for() {
        let setup = setup().await;
        let id = requested(&setup).await;
        let consent = async |slugs: &[&str]| {
            let slugs: Vec<String> = slugs.iter().map(|&slug| slug.to_owned()).collect();
            let client = Client::default();
            allow(
                &setup.master,
                &setup.public,
                &id,
                setup.session,
                &slugs,
                &client,
            )
            .await
        };

        let foreign = consent(&["acme", "gamma"]).await;
        assert!(
            matches!(&foreign, Err(AppError::Unprocessable(message)) if message.contains("gamma")),
            "{foreign:?}"
        );
        let unproved = consent(&["acme", "beta"]).await;
        assert!(
            matches!(unproved, Err(AppError::MfaRequired)),
            "{unproved:?}"
        );
        consent(&["acme"]).await.unwrap();

        let mut found = setup
            .master
            .query(
                "SELECT VALUE tenants.slug FROM oauth_grant;
                 SELECT VALUE tenants.slug FROM audit_event WHERE event = 'app_allowed';",
            )
            .await
            .unwrap();
        let granted: Vec<Vec<String>> = found.take(0).unwrap();
        let recorded: Vec<Vec<String>> = found.take(1).unwrap();
        assert_eq!(granted, [["acme"]]);
        assert_eq!(recorded, granted);
    }

    #[tokio::test]
    async fn a_refresh_token_works_once_and_a_second_use_or_a_revocation_ends_the_grant() {
        let setup = setup().await;
        let code = consented(&setup).await;
        let first = exchange_code(&setup.master, &setup.public, code_form(&code, &verifier()))
            .await
            .unwrap();

        let second = refresh(&setup.master, refresh_form(&first.refresh_token))
            .await
            .unwrap();
        assert!(live(&setup.master, &second.access_token).await);
        let reused = refresh(&setup.master, refresh_form(&first.refresh_token)).await;
        assert_eq!(error_of(reused), "invalid_grant");
        assert!(
            !live(&setup.master, &second.access_token).await,
            "a reused refresh token left its grant working"
        );

        let code = consented(&setup).await;
        let third = exchange_code(&setup.master, &setup.public, code_form(&code, &verifier()))
            .await
            .unwrap();
        revoke_token(&setup.master, &third.refresh_token)
            .await
            .unwrap();
        assert!(!live(&setup.master, &third.access_token).await);
        let events: Vec<String> = setup
            .master
            .query("SELECT VALUE event FROM audit_event WHERE client = $app ORDER BY at")
            .bind(("app", APP))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let expected = [
            "app_allowed",
            "access_token_issued",
            "access_token_issued",
            "refresh_token_reused",
            "app_allowed",
            "access_token_issued",
            "grant_revoked",
        ];
        assert_eq!(events, expected);
    }
}
