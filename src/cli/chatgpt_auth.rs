//! Finch-native ChatGPT device authentication UX.
//!
//! This module owns only local OAuth ceremony and named credential metadata.
//! It never discovers or launches Codex and never reads another application's
//! credential store.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::config::{CredentialLifecycle, CredentialProvider, ProviderCredential};
use crate::oauth::file_store::FileOAuthCredentialStore;
use crate::oauth::{OAuthClient, OAuthCredentialStore, OAuthDialect, OAuthTokenRecord};
use crate::providers::chatgpt_oauth::OpenAiChatGptOAuthDialect;
use chrono::Utc;

/// Default descriptor-anchored Finch store. No foreign application path is
/// consulted or migrated implicitly.
pub fn default_chatgpt_oauth_store_root() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("Could not determine the Finch home directory for ChatGPT login")?
        .join(".finch")
        .join("oauth"))
}

/// Secret-free status suitable for script output, Debug, and restart checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptAuthStatus {
    pub credential_ref: String,
    pub account: Option<String>,
    pub lifecycle: CredentialLifecycle,
}

/// User-selected presentation actions. Opening a browser is never implicit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceLoginPresentation {
    pub copy_code: bool,
    pub open_browser: bool,
}

/// Setup-facing named-account boundary. Production and deterministic setup
/// tests consume the same contract without exposing token records.
#[async_trait]
pub trait ChatGptCredentialAuthenticator: Send + Sync {
    async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<ProviderCredential>;
}

/// Production Finch-native ChatGPT authentication service.
pub struct ChatGptAuthService {
    store: Arc<FileOAuthCredentialStore>,
}

impl std::fmt::Debug for ChatGptAuthService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ChatGptAuthService([REDACTED CREDENTIAL STORE])")
    }
}

impl ChatGptAuthService {
    pub fn production() -> Result<Self> {
        Ok(Self {
            store: Arc::new(FileOAuthCredentialStore::new(
                default_chatgpt_oauth_store_root()?,
            )),
        })
    }

    fn client(
        &self,
    ) -> Result<
        OAuthClient<
            OpenAiChatGptOAuthDialect<crate::providers::openai_jwks::OpenAiJwksVerifier>,
            FileOAuthCredentialStore,
        >,
    > {
        OAuthClient::new(
            Arc::new(OpenAiChatGptOAuthDialect::production()?),
            self.store.clone(),
        )
    }

    /// Read local status without refresh, HTTP, discovery, or provider construction.
    pub fn status(&self, reference: &str) -> Result<ChatGptAuthStatus> {
        status_from_record(reference, self.store.load_existing(reference)?)
    }

    /// Run the exact device lifecycle and return #174 metadata only after
    /// signed-token validation and crash-safe persistence.
    pub async fn login(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<ProviderCredential> {
        let client = self.client()?;
        login_device_with(&client, reference, presentation, cancel).await
    }

    /// Reuse a valid local named account, refresh the same account when
    /// necessary, or start explicit device login. No alternate reference or
    /// API-key fallback is consulted.
    pub async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<ProviderCredential> {
        match self.store.load(reference)? {
            Some(record)
                if record.provider == CredentialProvider::ChatgptSubscription
                    && !record.revoked
                    && !record.mutation_pending
                    && record.expires_at > Utc::now() =>
            {
                Ok(record.provider_credential(reference))
            }
            Some(record)
                if record.provider == CredentialProvider::ChatgptSubscription
                    && !record.revoked
                    && !record.mutation_pending
                    && record.refresh_token.is_some() =>
            {
                self.client()?.refresh(reference, cancel).await
            }
            Some(record) if record.provider != CredentialProvider::ChatgptSubscription => {
                bail!("named credential belongs to a different provider")
            }
            Some(record) if record.mutation_pending => bail!(
                "ChatGPT credential has an interrupted mutation; recover or revoke it before signing in again"
            ),
            Some(record) if !record.revoked => bail!(
                "ChatGPT credential is expired and unrefreshable; revoke it before explicit re-authentication"
            ),
            _ => self.login(reference, presentation, cancel).await,
        }
    }

    /// Revoke remotely and persist a local tombstone. No alternate account or
    /// Platform API-key fallback is attempted.
    pub async fn logout(
        &self,
        reference: &str,
        cancel: CancellationToken,
    ) -> Result<ProviderCredential> {
        if cancel.is_cancelled() {
            bail!("ChatGPT logout was cancelled before revocation");
        }
        self.client()?.revoke(reference, cancel).await
    }
}

#[async_trait]
impl ChatGptCredentialAuthenticator for ChatGptAuthService {
    async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<ProviderCredential> {
        ChatGptAuthService::ensure_named_credential(self, reference, presentation, cancel).await
    }
}

/// Shared device ceremony used by setup and scriptable login. Tests inject the
/// same OAuth production boundary with deterministic dialect/server fixtures.
pub async fn login_device_with<D, S>(
    client: &OAuthClient<D, S>,
    reference: &str,
    presentation: DeviceLoginPresentation,
    cancel: CancellationToken,
) -> Result<ProviderCredential>
where
    D: OAuthDialect + 'static,
    S: OAuthCredentialStore + 'static,
{
    let pending = client
        .begin_device_authorization_cancellable(cancel.clone())
        .await
        .context("ChatGPT device login could not start")?;
    present_device_authorization(
        &pending.verification_uri,
        &pending.user_code,
        pending.expires_in,
        presentation,
    )?;

    let countdown_cancel = cancel.child_token();
    let countdown = tokio::spawn(countdown_status(
        pending.expires_in,
        countdown_cancel.clone(),
    ));
    let result = client
        .finish_device_authorization(reference, &pending, cancel)
        .await
        .context("ChatGPT device login did not complete");
    countdown_cancel.cancel();
    let _ = countdown.await;
    result
}

fn status_from_store(
    store: &impl OAuthCredentialStore,
    reference: &str,
) -> Result<ChatGptAuthStatus> {
    status_from_record(reference, store.load(reference)?)
}

fn status_from_record(
    reference: &str,
    record: Option<OAuthTokenRecord>,
) -> Result<ChatGptAuthStatus> {
    Ok(match record {
        Some(record) if record.provider == CredentialProvider::ChatgptSubscription => {
            ChatGptAuthStatus {
                credential_ref: reference.to_string(),
                account: Some(record.account),
                lifecycle: if record.revoked || record.mutation_pending {
                    CredentialLifecycle::Revoked
                } else {
                    CredentialLifecycle::Active {
                        expires_at: Some(record.expires_at),
                        refreshable: record.refresh_token.is_some(),
                    }
                },
            }
        }
        Some(_) => bail!("named credential belongs to a different provider"),
        None => ChatGptAuthStatus {
            credential_ref: reference.to_string(),
            account: None,
            lifecycle: CredentialLifecycle::Revoked,
        },
    })
}

fn present_device_authorization(
    verification_uri: &str,
    user_code: &str,
    expires_in: Duration,
    presentation: DeviceLoginPresentation,
) -> Result<()> {
    println!("ChatGPT sign-in URL: {verification_uri}");
    println!("One-time code: {user_code}");
    println!(
        "This code expires in {} minutes. Press Ctrl+C to cancel.",
        expires_in.as_secs().div_ceil(60)
    );
    io::stdout().flush()?;
    if presentation.copy_code {
        let mut clipboard = arboard::Clipboard::new()
            .context("Could not access the clipboard; copy the displayed one-time code")?;
        clipboard
            .set_text(user_code.to_string())
            .context("Could not copy the one-time code; copy it from the terminal")?;
        println!("One-time code copied to the clipboard.");
    }
    if presentation.open_browser {
        open_browser(verification_uri)?;
        println!("Opened the ChatGPT sign-in page in the default browser.");
    }
    Ok(())
}

async fn countdown_status(lifetime: Duration, cancel: CancellationToken) {
    let deadline = tokio::time::Instant::now() + lifetime;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return;
                }
                eprintln!("Waiting for ChatGPT sign-in ({} minutes remaining)…", remaining.as_secs().div_ceil(60));
            }
        }
    }
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
        status.context("Could not open a browser; use the displayed ChatGPT sign-in URL")?;
    if !status.success() {
        bail!("Browser opener failed; use the displayed ChatGPT sign-in URL");
    }
    Ok(())
}

/// Replace or append one secret-free named credential and save no token data
/// to config.toml.
pub fn save_named_credential(
    mut config: crate::config::Config,
    credential: ProviderCredential,
) -> Result<()> {
    let mut credentials = config.credentials().to_vec();
    credentials.retain(|existing| existing.name != credential.name);
    credentials.push(credential);
    config = config.with_credentials(credentials);
    config.save()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudienceBinding, CredentialKind, EndpointFamily};
    use chrono::{TimeDelta, Utc};
    use std::collections::BTreeSet;
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
            dialect_id: "openai_chatgpt_subscription".into(),
            protocol_revision: "pinned".into(),
            provider: CredentialProvider::ChatgptSubscription,
            kind: CredentialKind::OauthDevice,
            issuer: "openai-chatgpt".into(),
            audience: AudienceBinding::standard(EndpointFamily::ChatgptSubscription),
            client_id: "public-client".into(),
            account: "acct-redacted".into(),
            tenant: None,
            project: None,
            scopes: BTreeSet::from(["openid".into()]),
            access_token: "access-secret".into(),
            refresh_token: Some("refresh-secret".into()),
            id_token: Some("identity-secret".into()),
            expires_at: Utc::now() + TimeDelta::hours(1),
            generation: "generation".into(),
            revoked: false,
            mutation_pending: false,
        }
    }

    #[test]
    fn status_and_debug_are_local_secret_free_restart_projections() {
        let store = MemoryStore(Mutex::new(Some(record())));
        let status = status_from_store(&store, "chatgpt:work").unwrap();
        assert_eq!(status.account.as_deref(), Some("acct-redacted"));
        let rendered = format!("{status:?}");
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
        assert!(!rendered.contains("identity-secret"));
    }

    #[test]
    fn wrong_provider_status_fails_without_mutation() {
        let mut hostile = record();
        hostile.provider = CredentialProvider::OpenaiPlatform;
        let store = MemoryStore(Mutex::new(Some(hostile.clone())));
        assert!(status_from_store(&store, "chatgpt:work").is_err());
        assert_eq!(store.0.lock().unwrap().as_ref(), Some(&hostile));
    }
}
