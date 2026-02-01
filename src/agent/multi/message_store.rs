//! Persistent message store with TTL support for polling-based conflict resolution.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::hierarchy::AgentId;
use crate::error::{PilotError, Result};

/// Default message TTL of 1 hour.
/// This is intentionally longer than file lease duration (5 minutes) because:
/// - Messages are for async negotiation between stateless agent invocations
/// - Agents may not be invoked for extended periods in polling patterns
/// - Failed message delivery can be retried within the TTL window
const DEFAULT_TTL_SECS: u64 = 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Pending,
    Delivered,
    Responded,
    Expired,
}

impl MessageStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Responded => "responded",
            Self::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "delivered" => Self::Delivered,
            "responded" => Self::Responded,
            "expired" => Self::Expired,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub to_agent: String,
    pub from_agent: String,
    pub action: String,
    pub payload: String,
    pub status: MessageStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub response: Option<String>,
}

pub struct MessageStore {
    conn: Arc<Mutex<Connection>>,
    db_path: PathBuf,
    ttl: Duration,
}

impl MessageStore {
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        Self::with_ttl(db_path, Duration::from_secs(DEFAULT_TTL_SECS))
    }

    pub fn with_ttl(db_path: impl AsRef<Path>, ttl: Duration) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                PilotError::State(format!("Failed to create message store dir: {}", e))
            })?;
        }

        let conn = Connection::open(&db_path)
            .map_err(|e| PilotError::State(format!("Failed to open message store: {}", e)))?;

        Self::init_schema(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
            ttl,
        })
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                to_agent TEXT NOT NULL,
                from_agent TEXT NOT NULL,
                action TEXT NOT NULL,
                payload TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                response TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_messages_to_agent
                ON messages(to_agent, status);
            CREATE INDEX IF NOT EXISTS idx_messages_expires
                ON messages(expires_at);
            ",
        )
        .map_err(|e| PilotError::State(format!("Failed to init message schema: {}", e)))?;

        Ok(())
    }

    pub fn enqueue(
        &self,
        to_agent: &AgentId,
        from_agent: &AgentId,
        action: &str,
        payload: &str,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let expires_at = now
            + chrono::Duration::from_std(self.ttl)
                .map_err(|e| PilotError::State(format!("Invalid TTL duration: {}", e)))?;

        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO messages (id, to_agent, from_agent, action, payload, status, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7)",
            params![
                &id,
                to_agent.as_str(),
                from_agent.as_str(),
                action,
                payload,
                now.to_rfc3339(),
                expires_at.to_rfc3339(),
            ],
        )
        .map_err(|e| PilotError::State(format!("Failed to enqueue message: {}", e)))?;

        debug!(
            message_id = %id,
            to = %to_agent,
            from = %from_agent,
            action,
            "Message enqueued"
        );

        Ok(id)
    }

    pub fn pending_for(&self, agent_id: &AgentId) -> Result<Vec<StoredMessage>> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();

        let mut stmt = conn
            .prepare(
                "SELECT id, to_agent, from_agent, action, payload, status, created_at, expires_at, response
                 FROM messages
                 WHERE to_agent = ?1 AND status = 'pending' AND expires_at > ?2
                 ORDER BY created_at ASC",
            )
            .map_err(|e| PilotError::State(format!("Failed to prepare query: {}", e)))?;

        let rows = stmt
            .query_map(params![agent_id.as_str(), &now], |row| {
                Ok(StoredMessage {
                    id: row.get(0)?,
                    to_agent: row.get(1)?,
                    from_agent: row.get(2)?,
                    action: row.get(3)?,
                    payload: row.get(4)?,
                    status: MessageStatus::parse(&row.get::<_, String>(5)?),
                    created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(6)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    expires_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    response: row.get(8)?,
                })
            })
            .map_err(|e| PilotError::State(format!("Failed to query messages: {}", e)))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| PilotError::State(format!("Failed to read messages: {}", e)))
    }

    pub fn mark_delivered(&self, message_id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE messages SET status = 'delivered' WHERE id = ?1",
            params![message_id],
        )
        .map_err(|e| PilotError::State(format!("Failed to mark delivered: {}", e)))?;

        Ok(())
    }

    pub fn respond(&self, message_id: &str, response: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE messages SET status = 'responded', response = ?2 WHERE id = ?1",
            params![message_id, response],
        )
        .map_err(|e| PilotError::State(format!("Failed to store response: {}", e)))?;

        debug!(message_id, "Message responded");
        Ok(())
    }

    pub fn get_response(&self, message_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let result: Option<String> = conn
            .query_row(
                "SELECT response FROM messages WHERE id = ?1 AND status = 'responded'",
                params![message_id],
                |row| row.get(0),
            )
            .ok();

        Ok(result)
    }

    pub fn cleanup_expired(&self) -> Result<usize> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();

        let deleted = conn
            .execute(
                "DELETE FROM messages WHERE expires_at < ?1 AND status = 'pending'",
                params![&now],
            )
            .map_err(|e| PilotError::State(format!("Failed to cleanup expired: {}", e)))?;

        if deleted > 0 {
            debug!(deleted, "Cleaned up expired messages");
        }

        Ok(deleted)
    }

    pub fn message_count(&self, agent_id: Option<&AgentId>) -> Result<usize> {
        let conn = self.conn.lock();

        let count: i64 = if let Some(agent) = agent_id {
            conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE to_agent = ?1 AND status = 'pending'",
                params![agent.as_str()],
                |row| row.get(0),
            )
        } else {
            conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )
        }
        .map_err(|e| PilotError::State(format!("Failed to count messages: {}", e)))?;

        Ok(count as usize)
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

impl Clone for MessageStore {
    fn clone(&self) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
            db_path: self.db_path.clone(),
            ttl: self.ttl,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, MessageStore) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test_messages.db");
        let store = MessageStore::with_ttl(&db_path, Duration::from_secs(60)).unwrap();
        (dir, store)
    }

    #[test]
    fn test_enqueue_and_pending() {
        let (_dir, store) = temp_store();

        let to = AgentId::new("coder-0");
        let from = AgentId::new("coordinator-0");

        let id = store
            .enqueue(
                &to,
                &from,
                "request_file_release",
                r#"{"file": "src/main.rs"}"#,
            )
            .unwrap();

        assert!(!id.is_empty());

        let pending = store.pending_for(&to).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].action, "request_file_release");
    }

    #[test]
    fn test_respond_and_get_response() {
        let (_dir, store) = temp_store();

        let to = AgentId::new("coder-0");
        let from = AgentId::new("coordinator-0");

        let id = store.enqueue(&to, &from, "test_action", "{}").unwrap();

        store.respond(&id, r#"{"accepted": true}"#).unwrap();

        let response = store.get_response(&id).unwrap();
        assert!(response.is_some());
        assert!(response.unwrap().contains("accepted"));
    }

    #[test]
    fn test_mark_delivered() {
        let (_dir, store) = temp_store();

        let to = AgentId::new("coder-0");
        let from = AgentId::new("coordinator-0");

        let id = store.enqueue(&to, &from, "test_action", "{}").unwrap();

        let pending_before = store.pending_for(&to).unwrap();
        assert_eq!(pending_before.len(), 1);

        store.mark_delivered(&id).unwrap();

        let pending_after = store.pending_for(&to).unwrap();
        assert_eq!(pending_after.len(), 0);
    }

    #[test]
    fn test_cleanup_expired() {
        let (_dir, store) = temp_store();

        let to = AgentId::new("coder-0");
        let from = AgentId::new("coordinator-0");

        store.enqueue(&to, &from, "test_action", "{}").unwrap();

        let count_before = store.message_count(None).unwrap();
        assert_eq!(count_before, 1);

        let deleted = store.cleanup_expired().unwrap();
        assert_eq!(deleted, 0);
    }

    #[test]
    fn test_message_status_as_str() {
        assert_eq!(MessageStatus::Pending.as_str(), "pending");
        assert_eq!(MessageStatus::Delivered.as_str(), "delivered");
        assert_eq!(MessageStatus::Responded.as_str(), "responded");
        assert_eq!(MessageStatus::Expired.as_str(), "expired");
    }
}
