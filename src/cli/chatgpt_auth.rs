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

use crate::config::{CredentialProvider, ProviderCredential};
use crate::oauth::{
    DeviceAuthorization, FileOAuthCredentialStore, OAuthClient, OAuthCredentialStore, OAuthDialect,
    OAuthTokenRecord,
};
use crate::providers::OpenAiChatGptOAuthDialect;
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
    pub state: ChatGptAuthState,
}

/// Local OAuth status without conflating an interrupted durable mutation with
/// an intentional signed-out tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatGptAuthState {
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
pub struct ChatGptCompensationHandle {
    reference: String,
    generation: String,
}

impl std::fmt::Debug for ChatGptCompensationHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptCompensationHandle")
            .field("reference", &self.reference)
            .field("generation", &"[REDACTED GENERATION]")
            .finish()
    }
}

impl ChatGptCompensationHandle {
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
pub struct EnsuredChatGptCredential {
    pub credential: ProviderCredential,
    pub compensation: Option<ChatGptCompensationHandle>,
}

/// First phase of the named-credential ceremony: either the local account
/// already satisfied the request without user interaction (reused or
/// refreshed), or a device authorization must be shown to the user and
/// completed with `finish_named_credential`.
#[derive(Debug, Clone)]
pub enum ChatGptNamedCredentialStart {
    /// A validated named credential satisfied the request; no dialog is needed.
    Ensured(EnsuredChatGptCredential),
    /// Present the pending device authorization's one-time code, then commit.
    AuthorizationRequired(DeviceAuthorization),
}

/// Render one stable, secret-free status line for scripts and interactive use.
pub fn render_status_line(status: &ChatGptAuthStatus) -> Result<String> {
    crate::oauth::validate_reference(&status.credential_ref)?;
    if let Some(account) = status.account.as_deref() {
        validate_terminal_identifier(account, "ChatGPT account identifier")?;
    }
    Ok(match &status.state {
        ChatGptAuthState::Active {
            expires_at,
            refreshable,
        } => format!(
            "chatgpt credential={} status=active account={} expires_at={} refreshable={}",
            status.credential_ref,
            status.account.as_deref().unwrap_or("unknown"),
            expires_at.to_rfc3339(),
            refreshable
        ),
        ChatGptAuthState::Expired {
            expires_at,
            refreshable,
        } => format!(
            "chatgpt credential={} status=expired account={} expires_at={} refreshable={} action=login",
            status.credential_ref,
            status.account.as_deref().unwrap_or("unknown"),
            expires_at.to_rfc3339(),
            refreshable
        ),
        ChatGptAuthState::SignedOut => format!(
            "chatgpt credential={} status=signed_out",
            status.credential_ref
        ),
        ChatGptAuthState::RecoveryRequired => format!(
            "chatgpt credential={} status=recovery_required action=finch_auth_recover",
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
    /// Compensate a newly-issued account after a multi-account setup failure.
    fn compensate_with_tombstone(&self, _handle: &ChatGptCompensationHandle) -> Result<()> {
        Ok(())
    }

    async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<EnsuredChatGptCredential>;

    /// Begin the named-credential ceremony: reuse or refresh a valid local
    /// account when one exists, or start a device authorization whose one-time
    /// code the caller presents (interactive setup dialogs, #424).
    ///
    /// The default returns an error so scripted fakes that only exercise the
    /// combined ceremony fail loudly instead of silently skipping a phase.
    async fn begin_named_credential(
        &self,
        _reference: &str,
        _cancel: CancellationToken,
    ) -> Result<ChatGptNamedCredentialStart> {
        bail!("this authenticator does not script the phased named-credential ceremony")
    }

    /// Complete a began ceremony: poll to one terminal outcome, persist the
    /// validated account, and return its compensation authority.
    async fn finish_named_credential(
        &self,
        _reference: &str,
        _pending: &DeviceAuthorization,
        _cancel: CancellationToken,
    ) -> Result<EnsuredChatGptCredential> {
        bail!("this authenticator does not script the phased named-credential ceremony")
    }
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
            OpenAiChatGptOAuthDialect<crate::providers::OpenAiJwksVerifier>,
            FileOAuthCredentialStore,
        >,
    > {
        OAuthClient::new(
            Arc::new(OpenAiChatGptOAuthDialect::production()?),
            self.store.clone(),
        )
    }

    /// Read local status without refresh, HTTP, or discovery. Existing records
    /// are checked against the production dialect before projection.
    pub fn status(&self, reference: &str) -> Result<ChatGptAuthStatus> {
        let record = self.store.load_existing(reference)?;
        if let Some(record) = record.as_ref() {
            self.client()?.validate_existing_binding(record)?;
        }
        status_from_record(reference, record)
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
    ) -> Result<EnsuredChatGptCredential> {
        match self
            .begin_named_credential(reference, cancel.clone())
            .await?
        {
            ChatGptNamedCredentialStart::Ensured(ensured) => Ok(ensured),
            ChatGptNamedCredentialStart::AuthorizationRequired(pending) => {
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
                let result = self
                    .finish_named_credential(reference, &pending, cancel)
                    .await;
                countdown_cancel.cancel();
                let _ = countdown.await;
                result
            }
        }
    }

    /// Begin the named-credential ceremony without presenting anything: reuse
    /// or refresh a valid local account when one exists, or start a device
    /// authorization for the caller to present (#424).
    pub async fn begin_named_credential(
        &self,
        reference: &str,
        cancel: CancellationToken,
    ) -> Result<ChatGptNamedCredentialStart> {
        let client = self.client()?;
        begin_named_credential_with(&client, self.store.as_ref(), reference, cancel).await
    }

    /// Complete a began ceremony: poll to one terminal outcome, persist the
    /// validated account, and return its compensation authority (#424).
    pub async fn finish_named_credential(
        &self,
        reference: &str,
        pending: &DeviceAuthorization,
        cancel: CancellationToken,
    ) -> Result<EnsuredChatGptCredential> {
        let client = self.client()?;
        finish_device_login_with(&client, reference, pending, cancel).await
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

    /// Resolve an interrupted refresh/revoke locally without contacting the
    /// provider, retaining a durable tombstone for explicit reauthentication.
    pub fn recover(&self, reference: &str) -> Result<ProviderCredential> {
        self.client()?.recover_interrupted_as_revoked(reference)
    }
}

#[async_trait]
impl ChatGptCredentialAuthenticator for ChatGptAuthService {
    fn compensate_with_tombstone(&self, handle: &ChatGptCompensationHandle) -> Result<()> {
        self.client()?
            .tombstone_local_generation(handle.reference(), handle.generation())?;
        Ok(())
    }

    async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<EnsuredChatGptCredential> {
        ChatGptAuthService::ensure_named_credential(self, reference, presentation, cancel).await
    }

    async fn begin_named_credential(
        &self,
        reference: &str,
        cancel: CancellationToken,
    ) -> Result<ChatGptNamedCredentialStart> {
        ChatGptAuthService::begin_named_credential(self, reference, cancel).await
    }

    async fn finish_named_credential(
        &self,
        reference: &str,
        pending: &DeviceAuthorization,
        cancel: CancellationToken,
    ) -> Result<EnsuredChatGptCredential> {
        ChatGptAuthService::finish_named_credential(self, reference, pending, cancel).await
    }
}

async fn login_device_with_commit<D, S>(
    client: &OAuthClient<D, S>,
    reference: &str,
    presentation: DeviceLoginPresentation,
    cancel: CancellationToken,
) -> Result<EnsuredChatGptCredential>
where
    D: OAuthDialect + 'static,
    S: OAuthCredentialStore + 'static,
{
    let pending = begin_device_login_with(client, reference, cancel.clone()).await?;
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
    let result = finish_device_login_with(client, reference, &pending, cancel).await;
    countdown_cancel.cancel();
    let _ = countdown.await;
    result
}

/// Validate the reference, reject any conflicting local record, and request
/// the device one-time code. Shared by the terminal and phased ceremonies.
async fn begin_device_login_with<D, S>(
    client: &OAuthClient<D, S>,
    reference: &str,
    cancel: CancellationToken,
) -> Result<DeviceAuthorization>
where
    D: OAuthDialect + 'static,
    S: OAuthCredentialStore + 'static,
{
    crate::oauth::validate_reference(reference)?;
    client.preflight_reauthentication(reference)?;
    client
        .begin_device_authorization_cancellable(cancel)
        .await
        .context("ChatGPT device login could not start")
}

/// Poll to one terminal outcome, persist the validated account, and project
/// its compensation authority. Shared by the terminal and phased ceremonies.
async fn finish_device_login_with<D, S>(
    client: &OAuthClient<D, S>,
    reference: &str,
    pending: &DeviceAuthorization,
    cancel: CancellationToken,
) -> Result<EnsuredChatGptCredential>
where
    D: OAuthDialect + 'static,
    S: OAuthCredentialStore + 'static,
{
    let commit = client
        .finish_device_authorization_commit(reference, pending, cancel)
        .await
        .context("ChatGPT device login did not complete")?;
    Ok(EnsuredChatGptCredential {
        credential: commit.credential,
        compensation: Some(ChatGptCompensationHandle::issued(
            reference,
            commit.generation,
        )),
    })
}

/// Phased named-credential begin over an explicit client and store. Tests
/// inject the same OAuth production boundary with deterministic fixtures.
async fn begin_named_credential_with<D, S>(
    client: &OAuthClient<D, S>,
    store: &S,
    reference: &str,
    cancel: CancellationToken,
) -> Result<ChatGptNamedCredentialStart>
where
    D: OAuthDialect + 'static,
    S: OAuthCredentialStore + 'static,
{
    if let Some(record) = store.load(reference)? {
        client.validate_existing_binding(&record)?;
        if record.mutation_pending {
            bail!(
                "ChatGPT credential has an interrupted mutation; run `finch auth recover chatgpt --credential {reference}` before signing in again"
            );
        }
        if !record.revoked {
            if record.expires_at > Utc::now() {
                client.validate_active_reuse(&record)?;
                return Ok(ChatGptNamedCredentialStart::Ensured(
                    EnsuredChatGptCredential {
                        credential: record.provider_credential(reference),
                        compensation: None,
                    },
                ));
            }
            if record.refresh_token.is_some() {
                return Ok(ChatGptNamedCredentialStart::Ensured(
                    EnsuredChatGptCredential {
                        credential: client.refresh(reference, cancel).await?,
                        compensation: None,
                    },
                ));
            }
            bail!(
                "ChatGPT credential is expired and unrefreshable; log it out before explicit re-authentication"
            );
        }
    }
    Ok(ChatGptNamedCredentialStart::AuthorizationRequired(
        begin_device_login_with(client, reference, cancel).await?,
    ))
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
    Ok(
        login_device_with_commit(client, reference, presentation, cancel)
            .await?
            .credential,
    )
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
            validate_terminal_identifier(&record.account, "ChatGPT account identifier")?;
            ChatGptAuthStatus {
                credential_ref: reference.to_string(),
                account: Some(record.account.clone()),
                state: if record.mutation_pending {
                    ChatGptAuthState::RecoveryRequired
                } else if record.revoked {
                    ChatGptAuthState::SignedOut
                } else if record.expires_at <= Utc::now() {
                    ChatGptAuthState::Expired {
                        expires_at: record.expires_at,
                        refreshable: record.refresh_token.is_some(),
                    }
                } else {
                    ChatGptAuthState::Active {
                        expires_at: record.expires_at,
                        refreshable: record.refresh_token.is_some(),
                    }
                },
            }
        }
        Some(_) => bail!("named credential belongs to a different provider"),
        None => ChatGptAuthStatus {
            credential_ref: reference.to_string(),
            account: None,
            state: ChatGptAuthState::SignedOut,
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
        "ChatGPT token remains safely stored, but config metadata could not be saved; run `finch auth status` and then `finch setup` to finish binding it",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudienceBinding, CredentialKind, EndpointFamily};
    use crate::providers::{OpenAiTokenVerifier, VerifiedOpenAiClaims};
    use chrono::{TimeDelta, Utc};
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryStore(Mutex<Option<OAuthTokenRecord>>);

    struct UnreachableVerifier;

    #[async_trait]
    impl OpenAiTokenVerifier for UnreachableVerifier {
        fn preflight(&self) -> Result<()> {
            Ok(())
        }

        async fn verify(
            &self,
            _id_token: Option<&str>,
            _access_token: &str,
            _cancel: &CancellationToken,
        ) -> Result<VerifiedOpenAiClaims> {
            bail!("token verifier must remain unreachable")
        }
    }

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
        let stored = store.0.lock().unwrap();
        let stored = stored.as_ref().unwrap();
        assert_eq!(stored.provider, CredentialProvider::OpenaiPlatform);
        assert_eq!(stored.access_token, "access-secret");
        assert_eq!(stored.generation, hostile.generation);
    }

    #[test]
    fn interrupted_mutation_restart_status_requires_recovery_without_secret_output() {
        let mut interrupted = record();
        interrupted.mutation_pending = true;
        let status = status_from_record("chatgpt:work", Some(interrupted)).unwrap();
        assert_eq!(status.state, ChatGptAuthState::RecoveryRequired);
        let rendered = format!("{status:?}");
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
        assert_eq!(
            render_status_line(&status).unwrap(),
            "chatgpt credential=chatgpt:work status=recovery_required action=finch_auth_recover"
        );
    }

    #[test]
    fn script_status_lines_distinguish_active_and_signed_out_without_secrets() {
        let active = status_from_record("chatgpt:work", Some(record())).unwrap();
        let active_line = render_status_line(&active).unwrap();
        assert!(active_line.starts_with(
            "chatgpt credential=chatgpt:work status=active account=acct-redacted expires_at="
        ));
        assert!(active_line.ends_with(" refreshable=true"));
        assert!(!active_line.contains("access-secret"));
        assert_eq!(
            render_status_line(&status_from_record("chatgpt:work", None).unwrap()).unwrap(),
            "chatgpt credential=chatgpt:work status=signed_out"
        );

        let mut expired = record();
        expired.expires_at = Utc::now() - TimeDelta::minutes(1);
        let expired = status_from_record("chatgpt:work", Some(expired)).unwrap();
        assert!(matches!(expired.state, ChatGptAuthState::Expired { .. }));
        assert!(render_status_line(&expired)
            .unwrap()
            .contains("status=expired"));

        for hostile in [
            "acct\nforged".to_string(),
            "acct\u{1b}[31m".to_string(),
            "x".repeat(257),
        ] {
            let mut record = record();
            record.account = hostile;
            assert!(status_from_record("chatgpt:work", Some(record)).is_err());
        }
    }

    #[test]
    fn config_save_failure_after_token_commit_is_actionable_and_keeps_token_record() {
        let stored = record();
        let store = MemoryStore(Mutex::new(Some(stored.clone())));
        let credential = stored.provider_credential("chatgpt:work");
        let error = save_named_credential_with(
            crate::config::Config::with_providers(vec![]),
            credential,
            |_| anyhow::bail!("read-only config sentinel"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("token remains safely stored"));
        assert!(error.contains("finch auth status"));
        let persisted = store.0.lock().unwrap();
        let persisted = persisted.as_ref().unwrap();
        assert_eq!(persisted.provider, stored.provider);
        assert_eq!(persisted.generation, stored.generation);
        assert_eq!(persisted.access_token, "access-secret");
        assert!(!error.contains("access-secret"));
    }

    #[tokio::test]
    async fn explicit_login_conflicts_fail_before_device_socket_or_store_mutation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        for defect in [
            "active", "provider", "dialect", "revision", "kind", "issuer", "audience", "client",
            "scope", "account", "pending",
        ] {
            let dialect = Arc::new(
                OpenAiChatGptOAuthDialect::for_test(&origin, Arc::new(UnreachableVerifier))
                    .unwrap(),
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
            match defect {
                "active" => {}
                "provider" => hostile.provider = CredentialProvider::OpenaiPlatform,
                "dialect" => hostile.dialect_id = "foreign".into(),
                "revision" => hostile.protocol_revision = "foreign".into(),
                "kind" => hostile.kind = CredentialKind::ApiKey,
                "issuer" => hostile.issuer = "foreign".into(),
                "audience" => {
                    hostile.audience = AudienceBinding::standard(EndpointFamily::OpenaiPlatform)
                }
                "client" => hostile.client_id = "foreign".into(),
                "scope" => hostile.scopes.clear(),
                "account" => hostile.account = "acct\nforged".into(),
                "pending" => hostile.mutation_pending = true,
                _ => unreachable!(),
            }
            let generation = hostile.generation.clone();
            let store = Arc::new(MemoryStore(Mutex::new(Some(hostile))));
            let client = OAuthClient::new(dialect, store.clone()).unwrap();
            assert!(login_device_with(
                &client,
                "chatgpt:work",
                DeviceLoginPresentation::default(),
                CancellationToken::new(),
            )
            .await
            .is_err());
            assert_eq!(
                store.0.lock().unwrap().as_ref().unwrap().generation,
                generation
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }

    fn aligned_active_record(
        descriptor: &crate::oauth::OAuthDialectDescriptor,
    ) -> OAuthTokenRecord {
        let mut aligned = record();
        aligned.dialect_id = descriptor.dialect_id.clone();
        aligned.protocol_revision = descriptor.protocol_revision.clone();
        aligned.provider = descriptor.provider;
        aligned.kind = descriptor.credential_kind;
        aligned.issuer = descriptor.issuer.clone();
        aligned.audience = descriptor.audience.clone();
        aligned.client_id = descriptor.client_id.clone();
        aligned.scopes = descriptor.scopes.clone();
        aligned
    }

    struct PhasedVerifier;

    #[async_trait]
    impl OpenAiTokenVerifier for PhasedVerifier {
        fn preflight(&self) -> Result<()> {
            Ok(())
        }

        async fn verify(
            &self,
            _id_token: Option<&str>,
            _access_token: &str,
            _cancel: &CancellationToken,
        ) -> Result<VerifiedOpenAiClaims> {
            Ok(VerifiedOpenAiClaims {
                issuer: crate::providers::REQUIRED_TOKEN_ISSUER.into(),
                audiences: BTreeSet::from([crate::providers::OPENAI_PUBLIC_CLIENT_ID.into()]),
                authorized_party: None,
                subject: "subject-phased".into(),
                account_id: Some("acct-phased".into()),
                chatgpt_plan_type: Some("plus".into()),
                account_is_fedramp: false,
                nonce: None,
                expires_at: Utc::now() + TimeDelta::hours(1),
                not_before: None,
            })
        }
    }

    #[derive(Default)]
    struct PhasedFlowState {
        starts: Mutex<Vec<String>>,
        polls: std::sync::atomic::AtomicUsize,
        exchanges: std::sync::atomic::AtomicUsize,
    }

    async fn phased_flow_handler(
        axum::extract::State(state): axum::extract::State<std::sync::Arc<PhasedFlowState>>,
        request: axum::extract::Request,
    ) -> axum::http::Response<axum::body::Body> {
        use base64::Engine;
        use sha2::Digest;
        let path = request.uri().path().to_string();
        let body = axum::body::to_bytes(request.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let (status, response) = match path.as_str() {
            "/api/accounts/deviceauth/usercode" => {
                state
                    .starts
                    .lock()
                    .unwrap()
                    .push(json["client_id"].as_str().unwrap_or("").to_string());
                (
                    axum::http::StatusCode::OK,
                    serde_json::json!({
                        "device_auth_id": "device-1",
                        "user_code": "CODE-1234",
                        "interval": "0"
                    }),
                )
            }
            "/api/accounts/deviceauth/token" => {
                state
                    .polls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let verifier = "phased-pkce-verifier";
                let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(sha2::Sha256::digest(verifier.as_bytes()));
                (
                    axum::http::StatusCode::OK,
                    serde_json::json!({
                        "authorization_code": "phased-authorization-code",
                        "code_verifier": verifier,
                        "code_challenge": challenge
                    }),
                )
            }
            "/oauth/token" => {
                state
                    .exchanges
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (
                    axum::http::StatusCode::OK,
                    serde_json::json!({
                        "access_token": "phased-access-secret",
                        "refresh_token": "phased-refresh-secret",
                        "id_token": "phased-id-secret",
                        "expires_in": 3600
                    }),
                )
            }
            _ => (
                axum::http::StatusCode::NOT_FOUND,
                serde_json::json!({"error": "unexpected-path"}),
            ),
        };
        axum::http::Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(response.to_string()))
            .unwrap()
    }

    struct PhasedFlowServer {
        origin: String,
        state: std::sync::Arc<PhasedFlowState>,
        task: tokio::task::JoinHandle<()>,
    }

    impl PhasedFlowServer {
        async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let state = std::sync::Arc::new(PhasedFlowState::default());
            let app = axum::Router::new()
                .fallback(axum::routing::post(phased_flow_handler))
                .with_state(state.clone());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                origin,
                state,
                task,
            }
        }
    }

    impl Drop for PhasedFlowServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    #[tokio::test]
    async fn begin_named_credential_reuses_active_record_without_device_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let dialect = Arc::new(
            OpenAiChatGptOAuthDialect::for_test(&origin, Arc::new(UnreachableVerifier)).unwrap(),
        );
        let aligned = aligned_active_record(dialect.descriptor());
        let generation = aligned.generation.clone();
        let store = Arc::new(MemoryStore(Mutex::new(Some(aligned))));
        let client = OAuthClient::new(dialect, store.clone()).unwrap();
        let start = begin_named_credential_with(
            &client,
            store.as_ref(),
            "chatgpt:work",
            CancellationToken::new(),
        )
        .await
        .expect("an active named credential must begin as ensured without any dialog");
        let ChatGptNamedCredentialStart::Ensured(ensured) = start else {
            panic!("expected the reuse path, got a device authorization requirement");
        };
        assert_eq!(
            ensured.credential.account.as_deref(),
            Some("acct-redacted"),
            "reuse must project the stored account; got {:?}",
            ensured.credential
        );
        assert!(
            ensured.compensation.is_none(),
            "reuse must not hand out rollback authority for an untouched record"
        );
        let persisted = store.0.lock().unwrap();
        let persisted = persisted.as_ref().unwrap();
        assert_eq!(
            persisted.generation, generation,
            "reuse must not rewrite the record"
        );
        assert!(!persisted.mutation_pending && !persisted.revoked);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err(),
            "the reuse path must not open a device-authorization request"
        );
    }

    #[tokio::test]
    async fn phased_named_credential_presents_code_then_persists_one_account_with_compensation() {
        let server = PhasedFlowServer::start().await;
        let dialect = Arc::new(
            OpenAiChatGptOAuthDialect::for_test(&server.origin, Arc::new(PhasedVerifier)).unwrap(),
        );
        let store = Arc::new(MemoryStore(Mutex::new(None)));
        let client = OAuthClient::new(dialect, store.clone()).unwrap();

        let start = begin_named_credential_with(
            &client,
            store.as_ref(),
            "chatgpt:work",
            CancellationToken::new(),
        )
        .await
        .expect("begin over an empty store must start a device authorization");
        let ChatGptNamedCredentialStart::AuthorizationRequired(pending) = start else {
            panic!("an absent record must require device authorization");
        };
        assert_eq!(pending.user_code, "CODE-1234", "the dialog shows this code");
        assert!(
            !pending.verification_uri.is_empty(),
            "the dialog must present a verification URL"
        );
        assert!(
            !pending.verification_uri.contains("CODE-1234"),
            "the verification URL must not embed the one-time code"
        );
        assert_eq!(server.state.starts.lock().unwrap().len(), 1);

        let ensured = finish_device_login_with(&client, "chatgpt:work", &pending, {
            CancellationToken::new()
        })
        .await
        .expect("the scripted provider approves the first poll");
        assert_eq!(
            ensured.credential.account.as_deref(),
            Some("acct-phased"),
            "the persisted account must come from the signed claims; got {:?}",
            ensured.credential
        );
        assert_eq!(ensured.credential.name, "chatgpt:work");
        let compensation = ensured
            .compensation
            .expect("a fresh issuance must return rollback authority");
        assert_eq!(compensation.reference(), "chatgpt:work");

        let persisted = store.0.lock().unwrap();
        let persisted = persisted
            .as_ref()
            .expect("exactly one record must be persisted");
        assert_eq!(persisted.account, "acct-phased");
        assert_eq!(persisted.access_token, "phased-access-secret");
        assert!(persisted.refresh_token.is_some());
        assert!(!persisted.revoked && !persisted.mutation_pending);
        assert_eq!(
            server.state.polls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the phased ceremony must poll to exactly one terminal outcome"
        );
        assert_eq!(
            server
                .state
                .exchanges
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn unscripted_named_credential_phases_return_errors_without_panicking() {
        struct UnscriptedAuthenticator;

        #[async_trait]
        impl ChatGptCredentialAuthenticator for UnscriptedAuthenticator {
            async fn ensure_named_credential(
                &self,
                _reference: &str,
                _presentation: DeviceLoginPresentation,
                _cancel: CancellationToken,
            ) -> Result<EnsuredChatGptCredential> {
                bail!("ensure is not scripted")
            }
        }

        let authenticator = UnscriptedAuthenticator;
        let begin = authenticator
            .begin_named_credential("chatgpt:work", CancellationToken::new())
            .await
            .expect_err("an unscripted phase must fail, not default to a real flow");
        assert!(
            begin
                .to_string()
                .contains("phased named-credential ceremony"),
            "the stub error must name the missing scripting; got {begin}"
        );
        let pending = DeviceAuthorization::issued(
            "device-secret".into(),
            "CODE-0000".into(),
            "https://auth.example/activate".into(),
            None,
            Duration::from_secs(600),
            Duration::from_secs(1),
        )
        .unwrap();
        let finish = authenticator
            .finish_named_credential("chatgpt:work", &pending, CancellationToken::new())
            .await
            .expect_err("an unscripted phase must fail, not default to a real flow");
        assert!(
            finish
                .to_string()
                .contains("phased named-credential ceremony"),
            "the stub error must name the missing scripting; got {finish}"
        );
    }
}
