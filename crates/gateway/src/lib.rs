pub mod admin_auth;
pub mod app;
pub mod billing;
pub mod config;
pub mod credentials;
pub mod health;
pub mod management;
pub mod oauth;
mod operations;
pub mod proxy;
pub mod quota;
pub mod session;
pub mod storage;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn package_version_is_present() {
        assert!(!VERSION.trim().is_empty());
    }
}
