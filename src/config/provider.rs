// Unified provider entry — covers both cloud and local inference backends.

use crate::config::backend::ExecutionTarget;
use crate::config::{CredentialBinding, CredentialProvider, ReasoningEffort};
use crate::models::{InferenceProvider, ModelFamily, ModelSize};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_true() -> bool {
    true
}

fn default_ollama_model() -> String {
    "qwen2.5:7b".to_string()
}

fn default_ollama_base_url() -> String {
    "http://localhost:11434".to_string()
}

fn default_inference_provider() -> InferenceProvider {
    InferenceProvider::LlamaCpp
}

fn default_execution_target() -> ExecutionTarget {
    ExecutionTarget::Auto
}

fn default_model_family() -> ModelFamily {
    ModelFamily::Qwen2
}

fn default_model_size() -> ModelSize {
    ModelSize::Medium
}

/// A single provider entry — either a cloud API or a local inference backend.
///
/// Serializes with a `type` tag, e.g.:
/// ```toml
/// [[providers]]
/// type = "grok"
/// api_key = "xai-..."
///
/// [[providers]]
/// type = "local"
/// inference_provider = "llama_cpp"
/// execution_target = "auto"
/// ```
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ProviderEntry {
    /// A cloud model profile bound to a first-class named credential.
    #[serde(rename = "credentialed")]
    Credentialed {
        provider: CredentialProvider,
        credential: CredentialBinding,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<ReasoningEffort>,
    },
    #[serde(rename = "chatgpt_subscription")]
    /// Legacy Codex app-server profile retained only so old configuration can
    /// be diagnosed without silently treating a subscription as a Platform key.
    LegacyChatgptSubscription {
        #[serde(default)]
        credential_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Claude {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        /// Messages path relative to `base_url`, or a complete URL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat_path: Option<String>,
        /// Model-list path relative to `base_url`, or a complete URL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// The official `claude` CLI (Claude Code) driven as a subscription
    /// subprocess: `--print --input-format stream-json --output-format
    /// stream-json`, system prompt overridden, tools disabled. The CLI holds
    /// its own OAuth login; Finch never touches credentials.
    ///
    /// WARNING: this back-ends Finch with a personal Claude.ai subscription
    /// through the interactive CLI product. Anthropic has enforced account
    /// suspensions against CLI-wrapper/proxy usage that disguises a
    /// subscription-gated product as a generic backend; enabling this entry
    /// is an explicit, informed acceptance of that risk. It is never offered
    /// by the setup wizard and is off unless configured by hand.
    #[serde(rename = "claude_cli_backend")]
    ClaudeCliBackend {
        /// Upstream model the CLI is asked to serve (`--model`). Unset means
        /// the CLI's own measured default (claude-sonnet-5, 2026-09-25).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Path to the CLI binary; defaults to `claude` on PATH.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        binary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Openai {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        /// Chat-completions path relative to `base_url`, or a complete URL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat_path: Option<String>,
        /// Model-list path relative to `base_url`, or a complete URL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<ReasoningEffort>,
    },
    Grok {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Gemini {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Mistral {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Groq {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Openrouter {
        api_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Ollama local/remote inference (OpenAI-compatible API).
    ///
    /// ```toml
    /// [[providers]]
    /// type = "ollama"
    /// model = "qwen2.5:7b"
    /// base_url = "http://localhost:11434"   # default
    /// ```
    Ollama {
        #[serde(default = "default_ollama_model")]
        model: String,
        #[serde(default = "default_ollama_base_url")]
        base_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Remote finch daemon as a provider.
    ///
    /// ```toml
    /// [[providers]]
    /// type = "remote_daemon"
    /// address = "http://192.168.1.50:11435"
    /// ```
    #[serde(rename = "remote_daemon")]
    RemoteDaemon {
        address: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Local {
        #[serde(default = "default_inference_provider")]
        inference_provider: InferenceProvider,
        #[serde(default = "default_execution_target")]
        execution_target: ExecutionTarget,
        #[serde(default = "default_model_family")]
        model_family: ModelFamily,
        #[serde(default = "default_model_size")]
        model_size: ModelSize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_path: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        managed_artifact: Option<crate::models::ManagedGgufArtifact>,
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

impl std::fmt::Debug for ProviderEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("ProviderEntry");
        debug.field("type", &self.provider_type());
        debug.field("profile_name", &self.profile_name());
        if let Some(binding) = self.credential_binding() {
            debug.field("credential_ref", &binding.credential_ref);
        } else if self.api_key().is_some() {
            debug.field("legacy_inline_api_key", &"[REDACTED]");
        }
        debug.finish_non_exhaustive()
    }
}

impl ProviderEntry {
    /// Return a copy of this entry with the inline API key replaced.
    ///
    /// Entries that do not carry an inline key (credential-bound profiles,
    /// subscription placeholders, Ollama, remote daemons, local models) return
    /// an equivalent entry unchanged — they authenticate another way.
    pub fn with_api_key(&self, api_key: String) -> Self {
        match self {
            Self::Claude { .. }
            | Self::Openai { .. }
            | Self::Grok { .. }
            | Self::Gemini { .. }
            | Self::Mistral { .. }
            | Self::Groq { .. }
            | Self::Openrouter { .. } => {
                let mut copy = self.clone();
                match &mut copy {
                    Self::Claude { api_key: key, .. }
                    | Self::Openai { api_key: key, .. }
                    | Self::Grok { api_key: key, .. }
                    | Self::Gemini { api_key: key, .. }
                    | Self::Mistral { api_key: key, .. }
                    | Self::Groq { api_key: key, .. }
                    | Self::Openrouter { api_key: key, .. } => *key = api_key,
                    _ => unreachable!("outer match guarantees a keyed cloud variant"),
                }
                copy
            }
            other => other.clone(),
        }
    }

    /// Stable, user-facing selector for this configured provider profile.
    ///
    /// Explicit `name` values win. Older configs without names remain usable by
    /// falling back to the configured model, then to a provider-specific label.
    pub fn profile_name(&self) -> String {
        let explicit_name = match self {
            Self::Credentialed { name, .. }
            | Self::LegacyChatgptSubscription { name, .. }
            | Self::Claude { name, .. }
            | Self::Openai { name, .. }
            | Self::Grok { name, .. }
            | Self::Gemini { name, .. }
            | Self::Mistral { name, .. }
            | Self::Groq { name, .. }
            | Self::Openrouter { name, .. }
            | Self::Ollama { name, .. }
            | Self::RemoteDaemon { name, .. }
            | Self::Local { name, .. }
            | Self::ClaudeCliBackend { name, .. } => name.as_deref(),
        };

        if let Some(name) = explicit_name.filter(|name| !name.trim().is_empty()) {
            return name.to_string();
        }

        if let Some(model) = self.model().filter(|model| !model.trim().is_empty()) {
            return model.to_string();
        }

        match self {
            Self::Credentialed { provider, .. } => provider.as_str().to_string(),
            Self::LegacyChatgptSubscription { .. } => "chatgpt-subscription-legacy".to_string(),
            Self::ClaudeCliBackend { .. } => "claude-cli-subscription".to_string(),
            Self::Local { .. } => {
                let descriptor = self
                    .local_model_descriptor()
                    .expect("Local variant always yields a descriptor");
                format!(
                    "local-{}",
                    descriptor.to_ascii_lowercase().replace(' ', "-")
                )
            }
            Self::RemoteDaemon { .. } => "remote-daemon".to_string(),
            _ => self.provider_type().to_string(),
        }
    }

    /// Family+size descriptor a daemon-reported local model status names when
    /// it refers to THIS entry, e.g. `"Gemma 2 9b"` for a `Gemma2`/`Medium`
    /// local profile. `None` for non-local variants.
    ///
    /// The daemon formats `LocalModelStatus::Ready`/`Loading` as this
    /// descriptor followed by `" (<engine>; requested <target>)"`
    /// (`load_generator_async` in `src/models/bootstrap.rs`), so compare a
    /// daemon-reported string with [`Self::local_model_status_matches`]
    /// rather than exact equality against this descriptor.
    pub fn local_model_descriptor(&self) -> Option<String> {
        match self {
            Self::Local {
                model_family,
                model_size,
                ..
            } => Some(format!(
                "{} {}",
                model_family.name(),
                model_size.to_size_string(*model_family)
            )),
            _ => None,
        }
    }

    /// True when a daemon-reported local model descriptor (from
    /// `LocalModelStatus::Ready`/`Loading`) names THIS entry's model, rather
    /// than a different local profile the daemon already had loaded from an
    /// earlier activation.
    ///
    /// The daemon bootstraps exactly one local model for its whole process
    /// lifetime — chosen once from the launch-time config and never
    /// reloaded — so "some local model is ready" is not the same claim as
    /// "the requested local entry is ready". Non-local entries never match.
    pub fn local_model_status_matches(&self, reported: &str) -> bool {
        match self.local_model_descriptor() {
            Some(expected) => {
                reported == expected || reported.starts_with(&format!("{expected} ("))
            }
            None => false,
        }
    }

    /// Shared wording for "the daemon cannot switch to this entry because it
    /// already has a different local model loaded and ready as `reported`".
    ///
    /// Every caller that discovers a [`Self::local_model_status_matches`]
    /// mismatch (the synchronous and deferred activation paths in both
    /// `handle_provider_switch` and `apply_effective_selection`) explains it
    /// with this same core wording, so a future correction to it cannot
    /// drift between call sites the way the identity check itself used to.
    pub fn local_model_switch_blocked_message(&self, reported: &str) -> String {
        format!(
            "This daemon is already running {reported}; switching to {} requires restarting the daemon with that model configured",
            self.profile_name()
        )
    }

    /// Human-readable name for UI display.
    pub fn display_name(&self) -> &str {
        match self {
            Self::Credentialed { name, provider, .. } => {
                name.as_deref().unwrap_or_else(|| provider.as_str())
            }
            Self::LegacyChatgptSubscription { name, .. } => name
                .as_deref()
                .unwrap_or("Unsupported legacy ChatGPT subscription"),
            Self::Claude { name, .. } => name.as_deref().unwrap_or("Claude"),
            Self::ClaudeCliBackend { name, .. } => {
                name.as_deref().unwrap_or("Claude CLI (subscription)")
            }
            Self::Openai { name, .. } => name.as_deref().unwrap_or("OpenAI"),
            Self::Grok { name, .. } => name.as_deref().unwrap_or("Grok"),
            Self::Gemini { name, .. } => name.as_deref().unwrap_or("Gemini"),
            Self::Mistral { name, .. } => name.as_deref().unwrap_or("Mistral"),
            Self::Groq { name, .. } => name.as_deref().unwrap_or("Groq"),
            Self::Openrouter { name, .. } => name.as_deref().unwrap_or("OpenRouter"),
            Self::Ollama { name, .. } => name.as_deref().unwrap_or("Ollama"),
            Self::RemoteDaemon { name, .. } => name.as_deref().unwrap_or("Remote Daemon"),
            Self::Local { name, .. } => name.as_deref().unwrap_or("Local"),
        }
    }

    /// Short provider-type tag (e.g. "claude", "grok", "local").
    pub fn provider_type(&self) -> &'static str {
        match self {
            Self::Credentialed { provider, .. } => provider.as_str(),
            Self::LegacyChatgptSubscription { .. } => "chatgpt_subscription",
            Self::Claude { .. } => "claude",
            Self::Openai { .. } => "openai",
            Self::ClaudeCliBackend { .. } => "claude_cli",
            Self::Grok { .. } => "grok",
            Self::Gemini { .. } => "gemini",
            Self::Mistral { .. } => "mistral",
            Self::Groq { .. } => "groq",
            Self::Openrouter { .. } => "openrouter",
            Self::Ollama { .. } => "ollama",
            Self::RemoteDaemon { .. } => "remote_daemon",
            Self::Local { .. } => "local",
        }
    }

    /// True for `Local` variants.
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    /// API key for cloud variants; `None` for Local, Ollama, and RemoteDaemon.
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::Claude { api_key, .. } => Some(api_key.as_str()),
            Self::Openai { api_key, .. } => Some(api_key.as_str()),
            Self::Grok { api_key, .. } => Some(api_key.as_str()),
            Self::Gemini { api_key, .. } => Some(api_key.as_str()),
            Self::Mistral { api_key, .. } => Some(api_key.as_str()),
            Self::Groq { api_key, .. } => Some(api_key.as_str()),
            Self::Openrouter { api_key, .. } => Some(api_key.as_str()),
            Self::Credentialed { .. }
            | Self::LegacyChatgptSubscription { .. }
            | Self::ClaudeCliBackend { .. }
            | Self::Ollama { .. }
            | Self::RemoteDaemon { .. }
            | Self::Local { .. } => None,
        }
    }

    /// Optional model override (cloud providers only).
    pub fn model(&self) -> Option<&str> {
        match self {
            Self::Credentialed { model, .. } => model.as_deref(),
            Self::LegacyChatgptSubscription { model, .. } => model.as_deref(),
            Self::Claude { model, .. } => model.as_deref(),
            Self::ClaudeCliBackend { model, .. } => model.as_deref(),
            Self::Openai { model, .. } => model.as_deref(),
            Self::Grok { model, .. } => model.as_deref(),
            Self::Gemini { model, .. } => model.as_deref(),
            Self::Mistral { model, .. } => model.as_deref(),
            Self::Groq { model, .. } => model.as_deref(),
            Self::Openrouter { model, .. } => model.as_deref(),
            Self::Ollama { model, .. } => Some(model.as_str()),
            Self::RemoteDaemon { .. } | Self::Local { .. } => None,
        }
    }

    /// Configured reasoning effort, when this entry schema carries one.
    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        match self {
            Self::Credentialed {
                reasoning_effort, ..
            }
            | Self::Openai {
                reasoning_effort, ..
            } => *reasoning_effort,
            _ => None,
        }
    }

    /// Whether `/thinking` is meaningful for this provider type.
    pub fn supports_reasoning_effort(&self) -> bool {
        matches!(self, Self::Credentialed { .. } | Self::Openai { .. })
    }

    /// Clone this entry with a Brain-local model overlay. Does not write config.
    pub fn with_model_overlay(&self, overlay: Option<String>) -> Self {
        let mut entry = self.clone();
        match &mut entry {
            Self::Credentialed { model, .. }
            | Self::LegacyChatgptSubscription { model, .. }
            | Self::Claude { model, .. }
            | Self::Openai { model, .. }
            | Self::Grok { model, .. }
            | Self::Gemini { model, .. }
            | Self::Mistral { model, .. }
            | Self::Groq { model, .. }
            | Self::Openrouter { model, .. }
            | Self::ClaudeCliBackend { model, .. } => *model = overlay,
            Self::Ollama { model, .. } => {
                if let Some(value) = overlay {
                    *model = value;
                }
            }
            Self::RemoteDaemon { .. } | Self::Local { .. } => {}
        }
        entry
    }

    /// Clone this entry with a Brain-local thinking overlay. Does not write config.
    pub fn with_reasoning_effort_overlay(&self, overlay: Option<ReasoningEffort>) -> Self {
        let mut entry = self.clone();
        match &mut entry {
            Self::Credentialed {
                reasoning_effort, ..
            }
            | Self::Openai {
                reasoning_effort, ..
            } => *reasoning_effort = overlay,
            _ => {}
        }
        entry
    }

    /// Named provider credential binding, if this is a credentialed profile.
    pub fn credential_binding(&self) -> Option<&CredentialBinding> {
        match self {
            Self::Credentialed { credential, .. } => Some(credential),
            _ => None,
        }
    }

    /// Named credential provider namespace, if applicable.
    pub fn credential_provider(&self) -> Option<CredentialProvider> {
        match self {
            Self::Credentialed { provider, .. } => Some(*provider),
            _ => None,
        }
    }

    /// Configured base endpoint for credential audience validation.
    pub fn credential_base_url(&self) -> Option<&str> {
        match self {
            Self::Credentialed { base_url, .. } => base_url.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloud_serde_roundtrip() {
        let entry = ProviderEntry::Claude {
            api_key: "sk-ant-test".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("Claude Primary".to_string()),
        };
        let toml = toml::to_string(&entry).unwrap();
        let decoded: ProviderEntry = toml::from_str(&toml).unwrap();
        assert_eq!(entry, decoded);
    }

    #[test]
    fn claude_cli_backend_toml_roundtrip_carries_all_fields_and_defaults() {
        let entry = ProviderEntry::ClaudeCliBackend {
            model: Some("claude-opus-4-6".to_string()),
            binary: Some("/Users/x/.local/bin/claude".to_string()),
            name: Some("subscription-cli".to_string()),
        };
        let toml = toml::to_string(&entry).unwrap();
        let decoded: ProviderEntry = toml::from_str(&toml).unwrap();
        assert_eq!(entry, decoded);

        // Minimal hand-written form: only the type tag. Every other field
        // must default so the entry stays off unless deliberately written.
        let entry = toml::from_str::<ProviderEntry>(
            r#"type = "claude_cli_backend"
"#,
        )
        .unwrap();
        assert!(matches!(
            entry,
            ProviderEntry::ClaudeCliBackend {
                model: None,
                binary: None,
                name: None,
            }
        ));
        assert_eq!(entry.provider_type(), "claude_cli");
        assert!(!entry.is_local());
        assert_eq!(
            entry.profile_name(),
            "claude-cli-subscription",
            "the fallback name must name the entry's nature unambiguously"
        );
        assert!(entry.api_key().is_none());

        // The serialized entry must never grow an api_key field.
        let rendered = toml::to_string(&entry).unwrap();
        assert!(!rendered.contains("api_key"));
    }

    #[test]
    fn test_legacy_inline_api_key_debug_is_redacted() {
        let secret = "sk-ant-super-secret-marker";
        let entry = ProviderEntry::Claude {
            api_key: secret.into(),
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("legacy".into()),
        };
        let debug = format!("{entry:?}");
        assert!(!debug.contains(secret));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn legacy_chatgpt_subscription_deserializes_without_reinterpretation() {
        let encoded = r#"type = "chatgpt_subscription"
credential_ref = "codex-app-server:managed"
model = "gpt-5.6-sol"
name = "subscription"
"#;
        let entry = toml::from_str::<ProviderEntry>(encoded).unwrap();
        assert!(matches!(
            entry,
            ProviderEntry::LegacyChatgptSubscription {
                ref credential_ref,
                ..
            } if credential_ref == "codex-app-server:managed"
        ));
        let encoded = toml::to_string(&entry).unwrap();
        assert!(encoded.contains("codex-app-server:managed"));
        assert!(!encoded.contains("api_key"));
    }

    #[test]
    fn legacy_chatgpt_subscription_accepts_missing_credential_reference() {
        let entry = toml::from_str::<ProviderEntry>(
            r#"type = "chatgpt_subscription"
model = "gpt-5.6-sol"
"#,
        )
        .unwrap();
        assert_eq!(
            entry,
            ProviderEntry::LegacyChatgptSubscription {
                credential_ref: String::new(),
                model: Some("gpt-5.6-sol".into()),
                name: None,
            }
        );
    }

    #[test]
    fn test_profile_name_prefers_explicit_name() {
        let entry = ProviderEntry::Claude {
            api_key: "test-key".to_string(),
            model: Some("claude-haiku".to_string()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("fast".to_string()),
        };
        assert_eq!(entry.profile_name(), "fast");
    }

    #[test]
    fn test_profile_name_falls_back_to_model_for_legacy_config() {
        let entry = ProviderEntry::Claude {
            api_key: "test-key".to_string(),
            model: Some("claude-haiku".to_string()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: None,
        };
        assert_eq!(entry.profile_name(), "claude-haiku");
    }

    #[test]
    fn test_openai_reasoning_effort_toml_roundtrip() {
        let entry = ProviderEntry::Openai {
            api_key: "test-key".to_string(),
            model: Some("gpt-5.6-sol".to_string()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("deep".to_string()),
            reasoning_effort: Some(ReasoningEffort::Xhigh),
        };
        let toml = toml::to_string(&entry).unwrap();
        assert!(toml.contains("reasoning_effort = \"xhigh\""));
        assert_eq!(toml::from_str::<ProviderEntry>(&toml).unwrap(), entry);
    }

    #[test]
    fn test_chatgpt_subscription_reasoning_omission_and_explicit_value_roundtrip() {
        let omitted = r#"type = "credentialed"
provider = "chatgpt_subscription"
model = "gpt-5.6-sol"
name = "chatgpt"

[credential]
credential_ref = "work"
"#;
        let omitted_entry = toml::from_str::<ProviderEntry>(omitted)
            .expect("omitted ChatGPT subscription reasoning must deserialize");
        let ProviderEntry::Credentialed {
            reasoning_effort, ..
        } = &omitted_entry
        else {
            panic!("omitted ChatGPT subscription profile changed variant")
        };
        assert_eq!(
            *reasoning_effort, None,
            "omitted reasoning must remain distinguishable from an explicit value"
        );
        let omitted_encoded = toml::to_string(&omitted_entry).unwrap();
        assert!(
            !omitted_encoded.contains("reasoning_effort"),
            "round-trip invented an explicit reasoning setting: {omitted_encoded}"
        );

        let explicit = omitted.replace(
            "name = \"chatgpt\"",
            "name = \"chatgpt\"\nreasoning_effort = \"xhigh\"",
        );
        let explicit_entry = toml::from_str::<ProviderEntry>(&explicit)
            .expect("explicit ChatGPT subscription reasoning must deserialize");
        let explicit_encoded = toml::to_string(&explicit_entry).unwrap();
        assert!(
            explicit_encoded.contains("reasoning_effort = \"xhigh\""),
            "round-trip lost the explicit reasoning setting: {explicit_encoded}"
        );
        assert_eq!(
            toml::from_str::<ProviderEntry>(&explicit_encoded).unwrap(),
            explicit_entry,
            "explicit ChatGPT subscription reasoning changed across config round-trip"
        );
    }

    #[test]
    fn test_local_serde_roundtrip() {
        let entry = ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: Some("Local Qwen 3B".to_string()),
        };
        let toml = toml::to_string(&entry).unwrap();
        let decoded: ProviderEntry = toml::from_str(&toml).unwrap();
        assert_eq!(entry, decoded);
    }

    #[test]
    fn test_grok_serde_roundtrip() {
        let entry = ProviderEntry::Grok {
            api_key: "xai-test".to_string(),
            model: Some("grok-code-fast-1".to_string()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: None,
        };
        let toml = toml::to_string(&entry).unwrap();
        let decoded: ProviderEntry = toml::from_str(&toml).unwrap();
        assert_eq!(entry, decoded);
    }

    #[test]
    fn test_display_name_fallback() {
        let entry = ProviderEntry::Claude {
            api_key: "key".to_string(),
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: None,
        };
        assert_eq!(entry.display_name(), "Claude");
    }

    #[test]
    fn test_display_name_custom() {
        let entry = ProviderEntry::Grok {
            api_key: "key".to_string(),
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("Grok (Primary)".to_string()),
        };
        assert_eq!(entry.display_name(), "Grok (Primary)");
    }

    #[test]
    fn test_is_local() {
        let local = ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: None,
        };
        assert!(local.is_local());

        let cloud = ProviderEntry::Claude {
            api_key: "key".to_string(),
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: None,
        };
        assert!(!cloud.is_local());
    }

    #[test]
    fn test_api_key_none_for_local() {
        let local = ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: None,
        };
        assert!(local.api_key().is_none());
    }

    #[test]
    fn test_provider_type_tags() {
        assert_eq!(
            ProviderEntry::Claude {
                api_key: "k".to_string(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: None
            }
            .provider_type(),
            "claude"
        );
        assert_eq!(
            ProviderEntry::Grok {
                api_key: "k".to_string(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: None
            }
            .provider_type(),
            "grok"
        );
        assert_eq!(
            ProviderEntry::Local {
                inference_provider: InferenceProvider::LlamaCpp,
                execution_target: ExecutionTarget::Auto,
                model_family: ModelFamily::Qwen2,
                model_size: ModelSize::Medium,
                model_path: None,
                managed_artifact: None,
                enabled: true,
                name: None,
            }
            .provider_type(),
            "local"
        );
    }

    #[test]
    fn test_array_of_providers_toml() {
        let providers = vec![
            ProviderEntry::Grok {
                api_key: "xai-test".to_string(),
                model: Some("grok-code-fast-1".to_string()),
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("Grok (Primary)".to_string()),
            },
            ProviderEntry::Claude {
                api_key: "sk-ant-test".to_string(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: None,
            },
            ProviderEntry::Local {
                inference_provider: InferenceProvider::LlamaCpp,
                execution_target: ExecutionTarget::Auto,
                model_family: ModelFamily::Qwen2,
                model_size: ModelSize::Medium,
                model_path: None,
                managed_artifact: None,
                enabled: true,
                name: None,
            },
        ];

        // Serialize as TOML array
        #[derive(Serialize, Deserialize)]
        struct Wrapper {
            providers: Vec<ProviderEntry>,
        }
        let w = Wrapper {
            providers: providers.clone(),
        };
        let toml_str = toml::to_string(&w).unwrap();
        let decoded: Wrapper = toml::from_str(&toml_str).unwrap();
        assert_eq!(decoded.providers.len(), 3);
        assert_eq!(decoded.providers[0].provider_type(), "grok");
        assert_eq!(decoded.providers[1].provider_type(), "claude");
        assert_eq!(decoded.providers[2].provider_type(), "local");
    }

    #[test]
    fn local_model_descriptor_matches_daemon_bootstrap_formatting() {
        let gemma = ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Gemma2,
            model_size: ModelSize::Medium,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: None,
        };
        assert_eq!(
            gemma.local_model_descriptor().as_deref(),
            Some("Gemma 2 9b")
        );
        assert_eq!(gemma.profile_name(), "local-gemma-2-9b");

        let qwen = ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: None,
        };
        assert_eq!(
            qwen.local_model_descriptor().as_deref(),
            Some("Qwen 2.5 3B")
        );
        assert_eq!(qwen.profile_name(), "local-qwen-2.5-3b");

        assert_eq!(
            ProviderEntry::Claude {
                api_key: "sk-ant-test".to_string(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: None,
            }
            .local_model_descriptor(),
            None,
            "non-local entries must never claim a local descriptor"
        );
    }

    #[test]
    fn local_model_status_matches_rejects_a_different_already_ready_local_model() {
        let qwen = ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: None,
        };

        // The daemon reports its own family+size descriptor followed by
        // " (<engine>; requested <target>)" (see `load_generator_async` in
        // src/models/bootstrap.rs). A caller switching to `qwen` must accept
        // that exact shape...
        assert!(qwen.local_model_status_matches("Qwen 2.5 3B (LlamaCpp; requested Auto)"));
        // ...and the bare descriptor with no suffix...
        assert!(qwen.local_model_status_matches("Qwen 2.5 3B"));
        // ...but must reject a different model's descriptor, even with a
        // shared word prefix, which is the bug this test guards: treating
        // "some local model is ready" as "the requested local entry is
        // ready" reported the wrong model's name as if the switch to Qwen
        // had succeeded.
        assert!(!qwen.local_model_status_matches("Gemma 2 9b (LlamaCpp; requested Auto)"));
        assert!(!qwen.local_model_status_matches("Qwen 2.5 3B Instruct"));

        assert!(
            !ProviderEntry::Gemini {
                api_key: "gk-test".to_string(),
                model: None,
                name: None,
            }
            .local_model_status_matches("Qwen 2.5 3B"),
            "non-local entries must never match a reported local model status"
        );
    }
}
