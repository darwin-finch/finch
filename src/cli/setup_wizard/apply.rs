//! From wizard state to saved configuration.
//!
//! `build_setup_result` turns a `WizardState` into a `SetupResult`; the `validate_*` entry
//! points and `apply_setup_result_to_config` turn that `SetupResult` into a `crate::config`
//! `Config` and persist it.

use super::*;

/// Apply setup through the shared ceremony without a specific UI entry-point label.
///
/// New interactive entry points should call [`validate_and_apply_for`] explicitly so tests and
/// diagnostics can prove that first-run, command, and REPL setup use the same boundary.
pub async fn validate_and_apply(result: &SetupResult) -> Result<SetupApplyOutcome> {
    validate_and_apply_for(SetupInvocation::Command, result).await
}

/// Run the shared ceremony for automatic first-run setup.
pub async fn validate_first_run_and_apply(result: &SetupResult) -> Result<SetupApplyOutcome> {
    validate_and_apply_for(SetupInvocation::FirstRun, result).await
}

/// Run the shared ceremony for the explicit `finch setup` command.
pub async fn validate_command_and_apply(result: &SetupResult) -> Result<SetupApplyOutcome> {
    validate_and_apply_for(SetupInvocation::Command, result).await
}

/// Run the shared ceremony for the in-session `/setup` command.
pub async fn validate_repl_and_apply(result: &SetupResult) -> Result<SetupApplyOutcome> {
    validate_and_apply_for(SetupInvocation::Repl, result).await
}

/// Convert wizard output into the complete configuration written to disk.
///
/// Keep this mapping in one place so first-run setup, `finch setup`, and the
/// in-REPL `/setup` command cannot silently persist different subsets of the
/// settings shown by the wizard.
pub(super) fn config_from_setup_result(result: &SetupResult) -> crate::config::Config {
    use crate::config::Config;

    let providers = result.providers.clone();
    apply_setup_result_to_config(
        result,
        Config::with_providers(providers).with_credentials(result.credentials.clone()),
    )
}

#[cfg(test)]
pub(super) fn config_from_setup_result_with_paths(
    result: &SetupResult,
    metrics_dir: std::path::PathBuf,
    constitution_path: Option<std::path::PathBuf>,
) -> crate::config::Config {
    use crate::config::Config;

    let providers = result.providers.clone();
    apply_setup_result_to_config(
        result,
        Config::with_providers_and_paths(providers, metrics_dir, constitution_path)
            .with_credentials(result.credentials.clone()),
    )
}

pub(super) fn apply_setup_result_to_config(
    result: &SetupResult,
    mut new_config: crate::config::Config,
) -> crate::config::Config {
    use crate::config::FeaturesConfig;

    apply_daemon_api_key(&mut new_config, &result.finch_api_key);
    new_config.backend.coreml = result.coreml;
    new_config.active_theme = result.active_theme.clone();
    new_config.active_persona = result.default_persona.clone();
    if let Some(ref hf_tok) = result.hf_token {
        if !hf_tok.is_empty() {
            new_config.huggingface_token = Some(hf_tok.clone());
        }
    }
    new_config.features = FeaturesConfig {
        auto_approve_tools: result.auto_approve_tools,
        streaming_enabled: result.streaming_enabled,
        debug_logging: result.debug_logging,
        #[cfg(target_os = "macos")]
        gui_automation: result.gui_automation,
        #[cfg(target_os = "macos")]
        gui_automation_prompted: result.gui_automation_prompted,
        #[cfg(target_os = "macos")]
        gui_automation_last_known_available: result.gui_automation_last_known_available,
        #[cfg(target_os = "macos")]
        gui_automation_permission_context: result.gui_automation_permission_context.clone(),
        memory_context_lines: result.memory_context_lines,
        max_verbatim_messages: new_config.features.max_verbatim_messages,
        context_recall_k: new_config.features.context_recall_k,
        enable_summarization: new_config.features.enable_summarization,
        auto_compact_enabled: new_config.features.auto_compact_enabled,
    };
    new_config.server.mode = if result.daemon_only_mode {
        "daemon-only".to_string()
    } else {
        "full".to_string()
    };
    new_config.server.advertise = result.mdns_discovery;
    new_config.client.auto_discover = result.auto_discover;
    #[allow(deprecated)]
    {
        new_config.streaming_enabled = new_config.features.streaming_enabled;
    }
    new_config
}

/// Apply the wizard's single client key to the existing server representation.
pub fn apply_daemon_api_key(config: &mut crate::config::Config, api_key: &str) {
    let api_key = api_key.trim();
    config.server.auth_enabled = !api_key.is_empty();
    config.server.api_keys = if api_key.is_empty() {
        Vec::new()
    } else {
        vec![api_key.to_string()]
    };
}

/// Build the final SetupResult from wizard state
pub(super) fn build_setup_result(state: &WizardState) -> Result<SetupResult> {
    use crate::theme::ColorTheme;

    // Extract theme
    let active_theme = if let Some(SectionState::Themes { selected_theme }) =
        state.sections.get(&WizardSection::Themes)
    {
        let themes = ColorTheme::all();
        themes[*selected_theme].name().to_lowercase()
    } else {
        "dark".to_string()
    };

    // Extract models
    let (primary_model, tool_models) = if let Some(SectionState::Models {
        primary_model,
        tool_models,
        ..
    }) = state.sections.get(&WizardSection::Models)
    {
        (primary_model.clone(), tool_models.clone())
    } else {
        anyhow::bail!("Models not configured");
    };

    // Extract persona
    let (default_persona, custom_system_prompt) = if let Some(SectionState::Personas {
        available_personas,
        selected_idx,
        default_persona,
        ..
    }) =
        state.sections.get(&WizardSection::Personas)
    {
        let custom_prompt = available_personas.get(*selected_idx).and_then(|persona| {
            let builtin = crate::config::Persona::load_builtin(&persona.slug).ok()?;
            (persona.system_prompt != builtin.behavior.system_prompt)
                .then(|| persona.system_prompt.clone())
        });
        let selected_persona = available_personas
            .get(*selected_idx)
            .map(|persona| persona.slug.clone())
            .unwrap_or_else(|| default_persona.clone());
        (selected_persona, custom_prompt)
    } else {
        ("default".to_string(), None)
    };

    // Extract features
    let (
        auto_approve,
        streaming,
        debug,
        hf_token_val,
        finch_api_key_val,
        daemon_only,
        mdns,
        auto_disc,
        memory_ctx_lines,
    ) = if let Some(SectionState::Features {
        auto_approve,
        streaming,
        debug,
        hf_token,
        finch_api_key,
        daemon_only_mode,
        mdns_discovery,
        auto_discover,
        memory_context_lines,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        (
            *auto_approve,
            *streaming,
            *debug,
            if hf_token.is_empty() {
                None
            } else {
                Some(hf_token.clone())
            },
            finch_api_key.trim().to_string(),
            *daemon_only_mode,
            *mdns_discovery,
            *auto_discover,
            *memory_context_lines,
        )
    } else {
        (
            false,
            true,
            false,
            None,
            String::new(),
            false,
            false,
            true,
            4,
        )
    };

    #[cfg(target_os = "macos")]
    let gui_automation = if let Some(SectionState::Features { gui_automation, .. }) =
        state.sections.get(&WizardSection::Features)
    {
        *gui_automation
    } else {
        false
    };

    #[cfg(target_os = "macos")]
    let (
        gui_automation_prompted,
        gui_automation_last_known_available,
        gui_automation_permission_context,
    ) = if let Some(SectionState::Features {
        gui_automation_prompted,
        gui_automation_last_known_available,
        gui_automation_permission_context,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        (
            *gui_automation_prompted,
            *gui_automation_last_known_available,
            gui_automation_permission_context.clone(),
        )
    } else {
        (false, false, String::new())
    };

    // Map to backward-compatible fields
    let (
        claude_api_key,
        backend_enabled,
        inference_provider,
        execution_target,
        model_family,
        model_size,
    ) = match &primary_model {
        ModelConfig::Local {
            family,
            size,
            execution,
            inference_provider,
            ..
        } => (
            String::new(), // No API key for local
            true,
            *inference_provider,
            *execution,
            *family,
            *size,
        ),
        ModelConfig::Remote {
            provider: _,
            api_key,
            ..
        } => {
            // Remote API is primary - backend disabled
            (
                api_key.clone(),
                false,
                InferenceProvider::Onnx,
                ExecutionTarget::Cpu, // Placeholder
                ModelFamily::Qwen2,   // Placeholder
                ModelSize::Medium,    // Placeholder
            )
        }
    };

    // Build teachers list from primary + tool models
    let mut teachers: Vec<TeacherEntry> = Vec::new();

    // Primary model as first teacher (if remote)
    if let ModelConfig::Remote {
        provider,
        name,
        api_key,
        model,
        ..
    } = &primary_model
    {
        teachers.push(TeacherEntry {
            provider: provider.clone(),
            api_key: api_key.clone(),
            model: if model.is_empty() {
                None
            } else {
                Some(model.clone())
            },
            base_url: None,
            name: Some(name.clone()),
        });
    }

    // Tool models as additional teachers
    for tool_model in &tool_models {
        if let ModelConfig::Remote {
            provider,
            name,
            api_key,
            model,
            enabled,
            ..
        } = tool_model
        {
            if *enabled {
                teachers.push(TeacherEntry {
                    provider: provider.clone(),
                    api_key: api_key.clone(),
                    model: if model.is_empty() {
                        None
                    } else {
                        Some(model.clone())
                    },
                    base_url: None,
                    name: Some(name.clone()),
                });
            }
        }
    }

    // A profile name is the stable `/model <name>` selector. Keep generated
    // names unique even when the same provider/model is added more than once.
    let mut used_names: HashMap<String, usize> = HashMap::new();
    for teacher in &mut teachers {
        let base = teacher
            .name
            .clone()
            .unwrap_or_else(|| teacher.provider.clone());
        let count = used_names.entry(base.to_ascii_lowercase()).or_default();
        *count += 1;
        if *count > 1 {
            teacher.name = Some(format!("{}-{}", base, count));
        }
    }

    // Rebuild the unified provider list in the exact order shown. Remote
    // models have already been normalized in `teachers`; local models must be
    // emitted directly because they have no teacher representation.
    let providers: Vec<ProviderEntry> = std::iter::once(&primary_model)
        .chain(tool_models.iter())
        .enumerate()
        .filter_map(|(index, model)| match model {
            ModelConfig::Remote {
                provider,
                name,
                api_key,
                model,
                enabled,
                persisted,
            } if index == 0 || *enabled => Some(provider_entry_from_remote_model(
                provider,
                name,
                api_key,
                model,
                persisted.as_ref(),
            )),
            ModelConfig::Remote { .. } => None,
            ModelConfig::Local {
                family,
                size,
                execution,
                inference_provider,
                enabled,
                persisted,
            } => {
                let (name, model_repo, model_path) = match persisted {
                    Some(ProviderEntry::Local {
                        name,
                        model_repo,
                        model_path,
                        ..
                    }) => (name.clone(), model_repo.clone(), model_path.clone()),
                    _ => (
                        Some(format!(
                            "local-{}-{}",
                            family.name().to_ascii_lowercase().replace(' ', "-"),
                            size.to_size_string(*family)
                                .to_ascii_lowercase()
                                .replace(' ', "-")
                        )),
                        None,
                        None,
                    ),
                };
                Some(ProviderEntry::Local {
                    inference_provider: *inference_provider,
                    execution_target: *execution,
                    model_family: *family,
                    model_size: *size,
                    model_repo,
                    model_path,
                    enabled: *enabled,
                    name,
                })
            }
        })
        .collect();

    Ok(SetupResult {
        active_theme,
        primary_model,
        tool_models,
        providers,
        credentials: state.credentials.clone(),
        claude_api_key,
        hf_token: hf_token_val,
        backend_enabled,
        inference_provider,
        execution_target,
        coreml: state.coreml,
        model_family,
        model_size,
        custom_model_repo: None,
        teachers,
        finch_api_key: finch_api_key_val,
        default_persona,
        custom_system_prompt,
        auto_approve_tools: auto_approve,
        streaming_enabled: streaming,
        debug_logging: debug,
        #[cfg(target_os = "macos")]
        gui_automation,
        #[cfg(target_os = "macos")]
        gui_automation_prompted,
        #[cfg(target_os = "macos")]
        gui_automation_last_known_available,
        #[cfg(target_os = "macos")]
        gui_automation_permission_context,
        daemon_only_mode: daemon_only,
        mdns_discovery: mdns,
        auto_discover: auto_disc,
        memory_context_lines: memory_ctx_lines,
    })
}
