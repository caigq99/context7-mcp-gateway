use std::collections::BTreeMap;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqliteConnection, SqlitePool};
use uuid::Uuid;

use crate::{
    billing,
    management::{ApiError, ApiErrorCode, AppState, authorize_admin},
    oauth::authenticate_browser_session,
};

use super::{
    Operation, PlanRow, RATE_WINDOW_SECONDS, SECONDS_PER_DAY, SECONDS_PER_HOUR, Windows,
    active_plan, balances,
};

const MAX_LIMIT: i64 = 1_000_000_000;
const MAX_WEIGHT: i64 = 1000;
const DAILY_HISTORY_DAYS: i64 = 7;

pub fn admin_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/admin/users/{user_id}/quota",
            get(get_quota).patch(set_quota),
        )
        .route("/api/admin/usage/summary", get(admin_summary))
        .route("/api/admin/logs", get(admin_logs))
        .route("/api/admin/logs/export", get(export_logs))
}

pub fn user_routes() -> Router<AppState> {
    Router::new()
        .route("/api/user/usage/summary", get(user_summary))
        .route("/api/user/usage/events", get(user_events))
}

fn database_error(error: sqlx::Error) -> ApiError {
    tracing::error!(%error, "quota API storage error");
    ApiError::new(ApiErrorCode::InternalError)
}

#[derive(Serialize)]
struct Plan {
    #[serde(flatten)]
    limits: PlanRow,
    weights: BTreeMap<Operation, i64>,
}

impl TryFrom<PlanRow> for Plan {
    type Error = sqlx::Error;

    fn try_from(limits: PlanRow) -> Result<Self, Self::Error> {
        let weights = limits.weights()?;
        Ok(Self { limits, weights })
    }
}

#[derive(Serialize)]
struct Window {
    used: i64,
    limit: i64,
    reserved: i64,
    resets_at: Option<String>,
}

#[derive(Serialize)]
struct QuotaWindows {
    requests_5m: Window,
    business_1h: Window,
    daily: Window,
    monthly: Window,
}

#[derive(Serialize)]
struct Assignment {
    plan: Option<Plan>,
    windows: QuotaWindows,
}

async fn assignment(
    connection: &mut SqliteConnection,
    user_id: &str,
    windows: Windows,
) -> Result<Assignment, sqlx::Error> {
    let plan = active_plan(connection, user_id)
        .await?
        .map(Plan::try_from)
        .transpose()?;
    let usage = balances(connection, user_id, windows).await?;
    let bonus_capacity = if plan.is_some() {
        billing::balance(connection, user_id, windows.now).await?
            + usage.monthly_bonus_used
            + usage.monthly_bonus_reserved
    } else {
        0
    };
    let (requests_reset, hour_reset, day_reset, month_reset) = sqlx::query_as(
        "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', ?, 'unixepoch'),
                strftime('%Y-%m-%dT%H:%M:%SZ', ?, 'unixepoch'),
                strftime('%Y-%m-%dT%H:%M:%SZ', ?, 'unixepoch'),
                strftime('%Y-%m-%dT%H:%M:%SZ', ?, 'unixepoch')",
    )
    .bind(Some(
        usage
            .oldest_rate_attempt
            .map_or(windows.now + RATE_WINDOW_SECONDS, |since| {
                since + RATE_WINDOW_SECONDS
            }),
    ))
    .bind(windows.hour + SECONDS_PER_HOUR)
    .bind(windows.day + SECONDS_PER_DAY)
    .bind(windows.next_month)
    .fetch_one(connection)
    .await?;
    let limits = plan.as_ref().map(|plan| &plan.limits);
    let window = |used, limit, reserved, resets_at| Window {
        used,
        limit,
        reserved,
        resets_at,
    };
    let quota_windows = QuotaWindows {
        requests_5m: window(
            usage.rate_used,
            limits.map_or(0, |row| row.requests_per_five_minutes),
            0,
            requests_reset,
        ),
        business_1h: window(
            usage.hourly_calls,
            limits.map_or(0, |row| row.business_calls_per_hour),
            0,
            hour_reset,
        ),
        daily: window(
            usage.daily_used,
            limits.map_or(0, |row| row.daily_units),
            usage.daily_reserved,
            day_reset,
        ),
        monthly: window(
            usage.monthly_used,
            limits.map_or(0, |row| row.monthly_units) + bonus_capacity,
            usage.monthly_reserved,
            month_reset,
        ),
    };
    Ok(Assignment {
        plan,
        windows: quota_windows,
    })
}

async fn user_exists(connection: &mut SqliteConnection, user_id: &str) -> Result<(), ApiError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = ?)")
        .bind(user_id)
        .fetch_one(connection)
        .await
        .map_err(database_error)?;
    if !exists {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    Ok(())
}

async fn get_quota(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Assignment>, ApiError> {
    authorize_admin(&state, &headers).await?;
    let mut transaction = state.pool.begin().await.map_err(database_error)?;
    user_exists(&mut transaction, &user_id).await?;
    let windows = Windows::at(&mut transaction, None)
        .await
        .map_err(database_error)?;
    let result = assignment(&mut transaction, &user_id, windows)
        .await
        .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(Json(result))
}

#[derive(Deserialize)]
struct ChangeQuota {
    requests_per_five_minutes: i64,
    business_calls_per_hour: i64,
    daily_units: i64,
    monthly_units: i64,
    weights: Option<BTreeMap<Operation, i64>>,
    expected_version: Option<i64>,
}

impl ChangeQuota {
    fn validate(&self) -> Result<(), ApiError> {
        let limits = [
            self.requests_per_five_minutes,
            self.business_calls_per_hour,
            self.daily_units,
            self.monthly_units,
        ];
        let valid_limits = limits.iter().all(|limit| (0..=MAX_LIMIT).contains(limit));
        let valid_weights = self.weights.as_ref().is_none_or(|weights| {
            weights.iter().all(|(operation, weight)| {
                operation.business() && (1..=MAX_WEIGHT).contains(weight)
            })
        });
        if valid_limits && valid_weights {
            Ok(())
        } else {
            Err(ApiError::new(ApiErrorCode::InvalidRequest))
        }
    }
}

async fn set_quota(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    headers: HeaderMap,
    Json(change): Json<ChangeQuota>,
) -> Result<Json<Plan>, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    change.validate()?;
    let mut transaction = state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(database_error)?;
    user_exists(&mut transaction, &user_id).await?;
    let previous = active_plan(&mut transaction, &user_id)
        .await
        .map_err(database_error)?;
    let previous_version = previous.as_ref().map_or(0, |plan| plan.version);
    if change
        .expected_version
        .is_some_and(|version| version != previous_version)
    {
        return Err(ApiError::new(ApiErrorCode::Conflict));
    }
    let prior_plan = previous
        .map(Plan::try_from)
        .transpose()
        .map_err(database_error)?;
    let weights = match change.weights {
        Some(weights) => serde_json::to_string(&weights)
            .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?,
        None => prior_plan
            .as_ref()
            .map_or_else(|| "{}".to_owned(), |plan| plan.limits.weights.clone()),
    };
    let row = sqlx::query_as::<_, PlanRow>(
        "INSERT INTO user_quota_assignments (user_id, version, requests_per_five_minutes, business_calls_per_hour, daily_units, monthly_units, weights)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         RETURNING version, requests_per_five_minutes, business_calls_per_hour, daily_units, monthly_units, weights, created_at",
    )
    .bind(&user_id).bind(previous_version + 1)
    .bind(change.requests_per_five_minutes).bind(change.business_calls_per_hour)
    .bind(change.daily_units).bind(change.monthly_units).bind(weights)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    let plan = Plan::try_from(row).map_err(database_error)?;
    let details = serde_json::json!({ "previous": prior_plan, "current": &plan });
    sqlx::query("INSERT INTO admin_audit_events (id, actor, action, object_type, object_id, details_json, request_id) VALUES (?, ?, 'set_quota', 'user', ?, ?, ?)")
        .bind(Uuid::new_v4().simple().to_string())
        .bind(actor)
        .bind(user_id)
        .bind(details.to_string())
        .bind(headers.get("x-gateway-request-id").and_then(|value| value.to_str().ok()))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(Json(plan))
}

#[derive(FromRow, Serialize)]
struct DailyUsage {
    date: String,
    requests: i64,
    successful: i64,
    failed: i64,
    units: i64,
}

#[derive(FromRow, Serialize)]
struct UsageTotals {
    requests: i64,
    successful: i64,
    failed: i64,
    units: i64,
    quota_rejections: i64,
    latency_p50_ms: Option<i64>,
    latency_p95_ms: Option<i64>,
    latency_p99_ms: Option<i64>,
}

#[derive(Serialize)]
struct Summary {
    totals: UsageTotals,
    daily: Vec<DailyUsage>,
}

#[derive(FromRow, Serialize)]
struct PoolAvailability {
    total: i64,
    enabled: i64,
    available_keys: i64,
    unavailable_keys: i64,
}

#[derive(Serialize)]
struct UserSummary {
    #[serde(flatten)]
    quota: Assignment,
    #[serde(flatten)]
    summary: Summary,
}

#[derive(Serialize)]
struct AdminSummary {
    #[serde(flatten)]
    summary: Summary,
    pools: PoolAvailability,
}

async fn read_summary(
    connection: &mut SqliteConnection,
    day: i64,
    month: i64,
    user_id: Option<&str>,
) -> Result<Summary, sqlx::Error> {
    let daily_start = day - (DAILY_HISTORY_DAYS - 1) * SECONDS_PER_DAY;
    let daily = sqlx::query_as(
        "WITH RECURSIVE dates(ts) AS (
             SELECT ?1 UNION ALL SELECT ts + 86400 FROM dates WHERE ts < ?2
         ), daily_totals AS (
             SELECT window_start, SUM(requests) AS requests, SUM(successful) AS successful, SUM(failed) AS failed, SUM(units) AS units
             FROM quota_counters WHERE period = 'day' AND window_start BETWEEN ?1 AND ?2 AND (?3 IS NULL OR user_id = ?3)
             GROUP BY window_start
         )
         SELECT strftime('%Y-%m-%d', ts, 'unixepoch') AS date, COALESCE(requests, 0) AS requests,
                COALESCE(successful, 0) AS successful, COALESCE(failed, 0) AS failed, COALESCE(units, 0) AS units
         FROM dates LEFT JOIN daily_totals ON window_start = ts ORDER BY ts",
    ).bind(daily_start).bind(day).bind(user_id).fetch_all(&mut *connection).await?;
    let totals = sqlx::query_as(
        "WITH counts AS (
              SELECT COALESCE(SUM(requests), 0) AS requests, COALESCE(SUM(successful), 0) AS successful,
                     COALESCE(SUM(failed), 0) AS failed, COALESCE(SUM(units), 0) AS units,
                     COALESCE(SUM(quota_rejections), 0) AS quota_rejections
              FROM quota_counters WHERE period = 'month' AND window_start = ?1 AND (?2 IS NULL OR user_id = ?2)
         ), latency AS (
              SELECT duration_ms, ROW_NUMBER() OVER (ORDER BY duration_ms) AS rank, COUNT(*) OVER () AS size
              FROM usage_events WHERE started_unix >= ?1 AND admitted = 1 AND (?2 IS NULL OR user_id = ?2)
         ), percentiles AS (
              SELECT MAX(CASE WHEN rank = (size + 1) / 2 THEN duration_ms END) AS latency_p50_ms,
                     MAX(CASE WHEN rank = (size * 95 + 99) / 100 THEN duration_ms END) AS latency_p95_ms,
                     MAX(CASE WHEN rank = (size * 99 + 99) / 100 THEN duration_ms END) AS latency_p99_ms FROM latency
         )
         SELECT counts.*, percentiles.* FROM counts CROSS JOIN percentiles",
    ).bind(month).bind(user_id).fetch_one(connection).await?;
    Ok(Summary { totals, daily })
}

async fn user_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<UserSummary>, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let mut transaction = state.pool.begin().await.map_err(database_error)?;
    let windows = Windows::at(&mut transaction, None)
        .await
        .map_err(database_error)?;
    let quota = assignment(&mut transaction, &user.id, windows)
        .await
        .map_err(database_error)?;
    let summary = read_summary(&mut transaction, windows.day, windows.month, Some(&user.id))
        .await
        .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(Json(UserSummary { quota, summary }))
}

async fn admin_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AdminSummary>, ApiError> {
    authorize_admin(&state, &headers).await?;
    let mut transaction = state.pool.begin().await.map_err(database_error)?;
    let windows = Windows::at(&mut transaction, None)
        .await
        .map_err(database_error)?;
    let summary = read_summary(&mut transaction, windows.day, windows.month, None)
        .await
        .map_err(database_error)?;
    let pools = sqlx::query_as(
        "WITH counts AS (SELECT COUNT(*) AS total, COALESCE(SUM(enabled), 0) AS enabled FROM upstream_pools),
           keys AS (
             SELECT COUNT(*) AS key_count, COALESCE(SUM(p.enabled = 1
               AND (p.rate_limited_until IS NULL OR unixepoch(p.rate_limited_until) <= unixepoch())
               AND (p.breaker_until IS NULL OR unixepoch(p.breaker_until) <= unixepoch())
               AND (k.status = 'active' OR (k.status = 'rate_limited' AND unixepoch(k.rate_limited_until) <= unixepoch()))), 0) AS available_keys
             FROM upstream_keys k JOIN upstream_pools p ON p.id = k.pool_id WHERE k.deleted_at IS NULL
           )
           SELECT counts.*, available_keys, key_count - available_keys AS unavailable_keys FROM counts CROSS JOIN keys",
    ).fetch_one(&mut *transaction).await.map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(Json(AdminSummary { summary, pools }))
}

#[derive(Deserialize, Default)]
struct EventFilters {
    limit: Option<u16>,
    cursor: Option<String>,
    operation: Option<Operation>,
    status: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    token_id: Option<String>,
    user_id: Option<String>,
    request_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    started_unix: i64,
    id: Uuid,
}

impl Cursor {
    fn decode(input: &str) -> Result<Self, ApiError> {
        if input.len() > 192 {
            return Err(ApiError::new(ApiErrorCode::InvalidRequest));
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(input)
            .map_err(|_| ApiError::new(ApiErrorCode::InvalidRequest))?;
        serde_json::from_slice(&bytes).map_err(|_| ApiError::new(ApiErrorCode::InvalidRequest))
    }

    fn encode(row: &Event) -> Result<String, ApiError> {
        let id =
            Uuid::parse_str(&row.id).map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
        let value = Self {
            started_unix: row.started_unix,
            id,
        };
        let bytes =
            serde_json::to_vec(&value).map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
        Ok(URL_SAFE_NO_PAD.encode(bytes))
    }
}

#[derive(FromRow, Serialize)]
struct Event {
    id: String,
    gateway_token_id: String,
    operation: String,
    request_id: String,
    started_at: String,
    #[serde(skip)]
    started_unix: i64,
    duration_ms: i64,
    gateway_status: i64,
    upstream_status: Option<i64>,
    success_units: i64,
    failure_code: Option<String>,
}

#[derive(FromRow, Serialize)]
struct AdminEvent {
    #[sqlx(flatten)]
    #[serde(flatten)]
    event: Event,
    user_id: String,
    pool_id: Option<String>,
    upstream_key_id: Option<String>,
}

#[derive(Serialize)]
struct EventPage<T> {
    items: Vec<T>,
    next_cursor: Option<String>,
}

const EVENT_COLUMNS: &str = "id, gateway_token_id, operation, request_id, started_at, started_unix, duration_ms, gateway_status, upstream_status, success_units, failure_code";

fn events_query<'a>(
    filters: &'a EventFilters,
    user: Option<&'a str>,
    limit: i64,
) -> Result<QueryBuilder<'a, Sqlite>, ApiError> {
    if filters
        .from
        .zip(filters.to)
        .is_some_and(|(from, to)| from > to)
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let mut query = QueryBuilder::new("SELECT ");
    query.push(EVENT_COLUMNS);
    if user.is_none() {
        query.push(", user_id, pool_id, upstream_key_id");
    }
    query.push(" FROM usage_events WHERE 1 = 1");
    if let Some(user_id) = user.or(filters.user_id.as_deref()) {
        query.push(" AND user_id = ").push_bind(user_id);
    }
    if let Some(token_id) = &filters.token_id {
        query.push(" AND gateway_token_id = ").push_bind(token_id);
    }
    if let Some(request_id) = &filters.request_id {
        query.push(" AND request_id = ").push_bind(request_id);
    }
    if let Some(operation) = filters.operation {
        query
            .push(" AND operation = ")
            .push_bind(operation.as_str());
    }
    match filters.status.as_deref() {
        None | Some("") => {}
        Some("success") => {
            query.push(" AND failure_code IS NULL");
        }
        Some("failure") => {
            query.push(" AND failure_code IS NOT NULL");
        }
        Some(status) => {
            let http = status
                .parse::<i64>()
                .ok()
                .filter(|value| (100..=599).contains(value))
                .ok_or_else(|| ApiError::new(ApiErrorCode::InvalidRequest))?;
            query.push(" AND gateway_status = ").push_bind(http);
        }
    }
    if let Some(from) = filters.from {
        query.push(" AND started_unix >= ").push_bind(from);
    }
    if let Some(to) = filters.to {
        query.push(" AND started_unix <= ").push_bind(to);
    }
    if let Some(cursor) = filters.cursor.as_deref() {
        let cursor = Cursor::decode(cursor)?;
        query
            .push(" AND (started_unix, id) < (")
            .push_bind(cursor.started_unix)
            .push(", ")
            .push_bind(cursor.id.simple().to_string())
            .push(")");
    }
    query
        .push(" ORDER BY started_unix DESC, id DESC LIMIT ")
        .push_bind(limit + 1);
    Ok(query)
}

fn event_page<T>(
    mut items: Vec<T>,
    limit: usize,
    row: impl Fn(&T) -> &Event,
) -> Result<EventPage<T>, ApiError> {
    let next_cursor = if items.len() > limit {
        items.truncate(limit);
        items.last().map(row).map(Cursor::encode).transpose()?
    } else {
        None
    };
    Ok(EventPage { items, next_cursor })
}

async fn user_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filters): Query<EventFilters>,
) -> Result<Json<EventPage<Event>>, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let limit = filters.limit.unwrap_or(50).clamp(1, 200);
    let rows = events_query(&filters, Some(&user.id), i64::from(limit))?
        .build_query_as::<Event>()
        .fetch_all(&state.pool)
        .await
        .map_err(database_error)?;
    Ok(Json(event_page(rows, usize::from(limit), |event| event)?))
}

async fn admin_page(
    pool: &SqlitePool,
    filters: &EventFilters,
    limit: u16,
) -> Result<EventPage<AdminEvent>, ApiError> {
    let rows = events_query(filters, None, i64::from(limit))?
        .build_query_as::<AdminEvent>()
        .fetch_all(pool)
        .await
        .map_err(database_error)?;
    event_page(rows, usize::from(limit), |event| &event.event)
}

async fn admin_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filters): Query<EventFilters>,
) -> Result<Json<EventPage<AdminEvent>>, ApiError> {
    authorize_admin(&state, &headers).await?;
    let limit = filters.limit.unwrap_or(50).clamp(1, 200);
    Ok(Json(admin_page(&state.pool, &filters, limit).await?))
}

async fn export_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filters): Query<EventFilters>,
) -> Result<Response, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    let page = admin_page(
        &state.pool,
        &filters,
        filters.limit.unwrap_or(1000).clamp(1, 1000),
    )
    .await?;
    let mut output = Vec::new();
    for row in &page.items {
        serde_json::to_writer(&mut output, row)
            .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
        output.push(b'\n');
    }
    sqlx::query("INSERT INTO admin_audit_events (id, actor, action, object_type, details_json) VALUES (?, ?, 'export_usage_events', 'usage', json_object('rows', ?))")
        .bind(Uuid::new_v4().simple().to_string()).bind(actor).bind(i64::try_from(page.items.len()).unwrap_or(1000))
        .execute(&state.pool).await.map_err(database_error)?;
    let mut response = output.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=gateway-usage.ndjson"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(cursor) = page.next_cursor {
        let cursor = HeaderValue::from_str(&cursor)
            .map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
        headers.insert("x-next-cursor", cursor);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::quota::{
        reserve,
        tests::{Runner, outcome, test_pool},
    };

    #[tokio::test]
    async fn user_log_scope_survives_filters_and_cursor_and_does_not_expose_credentials() {
        let database = test_pool().await;
        let first = Runner::insert(&database, "first").await;
        let second = Runner::insert(&database, "second").await;
        first.assign(&database, 10, 10, 10, 10).await;
        second.assign(&database, 10, 10, 10, 10).await;
        for (runner, request) in [(&first, "r1"), (&first, "r2"), (&second, "r3")] {
            reserve(
                &database,
                runner.invocation(request, Operation::McpToolCall),
            )
            .await
            .expect("request reserves")
            .settle(outcome(None))
            .await
            .expect("request settles");
        }
        let filters = EventFilters {
            user_id: Some(second.user.to_owned()),
            status: Some("success".to_owned()),
            ..EventFilters::default()
        };
        let events = events_query(&filters, Some(first.user), 1)
            .expect("filter parses")
            .build_query_as::<Event>()
            .fetch_all(&database)
            .await
            .expect("events query");
        let first_page = event_page(events, 1, |event| event).expect("page cursor encode");
        let serialized = serde_json::to_value(&first_page.items[0]).expect("response JSON");
        assert_eq!(serialized["gateway_token_id"], first.token.0);
        assert!(serialized.get("user_id").is_none());
        assert!(serialized.get("pool_id").is_none());
        assert!(serialized.get("upstream_key_id").is_none());
        assert!(serialized.get("query").is_none());
        let filters = EventFilters {
            cursor: first_page.next_cursor,
            ..filters
        };
        let events = events_query(&filters, Some(first.user), 1)
            .expect("cursor parses")
            .build_query_as::<Event>()
            .fetch_all(&database)
            .await
            .expect("second page query");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].gateway_token_id, first.token.0);
        assert_ne!(events[0].request_id, first_page.items[0].request_id);
        let mut connection = database.acquire().await.expect("summary connection");
        let windows = Windows::at(&mut connection, None)
            .await
            .expect("window loads");
        let summary = read_summary(
            &mut connection,
            windows.day,
            windows.month,
            Some(second.user),
        )
        .await
        .expect("second user's summary");
        assert_eq!(summary.totals.requests, 1);
        assert_eq!(summary.totals.units, 1);
        assert_eq!(summary.totals.latency_p95_ms, Some(42));
    }
}
