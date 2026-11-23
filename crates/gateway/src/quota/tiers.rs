use sqlx::{FromRow, SqliteConnection};

use crate::management::{ApiError, map_database_error, write_audit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TierQuota {
    pub requests_per_five_minutes: i64,
    pub business_calls_per_hour: i64,
    pub daily_units: i64,
    pub monthly_units: i64,
}

pub(crate) const fn linuxdo_tier_quota(level: i64) -> Option<TierQuota> {
    let quota = match level {
        0 => TierQuota {
            requests_per_five_minutes: 10,
            business_calls_per_hour: 30,
            daily_units: 10,
            monthly_units: 50,
        },
        1 => TierQuota {
            requests_per_five_minutes: 30,
            business_calls_per_hour: 60,
            daily_units: 20,
            monthly_units: 300,
        },
        2 => TierQuota {
            requests_per_five_minutes: 60,
            business_calls_per_hour: 240,
            daily_units: 70,
            monthly_units: 1_000,
        },
        3 => TierQuota {
            requests_per_five_minutes: 120,
            business_calls_per_hour: 600,
            daily_units: 200,
            monthly_units: 3_000,
        },
        4 => TierQuota {
            requests_per_five_minutes: 180,
            business_calls_per_hour: 900,
            daily_units: 400,
            monthly_units: 6_000,
        },
        _ => return None,
    };
    Some(quota)
}

#[derive(Debug, FromRow)]
struct QuotaRow {
    version: i64,
    requests_per_five_minutes: i64,
    business_calls_per_hour: i64,
    daily_units: i64,
    monthly_units: i64,
    weights: String,
}

impl QuotaRow {
    fn matches(&self, quota: TierQuota) -> bool {
        self.weights == "{}"
            && self.requests_per_five_minutes == quota.requests_per_five_minutes
            && self.business_calls_per_hour == quota.business_calls_per_hour
            && self.daily_units == quota.daily_units
            && self.monthly_units == quota.monthly_units
    }
}

pub(crate) async fn apply_linuxdo_tier_quota(
    connection: &mut SqliteConnection,
    user_id: &str,
    current_trust_level: Option<i64>,
    previous_trust_level: Option<i64>,
) -> Result<(), ApiError> {
    let Some(current_level) = current_trust_level.filter(|level| (0..=4).contains(level)) else {
        return Ok(());
    };
    let Some(current_quota) = linuxdo_tier_quota(current_level) else {
        return Ok(());
    };
    let latest = sqlx::query_as::<_, QuotaRow>(
        "SELECT version, requests_per_five_minutes, business_calls_per_hour, daily_units, monthly_units, weights \
         FROM user_quota_assignments WHERE user_id = ? ORDER BY version DESC LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(map_database_error)?;

    let Some(latest) = latest else {
        insert_tier_quota(connection, user_id, 1, current_quota).await?;
        write_audit(
            &mut *connection,
            "oauth-auto-provision",
            "quota.linuxdo_tier",
            "user",
            user_id,
        )
        .await?;
        return Ok(());
    };

    let Some(previous_level) = previous_trust_level.filter(|level| (0..=4).contains(level)) else {
        return Ok(());
    };
    if previous_level == current_level {
        return Ok(());
    }
    let Some(previous_quota) = linuxdo_tier_quota(previous_level) else {
        return Ok(());
    };
    if !latest.matches(previous_quota) {
        return Ok(());
    }

    insert_tier_quota(
        connection,
        user_id,
        latest
            .version
            .checked_add(1)
            .ok_or_else(|| ApiError::new(crate::management::ApiErrorCode::InternalError))?,
        current_quota,
    )
    .await?;
    write_audit(
        &mut *connection,
        "oauth-auto-provision",
        "quota.linuxdo_tier",
        "user",
        user_id,
    )
    .await?;
    Ok(())
}

async fn insert_tier_quota(
    connection: &mut SqliteConnection,
    user_id: &str,
    version: i64,
    quota: TierQuota,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO user_quota_assignments \
         (user_id, version, requests_per_five_minutes, business_calls_per_hour, daily_units, monthly_units, weights) \
         VALUES (?, ?, ?, ?, ?, ?, '{}')",
    )
    .bind(user_id)
    .bind(version)
    .bind(quota.requests_per_five_minutes)
    .bind(quota.business_calls_per_hour)
    .bind(quota.daily_units)
    .bind(quota.monthly_units)
    .execute(&mut *connection)
    .await
    .map_err(map_database_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use sqlx::Row;

    use super::{TierQuota, apply_linuxdo_tier_quota, linuxdo_tier_quota};
    use crate::storage;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = storage::connect(std::path::Path::new(":memory:"))
            .await
            .expect("test database connects");
        sqlx::query(
            "INSERT INTO upstream_pools(id, name, authorization_scope) VALUES ('pool','pool','test')",
        )
        .execute(&pool)
        .await
        .expect("test pool exists");
        sqlx::query(
            "INSERT INTO users(id, display_name, upstream_pool_id) VALUES ('user','user','pool')",
        )
        .execute(&pool)
        .await
        .expect("test user exists");
        pool
    }

    #[test]
    fn maps_linuxdo_levels_to_expected_quotas() {
        assert_eq!(
            linuxdo_tier_quota(0),
            Some(TierQuota {
                requests_per_five_minutes: 10,
                business_calls_per_hour: 30,
                daily_units: 10,
                monthly_units: 50,
            })
        );
        assert_eq!(
            linuxdo_tier_quota(1),
            Some(TierQuota {
                requests_per_five_minutes: 30,
                business_calls_per_hour: 60,
                daily_units: 20,
                monthly_units: 300,
            })
        );
        assert_eq!(
            linuxdo_tier_quota(2),
            Some(TierQuota {
                requests_per_five_minutes: 60,
                business_calls_per_hour: 240,
                daily_units: 70,
                monthly_units: 1_000,
            })
        );
        assert_eq!(
            linuxdo_tier_quota(3),
            Some(TierQuota {
                requests_per_five_minutes: 120,
                business_calls_per_hour: 600,
                daily_units: 200,
                monthly_units: 3_000,
            })
        );
        assert_eq!(
            linuxdo_tier_quota(4),
            Some(TierQuota {
                requests_per_five_minutes: 180,
                business_calls_per_hour: 900,
                daily_units: 400,
                monthly_units: 6_000,
            })
        );
        assert_eq!(linuxdo_tier_quota(5), None);
    }

    #[tokio::test]
    async fn assigns_and_upgrades_unmodified_linuxdo_tier_quotas() {
        let pool = test_pool().await;
        let mut connection = pool.acquire().await.expect("connection acquires");

        apply_linuxdo_tier_quota(&mut connection, "user", Some(2), None)
            .await
            .expect("initial tier quota is assigned");
        let row = sqlx::query(
            "SELECT version, requests_per_five_minutes, business_calls_per_hour, daily_units, monthly_units \
             FROM user_quota_assignments WHERE user_id = 'user'",
        )
        .fetch_one(&mut *connection)
        .await
        .expect("tier quota exists");
        assert_eq!(row.get::<i64, _>("version"), 1);
        assert_eq!(row.get::<i64, _>("monthly_units"), 1_000);

        apply_linuxdo_tier_quota(&mut connection, "user", Some(3), Some(2))
            .await
            .expect("tier upgrade is applied");
        let row = sqlx::query(
            "SELECT version, monthly_units FROM user_quota_assignments \
             WHERE user_id = 'user' ORDER BY version DESC LIMIT 1",
        )
        .fetch_one(&mut *connection)
        .await
        .expect("upgraded tier quota exists");
        assert_eq!(row.get::<i64, _>("version"), 2);
        assert_eq!(row.get::<i64, _>("monthly_units"), 3_000);
    }

    #[tokio::test]
    async fn does_not_overwrite_admin_customized_quotas() {
        let pool = test_pool().await;
        let mut connection = pool.acquire().await.expect("connection acquires");
        apply_linuxdo_tier_quota(&mut connection, "user", Some(2), None)
            .await
            .expect("initial tier quota is assigned");
        sqlx::query(
            "UPDATE user_quota_assignments SET monthly_units = 99 WHERE user_id = 'user' AND version = 1",
        )
        .execute(&mut *connection)
        .await
        .expect("admin override is stored");

        apply_linuxdo_tier_quota(&mut connection, "user", Some(1), Some(2))
            .await
            .expect("downgrade is ignored");
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_quota_assignments WHERE user_id = 'user'",
        )
        .fetch_one(&mut *connection)
        .await
        .expect("quota count reads");
        assert_eq!(count, 1);
    }
}
