use std::{ffi::OsString, path::Path};

use anyhow::{Context, Result, ensure};
use context7_gateway::{admin_auth, admin_auth::EnrollmentMode, app, config::Config, storage};
use tokio::net::TcpListener;
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [] => serve().await,
        [help] if help == "--help" => {
            println!("context7-gateway [admin issue-enrollment [--recovery]]");
            Ok(())
        }
        [admin, issue] if is_enrollment_command(admin, issue) => {
            issue_enrollment(EnrollmentMode::Initial).await
        }
        [admin, issue, flag] if is_enrollment_command(admin, issue) && flag == "--recovery" => {
            issue_enrollment(EnrollmentMode::Recovery).await
        }
        _ => anyhow::bail!("usage: context7-gateway [admin issue-enrollment [--recovery]]"),
    }
}

fn is_enrollment_command(admin: &OsString, issue: &OsString) -> bool {
    admin == "admin" && issue == "issue-enrollment"
}

async fn issue_enrollment(mode: EnrollmentMode) -> Result<()> {
    let config = Config::from_env().context("invalid gateway configuration")?;
    ensure!(
        config.admin_passkey_enabled(),
        "administrator passkeys require an HTTPS DNS origin or http://localhost in development"
    );
    ensure!(
        config.database_path != Path::new(":memory:"),
        "administrator enrollment requires the running gateway's persistent DATABASE_PATH"
    );
    let pool = storage::connect(&config.database_path).await?;
    let code = admin_auth::issue_enrollment_ticket(&pool, mode).await?;
    println!("{}", code.as_str());
    Ok(())
}

async fn serve() -> Result<()> {
    init_tracing();
    let config = Config::from_env().context("invalid gateway configuration")?;
    let pool = storage::connect(&config.database_path).await?;
    let reset = storage::reset_upstream_leases(&pool).await?;
    if reset > 0 {
        tracing::info!(reset, "cleared stale upstream request leases");
    }
    let quota_cleanup_task = tokio::spawn(context7_gateway::quota::run_cleanup(
        pool.clone(),
        config.gateway_event_retention_days,
    ));
    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.bind_addr))?;

    tracing::info!(
        bind = %config.bind_addr,
        database = %config.database_path.display(),
        public_origin = %config.public_origin,
        upstream_origin = %config.context7_mcp_origin,
        "Context7 Gateway is listening"
    );

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("failed to build HTTP client")?;
    axum::serve(listener, app::router(config, pool, http))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server failed")?;
    quota_cleanup_task.abort();
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::error!(%error, "failed to install SIGTERM handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
