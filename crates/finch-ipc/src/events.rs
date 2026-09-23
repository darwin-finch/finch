//! Crate-local event-bus test harness with continuation support.
//!
//! # Model
//!
//! Events are queued as [`QueuedEvent`] values.  Each event has a `name` (the
//! dispatch key) and an opaque `payload` (JSON for now; Cap'n Proto bytes once
//! callers are wired to the schema).
//!
//! A handler is an async closure registered under a name.  It receives the
//! event and returns `Option<QueuedEvent>`:
//!
//! - `None`  — done; no follow-up.
//! - `Some(e)` — continuation; `e` is re-queued with the same `id` so the
//!   caller can correlate the chain.
//!
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Event type
// ---------------------------------------------------------------------------

/// A single event on the bus.
#[derive(Debug, Clone)]
struct QueuedEvent {
    /// Dispatch key — matches a registered handler name.
    name: String,
    /// Stable ID across the continuation chain.
    id: String,
    /// Payload — free-form JSON until callers are wired to the capnp schema.
    payload: serde_json::Value,
}

impl QueuedEvent {
    fn new(name: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            id: Uuid::new_v4().to_string(),
            payload,
        }
    }

    /// Produce a continuation event: same `id`, new `name` and `payload`.
    fn continue_as(&self, name: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            id: self.id.clone(),
            payload,
        }
    }
}

// ---------------------------------------------------------------------------
// Handler type
// ---------------------------------------------------------------------------

type HandlerFuture = Pin<Box<dyn Future<Output = Option<QueuedEvent>> + Send>>;
type HandlerFn = Arc<dyn Fn(QueuedEvent) -> HandlerFuture + Send + Sync>;

// ---------------------------------------------------------------------------
// EventBus
// ---------------------------------------------------------------------------

/// Named async event bus with continuation support.
struct EventBus {
    tx: mpsc::UnboundedSender<QueuedEvent>,
    rx: mpsc::UnboundedReceiver<QueuedEvent>,
    handlers: HashMap<String, HandlerFn>,
}

impl EventBus {
    fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx,
            handlers: HashMap::new(),
        }
    }

    /// Register an async handler for events with the given name.
    fn register<F, Fut>(&mut self, name: impl Into<String>, handler: F)
    where
        F: Fn(QueuedEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<QueuedEvent>> + Send + 'static,
    {
        self.handlers
            .insert(name.into(), Arc::new(move |ev| Box::pin(handler(ev))));
    }

    /// Enqueue an event.
    fn send(&self, event: QueuedEvent) {
        let _ = self.tx.send(event);
    }

    /// Process all currently queued events (non-blocking once the queue is
    /// empty). Awaits each handler before returning so continuations
    /// are fully resolved.  Useful in tests.
    async fn flush(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    if let Some(handler) = self.handlers.get(&event.name) {
                        let handler = Arc::clone(handler);
                        let tx = self.tx.clone();
                        if let Some(continuation) = handler(event).await {
                            let _ = tx.send(continuation);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn test_handler_fires_for_registered_name() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);

        let mut bus = EventBus::new();
        bus.register("ping", move |ev| {
            let c = Arc::clone(&c);
            Box::pin(async move {
                assert_eq!(ev.payload, serde_json::Value::Null, "ping payload");
                c.fetch_add(1, Ordering::SeqCst);
                None
            })
        });

        bus.send(QueuedEvent::new("ping", serde_json::Value::Null));
        bus.flush().await;

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_unknown_event_is_silently_ignored() {
        let mut bus = EventBus::new();
        bus.send(QueuedEvent::new("no.handler", serde_json::Value::Null));
        bus.flush().await; // no panic
    }

    #[tokio::test]
    async fn test_continuation_is_requeued_with_same_id() {
        let ids = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));

        let ids1 = Arc::clone(&ids);
        let ids2 = Arc::clone(&ids);

        let mut bus = EventBus::new();

        bus.register("step.one", move |ev| {
            let ids1 = Arc::clone(&ids1);
            Box::pin(async move {
                ids1.lock().await.push(ev.id.clone());
                Some(ev.continue_as("step.two", serde_json::Value::Null))
            })
        });

        bus.register("step.two", move |ev| {
            let ids2 = Arc::clone(&ids2);
            Box::pin(async move {
                ids2.lock().await.push(ev.id.clone());
                None
            })
        });

        bus.send(QueuedEvent::new("step.one", serde_json::Value::Null));
        bus.flush().await; // processes step.one, which re-queues step.two
        bus.flush().await; // processes step.two

        let seen = ids.lock().await;
        assert_eq!(seen.len(), 2, "both steps should fire");
        assert_eq!(seen[0], seen[1], "continuation preserves event id");
    }
}
