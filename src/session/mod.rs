// Session — bidirectional event bus between two participants.
//
// Both the user loop and the AI loop hold one end of a SessionBus.
// Either side can throw a Chat message or a Dialog at the other at any time;
// neither blocks waiting for a reply.
//
// Transport is transparent:
//   SessionPair::local()  →  in-process mpsc channels
//   SessionPair::websocket_server(ws)  →  incoming WebSocket connection
//   SessionPair::websocket_client(url) →  outgoing WebSocket connection
//
// Wire encoding: newline-delimited JSON (one SessionEvent per line).

pub mod diff_store;
pub mod names;
pub mod transport;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

// ── Wire types ────────────────────────────────────────────────────────────────

/// An event exchanged between two session participants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// A plain chat message.
    Chat { text: String },

    /// A proposed diff — unified diff format, against a named file or buffer.
    /// Any peer can propose; others can accept, edit, or comment.
    Diff {
        /// Unique ID for this proposal (so edits and accepts can reference it).
        id: Uuid,
        /// Display name — file path, buffer name, or free label.
        label: String,
        /// Unified diff text (--- a/... +++ b/... @@ ... @@).
        patch: String,
        /// Optional prose description of what this diff does.
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },

    /// An edit to a previously proposed diff — replaces the patch.
    DiffEdit {
        /// The diff proposal being edited.
        diff_id: Uuid,
        /// New patch text.
        patch: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },

    /// Accept a proposed diff (apply it).
    DiffAccept { diff_id: Uuid },

    /// Reject a proposed diff.
    DiffReject { diff_id: Uuid, reason: Option<String> },

    /// One side asks the other to render a dialog and respond.
    Dialog { id: Uuid, spec: DialogSpec },

    /// Response to a Dialog — sent back by the receiving side.
    DialogAnswer { id: Uuid, answer: DialogAnswer },

    /// Signals that this side is closing the session.
    Close,
}

/// Serialisable description of a dialog that the remote side should render.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialogSpec {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub kind: DialogKind,
}

/// What kind of dialog to show.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DialogKind {
    /// Single-choice list.
    Select { options: Vec<String> },
    /// Multi-choice list.
    MultiSelect { options: Vec<String> },
    /// Free-text entry.
    TextInput {
        prompt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        default: Option<String>,
    },
    /// Yes / No.
    Confirm {
        prompt: String,
        default: bool,
    },
}

/// The answer the remote side sends back for a Dialog event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DialogAnswer {
    Selected(usize),
    MultiSelected(Vec<usize>),
    Text(String),
    Confirmed(bool),
    Cancelled,
}

// ── SessionBus ────────────────────────────────────────────────────────────────

/// One end of a session — send events out, receive events in.
pub struct SessionBus {
    pub tx: mpsc::Sender<SessionEvent>,
    pub rx: mpsc::Receiver<SessionEvent>,
}

impl SessionBus {
    /// Send an event to the other participant (non-blocking if buffer has space).
    pub async fn send(&self, event: SessionEvent) -> Result<()> {
        self.tx.send(event).await?;
        Ok(())
    }

    /// Receive the next event from the other participant.
    pub async fn recv(&mut self) -> Option<SessionEvent> {
        self.rx.recv().await
    }
}

// ── SessionPair ───────────────────────────────────────────────────────────────

/// A connected pair of buses — one for each participant.
pub struct SessionPair {
    pub a: SessionBus,
    pub b: SessionBus,
}

impl SessionPair {
    /// Create a fully in-process session (no network).
    pub fn local() -> Self {
        let (a_tx, b_rx) = mpsc::channel::<SessionEvent>(64);
        let (b_tx, a_rx) = mpsc::channel::<SessionEvent>(64);
        SessionPair {
            a: SessionBus { tx: a_tx, rx: a_rx },
            b: SessionBus { tx: b_tx, rx: b_rx },
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

impl SessionEvent {
    pub fn chat(text: impl Into<String>) -> Self {
        SessionEvent::Chat { text: text.into() }
    }

    pub fn diff(label: impl Into<String>, patch: impl Into<String>, description: Option<String>) -> (Self, Uuid) {
        let id = Uuid::new_v4();
        (SessionEvent::Diff { id, label: label.into(), patch: patch.into(), description }, id)
    }

    pub fn diff_edit(diff_id: Uuid, patch: impl Into<String>, description: Option<String>) -> Self {
        SessionEvent::DiffEdit { diff_id, patch: patch.into(), description }
    }

    pub fn diff_accept(diff_id: Uuid) -> Self {
        SessionEvent::DiffAccept { diff_id }
    }

    pub fn diff_reject(diff_id: Uuid, reason: Option<String>) -> Self {
        SessionEvent::DiffReject { diff_id, reason }
    }

    pub fn dialog(spec: DialogSpec) -> (Self, Uuid) {
        let id = Uuid::new_v4();
        (SessionEvent::Dialog { id, spec }, id)
    }

    pub fn answer(id: Uuid, answer: DialogAnswer) -> Self {
        SessionEvent::DialogAnswer { id, answer }
    }
}

impl DialogSpec {
    pub fn select(title: impl Into<String>, options: Vec<String>) -> Self {
        DialogSpec {
            title: title.into(),
            body: None,
            kind: DialogKind::Select { options },
        }
    }

    pub fn confirm(title: impl Into<String>, prompt: impl Into<String>, default: bool) -> Self {
        DialogSpec {
            title: title.into(),
            body: None,
            kind: DialogKind::Confirm { prompt: prompt.into(), default },
        }
    }

    pub fn text_input(title: impl Into<String>, prompt: impl Into<String>) -> Self {
        DialogSpec {
            title: title.into(),
            body: None,
            kind: DialogKind::TextInput { prompt: prompt.into(), default: None },
        }
    }

    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_local_pair_chat() {
        let SessionPair { mut a, mut b } = SessionPair::local();

        a.send(SessionEvent::chat("hello from a")).await.unwrap();
        let ev = b.recv().await.unwrap();
        assert!(matches!(ev, SessionEvent::Chat { text } if text == "hello from a"));
    }

    #[tokio::test]
    async fn test_local_pair_dialog_roundtrip() {
        let SessionPair { mut a, mut b } = SessionPair::local();

        // a throws a dialog at b
        let spec = DialogSpec::select("Pick one", vec!["Yes".into(), "No".into()]);
        let (event, dialog_id) = SessionEvent::dialog(spec);
        a.send(event).await.unwrap();

        // b receives it
        let ev = b.recv().await.unwrap();
        let received_id = match &ev {
            SessionEvent::Dialog { id, .. } => *id,
            other => panic!("expected Dialog, got {:?}", other),
        };
        assert_eq!(received_id, dialog_id);

        // b sends the answer back
        b.send(SessionEvent::answer(received_id, DialogAnswer::Selected(0)))
            .await
            .unwrap();

        // a receives the answer
        let reply = a.recv().await.unwrap();
        assert!(matches!(
            reply,
            SessionEvent::DialogAnswer { id, answer: DialogAnswer::Selected(0) }
            if id == dialog_id
        ));
    }

    #[tokio::test]
    async fn test_both_sides_send_concurrently() {
        let SessionPair { mut a, mut b } = SessionPair::local();

        // Both sides fire without waiting for the other
        a.send(SessionEvent::chat("from a")).await.unwrap();
        b.send(SessionEvent::chat("from b")).await.unwrap();

        let ev_b = b.recv().await.unwrap();
        let ev_a = a.recv().await.unwrap();

        assert!(matches!(ev_b, SessionEvent::Chat { text } if text == "from a"));
        assert!(matches!(ev_a, SessionEvent::Chat { text } if text == "from b"));
    }

    #[tokio::test]
    async fn test_close_event_roundtrips() {
        let SessionPair { mut a, mut b } = SessionPair::local();
        a.send(SessionEvent::Close).await.unwrap();
        assert!(matches!(b.recv().await.unwrap(), SessionEvent::Close));
    }

    #[test]
    fn test_session_event_json_roundtrip() {
        let ev = SessionEvent::chat("hello");
        let json = serde_json::to_string(&ev).unwrap();
        let back: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, SessionEvent::Chat { text } if text == "hello"));
    }

    #[test]
    fn test_dialog_spec_json_roundtrip() {
        let spec = DialogSpec::select("Choose", vec!["A".into(), "B".into()]);
        let (ev, id) = SessionEvent::dialog(spec);
        let json = serde_json::to_string(&ev).unwrap();
        let back: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, SessionEvent::Dialog { id: rid, .. } if rid == id));
    }
}
