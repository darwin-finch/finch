//! Key handling, one function per wizard section.
//!
//! Themes, Models (including the add-provider and configure overlays), Personas, Features and
//! Review. Deliberately holds no drawing code and almost no dependencies: the whole file
//! reaches outside `cli` only for `crate::models` and `crate::config`.

use super::*;

/// Handle input for Themes section
pub(super) fn handle_themes_input(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
) -> Result<bool> {
    if let Some(SectionState::Themes { selected_theme }) =
        state.sections.get_mut(&WizardSection::Themes)
    {
        use crate::theme::ColorTheme;
        let themes = ColorTheme::all();

        match key.code {
            KeyCode::Up => {
                if *selected_theme > 0 {
                    *selected_theme -= 1;
                }
            }
            KeyCode::Down => {
                if *selected_theme < themes.len() - 1 {
                    *selected_theme += 1;
                }
            }
            KeyCode::Enter => {
                state.mark_completed(WizardSection::Themes);
                state.next_section();
            }
            KeyCode::Esc => {
                state.prev_section();
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Handle input for Models section (unified Backend + Teachers)
pub(super) fn handle_models_input(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
) -> Result<bool> {
    let catalog_cache_dir = state.catalog_cache_dir.clone();
    let credentials = state.credentials.clone();
    if let Some(SectionState::Models {
        primary_model,
        tool_models,
        selected_idx,
        editing_mode,
        editing_model_mode,
        model_input,
        adding_provider,
        catalog_models,
        catalog_model_provenance,
        catalog_source,
        catalog_refresh,
        catalog_generation,
        catalog_refreshed_at,
        catalog_error,
        error,
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        // Clear error on any input
        *error = None;

        // Handle add-provider overlay first
        if adding_provider.is_some() {
            // Build option lists used by ConfigureLocal cycling
            let local_backends: Vec<InferenceProvider> = {
                let mut v = vec![InferenceProvider::Onnx];
                #[cfg(feature = "candle")]
                v.push(InferenceProvider::Candle);
                v
            };
            // When Candle is selected, only Qwen 2.5 is currently supported
            let candle_selected = {
                #[cfg(feature = "candle")]
                {
                    matches!(
                        adding_provider,
                        Some(AddProviderStep::ConfigureLocal {
                            inference_provider: InferenceProvider::Candle,
                            ..
                        })
                    )
                }
                #[cfg(not(feature = "candle"))]
                {
                    false
                }
            };
            let local_families: Vec<ModelFamily> = if candle_selected {
                vec![ModelFamily::Qwen2]
            } else {
                vec![
                    ModelFamily::Qwen2,
                    ModelFamily::Gemma2,
                    ModelFamily::Llama3,
                    ModelFamily::Mistral,
                    ModelFamily::Phi,
                    ModelFamily::DeepSeek,
                ]
            };
            let local_sizes = [
                ModelSize::Small,
                ModelSize::Medium,
                ModelSize::Large,
                ModelSize::XLarge,
            ];
            let local_devices: Vec<ExecutionTarget> = {
                let mut v = vec![ExecutionTarget::Auto];
                #[cfg(target_os = "macos")]
                v.push(ExecutionTarget::CoreML);
                v.push(ExecutionTarget::Cpu);
                #[cfg(feature = "cuda")]
                v.push(ExecutionTarget::Cuda);
                v
            };

            match key.code {
                KeyCode::Esc => {
                    *adding_provider = None;
                    *catalog_generation = catalog_generation.wrapping_add(1);
                    *catalog_refresh = None;
                }
                KeyCode::Up => match adding_provider {
                    Some(AddProviderStep::SelectAddType { selected }) => {
                        if *selected > 0 {
                            *selected -= 1;
                        }
                    }
                    Some(AddProviderStep::ConfigureLocal { focused_field, .. }) => {
                        *focused_field = focused_field.saturating_sub(1);
                    }
                    Some(AddProviderStep::ConfigureRemote { focused_field, .. }) => {
                        *focused_field = focused_field.saturating_sub(1);
                    }
                    Some(AddProviderStep::SelectAgent { selected, .. }) => {
                        if *selected > 0 {
                            *selected -= 1;
                        }
                    }
                    _ => {}
                },
                KeyCode::Down => match adding_provider {
                    Some(AddProviderStep::SelectAddType { selected }) => {
                        if *selected < CLOUD_PROVIDERS.len() + 1 {
                            *selected += 1;
                        }
                    }
                    Some(AddProviderStep::ConfigureLocal { focused_field, .. }) => {
                        if *focused_field < 3 {
                            *focused_field += 1;
                        }
                    }
                    Some(AddProviderStep::ConfigureRemote {
                        api_key,
                        focused_field,
                        ..
                    }) => {
                        let last_field = if api_key.is_some() { 3 } else { 2 };
                        if *focused_field < last_field {
                            *focused_field += 1;
                        }
                    }
                    Some(AddProviderStep::SelectAgent { agents, selected }) => {
                        if *selected + 1 < agents.len() {
                            *selected += 1;
                        }
                    }
                    _ => {}
                },
                KeyCode::Left => {
                    match adding_provider {
                        Some(AddProviderStep::ConfigureLocal {
                            inference_provider,
                            family,
                            size,
                            execution,
                            focused_field,
                        }) => {
                            match *focused_field {
                                0 => {
                                    if let Some(pos) = local_backends
                                        .iter()
                                        .position(|x| *x == *inference_provider)
                                    {
                                        *inference_provider =
                                            local_backends[(pos + local_backends.len() - 1)
                                                % local_backends.len()];
                                    }
                                    // Candle only supports Qwen 2.5; reset family if needed
                                    #[cfg(feature = "candle")]
                                    if *inference_provider == InferenceProvider::Candle {
                                        *family = ModelFamily::Qwen2;
                                    }
                                }
                                1 => {
                                    if let Some(pos) =
                                        local_families.iter().position(|x| *x == *family)
                                    {
                                        *family = local_families[(pos + local_families.len() - 1)
                                            % local_families.len()];
                                    }
                                }
                                2 => {
                                    if let Some(pos) = local_sizes.iter().position(|x| *x == *size)
                                    {
                                        *size = local_sizes
                                            [(pos + local_sizes.len() - 1) % local_sizes.len()];
                                    }
                                }
                                3 => {
                                    if let Some(pos) =
                                        local_devices.iter().position(|x| *x == *execution)
                                    {
                                        *execution = local_devices
                                            [(pos + local_devices.len() - 1) % local_devices.len()];
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some(AddProviderStep::ConfigureRemote {
                            provider_idx,
                            model,
                            api_key,
                            focused_field,
                            ..
                        }) => {
                            match *focused_field {
                                0 => {
                                    let new_idx = (*provider_idx + CLOUD_PROVIDERS.len() - 1)
                                        % CLOUD_PROVIDERS.len();
                                    *provider_idx = new_idx;
                                    *api_key = remote_api_key_input(CLOUD_PROVIDERS[new_idx].0);
                                    // Reset model to default for new provider
                                    let default = CLOUD_PROVIDERS[new_idx].2;
                                    *model = default.to_string();
                                    *catalog_model_provenance = if default.is_empty() {
                                        ModelSelectionProvenance::Blank
                                    } else {
                                        ModelSelectionProvenance::DefaultGenerated
                                    };
                                    *catalog_models = known_models_for(CLOUD_PROVIDERS[new_idx].0);
                                    *catalog_source = CatalogSource::StaticFallback;
                                    *catalog_refreshed_at = None;
                                    *catalog_error = None;
                                    *catalog_generation = catalog_generation.wrapping_add(1);
                                    *catalog_refresh = None;
                                }
                                2 => {
                                    if !catalog_models.is_empty() {
                                        let pos = catalog_models
                                            .iter()
                                            .position(|m| m == model)
                                            .unwrap_or(0);
                                        let new_pos =
                                            (pos + catalog_models.len() - 1) % catalog_models.len();
                                        *model = catalog_models[new_pos].clone();
                                        *catalog_model_provenance =
                                            ModelSelectionProvenance::Cycled;
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                KeyCode::Right => {
                    match adding_provider {
                        Some(AddProviderStep::ConfigureLocal {
                            inference_provider,
                            family,
                            size,
                            execution,
                            focused_field,
                        }) => {
                            match *focused_field {
                                0 => {
                                    if let Some(pos) = local_backends
                                        .iter()
                                        .position(|x| *x == *inference_provider)
                                    {
                                        *inference_provider =
                                            local_backends[(pos + 1) % local_backends.len()];
                                    }
                                    // Candle only supports Qwen 2.5; reset family if needed
                                    #[cfg(feature = "candle")]
                                    if *inference_provider == InferenceProvider::Candle {
                                        *family = ModelFamily::Qwen2;
                                    }
                                }
                                1 => {
                                    if let Some(pos) =
                                        local_families.iter().position(|x| *x == *family)
                                    {
                                        *family = local_families[(pos + 1) % local_families.len()];
                                    }
                                }
                                2 => {
                                    if let Some(pos) = local_sizes.iter().position(|x| *x == *size)
                                    {
                                        *size = local_sizes[(pos + 1) % local_sizes.len()];
                                    }
                                }
                                3 => {
                                    if let Some(pos) =
                                        local_devices.iter().position(|x| *x == *execution)
                                    {
                                        *execution = local_devices[(pos + 1) % local_devices.len()];
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some(AddProviderStep::ConfigureRemote {
                            provider_idx,
                            model,
                            api_key,
                            focused_field,
                            ..
                        }) => {
                            match *focused_field {
                                0 => {
                                    let new_idx = (*provider_idx + 1) % CLOUD_PROVIDERS.len();
                                    *provider_idx = new_idx;
                                    *api_key = remote_api_key_input(CLOUD_PROVIDERS[new_idx].0);
                                    // Reset model to default for new provider
                                    let default = CLOUD_PROVIDERS[new_idx].2;
                                    *model = default.to_string();
                                    *catalog_model_provenance = if default.is_empty() {
                                        ModelSelectionProvenance::Blank
                                    } else {
                                        ModelSelectionProvenance::DefaultGenerated
                                    };
                                    *catalog_models = known_models_for(CLOUD_PROVIDERS[new_idx].0);
                                    *catalog_source = CatalogSource::StaticFallback;
                                    *catalog_refreshed_at = None;
                                    *catalog_error = None;
                                    *catalog_generation = catalog_generation.wrapping_add(1);
                                    *catalog_refresh = None;
                                }
                                2 => {
                                    if !catalog_models.is_empty() {
                                        let pos = catalog_models
                                            .iter()
                                            .position(|m| m == model)
                                            .unwrap_or(0);
                                        *model = catalog_models[(pos + 1) % catalog_models.len()]
                                            .clone();
                                        *catalog_model_provenance =
                                            ModelSelectionProvenance::Cycled;
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let Some(AddProviderStep::ConfigureRemote {
                        provider_idx,
                        name,
                        api_key,
                        model,
                        editing_idx,
                        ..
                    }) = adding_provider.as_ref()
                    else {
                        return Ok(false);
                    };
                    let persisted = editing_idx.and_then(|index| {
                        if index == 0 {
                            match primary_model {
                                ModelConfig::Remote { persisted, .. } => persisted.as_ref(),
                                ModelConfig::Local { .. } => None,
                            }
                        } else {
                            tool_models.get(index - 1).and_then(|model| match model {
                                ModelConfig::Remote { persisted, .. } => persisted.as_ref(),
                                ModelConfig::Local { .. } => None,
                            })
                        }
                    });
                    let provider_id = CLOUD_PROVIDERS[*provider_idx].0;
                    let Some(profile) = model_catalog_profile(
                        provider_id,
                        name,
                        api_key.as_deref().unwrap_or(""),
                        persisted,
                    ) else {
                        *catalog_error = Some(format!(
                            "{} does not advertise model discovery; enter a model ID manually",
                            CLOUD_PROVIDERS[*provider_idx].1
                        ));
                        return Ok(false);
                    };
                    let selected_entry = provider_entry_from_remote_model(
                        provider_id,
                        name,
                        api_key.as_deref().unwrap_or(""),
                        model,
                        persisted,
                    );
                    let named_config = matches!(selected_entry, ProviderEntry::Credentialed { .. })
                        .then(|| {
                            named_catalog_refresh_config(
                                primary_model,
                                tool_models,
                                editing_idx.unwrap_or(0),
                                &selected_entry,
                                credentials.clone(),
                            )
                        });
                    let named_profile = selected_entry.profile_name();
                    let cache_dir = match catalog_cache_dir.clone() {
                        Some(path) => path,
                        None => {
                            *catalog_error = Some(
                                "Cannot locate home directory for model catalogue cache"
                                    .to_string(),
                            );
                            return Ok(false);
                        }
                    };
                    let result: Arc<Mutex<CatalogRefreshResult>> = Arc::new(Mutex::new(None));
                    let result_for_thread = Arc::clone(&result);
                    *catalog_generation = catalog_generation.wrapping_add(1);
                    let generation = *catalog_generation;
                    let selection_identity = model_catalog::profile_cache_identity(&profile);
                    std::thread::spawn(move || {
                        let refreshed = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .map_err(anyhow::Error::from)
                            .map(|runtime| {
                                if let Some(config) = named_config {
                                    runtime.block_on(async {
                                        match model_catalog::refresh_from_config(
                                            &config,
                                            &named_profile,
                                            &crate::config::EnvironmentCredentialResolver,
                                            &cache_dir,
                                        )
                                        .await
                                        {
                                            Ok(catalog) => (catalog, None),
                                            Err(error) => {
                                                let mut fallback = model_catalog::fallback_catalog(
                                                    &profile.provider,
                                                    &profile.endpoints.models_url,
                                                );
                                                fallback.profile_id = profile.profile_id.clone();
                                                (fallback, Some(error.to_string()))
                                            }
                                        }
                                    })
                                } else {
                                    runtime.block_on(model_catalog::refresh_with_fallback(
                                        &profile, &cache_dir,
                                    ))
                                }
                            });
                        *result_for_thread.lock().unwrap() = Some(match refreshed {
                            Ok(result) => result,
                            Err(_) => {
                                let mut fallback = model_catalog::fallback_catalog(
                                    &profile.provider,
                                    &profile.endpoints.models_url,
                                );
                                fallback.profile_id = profile.profile_id.clone();
                                (
                                    fallback,
                                    Some(
                                        "Could not initialize model catalogue refresh".to_string(),
                                    ),
                                )
                            }
                        });
                    });
                    *catalog_refresh = Some(CatalogRefresh {
                        generation,
                        selection_identity,
                        result,
                    });
                    *catalog_error = None;
                }
                KeyCode::Char(c) => {
                    if let Some(AddProviderStep::ConfigureRemote {
                        provider_idx,
                        name,
                        model,
                        api_key,
                        focused_field,
                        ..
                    }) = adding_provider
                    {
                        match *focused_field {
                            1 => {
                                name.push(c);
                                *catalog_generation = catalog_generation.wrapping_add(1);
                                *catalog_refresh = None;
                            }
                            2 => {
                                model.push(c);
                                *catalog_model_provenance = ModelSelectionProvenance::Manual;
                            }
                            3 => {
                                if let Some(api_key) = api_key.as_mut() {
                                    api_key.push(c);
                                    *catalog_generation = catalog_generation.wrapping_add(1);
                                    *catalog_refresh = None;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                KeyCode::Backspace => {
                    if let Some(AddProviderStep::ConfigureRemote {
                        provider_idx,
                        name,
                        model,
                        api_key,
                        focused_field,
                        ..
                    }) = adding_provider
                    {
                        match *focused_field {
                            1 => {
                                name.pop();
                                *catalog_generation = catalog_generation.wrapping_add(1);
                                *catalog_refresh = None;
                            }
                            2 => {
                                model.pop();
                                *catalog_model_provenance = if model.is_empty() {
                                    ModelSelectionProvenance::Blank
                                } else {
                                    ModelSelectionProvenance::Manual
                                };
                            }
                            3 => {
                                if let Some(api_key) = api_key.as_mut() {
                                    api_key.pop();
                                    *catalog_generation = catalog_generation.wrapping_add(1);
                                    *catalog_refresh = None;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                KeyCode::Enter => {
                    // Ignore Enter while network scan is in progress
                    if matches!(adding_provider, Some(AddProviderStep::Scanning { .. })) {
                        return Ok(false);
                    }

                    let next_step = match adding_provider.take() {
                        // ── type selection ──────────────────────────────────────────────
                        Some(AddProviderStep::SelectAddType { selected }) => {
                            let n_cloud = CLOUD_PROVIDERS.len();
                            if selected < n_cloud {
                                // Open single-screen remote dialog pre-selected to this provider
                                let default_model = CLOUD_PROVIDERS[selected].2.to_string();
                                *catalog_model_provenance = if default_model.is_empty() {
                                    ModelSelectionProvenance::Blank
                                } else {
                                    ModelSelectionProvenance::DefaultGenerated
                                };
                                *catalog_models = known_models_for(CLOUD_PROVIDERS[selected].0);
                                *catalog_source = CatalogSource::StaticFallback;
                                *catalog_refreshed_at = None;
                                *catalog_error = None;
                                *catalog_generation = catalog_generation.wrapping_add(1);
                                *catalog_refresh = None;
                                if let (Some(profile), Some(cache_dir)) = (
                                    model_catalog_profile(
                                        CLOUD_PROVIDERS[selected].0,
                                        CLOUD_PROVIDERS[selected].0,
                                        "",
                                        None,
                                    ),
                                    catalog_cache_dir.clone(),
                                ) {
                                    if let Ok(Some(cached)) =
                                        model_catalog::read_cache(&profile, &cache_dir)
                                    {
                                        *catalog_models = cached.models;
                                        *catalog_source = CatalogSource::Cache;
                                        *catalog_refreshed_at = Some(cached.refreshed_at);
                                    }
                                }
                                Some(AddProviderStep::ConfigureRemote {
                                    provider_idx: selected,
                                    name: CLOUD_PROVIDERS[selected].0.to_string(),
                                    model: default_model,
                                    api_key: remote_api_key_input(CLOUD_PROVIDERS[selected].0),
                                    focused_field: if CLOUD_PROVIDERS[selected].0 == "chatgpt" {
                                        1
                                    } else {
                                        3
                                    },
                                    editing_idx: None,
                                })
                            } else if selected == n_cloud {
                                // Open single-screen local model dialog
                                Some(AddProviderStep::ConfigureLocal {
                                    inference_provider: InferenceProvider::Onnx,
                                    family: ModelFamily::Qwen2,
                                    size: ModelSize::Medium,
                                    execution: ExecutionTarget::Auto,
                                    focused_field: 0,
                                })
                            } else {
                                // Network scan
                                let results_arc: Arc<Mutex<Option<Vec<DiscoveredService>>>> =
                                    Arc::new(Mutex::new(None));
                                let arc_clone = Arc::clone(&results_arc);
                                std::thread::spawn(move || {
                                    if let Ok(client) = ServiceDiscoveryClient::new() {
                                        let found = client
                                            .discover(Duration::from_secs(5))
                                            .unwrap_or_default();
                                        *arc_clone.lock().unwrap() = Some(found);
                                    } else {
                                        *arc_clone.lock().unwrap() = Some(vec![]);
                                    }
                                });
                                Some(AddProviderStep::Scanning {
                                    results: results_arc,
                                })
                            }
                        }
                        // ── single-screen remote dialog — confirm ────────────────────
                        Some(AddProviderStep::ConfigureRemote {
                            provider_idx,
                            name,
                            model,
                            api_key,
                            editing_idx,
                            ..
                        }) => {
                            let (provider_id, _, default_model, _) =
                                CLOUD_PROVIDERS[provider_idx.min(CLOUD_PROVIDERS.len() - 1)];
                            let resolved_model = if model.is_empty() {
                                default_model.to_string()
                            } else {
                                model
                            };
                            if resolved_model.trim().is_empty() {
                                *catalog_error = Some(
                                    "Refresh the authenticated catalogue with Ctrl+R or enter a model ID manually"
                                        .to_string(),
                                );
                                Some(AddProviderStep::ConfigureRemote {
                                    provider_idx,
                                    name,
                                    model: resolved_model,
                                    api_key,
                                    focused_field: 2,
                                    editing_idx,
                                })
                            } else {
                                let persisted = editing_idx
                                    .and_then(|index| {
                                        if index == 0 {
                                            match &*primary_model {
                                                ModelConfig::Remote { persisted, .. } => {
                                                    persisted.clone()
                                                }
                                                ModelConfig::Local { .. } => None,
                                            }
                                        } else {
                                            tool_models.get(index - 1).and_then(|model| match model
                                            {
                                                ModelConfig::Remote { persisted, .. } => {
                                                    persisted.clone()
                                                }
                                                ModelConfig::Local { .. } => None,
                                            })
                                        }
                                    })
                                    .filter(|entry| entry.provider_type() == provider_id);
                                let edited = ModelConfig::Remote {
                                    provider: provider_id.to_string(),
                                    name: if name.trim().is_empty() {
                                        provider_id.to_string()
                                    } else {
                                        name.trim().to_string()
                                    },
                                    api_key: api_key.unwrap_or_default(),
                                    model: resolved_model,
                                    enabled: true,
                                    persisted,
                                };
                                if let Some(index) = editing_idx {
                                    if index == 0 {
                                        *primary_model = edited;
                                    } else if let Some(slot) = tool_models.get_mut(index - 1) {
                                        let enabled = slot.enabled();
                                        *slot = edited;
                                        slot.set_enabled(enabled);
                                    }
                                    *selected_idx = index;
                                } else if tool_models.is_empty()
                                    && is_unconfigured_placeholder(primary_model)
                                {
                                    *primary_model = edited;
                                    *selected_idx = 0;
                                } else {
                                    tool_models.push(edited);
                                    *selected_idx = tool_models.len();
                                }
                                None
                            }
                        }
                        // ── single-screen local dialog — confirm ─────────────────────
                        Some(AddProviderStep::ConfigureLocal {
                            inference_provider,
                            family,
                            size,
                            execution,
                            ..
                        }) => {
                            let replace_primary = tool_models.is_empty()
                                && is_unconfigured_placeholder(primary_model);
                            if replace_primary {
                                *primary_model = ModelConfig::Local {
                                    family,
                                    size,
                                    execution,
                                    inference_provider,
                                    enabled: true,
                                    persisted: None,
                                };
                                *selected_idx = 0;
                            } else {
                                tool_models.push(ModelConfig::Local {
                                    family,
                                    size,
                                    execution,
                                    inference_provider,
                                    enabled: true,
                                    persisted: None,
                                });
                                *selected_idx = tool_models.len();
                            }
                            None
                        }
                        // ── network scan results ─────────────────────────────────────
                        Some(AddProviderStep::SelectAgent { agents, selected }) => {
                            if !agents.is_empty() {
                                let agent = &agents[selected.min(agents.len() - 1)];
                                tool_models.push(ModelConfig::Remote {
                                    provider: "finch".to_string(),
                                    name: agent.name.clone(),
                                    api_key: String::new(),
                                    model: format!("{}:{}", agent.host, agent.port),
                                    enabled: true,
                                    persisted: None,
                                });
                                *selected_idx = tool_models.len();
                            }
                            None
                        }
                        None => None,
                        // Scanning handled above with early return
                        Some(AddProviderStep::Scanning { .. }) => None,
                    };
                    *adding_provider = next_step;
                }
                _ => {}
            }
            return Ok(false);
        }

        if *editing_model_mode {
            // Editing model name for the selected entry
            match key.code {
                KeyCode::Char(c) => {
                    model_input.push(c);
                }
                KeyCode::Backspace => {
                    model_input.pop();
                }
                KeyCode::Enter | KeyCode::Esc => {
                    // Save model name
                    let mi = model_input.clone();
                    if *selected_idx == 0 {
                        if let ModelConfig::Remote { model, .. } = primary_model {
                            *model = mi;
                        }
                    } else {
                        let tool_idx = *selected_idx - 1;
                        if let Some(ModelConfig::Remote { model, .. }) =
                            tool_models.get_mut(tool_idx)
                        {
                            *model = mi;
                        }
                    }
                    *editing_model_mode = false;
                    model_input.clear();
                }
                _ => {}
            }
        } else if *editing_mode {
            // In API key editing mode
            let accepts_api_key = if *selected_idx == 0 {
                primary_model.accepts_api_key()
            } else {
                tool_models
                    .get(*selected_idx - 1)
                    .is_some_and(ModelConfig::accepts_api_key)
            };
            if !accepts_api_key {
                *editing_mode = false;
                *error = Some(
                    "ChatGPT subscription uses a named Finch device credential, not an API key"
                        .into(),
                );
                return Ok(false);
            }
            match key.code {
                KeyCode::Char(c) => {
                    if *selected_idx == 0 {
                        if let ModelConfig::Remote { api_key, .. } = primary_model {
                            api_key.push(c);
                        }
                    } else {
                        let tool_idx = *selected_idx - 1;
                        if let Some(ModelConfig::Remote { api_key, .. }) =
                            tool_models.get_mut(tool_idx)
                        {
                            api_key.push(c);
                        }
                    }
                }
                KeyCode::Backspace => {
                    if *selected_idx == 0 {
                        if let ModelConfig::Remote { api_key, .. } = primary_model {
                            api_key.pop();
                        }
                    } else {
                        let tool_idx = *selected_idx - 1;
                        if let Some(ModelConfig::Remote { api_key, .. }) =
                            tool_models.get_mut(tool_idx)
                        {
                            api_key.pop();
                        }
                    }
                }
                KeyCode::Enter | KeyCode::Esc => {
                    *editing_mode = false;
                }
                _ => {}
            }
        } else {
            // Navigation mode
            match key.code {
                KeyCode::Up => {
                    if *selected_idx > 0 {
                        *selected_idx -= 1;
                    }
                }
                KeyCode::Down => {
                    let total = 1 + tool_models.len();
                    if *selected_idx < total - 1 {
                        *selected_idx += 1;
                    }
                }
                KeyCode::Char(' ') => {
                    // Toggle enabled for tool models
                    if *selected_idx > 0 {
                        let tool_idx = *selected_idx - 1;
                        if let Some(model) = tool_models.get_mut(tool_idx) {
                            model.set_enabled(!model.enabled());
                        }
                    }
                }
                KeyCode::Enter | KeyCode::Char('e') | KeyCode::Char('E') => {
                    let selected = if *selected_idx == 0 {
                        Some(&*primary_model)
                    } else {
                        tool_models.get(*selected_idx - 1)
                    };
                    if let Some(ModelConfig::Remote {
                        provider,
                        name,
                        model,
                        api_key,
                        persisted,
                        ..
                    }) = selected
                    {
                        let provider_idx = CLOUD_PROVIDERS
                            .iter()
                            .position(|(id, ..)| *id == provider)
                            .unwrap_or(0);
                        *catalog_models = known_models_for(CLOUD_PROVIDERS[provider_idx].0);
                        *catalog_model_provenance = if model.trim().is_empty() {
                            ModelSelectionProvenance::Blank
                        } else {
                            ModelSelectionProvenance::Persisted
                        };
                        *catalog_source = CatalogSource::StaticFallback;
                        *catalog_refreshed_at = None;
                        *catalog_error = None;
                        *catalog_generation = catalog_generation.wrapping_add(1);
                        *catalog_refresh = None;
                        if let (Some(profile), Some(cache_dir)) = (
                            model_catalog_profile(provider, name, api_key, persisted.as_ref()),
                            catalog_cache_dir.clone(),
                        ) {
                            if let Ok(Some(cached)) =
                                model_catalog::read_cache(&profile, &cache_dir)
                            {
                                *catalog_models = cached.models;
                                *catalog_source = CatalogSource::Cache;
                                *catalog_refreshed_at = Some(cached.refreshed_at);
                            }
                        }
                        *adding_provider = Some(AddProviderStep::ConfigureRemote {
                            provider_idx,
                            name: name.clone(),
                            model: model.clone(),
                            api_key: if provider.eq_ignore_ascii_case("chatgpt") {
                                None
                            } else {
                                Some(api_key.clone())
                            },
                            focused_field: 1,
                            editing_idx: Some(*selected_idx),
                        });
                    } else {
                        *error = Some(
                            "Local-model editing is available when adding a replacement".into(),
                        );
                    }
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    // Open add-provider overlay (type selection first)
                    *adding_provider = Some(AddProviderStep::SelectAddType { selected: 0 });
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    // Delete selected tool model (cannot delete primary)
                    if *selected_idx > 0 {
                        let tool_idx = *selected_idx - 1;
                        if tool_idx < tool_models.len() {
                            tool_models.remove(tool_idx);
                            // Adjust selection
                            if *selected_idx > tool_models.len() {
                                *selected_idx = tool_models.len();
                            }
                        }
                    }
                }
                KeyCode::Char('p') | KeyCode::Char('P') => {
                    // Promote selected tool to primary (swap with current primary)
                    if *selected_idx > 0 {
                        let tool_idx = *selected_idx - 1;
                        if tool_idx < tool_models.len() {
                            std::mem::swap(primary_model, &mut tool_models[tool_idx]);
                        }
                    }
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    // Skip - just move to next section without validation
                    state.mark_completed(WizardSection::Models);
                    state.next_section();
                }
                KeyCode::Tab => {
                    // Always allow advancing — no key format validation
                    state.mark_completed(WizardSection::Models);
                    state.next_section();
                }
                KeyCode::Esc => {
                    state.prev_section();
                }
                _ => {}
            }
        }
    }
    Ok(false)
}

/// Handle input for Personas section
pub(super) fn handle_personas_input(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
) -> Result<bool> {
    if let Some(SectionState::Personas {
        available_personas,
        selected_idx,
        default_persona,
        editing_prompt,
        prompt_input,
        cursor_pos,
    }) = state.sections.get_mut(&WizardSection::Personas)
    {
        if *editing_prompt {
            // Helper: convert char index to byte offset
            let char_to_byte = |s: &str, char_idx: usize| -> usize {
                s.char_indices()
                    .nth(char_idx)
                    .map(|(b, _)| b)
                    .unwrap_or(s.len())
            };

            match key.code {
                KeyCode::Char('s')
                    if key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL) =>
                {
                    let new_prompt = prompt_input.clone();
                    if let Some(persona) = available_personas.get_mut(*selected_idx) {
                        persona.system_prompt = new_prompt;
                    }
                    *editing_prompt = false;
                }
                KeyCode::Esc => {
                    *editing_prompt = false;
                    prompt_input.clear();
                    *cursor_pos = 0;
                }
                KeyCode::Enter => {
                    let byte = char_to_byte(prompt_input, *cursor_pos);
                    prompt_input.insert(byte, '\n');
                    *cursor_pos += 1;
                }
                KeyCode::Backspace => {
                    if *cursor_pos > 0 {
                        *cursor_pos -= 1;
                        let byte = char_to_byte(prompt_input, *cursor_pos);
                        prompt_input.remove(byte);
                    }
                }
                KeyCode::Delete => {
                    let len = prompt_input.chars().count();
                    if *cursor_pos < len {
                        let byte = char_to_byte(prompt_input, *cursor_pos);
                        prompt_input.remove(byte);
                    }
                }
                KeyCode::Left => {
                    if *cursor_pos > 0 {
                        *cursor_pos -= 1;
                    }
                }
                KeyCode::Right => {
                    let len = prompt_input.chars().count();
                    if *cursor_pos < len {
                        *cursor_pos += 1;
                    }
                }
                KeyCode::Up => {
                    // Move cursor to the same column on the line above
                    let before: String = prompt_input.chars().take(*cursor_pos).collect();
                    let col = before
                        .rfind('\n')
                        .map(|i| before[i + 1..].chars().count())
                        .unwrap_or(before.chars().count());
                    if let Some(prev_nl) = before.rfind('\n') {
                        let line_before_prev = &before[..prev_nl];
                        let prev_line_len = line_before_prev
                            .rfind('\n')
                            .map(|i| line_before_prev[i + 1..].chars().count())
                            .unwrap_or(line_before_prev.chars().count());
                        let new_col = col.min(prev_line_len);
                        *cursor_pos = line_before_prev.chars().count() + 1 + new_col;
                    } else {
                        *cursor_pos = 0;
                    }
                }
                KeyCode::Down => {
                    let before: String = prompt_input.chars().take(*cursor_pos).collect();
                    let col = before
                        .rfind('\n')
                        .map(|i| before[i + 1..].chars().count())
                        .unwrap_or(before.chars().count());
                    let after: String = prompt_input.chars().skip(*cursor_pos).collect();
                    if let Some(next_nl) = after.find('\n') {
                        let before_count =
                            prompt_input.chars().take(*cursor_pos + next_nl + 1).count();
                        let next_line: String = prompt_input.chars().skip(before_count).collect();
                        let next_line_len = next_line
                            .find('\n')
                            .map(|i| next_line[..i].chars().count())
                            .unwrap_or(next_line.chars().count());
                        let new_col = col.min(next_line_len);
                        *cursor_pos = before_count + new_col;
                    } else {
                        *cursor_pos = prompt_input.chars().count();
                    }
                }
                KeyCode::Home => {
                    let before: String = prompt_input.chars().take(*cursor_pos).collect();
                    *cursor_pos = if let Some(last_nl) = before.rfind('\n') {
                        before[..last_nl].chars().count() + 1
                    } else {
                        0
                    };
                }
                KeyCode::End => {
                    let after: String = prompt_input.chars().skip(*cursor_pos).collect();
                    let to_eol = after.find('\n').unwrap_or(after.chars().count());
                    *cursor_pos += to_eol;
                }
                KeyCode::Char(c) => {
                    let byte = char_to_byte(prompt_input, *cursor_pos);
                    prompt_input.insert(byte, c);
                    *cursor_pos += 1;
                }
                _ => {}
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Up => {
                if *selected_idx > 0 {
                    *selected_idx -= 1;
                }
            }
            KeyCode::Down => {
                if *selected_idx < available_personas.len() - 1 {
                    *selected_idx += 1;
                }
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                // Enter system prompt editing mode; place cursor at end
                if let Some(persona) = available_personas.get(*selected_idx) {
                    *prompt_input = persona.system_prompt.clone();
                    *cursor_pos = prompt_input.chars().count();
                    *editing_prompt = true;
                }
            }
            KeyCode::Enter => {
                // Save the slug (not display name) so it loads correctly
                if let Some(persona) = available_personas.get(*selected_idx) {
                    *default_persona = persona.slug.clone();
                }
                state.mark_completed(WizardSection::Personas);
                state.next_section();
            }
            KeyCode::Esc => {
                state.prev_section();
            }
            _ => {}
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
pub(super) const SETTINGS_FEATURE_COUNT: usize = 10;
#[cfg(not(target_os = "macos"))]
pub(super) const SETTINGS_FEATURE_COUNT: usize = 9;
#[cfg(target_os = "macos")]
pub(super) const SETTINGS_HF_TOKEN_IDX: usize = 4;
#[cfg(not(target_os = "macos"))]
pub(super) const SETTINGS_HF_TOKEN_IDX: usize = 3;
#[cfg(target_os = "macos")]
pub(super) const SETTINGS_FINCH_API_KEY_IDX: usize = 5;
#[cfg(not(target_os = "macos"))]
pub(super) const SETTINGS_FINCH_API_KEY_IDX: usize = 4;
#[cfg(target_os = "macos")]
pub(super) const SETTINGS_DAEMON_ONLY_IDX: usize = 6;
#[cfg(not(target_os = "macos"))]
pub(super) const SETTINGS_DAEMON_ONLY_IDX: usize = 5;
#[cfg(target_os = "macos")]
pub(super) const SETTINGS_MDNS_IDX: usize = 7;
#[cfg(not(target_os = "macos"))]
pub(super) const SETTINGS_MDNS_IDX: usize = 6;
#[cfg(target_os = "macos")]
pub(super) const SETTINGS_AUTO_DISCOVER_IDX: usize = 8;
#[cfg(not(target_os = "macos"))]
pub(super) const SETTINGS_AUTO_DISCOVER_IDX: usize = 7;
#[cfg(target_os = "macos")]
pub(super) const SETTINGS_CONTEXT_IDX: usize = 9;
#[cfg(not(target_os = "macos"))]
pub(super) const SETTINGS_CONTEXT_IDX: usize = 8;

/// Handle input for Features section (with arrow key navigation)
pub(super) fn handle_features_input(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        return handle_features_input_with_gui_actions(
            state,
            key,
            &mut || AutomationBroker::new(true).availability(),
            &mut || {
                AutomationBroker::new(true)
                    .request_permission(AutomationPromptContext::for_current_session(true))
            },
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        handle_features_input_impl(state, key)
    }
}

#[cfg(target_os = "macos")]
pub(super) fn handle_features_input_with_gui_actions(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
    passive_check: &mut dyn FnMut() -> AutomationAvailability,
    request_permission: &mut dyn FnMut() -> AutomationPermissionResult,
) -> Result<bool> {
    handle_features_input_impl(state, key, passive_check, request_permission)
}

pub(super) fn handle_features_input_impl(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
    #[cfg(target_os = "macos")] passive_check: &mut dyn FnMut() -> AutomationAvailability,
    #[cfg(target_os = "macos")] request_permission: &mut dyn FnMut() -> AutomationPermissionResult,
) -> Result<bool> {
    if let Some(SectionState::Features {
        auto_approve,
        streaming,
        debug,
        hf_token,
        editing_hf_token,
        finch_api_key,
        editing_finch_api_key,
        #[cfg(target_os = "macos")]
        gui_automation,
        #[cfg(target_os = "macos")]
        gui_automation_availability,
        #[cfg(target_os = "macos")]
        gui_automation_prompt,
        #[cfg(target_os = "macos")]
        gui_automation_prompted,
        #[cfg(target_os = "macos")]
        gui_automation_last_known_available,
        #[cfg(target_os = "macos")]
        gui_automation_permission_context,
        #[cfg(target_os = "macos")]
        gui_automation_settings_feedback,
        #[cfg(target_os = "macos")]
        gui_automation_details_expanded,
        #[cfg(target_os = "macos")]
        gui_automation_details_scroll,
        daemon_only_mode,
        mdns_discovery,
        auto_discover,
        memory_context_lines,
        selected_idx,
    }) = state.sections.get_mut(&WizardSection::Features)
    {
        #[cfg(target_os = "macos")]
        if *gui_automation_details_expanded {
            match key.code {
                KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Esc => {
                    *gui_automation_details_expanded = false;
                    *gui_automation_details_scroll = 0;
                }
                KeyCode::Up => {
                    *gui_automation_details_scroll =
                        gui_automation_details_scroll.saturating_sub(1);
                }
                KeyCode::Down => {
                    *gui_automation_details_scroll =
                        gui_automation_details_scroll.saturating_add(1);
                }
                KeyCode::PageUp => {
                    *gui_automation_details_scroll =
                        gui_automation_details_scroll.saturating_sub(5);
                }
                KeyCode::PageDown => {
                    *gui_automation_details_scroll =
                        gui_automation_details_scroll.saturating_add(5);
                }
                KeyCode::Home => {
                    *gui_automation_details_scroll = 0;
                }
                _ => {}
            }
            return Ok(false);
        }

        if *editing_hf_token {
            // In HF token editing mode
            match key.code {
                KeyCode::Char(c) => {
                    hf_token.push(c);
                }
                KeyCode::Backspace => {
                    hf_token.pop();
                }
                KeyCode::Enter | KeyCode::Esc => {
                    *editing_hf_token = false;
                }
                _ => {}
            }
            return Ok(false);
        }

        if *editing_finch_api_key {
            match key.code {
                KeyCode::Char(c) => finch_api_key.push(c),
                KeyCode::Backspace => {
                    finch_api_key.pop();
                }
                KeyCode::Enter | KeyCode::Esc => {
                    *editing_finch_api_key = false;
                }
                _ => {}
            }
            return Ok(false);
        }

        #[cfg(target_os = "macos")]
        if *selected_idx == 3
            && *gui_automation
            && handle_gui_permission_input_with(
                key.code,
                gui_automation_availability,
                gui_automation_prompt,
                gui_automation_prompted,
                gui_automation_last_known_available,
                gui_automation_permission_context,
                gui_automation_settings_feedback,
                gui_automation_details_scroll,
                passive_check,
                request_permission,
            )
        {
            return Ok(false);
        }

        // Text fields and toggle rows share these constants with the renderer so
        // keyboard focus and visual selection cannot drift apart.
        match key.code {
            KeyCode::Up => {
                if *selected_idx > 0 {
                    *selected_idx -= 1;
                }
            }
            KeyCode::Down => {
                if *selected_idx < SETTINGS_FEATURE_COUNT - 1 {
                    *selected_idx += 1;
                }
            }
            KeyCode::Left => {
                // Decrement context_lines spinner (min 1)
                if *selected_idx == SETTINGS_CONTEXT_IDX && *memory_context_lines > 1 {
                    *memory_context_lines -= 1;
                }
            }
            KeyCode::Right => {
                // Increment context_lines spinner (max 8)
                if *selected_idx == SETTINGS_CONTEXT_IDX && *memory_context_lines < 8 {
                    *memory_context_lines += 1;
                }
            }
            KeyCode::Char(' ') => {
                // Toggle selected feature (all except hf_token and ctx_lines)
                #[cfg(target_os = "macos")]
                match *selected_idx {
                    0 => *streaming = !*streaming,
                    1 => *auto_approve = !*auto_approve,
                    2 => *debug = !*debug,
                    3 => {
                        *gui_automation_settings_feedback = None;
                        *gui_automation_details_scroll = 0;
                        toggle_gui_automation_with(
                            gui_automation,
                            gui_automation_availability,
                            gui_automation_prompt,
                            gui_automation_prompted,
                            gui_automation_last_known_available,
                            gui_automation_permission_context,
                            || {
                                AutomationBroker::new(true).request_permission(
                                    AutomationPromptContext::for_current_session(true),
                                )
                            },
                        )
                    }
                    SETTINGS_DAEMON_ONLY_IDX => *daemon_only_mode = !*daemon_only_mode,
                    SETTINGS_MDNS_IDX => *mdns_discovery = !*mdns_discovery,
                    SETTINGS_AUTO_DISCOVER_IDX => *auto_discover = !*auto_discover,
                    // index 8 = ctx_lines (use ◀/▶)
                    _ => {}
                }
                #[cfg(not(target_os = "macos"))]
                match *selected_idx {
                    0 => *streaming = !*streaming,
                    1 => *auto_approve = !*auto_approve,
                    2 => *debug = !*debug,
                    SETTINGS_DAEMON_ONLY_IDX => *daemon_only_mode = !*daemon_only_mode,
                    SETTINGS_MDNS_IDX => *mdns_discovery = !*mdns_discovery,
                    SETTINGS_AUTO_DISCOVER_IDX => *auto_discover = !*auto_discover,
                    // index 7 = ctx_lines (use ◀/▶)
                    _ => {}
                }
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                if *selected_idx == SETTINGS_HF_TOKEN_IDX {
                    *editing_hf_token = true;
                } else if *selected_idx == SETTINGS_FINCH_API_KEY_IDX {
                    *editing_finch_api_key = true;
                }
            }
            #[cfg(target_os = "macos")]
            KeyCode::Char('o') | KeyCode::Char('O') if *selected_idx == 3 && *gui_automation => {
                open_gui_settings_with(gui_automation_settings_feedback, || {
                    AutomationBroker::new(true).open_permission_settings(
                        AutomationPromptContext::for_current_session(true),
                    )
                });
            }
            #[cfg(target_os = "macos")]
            KeyCode::Char('d') | KeyCode::Char('D') if *selected_idx == 3 && *gui_automation => {
                *gui_automation_details_expanded = true;
                *gui_automation_details_scroll = 0;
            }
            KeyCode::Enter => {
                state.mark_completed(WizardSection::Features);
                state.next_section();
            }
            KeyCode::Esc => {
                state.prev_section();
            }
            _ => {}
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_gui_permission_input_with(
    key: KeyCode,
    availability: &mut AutomationAvailability,
    prompt: &mut AutomationPromptDisposition,
    prompted: &mut bool,
    last_known_available: &mut bool,
    permission_context: &mut String,
    settings_feedback: &mut Option<GuiSettingsFeedback>,
    details_scroll: &mut u16,
    passive_check: impl FnOnce() -> AutomationAvailability,
    request_permission: impl FnOnce() -> AutomationPermissionResult,
) -> bool {
    let result = match key {
        KeyCode::Char('r') | KeyCode::Char('R') => {
            let availability = passive_check();
            AutomationPermissionResult {
                availability,
                prompt: AutomationPromptDisposition::NotNeeded,
            }
        }
        KeyCode::Char('p') | KeyCode::Char('P') => request_permission(),
        _ => return false,
    };

    *settings_feedback = None;
    *details_scroll = 0;
    if result.prompt == AutomationPromptDisposition::Requested {
        *prompted = true;
    }
    if result.availability.state == AutomationState::Available {
        *last_known_available = true;
    }
    if result.prompt == AutomationPromptDisposition::Requested
        || result.availability.state == AutomationState::Available
    {
        *permission_context = permission_context_key();
    }
    *availability = result.availability;
    *prompt = result.prompt;
    true
}

#[cfg(target_os = "macos")]
pub(super) fn toggle_gui_automation_with(
    configured: &mut bool,
    availability: &mut AutomationAvailability,
    prompt: &mut AutomationPromptDisposition,
    prompted: &mut bool,
    last_known_available: &mut bool,
    permission_context: &mut String,
    request_permission: impl FnOnce() -> AutomationPermissionResult,
) {
    if *configured {
        *configured = false;
        *availability = AutomationBroker::new(false).availability();
        *prompt = AutomationPromptDisposition::NotNeeded;
    } else {
        // Persist Finch's explicit capability consent independently from the
        // result of the native macOS permission request.
        *configured = true;
        let result = request_permission();
        if result.prompt == AutomationPromptDisposition::Requested {
            *prompted = true;
        }
        if result.availability.state == AutomationState::Available {
            *last_known_available = true;
        }
        if result.prompt == AutomationPromptDisposition::Requested
            || result.availability.state == AutomationState::Available
        {
            *permission_context = permission_context_key();
        }
        *availability = result.availability;
        *prompt = result.prompt;
    }
}

#[cfg(target_os = "macos")]
pub(super) fn open_gui_settings_with(
    feedback: &mut Option<GuiSettingsFeedback>,
    open_settings: impl FnOnce() -> Result<bool>,
) {
    *feedback = Some(match open_settings() {
        Ok(true) => GuiSettingsFeedback::OpenRequested,
        Ok(false) => GuiSettingsFeedback::Suppressed,
        Err(error) => {
            tracing::warn!("Could not open macOS Accessibility settings: {error}");
            GuiSettingsFeedback::Failed(error.to_string())
        }
    });
}

/// Handle input for Review section
pub(super) fn handle_review_input(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
) -> Result<bool> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            // Confirm and exit
            Ok(true)
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            state.prev_section();
            Ok(false)
        }
        _ => Ok(false),
    }
}
