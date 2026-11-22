#![allow(clippy::expect_used)]

use std::{path::Path, sync::Arc};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    response::Response,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use url::Url;
use webauthn_authenticator_rs::{WebauthnAuthenticator, softpasskey::SoftPasskey};

use super::{CookieKind, EnrollmentMode, authorize_admin, issue_enrollment_ticket, routes};
use crate::{
    config::{Config, Environment},
    management::AppState,
    storage,
};

const ORIGIN: &str = "https://gateway.example";
const ADMIN_BEARER: &str = "long-development-test-token-for-management";

struct TestGateway {
    state: Arc<AppState>,
    app: Router,
}

impl TestGateway {
    async fn new() -> Self {
        let state = Arc::new(AppState {
            config: Arc::new(Config {
                bind_addr: "127.0.0.1:8787".parse().expect("valid socket address"),
                database_path: ":memory:".into(),
                static_dir: "target/nonexistent-web-dist".into(),
                public_origin: Url::parse(ORIGIN).expect("valid test origin"),
                context7_mcp_origin: url::Url::parse("https://mcp.context7.com")
                    .expect("upstream MCP origin parses"),
                context7_rest_origin: url::Url::parse("https://context7.com")
                    .expect("upstream REST origin parses"),
                environment: Environment::Production,
                encryption_key: Some([1; 32]),
                token_pepper: Some([2; 32]),
                admin_token_digest: Some(super::digest(ADMIN_BEARER)),
                gateway_event_retention_days: 30,
                linuxdo_oauth: None,
                default_upstream_pool_id: None,
            }),
            http: reqwest::Client::new(),
            pool: storage::connect(Path::new(":memory:"))
                .await
                .expect("administrator auth test database opens"),
        });
        let app = Router::new()
            .nest("/api/admin/auth", routes())
            .with_state(Arc::clone(&state));
        Self { state, app }
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        origin: &str,
        cookies: &[&str],
        body: Value,
    ) -> Response {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("/api/admin/auth{path}"))
            .header(header::ORIGIN, origin)
            .header(header::CONTENT_TYPE, "application/json");
        if !cookies.is_empty() {
            request = request.header(header::COOKIE, cookies.join("; "));
        }
        self.app
            .clone()
            .oneshot(
                request
                    .body(Body::from(body.to_string()))
                    .expect("test request builds"),
            )
            .await
            .expect("administrator route responds")
    }

    async fn register_start(&self, code: Option<&str>, cookie: Option<&str>) -> Response {
        self.request(
            "POST",
            "/register/start",
            ORIGIN,
            &cookie.into_iter().collect::<Vec<_>>(),
            json!({ "name": "Security key", "enrollment_code": code }),
        )
        .await
    }

    async fn register(
        &self,
        authenticator: &mut SoftPasskey,
        code: Option<&str>,
        session: Option<&str>,
    ) -> String {
        let response = self.register_start(code, session).await;
        assert_eq!(response.status(), StatusCode::OK);
        let ceremony = cookie(&response, CookieKind::Registration);
        let credential = registration_credential(authenticator, response).await;
        let mut cookies = vec![ceremony.as_str()];
        cookies.extend(session);
        let response = self
            .request("POST", "/register/finish", ORIGIN, &cookies, credential)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        cookie(&response, CookieKind::Session)
    }

    async fn enroll(&self, authenticator: &mut SoftPasskey) -> String {
        let code = issue_enrollment_ticket(&self.state.pool, EnrollmentMode::Initial)
            .await
            .expect("first account enrollment is available");
        self.register(authenticator, Some(&code), None).await
    }

    async fn session_authenticated(&self, cookie: &str) -> bool {
        let response = self
            .request("GET", "/session", ORIGIN, &[cookie], Value::Null)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .expect("cache policy"),
            "no-store"
        );
        body_json(response).await["authenticated"]
            .as_bool()
            .expect("session reports status")
    }

    async fn login_challenge(
        &self,
        authenticator: &mut SoftPasskey,
        client_origin: &str,
    ) -> (String, Value) {
        let response = self
            .request("POST", "/login/start", ORIGIN, &[], Value::Null)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let ceremony = cookie(&response, CookieKind::Login);
        let challenge = serde_json::from_value(body_json(response).await)
            .expect("a login challenge has native WebAuthn options");
        let credential = authenticator
            .do_authentication(
                Url::parse(client_origin).expect("valid client origin"),
                challenge,
            )
            .expect("soft authenticator signs the login challenge");
        (ceremony, json!({"credential": credential}))
    }
}

fn cookie(response: &Response, kind: CookieKind) -> String {
    let name = kind.name(true);
    let header = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&format!("{name}=")))
        .expect("administrator cookie is set");
    assert!(header.contains("; Path=/; HttpOnly; SameSite=Strict;"));
    assert!(header.ends_with("; Secure"));
    header
        .split(';')
        .next()
        .expect("cookie contains name and value")
        .to_owned()
}

async fn body_json(response: Response) -> Value {
    let body = response
        .into_body()
        .collect()
        .await
        .expect("reads response")
        .to_bytes();
    serde_json::from_slice(&body).expect("response contains JSON")
}

async fn registration_credential(authenticator: &mut SoftPasskey, response: Response) -> Value {
    let challenge = serde_json::from_value(body_json(response).await)
        .expect("registration challenge has native WebAuthn options");
    let credential = authenticator
        .do_registration(Url::parse(ORIGIN).expect("valid test origin"), challenge)
        .expect("soft authenticator registers a passkey");
    json!({"credential": credential})
}

#[tokio::test]
async fn registration_ticket_and_assertions_are_single_use() {
    let gateway = TestGateway::new().await;
    let mut authenticator = SoftPasskey::new(true);
    let code = issue_enrollment_ticket(&gateway.state.pool, EnrollmentMode::Initial)
        .await
        .expect("issue enrollment code");
    let response = gateway.register_start(Some(&code), None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let ceremony = cookie(&response, CookieKind::Registration);
    let credential = registration_credential(&mut authenticator, response).await;
    let cookies = [ceremony.as_str()];
    let (a, b) = tokio::join!(
        gateway.request(
            "POST",
            "/register/finish",
            ORIGIN,
            &cookies,
            credential.clone()
        ),
        gateway.request("POST", "/register/finish", ORIGIN, &cookies, credential),
    );
    let (success, duplicate) = if a.status() == StatusCode::OK {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(success.status(), StatusCode::OK);
    assert_eq!(duplicate.status(), StatusCode::UNAUTHORIZED);
    let old_session = cookie(&success, CookieKind::Session);
    assert!(gateway.session_authenticated(&old_session).await);
    assert_eq!(
        gateway.register_start(Some(&code), None).await.status(),
        StatusCode::UNAUTHORIZED
    );

    let (ceremony, assertion) = gateway.login_challenge(&mut authenticator, ORIGIN).await;
    let wrong_browser = gateway
        .request("POST", "/login/finish", ORIGIN, &[], assertion.clone())
        .await;
    assert_eq!(wrong_browser.status(), StatusCode::UNAUTHORIZED);
    let success = gateway
        .request(
            "POST",
            "/login/finish",
            ORIGIN,
            &[&ceremony, &old_session],
            assertion.clone(),
        )
        .await;
    assert_eq!(success.status(), StatusCode::OK);
    let current_session = cookie(&success, CookieKind::Session);
    assert_ne!(current_session, old_session);
    assert!(!gateway.session_authenticated(&old_session).await);
    assert!(gateway.session_authenticated(&current_session).await);
    let replay = gateway
        .request("POST", "/login/finish", ORIGIN, &[&ceremony], assertion)
        .await;
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);

    let logout = gateway
        .request("POST", "/logout", ORIGIN, &[&current_session], Value::Null)
        .await;
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        cookie(&logout, CookieKind::Session),
        "__Host-c7gw-admin-session="
    );
    assert!(!gateway.session_authenticated(&current_session).await);
}

#[tokio::test]
async fn requires_expected_browser_origin_and_unexpired_ceremony() {
    let gateway = TestGateway::new().await;
    let mut authenticator = SoftPasskey::new(true);
    let session = gateway.enroll(&mut authenticator).await;
    let (ceremony, assertion) = gateway.login_challenge(&mut authenticator, ORIGIN).await;
    let denied = gateway
        .request(
            "POST",
            "/login/finish",
            "https://elsewhere.example",
            &[&ceremony],
            assertion.clone(),
        )
        .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let denied = gateway
        .request(
            "POST",
            "/logout",
            "https://elsewhere.example",
            &[&session],
            Value::Null,
        )
        .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(gateway.session_authenticated(&session).await);
    sqlx::query("UPDATE admin_ceremonies SET expires_at = unixepoch()")
        .execute(&gateway.state.pool)
        .await
        .expect("expire outstanding challenge");
    let expired = gateway
        .request("POST", "/login/finish", ORIGIN, &[&ceremony], assertion)
        .await;
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);
    let (ceremony, assertion) = gateway
        .login_challenge(&mut authenticator, "https://gateway.example:444")
        .await;
    let forged_origin = gateway
        .request("POST", "/login/finish", ORIGIN, &[&ceremony], assertion)
        .await;
    assert_eq!(forged_origin.status(), StatusCode::UNAUTHORIZED);
    assert!(gateway.session_authenticated(&session).await);
    sqlx::query("UPDATE admin_sessions SET expires_at = unixepoch()")
        .execute(&gateway.state.pool)
        .await
        .expect("expire browser session");
    assert!(!gateway.session_authenticated(&session).await);
}

#[tokio::test]
async fn protects_last_passkey_and_revokes_previous_access_after_recovery() {
    let gateway = TestGateway::new().await;
    let mut first_authenticator = SoftPasskey::new(true);
    let old_session = gateway.enroll(&mut first_authenticator).await;
    let entries = gateway
        .request("GET", "/passkeys", ORIGIN, &[&old_session], Value::Null)
        .await;
    let entries = body_json(entries).await;
    let first_id = entries[0]["id"].as_str().expect("first passkey listed");
    let remove_url = format!("/passkeys/{first_id}");
    let result = gateway
        .request("DELETE", &remove_url, ORIGIN, &[&old_session], Value::Null)
        .await;
    assert_eq!(result.status(), StatusCode::CONFLICT);
    assert!(gateway.session_authenticated(&old_session).await);

    let mut other_authenticator = SoftPasskey::new(true);
    let other_session = gateway
        .register(&mut other_authenticator, None, Some(&old_session))
        .await;
    assert!(!gateway.session_authenticated(&old_session).await);
    let (ceremony, assertion) = gateway
        .login_challenge(&mut first_authenticator, ORIGIN)
        .await;
    let result = gateway
        .request("POST", "/login/finish", ORIGIN, &[&ceremony], assertion)
        .await;
    assert_eq!(result.status(), StatusCode::OK);
    let first_session = cookie(&result, CookieKind::Session);
    let result = gateway
        .request(
            "DELETE",
            &remove_url,
            ORIGIN,
            &[&other_session],
            Value::Null,
        )
        .await;
    assert_eq!(result.status(), StatusCode::NO_CONTENT);
    assert!(!gateway.session_authenticated(&first_session).await);
    assert!(gateway.session_authenticated(&other_session).await);

    let recovery = issue_enrollment_ticket(&gateway.state.pool, EnrollmentMode::Recovery)
        .await
        .expect("operator issues a recovery ticket");
    let (ceremony, assertion) = gateway
        .login_challenge(&mut other_authenticator, ORIGIN)
        .await;
    let unfinished = gateway.register_start(Some(&recovery), None).await;
    assert_eq!(unfinished.status(), StatusCode::OK);
    assert!(gateway.session_authenticated(&other_session).await);

    let mut recovered_authenticator = SoftPasskey::new(true);
    let recovered_session = gateway
        .register(&mut recovered_authenticator, Some(&recovery), None)
        .await;
    assert!(gateway.session_authenticated(&recovered_session).await);
    assert!(!gateway.session_authenticated(&other_session).await);
    let login = gateway
        .request("POST", "/login/finish", ORIGIN, &[&ceremony], assertion)
        .await;
    assert_eq!(login.status(), StatusCode::UNAUTHORIZED);
    let entries = gateway
        .request(
            "GET",
            "/passkeys",
            ORIGIN,
            &[&recovered_session],
            Value::Null,
        )
        .await;
    assert_eq!(
        body_json(entries)
            .await
            .as_array()
            .expect("passkey list")
            .len(),
        1
    );
    let audited = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM admin_audit_events WHERE action = 'admin.passkey.recover'",
    )
    .fetch_one(&gateway.state.pool)
    .await
    .expect("count recovery audit entries");
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn development_bearer_and_user_cookie_cannot_authorize_production_administration() {
    let gateway = TestGateway::new().await;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {ADMIN_BEARER}")
            .parse()
            .expect("valid request header"),
    );
    headers.insert(
        header::COOKIE,
        "c7gw_session=user-session".parse().expect("valid cookie"),
    );
    assert!(authorize_admin(&gateway.state, &headers).await.is_err());
    let mut authenticator = SoftPasskey::new(true);
    let session = gateway.enroll(&mut authenticator).await;
    headers.insert(
        header::COOKIE,
        session.parse().expect("valid administrator cookie"),
    );
    assert_eq!(
        authorize_admin(&gateway.state, &headers)
            .await
            .expect("administrator can access management"),
        "passkey-admin"
    );
}
