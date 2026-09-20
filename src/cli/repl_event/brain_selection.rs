//! Resolve the effective provider/model/thinking identity for one Brain.

use anyhow::{bail, Result};

use crate::brain::BrainProviderSelection;
use crate::config::{ProviderEntry, ReasoningEffort};

/// Where the effective provider/model identity came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSource {
    /// Copied from the configured global default at Brain creation.
    Inherited,
    /// Explicit per-Brain overlay (`/provider`, `/model`, `/thinking`).
    Override,
    /// Process-only CLI `--model` that must not be written to the Brain.
    OneShot,
}

/// Secret-free identity the status line, `/status`, and `/model` all project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSelection {
    pub provider_index: usize,
    pub provider_name: String,
    pub display_name: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub source: SelectionSource,
    pub local: bool,
}

impl EffectiveSelection {
    /// Compact status-rule / banner form, e.g. `ChatGPT · gpt-5.6-sol`.
    pub fn identity_label(&self) -> String {
        let model = self
            .model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or(if self.local { "local" } else { "default" });
        let mut label = format!("{} · {}", self.display_name, model);
        match self.source {
            SelectionSource::Inherited => label.push_str(" · inherited"),
            SelectionSource::Override => {}
            SelectionSource::OneShot => label.push_str(" · one-shot"),
        }
        if let Some(effort) = self.reasoning_effort {
            if !self.local {
                label.push_str(" · ");
                label.push_str(effort.as_str());
            }
        }
        label
    }

    pub fn status_report(&self, global_default: Option<&str>) -> String {
        let mut lines = vec![
            format!("provider: {}", self.provider_name),
            format!(
                "model: {}",
                self.model.as_deref().unwrap_or("provider default")
            ),
            format!(
                "thinking: {}",
                self.reasoning_effort
                    .map(ReasoningEffort::as_str)
                    .unwrap_or(if self.local {
                        "unsupported"
                    } else {
                        "provider default"
                    })
            ),
            format!(
                "source: {}",
                match self.source {
                    SelectionSource::Inherited => "inherited global default",
                    SelectionSource::Override => "Brain override",
                    SelectionSource::OneShot => "CLI --model (this invocation only)",
                }
            ),
        ];
        if let Some(default) = global_default {
            lines.insert(0, format!("global default: {default}"));
        }
        lines.join("\n")
    }
}

/// Inputs used to resolve one Brain's effective provider/model.
#[derive(Debug, Clone, Default)]
pub struct SelectionRequest {
    pub default_provider: Option<String>,
    pub persisted: BrainProviderSelection,
    /// Persist this named provider entry on the Brain.
    pub cli_provider: Option<String>,
    /// One-shot model overlay; never written to metadata.
    pub cli_model: Option<String>,
}

/// Pick the configured entry and overlays. Unknown names fail visibly.
pub fn resolve_selection(
    providers: &[ProviderEntry],
    request: &SelectionRequest,
) -> Result<EffectiveSelection> {
    if providers.is_empty() {
        bail!("No provider entries are configured. Run `finch setup` or `/config`.");
    }

    let (provider_index, source) = if let Some(name) = request.cli_provider.as_deref() {
        (
            index_of_provider(providers, name)?,
            SelectionSource::Override,
        )
    } else if let Some(name) = request.persisted.provider.as_deref() {
        let source = if request.persisted.provider_inherited && request.cli_model.is_none() {
            SelectionSource::Inherited
        } else {
            SelectionSource::Override
        };
        (index_of_provider(providers, name)?, source)
    } else if let Some(name) = request.default_provider.as_deref() {
        (
            index_of_provider(providers, name)?,
            SelectionSource::Inherited,
        )
    } else {
        (0, SelectionSource::Inherited)
    };

    let entry = &providers[provider_index];
    let mut source = source;
    let model = if let Some(model) = request
        .cli_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        if entry.is_local() {
            bail!(
                "Local provider '{}' has no ChatGPT-style model picker; family/size lives on the provider entry. Use /provider to switch entries.",
                entry.profile_name()
            );
        }
        source = SelectionSource::OneShot;
        Some(model.to_string())
    } else if let Some(model) = request
        .persisted
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        if entry.is_local() {
            None
        } else {
            if source == SelectionSource::Inherited {
                source = SelectionSource::Override;
            }
            Some(model.to_string())
        }
    } else {
        entry.model().map(str::to_string)
    };

    let reasoning_effort = if entry.supports_reasoning_effort() {
        request
            .persisted
            .reasoning_effort
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(parse_reasoning_effort)
            .transpose()?
            .or_else(|| entry.reasoning_effort())
    } else {
        None
    };

    Ok(EffectiveSelection {
        provider_index,
        provider_name: entry.profile_name(),
        display_name: entry.display_name().to_string(),
        model,
        reasoning_effort,
        source,
        local: entry.is_local(),
    })
}

/// Overlay to persist after a successful in-session change.
pub fn persistable_selection(
    effective: &EffectiveSelection,
    request: &SelectionRequest,
) -> BrainProviderSelection {
    let mut selection = request.persisted.clone();
    selection.provider = Some(effective.provider_name.clone());
    // A one-shot model changes `effective.source`, but it must not turn an
    // inherited provider binding into an explicit override. Preserve the
    // underlying provider provenance independently from the temporary model.
    selection.provider_inherited = request.cli_provider.is_none()
        && (request.persisted.provider_inherited
            || (request.persisted.provider.is_none()
                && request.persisted.model.is_none()
                && request.persisted.reasoning_effort.is_none()));
    // `effective` also contains defaults supplied by the provider entry. Do
    // not materialize those as Brain overlays: callers put only deliberate
    // `/model` and `/thinking` values into `request.persisted`.
    selection
}

pub fn parse_reasoning_effort(value: &str) -> Result<ReasoningEffort> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Ok(ReasoningEffort::None),
        "minimal" => Ok(ReasoningEffort::Minimal),
        "low" => Ok(ReasoningEffort::Low),
        "medium" => Ok(ReasoningEffort::Medium),
        "high" => Ok(ReasoningEffort::High),
        "xhigh" => Ok(ReasoningEffort::Xhigh),
        "max" => Ok(ReasoningEffort::Max),
        other => bail!(
            "Unknown thinking level '{other}'. Use none, minimal, low, medium, high, xhigh, or max."
        ),
    }
}

fn index_of_provider(providers: &[ProviderEntry], name: &str) -> Result<usize> {
    let matches: Vec<usize> = providers
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.profile_name().eq_ignore_ascii_case(name))
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => bail!(
            "Unknown provider profile '{name}'. Run /providers to inspect configured entries."
        ),
        _ => bail!(
            "Provider profile '{name}' is ambiguous; give these entries unique names in config."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grok(name: &str, model: &str) -> ProviderEntry {
        ProviderEntry::Grok {
            api_key: "xai-test".into(),
            model: Some(model.into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some(name.into()),
        }
    }

    fn claude(name: &str, model: &str) -> ProviderEntry {
        ProviderEntry::Claude {
            api_key: "sk-ant-test".into(),
            model: Some(model.into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some(name.into()),
        }
    }

    #[test]
    fn test_new_brain_inherits_named_global_default() {
        let providers = vec![
            grok("fast", "grok-code-fast-1"),
            claude("review", "claude-sonnet"),
        ];
        let effective = resolve_selection(
            &providers,
            &SelectionRequest {
                default_provider: Some("review".into()),
                ..SelectionRequest::default()
            },
        )
        .unwrap();
        assert_eq!(effective.provider_name, "review");
        assert_eq!(effective.model.as_deref(), Some("claude-sonnet"));
        assert_eq!(effective.source, SelectionSource::Inherited);
        assert!(
            effective.identity_label().contains("inherited"),
            "inherited identity must be visible: {}",
            effective.identity_label()
        );
    }

    #[test]
    fn test_brain_override_beats_global_default() {
        let providers = vec![
            grok("fast", "grok-code-fast-1"),
            claude("review", "claude-sonnet"),
        ];
        let effective = resolve_selection(
            &providers,
            &SelectionRequest {
                default_provider: Some("review".into()),
                persisted: BrainProviderSelection {
                    provider: Some("fast".into()),
                    model: Some("grok-4.6".into()),
                    reasoning_effort: None,
                    provider_inherited: false,
                },
                ..SelectionRequest::default()
            },
        )
        .unwrap();
        assert_eq!(effective.provider_name, "fast");
        assert_eq!(effective.model.as_deref(), Some("grok-4.6"));
        assert_eq!(effective.source, SelectionSource::Override);
        assert!(
            !effective.identity_label().contains("inherited"),
            "persisted overlay must not look inherited: {}",
            effective.identity_label()
        );
    }

    #[test]
    fn test_cli_model_is_one_shot_and_does_not_persist_model() {
        let providers = vec![grok("fast", "grok-code-fast-1")];
        let request = SelectionRequest {
            default_provider: Some("fast".into()),
            persisted: BrainProviderSelection {
                provider: Some("fast".into()),
                model: None,
                reasoning_effort: None,
                provider_inherited: true,
            },
            cli_model: Some("grok-4.6".into()),
            ..SelectionRequest::default()
        };
        let effective = resolve_selection(&providers, &request).unwrap();
        assert_eq!(effective.model.as_deref(), Some("grok-4.6"));
        assert_eq!(effective.source, SelectionSource::OneShot);
        let persistable = persistable_selection(&effective, &request);
        assert_eq!(
            persistable.model, None,
            "--model must not write the overlay onto the Brain; persistable={persistable:?}"
        );
        assert_eq!(persistable.provider.as_deref(), Some("fast"));
        assert!(
            persistable.provider_inherited,
            "one-shot --model must preserve inherited provider provenance"
        );
    }

    #[test]
    fn test_inherited_provider_defaults_do_not_become_brain_overlays() {
        let providers = vec![grok("fast", "grok-code-fast-1")];
        let request = SelectionRequest {
            default_provider: Some("fast".into()),
            ..SelectionRequest::default()
        };
        let effective = resolve_selection(&providers, &request).unwrap();
        let persistable = persistable_selection(&effective, &request);
        assert_eq!(persistable.provider.as_deref(), Some("fast"));
        assert!(persistable.provider_inherited);
        assert_eq!(persistable.model, None);
        assert_eq!(persistable.reasoning_effort, None);

        let restored = resolve_selection(
            &providers,
            &SelectionRequest {
                persisted: persistable,
                ..request
            },
        )
        .unwrap();
        assert_eq!(restored.source, SelectionSource::Inherited);
        assert_eq!(restored.model.as_deref(), Some("grok-code-fast-1"));
    }

    #[test]
    fn test_unknown_provider_fails_visibly() {
        let providers = vec![grok("fast", "grok-code-fast-1")];
        let error = resolve_selection(
            &providers,
            &SelectionRequest {
                persisted: BrainProviderSelection {
                    provider: Some("missing".into()),
                    ..BrainProviderSelection::default()
                },
                ..SelectionRequest::default()
            },
        )
        .expect_err("unknown persisted provider must not silently fall back");
        assert!(
            error
                .to_string()
                .contains("Unknown provider profile 'missing'"),
            "missing provider must fail with a repairable name; error={error}"
        );
    }
}
