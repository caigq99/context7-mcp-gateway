//! Administrator-only authentication backed by server-side WebAuthn ceremonies.

mod store;

use std::{
    sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use webauthn_rs::prelude::{
    CreationChallengeResponse, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, RequestChallengeResponse, Webauthn, WebauthnBuilder,
};
use zeroize::Zeroizing;

use crate::{
    config::{Config, Environment},
    management::{ApiError, ApiErrorCode, AppState},
};

pub use store::issue_enrollment_ticket;

const CEREMONY_TTL_SECONDS: i64 = 300;
const SESSION_TTL_SECONDS: i64 = 8 * 60 * 60;
const MAX_PASSKEYS: i64 = 12;
const MAX_CREDENTIAL_ID_BYTES: usize = 1023;
const PASSKEY_ACTOR: &str = "passkey-admin";
const PASSWORD_ACTOR: &str = "password-admin";
const PASSWORD_FAILURE_LIMIT: u32 = 5;
const PASSWORD_FAILURE_WINDOW: Duration = Duration::from_secs(10 * 60);
const PASSWORD_LOCKOUT: Duration = Duration::from_secs(10 * 60);

type SecretDigest = [u8; 32];

/// An initial ticket only works before enrollment. A recovery ticket replaces the active passkeys.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentMode {
    Initial,
    Recovery,
}

impl EnrollmentMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Recovery => "recovery",
        }
    }

    const fn matches_active_count(self, active: i64) -> bool {
        match self {
            Self::Initial => active == 0,
            Self::Recovery => active > 0,
        }
    }
}

#[derive(Clone, Copy)]
enum CookieKind {
    Login,
    Registration,
    Session,
}

impl CookieKind {
    const fn name(self, secure: bool) -> &'static str {
        match (self, secure) {
            (Self::Login, true) => "__Host-c7gw-admin-login",
            (Self::Registration, true) => "__Host-c7gw-admin-registration",
            (Self::Session, true) => "__Host-c7gw-admin-session",
            (Self::Login, false) => "c7gw-local-admin-login",
            (Self::Registration, false) => "c7gw-local-admin-registration",
            (Self::Session, false) => "c7gw-local-admin-session",
        }
    }

    const fn ttl(self) -> i64 {
        match self {
            Self::Login | Self::Registration => CEREMONY_TTL_SECONDS,
            Self::Session => SESSION_TTL_SECONDS,
        }
    }
}

#[derive(Clone, Copy)]
enum AdminSessionKind {
    Passkey,
    Password,
}

#[derive(Clone, Copy)]
enum CeremonyPurpose {
    Login,
    Registration,
}

impl CeremonyPurpose {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Registration => "registration",
        }
    }

    const fn cookie(self) -> CookieKind {
        match self {
            Self::Login => CookieKind::Login,
            Self::Registration => CookieKind::Registration,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RegistrationPermit {
    Ticket {
        mode: EnrollmentMode,
        digest: SecretDigest,
    },
    Session {
        digest: SecretDigest,
    },
    PasswordSession {
        digest: SecretDigest,
    },
}

#[derive(Serialize, Deserialize)]
struct RegistrationState {
    passkey_state: PasskeyRegistration,
    permit: RegistrationPermit,
    name: String,
}

#[derive(Deserialize)]
struct RegistrationStart {
    name: String,
    enrollment_code: Option<String>,
}

#[derive(Deserialize)]
struct CredentialSubmission<T> {
    credential: T,
}

#[derive(Serialize)]
struct SessionStatus {
    authenticated: bool,
    password_login_available: bool,
    passkey_available: bool,
    passkey_login_available: bool,
}

#[derive(Serialize)]
struct PasskeySummary {
    id: String,
    name: String,
    created_at: i64,
    last_used_at: Option<i64>,
}

#[derive(Deserialize)]
struct PasswordLoginRequest {
    username: String,
    password: String,
}

#[derive(Default)]
struct PasswordFailureState {
    failures: u32,
    window_started: Option<Instant>,
    blocked_until: Option<Instant>,
}

static PASSWORD_FAILURES: LazyLock<Mutex<PasswordFailureState>> =
    LazyLock::new(|| Mutex::new(PasswordFailureState::default()));

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/session", get(session))
        .route(
            "/password/login",
            post(password_login).layer(DefaultBodyLimit::max(8 * 1024)),
        )
        .route("/login/start", post(login_start))
        .route("/login/finish", post(login_finish))
        .route("/register/start", post(registration_start))
        .route("/register/finish", post(registration_finish))
        .route("/logout", post(logout))
        .route("/passkeys", get(list_passkeys))
        .route("/passkeys/{id}", delete(remove_passkey))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn(no_cache))
}

pub(crate) async fn authorize_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, ApiError> {
    if let Some((_, kind)) = admin_session(state, headers).await? {
        return Ok(match kind {
            AdminSessionKind::Passkey => PASSKEY_ACTOR,
            AdminSessionKind::Password => PASSWORD_ACTOR,
        }
        .to_owned());
    }
    if has_development_bearer(&state.config, headers) {
        return Ok("development-admin".to_owned());
    }
    Err(unauthorized())
}

pub(crate) fn has_development_bearer(config: &Config, headers: &HeaderMap) -> bool {
    if config.environment == Environment::Production {
        return false;
    }
    let Some(expected) = config.admin_token_digest else {
        return false;
    };
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let supplied = values
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    match (supplied, values.next()) {
        (Some(value), None) => bool::from(expected.ct_eq(&digest(value))),
        _ => false,
    }
}

async fn no_cache(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<SessionStatus>, ApiError> {
    let authenticated = admin_session(&state, &headers).await?.is_some();
    let passkey_login_available = store::active_passkey_count(&state.pool).await? > 0;
    Ok(Json(SessionStatus {
        authenticated,
        password_login_available: state.config.admin_password_enabled(),
        passkey_available: state.config.admin_passkey_enabled(),
        passkey_login_available,
    }))
}

async fn password_login(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    payload: Result<Json<PasswordLoginRequest>, JsonRejection>,
) -> Result<(HeaderMap, Json<SessionStatus>), ApiError> {
    check_origin(&state.config, &headers)?;
    let config = state
        .config
        .admin_password
        .as_ref()
        .ok_or_else(|| ApiError::new(ApiErrorCode::NotConfigured))?;
    let Json(request) = payload.map_err(|_| ApiError::new(ApiErrorCode::InvalidRequest))?;
    ensure_password_login_allowed()?;
    let password = Zeroizing::new(request.password);
    if !config.verify(&request.username, password.as_str()) {
        return Err(record_password_failure());
    }
    clear_password_failures();
    let previous_session = cookie_digest(&state.config, &headers, CookieKind::Session);
    let secret = store::new_password_session(
        &state.pool,
        state.config.public_origin.as_str(),
        previous_session,
        config.session_version(),
    )
    .await?;
    let passkey_login_available = store::active_passkey_count(&state.pool).await? > 0;
    authenticated_response(&state.config, None, &secret, passkey_login_available)
}

async fn login_start(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<RequestChallengeResponse>), ApiError> {
    check_origin(&state.config, &headers)?;
    let webauthn = webauthn(&state.config)?;
    let passkeys = store::active_passkeys(&state.pool).await?;
    if passkeys.is_empty() {
        return Err(unauthorized());
    }
    let (challenge, passkey_state) = webauthn
        .start_passkey_authentication(&passkeys)
        .map_err(|_| internal_error())?;
    let cookie = start_ceremony(&state, &headers, CeremonyPurpose::Login, &passkey_state).await?;
    Ok((cookie, Json(challenge)))
}

async fn login_finish(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CredentialSubmission<PublicKeyCredential>>,
) -> Result<(HeaderMap, Json<SessionStatus>), ApiError> {
    check_origin(&state.config, &headers)?;
    let webauthn = webauthn(&state.config)?;
    let passkey_state: PasskeyAuthentication =
        take_ceremony(&state, &headers, CeremonyPurpose::Login).await?;
    let result = webauthn
        .finish_passkey_authentication(&request.credential, &passkey_state)
        .map_err(|_| unauthorized())?;
    let previous_session = cookie_digest(&state.config, &headers, CookieKind::Session);
    let secret = store::complete_login(
        &state.pool,
        state.config.public_origin.as_str(),
        previous_session,
        &result,
    )
    .await?;
    authenticated_response(&state.config, Some(CookieKind::Login), &secret, true)
}

async fn registration_start(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<RegistrationStart>,
) -> Result<(HeaderMap, Json<CreationChallengeResponse>), ApiError> {
    check_origin(&state.config, &headers)?;
    let webauthn = webauthn(&state.config)?;
    let name = request.name.trim();
    if name.is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let passkeys = store::active_passkeys(&state.pool).await?;
    let permit = match request.enrollment_code {
        Some(code) => {
            let code = Zeroizing::new(code);
            let ticket_digest = checked_digest(code.trim()).ok_or_else(unauthorized)?;
            let mode = store::enrollment_mode(&state.pool, &ticket_digest).await?;
            RegistrationPermit::Ticket {
                mode,
                digest: ticket_digest,
            }
        }
        None => {
            let (session_digest, kind) = admin_session(&state, &headers)
                .await?
                .ok_or_else(unauthorized)?;
            if passkeys.len() as i64 >= MAX_PASSKEYS {
                return Err(ApiError::new(ApiErrorCode::Conflict));
            }
            match kind {
                AdminSessionKind::Passkey => RegistrationPermit::Session {
                    digest: session_digest,
                },
                AdminSessionKind::Password => RegistrationPermit::PasswordSession {
                    digest: session_digest,
                },
            }
        }
    };
    let exclude_credentials = passkeys.iter().map(|key| key.cred_id().clone()).collect();
    let (challenge, passkey_state) = webauthn
        .start_passkey_registration(
            store::user_handle(&state.pool).await?,
            "gateway-admin",
            "Context7 Gateway Administrator",
            Some(exclude_credentials),
        )
        .map_err(|_| internal_error())?;
    let registration = RegistrationState {
        passkey_state,
        permit,
        name: name.to_owned(),
    };
    let cookie = start_ceremony(
        &state,
        &headers,
        CeremonyPurpose::Registration,
        &registration,
    )
    .await?;
    Ok((cookie, Json(challenge)))
}

async fn registration_finish(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<CredentialSubmission<RegisterPublicKeyCredential>>,
) -> Result<(HeaderMap, Json<SessionStatus>), ApiError> {
    check_origin(&state.config, &headers)?;
    let webauthn = webauthn(&state.config)?;
    let registration: RegistrationState =
        take_ceremony(&state, &headers, CeremonyPurpose::Registration).await?;
    let previous_session = cookie_digest(&state.config, &headers, CookieKind::Session);
    match &registration.permit {
        RegistrationPermit::Session { digest } | RegistrationPermit::PasswordSession { digest }
            if previous_session != Some(*digest) =>
        {
            return Err(unauthorized());
        }
        _ => {}
    }
    let passkey = webauthn
        .finish_passkey_registration(&request.credential, &registration.passkey_state)
        .map_err(|_| unauthorized())?;
    let secret = store::complete_registration(
        &state.pool,
        state.config.public_origin.as_str(),
        previous_session,
        &registration,
        &passkey,
        state
            .config
            .admin_password
            .as_ref()
            .map(|password| password.session_version()),
    )
    .await?;
    authenticated_response(&state.config, Some(CookieKind::Registration), &secret, true)
}

async fn logout(
    State(state): State<Arc<AppState>>,
    request_headers: HeaderMap,
) -> Result<(StatusCode, HeaderMap), ApiError> {
    check_origin(&state.config, &request_headers)?;
    let mut headers = HeaderMap::new();
    for kind in [
        CookieKind::Session,
        CookieKind::Login,
        CookieKind::Registration,
    ] {
        headers.append(header::SET_COOKIE, cookie(&state.config, kind, "", 0)?);
    }
    store::logout(
        &state.pool,
        state.config.public_origin.as_str(),
        cookie_digest(&state.config, &request_headers, CookieKind::Session),
        cookie_digest(&state.config, &request_headers, CookieKind::Login),
        cookie_digest(&state.config, &request_headers, CookieKind::Registration),
    )
    .await?;
    Ok((StatusCode::NO_CONTENT, headers))
}

async fn list_passkeys(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<PasskeySummary>>, ApiError> {
    admin_session(&state, &headers)
        .await?
        .ok_or_else(unauthorized)?;
    Ok(Json(store::passkey_summaries(&state.pool).await?))
}

async fn remove_passkey(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    check_origin(&state.config, &headers)?;
    let session =
        cookie_digest(&state.config, &headers, CookieKind::Session).ok_or_else(unauthorized)?;
    if id.len() > (MAX_CREDENTIAL_ID_BYTES * 4).div_ceil(3) {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let credential_id = URL_SAFE_NO_PAD
        .decode(id)
        .map_err(|_| ApiError::new(ApiErrorCode::InvalidRequest))?;
    if credential_id.is_empty() {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    store::remove_passkey(
        &state.pool,
        state.config.public_origin.as_str(),
        &session,
        state
            .config
            .admin_password
            .as_ref()
            .map(|password| password.session_version()),
        state.config.admin_password_enabled(),
        &credential_id,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn webauthn(config: &Config) -> Result<Webauthn, ApiError> {
    if !config.admin_passkey_enabled() {
        return Err(ApiError::new(ApiErrorCode::NotConfigured));
    }
    let origin = &config.public_origin;
    let relying_party = origin.domain().ok_or_else(internal_error)?;
    WebauthnBuilder::new(relying_party, origin)
        .map(|builder| {
            builder
                .rp_name("Context7 Gateway")
                .timeout(Duration::from_secs(CEREMONY_TTL_SECONDS.cast_unsigned()))
        })
        .and_then(WebauthnBuilder::build)
        .map_err(|_| internal_error())
}

fn check_origin(config: &Config, headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = config.public_origin.origin().ascii_serialization();
    let mut origins = headers.get_all(header::ORIGIN).iter();
    let supplied = origins.next().and_then(|value| value.to_str().ok());
    if supplied == Some(expected.as_str()) && origins.next().is_none() {
        return Ok(());
    }
    Err(ApiError::new(ApiErrorCode::Forbidden))
}

async fn passkey_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<SecretDigest>, ApiError> {
    if !state.config.admin_passkey_enabled() {
        return Ok(None);
    }
    let Some(digest) = cookie_digest(&state.config, headers, CookieKind::Session) else {
        return Ok(None);
    };
    let credential =
        store::session_credential(&state.pool, state.config.public_origin.as_str(), &digest)
            .await?;
    Ok(credential.map(|_| digest))
}

async fn admin_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<(SecretDigest, AdminSessionKind)>, ApiError> {
    if let Some(digest) = passkey_session(state, headers).await? {
        return Ok(Some((digest, AdminSessionKind::Passkey)));
    }
    if let Some(digest) = password_session(state, headers).await? {
        return Ok(Some((digest, AdminSessionKind::Password)));
    }
    Ok(None)
}

async fn password_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<SecretDigest>, ApiError> {
    let Some(config) = state.config.admin_password.as_ref() else {
        return Ok(None);
    };
    let Some(digest) = cookie_digest(&state.config, headers, CookieKind::Session) else {
        return Ok(None);
    };
    let valid = store::password_session_exists(
        &state.pool,
        state.config.public_origin.as_str(),
        &digest,
        config.session_version(),
    )
    .await?;
    Ok(valid.then_some(digest))
}

async fn start_ceremony<T: Serialize>(
    state: &AppState,
    request_headers: &HeaderMap,
    purpose: CeremonyPurpose,
    ceremony: &T,
) -> Result<HeaderMap, ApiError> {
    let kind = purpose.cookie();
    let secret = new_secret().map_err(|_| internal_error())?;
    let mut headers = HeaderMap::new();
    headers.append(
        header::SET_COOKIE,
        cookie(&state.config, kind, &secret, kind.ttl())?,
    );
    let stored_ceremony = serde_json::to_string(ceremony).map_err(|_| internal_error())?;
    store::start_ceremony(
        &state.pool,
        purpose,
        &digest(&secret),
        cookie_digest(&state.config, request_headers, kind),
        &stored_ceremony,
    )
    .await?;
    Ok(headers)
}

async fn take_ceremony<T: for<'de> Deserialize<'de>>(
    state: &AppState,
    headers: &HeaderMap,
    purpose: CeremonyPurpose,
) -> Result<T, ApiError> {
    let digest =
        cookie_digest(&state.config, headers, purpose.cookie()).ok_or_else(unauthorized)?;
    let stored_ceremony = store::take_ceremony(&state.pool, purpose, &digest).await?;
    serde_json::from_str(&stored_ceremony).map_err(|_| internal_error())
}

fn authenticated_response(
    config: &Config,
    ceremony_cookie: Option<CookieKind>,
    secret: &str,
    passkey_login_available: bool,
) -> Result<(HeaderMap, Json<SessionStatus>), ApiError> {
    let mut headers = HeaderMap::new();
    if let Some(ceremony_cookie) = ceremony_cookie {
        headers.append(header::SET_COOKIE, cookie(config, ceremony_cookie, "", 0)?);
    }
    headers.append(
        header::SET_COOKIE,
        cookie(config, CookieKind::Session, secret, SESSION_TTL_SECONDS)?,
    );
    Ok((
        headers,
        Json(SessionStatus {
            authenticated: true,
            password_login_available: config.admin_password_enabled(),
            passkey_available: config.admin_passkey_enabled(),
            passkey_login_available,
        }),
    ))
}

fn cookie(
    config: &Config,
    kind: CookieKind,
    value: &str,
    age: i64,
) -> Result<HeaderValue, ApiError> {
    let secure = config.public_origin.scheme() == "https";
    let name = kind.name(secure);
    let attributes = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={age}{attributes}"
    ))
    .map_err(|_| internal_error())
}

fn cookie_digest(config: &Config, headers: &HeaderMap, kind: CookieKind) -> Option<SecretDigest> {
    let name = kind.name(config.public_origin.scheme() == "https");
    let mut matches = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|part| part.trim().split_once('='))
        .filter_map(|(key, value)| (key == name).then_some(value));
    let value = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    checked_digest(value)
}

fn checked_digest(value: &str) -> Option<SecretDigest> {
    (value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
    .then(|| digest(value))
}

fn digest(value: &str) -> SecretDigest {
    Sha256::digest(value.as_bytes()).into()
}

fn new_secret() -> Result<Zeroizing<String>, getrandom::Error> {
    let mut data = Zeroizing::new([0_u8; 32]);
    getrandom::fill(data.as_mut())?;
    Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(data.as_ref())))
}

fn password_failures() -> MutexGuard<'static, PasswordFailureState> {
    PASSWORD_FAILURES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn ensure_password_login_allowed() -> Result<(), ApiError> {
    let now = Instant::now();
    let mut failures = password_failures();
    if let Some(blocked_until) = failures.blocked_until {
        if now < blocked_until {
            return Err(ApiError::new(ApiErrorCode::RateLimited)
                .retry_after(blocked_until.duration_since(now).as_secs().max(1) as i64));
        }
        *failures = PasswordFailureState::default();
    }
    if failures
        .window_started
        .is_some_and(|started| now.duration_since(started) > PASSWORD_FAILURE_WINDOW)
    {
        *failures = PasswordFailureState::default();
    }
    Ok(())
}

fn record_password_failure() -> ApiError {
    let now = Instant::now();
    let mut failures = password_failures();
    if failures
        .window_started
        .is_none_or(|started| now.duration_since(started) > PASSWORD_FAILURE_WINDOW)
    {
        failures.failures = 0;
        failures.window_started = Some(now);
    }
    failures.failures = failures.failures.saturating_add(1);
    if failures.failures >= PASSWORD_FAILURE_LIMIT {
        failures.blocked_until = Some(now + PASSWORD_LOCKOUT);
    }
    ApiError::new(ApiErrorCode::InvalidAdminCredentials)
}

fn clear_password_failures() {
    *password_failures() = PasswordFailureState::default();
}

#[cfg(test)]
pub(crate) fn reset_password_failures() {
    clear_password_failures();
}

fn unauthorized() -> ApiError {
    ApiError::new(ApiErrorCode::Unauthorized)
}

fn internal_error() -> ApiError {
    ApiError::new(ApiErrorCode::InternalError)
}

#[cfg(test)]
mod tests;
