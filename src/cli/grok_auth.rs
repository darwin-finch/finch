//! Finch-native SuperGrok device authentication UX.
//!
//! This module owns only local OAuth ceremony and named credential metadata.
//! It never discovers or launches grok-build and never reads another
//! application's credential store.

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
use crate::providers::XaiGrokOAuthDialect;
use chrono::Utc;

/// Default descriptor-anchored Finch store. No foreign application path is
/// consulted or migrated implicitly.
pub fn default_grok_oauth_store_root() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("Could not determine the Finch home directory for Grok login")?
        .join(".finch")
        .join("oauth"))
}

/// Secret-free status suitable for script output, Debug, and restart checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrokAuthStatus {
    pub credential_ref: String,
    pub account: Option<String>,
    pub state: GrokAuthState,
}

/// Local OAuth status without conflating an interrupted durable mutation with
/// an intentional signed-out tombstone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokAuthState {
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
pub struct GrokCompensationHandle {
    reference: String,
    generation: String,
}

impl std::fmt::Debug for GrokCompensationHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrokCompensationHandle")
            .field("reference", &self.reference)
            .field("generation", &"[REDACTED GENERATION]")
            .finish()
    }
}

impl GrokCompensationHandle {
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
pub struct EnsuredGrokCredential {
    pub credential: ProviderCredential,
    pub compensation: Option<GrokCompensationHandle>,
}

/// First phase of the named-credential ceremony.
#[derive(Debug, Clone)]
pub enum GrokNamedCredentialStart {
    Ensured(EnsuredGrokCredential),
    AuthorizationRequired(DeviceAuthorization),
}

/// Render one stable, secret-free status line for scripts and interactive use.
pub fn render_status_line(status: &GrokAuthStatus) -> Result<String> {
    crate::oauth::validate_reference(&status.credential_ref)?;
    if let Some(account) = status.account.as_deref() {
        validate_terminal_identifier(account, "Grok account identifier")?;
    }
    Ok(match &status.state {
        GrokAuthState::Active {
            expires_at,
            refreshable,
        } => format!(
            "grok-sub credential={} status=active account={} expires_at={} refreshable={}",
            status.credential_ref,
            status.account.as_deref().unwrap_or("unknown"),
            expires_at.to_rfc3339(),
            refreshable
        ),
        GrokAuthState::Expired {
            expires_at,
            refreshable,
        } => format!(
            "grok-sub credential={} status=expired account={} expires_at={} refreshable={} action=login",
            status.credential_ref,
            status.account.as_deref().unwrap_or("unknown"),
            expires_at.to_rfc3339(),
            refreshable
        ),
        GrokAuthState::SignedOut => format!(
            "grok-sub credential={} status=signed_out",
            status.credential_ref
        ),
        GrokAuthState::RecoveryRequired => format!(
            "grok-sub credential={} status=recovery_required action=finch_auth_recover",
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

/// Setup-facing named-account boundary.
#[async_trait]
pub trait GrokCredentialAuthenticator: Send + Sync {
    fn compensate_with_tombstone(&self, _handle: &GrokCompensationHandle) -> Result<()> {
        Ok(())
    }

    async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<EnsuredGrokCredential>;

    async fn begin_named_credential(
        &self,
        _reference: &str,
        _cancel: CancellationToken,
    ) -> Result<GrokNamedCredentialStart> {
        bail!("this authenticator does not script the phased named-credential ceremony")
    }

    async fn finish_named_credential(
        &self,
        _reference: &str,
        _pending: &DeviceAuthorization,
        _cancel: CancellationToken,
    ) -> Result<EnsuredGrokCredential> {
        bail!("this authenticator does not script the phased named-credential ceremony")
    }
}

/// Production Finch-native SuperGrok authentication service.
pub struct GrokAuthService {
    store: Arc<FileOAuthCredentialStore>,
}

impl std::fmt::Debug for GrokAuthService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("GrokAuthService([REDACTED CREDENTIAL STORE])")
    }
}

impl GrokAuthService {
    pub fn production() -> Result<Self> {
        Ok(Self {
            store: Arc::new(FileOAuthCredentialStore::new(
                default_grok_oauth_store_root()?,
            )),
        })
    }

    fn client(
        &self,
    ) -> Result<
        OAuthClient<
            XaiGrokOAuthDialect<crate::providers::GrokJwksVerifier>,
            FileOAuthCredentialStore,
        >,
    > {
        OAuthClient::new(
            Arc::new(XaiGrokOAuthDialect::production()?),
            self.store.clone(),
        )
    }

    pub fn status(&self, reference: &str) -> Result<GrokAuthStatus> {
        let record = self.store.load_existing(reference)?;
        if let Some(record) = record.as_ref() {
            self.client()?.validate_existing_binding(record)?;
        }
        status_from_record(reference, record)
    }

    pub async fn login(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<ProviderCredential> {
        let client = self.client()?;
        login_device_with(&client, reference, presentation, cancel).await
    }

    pub async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<EnsuredGrokCredential> {
        match self
            .begin_named_credential(reference, cancel.clone())
            .await?
        {
            GrokNamedCredentialStart::Ensured(ensured) => Ok(ensured),
            GrokNamedCredentialStart::AuthorizationRequired(pending) => {
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

    pub async fn begin_named_credential(
        &self,
        reference: &str,
        cancel: CancellationToken,
    ) -> Result<GrokNamedCredentialStart> {
        let client = self.client()?;
        begin_named_credential_with(&client, self.store.as_ref(), reference, cancel).await
    }

    pub async fn finish_named_credential(
        &self,
        reference: &str,
        pending: &DeviceAuthorization,
        cancel: CancellationToken,
    ) -> Result<EnsuredGrokCredential> {
        let client = self.client()?;
        finish_device_login_with(&client, reference, pending, cancel).await
    }

    pub async fn logout(
        &self,
        reference: &str,
        cancel: CancellationToken,
    ) -> Result<ProviderCredential> {
        if cancel.is_cancelled() {
            bail!("Grok logout was cancelled before revocation");
        }
        self.client()?.revoke(reference, cancel).await
    }

    pub fn recover(&self, reference: &str) -> Result<ProviderCredential> {
        self.client()?.recover_interrupted_as_revoked(reference)
    }
}

#[async_trait]
impl GrokCredentialAuthenticator for GrokAuthService {
    fn compensate_with_tombstone(&self, handle: &GrokCompensationHandle) -> Result<()> {
        self.client()?
            .tombstone_local_generation(handle.reference(), handle.generation())?;
        Ok(())
    }

    async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: DeviceLoginPresentation,
        cancel: CancellationToken,
    ) -> Result<EnsuredGrokCredential> {
        GrokAuthService::ensure_named_credential(self, reference, presentation, cancel).await
    }

    async fn begin_named_credential(
        &self,
        reference: &str,
        cancel: CancellationToken,
    ) -> Result<GrokNamedCredentialStart> {
        GrokAuthService::begin_named_credential(self, reference, cancel).await
    }

    async fn finish_named_credential(
        &self,
        reference: &str,
        pending: &DeviceAuthorization,
        cancel: CancellationToken,
    ) -> Result<EnsuredGrokCredential> {
        GrokAuthService::finish_named_credential(self, reference, pending, cancel).await
    }
}

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
        .context("Grok device login could not start")
}

async fn finish_device_login_with<D, S>(
    client: &OAuthClient<D, S>,
    reference: &str,
    pending: &DeviceAuthorization,
    cancel: CancellationToken,
) -> Result<EnsuredGrokCredential>
where
    D: OAuthDialect + 'static,
    S: OAuthCredentialStore + 'static,
{
    let commit = client
        .finish_device_authorization_commit(reference, pending, cancel)
        .await
        .context("Grok device login did not complete")?;
    Ok(EnsuredGrokCredential {
        credential: commit.credential,
        compensation: Some(GrokCompensationHandle::issued(reference, commit.generation)),
    })
}

async fn begin_named_credential_with<D, S>(
    client: &OAuthClient<D, S>,
    store: &S,
    reference: &str,
    cancel: CancellationToken,
) -> Result<GrokNamedCredentialStart>
where
    D: OAuthDialect + 'static,
    S: OAuthCredentialStore + 'static,
{
    if let Some(record) = store.load(reference)? {
        client.validate_existing_binding(&record)?;
        if record.mutation_pending {
            bail!(
                "Grok credential has an interrupted mutation; run `finch auth recover grok-sub --credential {reference}` before signing in again"
            );
        }
        if !record.revoked {
            if record.expires_at > Utc::now() {
                client.validate_active_reuse(&record)?;
                return Ok(GrokNamedCredentialStart::Ensured(EnsuredGrokCredential {
                    credential: record.provider_credential(reference),
                    compensation: None,
                }));
            }
            if record.refresh_token.is_some() {
                return Ok(GrokNamedCredentialStart::Ensured(EnsuredGrokCredential {
                    credential: client.refresh(reference, cancel).await?,
                    compensation: None,
                }));
            }
            bail!(
                "Grok credential is expired and unrefreshable; log it out before explicit re-authentication"
            );
        }
    }
    Ok(GrokNamedCredentialStart::AuthorizationRequired(
        begin_device_login_with(client, reference, cancel).await?,
    ))
}

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
    let pending = begin_device_login_with(client, reference, cancel.clone()).await?;
    present_device_authorization(
        &pending.verification_uri,
        &pending.user_code,
        pending.expires_in,
        presentation,
    )?;
    Ok(
        finish_device_login_with(client, reference, &pending, cancel)
            .await?
            .credential,
    )
}

fn status_from_record(reference: &str, record: Option<OAuthTokenRecord>) -> Result<GrokAuthStatus> {
    Ok(match record {
        Some(record) if record.provider == CredentialProvider::GrokSubscription => {
            validate_terminal_identifier(&record.account, "Grok account identifier")?;
            GrokAuthStatus {
                credential_ref: reference.to_string(),
                account: Some(record.account.clone()),
                state: if record.mutation_pending {
                    GrokAuthState::RecoveryRequired
                } else if record.revoked {
                    GrokAuthState::SignedOut
                } else if record.expires_at <= Utc::now() {
                    GrokAuthState::Expired {
                        expires_at: record.expires_at,
                        refreshable: record.refresh_token.is_some(),
                    }
                } else {
                    GrokAuthState::Active {
                        expires_at: record.expires_at,
                        refreshable: record.refresh_token.is_some(),
                    }
                },
            }
        }
        Some(_) => bail!("named credential belongs to a different provider"),
        None => GrokAuthStatus {
            credential_ref: reference.to_string(),
            account: None,
            state: GrokAuthState::SignedOut,
        },
    })
}

fn present_device_authorization(
    verification_uri: &str,
    user_code: &str,
    expires_in: Duration,
    presentation: DeviceLoginPresentation,
) -> Result<()> {
    println!("Grok subscription sign-in URL: {verification_uri}");
    println!("One-time code: {user_code}");
    println!(
        "This code expires in {} minutes. Press Ctrl+C to cancel.",
        expires_in.as_secs().div_ceil(60)
    );
    println!(
        "This uses SuperGrok / Grok Business entitlement. It is not an xAI Console API key and will not fall back to API billing."
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
        println!("Opened the Grok sign-in page in the default browser.");
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
                eprintln!("Waiting for Grok sign-in ({} minutes remaining)…", remaining.as_secs().div_ceil(60));
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
    let status = status.context("Could not open a browser; use the displayed Grok sign-in URL")?;
    if !status.success() {
        bail!("Browser opener failed; use the displayed Grok sign-in URL");
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
        "Grok token remains safely stored, but config metadata could not be saved; run `finch setup` to finish binding it",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudienceBinding, CredentialKind, EndpointFamily};
    use crate::providers::{GrokTokenVerifier, VerifiedGrokClaims, XAI_PUBLIC_CLIENT_ID};
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
            dialect_id: "xai_grok_subscription".into(),
            protocol_revision: crate::providers::GROK_OAUTH_PROTOCOL_REVISION.into(),
            provider: CredentialProvider::GrokSubscription,
            kind: CredentialKind::OauthDevice,
            issuer: "xai-grok".into(),
            audience: AudienceBinding::standard(EndpointFamily::GrokSubscription),
            client_id: XAI_PUBLIC_CLIENT_ID.into(),
            account: "acct-redacted".into(),
            tenant: None,
            project: None,
            scopes: crate::providers::grok_required_scopes(),
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
        let status = status_from_record("grok-sub:work", Some(record())).unwrap();
        assert_eq!(status.account.as_deref(), Some("acct-redacted"));
        let rendered = format!("{status:?}");
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
        let line = render_status_line(&status).unwrap();
        assert!(line.contains("grok-sub credential=grok-sub:work status=active"));
        assert!(!line.contains("access-secret"));
    }

    #[test]
    fn wrong_provider_status_fails_without_treating_api_key_as_subscription() {
        let mut hostile = record();
        hostile.provider = CredentialProvider::Xai;
        assert!(status_from_record("grok-sub:work", Some(hostile)).is_err());
    }

    struct UnreachableVerifier;

    #[async_trait]
    impl GrokTokenVerifier for UnreachableVerifier {
        fn preflight(&self) -> Result<()> {
            Ok(())
        }

        async fn verify(
            &self,
            _id_token: Option<&str>,
            _access_token: &str,
            _cancel: &CancellationToken,
        ) -> Result<VerifiedGrokClaims> {
            bail!("token verifier must remain unreachable")
        }
    }

    #[tokio::test]
    async fn begin_named_credential_reuses_active_record_without_device_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let dialect = Arc::new(
            XaiGrokOAuthDialect::for_test(&origin, Arc::new(UnreachableVerifier)).unwrap(),
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
            "grok-sub:work",
            CancellationToken::new(),
        )
        .await
        .expect("an active named credential must begin as ensured without any dialog");
        let GrokNamedCredentialStart::Ensured(ensured) = start else {
            panic!("expected the reuse path, got a device authorization requirement");
        };
        assert_eq!(ensured.credential.account.as_deref(), Some("acct-redacted"));
        assert!(ensured.compensation.is_none());
        let persisted = store.0.lock().unwrap();
        assert_eq!(persisted.as_ref().unwrap().generation, generation);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err(),
            "the reuse path must not open a device-authorization request"
        );
    }
}
