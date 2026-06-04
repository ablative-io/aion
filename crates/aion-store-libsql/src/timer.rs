//! Durable timer scheduling and expiry retrieval.

use aion_store::{StoreError, TimerEntry, TimerId, WorkflowId};
use chrono::{DateTime, SecondsFormat, Utc};

const UPSERT_TIMER_SQL: &str = "
INSERT INTO timers (workflow_id, timer_id, fire_at)
VALUES (?1, ?2, ?3)
ON CONFLICT(workflow_id, timer_id) DO UPDATE SET fire_at = excluded.fire_at";

const EXPIRED_TIMERS_SQL: &str = "
SELECT workflow_id, timer_id, fire_at
FROM timers
WHERE fire_at <= ?1
ORDER BY fire_at ASC, workflow_id ASC, timer_id ASC";

/// Persist or replace a durable timer row keyed by workflow and timer identifiers.
///
/// # Errors
///
/// Returns `StoreError::Serialization` when identifiers cannot be encoded, and
/// `StoreError::Backend` when libSQL rejects the upsert.
pub(crate) async fn schedule_timer(
    conn: &libsql::Connection,
    workflow_id: &WorkflowId,
    timer_id: &TimerId,
    fire_at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let workflow_id = encode_workflow_id(workflow_id)?;
    let timer_id = encode_timer_id(timer_id)?;
    let fire_at = encode_fire_at(fire_at);

    conn.execute(UPSERT_TIMER_SQL, (workflow_id, timer_id, fire_at))
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    Ok(())
}

/// Return every durable timer due at or before `as_of`.
///
/// # Errors
///
/// Returns `StoreError::Backend` for libSQL read failures and `StoreError::Serialization` when a
/// persisted row cannot be reconstructed into Aion timer types.
pub(crate) async fn expired_timers(
    conn: &libsql::Connection,
    as_of: DateTime<Utc>,
) -> Result<Vec<TimerEntry>, StoreError> {
    let as_of = encode_fire_at(as_of);
    let mut rows = conn
        .query(EXPIRED_TIMERS_SQL, libsql::params![as_of])
        .await
        .map_err(|error| crate::error::libsql_error(&error))?;

    let mut timers = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| crate::error::libsql_error(&error))?
    {
        let workflow_id: String = row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error))?;
        let timer_id: String = row
            .get(1)
            .map_err(|error| crate::error::libsql_error(&error))?;
        let fire_at: String = row
            .get(2)
            .map_err(|error| crate::error::libsql_error(&error))?;

        timers.push(TimerEntry {
            workflow_id: decode_workflow_id(&workflow_id)?,
            timer_id: decode_timer_id(&timer_id)?,
            fire_at: decode_fire_at(&fire_at)?,
        });
    }

    Ok(timers)
}

fn encode_workflow_id(workflow_id: &WorkflowId) -> Result<String, StoreError> {
    serde_json::to_string(workflow_id).map_err(|error| crate::error::serde_json_error(&error))
}

fn decode_workflow_id(value: &str) -> Result<WorkflowId, StoreError> {
    serde_json::from_str(value).map_err(|error| crate::error::serde_json_error(&error))
}

fn encode_timer_id(timer_id: &TimerId) -> Result<String, StoreError> {
    serde_json::to_string(timer_id).map_err(|error| crate::error::serde_json_error(&error))
}

fn decode_timer_id(value: &str) -> Result<TimerId, StoreError> {
    serde_json::from_str(value).map_err(|error| crate::error::serde_json_error(&error))
}

fn encode_fire_at(fire_at: DateTime<Utc>) -> String {
    fire_at.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn decode_fire_at(value: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(value)
        .map(|date_time| date_time.with_timezone(&Utc))
        .map_err(|error| StoreError::Serialization(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use aion_store::{StoreError, TimerId, WorkflowId};
    use chrono::{DateTime, TimeZone, Utc};

    use super::{decode_fire_at, encode_fire_at, schedule_timer};
    use crate::config::{LibSqlConfig, LibSqlMode};

    #[tokio::test]
    async fn schedule_timer_replaces_existing_row_for_same_key() -> Result<(), StoreError> {
        let conn = open_test_connection("upsert").await?;
        let workflow_id = WorkflowId::new_v4();
        let timer_id = TimerId::named("wake-up").map_err(|error| {
            StoreError::Serialization(format!("failed to create test timer id: {error}"))
        })?;
        let original_fire_at = instant(2026, 6, 3, 1, 0, 0)?;
        let replacement_fire_at = instant(2026, 6, 3, 2, 30, 0)?;

        schedule_timer(&conn, &workflow_id, &timer_id, original_fire_at).await?;
        schedule_timer(&conn, &workflow_id, &timer_id, replacement_fire_at).await?;

        let (count, stored_fire_at) =
            stored_timer_count_and_fire_at(&conn, &workflow_id, &timer_id).await?;
        assert_eq!(count, 1);
        assert_eq!(decode_fire_at(&stored_fire_at)?, replacement_fire_at);

        Ok(())
    }

    #[tokio::test]
    async fn expired_timers_returns_due_rows_including_as_of_boundary() -> Result<(), StoreError> {
        let conn = open_test_connection("expired").await?;
        let past_workflow = WorkflowId::new_v4();
        let boundary_workflow = WorkflowId::new_v4();
        let future_workflow = WorkflowId::new_v4();
        let past_timer = TimerId::named("past").map_err(|error| {
            StoreError::Serialization(format!("failed to create past timer id: {error}"))
        })?;
        let boundary_timer = TimerId::named("boundary").map_err(|error| {
            StoreError::Serialization(format!("failed to create boundary timer id: {error}"))
        })?;
        let future_timer = TimerId::named("future").map_err(|error| {
            StoreError::Serialization(format!("failed to create future timer id: {error}"))
        })?;
        let as_of = instant(2026, 6, 3, 12, 0, 0)?;

        schedule_timer(
            &conn,
            &past_workflow,
            &past_timer,
            instant(2026, 6, 3, 11, 0, 0)?,
        )
        .await?;
        schedule_timer(&conn, &boundary_workflow, &boundary_timer, as_of).await?;
        schedule_timer(
            &conn,
            &future_workflow,
            &future_timer,
            instant(2026, 6, 3, 13, 0, 0)?,
        )
        .await?;

        let expired = super::expired_timers(&conn, as_of).await?;

        assert_eq!(expired.len(), 2);
        assert!(
            expired.iter().any(|entry| {
                entry.workflow_id == past_workflow && entry.timer_id == past_timer
            })
        );
        assert!(expired.iter().any(|entry| {
            entry.workflow_id == boundary_workflow && entry.timer_id == boundary_timer
        }));
        assert!(!expired.iter().any(|entry| {
            entry.workflow_id == future_workflow && entry.timer_id == future_timer
        }));
        assert!(expired.iter().all(|entry| entry.fire_at <= as_of));

        Ok(())
    }

    #[test]
    fn fire_at_encoding_round_trips_losslessly() -> Result<(), StoreError> {
        let fire_at = Utc
            .with_ymd_and_hms(2026, 6, 3, 2, 30, 0)
            .single()
            .ok_or_else(|| StoreError::Serialization(String::from("invalid test instant")))?;

        assert_eq!(decode_fire_at(&encode_fire_at(fire_at))?, fire_at);
        Ok(())
    }

    async fn open_test_connection(name: &str) -> Result<libsql::Connection, StoreError> {
        let config = LibSqlConfig {
            mode: LibSqlMode::Embedded {
                path: unique_temp_path(name),
            },
            journal_mode: None,
            synchronous: None,
            sync_interval_seconds: None,
        };
        let conn = crate::connection::open_connection(&config)
            .await?
            .connection;
        crate::schema::ensure_schema(&conn).await?;
        Ok(conn)
    }

    async fn stored_timer_count_and_fire_at(
        conn: &libsql::Connection,
        workflow_id: &WorkflowId,
        timer_id: &TimerId,
    ) -> Result<(i64, String), StoreError> {
        let workflow_id = super::encode_workflow_id(workflow_id)?;
        let timer_id = super::encode_timer_id(timer_id)?;
        let mut rows = conn
            .query(
                "SELECT COUNT(*), MAX(fire_at) FROM timers WHERE workflow_id = ?1 AND timer_id = ?2",
                (workflow_id, timer_id),
            )
            .await
            .map_err(|error| crate::error::libsql_error(&error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| crate::error::libsql_error(&error))?
            .ok_or_else(|| StoreError::Backend(String::from("timer aggregate returned no row")))?;
        let count = row
            .get(0)
            .map_err(|error| crate::error::libsql_error(&error))?;
        let fire_at = row
            .get(1)
            .map_err(|error| crate::error::libsql_error(&error))?;

        Ok((count, fire_at))
    }

    fn instant(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Result<DateTime<Utc>, StoreError> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
            .ok_or_else(|| StoreError::Serialization(String::from("invalid test instant")))
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "aion-store-libsql-timer-{name}-{}-{nanos}.db",
            std::process::id()
        ))
    }
}
