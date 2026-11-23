//! SQLite repository for the administrator account, credentials and short-lived secrets.

use anyhow::{Context, Result as AnyResult, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sqlx::{SqliteConnection, SqliteExecutor, SqlitePool};
use uuid::Uuid;
use webauthn_rs::prelude::{AuthenticationResult, Passkey};
use zeroize::Zeroizing;

use crate::management::{ApiError, ApiErrorCode};

use super::{
    CEREMONY_TTL_SECONDS, CeremonyPurpose, EnrollmentMode, MAX_PASSKEYS, PASSKEY_ACTOR,
    PASSWORD_ACTOR, PasskeySummary, RegistrationPermit, RegistrationState, SESSION_TTL_SECONDS,
    SecretDigest, digest, internal_error, new_secret, unauthorized,
};

const ENROLLMENT_TTL_SECONDS: i64 = 15 * 60;
const MAX_PENDING_CEREMONIES_PER_PURPOSE: i64 = 256;

/// Issues the only outstanding administrator enrollment ticket from a trusted local CLI.
///
/// The caller must display the returned secret once and must not log it. A new ticket invalidates
/// any previous one. Recovery does not revoke credentials until a successful registration.
pub async fn issue_enrollment_ticket(
    pool: &SqlitePool,
    mode: EnrollmentMode,
) -> AnyResult<Zeroizing<String>> {
    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .context("could not acquire administrator enrollment transaction")?;
    let active = active_count(&mut transaction).await?;
    if !mode.matches_active_count(active) {
        match mode {
            EnrollmentMode::Initial => {
                bail!("an administrator passkey already exists; use --recovery to replace access")
            }
            EnrollmentMode::Recovery => bail!("there is no passkey to recover; omit --recovery"),
        }
    }
    let secret = new_secret().map_err(|_| anyhow::anyhow!("could not generate enrollment code"))?;
    sqlx::query("DELETE FROM admin_enrollment_tickets")
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO admin_enrollment_tickets(digest, mode, expires_at) VALUES (?, ?, unixepoch() + ?)",
    )
    .bind(digest(&secret).as_slice())
    .bind(mode.as_str())
    .bind(ENROLLMENT_TTL_SECONDS)
    .execute(&mut *transaction)
    .await?;
    let action = match mode {
        EnrollmentMode::Initial => "admin.enrollment.issue",
        EnrollmentMode::Recovery => "admin.recovery.issue",
    };
    audit(
        &mut transaction,
        "operator-cli",
        action,
        "admin_enrollment_ticket",
        None,
    )
    .await?;
    transaction.commit().await?;
    Ok(secret)
}

pub(super) async fn active_passkeys(pool: &SqlitePool) -> Result<Vec<Passkey>, ApiError> {
    let credentials = sqlx::query_scalar::<_, String>(
        "SELECT credential_json FROM admin_passkeys WHERE revoked_at IS NULL",
    )
    .fetch_all(pool)
    .await
    .map_err(db_error)?;
    credentials
        .iter()
        .map(|credential| serde_json::from_str(credential).map_err(|_| internal_error()))
        .collect()
}

pub(super) async fn active_passkey_count(pool: &SqlitePool) -> Result<i64, ApiError> {
    sqlx::query_scalar("SELECT count(*) FROM admin_passkeys WHERE revoked_at IS NULL")
        .fetch_one(pool)
        .await
        .map_err(db_error)
}

pub(super) async fn passkey_summaries(pool: &SqlitePool) -> Result<Vec<PasskeySummary>, ApiError> {
    let rows = sqlx::query_as::<_, (Vec<u8>, String, i64, Option<i64>)>(
        "SELECT credential_id, name, created_at, last_used_at FROM admin_passkeys \
         WHERE revoked_at IS NULL ORDER BY created_at DESC, credential_id",
    )
    .fetch_all(pool)
    .await
    .map_err(db_error)?;
    Ok(rows
        .into_iter()
        .map(|(id, name, created_at, last_used_at)| PasskeySummary {
            id: URL_SAFE_NO_PAD.encode(id),
            name,
            created_at,
            last_used_at,
        })
        .collect())
}

pub(super) async fn user_handle(pool: &SqlitePool) -> Result<Uuid, ApiError> {
    let bytes = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT user_handle FROM admin_account WHERE singleton = 1",
    )
    .fetch_one(pool)
    .await
    .map_err(db_error)?;
    Uuid::from_slice(&bytes).map_err(|_| internal_error())
}

pub(super) async fn enrollment_mode(
    pool: &SqlitePool,
    digest: &SecretDigest,
) -> Result<EnrollmentMode, ApiError> {
    let mode = sqlx::query_scalar::<_, String>(
        "SELECT mode FROM admin_enrollment_tickets WHERE digest = ? AND expires_at > unixepoch()",
    )
    .bind(digest.as_slice())
    .fetch_optional(pool)
    .await
    .map_err(db_error)?;
    match mode.as_deref() {
        Some("initial") => Ok(EnrollmentMode::Initial),
        Some("recovery") => Ok(EnrollmentMode::Recovery),
        _ => Err(unauthorized()),
    }
}

pub(super) async fn session_credential<'e>(
    executor: impl SqliteExecutor<'e>,
    origin: &str,
    digest: &SecretDigest,
) -> Result<Option<Vec<u8>>, ApiError> {
    sqlx::query_scalar(
        "SELECT s.credential_id FROM admin_sessions s JOIN admin_passkeys p USING (credential_id) \
         WHERE s.digest = ? AND s.origin = ? AND s.expires_at > unixepoch() \
         AND s.revoked_at IS NULL AND p.revoked_at IS NULL",
    )
    .bind(digest.as_slice())
    .bind(origin)
    .fetch_optional(executor)
    .await
    .map_err(db_error)
}

pub(super) async fn password_session_exists(
    pool: &SqlitePool,
    origin: &str,
    digest: &SecretDigest,
    account_version: &SecretDigest,
) -> Result<bool, ApiError> {
    sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(SELECT 1 FROM admin_password_sessions \
         WHERE digest = ? AND account_version = ? AND origin = ? \
         AND expires_at > unixepoch() AND revoked_at IS NULL)",
    )
    .bind(digest.as_slice())
    .bind(account_version.as_slice())
    .bind(origin)
    .fetch_one(pool)
    .await
    .map(|value| value == 1)
    .map_err(db_error)
}

pub(super) async fn new_password_session(
    pool: &SqlitePool,
    origin: &str,
    previous: Option<SecretDigest>,
    account_version: &SecretDigest,
) -> Result<Zeroizing<String>, ApiError> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(db_error)?;
    if let Some(previous) = previous {
        revoke_any_session(&mut transaction, origin, &previous).await?;
    }
    sqlx::query(
        "DELETE FROM admin_password_sessions WHERE expires_at <= unixepoch() OR revoked_at IS NOT NULL",
    )
    .execute(&mut *transaction)
    .await
    .map_err(db_error)?;
    sqlx::query(
        "DELETE FROM admin_sessions WHERE expires_at <= unixepoch() OR revoked_at IS NOT NULL",
    )
    .execute(&mut *transaction)
    .await
    .map_err(db_error)?;
    let secret = new_secret().map_err(|_| internal_error())?;
    sqlx::query(
        "INSERT INTO admin_password_sessions(digest, account_version, origin, expires_at) \
         VALUES (?, ?, ?, unixepoch() + ?)",
    )
    .bind(digest(&secret).as_slice())
    .bind(account_version.as_slice())
    .bind(origin)
    .bind(SESSION_TTL_SECONDS)
    .execute(&mut *transaction)
    .await
    .map_err(db_error)?;
    audit(
        &mut transaction,
        PASSWORD_ACTOR,
        "admin.session.login",
        "admin_password",
        None,
    )
    .await
    .map_err(db_error)?;
    transaction.commit().await.map_err(db_error)?;
    Ok(secret)
}

pub(super) async fn start_ceremony(
    pool: &SqlitePool,
    purpose: CeremonyPurpose,
    digest: &SecretDigest,
    previous: Option<SecretDigest>,
    state_json: &str,
) -> Result<(), ApiError> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(db_error)?;
    sqlx::query("DELETE FROM admin_ceremonies WHERE expires_at <= unixepoch()")
        .execute(&mut *transaction)
        .await
        .map_err(db_error)?;
    if let Some(previous) = previous {
        sqlx::query("DELETE FROM admin_ceremonies WHERE digest = ? AND purpose = ?")
            .bind(previous.as_slice())
            .bind(purpose.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(db_error)?;
    }
    let pending =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM admin_ceremonies WHERE purpose = ?")
            .bind(purpose.as_str())
            .fetch_one(&mut *transaction)
            .await
            .map_err(db_error)?;
    if pending >= MAX_PENDING_CEREMONIES_PER_PURPOSE {
        return Err(ApiError::new(ApiErrorCode::RateLimited).retry_after(CEREMONY_TTL_SECONDS));
    }
    sqlx::query(
        "INSERT INTO admin_ceremonies(digest, purpose, state_json, expires_at) \
         VALUES (?, ?, ?, unixepoch() + ?)",
    )
    .bind(digest.as_slice())
    .bind(purpose.as_str())
    .bind(state_json)
    .bind(CEREMONY_TTL_SECONDS)
    .execute(&mut *transaction)
    .await
    .map_err(db_error)?;
    transaction.commit().await.map_err(db_error)
}

pub(super) async fn take_ceremony(
    pool: &SqlitePool,
    purpose: CeremonyPurpose,
    digest: &SecretDigest,
) -> Result<String, ApiError> {
    // One SQL statement commits the deletion even if later cryptographic verification fails.
    sqlx::query_scalar(
        "DELETE FROM admin_ceremonies WHERE digest = ? AND purpose = ? AND expires_at > unixepoch() \
         RETURNING state_json",
    )
    .bind(digest.as_slice())
    .bind(purpose.as_str())
    .fetch_optional(pool)
    .await
    .map_err(db_error)?
    .ok_or_else(unauthorized)
}

pub(super) async fn complete_login(
    pool: &SqlitePool,
    origin: &str,
    previous_session: Option<SecretDigest>,
    result: &AuthenticationResult,
) -> Result<Zeroizing<String>, ApiError> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(db_error)?;
    let credential_id: &[u8] = result.cred_id().as_ref();
    let (credential_json, previous_counter) = sqlx::query_as::<_, (String, i64)>(
        "SELECT credential_json, signature_counter FROM admin_passkeys WHERE credential_id = ? AND revoked_at IS NULL",
    )
    .bind(credential_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(db_error)?
    .ok_or_else(unauthorized)?;
    let counter = i64::from(result.counter());
    // WebAuthn checked the challenge snapshot. This additionally detects counter rollback
    // relative to an assertion that finished after this ceremony began.
    if (previous_counter != 0 || counter != 0) && counter <= previous_counter {
        return Err(unauthorized());
    }
    let mut passkey: Passkey =
        serde_json::from_str(&credential_json).map_err(|_| internal_error())?;
    passkey
        .update_credential(result)
        .ok_or_else(internal_error)?;
    let updated_credential = serde_json::to_string(&passkey).map_err(|_| internal_error())?;
    sqlx::query(
        "UPDATE admin_passkeys SET credential_json = ?, signature_counter = ?, last_used_at = unixepoch() WHERE credential_id = ?",
    )
    .bind(updated_credential)
    .bind(counter)
    .bind(credential_id)
    .execute(&mut *transaction)
    .await
    .map_err(db_error)?;
    let secret = new_session(&mut transaction, origin, previous_session, credential_id).await?;
    audit(
        &mut transaction,
        PASSKEY_ACTOR,
        "admin.session.login",
        "admin_passkey",
        Some(credential_id),
    )
    .await
    .map_err(db_error)?;
    transaction.commit().await.map_err(db_error)?;
    Ok(secret)
}

pub(super) async fn complete_registration(
    pool: &SqlitePool,
    origin: &str,
    previous_session: Option<SecretDigest>,
    registration: &RegistrationState,
    passkey: &Passkey,
    password_session_version: Option<&SecretDigest>,
) -> Result<Zeroizing<String>, ApiError> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(db_error)?;
    let active = active_count(&mut transaction).await.map_err(db_error)?;
    let action = match &registration.permit {
        RegistrationPermit::Session { digest } => {
            session_credential(&mut *transaction, origin, digest)
                .await?
                .ok_or_else(unauthorized)?;
            if active >= MAX_PASSKEYS {
                return Err(ApiError::new(ApiErrorCode::Conflict));
            }
            "admin.passkey.add"
        }
        RegistrationPermit::PasswordSession { digest } => {
            let account_version = password_session_version.ok_or_else(unauthorized)?;
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT EXISTS(SELECT 1 FROM admin_password_sessions \
                 WHERE digest = ? AND account_version = ? AND origin = ? \
                 AND expires_at > unixepoch() AND revoked_at IS NULL)",
            )
            .bind(digest.as_slice())
            .bind(account_version.as_slice())
            .bind(origin)
            .fetch_one(&mut *transaction)
            .await
            .map_err(db_error)?;
            if exists != 1 {
                return Err(unauthorized());
            }
            if active >= MAX_PASSKEYS {
                return Err(ApiError::new(ApiErrorCode::Conflict));
            }
            "admin.passkey.add"
        }
        RegistrationPermit::Ticket { mode, digest } => {
            if !mode.matches_active_count(active) {
                return Err(unauthorized());
            }
            let ticket = sqlx::query(
                "DELETE FROM admin_enrollment_tickets WHERE digest = ? AND mode = ? AND expires_at > unixepoch()",
            )
            .bind(digest.as_slice())
            .bind(mode.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(db_error)?;
            if ticket.rows_affected() != 1 {
                return Err(unauthorized());
            }
            match mode {
                EnrollmentMode::Initial => "admin.passkey.enroll",
                EnrollmentMode::Recovery => {
                    revoke_previous_access(&mut transaction).await?;
                    "admin.passkey.recover"
                }
            }
        }
    };
    let credential_id: &[u8] = passkey.cred_id().as_ref();
    let credential = serde_json::to_string(passkey).map_err(|_| internal_error())?;
    sqlx::query(
        "INSERT INTO admin_passkeys(credential_id, name, credential_json) VALUES (?, ?, ?)",
    )
    .bind(credential_id)
    .bind(&registration.name)
    .bind(credential)
    .execute(&mut *transaction)
    .await
    .map_err(db_error)?;
    let secret = new_session(&mut transaction, origin, previous_session, credential_id).await?;
    audit(
        &mut transaction,
        PASSKEY_ACTOR,
        action,
        "admin_passkey",
        Some(credential_id),
    )
    .await
    .map_err(db_error)?;
    transaction.commit().await.map_err(db_error)?;
    Ok(secret)
}

pub(super) async fn remove_passkey(
    pool: &SqlitePool,
    origin: &str,
    session: &SecretDigest,
    password_session_version: Option<&SecretDigest>,
    allow_last_removal: bool,
    credential_id: &[u8],
) -> Result<(), ApiError> {
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(db_error)?;
    let passkey_session = session_credential(&mut *transaction, origin, session)
        .await?
        .is_some();
    let password_session = if let Some(account_version) = password_session_version {
        sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(SELECT 1 FROM admin_password_sessions \
             WHERE digest = ? AND account_version = ? AND origin = ? \
             AND expires_at > unixepoch() AND revoked_at IS NULL)",
        )
        .bind(session.as_slice())
        .bind(account_version.as_slice())
        .bind(origin)
        .fetch_one(&mut *transaction)
        .await
        .map_err(db_error)?
            == 1
    } else {
        false
    };
    if !passkey_session && !password_session {
        return Err(unauthorized());
    }
    let changed = sqlx::query(
        "UPDATE admin_passkeys SET revoked_at = unixepoch() WHERE credential_id = ? AND revoked_at IS NULL",
    )
    .bind(credential_id)
    .execute(&mut *transaction)
    .await
    .map_err(db_error)?;
    if changed.rows_affected() != 1 {
        return Err(ApiError::new(ApiErrorCode::NotFound));
    }
    if !allow_last_removal && active_count(&mut transaction).await.map_err(db_error)? == 0 {
        return Err(ApiError::new(ApiErrorCode::Conflict));
    }
    sqlx::query(
        "UPDATE admin_sessions SET revoked_at = unixepoch() WHERE credential_id = ? AND revoked_at IS NULL",
    )
    .bind(credential_id)
    .execute(&mut *transaction)
    .await
    .map_err(db_error)?;
    audit(
        &mut transaction,
        PASSKEY_ACTOR,
        "admin.passkey.remove",
        "admin_passkey",
        Some(credential_id),
    )
    .await
    .map_err(db_error)?;
    transaction.commit().await.map_err(db_error)
}

pub(super) async fn logout(
    pool: &SqlitePool,
    origin: &str,
    session: Option<SecretDigest>,
    login: Option<SecretDigest>,
    registration: Option<SecretDigest>,
) -> Result<(), ApiError> {
    if session.is_none() && login.is_none() && registration.is_none() {
        return Ok(());
    }
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await.map_err(db_error)?;
    if let Some(session) = session {
        if let Some(credential_id) = revoke_session(&mut transaction, origin, &session).await? {
            audit(
                &mut transaction,
                PASSKEY_ACTOR,
                "admin.session.logout",
                "admin_passkey",
                Some(&credential_id),
            )
            .await
            .map_err(db_error)?;
        } else if revoke_password_session(&mut transaction, origin, &session).await? {
            audit(
                &mut transaction,
                PASSWORD_ACTOR,
                "admin.session.logout",
                "admin_password",
                None,
            )
            .await
            .map_err(db_error)?;
        }
    }
    for digest in [login, registration].into_iter().flatten() {
        sqlx::query("DELETE FROM admin_ceremonies WHERE digest = ?")
            .bind(digest.as_slice())
            .execute(&mut *transaction)
            .await
            .map_err(db_error)?;
    }
    transaction.commit().await.map_err(db_error)
}

async fn active_count(connection: &mut SqliteConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM admin_passkeys WHERE revoked_at IS NULL")
        .fetch_one(connection)
        .await
}

async fn new_session(
    connection: &mut SqliteConnection,
    origin: &str,
    previous: Option<SecretDigest>,
    credential_id: &[u8],
) -> Result<Zeroizing<String>, ApiError> {
    if let Some(previous) = previous {
        revoke_any_session(connection, origin, &previous).await?;
    }
    sqlx::query(
        "DELETE FROM admin_sessions WHERE expires_at <= unixepoch() OR revoked_at IS NOT NULL",
    )
    .execute(&mut *connection)
    .await
    .map_err(db_error)?;
    sqlx::query(
        "DELETE FROM admin_password_sessions WHERE expires_at <= unixepoch() OR revoked_at IS NOT NULL",
    )
    .execute(&mut *connection)
    .await
    .map_err(db_error)?;
    let secret = new_secret().map_err(|_| internal_error())?;
    sqlx::query(
        "INSERT INTO admin_sessions(digest, credential_id, origin, expires_at) VALUES (?, ?, ?, unixepoch() + ?)",
    )
    .bind(digest(&secret).as_slice())
    .bind(credential_id)
    .bind(origin)
    .bind(SESSION_TTL_SECONDS)
    .execute(connection)
    .await
    .map_err(db_error)?;
    Ok(secret)
}

async fn revoke_session(
    connection: &mut SqliteConnection,
    origin: &str,
    digest: &SecretDigest,
) -> Result<Option<Vec<u8>>, ApiError> {
    sqlx::query_scalar(
        "UPDATE admin_sessions SET revoked_at = unixepoch() WHERE digest = ? AND origin = ? \
         AND revoked_at IS NULL AND expires_at > unixepoch() RETURNING credential_id",
    )
    .bind(digest.as_slice())
    .bind(origin)
    .fetch_optional(connection)
    .await
    .map_err(db_error)
}

async fn revoke_password_session(
    connection: &mut SqliteConnection,
    origin: &str,
    digest: &SecretDigest,
) -> Result<bool, ApiError> {
    let result = sqlx::query(
        "UPDATE admin_password_sessions SET revoked_at = unixepoch() \
         WHERE digest = ? AND origin = ? AND revoked_at IS NULL AND expires_at > unixepoch()",
    )
    .bind(digest.as_slice())
    .bind(origin)
    .execute(connection)
    .await
    .map_err(db_error)?;
    Ok(result.rows_affected() == 1)
}

async fn revoke_any_session(
    connection: &mut SqliteConnection,
    origin: &str,
    digest: &SecretDigest,
) -> Result<Option<Vec<u8>>, ApiError> {
    let credential = revoke_session(connection, origin, digest).await?;
    revoke_password_session(connection, origin, digest).await?;
    Ok(credential)
}

async fn revoke_previous_access(connection: &mut SqliteConnection) -> Result<(), ApiError> {
    sqlx::query("UPDATE admin_passkeys SET revoked_at = unixepoch() WHERE revoked_at IS NULL")
        .execute(&mut *connection)
        .await
        .map_err(db_error)?;
    sqlx::query("UPDATE admin_sessions SET revoked_at = unixepoch() WHERE revoked_at IS NULL")
        .execute(&mut *connection)
        .await
        .map_err(db_error)?;
    sqlx::query("DELETE FROM admin_ceremonies")
        .execute(connection)
        .await
        .map_err(db_error)?;
    Ok(())
}

async fn audit(
    connection: &mut SqliteConnection,
    actor: &str,
    action: &str,
    object_type: &str,
    credential_id: Option<&[u8]>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO admin_audit_events(id, actor, action, object_type, object_id) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().simple().to_string())
    .bind(actor)
    .bind(action)
    .bind(object_type)
    .bind(credential_id.map(|id| URL_SAFE_NO_PAD.encode(id)))
    .execute(connection)
    .await?;
    Ok(())
}

fn db_error(error: sqlx::Error) -> ApiError {
    if matches!(&error, sqlx::Error::Database(error) if error.is_unique_violation()) {
        return ApiError::new(ApiErrorCode::Conflict);
    }
    tracing::error!(%error, "administrator authentication storage operation failed");
    internal_error()
}
