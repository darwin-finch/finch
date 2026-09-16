//! Finch mapping from application Config onto crate catalog refresh.
use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::config::{Config, CredentialProvider, CredentialResolver, ProviderEntry};
use crate::providers::{
    preflight_provider_config, refresh, CatalogAuth, ModelCatalog, ModelCatalogProfile,
    ProviderEndpoints,
};

/// Revalidate and resolve a named provider profile immediately before model
/// discovery. Invalid graphs return before the HTTP client is constructed.
pub async fn refresh_from_config(
    config: &Config,
    profile_name: &str,
    resolver: &dyn CredentialResolver,
    cache_dir: &Path,
) -> Result<ModelCatalog> {
    crate::providers::preflight_provider_config(config)?;
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
        CredentialProvider::Openrouter => (
            "https://openrouter.ai/api",
            "/v1/chat/completions",
            "/v1/models",
            CatalogAuth::Bearer,
            "openrouter",
        ),
        _ => bail!(
            "provider profile '{profile_name}' does not have a supported named-credential catalogue transport"
        ),
    };
    let credentials = crate::config::credential_index(config.credentials())?;
    let metadata = credentials
        .get(credential.credential_ref.as_str())
        .expect("Config::validate checked the named credential reference");
    let resolved = resolver.resolve(metadata).map_err(|_| {
        anyhow::anyhow!("Failed to resolve named credential '{}'; inspect the credential store without printing secret material", credential.credential_ref)
    })?;
    if resolved.credential_name != credential.credential_ref {
        bail!("credential resolver returned a handle for the wrong named credential");
    }
    // Resolution may block on a local store. Revalidate the complete graph at
    // the last local boundary before constructing HTTP state so revocation or
    // expiry during resolution still produces zero network activity.
    config.validate()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AudienceBinding, CredentialBinding, CredentialKind, CredentialLifecycle, EndpointFamily,
        ProviderCredential, ResolvedCredential, ResolvedSecret,
    };
    use crate::providers::{CatalogAuth, ModelCatalog, ProviderEndpoints};
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;

    struct CountingResolver(AtomicUsize);

    struct BlockingResolver {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl CredentialResolver for CountingResolver {
        fn resolve(&self, credential: &ProviderCredential) -> Result<ResolvedCredential> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ResolvedCredential {
                credential_name: credential.name.clone(),
                secret: ResolvedSecret::new("not-sent")?,
            })
        }
    }

    impl CredentialResolver for BlockingResolver {
        fn resolve(&self, credential: &ProviderCredential) -> Result<ResolvedCredential> {
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            Ok(ResolvedCredential {
                credential_name: credential.name.clone(),
                secret: ResolvedSecret::new("must-not-be-sent")?,
            })
        }
    }

    fn live_catalog_config(
        origin: &str,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Config {
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
            base_url: Some(origin.into()),
            chat_path: None,
            models_path: None,
            name: Some("primary".into()),
            reasoning_effort: None,
        };
        let credential = ProviderCredential {
            name: "work".into(),
            kind: CredentialKind::ApiKey,
            provider: CredentialProvider::OpenaiPlatform,
            issuer: "openai-platform".into(),
            audience: AudienceBinding::custom(origin).unwrap(),
            tenant: None,
            project: None,
            account: None,
            scopes: BTreeSet::new(),
            secret_ref: "test:work".into(),
            lifecycle: CredentialLifecycle::Active {
                expires_at,
                refreshable: false,
            },
            revocation: Default::default(),
        };
        Config::with_providers(vec![profile]).with_credentials(vec![credential])
    }

    fn unsupported_catalog_sibling(
        provider: CredentialProvider,
        origin: &str,
    ) -> (ProviderEntry, ProviderCredential) {
        let (kind, issuer, audience, base_url) = match provider {
            CredentialProvider::ChatgptSubscription => (
                CredentialKind::Bearer,
                "openai-chatgpt",
                AudienceBinding::custom(origin).unwrap(),
                Some(origin.to_string()),
            ),
            CredentialProvider::GoogleVertex => (
                CredentialKind::CloudIdentity,
                "google-cloud",
                AudienceBinding::standard(EndpointFamily::GoogleVertex),
                None,
            ),
            CredentialProvider::GeminiAiStudio => (
                CredentialKind::ApiKey,
                "google-ai-studio",
                AudienceBinding::custom(origin).unwrap(),
                Some(origin.to_string()),
            ),
            _ => panic!("test helper requires an unsupported sibling transport"),
        };
        let credential_name = format!("unsupported-{}", provider.as_str());
        (
            ProviderEntry::Credentialed {
                provider,
                credential: CredentialBinding {
                    credential_ref: credential_name.clone(),
                    audience: None,
                    tenant: None,
                    project: None,
                    account: None,
                    required_scopes: BTreeSet::new(),
                },
                model: Some("unsupported-model".into()),
                base_url,
                chat_path: None,
                models_path: None,
                name: Some(format!("unsupported-{}", provider.as_str())),
                reasoning_effort: None,
            },
            ProviderCredential {
                name: credential_name.clone(),
                kind,
                provider,
                issuer: issuer.into(),
                audience,
                tenant: None,
                project: None,
                account: None,
                scopes: BTreeSet::new(),
                secret_ref: format!("test:{credential_name}"),
                lifecycle: CredentialLifecycle::default(),
                revocation: Default::default(),
            },
        )
    }

    fn start_blocked_catalog_refresh(
        config: Config,
    ) -> (
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        std::thread::JoinHandle<Result<ModelCatalog>>,
    ) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let cache = tempfile::tempdir().unwrap();
            let resolver = BlockingResolver {
                entered: entered_tx,
                release: Mutex::new(release_rx),
            };
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(refresh_from_config(
                    &config,
                    "primary",
                    &resolver,
                    cache.path(),
                ))
        });
        (entered_rx, release_tx, handle)
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
            revocation: Default::default(),
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
    async fn catalog_preflights_every_sibling_transport_before_selected_resolution_or_http() {
        let cases = [
            (
                CredentialProvider::ChatgptSubscription,
                "ChatGPT subscription custom endpoints and paths are not supported",
            ),
            (
                CredentialProvider::GoogleVertex,
                "Google Vertex named credentials are modeled",
            ),
            (
                CredentialProvider::GeminiAiStudio,
                "Gemini AI Studio custom endpoints are not supported",
            ),
        ];
        for (provider, expected) in cases {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let selected = live_catalog_config(&endpoint, None);
            let mut profiles = selected.providers.clone();
            let mut credentials = selected.credentials().to_vec();
            let (sibling, sibling_credential) = unsupported_catalog_sibling(provider, &endpoint);
            profiles.push(sibling);
            credentials.push(sibling_credential);
            let config = Config::with_providers(profiles).with_credentials(credentials);
            let resolver = CountingResolver(AtomicUsize::new(0));
            let cache = tempfile::tempdir().unwrap();

            let error = refresh_from_config(&config, "primary", &resolver, cache.path())
                .await
                .unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "wrong sibling preflight error for {provider:?}: {error:#}"
            );
            assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
            assert!(matches!(
                listener.accept(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ));
        }
    }

    #[test]
    fn catalog_revocation_while_resolving_rejects_before_socket_activity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let mut config = live_catalog_config(&origin, None);
        let (entered, release, refresh) = start_blocked_catalog_refresh(config.clone());
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        config.revoke_credential("work").unwrap();
        release.send(()).unwrap();
        assert!(format!("{:#}", refresh.join().unwrap().unwrap_err()).contains("revoked"));
        assert!(matches!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn catalog_expiry_while_resolving_rejects_before_socket_activity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let expires_at = chrono::Utc::now() + chrono::Duration::milliseconds(500);
        let config = live_catalog_config(&origin, Some(expires_at));
        let (entered, release, refresh) = start_blocked_catalog_refresh(config);
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(550));
        release.send(()).unwrap();
        assert!(format!("{:#}", refresh.join().unwrap().unwrap_err()).contains("expired"));
        assert!(matches!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        ));
    }
}
