//! Finch-native Google Gemini browser (PKCE) authentication UX.
//!
//! This module owns only local OAuth ceremony and named credential metadata.
//! It never reads another application's credential store.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use axum::extract::{RawQuery, State};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use chrono::Utc;
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::config::{CredentialProvider, ProviderCredential};
use crate::oauth::{
    DeviceAuthorization, FileOAuthCredentialStore, OAuthClient, OAuthCredentialStore, OAuthDialect,
    OAuthTokenRecord, PendingBrowserAuthorization,
};
use crate::providers::GoogleGeminiOAuthDialect;

/// How long a browser authorization stays valid before it must be restarted.
const BROWSER_AUTHORIZATION_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// Default descriptor-anchored Finch store.
pub fn default_gemini_oauth_store_root() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("Could not determine the Finch home directory for Gemini login")?
        .join(".finch")
        .join("oauth"))
}

/// Secret-free status suitable for script output, Debug, and restart checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiAuthStatus {
    pub credential_ref: String,
    pub account: Option<String>,
    pub state: GeminiAuthState,
}

/// Local OAuth status without conflating an interrupted durable mutation with
/// an intentional signed-out tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiAuthState {
    Active {
        expires_at: chrono::DateTime<Utc>,
        refreshable: bool,
    },
    Expired {
        expires_at: chrono::DateTime<Utc>,
        refreshable: bool,
    },
    SignedOut,
    RecoveryRequired,
}

/// Opaque exact-generation authority for compensating one setup issuance.
#[derive(Clone)]
#[allow(dead_code)]
pub struct GeminiCompensationHandle {
    reference: String,
    generation: String,
}

impl std::fmt::Debug for GeminiCompensationHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GeminiCompensationHandle")
            .field("reference", &self.reference)
            .field("generation", &"[REDACTED GENERATION]")
            .finish()
    }
}

impl GeminiCompensationHandle {
    pub(crate) fn issued(reference: &str, generation: String) -> Self {
        Self {
            reference: reference.to_string(),
            generation,
        }
    }

    pub(crate) fn reference(&self) -> &str {
        &self.reference
    }

    pub(crate) fn generation(&self) -> &str {
        &self.generation
    }
}

/// Metadata plus exact rollback authority from one ensure operation.
#[derive(Debug, Clone)]
pub struct EnsuredGeminiCredential {
    pub credential: ProviderCredential,
    pub compensation: Option<GeminiCompensationHandle>,
}

/// First phase of the named-credential ceremony.
#[derive(Debug, Clone)]
pub enum GeminiNamedCredentialStart {
    Ensured(EnsuredGeminiCredential),
    AuthorizationRequired(DeviceAuthorization),
}

/// Render one stable, secret-free status line for scripts and interactive use.
pub fn render_status_line(status: &GeminiAuthStatus) -> Result<String> {
    crate::oauth::validate_reference(&status.credential_ref)?;
    if let Some(account) = status.account.as_deref() {
        validate_terminal_identifier(account, "Gemini account identifier")?;
    }
    Ok(match &status.state {
        GeminiAuthState::Active {
            expires_at,
            refreshable,
        } => format!(
            "gemini-sub credential={} status=active account={} expires_at={} refreshable={}",
            status.credential_ref,
            status.account.as_deref().unwrap_or("unknown"),
            expires_at.to_rfc3339(),
            refreshable
        ),
        GeminiAuthState::Expired {
            expires_at,
            refreshable,
        } => format!(
            "gemini-sub credential={} status=expired account={} expires_at={} refreshable={} action=login",
            status.credential_ref,
            status.account.as_deref().unwrap_or("unknown"),
            expires_at.to_rfc3339(),
            refreshable
        ),
        GeminiAuthState::SignedOut => format!(
            "gemini-sub credential={} status=signed_out",
            status.credential_ref
        ),
        GeminiAuthState::RecoveryRequired => format!(
            "gemini-sub credential={} status=recovery_required action=finch_auth_recover",
            status.credential_ref
        ),
    })
}

fn validate_terminal_identifier(value: &str, label: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("{label} is unsafe for terminal output");
    }
    Ok(())
}

/// User-selected presentation actions for browser PKCE flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserLoginPresentation {
    pub open_browser: bool,
}

impl Default for BrowserLoginPresentation {
    fn default() -> Self {
        Self { open_browser: true }
    }
}

/// Legacy/device presentation compatibility struct.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceLoginPresentation {
    pub copy_code: bool,
    pub open_browser: bool,
}

impl From<DeviceLoginPresentation> for BrowserLoginPresentation {
    fn from(device: DeviceLoginPresentation) -> Self {
        Self {
            open_browser: device.open_browser,
        }
    }
}

/// Setup-facing named-account boundary.
#[async_trait]
pub trait GeminiCredentialAuthenticator: Send + Sync {
    fn compensate_with_tombstone(&self, _handle: &GeminiCompensationHandle) -> Result<()> {
        Ok(())
    }

    async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<EnsuredGeminiCredential>;

    async fn begin_named_credential(
        &self,
        _reference: &str,
        _cancel: CancellationToken,
    ) -> Result<GeminiNamedCredentialStart> {
        bail!("this authenticator does not script the phased named-credential ceremony")
    }

    async fn finish_named_credential(
        &self,
        _reference: &str,
        _pending: &DeviceAuthorization,
        _cancel: CancellationToken,
    ) -> Result<EnsuredGeminiCredential> {
        bail!("this authenticator does not script the phased named-credential ceremony")
    }
}

struct PendingBrowserSession {
    pending: PendingBrowserAuthorization,
    receiver: oneshot::Receiver<Result<String>>,
    server_cancel: CancellationToken,
}

/// Production Finch-native Google Gemini authentication service.
pub struct GeminiAuthService {
    store: Arc<FileOAuthCredentialStore>,
    pending_browser: Arc<Mutex<HashMap<String, PendingBrowserSession>>>,
}

impl std::fmt::Debug for GeminiAuthService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GeminiAuthService([REDACTED CREDENTIAL STORE])")
    }
}

impl GeminiAuthService {
    pub fn production() -> Result<Self> {
        Ok(Self {
            store: Arc::new(FileOAuthCredentialStore::new(
                default_gemini_oauth_store_root()?,
            )),
            pending_browser: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn client(&self) -> Result<OAuthClient<GoogleGeminiOAuthDialect, FileOAuthCredentialStore>> {
        OAuthClient::new(
            Arc::new(GoogleGeminiOAuthDialect::production()?),
            self.store.clone(),
        )
    }

    pub fn status(&self, reference: &str) -> Result<GeminiAuthStatus> {
        let record = self.store.load_existing(reference)?;
        if let Some(record) = record.as_ref() {
            self.client()?.validate_existing_binding(record)?;
        }
        status_from_record(reference, record)
    }

    pub async fn login(
        &self,
        reference: &str,
        presentation: BrowserLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<ProviderCredential> {
        let client = self.client()?;
        login_browser_with(&client, reference, presentation, cancel).await
    }

    pub async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<EnsuredGeminiCredential> {
        match self
            .begin_named_credential(reference, cancel.clone())
            .await?
        {
            GeminiNamedCredentialStart::Ensured(ensured) => Ok(ensured),
            GeminiNamedCredentialStart::AuthorizationRequired(pending) => {
                present_authorization_url(
                    &pending.verification_uri,
                    BrowserLoginPresentation {
                        open_browser: presentation.open_browser,
                    },
                )?;
                self.finish_named_credential(reference, &pending, cancel)
                    .await
            }
        }
    }

    pub async fn begin_named_credential(
        &self,
        reference: &str,
        cancel: CancellationToken,
    ) -> Result<GeminiNamedCredentialStart> {
        let client = self.client()?;
        if let Some(record) = self.store.load(reference)? {
            client.validate_existing_binding(&record)?;
            if record.mutation_pending {
                bail!(
                    "Gemini credential has an interrupted mutation; run `finch auth recover gemini-sub --credential {reference}` before signing in again"
                );
            }
            if !record.revoked {
                if record.expires_at > Utc::now() {
                    client.validate_active_reuse(&record)?;
                    return Ok(GeminiNamedCredentialStart::Ensured(
                        EnsuredGeminiCredential {
                            credential: record.provider_credential(reference),
                            compensation: None,
                        },
                    ));
                }
                if record.refresh_token.is_some() {
                    return Ok(GeminiNamedCredentialStart::Ensured(
                        EnsuredGeminiCredential {
                            credential: client.refresh(reference, cancel).await?,
                            compensation: None,
                        },
                    ));
                }
                bail!(
                    "Gemini credential is expired and unrefreshable; log it out before explicit re-authentication"
                );
            }
        }

        crate::oauth::validate_reference(reference)?;
        client.preflight_reauthentication(reference)?;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("Gemini sign-in could not open a local callback listener")?;
        let port = listener
            .local_addr()
            .context("Gemini sign-in callback listener has no local address")?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");

        let pending = client
            .begin_browser_authorization(&redirect_uri, BROWSER_AUTHORIZATION_LIFETIME)
            .context("Gemini sign-in could not start")?;

        let (sender, receiver) = oneshot::channel::<Result<String>>();
        let state = CallbackState {
            redirect_uri: redirect_uri.clone(),
            result: Arc::new(Mutex::new(Some(sender))),
        };
        let app = axum::Router::new()
            .route("/callback", get(callback_handler))
            .with_state(state);
        let server_cancel = detached_loopback_server_cancel(&cancel);
        let serve_cancel = server_cancel.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move { serve_cancel.cancelled().await })
                .await;
        });

        let auth_url = pending.authorization_url.clone();
        let session_id = uuid::Uuid::new_v4().to_string();
        self.pending_browser.lock().unwrap().insert(
            session_id.clone(),
            PendingBrowserSession {
                pending,
                receiver,
                server_cancel,
            },
        );

        let device_auth = DeviceAuthorization::issued(
            session_id,
            String::new(),
            auth_url,
            None,
            BROWSER_AUTHORIZATION_LIFETIME,
            Duration::from_secs(1),
        )?;
        Ok(GeminiNamedCredentialStart::AuthorizationRequired(
            device_auth,
        ))
    }

    pub async fn finish_named_credential(
        &self,
        reference: &str,
        pending: &DeviceAuthorization,
        cancel: CancellationToken,
    ) -> Result<EnsuredGeminiCredential> {
        let session = self
            .pending_browser
            .lock()
            .unwrap()
            .remove(&pending.device_code)
            .context(
                "Pending Gemini browser authorization session not found or already completed",
            )?;

        let deadline = tokio::time::Instant::now() + BROWSER_AUTHORIZATION_LIFETIME;
        let callback_result = tokio::select! {
            _ = cancel.cancelled() => {
                session.server_cancel.cancel();
                Err(anyhow::anyhow!("Gemini sign-in was cancelled"))
            }
            _ = tokio::time::sleep_until(deadline) => {
                session.server_cancel.cancel();
                Err(anyhow::anyhow!("Gemini sign-in timed out waiting for the browser callback"))
            }
            received = session.receiver => {
                session.server_cancel.cancel();
                match received {
                    Ok(res) => res,
                    Err(_) => Err(anyhow::anyhow!("Gemini sign-in callback listener closed unexpectedly")),
                }
            }
        };
        let callback_url =
            callback_result.context("Gemini sign-in did not receive a browser callback")?;

        let client = self.client()?;
        let credential = client
            .finish_browser_authorization(reference, session.pending, &callback_url, cancel)
            .await
            .context("Gemini sign-in did not complete")?;

        Ok(EnsuredGeminiCredential {
            credential,
            compensation: None,
        })
    }

    pub async fn logout(
        &self,
        reference: &str,
        cancel: CancellationToken,
    ) -> Result<ProviderCredential> {
        if cancel.is_cancelled() {
            bail!("Gemini logout was cancelled before revocation");
        }
        self.client()?.revoke(reference, cancel).await
    }

    pub fn recover(&self, reference: &str) -> Result<ProviderCredential> {
        self.client()?.recover_interrupted_as_revoked(reference)
    }
}

#[async_trait]
impl GeminiCredentialAuthenticator for GeminiAuthService {
    fn compensate_with_tombstone(&self, handle: &GeminiCompensationHandle) -> Result<()> {
        self.client()?
            .tombstone_local_generation(handle.reference(), handle.generation())?;
        Ok(())
    }

    async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<EnsuredGeminiCredential> {
        GeminiAuthService::ensure_named_credential(self, reference, presentation, cancel).await
    }

    async fn begin_named_credential(
        &self,
        reference: &str,
        cancel: CancellationToken,
    ) -> Result<GeminiNamedCredentialStart> {
        GeminiAuthService::begin_named_credential(self, reference, cancel).await
    }

    async fn finish_named_credential(
        &self,
        reference: &str,
        pending: &DeviceAuthorization,
        cancel: CancellationToken,
    ) -> Result<EnsuredGeminiCredential> {
        GeminiAuthService::finish_named_credential(self, reference, pending, cancel).await
    }
}

pub async fn begin_named_credential_with<D, S>(
    client: &OAuthClient<D, S>,
    store: &S,
    reference: &str,
    cancel: CancellationToken,
) -> Result<GeminiNamedCredentialStart>
where
    D: OAuthDialect + 'static,
    S: OAuthCredentialStore + 'static,
{
    if let Some(record) = store.load(reference)? {
        client.validate_existing_binding(&record)?;
        if record.mutation_pending {
            bail!(
                "Gemini credential has an interrupted mutation; run `finch auth recover gemini-sub --credential {reference}` before signing in again"
            );
        }
        if !record.revoked {
            if record.expires_at > Utc::now() {
                client.validate_active_reuse(&record)?;
                return Ok(GeminiNamedCredentialStart::Ensured(
                    EnsuredGeminiCredential {
                        credential: record.provider_credential(reference),
                        compensation: None,
                    },
                ));
            }
            if record.refresh_token.is_some() {
                return Ok(GeminiNamedCredentialStart::Ensured(
                    EnsuredGeminiCredential {
                        credential: client.refresh(reference, cancel).await?,
                        compensation: None,
                    },
                ));
            }
            bail!(
                "Gemini credential is expired and unrefreshable; log it out before explicit re-authentication"
            );
        }
    }
    crate::oauth::validate_reference(reference)?;
    client.preflight_reauthentication(reference)?;
    let pending = client
        .begin_browser_authorization(
            "http://127.0.0.1:0/callback",
            BROWSER_AUTHORIZATION_LIFETIME,
        )
        .context("Gemini sign-in could not start")?;
    let auth_url = pending.authorization_url.clone();
    let device_auth = DeviceAuthorization::issued(
        "placeholder-session".into(),
        String::new(),
        auth_url,
        None,
        BROWSER_AUTHORIZATION_LIFETIME,
        Duration::from_secs(1),
    )?;
    Ok(GeminiNamedCredentialStart::AuthorizationRequired(
        device_auth,
    ))
}

pub async fn login_browser_with<D, S>(
    client: &OAuthClient<D, S>,
    reference: &str,
    presentation: BrowserLoginPresentation,
    cancel: CancellationToken,
) -> Result<ProviderCredential>
where
    D: OAuthDialect + 'static,
    S: OAuthCredentialStore + 'static,
{
    crate::oauth::validate_reference(reference)?;
    client.preflight_reauthentication(reference)?;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("Gemini sign-in could not open a local callback listener")?;
    let port = listener
        .local_addr()
        .context("Gemini sign-in callback listener has no local address")?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let pending = client
        .begin_browser_authorization(&redirect_uri, BROWSER_AUTHORIZATION_LIFETIME)
        .context("Gemini sign-in could not start")?;

    present_authorization_url(&pending.authorization_url, presentation)?;

    let callback_url = wait_for_callback(
        listener,
        redirect_uri,
        BROWSER_AUTHORIZATION_LIFETIME,
        cancel.clone(),
    )
    .await
    .context("Gemini sign-in did not receive a browser callback")?;

    client
        .finish_browser_authorization(reference, pending, &callback_url, cancel)
        .await
        .context("Gemini sign-in did not complete")
}

#[derive(Clone)]
struct CallbackState {
    redirect_uri: String,
    result: Arc<Mutex<Option<oneshot::Sender<Result<String>>>>>,
}

fn detached_loopback_server_cancel(_flow_cancel: &CancellationToken) -> CancellationToken {
    // The listener has a shorter lifetime than the OAuth operation. Shutting it
    // down after the callback must not cancel the PKCE exchange or token
    // verification that follows on the flow token.
    CancellationToken::new()
}

async fn wait_for_callback(
    listener: tokio::net::TcpListener,
    redirect_uri: String,
    lifetime: Duration,
    cancel: CancellationToken,
) -> Result<String> {
    let (sender, receiver) = oneshot::channel::<Result<String>>();
    let state = CallbackState {
        redirect_uri,
        result: Arc::new(Mutex::new(Some(sender))),
    };
    let app = axum::Router::new()
        .route("/callback", get(callback_handler))
        .with_state(state);
    let server_cancel = detached_loopback_server_cancel(&cancel);
    let serve_cancel = server_cancel.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { serve_cancel.cancelled().await })
            .await;
    });

    let deadline = tokio::time::Instant::now() + lifetime;
    let outcome = tokio::select! {
        _ = cancel.cancelled() => Err(anyhow::anyhow!("Gemini sign-in was cancelled")),
        _ = tokio::time::sleep_until(deadline) => Err(anyhow::anyhow!("Gemini sign-in timed out waiting for the browser callback")),
        received = receiver => match received {
            Ok(res) => res,
            Err(_) => Err(anyhow::anyhow!("Gemini sign-in callback listener closed unexpectedly")),
        },
    };
    server_cancel.cancel();
    let _ = server.await;
    outcome
}

async fn callback_handler(
    State(state): State<CallbackState>,
    RawQuery(query): RawQuery,
) -> impl IntoResponse {
    let query_str = query.unwrap_or_default();
    let callback_url_result = reqwest::Url::parse(&format!("{}?{}", state.redirect_uri, query_str));
    let parsed_url = match callback_url_result {
        Ok(u) => u,
        Err(err) => {
            if let Some(sender) = state
                .result
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
            {
                let _ = sender.send(Err(anyhow::anyhow!("Invalid callback query: {err}")));
            }
            return Html(
                "<html><body><h3>Invalid callback</h3><p>Could not parse query parameters.</p></body></html>".to_string()
            );
        }
    };
    let parsed: Vec<(String, String)> = parsed_url.query_pairs().into_owned().collect();

    if let Some((_, error)) = parsed.iter().find(|(k, _)| k == "error") {
        let desc = parsed
            .iter()
            .find(|(k, _)| k == "error_description")
            .map(|(_, v)| v.as_str())
            .unwrap_or(error);
        if let Some(sender) = state
            .result
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = sender.send(Err(anyhow::anyhow!("Google sign-in error: {desc}")));
        }
        return Html(format!(
            "<html><body><h3>Google Gemini sign-in failed</h3><p>{desc}</p><p>You can close this tab and return to the terminal.</p></body></html>"
        ));
    }

    let state_val = parsed
        .iter()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.as_str());
    let code_val = parsed
        .iter()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.as_str());

    match (state_val, code_val) {
        (Some(state_param), Some(code_param)) => {
            let mut sanitized = match reqwest::Url::parse(&state.redirect_uri) {
                Ok(u) => u,
                Err(err) => {
                    if let Some(sender) = state.result.lock().unwrap_or_else(|p| p.into_inner()).take() {
                        let _ = sender.send(Err(anyhow::anyhow!("Invalid redirect URI: {err}")));
                    }
                    return Html(
                        "<html><body><h3>Google Gemini sign-in error</h3><p>Internal callback error.</p></body></html>".to_string()
                    );
                }
            };
            sanitized
                .query_pairs_mut()
                .append_pair("state", state_param)
                .append_pair("code", code_param);

            if let Some(sender) = state.result.lock().unwrap_or_else(|p| p.into_inner()).take() {
                let _ = sender.send(Ok(sanitized.to_string()));
            }

            Html(
                "<html><body><h3>Google Gemini sign-in complete</h3><p>You can close this tab and return to Finch.</p></body></html>".to_string()
            )
        }
        _ => Html(
            "<html><body><h3>Invalid callback</h3><p>Missing state or authorization code.</p></body></html>".to_string()
        ),
    }
}

fn status_from_record(
    reference: &str,
    record: Option<OAuthTokenRecord>,
) -> Result<GeminiAuthStatus> {
    Ok(match record {
        Some(record) if record.provider == CredentialProvider::GeminiSubscription => {
            validate_terminal_identifier(&record.account, "Gemini account identifier")?;
            GeminiAuthStatus {
                credential_ref: reference.to_string(),
                account: Some(record.account.clone()),
                state: if record.mutation_pending {
                    GeminiAuthState::RecoveryRequired
                } else if record.revoked {
                    GeminiAuthState::SignedOut
                } else if record.expires_at <= Utc::now() {
                    GeminiAuthState::Expired {
                        expires_at: record.expires_at,
                        refreshable: record.refresh_token.is_some(),
                    }
                } else {
                    GeminiAuthState::Active {
                        expires_at: record.expires_at,
                        refreshable: record.refresh_token.is_some(),
                    }
                },
            }
        }
        Some(_) => bail!("named credential belongs to a different provider"),
        None => GeminiAuthStatus {
            credential_ref: reference.to_string(),
            account: None,
            state: GeminiAuthState::SignedOut,
        },
    })
}

fn present_authorization_url(url: &str, presentation: BrowserLoginPresentation) -> Result<()> {
    println!("Google Gemini subscription sign-in URL: {url}");
    println!("Waiting for the browser sign-in to complete… Press Ctrl+C to cancel.");
    let _ = io::stdout().flush();
    if presentation.open_browser {
        if let Err(err) = open_browser(url) {
            eprintln!(
                "Could not open browser automatically ({err}); please use the displayed URL."
            );
        } else {
            println!("Opened the Gemini sign-in page in the default browser.");
        }
    }
    Ok(())
}

pub(crate) fn open_browser(url: &str) -> Result<()> {
    #[cfg(test)]
    return Ok(());
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg("--").arg(url).status();
    #[cfg(target_os = "linux")]
    let status = Command::new("xdg-open").arg(url).status();
    #[cfg(target_os = "windows")]
    let status = Command::new("rundll32")
        .arg("url.dll,FileProtocolHandler")
        .arg(url)
        .status();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let status: std::io::Result<std::process::ExitStatus> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unsupported browser launcher",
    ));
    let status =
        status.context("Could not open a browser; use the displayed Gemini sign-in URL")?;
    if !status.success() {
        bail!("Browser opener failed; use the displayed Gemini sign-in URL");
    }
    Ok(())
}

pub fn save_named_credential(
    config: crate::config::Config,
    credential: ProviderCredential,
) -> Result<()> {
    let mut credentials = config.credentials().to_vec();
    credentials.retain(|existing| existing.name != credential.name);
    credentials.push(credential);
    config
        .with_credentials(credentials)
        .save()
        .context(
            "Gemini token remains safely stored, but config metadata could not be saved; run `finch setup` to finish binding it",
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudienceBinding, CredentialKind, EndpointFamily};
    use chrono::TimeDelta;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryStore(Mutex<Option<OAuthTokenRecord>>);

    impl OAuthCredentialStore for MemoryStore {
        fn load(&self, _reference: &str) -> Result<Option<OAuthTokenRecord>> {
            Ok(self.0.lock().unwrap().clone())
        }

        fn compare_and_swap(
            &self,
            _reference: &str,
            _expected_generation: Option<&str>,
            replacement: &OAuthTokenRecord,
        ) -> Result<()> {
            *self.0.lock().unwrap() = Some(replacement.clone());
            Ok(())
        }
    }

    fn record() -> OAuthTokenRecord {
        OAuthTokenRecord {
            dialect_id: "google_gemini_subscription".into(),
            protocol_revision: crate::providers::GEMINI_OAUTH_PROTOCOL_REVISION.into(),
            provider: CredentialProvider::GeminiSubscription,
            kind: CredentialKind::OauthBrowserPkce,
            issuer: "google-gemini".into(),
            audience: AudienceBinding::standard(EndpointFamily::GeminiSubscription),
            client_id: "764086051850-6qr4p6gpi6hn506pt8ejuq83di341hur.apps.googleusercontent.com"
                .into(),
            account: "user@example.com".into(),
            tenant: None,
            project: None,
            scopes: crate::providers::gemini_required_scopes(),
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            id_token: None,
            expires_at: Utc::now() + TimeDelta::hours(1),
            generation: "gen-1".into(),
            revoked: false,
            mutation_pending: false,
        }
    }

    #[test]
    fn loopback_server_cancellation_is_detached_from_oauth_flow() {
        let flow_cancel = CancellationToken::new();
        let server_cancel = detached_loopback_server_cancel(&flow_cancel);

        flow_cancel.cancel();

        assert!(
            !server_cancel.is_cancelled(),
            "Gemini loopback listener cancellation must be independently owned so flow cancellation is handled explicitly"
        );
        server_cancel.cancel();
        assert!(
            server_cancel.is_cancelled(),
            "Gemini loopback listener must remain directly cancellable after detaching it from the OAuth flow"
        );
    }

    #[test]
    fn render_status_line_formats_correctly() {
        let status = GeminiAuthStatus {
            credential_ref: "gemini:default".into(),
            account: Some("user@example.com".into()),
            state: GeminiAuthState::Active {
                expires_at: Utc::now() + TimeDelta::hours(1),
                refreshable: true,
            },
        };
        let line = render_status_line(&status).unwrap();
        assert!(line.contains("status=active"));
        assert!(line.contains("account=user@example.com"));
        assert!(line.contains("refreshable=true"));
    }

    #[test]
    fn status_from_record_handles_signed_out() {
        let status = status_from_record("gemini:default", None).unwrap();
        assert_eq!(status.state, GeminiAuthState::SignedOut);
    }

    #[test]
    fn status_from_record_handles_active() {
        let status = status_from_record("gemini:default", Some(record())).unwrap();
        assert!(matches!(status.state, GeminiAuthState::Active { .. }));
    }

    #[test]
    fn wrong_provider_status_fails_without_treating_api_key_as_subscription() {
        let mut hostile = record();
        hostile.provider = CredentialProvider::GeminiAiStudio;
        assert!(status_from_record("gemini:default", Some(hostile)).is_err());
    }

    struct UnreachableVerifier;

    #[async_trait]
    impl crate::providers::GeminiTokenVerifier for UnreachableVerifier {
        fn preflight(&self) -> Result<()> {
            Ok(())
        }

        async fn verify(
            &self,
            _id_token: Option<&str>,
            _access_token: &str,
            _cancel: &CancellationToken,
        ) -> Result<crate::providers::VerifiedGeminiClaims> {
            bail!("token verifier must remain unreachable")
        }
    }

    #[tokio::test]
    async fn begin_named_credential_reuses_active_record_without_device_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let dialect = Arc::new(
            GoogleGeminiOAuthDialect::for_test(&origin, Arc::new(UnreachableVerifier)).unwrap(),
        );
        let mut aligned = record();
        let descriptor = dialect.descriptor();
        aligned.dialect_id = descriptor.dialect_id.clone();
        aligned.protocol_revision = descriptor.protocol_revision.clone();
        aligned.provider = descriptor.provider;
        aligned.kind = descriptor.credential_kind;
        aligned.issuer = descriptor.issuer.clone();
        aligned.audience = descriptor.audience.clone();
        aligned.client_id = descriptor.client_id.clone();
        aligned.scopes = descriptor.scopes.clone();
        let generation = aligned.generation.clone();
        let store = Arc::new(MemoryStore(Mutex::new(Some(aligned))));
        let client = OAuthClient::new(dialect, store.clone()).unwrap();
        let start = begin_named_credential_with(
            &client,
            store.as_ref(),
            "gemini-sub:work",
            CancellationToken::new(),
        )
        .await
        .expect("an active named credential must begin as ensured without any dialog");
        let GeminiNamedCredentialStart::Ensured(ensured) = start else {
            panic!("expected the reuse path, got a device authorization requirement");
        };
        assert_eq!(
            ensured.credential.account.as_deref(),
            Some("user@example.com")
        );
        assert!(ensured.compensation.is_none());
        let persisted = store.0.lock().unwrap();
        assert_eq!(persisted.as_ref().unwrap().generation, generation);
    }

    #[tokio::test]
    async fn wait_for_callback_sanitizes_google_params_and_extracts_state_and_code() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let cancel = CancellationToken::new();

        let wait_handle = tokio::spawn(wait_for_callback(
            listener,
            redirect_uri.clone(),
            Duration::from_secs(5),
            cancel,
        ));

        // Make an HTTP GET mimicking Google's redirect with extra params (scope, authuser, prompt)
        let client = reqwest::Client::new();
        let test_url = format!(
            "http://127.0.0.1:{port}/callback?state=secret_state&code=4%2F0Abc123&scope=openid%20email&authuser=0&prompt=consent"
        );
        let resp = client.get(&test_url).send().await.unwrap();
        assert!(resp.status().is_success());
        let body = resp.text().await.unwrap();
        assert!(body.contains("Google Gemini sign-in complete"));

        let callback_url = wait_handle.await.unwrap().unwrap();
        assert_eq!(
            callback_url,
            format!("http://127.0.0.1:{port}/callback?state=secret_state&code=4%2F0Abc123")
        );
    }

    #[tokio::test]
    async fn wait_for_callback_handles_google_error_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let cancel = CancellationToken::new();

        let wait_handle = tokio::spawn(wait_for_callback(
            listener,
            redirect_uri.clone(),
            Duration::from_secs(5),
            cancel,
        ));

        let client = reqwest::Client::new();
        let test_url = format!(
            "http://127.0.0.1:{port}/callback?error=access_denied&error_description=User%20denied%20consent"
        );
        let resp = client.get(&test_url).send().await.unwrap();
        assert!(resp.status().is_success());
        let body = resp.text().await.unwrap();
        assert!(body.contains("Google Gemini sign-in failed"));

        let err = wait_handle.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("User denied consent"));
    }
}
