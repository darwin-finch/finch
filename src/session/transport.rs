// Session transport — WebSocket bridge.
//
// `serve(ws)`  — called from axum's WS upgrade handler; returns a SessionBus
//                whose other end is the remote peer over the wire.
//
// `connect(url)` — dials a remote finch daemon's /v1/session endpoint; returns
//                  a SessionBus whose other end is the remote peer.
//
// Wire format: newline-delimited JSON.  Each WS text frame carries one
// SessionEvent serialised as JSON.  Binary frames and pings are ignored.

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;

use super::{SessionBus, SessionEvent};

// ── Server side (axum WebSocket upgrade) ─────────────────────────────────────

/// Bridge an axum `WebSocket` to a `SessionBus`.
///
/// Spawns two background tasks (read loop + write loop) and returns the local
/// bus end immediately.  The caller owns the bus and reads/writes to the remote
/// peer through it.
pub fn serve(ws: WebSocket) -> SessionBus {
    let (local_tx, local_rx) = mpsc::channel::<SessionEvent>(64);
    let (remote_tx, remote_rx) = mpsc::channel::<SessionEvent>(64);

    let (mut ws_tx, mut ws_rx) = ws.split();

    // WS → local_tx: forward incoming frames from the peer into the bus
    let read_tx = local_tx.clone();
    tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let Message::Text(text) = msg {
                match serde_json::from_str::<SessionEvent>(&text) {
                    Ok(event) => {
                        if matches!(event, SessionEvent::Close) {
                            let _ = read_tx.send(SessionEvent::Close).await;
                            break;
                        }
                        if read_tx.send(event).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => tracing::warn!("session: bad JSON from peer: {e}"),
                }
            }
        }
    });

    // remote_rx → WS: forward outgoing events from the bus to the peer
    tokio::spawn(async move {
        let mut rx = remote_rx;
        while let Some(event) = rx.recv().await {
            let is_close = matches!(event, SessionEvent::Close);
            match serde_json::to_string(&event) {
                Ok(json) => {
                    if ws_tx.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                Err(e) => tracing::error!("session: serialise error: {e}"),
            }
            if is_close {
                break;
            }
        }
    });

    SessionBus {
        tx: remote_tx,
        rx: local_rx,
    }
}

// ── Client side (tokio-tungstenite dial) ─────────────────────────────────────

/// Connect to a remote finch daemon's session endpoint and return a `SessionBus`.
///
/// `url` should be a `ws://` or `wss://` URL, e.g.
/// `ws://192.168.1.42:11435/v1/session`.
pub async fn connect(url: &str) -> Result<SessionBus> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(url)
        .await
        .with_context(|| format!("WebSocket connect failed: {url}"))?;

    let (local_tx, local_rx) = mpsc::channel::<SessionEvent>(64);
    let (remote_tx, remote_rx) = mpsc::channel::<SessionEvent>(64);

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // WS → local_tx
    let read_tx = local_tx.clone();
    tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let TungsteniteMessage::Text(text) = msg {
                match serde_json::from_str::<SessionEvent>(&text) {
                    Ok(event) => {
                        let is_close = matches!(event, SessionEvent::Close);
                        let _ = read_tx.send(event).await;
                        if is_close {
                            break;
                        }
                    }
                    Err(e) => tracing::warn!("session client: bad JSON: {e}"),
                }
            }
        }
    });

    // remote_rx → WS
    tokio::spawn(async move {
        let mut rx = remote_rx;
        while let Some(event) = rx.recv().await {
            let is_close = matches!(event, SessionEvent::Close);
            match serde_json::to_string(&event) {
                Ok(json) => {
                    if ws_tx
                        .send(TungsteniteMessage::Text(json.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(e) => tracing::error!("session client: serialise error: {e}"),
            }
            if is_close {
                break;
            }
        }
    });

    Ok(SessionBus {
        tx: remote_tx,
        rx: local_rx,
    })
}
