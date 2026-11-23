use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{AUTHORIZATION, RETRY_AFTER},
    },
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{
    config::Config,
    credentials::{
        CredentialCipher, GatewayTokenId, IssuedGatewayToken, PoolId, UpstreamKeyId,
        issue_gateway_token, parse_gateway_token, verify_gateway_token,
    },
};

#[derive(Clone)]
pub struct AppState {
    pub config: std::sync::Arc<Config>,
    pub pool: SqlitePool,
    pub http: reqwest::Client,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    error: ApiErrorCode,
    #[serde(skip)]
    retry_after_seconds: Option<i64>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    Unauthorized,
    InvalidAdminCredentials,
    InvalidGatewayKey,
    TokenDisabled,
    RegistrationClosed,
    QuotaExceeded,
    Forbidden,
    NotFound,
    InvalidSession,
    Conflict,
    MethodNotAllowed,
    InvalidRequest,
    PayloadTooLarge,
    NotConfigured,
    PoolUnavailable,
    CredentialUnavailable,
    UpstreamRateLimited,
    RateLimited,
    UpstreamUnavailable,
    InternalError,
}

impl ApiError {
    pub fn new(error: ApiErrorCode) -> Self {
        Self {
            error,
            retry_after_seconds: None,
        }
    }

    pub fn retry_after(mut self, seconds: i64) -> Self {
        self.retry_after_seconds = Some(seconds);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.error {
            ApiErrorCode::Unauthorized
            | ApiErrorCode::InvalidAdminCredentials
            | ApiErrorCode::InvalidGatewayKey
            | ApiErrorCode::TokenDisabled => StatusCode::UNAUTHORIZED,
            ApiErrorCode::Forbidden | ApiErrorCode::RegistrationClosed => StatusCode::FORBIDDEN,
            ApiErrorCode::NotFound | ApiErrorCode::InvalidSession => StatusCode::NOT_FOUND,
            ApiErrorCode::InvalidRequest => StatusCode::BAD_REQUEST,
            ApiErrorCode::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ApiErrorCode::Conflict => StatusCode::CONFLICT,
            ApiErrorCode::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            ApiErrorCode::NotConfigured
            | ApiErrorCode::PoolUnavailable
            | ApiErrorCode::CredentialUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ApiErrorCode::UpstreamRateLimited
            | ApiErrorCode::RateLimited
            | ApiErrorCode::QuotaExceeded => StatusCode::TOO_MANY_REQUESTS,
            ApiErrorCode::UpstreamUnavailable => StatusCode::BAD_GATEWAY,
            ApiErrorCode::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let retry_after = self.retry_after_seconds;
        let challenge = matches!(
            self.error,
            ApiErrorCode::InvalidGatewayKey | ApiErrorCode::TokenDisabled
        );
        let mut response = (status, Json(self)).into_response();
        if let Some(value) =
            retry_after.and_then(|seconds| HeaderValue::from_str(&seconds.to_string()).ok())
        {
            response.headers_mut().insert(RETRY_AFTER, value);
        }
        if challenge {
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"context7-gateway\""),
            );
        }
        response
    }
}

#[derive(Debug, Deserialize)]
pub struct CreatePoolRequest {
    pub name: String,
    pub authorization_scope: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateKeyRequest {
    pub api_key: String,
}

#[derive(Debug, Deserialize)]
pub struct BulkCreateKeysRequest {
    pub api_keys: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct BulkCreateKeysResponse {
    pub created: usize,
    pub duplicates: usize,
    pub invalid: usize,
    pub results: Vec<BulkKeyImportResult>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BulkKeyImportStatus {
    Created,
    Duplicate,
    Invalid,
}

#[derive(Debug, Serialize)]
pub struct BulkKeyImportResult {
    pub index: usize,
    pub status: BulkKeyImportStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_prefix: Option<String>,
}

const MAX_BULK_UPSTREAM_KEYS: usize = 100;

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub display_name: String,
    pub upstream_pool_id: String,
    pub oauth_subject: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateGatewayTokenRequest {
    pub name: String,
    pub expires_in_days: Option<u16>,
    pub scopes: Option<Vec<GatewayTokenScope>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum GatewayTokenScope {
    #[serde(rename = "mcp")]
    Mcp,
    #[serde(rename = "context7:read")]
    ContextRead,
}

pub(crate) fn encode_scopes(scopes: Option<Vec<GatewayTokenScope>>) -> Result<String, ApiError> {
    let defaults = [GatewayTokenScope::Mcp, GatewayTokenScope::ContextRead];
    let scopes = scopes.as_deref().unwrap_or(&defaults);
    if scopes.is_empty()
        || scopes.len() > defaults.len()
        || (scopes.len() == 2 && scopes[0] == scopes[1])
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    serde_json::to_string(scopes).map_err(|_| ApiError::new(ApiErrorCode::InternalError))
}

#[derive(Debug, Serialize)]
pub struct UpstreamKeyValidation {
    pub status: String,
    pub upstream_status: u16,
    pub checked_at: String,
}

#[derive(Debug, Serialize)]
pub struct PoolSummary {
    pub id: String,
    pub name: String,
    pub authorization_scope: String,
    pub enabled: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct UpstreamKeySummary {
    pub id: String,
    pub pool_id: String,
    pub display_prefix: String,
    pub status: String,
    pub last_checked_at: Option<String>,
    pub last_used_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct UserSummary {
    pub id: String,
    pub display_name: String,
    pub status: String,
    pub created_at: String,
    pub last_login_at: Option<String>,
    pub upstream_pool_id: Option<String>,
    pub trust_level: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct GatewayTokenSummary {
    pub id: String,
    pub prefix: String,
    pub name: String,
    pub status: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub scopes: Vec<GatewayTokenScope>,
}

#[derive(Serialize)]
pub struct CreatedGatewayToken {
    id: String,
    name: String,
    prefix: String,
    token: String,
    created_at: String,
    expires_at: Option<String>,
    scopes: Vec<GatewayTokenScope>,
}

pub async fn create_pool(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreatePoolRequest>,
) -> Result<(StatusCode, Json<PoolSummary>), ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let name = clean_name(&request.name)?;
    let authorization_scope = clean_scope(&request.authorization_scope)?;
    let id = Uuid::new_v4().simple().to_string();
    let record = sqlx::query_as::<_, (String, String, String, i64, String)>(
        "INSERT INTO upstream_pools(id, name, authorization_scope) VALUES (?, ?, ?) RETURNING id, name, authorization_scope, enabled, created_at",
    )
    .bind(&id)
    .bind(&name)
    .bind(&authorization_scope)
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    write_audit(&state.pool, &actor, "pool.create", "upstream_pool", &id).await?;
    Ok((StatusCode::CREATED, Json(PoolSummary::from(record))))
}

pub async fn list_pools(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PoolSummary>>, ApiError> {
    authorize_admin(&state, &headers).await?;
    let records = sqlx::query_as::<_, (String, String, String, i64, String)>(
        "SELECT id, name, authorization_scope, enabled, created_at FROM upstream_pools ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(records.into_iter().map(PoolSummary::from).collect()))
}

pub async fn create_upstream_key(
    State(state): State<AppState>,
    Path(pool_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<CreateKeyRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<UpstreamKeySummary>), ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let Json(request) = payload.map_err(map_key_json_rejection)?;
    let pool_id = PoolId(pool_id);
    let exists =
        sqlx::query_scalar::<_, i64>("SELECT 1 FROM upstream_pools WHERE id = ? AND enabled = 1")
            .bind(&pool_id.0)
            .fetch_optional(&state.pool)
            .await
            .map_err(map_database_error)?;
    if exists.is_none() {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    let cipher = CredentialCipher::new(state.config.encryption_key)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let id = UpstreamKeyId(Uuid::new_v4().simple().to_string());
    let api_key = zeroize::Zeroizing::new(request.api_key);
    let api_key = valid_upstream_key(api_key.as_str())
        .ok_or_else(|| ApiError::new(ApiErrorCode::InvalidRequest))?;
    let encrypted = cipher
        .encrypt(&id, &pool_id, api_key)
        .map_err(|_| ApiError::new(ApiErrorCode::InvalidRequest))?;
    let fingerprint = crate::session::upstream_fingerprint(api_key, state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let display_prefix = format!("ctx7_{}…", &id.0[..4]);
    let record = sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<String>, String)>(
        "INSERT INTO upstream_keys(id, pool_id, ciphertext, nonce, display_prefix, credential_fingerprint) VALUES (?, ?, ?, ?, ?, ?) RETURNING id, pool_id, display_prefix, status, last_checked_at, last_used_at, created_at",
    )
    .bind(&id.0)
    .bind(&pool_id.0)
    .bind(encrypted.ciphertext)
    .bind(encrypted.nonce.to_vec())
    .bind(display_prefix)
    .bind(fingerprint.to_vec())
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    write_audit(
        &state.pool,
        &actor,
        "upstream_key.create",
        "upstream_key",
        &id.0,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(UpstreamKeySummary::from(record))))
}

pub async fn bulk_create_upstream_keys(
    State(state): State<AppState>,
    Path(pool_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<BulkCreateKeysRequest>, JsonRejection>,
) -> Result<Json<BulkCreateKeysResponse>, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let Json(request) = payload.map_err(map_key_json_rejection)?;
    if request.api_keys.is_empty() || request.api_keys.len() > MAX_BULK_UPSTREAM_KEYS {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let requested = request.api_keys.len();
    let keys: Vec<_> = request
        .api_keys
        .into_iter()
        .map(zeroize::Zeroizing::new)
        .collect();
    let pool_id = PoolId(pool_id);
    let exists =
        sqlx::query_scalar::<_, i64>("SELECT 1 FROM upstream_pools WHERE id = ? AND enabled = 1")
            .bind(&pool_id.0)
            .fetch_optional(&state.pool)
            .await
            .map_err(map_database_error)?;
    if exists.is_none() {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    let cipher = CredentialCipher::new(state.config.encryption_key)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let mut tx = state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(map_database_error)?;
    let mut created = 0usize;
    let mut duplicates = 0usize;
    let mut invalid = 0usize;
    let mut results = Vec::with_capacity(requested);
    let mut seen = std::collections::HashSet::with_capacity(requested);

    for (index, api_key) in keys.into_iter().enumerate() {
        let Some(api_key) = valid_upstream_key(api_key.as_str()) else {
            invalid += 1;
            results.push(BulkKeyImportResult {
                index,
                status: BulkKeyImportStatus::Invalid,
                key_id: None,
                display_prefix: None,
            });
            continue;
        };
        let fingerprint = crate::session::upstream_fingerprint(api_key, state.config.token_pepper)
            .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
        if !seen.insert(fingerprint) {
            duplicates += 1;
            results.push(BulkKeyImportResult {
                index,
                status: BulkKeyImportStatus::Duplicate,
                key_id: None,
                display_prefix: None,
            });
            continue;
        }

        let id = UpstreamKeyId(Uuid::new_v4().simple().to_string());
        let encrypted = cipher
            .encrypt(&id, &pool_id, api_key)
            .map_err(|_| ApiError::new(ApiErrorCode::InvalidRequest))?;
        let display_prefix = format!("ctx7_{}…", &id.0[..4]);
        let record = sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<String>, String)>(
            "INSERT INTO upstream_keys(id, pool_id, ciphertext, nonce, display_prefix, credential_fingerprint) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(credential_fingerprint) WHERE deleted_at IS NULL DO NOTHING RETURNING id, pool_id, display_prefix, status, last_checked_at, last_used_at, created_at",
        )
        .bind(&id.0)
        .bind(&pool_id.0)
        .bind(encrypted.ciphertext)
        .bind(encrypted.nonce.to_vec())
        .bind(&display_prefix)
        .bind(fingerprint.to_vec())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_database_error)?;
        let Some(record) = record else {
            duplicates += 1;
            results.push(BulkKeyImportResult {
                index,
                status: BulkKeyImportStatus::Duplicate,
                key_id: None,
                display_prefix: None,
            });
            continue;
        };

        created += 1;
        let summary = UpstreamKeySummary::from(record);
        results.push(BulkKeyImportResult {
            index,
            status: BulkKeyImportStatus::Created,
            key_id: Some(summary.id.clone()),
            display_prefix: Some(summary.display_prefix),
        });
        write_audit(
            &mut *tx,
            &actor,
            "upstream_key.create",
            "upstream_key",
            &summary.id,
        )
        .await?;
    }

    let details = serde_json::json!({
        "requested": requested,
        "created": created,
        "duplicates": duplicates,
        "invalid": invalid,
    })
    .to_string();
    sqlx::query(
        "INSERT INTO admin_audit_events(id, actor, action, object_type, object_id, details_json) VALUES (?, ?, 'upstream_key.bulk_import', 'upstream_pool', ?, ?)",
    )
    .bind(Uuid::new_v4().simple().to_string())
    .bind(&actor)
    .bind(&pool_id.0)
    .bind(details)
    .execute(&mut *tx)
    .await
    .map_err(map_database_error)?;
    tx.commit().await.map_err(map_database_error)?;

    Ok(Json(BulkCreateKeysResponse {
        created,
        duplicates,
        invalid,
        results,
    }))
}

pub async fn list_upstream_keys(
    State(state): State<AppState>,
    Path(pool_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<UpstreamKeySummary>>, ApiError> {
    authorize_admin(&state, &headers).await?;
    let records = sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<String>, String)>(
        "SELECT id, pool_id, display_prefix, status, last_checked_at, last_used_at, created_at FROM upstream_keys WHERE pool_id = ? AND deleted_at IS NULL ORDER BY created_at DESC",
    )
    .bind(pool_id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(
        records.into_iter().map(UpstreamKeySummary::from).collect(),
    ))
}

pub async fn set_upstream_key_status(
    State(state): State<AppState>,
    Path((pool_id, key_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<SetStatusRequest>,
) -> Result<StatusCode, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let status = request.status.as_str();
    if !matches!(status, "active" | "disabled") {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let result = sqlx::query("UPDATE upstream_keys SET status = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ? AND pool_id = ? AND deleted_at IS NULL AND (? = 'disabled' OR (status IN ('active','disabled') AND last_success_at IS NOT NULL AND (last_failure_at IS NULL OR last_success_at > last_failure_at) AND (rate_limited_until IS NULL OR unixepoch(rate_limited_until) <= unixepoch())))")
        .bind(status)
        .bind(&key_id)
        .bind(&pool_id)
        .bind(status)
        .execute(&state.pool)
        .await
        .map_err(map_database_error)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::new(ApiErrorCode::Conflict));
    }
    write_audit(
        &state.pool,
        &actor,
        "upstream_key.status",
        "upstream_key",
        &key_id,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_upstream_key(
    State(state): State<AppState>,
    Path((pool_id, key_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let mut tx = state.pool.begin().await.map_err(map_database_error)?;
    let result = sqlx::query("UPDATE upstream_keys SET status = 'disabled', deleted_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'), ciphertext = X'', nonce = zeroblob(12) WHERE id = ? AND pool_id = ? AND deleted_at IS NULL")
        .bind(&key_id).bind(pool_id).execute(&mut *tx).await.map_err(map_database_error)?;
    if result.rows_affected() != 1 {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    write_audit(
        &mut *tx,
        &actor,
        "upstream_key.delete",
        "upstream_key",
        &key_id,
    )
    .await?;
    tx.commit().await.map_err(map_database_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn validate_upstream_key(
    State(state): State<AppState>,
    Path((pool_id, key_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<UpstreamKeyValidation>, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let (api_key, _) =
        crate::operations::load_upstream_credential(&state, &pool_id, &key_id).await?;
    let response = crate::operations::validate_read_key(&state, &api_key).await?;
    let upstream_status = response.status();
    let status = match upstream_status.as_u16() {
        200..=299 => "active",
        401 => "invalid",
        403 => "permission_error",
        429 => "rate_limited",
        _ => "pending",
    };
    let mut tx = state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(map_database_error)?;
    let retry = crate::proxy::retry_after_seconds(response.headers());
    let checked_at = sqlx::query_scalar::<_, String>(
        "UPDATE upstream_keys SET status = ?, last_checked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), last_success_at = CASE WHEN ? = 'active' THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now') ELSE last_success_at END, last_failure_at = CASE WHEN ? = 'active' THEN last_failure_at ELSE strftime('%Y-%m-%dT%H:%M:%fZ', 'now') END, rate_limited_until = CASE WHEN ? = 'rate_limited' THEN datetime('now', '+' || ? || ' seconds') ELSE NULL END WHERE id = ? AND pool_id = ? AND deleted_at IS NULL RETURNING last_checked_at",
    ).bind(status).bind(status).bind(status).bind(status).bind(retry).bind(&key_id).bind(&pool_id)
    .fetch_one(&mut *tx).await.map_err(map_database_error)?;
    if upstream_status.as_u16() == 429 {
        sqlx::query("UPDATE upstream_pools SET rate_limited_until = max(COALESCE(rate_limited_until, ''), datetime('now', '+' || ? || ' seconds')) WHERE id = ?")
            .bind(retry).bind(&pool_id).execute(&mut *tx).await.map_err(map_database_error)?;
    }
    write_audit(
        &mut *tx,
        &actor,
        "upstream_key.validate",
        "upstream_key",
        &key_id,
    )
    .await?;
    tx.commit().await.map_err(map_database_error)?;
    Ok(Json(UpstreamKeyValidation {
        status: status.to_owned(),
        upstream_status: upstream_status.as_u16(),
        checked_at,
    }))
}

pub async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserSummary>), ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let display_name = clean_name(&request.display_name)?;
    let identity = request
        .oauth_subject
        .as_deref()
        .map(|subject| {
            let subject = subject.trim();
            let oauth = state
                .config
                .linuxdo_oauth
                .as_ref()
                .ok_or_else(|| ApiError::new(ApiErrorCode::NotConfigured))?;
            if subject.is_empty()
                || subject.len() > 128
                || subject.chars().any(|c| c.is_control() || c.is_whitespace())
            {
                return Err(ApiError::new(ApiErrorCode::InvalidRequest));
            }
            Ok((oauth.userinfo_url.origin().ascii_serialization(), subject))
        })
        .transpose()?;
    let pool_exists =
        sqlx::query_scalar::<_, i64>("SELECT 1 FROM upstream_pools WHERE id = ? AND enabled = 1")
            .bind(&request.upstream_pool_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(map_database_error)?;
    if pool_exists.is_none() {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    let id = Uuid::new_v4().simple().to_string();
    let record = sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<String>, Option<i64>)>(
        "INSERT INTO users(id, display_name, upstream_pool_id, oauth_issuer, oauth_subject) VALUES (?, ?, ?, ?, ?) RETURNING id, display_name, status, created_at, last_login_at, upstream_pool_id, trust_level",
    )
    .bind(&id)
    .bind(display_name)
    .bind(&request.upstream_pool_id)
    .bind(identity.as_ref().map(|(issuer, _)| issuer.as_str()))
    .bind(identity.as_ref().map(|(_, subject)| *subject))
    .fetch_one(&state.pool)
    .await
    .map_err(map_database_error)?;
    write_audit(&state.pool, &actor, "user.create", "user", &id).await?;
    Ok((StatusCode::CREATED, Json(UserSummary::from(record))))
}

pub async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<UserSummary>>, ApiError> {
    authorize_admin(&state, &headers).await?;
    let records = sqlx::query_as::<_, (String, String, String, String, Option<String>, Option<String>, Option<i64>)>(
        "SELECT id, display_name, status, created_at, last_login_at, upstream_pool_id, trust_level FROM users ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(records.into_iter().map(UserSummary::from).collect()))
}

#[derive(Deserialize)]
pub struct PatchUserRequest {
    status: Option<String>,
    upstream_pool_id: Option<String>,
}

pub async fn set_user_status(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<PatchUserRequest>,
) -> Result<StatusCode, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let status = request.status.as_deref();
    if status.is_some_and(|status| !matches!(status, "active" | "disabled"))
        || (status.is_none() && request.upstream_pool_id.is_none())
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let mut tx = state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(map_database_error)?;
    if let Some(pool_id) = &request.upstream_pool_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM upstream_pools WHERE id = ? AND enabled = 1)",
        )
        .bind(pool_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_database_error)?;
        if !exists {
            return Err(ApiError::new(ApiErrorCode::NotFound));
        }
    }
    let result = sqlx::query("UPDATE users SET status = COALESCE(?, status), upstream_pool_id = COALESCE(?, upstream_pool_id), updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?")
        .bind(status)
        .bind(&request.upstream_pool_id)
        .bind(&user_id)
        .execute(&mut *tx)
        .await
        .map_err(map_database_error)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    if status == Some("disabled") {
        sqlx::query("UPDATE auth_sessions SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE user_id = ? AND revoked_at IS NULL")
            .bind(&user_id).execute(&mut *tx).await.map_err(map_database_error)?;
    }
    if status == Some("disabled") || request.upstream_pool_id.is_some() {
        sqlx::query("UPDATE mcp_sessions SET closed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE closed_at IS NULL AND gateway_token_id IN (SELECT id FROM gateway_tokens WHERE user_id = ?)")
            .bind(&user_id).execute(&mut *tx).await.map_err(map_database_error)?;
    }
    write_audit(&mut *tx, &actor, "user.update", "user", &user_id).await?;
    tx.commit().await.map_err(map_database_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_gateway_token(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CreateGatewayTokenRequest>,
) -> Result<(StatusCode, Json<CreatedGatewayToken>), ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let name = clean_name(&request.name)?;
    let expires = crate::oauth::expiry_modifier(request.expires_in_days)?;
    let scopes = encode_scopes(request.scopes)?;
    let user_exists =
        sqlx::query_scalar::<_, i64>("SELECT 1 FROM users WHERE id = ? AND status = 'active'")
            .bind(&user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(map_database_error)?;
    if user_exists.is_none() {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    let issued = issue_gateway_token(state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let cipher = CredentialCipher::new(state.config.encryption_key)
        .map_err(|_| ApiError::new(ApiErrorCode::NotConfigured))?;
    let token = insert_gateway_token(
        &state.pool,
        &user_id,
        &name,
        issued,
        expires.as_deref(),
        &scopes,
        &cipher,
    )
    .await?;
    write_audit(
        &state.pool,
        &actor,
        "gateway_token.create",
        "gateway_token",
        &token.id,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(token)))
}

pub async fn list_gateway_tokens(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<GatewayTokenSummary>>, ApiError> {
    authorize_admin(&state, &headers).await?;
    let records = sqlx::query_as::<_, (String, String, String, String, String, Option<String>, Option<String>, String)>(
        "SELECT t.id, t.prefix, t.name, CASE WHEN t.status = 'active' AND unixepoch(t.expires_at) <= unixepoch() THEN 'expired' ELSE t.status END, t.created_at, t.last_used_at, t.expires_at, t.scopes_json FROM gateway_tokens t WHERE t.user_id = ? ORDER BY t.created_at DESC",
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(map_database_error)?;
    Ok(Json(
        records.into_iter().map(GatewayTokenSummary::from).collect(),
    ))
}

pub async fn revoke_gateway_token(
    State(state): State<AppState>,
    Path((user_id, token_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let result = sqlx::query("UPDATE gateway_tokens SET status = 'revoked', revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ? AND user_id = ? AND status = 'active'")
        .bind(&token_id)
        .bind(&user_id)
        .execute(&state.pool)
        .await
        .map_err(map_database_error)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    write_audit(
        &state.pool,
        &actor,
        "gateway_token.revoke",
        "gateway_token",
        &token_id,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn authenticate_gateway_token(
    headers: &HeaderMap,
    state: &AppState,
    scope: GatewayTokenScope,
) -> Result<(GatewayTokenId, String, String), ApiError> {
    let invalid_key = || ApiError::new(ApiErrorCode::InvalidGatewayKey);
    let raw_token = bearer_token(headers).ok_or_else(invalid_key)?;
    let token_id = parse_gateway_token(raw_token).map_err(|_| invalid_key())?;
    let record = sqlx::query_as::<_, (Vec<u8>, String, bool, String)>(
        "SELECT t.token_digest, t.user_id, (u.status = 'active' AND t.status = 'active' AND (t.expires_at IS NULL OR COALESCE(unixepoch(t.expires_at) > unixepoch(), 0))), t.scopes_json FROM gateway_tokens t JOIN users u ON u.id = t.user_id WHERE t.id = ?",
    )
    .bind(&token_id.0)
    .fetch_optional(&state.pool)
    .await
    .map_err(map_database_error)?
    .ok_or_else(invalid_key)?;
    if !verify_gateway_token(raw_token, &record.0, state.config.token_pepper)
        .map_err(|_| invalid_key())?
    {
        return Err(invalid_key());
    }
    if !record.2 {
        return Err(ApiError::new(ApiErrorCode::TokenDisabled));
    }
    let scopes: Vec<GatewayTokenScope> =
        serde_json::from_str(&record.3).map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    if !scopes.contains(&scope) {
        return Err(ApiError::new(ApiErrorCode::Forbidden));
    }
    if sqlx::query("UPDATE gateway_tokens SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ? AND status = 'active' AND EXISTS (SELECT 1 FROM users WHERE id = gateway_tokens.user_id AND status = 'active')")
        .bind(&token_id.0)
        .execute(&state.pool)
        .await
        .map_err(map_database_error)?.rows_affected() != 1
    { return Err(ApiError::new(ApiErrorCode::TokenDisabled)); }
    Ok((token_id, record.1, "active".to_owned()))
}

#[derive(Debug, Deserialize)]
pub struct SetStatusRequest {
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct SetPoolEnabledRequest {
    pub enabled: bool,
}

pub async fn set_pool_enabled(
    State(state): State<AppState>,
    Path(pool_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SetPoolEnabledRequest>,
) -> Result<StatusCode, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let result = sqlx::query("UPDATE upstream_pools SET enabled = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?")
        .bind(i64::from(request.enabled))
        .bind(&pool_id)
        .execute(&state.pool)
        .await
        .map_err(map_database_error)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    write_audit(
        &state.pool,
        &actor,
        if request.enabled {
            "pool.enable"
        } else {
            "pool.disable"
        },
        "upstream_pool",
        &pool_id,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

impl From<(String, String, String, i64, String)> for PoolSummary {
    fn from(row: (String, String, String, i64, String)) -> Self {
        Self {
            id: row.0,
            name: row.1,
            authorization_scope: row.2,
            enabled: row.3 == 1,
            created_at: row.4,
        }
    }
}

impl
    From<(
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    )> for UpstreamKeySummary
{
    fn from(
        row: (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        ),
    ) -> Self {
        Self {
            id: row.0,
            pool_id: row.1,
            display_prefix: row.2,
            status: row.3,
            last_checked_at: row.4,
            last_used_at: row.5,
            created_at: row.6,
        }
    }
}

impl
    From<(
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
    )> for UserSummary
{
    fn from(
        row: (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i64>,
        ),
    ) -> Self {
        Self {
            id: row.0,
            display_name: row.1,
            status: row.2,
            created_at: row.3,
            last_login_at: row.4,
            upstream_pool_id: row.5,
            trust_level: row.6,
        }
    }
}

impl
    From<(
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    )> for GatewayTokenSummary
{
    fn from(
        row: (
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        ),
    ) -> Self {
        Self {
            id: row.0,
            prefix: row.1,
            name: row.2,
            status: row.3,
            created_at: row.4,
            last_used_at: row.5,
            expires_at: row.6,
            scopes: serde_json::from_str(&row.7).unwrap_or_default(),
        }
    }
}

async fn insert_gateway_token(
    pool: &SqlitePool,
    user_id: &str,
    name: &str,
    issued: IssuedGatewayToken,
    expires: Option<&str>,
    scopes: &str,
    cipher: &CredentialCipher,
) -> Result<CreatedGatewayToken, ApiError> {
    let encrypted = cipher
        .encrypt_gateway_token(&issued.id, &issued.raw_token)
        .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    let record = sqlx::query_as::<_, (String, String, Option<String>)>(
        "INSERT INTO gateway_tokens(id, user_id, token_digest, prefix, name, expires_at, scopes_json, token_ciphertext, token_nonce, token_encryption_version) VALUES (?, ?, ?, ?, ?, CASE WHEN ? IS NULL THEN NULL ELSE datetime('now', ?) END, ?, ?, ?, 1) RETURNING id, created_at, expires_at",
    )
    .bind(&issued.id.0)
    .bind(user_id)
    .bind(issued.digest.to_vec())
    .bind(&issued.prefix)
    .bind(name)
    .bind(expires)
    .bind(expires)
    .bind(scopes)
    .bind(encrypted.ciphertext)
    .bind(encrypted.nonce.to_vec())
    .fetch_one(pool)
    .await
    .map_err(map_database_error)?;
    Ok(CreatedGatewayToken {
        id: record.0,
        name: name.to_owned(),
        prefix: issued.prefix,
        token: issued.raw_token,
        created_at: record.1,
        expires_at: record.2,
        scopes: serde_json::from_str(scopes)
            .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?,
    })
}

pub(crate) use crate::admin_auth::authorize_admin;

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let (scheme, token) = values.next()?.to_str().ok()?.split_once(' ')?;
    (values.next().is_none() && scheme.eq_ignore_ascii_case("bearer") && !token.is_empty())
        .then_some(token)
}

fn clean_name(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 120 || value.chars().any(char::is_control) {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    Ok(value.to_owned())
}

fn clean_scope(value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 240 || value.chars().any(char::is_control) {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    Ok(value.to_owned())
}

fn valid_upstream_key(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= 256
        && value.starts_with("ctx7sk-")
        && value.bytes().all(|c| c.is_ascii_graphic()))
    .then_some(value)
}

fn map_key_json_rejection(rejection: JsonRejection) -> ApiError {
    if let JsonRejection::BytesRejection(rejection) = rejection
        && rejection.into_response().status() == StatusCode::PAYLOAD_TOO_LARGE
    {
        return ApiError::new(ApiErrorCode::PayloadTooLarge);
    }
    ApiError::new(ApiErrorCode::InvalidRequest)
}

pub(crate) async fn write_audit<'a>(
    executor: impl sqlx::Executor<'a, Database = sqlx::Sqlite>,
    actor: &str,
    action: &str,
    object_type: &str,
    object_id: &str,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO admin_audit_events(id, actor, action, object_type, object_id) VALUES (?, ?, ?, ?, ?)")
        .bind(Uuid::new_v4().simple().to_string())
        .bind(actor)
        .bind(action)
        .bind(object_type)
        .bind(object_id)
        .execute(executor)
        .await
        .map_err(map_database_error)?;
    Ok(())
}

pub(crate) fn map_database_error(error: sqlx::Error) -> ApiError {
    match error {
        sqlx::Error::Database(database_error) if database_error.is_unique_violation() => {
            ApiError::new(ApiErrorCode::Conflict)
        }
        sqlx::Error::RowNotFound => ApiError::new(ApiErrorCode::NotFound),
        error => {
            tracing::error!(%error, "database operation failed");
            ApiError::new(ApiErrorCode::InternalError)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{
        AppState, BulkCreateKeysRequest, BulkKeyImportStatus, CreateGatewayTokenRequest,
        CreateKeyRequest, CreatePoolRequest, CreateUserRequest, authenticate_gateway_token,
        bulk_create_upstream_keys, create_gateway_token, create_pool, create_upstream_key,
        create_user, list_gateway_tokens, list_pools, list_upstream_keys, revoke_gateway_token,
    };
    use crate::{
        config::{Config, Environment},
        storage,
    };
    use axum::{
        Json,
        extract::State,
        http::{HeaderMap, HeaderValue, StatusCode, header::AUTHORIZATION},
    };
    use sha2::Digest;
    use sqlx::{Row, SqlitePool};
    use std::{path::PathBuf, sync::Arc};

    async fn test_state(admin_token: Option<&str>) -> AppState {
        let pool = storage::connect(std::path::Path::new(":memory:"))
            .await
            .expect("test database connects");
        AppState {
            config: Arc::new(Config {
                bind_addr: "127.0.0.1:8787".parse().expect("socket address parses"),
                database_path: PathBuf::from(":memory:"),
                static_dir: PathBuf::from("web/dist"),
                public_origin: url::Url::parse("http://127.0.0.1:8787").expect("URL parses"),
                context7_mcp_origin: url::Url::parse("https://mcp.context7.com")
                    .expect("upstream MCP origin parses"),
                context7_rest_origin: url::Url::parse("https://context7.com")
                    .expect("upstream REST origin parses"),
                environment: Environment::Test,
                encryption_key: Some([1; 32]),
                token_pepper: Some([2; 32]),
                admin_token_digest: admin_token
                    .map(|token| sha2::Sha256::digest(token.as_bytes()).into()),
                admin_password: None,
                gateway_event_retention_days: 30,
                linuxdo_oauth: None,
                default_upstream_pool_id: None,
            }),
            pool,
            http: reqwest::Client::new(),
        }
    }

    fn admin_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("header is valid"),
        );
        headers
    }

    #[tokio::test]
    async fn pool_creation_requires_admin_and_never_returns_secrets() {
        let state = test_state(Some("bootstrap-secret-that-is-long-enough-123")).await;
        let created = create_pool(
            State(state.clone()),
            admin_headers("bootstrap-secret-that-is-long-enough-123"),
            Json(CreatePoolRequest {
                name: "Primary Teamspace".to_owned(),
                authorization_scope: "teamspace:core".to_owned(),
            }),
        )
        .await
        .expect("admin can create pool");
        assert_eq!(created.0, StatusCode::CREATED);
        let list = list_pools(State(state.clone()), admin_headers("wrong-secret")).await;
        assert!(list.is_err());
        let pools = list_pools(
            State(state),
            admin_headers("bootstrap-secret-that-is-long-enough-123"),
        )
        .await
        .expect("admin lists pools");
        assert_eq!(pools.0.len(), 1);
        assert_eq!(pools.0[0].name, "Primary Teamspace");
    }

    #[tokio::test]
    async fn user_management_is_not_authorized_by_a_gateway_token() {
        let state = test_state(None).await;
        let _pool: SqlitePool = state.pool.clone();
        assert!(list_pools(State(state), HeaderMap::new()).await.is_err());
    }

    #[tokio::test]
    async fn upstream_keys_are_encrypted_and_gateway_tokens_are_one_time() {
        let state = test_state(Some("bootstrap-secret-that-is-long-enough-123")).await;
        let admin = admin_headers("bootstrap-secret-that-is-long-enough-123");
        let (_, created_pool) = create_pool(
            State(state.clone()),
            admin.clone(),
            Json(CreatePoolRequest {
                name: "teamspace-test".to_owned(),
                authorization_scope: "approved-scope".to_owned(),
            }),
        )
        .await
        .expect("pool created");
        let secret_key = "ctx7sk-test-upstream-private-value";
        let (_, created_key) = create_upstream_key(
            State(state.clone()),
            axum::extract::Path(created_pool.id.clone()),
            admin.clone(),
            Ok(Json(CreateKeyRequest {
                api_key: secret_key.to_owned(),
            })),
        )
        .await
        .expect("key encrypted and saved");
        assert!(!created_key.display_prefix.contains("ctx7sk-test-upstream"));
        let ciphertext = sqlx::query("SELECT ciphertext FROM upstream_keys WHERE id = ?")
            .bind(&created_key.id)
            .fetch_one(&state.pool)
            .await
            .expect("encrypted key row exists");
        let ciphertext = ciphertext.get::<Vec<u8>, _>("ciphertext");
        assert!(!String::from_utf8_lossy(&ciphertext).contains(secret_key));

        let (_, user) = create_user(
            State(state.clone()),
            admin.clone(),
            Json(CreateUserRequest {
                display_name: "key-owner".to_owned(),
                upstream_pool_id: created_pool.id.clone(),
                oauth_subject: None,
            }),
        )
        .await
        .expect("user created");
        let (_, issued) = create_gateway_token(
            State(state.clone()),
            axum::extract::Path(user.id.clone()),
            admin.clone(),
            Json(CreateGatewayTokenRequest {
                name: "integration-test".to_owned(),
                expires_in_days: None,
                scopes: None,
            }),
        )
        .await
        .expect("gateway token issued");
        assert!(issued.token.starts_with("c7g_"));

        let summaries = list_gateway_tokens(
            State(state.clone()),
            axum::extract::Path(user.id.clone()),
            admin.clone(),
        )
        .await
        .expect("admin can list summaries");
        assert_eq!(summaries.0.len(), 1);
        assert!(
            !serde_json::to_string(&summaries.0[0])
                .expect("serializes")
                .contains(&issued.token)
        );

        let token_headers = admin_headers(&issued.token);
        assert!(
            authenticate_gateway_token(&token_headers, &state, super::GatewayTokenScope::Mcp)
                .await
                .is_ok()
        );
        revoke_gateway_token(
            State(state.clone()),
            axum::extract::Path((user.id.clone(), issued.id.clone())),
            admin,
        )
        .await
        .expect("token can be revoked");
        assert!(
            authenticate_gateway_token(&token_headers, &state, super::GatewayTokenScope::Mcp)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn bulk_upstream_key_import_reports_without_echoing_secret_values() {
        let state = test_state(Some("bootstrap-secret-that-is-long-enough-123")).await;
        let admin = admin_headers("bootstrap-secret-that-is-long-enough-123");
        let (_, pool) = create_pool(
            State(state.clone()),
            admin.clone(),
            Json(CreatePoolRequest {
                name: "bulk-teamspace".to_owned(),
                authorization_scope: "operator-managed-source".to_owned(),
            }),
        )
        .await
        .expect("pool created");
        let valid = "ctx7sk-bulk-private-value-0001";
        let invalid = "not-a-context7-key";
        let imported = bulk_create_upstream_keys(
            State(state.clone()),
            axum::extract::Path(pool.id.clone()),
            admin.clone(),
            Ok(Json(BulkCreateKeysRequest {
                api_keys: vec![format!("  {valid}  "), valid.to_owned(), invalid.to_owned()],
            })),
        )
        .await
        .expect("bulk import accepts mixed entries");
        assert_eq!(imported.0.created, 1);
        assert_eq!(imported.0.duplicates, 1);
        assert_eq!(imported.0.invalid, 1);
        assert_eq!(
            imported
                .0
                .results
                .iter()
                .map(|result| result.status)
                .collect::<Vec<_>>(),
            vec![
                BulkKeyImportStatus::Created,
                BulkKeyImportStatus::Duplicate,
                BulkKeyImportStatus::Invalid,
            ]
        );
        let serialized = serde_json::to_string(&imported.0).expect("response serializes");
        assert!(!serialized.contains(valid));
        assert!(!serialized.contains(invalid));

        let keys = list_upstream_keys(
            State(state.clone()),
            axum::extract::Path(pool.id.clone()),
            admin.clone(),
        )
        .await
        .expect("admin lists imported keys");
        assert_eq!(keys.0.len(), 1);
        assert_eq!(keys.0[0].status, "pending");
        let ciphertext = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT ciphertext FROM upstream_keys WHERE pool_id = ?",
        )
        .bind(&pool.id)
        .fetch_one(&state.pool)
        .await
        .expect("encrypted row exists");
        assert!(!String::from_utf8_lossy(&ciphertext).contains(valid));
        let details = sqlx::query_scalar::<_, String>(
            "SELECT details_json FROM admin_audit_events WHERE action = 'upstream_key.bulk_import' ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_one(&state.pool)
        .await
        .expect("bulk audit event exists");
        assert!(details.contains("\"created\":1"));
        assert!(details.contains("\"duplicates\":1"));
        assert!(details.contains("\"invalid\":1"));

        let repeated = bulk_create_upstream_keys(
            State(state.clone()),
            axum::extract::Path(pool.id.clone()),
            admin,
            Ok(Json(BulkCreateKeysRequest {
                api_keys: vec![valid.to_owned()],
            })),
        )
        .await
        .expect("repeat import is handled");
        assert_eq!(repeated.0.created, 0);
        assert_eq!(repeated.0.duplicates, 1);
        assert_eq!(repeated.0.invalid, 0);
    }
}
