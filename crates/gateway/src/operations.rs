use axum::{
    Json,
    body::{Body, to_bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    credentials::{CredentialCipher, EncryptedCredential, PoolId, UpstreamKeyId},
    management::{
        ApiError, ApiErrorCode, AppState, authorize_admin, map_database_error, write_audit,
    },
    oauth::authenticate_browser_session,
};

const MAX_USAGE_BYTES: usize = 512 * 1024;
const USAGE_PATH: &str = "/api/v2/usage";
const VALIDATION_PATH: &str = "/api/v2/libs/search";

#[derive(Deserialize)]
pub struct UsageParams {
    days: Option<u8>,
}

#[derive(Serialize)]
pub struct KeyUsage {
    authorization_scope: String,
    fetched_at: String,
    upstream_status: u16,
    status: &'static str,
    usage: Option<Value>,
}

pub async fn key_usage(
    State(state): State<AppState>,
    Path((pool_id, key_id)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<UsageParams>,
) -> Result<Json<KeyUsage>, ApiError> {
    authorize_admin(&state, &headers).await?;
    let days = query.days.unwrap_or(1);
    if !(1..=31).contains(&days) {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let (key, authorization_scope) = load_upstream_credential(&state, &pool_id, &key_id).await?;
    let response = fetch_usage(&state, &key, days).await?;
    let upstream_status = response.status().as_u16();
    let usage = if response.status().is_success() {
        let body = to_bytes(Body::from_stream(response.bytes_stream()), MAX_USAGE_BYTES)
            .await
            .map_err(|_| ApiError::new(ApiErrorCode::UpstreamUnavailable))?;
        let value: Value = serde_json::from_slice(&body)
            .map_err(|_| ApiError::new(ApiErrorCode::UpstreamUnavailable))?;
        if !value.is_object() || body.windows(key.len()).any(|value| value == key.as_bytes()) {
            return Err(ApiError::new(ApiErrorCode::UpstreamUnavailable));
        }
        Some(value)
    } else {
        None
    };
    let fetched_at = if let Some(usage) = &usage {
        sqlx::query_scalar("INSERT INTO upstream_usage_snapshots(id, key_id, days, usage_json) VALUES (?, ?, ?, ?) RETURNING fetched_at")
            .bind(Uuid::new_v4().simple().to_string()).bind(key_id).bind(days).bind(usage.to_string())
            .fetch_one(&state.pool).await.map_err(map_database_error)?
    } else {
        sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')")
            .fetch_one(&state.pool)
            .await
            .map_err(map_database_error)?
    };
    let status = match upstream_status {
        200..=299 => "available",
        403 | 404 => "unsupported",
        _ => "upstream_error",
    };
    Ok(Json(KeyUsage {
        authorization_scope,
        fetched_at,
        upstream_status,
        status,
        usage,
    }))
}

pub(crate) async fn fetch_usage(
    state: &AppState,
    secret: &str,
    days: u8,
) -> Result<reqwest::Response, ApiError> {
    let mut url = state.config.context7_rest_origin.clone();
    url.set_path(USAGE_PATH);
    state
        .http
        .get(url)
        .query(&[("days", days)])
        .bearer_auth(secret)
        .send()
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::UpstreamUnavailable))
}

pub(crate) async fn validate_read_key(
    state: &AppState,
    secret: &str,
) -> Result<reqwest::Response, ApiError> {
    let mut url = state.config.context7_rest_origin.clone();
    url.set_path(VALIDATION_PATH);
    state
        .http
        .get(url)
        .query(&[
            ("libraryName", "axum"),
            ("query", "Context7 read API credential validation"),
            ("fast", "true"),
        ])
        .bearer_auth(secret)
        .send()
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::UpstreamUnavailable))
}

pub(crate) async fn load_upstream_credential(
    state: &AppState,
    pool_id: &str,
    key_id: &str,
) -> Result<(Zeroizing<String>, String), ApiError> {
    let (ciphertext, nonce, version, scope) = sqlx::query_as::<_, (Vec<u8>, Vec<u8>, i64, String)>(
        "SELECT k.ciphertext, k.nonce, k.encryption_version, p.authorization_scope FROM upstream_keys k JOIN upstream_pools p ON k.pool_id = p.id WHERE k.id = ? AND k.pool_id = ? AND k.deleted_at IS NULL",
    ).bind(key_id).bind(pool_id).fetch_optional(&state.pool).await.map_err(map_database_error)?
        .ok_or_else(|| ApiError::new(ApiErrorCode::NotFound))?;
    let secret = decrypt_upstream_credential(
        state,
        &PoolId(pool_id.to_owned()),
        &UpstreamKeyId(key_id.to_owned()),
        ciphertext,
        &nonce,
        version,
    )?;
    Ok((secret, scope))
}

pub(crate) fn decrypt_upstream_credential(
    state: &AppState,
    pool_id: &PoolId,
    key_id: &UpstreamKeyId,
    ciphertext: Vec<u8>,
    nonce: &[u8],
    version: i64,
) -> Result<Zeroizing<String>, ApiError> {
    let unavailable = || ApiError::new(ApiErrorCode::CredentialUnavailable);
    if version != 1 {
        return Err(unavailable());
    }
    let encrypted = EncryptedCredential {
        nonce: nonce.try_into().map_err(|_| unavailable())?,
        ciphertext,
    };
    CredentialCipher::new(state.config.encryption_key)
        .and_then(|cipher| cipher.decrypt(key_id, pool_id, &encrypted))
        .map(Zeroizing::new)
        .map_err(|_| unavailable())
}

#[derive(Serialize, Deserialize)]
pub struct Settings {
    registration_open: bool,
}

pub async fn get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Settings>, ApiError> {
    authorize_admin(&state, &headers).await?;
    let registration_open =
        sqlx::query_scalar("SELECT registration_open FROM gateway_settings WHERE id = 1")
            .fetch_one(&state.pool)
            .await
            .map_err(map_database_error)?;
    Ok(Json(Settings { registration_open }))
}

pub async fn update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(settings): Json<Settings>,
) -> Result<Json<Settings>, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let mut tx = state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(map_database_error)?;
    if settings.registration_open {
        let eligible_pool: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM upstream_pools p JOIN upstream_keys k ON k.pool_id = p.id WHERE p.id = ? AND p.enabled = 1 AND k.deleted_at IS NULL AND k.encryption_version = 1 AND (k.status = 'active' OR (k.status = 'rate_limited' AND k.rate_limited_until IS NOT NULL AND unixepoch(k.rate_limited_until) <= unixepoch())) AND (p.rate_limited_until IS NULL OR unixepoch(p.rate_limited_until) <= unixepoch()) AND (p.breaker_until IS NULL OR unixepoch(p.breaker_until) <= unixepoch()))",
        )
        .bind(&state.config.default_upstream_pool_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_database_error)?;
        if !eligible_pool
            || state.config.linuxdo_oauth.is_none()
            || state.config.encryption_key.is_none()
            || state.config.token_pepper.is_none()
        {
            return Err(ApiError::new(ApiErrorCode::NotConfigured));
        }
    }
    sqlx::query("UPDATE gateway_settings SET registration_open = ? WHERE id = 1")
        .bind(settings.registration_open)
        .execute(&mut *tx)
        .await
        .map_err(map_database_error)?;
    write_audit(
        &mut *tx,
        &actor,
        if settings.registration_open {
            "registration.open"
        } else {
            "registration.close"
        },
        "gateway_settings",
        "1",
    )
    .await?;
    tx.commit().await.map_err(map_database_error)?;
    Ok(Json(settings))
}

#[derive(Serialize, FromRow)]
pub struct Announcement {
    id: String,
    title: String,
    body: String,
    enabled: bool,
    created_at: String,
    ends_at: Option<i64>,
}

#[derive(Deserialize)]
pub struct CreateAnnouncement {
    title: String,
    body: String,
    enabled: Option<bool>,
    ends_at: Option<i64>,
}

#[derive(Deserialize)]
pub struct SetAnnouncementEnabled {
    enabled: bool,
}

pub async fn list_announcements(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Announcement>>, ApiError> {
    authorize_admin(&state, &headers).await?;
    announcements(&state, false).await.map(Json)
}

pub async fn user_announcements(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Announcement>>, ApiError> {
    authenticate_browser_session(&state, &headers).await?;
    announcements(&state, true).await.map(Json)
}

async fn announcements(state: &AppState, active: bool) -> Result<Vec<Announcement>, ApiError> {
    sqlx::query_as("SELECT id, title, body, enabled, strftime('%Y-%m-%dT%H:%M:%SZ', created_at, 'unixepoch') AS created_at, ends_at FROM announcements WHERE NOT ? OR (enabled = 1 AND (ends_at IS NULL OR ends_at > unixepoch())) ORDER BY created_at DESC, id DESC LIMIT 100")
        .bind(active).fetch_all(&state.pool).await.map_err(map_database_error)
}

pub async fn create_announcement(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateAnnouncement>,
) -> Result<(StatusCode, Json<Announcement>), ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let title = request.title.trim();
    let body = request.body.trim();
    if title.is_empty()
        || title.len() > 240
        || title.chars().any(char::is_control)
        || body.is_empty()
        || body.len() > 12_000
        || request
            .ends_at
            .is_some_and(|value| value <= 0 || value > 253_402_300_799)
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let id = Uuid::new_v4().simple().to_string();
    let mut tx = state.pool.begin().await.map_err(map_database_error)?;
    let announcement = sqlx::query_as("INSERT INTO announcements(id, title, body, enabled, ends_at) VALUES (?, ?, ?, ?, ?) RETURNING id, title, body, enabled, strftime('%Y-%m-%dT%H:%M:%SZ', created_at, 'unixepoch') AS created_at, ends_at")
        .bind(&id).bind(title).bind(body).bind(request.enabled.unwrap_or(true)).bind(request.ends_at)
        .fetch_one(&mut *tx).await.map_err(map_database_error)?;
    write_audit(&mut *tx, &actor, "announcement.create", "announcement", &id).await?;
    tx.commit().await.map_err(map_database_error)?;
    Ok((StatusCode::CREATED, Json(announcement)))
}

pub async fn set_announcement_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<SetAnnouncementEnabled>,
) -> Result<StatusCode, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let mut tx = state.pool.begin().await.map_err(map_database_error)?;
    let result = sqlx::query("UPDATE announcements SET enabled = ? WHERE id = ?")
        .bind(request.enabled)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(map_database_error)?;
    if result.rows_affected() != 1 {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    write_audit(
        &mut *tx,
        &actor,
        if request.enabled {
            "announcement.enable"
        } else {
            "announcement.disable"
        },
        "announcement",
        &id,
    )
    .await?;
    tx.commit().await.map_err(map_database_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct ServiceStatus {
    status: &'static str,
    active_tokens: i64,
}

pub async fn user_service_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ServiceStatus>, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let (available, active_tokens) = sqlx::query_as::<_, (bool, i64)>(
        "SELECT EXISTS(SELECT 1 FROM upstream_pools p JOIN upstream_keys k ON k.pool_id = p.id JOIN users u ON u.upstream_pool_id = p.id WHERE u.id = ?1 AND p.enabled = 1 AND k.deleted_at IS NULL AND k.encryption_version = 1 AND (p.rate_limited_until IS NULL OR unixepoch(p.rate_limited_until) <= unixepoch()) AND (p.breaker_until IS NULL OR unixepoch(p.breaker_until) <= unixepoch()) AND (k.status = 'active' OR (k.status = 'rate_limited' AND unixepoch(k.rate_limited_until) <= unixepoch()))), (SELECT COUNT(*) FROM gateway_tokens WHERE user_id = ?1 AND status = 'active' AND (expires_at IS NULL OR unixepoch(expires_at) > unixepoch()))",
    ).bind(user.id).fetch_one(&state.pool).await.map_err(map_database_error)?;
    Ok(Json(ServiceStatus {
        status: if available && state.config.encryption_key.is_some() {
            "available"
        } else {
            "unavailable"
        },
        active_tokens,
    }))
}

#[derive(Serialize)]
pub struct SetupGuides {
    origin: String,
    mcp_url: String,
    rest_origin: String,
    token_env: &'static str,
}

pub async fn user_setup_guides(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SetupGuides>, ApiError> {
    authenticate_browser_session(&state, &headers).await?;
    let origin = state.config.public_origin.origin().ascii_serialization();
    Ok(Json(SetupGuides {
        mcp_url: format!("{origin}/mcp"),
        rest_origin: format!("{origin}/api/context7"),
        origin,
        token_env: "GATEWAY_API_KEY",
    }))
}
