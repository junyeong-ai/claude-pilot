//! Concurrency-safe event store with dedicated writer thread and read pool.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;

use chrono::DateTime;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::oneshot;
use tracing::debug;

use super::events::{AggregateId, DomainEvent, EventFilter, EventId, EventPayload};
use super::writer::{EventWriter, WriteCommand};
use super::{state_err, state_err_with};
use crate::error::Result;

const DEFAULT_READ_POOL_SIZE: usize = 4;

/// Raw event row tuple from database query.
/// Fields: (id, aggregate_id, version, global_seq, timestamp, payload, causation_id, correlation_id)
type EventRowTuple = (
    EventId,
    AggregateId,
    u32,
    u64,
    String,
    String,
    Option<String>,
    Option<String>,
);

struct ReadPool {
    connections: Vec<Mutex<Connection>>,
    next: std::sync::atomic::AtomicUsize,
}

impl ReadPool {
    fn new(db_path: &Path, size: usize) -> Result<Self> {
        let mut connections = Vec::with_capacity(size);
        for _ in 0..size {
            let conn = Connection::open_with_flags(
                db_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|e| state_err_with("Failed to open read connection", e))?;
            connections.push(Mutex::new(conn));
        }
        Ok(Self {
            connections,
            next: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn acquire(&self) -> parking_lot::MutexGuard<'_, Connection> {
        let idx =
            self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.connections.len();
        self.connections[idx].lock()
    }
}

struct EventStoreInner {
    writer_tx: Sender<WriteCommand>,
    read_pool: ReadPool,
    db_path: PathBuf,
    /// Holds the writer thread handle. Must not be dropped while EventStore is alive.
    #[allow(dead_code)]
    writer: EventWriter,
}

#[derive(Clone)]
pub struct EventStore {
    inner: Arc<EventStoreInner>,
}

impl EventStore {
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        Self::with_read_pool_size(db_path, DEFAULT_READ_POOL_SIZE)
    }

    pub fn with_read_pool_size(db_path: impl AsRef<Path>, pool_size: usize) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| state_err_with("Failed to create db directory", e))?;
        }

        let writer = EventWriter::new(db_path.clone())?;
        let writer_tx = writer.sender();

        let read_pool = ReadPool::new(&db_path, pool_size)?;

        Ok(Self {
            inner: Arc::new(EventStoreInner {
                writer_tx,
                read_pool,
                db_path,
                writer,
            }),
        })
    }

    pub async fn append(&self, event: DomainEvent) -> Result<u32> {
        self.append_with_version(event, None).await
    }

    pub async fn append_with_version(
        &self,
        event: DomainEvent,
        expected_version: Option<u32>,
    ) -> Result<u32> {
        let (tx, rx) = oneshot::channel();

        self.inner
            .writer_tx
            .send(WriteCommand::Append {
                event: Box::new(event),
                expected_version,
                response: tx,
            })
            .map_err(|_| state_err("Writer thread disconnected"))?;

        rx.await
            .map_err(|_| state_err("Writer response channel dropped"))?
    }

    pub async fn append_batch(&self, events: Vec<DomainEvent>) -> Result<Vec<u32>> {
        let (tx, rx) = oneshot::channel();

        self.inner
            .writer_tx
            .send(WriteCommand::AppendBatch {
                events,
                response: tx,
            })
            .map_err(|_| state_err("Writer thread disconnected"))?;

        rx.await
            .map_err(|_| state_err("Writer response channel dropped"))?
    }

    pub async fn query(&self, aggregate_id: &str, from_version: u32) -> Result<Vec<DomainEvent>> {
        let aggregate_id = aggregate_id.to_string();
        let inner = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = inner.read_pool.acquire();
            Self::query_impl(&guard, &aggregate_id, from_version)
        })
        .await
        .map_err(|e| state_err_with("Query task failed", e))?
    }

    pub async fn query_filtered(
        &self,
        aggregate_id: &str,
        filter: EventFilter,
    ) -> Result<Vec<DomainEvent>> {
        let aggregate_id = aggregate_id.to_string();
        let inner = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = inner.read_pool.acquire();
            let mut events = Self::query_impl(&guard, &aggregate_id, 0)?;
            events.retain(|e| filter.matches(e));
            if let Some(limit) = filter.limit {
                events.truncate(limit);
            }
            Ok(events)
        })
        .await
        .map_err(|e| state_err_with("Query task failed", e))?
    }

    pub async fn get_latest_version(&self, aggregate_id: &str) -> Result<Option<u32>> {
        let aggregate_id = aggregate_id.to_string();
        let inner = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = inner.read_pool.acquire();
            guard
                .query_row(
                    "SELECT MAX(version) FROM events WHERE aggregate_id = ?1",
                    params![&aggregate_id],
                    |row| row.get::<_, Option<u32>>(0),
                )
                .map_err(|e| state_err_with("Failed to get latest version", e))
        })
        .await
        .map_err(|e| state_err_with("Query task failed", e))?
    }

    pub async fn count_events(&self, aggregate_id: &str) -> Result<i64> {
        let aggregate_id = aggregate_id.to_string();
        let inner = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = inner.read_pool.acquire();
            guard
                .query_row(
                    "SELECT COUNT(*) FROM events WHERE aggregate_id = ?1",
                    params![&aggregate_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|e| state_err_with("Failed to count events", e))
        })
        .await
        .map_err(|e| state_err_with("Query task failed", e))?
    }

    pub async fn list_aggregates(&self) -> Result<Vec<AggregateId>> {
        let inner = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = inner.read_pool.acquire();
            let mut stmt = guard
                .prepare("SELECT DISTINCT aggregate_id FROM events ORDER BY aggregate_id")
                .map_err(|e| state_err_with("Failed to prepare statement", e))?;

            let rows = stmt
                .query_map([], |row| row.get(0))
                .map_err(|e| state_err_with("Failed to query aggregates", e))?;

            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| state_err_with("Failed to collect aggregates", e))
        })
        .await
        .map_err(|e| state_err_with("Query task failed", e))?
    }

    fn query_impl(
        conn: &Connection,
        aggregate_id: &str,
        from_version: u32,
    ) -> Result<Vec<DomainEvent>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, aggregate_id, version, global_seq, timestamp, payload, causation_id, correlation_id
                   FROM events
                   WHERE aggregate_id = ?1 AND version >= ?2
                   ORDER BY version ASC",
            )
            .map_err(|e| state_err_with("Failed to prepare statement", e))?;

        let rows = stmt
            .query_map(params![aggregate_id, from_version], |row| {
                let id: EventId = row.get(0)?;
                let aggregate_id: AggregateId = row.get(1)?;
                let version: u32 = row.get(2)?;
                let global_seq: i64 = row.get(3)?;
                let timestamp_str: String = row.get(4)?;
                let payload_str: String = row.get(5)?;
                let causation_id: Option<String> = row.get(6)?;
                let correlation_id: Option<String> = row.get(7)?;

                Ok((
                    id,
                    aggregate_id,
                    version,
                    global_seq as u64,
                    timestamp_str,
                    payload_str,
                    causation_id,
                    correlation_id,
                ))
            })
            .map_err(|e| state_err_with("Failed to query events", e))?;

        let mut events = Vec::new();
        for row_result in rows {
            let (
                id,
                aggregate_id,
                version,
                global_seq,
                timestamp_str,
                payload_str,
                causation_id,
                correlation_id,
            ) = row_result.map_err(|e| state_err_with("Failed to read row", e))?;

            let timestamp = DateTime::parse_from_rfc3339(&timestamp_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| state_err_with("Failed to parse timestamp", e))?;

            let payload: EventPayload = serde_json::from_str(&payload_str)
                .map_err(|e| state_err_with("Failed to deserialize payload", e))?;

            events.push(DomainEvent {
                id,
                aggregate_id,
                version,
                global_seq,
                timestamp,
                payload,
                causation_id: causation_id.map(EventId::from),
                correlation_id,
            });
        }

        debug!(
            aggregate_id,
            from_version,
            count = events.len(),
            "Events queried"
        );

        Ok(events)
    }

    pub fn db_path(&self) -> &Path {
        &self.inner.db_path
    }

    pub async fn query_from_global_seq(&self, from_seq: u64) -> Result<Vec<DomainEvent>> {
        let inner = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = inner.read_pool.acquire();
            Self::query_global_impl(&guard, from_seq, None)
        })
        .await
        .map_err(|e| state_err_with("Query task failed", e))?
    }

    pub async fn query_global_range(
        &self,
        from_seq: u64,
        limit: usize,
    ) -> Result<Vec<DomainEvent>> {
        let inner = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = inner.read_pool.acquire();
            Self::query_global_impl(&guard, from_seq, Some(limit))
        })
        .await
        .map_err(|e| state_err_with("Query task failed", e))?
    }

    pub async fn get_max_global_seq(&self) -> Result<u64> {
        let inner = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = inner.read_pool.acquire();
            guard
                .query_row("SELECT MAX(global_seq) FROM events", [], |row| {
                    row.get::<_, Option<i64>>(0)
                })
                .map(|opt| opt.unwrap_or(0) as u64)
                .map_err(|e| state_err_with("Failed to get max global_seq", e))
        })
        .await
        .map_err(|e| state_err_with("Query task failed", e))?
    }

    fn query_global_impl(
        conn: &Connection,
        from_seq: u64,
        limit: Option<usize>,
    ) -> Result<Vec<DomainEvent>> {
        let query = if limit.is_some() {
            "SELECT id, aggregate_id, version, global_seq, timestamp, payload, causation_id, correlation_id
               FROM events
               WHERE global_seq > ?1
               ORDER BY global_seq ASC
               LIMIT ?2"
        } else {
            "SELECT id, aggregate_id, version, global_seq, timestamp, payload, causation_id, correlation_id
               FROM events
               WHERE global_seq > ?1
               ORDER BY global_seq ASC"
        };

        let mut stmt = conn
            .prepare(query)
            .map_err(|e| state_err_with("Failed to prepare statement", e))?;

        let rows = if let Some(lim) = limit {
            stmt.query_map(params![from_seq as i64, lim as i64], Self::map_event_row)
        } else {
            stmt.query_map(params![from_seq as i64], Self::map_event_row)
        }
        .map_err(|e| state_err_with("Failed to query events", e))?;

        let mut events = Vec::new();
        for row_result in rows {
            let tuple = row_result.map_err(|e| state_err_with("Failed to read row", e))?;
            events.push(Self::tuple_to_event(tuple)?);
        }

        Ok(events)
    }

    fn map_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRowTuple> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get::<_, i64>(3)? as u64,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
        ))
    }

    fn tuple_to_event(tuple: EventRowTuple) -> Result<DomainEvent> {
        let (
            id,
            aggregate_id,
            version,
            global_seq,
            timestamp_str,
            payload_str,
            causation_id,
            correlation_id,
        ) = tuple;

        let timestamp = DateTime::parse_from_rfc3339(&timestamp_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .map_err(|e| state_err_with("Failed to parse timestamp", e))?;

        let payload: EventPayload = serde_json::from_str(&payload_str)
            .map_err(|e| state_err_with("Failed to deserialize payload", e))?;

        Ok(DomainEvent {
            id,
            aggregate_id,
            version,
            global_seq,
            timestamp,
            payload,
            causation_id: causation_id.map(EventId::from),
            correlation_id,
        })
    }

    /// Query events by correlation_id (indexed O(log n) lookup).
    ///
    /// Use this for recovering session/consensus state instead of scanning all events.
    pub async fn query_by_correlation(&self, correlation_id: &str) -> Result<Vec<DomainEvent>> {
        let correlation_id = correlation_id.to_string();
        let inner = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = inner.read_pool.acquire();
            let mut stmt = guard
                .prepare_cached(
                    "SELECT id, aggregate_id, version, global_seq, timestamp, payload, causation_id, correlation_id
                     FROM events
                     WHERE correlation_id = ?1
                     ORDER BY global_seq ASC",
                )
                .map_err(|e| state_err_with("Failed to prepare statement", e))?;

            let rows = stmt
                .query_map(params![&correlation_id], Self::map_event_row)
                .map_err(|e| state_err_with("Failed to query events by correlation", e))?;

            let mut events = Vec::new();
            for row_result in rows {
                let tuple = row_result.map_err(|e| state_err_with("Failed to read row", e))?;
                events.push(Self::tuple_to_event(tuple)?);
            }

            debug!(
                correlation_id,
                count = events.len(),
                "Events queried by correlation"
            );

            Ok(events)
        })
        .await
        .map_err(|e| state_err_with("Query task failed", e))?
    }

    /// Query the latest checkpoint event for a session.
    ///
    /// Returns the most recent ConsensusCheckpointCreated event for the given session.
    pub async fn query_latest_checkpoint(&self, session_id: &str) -> Result<Option<DomainEvent>> {
        let session_id = session_id.to_string();
        let inner = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = inner.read_pool.acquire();
            let mut stmt = guard
                .prepare_cached(
                    "SELECT id, aggregate_id, version, global_seq, timestamp, payload, causation_id, correlation_id
                     FROM events
                     WHERE correlation_id = ?1
                       AND event_type = 'ConsensusCheckpointCreated'
                     ORDER BY global_seq DESC
                     LIMIT 1",
                )
                .map_err(|e| state_err_with("Failed to prepare statement", e))?;

            let result = stmt
                .query_row(params![&session_id], Self::map_event_row)
                .optional()
                .map_err(|e| state_err_with("Failed to query checkpoint", e))?;

            match result {
                Some(tuple) => {
                    let event = Self::tuple_to_event(tuple)?;
                    debug!(session_id, "Checkpoint queried");
                    Ok(Some(event))
                }
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| state_err_with("Query task failed", e))?
    }

    /// Query events by event type (indexed O(log n) lookup).
    pub async fn query_by_event_type(
        &self,
        event_type: &str,
        limit: Option<usize>,
    ) -> Result<Vec<DomainEvent>> {
        let event_type = event_type.to_string();
        let inner = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = inner.read_pool.acquire();

            let query = if limit.is_some() {
                "SELECT id, aggregate_id, version, global_seq, timestamp, payload, causation_id, correlation_id
                 FROM events
                 WHERE event_type = ?1
                 ORDER BY global_seq DESC
                 LIMIT ?2"
            } else {
                "SELECT id, aggregate_id, version, global_seq, timestamp, payload, causation_id, correlation_id
                 FROM events
                 WHERE event_type = ?1
                 ORDER BY global_seq DESC"
            };

            let mut stmt = guard
                .prepare_cached(query)
                .map_err(|e| state_err_with("Failed to prepare statement", e))?;

            let rows = if let Some(lim) = limit {
                stmt.query_map(params![&event_type, lim as i64], Self::map_event_row)
            } else {
                stmt.query_map(params![&event_type], Self::map_event_row)
            }
            .map_err(|e| state_err_with("Failed to query events by type", e))?;

            let mut events = Vec::new();
            for row_result in rows {
                let tuple = row_result.map_err(|e| state_err_with("Failed to read row", e))?;
                events.push(Self::tuple_to_event(tuple)?);
            }

            debug!(event_type, count = events.len(), "Events queried by type");

            Ok(events)
        })
        .await
        .map_err(|e| state_err_with("Query task failed", e))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, EventStore) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test_events.db");
        let store = EventStore::new(&db_path).unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn test_append_and_query() {
        let (_dir, store) = temp_store();

        let event = DomainEvent::mission_created("mission-1", "Test mission");
        let version = store.append(event).await.unwrap();
        assert_eq!(version, 1);

        let events = store.query("mission-1", 0).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].version, 1);
    }

    #[tokio::test]
    async fn test_version_increment() {
        let (_dir, store) = temp_store();

        for i in 1..=5 {
            let event = DomainEvent::task_started("mission-1", &format!("task-{}", i), "desc");
            let version = store.append(event).await.unwrap();
            assert_eq!(version, i);
        }

        let events = store.query("mission-1", 0).await.unwrap();
        assert_eq!(events.len(), 5);

        for (i, event) in events.iter().enumerate() {
            assert_eq!(event.version, (i + 1) as u32);
        }
    }

    #[tokio::test]
    async fn test_optimistic_locking() {
        let (_dir, store) = temp_store();

        let event1 = DomainEvent::mission_created("mission-1", "Test");
        store.append(event1).await.unwrap();

        let event2 = DomainEvent::task_started("mission-1", "task-1", "desc");
        let result = store.append_with_version(event2.clone(), Some(0)).await;
        assert!(result.is_err());

        let result = store.append_with_version(event2, Some(1)).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_concurrent_appends() {
        let (_dir, store) = temp_store();

        let handles: Vec<_> = (0..50)
            .map(|i| {
                let store = store.clone();
                tokio::spawn(async move {
                    let event =
                        DomainEvent::task_started("mission-1", &format!("task-{}", i), "desc");
                    store.append(event).await
                })
            })
            .collect();

        let results: Vec<_> = futures::future::join_all(handles).await;
        assert!(results.iter().all(|r| r.is_ok()));

        let events = store.query("mission-1", 0).await.unwrap();
        assert_eq!(events.len(), 50);

        let mut versions: Vec<_> = events.iter().map(|e| e.version).collect();
        versions.sort();
        let expected: Vec<u32> = (1..=50).collect();
        assert_eq!(versions, expected);
    }

    #[tokio::test]
    async fn test_clone_shares_state() {
        let (_dir, store1) = temp_store();
        let store2 = store1.clone();

        let event = DomainEvent::mission_created("mission-1", "Test");
        store1.append(event).await.unwrap();

        let events = store2.query("mission-1", 0).await.unwrap();
        assert_eq!(events.len(), 1);
    }
}
