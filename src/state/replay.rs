//! Event replay system with snapshot support for fast recovery.

use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, info};

use super::event_store::EventStore;
use super::projections::Projection;
use super::snapshots::ProjectionSnapshotStore;
use crate::error::Result;

const EVENTS_PER_SNAPSHOT: u64 = 1000;

pub struct EventReplayer {
    event_store: EventStore,
    snapshot_store: ProjectionSnapshotStore,
    snapshot_interval: u64,
}

impl EventReplayer {
    pub fn new(event_store: EventStore, snapshot_store: ProjectionSnapshotStore) -> Self {
        Self {
            event_store,
            snapshot_store,
            snapshot_interval: EVENTS_PER_SNAPSHOT,
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

        let events = self.event_store.query_from_global_seq(from_seq).await?;

        let events_for_aggregate: Vec<_> = events
            .into_iter()
            .filter(|e| e.aggregate_id.as_str() == aggregate_id)
            .collect();

        let event_count = events_for_aggregate.len();
        for event in events_for_aggregate {
            projection.apply_idempotent(&event);
        }

        info!(
            aggregate_id,
            events_applied = event_count,
            final_seq = projection.last_applied_seq(),
            "Projection rebuilt"
        );

        if event_count as u64 >= self.snapshot_interval
            && let Err(e) = self.snapshot_store.save(aggregate_id, &projection)
        {
            debug!(error = %e, "Failed to save snapshot after rebuild");
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

        let events = self.event_store.query_from_global_seq(from_seq).await?;

        let events_for_aggregate: Vec<_> = events
            .into_iter()
            .filter(|e| e.aggregate_id.as_str() == aggregate_id && e.global_seq <= checkpoint_seq)
            .collect();

        for event in events_for_aggregate {
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
            Ok(true)
        } else {
            Ok(false)
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
