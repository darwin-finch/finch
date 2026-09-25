//! The wizard's run loop: what happens on a tick and what a key does at the top level.
//!
//! Background scan and catalog-refresh polling, overlay and nested-interaction state, the
//! top-level key dispatch, and the terminal loop that draws and reads events.

use super::*;

/// Returns true if the Models section is in a sub-step that polls background
/// work (network scan, catalog refresh, or the add-time ChatGPT device
/// ceremony #424), so the run loop uses a short poll timeout and keeps
/// redrawing instead of blocking on keyboard input.
pub(super) fn is_scanning_state(state: &WizardState) -> bool {
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get(&WizardSection::Models)
    {
        matches!(
            adding_provider,
            Some(AddProviderStep::Scanning { .. }) | Some(AddProviderStep::DeviceAuth { .. })
        ) || matches!(
            state.sections.get(&WizardSection::Models),
            Some(SectionState::Models {
                catalog_refresh: Some(_),
                ..
            })
        )
    } else {
        false
    }
}

pub(super) fn advance_catalog_refresh_if_done(state: &mut WizardState) {
    let Some(SectionState::Models {
        primary_model,
        tool_models,
        adding_provider,
        catalog_models,
        catalog_model_provenance,
        catalog_source,
        catalog_refresh,
        catalog_generation,
        catalog_refreshed_at,
        catalog_error,
        ..
    }) = state.sections.get_mut(&WizardSection::Models)
    else {
        return;
    };
    let completed = catalog_refresh.as_ref().and_then(|refresh| {
        refresh
            .result
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
            .map(|result| {
                (
                    refresh.generation,
                    refresh.selection_identity.clone(),
                    result,
                )
            })
    });
    let Some((generation, selection_identity, (catalog, refresh_error))) = completed else {
        return;
    };
    *catalog_refresh = None;

    let Some(AddProviderStep::ConfigureRemote {
        provider_idx,
        name,
        api_key,
        editing_idx,
        ..
    }) = adding_provider.as_ref()
    else {
        return;
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
    let Some(current_profile) = model_catalog_profile(
        provider_id,
        name,
        api_key.as_deref().unwrap_or(""),
        persisted,
    ) else {
        return;
    };
    if generation != *catalog_generation
        || selection_identity != profile_cache_identity(&current_profile)
    {
        return;
    }

    if let Some(AddProviderStep::ConfigureRemote { model, .. }) = adding_provider.as_mut() {
        if catalog.source == CatalogSource::Discovered
            && matches!(
                catalog_model_provenance,
                ModelSelectionProvenance::Blank | ModelSelectionProvenance::DefaultGenerated
            )
        {
            if let Some(discovered) = catalog.models.first() {
                *model = discovered.clone();
                *catalog_model_provenance = ModelSelectionProvenance::DefaultGenerated;
            }
        }
    }
    *catalog_models = catalog.models;
    *catalog_source = catalog.source;
    *catalog_refreshed_at = Some(catalog.refreshed_at);
    *catalog_error = refresh_error;
}

/// Returns true if the Models section currently has any overlay open
pub(super) fn is_overlay_active(state: &WizardState) -> bool {
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get(&WizardSection::Models)
    {
        adding_provider.is_some()
    } else {
        false
    }
}

/// Returns true while a section owns keyboard input for a nested editor or overlay.
/// Global save/navigation shortcuts must not steal keys from these interactions.
pub(super) fn is_nested_interaction_active(state: &WizardState) -> bool {
    match state.sections.get(&state.current_section) {
        Some(SectionState::Models {
            editing_mode,
            editing_model_mode,
            ..
        }) => *editing_mode || *editing_model_mode || is_overlay_active(state),
        Some(SectionState::Personas { editing_prompt, .. }) => *editing_prompt,
        Some(SectionState::Features {
            editing_hf_token,
            editing_finch_api_key,
            #[cfg(target_os = "macos")]
            gui_automation_details_expanded,
            ..
        }) => {
            let editing = *editing_hf_token || *editing_finch_api_key;
            #[cfg(target_os = "macos")]
            {
                editing || *gui_automation_details_expanded
            }
            #[cfg(not(target_os = "macos"))]
            {
                editing
            }
        }
        _ => false,
    }
}

/// True while the currently selected row consumes Left/Right itself — today
/// only the Features section's context-lines spinner (`SETTINGS_CONTEXT_IDX`,
/// `handle_features_input_impl`'s `KeyCode::Left`/`KeyCode::Right` arms).
///
/// `handle_wizard_key` treats Left/Right as a global prev/next-section
/// shortcut so arrow keys double as Tab/Shift+Tab. That shortcut fires before
/// `handle_section_input` ever sees the key, which made the spinner's own
/// Left/Right handling unreachable through the real key-dispatch path even
/// though it works fine when called directly in a test: pressing ◀/▶ on the
/// context-lines row silently changed tabs instead of the value (the
/// "Context lines can't be edited" report). This narrower check — unlike
/// folding the row into `is_nested_interaction_active` — deliberately leaves
/// Tab/Shift+Tab and Enter free to leave the row, since only Left/Right is
/// ambiguous between "adjust the spinner" and "switch tabs".
pub(super) fn current_row_owns_left_right(state: &WizardState) -> bool {
    matches!(
        state.sections.get(&WizardSection::Features),
        Some(SectionState::Features { selected_idx, .. })
            if state.current_section == WizardSection::Features
                && *selected_idx == SETTINGS_CONTEXT_IDX
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WizardAction {
    Continue,
    Save,
    Cancel,
}

/// Apply one key event independently of terminal I/O so navigation behavior is
/// consistent and directly testable.
pub(super) fn handle_wizard_key(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
) -> Result<WizardAction> {
    if state.confirming_cancel {
        return Ok(match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => WizardAction::Cancel,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                state.confirming_cancel = false;
                WizardAction::Continue
            }
            _ => WizardAction::Continue,
        });
    }

    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        state.confirming_cancel = true;
        return Ok(WizardAction::Continue);
    }

    let nested = is_nested_interaction_active(state);
    if !nested
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S'))
    {
        return Ok(WizardAction::Save);
    }

    match key.code {
        KeyCode::Tab if !nested => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                state.prev_section();
            } else {
                state.next_section();
            }
        }
        KeyCode::Left | KeyCode::Right if !nested && !current_row_owns_left_right(state) => {
            if key.code == KeyCode::Left {
                state.prev_section();
            } else {
                state.next_section();
            }
        }
        _ => {
            if handle_section_input(state, key)? {
                return Ok(WizardAction::Save);
            }
        }
    }

    Ok(WizardAction::Continue)
}

/// If a network scan has finished, advance to SelectAgent (or close overlay if no agents)
pub(super) fn advance_scan_if_done(state: &mut WizardState) {
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        // Check if results are ready without holding the lock across the reassignment
        let agents_opt =
            if let Some(AddProviderStep::Scanning { results }) = adding_provider.as_ref() {
                results.try_lock().ok().and_then(|g| g.as_ref().cloned())
            } else {
                return;
            };

        if let Some(agents) = agents_opt {
            *adding_provider = if agents.is_empty() {
                None // No agents found — close overlay
            } else {
                Some(AddProviderStep::SelectAgent {
                    agents,
                    selected: 0,
                })
            };
        }
    }
}

/// Run the NEW tabbed wizard with section navigation.
///
/// #812: the frame is planned by the widget host (`wizard_view` →
/// `plan_wizard_frame`) and blitted through the shadow buffer — the wizard
/// constructs no private terminal and owns no second painter.
pub(super) fn run_tabbed_wizard(
    existing_config: Option<&crate::config::Config>,
) -> Result<SetupResult> {
    let mut state = WizardState::new(existing_config);
    // #424: the add-provider dialog runs the ChatGPT device exchange itself,
    // so the live wizard carries the production credential authority. Without
    // one (no home directory), the dialog falls back to the save-time ceremony.
    state.chatgpt_authenticator = match crate::cli::chatgpt_auth::ChatGptAuthService::production() {
        Ok(service) => {
            Some(Arc::new(service)
                as Arc<
                    dyn crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator,
                >)
        }
        Err(error) => {
            tracing::warn!(
                ?error,
                "ChatGPT device sign-in is unavailable in setup; the exchange will run when setup is saved"
            );
            None
        }
    };
    state.grok_authenticator = match crate::cli::grok_auth::GrokAuthService::production() {
        Ok(service) => {
            Some(Arc::new(service)
                as Arc<
                    dyn crate::cli::grok_auth::GrokCredentialAuthenticator,
                >)
        }
        Err(error) => {
            tracing::warn!(
                ?error,
                "Grok subscription device sign-in is unavailable in setup; the exchange will run when setup is saved"
            );
            None
        }
    };

    let mut host = crate::cli::tui::WizardHost::new();

    loop {
        // Resize is a fresh planning pass every frame; the host diffs rows
        // through the shadow buffer, so only changed lines reach the wire.
        let (term_w, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
        let (width, height) = (term_w as usize, term_h as usize);
        let view = wizard_view(&state, width, height);
        let frame = crate::cli::tui::plan_wizard_frame(&view, width, height);
        host.paint(&mut io::stdout(), &frame, width, height)?;

        // When scanning for network agents, poll with a short timeout so we can check
        // the background thread's results without blocking on keyboard input.
        let key_opt: Option<crossterm::event::KeyEvent> = if is_scanning_state(&state) {
            advance_scan_if_done(&mut state);
            advance_catalog_refresh_if_done(&mut state);
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) => Some(key),
                    _ => None,
                }
            } else {
                None
            }
        } else {
            match event::read()? {
                Event::Key(key) => Some(key),
                _ => None,
            }
        };

        let Some(key) = key_opt else {
            continue;
        };

        match handle_wizard_key(&mut state, key)? {
            WizardAction::Continue => {}
            WizardAction::Save => return build_setup_result(&state),
            WizardAction::Cancel => anyhow::bail!("Setup cancelled"),
        }
    }
}

/// Handle input for the current section
pub(super) fn handle_section_input(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
) -> Result<bool> {
    match state.current_section {
        WizardSection::Themes => handle_themes_input(state, key),
        WizardSection::Models => handle_models_input(state, key),
        WizardSection::LocalHelpers => handle_local_helpers_input(state, key),
        WizardSection::Personas => handle_personas_input(state, key),
        WizardSection::Features => handle_features_input(state, key),
        WizardSection::Review => handle_review_input(state, key),
    }
}
