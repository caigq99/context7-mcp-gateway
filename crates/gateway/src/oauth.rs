use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    config::LinuxDoOAuthConfig,
    credentials::{
        CredentialCipher, EncryptedCredential, GatewayTokenId, issue_gateway_token,
        parse_gateway_token, verify_gateway_token,
    },
    management::{
        ApiError, ApiErrorCode, AppState, GatewayTokenScope, encode_scopes, map_database_error,
        write_audit,
    },
    session,
};

const OAUTH_STATE_TTL_SECONDS: i64 = 600;
const USER_SESSION_TTL_SECONDS: i64 = 14 * 24 * 60 * 60;
const INITIAL_TOKEN_TTL_SECONDS: i64 = 5 * 60;
const OAUTH_STATE_COOKIE: &str = "c7g_oauth_state";
const USER_SESSION_COOKIE: &str = "c7g_session";
const INITIAL_TOKEN_COOKIE: &str = "c7g_initial_token";

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    token_type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UserProfile {
    pub id: String,
    pub display_name: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct CurrentSession {
    pub user: UserProfile,
}

#[derive(Serialize)]
pub struct UserGatewayToken {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub token: Option<String>,
    pub status: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub scopes: Vec<GatewayTokenScope>,
    pub can_copy: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateTokenRequest {
    name: String,
    expires_in_days: Option<u16>,
    scopes: Option<Vec<GatewayTokenScope>>,
}

pub async fn login(State(state): State<AppState>) -> Result<Response, ApiError> {
    let oauth = state
        .config
        .linuxdo_oauth
        .as_ref()
        .ok_or_else(|| ApiError::new(ApiErrorCode::NotConfigured))?;
    let raw_state = random_url_token(32)?;
    let digest = session::token_digest(&raw_state, state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let verifier = pkce_verifier(&state, &raw_state)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    sqlx::query("DELETE FROM oauth_states WHERE unixepoch(expires_at) <= unixepoch() OR used_at IS NOT NULL")
        .execute(&state.pool)
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    sqlx::query("INSERT INTO oauth_states(state_digest, expires_at) VALUES (?, datetime('now', '+' || ? || ' seconds'))")
        .bind(digest.to_vec())
        .bind(OAUTH_STATE_TTL_SECONDS)
        .execute(&state.pool)
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;

    let mut authorization_url = oauth.authorize_url.clone();
    authorization_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &oauth.client_id)
        .append_pair("redirect_uri", oauth.redirect_url.as_str())
        .append_pair("scope", &oauth.scope)
        .append_pair("state", &raw_state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let mut response = Redirect::to(authorization_url.as_str()).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        cookie_header(
            &state,
            OAUTH_STATE_COOKIE,
            &raw_state,
            OAUTH_STATE_TTL_SECONDS,
        )?,
    );
    Ok(response)
}

pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Result<Response, ApiError> {
    if query.error.is_some() {
        return Err(ApiError::new(ApiErrorCode::Unauthorized));
    }
    let oauth = state
        .config
        .linuxdo_oauth
        .as_ref()
        .ok_or_else(|| ApiError::new(ApiErrorCode::NotConfigured))?;
    let authorization_code = query
        .code
        .as_deref()
        .filter(|value| !value.is_empty() && value.len() <= 4096)
        .ok_or_else(|| ApiError::new(ApiErrorCode::InvalidRequest))?;
    let state_value = query
        .state
        .as_deref()
        .filter(|value| value.len() == 43)
        .ok_or_else(|| ApiError::new(ApiErrorCode::Unauthorized))?;
    let cookie_state = cookie_value(&headers, OAUTH_STATE_COOKIE)
        .ok_or_else(|| ApiError::new(ApiErrorCode::Unauthorized))?;
    if !constant_time_eq(state_value.as_bytes(), cookie_state.as_bytes()) {
        return Err(ApiError::new(ApiErrorCode::Unauthorized));
    }
    let state_digest = session::token_digest(state_value, state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::Unauthorized))?;
    let consumed = sqlx::query("UPDATE oauth_states SET used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE state_digest = ? AND used_at IS NULL AND unixepoch(expires_at) > unixepoch()")
        .bind(state_digest.to_vec())
        .execute(&state.pool)
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    if consumed.rows_affected() != 1 {
        return Err(ApiError::new(ApiErrorCode::Unauthorized));
    }

    let verifier = pkce_verifier(&state, state_value)?;
    let access_token = exchange_code(&state.http, oauth, authorization_code, &verifier).await?;
    let profile = fetch_profile(&state.http, oauth, &access_token).await?;
    let (user, initial_token) = upsert_user(&state, oauth, &profile).await?;
    let session_token = random_url_token(32)?;
    let session_digest = session::token_digest(&session_token, state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let session_id = Uuid::new_v4().simple().to_string();
    let mut tx = state.pool.begin().await.map_err(map_database_error)?;
    if let Some(old) = cookie_value(&headers, user_cookie_name(&state)) {
        revoke_browser_session(&mut tx, &state, old).await?;
    }
    sqlx::query("INSERT INTO auth_sessions(id, user_id, session_digest, expires_at) VALUES (?, ?, ?, datetime('now', '+' || ? || ' seconds'))")
        .bind(session_id)
        .bind(&user.id)
        .bind(session_digest.to_vec())
        .bind(USER_SESSION_TTL_SECONDS)
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    tx.commit().await.map_err(map_database_error)?;

    let mut response = Redirect::to("/console/dashboard").into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        cookie_header(
            &state,
            user_cookie_name(&state),
            &session_token,
            USER_SESSION_TTL_SECONDS,
        )?,
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        expire_cookie(&state, OAUTH_STATE_COOKIE)?,
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        if let Some(token) = initial_token {
            initial_token_cookie(&state, token.as_str(), INITIAL_TOKEN_TTL_SECONDS)?
        } else {
            expire_initial_token_cookie(&state)?
        },
    );
    Ok(response)
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(raw) = cookie_value(&headers, user_cookie_name(&state)) {
        let mut connection = state.pool.acquire().await.map_err(map_database_error)?;
        revoke_browser_session(&mut connection, &state, raw).await?;
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        expire_cookie(&state, user_cookie_name(&state))?,
    );
    response
        .headers_mut()
        .append(header::SET_COOKIE, expire_initial_token_cookie(&state)?);
    Ok(response)
}

pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<CurrentSession>, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    Ok(Json(CurrentSession { user }))
}

pub async fn list_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<UserGatewayToken>>, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let rows = sqlx::query_as::<_, UserGatewayTokenRow>(
        "SELECT id, prefix, name, CASE WHEN status = 'active' AND unixepoch(expires_at) <= unixepoch() THEN 'expired' ELSE status END AS status, created_at, last_used_at, expires_at, scopes_json, CASE WHEN token_ciphertext IS NOT NULL AND status = 'active' AND (expires_at IS NULL OR unixepoch(expires_at) > unixepoch()) THEN 1 ELSE 0 END AS can_copy FROM gateway_tokens WHERE user_id = ? ORDER BY created_at DESC",
    )
    .bind(&user.id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    Ok(Json(
        rows.into_iter()
            .map(|row| UserGatewayToken {
                id: row.id,
                prefix: row.prefix,
                name: row.name,
                status: row.status,
                created_at: row.created_at,
                last_used_at: row.last_used_at,
                expires_at: row.expires_at,
                scopes: serde_json::from_str(&row.scopes_json).unwrap_or_default(),
                can_copy: row.can_copy,
                token: None,
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct InitialGatewayToken {
    token: String,
    name: String,
}

pub async fn initial_token(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let cookie_name = initial_token_cookie_name(&state);
    let Some(raw_token) = cookie_value(&headers, cookie_name) else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let invalid = || {
        let mut response = StatusCode::NO_CONTENT.into_response();
        if let Ok(cookie) = expire_initial_token_cookie(&state) {
            response.headers_mut().append(header::SET_COOKIE, cookie);
        }
        response
    };
    let Ok(token_id) = parse_gateway_token(raw_token) else {
        return Ok(invalid());
    };
    let record = sqlx::query_as::<_, (Vec<u8>, String, String)>(
        "SELECT token_digest, user_id, name FROM gateway_tokens \
         WHERE id = ? AND status = 'active' \
         AND (expires_at IS NULL OR unixepoch(expires_at) > unixepoch())",
    )
    .bind(&token_id.0)
    .fetch_optional(&state.pool)
    .await;
    let record = match record {
        Ok(Some(record)) => record,
        _ => return Ok(invalid()),
    };
    let verified =
        verify_gateway_token(raw_token, &record.0, state.config.token_pepper).unwrap_or(false);
    if record.1 != user.id || !verified {
        return Ok(invalid());
    }
    Ok(Json(InitialGatewayToken {
        token: raw_token.to_owned(),
        name: record.2,
    })
    .into_response())
}

pub async fn acknowledge_initial_token(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(StatusCode, HeaderMap), ApiError> {
    authenticate_browser_session(&state, &headers).await?;
    let mut response_headers = HeaderMap::new();
    response_headers.append(header::SET_COOKIE, expire_initial_token_cookie(&state)?);
    Ok((StatusCode::NO_CONTENT, response_headers))
}

#[derive(Serialize)]
pub struct CopiedGatewayToken {
    token: String,
}

pub async fn copy_token(
    State(state): State<AppState>,
    Path(token_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<CopiedGatewayToken>, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let row = sqlx::query_as::<_, (Option<Vec<u8>>, Option<Vec<u8>>, Option<i64>)>(
        "SELECT token_ciphertext, token_nonce, token_encryption_version \
         FROM gateway_tokens WHERE id = ? AND user_id = ? AND status = 'active' \
         AND (expires_at IS NULL OR unixepoch(expires_at) > unixepoch())",
    )
    .bind(&token_id)
    .bind(&user.id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?
    .ok_or_else(|| ApiError::new(ApiErrorCode::NotFound))?;
    let (Some(ciphertext), Some(nonce), Some(1)) = row else {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    };
    let nonce = nonce
        .try_into()
        .map_err(|_| ApiError::new(ApiErrorCode::NotFound))?;
    let cipher = CredentialCipher::new(state.config.encryption_key)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let token = cipher
        .decrypt_gateway_token(
            &GatewayTokenId(token_id),
            &EncryptedCredential { nonce, ciphertext },
        )
        .map_err(|_| ApiError::new(ApiErrorCode::NotFound))?;
    Ok(Json(CopiedGatewayToken { token }))
}

pub async fn create_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<UserGatewayToken>), ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let name = clean_token_name(&request.name)?;
    let expires = expiry_modifier(request.expires_in_days)?;
    let scopes = encode_scopes(request.scopes)?;
    let issued = issue_gateway_token(state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let row = sqlx::query_as::<_, UserGatewayTokenRow>(
        "INSERT INTO gateway_tokens(id, user_id, token_digest, prefix, name, expires_at, scopes_json) VALUES (?, ?, ?, ?, ?, datetime('now', ?), ?) RETURNING id, prefix, name, status, created_at, last_used_at, expires_at, scopes_json",
    )
    .bind(&issued.id.0)
    .bind(&user.id)
    .bind(issued.digest.to_vec())
    .bind(&issued.prefix)
    .bind(&name)
    .bind(expires)
    .bind(scopes)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    Ok((
        StatusCode::CREATED,
        Json(UserGatewayToken {
            id: row.id,
            name: row.name,
            prefix: row.prefix,
            token: Some(issued.raw_token),
            status: row.status,
            created_at: row.created_at,
            last_used_at: None,
            expires_at: row.expires_at,
            scopes: serde_json::from_str(&row.scopes_json).unwrap_or_default(),
            can_copy: false,
        }),
    ))
}

pub async fn revoke_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(token_id): axum::extract::Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let result = sqlx::query("UPDATE gateway_tokens SET status = 'revoked', revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ? AND user_id = ? AND status = 'active'")
        .bind(token_id)
        .bind(user.id)
        .execute(&state.pool)
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    if result.rows_affected() == 0 {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(sqlx::FromRow)]
struct UserGatewayTokenRow {
    id: String,
    name: String,
    prefix: String,
    status: String,
    created_at: String,
    last_used_at: Option<String>,
    expires_at: Option<String>,
    scopes_json: String,
    can_copy: bool,
}

pub async fn rotate_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token_id): Path<String>,
) -> Result<(StatusCode, Json<UserGatewayToken>), ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let issued = issue_gateway_token(state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(crate::management::map_database_error)?;
    let prior = sqlx::query_as::<_, (String, String, Option<String>)>(
        "UPDATE gateway_tokens SET status = 'revoked', revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ? AND user_id = ? AND status = 'active' AND (expires_at IS NULL OR unixepoch(expires_at) > unixepoch()) RETURNING name, scopes_json, expires_at",
    )
    .bind(token_id)
    .bind(&user.id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(crate::management::map_database_error)?
    .ok_or_else(|| ApiError::new(ApiErrorCode::NotFound))?;
    let created_at = sqlx::query_scalar::<_, String>(
        "INSERT INTO gateway_tokens(id, user_id, token_digest, prefix, name, scopes_json, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING created_at",
    )
    .bind(&issued.id.0)
    .bind(&user.id)
    .bind(issued.digest.to_vec())
    .bind(&issued.prefix)
    .bind(&prior.0)
    .bind(&prior.1)
    .bind(&prior.2)
    .fetch_one(&mut *tx)
    .await
    .map_err(crate::management::map_database_error)?;
    tx.commit()
        .await
        .map_err(crate::management::map_database_error)?;
    Ok((
        StatusCode::CREATED,
        Json(UserGatewayToken {
            id: issued.id.0,
            name: prior.0,
            prefix: issued.prefix,
            token: Some(issued.raw_token),
            status: "active".to_owned(),
            created_at,
            last_used_at: None,
            expires_at: prior.2,
            scopes: serde_json::from_str(&prior.1).unwrap_or_default(),
            can_copy: false,
        }),
    ))
}

pub(crate) async fn authenticate_browser_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<UserProfile, ApiError> {
    let raw_session = cookie_value(headers, user_cookie_name(state))
        .filter(|value| value.len() == 43)
        .ok_or_else(|| ApiError::new(ApiErrorCode::Unauthorized))?;
    let digest = session::token_digest(raw_session, state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::Unauthorized))?;
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT u.id, u.display_name, u.status FROM auth_sessions s JOIN users u ON u.id = s.user_id WHERE s.session_digest = ? AND s.revoked_at IS NULL AND unixepoch(s.expires_at) > unixepoch() AND u.status = 'active'",
    )
    .bind(digest.to_vec())
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?
    .ok_or_else(|| ApiError::new(ApiErrorCode::Unauthorized))?;
    Ok(UserProfile {
        id: row.0,
        display_name: row.1,
        status: row.2,
    })
}

#[derive(Debug, Deserialize)]
struct UpstreamUserProfile {
    id: Option<Value>,
    sub: Option<String>,
    username: Option<String>,
    name: Option<String>,
    active: Option<bool>,
    silenced: Option<bool>,
}

async fn exchange_code(
    http: &reqwest::Client,
    config: &LinuxDoOAuthConfig,
    code: &str,
    verifier: &str,
) -> Result<Zeroizing<String>, ApiError> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", config.redirect_url.as_str()),
        ("client_id", config.client_id.as_str()),
        ("client_secret", config.client_secret.as_str()),
        ("code_verifier", verifier),
    ];
    let response = http
        .post(config.token_url.clone())
        .form(&form)
        .send()
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::Unauthorized))?;
    if !response.status().is_success() {
        return Err(ApiError::new(ApiErrorCode::Unauthorized));
    }
    let token: OAuthTokenResponse = response
        .json()
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::Unauthorized))?;
    if token
        .token_type
        .as_deref()
        .is_some_and(|kind| !kind.eq_ignore_ascii_case("bearer"))
    {
        return Err(ApiError::new(ApiErrorCode::Unauthorized));
    }
    Ok(Zeroizing::new(token.access_token))
}

async fn fetch_profile(
    http: &reqwest::Client,
    config: &LinuxDoOAuthConfig,
    access_token: &str,
) -> Result<UpstreamUserProfile, ApiError> {
    let response = http
        .get(config.userinfo_url.clone())
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::Unauthorized))?;
    if !response.status().is_success() {
        return Err(ApiError::new(ApiErrorCode::Unauthorized));
    }
    response
        .json()
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::Unauthorized))
}

async fn upsert_user(
    state: &AppState,
    config: &LinuxDoOAuthConfig,
    profile: &UpstreamUserProfile,
) -> Result<(UserProfile, Option<Zeroizing<String>>), ApiError> {
    if profile.active == Some(false) || profile.silenced == Some(true) {
        return Err(ApiError::new(ApiErrorCode::Forbidden));
    }
    let issuer = config.userinfo_url.origin().ascii_serialization();
    let subject = profile
        .id
        .as_ref()
        .and_then(value_to_string)
        .or_else(|| profile.sub.clone())
        .filter(|subject| !subject.is_empty() && subject.len() <= 128)
        .ok_or_else(|| ApiError::new(ApiErrorCode::Unauthorized))?;
    let display_name = profile
        .name
        .as_deref()
        .or(profile.username.as_deref())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Linux.do user")
        .trim()
        .chars()
        .take(120)
        .collect::<String>();
    let mut transaction = state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(map_database_error)?;
    let row = sqlx::query_as::<_, (String, String, String)>(
        "UPDATE users SET display_name = ?, last_login_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE oauth_issuer = ? AND oauth_subject = ? AND status = 'active' RETURNING id, display_name, status",
    )
    .bind(&display_name)
    .bind(&issuer)
    .bind(&subject)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(crate::management::map_database_error)?;
    let row = if let Some(row) = row {
        row
    } else {
        sqlx::query_as::<_, (String, String, String)>(
            "INSERT INTO users(id, display_name, oauth_issuer, oauth_subject, upstream_pool_id, last_login_at)
             SELECT ?, ?, ?, ?, p.id, strftime('%Y-%m-%dT%H:%M:%fZ', 'now') FROM upstream_pools p, gateway_settings s
             WHERE p.id = ? AND p.enabled = 1 AND s.id = 1 AND s.registration_open = 1
             ON CONFLICT(oauth_issuer, oauth_subject) DO UPDATE SET display_name = excluded.display_name,
             last_login_at = excluded.last_login_at, updated_at = excluded.last_login_at WHERE users.status = 'active'
             RETURNING id, display_name, status",
        )
        .bind(Uuid::new_v4().simple().to_string())
        .bind(display_name)
        .bind(issuer)
        .bind(subject)
        .bind(&state.config.default_upstream_pool_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(crate::management::map_database_error)?
        .ok_or_else(|| ApiError::new(ApiErrorCode::RegistrationClosed))?
    };
    let initial_token = ensure_initial_gateway_token(state, &mut transaction, &row.0).await?;
    transaction.commit().await.map_err(map_database_error)?;
    Ok((
        UserProfile {
            id: row.0,
            display_name: row.1,
            status: row.2,
        },
        initial_token,
    ))
}

async fn ensure_initial_gateway_token(
    state: &AppState,
    connection: &mut sqlx::SqliteConnection,
    user_id: &str,
) -> Result<Option<Zeroizing<String>>, ApiError> {
    let has_token = sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(SELECT 1 FROM gateway_tokens WHERE user_id = ?)",
    )
    .bind(user_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(map_database_error)?;
    if has_token != 0 {
        return Ok(None);
    }
    let issued = issue_gateway_token(state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let cipher = crate::credentials::CredentialCipher::new(state.config.encryption_key)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let encrypted = cipher
        .encrypt_gateway_token(&issued.id, &issued.raw_token)
        .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    let scopes = encode_scopes(None)?;
    sqlx::query(
        "INSERT INTO gateway_tokens(id, user_id, token_digest, prefix, name, scopes_json, token_ciphertext, token_nonce, token_encryption_version) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1)",
    )
    .bind(&issued.id.0)
    .bind(user_id)
    .bind(issued.digest.to_vec())
    .bind(&issued.prefix)
    .bind("Initial API key")
    .bind(scopes)
    .bind(encrypted.ciphertext)
    .bind(encrypted.nonce.to_vec())
    .execute(&mut *connection)
    .await
    .map_err(map_database_error)?;
    write_audit(
        &mut *connection,
        "oauth-auto-provision",
        "gateway_token.auto_issue",
        "gateway_token",
        &issued.id.0,
    )
    .await?;
    Ok(Some(Zeroizing::new(issued.raw_token)))
}

pub(crate) fn expiry_modifier(days: Option<u16>) -> Result<Option<String>, ApiError> {
    days.map(|days| {
        if (1..=365).contains(&days) {
            Ok(format!("+{days} days"))
        } else {
            Err(ApiError::new(ApiErrorCode::InvalidRequest))
        }
    })
    .transpose()
}

fn pkce_verifier(state: &AppState, raw_state: &str) -> Result<Zeroizing<String>, ApiError> {
    session::oauth_verifier(raw_state, state.config.token_pepper)
        .map(|digest| Zeroizing::new(URL_SAFE_NO_PAD.encode(digest)))
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))
}

async fn revoke_browser_session(
    db: &mut sqlx::SqliteConnection,
    state: &AppState,
    raw: &str,
) -> Result<(), ApiError> {
    let digest = session::token_digest(raw, state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    sqlx::query("UPDATE auth_sessions SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE session_digest = ? AND revoked_at IS NULL")
        .bind(digest.to_vec())
        .execute(db)
        .await
        .map_err(crate::management::map_database_error)?;
    Ok(())
}

fn user_cookie_name(state: &AppState) -> &'static str {
    if state.config.public_origin.scheme() == "https" {
        "__Host-c7g_session"
    } else {
        USER_SESSION_COOKIE
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_i64().map(|number| number.to_string()))
}

fn random_url_token(bytes: usize) -> Result<String, ApiError> {
    let mut random = vec![0_u8; bytes];
    getrandom::fill(&mut random).map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    Ok(URL_SAFE_NO_PAD.encode(random))
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

fn cookie_header(
    state: &AppState,
    name: &str,
    value: &str,
    max_age: i64,
) -> Result<HeaderValue, ApiError> {
    let secure = if state.config.public_origin.scheme() == "https" {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .map_err(|_| ApiError::new(ApiErrorCode::InternalError))
}

fn expire_cookie(state: &AppState, name: &str) -> Result<HeaderValue, ApiError> {
    cookie_header(state, name, "", 0)
}

fn initial_token_cookie_name(_state: &AppState) -> &'static str {
    INITIAL_TOKEN_COOKIE
}

fn initial_token_cookie(
    state: &AppState,
    value: &str,
    max_age: i64,
) -> Result<HeaderValue, ApiError> {
    let secure = if state.config.public_origin.scheme() == "https" {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{}={value}; Path=/api/user/initial-token; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}",
        initial_token_cookie_name(state)
    ))
    .map_err(|_| ApiError::new(ApiErrorCode::InternalError))
}

fn expire_initial_token_cookie(state: &AppState) -> Result<HeaderValue, ApiError> {
    initial_token_cookie(state, "", 0)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn clean_token_name(name: &str) -> Result<String, ApiError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 120 || trimmed.chars().any(char::is_control) {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    use axum::{
        Json, Router,
        body::Bytes,
        extract::State,
        http::{HeaderMap, HeaderValue, StatusCode, header},
        response::IntoResponse,
        routing::{get, post},
    };
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    use super::{
        CallbackQuery, CurrentSession, acknowledge_initial_token, callback, copy_token,
        initial_token, list_tokens, login, logout, me,
    };
    use crate::{
        config::{Config, Environment, LinuxDoOAuthConfig},
        management::{AppState, GatewayTokenScope, authenticate_gateway_token},
        storage,
    };

    async fn token_endpoint(
        State(expected_challenge): State<Arc<Mutex<Option<String>>>>,
        body: Bytes,
    ) -> (StatusCode, Json<Value>) {
        let form: HashMap<String, String> = url::form_urlencoded::parse(&body)
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let actual = form
            .get("code_verifier")
            .map(|verifier| URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())));
        let is_valid = actual == *expected_challenge.lock().expect("mock state lock");
        if is_valid && form.get("code").is_some_and(|code| code == "mock-code") {
            (
                StatusCode::OK,
                Json(json!({"access_token":"mock-oauth-access","token_type":"Bearer"})),
            )
        } else {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_grant"})),
            )
        }
    }

    async fn userinfo_endpoint() -> Json<Value> {
        Json(
            json!({"sub":"linuxdo-subject-1","username":"mock-user","name":"Mock User","active":true}),
        )
    }

    async fn test_state(token_url: url::Url, userinfo_url: url::Url) -> AppState {
        let pool = storage::connect(std::path::Path::new(":memory:"))
            .await
            .expect("test database connects");
        sqlx::query("INSERT INTO upstream_pools(id, name, authorization_scope) VALUES ('pool','pool','authorized')")
            .execute(&pool).await.expect("default upstream pool exists");
        sqlx::query("UPDATE gateway_settings SET registration_open = 1 WHERE id = 1")
            .execute(&pool)
            .await
            .expect("initial registration enabled for test");
        let public_origin = url::Url::parse("http://127.0.0.1:8787").expect("origin parses");
        let config = Config {
            bind_addr: "127.0.0.1:8787".parse().expect("socket address parses"),
            database_path: PathBuf::from(":memory:"),
            static_dir: PathBuf::from("web/dist"),
            public_origin: public_origin.clone(),
            context7_mcp_origin: url::Url::parse("https://mcp.context7.com")
                .expect("upstream MCP origin parses"),
            context7_rest_origin: url::Url::parse("https://context7.com")
                .expect("upstream REST origin parses"),
            environment: Environment::Test,
            encryption_key: Some([1; 32]),
            token_pepper: Some([2; 32]),
            admin_token_digest: None,
            admin_password: None,
            gateway_event_retention_days: 30,
            linuxdo_oauth: Some(LinuxDoOAuthConfig {
                client_id: "mock-client".to_owned(),
                client_secret: zeroize::Zeroizing::new("mock-secret".to_owned()),
                redirect_url: url::Url::parse("http://127.0.0.1:8787/auth/linuxdo/callback")
                    .expect("callback URL parses"),
                authorize_url: url::Url::parse("https://connect.linux.do/oauth2/authorize")
                    .expect("authorize URL parses"),
                token_url,
                userinfo_url,
                scope: "openid profile".to_owned(),
            }),
            default_upstream_pool_id: Some("pool".to_owned()),
        };
        AppState {
            config: Arc::new(config),
            pool,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("test HTTP client builds"),
        }
    }

    async fn start_login(
        state: &AppState,
        challenge_state: &Mutex<Option<String>>,
    ) -> (String, String) {
        let response = login(State(state.clone())).await.expect("login redirects");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .expect("authorize location")
            .to_str()
            .expect("location is text");
        let authorize = url::Url::parse(location).expect("authorize URL parses");
        let params: HashMap<String, String> = authorize
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let state_value = params.get("state").expect("state parameter exists").clone();
        let challenge = params
            .get("code_challenge")
            .expect("PKCE challenge exists")
            .clone();
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        *challenge_state.lock().expect("mock state lock") = Some(challenge);
        let cookie = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .find_map(|header| {
                header
                    .to_str()
                    .ok()?
                    .strip_prefix("c7g_oauth_state=")?
                    .split(';')
                    .next()
            })
            .expect("OAuth state cookie exists");
        assert_eq!(cookie, state_value);
        (state_value, cookie.to_owned())
    }

    async fn finish_login(
        state: &AppState,
        state_value: String,
        cookie: String,
        existing_session: Option<&str>,
    ) -> axum::response::Response {
        let mut headers = HeaderMap::new();
        let cookie = match existing_session {
            Some(session) => format!("c7g_oauth_state={cookie}; c7g_session={session}"),
            None => format!("c7g_oauth_state={cookie}"),
        };
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&cookie).expect("cookie header is valid"),
        );
        callback(
            State(state.clone()),
            headers,
            axum::extract::Query(CallbackQuery {
                code: Some("mock-code".to_owned()),
                state: Some(state_value),
                error: None,
            }),
        )
        .await
        .expect("OAuth callback succeeds")
    }

    fn session_cookie(response: &axum::response::Response) -> String {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .find_map(|header| {
                header
                    .to_str()
                    .ok()?
                    .strip_prefix("c7g_session=")?
                    .split(';')
                    .next()
                    .map(str::to_owned)
            })
            .expect("new user session cookie exists")
    }

    fn initial_token_cookie(response: &axum::response::Response) -> String {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .find_map(|header| {
                let value = header
                    .to_str()
                    .ok()?
                    .strip_prefix("c7g_initial_token=")?
                    .split(';')
                    .next()?;
                (!value.is_empty()).then(|| value.to_owned())
            })
            .expect("initial API key cookie exists")
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body reads")
            .to_bytes();
        serde_json::from_slice(&body).expect("response contains JSON")
    }

    #[tokio::test]
    async fn oauth_state_pkce_registration_gate_session_rotation_and_logout_work() {
        let expected_challenge = Arc::new(Mutex::new(None));
        let mock = Router::new()
            .route("/token", post(token_endpoint))
            .route("/userinfo", get(userinfo_endpoint))
            .with_state(expected_challenge.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock listener binds");
        let mock_origin = format!("http://{}", listener.local_addr().expect("mock address"));
        let server = tokio::spawn(async move {
            axum::serve(listener, mock)
                .await
                .expect("mock OAuth server serves");
        });
        let state = test_state(
            url::Url::parse(&format!("{mock_origin}/token")).expect("token URL parses"),
            url::Url::parse(&format!("{mock_origin}/userinfo")).expect("userinfo URL parses"),
        )
        .await;

        let (first_state, first_cookie) = start_login(&state, &expected_challenge).await;
        let first_response =
            finish_login(&state, first_state.clone(), first_cookie.clone(), None).await;
        assert_eq!(first_response.status(), StatusCode::SEE_OTHER);
        let first_session = session_cookie(&first_response);
        let current = me(State(state.clone()), {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::COOKIE,
                HeaderValue::from_str(&format!("c7g_session={first_session}"))
                    .expect("session cookie header"),
            );
            headers
        })
        .await
        .expect("new browser session authenticates");
        let CurrentSession { user } = current.0;
        assert_eq!(user.display_name, "Mock User");
        assert_eq!(user.status, "active");
        let initial_token_value = initial_token_cookie(&first_response);
        assert!(initial_token_value.starts_with("c7g_"));

        let mut cookie_headers = HeaderMap::new();
        cookie_headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("c7g_session={first_session}")).expect("session cookie"),
        );
        let listed = list_tokens(State(state.clone()), cookie_headers.clone())
            .await
            .expect("tokens list")
            .0;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Initial API key");
        assert!(listed[0].can_copy);
        assert_eq!(
            listed[0].scopes,
            vec![GatewayTokenScope::Mcp, GatewayTokenScope::ContextRead]
        );
        assert!(
            listed[0].token.is_none(),
            "list must not return the raw secret"
        );
        let copied = copy_token(
            State(state.clone()),
            axum::extract::Path(listed[0].id.clone()),
            cookie_headers.clone(),
        )
        .await
        .expect("encrypted Gateway token can be copied");
        assert_eq!(copied.0.token, initial_token_value);
        let initial_headers = HeaderMap::from_iter([(
            header::COOKIE,
            HeaderValue::from_str(&format!(
                "c7g_session={first_session}; c7g_initial_token={initial_token_value}"
            ))
            .expect("initial token cookie header"),
        )]);
        let issued = initial_token(State(state.clone()), initial_headers.clone())
            .await
            .expect("initial API key can be retrieved");
        assert_eq!(issued.status(), StatusCode::OK);
        let issued = body_json(issued).await;
        assert_eq!(issued["token"], initial_token_value);
        assert_eq!(issued["name"], "Initial API key");
        let acknowledged = acknowledge_initial_token(State(state.clone()), initial_headers)
            .await
            .expect("initial API key acknowledgement succeeds");
        assert_eq!(acknowledged.0, StatusCode::NO_CONTENT);
        assert!(
            acknowledged
                .1
                .get_all(header::SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .any(
                    |value| value.starts_with("c7g_initial_token=;") && value.contains("Max-Age=0")
                )
        );
        let bearer = |token: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}")).expect("bearer header"),
            );
            headers
        };
        assert!(
            authenticate_gateway_token(
                &bearer(&initial_token_value),
                &state,
                GatewayTokenScope::Mcp
            )
            .await
            .is_ok()
        );
        assert!(
            authenticate_gateway_token(
                &bearer(&initial_token_value),
                &state,
                GatewayTokenScope::ContextRead
            )
            .await
            .is_ok()
        );

        sqlx::query("UPDATE gateway_settings SET registration_open = 0 WHERE id = 1")
            .execute(&state.pool)
            .await
            .expect("registration closes after initial user");
        let (second_state, second_cookie) = start_login(&state, &expected_challenge).await;
        let second_response =
            finish_login(&state, second_state, second_cookie, Some(&first_session)).await;
        let second_session = session_cookie(&second_response);
        assert_ne!(first_session, second_session);
        let old_headers = {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::COOKIE,
                HeaderValue::from_str(&format!("c7g_session={first_session}"))
                    .expect("old cookie header"),
            );
            headers
        };
        assert_eq!(
            me(State(state.clone()), old_headers)
                .await
                .expect_err("login rotates old session")
                .into_response()
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let mut logout_headers = HeaderMap::new();
        logout_headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("c7g_session={second_session}"))
                .expect("session cookie header"),
        );
        let logged_out = logout(State(state.clone()), logout_headers.clone())
            .await
            .expect("logout is idempotent");
        assert_eq!(logged_out.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            me(State(state.clone()), logout_headers)
                .await
                .expect_err("logout revokes session")
                .into_response()
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let mut replay_headers = HeaderMap::new();
        replay_headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("c7g_oauth_state={first_cookie}"))
                .expect("replay state cookie"),
        );
        let replay = callback(
            State(state.clone()),
            replay_headers,
            axum::extract::Query(CallbackQuery {
                code: Some("mock-code".to_owned()),
                state: Some(first_state),
                error: None,
            }),
        )
        .await;
        assert!(replay.is_err(), "single-use OAuth state cannot be replayed");
        server.abort();
    }
}
