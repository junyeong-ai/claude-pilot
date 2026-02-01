//! Projection snapshots with checksum validation and corruption recovery.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tracing::warn;

use super::projections::Projection;
use super::state_err_with;
use crate::error::Result;

const MAX_SNAPSHOTS_PER_AGGREGATE: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableSnapshot<T> {
    pub id: String,
    pub aggregate_id: String,
    pub global_seq: u64,
    pub projection_type: String,
    pub checksum: u32,
    pub created_at: DateTime<Utc>,
    pub data: T,
}

pub struct ProjectionSnapshotStore {
    conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
}

impl ProjectionSnapshotStore {
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| state_err_with("Failed to create snapshot db directory", e))?;
        }

        let conn = Connection::open(&db_path)
            .map_err(|e| state_err_with("Failed to open snapshot database", e))?;

        Self::init_schema(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
        })
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS projection_snapshots (
                id TEXT PRIMARY KEY,
                aggregate_id TEXT NOT NULL,
                global_seq INTEGER NOT NULL,
                projection_type TEXT NOT NULL,
                data BLOB NOT NULL,
                checksum INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_snapshot_lookup
                ON projection_snapshots(aggregate_id, projection_type, global_seq DESC);
            ",
        )
        .map_err(|e| state_err_with("Failed to init snapshot schema", e))?;

        Ok(())
    }

    fn compute_checksum(data: &[u8]) -> u32 {
        crc32fast::hash(data)
    }

    pub fn save<P>(&self, aggregate_id: &str, projection: &P) -> Result<String>
    where
        P: Projection + Serialize,
    {
        let projection_type = std::any::type_name::<P>();
        let data = bincode::serialize(projection)
            .map_err(|e| state_err_with("Failed to serialize projection", e))?;
        let checksum = Self::compute_checksum(&data);
        let id = uuid::Uuid::new_v4().to_string();

        let conn = self.conn.lock();

        conn.execute(
            "INSERT INTO projection_snapshots (id, aggregate_id, global_seq, projection_type, data, checksum, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &id,
                aggregate_id,
                projection.last_applied_seq() as i64,
                projection_type,
                &data,
                checksum as i64,
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| state_err_with("Failed to save snapshot", e))?;

        self.cleanup_old_snapshots(&conn, aggregate_id, projection_type)?;

        Ok(id)
    }

    fn cleanup_old_snapshots(
        &self,
        conn: &Connection,
        aggregate_id: &str,
        projection_type: &str,
    ) -> Result<()> {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM projection_snapshots WHERE aggregate_id = ?1 AND projection_type = ?2",
                params![aggregate_id, projection_type],
                |row| row.get(0),
            )
            .map_err(|e| state_err_with("Failed to count snapshots", e))?;

        if count as usize > MAX_SNAPSHOTS_PER_AGGREGATE {
            let to_delete = count as usize - MAX_SNAPSHOTS_PER_AGGREGATE;
            conn.execute(
                "DELETE FROM projection_snapshots WHERE id IN (
                    SELECT id FROM projection_snapshots
                    WHERE aggregate_id = ?1 AND projection_type = ?2
                    ORDER BY global_seq ASC
                    LIMIT ?3
                )",
                params![aggregate_id, projection_type, to_delete as i64],
            )
            .map_err(|e| state_err_with("Failed to cleanup old snapshots", e))?;
        }

        Ok(())
    }

    pub fn load_latest<P>(&self, aggregate_id: &str) -> Result<Option<(P, u64)>>
    where
        P: Projection + DeserializeOwned,
    {
        let projection_type = std::any::type_name::<P>();
        let conn = self.conn.lock();

        let snapshots = self.get_snapshots_ordered(&conn, aggregate_id, projection_type)?;

        for (data, checksum, global_seq) in snapshots {
            if Self::compute_checksum(&data) == checksum {
                match bincode::deserialize::<P>(&data) {
                    Ok(projection) => return Ok(Some((projection, global_seq))),
                    Err(e) => {
                        warn!(
                            aggregate_id,
                            global_seq, "Snapshot deserialization failed: {}", e
                        );
                    }
                }
            } else {
                warn!(
                    aggregate_id,
                    global_seq,
                    expected_checksum = checksum,
                    "Snapshot corrupted, trying previous"
                );
            }
        }

        warn!(
            aggregate_id,
            "All snapshots corrupted or missing, full replay required"
        );
        Ok(None)
    }

    fn get_snapshots_ordered(
        &self,
        conn: &Connection,
        aggregate_id: &str,
        projection_type: &str,
    ) -> Result<Vec<(Vec<u8>, u32, u64)>> {
        let mut stmt = conn
            .prepare(
                "SELECT data, checksum, global_seq FROM projection_snapshots
                 WHERE aggregate_id = ?1 AND projection_type = ?2
                 ORDER BY global_seq DESC
                 LIMIT ?3",
            )
            .map_err(|e| state_err_with("Failed to prepare snapshot query", e))?;

        let rows = stmt
            .query_map(
                params![
                    aggregate_id,
                    projection_type,
                    MAX_SNAPSHOTS_PER_AGGREGATE as i64
                ],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, i64>(1)? as u32,
                        row.get::<_, i64>(2)? as u64,
                    ))
                },
            )
            .map_err(|e| state_err_with("Failed to query snapshots", e))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| state_err_with("Failed to read snapshot rows", e))
    }

    pub fn delete_all(&self, aggregate_id: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let deleted = conn
            .execute(
                "DELETE FROM projection_snapshots WHERE aggregate_id = ?1",
                params![aggregate_id],
            )
            .map_err(|e| state_err_with("Failed to delete snapshots", e))?;

        Ok(deleted)
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

impl Clone for ProjectionSnapshotStore {
    fn clone(&self) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
            db_path: self.db_path.clone(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::state::projections::ProgressProjection;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, ProjectionSnapshotStore) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test_snapshots.db");
        let store = ProjectionSnapshotStore::new(&db_path).unwrap();
        (dir, store)
    }

    #[test]
    fn test_save_and_load() {
        let (_dir, store) = temp_store();

        let mut projection = ProgressProjection::default();
        projection.mission_id = Some("test-mission".into());
        projection.completed_tasks = 5;
        projection.last_applied_seq = 100;

        let id = store.save("test-mission", &projection).unwrap();
        assert!(!id.is_empty());

        let (loaded, seq): (ProgressProjection, u64) = store
            .load_latest("test-mission")
            .unwrap()
            .expect("should find snapshot");

        assert_eq!(loaded.mission_id, Some("test-mission".into()));
        assert_eq!(loaded.completed_tasks, 5);
        assert_eq!(seq, 100);
    }

    #[test]
    fn test_corruption_fallback() {
        let (_dir, store) = temp_store();

        let mut proj1 = ProgressProjection::default();
        proj1.last_applied_seq = 50;
        proj1.completed_tasks = 3;
        store.save("m-1", &proj1).unwrap();

        let mut proj2 = ProgressProjection::default();
        proj2.last_applied_seq = 100;
        proj2.completed_tasks = 5;
        store.save("m-1", &proj2).unwrap();

        {
            let conn = store.conn.lock();
            // Corrupt the latest snapshot (seq=100) by updating its checksum
            conn.execute(
                "UPDATE projection_snapshots SET checksum = 12345
                 WHERE aggregate_id = 'm-1' AND global_seq = 100",
                [],
            )
            .unwrap();
        }

        let (loaded, seq): (ProgressProjection, u64) = store
            .load_latest("m-1")
            .unwrap()
            .expect("should fall back to previous snapshot");

        assert_eq!(seq, 50);
        assert_eq!(loaded.completed_tasks, 3);
    }

    #[test]
    fn test_cleanup_old_snapshots() {
        let (_dir, store) = temp_store();

        for i in 1..=5 {
            let mut proj = ProgressProjection::default();
            proj.last_applied_seq = i as u64 * 100;
            store.save("m-1", &proj).unwrap();
        }

        let conn = store.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM projection_snapshots WHERE aggregate_id = 'm-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(count, MAX_SNAPSHOTS_PER_AGGREGATE as i64);
    }

    #[test]
    fn test_missing_aggregate() {
        let (_dir, store) = temp_store();

        let result: Option<(ProgressProjection, u64)> = store.load_latest("nonexistent").unwrap();

        assert!(result.is_none());
    }
}
