use std::{env, net::SocketAddr, path::PathBuf, str::FromStr};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use url::Url;
use zeroize::Zeroizing;

pub const CONTEXT7_MCP_ORIGIN: &str = "https://mcp.context7.com";
pub const CONTEXT7_REST_ORIGIN: &str = "https://context7.com";

#[derive(PartialEq, Eq)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub database_path: PathBuf,
    pub static_dir: PathBuf,
    pub public_origin: Url,
    pub context7_mcp_origin: Url,
    pub context7_rest_origin: Url,
    pub environment: Environment,
    pub encryption_key: Option<[u8; 32]>,
    pub token_pepper: Option<[u8; 32]>,
    pub admin_token_digest: Option<[u8; 32]>,
    pub admin_password: Option<AdminPasswordConfig>,
    pub gateway_event_retention_days: u16,
    pub linuxdo_oauth: Option<LinuxDoOAuthConfig>,
    pub default_upstream_pool_id: Option<String>,
}

#[derive(PartialEq, Eq)]
pub struct AdminPasswordConfig {
    pub username: String,
    password_hash: Zeroizing<String>,
    session_version: [u8; 32],
}

impl std::fmt::Debug for AdminPasswordConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdminPasswordConfig")
            .field("username", &self.username)
            .field("password_hash", &"[REDACTED]")
            .finish()
    }
}

impl AdminPasswordConfig {
    pub fn verify(&self, username: &str, password: &str) -> bool {
        let password_ok = PasswordHash::new(self.password_hash.as_str())
            .ok()
            .is_some_and(|hash| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &hash)
                    .is_ok()
            });
        let username_ok = bool::from(self.username.as_bytes().ct_eq(username.as_bytes()));
        username_ok & password_ok
    }

    pub(crate) fn session_version(&self) -> &[u8; 32] {
        &self.session_version
    }

    pub(crate) fn build(
        username: &str,
        password: &str,
        pepper: Option<&[u8; 32]>,
        environment: Environment,
    ) -> Result<Self, ConfigError> {
        let username = username.trim();
        let minimum_password_length = if environment == Environment::Production {
            12
        } else {
            8
        };
        if username.is_empty()
            || username.len() > 64
            || username.chars().any(char::is_control)
            || password.len() < minimum_password_length
            || password.len() > 256
            || password.chars().any(char::is_control)
        {
            return Err(ConfigError::InvalidAdminPassword);
        }
        let mut salt_bytes = [0_u8; 16];
        getrandom::fill(&mut salt_bytes).map_err(|_| ConfigError::InvalidAdminPassword)?;
        let salt =
            SaltString::encode_b64(&salt_bytes).map_err(|_| ConfigError::InvalidAdminPassword)?;
        let password_hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|_| ConfigError::InvalidAdminPassword)?
            .to_string();
        Ok(Self {
            username: username.to_owned(),
            password_hash: Zeroizing::new(password_hash),
            session_version: password_session_version(username, password, pepper)?,
        })
    }
}

#[derive(PartialEq, Eq)]
pub struct LinuxDoOAuthConfig {
    pub client_id: String,
    pub client_secret: Zeroizing<String>,
    pub redirect_url: Url,
    pub authorize_url: Url,
    pub token_url: Url,
    pub userinfo_url: Url,
    pub scope: String,
}

impl std::fmt::Debug for LinuxDoOAuthConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinuxDoOAuthConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("redirect_url", &self.redirect_url)
            .field("authorize_url", &self.authorize_url)
            .field("token_url", &self.token_url)
            .field("userinfo_url", &self.userinfo_url)
            .field("scope", &self.scope)
            .finish()
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("database_path", &self.database_path)
            .field("static_dir", &self.static_dir)
            .field("public_origin", &self.public_origin)
            .field("context7_mcp_origin", &self.context7_mcp_origin)
            .field("context7_rest_origin", &self.context7_rest_origin)
            .field("environment", &self.environment)
            .field("encryption_key_configured", &self.encryption_key.is_some())
            .field("token_pepper_configured", &self.token_pepper.is_some())
            .field("admin_token_configured", &self.admin_token_digest.is_some())
            .field("admin_password_configured", &self.admin_password.is_some())
            .field(
                "gateway_event_retention_days",
                &self.gateway_event_retention_days,
            )
            .field("linuxdo_oauth_enabled", &self.linuxdo_oauth.is_some())
            .field(
                "default_pool_configured",
                &self.default_upstream_pool_id.is_some(),
            )
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Environment {
    Development,
    Test,
    Production,
}

impl Environment {
    fn parse(raw: Option<String>) -> Result<Self, ConfigError> {
        match raw.as_deref().unwrap_or("development") {
            "development" | "dev" => Ok(Self::Development),
            "test" => Ok(Self::Test),
            "production" | "prod" => Ok(Self::Production),
            value => Err(ConfigError::InvalidEnvironment(value.to_owned())),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("APP_ENV must be development, test, or production")]
    InvalidEnvironment(String),
    #[error("GATEWAY_BIND must be a valid socket address")]
    InvalidBindAddress,
    #[error("DATABASE_PATH must not be empty")]
    EmptyDatabasePath,
    #[error("PUBLIC_ORIGIN must be a valid absolute HTTP(S) origin")]
    InvalidPublicOrigin,
    #[error("PUBLIC_ORIGIN is required in production")]
    MissingPublicOrigin,
    #[error("GATEWAY_ENCRYPTION_KEY and GATEWAY_TOKEN_PEPPER must be 32-byte base64url values")]
    InvalidCryptographicSecret,
    #[error("GATEWAY_ADMIN_TOKEN must contain at least 32 characters")]
    InvalidAdminToken,
    #[error("production requires GATEWAY_ENCRYPTION_KEY and GATEWAY_TOKEN_PEPPER")]
    MissingProductionSecrets,
    #[error("GATEWAY_ADMIN_TOKEN is only available in development and test")]
    ProductionAdminBearer,
    #[error(
        "GATEWAY_ADMIN_USERNAME and GATEWAY_ADMIN_PASSWORD must be set together; username is 1-64 characters and password must be 8+ characters (12+ in production)"
    )]
    InvalidAdminPassword,
    #[error("LINUXDO_OAUTH_ENABLED must be true or false")]
    InvalidOAuthEnabled,
    #[error("Linux.do OAuth requires client ID, client secret, redirect URL, and HTTPS endpoints")]
    InvalidOAuthConfiguration,
    #[error("LINUXDO_OAUTH_REDIRECT_URL must equal PUBLIC_ORIGIN with /auth/linuxdo/callback")]
    MismatchedOAuthCallback,
    #[error("production requires Linux.do OAuth configuration and a default upstream pool")]
    MissingProductionOAuth,
    #[error("PUBLIC_ORIGIN must use HTTPS in production")]
    ProductionRequiresHttps,
    #[error("PUBLIC_ORIGIN must use a DNS hostname for administrator passkeys in production")]
    ProductionRequiresDnsName,
    #[error("GATEWAY_EVENT_RETENTION_DAYS must be an integer from 1 through 365")]
    InvalidEventRetentionDays,
    #[error("CONTEXT7_MCP_ORIGIN/CONTEXT7_REST_ORIGIN may not be overridden in production")]
    ProductionUpstreamOverride,
    #[error(
        "upstream origin overrides must be absolute HTTP(S) origins without credentials or paths"
    )]
    InvalidUpstreamOrigin,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| env::var(name).ok())
    }

    pub fn admin_passkey_enabled(&self) -> bool {
        self.public_origin.domain().is_some()
            && (self.public_origin.scheme() == "https"
                || (self.environment != Environment::Production
                    && self.public_origin.scheme() == "http"
                    && self.public_origin.domain() == Some("localhost")))
    }

    pub fn admin_password_enabled(&self) -> bool {
        self.admin_password.is_some()
    }

    fn from_lookup(mut get: impl FnMut(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let environment = Environment::parse(get("APP_ENV"))?;
        let bind_addr = get("GATEWAY_BIND")
            .map(|value| SocketAddr::from_str(&value).map_err(|_| ConfigError::InvalidBindAddress))
            .transpose()?
            .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8787)));

        let database_path = get("DATABASE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data/context7_gateway.sqlite3"));
        if database_path.as_os_str().is_empty() {
            return Err(ConfigError::EmptyDatabasePath);
        }

        let static_dir = get("WEB_DIST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("web/dist"));

        let gateway_event_retention_days = get("GATEWAY_EVENT_RETENTION_DAYS")
            .map(|value| {
                value
                    .parse::<u16>()
                    .ok()
                    .filter(|days| (1..=365).contains(days))
                    .ok_or(ConfigError::InvalidEventRetentionDays)
            })
            .transpose()?
            .unwrap_or(30);

        let public_origin = match get("PUBLIC_ORIGIN") {
            Some(value) => parse_public_origin(&value)?,
            None if environment == Environment::Production => {
                return Err(ConfigError::MissingPublicOrigin);
            }
            None => {
                Url::parse("http://127.0.0.1:8787").map_err(|_| ConfigError::InvalidPublicOrigin)?
            }
        };
        let context7_mcp_origin =
            parse_upstream_origin(get("CONTEXT7_MCP_ORIGIN"), CONTEXT7_MCP_ORIGIN, environment)?;
        let context7_rest_origin = parse_upstream_origin(
            get("CONTEXT7_REST_ORIGIN"),
            CONTEXT7_REST_ORIGIN,
            environment,
        )?;

        let encryption_key = get("GATEWAY_ENCRYPTION_KEY")
            .map(|value| decode_32_byte_secret(&value))
            .transpose()?;
        let token_pepper = get("GATEWAY_TOKEN_PEPPER")
            .map(|value| decode_32_byte_secret(&value))
            .transpose()?;
        let admin_token = get("GATEWAY_ADMIN_TOKEN")
            .map(Zeroizing::new)
            .filter(|value| !value.trim().is_empty());
        if admin_token.is_some() && environment == Environment::Production {
            return Err(ConfigError::ProductionAdminBearer);
        }
        let admin_token_digest = admin_token
            .map(|value| {
                if value.trim().len() < 32 {
                    Err(ConfigError::InvalidAdminToken)
                } else {
                    Ok(Sha256::digest(value.as_bytes()).into())
                }
            })
            .transpose()?;
        let admin_username = get("GATEWAY_ADMIN_USERNAME")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let admin_password = get("GATEWAY_ADMIN_PASSWORD")
            .filter(|value| !value.is_empty())
            .map(Zeroizing::new);
        let admin_password = match (admin_username, admin_password) {
            (Some(username), Some(password)) => Some(AdminPasswordConfig::build(
                &username,
                password.as_str(),
                token_pepper.as_ref(),
                environment,
            )?),
            (None, None) => None,
            _ => return Err(ConfigError::InvalidAdminPassword),
        };

        let default_upstream_pool_id = get("DEFAULT_UPSTREAM_POOL_ID")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let linuxdo_oauth = parse_linuxdo_oauth(&mut get, environment)?;
        if let Some(oauth) = &linuxdo_oauth
            && (oauth.redirect_url.origin() != public_origin.origin()
                || oauth.redirect_url.path() != "/auth/linuxdo/callback"
                || oauth.redirect_url.query().is_some())
        {
            return Err(ConfigError::MismatchedOAuthCallback);
        }

        if environment == Environment::Production {
            if public_origin.scheme() != "https" {
                return Err(ConfigError::ProductionRequiresHttps);
            }
            if public_origin.domain().is_none() {
                return Err(ConfigError::ProductionRequiresDnsName);
            }
            if encryption_key.is_none() || token_pepper.is_none() {
                return Err(ConfigError::MissingProductionSecrets);
            }
            if linuxdo_oauth.is_none() || default_upstream_pool_id.is_none() {
                return Err(ConfigError::MissingProductionOAuth);
            }
        }

        Ok(Self {
            bind_addr,
            database_path,
            static_dir,
            public_origin,
            context7_mcp_origin,
            context7_rest_origin,
            environment,
            encryption_key,
            token_pepper,
            admin_token_digest,
            admin_password,
            gateway_event_retention_days,
            linuxdo_oauth,
            default_upstream_pool_id,
        })
    }
}

/// Production always contacts the documented Context7 origins. Development and
/// test may point the fixed upstream client at a loopback mock, but still may
/// not accept paths, credentials or query strings from a caller.
fn parse_upstream_origin(
    raw: Option<String>,
    official: &str,
    environment: Environment,
) -> Result<Url, ConfigError> {
    let official_url = Url::parse(official).map_err(|_| ConfigError::InvalidUpstreamOrigin)?;
    let Some(raw) = raw
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    else {
        return Ok(official_url);
    };
    if environment == Environment::Production {
        return Err(ConfigError::ProductionUpstreamOverride);
    }
    let url = Url::parse(&raw).map_err(|_| ConfigError::InvalidUpstreamOrigin)?;
    let valid = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    if valid {
        Ok(url)
    } else {
        Err(ConfigError::InvalidUpstreamOrigin)
    }
}

fn parse_linuxdo_oauth(
    get: &mut impl FnMut(&str) -> Option<String>,
    environment: Environment,
) -> Result<Option<LinuxDoOAuthConfig>, ConfigError> {
    let enabled = match get("LINUXDO_OAUTH_ENABLED").as_deref().unwrap_or("false") {
        "true" => true,
        "false" => false,
        _ => return Err(ConfigError::InvalidOAuthEnabled),
    };
    if !enabled {
        return Ok(None);
    }
    let mut required = |name: &str| {
        get(name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or(ConfigError::InvalidOAuthConfiguration)
    };
    let parse_endpoint = |raw: String| -> Result<Url, ConfigError> {
        let url = Url::parse(&raw).map_err(|_| ConfigError::InvalidOAuthConfiguration)?;
        let scheme_allowed = if environment == Environment::Production {
            url.scheme() == "https"
        } else {
            matches!(url.scheme(), "https" | "http")
        };
        if scheme_allowed
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
        {
            Ok(url)
        } else {
            Err(ConfigError::InvalidOAuthConfiguration)
        }
    };
    Ok(Some(LinuxDoOAuthConfig {
        client_id: required("LINUXDO_OAUTH_CLIENT_ID")?,
        client_secret: Zeroizing::new(required("LINUXDO_OAUTH_CLIENT_SECRET")?),
        redirect_url: parse_endpoint(required("LINUXDO_OAUTH_REDIRECT_URL")?)?,
        authorize_url: parse_endpoint(
            get("LINUXDO_OAUTH_AUTHORIZE_URL")
                .unwrap_or_else(|| "https://connect.linux.do/oauth2/authorize".to_owned()),
        )?,
        token_url: parse_endpoint(
            get("LINUXDO_OAUTH_TOKEN_URL")
                .unwrap_or_else(|| "https://connect.linux.do/oauth2/token".to_owned()),
        )?,
        userinfo_url: parse_endpoint(
            get("LINUXDO_OAUTH_USERINFO_URL")
                .unwrap_or_else(|| "https://connect.linux.do/api/user".to_owned()),
        )?,
        scope: get("LINUXDO_OAUTH_SCOPE").unwrap_or_else(|| "openid profile".to_owned()),
    }))
}

fn decode_32_byte_secret(raw: &str) -> Result<[u8; 32], ConfigError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| ConfigError::InvalidCryptographicSecret)?;
    decoded
        .try_into()
        .map_err(|_| ConfigError::InvalidCryptographicSecret)
}

fn password_session_version(
    username: &str,
    password: &str,
    pepper: Option<&[u8; 32]>,
) -> Result<[u8; 32], ConfigError> {
    let key = pepper.map_or(&[][..], |pepper| pepper.as_slice());
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).map_err(|_| ConfigError::InvalidAdminPassword)?;
    mac.update(b"context7-gateway-admin-password-v1\0");
    mac.update(username.as_bytes());
    mac.update(b"\0");
    mac.update(password.as_bytes());
    Ok(mac.finalize().into_bytes().into())
}

fn parse_public_origin(raw: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(raw).map_err(|_| ConfigError::InvalidPublicOrigin)?;
    let supported_scheme = matches!(url.scheme(), "http" | "https");
    let has_host = url.host_str().is_some();
    let is_origin_only = url.path() == "/" && url.query().is_none() && url.fragment().is_none();
    if supported_scheme
        && has_host
        && is_origin_only
        && url.username().is_empty()
        && url.password().is_none()
    {
        Ok(url)
    } else {
        Err(ConfigError::InvalidPublicOrigin)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{Config, ConfigError, Environment, parse_public_origin};
    use std::{collections::HashMap, path::PathBuf};

    fn config(values: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<&str, &str> = values.iter().copied().collect();
        Config::from_lookup(|name| map.get(name).map(|value| (*value).to_owned()))
    }

    #[test]
    fn uses_loopback_defaults_in_development() {
        let loaded = config(&[]).expect("development defaults are valid");
        assert_eq!(loaded.bind_addr.to_string(), "127.0.0.1:8787");
        assert_eq!(
            loaded.database_path,
            PathBuf::from("data/context7_gateway.sqlite3")
        );
        assert_eq!(loaded.static_dir, PathBuf::from("web/dist"));
        assert_eq!(loaded.environment, Environment::Development);
    }

    #[test]
    fn production_requires_a_public_origin() {
        let result = config(&[("APP_ENV", "production")]);
        assert_eq!(result, Err(ConfigError::MissingPublicOrigin));
    }

    #[test]
    fn rejects_non_origin_public_urls() {
        for invalid in [
            "not-a-url",
            "ftp://gateway.example",
            "https://gateway.example/path",
        ] {
            assert_eq!(
                parse_public_origin(invalid),
                Err(ConfigError::InvalidPublicOrigin)
            );
        }
    }

    #[test]
    fn accepts_https_origin() {
        let origin = parse_public_origin("https://gateway.example").expect("valid origin");
        assert_eq!(origin.as_str(), "https://gateway.example/");
    }

    #[test]
    fn rejects_invalid_bind_address() {
        assert_eq!(
            config(&[("GATEWAY_BIND", "127.0.0.1:bad")]),
            Err(ConfigError::InvalidBindAddress)
        );
    }

    #[test]
    fn production_requires_security_secrets() {
        assert_eq!(
            config(&[
                ("APP_ENV", "production"),
                ("PUBLIC_ORIGIN", "https://gateway.example")
            ]),
            Err(ConfigError::MissingProductionSecrets)
        );
    }

    #[test]
    fn production_rejects_ip_origins_and_development_admin_tokens() {
        assert_eq!(
            config(&[
                ("APP_ENV", "production"),
                ("PUBLIC_ORIGIN", "https://192.0.2.1")
            ]),
            Err(ConfigError::ProductionRequiresDnsName)
        );
        assert_eq!(
            config(&[
                ("APP_ENV", "production"),
                ("PUBLIC_ORIGIN", "https://gateway.example"),
                (
                    "GATEWAY_ADMIN_TOKEN",
                    "only-used-for-the-development-gateway"
                ),
            ]),
            Err(ConfigError::ProductionAdminBearer)
        );
    }

    #[test]
    fn production_starts_with_passkey_support_and_no_bearer() {
        let loaded = config(&[
            ("APP_ENV", "production"),
            ("PUBLIC_ORIGIN", "https://gateway.example"),
            (
                "GATEWAY_ENCRYPTION_KEY",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
            (
                "GATEWAY_TOKEN_PEPPER",
                "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE",
            ),
            ("LINUXDO_OAUTH_ENABLED", "true"),
            ("LINUXDO_OAUTH_CLIENT_ID", "test-client"),
            ("LINUXDO_OAUTH_CLIENT_SECRET", "test-secret"),
            (
                "LINUXDO_OAUTH_REDIRECT_URL",
                "https://gateway.example/auth/linuxdo/callback",
            ),
            ("DEFAULT_UPSTREAM_POOL_ID", "pool-id"),
        ])
        .expect("production configuration supports passkeys");
        assert!(loaded.admin_passkey_enabled());
        assert!(loaded.admin_token_digest.is_none());
    }

    #[test]
    fn debug_output_does_not_expose_secrets() {
        let loaded = config(&[
            (
                "GATEWAY_ENCRYPTION_KEY",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
            (
                "GATEWAY_TOKEN_PEPPER",
                "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE",
            ),
            (
                "GATEWAY_ADMIN_TOKEN",
                "this-is-a-long-admin-bootstrap-token-secret",
            ),
        ])
        .expect("development security settings are valid");
        let debug = format!("{loaded:?}");
        assert!(!debug.contains("this-is-a-long-admin-bootstrap-token-secret"));
        assert!(!debug.contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
    }

    #[test]
    fn administrator_password_login_requires_a_username_and_password_pair() {
        let loaded = config(&[
            ("GATEWAY_ADMIN_USERNAME", "admin"),
            ("GATEWAY_ADMIN_PASSWORD", "correct-horse"),
            (
                "GATEWAY_TOKEN_PEPPER",
                "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE",
            ),
        ])
        .expect("development password login configuration is valid");
        let admin = loaded
            .admin_password
            .as_ref()
            .expect("administrator password is configured");
        assert!(admin.verify("admin", "correct-horse"));
        assert!(!admin.verify("admin", "wrong-password"));
        assert!(!admin.verify("other", "correct-horse"));
        let debug = format!("{admin:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("correct-horse"));

        assert_eq!(
            config(&[("GATEWAY_ADMIN_USERNAME", "admin")]),
            Err(ConfigError::InvalidAdminPassword)
        );
        assert_eq!(
            config(&[("GATEWAY_ADMIN_PASSWORD", "correct-horse")]),
            Err(ConfigError::InvalidAdminPassword)
        );
    }

    #[test]
    fn production_administrator_password_requires_twelve_characters() {
        assert_eq!(
            config(&[
                ("APP_ENV", "production"),
                ("PUBLIC_ORIGIN", "https://gateway.example"),
                ("GATEWAY_ADMIN_USERNAME", "admin"),
                ("GATEWAY_ADMIN_PASSWORD", "short-pass"),
            ]),
            Err(ConfigError::InvalidAdminPassword)
        );
    }

    #[test]
    fn validates_event_retention_range_and_defaults_to_30_days() {
        assert_eq!(
            config(&[])
                .expect("defaults are valid")
                .gateway_event_retention_days,
            30
        );
        assert_eq!(
            config(&[("GATEWAY_EVENT_RETENTION_DAYS", "1")])
                .expect("minimum retention is valid")
                .gateway_event_retention_days,
            1
        );
        assert_eq!(
            config(&[("GATEWAY_EVENT_RETENTION_DAYS", "365")])
                .expect("maximum retention is valid")
                .gateway_event_retention_days,
            365
        );
        for invalid in ["0", "366", "30.5", "0x1e", ""] {
            assert_eq!(
                config(&[("GATEWAY_EVENT_RETENTION_DAYS", invalid)]),
                Err(ConfigError::InvalidEventRetentionDays)
            );
        }
    }

    #[test]
    fn local_mock_upstream_overrides_are_development_only_origins() {
        let loaded = config(&[
            ("CONTEXT7_MCP_ORIGIN", "http://127.0.0.1:19090"),
            ("CONTEXT7_REST_ORIGIN", "http://127.0.0.1:19090"),
        ])
        .expect("development may point at a loopback mock");
        assert_eq!(
            loaded.context7_mcp_origin.as_str(),
            "http://127.0.0.1:19090/"
        );
        assert_eq!(
            loaded.context7_rest_origin.as_str(),
            "http://127.0.0.1:19090/"
        );
        assert_eq!(
            config(&[
                ("APP_ENV", "production"),
                ("PUBLIC_ORIGIN", "https://gateway.example"),
                ("CONTEXT7_MCP_ORIGIN", "http://127.0.0.1:19090"),
            ]),
            Err(ConfigError::ProductionUpstreamOverride)
        );
        for invalid in [
            "http://127.0.0.1:19090/path",
            "http://user:pass@127.0.0.1:19090",
            "ftp://127.0.0.1:19090",
            "not a url",
        ] {
            assert_eq!(
                config(&[("CONTEXT7_REST_ORIGIN", invalid)]),
                Err(ConfigError::InvalidUpstreamOrigin)
            );
        }
    }
}
