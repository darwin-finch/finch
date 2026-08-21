//! Client for attaching a Finch TUI to a named brain on another daemon.

use anyhow::{Context, Result};
use futures::StreamExt;
use reqwest::Client;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::shared::{BrainEventKind, BrainSnapshot, BrainWireMessage};

pub const DEFAULT_BRAIN_PORT: u16 = 11435;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBrainTarget {
    pub brain: String,
    pub machine: String,
    pub address: String,
}

impl RemoteBrainTarget {
    pub fn parse(value: &str) -> Result<Self> {
        let (brain, host) = value
            .trim()
            .split_once('@')
            .context("brain target must be NAME@MACHINE[:PORT]")?;
        super::shared::SharedBrainStore::validate_name(brain)?;
        let host = host.trim();
        if host.is_empty() || host.contains('/') || host.contains(char::is_whitespace) {
            anyhow::bail!("brain machine must be a hostname or host:port");
        }
        let address = if has_explicit_port(host) {
            host.to_string()
        } else {
            format!("{host}:{DEFAULT_BRAIN_PORT}")
        };
        let machine = host
            .rsplit_once(':')
            .filter(|(_, port)| port.parse::<u16>().is_ok())
            .map(|(host, _)| host)
            .unwrap_or(host)
            .to_string();
        Ok(Self {
            brain: brain.to_string(),
            machine,
            address,
        })
    }

    pub fn display_name(&self) -> String {
        format!("{}@{}", self.brain, self.machine)
    }

    fn http_url(&self) -> String {
        format!("http://{}/v1/brains/named/{}", self.address, self.brain)
    }

    fn ws_url(&self) -> String {
        format!("ws://{}/v1/brains/named/{}/ws", self.address, self.brain)
    }
}

fn has_explicit_port(host: &str) -> bool {
    host.rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
}

#[derive(Clone)]
pub struct RemoteBrainClient {
    pub target: RemoteBrainTarget,
    password: String,
    http: Client,
}

impl RemoteBrainClient {
    pub fn new(target: RemoteBrainTarget, password: impl Into<String>) -> Result<Self> {
        Ok(Self {
            target,
            password: password.into(),
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(180))
                .build()?,
        })
    }

    pub async fn snapshot(&self) -> Result<BrainSnapshot> {
        self.http
            .get(self.target.http_url())
            .bearer_auth(&self.password)
            .send()
            .await
            .context("could not reach brain host")?
            .error_for_status()
            .context("brain attach rejected")?
            .json()
            .await
            .context("invalid brain snapshot")
    }

    pub async fn push(&self, sender: &str, kind: BrainEventKind) -> Result<()> {
        #[derive(Serialize)]
        struct Push<'a> {
            sender: &'a str,
            #[serde(flatten)]
            kind: BrainEventKind,
        }

        self.http
            .post(self.target.http_url())
            .bearer_auth(&self.password)
            .json(&Push { sender, kind })
            .send()
            .await
            .context("could not reach brain host")?
            .error_for_status()
            .context("brain push rejected")?;
        Ok(())
    }

    /// Connect to the brain's snapshot/live-event stream.
    pub async fn watch(&self) -> Result<mpsc::UnboundedReceiver<BrainWireMessage>> {
        let mut request = self.target.ws_url().into_client_request()?;
        request.headers_mut().insert(
            tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
            format!("Bearer {}", self.password).parse()?,
        );
        let (socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .context("could not open brain event stream")?;
        let (_, mut incoming) = socket.split();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(Ok(message)) = incoming.next().await {
                if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                    if let Ok(message) = serde_json::from_str::<BrainWireMessage>(&text) {
                        if tx.send(message).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_defaults_to_daemon_port_and_keeps_mdns_name() {
        let target = RemoteBrainTarget::parse("finch@workstation.local").unwrap();
        assert_eq!(target.display_name(), "finch@workstation.local");
        assert_eq!(target.address, "workstation.local:11435");
    }

    #[test]
    fn target_accepts_an_explicit_port() {
        let target = RemoteBrainTarget::parse("review@10.0.0.4:9000").unwrap();
        assert_eq!(target.machine, "10.0.0.4");
        assert_eq!(target.address, "10.0.0.4:9000");
    }

    #[test]
    fn target_rejects_ambiguous_or_unsafe_values() {
        assert!(RemoteBrainTarget::parse("brain-only").is_err());
        assert!(RemoteBrainTarget::parse("../brain@host").is_err());
        assert!(RemoteBrainTarget::parse("brain@host/path").is_err());
    }
}
