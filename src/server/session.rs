// Session management for concurrent HTTP clients

use crate::cli::ConversationHistory;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use uuid::Uuid;

/// Per-session state
#[derive(Debug, Clone)]
pub struct SessionState {
    /// Unique session identifier
    pub id: String,
    /// Conversation history for this session
    pub conversation: ConversationHistory,
    /// Last activity timestamp
    pub last_activity: DateTime<Utc>,
    /// Session creation time
    pub created_at: DateTime<Utc>,
}

impl SessionState {
    /// Create a new session
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            conversation: ConversationHistory::new(),
            last_activity: Utc::now(),
            created_at: Utc::now(),
        }
    }

    /// Update last activity timestamp
    pub fn touch(&mut self) {
        self.last_activity = Utc::now();
    }

    /// Check if session has expired
    pub fn is_expired(&self, timeout_minutes: u64) -> bool {
        let now = Utc::now();
        let elapsed = now.signed_duration_since(self.last_activity);
        elapsed.num_minutes() >= timeout_minutes as i64
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

/// Concurrent session manager using DashMap
pub struct SessionManager {
    /// Active sessions (thread-safe concurrent HashMap)
    sessions: Arc<DashMap<String, SessionState>>,
    /// Maximum number of concurrent sessions
    max_sessions: usize,
    /// Session timeout in minutes
    timeout_minutes: u64,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new(max_sessions: usize, timeout_minutes: u64) -> Self {
        let manager = Self {
            sessions: Arc::new(DashMap::new()),
            max_sessions,
            timeout_minutes,
        };

        // Start background cleanup task
        manager.start_cleanup_task();

        manager
    }

    /// Get or create a session
    pub fn get_or_create(&self, session_id: Option<&str>) -> anyhow::Result<SessionState> {
        // If session_id provided, try to retrieve existing session
        if let Some(id) = session_id {
            if let Some(mut session) = self.sessions.get_mut(id) {
                session.touch();
                return Ok(session.clone());
            }
            // Session not found, will create new one below
        }

        // Check session limit
        if self.sessions.len() >= self.max_sessions {
            anyhow::bail!(
                "Maximum session limit reached ({}/{})",
                self.sessions.len(),
                self.max_sessions
            );
        }

        // Create new session
        let session = SessionState::new();
        let id = session.id.clone();
        self.sessions.insert(id.clone(), session.clone());

        tracing::info!(session_id = %id, "Created new session");
        Ok(session)
    }

    /// Update session state
    pub fn update(&self, session_id: &str, session: SessionState) -> anyhow::Result<()> {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            *entry = session;
            Ok(())
        } else {
            anyhow::bail!("Session not found: {}", session_id)
        }
    }

    /// Delete a session
    pub fn delete(&self, session_id: &str) -> bool {
        self.sessions.remove(session_id).is_some()
    }

    /// Get active session count
    pub fn active_count(&self) -> usize {
        self.sessions.len()
    }

    /// Cleanup expired sessions
    fn cleanup_expired(&self) {
        let mut removed_count = 0;
        let expired_sessions: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| entry.value().is_expired(self.timeout_minutes))
            .map(|entry| entry.key().clone())
            .collect();

        for session_id in expired_sessions {
            if self.sessions.remove(&session_id).is_some() {
                removed_count += 1;
                tracing::debug!(session_id = %session_id, "Removed expired session");
            }
        }

        if removed_count > 0 {
            tracing::info!(
                removed = removed_count,
                active = self.sessions.len(),
                "Cleaned up expired sessions"
            );
        }
    }

    /// Start background cleanup task
    fn start_cleanup_task(&self) {
        let sessions = Arc::clone(&self.sessions);
        let timeout_minutes = self.timeout_minutes;

        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(60)); // Check every minute

            loop {
                interval.tick().await;

                // Cleanup expired sessions
                let mut removed_count = 0;
                let expired_sessions: Vec<String> = sessions
                    .iter()
                    .filter(|entry| entry.value().is_expired(timeout_minutes))
                    .map(|entry| entry.key().clone())
                    .collect();

                for session_id in expired_sessions {
                    if sessions.remove(&session_id).is_some() {
                        removed_count += 1;
                        tracing::debug!(session_id = %session_id, "Removed expired session");
                    }
                }

                if removed_count > 0 {
                    tracing::info!(
                        removed = removed_count,
                        active = sessions.len(),
                        "Cleaned up expired sessions"
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_session_creation() {
        let manager = SessionManager::new(10, 30);

        // Create first session
        let session1 = manager.get_or_create(None).unwrap();
        assert_eq!(manager.active_count(), 1);

        // Create second session
        let session2 = manager.get_or_create(None).unwrap();
        assert_eq!(manager.active_count(), 2);

        // Different session IDs
        assert_ne!(session1.id, session2.id);
    }

    #[tokio::test]
    async fn test_session_retrieval() {
        let manager = SessionManager::new(10, 30);

        // Create session
        let session1 = manager.get_or_create(None).unwrap();
        let session_id = session1.id.clone();

        // Retrieve same session
        let session2 = manager.get_or_create(Some(&session_id)).unwrap();
        assert_eq!(session1.id, session2.id);
        assert_eq!(manager.active_count(), 1); // Still only 1 session
    }

    #[tokio::test]
    async fn test_session_limit() {
        let manager = SessionManager::new(2, 30);

        // Create 2 sessions (at limit)
        manager.get_or_create(None).unwrap();
        manager.get_or_create(None).unwrap();

        // Third session should fail
        let result = manager.get_or_create(None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Maximum session limit"));
    }

    #[tokio::test]
    async fn test_session_deletion() {
        let manager = SessionManager::new(10, 30);

        let session = manager.get_or_create(None).unwrap();
        let session_id = session.id.clone();

        assert_eq!(manager.active_count(), 1);

        // Delete session
        assert!(manager.delete(&session_id));
        assert_eq!(manager.active_count(), 0);

        // Delete non-existent session
        assert!(!manager.delete(&session_id));
    }
}
