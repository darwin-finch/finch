//! Finch-native Claude subscription browser (authorization-code + PKCE)
//! authentication UX.
//!
//! This module owns only local OAuth ceremony and named credential metadata.
//! It never discovers or launches the Claude Code CLI and never reads
//! another application's credential store.
//!
//! **Opt-in, disabled by default.** This feature reuses Claude Code's own
//! OAuth client identity (Finch has no client id of its own registered with
//! Anthropic for this surface), which matches a reused-client-identity
//! pattern Anthropic has a documented history of actively detecting and
//! blocking for other third-party tools. Callers must check
//! `Config::features.claude_subscription_oauth_enabled` before invoking
//! anything here that talks to Anthropic (see `run_claude_auth` in
//! `src/main.rs` and the construction guard in `src/providers/factory.rs`).
//! See `crates/finch-providers/AGENTS.md` for the full rationale.

use anyhow::{bail, Context, Result};
use axum::extract::{RawQuery, State};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::config::{CredentialProvider, ProviderCredential};
use crate::oauth::{FileOAuthCredentialStore, OAuthClient, OAuthCredentialStore, OAuthTokenRecord};
use crate::providers::ClaudeOAuthDialect;
use chrono::Utc;

/// How long a browser authorization stays valid before it must be restarted.
/// Anthropic does not document this value; ten minutes is a generous window
/// for an interactive sign-in (including SSO/2FA) without leaving a loopback
/// listener open indefinitely.
const BROWSER_AUTHORIZATION_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// Default descriptor-anchored Finch store. No foreign application path is
/// consulted or migrated implicitly. Shares the same `~/.finch/oauth` root as
/// ChatGPT/Grok — the store is keyed by named reference, not provider.
pub fn default_claude_oauth_store_root() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("Could not determine the Finch home directory for Claude login")?
        .join(".finch")
        .join("oauth"))
}

/// Secret-free status suitable for script output, Debug, and restart checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeAuthStatus {
    pub credential_ref: String,
    pub account: Option<String>,
    pub state: ClaudeAuthState,
}

/// Local OAuth status without conflating an interrupted durable mutation with
/// an intentional signed-out tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeAuthState {
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

/// User-selected presentation actions. Opening a browser is never implicit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BrowserLoginPresentation {
    pub open_browser: bool,
}

/// Render one stable, secret-free status line for scripts and interactive use.
pub fn render_status_line(status: &ClaudeAuthStatus) -> Result<String> {
    crate::oauth::validate_reference(&status.credential_ref)?;
    if let Some(account) = status.account.as_deref() {
        validate_terminal_identifier(account, "Claude account identifier")?;
    }
    Ok(match &status.state {
        ClaudeAuthState::Active {
            expires_at,
            refreshable,
        } => format!(
            "claude credential={} status=active account={} expires_at={} refreshable={}",
            status.credential_ref,
            status.account.as_deref().unwrap_or("unknown"),
            expires_at.to_rfc3339(),
            refreshable
        ),
        ClaudeAuthState::Expired {
            expires_at,
            refreshable,
        } => format!(
            "claude credential={} status=expired account={} expires_at={} refreshable={} action=login",
            status.credential_ref,
            status.account.as_deref().unwrap_or("unknown"),
            expires_at.to_rfc3339(),
            refreshable
        ),
        ClaudeAuthState::SignedOut => format!(
            "claude credential={} status=signed_out",
            status.credential_ref
        ),
        ClaudeAuthState::RecoveryRequired => format!(
            "claude credential={} status=recovery_required action=finch_auth_recover",
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

/// Production Finch-native Claude subscription authentication service.
pub struct ClaudeAuthService {
    store: Arc<FileOAuthCredentialStore>,
}

impl std::fmt::Debug for ClaudeAuthService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ClaudeAuthService([REDACTED CREDENTIAL STORE])")
    }
}

impl ClaudeAuthService {
    pub fn production() -> Result<Self> {
        Ok(Self {
            store: Arc::new(FileOAuthCredentialStore::new(
                default_claude_oauth_store_root()?,
            )),
        })
    }

    fn client(&self) -> Result<OAuthClient<ClaudeOAuthDialect, FileOAuthCredentialStore>> {
        OAuthClient::new(
            Arc::new(ClaudeOAuthDialect::production()?),
            self.store.clone(),
        )
    }

    /// Read local status without refresh, HTTP, or discovery. Existing
    /// records are checked against the production dialect before projection.
    pub fn status(&self, reference: &str) -> Result<ClaudeAuthStatus> {
        let record = self.store.load(reference)?;
        if let Some(record) = record.as_ref() {
            self.client()?.validate_existing_binding(record)?;
        }
        status_from_record(reference, record)
    }

    /// Run the full browser ceremony — loopback listener, authorization URL,
    /// callback correlation, code exchange — and return #174 metadata only
    /// after signed-token validation and crash-safe persistence.
    ///
    /// Callers MUST check `Config::features.claude_subscription_oauth_enabled`
    /// before calling this; it is not checked here because this service has
    /// no `Config` dependency (mirrors `ChatGptAuthService`/`GrokAuthService`).
    pub async fn login(
        &self,
        reference: &str,
        presentation: BrowserLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<ProviderCredential> {
        let client = self.client()?;
        login_browser_with(&client, reference, presentation, cancel).await
    }

    /// Locally tombstone the named credential. Anthropic exposes no known
    /// public revocation endpoint for this client id (see
    /// `finch_providers::ClaudeAuthStageError::RevocationUnsupported` and the
    /// dialect's module doc comment), so this is local-only: Finch forgets
    /// the token, but it is not invalidated server-side. Advise the user to
    /// revoke access from their Anthropic account settings for full
    /// server-side invalidation.
    pub fn logout(&self, reference: &str) -> Result<ProviderCredential> {
        crate::oauth::validate_reference(reference)?;
        let client = self.client()?;
        let current = self
            .store
            .load(reference)?
            .context("named Claude subscription credential is missing")?;
        client.validate_existing_binding(&current)?;
        if current.mutation_pending {
            bail!(
                "Claude credential `{reference}` has an interrupted mutation; run `finch auth recover claude --credential {reference}` first"
            );
        }
        let mut tombstone = current.clone();
        tombstone.access_token.clear();
        tombstone.refresh_token = None;
        tombstone.id_token = None;
        tombstone.generation = uuid::Uuid::new_v4().to_string();
        tombstone.revoked = true;
        tombstone.mutation_pending = false;
        self.store
            .compare_and_swap(reference, Some(&current.generation), &tombstone)?;
        Ok(tombstone.provider_credential(reference))
    }

    /// Resolve an interrupted refresh locally without contacting Anthropic,
    /// retaining a durable tombstone for explicit reauthentication.
    pub fn recover(&self, reference: &str) -> Result<ProviderCredential> {
        self.client()?.recover_interrupted_as_revoked(reference)
    }
}

/// Shared browser ceremony used by scriptable login. Tests inject the same
/// OAuth production boundary with a deterministic dialect/server fixture.
async fn login_browser_with<D, S>(
    client: &OAuthClient<D, S>,
    reference: &str,
    presentation: BrowserLoginPresentation,
    cancel: CancellationToken,
) -> Result<ProviderCredential>
where
    D: crate::oauth::OAuthDialect + 'static,
    S: OAuthCredentialStore + 'static,
{
    crate::oauth::validate_reference(reference)?;
    client.preflight_reauthentication(reference)?;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("Claude sign-in could not open a local callback listener")?;
    let port = listener
        .local_addr()
        .context("Claude sign-in callback listener has no local address")?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let pending = client
        .begin_browser_authorization(&redirect_uri, BROWSER_AUTHORIZATION_LIFETIME)
        .context("Claude sign-in could not start")?;

    present_authorization_url(&pending.authorization_url, presentation)?;

    let callback_url = wait_for_callback(
        listener,
        redirect_uri,
        BROWSER_AUTHORIZATION_LIFETIME,
        cancel.clone(),
    )
    .await
    .context("Claude sign-in did not receive a browser callback")?;

    client
        .finish_browser_authorization(reference, pending, &callback_url, cancel)
        .await
        .context("Claude sign-in did not complete")
}

#[derive(Clone)]
struct CallbackState {
    /// The exact scheme+host+port+path Finch told Anthropic to redirect to
    /// (no query). The handler appends the browser's raw query string to
    /// reconstruct the full callback URL `finish_browser_authorization`
    /// validates against `pending`.
    redirect_uri: String,
    result: Arc<Mutex<Option<oneshot::Sender<String>>>>,
}

/// Accept exactly one browser redirect on `listener` and return the full
/// callback URL (scheme/host/port/path/query), or fail on cancellation or
/// the authorization's own lifetime elapsing. The loopback HTTP server is
/// torn down as soon as one callback is observed or the deadline passes.
async fn wait_for_callback(
    listener: tokio::net::TcpListener,
    redirect_uri: String,
    lifetime: Duration,
    cancel: CancellationToken,
) -> Result<String> {
    let (sender, receiver) = oneshot::channel::<String>();
    let state = CallbackState {
        redirect_uri,
        result: Arc::new(Mutex::new(Some(sender))),
    };
    let app = axum::Router::new()
        .route("/callback", get(callback_handler))
        .with_state(state);
    let server_cancel = cancel.child_token();
    let serve_cancel = server_cancel.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { serve_cancel.cancelled().await })
            .await;
    });

    let deadline = tokio::time::Instant::now() + lifetime;
    let outcome = tokio::select! {
        _ = cancel.cancelled() => Err(anyhow::anyhow!("Claude sign-in was cancelled")),
        _ = tokio::time::sleep_until(deadline) => Err(anyhow::anyhow!("Claude sign-in timed out waiting for the browser callback")),
        received = receiver => received.context("Claude sign-in callback listener closed unexpectedly"),
    };
    server_cancel.cancel();
    let _ = server.await;
    outcome
}

async fn callback_handler(
    State(state): State<CallbackState>,
    RawQuery(query): RawQuery,
) -> impl IntoResponse {
    // The browser's raw query string is forwarded verbatim (not decoded and
    // re-encoded), so `finish_browser_authorization`'s exact
    // parameter-count/name check sees exactly what Anthropic sent.
    let callback_url = match query {
        Some(query) => format!("{}?{query}", state.redirect_uri),
        None => state.redirect_uri.clone(),
    };
    if let Some(sender) = state
        .result
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take()
    {
        let _ = sender.send(callback_url);
    }
    Html(
        "<html><body><p>Claude sign-in complete. You can close this tab and return to the terminal.</p></body></html>",
    )
}

fn present_authorization_url(url: &str, presentation: BrowserLoginPresentation) -> Result<()> {
    println!("Claude sign-in URL: {url}");
    println!("Waiting for the browser sign-in to complete… Press Ctrl+C to cancel.");
    use std::io::Write;
    std::io::stdout().flush()?;
    if presentation.open_browser {
        open_browser(url)?;
        println!("Opened the Claude sign-in page in the default browser.");
    }
    Ok(())
}

fn open_browser(url: &str) -> Result<()> {
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
        status.context("Could not open a browser; use the displayed Claude sign-in URL")?;
    if !status.success() {
        bail!("Browser opener failed; use the displayed Claude sign-in URL");
    }
    Ok(())
}

fn status_from_record(
    reference: &str,
    record: Option<OAuthTokenRecord>,
) -> Result<ClaudeAuthStatus> {
    Ok(match record {
        Some(record) if record.provider == CredentialProvider::ClaudeSubscription => {
            validate_terminal_identifier(&record.account, "Claude account identifier")?;
            ClaudeAuthStatus {
                credential_ref: reference.to_string(),
                account: Some(record.account.clone()),
                state: if record.mutation_pending {
                    ClaudeAuthState::RecoveryRequired
                } else if record.revoked {
                    ClaudeAuthState::SignedOut
                } else if record.expires_at <= Utc::now() {
                    ClaudeAuthState::Expired {
                        expires_at: record.expires_at,
                        refreshable: record.refresh_token.is_some(),
                    }
                } else {
                    ClaudeAuthState::Active {
                        expires_at: record.expires_at,
                        refreshable: record.refresh_token.is_some(),
                    }
                },
            }
        }
        Some(_) => bail!("named credential belongs to a different provider"),
        None => ClaudeAuthStatus {
            credential_ref: reference.to_string(),
            account: None,
            state: ClaudeAuthState::SignedOut,
        },
    })
}

/// Replace or append one secret-free named credential and save no token data
/// to config.toml.
pub fn save_named_credential(
    config: crate::config::Config,
    credential: ProviderCredential,
) -> Result<()> {
    save_named_credential_with(config, credential, crate::config::Config::save)
}

fn save_named_credential_with<F>(
    mut config: crate::config::Config,
    credential: ProviderCredential,
    save: F,
) -> Result<()>
where
    F: FnOnce(&crate::config::Config) -> Result<()>,
{
    let mut credentials = config.credentials().to_vec();
    credentials.retain(|existing| existing.name != credential.name);
    credentials.push(credential);
    config = config.with_credentials(credentials);
    save(&config).context(
        "Claude token remains safely stored, but config metadata could not be saved; run `finch auth status claude` and then `finch setup` to finish binding it",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudienceBinding, CredentialKind, EndpointFamily};
    use chrono::TimeDelta;
    use finch_providers::claude_required_scopes;
    use finch_providers::OAuthDialect;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct MemoryStore(StdMutex<Option<OAuthTokenRecord>>);

    impl OAuthCredentialStore for MemoryStore {
        fn load(&self, _reference: &str) -> Result<Option<OAuthTokenRecord>> {
            Ok(self.0.lock().unwrap().clone())
        }

        fn compare_and_swap(
            &self,
            _reference: &str,
            expected_generation: Option<&str>,
            replacement: &OAuthTokenRecord,
        ) -> Result<()> {
            let mut record = self.0.lock().unwrap();
            if record.as_ref().map(|value| value.generation.as_str()) != expected_generation {
                bail!("generation mismatch");
            }
            *record = Some(replacement.clone());
            Ok(())
        }
    }

    fn record() -> OAuthTokenRecord {
        OAuthTokenRecord {
            dialect_id: "anthropic_claude_subscription".into(),
            protocol_revision: "pinned".into(),
            provider: CredentialProvider::ClaudeSubscription,
            kind: CredentialKind::OauthBrowserPkce,
            issuer: "anthropic-claude".into(),
            audience: AudienceBinding::standard(EndpointFamily::ClaudeSubscription),
            client_id: "public-client".into(),
            account: "acct-redacted".into(),
            tenant: None,
            project: None,
            scopes: claude_required_scopes(),
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            id_token: None,
            expires_at: Utc::now() + TimeDelta::hours(1),
            generation: "generation".into(),
            revoked: false,
            mutation_pending: false,
        }
    }

    #[test]
    fn status_and_debug_are_local_secret_free_restart_projections() {
        let status = status_from_record("claude:work", Some(record())).unwrap();
        assert_eq!(status.account.as_deref(), Some("acct-redacted"));
        let rendered = format!("{status:?}");
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
    }

    #[test]
    fn wrong_provider_status_fails_without_mutation() {
        let mut hostile = record();
        hostile.provider = CredentialProvider::Anthropic;
        assert!(status_from_record("claude:work", Some(hostile)).is_err());
    }

    #[test]
    fn interrupted_mutation_status_requires_recovery_without_secret_output() {
        let mut interrupted = record();
        interrupted.mutation_pending = true;
        let status = status_from_record("claude:work", Some(interrupted)).unwrap();
        assert_eq!(status.state, ClaudeAuthState::RecoveryRequired);
        assert_eq!(
            render_status_line(&status).unwrap(),
            "claude credential=claude:work status=recovery_required action=finch_auth_recover"
        );
    }

    #[test]
    fn script_status_lines_distinguish_active_and_signed_out_without_secrets() {
        let active = status_from_record("claude:work", Some(record())).unwrap();
        let active_line = render_status_line(&active).unwrap();
        assert!(active_line.starts_with(
            "claude credential=claude:work status=active account=acct-redacted expires_at="
        ));
        assert!(active_line.ends_with(" refreshable=true"));
        assert!(!active_line.contains("access-secret"));
        assert_eq!(
            render_status_line(&status_from_record("claude:work", None).unwrap()).unwrap(),
            "claude credential=claude:work status=signed_out"
        );

        let mut expired = record();
        expired.expires_at = Utc::now() - TimeDelta::minutes(1);
        let expired = status_from_record("claude:work", Some(expired)).unwrap();
        assert!(matches!(expired.state, ClaudeAuthState::Expired { .. }));
        assert!(render_status_line(&expired)
            .unwrap()
            .contains("status=expired"));
    }

    #[test]
    fn config_save_failure_after_token_commit_is_actionable_and_keeps_token_record() {
        let stored = record();
        let credential = stored.provider_credential("claude:work");
        let error = save_named_credential_with(
            crate::config::Config::with_providers(vec![]),
            credential,
            |_| anyhow::bail!("read-only config sentinel"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("token remains safely stored"));
        assert!(error.contains("finch auth status claude"));
    }

    #[tokio::test]
    async fn explicit_login_conflicts_fail_before_any_socket_or_store_mutation() {
        let dialect = Arc::new(
            ClaudeOAuthDialect::for_test("http://127.0.0.1:1", "http://127.0.0.1:1").unwrap(),
        );
        let descriptor = dialect.descriptor();
        let mut hostile = record();
        hostile.dialect_id = descriptor.dialect_id.clone();
        hostile.protocol_revision = descriptor.protocol_revision.clone();
        hostile.provider = descriptor.provider;
        hostile.kind = descriptor.credential_kind;
        hostile.issuer = descriptor.issuer.clone();
        hostile.audience = descriptor.audience.clone();
        hostile.client_id = descriptor.client_id.clone();
        hostile.scopes = descriptor.scopes.clone();
        // An active (non-revoked) record already exists locally; login must
        // refuse to start (no listener, no store mutation) rather than race
        // a second authorization against it.
        let generation = hostile.generation.clone();
        let store = Arc::new(MemoryStore(StdMutex::new(Some(hostile))));
        let client = finch_providers::OAuthClient::new(dialect, store.clone()).unwrap();
        assert!(login_browser_with(
            &client,
            "claude:work",
            BrowserLoginPresentation::default(),
            CancellationToken::new(),
        )
        .await
        .is_err());
        assert_eq!(
            store.0.lock().unwrap().as_ref().unwrap().generation,
            generation
        );
    }

    #[tokio::test]
    async fn login_completes_a_real_loopback_callback_round_trip() {
        let mut server = mockito::Server::new_async().await;
        let token_mock = server
            .mock("POST", "/v1/oauth/token")
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "access_token": "browser-access-secret",
                    "refresh_token": "browser-refresh-secret",
                    "expires_in": 28800,
                    "account": {"uuid": "acct-browser"}
                })
                .to_string(),
            )
            .create_async()
            .await;
        let dialect = Arc::new(ClaudeOAuthDialect::for_test(&server.url(), &server.url()).unwrap());
        let store = Arc::new(MemoryStore(StdMutex::new(None)));
        let client = finch_providers::OAuthClient::new(dialect, store).unwrap();

        // Bind a real listener, spawn the callback server, and fire a real
        // HTTP GET at it exactly as a browser redirect would, then confirm
        // the reconstructed callback URL round-trips through
        // `finish_browser_authorization` and the token exchange.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let pending = client
            .begin_browser_authorization(&redirect_uri, Duration::from_secs(30))
            .unwrap();
        let state = reqwest::Url::parse(&pending.authorization_url)
            .unwrap()
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.to_string())
            .unwrap();

        let wait = tokio::spawn(wait_for_callback(
            listener,
            redirect_uri.clone(),
            Duration::from_secs(5),
            CancellationToken::new(),
        ));
        // Give the axum server a moment to start listening before the
        // "browser" fires its GET.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let http = reqwest::Client::new();
        let response = http
            .get(format!(
                "{redirect_uri}?state={state}&code=browser-authorization-code"
            ))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let callback_url = wait.await.unwrap().unwrap();
        assert_eq!(
            callback_url,
            format!("{redirect_uri}?state={state}&code=browser-authorization-code")
        );

        let credential = client
            .finish_browser_authorization(
                "claude:work",
                pending,
                &callback_url,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(credential.account.as_deref(), Some("acct-browser"));
        assert_eq!(credential.provider, CredentialProvider::ClaudeSubscription);
        token_mock.assert_async().await;
    }
}
