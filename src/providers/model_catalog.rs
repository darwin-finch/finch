//! Authenticated provider model discovery with a secret-free local cache.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::endpoints::ProviderEndpoints;
use crate::config::{Config, CredentialProvider, CredentialResolver, ProviderEntry};

const CATALOG_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CATALOG_BODY_BYTES: usize = 1_048_576;
const MAX_MODEL_COUNT: usize = 4_096;
const MAX_MODEL_ID_BYTES: usize = 512;

/// Date on which Finch's bundled, deliberately incomplete model fallback was reviewed.
pub const STATIC_FALLBACK_AS_OF: &str = "2026-08-26";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogAuth {
    AnthropicApiKey,
    Bearer,
}

#[derive(Clone)]
pub struct ModelCatalogProfile {
    pub provider: String,
    /// Stable, non-secret configured profile/account label.
    pub profile_id: String,
    pub api_key: String,
    pub endpoints: ProviderEndpoints,
    pub auth: CatalogAuth,
    pub request_timeout: Duration,
}

impl std::fmt::Debug for ModelCatalogProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelCatalogProfile")
            .field("provider", &self.provider)
            .field("profile_id", &self.profile_id)
            .field("api_key", &"[REDACTED]")
            .field("endpoints", &self.endpoints)
            .field("auth", &self.auth)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl ModelCatalogProfile {
    pub fn new(
        provider: impl Into<String>,
        profile_id: impl Into<String>,
        api_key: impl Into<String>,
        endpoints: ProviderEndpoints,
        auth: CatalogAuth,
    ) -> Self {
        Self {
            provider: provider.into(),
            profile_id: profile_id.into(),
            api_key: api_key.into(),
            endpoints,
            auth,
            request_timeout: CATALOG_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    Discovered,
    Cache,
    StaticFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCatalog {
    pub provider: String,
    pub profile_id: String,
    pub models_url: String,
    pub models: Vec<String>,
    pub source: CatalogSource,
    pub refreshed_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelRecord>,
}

#[derive(Debug, Deserialize)]
struct ModelRecord {
    id: String,
}

pub fn default_cache_dir() -> Result<PathBuf> {
    let home =
        dirs::home_dir().context("Cannot locate home directory for model catalogue cache")?;
    Ok(home.join(".finch").join("cache").join("model-catalogs"))
}

pub fn static_fallback(provider: &str) -> Vec<String> {
    let models: &[&str] = match provider {
        "claude" => &["claude-sonnet-5"],
        // Keep this deliberately short. Authenticated discovery is authoritative;
        // this offline snapshot only offers the three general-purpose API tiers
        // documented when STATIC_FALLBACK_AS_OF was reviewed.
        "openai" => &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"],
        "grok" => &["grok-4.6"],
        "gemini" => &["gemini-2.5-flash"],
        "mistral" => &["mistral-large-2512"],
        "groq" => &["openai/gpt-oss-120b"],
        _ => &[],
    };
    models.iter().map(|model| (*model).to_string()).collect()
}

pub fn fallback_catalog(provider: &str, models_url: &str) -> ModelCatalog {
    ModelCatalog {
        provider: provider.to_string(),
        profile_id: provider.to_string(),
        models_url: cache_safe_url(models_url),
        models: static_fallback(provider),
        source: CatalogSource::StaticFallback,
        refreshed_at: Utc::now(),
    }
}

/// Fetch a catalogue and persist only endpoint/model metadata. API keys are
/// used solely to construct request headers and never serialized or logged.
pub async fn refresh(profile: &ModelCatalogProfile, cache_dir: &Path) -> Result<ModelCatalog> {
    if profile.api_key.trim().is_empty() {
        bail!(
            "An API key is required to refresh {} models",
            profile.provider
        );
    }

    let client = Client::builder()
        .timeout(profile.request_timeout)
        .build()
        .context("Failed to create model catalogue HTTP client")?;
    let mut request = client.get(&profile.endpoints.models_url);
    request = match profile.auth {
        CatalogAuth::AnthropicApiKey => request
            .header("x-api-key", &profile.api_key)
            .header("anthropic-version", "2023-06-01"),
        CatalogAuth::Bearer => request.bearer_auth(&profile.api_key),
    };

    let origin = sanitized_origin(&profile.endpoints.models_url);
    let response = request.send().await.map_err(|_| {
        anyhow::anyhow!(
            "Could not reach {} model catalogue at {}",
            profile.provider,
            origin
        )
    })?;
    let status = response.status();
    if !status.is_success() {
        bail!(
            "{} model catalogue at {} returned HTTP {}",
            profile.provider,
            origin,
            status
        );
    }

    if response
        .content_length()
        .is_some_and(|length| length > MAX_CATALOG_BODY_BYTES as u64)
    {
        bail!(
            "{} model catalogue response exceeded size limit",
            profile.provider
        );
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            anyhow::anyhow!(
                "Could not read {} model catalogue response",
                profile.provider
            )
        })?;
        if body.len().saturating_add(chunk.len()) > MAX_CATALOG_BODY_BYTES {
            bail!(
                "{} model catalogue response exceeded size limit",
                profile.provider
            );
        }
        body.extend_from_slice(&chunk);
    }
    let payload: ModelsResponse = serde_json::from_slice(&body)
        .with_context(|| format!("Invalid {} model catalogue response", profile.provider))?;
    if payload.data.len() > MAX_MODEL_COUNT {
        bail!(
            "{} model catalogue exceeded model count limit",
            profile.provider
        );
    }
    let mut models: Vec<String> = payload
        .data
        .into_iter()
        .map(|record| record.id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    validate_models(&profile.provider, &models)?;
    models.sort();
    models.dedup();
    if models.is_empty() {
        bail!("{} returned an empty model catalogue", profile.provider);
    }

    let catalog = ModelCatalog {
        provider: profile.provider.clone(),
        profile_id: profile.profile_id.clone(),
        models_url: cache_safe_url(&profile.endpoints.models_url),
        models,
        source: CatalogSource::Discovered,
        refreshed_at: Utc::now(),
    };
    write_cache(&catalog, profile, cache_dir)?;
    Ok(catalog)
}

/// Revalidate and resolve a named provider profile immediately before model
/// discovery. Invalid graphs return before the HTTP client is constructed.
pub async fn refresh_from_config(
    config: &Config,
    profile_name: &str,
    resolver: &dyn CredentialResolver,
    cache_dir: &Path,
) -> Result<ModelCatalog> {
    config.validate()?;
    let entry = config
        .providers
        .iter()
        .find(|entry| entry.profile_name() == profile_name)
        .with_context(|| format!("provider profile '{profile_name}' was not found"))?;
    let ProviderEntry::Credentialed {
        provider,
        credential,
        base_url,
        chat_path,
        models_path,
        ..
    } = entry
    else {
        bail!("provider profile '{profile_name}' does not use a named credential");
    };
    let credentials = crate::config::credential::credential_index(&config.credentials)?;
    let metadata = credentials
        .get(credential.credential_ref.as_str())
        .expect("Config::validate checked the named credential reference");
    let resolved = resolver.resolve(metadata).map_err(|_| {
        anyhow::anyhow!("Failed to resolve named credential '{}'; inspect the credential store without printing secret material", credential.credential_ref)
    })?;
    if resolved.credential_name != credential.credential_ref {
        bail!("credential resolver returned a handle for the wrong named credential");
    }
    let (default_base, default_chat, default_models, auth, provider_name) = match provider {
        CredentialProvider::Anthropic => (
            "https://api.anthropic.com",
            "/v1/messages",
            "/v1/models",
            CatalogAuth::AnthropicApiKey,
            "claude",
        ),
        CredentialProvider::OpenaiPlatform => (
            "https://api.openai.com",
            "/v1/chat/completions",
            "/v1/models",
            CatalogAuth::Bearer,
            "openai",
        ),
        CredentialProvider::Xai => (
            "https://api.x.ai",
            "/v1/chat/completions",
            "/v1/models",
            CatalogAuth::Bearer,
            "grok",
        ),
        CredentialProvider::Mistral => (
            "https://api.mistral.ai",
            "/v1/chat/completions",
            "/v1/models",
            CatalogAuth::Bearer,
            "mistral",
        ),
        CredentialProvider::Groq => (
            "https://api.groq.com/openai",
            "/v1/chat/completions",
            "/v1/models",
            CatalogAuth::Bearer,
            "groq",
        ),
        _ => bail!(
            "provider profile '{profile_name}' does not have a supported named-credential catalogue transport"
        ),
    };
    let profile = ModelCatalogProfile::new(
        provider_name,
        profile_name,
        resolved.secret.expose(),
        ProviderEndpoints::new(
            base_url.as_deref().unwrap_or(default_base),
            chat_path.as_deref().unwrap_or(default_chat),
            models_path.as_deref().unwrap_or(default_models),
        ),
        auth,
    );
    refresh(&profile, cache_dir).await
}

/// Use a successful live refresh, then the matching cache, then a visibly
/// labelled static fallback. A failed refresh is returned alongside fallback
/// data so callers can tell the user discovery did not succeed.
pub async fn refresh_with_fallback(
    profile: &ModelCatalogProfile,
    cache_dir: &Path,
) -> (ModelCatalog, Option<String>) {
    match refresh(profile, cache_dir).await {
        Ok(catalog) => (catalog, None),
        Err(error) => {
            if let Ok(Some(cached)) = read_cache(profile, cache_dir) {
                (cached, Some(error.to_string()))
            } else {
                let mut fallback =
                    fallback_catalog(&profile.provider, &profile.endpoints.models_url);
                fallback.profile_id = profile.profile_id.clone();
                (fallback, Some(error.to_string()))
            }
        }
    }
}

pub fn read_cache(profile: &ModelCatalogProfile, cache_dir: &Path) -> Result<Option<ModelCatalog>> {
    let path = cache_path(profile, cache_dir);
    let contents = match std::fs::metadata(&path) {
        Ok(metadata) if metadata.len() > MAX_CATALOG_BODY_BYTES as u64 => {
            bail!("Model catalogue cache exceeded size limit")
        }
        Ok(_) => std::fs::read(&path)
            .with_context(|| "Failed to read model catalogue cache".to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Failed to inspect model catalogue cache"),
    };
    let mut catalog: ModelCatalog =
        serde_json::from_slice(&contents).context("Invalid model catalogue cache")?;
    if catalog.provider != profile.provider
        || catalog.profile_id != profile.profile_id
        || catalog.models_url != cache_safe_url(&profile.endpoints.models_url)
    {
        return Ok(None);
    }
    validate_models(&catalog.provider, &catalog.models)?;
    catalog.source = CatalogSource::Cache;
    Ok(Some(catalog))
}

fn write_cache(
    catalog: &ModelCatalog,
    profile: &ModelCatalogProfile,
    cache_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("Failed to create {}", cache_dir.display()))?;
    let path = cache_path(profile, cache_dir);
    let json =
        serde_json::to_vec_pretty(catalog).context("Failed to encode model catalogue cache")?;
    std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))
}

fn cache_path(profile: &ModelCatalogProfile, cache_dir: &Path) -> PathBuf {
    cache_dir.join(format!("catalog-{}.json", profile_cache_identity(profile)))
}

fn cache_safe_url(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return "<configured-endpoint>".to_string();
    };
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string()
}

fn sanitized_origin(url: &str) -> String {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return "<configured-endpoint>".to_string();
    };
    let Some(host) = parsed.host_str() else {
        return "<configured-endpoint>".to_string();
    };
    match parsed.port() {
        Some(port) => format!("{}://{}:{}", parsed.scheme(), host, port),
        None => format!("{}://{}", parsed.scheme(), host),
    }
}

/// Opaque full-width cache/request identity. Credential material participates
/// in the digest but is never persisted or returned directly.
pub fn profile_cache_identity(profile: &ModelCatalogProfile) -> String {
    let mut hasher = Sha256::new();
    let normalized_private_url = reqwest::Url::parse(&profile.endpoints.models_url)
        .map(|mut url| {
            url.set_fragment(None);
            url.to_string()
        })
        .unwrap_or_else(|_| profile.endpoints.models_url.clone());
    for component in [
        profile.provider.as_bytes(),
        profile.profile_id.as_bytes(),
        cache_safe_url(&profile.endpoints.models_url).as_bytes(),
        profile.api_key.as_bytes(),
        normalized_private_url.as_bytes(),
    ] {
        hasher.update((component.len() as u64).to_be_bytes());
        hasher.update(component);
    }
    format!("{:x}", hasher.finalize())
}

fn validate_models(provider: &str, models: &[String]) -> Result<()> {
    if models.len() > MAX_MODEL_COUNT {
        bail!("{} model catalogue exceeded model count limit", provider);
    }
    if models.iter().any(|id| id.len() > MAX_MODEL_ID_BYTES) {
        bail!(
            "{} model catalogue contained an oversized model ID",
            provider
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AudienceBinding, CredentialBinding, CredentialKind, CredentialLifecycle, EndpointFamily,
        ProviderCredential, ResolvedCredential, ResolvedSecret,
    };
    use mockito::Matcher;
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingResolver(AtomicUsize);

    impl CredentialResolver for CountingResolver {
        fn resolve(&self, credential: &ProviderCredential) -> Result<ResolvedCredential> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ResolvedCredential {
                credential_name: credential.name.clone(),
                secret: ResolvedSecret::new("not-sent")?,
            })
        }
    }

    fn profile(server_url: &str, provider: &str, auth: CatalogAuth) -> ModelCatalogProfile {
        ModelCatalogProfile::new(
            provider,
            format!("{provider}-work"),
            "secret-that-must-not-be-cached",
            ProviderEndpoints::new(server_url, "/v1/chat/completions", "/custom/catalog/models"),
            auth,
        )
    }

    #[tokio::test]
    async fn rejected_catalog_binding_has_zero_resolution_or_socket_activity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let profile = ProviderEntry::Credentialed {
            provider: CredentialProvider::OpenaiPlatform,
            credential: CredentialBinding {
                credential_ref: "work".into(),
                audience: None,
                tenant: None,
                project: None,
                account: None,
                required_scopes: BTreeSet::new(),
            },
            model: Some("gpt-4o".into()),
            base_url: None,
            chat_path: None,
            models_path: Some(format!("{endpoint}/models")),
            name: Some("primary".into()),
            reasoning_effort: None,
        };
        let credential = ProviderCredential {
            name: "work".into(),
            kind: CredentialKind::ApiKey,
            provider: CredentialProvider::OpenaiPlatform,
            issuer: "openai-platform".into(),
            // Host substitution: the profile points at the listener while the
            // credential remains bound to the Platform audience.
            audience: AudienceBinding::standard(EndpointFamily::OpenaiPlatform),
            tenant: None,
            project: None,
            account: None,
            scopes: BTreeSet::new(),
            secret_ref: "test:work".into(),
            lifecycle: CredentialLifecycle::default(),
        };
        let config = Config::with_providers(vec![profile]).with_credentials(vec![credential]);
        let resolver = CountingResolver(AtomicUsize::new(0));
        let cache = tempfile::tempdir().unwrap();

        let error = refresh_from_config(&config, "primary", &resolver, cache.path())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("authenticated endpoint origin mismatch"));
        assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
        assert!(matches!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        ));
    }

    #[tokio::test]
    async fn anthropic_refresh_uses_exact_path_and_headers() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/custom/catalog/models")
            .match_header("x-api-key", "secret-that-must-not-be-cached")
            .match_header("anthropic-version", "2023-06-01")
            .match_header("authorization", Matcher::Missing)
            .with_status(200)
            .with_body(r#"{"data":[{"id":"claude-b"},{"id":"claude-a"},{"id":"claude-a"}]}"#)
            .create_async()
            .await;
        let cache = tempfile::tempdir().unwrap();
        let catalog = refresh(
            &profile(&server.url(), "claude", CatalogAuth::AnthropicApiKey),
            cache.path(),
        )
        .await
        .unwrap();
        mock.assert_async().await;
        assert_eq!(catalog.models, vec!["claude-a", "claude-b"]);
    }

    #[tokio::test]
    async fn openai_compatible_refresh_uses_bearer_and_cache_has_no_secret() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/custom/catalog/models")
            .match_header("authorization", "Bearer secret-that-must-not-be-cached")
            .with_status(200)
            .with_body(r#"{"data":[{"id":"gpt-test"}]}"#)
            .create_async()
            .await;
        let cache = tempfile::tempdir().unwrap();
        let profile = profile(&server.url(), "openai", CatalogAuth::Bearer);
        let live = refresh(&profile, cache.path()).await.unwrap();
        mock.assert_async().await;

        let cached = read_cache(&profile, cache.path()).unwrap().unwrap();
        assert_eq!(cached.source, CatalogSource::Cache);
        assert_eq!(cached.refreshed_at, live.refreshed_at);
        let contents = std::fs::read_to_string(cache_path(&profile, cache.path())).unwrap();
        assert!(!contents.contains("secret-that-must-not-be-cached"));
        assert!(contents.contains("refreshed_at"));
        assert!(contents.contains("models_url"));
    }

    #[tokio::test]
    async fn mistral_refresh_uses_configured_path_and_bearer_auth() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/custom/catalog/models")
            .match_header("authorization", "Bearer secret-that-must-not-be-cached")
            .with_status(200)
            .with_body(r#"{"data":[{"id":"mistral-account-model"}]}"#)
            .create_async()
            .await;
        let cache = tempfile::tempdir().unwrap();
        let catalog = refresh(
            &profile(&server.url(), "mistral", CatalogAuth::Bearer),
            cache.path(),
        )
        .await
        .unwrap();
        mock.assert_async().await;
        assert_eq!(catalog.models, vec!["mistral-account-model"]);
    }

    #[tokio::test]
    async fn endpoint_query_credentials_are_used_but_not_cached() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/custom/catalog/models")
            .match_query(Matcher::UrlEncoded(
                "access_token".to_string(),
                "query-secret".to_string(),
            ))
            .with_status(200)
            .with_body(r#"{"data":[{"id":"query-auth-model"}]}"#)
            .create_async()
            .await;
        let cache = tempfile::tempdir().unwrap();
        let mut profile = profile(&server.url(), "openai", CatalogAuth::Bearer);
        profile
            .endpoints
            .models_url
            .push_str("?access_token=query-secret");
        let catalog = refresh(&profile, cache.path()).await.unwrap();
        mock.assert_async().await;
        assert!(!catalog.models_url.contains("query-secret"));

        let contents = std::fs::read_to_string(cache_path(&profile, cache.path())).unwrap();
        assert!(!contents.contains("query-secret"));
        assert!(read_cache(&profile, cache.path()).unwrap().is_some());
    }

    #[tokio::test]
    async fn accounts_at_same_endpoint_have_isolated_full_digest_caches() {
        let mut server = mockito::Server::new_async().await;
        let account_a = server
            .mock("GET", "/custom/catalog/models")
            .match_header("authorization", "Bearer account-a-key")
            .with_status(200)
            .with_body(r#"{"data":[{"id":"account-a-model"}]}"#)
            .create_async()
            .await;
        let account_b = server
            .mock("GET", "/custom/catalog/models")
            .match_header("authorization", "Bearer account-b-key")
            .with_status(200)
            .with_body(r#"{"data":[{"id":"account-b-model"}]}"#)
            .create_async()
            .await;
        let cache = tempfile::tempdir().unwrap();
        let mut a = profile(&server.url(), "openai", CatalogAuth::Bearer);
        a.api_key = "account-a-key".to_string();
        let mut b = profile(&server.url(), "openai", CatalogAuth::Bearer);
        b.api_key = "account-b-key".to_string();

        refresh(&a, cache.path()).await.unwrap();
        refresh(&b, cache.path()).await.unwrap();
        account_a.assert_async().await;
        account_b.assert_async().await;
        assert_ne!(profile_cache_identity(&a), profile_cache_identity(&b));
        assert_eq!(profile_cache_identity(&a).len(), 64);
        assert_ne!(cache_path(&a, cache.path()), cache_path(&b, cache.path()));
        assert_eq!(
            read_cache(&a, cache.path()).unwrap().unwrap().models,
            vec!["account-a-model"]
        );
        assert_eq!(
            read_cache(&b, cache.path()).unwrap().unwrap().models,
            vec!["account-b-model"]
        );
    }

    #[tokio::test]
    async fn oversized_body_count_and_id_are_rejected() {
        let cache = tempfile::tempdir().unwrap();

        let mut body_server = mockito::Server::new_async().await;
        let _body = body_server
            .mock("GET", "/custom/catalog/models")
            .with_status(200)
            .with_body("x".repeat(MAX_CATALOG_BODY_BYTES + 1))
            .create_async()
            .await;
        let body_error = refresh(
            &profile(&body_server.url(), "openai", CatalogAuth::Bearer),
            cache.path(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(body_error.contains("size limit"));

        let mut count_server = mockito::Server::new_async().await;
        let count_body = serde_json::json!({
            "data": (0..=MAX_MODEL_COUNT)
                .map(|index| serde_json::json!({"id": format!("model-{index}")}))
                .collect::<Vec<_>>()
        })
        .to_string();
        let _count = count_server
            .mock("GET", "/custom/catalog/models")
            .with_status(200)
            .with_body(count_body)
            .create_async()
            .await;
        let count_error = refresh(
            &profile(&count_server.url(), "openai", CatalogAuth::Bearer),
            cache.path(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(count_error.contains("count limit"));

        let mut id_server = mockito::Server::new_async().await;
        let _id = id_server
            .mock("GET", "/custom/catalog/models")
            .with_status(200)
            .with_body(
                serde_json::json!({"data": [{"id": "x".repeat(MAX_MODEL_ID_BYTES + 1)}]})
                    .to_string(),
            )
            .create_async()
            .await;
        let id_error = refresh(
            &profile(&id_server.url(), "openai", CatalogAuth::Bearer),
            cache.path(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(id_error.contains("oversized model ID"));
    }

    #[tokio::test]
    async fn transport_and_timeout_errors_redact_url_credentials() {
        let cache = tempfile::tempdir().unwrap();
        let mut refused = ModelCatalogProfile::new(
            "openai",
            "redaction-test",
            "header-secret",
            ProviderEndpoints::new(
                "http://visible-user:visible-pass@127.0.0.1:9",
                "/v1/chat/completions",
                "/private/models?access_token=query-secret",
            ),
            CatalogAuth::Bearer,
        );
        refused.request_timeout = Duration::from_millis(50);
        let error = refresh(&refused, cache.path())
            .await
            .unwrap_err()
            .to_string();
        for secret in [
            "visible-user",
            "visible-pass",
            "header-secret",
            "query-secret",
            "/private/models",
        ] {
            assert!(!error.contains(secret), "error leaked {secret}: {error}");
        }
        assert!(error.contains("http://127.0.0.1:9"));

        let mut timeout_server = mockito::Server::new_async().await;
        let _slow = timeout_server
            .mock("GET", "/private/models")
            .with_chunked_body(|writer| {
                std::thread::sleep(Duration::from_millis(100));
                writer.write_all(br#"{"data":[{"id":"late"}]}"#)
            })
            .create_async()
            .await;
        let mut timeout = ModelCatalogProfile::new(
            "openai",
            "timeout-test",
            "timeout-header-secret",
            ProviderEndpoints::new(
                &timeout_server.url(),
                "/v1/chat/completions",
                "/private/models?access_token=timeout-query-secret",
            ),
            CatalogAuth::Bearer,
        );
        timeout.request_timeout = Duration::from_millis(10);
        let error = refresh(&timeout, cache.path())
            .await
            .unwrap_err()
            .to_string();
        assert!(!error.contains("timeout-header-secret"));
        assert!(!error.contains("timeout-query-secret"));
        assert!(!error.contains("/private/models"));
        assert!(error.contains(&timeout_server.url()));
    }

    #[tokio::test]
    async fn failed_refresh_prefers_matching_cache_then_static_fallback() {
        let mut server = mockito::Server::new_async().await;
        let ok = server
            .mock("GET", "/custom/catalog/models")
            .with_status(200)
            .with_body(r#"{"data":[{"id":"cached-model"}]}"#)
            .expect(1)
            .create_async()
            .await;
        let cache = tempfile::tempdir().unwrap();
        let profile = profile(&server.url(), "openai", CatalogAuth::Bearer);
        refresh(&profile, cache.path()).await.unwrap();
        ok.assert_async().await;
        ok.remove_async().await;
        let (catalog, error) = refresh_with_fallback(&profile, cache.path()).await;
        assert!(error.is_some());
        assert_eq!(catalog.source, CatalogSource::Cache);
        assert_eq!(catalog.models, vec!["cached-model"]);

        let empty_cache = tempfile::tempdir().unwrap();
        let (catalog, error) = refresh_with_fallback(&profile, empty_cache.path()).await;
        assert!(error.is_some());
        assert_eq!(catalog.source, CatalogSource::StaticFallback);
        assert_eq!(
            catalog.models,
            vec!["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]
        );
        assert_eq!(STATIC_FALLBACK_AS_OF, "2026-08-26");
    }

    #[test]
    fn stale_claude_46_is_not_a_static_fallback() {
        assert!(!static_fallback("claude")
            .iter()
            .any(|model| model == "claude-sonnet-4-6"));
    }

    #[test]
    fn cache_metadata_drops_endpoint_query_credentials() {
        assert_eq!(
            cache_safe_url(
                "https://secret-user:secret-pass@models.example/v1/models?access_token=secret#fragment"
            ),
            "https://models.example/v1/models"
        );
    }

    #[test]
    fn catalog_profile_debug_redacts_api_key() {
        let profile = profile("https://models.example", "openai", CatalogAuth::Bearer);
        let debug = format!("{profile:?}");
        assert!(!debug.contains("secret-that-must-not-be-cached"));
        assert!(debug.contains("[REDACTED]"));
    }
}
