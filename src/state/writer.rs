//! Dedicated writer thread for SQLite event store.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use rusqlite::{Connection, params};
use tracing::{debug, error, warn};

use super::{events::DomainEvent, state_err, state_err_with};
use crate::error::Result;

/// Allocates global sequences for events across potentially distributed nodes.
/// Format: [node_id:8bit][sequence:56bit] for future multi-node support.
/// For single-node (current): node_id = 0, full sequence range.
#[derive(Debug)]
pub(crate) struct SequenceAllocator {
    node_id: u8,
    next_seq: AtomicU64,
}

impl SequenceAllocator {
    #[allow(dead_code)]
    pub fn new(node_id: u8) -> Self {
        Self {
            node_id,
            next_seq: AtomicU64::new(1),
        }
    }

    pub fn with_initial_seq(node_id: u8, initial_seq: u64) -> Self {
        Self {
            node_id,
            next_seq: AtomicU64::new(initial_seq),
        }
    }

    pub fn next(&self) -> u64 {
        let local_seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        ((self.node_id as u64) << 56) | (local_seq & 0x00FF_FFFF_FFFF_FFFF)
    }

    #[allow(dead_code)]
    pub fn current(&self) -> u64 {
        let local_seq = self.next_seq.load(Ordering::SeqCst);
        ((self.node_id as u64) << 56) | (local_seq.saturating_sub(1) & 0x00FF_FFFF_FFFF_FFFF)
    }

    #[allow(dead_code)]
    pub fn node_id(&self) -> u8 {
        self.node_id
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
    Shutdown,
}

pub(super) struct EventWriter {
    tx: Sender<WriteCommand>,
    handle: Option<JoinHandle<()>>,
}

impl EventWriter {
    pub fn new(db_path: PathBuf) -> Result<Self> {
        Self::with_node_id(db_path, 0)
    }

    pub fn with_node_id(db_path: PathBuf, node_id: u8) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<WriteCommand>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();

        let handle = thread::Builder::new()
            .name("event-writer".into())
            .spawn(move || match Self::init_db(&db_path) {
                Ok(conn) => {
                    let max_seq = Self::get_max_global_seq(&conn).unwrap_or(0);
                    let allocator = SequenceAllocator::with_initial_seq(node_id, max_seq + 1);
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

    pub fn sender(&self) -> Sender<WriteCommand> {
        self.tx.clone()
    }

    fn get_max_global_seq(conn: &Connection) -> Result<u64> {
        conn.query_row("SELECT MAX(global_seq) FROM events", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .map(|opt| opt.unwrap_or(0) as u64)
        .map_err(|e| state_err_with("Failed to get max global_seq", e))
    }

    fn init_db(db_path: &PathBuf) -> Result<Connection> {
        let conn =
            Connection::open(db_path).map_err(|e| state_err_with("Failed to open database", e))?;
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
                    let _ = response.send(result);
                }
                WriteCommand::AppendBatch { events, response } => {
                    let result = Self::append_batch(conn, events, &allocator);
                    let _ = response.send(result);
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
