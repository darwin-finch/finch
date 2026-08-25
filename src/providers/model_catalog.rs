//! Authenticated provider model discovery with a secret-free local cache.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::endpoints::ProviderEndpoints;

const CATALOG_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogAuth {
    AnthropicApiKey,
    Bearer,
}

#[derive(Debug, Clone)]
pub struct ModelCatalogProfile {
    pub provider: String,
    pub api_key: String,
    pub endpoints: ProviderEndpoints,
    pub auth: CatalogAuth,
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
        "claude" => &["claude-sonnet-4-5-20250929", "claude-haiku-4-5-20251001"],
        "openai" => &["gpt-4o", "gpt-4-turbo", "o3-mini"],
        "grok" => &["grok-code-fast-1", "grok-2-latest"],
        "gemini" => &["gemini-2.0-flash", "gemini-1.5-pro"],
        "mistral" => &["mistral-large-latest", "codestral-latest"],
        "groq" => &["llama-3.3-70b-versatile", "gemma2-9b-it"],
        _ => &[],
    };
    models.iter().map(|model| (*model).to_string()).collect()
}

pub fn fallback_catalog(provider: &str, models_url: &str) -> ModelCatalog {
    ModelCatalog {
        provider: provider.to_string(),
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
        .timeout(CATALOG_TIMEOUT)
        .build()
        .context("Failed to create model catalogue HTTP client")?;
    let mut request = client.get(&profile.endpoints.models_url);
    request = match profile.auth {
        CatalogAuth::AnthropicApiKey => request
            .header("x-api-key", &profile.api_key)
            .header("anthropic-version", "2023-06-01"),
        CatalogAuth::Bearer => request.bearer_auth(&profile.api_key),
    };

    let response = request
        .send()
        .await
        .with_context(|| format!("Failed to refresh {} model catalogue", profile.provider))?;
    let status = response.status();
    if !status.is_success() {
        bail!(
            "{} model catalogue returned HTTP {}",
            profile.provider,
            status
        );
    }

    let payload: ModelsResponse = response
        .json()
        .await
        .with_context(|| format!("Invalid {} model catalogue response", profile.provider))?;
    let mut models: Vec<String> = payload
        .data
        .into_iter()
        .map(|record| record.id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    models.sort();
    models.dedup();
    if models.is_empty() {
        bail!("{} returned an empty model catalogue", profile.provider);
    }

    let catalog = ModelCatalog {
        provider: profile.provider.clone(),
        models_url: cache_safe_url(&profile.endpoints.models_url),
        models,
        source: CatalogSource::Discovered,
        refreshed_at: Utc::now(),
    };
    write_cache(&catalog, profile, cache_dir)?;
    Ok(catalog)
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
                (
                    fallback_catalog(&profile.provider, &profile.endpoints.models_url),
                    Some(error.to_string()),
                )
            }
        }
    }
}

pub fn read_cache(profile: &ModelCatalogProfile, cache_dir: &Path) -> Result<Option<ModelCatalog>> {
    let path = cache_path(profile, cache_dir);
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read {}", path.display()))
        }
    };
    let mut catalog: ModelCatalog = serde_json::from_str(&contents)
        .with_context(|| format!("Invalid model catalogue cache {}", path.display()))?;
    if catalog.provider != profile.provider
        || catalog.models_url != cache_safe_url(&profile.endpoints.models_url)
    {
        return Ok(None);
    }
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
    let mut hasher = Sha256::new();
    hasher.update(profile.provider.as_bytes());
    hasher.update([0]);
    hasher.update(profile.endpoints.models_url.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    cache_dir.join(format!("{}-{}.json", profile.provider, &digest[..16]))
}

fn cache_safe_url(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    fn profile(server_url: &str, provider: &str, auth: CatalogAuth) -> ModelCatalogProfile {
        ModelCatalogProfile {
            provider: provider.to_string(),
            api_key: "secret-that-must-not-be-cached".to_string(),
            endpoints: ProviderEndpoints::new(
                server_url,
                "/v1/chat/completions",
                "/custom/catalog/models",
            ),
            auth,
        }
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
        refresh(&profile, cache.path()).await.unwrap();
        mock.assert_async().await;

        let cached = read_cache(&profile, cache.path()).unwrap().unwrap();
        assert_eq!(cached.source, CatalogSource::Cache);
        let contents = std::fs::read_to_string(cache_path(&profile, cache.path())).unwrap();
        assert!(!contents.contains("secret-that-must-not-be-cached"));
        assert!(contents.contains("refreshed_at"));
        assert!(contents.contains("models_url"));
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
        assert!(!catalog.models.is_empty());
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
            cache_safe_url("https://models.example/v1/models?access_token=secret#fragment"),
            "https://models.example/v1/models"
        );
    }
}
