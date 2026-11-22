use std::{env, sync::Arc};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sqlx::{FromRow, SqliteConnection};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::config::Environment;

mod api;
mod provider;

pub use api::{admin_routes, callback_routes, user_routes};

const SECONDS_PER_DAY: i64 = 86400;
const NO_EXPIRY: i64 = 253_402_300_799;

#[derive(Clone, Default)]
pub struct BillingProvider(Option<Arc<provider::Sandbox>>);

impl BillingProvider {
    pub fn is_enabled(&self) -> bool {
        self.0.is_some()
    }
}

pub fn provider_for(environment: Environment) -> BillingProvider {
    if environment == Environment::Production {
        return BillingProvider::default();
    }
    let Ok(raw_secret) = env::var("GATEWAY_BILLING_SANDBOX_SECRET") else {
        return BillingProvider::default();
    };
    let raw_secret = Zeroizing::new(raw_secret);
    if raw_secret.is_empty() {
        return BillingProvider::default();
    }
    let mut secret = Zeroizing::new([0_u8; 32]);
    if URL_SAFE_NO_PAD
        .decode_slice(raw_secret.as_bytes(), secret.as_mut())
        .ok()
        != Some(32)
    {
        tracing::error!("GATEWAY_BILLING_SANDBOX_SECRET is invalid. Sandbox billing is disabled");
        return BillingProvider::default();
    }
    BillingProvider(Some(Arc::new(provider::Sandbox::new(secret))))
}

#[derive(Clone, Copy, Debug, Serialize)]
struct Quote {
    id: &'static str,
    version: u16,
    label: &'static str,
    units: i64,
    valid_days: i64,
    amount_minor: i64,
    currency: &'static str,
}

const QUOTES: [Quote; 2] = [
    Quote {
        id: "sandbox-100",
        version: 1,
        label: "Sandbox · 100",
        units: 100,
        valid_days: 30,
        amount_minor: 100,
        currency: provider::CURRENCY,
    },
    Quote {
        id: "sandbox-500",
        version: 1,
        label: "Sandbox · 500",
        units: 500,
        valid_days: 30,
        amount_minor: 500,
        currency: provider::CURRENCY,
    },
];

#[derive(Clone, Copy)]
enum GrantSource {
    Order,
    Redemption,
}

impl GrantSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Order => "order",
            Self::Redemption => "redemption",
        }
    }
}

#[derive(Clone, Copy)]
struct Grant<'a> {
    source: GrantSource,
    source_id: &'a str,
    user_id: &'a str,
    units: i64,
    expires: i64,
}

async fn grant(connection: &mut SqliteConnection, grant: Grant<'_>) -> Result<(), sqlx::Error> {
    let lot = Uuid::new_v4().simple().to_string();
    sqlx::query("INSERT INTO credit_lots (id, user_id, source_type, source_id, expires_unix) VALUES (?, ?, ?, ?, ?)")
        .bind(&lot).bind(grant.user_id).bind(grant.source.as_str()).bind(grant.source_id).bind(grant.expires)
        .execute(&mut *connection).await?;
    append_ledger(
        connection,
        &lot,
        LedgerAction::Grant(grant.units),
        grant.source_id,
    )
    .await
}

enum LedgerAction {
    Grant(i64),
    Consume(i64),
    Reverse(i64),
    Manual(i64),
}

impl LedgerAction {
    fn fields(&self) -> (&'static str, i64) {
        match *self {
            Self::Grant(units) => ("grant", units),
            Self::Consume(units) => ("consume", -units),
            Self::Reverse(units) => ("reverse", -units),
            Self::Manual(units) => ("manual", units),
        }
    }
}

pub(crate) struct Adjustment<'a> {
    pub user_id: &'a str,
    pub actor: &'a str,
    pub idempotency_key: &'a str,
    pub units: i64,
    pub valid_days: i64,
    pub reason: &'a str,
    pub now: i64,
}

pub(crate) async fn adjust_balance(
    connection: &mut SqliteConnection,
    adjustment: Adjustment<'_>,
) -> Result<(), sqlx::Error> {
    let Adjustment {
        user_id,
        actor,
        idempotency_key,
        units,
        valid_days,
        reason,
        now,
    } = adjustment;
    if units == 0
        || units.unsigned_abs() > 1_000_000
        || !(0..=365).contains(&valid_days)
        || idempotency_key.is_empty()
        || idempotency_key.len() > 128
        || reason.trim().is_empty()
        || reason.len() > 500
    {
        return Err(sqlx::Error::Protocol(
            "invalid local quota adjustment".to_owned(),
        ));
    }
    let existing: Option<(String, i64, i64, String, String)> = sqlx::query_as("SELECT user_id, units, valid_days, reason, actor FROM quota_adjustments WHERE idempotency_key = ?")
        .bind(idempotency_key)
        .fetch_optional(&mut *connection)
        .await?;
    if let Some(existing) = existing {
        return if existing
            == (
                user_id.to_owned(),
                units,
                valid_days,
                reason.trim().to_owned(),
                actor.to_owned(),
            ) {
            Ok(())
        } else {
            Err(sqlx::Error::Protocol(
                "idempotency key already used for another adjustment".to_owned(),
            ))
        };
    }
    let id = Uuid::new_v4().simple().to_string();
    sqlx::query("INSERT INTO quota_adjustments (idempotency_key, user_id, actor, units, valid_days, reason, created_unix) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(idempotency_key).bind(user_id).bind(actor).bind(units).bind(valid_days).bind(reason.trim()).bind(now)
        .execute(&mut *connection).await?;
    if units > 0 {
        let lot_id = format!("manual-{id}");
        sqlx::query("INSERT INTO credit_lots (id, user_id, source_type, source_id, expires_unix) VALUES (?, ?, 'manual', ?, ?)")
            .bind(&lot_id).bind(user_id).bind(&id)
            .bind(if valid_days == 0 { NO_EXPIRY } else { now.saturating_add(valid_days.saturating_mul(SECONDS_PER_DAY)) })
            .execute(&mut *connection).await?;
        append_ledger(connection, &lot_id, LedgerAction::Manual(units), &id).await
    } else {
        if valid_days != 0 {
            return Err(sqlx::Error::Protocol(
                "a debit adjustment cannot have an expiry".to_owned(),
            ));
        }
        let required = units.unsigned_abs() as i64;
        let allocations = allocate(connection, user_id, now, required)
            .await?
            .ok_or_else(|| {
                sqlx::Error::Protocol("insufficient credit balance for debit adjustment".to_owned())
            })?;
        for allocation in allocations {
            append_ledger(
                connection,
                &allocation.lot_id,
                LedgerAction::Manual(-allocation.units),
                &id,
            )
            .await?;
        }
        Ok(())
    }
}

async fn append_ledger(
    connection: &mut SqliteConnection,
    lot: &str,
    action: LedgerAction,
    reference: &str,
) -> Result<(), sqlx::Error> {
    let (kind, units) = action.fields();
    sqlx::query(
        "INSERT INTO quota_ledger (id, lot_id, kind, reference_id, units) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().simple().to_string())
    .bind(lot)
    .bind(kind)
    .bind(reference)
    .bind(units)
    .execute(connection)
    .await?;
    Ok(())
}

#[derive(Debug, FromRow)]
pub(crate) struct Allocation {
    lot_id: String,
    units: i64,
}

async fn available_lots(
    connection: &mut SqliteConnection,
    user: &str,
    now: i64,
) -> Result<Vec<Allocation>, sqlx::Error> {
    sqlx::query_as(
        "WITH lots AS (
             SELECT l.id AS lot_id, l.expires_unix,
               (SELECT COALESCE(SUM(units), 0) FROM quota_ledger WHERE lot_id = l.id) -
               (SELECT COALESCE(SUM(a.units), 0) FROM quota_reservation_allocations a
                JOIN quota_reservations r ON r.id = a.reservation_id WHERE a.lot_id = l.id AND r.expires_at > ?2) AS units
             FROM credit_lots l WHERE l.user_id = ?1 AND l.expires_unix > ?2
         ) SELECT lot_id, units FROM lots WHERE units > 0 ORDER BY expires_unix, lot_id",
    ).bind(user).bind(now).fetch_all(connection).await
}

pub(crate) async fn balance(
    connection: &mut SqliteConnection,
    user: &str,
    now: i64,
) -> Result<i64, sqlx::Error> {
    available_lots(connection, user, now)
        .await
        .map(|lots| lots.iter().map(|lot| lot.units).sum())
}

pub(crate) async fn allocate(
    connection: &mut SqliteConnection,
    user: &str,
    now: i64,
    requested: i64,
) -> Result<Option<Vec<Allocation>>, sqlx::Error> {
    if requested == 0 {
        return Ok(Some(Vec::new()));
    }
    let mut remaining = requested;
    let mut allocations = Vec::new();
    for mut lot in available_lots(connection, user, now).await? {
        lot.units = lot.units.min(remaining);
        remaining -= lot.units;
        allocations.push(lot);
        if remaining == 0 {
            return Ok(Some(allocations));
        }
    }
    Ok(None)
}

pub(crate) async fn reserve_allocations(
    connection: &mut SqliteConnection,
    reservation_id: &str,
    allocations: Vec<Allocation>,
) -> Result<(), sqlx::Error> {
    for lot in allocations {
        sqlx::query("INSERT INTO quota_reservation_allocations (reservation_id, lot_id, units) VALUES (?, ?, ?)")
            .bind(reservation_id).bind(lot.lot_id).bind(lot.units).execute(&mut *connection).await?;
    }
    Ok(())
}

pub(crate) async fn consume_allocations(
    connection: &mut SqliteConnection,
    reservation_id: &str,
) -> Result<(), sqlx::Error> {
    let allocations = sqlx::query_as::<_, Allocation>(
        "SELECT lot_id, units FROM quota_reservation_allocations WHERE reservation_id = ?",
    )
    .bind(reservation_id)
    .fetch_all(&mut *connection)
    .await?;
    for lot in allocations {
        append_ledger(
            connection,
            &lot.lot_id,
            LedgerAction::Consume(lot.units),
            reservation_id,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn release_allocations(
    connection: &mut SqliteConnection,
    reservation_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM quota_reservation_allocations WHERE reservation_id = ?")
        .bind(reservation_id)
        .execute(&mut *connection)
        .await?;
    Ok(())
}
