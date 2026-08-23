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

// ── Promise ───────────────────────────────────────────────────────────────────

// ── ProofBundle ───────────────────────────────────────────────────────────────

/// The same idea proven in multiple languages simultaneously.
///
/// When two or more `Promise`s agree on stack effect, you have a
/// cross-architecture proof — something true about the idea itself,
/// not just one encoding of it.
///
/// Comments extracted from the source (Forth `\ ...` and `( ... )` blocks)
/// travel with the bundle so recipients can read the intent, not just the code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofBundle {
    /// Stable ID shared by all translations of the same idea.
    pub id: Uuid,
    /// One promise per language — same idea, different encodings.
    pub proofs: Vec<Promise>,
    /// Comments extracted from the original source — the human explanation.
    pub comments: Vec<String>,
}

impl ProofBundle {
    /// Create a bundle from a single proven promise and its extracted comments.
    pub fn new(promise: Promise, comments: Vec<String>) -> Self {
        Self {
            id: promise.id,
            proofs: vec![promise],
            comments,
        }
    }

    /// Add another language's proof to this bundle.
    pub fn with_proof(mut self, promise: Promise) -> Self {
        self.proofs.push(promise);
        self
    }

    /// Returns true if all proofs with known effects agree on `( pops -- pushes )`.
    pub fn effects_agree(&self) -> bool {
        let effects: Vec<_> = self
            .proofs
            .iter()
            .filter_map(|p| p.effect.as_ref())
            .collect();
        if effects.len() < 2 {
            return true; // nothing to disagree about yet
        }
        let first = effects[0];
        effects
            .iter()
            .all(|e| e.pops == first.pops && e.pushes == first.pushes)
    }

    /// Primary promise — the first one added (the original source language).
    pub fn primary(&self) -> &Promise {
        &self.proofs[0]
    }
}

// ── Promise ───────────────────────────────────────────────────────────────────

/// Which language a `Promise` is written in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    Forth,
    Lisp,
    Natural,
}

/// A piece of code (or natural language) together with its proof.
///
/// A `Promise` is the unit of exchange between sessions.  Raw strings are
/// never passed directly — everything is wrapped in a `Promise` so the
/// receiver knows the language, has a stable identity, and (for Forth) knows
/// the stack effect before executing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Promise {
    /// Stable ID — same across a continuation chain.
    pub id: Uuid,
    /// The language this code is written in.
    pub lang: Lang,
    /// The code or text.
    pub code: String,
    /// Proven stack effect (Forth only; `None` means not yet verified).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect: Option<crate::coforth::StackEffect>,
    /// SHA-256 hex of `code` — integrity check.
    pub hash: String,
}

impl Promise {
    fn sha256(code: &str) -> String {
        use std::hash::Hasher;
        // Fast non-crypto hash is fine here (wire integrity, not security).
        // For cryptographic needs use the `sha256` Forth word instead.
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash_slice(code.as_bytes(), &mut h);
        format!("{:016x}", h.finish())
    }

    pub fn forth(code: impl Into<String>) -> Self {
        let code = code.into();
        Self {
            id: Uuid::new_v4(),
            hash: Self::sha256(&code),
            lang: Lang::Forth,
            code,
            effect: None,
        }
    }

    pub fn forth_proven(code: impl Into<String>, effect: crate::coforth::StackEffect) -> Self {
        let mut p = Self::forth(code);
        p.effect = Some(effect);
        p
    }

    pub fn lisp(code: impl Into<String>) -> Self {
        let code = code.into();
        Self {
            id: Uuid::new_v4(),
            hash: Self::sha256(&code),
            lang: Lang::Lisp,
            code,
            effect: None,
        }
    }

    pub fn natural(text: impl Into<String>) -> Self {
        let code = text.into();
        Self {
            id: Uuid::new_v4(),
            hash: Self::sha256(&code),
            lang: Lang::Natural,
            code,
            effect: None,
        }
    }
}

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
    DiffReject {
        diff_id: Uuid,
        reason: Option<String>,
    },

    /// One side asks the other to render a dialog and respond.
    Dialog { id: Uuid, spec: DialogSpec },

    /// Response to a Dialog — sent back by the receiving side.
    DialogAnswer { id: Uuid, answer: DialogAnswer },

    /// A proof bundle delivered to a named channel (e.g. "#all").
    /// Unlike `Chat`, this is display-only until the receiver explicitly
    /// calls `/exec [n]` — which executes the primary promise in their VM.
    /// The bundle carries all language translations and extracted comments,
    /// so the receiver can read the intent before executing.
    ChannelMessage {
        channel: String,
        sender: String,
        bundle: ProofBundle,
    },

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
    Confirm { prompt: String, default: bool },
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

    pub fn diff(
        label: impl Into<String>,
        patch: impl Into<String>,
        description: Option<String>,
    ) -> (Self, Uuid) {
        let id = Uuid::new_v4();
        (
            SessionEvent::Diff {
                id,
                label: label.into(),
                patch: patch.into(),
                description,
            },
            id,
        )
    }

    pub fn diff_edit(diff_id: Uuid, patch: impl Into<String>, description: Option<String>) -> Self {
        SessionEvent::DiffEdit {
            diff_id,
            patch: patch.into(),
            description,
        }
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
            kind: DialogKind::Confirm {
                prompt: prompt.into(),
                default,
            },
        }
    }

    pub fn text_input(title: impl Into<String>, prompt: impl Into<String>) -> Self {
        DialogSpec {
            title: title.into(),
            body: None,
            kind: DialogKind::TextInput {
                prompt: prompt.into(),
                default: None,
            },
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

    // ── ProofBundle tests ─────────────────────────────────────────────────────

    #[test]
    fn test_proof_bundle_single_proof_effects_agree() {
        let p = Promise::forth("2 3 +");
        let bundle = ProofBundle::new(p, vec![]);
        // One proof — nothing to disagree with.
        assert!(bundle.effects_agree());
    }

    #[test]
    fn test_proof_bundle_matching_effects_agree() {
        use crate::coforth::StackEffect;
        let p1 = Promise::forth_proven("2 3 +", StackEffect::new(2, 1));
        let p2 = Promise::forth_proven("dup *", StackEffect::new(2, 1));
        let bundle = ProofBundle::new(p1, vec![]).with_proof(p2);
        assert!(bundle.effects_agree());
    }

    #[test]
    fn test_proof_bundle_mismatched_effects_disagree() {
        use crate::coforth::StackEffect;
        let p1 = Promise::forth_proven("dup", StackEffect::new(1, 2));
        let p2 = Promise::forth_proven("drop", StackEffect::new(1, 0));
        let bundle = ProofBundle::new(p1, vec![]).with_proof(p2);
        assert!(!bundle.effects_agree());
    }

    #[test]
    fn test_proof_bundle_primary_is_first() {
        let p1 = Promise::forth("first");
        let p2 = Promise::lisp("(second)");
        let code = p1.code.clone();
        let bundle = ProofBundle::new(p1, vec![]).with_proof(p2);
        assert_eq!(bundle.primary().code, code);
    }

    #[test]
    fn test_proof_bundle_carries_comments() {
        let p = Promise::forth(": double \\ multiply by two\n  dup + ;");
        let comments = vec!["multiply by two".to_string()];
        let bundle = ProofBundle::new(p, comments.clone());
        assert_eq!(bundle.comments, comments);
    }

    #[test]
    fn test_proof_bundle_channel_message_roundtrip() {
        let p = Promise::natural("hello world");
        let bundle = ProofBundle::new(p, vec![]);
        let ev = SessionEvent::ChannelMessage {
            channel: "#all".into(),
            sender: "alice".into(),
            bundle,
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, SessionEvent::ChannelMessage { channel, .. } if channel == "#all"));
    }
}
