use std::path::Path;

use anyhow::{Context, Result};
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};

pub async fn connect(database_path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = database_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create database directory {}", parent.display()))?;
    }

    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(database_path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

    let max_connections = if database_path == Path::new(":memory:") {
        1
    } else {
        5
    };
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await
        .context("failed to connect to SQLite database")?;

    sqlx::migrate!()
        .run(&pool)
        .await
        .context("failed to run database migrations")?;

    Ok(pool)
}

/// Proxy streams cannot survive a process restart, so reset their process-local
/// scheduler leases before admitting traffic in the new process.
pub async fn reset_upstream_leases(pool: &SqlitePool) -> Result<u64> {
    let result = sqlx::query("UPDATE upstream_keys SET in_flight = 0 WHERE in_flight != 0")
        .execute(pool)
        .await
        .context("failed to reset stale upstream key leases")?;
    Ok(result.rows_affected())
}

/// Prunes identifying-minimal request events in bounded batches, retaining quota aggregates.
///
/// Audit events and quota counters have separate retention semantics and are not deleted here.
pub async fn prune_usage_events(pool: &SqlitePool, retention_days: u16) -> Result<u64> {
    anyhow::ensure!(
        (1..=365).contains(&retention_days),
        "retention must be 1..=365 days"
    );
    let cutoff = i64::from(retention_days) * 86_400;
    let mut deleted = 0_u64;
    loop {
        let result = sqlx::query(
            "DELETE FROM usage_events WHERE id IN ( \
                SELECT id FROM usage_events \
                WHERE started_unix > 0 AND started_unix < unixepoch() - ? \
                ORDER BY started_unix ASC LIMIT 5000 \
             )",
        )
        .bind(cutoff)
        .execute(pool)
        .await
        .context("failed to prune expired usage events")?;
        let batch = result.rows_affected();
        deleted = deleted.saturating_add(batch);
        if batch < 5000 {
            break;
        }
        tokio::task::yield_now().await;
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{connect, prune_usage_events, reset_upstream_leases};
    use sqlx::Row;

    #[tokio::test]
    async fn opens_sqlite_and_applies_embedded_migrations() {
        let pool = connect(std::path::Path::new(":memory:"))
            .await
            .expect("in-memory database connects");
        let row = sqlx::query("SELECT value FROM app_metadata WHERE key = 'schema_version'")
            .fetch_one(&pool)
            .await
            .expect("migration metadata exists");
        assert_eq!(row.get::<String, _>("value"), "11");
        pool.close().await;
    }

    #[tokio::test]
    async fn prunes_old_events_in_scope_without_removing_quota_or_audit_records() {
        let pool = connect(std::path::Path::new(":memory:"))
            .await
            .expect("in-memory database connects");
        sqlx::query("INSERT INTO upstream_pools(id, name, authorization_scope) VALUES ('pool', 'pool', 'approved')")
            .execute(&pool)
            .await
            .expect("pool exists for FK");
        sqlx::query(
            "INSERT INTO users(id, display_name, upstream_pool_id) VALUES ('user', 'user', 'pool')",
        )
        .execute(&pool)
        .await
        .expect("user exists for FK");
        sqlx::query("INSERT INTO gateway_tokens(id, user_id, token_digest, prefix, name) VALUES ('token', 'user', zeroblob(32), 'prefix', 'test')")
            .execute(&pool)
            .await
            .expect("token exists for FK");
        sqlx::query("INSERT INTO usage_events(id, user_id, gateway_token_id, operation, request_id, gateway_status, started_unix) VALUES ('old', 'user', 'token', 'mcp_other', 'old-request', 500, unixepoch() - 31 * 86400), ('new', 'user', 'token', 'mcp_other', 'new-request', 200, unixepoch() - 29 * 86400)")
            .execute(&pool)
            .await
            .expect("usage events insert");
        sqlx::query("INSERT INTO quota_counters(user_id, period, window_start, operation) VALUES ('user', 'day', unixepoch(), 'mcp_other')")
            .execute(&pool)
            .await
            .expect("quota aggregate insert");
        sqlx::query("INSERT INTO admin_audit_events(id, actor, action, object_type) VALUES ('audit', 'operator', 'restore.test', 'database')")
            .execute(&pool)
            .await
            .expect("audit entry insert");

        assert_eq!(
            prune_usage_events(&pool, 30).await.expect("retention runs"),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM usage_events")
                .fetch_one(&pool)
                .await
                .expect("count usage rows"),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM quota_counters")
                .fetch_one(&pool)
                .await
                .expect("count quota rows"),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM admin_audit_events")
                .fetch_one(&pool)
                .await
                .expect("count audit rows"),
            1
        );
    }

    #[tokio::test]
    async fn reset_upstream_leases_clears_stale_process_state() {
        let pool = connect(std::path::Path::new(":memory:"))
            .await
            .expect("in-memory database connects");
        sqlx::query("INSERT INTO upstream_pools(id, name, authorization_scope) VALUES ('pool', 'pool', 'approved')")
            .execute(&pool).await.expect("pool exists");
        sqlx::query("INSERT INTO upstream_keys(id, pool_id, ciphertext, nonce, display_prefix, in_flight) VALUES ('key', 'pool', X'00', zeroblob(12), 'ctx7_x', 3)")
            .execute(&pool).await.expect("key exists");
        assert_eq!(reset_upstream_leases(&pool).await.expect("leases reset"), 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT in_flight FROM upstream_keys WHERE id = 'key'")
                .fetch_one(&pool)
                .await
                .expect("lease counter exists"),
            0
        );
    }
}
