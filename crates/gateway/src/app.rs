use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderName, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, delete, get, patch, post},
};
use sqlx::SqlitePool;
use tower::ServiceBuilder;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

use crate::{
    config::Config,
    health,
    management::{self, AppState},
    oauth, operations, proxy,
};

pub fn router(config: Config, pool: SqlitePool, http: reqwest::Client) -> Router {
    let request_id_header = HeaderName::from_static("x-gateway-request-id");
    let static_dir = config.static_dir.clone();
    let management_state = std::sync::Arc::new(AppState {
        config: std::sync::Arc::new(config),
        pool: pool.clone(),
        http,
    });
    let billing = crate::billing::provider_for(management_state.config.environment);

    let management_routes = Router::new()
        .route(
            "/api/admin/pools",
            get(management::list_pools).post(management::create_pool),
        )
        .route(
            "/api/admin/pools/{pool_id}/status",
            patch(management::set_pool_enabled),
        )
        .route(
            "/api/admin/pools/{pool_id}/keys",
            get(management::list_upstream_keys)
                .post(management::create_upstream_key)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/admin/pools/{pool_id}/keys/bulk",
            post(management::bulk_create_upstream_keys).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/api/admin/pools/{pool_id}/keys/{key_id}/status",
            patch(management::set_upstream_key_status),
        )
        .route(
            "/api/admin/pools/{pool_id}/keys/{key_id}/validate",
            post(management::validate_upstream_key),
        )
        .route(
            "/api/admin/pools/{pool_id}/keys/{key_id}/usage",
            get(operations::key_usage),
        )
        .route(
            "/api/admin/pools/{pool_id}/keys/{key_id}",
            delete(management::delete_upstream_key),
        )
        .route(
            "/api/admin/users",
            get(management::list_users).post(management::create_user),
        )
        .route(
            "/api/admin/users/{user_id}",
            patch(management::set_user_status),
        )
        .route(
            "/api/admin/users/{user_id}/tokens",
            get(management::list_gateway_tokens).post(management::create_gateway_token),
        )
        .route(
            "/api/admin/users/{user_id}/tokens/{token_id}",
            delete(management::revoke_gateway_token),
        )
        .route("/mcp", axum::routing::any(proxy::mcp))
        .route("/api/context7/v3/search", get(proxy::rest_readonly))
        .route("/api/context7/v3/fetch", get(proxy::rest_readonly))
        .route("/api/context7/v2/libs/search", get(proxy::rest_readonly))
        .route("/api/context7/v2/context", get(proxy::rest_readonly))
        .route("/api", any(api_not_found))
        .route("/api/{*path}", any(api_not_found))
        .with_state((*management_state).clone())
        .layer(middleware::from_fn_with_state(
            management_state.clone(),
            admin_csrf,
        ));

    let browser_routes = Router::new()
        .route("/api/user/me", get(oauth::me))
        .route("/api/user/logout", post(oauth::logout))
        .route(
            "/api/user/tokens",
            get(oauth::list_tokens).post(oauth::create_token),
        )
        .route(
            "/api/user/tokens/{token_id}/rotate",
            post(oauth::rotate_token),
        )
        .route("/api/user/tokens/{token_id}", delete(oauth::revoke_token))
        .route(
            "/api/user/service-status",
            get(operations::user_service_status),
        )
        .route("/api/user/setup-guides", get(operations::user_setup_guides))
        .route(
            "/api/user/announcements",
            get(operations::user_announcements),
        )
        .route("/auth/linuxdo", get(oauth::login))
        .route("/auth/linuxdo/callback", get(oauth::callback))
        .merge(crate::quota::user_routes())
        .merge(crate::billing::user_routes(billing.clone()))
        .with_state((*management_state).clone())
        .layer(middleware::from_fn_with_state(
            management_state.clone(),
            user_csrf,
        ));

    let health_routes = Router::new()
        .route("/health/live", get(health::live))
        .route("/health/ready", get(health::ready))
        .with_state(pool);

    let admin_operations_routes = Router::new()
        .route(
            "/api/admin/announcements",
            get(operations::list_announcements).post(operations::create_announcement),
        )
        .route(
            "/api/admin/announcements/{id}",
            patch(operations::set_announcement_enabled),
        )
        .route(
            "/api/admin/settings",
            get(operations::get_settings).patch(operations::update_settings),
        )
        .with_state((*management_state).clone())
        .layer(middleware::from_fn_with_state(
            management_state.clone(),
            admin_csrf,
        ));

    Router::new()
        .merge(health_routes)
        .merge(management_routes)
        .merge(browser_routes)
        .merge(admin_operations_routes)
        .merge(crate::quota::admin_routes().with_state((*management_state).clone()).layer(
            middleware::from_fn_with_state(management_state.clone(), admin_csrf),
        ))
        .merge(crate::billing::admin_routes().with_state((*management_state).clone()).layer(
            middleware::from_fn_with_state(management_state.clone(), admin_csrf),
        ))
        .merge(crate::billing::callback_routes(billing).with_state((*management_state).clone()))
        .nest("/api/admin/auth", crate::admin_auth::routes().with_state(management_state.clone()))
        .fallback_service(
            ServeDir::new(&static_dir)
                .append_index_html_on_directories(true)
                .fallback(ServeFile::new(static_dir.join("index.html"))),
        )
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::new(
                    request_id_header.clone(),
                    MakeRequestUuid,
                ))
                .layer(TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                    tracing::info_span!("http_request", method = %request.method(), path = %request.uri().path())
                }))
                .layer(PropagateRequestIdLayer::new(request_id_header)),
        )
        .layer(middleware::from_fn_with_state(
            management_state.clone(),
            security_headers,
        ))
}

async fn security_headers(
    State(state): State<std::sync::Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let api_route =
        request.uri().path().starts_with("/api/") || request.uri().path().starts_with("/auth/");
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; style-src-attr 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; font-src 'self'; frame-ancestors 'none'; base-uri 'self'; object-src 'none'"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        "referrer-policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response
        .headers_mut()
        .insert("x-frame-options", HeaderValue::from_static("DENY"));
    if api_route {
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
    }
    if state.config.public_origin.scheme() == "https" {
        response.headers_mut().insert(
            "strict-transport-security",
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    response
}

async fn user_csrf(
    State(state): State<std::sync::Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    csrf_guard(&state, request, next, false).await
}

async fn admin_csrf(
    State(state): State<std::sync::Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    csrf_guard(&state, request, next, true).await
}

async fn csrf_guard(state: &AppState, request: Request, next: Next, admin: bool) -> Response {
    if admin && !request.uri().path().starts_with("/api/admin/") {
        return next.run(request).await;
    }
    if !matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        return next.run(request).await;
    }
    if admin && crate::admin_auth::has_development_bearer(&state.config, request.headers()) {
        return next.run(request).await;
    }
    let origin = request
        .headers()
        .get("origin")
        .and_then(|value| value.to_str().ok());
    let fetch_site = request
        .headers()
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok());
    let content_type = request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok());
    let content_length = request
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let has_body = content_length > 0
        || request.headers().contains_key("transfer-encoding")
        || request.headers().contains_key("content-type");
    let valid_body =
        !has_body || content_type.is_some_and(|value| value.starts_with("application/json"));
    let valid = origin
        .is_some_and(|origin| origin == state.config.public_origin.origin().ascii_serialization())
        && fetch_site != Some("cross-site")
        && valid_body;
    if valid {
        next.run(request).await
    } else {
        (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({"error":"csrf_rejected"})),
        )
            .into_response()
    }
}

async fn api_not_found() -> impl axum::response::IntoResponse {
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({ "error": "not_found" })),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::router;
    use crate::{
        config::{Config, Environment},
        storage,
    };
    use axum::{
        body::Body,
        http::{
            Request, StatusCode,
            header::{AUTHORIZATION, CONTENT_TYPE},
        },
    };
    use http_body_util::BodyExt;
    use sha2::{Digest, Sha256};
    use sqlx::SqlitePool;
    use std::path::PathBuf;
    use tower::ServiceExt;

    async fn test_pool() -> SqlitePool {
        storage::connect(std::path::Path::new(":memory:"))
            .await
            .expect("test database connects")
    }

    fn test_config() -> Config {
        Config {
            bind_addr: "127.0.0.1:8787".parse().expect("socket address parses"),
            database_path: PathBuf::from(":memory:"),
            static_dir: PathBuf::from("target/nonexistent-web-dist"),
            public_origin: url::Url::parse("http://127.0.0.1:8787").expect("URL parses"),
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
            linuxdo_oauth: None,
            default_upstream_pool_id: None,
        }
    }

    #[tokio::test]
    async fn health_routes_return_request_id_and_json_status() {
        let pool = test_pool().await;
        let response = router(test_config(), pool, test_http())
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("x-gateway-request-id"));
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body reads")
            .to_bytes();
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("JSON response");
        assert_eq!(payload["status"], "ok");
    }

    #[tokio::test]
    async fn frontend_route_falls_back_to_index_html() {
        let static_dir = tempfile::tempdir().expect("temporary dir is created");
        std::fs::write(
            static_dir.path().join("index.html"),
            "<!doctype html><title>gateway</title>",
        )
        .expect("index is written");
        let mut config = test_config();
        config.static_dir = static_dir.path().to_path_buf();
        let response = router(config, test_pool().await, test_http())
            .oneshot(
                Request::builder()
                    .uri("/console/dashboard")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body reads")
            .to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("gateway"));
    }

    #[tokio::test]
    async fn unknown_api_route_does_not_return_spa_html() {
        let response = router(test_config(), test_pool().await, test_http())
            .oneshot(
                Request::builder()
                    .uri("/api/not-a-real-route")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
    }

    #[tokio::test]
    async fn mcp_endpoint_rejects_unsupported_methods() {
        let response = router(test_config(), test_pool().await, test_http())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/mcp")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn bulk_upstream_key_route_accepts_development_admin_bearer() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO upstream_pools(id, name, authorization_scope) VALUES ('pool-bulk', 'pool-bulk', 'operator-source')",
        )
        .execute(&pool)
        .await
        .expect("pool exists");
        let token = "bootstrap-secret-that-is-long-enough-123";
        let mut config = test_config();
        config.admin_token_digest = Some(Sha256::digest(token.as_bytes()).into());
        let response = router(config, pool, test_http())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/pools/pool-bulk/keys/bulk")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"api_keys":["ctx7sk-route-test-private-value"]}"#,
                    ))
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body reads")
            .to_bytes();
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("JSON response");
        assert_eq!(payload["created"], 1);
        assert_eq!(payload["duplicates"], 0);
        assert_eq!(payload["invalid"], 0);
        assert!(!String::from_utf8_lossy(&body).contains("ctx7sk-route-test-private-value"));
    }

    #[tokio::test]
    async fn malformed_key_json_is_rejected_without_echoing_secret_values() {
        let token = "bootstrap-secret-that-is-long-enough-123";
        let mut config = test_config();
        config.admin_token_digest = Some(Sha256::digest(token.as_bytes()).into());
        let app = router(config, test_pool().await, test_http());
        for (uri, secret, body) in [
            (
                "/api/admin/pools/pool/keys",
                "ctx7sk-shape-secret-single",
                r#"{"api_key":["ctx7sk-shape-secret-single"]}"#,
            ),
            (
                "/api/admin/pools/pool/keys/bulk",
                "ctx7sk-shape-secret-bulk",
                r#"{"api_keys":"ctx7sk-shape-secret-bulk"}"#,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header(AUTHORIZATION, format!("Bearer {token}"))
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .expect("request builds"),
                )
                .await
                .expect("router responds");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = response
                .into_body()
                .collect()
                .await
                .expect("body reads")
                .to_bytes();
            let payload: serde_json::Value = serde_json::from_slice(&body).expect("JSON response");
            assert_eq!(payload["error"], "invalid_request");
            assert!(!String::from_utf8_lossy(&body).contains(secret));
        }

        let oversized = serde_json::json!({
            "api_keys": vec!["ctx7sk-".to_owned() + &"x".repeat(300); 300],
        })
        .to_string();
        assert!(oversized.len() > 64 * 1024);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/pools/pool/keys/bulk")
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(oversized))
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body reads")
            .to_bytes();
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("JSON response");
        assert_eq!(payload["error"], "payload_too_large");
        assert!(!String::from_utf8_lossy(&body).contains("ctx7sk-"));
    }

    fn test_http() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("test HTTP client builds")
    }
}
