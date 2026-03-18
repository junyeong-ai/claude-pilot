//! Dedicated writer thread for SQLite event store.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use rusqlite::{Connection, params};
use tracing::{debug, error, warn};

use super::{events::DomainEvent, state_err, state_err_with};
use crate::error::Result;

#[derive(Debug)]
pub(crate) struct SequenceAllocator {
    next_seq: AtomicU64,
}

impl SequenceAllocator {
    pub fn new(initial_seq: u64) -> Self {
        Self {
            next_seq: AtomicU64::new(initial_seq),
        }
    }

    pub fn next(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::SeqCst)
    }
}

pub(super) enum WriteCommand {
    Append {
        event: Box<DomainEvent>,
        expected_version: Option<u32>,
        response: tokio::sync::oneshot::Sender<Result<u32>>,
    },
    AppendBatch {
        events: Vec<DomainEvent>,
        response: tokio::sync::oneshot::Sender<Result<Vec<u32>>>,
    },
    ArchiveThrough {
        global_seq: u64,
        response: tokio::sync::oneshot::Sender<Result<u64>>,
    },
    Compact {
        response: tokio::sync::oneshot::Sender<Result<()>>,
    },
    Shutdown,
}

pub(super) struct EventWriter {
    tx: SyncSender<WriteCommand>,
    handle: Option<JoinHandle<()>>,
}

impl EventWriter {
    pub fn with_config(
        db_path: PathBuf,
        sync_mode: &str,
        queue_capacity: usize,
    ) -> Result<Self> {
        let sync_mode = sync_mode.to_string();
        let (tx, rx) = mpsc::sync_channel::<WriteCommand>(queue_capacity);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();

        let handle = thread::Builder::new()
            .name("event-writer".into())
            .spawn(move || match Self::init_db_with_config(&db_path, &sync_mode) {
                Ok(conn) => {
                    let max_seq = Self::get_max_global_seq(&conn).unwrap_or(0);
                    let allocator = SequenceAllocator::new(max_seq + 1);
                    let _ = ready_tx.send(Ok(()));
                    Self::process_commands(&conn, rx, allocator);
                }
                Err(e) => {
                    error!(error = %e, "Event writer init failed");
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| state_err_with("Failed to spawn writer thread", e))?;

        ready_rx
            .recv()
            .map_err(|_| state_err("Writer thread died during init"))??;

        Ok(Self {
            tx,
            handle: Some(handle),
        })
    }

    pub fn sender(&self) -> SyncSender<WriteCommand> {
        self.tx.clone()
    }

    fn get_max_global_seq(conn: &Connection) -> Result<u64> {
        conn.query_row("SELECT MAX(global_seq) FROM events", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .map(|opt| {
            let seq = opt.unwrap_or(0);
            if seq < 0 {
                warn!("Negative sequence number {} detected, resetting to 0", seq);
            }
            u64::try_from(seq.max(0)).unwrap_or(0)
        })
        .map_err(|e| state_err_with("Failed to get max global_seq", e))
    }

    fn init_db_with_config(db_path: &PathBuf, sync_mode: &str) -> Result<Connection> {
        let conn =
            Connection::open(db_path).map_err(|e| state_err_with("Failed to open database", e))?;
        let pragma = format!("PRAGMA journal_mode=WAL; PRAGMA synchronous={sync_mode};");
        conn.execute_batch(&pragma)
            .map_err(|e| state_err_with("Failed to set WAL mode", e))?;
        Self::init_schema(&conn)?;
        Ok(conn)
    }

    fn process_commands(
        conn: &Connection,
        rx: Receiver<WriteCommand>,
        allocator: SequenceAllocator,
    ) {
        for cmd in rx {
            match cmd {
                WriteCommand::Append {
                    event,
                    expected_version,
                    response,
                } => {
                    let result = Self::append_event(conn, *event, expected_version, &allocator);
                    if response.send(result).is_err() {
                        warn!("Event write response receiver dropped");
                    }
                }
                WriteCommand::AppendBatch { events, response } => {
                    let result = Self::append_batch(conn, events, &allocator);
                    if response.send(result).is_err() {
                        warn!("Event write response receiver dropped");
                    }
                }
                WriteCommand::ArchiveThrough {
                    global_seq,
                    response,
                } => {
                    let result = Self::archive_through(conn, global_seq);
                    if response.send(result).is_err() {
                        warn!("Archive response receiver dropped");
                    }
                }
                WriteCommand::Compact { response } => {
                    let result = Self::compact(conn);
                    if response.send(result).is_err() {
                        warn!("Compact response receiver dropped");
                    }
                }
                WriteCommand::Shutdown => {
                    debug!("Writer thread received shutdown signal");
                    break;
                }
            }
        }
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                aggregate_id TEXT NOT NULL,
                version INTEGER NOT NULL,
                global_seq INTEGER NOT NULL DEFAULT 0,
                event_type TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                payload TEXT NOT NULL,
                causation_id TEXT,
                correlation_id TEXT
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_aggregate_version
                ON events(aggregate_id, version);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_global_seq
                ON events(global_seq) WHERE global_seq > 0;
            CREATE INDEX IF NOT EXISTS idx_event_type
                ON events(event_type);
            CREATE INDEX IF NOT EXISTS idx_timestamp
                ON events(timestamp);
            CREATE INDEX IF NOT EXISTS idx_correlation_id
                ON events(correlation_id) WHERE correlation_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_event_type_correlation
                ON events(event_type, correlation_id) WHERE correlation_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_aggregate_global_seq
                ON events(aggregate_id, global_seq);

            CREATE TABLE IF NOT EXISTS snapshots (
                id TEXT PRIMARY KEY,
                aggregate_id TEXT NOT NULL,
                version INTEGER NOT NULL,
                snapshot_type TEXT NOT NULL,
                data TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_snapshot_aggregate
                ON snapshots(aggregate_id, version DESC);

            CREATE TABLE IF NOT EXISTS projection_snapshots (
                id TEXT PRIMARY KEY,
                aggregate_id TEXT NOT NULL,
                global_seq INTEGER NOT NULL,
                projection_type TEXT NOT NULL,
                data BLOB NOT NULL,
                checksum INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_projection_snapshot_aggregate
                ON projection_snapshots(aggregate_id, global_seq DESC);

            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER PRIMARY KEY
            );
            INSERT OR IGNORE INTO schema_version VALUES (2);
            ",
        )
        .map_err(|e| state_err_with("Failed to init schema", e))?;

        Self::run_migrations(conn)?;
        Ok(())
    }

    fn run_migrations(conn: &Connection) -> Result<()> {
        let current_version: i32 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap_or(1);

        if current_version < 2 {
            let has_global_seq: bool = conn
                .prepare("SELECT global_seq FROM events LIMIT 1")
                .is_ok();

            if !has_global_seq {
                conn.execute_batch(
                    r"
                    ALTER TABLE events ADD COLUMN global_seq INTEGER NOT NULL DEFAULT 0;
                    UPDATE events SET global_seq = rowid WHERE global_seq = 0;
                    CREATE UNIQUE INDEX IF NOT EXISTS idx_global_seq ON events(global_seq) WHERE global_seq > 0;
                    UPDATE schema_version SET version = 2;
                    ",
                )
                .map_err(|e| state_err_with("Failed to run migration", e))?;
            }
        }

        Ok(())
    }

    fn append_event(
        conn: &Connection,
        mut event: DomainEvent,
        expected_version: Option<u32>,
        allocator: &SequenceAllocator,
    ) -> Result<u32> {
        // Safety: unchecked_transaction is safe here because this connection is
        // exclusively owned by the single writer thread (no concurrent access).
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| state_err_with("Failed to start transaction", e))?;

        let current_version: Option<u32> = tx
            .query_row(
                "SELECT MAX(version) FROM events WHERE aggregate_id = ?1",
                params![&event.aggregate_id],
                |row| row.get(0),
            )
            .ok();

        let next_version = current_version.map(|v| v + 1).unwrap_or(1);

        if let Some(expected) = expected_version {
            let actual = current_version.unwrap_or(0);
            if actual != expected {
                return Err(state_err(format!(
                    "Concurrency conflict: expected version {}, actual {}",
                    expected, actual
                )));
            }
        }

        event.version = next_version;
        event.global_seq = allocator.next();

        let payload = serde_json::to_string(&event.payload)
            .map_err(|e| state_err_with("Failed to serialize payload", e))?;

        tx.execute(
            "INSERT INTO events (id, aggregate_id, version, global_seq, event_type, timestamp, payload, causation_id, correlation_id)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &event.id,
                &event.aggregate_id,
                event.version,
                event.global_seq as i64,
                event.event_type(),
                event.timestamp.to_rfc3339(),
                payload,
                &event.causation_id,
                &event.correlation_id,
            ],
        ).map_err(|e| state_err_with("Failed to insert event", e))?;

        tx.commit()
            .map_err(|e| state_err_with("Failed to commit", e))?;

        debug!(
            event_id = %event.id,
            aggregate_id = %event.aggregate_id,
            version = event.version,
            global_seq = event.global_seq,
            "Event appended"
        );

        Ok(next_version)
    }

    fn append_batch(
        conn: &Connection,
        mut events: Vec<DomainEvent>,
        allocator: &SequenceAllocator,
    ) -> Result<Vec<u32>> {
        if events.is_empty() {
            return Ok(vec![]);
        }

        // Safety: unchecked_transaction is safe here because this connection is
        // exclusively owned by the single writer thread (no concurrent access).
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| state_err_with("Failed to start transaction", e))?;

        let mut versions = Vec::with_capacity(events.len());

        for event in &mut events {
            let current_version: Option<u32> = tx
                .query_row(
                    "SELECT MAX(version) FROM events WHERE aggregate_id = ?1",
                    params![&event.aggregate_id],
                    |row| row.get(0),
                )
                .ok();

            let next_version = current_version.map(|v| v + 1).unwrap_or(1);
            event.version = next_version;
            event.global_seq = allocator.next();

            let payload = serde_json::to_string(&event.payload)
                .map_err(|e| state_err_with("Failed to serialize payload", e))?;

            tx.execute(
                "INSERT INTO events (id, aggregate_id, version, global_seq, event_type, timestamp, payload, causation_id, correlation_id)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    &event.id,
                    &event.aggregate_id,
                    event.version,
                    event.global_seq as i64,
                    event.event_type(),
                    event.timestamp.to_rfc3339(),
                    payload,
                    &event.causation_id,
                    &event.correlation_id,
                ],
            ).map_err(|e| state_err_with("Failed to insert event", e))?;

            versions.push(next_version);
        }

        tx.commit()
            .map_err(|e| state_err_with("Failed to commit batch", e))?;

        debug!(count = events.len(), "Batch events appended");

        Ok(versions)
    }

    fn archive_through(conn: &Connection, global_seq: u64) -> Result<u64> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS archived_events (
                id TEXT PRIMARY KEY,
                aggregate_id TEXT NOT NULL,
                version INTEGER NOT NULL,
                global_seq INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                payload TEXT NOT NULL,
                causation_id TEXT,
                correlation_id TEXT
            )",
        )
        .map_err(|e| state_err_with("Failed to create archive table", e))?;

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| state_err_with("Failed to start archive transaction", e))?;

        tx.execute(
            "INSERT INTO archived_events
                SELECT id, aggregate_id, version, global_seq, event_type, timestamp, payload, causation_id, correlation_id
                FROM events WHERE global_seq <= ?1",
            params![global_seq as i64],
        )
        .map_err(|e| state_err_with("Failed to archive events", e))?;

        let deleted = tx
            .execute(
                "DELETE FROM events WHERE global_seq <= ?1",
                params![global_seq as i64],
            )
            .map_err(|e| state_err_with("Failed to delete archived events", e))?;

        tx.commit()
            .map_err(|e| state_err_with("Failed to commit archive", e))?;

        debug!(global_seq, deleted, "Events archived");
        Ok(deleted as u64)
    }

    fn compact(conn: &Connection) -> Result<()> {
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .map_err(|e| state_err_with("Failed to WAL checkpoint", e))?;

        conn.execute_batch("VACUUM")
            .map_err(|e| state_err_with("Failed to VACUUM", e))?;

        debug!("Database compacted");
        Ok(())
    }
}

impl Drop for EventWriter {
    fn drop(&mut self) {
        let _ = self.tx.send(WriteCommand::Shutdown);
        if let Some(handle) = self.handle.take()
            && let Err(e) = handle.join()
        {
            warn!("Writer thread panicked: {:?}", e);
        }
    }
}
