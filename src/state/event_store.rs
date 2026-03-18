//! Concurrency-safe event store with dedicated writer thread and read pool.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

use chrono::DateTime;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::oneshot;
use tracing::debug;

use super::events::{AggregateId, DomainEvent, EventFilter, EventId, EventPayload};
use super::writer::{EventWriter, WriteCommand};
use super::{state_err, state_err_with};
use crate::error::Result;

use crate::config::StateConfig;

/// SQL column list for the events table.
/// Used in all SELECT queries to ensure consistent field ordering with `map_event_row`.
const EVENT_COLUMNS: &str =
    "id, aggregate_id, version, global_seq, timestamp, payload, causation_id, correlation_id";

/// Error message for spawn_blocking join failures.
const BLOCKING_TASK_ERR: &str = "Blocking task join failed";

/// Error message when the writer thread's command channel is closed.
const WRITER_DISCONNECTED: &str = "Writer thread disconnected";

/// Error message when the writer's oneshot response channel is dropped.
const WRITER_RESPONSE_DROPPED: &str = "Writer response channel dropped";

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
    writer_tx: SyncSender<WriteCommand>,
    read_pool: ReadPool,
    db_path: PathBuf,
    default_query_limit: usize,
    operation_timeout: Duration,
    _writer: EventWriter,
}

#[derive(Clone)]
pub struct EventStore {
    inner: Arc<EventStoreInner>,
}

impl EventStore {
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        Self::with_config(db_path, &StateConfig::default())
    }

    pub fn with_config(db_path: impl AsRef<Path>, config: &StateConfig) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| state_err_with("Failed to create db directory", e))?;
        }

        let writer = EventWriter::with_config(
            db_path.clone(),
            &config.synchronous_mode,
            config.write_queue_capacity,
        )?;
        let writer_tx = writer.sender();
        let read_pool = ReadPool::new(&db_path, config.read_pool_size)?;

        Ok(Self {
            inner: Arc::new(EventStoreInner {
                writer_tx,
                read_pool,
                db_path,
                default_query_limit: config.default_query_limit,
                operation_timeout: Duration::from_secs(config.operation_timeout_secs),
                _writer: writer,
            }),
        })
    }

    async fn with_timeout<T>(
        &self,
        operation: &str,
        future: impl Future<Output = T>,
    ) -> std::result::Result<T, crate::error::PilotError> {
        tokio::time::timeout(self.inner.operation_timeout, future)
            .await
            .map_err(|_| state_err(format!(
                "Event store operation timed out after {}s: {}",
                self.inner.operation_timeout.as_secs(),
                operation,
            )))
    }

    /// Execute a blocking read query with timeout protection.
    ///
    /// Acquires a read pool connection, runs the closure on a blocking thread,
    /// and wraps the result with the configured operation timeout.
    /// Note: if the timeout fires, the spawned blocking task continues to completion
    /// (Tokio limitation) but its result is discarded.
    async fn query_blocking<T: Send + 'static>(
        &self,
        operation: &str,
        f: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let inner = Arc::clone(&self.inner);
        self.with_timeout(
            operation,
            tokio::task::spawn_blocking(move || {
                let guard = inner.read_pool.acquire();
                f(&guard)
            }),
        )
        .await?
        .map_err(|e| state_err_with(BLOCKING_TASK_ERR, e))?
    }

    /// Send a write command to the writer thread and await the response with timeout.
    async fn send_write<T>(
        &self,
        operation: &str,
        command: WriteCommand,
        rx: oneshot::Receiver<Result<T>>,
    ) -> Result<T> {
        self.inner
            .writer_tx
            .send(command)
            .map_err(|_| state_err(WRITER_DISCONNECTED))?;

        self.with_timeout(operation, rx)
            .await?
            .map_err(|_| state_err(WRITER_RESPONSE_DROPPED))?
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
        self.send_write(
            "append",
            WriteCommand::Append {
                event: Box::new(event),
                expected_version,
                response: tx,
            },
            rx,
        )
        .await
    }

    pub async fn append_batch(&self, events: Vec<DomainEvent>) -> Result<Vec<u32>> {
        let (tx, rx) = oneshot::channel();
        self.send_write(
            "append_batch",
            WriteCommand::AppendBatch {
                events,
                response: tx,
            },
            rx,
        )
        .await
    }

    pub async fn query(&self, aggregate_id: &str, from_version: u32) -> Result<Vec<DomainEvent>> {
        let aggregate_id = aggregate_id.to_string();
        self.query_blocking("query", move |conn| {
            Self::query_impl(conn, &aggregate_id, from_version)
        })
        .await
    }

    pub async fn query_filtered(
        &self,
        aggregate_id: &str,
        filter: EventFilter,
    ) -> Result<Vec<DomainEvent>> {
        let aggregate_id = aggregate_id.to_string();
        self.query_blocking("query_filtered", move |conn| {
            let mut sql = format!(
                "SELECT {EVENT_COLUMNS} FROM events WHERE aggregate_id = ?1",
            );
            let mut param_idx = 2u32;

            if let Some(ref types) = filter.event_types {
                let placeholders: Vec<String> = types
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("?{}", param_idx as usize + i))
                    .collect();
                sql.push_str(&format!(" AND event_type IN ({})", placeholders.join(",")));
                param_idx += types.len() as u32;
            }

            if filter.from_time.is_some() {
                sql.push_str(&format!(" AND timestamp >= ?{param_idx}"));
                param_idx += 1;
            }

            if filter.to_time.is_some() {
                sql.push_str(&format!(" AND timestamp <= ?{param_idx}"));
                param_idx += 1;
            }

            if filter.from_global_seq.is_some() {
                sql.push_str(&format!(" AND global_seq >= ?{param_idx}"));
                param_idx += 1;
            }

            sql.push_str(" ORDER BY global_seq ASC");

            if filter.limit.is_some() {
                sql.push_str(&format!(" LIMIT ?{param_idx}"));
            }

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| state_err_with("Failed to prepare filtered query", e))?;

            let mut bind_idx = 1usize;
            let agg_id_val = aggregate_id;
            stmt.raw_bind_parameter(bind_idx, &agg_id_val)
                .map_err(|e| state_err_with("Failed to bind aggregate_id", e))?;
            bind_idx += 1;

            if let Some(ref types) = filter.event_types {
                for t in types {
                    stmt.raw_bind_parameter(bind_idx, *t)
                        .map_err(|e| state_err_with("Failed to bind event_type", e))?;
                    bind_idx += 1;
                }
            }

            if let Some(ref from) = filter.from_time {
                stmt.raw_bind_parameter(bind_idx, from.to_rfc3339())
                    .map_err(|e| state_err_with("Failed to bind from_time", e))?;
                bind_idx += 1;
            }

            if let Some(ref to) = filter.to_time {
                stmt.raw_bind_parameter(bind_idx, to.to_rfc3339())
                    .map_err(|e| state_err_with("Failed to bind to_time", e))?;
                bind_idx += 1;
            }

            if let Some(from_seq) = filter.from_global_seq {
                stmt.raw_bind_parameter(bind_idx, from_seq as i64)
                    .map_err(|e| state_err_with("Failed to bind from_global_seq", e))?;
                bind_idx += 1;
            }

            if let Some(limit) = filter.limit {
                stmt.raw_bind_parameter(bind_idx, limit as i64)
                    .map_err(|e| state_err_with("Failed to bind limit", e))?;
            }

            let mut events = Vec::new();
            let mut rows = stmt.raw_query();

            while let Some(row) = rows
                .next()
                .map_err(|e| state_err_with("Failed to read row", e))?
            {
                let tuple = Self::map_event_row(row)
                    .map_err(|e| state_err_with("Failed to read row", e))?;
                let event = Self::tuple_to_event(tuple)?;

                if let Some(ref task_id) = filter.task_id
                    && event.payload.task_id() != Some(task_id.as_str())
                {
                    continue;
                }

                events.push(event);
            }

            Ok(events)
        })
        .await
    }

    pub async fn latest_version(&self, aggregate_id: &str) -> Result<Option<u32>> {
        let aggregate_id = aggregate_id.to_string();
        self.query_blocking("latest_version", move |conn| {
            conn.query_row(
                "SELECT MAX(version) FROM events WHERE aggregate_id = ?1",
                params![&aggregate_id],
                |row| row.get::<_, Option<u32>>(0),
            )
            .map_err(|e| state_err_with("Failed to get latest version", e))
        })
        .await
    }

    pub async fn count_events(&self, aggregate_id: &str) -> Result<i64> {
        let aggregate_id = aggregate_id.to_string();
        self.query_blocking("count_events", move |conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM events WHERE aggregate_id = ?1",
                params![&aggregate_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| state_err_with("Failed to count events", e))
        })
        .await
    }

    pub async fn list_aggregates(&self) -> Result<Vec<AggregateId>> {
        self.query_blocking("list_aggregates", move |conn| {
            let mut stmt = conn
                .prepare_cached("SELECT DISTINCT aggregate_id FROM events ORDER BY aggregate_id")
                .map_err(|e| state_err_with("Failed to prepare statement", e))?;

            let rows = stmt
                .query_map([], |row| row.get(0))
                .map_err(|e| state_err_with("Failed to query aggregates", e))?;

            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| state_err_with("Failed to collect aggregates", e))
        })
        .await
    }

    fn query_impl(
        conn: &Connection,
        aggregate_id: &str,
        from_version: u32,
    ) -> Result<Vec<DomainEvent>> {
        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT {EVENT_COLUMNS} FROM events \
                 WHERE aggregate_id = ?1 AND version >= ?2 \
                 ORDER BY version ASC",
            ))
            .map_err(|e| state_err_with("Failed to prepare statement", e))?;

        let rows = stmt
            .query_map(params![aggregate_id, from_version], Self::map_event_row)
            .map_err(|e| state_err_with("Failed to query events", e))?;

        let events = Self::collect_events(rows)?;

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

    pub async fn query_aggregate_from_seq(
        &self,
        aggregate_id: &str,
        from_seq: u64,
    ) -> Result<Vec<DomainEvent>> {
        let aggregate_id = aggregate_id.to_string();
        self.query_blocking("query_aggregate_from_seq", move |conn| {
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {EVENT_COLUMNS} FROM events \
                     WHERE aggregate_id = ?1 AND global_seq > ?2 \
                     ORDER BY global_seq ASC",
                ))
                .map_err(|e| state_err_with("Failed to prepare statement", e))?;

            let rows = stmt
                .query_map(
                    params![&aggregate_id, from_seq as i64],
                    Self::map_event_row,
                )
                .map_err(|e| state_err_with("Failed to query events", e))?;

            let events = Self::collect_events(rows)?;

            debug!(
                aggregate_id,
                from_seq,
                count = events.len(),
                "Events queried for aggregate from seq"
            );

            Ok(events)
        })
        .await
    }

    pub async fn query_aggregate_from_seq_until(
        &self,
        aggregate_id: &str,
        from_seq: u64,
        until_seq: u64,
    ) -> Result<Vec<DomainEvent>> {
        let aggregate_id = aggregate_id.to_string();
        self.query_blocking("query_aggregate_from_seq_until", move |conn| {
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {EVENT_COLUMNS} FROM events \
                     WHERE aggregate_id = ?1 AND global_seq > ?2 AND global_seq <= ?3 \
                     ORDER BY global_seq ASC",
                ))
                .map_err(|e| state_err_with("Failed to prepare statement", e))?;

            let rows = stmt
                .query_map(
                    params![&aggregate_id, from_seq as i64, until_seq as i64],
                    Self::map_event_row,
                )
                .map_err(|e| state_err_with("Failed to query events", e))?;

            Self::collect_events(rows)
        })
        .await
    }

    pub async fn query_from_global_seq(&self, from_seq: u64) -> Result<Vec<DomainEvent>> {
        let limit = self.inner.default_query_limit;
        self.query_blocking("query_from_global_seq", move |conn| {
            Self::query_global_impl(conn, from_seq, Some(limit))
        })
        .await
    }

    pub async fn max_global_seq(&self) -> Result<u64> {
        self.query_blocking("max_global_seq", move |conn| {
            conn.query_row("SELECT MAX(global_seq) FROM events", [], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .map(|opt| u64::try_from(opt.unwrap_or(0)).unwrap_or(0))
            .map_err(|e| state_err_with("Failed to get max global_seq", e))
        })
        .await
    }

    fn query_global_impl(
        conn: &Connection,
        from_seq: u64,
        limit: Option<usize>,
    ) -> Result<Vec<DomainEvent>> {
        let effective_limit = limit.unwrap_or(10_000);

        let mut stmt = conn
            .prepare_cached(&format!(
                "SELECT {EVENT_COLUMNS} FROM events \
                 WHERE global_seq > ?1 \
                 ORDER BY global_seq ASC \
                 LIMIT ?2",
            ))
            .map_err(|e| state_err_with("Failed to prepare statement", e))?;

        let rows = stmt
            .query_map(
                params![from_seq as i64, effective_limit as i64],
                Self::map_event_row,
            )
            .map_err(|e| state_err_with("Failed to query events", e))?;

        Self::collect_events(rows)
    }

    fn collect_events(
        rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<EventRowTuple>>,
    ) -> Result<Vec<DomainEvent>> {
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
            u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
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

    pub async fn archive_events_through(&self, global_seq: u64) -> Result<u64> {
        let (tx, rx) = oneshot::channel();
        self.send_write(
            "archive_events_through",
            WriteCommand::ArchiveThrough {
                global_seq,
                response: tx,
            },
            rx,
        )
        .await
    }

    pub async fn compact(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send_write("compact", WriteCommand::Compact { response: tx }, rx)
            .await
    }

    pub async fn database_size_bytes(&self) -> Result<u64> {
        let db_path = self.inner.db_path.clone();
        self.with_timeout(
            "database_size_bytes",
            tokio::task::spawn_blocking(move || {
                std::fs::metadata(&db_path)
                    .map(|m| m.len())
                    .map_err(|e| state_err_with("Failed to get database size", e))
            }),
        )
        .await?
        .map_err(|e| state_err_with(BLOCKING_TASK_ERR, e))?
    }

    /// Query events by correlation_id (indexed O(log n) lookup).
    ///
    /// Use this for recovering session/consensus state instead of scanning all events.
    pub async fn query_by_correlation(&self, correlation_id: &str) -> Result<Vec<DomainEvent>> {
        let correlation_id = correlation_id.to_string();
        let limit = self.inner.default_query_limit;
        self.query_blocking("query_by_correlation", move |conn| {
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {EVENT_COLUMNS} FROM events \
                     WHERE correlation_id = ?1 \
                     ORDER BY global_seq ASC \
                     LIMIT ?2",
                ))
                .map_err(|e| state_err_with("Failed to prepare statement", e))?;

            let rows = stmt
                .query_map(
                    params![&correlation_id, limit as i64],
                    Self::map_event_row,
                )
                .map_err(|e| state_err_with("Failed to query events by correlation", e))?;

            let events = Self::collect_events(rows)?;

            debug!(
                correlation_id,
                count = events.len(),
                "Events queried by correlation"
            );

            Ok(events)
        })
        .await
    }

    /// Query the latest checkpoint event for a session.
    ///
    /// Returns the most recent ConsensusCheckpointCreated event for the given session.
    pub async fn query_latest_checkpoint(&self, session_id: &str) -> Result<Option<DomainEvent>> {
        let session_id = session_id.to_string();
        self.query_blocking("query_latest_checkpoint", move |conn| {
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {EVENT_COLUMNS} FROM events \
                     WHERE correlation_id = ?1 \
                       AND event_type = 'consensus_checkpoint_created' \
                     ORDER BY global_seq DESC \
                     LIMIT 1",
                ))
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
    }

    /// Query events by event type (indexed O(log n) lookup).
    pub async fn query_by_event_type(
        &self,
        event_type: &str,
        limit: Option<usize>,
    ) -> Result<Vec<DomainEvent>> {
        let event_type = event_type.to_string();
        let default_limit = self.inner.default_query_limit;
        self.query_blocking("query_by_event_type", move |conn| {
            let effective_limit = limit.unwrap_or(default_limit);

            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {EVENT_COLUMNS} FROM events \
                     WHERE event_type = ?1 \
                     ORDER BY global_seq DESC \
                     LIMIT ?2",
                ))
                .map_err(|e| state_err_with("Failed to prepare statement", e))?;

            let rows = stmt
                .query_map(
                    params![&event_type, effective_limit as i64],
                    Self::map_event_row,
                )
                .map_err(|e| state_err_with("Failed to query events by type", e))?;

            let events = Self::collect_events(rows)?;

            debug!(event_type, count = events.len(), "Events queried by type");

            Ok(events)
        })
        .await
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

    // ── query_filtered tests ──

    #[tokio::test]
    async fn test_query_filtered_by_event_type() {
        let (_dir, store) = temp_store();

        store.append(DomainEvent::mission_created("m-1", "Test mission")).await.unwrap();
        store.append(DomainEvent::task_started("m-1", "t-1", "task desc")).await.unwrap();
        store.append(DomainEvent::task_started("m-1", "t-2", "task desc 2")).await.unwrap();

        let filter = EventFilter::new().with_types(vec!["task_started"]);
        let events = store.query_filtered("m-1", filter).await.unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.event_type() == "task_started"));
    }

    #[tokio::test]
    async fn test_query_filtered_with_limit() {
        let (_dir, store) = temp_store();

        for i in 0..10 {
            store.append(DomainEvent::task_started("m-1", &format!("t-{}", i), "desc")).await.unwrap();
        }

        let filter = EventFilter::new().with_limit(3);
        let events = store.query_filtered("m-1", filter).await.unwrap();
        assert_eq!(events.len(), 3);
    }

    #[tokio::test]
    async fn test_query_filtered_empty_result() {
        let (_dir, store) = temp_store();

        store.append(DomainEvent::mission_created("m-1", "Test")).await.unwrap();

        let filter = EventFilter::new();
        let events = store.query_filtered("nonexistent-agg", filter).await.unwrap();
        assert!(events.is_empty());
    }

    // ── query_by_correlation tests ──

    #[tokio::test]
    async fn test_query_by_correlation_found() {
        let (_dir, store) = temp_store();

        let event = DomainEvent::mission_created("m-1", "Test")
            .with_correlation("session-abc");
        store.append(event).await.unwrap();

        let event2 = DomainEvent::task_started("m-1", "t-1", "desc")
            .with_correlation("session-abc");
        store.append(event2).await.unwrap();

        // Different correlation
        let event3 = DomainEvent::task_started("m-1", "t-2", "desc")
            .with_correlation("session-xyz");
        store.append(event3).await.unwrap();

        let events = store.query_by_correlation("session-abc").await.unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.correlation_id.as_deref() == Some("session-abc")));
    }

    #[tokio::test]
    async fn test_query_by_correlation_not_found() {
        let (_dir, store) = temp_store();

        store.append(DomainEvent::mission_created("m-1", "Test")).await.unwrap();

        let events = store.query_by_correlation("nonexistent-corr").await.unwrap();
        assert!(events.is_empty());
    }

    // ── query_latest_checkpoint tests ──

    #[tokio::test]
    async fn test_query_latest_checkpoint() {
        let (_dir, store) = temp_store();

        let cp1 = DomainEvent::new(
            "session-1",
            EventPayload::ConsensusCheckpointCreated {
                session_id: "session-1".into(),
                checkpoint_id: "cp-1".into(),
                round: 1,
                tier_level: None,
                state_hash: "hash-1".into(),
            },
        ).with_correlation("session-1");
        store.append(cp1).await.unwrap();

        let cp2 = DomainEvent::new(
            "session-1",
            EventPayload::ConsensusCheckpointCreated {
                session_id: "session-1".into(),
                checkpoint_id: "cp-2".into(),
                round: 3,
                tier_level: None,
                state_hash: "hash-2".into(),
            },
        ).with_correlation("session-1");
        store.append(cp2).await.unwrap();

        let latest = store.query_latest_checkpoint("session-1").await.unwrap();
        assert!(latest.is_some());
        let event = latest.unwrap();
        match &event.payload {
            EventPayload::ConsensusCheckpointCreated { checkpoint_id, round, .. } => {
                assert_eq!(checkpoint_id, "cp-2");
                assert_eq!(*round, 3);
            }
            _ => panic!("Expected ConsensusCheckpointCreated"),
        }
    }

    #[tokio::test]
    async fn test_query_latest_checkpoint_none() {
        let (_dir, store) = temp_store();

        store.append(DomainEvent::mission_created("m-1", "Test")).await.unwrap();

        let result = store.query_latest_checkpoint("nonexistent-session").await.unwrap();
        assert!(result.is_none());
    }

    // ── append_batch tests ──

    #[tokio::test]
    async fn test_append_batch() {
        let (_dir, store) = temp_store();

        let events = vec![
            DomainEvent::mission_created("m-1", "Mission A"),
            DomainEvent::task_started("m-1", "t-1", "desc 1"),
            DomainEvent::task_started("m-1", "t-2", "desc 2"),
        ];
        let versions = store.append_batch(events).await.unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions, vec![1, 2, 3]);

        let all = store.query("m-1", 0).await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn test_append_batch_empty() {
        let (_dir, store) = temp_store();

        let versions = store.append_batch(vec![]).await.unwrap();
        assert!(versions.is_empty());
    }

    // ── archive + compact tests ──

    #[tokio::test]
    async fn test_archive_events_through() {
        let (_dir, store) = temp_store();

        for i in 0..5 {
            store.append(DomainEvent::task_started("m-1", &format!("t-{}", i), "desc")).await.unwrap();
        }

        let max_seq = store.max_global_seq().await.unwrap();
        // archive_events_through uses global_seq <= threshold (inclusive)
        let archived = store.archive_events_through(3).await.unwrap();
        assert_eq!(archived, 3, "Should archive 3 events (global_seq 1, 2, 3)");

        let remaining = store.query("m-1", 0).await.unwrap();
        assert_eq!(remaining.len(), 2, "2 events should remain (global_seq 4, 5)");

        let max_seq_after = store.max_global_seq().await.unwrap();
        assert_eq!(max_seq, max_seq_after);
    }

    #[tokio::test]
    async fn test_compact() {
        let (_dir, store) = temp_store();

        for i in 0..10 {
            store.append(DomainEvent::task_started("m-1", &format!("t-{}", i), "desc")).await.unwrap();
        }

        let size_before = store.database_size_bytes().await.unwrap();
        assert!(size_before > 0, "Database should have non-zero size");

        // Compact should succeed without error
        store.compact().await.unwrap();

        // Database should still be functional
        let events = store.query("m-1", 0).await.unwrap();
        assert_eq!(events.len(), 10);

        let size_after = store.database_size_bytes().await.unwrap();
        assert!(size_after > 0, "Database should still exist after compact");
    }

    // ── query_by_event_type tests ──

    #[tokio::test]
    async fn test_query_by_event_type_with_limit() {
        let (_dir, store) = temp_store();

        for i in 0..5 {
            store.append(DomainEvent::task_started("m-1", &format!("t-{}", i), "desc")).await.unwrap();
        }
        store.append(DomainEvent::mission_created("m-2", "Other")).await.unwrap();

        let events = store.query_by_event_type("task_started", Some(3)).await.unwrap();
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|e| e.event_type() == "task_started"));
    }
}
