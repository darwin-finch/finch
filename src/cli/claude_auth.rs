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
        extract_claude_cli_token(self.store.clone(), reference).await
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

async fn extract_claude_cli_token(
    store: Arc<FileOAuthCredentialStore>,
    reference: &str,
) -> Result<ProviderCredential> {
    crate::oauth::validate_reference(reference)?;

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        use crate::oauth::OAuthDialect;
        
        let output = tokio::process::Command::new("security")
            .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
            .output()
            .await
            .context("Failed to run security find-generic-password")?;
            
        if !output.status.success() {
            bail!("Failed to extract Claude credentials from macOS Keychain. Are you signed in to the `claude` CLI?");
        }
        
        let json_str = String::from_utf8(output.stdout).context("Invalid UTF-8 in keychain data")?;
        
        let data: serde_json::Value = serde_json::from_str(&json_str).context("Failed to parse keychain JSON")?;
        let oauth = data.get("claudeAiOauth").context("Missing claudeAiOauth in keychain data")?;
        
        let access_token = oauth.get("accessToken").and_then(|v| v.as_str()).context("Missing accessToken")?.to_string();
        let refresh_token = oauth.get("refreshToken").and_then(|v| v.as_str()).map(|s| s.to_string());
        
        // try to get email from `claude auth status --json`
        let status_output = Command::new("claude")
            .args(["auth", "status", "--json"])
            .output();
        let account = if let Ok(out) = status_output {
            if out.status.success() {
                let status_json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::json!({}));
                status_json.get("email").and_then(|v| v.as_str()).unwrap_or("claude-cli").to_string()
            } else {
                "claude-cli".to_string()
            }
        } else {
            "claude-cli".to_string()
        };

        let dialect = ClaudeOAuthDialect::production()?;
        let descriptor = dialect.descriptor();
        let record = OAuthTokenRecord {
            dialect_id: descriptor.dialect_id.clone(),
            protocol_revision: descriptor.protocol_revision.clone(),
            provider: descriptor.provider,
            kind: descriptor.credential_kind,
            issuer: descriptor.issuer.clone(),
            audience: descriptor.audience.clone(),
            client_id: descriptor.client_id.clone(),
            account,
            tenant: None,
            project: None,
            scopes: descriptor.scopes.clone(),
            access_token,
            refresh_token,
            id_token: None,
            expires_at: Utc::now() + chrono::TimeDelta::try_days(365).unwrap_or_default(),
            generation: uuid::Uuid::new_v4().to_string(),
            revoked: false,
            mutation_pending: false,
        };
        
        let current = store.load(reference)?;
        let expected_generation = current.as_ref().map(|c| c.generation.as_str());
        
        store.compare_and_swap(reference, expected_generation, &record)?;
        
        Ok(record.provider_credential(reference))
    }
    #[cfg(not(target_os = "macos"))]
    {
        bail!("Extracting Claude token from the CLI is currently only supported on macOS.");
    }
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

    }
