//! Injected environmental ports for provider and OAuth runtimes.
//!
//! Production Finch supplies the default implementations. Deterministic tests
//! inject fakes for HTTP, time, sleep, cancellation, credential storage,
//! logging, and billing-action confirmation.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::oauth::{OAuthHttpRequest, OAuthRequestBody, MAX_AUTH_BODY_BYTES};

/// Bounded HTTP POST used by OAuth and catalog transports.
#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn post(
        &self,
        request: OAuthHttpRequest,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<(StatusCode, Vec<u8>)>;
}

/// Clock used for credential expiry and lease checks.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Backoff/sleeper used by OAuth polling and HTTP retry.
#[async_trait]
pub trait Sleeper: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

/// Optional presenter for device/browser authorization UX.
pub trait AuthorizationPresenter: Send + Sync {
    fn present_device_login(&self, verification_uri: &str, user_code: &str) -> Result<()>;
    fn present_browser_login(&self, authorization_url: &str) -> Result<()>;
}

/// Billing-action confirmation. Never auto-fallback between billing modes.
pub trait BillingActionConfirmer: Send + Sync {
    fn confirm(&self, action: &str, provider: &str) -> Result<bool>;
}

/// Secret-free log/telemetry sink.
pub trait ProviderTelemetry: Send + Sync {
    fn event(&self, name: &str, fields: &[(&str, &str)]);
}

/// Production and test runtime handles.
#[derive(Clone)]
pub struct ProviderPorts {
    pub http: Arc<dyn HttpTransport>,
    pub clock: Arc<dyn Clock>,
    pub sleeper: Arc<dyn Sleeper>,
    pub presenter: Arc<dyn AuthorizationPresenter>,
    pub billing: Arc<dyn BillingActionConfirmer>,
    pub telemetry: Arc<dyn ProviderTelemetry>,
    pub timeout: Duration,
}

impl ProviderPorts {
    /// Production ports: reqwest, wall clock, tokio sleep, fail-closed UX.
    pub fn production() -> Result<Self> {
        Ok(Self {
            http: Arc::new(ReqwestTransport::new()?),
            clock: Arc::new(SystemClock),
            sleeper: Arc::new(TokioSleeper),
            presenter: Arc::new(NoopPresenter),
            billing: Arc::new(FailClosedBilling),
            telemetry: Arc::new(TracingTelemetry),
            timeout: Duration::from_secs(30),
        })
    }
}

/// Default reqwest transport. Redirects are rejected.
pub struct ReqwestTransport {
    client: Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .context("Failed to construct bounded HTTP client")?,
        })
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn post(
        &self,
        request: OAuthHttpRequest,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<(StatusCode, Vec<u8>)> {
        let builder = self.client.post(&request.endpoint);
        let builder = match request.body {
            OAuthRequestBody::Form(fields) => builder.form(&fields),
            OAuthRequestBody::Json(value) => builder.json(&value),
        };
        tokio::select! {
            _ = cancel.cancelled() => bail!("HTTP request was cancelled"),
            result = tokio::time::timeout(timeout, async {
                let response = builder.send().await.context("HTTP request failed")?;
                if response.status().is_redirection() {
                    bail!("HTTP endpoint redirect was rejected");
                }
                let status = response.status();
                let mut bytes = Vec::new();
                let mut stream = response.bytes_stream();
                use futures::StreamExt;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.context("HTTP body read failed")?;
                    if bytes.len().saturating_add(chunk.len()) > MAX_AUTH_BODY_BYTES {
                        bail!("HTTP response exceeded size limit");
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok((status, bytes))
            }) => result.context("HTTP request timed out")?,
        }
    }
}

/// Wall-clock UTC.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Tokio sleeper.
pub struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

struct NoopPresenter;

impl AuthorizationPresenter for NoopPresenter {
    fn present_device_login(&self, _verification_uri: &str, _user_code: &str) -> Result<()> {
        Ok(())
    }
    fn present_browser_login(&self, _authorization_url: &str) -> Result<()> {
        Ok(())
    }
}

struct FailClosedBilling;

impl BillingActionConfirmer for FailClosedBilling {
    fn confirm(&self, action: &str, provider: &str) -> Result<bool> {
        bail!("billing action '{action}' for {provider} requires explicit user confirmation")
    }
}

struct TracingTelemetry;

impl ProviderTelemetry for TracingTelemetry {
    fn event(&self, name: &str, fields: &[(&str, &str)]) {
        tracing::debug!(event = name, ?fields, "provider telemetry");
    }
}

/// In-memory clock for deterministic tests.
#[derive(Debug, Default)]
pub struct FrozenClock {
    pub now: std::sync::Mutex<DateTime<Utc>>,
}

impl FrozenClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: std::sync::Mutex::new(now),
        }
    }
}

impl Clock for FrozenClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("frozen clock lock")
    }
}

/// Instant sleeper for deterministic tests.
pub struct InstantSleeper;

#[async_trait]
impl Sleeper for InstantSleeper {
    async fn sleep(&self, _duration: Duration) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::{OAuthHttpRequest, OAuthRequestBody};
    use reqwest::StatusCode;

    struct ScriptedTransport;

    #[async_trait]
    impl HttpTransport for ScriptedTransport {
        async fn post(
            &self,
            _request: OAuthHttpRequest,
            _timeout: Duration,
            _cancel: &CancellationToken,
        ) -> Result<(StatusCode, Vec<u8>)> {
            Ok((StatusCode::OK, b"{\"ok\":true}".to_vec()))
        }
    }

    #[tokio::test]
    async fn fake_http_clock_and_sleeper_are_injectable() {
        let transport = ScriptedTransport;
        let (status, body) = transport
            .post(
                OAuthHttpRequest {
                    endpoint: "https://example.invalid/token".into(),
                    body: OAuthRequestBody::Form(vec![("k".into(), "v".into())]),
                },
                Duration::from_secs(1),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"{\"ok\":true}");
        InstantSleeper.sleep(Duration::from_secs(3600)).await;
        let clock = FrozenClock::new(Utc::now());
        let _ = clock.now();
    }
}
