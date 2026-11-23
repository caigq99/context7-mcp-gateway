#![allow(clippy::expect_used)]

//! End-to-end proxy tests against a loopback Context7 stand-in. These cover the
//! status transitions and legacy session/SSE semantics that the live upstream
//! smoke cannot safely induce.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;
use url::Url;

use crate::{
    app,
    config::{Config, Environment},
    credentials::{CredentialCipher, GatewayTokenId, PoolId, UpstreamKeyId, issue_gateway_token},
    quota::{self, Invocation, Operation},
    session, storage,
};

#[derive(Clone, Default)]
struct MockUpstream {
    observations: Arc<Mutex<Vec<Observation>>>,
}

#[derive(Clone, Debug)]
struct Observation {
    path: String,
    authorization: Option<String>,
    session: Option<String>,
}

fn record(mock: &MockUpstream, path: &str, headers: &HeaderMap) {
    mock.observations
        .lock()
        .expect("mock observations lock")
        .push(Observation {
            path: path.to_owned(),
            authorization: headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            session: headers
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        });
}

async fn mock_mcp(
    State(mock): State<MockUpstream>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    record(&mock, "/mcp", &headers);
    if method == Method::GET {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 44,
            "result": {"content": [{"type": "text", "text": "resumed"}]}
        });
        return (
            [(header::CONTENT_TYPE, "text/event-stream")],
            format!("id: evt-2\ndata: {payload}\n\n"),
        )
            .into_response();
    }
    if method == Method::DELETE {
        return StatusCode::NO_CONTENT.into_response();
    }
    let message: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let rpc_id = message.get("id").cloned().unwrap_or(json!(0));
    match message.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let mut response = Json(json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "serverInfo": {"name": "context7-mock", "version": "1.0.0"}
                }
            }))
            .into_response();
            response.headers_mut().insert(
                "mcp-session-id",
                HeaderValue::from_static("upstream-session-1"),
            );
            response
        }
        Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
        Some("tools/list") => Json(json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": {"tools": [
                {"name": "resolve-library-id", "description": "mock"},
                {"name": "query-docs", "description": "mock"},
                {"name": "fail-tool", "description": "mock failure"}
            ]}
        }))
        .into_response(),
        Some("tools/call") => {
            let name = message
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let result = if name == "fail-tool" {
                json!({"isError": true, "content": []})
            } else {
                json!({"content": [{"type": "text", "text": "ok"}]})
            };
            let payload = json!({"jsonrpc": "2.0", "id": rpc_id, "result": result});
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                format!("event: message\ndata: {payload}\n\n"),
            )
                .into_response()
        }
        _ => Json(json!({"jsonrpc": "2.0", "id": rpc_id, "result": {}})).into_response(),
    }
}

async fn mock_rest(
    State(mock): State<MockUpstream>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    record(&mock, "/api/v3/search", &headers);
    match query.get("query").map(String::as_str) {
        Some("unauthorized") => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_api_key"})),
        )
            .into_response(),
        Some("forbidden") => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "plan_restricted"})),
        )
            .into_response(),
        Some("limited") => {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": "rate_limited"})),
            )
                .into_response();
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("120"));
            response
        }
        Some("down") => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "temporarily_unavailable"})),
        )
            .into_response(),
        _ => Json(json!({"codeSnippets": [], "infoSnippets": []})).into_response(),
    }
}

struct Fixture {
    app: Router,
    pool: SqlitePool,
    mock: MockUpstream,
    origin: Url,
    _server: tokio::task::JoinHandle<()>,
}

async fn fixture() -> Fixture {
    let pool = storage::connect(std::path::Path::new(":memory:"))
        .await
        .expect("test database connects");
    let mock = MockUpstream::default();
    let router = Router::new()
        .route("/mcp", any(mock_mcp))
        .route("/api/v3/search", get(mock_rest))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock listener binds");
    let address = listener.local_addr().expect("mock address");
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("mock upstream serves");
    });
    let origin = Url::parse(&format!("http://{address}")).expect("mock origin parses");
    let config = Config {
        bind_addr: "127.0.0.1:8787".parse().expect("gateway address parses"),
        database_path: std::path::PathBuf::from(":memory:"),
        static_dir: std::path::PathBuf::from("target/nonexistent-web-dist"),
        public_origin: Url::parse("http://127.0.0.1:8787").expect("public origin parses"),
        context7_mcp_origin: origin.clone(),
        context7_rest_origin: origin.clone(),
        environment: Environment::Test,
        encryption_key: Some([1; 32]),
        token_pepper: Some([2; 32]),
        admin_token_digest: None,
        admin_password: None,
        gateway_event_retention_days: 30,
        linuxdo_oauth: None,
        default_upstream_pool_id: None,
    };
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("HTTP client builds");
    Fixture {
        app: app::router(config, pool.clone(), http),
        pool,
        mock,
        origin,
        _server: server,
    }
}

/// Returns `(gateway_token, upstream_key_id)`.
async fn provision(fixture: &Fixture, suffix: &str) -> (String, String) {
    let pool_id = format!("pool-{suffix}");
    let key_id = format!("key-{suffix}");
    let user_id = format!("user-{suffix}");
    sqlx::query("INSERT INTO upstream_pools(id, name, authorization_scope) VALUES (?, ?, 'mock-authorized')")
        .bind(&pool_id)
        .bind(&pool_id)
        .execute(&fixture.pool)
        .await
        .expect("pool inserts");
    insert_key(fixture, &pool_id, &key_id, suffix).await;
    sqlx::query("INSERT INTO users(id, display_name, upstream_pool_id) VALUES (?, ?, ?)")
        .bind(&user_id)
        .bind(&user_id)
        .bind(&pool_id)
        .execute(&fixture.pool)
        .await
        .expect("user inserts");
    sqlx::query("INSERT INTO user_quota_assignments (user_id, version, requests_per_five_minutes, business_calls_per_hour, daily_units, monthly_units) VALUES (?, 1, 100, 100, 100, 100)")
        .bind(&user_id)
        .execute(&fixture.pool)
        .await
        .expect("quota plan inserts");
    let issued = issue_gateway_token(Some([2; 32])).expect("gateway token issues");
    sqlx::query("INSERT INTO gateway_tokens (id, user_id, token_digest, prefix, name) VALUES (?, ?, ?, ?, ?)")
        .bind(&issued.id.0)
        .bind(&user_id)
        .bind(issued.digest.to_vec())
        .bind(&issued.prefix)
        .bind(format!("token-{suffix}"))
        .execute(&fixture.pool)
        .await
        .expect("gateway token inserts");
    (issued.raw_token, key_id)
}

async fn insert_key(fixture: &Fixture, pool_id: &str, key_id: &str, suffix: &str) {
    let pool = PoolId(pool_id.to_owned());
    let key = UpstreamKeyId(key_id.to_owned());
    let encrypted = CredentialCipher::new(Some([1; 32]))
        .expect("cipher key configured")
        .encrypt(&key, &pool, &format!("ctx7sk-{suffix}"))
        .expect("upstream credential encrypts");
    sqlx::query("INSERT INTO upstream_keys (id, pool_id, ciphertext, nonce, display_prefix, status) VALUES (?, ?, ?, ?, ?, 'active')")
        .bind(key_id)
        .bind(pool_id)
        .bind(encrypted.ciphertext)
        .bind(encrypted.nonce.to_vec())
        .bind(format!("ctx7_{suffix}…"))
        .execute(&fixture.pool)
        .await
        .expect("upstream key inserts");
}

fn request(method: Method, path: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::ACCEPT, "application/json")
        .body(Body::empty())
        .expect("request builds")
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Bytes) {
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("gateway responds");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("response body reads")
        .to_bytes();
    (status, headers, body)
}

async fn search(fixture: &Fixture, token: &str, query: &str) -> (StatusCode, HeaderMap, Bytes) {
    send(
        &fixture.app,
        request(
            Method::GET,
            &format!("/api/context7/v3/search?query={query}"),
            token,
        ),
    )
    .await
}

fn mcp_post(token: &str, session: Option<&str>, payload: Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::CONTENT_TYPE, "application/json")
        .header("mcp-protocol-version", "2025-11-25");
    if let Some(session) = session {
        builder = builder.header("mcp-session-id", session);
    }
    builder
        .body(Body::from(payload.to_string()))
        .expect("MCP request builds")
}

#[tokio::test]
async fn upstream_status_transitions_are_propagated_and_never_retried() {
    let fixture = fixture().await;
    let (token_401, key_401) = provision(&fixture, "unauthorized").await;
    let (token_403, key_403) = provision(&fixture, "forbidden").await;
    let (token_429, key_429) = provision(&fixture, "limited").await;
    let (token_5xx, _) = provision(&fixture, "down").await;

    let (status, headers, _) = search(&fixture, &token_401, "unauthorized").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers.contains_key("x-gateway-request-id"));
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM upstream_keys WHERE id = ?")
            .bind(&key_401)
            .fetch_one(&fixture.pool)
            .await
            .expect("key status reads"),
        "invalid"
    );

    let (status, _, _) = search(&fixture, &token_403, "forbidden").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM upstream_keys WHERE id = ?")
            .bind(&key_403)
            .fetch_one(&fixture.pool)
            .await
            .expect("key status reads"),
        "permission_error"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT enabled FROM upstream_pools WHERE id = 'pool-forbidden'"
        )
        .fetch_one(&fixture.pool)
        .await
        .expect("pool state reads"),
        0
    );

    let (status, headers, _) = search(&fixture, &token_429, "limited").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        headers
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("120")
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM upstream_keys WHERE id = ?")
            .bind(&key_429)
            .fetch_one(&fixture.pool)
            .await
            .expect("key status reads"),
        "rate_limited"
    );
    let observations_before = fixture
        .mock
        .observations
        .lock()
        .expect("observations lock")
        .len();
    let (status, headers, _) = search(&fixture, &token_429, "limited").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        headers
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        Some("120")
    );
    assert_eq!(
        fixture
            .mock
            .observations
            .lock()
            .expect("observations lock")
            .len(),
        observations_before,
        "a cooled pool must not send a second upstream request or rotate keys"
    );

    for _ in 0..3 {
        let (status, _, _) = search(&fixture, &token_5xx, "down").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
    let (status, headers, _) = search(&fixture, &token_5xx, "down").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(headers.contains_key(header::RETRY_AFTER));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM usage_events WHERE user_id = 'user-down'"
        )
        .fetch_one(&fixture.pool)
        .await
        .expect("event count reads"),
        3,
        "the open circuit prevents a fourth upstream send"
    );
}

#[tokio::test]
async fn legacy_sessions_bind_key_and_token_and_business_units_settle_once() {
    let fixture = fixture().await;
    let (token, first_key) = provision(&fixture, "session").await;
    insert_key(
        &fixture,
        "pool-session",
        "key-session-second",
        "session-second",
    )
    .await;

    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "mock", "version": "1"}}
    });
    let (status, headers, _) = send(&fixture.app, mcp_post(&token, None, initialize)).await;
    assert_eq!(status, StatusCode::OK);
    let session = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .expect("gateway session id is returned")
        .to_owned();
    assert!(session.starts_with("c7ms_"));
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT upstream_key_id FROM mcp_sessions WHERE closed_at IS NULL"
        )
        .fetch_one(&fixture.pool)
        .await
        .expect("session key binding reads"),
        first_key
    );

    let (status, _, _) = send(
        &fixture.app,
        mcp_post(
            &token,
            Some(&session),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, _, body) = send(
        &fixture.app,
        mcp_post(
            &token,
            Some(&session),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "query-docs", "arguments": {"libraryId": "/mock", "query": "x"}}
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("\"result\""));
    let (status, _, _) = send(
        &fixture.app,
        mcp_post(
            &token,
            Some(&session),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "fail-tool", "arguments": {}}
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(SUM(success_units), 0) FROM usage_events WHERE user_id = 'user-session' AND operation = 'mcp_tools_call'"
        )
        .fetch_one(&fixture.pool)
        .await
        .expect("settled business units read"),
        1,
        "only the successful tool call consumes one business unit"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_events WHERE user_id = 'user-session' AND operation = 'mcp_tools_call' AND failure_code IS NOT NULL")
            .fetch_one(&fixture.pool)
            .await
            .expect("failed tool event reads"),
        1
    );
    let observations = fixture
        .mock
        .observations
        .lock()
        .expect("observations lock")
        .clone();
    let mcp: Vec<_> = observations
        .iter()
        .filter(|item| item.path == "/mcp")
        .collect();
    assert_eq!(mcp.len(), 4);
    assert!(
        mcp.iter()
            .all(|item| item.authorization.as_deref() == Some("Bearer ctx7sk-session"))
    );
    assert!(
        mcp.iter()
            .skip(1)
            .all(|item| item.session.as_deref() == Some("upstream-session-1"))
    );
    assert_eq!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT last_used_at FROM upstream_keys WHERE id = 'key-session-second'"
        )
        .fetch_one(&fixture.pool)
        .await
        .expect("second key state reads"),
        None,
        "a bound session must not switch to the second pool key"
    );

    let (other_token, _) = provision(&fixture, "other").await;
    let before = fixture
        .mock
        .observations
        .lock()
        .expect("observations lock")
        .len();
    let (status, _, _) = send(
        &fixture.app,
        mcp_post(
            &token,
            Some(&session),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "sanity: original token still works"
    );
    let other_request = mcp_post(
        &other_token,
        Some(&session),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    let (status, _, _) = send(&fixture.app, other_request).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        fixture
            .mock
            .observations
            .lock()
            .expect("observations lock")
            .len(),
        before + 1,
        "a foreign token cannot use another token's upstream session"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COALESCE(SUM(in_flight), 0) FROM upstream_keys")
            .fetch_one(&fixture.pool)
            .await
            .expect("lease counters read"),
        0
    );
}

#[tokio::test]
async fn resumed_sse_settles_the_original_business_reservation_once() {
    let fixture = fixture().await;
    let (token, _) = provision(&fixture, "resume").await;
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "mock", "version": "1"}}
    });
    let (status, headers, _) = send(&fixture.app, mcp_post(&token, None, initialize)).await;
    assert_eq!(status, StatusCode::OK);
    let session = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .expect("gateway session id")
        .to_owned();
    let internal_session: String = sqlx::query_scalar("SELECT id FROM mcp_sessions")
        .fetch_one(&fixture.pool)
        .await
        .expect("session row reads");
    let token_id = GatewayTokenId(
        sqlx::query_scalar::<_, String>("SELECT id FROM gateway_tokens LIMIT 1")
            .fetch_one(&fixture.pool)
            .await
            .expect("token id reads"),
    );
    let pool_id = PoolId("pool-resume".to_owned());
    let key_id = UpstreamKeyId("key-resume".to_owned());
    let reservation = quota::reserve(
        &fixture.pool,
        Invocation {
            user_id: "user-resume",
            token_id: &token_id,
            pool_id: &pool_id,
            key_id: &key_id,
            operation: Operation::McpToolCall,
            request_id: "interrupted-request",
        },
    )
    .await
    .expect("the interrupted tool call was admitted");
    let reservation_id = reservation
        .id()
        .expect("reservation id")
        .simple()
        .to_string();
    reservation.detach();
    let event_digest = session::token_digest("evt-1", Some([2; 32])).expect("bookmark digest");
    sqlx::query("INSERT INTO mcp_resume_bookmarks (session_id, event_digest, reservation_id, rpc_id_json, expires_at) VALUES (?, ?, ?, '44', unixepoch() + 300)")
        .bind(&internal_session)
        .bind(event_digest.to_vec())
        .bind(&reservation_id)
        .execute(&fixture.pool)
        .await
        .expect("resume bookmark inserts");

    let request = Request::builder()
        .method(Method::GET)
        .uri("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::ACCEPT, "text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .header("mcp-session-id", &session)
        .header("last-event-id", "evt-1")
        .body(Body::empty())
        .expect("resume request builds");
    let (status, _, body) = send(&fixture.app, request).await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("\"resumed\""));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM quota_reservations")
            .fetch_one(&fixture.pool)
            .await
            .expect("reservation count reads"),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(SUM(success_units), 0) FROM usage_events WHERE user_id = 'user-resume'"
        )
        .fetch_one(&fixture.pool)
        .await
        .expect("business units read"),
        1,
        "the resumed GET must not charge a second business unit"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM usage_events WHERE user_id = 'user-resume' AND operation = 'mcp_tools_call'"
        )
        .fetch_one(&fixture.pool)
        .await
        .expect("tool event count reads"),
        1
    );
}

#[tokio::test]
async fn failed_settlement_ignores_json_rpc_success_with_wrong_id() {
    let fixture = fixture().await;
    let (token, _) = provision(&fixture, "wrong-id").await;
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "mock", "version": "1"}}
    });
    let (status, headers, _) = send(&fixture.app, mcp_post(&token, None, initialize)).await;
    assert_eq!(status, StatusCode::OK);
    let session = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .expect("gateway session id")
        .to_owned();
    let (status, _, _) = send(
        &fixture.app,
        mcp_post(
            &token,
            Some(&session),
            json!({
                "jsonrpc": "2.0",
                "id": 99,
                "method": "tools/call",
                "params": {"name": "fail-tool", "arguments": {}}
            }),
        ),
    )
    .await;
    // The mock returns its own id; the gateway forwards the response unchanged.
    assert_eq!(status, StatusCode::OK);
    let (units, failure): (i64, Option<String>) = sqlx::query_as(
        "SELECT success_units, failure_code FROM usage_events WHERE user_id = 'user-wrong-id' AND operation = 'mcp_tools_call'",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("tool event reads");
    assert_eq!(units, 0);
    assert_eq!(failure.as_deref(), Some("mcp_error"));
    assert_eq!(fixture.origin.host_str(), Some("127.0.0.1"));
}
