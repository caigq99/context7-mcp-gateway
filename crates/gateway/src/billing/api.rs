use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqliteConnection, SqlitePool};
use uuid::Uuid;

use crate::{
    management::{ApiError, ApiErrorCode, AppState, authorize_admin},
    oauth::authenticate_browser_session,
};

use super::{
    Adjustment, BillingProvider, Grant, GrantSource, LedgerAction, QUOTES, Quote, SECONDS_PER_DAY,
    adjust_balance, append_ledger, balance, grant,
    provider::{MERCHANT, PROVIDER, PaymentProvider, ProviderEvent, ProviderStatus},
};

const ORDER_TTL: i64 = 30 * 60;
const MAX_CODE_UNITS: i64 = 1_000_000;
const MAX_CODE_DAYS: i64 = 365;

pub fn user_routes(provider: BillingProvider) -> Router<AppState> {
    Router::new()
        .route("/api/user/billing", get(user_billing))
        .route("/api/user/billing/redeem", post(redeem_code))
        .route("/api/user/orders", get(user_orders).post(create_order))
        .route("/api/user/orders/preview", post(preview_order))
        .route("/api/user/orders/{order_id}/cancel", post(cancel_order))
        .layer(Extension(provider))
}

pub fn admin_routes() -> Router<AppState> {
    Router::new()
        .route("/api/admin/orders", get(admin_orders))
        .route("/api/admin/orders/{order_id}/reverse", post(reverse_order))
        .route("/api/admin/billing/codes", post(create_code))
        .route("/api/admin/billing/adjustments", post(adjust_quota))
        .route("/api/admin/billing/ledger", get(admin_ledger))
}

pub fn callback_routes(provider: BillingProvider) -> Router<AppState> {
    Router::new()
        .route("/api/billing/callback/sandbox", post(callback))
        .layer(DefaultBodyLimit::max(8192))
        .layer(Extension(provider))
}

fn database_error(error: sqlx::Error) -> ApiError {
    tracing::error!(%error, "billing storage error");
    ApiError::new(ApiErrorCode::InternalError)
}

async fn now(connection: &mut SqliteConnection) -> Result<i64, ApiError> {
    sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(connection)
        .await
        .map_err(database_error)
}

#[derive(Serialize)]
struct BillingSummary {
    enabled: bool,
    quotes: &'static [Quote],
    balance: i64,
}

async fn user_billing(
    State(state): State<AppState>,
    Extension(provider): Extension<BillingProvider>,
    headers: HeaderMap,
) -> Result<Json<BillingSummary>, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let mut connection = state.pool.acquire().await.map_err(database_error)?;
    let timestamp = now(&mut connection).await?;
    let balance = balance(&mut connection, &user.id, timestamp)
        .await
        .map_err(database_error)?;
    let enabled = provider.is_enabled();
    let quotes = if enabled { QUOTES.as_slice() } else { &[] };
    Ok(Json(BillingSummary {
        enabled,
        quotes,
        balance,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteSelection {
    quote_id: String,
    quote_version: u16,
}

impl QuoteSelection {
    fn get(&self, provider: &BillingProvider) -> Result<Quote, ApiError> {
        if !provider.is_enabled() {
            return Err(ApiError::new(ApiErrorCode::NotConfigured));
        }
        QUOTES
            .iter()
            .copied()
            .find(|quote| quote.id == self.quote_id && quote.version == self.quote_version)
            .ok_or_else(|| ApiError::new(ApiErrorCode::Conflict))
    }
}

#[derive(Serialize)]
struct OrderPreview {
    quote: Quote,
    balance: i64,
    projected_balance: i64,
    applies_to: &'static str,
}

async fn preview_order(
    State(state): State<AppState>,
    Extension(provider): Extension<BillingProvider>,
    headers: HeaderMap,
    Json(selection): Json<QuoteSelection>,
) -> Result<Json<OrderPreview>, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let quote = selection.get(&provider)?;
    let mut connection = state.pool.acquire().await.map_err(database_error)?;
    let timestamp = now(&mut connection).await?;
    let current = balance(&mut connection, &user.id, timestamp)
        .await
        .map_err(database_error)?;
    Ok(Json(OrderPreview {
        quote,
        balance: current,
        projected_balance: current.saturating_add(quote.units),
        applies_to: "monthly",
    }))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
enum OrderStatus {
    Pending,
    Settled,
    Cancelled,
    Expired,
    Reversed,
}

#[derive(FromRow, Serialize)]
struct Order {
    id: String,
    quote_id: String,
    quote_version: i64,
    units: i64,
    valid_days: i64,
    amount_minor: i64,
    currency: String,
    status: OrderStatus,
    created_at: String,
    expires_at: String,
    settled_at: Option<String>,
}

async fn read_order(connection: &mut SqliteConnection, order_id: Uuid) -> Result<Order, ApiError> {
    sqlx::query_as("SELECT * FROM recharge_order_views WHERE id = ?")
        .bind(order_id.simple().to_string())
        .fetch_optional(connection)
        .await
        .map_err(database_error)?
        .ok_or_else(|| ApiError::new(ApiErrorCode::NotFound))
}

async fn place_order(
    connection: &mut SqliteConnection,
    user_id: &str,
    quote: Quote,
    timestamp: i64,
) -> Result<Order, ApiError> {
    let order_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO recharge_orders (id, user_id, quote_id, quote_version, units, valid_days, amount_minor, currency, provider, merchant_id, created_unix, expires_unix)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    ).bind(order_id.simple().to_string()).bind(user_id).bind(quote.id).bind(i64::from(quote.version))
    .bind(quote.units).bind(quote.valid_days).bind(quote.amount_minor).bind(quote.currency).bind(PROVIDER).bind(MERCHANT)
    .bind(timestamp).bind(timestamp + ORDER_TTL)
    .execute(&mut *connection).await.map_err(database_error)?;
    read_order(connection, order_id).await
}

async fn create_order(
    State(state): State<AppState>,
    Extension(provider): Extension<BillingProvider>,
    headers: HeaderMap,
    Json(selection): Json<QuoteSelection>,
) -> Result<(StatusCode, Json<Order>), ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let quote = selection.get(&provider)?;
    let mut transaction = state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(database_error)?;
    let timestamp = now(&mut transaction).await?;
    let order = place_order(&mut transaction, &user.id, quote, timestamp).await?;
    transaction.commit().await.map_err(database_error)?;
    Ok((StatusCode::CREATED, Json(order)))
}

async fn user_orders(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<Order>>, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let orders = sqlx::query_as("SELECT * FROM recharge_order_views WHERE user_id = ? ORDER BY created_unix DESC, id DESC LIMIT 200")
        .bind(&user.id).fetch_all(&state.pool).await.map_err(database_error)?;
    Ok(Json(orders))
}

async fn cancel_order(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(order_id): Path<Uuid>,
) -> Result<Json<Order>, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let mut transaction = state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(database_error)?;
    let updated = sqlx::query(
        "UPDATE recharge_orders SET cancelled_unix = unixepoch()
         WHERE id = ? AND user_id = ? AND cancelled_unix IS NULL AND settled_unix IS NULL AND expires_unix > unixepoch()",
    ).bind(order_id.simple().to_string()).bind(&user.id).execute(&mut *transaction).await.map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    let order = read_order(&mut transaction, order_id).await?;
    transaction.commit().await.map_err(database_error)?;
    Ok(Json(order))
}

#[derive(Deserialize)]
struct UserFilter {
    user_id: Option<String>,
}

#[derive(FromRow, Serialize)]
struct AdminOrder {
    #[sqlx(flatten)]
    #[serde(flatten)]
    order: Order,
    user_id: String,
}

async fn admin_orders(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filter): Query<UserFilter>,
) -> Result<Json<Vec<AdminOrder>>, ApiError> {
    authorize_admin(&state, &headers).await?;
    let orders = sqlx::query_as("SELECT * FROM recharge_order_views WHERE ?1 IS NULL OR user_id = ?1 ORDER BY created_unix DESC, id DESC LIMIT 200")
        .bind(filter.user_id).fetch_all(&state.pool).await.map_err(database_error)?;
    Ok(Json(orders))
}

#[derive(FromRow, Serialize)]
struct LedgerEntry {
    id: String,
    user_id: String,
    lot_id: String,
    kind: String,
    reference_id: String,
    units: i64,
    created_at: String,
    expires_at: String,
}

async fn admin_ledger(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(filter): Query<UserFilter>,
) -> Result<Json<Vec<LedgerEntry>>, ApiError> {
    authorize_admin(&state, &headers).await?;
    let records = sqlx::query_as(
        "SELECT e.id, l.user_id, e.lot_id, e.kind, e.reference_id, e.units,
                strftime('%Y-%m-%dT%H:%M:%SZ', e.created_unix, 'unixepoch') AS created_at,
                strftime('%Y-%m-%dT%H:%M:%SZ', l.expires_unix, 'unixepoch') AS expires_at
         FROM quota_ledger e JOIN credit_lots l ON l.id = e.lot_id WHERE ?1 IS NULL OR l.user_id = ?1
         ORDER BY e.created_unix DESC, e.id DESC LIMIT 200",
    ).bind(filter.user_id).fetch_all(&state.pool).await.map_err(database_error)?;
    Ok(Json(records))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuotaAdjustment {
    user_id: String,
    units: i64,
    #[serde(default)]
    valid_days: i64,
    idempotency_key: String,
    reason: String,
}

async fn adjust_quota(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(adjustment): Json<QuotaAdjustment>,
) -> Result<StatusCode, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    if adjustment.units == 0
        || adjustment.units.unsigned_abs() > 1_000_000
        || !(0..=365).contains(&adjustment.valid_days)
        || adjustment.idempotency_key.is_empty()
        || adjustment.idempotency_key.len() > 128
        || adjustment.reason.trim().is_empty()
        || adjustment.reason.len() > 500
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let mut transaction = state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(database_error)?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = ?)")
        .bind(&adjustment.user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
    if !exists {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    if adjustment.units < 0 && adjustment.valid_days != 0 {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let timestamp = now(&mut transaction).await?;
    if let Err(error) = adjust_balance(
        &mut transaction,
        Adjustment {
            user_id: &adjustment.user_id,
            actor: &actor,
            idempotency_key: &adjustment.idempotency_key,
            units: adjustment.units,
            valid_days: adjustment.valid_days,
            reason: &adjustment.reason,
            now: timestamp,
        },
    )
    .await
    {
        return Err(if matches!(&error, sqlx::Error::Protocol(_)) {
            ApiError::new(ApiErrorCode::Conflict)
        } else {
            database_error(error)
        });
    }
    audit(
        &mut transaction,
        &actor,
        "adjust_quota_balance",
        "quota_adjustment",
        &adjustment.idempotency_key,
        headers
            .get("x-gateway-request-id")
            .and_then(|value| value.to_str().ok()),
        serde_json::json!({
            "user_id": adjustment.user_id,
            "units": adjustment.units,
            "valid_days": adjustment.valid_days,
            "reason": adjustment.reason,
        }),
    )
    .await?;
    transaction.commit().await.map_err(database_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(FromRow)]
struct ProviderOrder {
    user_id: String,
    amount_minor: i64,
    currency: String,
    merchant_id: String,
    units: i64,
    valid_days: i64,
    expires_unix: i64,
    settled_unix: Option<i64>,
    cancelled_unix: Option<i64>,
}

async fn record_callback(
    pool: &SqlitePool,
    event: ProviderEvent,
    body: &[u8],
) -> Result<(), ApiError> {
    let digest = Sha256::digest(body);
    let event_id = event.event_id.simple().to_string();
    let order_id = event.order_id.simple().to_string();
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(database_error)?;
    let seen: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT body_digest FROM payment_events WHERE provider = ? AND event_id = ?",
    )
    .bind(PROVIDER)
    .bind(&event_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?;
    if let Some(seen) = seen {
        return if seen.as_slice() == &digest[..] {
            Ok(())
        } else {
            Err(ApiError::new(ApiErrorCode::Conflict))
        };
    }
    let order = sqlx::query_as::<_, ProviderOrder>(
        "SELECT user_id, amount_minor, currency, merchant_id, units, valid_days, expires_unix, settled_unix, cancelled_unix
         FROM recharge_orders WHERE id = ? AND provider = ?",
    ).bind(&order_id).bind(PROVIDER).fetch_optional(&mut *transaction).await.map_err(database_error)?
        .ok_or_else(|| ApiError::new(ApiErrorCode::NotFound))?;
    if event.amount_minor != order.amount_minor
        || event.currency != order.currency
        || event.merchant_id != order.merchant_id
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let timestamp = now(&mut transaction).await?;
    match (order.settled_unix, order.cancelled_unix, event.status) {
        (Some(_), _, _) => {} // A later failure cannot reverse an already settled payment.
        (_, Some(_), ProviderStatus::Failed | ProviderStatus::Cancelled) => {}
        (_, Some(_), ProviderStatus::Paid) => return Err(ApiError::new(ApiErrorCode::Conflict)),
        (None, None, _) if order.expires_unix <= timestamp => {
            return Err(ApiError::new(ApiErrorCode::Conflict));
        }
        (None, None, ProviderStatus::Paid) => {
            sqlx::query("UPDATE recharge_orders SET settled_unix = ? WHERE id = ?")
                .bind(timestamp)
                .bind(&order_id)
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
            grant(
                &mut transaction,
                Grant {
                    source: GrantSource::Order,
                    source_id: &order_id,
                    user_id: &order.user_id,
                    units: order.units,
                    expires: timestamp + SECONDS_PER_DAY * order.valid_days,
                },
            )
            .await
            .map_err(database_error)?;
        }
        (None, None, ProviderStatus::Failed | ProviderStatus::Cancelled) => {
            sqlx::query("UPDATE recharge_orders SET cancelled_unix = ? WHERE id = ?")
                .bind(timestamp)
                .bind(&order_id)
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
        }
    }
    sqlx::query("INSERT INTO payment_events (provider, event_id, body_digest, order_id) VALUES (?, ?, ?, ?)")
        .bind(PROVIDER).bind(event_id).bind(&digest[..]).bind(&order_id)
        .execute(&mut *transaction).await.map_err(database_error)?;
    transaction.commit().await.map_err(database_error)
}

async fn callback(
    State(state): State<AppState>,
    Extension(provider): Extension<BillingProvider>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let verifier = provider
        .0
        .as_ref()
        .ok_or_else(|| ApiError::new(ApiErrorCode::NotConfigured))?;
    let signature = headers
        .get("x-gateway-billing-signature")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() == 43)
        .ok_or_else(|| ApiError::new(ApiErrorCode::Unauthorized))?;
    let event = verifier
        .verify(signature, &body)
        .map_err(|_| ApiError::new(ApiErrorCode::Unauthorized))?;
    record_callback(&state.pool, event, &body).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeSpec {
    units: i64,
    valid_days: i64,
    expires_in_days: i64,
}

#[derive(Serialize)]
struct CreatedCode {
    id: String,
    code: String,
}

const REDEMPTION_PREFIX: &str = "c7rc_";

async fn create_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(spec): Json<CodeSpec>,
) -> Result<Response, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    if !(1..=MAX_CODE_UNITS).contains(&spec.units)
        || !(1..=MAX_CODE_DAYS).contains(&spec.valid_days)
        || !(1..=MAX_CODE_DAYS).contains(&spec.expires_in_days)
    {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let mut entropy = [0_u8; 24];
    getrandom::fill(&mut entropy).map_err(|_| ApiError::new(ApiErrorCode::InternalError))?;
    let code = format!("{REDEMPTION_PREFIX}{}", URL_SAFE_NO_PAD.encode(entropy));
    let digest = Sha256::digest(code.as_bytes());
    let code_id = Uuid::new_v4().simple().to_string();
    let mut transaction = state
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(database_error)?;
    let timestamp = now(&mut transaction).await?;
    sqlx::query("INSERT INTO redemption_codes (id, digest, units, valid_days, expires_unix) VALUES (?, ?, ?, ?, ?)")
        .bind(&code_id).bind(&digest[..]).bind(spec.units).bind(spec.valid_days).bind(timestamp + spec.expires_in_days * SECONDS_PER_DAY)
        .execute(&mut *transaction).await.map_err(database_error)?;
    audit(&mut transaction, &actor, "create_redemption_code", "redemption_code", &code_id,
        headers.get("x-gateway-request-id").and_then(|value| value.to_str().ok()),
        serde_json::json!({ "units": spec.units, "valid_days": spec.valid_days, "expires_in_days": spec.expires_in_days }),
    ).await?;
    transaction.commit().await.map_err(database_error)?;
    let mut response =
        (StatusCode::CREATED, Json(CreatedCode { id: code_id, code })).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(Deserialize)]
struct RedeemCode {
    code: String,
}

#[derive(Debug, Serialize)]
struct Redeemed {
    units: i64,
    balance: i64,
}

async fn redeem_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RedeemCode>,
) -> Result<Json<Redeemed>, ApiError> {
    let user = authenticate_browser_session(&state, &headers).await?;
    let code = zeroize::Zeroizing::new(request.code);
    redeem(&state.pool, &user.id, code.trim()).await.map(Json)
}

async fn redeem(pool: &SqlitePool, user_id: &str, code: &str) -> Result<Redeemed, ApiError> {
    if !code.starts_with(REDEMPTION_PREFIX) || code.len() != REDEMPTION_PREFIX.len() + 32 {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    }
    let digest = Sha256::digest(code.as_bytes());
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(database_error)?;
    let timestamp = now(&mut transaction).await?;
    let redeemed = sqlx::query_as::<_, (String, i64, i64)>(
        "UPDATE redemption_codes SET redeemed_user_id = ?1, redeemed_unix = ?2
         WHERE digest = ?3 AND redeemed_unix IS NULL AND expires_unix > ?2 RETURNING id, units, valid_days",
    ).bind(user_id).bind(timestamp).bind(&digest[..]).fetch_optional(&mut *transaction).await.map_err(database_error)?;
    let Some((id, units, days)) = redeemed else {
        return Err(ApiError::new(ApiErrorCode::InvalidRequest));
    };
    grant(
        &mut transaction,
        Grant {
            source: GrantSource::Redemption,
            source_id: &id,
            user_id,
            units,
            expires: timestamp + days * SECONDS_PER_DAY,
        },
    )
    .await
    .map_err(database_error)?;
    let balance = balance(&mut transaction, user_id, timestamp)
        .await
        .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(Redeemed { units, balance })
}

async fn reverse_order(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(order_id): Path<Uuid>,
) -> Result<Json<Order>, ApiError> {
    let actor = authorize_admin(&state, &headers).await?;
    reverse(&state.pool, order_id, &actor).await.map(Json)
}

async fn reverse(pool: &SqlitePool, order_id: Uuid, actor: &str) -> Result<Order, ApiError> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(database_error)?;
    let order = read_order(&mut transaction, order_id).await?;
    match order.status {
        OrderStatus::Reversed => return Ok(order),
        OrderStatus::Settled => {}
        _ => return Err(ApiError::new(ApiErrorCode::Conflict)),
    }
    let lot = sqlx::query_scalar::<_, String>(
        "SELECT l.id FROM credit_lots AS l WHERE source_type = 'order' AND source_id = ?
         AND NOT EXISTS (SELECT 1 FROM quota_ledger WHERE lot_id = l.id AND kind != 'grant')
         AND NOT EXISTS (SELECT 1 FROM quota_reservation_allocations a JOIN quota_reservations r ON r.id = a.reservation_id
                         WHERE a.lot_id = l.id AND r.expires_at > unixepoch())",
    ).bind(&order.id).fetch_optional(&mut *transaction).await.map_err(database_error)?
        .ok_or_else(|| ApiError::new(ApiErrorCode::Conflict))?;
    append_ledger(
        &mut transaction,
        &lot,
        LedgerAction::Reverse(order.units),
        &order.id,
    )
    .await
    .map_err(database_error)?;
    audit(
        &mut transaction,
        actor,
        "reverse_order",
        "order",
        &order.id,
        None,
        serde_json::json!({ "units": order.units }),
    )
    .await?;
    let result = read_order(&mut transaction, order_id).await?;
    transaction.commit().await.map_err(database_error)?;
    Ok(result)
}

async fn audit(
    connection: &mut SqliteConnection,
    actor: &str,
    action: &str,
    object_type: &str,
    object_id: &str,
    request_id: Option<&str>,
    details: serde_json::Value,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO admin_audit_events (id, actor, action, object_type, object_id, details_json, request_id) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(Uuid::new_v4().simple().to_string()).bind(actor).bind(action).bind(object_type).bind(object_id).bind(details.to_string()).bind(request_id)
        .execute(connection).await.map_err(database_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::{billing::provider, storage};
    use zeroize::Zeroizing;

    async fn fixture() -> SqlitePool {
        let pool = storage::connect(std::path::Path::new(":memory:"))
            .await
            .expect("fixture DB");
        sqlx::query(
            "INSERT INTO users (id, display_name) VALUES ('u1', 'test 1'), ('u2', 'test 2')",
        )
        .execute(&pool)
        .await
        .expect("fixture users");
        pool
    }

    fn paid_fixture(
        order: &Order,
        event_id: Uuid,
        status: &str,
        amount: i64,
        currency: &str,
        merchant: &str,
    ) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "event_id": event_id,
            "order_id": order.id,
            "merchant_id": merchant,
            "amount_minor": amount,
            "currency": currency,
            "status": status,
        }))
        .expect("callback JSON")
    }

    async fn apply(
        pool: &SqlitePool,
        sandbox: &provider::Sandbox,
        payload: &[u8],
    ) -> Result<(), ApiError> {
        let signature = sandbox.sign(payload).expect("fixture signature");
        let event = sandbox
            .verify(&signature, payload)
            .expect("fixture is verified");
        record_callback(pool, event, payload).await
    }

    async fn fixture_order(pool: &SqlitePool, created_at: Option<i64>) -> Order {
        let mut connection = pool.acquire().await.expect("order connection");
        let timestamp = now(&mut connection).await.expect("clock");
        place_order(
            &mut connection,
            "u1",
            QUOTES[0],
            created_at.unwrap_or(timestamp),
        )
        .await
        .expect("order saved")
    }

    async fn balance_for(pool: &SqlitePool, user: &str) -> i64 {
        let mut connection = pool.acquire().await.expect("balance connection");
        let timestamp = now(&mut connection).await.expect("clock");
        balance(&mut connection, user, timestamp)
            .await
            .expect("balance reads")
    }

    #[tokio::test]
    async fn signed_callbacks_settle_once_and_reversals_preserve_history() {
        let pool = fixture().await;
        let order = fixture_order(&pool, None).await;
        assert_eq!(order.status, OrderStatus::Pending);
        let sandbox = provider::Sandbox::new(Zeroizing::new([33; 32]));
        let paid_id = Uuid::new_v4();
        let payload = paid_fixture(
            &order,
            paid_id,
            "paid",
            order.amount_minor,
            &order.currency,
            MERCHANT,
        );
        let signature = sandbox.sign(&payload).expect("sandbox signing");
        let forged = provider::Sandbox::new(Zeroizing::new([94; 32]));
        assert!(forged.verify(&signature, &payload).is_err());
        let (first, retry) = tokio::join!(
            apply(&pool, &sandbox, &payload),
            apply(&pool, &sandbox, &payload)
        );
        first.expect("paid callback succeeds");
        retry.expect("same event is idempotent");
        let altered = paid_fixture(
            &order,
            paid_id,
            "paid",
            order.amount_minor + 1,
            &order.currency,
            MERCHANT,
        );
        assert!(apply(&pool, &sandbox, &altered).await.is_err());
        let duplicate = paid_fixture(
            &order,
            Uuid::new_v4(),
            "paid",
            order.amount_minor,
            &order.currency,
            MERCHANT,
        );
        apply(&pool, &sandbox, &duplicate)
            .await
            .expect("duplicate order event no-ops");
        let late_failure = paid_fixture(
            &order,
            Uuid::new_v4(),
            "failed",
            order.amount_minor,
            &order.currency,
            MERCHANT,
        );
        apply(&pool, &sandbox, &late_failure)
            .await
            .expect("late failure no-ops");
        assert_eq!(balance_for(&pool, "u1").await, 100);
        assert_eq!(balance_for(&pool, "u2").await, 0);
        let order_id = Uuid::parse_str(&order.id).expect("order ID parses");
        let reversed = reverse(&pool, order_id, "admin")
            .await
            .expect("unused grant reverses");
        assert_eq!(reversed.status, OrderStatus::Reversed);
        assert!(reversed.settled_at.is_some());
        reverse(&pool, order_id, "admin")
            .await
            .expect("repeat reversal is a no-op");
        assert_eq!(balance_for(&pool, "u1").await, 0);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quota_ledger")
            .fetch_one(&pool)
            .await
            .expect("ledger count");
        assert_eq!(rows, 2);
        assert!(
            sqlx::query("UPDATE quota_ledger SET units = 1")
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("DELETE FROM quota_ledger")
                .execute(&pool)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn payment_mismatch_expiry_and_paid_after_cancel_do_not_grant_credits() {
        let pool = fixture().await;
        let sandbox = provider::Sandbox::new(Zeroizing::new([19; 32]));
        let order = fixture_order(&pool, None).await;
        for (amount, currency, merchant) in [
            (order.amount_minor + 1, order.currency.as_str(), MERCHANT),
            (order.amount_minor, "USD", MERCHANT),
            (
                order.amount_minor,
                order.currency.as_str(),
                "other-merchant",
            ),
        ] {
            let wrong = paid_fixture(&order, Uuid::new_v4(), "paid", amount, currency, merchant);
            assert!(apply(&pool, &sandbox, &wrong).await.is_err());
        }
        let cancelled = paid_fixture(
            &order,
            Uuid::new_v4(),
            "cancelled",
            order.amount_minor,
            &order.currency,
            MERCHANT,
        );
        apply(&pool, &sandbox, &cancelled)
            .await
            .expect("valid cancellation");
        let late = paid_fixture(
            &order,
            Uuid::new_v4(),
            "paid",
            order.amount_minor,
            &order.currency,
            MERCHANT,
        );
        assert!(apply(&pool, &sandbox, &late).await.is_err());
        let expired_at: i64 = sqlx::query_scalar("SELECT unixepoch() - 86400")
            .fetch_one(&pool)
            .await
            .expect("past time");
        let expired = fixture_order(&pool, Some(expired_at)).await;
        assert_eq!(expired.status, OrderStatus::Expired);
        let paid = paid_fixture(
            &expired,
            Uuid::new_v4(),
            "paid",
            expired.amount_minor,
            &expired.currency,
            MERCHANT,
        );
        assert!(apply(&pool, &sandbox, &paid).await.is_err());
        assert_eq!(balance_for(&pool, "u1").await, 0);
    }

    #[tokio::test]
    async fn manual_adjustments_are_auditable_idempotent_and_cannot_overdraw_reserved_balance() {
        let pool = fixture().await;
        let mut transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("transaction begins");
        adjust_balance(
            &mut transaction,
            Adjustment {
                user_id: "u1",
                actor: "admin",
                idempotency_key: "manual-1",
                units: 40,
                valid_days: 0,
                reason: "grant for test",
                now: 1_800_000_000,
            },
        )
        .await
        .expect("grant manual credits");
        transaction.commit().await.expect("grant commits");
        let mut transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("idempotent transaction");
        adjust_balance(
            &mut transaction,
            Adjustment {
                user_id: "u1",
                actor: "admin",
                idempotency_key: "manual-1",
                units: 40,
                valid_days: 0,
                reason: "grant for test",
                now: 1_800_000_001,
            },
        )
        .await
        .expect("same adjustment is a no-op");
        transaction.commit().await.expect("idempotent call commits");
        assert!(balance_for(&pool, "u1").await >= 40);
        let mut transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("wrong idempotency transaction");
        assert!(
            adjust_balance(
                &mut transaction,
                Adjustment {
                    user_id: "u1",
                    actor: "admin",
                    idempotency_key: "manual-1",
                    units: 39,
                    valid_days: 0,
                    reason: "changed reason",
                    now: 1_800_000_001
                },
            )
            .await
            .is_err()
        );
        transaction
            .rollback()
            .await
            .expect("failed change rolls back");
        let mut transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("debit transaction");
        adjust_balance(
            &mut transaction,
            Adjustment {
                user_id: "u1",
                actor: "admin",
                idempotency_key: "manual-2",
                units: -25,
                valid_days: 0,
                reason: "operator correction",
                now: 1_800_000_002,
            },
        )
        .await
        .expect("debit available grant");
        transaction.commit().await.expect("debit commits");
        assert!(balance_for(&pool, "u1").await >= 15);
        let mut transaction = pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .expect("overdraw transaction");
        assert!(
            adjust_balance(
                &mut transaction,
                Adjustment {
                    user_id: "u1",
                    actor: "admin",
                    idempotency_key: "manual-3",
                    units: -100,
                    valid_days: 0,
                    reason: "reject overdraft",
                    now: 1_800_000_003
                },
            )
            .await
            .is_err()
        );
        transaction.rollback().await.expect("overdraft rolls back");
        let units: i64 =
            sqlx::query_scalar("SELECT SUM(units) FROM quota_ledger WHERE kind = 'manual'")
                .fetch_one(&pool)
                .await
                .expect("manual ledger total");
        assert_eq!(units, 15);
        let adjustments: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quota_adjustments")
            .fetch_one(&pool)
            .await
            .expect("adjustment count");
        assert_eq!(adjustments, 2);
    }

    #[tokio::test]
    async fn redemption_codes_can_only_grant_once_to_one_user() {
        let pool = fixture().await;
        let code = format!("{REDEMPTION_PREFIX}{}", URL_SAFE_NO_PAD.encode([5; 24]));
        let digest = Sha256::digest(code.as_bytes());
        sqlx::query("INSERT INTO redemption_codes (id, digest, units, valid_days, expires_unix) VALUES ('code-1', ?, 50, 30, unixepoch() + 60)")
            .bind(&digest[..]).execute(&pool).await.expect("fixture code saved");
        let (first, second) = tokio::join!(redeem(&pool, "u1", &code), redeem(&pool, "u2", &code));
        assert!(
            first.is_ok() || second.is_ok(),
            "redemption errors: {first:?}, {second:?}"
        );
        assert_eq!(
            balance_for(&pool, "u1").await + balance_for(&pool, "u2").await,
            50
        );
    }
}
