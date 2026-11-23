use std::{collections::BTreeMap, time::Duration};

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Sqlite, SqliteConnection, SqlitePool};
use uuid::Uuid;

use crate::{
    billing,
    credentials::{GatewayTokenId, PoolId, UpstreamKeyId},
};

mod api;
mod tiers;
pub use api::{admin_routes, user_routes};
pub(crate) use tiers::apply_linuxdo_tier_quota;

const RATE_WINDOW_SECONDS: i64 = 300;
const LEASE_SECONDS: i64 = 300;
const SECONDS_PER_HOUR: i64 = 3600;
const SECONDS_PER_DAY: i64 = 86400;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    McpInitialize,
    McpToolsList,
    #[serde(rename = "mcp_tools_call")]
    McpToolCall,
    McpNotification,
    McpPing,
    #[serde(rename = "mcp_stream")]
    McpSse,
    #[serde(rename = "mcp_close")]
    McpDelete,
    McpOther,
    SearchV3,
    FetchV3,
    LibrarySearchV2,
    ContextV2,
}

impl Operation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::McpInitialize => "mcp_initialize",
            Self::McpToolsList => "mcp_tools_list",
            Self::McpToolCall => "mcp_tools_call",
            Self::McpNotification => "mcp_notification",
            Self::McpPing => "mcp_ping",
            Self::McpSse => "mcp_stream",
            Self::McpDelete => "mcp_close",
            Self::McpOther => "mcp_other",
            Self::SearchV3 => "search_v3",
            Self::FetchV3 => "fetch_v3",
            Self::LibrarySearchV2 => "library_search_v2",
            Self::ContextV2 => "context_v2",
        }
    }

    const fn business(self) -> bool {
        matches!(
            self,
            Self::McpToolCall
                | Self::SearchV3
                | Self::FetchV3
                | Self::LibrarySearchV2
                | Self::ContextV2
        )
    }
}

pub struct Invocation<'a> {
    pub user_id: &'a str,
    pub token_id: &'a GatewayTokenId,
    pub pool_id: &'a PoolId,
    pub key_id: &'a UpstreamKeyId,
    pub operation: Operation,
    /// A gateway-generated request identifier. Do not pass through untrusted header values.
    pub request_id: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum AdmissionError {
    #[error("no quota assigned")]
    Unassigned,
    #[error("local quota exceeded")]
    Limited { retry_after_seconds: i64 },
    #[error("quota storage error")]
    Storage(#[from] sqlx::Error),
}

impl IntoResponse for AdmissionError {
    fn into_response(self) -> Response {
        let (status, code, retry_after) = match self {
            Self::Unassigned => (StatusCode::TOO_MANY_REQUESTS, "quota_exceeded", None),
            Self::Limited {
                retry_after_seconds,
            } => (
                StatusCode::TOO_MANY_REQUESTS,
                "quota_exceeded",
                HeaderValue::from_str(&retry_after_seconds.to_string()).ok(),
            ),
            Self::Storage(error) => {
                tracing::error!(%error, "quota admission storage error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", None)
            }
        };
        let mut response = (status, Json(serde_json::json!({"error": code}))).into_response();
        if let Some(value) = retry_after {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Failure {
    UpstreamTransport,
    UpstreamError,
    McpError,
    Cancelled,
    InvalidResponse,
    Expired,
}

impl Failure {
    const fn as_str(self) -> &'static str {
        match self {
            Self::UpstreamTransport => "upstream_transport_error",
            Self::UpstreamError => "upstream_error",
            Self::McpError => "mcp_error",
            Self::Cancelled => "cancelled",
            Self::InvalidResponse => "invalid_upstream_response",
            Self::Expired => "reservation_expired",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Outcome {
    pub gateway_status: StatusCode,
    pub upstream_status: Option<StatusCode>,
    pub duration: Duration,
    pub failure: Option<Failure>,
}

impl Outcome {
    fn abandoned(failure: Failure, duration: Duration) -> Self {
        Self {
            gateway_status: StatusCode::GATEWAY_TIMEOUT,
            upstream_status: None,
            duration,
            failure: Some(failure),
        }
    }

    fn failure_code(self) -> Option<&'static str> {
        self.failure.map(Failure::as_str).or_else(|| {
            (!self.gateway_status.is_success()).then_some(Failure::UpstreamError.as_str())
        })
    }
}

#[derive(Debug)]
struct ReservationId(Uuid);

#[derive(Debug)]
pub struct Reservation {
    pool: SqlitePool,
    id: Option<ReservationId>,
    started: std::time::Instant,
}

impl Reservation {
    pub fn id(&self) -> Option<Uuid> {
        self.id.as_ref().map(|id| id.0)
    }

    /// Transfer ownership to a persisted resumable stream mapping. Its lease still expires.
    pub fn detach(mut self) {
        self.id = None;
    }

    /// Cancel admission when the request is rejected before any upstream send.
    /// This releases both temporary capacity and the rate slot reserved by `reserve`.
    pub async fn reject_before_send(mut self) -> Result<bool, sqlx::Error> {
        let Some(id) = self.id.as_ref() else {
            return Ok(false);
        };
        let applied = reject_before_send(&self.pool, id).await?;
        self.id = None;
        Ok(applied)
    }

    /// Returns false if already settled, or if the lease expired before this outcome arrived.
    pub async fn settle(mut self, outcome: Outcome) -> Result<bool, sqlx::Error> {
        let Some(id) = self.id.as_ref() else {
            return Ok(false);
        };
        let result = settle(&self.pool, id, outcome, None).await?;
        self.id = None;
        Ok(result)
    }
}

async fn reject_before_send(pool: &SqlitePool, id: &ReservationId) -> Result<bool, sqlx::Error> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    let record =
        sqlx::query_as::<_, StoredReservation>("SELECT * FROM quota_reservations WHERE id = ?")
            .bind(id.0.simple().to_string())
            .fetch_optional(&mut *transaction)
            .await?;
    let Some(record) = record else {
        transaction.commit().await?;
        return Ok(false);
    };
    billing::release_allocations(&mut transaction, &record.id).await?;
    let business_calls = i64::from(is_business_operation(&record.operation));
    sqlx::query(
        "UPDATE quota_counters SET requests = max(0, requests - 1),
         business_calls = max(0, business_calls - CASE WHEN period = 'hour' THEN ? ELSE 0 END)
         WHERE user_id = ? AND operation = ? AND (period, window_start) IN
         (('hour', ?), ('day', ?), ('month', ?))",
    )
    .bind(business_calls)
    .bind(&record.user_id)
    .bind(&record.operation)
    .bind(record.hour_start)
    .bind(record.day_start)
    .bind(record.month_start)
    .execute(&mut *transaction)
    .await?;
    sqlx::query("DELETE FROM quota_reservations WHERE id = ?")
        .bind(&record.id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(true)
}

fn is_business_operation(operation: &str) -> bool {
    matches!(
        operation,
        "mcp_tools_call" | "search_v3" | "fetch_v3" | "library_search_v2" | "context_v2"
    )
}

pub async fn reattach(pool: &SqlitePool, id: Uuid) -> Result<Option<Reservation>, sqlx::Error> {
    let age: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(0, unixepoch() - started_unix) FROM quota_reservations WHERE id = ? AND expires_at > unixepoch()",
    )
    .bind(id.simple().to_string())
    .fetch_optional(pool)
    .await?;
    Ok(age.map(|age| Reservation {
        pool: pool.clone(),
        id: Some(ReservationId(id)),
        started: std::time::Instant::now()
            .checked_sub(Duration::from_secs(age.unsigned_abs()))
            .unwrap_or_else(std::time::Instant::now),
    }))
}

/// Keep a long-lived resumable stream's original business reservation alive.
/// Returns false when the reservation already expired or was settled.
pub async fn extend(pool: &SqlitePool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE quota_reservations SET expires_at = unixepoch() + ?
         WHERE id = ? AND expires_at > unixepoch()",
    )
    .bind(LEASE_SECONDS)
    .bind(id.simple().to_string())
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let (Some(id), Ok(runtime)) = (self.id.take(), tokio::runtime::Handle::try_current()) {
            // The request future can be cancelled before its body wrapper is polled.
            // The periodic lease reaper covers runtime shutdown and unavailable storage.
            let pool = self.pool.clone();
            let duration = self.started.elapsed();
            runtime.spawn(async move {
                if let Err(error) = settle(
                    &pool,
                    &id,
                    Outcome::abandoned(Failure::Cancelled, duration),
                    None,
                )
                .await
                {
                    tracing::error!(%error, "quota cancellation settlement failed");
                }
            });
        }
    }
}

#[derive(Debug, FromRow, Serialize)]
struct PlanRow {
    version: i64,
    requests_per_five_minutes: i64,
    business_calls_per_hour: i64,
    daily_units: i64,
    monthly_units: i64,
    #[serde(skip)]
    weights: String,
    created_at: String,
}

impl PlanRow {
    fn weights(&self) -> Result<BTreeMap<Operation, i64>, sqlx::Error> {
        serde_json::from_str(&self.weights).map_err(|error| sqlx::Error::Decode(Box::new(error)))
    }
}

async fn active_plan(
    connection: &mut SqliteConnection,
    user_id: &str,
) -> Result<Option<PlanRow>, sqlx::Error> {
    sqlx::query_as("SELECT version, requests_per_five_minutes, business_calls_per_hour, daily_units, monthly_units, weights, created_at FROM user_quota_assignments WHERE user_id = ? ORDER BY version DESC LIMIT 1")
        .bind(user_id)
        .fetch_optional(connection)
        .await
}

#[derive(Clone, Copy)]
struct Windows {
    now: i64,
    hour: i64,
    day: i64,
    month: i64,
    next_month: i64,
}

impl Windows {
    async fn at(connection: &mut SqliteConnection, now: Option<i64>) -> Result<Self, sqlx::Error> {
        let (now, month, next_month): (i64, i64, i64) = sqlx::query_as(
            "SELECT t, unixepoch(t, 'unixepoch', 'start of month'), unixepoch(t, 'unixepoch', 'start of month', '+1 month') FROM (SELECT COALESCE(?, unixepoch()) AS t)",
        )
        .bind(now)
        .fetch_one(connection)
        .await?;
        Ok(Self {
            now,
            hour: now - now.rem_euclid(SECONDS_PER_HOUR),
            day: now - now.rem_euclid(SECONDS_PER_DAY),
            month,
            next_month,
        })
    }
}

#[derive(Default, FromRow)]
struct Balances {
    rate_used: i64,
    oldest_rate_attempt: Option<i64>,
    hourly_calls: i64,
    daily_used: i64,
    monthly_used: i64,
    daily_reserved: i64,
    monthly_reserved: i64,
    monthly_bonus_used: i64,
    monthly_bonus_reserved: i64,
}

async fn balances(
    connection: &mut SqliteConnection,
    user: &str,
    windows: Windows,
) -> Result<Balances, sqlx::Error> {
    sqlx::query_as(
        "WITH recent AS (
            SELECT started_unix FROM quota_reservations WHERE user_id = ?1 AND started_unix > ?2
            UNION ALL
            SELECT started_unix FROM usage_events WHERE user_id = ?1 AND started_unix > ?2 AND admitted = 1
        ), finished AS (
            SELECT COALESCE(SUM(CASE WHEN period = 'hour' THEN business_calls END), 0) AS hourly_calls,
                COALESCE(SUM(CASE WHEN period = 'day' THEN units END), 0) AS daily_used,
                COALESCE(SUM(CASE WHEN period = 'month' THEN units END), 0) AS monthly_used,
                COALESCE(SUM(CASE WHEN period = 'month' THEN bonus_units END), 0) AS monthly_bonus_used
            FROM quota_counters WHERE user_id = ?1 AND ((period, window_start) IN (('hour', ?3), ('day', ?4), ('month', ?5)))
        ), pending AS (
            SELECT COALESCE(SUM(CASE WHEN day_start = ?4 THEN units END), 0) AS daily_reserved,
                COALESCE(SUM(CASE WHEN month_start = ?5 THEN units END), 0) AS monthly_reserved,
                COALESCE(SUM(CASE WHEN month_start = ?5 THEN bonus_units END), 0) AS monthly_bonus_reserved
            FROM quota_reservations WHERE user_id = ?1 AND expires_at > ?6
        )
        SELECT (SELECT COUNT(*) FROM recent) AS rate_used, (SELECT MIN(started_unix) FROM recent) AS oldest_rate_attempt,
            finished.*, pending.* FROM finished CROSS JOIN pending",
    )
    .bind(user)
    .bind(windows.now - RATE_WINDOW_SECONDS)
    .bind(windows.hour)
    .bind(windows.day)
    .bind(windows.month)
    .bind(windows.now)
    .fetch_one(connection)
    .await
}

pub async fn reserve(
    pool: &SqlitePool,
    invocation: Invocation<'_>,
) -> Result<Reservation, AdmissionError> {
    reserve_at(pool, invocation, None).await
}

async fn reserve_at(
    pool: &SqlitePool,
    invocation: Invocation<'_>,
    now: Option<i64>,
) -> Result<Reservation, AdmissionError> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    let windows = Windows::at(&mut transaction, now).await?;
    let (plan, usage) = (
        active_plan(&mut transaction, invocation.user_id).await?,
        balances(&mut transaction, invocation.user_id, windows).await?,
    );
    let units = match &plan {
        Some(plan) if invocation.operation.business() => plan
            .weights()?
            .get(&invocation.operation)
            .copied()
            .unwrap_or(1),
        _ => 0,
    };
    let bonus_units = plan.as_ref().map_or(0, |plan| {
        let base_committed = usage.monthly_used - usage.monthly_bonus_used + usage.monthly_reserved
            - usage.monthly_bonus_reserved;
        let base_remaining = (plan.monthly_units - base_committed).max(0);
        (units - base_remaining).max(0)
    });
    let allocations = billing::allocate(
        &mut transaction,
        invocation.user_id,
        windows.now,
        bonus_units,
    )
    .await?;
    let admission = check_admission(
        plan.as_ref(),
        &usage,
        &invocation,
        windows,
        units,
        allocations.is_some(),
    );
    let counters_delta = match &admission {
        Ok(()) => Delta {
            requests: 1,
            business_calls: i64::from(invocation.operation.business()),
            ..Delta::default()
        },
        Err(_) => Delta {
            quota_rejections: 1,
            ..Delta::default()
        },
    };
    let id = ReservationId(Uuid::new_v4());
    bump_counters(
        &mut transaction,
        invocation.user_id,
        invocation.operation.as_str(),
        windows,
        counters_delta,
    )
    .await?;
    if let Err(error) = admission {
        sqlx::query(
            "INSERT INTO usage_events (id, user_id, gateway_token_id, operation, request_id, started_at, started_unix, gateway_status, failure_code, admitted)
             VALUES (?, ?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', ?6, 'unixepoch'), ?6, 429, 'quota_exceeded', 0)",
        )
        .bind(id.0.simple().to_string())
        .bind(invocation.user_id)
        .bind(&invocation.token_id.0)
        .bind(invocation.operation.as_str())
        .bind(invocation.request_id)
        .bind(windows.now)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        return Err(error);
    }
    let reservation_id = id.0.simple().to_string();
    sqlx::query(
        "INSERT INTO quota_reservations (id, user_id, gateway_token_id, pool_id, upstream_key_id, operation, request_id, started_unix, expires_at, hour_start, day_start, month_start, units, bonus_units)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&reservation_id)
    .bind(invocation.user_id)
    .bind(&invocation.token_id.0)
    .bind(&invocation.pool_id.0)
    .bind(&invocation.key_id.0)
    .bind(invocation.operation.as_str())
    .bind(invocation.request_id)
    .bind(windows.now)
    .bind(windows.now + LEASE_SECONDS)
    .bind(windows.hour)
    .bind(windows.day)
    .bind(windows.month)
    .bind(units)
    .bind(bonus_units)
    .execute(&mut *transaction)
    .await?;
    if let Some(allocations) = allocations {
        billing::reserve_allocations(&mut transaction, &reservation_id, allocations).await?;
    }
    transaction.commit().await?;
    Ok(Reservation {
        pool: pool.clone(),
        id: Some(id),
        started: std::time::Instant::now(),
    })
}

fn check_admission(
    plan: Option<&PlanRow>,
    usage: &Balances,
    invocation: &Invocation<'_>,
    windows: Windows,
    units: i64,
    monthly_funded: bool,
) -> Result<(), AdmissionError> {
    let Some(plan) = plan else {
        return Err(AdmissionError::Unassigned);
    };
    let mut deadlines = Vec::with_capacity(4);
    if usage.rate_used >= plan.requests_per_five_minutes {
        deadlines.push(usage.oldest_rate_attempt.unwrap_or(windows.now) + RATE_WINDOW_SECONDS);
    }
    if invocation.operation.business() {
        if usage.hourly_calls >= plan.business_calls_per_hour {
            deadlines.push(windows.hour + SECONDS_PER_HOUR);
        }
        if units > 0 && usage.daily_used + usage.daily_reserved + units > plan.daily_units {
            deadlines.push(windows.day + SECONDS_PER_DAY);
        }
        if !monthly_funded {
            deadlines.push(windows.next_month);
        }
    }
    match deadlines.into_iter().max() {
        None => Ok(()),
        Some(deadline) => Err(AdmissionError::Limited {
            retry_after_seconds: (deadline - windows.now).max(1),
        }),
    }
}

#[derive(Default)]
struct Delta {
    requests: i64,
    business_calls: i64,
    successful: i64,
    failed: i64,
    units: i64,
    quota_rejections: i64,
    bonus_units: i64,
}

async fn bump_counters(
    connection: &mut SqliteConnection,
    user_id: &str,
    operation: &str,
    windows: Windows,
    delta: Delta,
) -> Result<(), sqlx::Error> {
    sqlx::query::<Sqlite>(
        "INSERT INTO quota_counters (user_id, operation, period, window_start, requests, business_calls, successful, failed, units, quota_rejections, bonus_units)
         VALUES (?1, ?2, 'hour', ?3, ?6, ?7, ?8, ?9, ?10, ?11, ?12),
                (?1, ?2, 'day', ?4, ?6, ?7, ?8, ?9, ?10, ?11, ?12),
                (?1, ?2, 'month', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT (user_id, period, window_start, operation) DO UPDATE SET
           requests = max(0, requests + excluded.requests), business_calls = max(0, business_calls + excluded.business_calls),
           successful = successful + excluded.successful, failed = failed + excluded.failed,
           units = units + excluded.units, quota_rejections = quota_rejections + excluded.quota_rejections,
           bonus_units = bonus_units + excluded.bonus_units",
    )
    .bind(user_id).bind(operation).bind(windows.hour).bind(windows.day).bind(windows.month)
    .bind(delta.requests).bind(delta.business_calls).bind(delta.successful).bind(delta.failed).bind(delta.units).bind(delta.quota_rejections)
    .bind(delta.bonus_units)
    .execute(connection)
    .await?;
    Ok(())
}

#[derive(FromRow)]
struct StoredReservation {
    id: String,
    user_id: String,
    gateway_token_id: String,
    pool_id: String,
    upstream_key_id: String,
    operation: String,
    request_id: String,
    started_unix: i64,
    expires_at: i64,
    hour_start: i64,
    day_start: i64,
    month_start: i64,
    units: i64,
    bonus_units: i64,
}

async fn settle(
    pool: &SqlitePool,
    id: &ReservationId,
    outcome: Outcome,
    now: Option<i64>,
) -> Result<bool, sqlx::Error> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    let now: i64 = sqlx::query_scalar("SELECT COALESCE(?, unixepoch())")
        .bind(now)
        .fetch_one(&mut *transaction)
        .await?;
    let record =
        sqlx::query_as::<_, StoredReservation>("SELECT * FROM quota_reservations WHERE id = ?")
            .bind(id.0.simple().to_string())
            .fetch_optional(&mut *transaction)
            .await?;
    let applied = if let Some(record) = record {
        record_outcome(&mut transaction, record, outcome, now).await?
    } else {
        false
    };
    transaction.commit().await?;
    Ok(applied)
}

async fn record_outcome(
    connection: &mut SqliteConnection,
    record: StoredReservation,
    outcome: Outcome,
    now: i64,
) -> Result<bool, sqlx::Error> {
    let live = record.expires_at > now;
    let outcome = if live {
        outcome
    } else {
        Outcome::abandoned(Failure::Expired, Duration::from_secs(LEASE_SECONDS as u64))
    };
    let failed = outcome.failure_code();
    let units = if failed.is_none() { record.units } else { 0 };
    let bonus_units = if failed.is_none() {
        record.bonus_units
    } else {
        0
    };
    if bonus_units > 0 {
        if failed.is_none() {
            billing::consume_allocations(connection, &record.id).await?;
        } else {
            billing::release_allocations(connection, &record.id).await?;
        }
    }
    let duration_ms = i64::try_from(outcome.duration.as_millis()).unwrap_or(i64::MAX);
    sqlx::query(
        "INSERT INTO usage_events (id, user_id, gateway_token_id, pool_id, upstream_key_id, operation, request_id, started_at, started_unix, duration_ms, gateway_status, upstream_status, success_units, failure_code)
         VALUES (?, ?, ?, ?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', ?8, 'unixepoch'), ?8, ?9, ?10, ?11, ?12, ?13)",
    )
    .bind(&record.id).bind(&record.user_id).bind(record.gateway_token_id).bind(record.pool_id).bind(record.upstream_key_id)
    .bind(&record.operation).bind(record.request_id).bind(record.started_unix).bind(duration_ms)
    .bind(i64::from(outcome.gateway_status.as_u16())).bind(outcome.upstream_status.map(|status| i64::from(status.as_u16())))
    .bind(units).bind(failed).execute(&mut *connection).await?;
    bump_counters(
        connection,
        &record.user_id,
        &record.operation,
        Windows {
            now,
            hour: record.hour_start,
            day: record.day_start,
            month: record.month_start,
            next_month: 0,
        },
        Delta {
            successful: i64::from(failed.is_none()),
            failed: i64::from(failed.is_some()),
            units,
            bonus_units,
            ..Delta::default()
        },
    )
    .await?;
    sqlx::query("DELETE FROM quota_reservations WHERE id = ?")
        .bind(&record.id)
        .execute(connection)
        .await?;
    Ok(live)
}

/// Reap abandoned work in bounded batches and delete detailed logs beyond the configured retention.
/// Counters remain available for quota enforcement and historical summaries.
pub async fn purge(pool: &SqlitePool, retention_days: u32) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(&mut *transaction)
        .await?;
    let expired = sqlx::query_as::<_, StoredReservation>(
        "SELECT * FROM quota_reservations WHERE expires_at <= ? ORDER BY expires_at LIMIT 256",
    )
    .bind(now)
    .fetch_all(&mut *transaction)
    .await?;
    for record in expired {
        record_outcome(
            &mut transaction,
            record,
            Outcome::abandoned(Failure::Expired, Duration::ZERO),
            now,
        )
        .await?;
    }
    sqlx::query("DELETE FROM usage_events WHERE id IN (SELECT id FROM usage_events WHERE started_unix < ? ORDER BY started_unix LIMIT 4096)")
        .bind(now - SECONDS_PER_DAY * i64::from(retention_days.clamp(1, 365)))
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM mcp_sessions WHERE unixepoch(expires_at) <= ? OR (closed_at IS NOT NULL AND unixepoch(closed_at) < ?)")
        .bind(now)
        .bind(now - SECONDS_PER_DAY)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM oauth_states WHERE unixepoch(expires_at) <= ? OR (used_at IS NOT NULL AND unixepoch(used_at) < ?)")
        .bind(now)
        .bind(now - SECONDS_PER_DAY)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM admin_ceremonies WHERE expires_at <= ?")
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM admin_enrollment_tickets WHERE expires_at <= ?")
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM admin_sessions WHERE expires_at <= ? OR (revoked_at IS NOT NULL AND revoked_at < ?)")
        .bind(now)
        .bind(now - 30 * SECONDS_PER_DAY)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM upstream_usage_snapshots WHERE fetched_at < strftime('%Y-%m-%dT%H:%M:%fZ', ?, 'unixepoch', '-90 days')")
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await
}

/// Reap expired reservations and privacy-sensitive event rows while keeping immutable counters.
pub async fn run_cleanup(pool: SqlitePool, retention_days: u16) {
    loop {
        match purge(&pool, u32::from(retention_days)).await {
            Ok(()) => {}
            Err(error) => {
                tracing::error!(%error, retention_days, "quota reservation cleanup failed")
            }
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

#[cfg(test)]
mod tests;
