#![allow(clippy::expect_used)]

use std::path::Path;

use super::*;
use crate::storage;

pub(super) struct Runner {
    pub user: &'static str,
    pub pool: PoolId,
    pub key: UpstreamKeyId,
    pub token: GatewayTokenId,
}

impl Runner {
    pub(super) async fn insert(database: &SqlitePool, user: &'static str) -> Self {
        let pool = PoolId(format!("{user}-pool"));
        let key = UpstreamKeyId(format!("{user}-key"));
        let token = GatewayTokenId(format!("{user}-token"));
        sqlx::query("INSERT INTO upstream_pools (id, name, authorization_scope) VALUES (?1, ?1, 'authorized fixture')")
            .bind(&pool.0).execute(database).await.expect("pool inserts");
        sqlx::query("INSERT INTO upstream_keys (id, pool_id, ciphertext, nonce, display_prefix, status) VALUES (?, ?, x'00', zeroblob(12), 'fixture', 'active')")
            .bind(&key.0).bind(&pool.0).execute(database).await.expect("key inserts");
        sqlx::query("INSERT INTO users (id, display_name, upstream_pool_id) VALUES (?1, ?1, ?2)")
            .bind(user)
            .bind(&pool.0)
            .execute(database)
            .await
            .expect("user inserts");
        sqlx::query("INSERT INTO gateway_tokens (id, user_id, token_digest, prefix, name) VALUES (?, ?, randomblob(32), 'fixture', 'test')")
            .bind(&token.0).bind(user).execute(database).await.expect("token inserts");
        Self {
            user,
            pool,
            key,
            token,
        }
    }

    pub(super) async fn assign(
        &self,
        database: &SqlitePool,
        rate: i64,
        hourly: i64,
        daily: i64,
        monthly: i64,
    ) {
        sqlx::query("INSERT INTO user_quota_assignments (user_id, version, requests_per_five_minutes, business_calls_per_hour, daily_units, monthly_units) VALUES (?, 1, ?, ?, ?, ?)")
            .bind(self.user).bind(rate).bind(hourly).bind(daily).bind(monthly).execute(database).await.expect("plan inserts");
    }

    pub(super) fn invocation<'a>(
        &'a self,
        request_id: &'a str,
        operation: Operation,
    ) -> Invocation<'a> {
        Invocation {
            user_id: self.user,
            token_id: &self.token,
            pool_id: &self.pool,
            key_id: &self.key,
            operation,
            request_id,
        }
    }
}

pub(super) async fn test_pool() -> SqlitePool {
    storage::connect(Path::new(":memory:"))
        .await
        .expect("open fixture DB")
}

pub(super) fn outcome(failure: Option<Failure>) -> Outcome {
    Outcome {
        gateway_status: StatusCode::OK,
        upstream_status: Some(StatusCode::OK),
        duration: Duration::from_millis(42),
        failure,
    }
}

#[tokio::test]
async fn concurrent_admission_cannot_reserve_more_than_daily_allowance_across_tokens() {
    let path = tempfile::tempdir().expect("working directory");
    let database = storage::connect(&path.path().join("quota.db"))
        .await
        .expect("open multi-connection DB");
    let runner = Runner::insert(&database, "user").await;
    runner.assign(&database, 100, 100, 3, 100).await;
    let second_token = GatewayTokenId("second-token".to_owned());
    sqlx::query("INSERT INTO gateway_tokens (id, user_id, token_digest, prefix, name) VALUES (?, ?, randomblob(32), 'fixture', 'test')")
        .bind(&second_token.0).bind(runner.user).execute(&database).await.expect("second token inserts");
    let runner = std::sync::Arc::new(runner);
    let second_token = std::sync::Arc::new(second_token);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(16));
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..16 {
        let database = database.clone();
        let runner = runner.clone();
        let barrier = barrier.clone();
        let second_token = second_token.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            let request_id = index.to_string();
            let mut invocation = runner.invocation(&request_id, Operation::McpToolCall);
            if index % 2 == 0 {
                invocation.token_id = &second_token;
            }
            reserve(&database, invocation).await
        });
    }
    let mut admitted = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result.expect("admission task completes") {
            Ok(reservation) => admitted.push(reservation),
            Err(AdmissionError::Limited { .. }) => {}
            Err(error) => panic!("unexpected admission error: {error:?}"),
        }
    }
    assert_eq!(admitted.len(), 3);
    for reservation in admitted {
        assert!(reservation.settle(outcome(None)).await.expect("settlement"));
    }
    let (used, rejections): (i64, i64) = sqlx::query_as(
        "SELECT SUM(units), SUM(quota_rejections) FROM quota_counters WHERE period = 'day'",
    )
    .fetch_one(&database)
    .await
    .expect("aggregate counters");
    assert_eq!((used, rejections), (3, 13));
}

#[tokio::test]
async fn rejected_before_send_releases_both_rate_and_business_reservations() {
    let database = test_pool().await;
    let runner = Runner::insert(&database, "not-sent-user").await;
    runner.assign(&database, 1, 1, 1, 1).await;
    let reservation = reserve(
        &database,
        runner.invocation("candidate", Operation::SearchV3),
    )
    .await
    .expect("first request is admitted");
    assert!(
        reservation
            .reject_before_send()
            .await
            .expect("admission is cancelled")
    );

    let next = reserve(
        &database,
        runner.invocation("actually-sent", Operation::SearchV3),
    )
    .await
    .expect("cancelled request did not consume rate or business budget");
    assert!(
        next.settle(outcome(None))
            .await
            .expect("sent request settles")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_events WHERE user_id = ?")
            .bind(runner.user)
            .fetch_one(&database)
            .await
            .expect("only sent requests create usage events"),
        1
    );
}

#[tokio::test]
async fn cleanup_reaps_expired_transport_and_oauth_state() {
    let database = test_pool().await;
    let runner = Runner::insert(&database, "cleanup-user").await;
    sqlx::query("INSERT INTO mcp_sessions (id, external_session_digest, gateway_token_id, pool_id, upstream_key_id, upstream_session_ciphertext, upstream_session_nonce, expires_at) VALUES ('session', randomblob(32), ?, ?, ?, x'00', zeroblob(12), datetime('now','-1 second'))")
        .bind(&runner.token.0).bind(&runner.pool.0).bind(&runner.key.0)
        .execute(&database).await.expect("expired MCP session inserted");
    sqlx::query("INSERT INTO oauth_states (state_digest, expires_at) VALUES (randomblob(32), datetime('now','-1 second'))")
        .execute(&database).await.expect("expired OAuth state inserted");

    purge(&database, 30).await.expect("cleanup pass succeeds");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mcp_sessions")
            .fetch_one(&database)
            .await
            .expect("session count reads"),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM oauth_states")
            .fetch_one(&database)
            .await
            .expect("state count reads"),
        0
    );
}

#[tokio::test]
async fn failed_calls_release_units_and_metadata_keeps_business_budget_unchanged() {
    let database = test_pool().await;
    let runner = Runner::insert(&database, "user").await;
    assert!(matches!(
        reserve(
            &database,
            runner.invocation("unassigned", Operation::McpToolCall)
        )
        .await,
        Err(AdmissionError::Unassigned)
    ));
    runner.assign(&database, 4, 2, 1, 1).await;
    let first = reserve(
        &database,
        runner.invocation("first", Operation::McpToolCall),
    )
    .await
    .expect("first reserves");
    assert!(matches!(
        reserve(
            &database,
            runner.invocation("over-daily", Operation::McpToolCall)
        )
        .await,
        Err(AdmissionError::Limited { .. })
    ));
    first
        .settle(outcome(Some(Failure::McpError)))
        .await
        .expect("failed call settles");
    reserve(&database, runner.invocation("second", Operation::FetchV3))
        .await
        .expect("failed call released units")
        .settle(outcome(None))
        .await
        .expect("settled business result");
    reserve(
        &database,
        runner.invocation("list", Operation::McpToolsList),
    )
    .await
    .expect("metadata ignores business quota")
    .settle(outcome(None))
    .await
    .expect("settled metadata");
    reserve(&database, runner.invocation("resume", Operation::McpSse))
        .await
        .expect("resume ignores business quota")
        .settle(outcome(None))
        .await
        .expect("settled resume");
    assert!(matches!(
        reserve(
            &database,
            runner.invocation("rate-limited", Operation::McpToolsList)
        )
        .await,
        Err(AdmissionError::Limited { .. })
    ));
    let (requests, business, successes, failures, units): (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT SUM(requests), SUM(business_calls), SUM(successful), SUM(failed), SUM(units) FROM quota_counters WHERE period = 'day'",
    ).fetch_one(&database).await.expect("usage values");
    assert_eq!(
        (requests, business, successes, failures, units),
        (4, 2, 3, 1, 1)
    );
}

#[tokio::test]
async fn reservations_use_admission_windows_through_utc_month_and_day_boundaries() {
    let database = test_pool().await;
    let runner = Runner::insert(&database, "user").await;
    runner.assign(&database, 20, 1, 1, 1).await;
    let boundary: i64 = sqlx::query_scalar("SELECT unixepoch('2026-02-01T00:00:00Z')")
        .fetch_one(&database)
        .await
        .expect("February UTC boundary");
    let january = reserve_at(
        &database,
        runner.invocation("january", Operation::SearchV3),
        Some(boundary - 1),
    )
    .await
    .expect("January reserves");
    let february = reserve_at(
        &database,
        runner.invocation("february", Operation::SearchV3),
        Some(boundary),
    )
    .await
    .expect("February independently reserves");
    let january_id = ReservationId(january.id().expect("reservation ID"));
    let february_id = ReservationId(february.id().expect("reservation ID"));
    january.detach();
    february.detach();
    assert!(
        settle(&database, &january_id, outcome(None), Some(boundary + 2))
            .await
            .expect("January settles after midnight")
    );
    assert!(
        settle(&database, &february_id, outcome(None), Some(boundary + 2))
            .await
            .expect("February settles")
    );
    assert!(
        !settle(&database, &january_id, outcome(None), Some(boundary + 2))
            .await
            .expect("duplicate is a no-op")
    );
    let monthly: Vec<(i64, i64)> = sqlx::query_as("SELECT window_start, SUM(units) FROM quota_counters WHERE period = 'month' GROUP BY window_start ORDER BY window_start")
        .fetch_all(&database).await.expect("two monthly windows");
    assert_eq!(
        monthly,
        vec![(boundary - 31 * SECONDS_PER_DAY, 1), (boundary, 1)]
    );
}

#[tokio::test]
async fn rolling_window_releases_at_exact_cutoff_and_expired_lease_is_not_charged() {
    let database = test_pool().await;
    let runner = Runner::insert(&database, "user").await;
    runner.assign(&database, 1, 100, 1, 1).await;
    let timestamp = sqlx::query_scalar::<_, i64>("SELECT unixepoch() - 2 * 86400")
        .fetch_one(&database)
        .await
        .expect("historic timestamp");
    let first = reserve_at(
        &database,
        runner.invocation("expired", Operation::SearchV3),
        Some(timestamp),
    )
    .await
    .expect("first request accepted");
    assert!(matches!(
        reserve_at(
            &database,
            runner.invocation("before", Operation::SearchV3),
            Some(timestamp + RATE_WINDOW_SECONDS - 1)
        )
        .await,
        Err(AdmissionError::Limited { .. }),
    ));
    let second = reserve_at(
        &database,
        runner.invocation("after", Operation::SearchV3),
        Some(timestamp + RATE_WINDOW_SECONDS),
    )
    .await
    .expect("expired reservation neither holds rate nor units");
    let expired_id = ReservationId(first.id().expect("stale ID"));
    first.detach();
    assert!(
        !settle(
            &database,
            &expired_id,
            outcome(None),
            Some(timestamp + LEASE_SECONDS)
        )
        .await
        .expect("late success is not charged")
    );
    second.detach();
    purge(&database, 1)
        .await
        .expect("purge expired historic fixtures");
    let logs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM usage_events WHERE started_unix < unixepoch() - 86400",
    )
    .fetch_one(&database)
    .await
    .expect("logs purged");
    assert_eq!(logs, 0);
    let durable: i64 =
        sqlx::query_scalar("SELECT SUM(requests) FROM quota_counters WHERE period = 'month'")
            .fetch_one(&database)
            .await
            .expect("aggregate kept");
    assert_eq!(durable, 2);
}

#[tokio::test]
async fn detached_reservation_can_be_reattached_and_settled_only_once() {
    let database = test_pool().await;
    let runner = Runner::insert(&database, "user").await;
    runner.assign(&database, 20, 5, 1, 1).await;
    let first = reserve(
        &database,
        runner.invocation("stream", Operation::McpToolCall),
    )
    .await
    .expect("stream accepted");
    let id = first.id().expect("reservation ID");
    first.detach();
    let first = reattach(&database, id)
        .await
        .expect("reattach queries")
        .expect("live reservation");
    let second = reattach(&database, id)
        .await
        .expect("another resumption queries")
        .expect("same reservation");
    assert!(
        first
            .settle(outcome(None))
            .await
            .expect("settlement succeeds")
    );
    assert!(
        !second
            .settle(outcome(None))
            .await
            .expect("idempotent late settlement")
    );
    assert!(
        reattach(&database, id)
            .await
            .expect("reattach queries")
            .is_none()
    );
}

#[tokio::test]
async fn extending_a_detached_stream_reservation_prevents_lease_expiry() {
    let database = test_pool().await;
    let runner = Runner::insert(&database, "extend-user").await;
    runner.assign(&database, 10, 10, 10, 10).await;
    let reservation = reserve(
        &database,
        runner.invocation("long-stream", Operation::McpToolCall),
    )
    .await
    .expect("stream call admitted");
    let id = reservation.id().expect("reservation id");
    reservation.detach();
    sqlx::query("UPDATE quota_reservations SET expires_at = unixepoch() + 1 WHERE id = ?")
        .bind(id.simple().to_string())
        .execute(&database)
        .await
        .expect("shorten lease for the test");
    assert!(extend(&database, id).await.expect("extend succeeds"));
    let remaining: i64 =
        sqlx::query_scalar("SELECT expires_at - unixepoch() FROM quota_reservations WHERE id = ?")
            .bind(id.simple().to_string())
            .fetch_one(&database)
            .await
            .expect("lease remaining reads");
    assert!(remaining > 100, "stream heartbeat renewed the lease");
}

#[tokio::test]
async fn funded_units_are_reserved_across_lots_and_only_consumed_after_success() {
    let database = test_pool().await;
    let runner = Runner::insert(&database, "billing-user").await;
    runner.assign(&database, 20, 20, 10, 0).await;
    sqlx::query(
        "UPDATE user_quota_assignments SET weights = '{\"search_v3\":2}' WHERE user_id = ?",
    )
    .bind(runner.user)
    .execute(&database)
    .await
    .expect("set fixture weight");
    for id in ["lot-a", "lot-b"] {
        sqlx::query("INSERT INTO credit_lots (id, user_id, source_type, source_id, expires_unix) VALUES (?1, ?2, 'redemption', ?1, unixepoch() + 60)")
            .bind(id).bind(runner.user).execute(&database).await.expect("lot fixture");
        sqlx::query("INSERT INTO quota_ledger (id, lot_id, kind, reference_id, units) VALUES (?1, ?1, 'grant', ?1, 1)")
            .bind(id).execute(&database).await.expect("ledger fixture");
    }
    let failed = reserve(&database, runner.invocation("pending", Operation::SearchV3))
        .await
        .expect("two lots fund one weighted call");
    assert!(matches!(
        reserve(
            &database,
            runner.invocation("overfunded", Operation::SearchV3)
        )
        .await,
        Err(AdmissionError::Limited { .. })
    ));
    failed
        .settle(outcome(Some(Failure::UpstreamTransport)))
        .await
        .expect("failure releases both allocations");
    reserve(&database, runner.invocation("success", Operation::SearchV3))
        .await
        .expect("same credit can be reserved again")
        .settle(outcome(None))
        .await
        .expect("settles successfully");
    let debits: Vec<(String, i64)> = sqlx::query_as(
        "SELECT lot_id, units FROM quota_ledger WHERE kind = 'consume' ORDER BY lot_id",
    )
    .fetch_all(&database)
    .await
    .expect("read immutable ledger debits");
    assert_eq!(debits, vec![("lot-a".into(), -1), ("lot-b".into(), -1)]);
    let (used, bonus): (i64, i64) = sqlx::query_as(
        "SELECT SUM(units), SUM(bonus_units) FROM quota_counters WHERE period = 'month'",
    )
    .fetch_one(&database)
    .await
    .expect("monthly aggregate reads");
    assert_eq!((used, bonus), (2, 2));
}
