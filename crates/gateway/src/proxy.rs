use std::time::Instant;

use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header},
    response::Response,
};
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::Url;
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    credentials::{CredentialCipher, EncryptedCredential, GatewayTokenId, PoolId, UpstreamKeyId},
    management::{ApiError, ApiErrorCode, AppState, GatewayTokenScope, authenticate_gateway_token},
    quota::{self, Failure, Invocation, Operation, Outcome, Reservation},
    session,
};

const MAX_INBOUND_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_JSON_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MCP_SESSION_LIFETIME_HOURS: i64 = 24;

#[derive(Debug, FromRow)]
struct UpstreamKey {
    id: String,
    pool_id: String,
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
    encryption_version: i64,
}

#[derive(Debug, FromRow)]
struct BoundSession {
    id: String,
    pool_id: String,
    upstream_key_id: String,
    upstream_session_ciphertext: Vec<u8>,
    upstream_session_nonce: Vec<u8>,
}

#[derive(Debug, FromRow)]
struct ResumeBookmark {
    reservation_id: String,
    rpc_id_json: String,
}

struct KeyLease {
    pool: sqlx::SqlitePool,
    key_id: UpstreamKeyId,
}

impl Drop for KeyLease {
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let key_id = self.key_id.0.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = sqlx::query(
                    "UPDATE upstream_keys SET in_flight = max(0, in_flight - 1) WHERE id = ?",
                )
                .bind(key_id)
                .execute(&pool)
                .await
                {
                    tracing::error!(%error, "upstream key lease release failed");
                }
            });
        }
    }
}

struct ForwardRequest {
    state: AppState,
    upstream_path: String,
    method: Method,
    original_uri: Uri,
    headers: HeaderMap,
    body: Body,
    is_mcp: bool,
}

pub async fn mcp(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    forward_context7(ForwardRequest {
        state,
        upstream_path: "/mcp".to_owned(),
        method,
        original_uri: uri,
        headers,
        body,
        is_mcp: true,
    })
    .await
}

pub async fn rest_readonly(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let upstream_path = match uri.path() {
        "/api/context7/v3/search" => "/api/v3/search",
        "/api/context7/v3/fetch" => "/api/v3/fetch",
        "/api/context7/v2/libs/search" => "/api/v2/libs/search",
        "/api/context7/v2/context" => "/api/v2/context",
        _ => return Err(ApiError::new(ApiErrorCode::NotFound)),
    };
    if method != Method::GET {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    forward_context7(ForwardRequest {
        state,
        upstream_path: upstream_path.to_owned(),
        method,
        original_uri: uri,
        headers,
        body,
        is_mcp: false,
    })
    .await
}

async fn forward_context7(input: ForwardRequest) -> Result<Response, ApiError> {
    let ForwardRequest {
        state,
        upstream_path,
        method,
        original_uri,
        headers,
        body,
        is_mcp,
    } = input;
    if is_mcp && !matches!(method, Method::POST | Method::GET | Method::DELETE) {
        return Err(ApiError::new(ApiErrorCode::MethodNotAllowed));
    }
    if is_mcp
        && matches!(method, Method::GET | Method::DELETE)
        && headers
            .get("mcp-protocol-version")
            .is_some_and(|value| value == "2026-07-28")
    {
        return Err(ApiError::new(ApiErrorCode::MethodNotAllowed));
    }
    let started = Instant::now();
    let scope = if is_mcp {
        GatewayTokenScope::Mcp
    } else {
        GatewayTokenScope::ContextRead
    };
    let (token_id, user_id, _) = authenticate_gateway_token(&headers, &state, scope).await?;
    let pool_id = user_upstream_pool(&state, &user_id).await?;
    if is_mcp && method == Method::POST {
        let accept = headers
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok());
        let content_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok());
        if !accept.is_some_and(|value| {
            value
                .split(',')
                .any(|item| item.trim().starts_with("application/json"))
        }) || !accept.is_some_and(|value| {
            value
                .split(',')
                .any(|item| item.trim().starts_with("text/event-stream"))
        }) || !content_type.is_some_and(|value| value.starts_with("application/json"))
        {
            return Err(ApiError::new(ApiErrorCode::InvalidRequest));
        }
    }
    let request_bytes = to_bytes(body, MAX_INBOUND_BODY_BYTES)
        .await
        .map_err(|_| ApiError::new(ApiErrorCode::InvalidRequest))?;
    if !is_mcp && !request_bytes.is_empty() {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    if is_mcp && method == Method::POST {
        let message: Value = serde_json::from_slice(&request_bytes)
            .map_err(|_| ApiError::new(ApiErrorCode::InvalidRequest))?;
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || message
                .get("method")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(ApiError::new(ApiErrorCode::InvalidRequest));
        }
    }
    let (operation, rpc_method, rpc_id) =
        classify_request(&headers, &request_bytes, &method, &upstream_path, is_mcp)?;
    let request_id = Uuid::new_v4().simple().to_string();
    let url = upstream_url(&state, &upstream_path, original_uri.query())?;
    let upstream_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|_| ApiError::new(ApiErrorCode::InvalidRequest))?;
    if is_mcp {
        validate_origin(&state, &headers)?;
    }

    let protocol_version = headers
        .get("mcp-protocol-version")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or_else(|| modern_jsonrpc_protocol(&request_bytes))
        .or_else(|| jsonrpc_protocol(&request_bytes));
    let legacy_session = protocol_version.as_deref() != Some("2026-07-28");
    let client_session_id = if legacy_session {
        headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
    } else {
        None
    };
    let bound_session = match (is_mcp, client_session_id) {
        (true, Some(id)) => Some(lookup_session(&state, &token_id, id).await?),
        _ => None,
    };
    if let Some(session) = &bound_session
        && (session.pool_id != pool_id || (rpc_method.as_deref() == Some("initialize")))
    {
        return Err(ApiError::new(ApiErrorCode::InvalidSession));
    }
    let upstream_key = match &bound_session {
        Some(session) => {
            load_upstream_key(
                &state,
                &PoolId(pool_id.clone()),
                &UpstreamKeyId(session.upstream_key_id.clone()),
            )
            .await?
        }
        None => select_upstream_key(&state, &PoolId(pool_id.clone())).await?,
    };
    let key_id = UpstreamKeyId(upstream_key.id.clone());
    let pool_id_newtype = PoolId(upstream_key.pool_id.clone());
    if upstream_key.encryption_version != 1 {
        return Err(ApiError::new(ApiErrorCode::CredentialUnavailable));
    }
    let cipher = CredentialCipher::new(state.config.encryption_key)
        .map_err(|_| ApiError::new(ApiErrorCode::CredentialUnavailable))?;
    let api_key = Zeroizing::new(
        cipher
            .decrypt(
                &key_id,
                &pool_id_newtype,
                &EncryptedCredential {
                    nonce: upstream_key
                        .nonce
                        .as_slice()
                        .try_into()
                        .map_err(|_| ApiError::new(ApiErrorCode::CredentialUnavailable))?,
                    ciphertext: upstream_key.ciphertext,
                },
            )
            .map_err(|_| ApiError::new(ApiErrorCode::CredentialUnavailable))?,
    );
    let upstream_session_id = bound_session
        .as_ref()
        .map(|session| {
            let encrypted = EncryptedCredential {
                nonce: session
                    .upstream_session_nonce
                    .as_slice()
                    .try_into()
                    .map_err(|_| ApiError::new(ApiErrorCode::InvalidSession))?,
                ciphertext: session.upstream_session_ciphertext.clone(),
            };
            cipher
                .decrypt(&key_id, &pool_id_newtype, &encrypted)
                .map(Zeroizing::new)
                .map_err(|_| ApiError::new(ApiErrorCode::InvalidSession))
        })
        .transpose()?;

    let resume_bookmark = match (
        bound_session.as_ref(),
        headers.get("last-event-id"),
        rpc_id.as_ref(),
    ) {
        (Some(session), Some(event_id), _) if method == Method::GET => {
            let event_id = event_id
                .to_str()
                .map_err(|_| ApiError::new(ApiErrorCode::InvalidRequest))?;
            Some(lookup_bookmark(&state, session, event_id).await?)
        }
        _ => None,
    };
    let reservation = quota::reserve(
        &state.pool,
        Invocation {
            user_id: &user_id,
            token_id: &token_id,
            pool_id: &pool_id_newtype,
            key_id: &key_id,
            operation,
            request_id: &request_id,
        },
    )
    .await
    .map_err(|error| match error {
        quota::AdmissionError::Unassigned => ApiError::new(ApiErrorCode::QuotaExceeded),
        quota::AdmissionError::Limited {
            retry_after_seconds,
        } => ApiError::new(ApiErrorCode::QuotaExceeded).retry_after(retry_after_seconds),
        quota::AdmissionError::Storage(error) => {
            tracing::error!(%error, "quota admission failed");
            ApiError::new(ApiErrorCode::InternalError)
        }
    })?;
    let leased = match sqlx::query("UPDATE upstream_keys SET in_flight = in_flight + 1, last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'), status = CASE WHEN status = 'rate_limited' THEN 'active' ELSE status END WHERE id = ? AND deleted_at IS NULL AND (status = 'active' OR (status = 'rate_limited' AND unixepoch(rate_limited_until) <= unixepoch()))")
        .bind(&key_id.0)
        .execute(&state.pool)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            if let Err(cleanup_error) = reservation.reject_before_send().await {
                tracing::error!(%cleanup_error, "failed to release quota after key lease error");
            }
            return Err(crate::management::map_database_error(error));
        }
    };
    if leased.rows_affected() != 1 {
        reservation
            .reject_before_send()
            .await
            .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
        return Err(ApiError::new(ApiErrorCode::PoolUnavailable));
    }
    let key_lease = KeyLease {
        pool: state.pool.clone(),
        key_id: key_id.clone(),
    };
    let (mut active_reservation, mut transport_reservation) =
        if let Some(bookmark) = &resume_bookmark {
            let id = Uuid::parse_str(&bookmark.reservation_id)
                .map_err(|_| ApiError::new(ApiErrorCode::InvalidSession))?;
            if let Some(active) = quota::reattach(&state.pool, id)
                .await
                .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?
            {
                (Some(active), Some(reservation))
            } else {
                (Some(reservation), None)
            }
        } else {
            (Some(reservation), None)
        };
    let active_reservation_id = active_reservation
        .as_ref()
        .and_then(Reservation::id)
        .map(|id| id.simple().to_string());

    let mut request = state.http.request(upstream_method, url);
    for (name, value) in &headers {
        if should_forward_request_header(name)
            && (name.as_str() != "last-event-id" || legacy_session)
        {
            request = request.header(name, value);
        }
    }
    request = request.header(
        header::AUTHORIZATION,
        format!("Bearer {}", api_key.as_str()),
    );
    if let Some(upstream_session_id) = upstream_session_id.as_deref() {
        request = request.header("mcp-session-id", upstream_session_id);
    }
    if !request_bytes.is_empty() {
        request = request.body(request_bytes.clone());
    }

    let upstream_response = match request.send().await {
        Ok(response) => response,
        Err(_) => {
            let failure = Outcome {
                gateway_status: StatusCode::BAD_GATEWAY,
                upstream_status: None,
                duration: started.elapsed(),
                failure: Some(Failure::UpstreamTransport),
            };
            if let Some(reservation) = active_reservation.take() {
                reservation
                    .settle(failure)
                    .await
                    .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
            }
            if let Some(reservation) = transport_reservation.take() {
                reservation
                    .settle(failure)
                    .await
                    .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
            }
            return Err(ApiError::new(ApiErrorCode::UpstreamUnavailable));
        }
    };
    let upstream_status = upstream_response.status();
    let upstream_headers = upstream_response.headers().clone();
    update_upstream_key_status(&state, &upstream_key.id, upstream_status, &upstream_headers).await;
    let status = StatusCode::from_u16(upstream_status.as_u16())
        .map_err(|_| ApiError::new(ApiErrorCode::UpstreamUnavailable))?;
    if let Some(reservation) = transport_reservation.take() {
        let failure = (!status.is_success()).then_some(Failure::UpstreamError);
        reservation
            .settle(Outcome {
                gateway_status: status,
                upstream_status: Some(status),
                duration: started.elapsed(),
                failure,
            })
            .await
            .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    }
    if operation == Operation::McpSse
        && resume_bookmark.is_none()
        && let Some(reservation) = active_reservation.take()
    {
        let failure = (!status.is_success()).then_some(Failure::UpstreamError);
        reservation
            .settle(Outcome {
                gateway_status: status,
                upstream_status: Some(status),
                duration: started.elapsed(),
                failure,
            })
            .await
            .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    }
    let upstream_session = upstream_headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok());
    let external_session_id = if is_mcp && legacy_session {
        match (client_session_id, upstream_session) {
            (Some(existing), _) if bound_session.is_some() => Some(existing.to_owned()),
            (None, Some(upstream))
                if rpc_method.as_deref() == Some("initialize")
                    && jsonrpc_protocol(&request_bytes).as_deref() != Some("2026-07-28") =>
            {
                Some(
                    create_session(
                        &state,
                        &token_id,
                        &pool_id_newtype,
                        &key_id,
                        upstream,
                        &cipher,
                    )
                    .await?,
                )
            }
            _ => None,
        }
    } else {
        None
    };
    if method == Method::DELETE
        && let Some(session) = &bound_session
    {
        sqlx::query("UPDATE mcp_sessions SET closed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?")
            .bind(&session.id).execute(&state.pool).await.map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    }

    let content_type = upstream_headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut response_builder = Response::builder().status(status);
    if let Some(response_headers) = response_builder.headers_mut() {
        for (name, value) in &upstream_headers {
            if should_forward_response_header(name) {
                response_headers.insert(name, value.clone());
            }
        }
        if let Ok(value) = HeaderValue::from_str(&request_id) {
            response_headers.insert(HeaderName::from_static("x-gateway-request-id"), value);
        }
        if let Some(external_session_id) = external_session_id.as_deref()
            && let Ok(value) = HeaderValue::from_str(external_session_id)
        {
            response_headers.insert(HeaderName::from_static("mcp-session-id"), value);
        }
    }
    let upstream_body = upstream_response.bytes_stream();
    let body_stream = if content_type.starts_with("text/event-stream")
        && operation != Operation::McpNotification
    {
        let effective_rpc_id = rpc_id.clone().or_else(|| {
            resume_bookmark
                .as_ref()
                .and_then(|bookmark| serde_json::from_str(&bookmark.rpc_id_json).ok())
        });
        let rpc_id_json = effective_rpc_id.as_ref().map(ToString::to_string);
        let reservation_id = active_reservation_id.clone();
        let pool = state.pool.clone();
        let bookmark_session = if let Some(session) = &bound_session {
            Some(session.id.clone())
        } else {
            lookup_created_session_id(&state, external_session_id.as_deref()).await?
        };
        let result_state = state.clone();
        let resumable_session = bookmark_session.is_some();
        let has_resume_bookmark = resume_bookmark.is_some();
        let lease = key_lease;
        let stream = async_stream::try_stream! {
            let _lease = lease;
            let mut source = upstream_body;
            let mut pending = BytesMut::new();
            let mut event = SseEvent::default();
            let mut reservation = active_reservation;
            let mut bookmark_saved = has_resume_bookmark;
            let mut transport_failed = false;
            while let Some(next) = source.next().await {
                let chunk = match next {
                    Ok(chunk) => chunk,
                    Err(_) => {
                        transport_failed = true;
                        break;
                    }
                };
                pending.extend_from_slice(&chunk);
                if pending.len() > MAX_JSON_RESPONSE_BYTES {
                    if let Some(reservation) = reservation.take() {
                        reservation.settle(Outcome { gateway_status: StatusCode::BAD_GATEWAY, upstream_status: Some(status), duration: started.elapsed(), failure: Some(Failure::InvalidResponse) }).await.map_err(|_| std::io::Error::other("quota settlement failed"))?;
                    }
                    Err(std::io::Error::other("MCP SSE line exceeded the response limit"))?;
                }
                while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                    let mut line = pending.split_to(end + 1).freeze();
                    if line.last() == Some(&b'\n') { line = line.slice(..line.len() - 1); }
                    if line.last() == Some(&b'\r') { line = line.slice(..line.len() - 1); }
                    if line.is_empty() {
                        if let Some(reservation_id) = reservation_id.as_deref()
                            && let Ok(id) = Uuid::parse_str(reservation_id)
                        {
                            let _ = quota::extend(&pool, id).await;
                        }
                        if let Some(event_id) = event.id.take()
                            && let (Some(session_id), Some(reservation_id), Some(rpc_id_json)) = (bookmark_session.as_deref(), reservation_id.as_deref(), rpc_id_json.as_deref())
                        {
                            save_bookmark(&pool, &result_state, session_id, &event_id, reservation_id, rpc_id_json).await
                                .map_err(|_| std::io::Error::other("resume bookmark write failed"))?;
                            bookmark_saved = true;
                            if let Some(reservation) = reservation.take() { reservation.detach(); }
                        }
                        if let Some(data) = event.data.take()
                            && let (Ok(value), Some(rpc_id)) = (serde_json::from_str::<Value>(&data), effective_rpc_id.as_ref())
                            && value.get("id") == Some(rpc_id)
                        {
                            let outcome = mcp_outcome(status, &value, started.elapsed());
                            settle_stream_reservation(&result_state, reservation.take(), reservation_id.as_deref(), outcome).await
                                .map_err(|_| std::io::Error::other("MCP stream processing failed"))?;
                            clear_bookmarks(&pool, bookmark_session.as_deref(), reservation_id.as_deref()).await
                                .map_err(|_| std::io::Error::other("MCP stream processing failed"))?;
                        }
                        event = SseEvent::default();
                    } else if let Some(value) = line.strip_prefix(b"id:") {
                        event.id = Some(String::from_utf8_lossy(value).trim().to_owned());
                    } else if let Some(value) = line.strip_prefix(b"data:") {
                        let data = String::from_utf8_lossy(value).trim_start().to_owned();
                        if let Some(current) = &mut event.data {
                            if current.len().saturating_add(data.len()).saturating_add(1) > MAX_JSON_RESPONSE_BYTES {
                                if let Some(reservation) = reservation.take() {
                                    reservation.settle(Outcome { gateway_status: StatusCode::BAD_GATEWAY, upstream_status: Some(status), duration: started.elapsed(), failure: Some(Failure::InvalidResponse) }).await.map_err(|_| std::io::Error::other("quota settlement failed"))?;
                                }
                                Err(std::io::Error::other("MCP SSE event exceeded the response limit"))?;
                            }
                            current.push('\n'); current.push_str(&data);
                        } else if data.len() > MAX_JSON_RESPONSE_BYTES {
                            if let Some(reservation) = reservation.take() {
                                reservation.settle(Outcome { gateway_status: StatusCode::BAD_GATEWAY, upstream_status: Some(status), duration: started.elapsed(), failure: Some(Failure::InvalidResponse) }).await.map_err(|_| std::io::Error::other("quota settlement failed"))?;
                            }
                            Err(std::io::Error::other("MCP SSE event exceeded the response limit"))?;
                        } else { event.data = Some(data); }
                    }
                }
                yield chunk;
            }
            if let Some(reservation) = reservation.take() {
                if bookmark_saved && resumable_session {
                    reservation.detach();
                } else {
                    let failure = if transport_failed { Some(Failure::UpstreamTransport) }
                        else if !status.is_success() { Some(Failure::UpstreamError) }
                        else if is_mcp && rpc_id.is_some() { Some(Failure::InvalidResponse) }
                        else { None };
                    let gateway_status = if transport_failed { StatusCode::BAD_GATEWAY } else { status };
                    reservation.settle(Outcome { gateway_status, upstream_status: (!transport_failed).then_some(status), duration: started.elapsed(), failure })
                        .await.map_err(|_| std::io::Error::other("quota settlement failed"))?;
                }
            }
            if transport_failed { Err(std::io::Error::other("Context7 SSE stream failed"))?; }
        };
        Box::pin(stream)
            as std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
            >
    } else if !is_mcp && content_type.starts_with("text/event-stream") {
        let response_state = state.clone();
        let lease = key_lease;
        let reservation_id = active_reservation_id.clone();
        let response_status = status;
        let stream = async_stream::try_stream! {
            let _lease = lease;
            let mut source = upstream_body;
            let mut reservation = active_reservation;
            let mut transport_failed = false;
            while let Some(next) = source.next().await {
                match next {
                    Ok(chunk) => yield chunk,
                    Err(_) => { transport_failed = true; break; }
                }
            }
            let failure = if transport_failed {
                Some(Failure::UpstreamTransport)
            } else if operation == Operation::McpNotification && response_status != StatusCode::ACCEPTED {
                Some(Failure::InvalidResponse)
            } else {
                (!response_status.is_success()).then_some(Failure::UpstreamError)
            };
            let gateway_status = if transport_failed { StatusCode::BAD_GATEWAY } else { response_status };
            settle_stream_reservation(&response_state, reservation.take(), reservation_id.as_deref(), Outcome { gateway_status, upstream_status: (!transport_failed).then_some(response_status), duration: started.elapsed(), failure }).await.map_err(|_| std::io::Error::other("quota settlement failed"))?;
        };
        Box::pin(stream)
            as std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
            >
    } else if content_type.starts_with("application/json") && is_mcp && rpc_id.is_some() {
        let rpc_id = rpc_id.clone();
        let response_status = status;
        let response_state = state.clone();
        let lease = key_lease;
        let reservation_id = active_reservation_id.clone();
        let stream = async_stream::try_stream! {
            let _lease = lease;
            let mut source = upstream_body;
            let mut bytes = BytesMut::new();
            let mut reservation = active_reservation;
            let mut transport_failed = false;
            while let Some(next) = source.next().await {
                let chunk = match next {
                    Ok(chunk) => chunk,
                    Err(_) => {
                        transport_failed = true;
                        break;
                    }
                };
                if bytes.len().saturating_add(chunk.len()) > MAX_JSON_RESPONSE_BYTES {
                    if let Some(reservation) = reservation.take() {
                        reservation.settle(Outcome { gateway_status: StatusCode::BAD_GATEWAY, upstream_status: Some(response_status), duration: started.elapsed(), failure: Some(Failure::InvalidResponse) }).await.map_err(|_| std::io::Error::other("quota settlement failed"))?;
                    }
                    Err(std::io::Error::other("MCP JSON response exceeded limit"))?;
                }
                bytes.extend_from_slice(&chunk);
                yield chunk;
            }
            let parsed = serde_json::from_slice::<Value>(&bytes);
            let failure = if transport_failed { Some(Failure::UpstreamTransport) } else { match parsed.as_ref() {
                Ok(value) if response_status.is_success() && rpc_id.as_ref().is_some_and(|id| value.get("id") != Some(id)) => Some(Failure::InvalidResponse),
                Ok(value) if response_status.is_success() => mcp_failure(value),
                Ok(_) => Some(Failure::UpstreamError),
                Err(_) => Some(Failure::InvalidResponse),
            }};
            let gateway_status = if transport_failed { StatusCode::BAD_GATEWAY } else { response_status };
            let outcome = Outcome { gateway_status, upstream_status: (!transport_failed).then_some(response_status), duration: started.elapsed(), failure };
            settle_stream_reservation(&response_state, reservation.take(), reservation_id.as_deref(), outcome).await.map_err(|_| std::io::Error::other("MCP stream processing failed"))?;
        };
        Box::pin(stream)
            as std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
            >
    } else {
        let response_state = state.clone();
        let lease = key_lease;
        let reservation_id = active_reservation_id.clone();
        let response_status = status;
        let stream = async_stream::try_stream! {
            let _lease = lease;
            let mut source = upstream_body;
            let mut reservation = active_reservation;
            let mut transport_failed = false;
            while let Some(next) = source.next().await {
                match next {
                    Ok(chunk) => yield chunk,
                    Err(_) => { transport_failed = true; break; }
                }
            }
            let failure = if transport_failed { Some(Failure::UpstreamTransport) } else { (!response_status.is_success()).then_some(Failure::UpstreamError) };
            let gateway_status = if transport_failed { StatusCode::BAD_GATEWAY } else { response_status };
            settle_stream_reservation(&response_state, reservation.take(), reservation_id.as_deref(), Outcome { gateway_status, upstream_status: (!transport_failed).then_some(response_status), duration: started.elapsed(), failure }).await.map_err(|_| std::io::Error::other("quota settlement failed"))?;
        };
        Box::pin(stream)
            as std::pin::Pin<
                Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
            >
    };
    let response = response_builder
        .body(Body::from_stream(body_stream))
        .map_err(|_| ApiError::new(ApiErrorCode::UpstreamUnavailable))?;
    Ok(response)
}

#[derive(Default)]
struct SseEvent {
    id: Option<String>,
    data: Option<String>,
}

async fn settle_stream_reservation(
    state: &AppState,
    reservation: Option<Reservation>,
    id: Option<&str>,
    outcome: Outcome,
) -> Result<(), sqlx::Error> {
    if let Some(reservation) = reservation {
        reservation.settle(outcome).await?;
    } else if let Some(id) = id
        && let Ok(id) = Uuid::parse_str(id)
        && let Some(reservation) = quota::reattach(&state.pool, id).await?
    {
        reservation.settle(outcome).await?;
    }
    Ok(())
}

fn mcp_failure(value: &Value) -> Option<Failure> {
    if value.get("error").is_some()
        || value.pointer("/result/isError").and_then(Value::as_bool) == Some(true)
    {
        Some(Failure::McpError)
    } else {
        None
    }
}

fn mcp_outcome(status: StatusCode, value: &Value, duration: std::time::Duration) -> Outcome {
    let failure = mcp_failure(value);
    Outcome {
        gateway_status: if failure.is_some() && status.is_success() {
            StatusCode::BAD_GATEWAY
        } else {
            status
        },
        upstream_status: Some(status),
        duration,
        failure,
    }
}

fn jsonrpc_protocol(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn modern_jsonrpc_protocol(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value
        .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn decode_mcp_name(value: &str) -> Option<String> {
    if !value.starts_with("=?base64?") {
        return value.is_ascii().then(|| value.to_owned());
    }
    let encoded = value.strip_prefix("=?base64?")?.strip_suffix("?=")?;
    use base64::Engine as _;
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?,
    )
    .ok()
}

fn classify_request(
    headers: &HeaderMap,
    body: &[u8],
    method: &Method,
    path: &str,
    is_mcp: bool,
) -> Result<(Operation, Option<String>, Option<Value>), ApiError> {
    if !is_mcp {
        let operation = match path {
            "/api/v3/search" => Operation::SearchV3,
            "/api/v3/fetch" => Operation::FetchV3,
            "/api/v2/libs/search" => Operation::LibrarySearchV2,
            "/api/v2/context" => Operation::ContextV2,
            _ => Operation::SearchV3,
        };
        return Ok((operation, None, None));
    }
    if *method == Method::GET {
        return Ok((Operation::McpSse, None, None));
    }
    if *method == Method::DELETE {
        return Ok((Operation::McpDelete, None, None));
    }
    let value: Value =
        serde_json::from_slice(body).map_err(|_| ApiError::new(ApiErrorCode::InvalidRequest))?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let method = headers
        .get("mcp-method")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .ok_or_else(|| ApiError::new(ApiErrorCode::InvalidRequest))?;
    if value.get("method").and_then(Value::as_str) != Some(method.as_str()) {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let rpc_id = value.get("id").cloned();
    let is_notification = method.starts_with("notifications/");
    if is_notification == rpc_id.is_some()
        || rpc_id
            .as_ref()
            .is_some_and(|id| !(id.is_string() || id.is_number()))
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let operation = match method.as_str() {
        "initialize" => Operation::McpInitialize,
        "tools/list" => Operation::McpToolsList,
        "tools/call" => Operation::McpToolCall,
        "ping" => Operation::McpPing,
        value if value.starts_with("notifications/") => Operation::McpNotification,
        _ => Operation::McpOther,
    };
    let notification = method.starts_with("notifications/");
    let protocol_header = headers
        .get("mcp-protocol-version")
        .and_then(|value| value.to_str().ok());
    let body_protocol = modern_jsonrpc_protocol(body);
    let modern =
        protocol_header == Some("2026-07-28") || body_protocol.as_deref() == Some("2026-07-28");
    if modern {
        if body_protocol.as_deref() != Some("2026-07-28")
            || protocol_header.is_some_and(|protocol| protocol != "2026-07-28")
        {
            return Err(ApiError::new(ApiErrorCode::InvalidRequest));
        }
        let header_method = headers.get("mcp-method").and_then(|v| v.to_str().ok());
        if !notification || protocol_header.is_some() {
            if protocol_header != Some("2026-07-28") || header_method != Some(method.as_str()) {
                return Err(ApiError::new(ApiErrorCode::InvalidRequest));
            }
        } else if header_method.is_some_and(|header_method| header_method != method) {
            return Err(ApiError::new(ApiErrorCode::InvalidRequest));
        }
        let expected_name = match method.as_str() {
            "tools/call" | "prompts/get" => value.pointer("/params/name").and_then(Value::as_str),
            "resources/read" => value.pointer("/params/uri").and_then(Value::as_str),
            _ => None,
        };
        if !notification {
            if let Some(expected_name) = expected_name {
                let header_name = headers.get("mcp-name").and_then(|v| v.to_str().ok());
                if header_name.and_then(decode_mcp_name).as_deref() != Some(expected_name) {
                    return Err(ApiError::new(ApiErrorCode::InvalidRequest));
                }
            } else if headers.contains_key("mcp-name") {
                return Err(ApiError::new(ApiErrorCode::InvalidRequest));
            }
        }
    } else if protocol_header.is_some_and(|protocol| protocol != "2025-11-25")
        || body_protocol.is_some()
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    Ok((operation, Some(method), rpc_id))
}

fn validate_origin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin
            .to_str()
            .map_err(|_| ApiError::new(ApiErrorCode::Forbidden))?;
        let parsed = Url::parse(origin).map_err(|_| ApiError::new(ApiErrorCode::Forbidden))?;
        if parsed.origin() != state.config.public_origin.origin()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(ApiError::new(ApiErrorCode::Forbidden));
        }
    }
    Ok(())
}

async fn user_upstream_pool(state: &AppState, user_id: &str) -> Result<String, ApiError> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT upstream_pool_id FROM users WHERE id = ? AND status = 'active'",
    )
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(crate::management::map_database_error)?
    .flatten()
    .ok_or_else(|| ApiError::new(ApiErrorCode::PoolUnavailable))
}

async fn select_upstream_key(state: &AppState, pool_id: &PoolId) -> Result<UpstreamKey, ApiError> {
    let pool = sqlx::query_as::<_, (i64, Option<i64>, Option<i64>)>(
        "SELECT enabled,
            CASE WHEN COALESCE(unixepoch(rate_limited_until), 0) <= unixepoch() THEN NULL
                 ELSE unixepoch(rate_limited_until) - unixepoch() END,
            CASE WHEN COALESCE(unixepoch(breaker_until), 0) <= unixepoch() THEN NULL
                 ELSE unixepoch(breaker_until) - unixepoch() END
         FROM upstream_pools WHERE id = ?",
    )
    .bind(&pool_id.0)
    .fetch_optional(&state.pool)
    .await
    .map_err(crate::management::map_database_error)?
    .ok_or_else(|| ApiError::new(ApiErrorCode::PoolUnavailable))?;
    if pool.0 != 1 {
        return Err(ApiError::new(ApiErrorCode::PoolUnavailable));
    }
    if let Some(seconds) = pool.1.filter(|seconds| *seconds > 0) {
        return Err(ApiError::new(ApiErrorCode::RateLimited).retry_after(seconds.max(1)));
    }
    if let Some(seconds) = pool.2.filter(|seconds| *seconds > 0) {
        return Err(ApiError::new(ApiErrorCode::PoolUnavailable).retry_after(seconds.max(1)));
    }
    sqlx::query_as::<_, UpstreamKey>("SELECT k.id, k.pool_id, k.ciphertext, k.nonce, k.encryption_version FROM upstream_keys k JOIN upstream_pools p ON p.id = k.pool_id WHERE k.pool_id = ? AND p.enabled = 1 AND COALESCE(unixepoch(p.rate_limited_until), 0) <= unixepoch() AND COALESCE(unixepoch(p.breaker_until), 0) <= unixepoch() AND k.deleted_at IS NULL AND (k.status = 'active' OR (k.status = 'rate_limited' AND COALESCE(unixepoch(k.rate_limited_until), 0) <= unixepoch())) ORDER BY k.in_flight ASC, COALESCE(k.last_used_at,'') ASC, k.created_at ASC LIMIT 1")
        .bind(&pool_id.0).fetch_optional(&state.pool).await.map_err(crate::management::map_database_error)?.ok_or_else(|| ApiError::new(ApiErrorCode::PoolUnavailable))
}

async fn load_upstream_key(
    state: &AppState,
    pool_id: &PoolId,
    key_id: &UpstreamKeyId,
) -> Result<UpstreamKey, ApiError> {
    sqlx::query_as::<_, UpstreamKey>("SELECT k.id, k.pool_id, k.ciphertext, k.nonce, k.encryption_version FROM upstream_keys k JOIN upstream_pools p ON p.id = k.pool_id WHERE k.id = ? AND k.pool_id = ? AND p.enabled = 1 AND COALESCE(unixepoch(p.rate_limited_until), 0) <= unixepoch() AND COALESCE(unixepoch(p.breaker_until), 0) <= unixepoch() AND k.deleted_at IS NULL AND k.status = 'active'")
        .bind(&key_id.0).bind(&pool_id.0).fetch_optional(&state.pool).await.map_err(crate::management::map_database_error)?.ok_or_else(|| ApiError::new(ApiErrorCode::PoolUnavailable))
}

async fn lookup_session(
    state: &AppState,
    token_id: &GatewayTokenId,
    external_session_id: &str,
) -> Result<BoundSession, ApiError> {
    let digest = session::token_digest(external_session_id, state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::InvalidSession))?;
    sqlx::query_as::<_, BoundSession>("SELECT id, pool_id, upstream_key_id, upstream_session_ciphertext, upstream_session_nonce FROM mcp_sessions WHERE external_session_digest = ? AND gateway_token_id = ? AND closed_at IS NULL AND unixepoch(expires_at) > unixepoch()")
        .bind(digest.to_vec()).bind(&token_id.0).fetch_optional(&state.pool).await.map_err(crate::management::map_database_error)?.ok_or_else(|| ApiError::new(ApiErrorCode::InvalidSession))
}

async fn lookup_created_session_id(
    state: &AppState,
    external_id: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let Some(external_id) = external_id else {
        return Ok(None);
    };
    let digest = session::token_digest(external_id, state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::InvalidSession))?;
    sqlx::query_scalar("SELECT id FROM mcp_sessions WHERE external_session_digest = ?")
        .bind(digest.to_vec())
        .fetch_optional(&state.pool)
        .await
        .map_err(crate::management::map_database_error)
}

async fn lookup_bookmark(
    state: &AppState,
    session: &BoundSession,
    event_id: &str,
) -> Result<ResumeBookmark, ApiError> {
    let digest = session::token_digest(event_id, state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::InvalidSession))?;
    sqlx::query_as::<_, ResumeBookmark>("SELECT reservation_id, rpc_id_json FROM mcp_resume_bookmarks WHERE session_id = ? AND event_digest = ? AND expires_at > unixepoch()")
        .bind(&session.id).bind(digest.to_vec()).fetch_optional(&state.pool).await.map_err(crate::management::map_database_error)?.ok_or_else(|| ApiError::new(ApiErrorCode::InvalidSession))
}

async fn save_bookmark(
    pool: &sqlx::SqlitePool,
    state: &AppState,
    session_id: &str,
    event_id: &str,
    reservation_id: &str,
    rpc_id_json: &str,
) -> Result<(), ApiError> {
    let digest = session::token_digest(event_id, state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::InvalidSession))?;
    sqlx::query("INSERT INTO mcp_resume_bookmarks(session_id, event_digest, reservation_id, rpc_id_json, expires_at) VALUES (?, ?, ?, ?, unixepoch()+300) ON CONFLICT(session_id, event_digest) DO UPDATE SET reservation_id=excluded.reservation_id, rpc_id_json=excluded.rpc_id_json, expires_at=excluded.expires_at")
        .bind(session_id).bind(digest.to_vec()).bind(reservation_id).bind(rpc_id_json).execute(pool).await.map_err(crate::management::map_database_error)?;
    Ok(())
}

async fn clear_bookmarks(
    pool: &sqlx::SqlitePool,
    session_id: Option<&str>,
    reservation_id: Option<&str>,
) -> Result<(), ApiError> {
    if let (Some(session_id), Some(reservation_id)) = (session_id, reservation_id) {
        sqlx::query("DELETE FROM mcp_resume_bookmarks WHERE session_id = ? AND reservation_id = ?")
            .bind(session_id)
            .bind(reservation_id)
            .execute(pool)
            .await
            .map_err(crate::management::map_database_error)?;
    }
    Ok(())
}

async fn create_session(
    state: &AppState,
    token_id: &GatewayTokenId,
    pool_id: &PoolId,
    key_id: &UpstreamKeyId,
    upstream_session_id: &str,
    cipher: &CredentialCipher,
) -> Result<String, ApiError> {
    let external_session_id = format!("c7ms_{}", Uuid::new_v4().simple());
    let digest = session::token_digest(&external_session_id, state.config.token_pepper)
        .map_err(|_| ApiError::new(ApiErrorCode::CredentialUnavailable))?;
    let encrypted = cipher
        .encrypt(key_id, pool_id, upstream_session_id)
        .map_err(|_| ApiError::new(ApiErrorCode::CredentialUnavailable))?;
    sqlx::query("INSERT INTO mcp_sessions(id, external_session_digest, gateway_token_id, pool_id, upstream_key_id, upstream_session_ciphertext, upstream_session_nonce, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, datetime('now', ?))")
        .bind(Uuid::new_v4().simple().to_string()).bind(digest.to_vec()).bind(&token_id.0).bind(&pool_id.0).bind(&key_id.0).bind(encrypted.ciphertext).bind(encrypted.nonce.to_vec()).bind(format!("+{MCP_SESSION_LIFETIME_HOURS} hours"))
        .execute(&state.pool).await.map_err(crate::management::map_database_error)?;
    Ok(external_session_id)
}

async fn update_upstream_key_status(
    state: &AppState,
    key_id: &str,
    status: reqwest::StatusCode,
    headers: &HeaderMap,
) {
    match status.as_u16() {
        401 => {
            let _ = sqlx::query("UPDATE upstream_keys SET status='invalid', last_checked_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'), last_failure_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?").bind(key_id).execute(&state.pool).await;
        }
        403 => {
            let _ = sqlx::query("UPDATE upstream_keys SET status='permission_error', last_checked_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'), last_failure_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?").bind(key_id).execute(&state.pool).await;
            let _ = sqlx::query("UPDATE upstream_pools SET enabled=0 WHERE id=(SELECT pool_id FROM upstream_keys WHERE id=?)").bind(key_id).execute(&state.pool).await;
        }
        429 => {
            let retry = retry_after_seconds(headers);
            let _ = sqlx::query("UPDATE upstream_keys SET status='rate_limited', rate_limited_until=datetime('now','+'||?||' seconds'), last_failure_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?").bind(retry).bind(key_id).execute(&state.pool).await;
            let _ = sqlx::query("UPDATE upstream_pools SET rate_limited_until=max(COALESCE(rate_limited_until,''),datetime('now','+'||?||' seconds')) WHERE id=(SELECT pool_id FROM upstream_keys WHERE id=?)").bind(retry).bind(key_id).execute(&state.pool).await;
        }
        500..=599 => {
            let _ = sqlx::query("UPDATE upstream_pools SET consecutive_failures=consecutive_failures+1, breaker_until=CASE WHEN consecutive_failures+1 >= 3 THEN datetime('now','+60 seconds') ELSE breaker_until END WHERE id=(SELECT pool_id FROM upstream_keys WHERE id=?)").bind(key_id).execute(&state.pool).await;
            let _ = sqlx::query("UPDATE upstream_keys SET last_failure_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?").bind(key_id).execute(&state.pool).await;
        }
        200..=299 => {
            let _ = sqlx::query("UPDATE upstream_keys SET last_checked_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'), last_success_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=? AND status='active'").bind(key_id).execute(&state.pool).await;
            let _ = sqlx::query("UPDATE upstream_pools SET consecutive_failures=0, breaker_until=NULL WHERE id=(SELECT pool_id FROM upstream_keys WHERE id=?) AND (breaker_until IS NULL OR unixepoch(breaker_until)<=unixepoch())").bind(key_id).execute(&state.pool).await;
        }
        _ => {
            let _ = sqlx::query("UPDATE upstream_keys SET last_checked_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'), last_failure_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?").bind(key_id).execute(&state.pool).await;
        }
    }
}

pub(crate) fn retry_after_seconds(headers: &HeaderMap) -> i64 {
    let Some(value) = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
    else {
        return 60;
    };
    if let Ok(seconds) = value.parse::<i64>() {
        return seconds.clamp(1, 86_400);
    }
    httpdate::parse_http_date(value)
        .ok()
        .and_then(|deadline| deadline.duration_since(std::time::SystemTime::now()).ok())
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(60)
        .clamp(1, 86_400)
}

fn upstream_url(state: &AppState, path: &str, query: Option<&str>) -> Result<Url, ApiError> {
    if path == "/mcp" && query.is_some() {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let mut url = if path == "/mcp" {
        state.config.context7_mcp_origin.clone()
    } else {
        state.config.context7_rest_origin.clone()
    };
    url.set_path(path);
    url.set_query(sanitize_query(path, query)?.as_deref());
    Ok(url)
}

fn sanitize_query(path: &str, query: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(query) = query else { return Ok(None) };
    if query.len() > 8 * 1024 {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let allowed: &[&str] = match path {
        "/api/v3/search" => &["query", "library", "version", "language", "type"],
        "/api/v3/fetch" => &["url", "query", "tokens", "offset", "type"],
        "/api/v2/libs/search" => &["libraryName", "query", "version", "language", "fast"],
        "/api/v2/context" => &["libraryId", "query", "language", "type"],
        _ => return Err(ApiError::new(ApiErrorCode::NotFound)),
    };
    let mut counts = std::collections::HashMap::<String, usize>::new();
    let mut values = std::collections::HashMap::<String, String>::new();
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let name = name.into_owned();
        let value = value.into_owned();
        if !allowed.contains(&name.as_str())
            || value.is_empty()
            || value.len() > 4096
            || name == "api_key"
        {
            return Err(ApiError::new(ApiErrorCode::InvalidRequest));
        }
        let count = counts.entry(name.clone()).or_default();
        *count += 1;
        if *count > if name == "library" { 4 } else { 1 } {
            return Err(ApiError::new(ApiErrorCode::InvalidRequest));
        }
        values.insert(name.clone(), value.clone());
        serializer.append_pair(&name, &value);
    }
    for required in match path {
        "/api/v3/search" => &["query"][..],
        "/api/v3/fetch" => &["url"][..],
        "/api/v2/libs/search" => &["libraryName", "query"][..],
        "/api/v2/context" => &["libraryId", "query"][..],
        _ => &[],
    } {
        if !values.contains_key(*required) {
            return Err(ApiError::new(ApiErrorCode::InvalidRequest));
        }
    }
    if path == "/api/v3/search" && values.contains_key("version") && !values.contains_key("library")
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    if path == "/api/v3/fetch" {
        if let Some(tokens) = values.get("tokens") {
            tokens
                .parse::<u32>()
                .ok()
                .filter(|v| (100..=10000).contains(v))
                .ok_or_else(|| ApiError::new(ApiErrorCode::InvalidRequest))?;
        }
        if values.contains_key("offset") && values.contains_key("query") {
            return Err(ApiError::new(ApiErrorCode::InvalidRequest));
        }
    }
    if let Some(response_type) = values.get("type")
        && !matches!(response_type.as_str(), "json" | "txt")
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    if let Some(fast) = values.get("fast")
        && !matches!(fast.as_str(), "true" | "false")
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let result = serializer.finish();
    Ok((!result.is_empty()).then_some(result))
}

fn should_forward_request_header(name: &HeaderName) -> bool {
    let name = name.as_str();
    (name.starts_with("mcp-") && name != "mcp-session-id")
        || matches!(
            name,
            "accept" | "content-type" | "last-event-id" | "if-none-match"
        )
}
fn should_forward_response_header(name: &HeaderName) -> bool {
    let name = name.as_str();
    (name.starts_with("mcp-") && name != "mcp-session-id")
        || name.starts_with("ratelimit-")
        || matches!(
            name,
            "content-type"
                | "cache-control"
                | "etag"
                | "retry-after"
                | "www-authenticate"
                | "x-context7-content-check"
        )
}

#[cfg(test)]
mod mock_tests;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{
        AppState, PoolId, UpstreamKeyId, classify_request, retry_after_seconds, sanitize_query,
        select_upstream_key, should_forward_request_header, update_upstream_key_status,
        upstream_url,
    };
    use crate::{
        config::{Config, Environment},
        credentials::CredentialCipher,
        storage,
    };
    use axum::http::{HeaderMap, HeaderName, HeaderValue, Method};
    use axum::response::IntoResponse;
    use reqwest::StatusCode;
    use sqlx::SqlitePool;
    use std::{path::PathBuf, sync::Arc};

    async fn state() -> AppState {
        let pool = storage::connect(std::path::Path::new(":memory:"))
            .await
            .expect("test database connects");
        AppState {
            config: Arc::new(Config {
                bind_addr: "127.0.0.1:8787".parse().expect("socket address parses"),
                database_path: PathBuf::from(":memory:"),
                static_dir: PathBuf::from("web/dist"),
                public_origin: url::Url::parse("http://127.0.0.1:8787").expect("origin parses"),
                context7_mcp_origin: url::Url::parse("https://mcp.context7.com")
                    .expect("upstream MCP origin parses"),
                context7_rest_origin: url::Url::parse("https://context7.com")
                    .expect("upstream REST origin parses"),
                environment: Environment::Test,
                encryption_key: Some([1; 32]),
                token_pepper: Some([2; 32]),
                admin_token_digest: None,
                gateway_event_retention_days: 30,
                linuxdo_oauth: None,
                default_upstream_pool_id: None,
            }),
            pool,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("test HTTP client builds"),
        }
    }

    async fn insert_pool(pool: &SqlitePool, enabled: bool) {
        sqlx::query("INSERT INTO upstream_pools(id, name, authorization_scope, enabled) VALUES ('pool', 'test-pool', 'test-scope', ?)")
            .bind(i64::from(enabled))
            .execute(pool)
            .await
            .expect("pool inserted");
    }

    async fn insert_key(pool: &SqlitePool, id: &str, created: &str, last_used: &str) {
        let key_id = UpstreamKeyId(id.to_owned());
        let pool_id = PoolId("pool".to_owned());
        let encrypted = CredentialCipher::new(Some([1; 32]))
            .expect("encryption key exists")
            .encrypt(&key_id, &pool_id, "context7-test-key")
            .expect("key encrypts");
        sqlx::query("INSERT INTO upstream_keys(id, pool_id, ciphertext, nonce, display_prefix, status, created_at, last_used_at) VALUES (?, 'pool', ?, ?, 'ctx7_x…', 'active', ?, ?)")
            .bind(id)
            .bind(encrypted.ciphertext)
            .bind(encrypted.nonce.to_vec())
            .bind(created)
            .bind(last_used)
            .execute(pool)
            .await
            .expect("key inserted");
    }

    #[test]
    fn rest_query_allowlist_blocks_unrecognized_parameters_and_preserves_known_values() {
        let safe = sanitize_query(
            "/api/v3/search",
            Some("query=tokio&library=Axum&library=SQLx&type=json"),
        )
        .expect("safe query passes");
        assert_eq!(
            safe.as_deref(),
            Some("query=tokio&library=Axum&library=SQLx&type=json")
        );
        assert!(sanitize_query("/api/v3/search", Some("query=test&api_key=secret")).is_err());
        assert!(
            sanitize_query("/api/v3/search", Some("query=test&target_host=127.0.0.1")).is_err()
        );
        assert!(sanitize_query("/api/v3/search", Some("query=x&type=xml")).is_err());
        assert!(
            sanitize_query(
                "/api/v3/fetch",
                Some("url=https%3A%2F%2Fexample.com&type=json&tokens=99")
            )
            .is_err()
        );
        assert!(
            sanitize_query(
                "/api/v2/libs/search",
                Some("libraryName=axum&query=test&fast=yes")
            )
            .is_err()
        );
    }

    #[test]
    fn classifier_validates_2026_headers_and_preserves_legacy_protocol_headers() {
        use serde_json::json;

        let modern = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "query-docs",
                "arguments": {"libraryId": "/upstash/context7", "query": "search"},
                "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}
            }
        }))
        .expect("modern MCP request serializes");
        let mut headers = HeaderMap::new();
        headers.insert(
            "mcp-protocol-version",
            HeaderValue::from_static("2026-07-28"),
        );
        headers.insert("mcp-method", HeaderValue::from_static("tools/call"));
        headers.insert("mcp-name", HeaderValue::from_static("query-docs"));
        let classified = classify_request(&headers, &modern, &Method::POST, "/mcp", true)
            .expect("modern header tuple matches body");
        assert_eq!(classified.0, crate::quota::Operation::McpToolCall);
        headers.insert("mcp-name", HeaderValue::from_static("wrong-tool"));
        assert!(classify_request(&headers, &modern, &Method::POST, "/mcp", true).is_err());

        let legacy = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let mut legacy_headers = HeaderMap::new();
        legacy_headers.insert(
            "mcp-protocol-version",
            HeaderValue::from_static("2025-11-25"),
        );
        let classified = classify_request(&legacy_headers, legacy, &Method::POST, "/mcp", true)
            .expect("legacy negotiated protocol remains supported");
        assert_eq!(classified.0, crate::quota::Operation::McpNotification);
    }

    #[tokio::test]
    async fn upstream_url_is_fixed_to_context7_and_only_known_query_is_forwarded() {
        let state = state().await;
        let url = upstream_url(&state, "/api/v3/search", Some("query=tokio")).expect("url builds");
        assert_eq!(url.host_str(), Some("context7.com"));
        assert_eq!(url.path(), "/api/v3/search");
        assert_eq!(url.query(), Some("query=tokio"));
        assert!(upstream_url(&state, "/api/v3/search", Some("query=test&api_key=bad")).is_err());
        assert!(upstream_url(&state, "/mcp", Some("target=https://example.org")).is_err());
    }

    #[test]
    fn request_header_allowlist_drops_credentials_and_cookies() {
        assert!(should_forward_request_header(&HeaderName::from_static(
            "mcp-protocol-version"
        )));
        assert!(should_forward_request_header(&HeaderName::from_static(
            "last-event-id"
        )));
        assert!(!should_forward_request_header(&HeaderName::from_static(
            "authorization"
        )));
        assert!(!should_forward_request_header(&HeaderName::from_static(
            "cookie"
        )));
        assert!(!should_forward_request_header(&HeaderName::from_static(
            "origin"
        )));
    }

    #[tokio::test]
    async fn scheduler_uses_pool_lru_and_respects_pool_disable() {
        let state = state().await;
        insert_pool(&state.pool, true).await;
        insert_key(
            &state.pool,
            "key-old",
            "2025-01-01T00:00:00Z",
            "2025-01-02T00:00:00Z",
        )
        .await;
        insert_key(
            &state.pool,
            "key-new",
            "2025-01-02T00:00:00Z",
            "2025-01-03T00:00:00Z",
        )
        .await;
        let pool_id = PoolId("pool".to_owned());
        let first = select_upstream_key(&state, &pool_id)
            .await
            .expect("active key selected");
        assert_eq!(first.id, "key-old");
        sqlx::query("UPDATE upstream_keys SET last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?")
            .bind(&first.id).execute(&state.pool).await.expect("first selected key is touched");
        let second = select_upstream_key(&state, &pool_id)
            .await
            .expect("next LRU key selected");
        assert_eq!(second.id, "key-new");

        sqlx::query("UPDATE upstream_pools SET enabled = 0 WHERE id = 'pool'")
            .execute(&state.pool)
            .await
            .expect("pool disabled");
        assert!(select_upstream_key(&state, &pool_id).await.is_err());
    }

    #[tokio::test]
    async fn rate_limit_cools_the_whole_authorized_pool_without_key_fallback() {
        let state = state().await;
        insert_pool(&state.pool, true).await;
        insert_key(
            &state.pool,
            "key-a",
            "2025-01-01T00:00:00Z",
            "2025-01-02T00:00:00Z",
        )
        .await;
        insert_key(
            &state.pool,
            "key-b",
            "2025-01-02T00:00:00Z",
            "2025-01-03T00:00:00Z",
        )
        .await;
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("3600"));
        update_upstream_key_status(&state, "key-a", StatusCode::TOO_MANY_REQUESTS, &headers).await;
        let limited = select_upstream_key(&state, &PoolId("pool".to_owned()))
            .await
            .expect_err("a cooled pool rejects new traffic");
        let response = limited.into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok()),
            Some("3600")
        );
    }

    #[test]
    fn retry_after_supports_delta_seconds_and_http_date() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("25"));
        assert_eq!(retry_after_seconds(&headers), 25);
        headers.insert(
            "retry-after",
            HeaderValue::from_static("Wed, 21 Oct 2037 07:28:00 GMT"),
        );
        assert!(retry_after_seconds(&headers) > 0);
    }
}
