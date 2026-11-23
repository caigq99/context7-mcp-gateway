use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::VERSION;

#[derive(Serialize)]
struct LiveStatus {
    status: &'static str,
    version: &'static str,
}

#[derive(Serialize)]
struct ReadyStatus {
    status: &'static str,
    database: &'static str,
    version: &'static str,
}

pub async fn live() -> impl IntoResponse {
    Json(LiveStatus {
        status: "ok",
        version: VERSION,
    })
}

pub async fn ready(
    axum::extract::State(pool): axum::extract::State<SqlitePool>,
) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&pool)
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(ReadyStatus {
                status: "ready",
                database: "ok",
                version: VERSION,
            }),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyStatus {
                status: "not_ready",
                database: "unavailable",
                version: VERSION,
            }),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::{
        app::router,
        config::{Config, Environment},
        storage,
    };
    use std::path::PathBuf;

    #[tokio::test]
    async fn ready_endpoint_checks_database_connectivity() {
        let pool = storage::connect(std::path::Path::new(":memory:"))
            .await
            .expect("test pool connects");
        let config = Config {
            bind_addr: "127.0.0.1:8787".parse().expect("socket address parses"),
            database_path: PathBuf::from(":memory:"),
            static_dir: PathBuf::from("web/dist"),
            public_origin: url::Url::parse("http://127.0.0.1:8787").expect("URL parses"),
            context7_mcp_origin: url::Url::parse("https://mcp.context7.com")
                .expect("upstream MCP origin parses"),
            context7_rest_origin: url::Url::parse("https://context7.com")
                .expect("upstream REST origin parses"),
            environment: Environment::Test,
            encryption_key: None,
            token_pepper: None,
            admin_token_digest: None,
            admin_password: None,
            gateway_event_retention_days: 30,
            linuxdo_oauth: None,
            default_upstream_pool_id: None,
        };
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("test HTTP client builds");
        let response = router(config, pool, http)
            .oneshot(
                Request::builder()
                    .uri("/health/ready")
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
        let payload: serde_json::Value = serde_json::from_slice(&body).expect("JSON response");
        assert_eq!(payload["database"], "ok");
    }
}
