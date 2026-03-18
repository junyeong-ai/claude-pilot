//! Event replay system with snapshot support for fast recovery.

use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, info};

use super::event_store::EventStore;
use super::projections::Projection;
use super::snapshots::ProjectionSnapshotStore;
use crate::config::StateConfig;
use crate::error::Result;

pub struct EventReplayer {
    event_store: EventStore,
    snapshot_store: ProjectionSnapshotStore,
    snapshot_interval: u64,
    archive_after_snapshot: bool,
    vacuum_interval_events: u64,
    archived_since_vacuum: std::sync::atomic::AtomicU64,
}

impl EventReplayer {
    pub fn new(event_store: EventStore, snapshot_store: ProjectionSnapshotStore) -> Self {
        let config = StateConfig::default();
        Self {
            event_store,
            snapshot_store,
            snapshot_interval: config.snapshot_interval as u64,
            archive_after_snapshot: config.archive_after_snapshot,
            vacuum_interval_events: config.vacuum_interval_events,
            archived_since_vacuum: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn with_config(event_store: EventStore, snapshot_store: ProjectionSnapshotStore, config: &StateConfig) -> Self {
        Self {
            event_store,
            snapshot_store,
            snapshot_interval: config.snapshot_interval as u64,
            archive_after_snapshot: config.archive_after_snapshot,
            vacuum_interval_events: config.vacuum_interval_events,
            archived_since_vacuum: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn with_snapshot_interval(mut self, interval: u64) -> Self {
        self.snapshot_interval = interval;
        self
    }

    pub async fn rebuild_projection<P>(&self, aggregate_id: &str) -> Result<P>
    where
        P: Projection + Serialize + DeserializeOwned,
    {
        let (mut projection, from_seq) = self
            .snapshot_store
            .load_latest::<P>(aggregate_id)?
            .unwrap_or_else(|| (P::default(), 0));

        debug!(
            aggregate_id,
            from_seq,
            projection_type = std::any::type_name::<P>(),
            "Replaying events from sequence"
        );

        let events = self
            .event_store
            .query_aggregate_from_seq(aggregate_id, from_seq)
            .await?;

        let event_count = events.len();
        for event in events {
            projection.apply_idempotent(&event);
        }

        info!(
            aggregate_id,
            events_applied = event_count,
            final_seq = projection.last_applied_seq(),
            "Projection rebuilt"
        );

        if event_count as u64 >= self.snapshot_interval {
            if let Err(e) = self.snapshot_store.save(aggregate_id, &projection) {
                debug!(error = %e, "Failed to save snapshot after rebuild");
            } else {
                self.auto_archive_if_enabled(projection.last_applied_seq()).await;
            }
        }

        Ok(projection)
    }

    pub async fn replay_from_checkpoint<P>(
        &self,
        aggregate_id: &str,
        checkpoint_seq: u64,
    ) -> Result<P>
    where
        P: Projection + Serialize + DeserializeOwned,
    {
        let (mut projection, snapshot_seq) = self
            .snapshot_store
            .load_latest::<P>(aggregate_id)?
            .unwrap_or_else(|| (P::default(), 0));

        let from_seq = if checkpoint_seq > snapshot_seq {
            snapshot_seq
        } else {
            0
        };

        let events = self
            .event_store
            .query_aggregate_from_seq_until(aggregate_id, from_seq, checkpoint_seq)
            .await?;

        for event in events {
            projection.apply_idempotent(&event);
        }

        Ok(projection)
    }

    pub async fn rebuild_all_projections<P>(
        &self,
        aggregate_ids: &[&str],
    ) -> Result<Vec<(String, P)>>
    where
        P: Projection + Serialize + DeserializeOwned,
    {
        let mut results = Vec::with_capacity(aggregate_ids.len());

        for aggregate_id in aggregate_ids {
            let projection = self.rebuild_projection(aggregate_id).await?;
            results.push((aggregate_id.to_string(), projection));
        }

        Ok(results)
    }

    pub async fn maybe_snapshot<P>(&self, aggregate_id: &str, projection: &P) -> Result<bool>
    where
        P: Projection + Serialize + DeserializeOwned,
    {
        let current_seq = projection.last_applied_seq();

        let should_snapshot =
            if let Some((_, last_seq)) = self.snapshot_store.load_latest::<P>(aggregate_id)? {
                current_seq - last_seq >= self.snapshot_interval
            } else {
                current_seq >= self.snapshot_interval
            };

        if should_snapshot {
            self.snapshot_store.save(aggregate_id, projection)?;
            debug!(aggregate_id, seq = current_seq, "Created periodic snapshot");
            self.auto_archive_if_enabled(current_seq).await;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn auto_archive_if_enabled(&self, snapshot_seq: u64) {
        if !self.archive_after_snapshot {
            return;
        }

        match self.event_store.archive_events_through(snapshot_seq).await {
            Ok(archived) => {
                debug!(archived, snapshot_seq, "Auto-archived events after snapshot");
                let total = self
                    .archived_since_vacuum
                    .fetch_add(archived, std::sync::atomic::Ordering::Relaxed)
                    + archived;
                if total >= self.vacuum_interval_events {
                    self.archived_since_vacuum
                        .store(0, std::sync::atomic::Ordering::Relaxed);
                    if let Err(e) = self.event_store.compact().await {
                        debug!(error = %e, "Auto-compact after archive failed");
                    } else {
                        debug!("Auto-compacted database after archive threshold");
                    }
                }
            }
            Err(e) => {
                debug!(error = %e, "Auto-archive after snapshot failed");
            }
        }
    }

    pub fn event_store(&self) -> &EventStore {
        &self.event_store
    }

    pub fn snapshot_store(&self) -> &ProjectionSnapshotStore {
        &self.snapshot_store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{DomainEvent, EventPayload, ProgressProjection};
    use tempfile::TempDir;

    async fn setup() -> (TempDir, EventReplayer) {
        let dir = TempDir::new().unwrap();
        let event_db = dir.path().join("events.db");
        let snapshot_db = dir.path().join("snapshots.db");

        let event_store = EventStore::new(&event_db).unwrap();
        let snapshot_store = ProjectionSnapshotStore::new(&snapshot_db).unwrap();
        let replayer = EventReplayer::new(event_store, snapshot_store).with_snapshot_interval(5);

        (dir, replayer)
    }

    #[tokio::test]
    async fn test_rebuild_from_empty() {
        let (_dir, replayer) = setup().await;

        let projection: ProgressProjection = replayer.rebuild_projection("m-1").await.unwrap();

        assert_eq!(projection.version, 0);
        assert_eq!(projection.completed_tasks, 0);
    }

    #[tokio::test]
    async fn test_rebuild_with_events() {
        let (_dir, replayer) = setup().await;

        let event1 = DomainEvent::mission_created("m-1", "Test mission");
        replayer.event_store.append(event1).await.unwrap();

        let event2 = DomainEvent::new(
            "m-1",
            EventPayload::PlanGenerated {
                task_count: 5,
                phase_count: 2,
            },
        );
        replayer.event_store.append(event2).await.unwrap();

        let event3 = DomainEvent::task_completed("m-1", "t-1", vec!["f.rs".into()], 100);
        replayer.event_store.append(event3).await.unwrap();

        let projection: ProgressProjection = replayer.rebuild_projection("m-1").await.unwrap();

        assert_eq!(projection.total_tasks, 5);
        assert_eq!(projection.completed_tasks, 1);
    }

    #[tokio::test]
    async fn test_rebuild_from_snapshot() {
        let (_dir, replayer) = setup().await;

        // First, insert some events to get to global_seq 50
        // We'll insert 50 dummy events to advance the sequence
        for i in 0..50 {
            let event = DomainEvent::task_started("m-1", &format!("t-{i}"), &format!("task-{i}"));
            replayer.event_store.append(event).await.unwrap();
        }

        // Mark 10 as completed
        for i in 0..10 {
            let event = DomainEvent::task_completed("m-1", &format!("t-{i}"), vec![], 100);
            replayer.event_store.append(event).await.unwrap();
        }

        // Save snapshot at this point
        let pre_projection: ProgressProjection = replayer.rebuild_projection("m-1").await.unwrap();
        assert_eq!(pre_projection.completed_tasks, 10);

        replayer
            .snapshot_store
            .save("m-1", &pre_projection)
            .unwrap();
        let snapshot_seq = pre_projection.last_applied_seq;

        // Add one more completed task
        let event = DomainEvent::task_completed("m-1", "t-60", vec![], 100);
        replayer.event_store.append(event).await.unwrap();

        // Rebuild should use snapshot and apply the new event
        let projection: ProgressProjection = replayer.rebuild_projection("m-1").await.unwrap();

        assert_eq!(projection.completed_tasks, 11);
        assert!(projection.last_applied_seq > snapshot_seq);
    }

    #[tokio::test]
    async fn test_idempotent_replay() {
        let (_dir, replayer) = setup().await;

        let event1 = DomainEvent::mission_created("m-1", "Test");
        replayer.event_store.append(event1).await.unwrap();

        let proj1: ProgressProjection = replayer.rebuild_projection("m-1").await.unwrap();
        let proj2: ProgressProjection = replayer.rebuild_projection("m-1").await.unwrap();

        assert_eq!(proj1.version, proj2.version);
        assert_eq!(proj1.last_applied_seq(), proj2.last_applied_seq());
    }
}
