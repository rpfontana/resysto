use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{FromRef, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use axum_client_ip::{ClientIp, Rejection};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use resysto_http::{ApiResponse, AppError, AppResult, Path, ValidatedJson};
use resysto_model::{Email, OrganizationId};
use resysto_surrealdb::{Actor, Db, Via};
use serde::{Deserialize, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use validator::Validate;

use super::account::{self, AccountRecord, Profile};
use super::agent::PublicUrl;
use super::audit::{self, Client};
use super::error::AuthError;
use super::login::{self, Login};
use super::membership::{self, MemberOf};
use super::mfa;
use super::oauth;
use super::password::{PasswordError, Passwords};
use super::policy::{Member, Policies, Verb};
use super::session::{self, Session, SessionRecord};
use super::token::key;
use super::totp::{Cipher, TotpSetup};

const SESSION_COOKIE: &str = "__Host-session";
/// Holds a login whose password was right while it waits for its second factor
const LOGIN_COOKIE: &str = "__Host-login";
/// The header the server's request id layer sets on every request
pub(crate) const REQUEST_ID: &str = "x-request-id";
/// Enough to tell browsers and devices apart in the list of sessions and in the audit
const USER_AGENT_MAX: usize = 200;

/// The name of the master database: `__master` when serving, a database of their own in tests
#[derive(Debug, Clone)]
pub struct MasterName(pub Arc<str>);

/// A member the policies let through to a tenant's routes, put in the request extensions
#[derive(Debug, Clone)]
pub struct TenantMember {
    /// The tenant's database
    pub database: String,
    /// Who the request writes as
    pub actor: Actor,
    /// `None` reaches the whole tenant; the organizations listed still bring their branches
    pub organizations: Option<Vec<OrganizationId>>,
    /// Whether the request changes data
    pub write: bool,
}

/// The `{slug}` of a tenant's routes
#[derive(Debug, Deserialize)]
pub struct TenantPath {
    pub(crate) slug: String,
}

/// The routes of `/api/v1/auth`
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    Db: FromRef<S>,
    MasterName: FromRef<S>,
    Passwords: FromRef<S>,
    Cipher: FromRef<S>,
    PublicUrl: FromRef<S>,
{
    Router::new()
        .route("/login", post(login))
        .route("/login/totp", post(login_with_totp))
        .route("/login/recovery-code", post(login_with_recovery_code))
        .route("/logout", post(logout))
        .route("/me", get(me))
        .route("/password", post(change_password))
        .route("/sessions", get(sessions).delete(end_other_sessions))
        .route("/sessions/{id}", delete(end_session))
        .route("/totp/setup", post(set_up_totp))
        .route("/totp/confirm", post(confirm_totp))
        .route("/totp/disable", post(disable_totp))
        .route("/oauth/requests/{id}", get(oauth::consent_request))
        .route("/oauth/requests/{id}/approve", post(oauth::approve))
        .route("/oauth/requests/{id}/deny", post(oauth::deny))
}

/// Lets a request through to a tenant's routes only with a live session of one of its members,
/// with a second factor if the tenant requires one and with roles that allow what the request
/// does to its resource; the member goes in the request extensions as a [`TenantMember`]
#[expect(
    clippy::too_many_arguments,
    reason = "an axum middleware takes its extractors as arguments"
)]
pub async fn require_member(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(policies): State<Arc<Policies>>,
    Path(TenantPath { slug }): Path<TenantPath>,
    jar: CookieJar,
    client: Client,
    mut request: Request,
    next: Next,
) -> AppResult<Response> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    let account = session.account;
    // a tenant the account is not a member of reads as one that does not exist
    let membership = membership::membership(&master, account, &slug)
        .await?
        .ok_or(AppError::NotFound)?;
    if membership.needs_mfa() && !session.mfa {
        tracing::info!(account = %account, "request refused: the tenant requires a second factor");
        return Err(AppError::MfaRequired);
    }
    let actor = Actor {
        member: account.cast::<resysto_model::Member>().into(),
        via: Via::Ui,
        client: None,
        session: Some(key(token(&jar)?)),
        request: request
            .headers()
            .get(REQUEST_ID)
            .and_then(|id| id.to_str().ok())
            .map(ToOwned::to_owned),
        ip: client.ip,
        user_agent: client.user_agent,
    };
    let verb = Verb::of(request.method());
    let member = Member {
        account,
        roles: membership.roles,
        whole_tenant: membership.organizations.is_none(),
        tenant: membership.database.clone(),
    };
    let resource = resource(request.uri().path());
    if !policies.allows(&member, verb, resource) {
        tracing::info!(account = %account, ?verb, resource, "request refused by the policies");
        // the answer stays a refusal even when its record fails
        let recorded =
            audit::record_refusal(&db, &membership.database, &actor, verb, resource).await;
        if let Err(error) = recorded {
            tracing::error!(%error, "refusal not recorded");
        }
        return Err(AppError::Forbidden);
    }
    request.extensions_mut().insert(TenantMember {
        database: membership.database,
        actor,
        organizations: membership.organizations,
        write: verb != Verb::Read,
    });
    Ok(next.run(request).await)
}

// the router this middleware wraps sees the path below `/t/{slug}/api/v1`, so the resource is
// its first segment
fn resource(path: &str) -> &str {
    path.trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default()
}

#[derive(Deserialize, Validate)]
struct LoginBody {
    email: Email,
    password: String,
}

#[derive(Deserialize, Validate)]
struct CodeBody {
    code: String,
}

#[derive(Deserialize, Validate)]
struct PasswordBody {
    password: String,
}

#[derive(Deserialize, Validate)]
struct PasswordChange {
    current_password: String,
    new_password: String,
}

/// What a login answers: signed in, or waiting for a code
#[derive(Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
enum LoginStep {
    SignedIn { account: AccountRecord },
    SecondFactor,
}

#[derive(Serialize)]
struct Me {
    #[serde(flatten)]
    account: Profile,
    memberships: Vec<MemberOf>,
}

#[derive(Serialize)]
struct RecoveryCodes {
    recovery_codes: Vec<String>,
}

async fn login(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(passwords): State<Passwords>,
    jar: CookieJar,
    client: Client,
    ValidatedJson(body): ValidatedJson<LoginBody>,
) -> AppResult<(CookieJar, Json<ApiResponse<LoginStep>>)> {
    let master = master(&db, &name).await?;
    if let Some(previous) = jar.get(SESSION_COOKIE) {
        session::logout(&master, previous.value(), &client).await?;
    }
    let login = login::login(&master, &passwords, body.email, body.password, &client).await?;
    let (jar, data) = match login {
        Login::SignedIn(token, account) => (
            jar.add(session_cookie(token.into_string())),
            LoginStep::SignedIn { account },
        ),
        Login::SecondFactor(challenge) => (
            jar.remove(session_cookie(String::new()))
                .add(login_cookie(challenge.into_string())),
            LoginStep::SecondFactor,
        ),
    };
    Ok((jar, Json(ApiResponse { data })))
}

async fn login_with_totp(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(cipher): State<Cipher>,
    jar: CookieJar,
    client: Client,
    ValidatedJson(body): ValidatedJson<CodeBody>,
) -> AppResult<(CookieJar, Json<ApiResponse<LoginStep>>)> {
    let master = master(&db, &name).await?;
    let challenge = challenge(&jar)?;
    let (token, account) =
        login::login_with_totp(&master, &cipher, challenge, &body.code, &client).await?;
    Ok(signed_in_with_two_factors(
        jar,
        token.into_string(),
        account,
    ))
}

async fn login_with_recovery_code(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(passwords): State<Passwords>,
    jar: CookieJar,
    client: Client,
    ValidatedJson(body): ValidatedJson<CodeBody>,
) -> AppResult<(CookieJar, Json<ApiResponse<LoginStep>>)> {
    let master = master(&db, &name).await?;
    let challenge = challenge(&jar)?;
    let (token, account) =
        login::login_with_recovery_code(&master, &passwords, challenge, &body.code, &client)
            .await?;
    Ok(signed_in_with_two_factors(
        jar,
        token.into_string(),
        account,
    ))
}

fn signed_in_with_two_factors(
    jar: CookieJar,
    token: String,
    account: AccountRecord,
) -> (CookieJar, Json<ApiResponse<LoginStep>>) {
    let jar = jar
        .remove(login_cookie(String::new()))
        .add(session_cookie(token));
    let data = LoginStep::SignedIn { account };
    (jar, Json(ApiResponse { data }))
}

async fn logout(
    State(db): State<Db>,
    State(name): State<MasterName>,
    jar: CookieJar,
    client: Client,
) -> AppResult<(CookieJar, StatusCode)> {
    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        session::logout(&master(&db, &name).await?, cookie.value(), &client).await?;
    }
    Ok((
        jar.remove(session_cookie(String::new())),
        StatusCode::NO_CONTENT,
    ))
}

async fn me(
    State(db): State<Db>,
    State(name): State<MasterName>,
    jar: CookieJar,
) -> AppResult<Json<ApiResponse<Me>>> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    let profile = account::profile(&master, session.account)
        .await?
        .ok_or(AppError::Unauthorized)?;
    let memberships = membership::memberships(&master, session.account).await?;
    Ok(Json(ApiResponse {
        data: Me {
            account: profile,
            memberships,
        },
    }))
}

async fn change_password(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(passwords): State<Passwords>,
    jar: CookieJar,
    client: Client,
    ValidatedJson(body): ValidatedJson<PasswordChange>,
) -> AppResult<StatusCode> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    account::change_password(
        &master,
        &passwords,
        session.account,
        token(&jar)?,
        body.current_password,
        body.new_password,
        &client,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn sessions(
    State(db): State<Db>,
    State(name): State<MasterName>,
    jar: CookieJar,
) -> AppResult<Json<ApiResponse<Vec<SessionRecord>>>> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    let data = session::sessions(&master, session.account, token(&jar)?).await?;
    Ok(Json(ApiResponse { data }))
}

async fn end_session(
    State(db): State<Db>,
    State(name): State<MasterName>,
    jar: CookieJar,
    client: Client,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    if session::end_session(&master, session.account, &id, &client).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

async fn end_other_sessions(
    State(db): State<Db>,
    State(name): State<MasterName>,
    jar: CookieJar,
    client: Client,
) -> AppResult<StatusCode> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    session::end_other_sessions(&master, session.account, token(&jar)?, &client).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_up_totp(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(passwords): State<Passwords>,
    State(cipher): State<Cipher>,
    jar: CookieJar,
    client: Client,
    ValidatedJson(body): ValidatedJson<PasswordBody>,
) -> AppResult<Json<ApiResponse<TotpSetup>>> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    let data = mfa::set_up_totp(
        &master,
        &passwords,
        &cipher,
        session,
        body.password,
        &client,
    )
    .await?;
    Ok(Json(ApiResponse { data }))
}

async fn confirm_totp(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(passwords): State<Passwords>,
    State(cipher): State<Cipher>,
    jar: CookieJar,
    client: Client,
    ValidatedJson(body): ValidatedJson<CodeBody>,
) -> AppResult<Json<ApiResponse<RecoveryCodes>>> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    let recovery_codes = mfa::confirm_totp(
        &master,
        &passwords,
        &cipher,
        session.account,
        token(&jar)?,
        &body.code,
        &client,
    )
    .await?;
    Ok(Json(ApiResponse {
        data: RecoveryCodes { recovery_codes },
    }))
}

async fn disable_totp(
    State(db): State<Db>,
    State(name): State<MasterName>,
    State(passwords): State<Passwords>,
    jar: CookieJar,
    client: Client,
    ValidatedJson(body): ValidatedJson<PasswordBody>,
) -> AppResult<StatusCode> {
    let master = master(&db, &name).await?;
    let session = signed_in(&master, &jar).await?;
    mfa::disable_totp(&master, &passwords, session, body.password, &client).await?;
    Ok(StatusCode::NO_CONTENT)
}

// the address comes from the ClientIpSource the server puts in the extensions; a request without
// it, such as one that bypassed the proxy, is recorded without an address
impl<S: Send + Sync> FromRequestParts<S> for Client {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let ip = match ClientIp::from_request_parts(parts, state).await {
            Ok(ClientIp(ip)) => Some(ip),
            Err(Rejection::NoClientIpSource) => {
                tracing::error!("no ClientIpSource in the extensions: the address goes unrecorded");
                None
            }
            Err(_) => None,
        };
        let user_agent = parts
            .headers
            .get(header::USER_AGENT)
            .and_then(|agent| agent.to_str().ok())
            .map(|agent| agent.chars().take(USER_AGENT_MAX).collect());
        Ok(Self { ip, user_agent })
    }
}

fn token(jar: &CookieJar) -> AppResult<&str> {
    jar.get(SESSION_COOKIE)
        .map(Cookie::value)
        .ok_or(AppError::Unauthorized)
}

fn challenge(jar: &CookieJar) -> AppResult<&str> {
    jar.get(LOGIN_COOKIE)
        .map(Cookie::value)
        .ok_or(AppError::Unauthorized)
}

pub(crate) async fn signed_in(master: &Surreal<Any>, jar: &CookieJar) -> AppResult<Session> {
    session::authenticate(master, token(jar)?)
        .await?
        .ok_or(AppError::Unauthorized)
}

pub(crate) async fn master(db: &Db, name: &MasterName) -> AppResult<Surreal<Any>> {
    db.system_session(&name.0)
        .await
        .map_err(|error| AppError::Internal(error.into()))
}

fn session_cookie(value: String) -> Cookie<'static> {
    cookie(SESSION_COOKIE, value)
}

fn login_cookie(value: String) -> Cookie<'static> {
    cookie(LOGIN_COOKIE, value)
}

// no Max-Age: the cookie goes with the browser, and the server ends what it holds on its own
fn cookie(name: &'static str, value: String) -> Cookie<'static> {
    Cookie::build((name, value))
        .path("/")
        .secure(true)
        .http_only(true)
        .same_site(SameSite::Strict)
        .build()
}

impl From<AuthError> for AppError {
    fn from(error: AuthError) -> Self {
        let message = error.to_string();
        match error {
            AuthError::InvalidCredentials | AuthError::LoginExpired => Self::Unauthorized,
            AuthError::LoginLocked { retry_after } => Self::TooManyRequests {
                message,
                retry_after: Some(retry_after),
            },
            AuthError::LoginBlocked => Self::TooManyRequests {
                message,
                retry_after: None,
            },
            AuthError::SecondFactorNeeded => Self::MfaRequired,
            AuthError::UnknownTenant(_) | AuthError::UnknownAccount(_) => Self::NotFound,
            AuthError::UnknownRole(_) | AuthError::Password(PasswordError::Length) => {
                Self::Validation(message)
            }
            AuthError::WrongPassword | AuthError::WrongCode => Self::Unprocessable(message),
            AuthError::TotpRequired | AuthError::NoTotpSetup => Self::Conflict(message),
            AuthError::Password(_)
            | AuthError::Cipher(_)
            | AuthError::Totp(_)
            | AuthError::Hashing(_)
            | AuthError::Db(_) => Self::Internal(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use axum::middleware;
    use axum_client_ip::ClientIpSource;
    use resysto_model::{Member, MemberId};
    use surrealdb::types::SurrealValue;
    use tower::ServiceExt;

    use super::*;
    use crate::account::create_account;
    use crate::membership::add_member;
    use crate::testing::{
        PASSWORD, ada, new_master, new_tenant, passwords, server, session_token, sign_in,
    };

    #[derive(Clone)]
    struct TestState {
        db: Db,
        master: MasterName,
        policies: Arc<Policies>,
    }

    impl FromRef<TestState> for Db {
        fn from_ref(state: &TestState) -> Self {
            state.db.clone()
        }
    }

    impl FromRef<TestState> for MasterName {
        fn from_ref(state: &TestState) -> Self {
            state.master.clone()
        }
    }

    impl FromRef<TestState> for Arc<Policies> {
        fn from_ref(state: &TestState) -> Self {
            Arc::clone(&state.policies)
        }
    }

    #[derive(Debug, PartialEq, SurrealValue)]
    struct Refusal {
        event: String,
        by: MemberId,
        session: String,
        request: String,
        ip: String,
        user_agent: String,
        action: String,
        resource: String,
    }

    #[tokio::test]
    async fn a_refused_request_leaves_who_asked_and_from_where_in_the_tenant() {
        let server = server().await;
        let (name, master) = new_master(&server).await;
        let tenant = new_tenant(&server, &master, "acme").await;
        let reader = vec!["tenant/reader".to_owned()];
        let account = create_account(&master, &passwords(), ada(), PASSWORD.into())
            .await
            .unwrap();
        add_member(&server, &master, account, "acme", &reader)
            .await
            .unwrap();
        let token = session_token(sign_in(&master, ada()).await);
        let state = TestState {
            db: server.clone(),
            master: MasterName(name.into()),
            policies: Arc::new(Policies::load().unwrap()),
        };
        let routes = Router::new()
            .route("/audit/changes", get(async || StatusCode::OK))
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                require_member,
            ));
        let app = Router::new()
            .nest("/t/{slug}/api/v1", routes)
            .with_state(state)
            .layer(ClientIpSource::ConnectInfo.into_extension());
        let request = Request::get("/t/acme/api/v1/audit/changes")
            .header(header::COOKIE, format!("{SESSION_COOKIE}={token}"))
            .header(REQUEST_ID, "r-9")
            .header(header::USER_AGENT, "Firefox")
            .extension(ConnectInfo(SocketAddr::from(([10, 0, 0, 7], 51000))))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let refusals: Vec<Refusal> = server
            .system_session(&tenant)
            .await
            .unwrap()
            .query(
                "SELECT event, by, session, request, ip, user_agent, action, resource
                 FROM audit_event",
            )
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let expected = Refusal {
            event: "refused".into(),
            by: account.cast::<Member>(),
            session: key(&token),
            request: "r-9".into(),
            ip: "10.0.0.7".into(),
            user_agent: "Firefox".into(),
            action: "read".into(),
            resource: "audit".into(),
        };
        assert_eq!(refusals, [expected]);
    }
}
