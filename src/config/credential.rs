//! Audience-bound provider credentials.
//!
//! These records describe credentials used by LLM providers. They are
//! deliberately separate from Brain attachment/runner credentials.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Authentication mechanism represented by a named credential.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    ApiKey,
    Bearer,
    OauthDevice,
    OauthBrowserPkce,
    CloudIdentity,
    LocalSocket,
    None,
}

/// Provider/account namespace. Same-company products intentionally have
/// distinct values when their authentication contracts are incompatible.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CredentialProvider {
    Anthropic,
    OpenaiPlatform,
    ChatgptSubscription,
    Xai,
    GeminiAiStudio,
    GoogleVertex,
    Mistral,
    Groq,
}

impl CredentialProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenaiPlatform => "openai_platform",
            Self::ChatgptSubscription => "chatgpt_subscription",
            Self::Xai => "xai",
            Self::GeminiAiStudio => "gemini_ai_studio",
            Self::GoogleVertex => "google_vertex",
            Self::Mistral => "mistral",
            Self::Groq => "groq",
        }
    }
}

/// Normalized service family. Custom services additionally carry an exact
/// normalized origin in [`AudienceBinding::endpoint`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EndpointFamily {
    AnthropicApi,
    OpenaiPlatform,
    ChatgptSubscription,
    XaiApi,
    GeminiAiStudio,
    GoogleVertex,
    MistralApi,
    GroqApi,
    Custom,
}

/// Audience or endpoint-family binding persisted with a credential/profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudienceBinding {
    pub family: EndpointFamily,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

impl AudienceBinding {
    pub fn standard(family: EndpointFamily) -> Self {
        Self {
            family,
            endpoint: None,
        }
    }

    pub fn custom(endpoint: &str) -> Result<Self> {
        Ok(Self {
            family: EndpointFamily::Custom,
            endpoint: Some(normalize_origin(endpoint)?),
        })
    }

    fn normalized(&self) -> Result<Self> {
        match (self.family, self.endpoint.as_deref()) {
            (EndpointFamily::Custom, Some(endpoint)) => Self::custom(endpoint),
            (EndpointFamily::Custom, None) => {
                bail!("custom audience requires an explicit endpoint origin")
            }
            (_, Some(_)) => {
                bail!("standard audience family must not specify a substitute endpoint")
            }
            (_, None) => Ok(self.clone()),
        }
    }
}

/// Persisted lifecycle metadata. Refreshability is metadata, not permission
/// for a resolver to contact an issuer during validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CredentialLifecycle {
    Active {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<DateTime<Utc>>,
        #[serde(default)]
        refreshable: bool,
    },
    Revoked,
    LegacyAmbiguous,
}

impl Default for CredentialLifecycle {
    fn default() -> Self {
        Self::Active {
            expires_at: None,
            refreshable: false,
        }
    }
}

/// Secret-free, named provider credential metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderCredential {
    pub name: String,
    pub kind: CredentialKind,
    pub provider: CredentialProvider,
    pub issuer: String,
    pub audience: AudienceBinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default)]
    pub scopes: BTreeSet<String>,
    pub secret_ref: String,
    #[serde(default)]
    pub lifecycle: CredentialLifecycle,
}

/// Profile-side authentication contract. Central provider descriptors supply
/// the required provider/issuer/kinds/family; identity and scope constraints
/// are explicit per profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialBinding {
    pub credential_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<AudienceBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default)]
    pub required_scopes: BTreeSet<String>,
}

/// Secret bytes returned by an injected local credential resolver.
pub struct ResolvedSecret(String);

impl ResolvedSecret {
    pub fn new(secret: impl Into<String>) -> Result<Self> {
        let secret = secret.into();
        if secret.trim().is_empty() {
            bail!("credential resolver returned empty secret material");
        }
        Ok(Self(secret))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResolvedSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResolvedSecret([REDACTED])")
    }
}

/// Immutable, redaction-safe resolved credential handle.
#[derive(Debug)]
pub struct ResolvedCredential {
    pub credential_name: String,
    pub secret: ResolvedSecret,
}

/// Injected secret store boundary. Implementations must be local and must not
/// refresh, connect, spawn, or perform any other external action.
pub trait CredentialResolver: Send + Sync {
    fn resolve(&self, credential: &ProviderCredential) -> Result<ResolvedCredential>;
}

/// Production resolver for explicit `env:VARIABLE_NAME` opaque references.
/// Other secret stores can implement [`CredentialResolver`] without changing
/// provider construction or validation.
#[derive(Debug, Default)]
pub struct EnvironmentCredentialResolver;

impl CredentialResolver for EnvironmentCredentialResolver {
    fn resolve(&self, credential: &ProviderCredential) -> Result<ResolvedCredential> {
        let variable = credential.secret_ref.strip_prefix("env:").ok_or_else(|| {
            anyhow::anyhow!(
                "credential '{}' uses unsupported secret_ref; use env:VARIABLE or configure a credential store",
                credential.name
            )
        })?;
        if variable.is_empty()
            || !variable
                .chars()
                .all(|character| character == '_' || character.is_ascii_alphanumeric())
        {
            bail!(
                "credential '{}' has invalid environment secret reference",
                credential.name
            );
        }
        let value = std::env::var(variable).with_context(|| {
            format!(
                "credential '{}' secret is unavailable; set environment variable {}",
                credential.name, variable
            )
        })?;
        Ok(ResolvedCredential {
            credential_name: credential.name.clone(),
            secret: ResolvedSecret::new(value).with_context(|| {
                format!("credential '{}' could not be resolved", credential.name)
            })?,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderAuthDescriptor {
    pub provider: CredentialProvider,
    pub issuer: &'static str,
    pub kinds: &'static [CredentialKind],
    pub family: EndpointFamily,
    pub standard_origin: &'static str,
}

const API_KEY: &[CredentialKind] = &[CredentialKind::ApiKey];
const CHATGPT_SESSION: &[CredentialKind] = &[
    CredentialKind::OauthDevice,
    CredentialKind::OauthBrowserPkce,
    CredentialKind::Bearer,
];

pub(crate) fn descriptor(provider: CredentialProvider) -> ProviderAuthDescriptor {
    match provider {
        CredentialProvider::Anthropic => ProviderAuthDescriptor {
            provider,
            issuer: "anthropic",
            kinds: API_KEY,
            family: EndpointFamily::AnthropicApi,
            standard_origin: "https://api.anthropic.com",
        },
        CredentialProvider::OpenaiPlatform => ProviderAuthDescriptor {
            provider,
            issuer: "openai-platform",
            kinds: API_KEY,
            family: EndpointFamily::OpenaiPlatform,
            standard_origin: "https://api.openai.com",
        },
        CredentialProvider::ChatgptSubscription => ProviderAuthDescriptor {
            provider,
            issuer: "openai-chatgpt",
            kinds: CHATGPT_SESSION,
            family: EndpointFamily::ChatgptSubscription,
            standard_origin: "https://chatgpt.com",
        },
        CredentialProvider::Xai => ProviderAuthDescriptor {
            provider,
            issuer: "xai",
            kinds: API_KEY,
            family: EndpointFamily::XaiApi,
            standard_origin: "https://api.x.ai",
        },
        CredentialProvider::GeminiAiStudio => ProviderAuthDescriptor {
            provider,
            issuer: "google-ai-studio",
            kinds: API_KEY,
            family: EndpointFamily::GeminiAiStudio,
            standard_origin: "https://generativelanguage.googleapis.com",
        },
        CredentialProvider::GoogleVertex => ProviderAuthDescriptor {
            provider,
            issuer: "google-cloud",
            kinds: &[CredentialKind::CloudIdentity, CredentialKind::Bearer],
            family: EndpointFamily::GoogleVertex,
            standard_origin: "https://aiplatform.googleapis.com",
        },
        CredentialProvider::Mistral => ProviderAuthDescriptor {
            provider,
            issuer: "mistral",
            kinds: API_KEY,
            family: EndpointFamily::MistralApi,
            standard_origin: "https://api.mistral.ai",
        },
        CredentialProvider::Groq => ProviderAuthDescriptor {
            provider,
            issuer: "groq",
            kinds: API_KEY,
            family: EndpointFamily::GroqApi,
            standard_origin: "https://api.groq.com",
        },
    }
}

/// Normalize a configured base URL to its lowercase scheme/host/port origin.
pub fn normalize_origin(endpoint: &str) -> Result<String> {
    let url = reqwest::Url::parse(endpoint).context("endpoint must be an absolute HTTP(S) URL")?;
    if url.scheme() != "https" && url.scheme() != "http" {
        bail!("endpoint scheme must be http or https");
    }
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("endpoint origin must not contain credentials, query, or fragment");
    }
    let host = url
        .host_str()
        .context("endpoint must contain a host")?
        .to_ascii_lowercase();
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Ok(format!(
        "{}://{}{}",
        url.scheme().to_ascii_lowercase(),
        host,
        port
    ))
}

/// Determine the binding required by a provider profile and endpoint. A
/// non-standard endpoint is always custom; callers cannot label it as the
/// provider's standard audience.
pub fn required_audience(
    provider: CredentialProvider,
    endpoint: Option<&str>,
) -> Result<AudienceBinding> {
    let expected = descriptor(provider);
    let endpoint = endpoint.unwrap_or(expected.standard_origin);
    let actual = normalize_origin(endpoint)?;
    if actual == normalize_origin(expected.standard_origin)? {
        return Ok(AudienceBinding::standard(expected.family));
    }
    AudienceBinding::custom(&actual)
}

/// Validate all credential metadata and return a stable name index.
pub fn credential_index(
    credentials: &[ProviderCredential],
) -> Result<BTreeMap<&str, &ProviderCredential>> {
    let mut index = BTreeMap::new();
    for credential in credentials {
        if credential.name.trim().is_empty() {
            bail!("credential name must not be empty");
        }
        if credential.secret_ref.trim().is_empty() {
            bail!("credential '{}' has empty secret_ref", credential.name);
        }
        credential
            .audience
            .normalized()
            .with_context(|| format!("credential '{}' has invalid audience", credential.name))?;
        if index.insert(credential.name.as_str(), credential).is_some() {
            bail!("duplicate credential name '{}'", credential.name);
        }
    }
    Ok(index)
}

/// Validate a profile reference against one named credential without resolving
/// secret material or performing external activity.
pub fn validate_binding(
    provider: CredentialProvider,
    endpoint: Option<&str>,
    binding: &CredentialBinding,
    credential: &ProviderCredential,
    now: DateTime<Utc>,
) -> Result<()> {
    let name = &binding.credential_ref;
    let expected = descriptor(provider);
    if credential.provider != expected.provider {
        bail!(
            "credential '{}' provider mismatch: profile requires {}, record is {}",
            name,
            expected.provider.as_str(),
            credential.provider.as_str()
        );
    }
    if !expected.kinds.contains(&credential.kind) {
        bail!(
            "credential '{}' kind mismatch: {:?} is not accepted by {}",
            name,
            credential.kind,
            expected.provider.as_str()
        );
    }
    if credential.issuer != expected.issuer {
        bail!(
            "credential '{}' issuer mismatch for {}",
            name,
            expected.provider.as_str()
        );
    }
    let required = required_audience(provider, endpoint)?;
    if let Some(declared) = &binding.audience {
        if declared.normalized()? != required {
            bail!(
                "credential '{}' profile audience does not match its normalized endpoint",
                name
            );
        }
    }
    if credential.audience.normalized()? != required {
        bail!(
            "credential '{}' audience mismatch for {} endpoint",
            name,
            expected.provider.as_str()
        );
    }
    for (field, required, actual) in [
        (
            "tenant",
            binding.tenant.as_deref(),
            credential.tenant.as_deref(),
        ),
        (
            "project",
            binding.project.as_deref(),
            credential.project.as_deref(),
        ),
        (
            "account",
            binding.account.as_deref(),
            credential.account.as_deref(),
        ),
    ] {
        if required.is_some() && required != actual {
            bail!("credential '{}' {} mismatch", name, field);
        }
    }
    if !binding.required_scopes.is_subset(&credential.scopes) {
        bail!("credential '{}' has insufficient scopes", name);
    }
    match &credential.lifecycle {
        CredentialLifecycle::Revoked => bail!("credential '{}' is revoked; choose or configure another named credential", name),
        CredentialLifecycle::LegacyAmbiguous => bail!("credential '{}' is an ambiguous legacy record; run `finch setup` and explicitly classify its provider, kind, issuer, and audience", name),
        CredentialLifecycle::Active { expires_at: Some(expires_at), refreshable: false } if *expires_at <= now => {
            bail!("credential '{}' is expired and cannot be refreshed", name)
        }
        CredentialLifecycle::Active { .. } => {}
    }
    Ok(())
}

/// Names of profiles that depend on a credential, for revoke/delete UX.
pub fn credential_dependencies<'a>(
    credential_name: &str,
    profiles: impl IntoIterator<Item = (&'a str, Option<&'a CredentialBinding>)>,
) -> Vec<String> {
    profiles
        .into_iter()
        .filter_map(|(profile, binding)| {
            (binding.is_some_and(|binding| binding.credential_ref == credential_name))
                .then(|| profile.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential(provider: CredentialProvider, kind: CredentialKind) -> ProviderCredential {
        let descriptor = descriptor(provider);
        ProviderCredential {
            name: "work".into(),
            kind,
            provider,
            issuer: descriptor.issuer.into(),
            audience: AudienceBinding::standard(descriptor.family),
            tenant: None,
            project: None,
            account: Some("account-1".into()),
            scopes: BTreeSet::new(),
            secret_ref: "env:FINCH_TEST_CREDENTIAL".into(),
            lifecycle: CredentialLifecycle::default(),
        }
    }

    fn binding() -> CredentialBinding {
        CredentialBinding {
            credential_ref: "work".into(),
            audience: None,
            tenant: None,
            project: None,
            account: None,
            required_scopes: BTreeSet::new(),
        }
    }

    #[test]
    fn test_provider_kind_audience_matrix() {
        let cases = [
            (CredentialProvider::Anthropic, CredentialKind::ApiKey, true),
            (
                CredentialProvider::OpenaiPlatform,
                CredentialKind::ApiKey,
                true,
            ),
            (
                CredentialProvider::ChatgptSubscription,
                CredentialKind::OauthDevice,
                true,
            ),
            (CredentialProvider::Xai, CredentialKind::Bearer, false),
            (
                CredentialProvider::GeminiAiStudio,
                CredentialKind::OauthDevice,
                false,
            ),
            (
                CredentialProvider::GoogleVertex,
                CredentialKind::CloudIdentity,
                true,
            ),
            (CredentialProvider::Mistral, CredentialKind::ApiKey, true),
            (CredentialProvider::Groq, CredentialKind::LocalSocket, false),
        ];
        for (provider, kind, accepted) in cases {
            let credential = credential(provider, kind);
            assert_eq!(
                validate_binding(provider, None, &binding(), &credential, Utc::now()).is_ok(),
                accepted,
                "provider={provider:?} kind={kind:?}"
            );
        }
    }

    #[test]
    fn test_openai_platform_and_chatgpt_subscription_never_cross_bind() {
        let platform = credential(CredentialProvider::OpenaiPlatform, CredentialKind::ApiKey);
        let subscription = credential(
            CredentialProvider::ChatgptSubscription,
            CredentialKind::OauthDevice,
        );
        assert!(validate_binding(
            CredentialProvider::ChatgptSubscription,
            None,
            &binding(),
            &platform,
            Utc::now()
        )
        .unwrap_err()
        .to_string()
        .contains("provider mismatch"));
        assert!(validate_binding(
            CredentialProvider::OpenaiPlatform,
            None,
            &binding(),
            &subscription,
            Utc::now()
        )
        .is_err());
    }

    #[test]
    fn test_standard_endpoint_normalization_and_custom_substitution() {
        let platform = credential(CredentialProvider::OpenaiPlatform, CredentialKind::ApiKey);
        assert!(validate_binding(
            CredentialProvider::OpenaiPlatform,
            Some("https://API.OPENAI.COM/v1/"),
            &binding(),
            &platform,
            Utc::now()
        )
        .is_ok());
        assert!(validate_binding(
            CredentialProvider::OpenaiPlatform,
            Some("https://compatible.example/v1"),
            &binding(),
            &platform,
            Utc::now()
        )
        .unwrap_err()
        .to_string()
        .contains("audience mismatch"));

        let mut custom = platform;
        custom.audience = AudienceBinding::custom("https://COMPATIBLE.example/v1").unwrap();
        assert!(validate_binding(
            CredentialProvider::OpenaiPlatform,
            Some("https://compatible.example/other/path"),
            &binding(),
            &custom,
            Utc::now()
        )
        .is_ok());
        assert!(validate_binding(
            CredentialProvider::OpenaiPlatform,
            Some("https://other.example/v1"),
            &binding(),
            &custom,
            Utc::now()
        )
        .is_err());
    }

    #[test]
    fn test_identity_scopes_and_lifecycle_rejections_are_field_specific() {
        let mut value = credential(
            CredentialProvider::GoogleVertex,
            CredentialKind::CloudIdentity,
        );
        value.tenant = Some("tenant-a".into());
        value.project = Some("project-a".into());
        value.scopes.insert("models.read".into());
        let mut expected = binding();
        expected.tenant = Some("tenant-b".into());
        assert!(validate_binding(
            CredentialProvider::GoogleVertex,
            None,
            &expected,
            &value,
            Utc::now()
        )
        .unwrap_err()
        .to_string()
        .contains("tenant mismatch"));
        expected.tenant = Some("tenant-a".into());
        expected.required_scopes.insert("models.write".into());
        assert!(validate_binding(
            CredentialProvider::GoogleVertex,
            None,
            &expected,
            &value,
            Utc::now()
        )
        .unwrap_err()
        .to_string()
        .contains("insufficient scopes"));
        expected.required_scopes.clear();
        value.lifecycle = CredentialLifecycle::Revoked;
        assert!(validate_binding(
            CredentialProvider::GoogleVertex,
            None,
            &expected,
            &value,
            Utc::now()
        )
        .unwrap_err()
        .to_string()
        .contains("revoked"));
        value.lifecycle = CredentialLifecycle::LegacyAmbiguous;
        assert!(validate_binding(
            CredentialProvider::GoogleVertex,
            None,
            &expected,
            &value,
            Utc::now()
        )
        .unwrap_err()
        .to_string()
        .contains("explicitly classify"));
        value.lifecycle = CredentialLifecycle::Active {
            expires_at: Some(Utc::now() - chrono::Duration::seconds(1)),
            refreshable: false,
        };
        assert!(validate_binding(
            CredentialProvider::GoogleVertex,
            None,
            &expected,
            &value,
            Utc::now()
        )
        .unwrap_err()
        .to_string()
        .contains("cannot be refreshed"));
    }

    #[test]
    fn test_secret_handle_debug_and_metadata_serialization_are_redacted() {
        let secret = "super-secret-marker";
        let handle = ResolvedCredential {
            credential_name: "work".into(),
            secret: ResolvedSecret::new(secret).unwrap(),
        };
        assert!(!format!("{handle:?}").contains(secret));
        let encoded = toml::to_string(&credential(
            CredentialProvider::OpenaiPlatform,
            CredentialKind::ApiKey,
        ))
        .unwrap();
        assert!(!encoded.contains(secret));
        assert!(encoded.contains("secret_ref"));
    }

    #[test]
    fn test_dependency_enumeration_reports_every_shared_profile() {
        let shared = binding();
        assert_eq!(
            credential_dependencies(
                "work",
                [
                    ("primary", Some(&shared)),
                    ("tools", Some(&shared)),
                    ("local", None)
                ]
            ),
            vec!["primary", "tools"]
        );
    }
}
