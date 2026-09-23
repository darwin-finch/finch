//! Tests for the setup wizard.
//!
//! Declared by `setup_wizard.rs` as `#[cfg(test)] mod tests;`, so this is the same module:
//! `use super::*` still reaches the wizard's private items.

use super::chatgpt_recovery::*;
use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[test]
fn models_tab_describes_model_setup() {
    assert_eq!(WizardSection::Models.name(), "Model Setup");
}

#[test]
fn global_save_is_available_from_every_top_level_screen() {
    for section in WizardSection::all() {
        let mut state = WizardState::new(None);
        state.current_section = section;
        assert_eq!(
            handle_wizard_key(
                &mut state,
                modified_key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            )
            .unwrap(),
            WizardAction::Save,
            "Ctrl+S should save from {}",
            section.name()
        );
    }
}

#[test]
fn editor_local_ctrl_s_is_not_stolen_by_global_save() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Personas;
    handle_personas_input(&mut state, key(KeyCode::Char('e'))).unwrap();

    assert_eq!(
        handle_wizard_key(
            &mut state,
            modified_key(KeyCode::Char('s'), KeyModifiers::CONTROL),
        )
        .unwrap(),
        WizardAction::Continue
    );
    assert!(!is_nested_interaction_active(&state));
}

#[test]
fn escape_closes_nested_overlay_before_leaving_screen() {
    let mut state = state_with_step(default_configure_remote(0));
    state.current_section = WizardSection::Models;
    assert_eq!(state.current_section, WizardSection::Models);

    assert_eq!(
        handle_wizard_key(&mut state, key(KeyCode::Esc)).unwrap(),
        WizardAction::Continue
    );
    assert!(get_step(&state).is_none());
    assert_eq!(state.current_section, WizardSection::Models);
}

#[test]
fn escape_closes_nested_editor_before_leaving_screen() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Personas;
    handle_personas_input(&mut state, key(KeyCode::Char('e'))).unwrap();
    assert!(is_nested_interaction_active(&state));

    assert_eq!(
        handle_wizard_key(&mut state, key(KeyCode::Esc)).unwrap(),
        WizardAction::Continue
    );
    assert!(!is_nested_interaction_active(&state));
    assert_eq!(state.current_section, WizardSection::Personas);
}

#[test]
fn escape_moves_back_one_top_level_screen_without_cancelling() {
    let expected = [
        (WizardSection::Themes, WizardSection::Themes),
        (WizardSection::Models, WizardSection::Themes),
        (WizardSection::Personas, WizardSection::Models),
        (WizardSection::Features, WizardSection::Personas),
        (WizardSection::Review, WizardSection::Features),
    ];

    for (current, previous) in expected {
        let mut state = WizardState::new(None);
        state.current_section = current;
        assert_eq!(
            handle_wizard_key(&mut state, key(KeyCode::Esc)).unwrap(),
            WizardAction::Continue
        );
        assert_eq!(state.current_section, previous);
        assert!(!state.confirming_cancel);
    }
}

#[test]
fn cancellation_requires_explicit_confirmation() {
    let mut state = WizardState::new(None);
    assert_eq!(
        handle_wizard_key(
            &mut state,
            modified_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .unwrap(),
        WizardAction::Continue
    );
    assert!(state.confirming_cancel);

    assert_eq!(
        handle_wizard_key(&mut state, key(KeyCode::Esc)).unwrap(),
        WizardAction::Continue
    );
    assert!(!state.confirming_cancel);

    handle_wizard_key(
        &mut state,
        modified_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .unwrap();
    assert_eq!(
        handle_wizard_key(&mut state, key(KeyCode::Char('y'))).unwrap(),
        WizardAction::Cancel
    );
}

#[test]
fn save_from_non_review_uses_accumulated_selection() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Personas;
    let expected_slug = if let Some(SectionState::Personas {
        available_personas,
        selected_idx,
        ..
    }) = state.sections.get_mut(&WizardSection::Personas)
    {
        *selected_idx = 1;
        available_personas[1].slug.clone()
    } else {
        panic!("expected personas section");
    };

    assert_eq!(
        handle_wizard_key(
            &mut state,
            modified_key(KeyCode::Char('s'), KeyModifiers::CONTROL),
        )
        .unwrap(),
        WizardAction::Save
    );
    assert_eq!(
        build_setup_result(&state).unwrap().default_persona,
        expected_slug
    );
}

#[test]
fn enter_opens_provider_editor_and_saves_public_name() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Models;

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    if let Some(AddProviderStep::ConfigureRemote {
        name,
        model,
        editing_idx,
        ..
    }) = state
        .sections
        .get_mut(&WizardSection::Models)
        .and_then(|section| match section {
            SectionState::Models {
                adding_provider, ..
            } => adding_provider.as_mut(),
            _ => None,
        })
    {
        *name = "work-claude".to_string();
        *model = "manually-selected-claude".to_string();
        assert_eq!(*editing_idx, Some(0));
    } else {
        panic!("expected provider editor");
    }

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    if let Some(ModelConfig::Remote { name, .. }) = get_primary(&state) {
        assert_eq!(name, "work-claude");
    } else {
        panic!("expected remote primary");
    }
}

#[test]
fn peer_discovery_and_context_lines_have_distinct_rows() {
    assert_ne!(SETTINGS_AUTO_DISCOVER_IDX, SETTINGS_CONTEXT_IDX);
    assert_ne!(SETTINGS_FINCH_API_KEY_IDX, SETTINGS_AUTO_DISCOVER_IDX);
    assert_eq!(SETTINGS_AUTO_DISCOVER_IDX + 1, SETTINGS_CONTEXT_IDX);
    assert_eq!(SETTINGS_CONTEXT_IDX, SETTINGS_FEATURE_COUNT - 1);
}

#[test]
fn wizard_settings_survive_config_mapping_and_reopen() {
    let mut state = WizardState::new(None);
    if let Some(SectionState::Features {
        auto_approve,
        #[cfg(target_os = "macos")]
        gui_automation,
        #[cfg(target_os = "macos")]
        gui_automation_prompted,
        #[cfg(target_os = "macos")]
        gui_automation_last_known_available,
        #[cfg(target_os = "macos")]
        gui_automation_permission_context,
        daemon_only_mode,
        mdns_discovery,
        auto_discover,
        ..
    }) = state.sections.get_mut(&WizardSection::Features)
    {
        *auto_approve = true;
        #[cfg(target_os = "macos")]
        {
            *gui_automation = true;
            *gui_automation_prompted = true;
            *gui_automation_last_known_available = true;
            *gui_automation_permission_context = permission_context_key();
        }
        *daemon_only_mode = true;
        *mdns_discovery = true;
        *auto_discover = true;
    } else {
        panic!("expected settings section");
    }

    let result = build_setup_result(&state).unwrap();
    let config = config_from_setup_result(&result);
    assert!(config.features.auto_approve_tools);
    #[cfg(target_os = "macos")]
    assert!(config.features.gui_automation);
    #[cfg(target_os = "macos")]
    assert!(config.features.gui_automation_prompted);
    #[cfg(target_os = "macos")]
    assert!(config.features.gui_automation_last_known_available);
    #[cfg(target_os = "macos")]
    assert_eq!(
        config.features.gui_automation_permission_context,
        permission_context_key()
    );
    assert_eq!(config.server.mode, "daemon-only");
    assert!(config.server.advertise);
    assert!(config.client.auto_discover);

    let reopened = WizardState::new(Some(&config));
    if let Some(SectionState::Features {
        auto_approve,
        #[cfg(target_os = "macos")]
        gui_automation,
        #[cfg(target_os = "macos")]
        gui_automation_prompted,
        #[cfg(target_os = "macos")]
        gui_automation_last_known_available,
        #[cfg(target_os = "macos")]
        gui_automation_permission_context,
        daemon_only_mode,
        mdns_discovery,
        auto_discover,
        ..
    }) = reopened.sections.get(&WizardSection::Features)
    {
        assert!(*auto_approve);
        #[cfg(target_os = "macos")]
        assert!(*gui_automation);
        #[cfg(target_os = "macos")]
        assert!(*gui_automation_prompted);
        #[cfg(target_os = "macos")]
        assert!(*gui_automation_last_known_available);
        #[cfg(target_os = "macos")]
        assert_eq!(gui_automation_permission_context, &permission_context_key());
        assert!(*daemon_only_mode);
        assert!(*mdns_discovery);
        assert!(*auto_discover);
    } else {
        panic!("expected settings section");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn test_gui_permission_keys_separate_passive_check_from_prompt_request() {
    use std::cell::Cell;

    let passive_checks = Cell::new(0);
    let prompt_requests = Cell::new(0);
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Features;
    if let Some(SectionState::Features {
        gui_automation,
        gui_automation_prompt,
        gui_automation_prompted,
        gui_automation_settings_feedback,
        gui_automation_details_scroll,
        selected_idx,
        ..
    }) = state.sections.get_mut(&WizardSection::Features)
    {
        *gui_automation = true;
        *gui_automation_prompt = AutomationPromptDisposition::Requested;
        *gui_automation_prompted = true;
        *gui_automation_settings_feedback = Some(GuiSettingsFeedback::OpenRequested);
        *gui_automation_details_scroll = 8;
        *selected_idx = 3;
    }

    handle_features_input_with_gui_actions(
        &mut state,
        key(KeyCode::Char('r')),
        &mut || {
            passive_checks.set(passive_checks.get() + 1);
            AutomationAvailability {
                state: AutomationState::PermissionRequired,
                backend: "test-native",
                operations: vec!["click", "type"],
            }
        },
        &mut || {
            prompt_requests.set(prompt_requests.get() + 1);
            panic!("passive R must never invoke the native prompt callback")
        },
    )
    .unwrap();
    assert_eq!(passive_checks.get(), 1);
    assert_eq!(prompt_requests.get(), 0);
    if let Some(SectionState::Features {
        gui_automation_prompt,
        gui_automation_settings_feedback,
        gui_automation_details_scroll,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        assert_eq!(
            *gui_automation_prompt,
            AutomationPromptDisposition::NotNeeded
        );
        assert!(gui_automation_settings_feedback.is_none());
        assert_eq!(*gui_automation_details_scroll, 0);
    }

    handle_features_input_with_gui_actions(
        &mut state,
        key(KeyCode::Char('p')),
        &mut || panic!("explicit P must use the prompt callback"),
        &mut || {
            prompt_requests.set(prompt_requests.get() + 1);
            AutomationPermissionResult {
                availability: AutomationAvailability {
                    state: AutomationState::PermissionRequired,
                    backend: "test-native",
                    operations: vec!["click", "type"],
                },
                prompt: AutomationPromptDisposition::Requested,
            }
        },
    )
    .unwrap();
    assert_eq!(passive_checks.get(), 1);
    assert_eq!(prompt_requests.get(), 1);
    if let Some(SectionState::Features {
        gui_automation_prompt,
        gui_automation_prompted,
        gui_automation_permission_context,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        assert_eq!(
            *gui_automation_prompt,
            AutomationPromptDisposition::Requested
        );
        assert!(*gui_automation_prompted);
        assert_eq!(gui_automation_permission_context, &permission_context_key());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn gui_toggle_persists_consent_without_claiming_prompt_granted_access() {
    use crate::runtime::{
        AutomationAvailability, AutomationPermissionResult, AutomationPromptDisposition,
        AutomationState,
    };

    let mut configured = false;
    let mut availability = AutomationBroker::new(false).availability();
    let mut prompt = AutomationPromptDisposition::NotNeeded;
    let mut prompted = false;
    let mut last_known_available = false;
    let mut permission_context = String::new();

    toggle_gui_automation_with(
        &mut configured,
        &mut availability,
        &mut prompt,
        &mut prompted,
        &mut last_known_available,
        &mut permission_context,
        || AutomationPermissionResult {
            availability: AutomationAvailability {
                state: AutomationState::PermissionRequired,
                backend: "test-native",
                operations: vec!["click", "type"],
            },
            prompt: AutomationPromptDisposition::Requested,
        },
    );

    assert!(configured, "Finch consent should be persisted separately");
    assert_eq!(availability.state, AutomationState::PermissionRequired);
    assert_eq!(prompt, AutomationPromptDisposition::Requested);
    assert!(prompted);
    assert!(!last_known_available);
    assert_eq!(permission_context, permission_context_key());

    toggle_gui_automation_with(
        &mut configured,
        &mut availability,
        &mut prompt,
        &mut prompted,
        &mut last_known_available,
        &mut permission_context,
        || panic!("disabling must not invoke the native prompt"),
    );
    assert!(!configured);
    assert_eq!(availability.state, AutomationState::Disabled);
    assert_eq!(prompt, AutomationPromptDisposition::NotNeeded);
}

#[cfg(target_os = "macos")]
#[test]
fn gui_permission_history_is_scoped_and_current_native_state_wins() {
    assert_eq!(
        scoped_permission_history(true, true, true, false),
        (true, true)
    );
    assert_eq!(
        scoped_permission_history(true, true, false, false),
        (false, false),
        "history from a different executable/launcher context must not imply denial or revocation"
    );
    assert_eq!(
        scoped_permission_history(false, false, false, true),
        (false, true),
        "a current native grant must be reported regardless of stale history"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn gui_permission_required_status_includes_non_authoritative_process_diagnostics() {
    let availability = AutomationAvailability {
        state: AutomationState::PermissionRequired,
        backend: "test-native",
        operations: vec!["click", "type"],
    };
    let target = "executable: /tmp/target/debug/finch; launcher hint: Apple_Terminal (diagnostic only, not the macOS TCC identity)";
    let lines = gui_automation_status_lines(
        true,
        &availability,
        AutomationPromptDisposition::SuppressedNonInteractive,
        false,
        false,
        target,
        None,
    );
    let output = lines
        .iter()
        .map(crate::cli::tui::WizardLine::plain_text)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(output.contains("current Finch process is not Accessibility-trusted"));
    assert!(output.contains("headless prompt suppressed"));
    assert!(output.contains("Diagnostic only — executable: /tmp/target/debug/finch"));
    assert!(output.contains("launcher hint: Apple_Terminal"));
    assert!(!output.contains("Accessibility is not granted to"));
}

#[cfg(target_os = "macos")]
#[test]
fn test_gui_accessibility_o_outcomes_visible_at_80x24() {
    for (feedback, needle) in [
        (GuiSettingsFeedback::OpenRequested, "Open requested"),
        (GuiSettingsFeedback::Suppressed, "Not opened (SSH/headless)"),
        (
            GuiSettingsFeedback::Failed("test opener failure".to_string()),
            "Open failed",
        ),
    ] {
        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Features;
        if let Some(SectionState::Features {
            gui_automation,
            gui_automation_availability,
            gui_automation_prompt,
            gui_automation_settings_feedback,
            selected_idx,
            ..
        }) = state.sections.get_mut(&WizardSection::Features)
        {
            *gui_automation = true;
            gui_automation_availability.state = AutomationState::PermissionRequired;
            *gui_automation_prompt = AutomationPromptDisposition::SuppressedNonInteractive;
            *gui_automation_settings_feedback = Some(feedback);
            *selected_idx = 3;
        }

        for (width, height) in [(80, 24), (40, 18)] {
            let rendered = wizard_text_with_permission_target(
                &state,
                "current Finch process: PID 42\nexecutable: /tmp/finch\nlauncher hint: Terminal",
                width,
                height,
            );
            assert!(rendered.contains("GUI automation"));
            assert!(rendered.contains(needle), "missing {needle}: {rendered}");
            assert!(rendered.contains("D: Full"));
            if !matches!(
                state.sections.get(&WizardSection::Features),
                Some(SectionState::Features {
                    gui_automation_settings_feedback: Some(GuiSettingsFeedback::OpenRequested),
                    ..
                })
            ) {
                assert!(rendered.contains("System Settings"));
                assert!(rendered.contains("Privacy"));
                assert!(rendered.contains("Security"));
                assert!(rendered.contains("Accessibility"));
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn test_gui_accessibility_fresh_40x18_shows_exact_recovery_keys_and_path() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Features;
    if let Some(SectionState::Features {
        gui_automation,
        gui_automation_availability,
        selected_idx,
        ..
    }) = state.sections.get_mut(&WizardSection::Features)
    {
        *gui_automation = true;
        gui_automation_availability.state = AutomationState::PermissionRequired;
        *selected_idx = 3;
    }
    let rendered = wizard_text_with_permission_target(
        &state,
        "current Finch process: PID 42\nexecutable: /tmp/finch\nlauncher hint: Terminal",
        40,
        18,
    );
    let _ = &rendered;

    assert!(rendered.contains("Current Finch process"));

    assert!(rendered.contains("Current Finch process"));
    assert!(rendered.contains("R: Passive check"));
    assert!(rendered.contains("P: Request prompt"));
    assert!(rendered.contains("O: System Settings"));
    assert!(rendered.contains("Privacy"));
    assert!(rendered.contains("Security"));
    assert!(rendered.contains("Accessibility"));
    assert!(rendered.contains("D: Full"));
}

#[cfg(target_os = "macos")]
#[test]
fn test_gui_accessibility_expanded_details_preserve_long_identity_hints() {
    let long_path = format!(
        "/private/tmp/{}/finch-ad-hoc-build",
        "long-development-directory/".repeat(4)
    );
    let target = format!(
            "executable: {long_path}\nlauncher hint: VeryLongLauncherNameForAccessibilityDiagnostics\nuse the app name macOS shows"
        );
    let feedback = GuiSettingsFeedback::Failed("test opener failure".to_string());
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Features;
    if let Some(SectionState::Features {
        gui_automation,
        gui_automation_availability,
        gui_automation_prompt,
        gui_automation_prompted,
        gui_automation_settings_feedback,
        gui_automation_details_expanded,
        selected_idx,
        ..
    }) = state.sections.get_mut(&WizardSection::Features)
    {
        *gui_automation = true;
        gui_automation_availability.state = AutomationState::PermissionRequired;
        *gui_automation_prompt = AutomationPromptDisposition::Requested;
        *gui_automation_prompted = true;
        *gui_automation_settings_feedback = Some(feedback);
        *gui_automation_details_expanded = true;
        *selected_idx = 3;
    }

    let mut render = |width, height, scroll| {
        if let Some(SectionState::Features {
            gui_automation_details_scroll,
            ..
        }) = state.sections.get_mut(&WizardSection::Features)
        {
            *gui_automation_details_scroll = scroll;
        }
        wizard_text_with_permission_target(&state, &target, width, height)
    };

    let full_status = gui_automation_status_lines(
        true,
        &AutomationAvailability {
            state: AutomationState::PermissionRequired,
            backend: "test-native",
            operations: vec!["click", "type"],
        },
        AutomationPromptDisposition::Requested,
        true,
        false,
        &target,
        Some(&GuiSettingsFeedback::Failed(
            "test opener failure".to_string(),
        )),
    )
    .iter()
    .map(crate::cli::tui::WizardLine::plain_text)
    .collect::<Vec<_>>()
    .join("\n");
    assert!(full_status.contains(&long_path));

    let full_size_pages = (0..16)
        .map(|scroll| render(80, 24, scroll))
        .collect::<Vec<_>>();
    assert!(full_size_pages
        .iter()
        .any(|page| page.contains("/private/tmp/long-development-directory")));
    assert!(full_size_pages
        .iter()
        .any(|page| page.contains("finch-ad-hoc-build")));
    assert!(full_size_pages
        .iter()
        .any(|page| page.contains("test opener failure")));
    assert!(full_size_pages
        .iter()
        .any(|page| page.contains("VeryLongLauncherNameForAccessibilityDiagnostics")));
    assert!(full_size_pages
        .iter()
        .any(|page| page.contains("clipboard copying is unavailable")));

    let narrow_pages = (0..24)
        .map(|scroll| render(40, 18, scroll))
        .collect::<Vec<_>>();
    let narrow_page_text = |page: &str| {
        page.chars()
            .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
            .collect::<String>()
    };
    assert!(narrow_pages
        .iter()
        .any(|page| narrow_page_text(page).contains("finch-ad-hoc-build")));
    assert!(narrow_pages
        .iter()
        .any(|page| narrow_page_text(page)
            .contains("VeryLongLauncherNameForAccessibilityDiagnostics")));
    assert!(narrow_pages.iter().any(|page| page.contains("PgUp/PgDn")));
}

#[cfg(target_os = "macos")]
#[test]
fn test_gui_accessibility_navigation_and_resize_keep_selected_row_visible() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Features;
    for _ in 0..SETTINGS_CONTEXT_IDX {
        handle_features_input(&mut state, key(KeyCode::Down)).unwrap();
    }
    for (width, height) in [(80, 24), (40, 18)] {
        let rendered = wizard_text_with_permission_target(&state, "", width, height);
        assert!(
            rendered.contains("Context lines: 4"),
            "selected row clipped after resize to {width}x{height}: {rendered}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn test_gui_accessibility_full_status_scrolls_and_closes_without_native_actions() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Features;
    if let Some(SectionState::Features {
        gui_automation,
        selected_idx,
        ..
    }) = state.sections.get_mut(&WizardSection::Features)
    {
        *gui_automation = true;
        *selected_idx = 3;
    }

    handle_features_input(&mut state, key(KeyCode::Char('d'))).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Right)).unwrap();
    assert_eq!(state.current_section, WizardSection::Features);
    handle_features_input(&mut state, key(KeyCode::Down)).unwrap();
    handle_features_input(&mut state, key(KeyCode::PageDown)).unwrap();
    if let Some(SectionState::Features {
        gui_automation_details_expanded,
        gui_automation_details_scroll,
        selected_idx,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        assert!(*gui_automation_details_expanded);
        assert_eq!(*gui_automation_details_scroll, 6);
        assert_eq!(
            *selected_idx, 3,
            "detail scrolling must not move the feature row"
        );
    }

    handle_features_input(&mut state, key(KeyCode::Home)).unwrap();
    if let Some(SectionState::Features {
        gui_automation_details_scroll,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        assert_eq!(*gui_automation_details_scroll, 0);
    }

    handle_features_input(&mut state, key(KeyCode::Esc)).unwrap();
    if let Some(SectionState::Features {
        gui_automation_details_expanded,
        gui_automation_details_scroll,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        assert!(!*gui_automation_details_expanded);
        assert_eq!(*gui_automation_details_scroll, 0);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn test_gui_accessibility_settings_open_outcomes_are_visible_and_actionable() {
    let mut feedback = None;
    open_gui_settings_with(&mut feedback, || Ok(false));
    let suppressed = feedback.as_ref().unwrap().full_message();
    assert!(suppressed.contains("was not opened"));
    assert!(suppressed.contains("local interactive session"));
    assert!(suppressed.contains("open System Settings"));

    open_gui_settings_with(&mut feedback, || {
        Err(anyhow::anyhow!("test opener failure"))
    });
    let failed = feedback.as_ref().unwrap().full_message();
    assert!(failed.contains("Could not open System Settings: test opener failure"));
    assert!(failed.contains("Privacy & Security → Accessibility"));

    open_gui_settings_with(&mut feedback, || Ok(true));
    let opened = feedback.as_ref().unwrap().full_message();
    assert!(opened.contains("open requested"));
    assert!(opened.contains("press R"));
}

#[test]
fn finch_client_key_can_be_entered_and_applied() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Features;
    if let Some(SectionState::Features { selected_idx, .. }) =
        state.sections.get_mut(&WizardSection::Features)
    {
        *selected_idx = SETTINGS_FINCH_API_KEY_IDX;
    }

    handle_features_input(&mut state, key(KeyCode::Char('e'))).unwrap();
    for c in "custom-secret".chars() {
        handle_features_input(&mut state, key(KeyCode::Char(c))).unwrap();
    }
    handle_features_input(&mut state, key(KeyCode::Enter)).unwrap();

    let result = build_setup_result(&state).unwrap();
    assert_eq!(result.finch_api_key, "custom-secret");

    let mut config = crate::config::Config::with_providers(result.providers);
    apply_daemon_api_key(&mut config, &result.finch_api_key);
    assert!(config.server.auth_enabled);
    assert_eq!(config.server.api_keys, vec!["custom-secret"]);
}

// ── helpers ──────────────────────────────────────────────────────────────

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn state_with_step(step: AddProviderStep) -> WizardState {
    let hermetic_config = crate::config::Config::with_providers_and_paths(
        vec![ProviderEntry::Claude {
            api_key: String::new(),
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("claude".to_string()),
        }],
        std::path::PathBuf::from("unused-test-metrics"),
        None,
    );
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&hermetic_config), None);
    if let Some(SectionState::Models {
        adding_provider,
        catalog_models,
        catalog_model_provenance,
        ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        if let AddProviderStep::ConfigureRemote {
            provider_idx,
            model,
            ..
        } = &step
        {
            *catalog_models = known_models_for(CLOUD_PROVIDERS[*provider_idx].0);
            *catalog_model_provenance = if model.is_empty() {
                ModelSelectionProvenance::Blank
            } else {
                ModelSelectionProvenance::Manual
            };
        }
        *adding_provider = Some(step);
    }
    state
}

fn set_catalog_model_provenance(state: &mut WizardState, provenance: ModelSelectionProvenance) {
    if let Some(SectionState::Models {
        catalog_model_provenance,
        ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        *catalog_model_provenance = provenance;
    }
}

fn get_step(state: &WizardState) -> Option<&AddProviderStep> {
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get(&WizardSection::Models)
    {
        adding_provider.as_ref()
    } else {
        None
    }
}

fn install_completed_catalog_refresh(
    state: &mut WizardState,
    profile: &ModelCatalogProfile,
    catalog: ModelCatalog,
) {
    install_completed_catalog_refresh_result(state, profile, catalog, None);
}

fn install_completed_catalog_refresh_result(
    state: &mut WizardState,
    profile: &ModelCatalogProfile,
    catalog: ModelCatalog,
    error: Option<String>,
) {
    if let Some(SectionState::Models {
        catalog_refresh,
        catalog_generation,
        ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        *catalog_generation = catalog_generation.wrapping_add(1);
        *catalog_refresh = Some(CatalogRefresh {
            generation: *catalog_generation,
            selection_identity: profile_cache_identity(profile),
            result: Arc::new(Mutex::new(Some((catalog, error)))),
        });
    }
}

/// Render the wizard through the widget host and return the visible rows the
/// shadow buffer would hold — the production path, not a TestBackend painter.
fn wizard_text_with_permission_target(
    state: &WizardState,
    permission_target: &str,
    width: usize,
    height: usize,
) -> String {
    let view = wizard_view_with_permission_target(state, permission_target, width, height);
    let frame = crate::cli::tui::plan_wizard_frame(&view, width, height);
    frame
        .to_shadow_buffer(width, height)
        .rows_as_text()
        .join("\n")
}

fn render_wizard_text(state: &WizardState) -> String {
    render_wizard_text_at(state, 180, 50)
}

fn render_wizard_text_at(state: &WizardState, width: usize, height: usize) -> String {
    wizard_text_with_permission_target(state, "", width, height)
}

/// One overlay card alone in a frame, the way the device-code and add-provider
/// dialogs occupy the claimed card region (#807).
fn render_card_text(card: crate::cli::tui::WizardCard, width: usize, height: usize) -> String {
    let view = crate::cli::tui::WizardView {
        title: " Finch Setup ".to_string(),
        tab_titles: vec![],
        selected_tab: 0,
        section: crate::cli::tui::WizardSectionContent::plain(Vec::new()),
        help: None,
        card: Some(card),
    };
    let frame = crate::cli::tui::plan_wizard_frame(&view, width, height);
    frame
        .to_shadow_buffer(width, height)
        .rows_as_text()
        .join("\n")
}

fn discovered_catalog(profile: &ModelCatalogProfile, models: &[&str]) -> ModelCatalog {
    ModelCatalog {
        provider: profile.provider.clone(),
        profile_id: profile.profile_id.clone(),
        models_url: profile.endpoints.models_url.clone(),
        models: models.iter().map(|model| (*model).to_string()).collect(),
        source: CatalogSource::Discovered,
        refreshed_at: Utc::now(),
    }
}

fn catalog_model_provenance(state: &WizardState) -> ModelSelectionProvenance {
    match state.sections.get(&WizardSection::Models) {
        Some(SectionState::Models {
            catalog_model_provenance,
            ..
        }) => *catalog_model_provenance,
        _ => panic!("expected models section"),
    }
}

fn edit_primary_remote(state: &mut WizardState, name: &str, model: &str, api_key: &str) {
    handle_models_input(state, key(KeyCode::Enter)).unwrap();
    let Some(SectionState::Models {
        adding_provider:
            Some(AddProviderStep::ConfigureRemote {
                name: editing_name,
                model: editing_model,
                api_key: editing_key,
                ..
            }),
        ..
    }) = state.sections.get_mut(&WizardSection::Models)
    else {
        panic!("expected remote editor");
    };
    *editing_name = name.to_string();
    *editing_model = model.to_string();
    *editing_key = Some(api_key.to_string());
    handle_models_input(state, key(KeyCode::Enter)).unwrap();
}

fn get_primary(state: &WizardState) -> Option<&ModelConfig> {
    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get(&WizardSection::Models)
    {
        Some(primary_model)
    } else {
        None
    }
}

fn get_tool_models(state: &WizardState) -> Vec<ModelConfig> {
    if let Some(SectionState::Models { tool_models, .. }) =
        state.sections.get(&WizardSection::Models)
    {
        tool_models.clone()
    } else {
        vec![]
    }
}

fn default_configure_local(focused_field: usize) -> AddProviderStep {
    AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        family: ModelFamily::Qwen2,
        size: ModelSize::Medium,
        quantization: GgufQuantization::Q4KM,
        execution: ExecutionTarget::Auto,
        model_path: test_gguf_path(),
        focused_field,
        editing_idx: None,
    }
}

fn test_gguf_path() -> String {
    static GGUF: std::sync::OnceLock<tempfile::NamedTempFile> = std::sync::OnceLock::new();
    GGUF.get_or_init(|| tempfile::Builder::new().suffix(".gguf").tempfile().unwrap())
        .path()
        .to_string_lossy()
        .into_owned()
}

fn default_configure_remote(focused_field: usize) -> AddProviderStep {
    let provider_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, _, _, _)| *id == "grok")
        .unwrap();
    AddProviderStep::ConfigureRemote {
        provider_idx,
        name: "grok".to_string(),
        model: CLOUD_PROVIDERS[provider_idx].2.to_string(),
        api_key: Some(String::new()),
        focused_field,
        editing_idx: None,
    }
}

// ── the widget host (#812) ────────────────────────────────────────────────

/// #812 structural pin: the wizard attaches to the tui widget host. No file
/// in this module's production code may reference ratatui at all — a private
/// terminal, a second painter, or a TestBackend painter would re-ship the
/// fork this ticket removes.
#[test]
fn test_setup_wizard_production_does_not_construct_a_private_ratatui_terminal() {
    let module_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli/setup_wizard");
    let offenders: Vec<String> = std::fs::read_dir(&module_dir)
        .expect("read setup_wizard directory")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .filter(|path| {
            // The test module may still import ratatui types for input
            // fixtures; production code may not.
            path.file_name().is_none_or(|name| name != "tests.rs")
        })
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).expect("read source file");
            text.contains("ratatui").then(|| path.display().to_string())
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "INVARIANT (#812): the setup wizard must not be a second ratatui app — \
         these production files still name ratatui: {offenders:?}"
    );
}

/// The Models section (the provider list) drives through the widget host: the
/// claiming pass records the section's rect, the lines land in the shadow
/// buffer, and the provider rows are visible at a real terminal size.
#[test]
fn test_provider_list_drives_through_the_widget_host_and_blits_visible_text() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Models;

    let view = wizard_view_with_permission_target(&state, "", 100, 24);
    let frame = crate::cli::tui::plan_wizard_frame(&view, 100, 24);
    let rows = frame.to_shadow_buffer(100, 24).rows_as_text();

    assert_eq!(
        (frame.rects.tab_row.height, frame.rects.section.height),
        (3, 20),
        "the claiming pass gives the provider list the leftover rows; rects={rect:?}",
        rect = frame.rects
    );
    let rendered = rows.join("\n");
    assert!(
        rendered.contains("AI Providers") && rendered.contains("★ Primary:"),
        "the provider list must blit through the shadow buffer with visible text; rows:\n{rendered}"
    );
    assert!(
        rendered.contains("[Not configured]"),
        "the unconfigured primary provider must be visible on the real path; rows:\n{rendered}"
    );
}

/// The Finish/confirm screen drives through the widget host at the default
/// terminal size and names its save action in visible text.
#[test]
fn test_confirm_screen_drives_through_the_widget_host_with_visible_text() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Review;

    let rendered = render_wizard_text_at(&state, 100, 24);
    assert!(
        rendered.contains("Ready to go!") && rendered.contains("save & start chatting"),
        "the confirm screen must be speakable through the widget host; rendered:\n{rendered}"
    );
}

/// The device-code overlay blits as a claimed card whose chrome — title and
/// controls — stays inside the claimed rect, the #807 contract.
#[test]
fn test_device_code_overlay_claims_a_card_with_chrome_inside_it() {
    let mut state = state_with_step(device_auth_step(Arc::new(Mutex::new(None))));
    state.current_section = WizardSection::Models;
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        if let Some(AddProviderStep::DeviceAuth { pending, .. }) = adding_provider.as_mut() {
            *pending.lock().unwrap() = Some(DeviceAuthPresentation {
                verification_uri: "https://auth.openai.com/activate".into(),
                user_code: "CODE-5678".into(),
                expires_in: std::time::Duration::from_secs(600),
            });
        }
    }

    let view = wizard_view_with_permission_target(&state, "", 80, 24);
    let frame = crate::cli::tui::plan_wizard_frame(&view, 80, 24);
    let card = frame.rects.card;
    assert!(
        card.height >= 5 && card.width == 80,
        "the device-code card must claim real rows at full width; got {card:?}"
    );
    let rows = frame.to_shadow_buffer(80, 24).rows_as_text();
    let card_text = rows[card.y..card.y + card.height].join("\n");
    assert!(
        card_text.contains("One-time code: CODE-5678")
            && card_text.contains("Open: https://auth.openai.com/activate"),
        "the device code and verification URL must blit through the shadow buffer; card:\n{card_text}"
    );
    assert!(
        card_text.contains("Esc: Cancel"),
        "the controls must stay pinned inside the card; card:\n{card_text}"
    );
    for row in &rows[card.y..card.y + card.height] {
        assert!(
            row.starts_with('┌') || row.starts_with('│') || row.starts_with('└') || row.is_empty(),
            "no chrome may escape the claimed card rect: {row:?}"
        );
    }
}

// ── known_models_for ──────────────────────────────────────────────────────

#[test]
fn test_known_models_for_returns_list_for_all_providers() {
    for (id, _, default_model, _) in CLOUD_PROVIDERS {
        let models = known_models_for(id);
        assert!(!models.is_empty(), "provider '{}' has no known models", id);
        // Discovery-capable providers intentionally start blank so the
        // wizard cannot silently select a stale compile-time identifier.
        assert!(
            default_model.is_empty() || models.iter().any(|model| model == default_model),
            "provider '{}': default model '{}' not in known_models_for list {:?}",
            id,
            default_model,
            models
        );
    }
}

#[test]
fn test_known_models_for_unknown_provider_returns_empty() {
    assert!(known_models_for("nonexistent").is_empty());
}

#[test]
fn discovery_capable_providers_do_not_start_with_compile_time_model_ids() {
    for provider in ["claude", "openai", "grok", "mistral"] {
        let (_, _, default_model, _) = CLOUD_PROVIDERS
            .iter()
            .find(|(id, ..)| *id == provider)
            .unwrap();
        assert!(
            default_model.is_empty(),
            "{provider} should start editable and blank"
        );
    }
}

#[test]
fn catalog_profile_preserves_configured_full_paths() {
    let persisted = ProviderEntry::Openai {
        api_key: "old-key".to_string(),
        model: Some("manual-model".to_string()),
        base_url: Some("https://compatible.example/v1".to_string()),
        chat_path: Some("https://chat.example/exact/completions?preview=1".to_string()),
        models_path: Some("https://models.example/exact/catalog?account=work".to_string()),
        name: Some("compatible".to_string()),
        reasoning_effort: None,
    };
    let profile =
        model_catalog_profile("openai", "compatible", "new-key", Some(&persisted)).unwrap();
    assert_eq!(profile.api_key, "new-key");
    assert_eq!(
        profile.endpoints.chat_url,
        "https://chat.example/exact/completions?preview=1"
    );
    assert_eq!(
        profile.endpoints.models_url,
        "https://models.example/exact/catalog?account=work"
    );
}

#[test]
fn named_catalog_profile_preserves_bound_custom_endpoints_without_inline_secret() {
    let persisted = ProviderEntry::Credentialed {
        provider: crate::config::CredentialProvider::OpenaiPlatform,
        credential: crate::config::CredentialBinding {
            credential_ref: "work".into(),
            audience: None,
            tenant: None,
            project: None,
            account: None,
            required_scopes: std::collections::BTreeSet::new(),
        },
        model: Some("manual-model".into()),
        base_url: Some("https://compatible.example/v1".into()),
        chat_path: Some("/v1/chat/completions".into()),
        models_path: Some("/v1/models".into()),
        name: Some("work-profile".into()),
        reasoning_effort: None,
    };
    let profile = model_catalog_profile("openai", "work-profile", "", Some(&persisted)).unwrap();
    assert!(profile.api_key.is_empty());
    assert_eq!(
        profile.endpoints.models_url,
        "https://compatible.example/v1/models"
    );
}

#[test]
fn named_catalog_refresh_config_uses_edited_name_and_validates_siblings() {
    use crate::config::{
        AudienceBinding, CredentialBinding, CredentialKind, CredentialLifecycle,
        CredentialProvider, EndpointFamily, ProviderCredential,
    };
    let credential = ProviderCredential {
        name: "work".into(),
        kind: CredentialKind::ApiKey,
        provider: CredentialProvider::OpenaiPlatform,
        issuer: "openai-platform".into(),
        audience: AudienceBinding::standard(EndpointFamily::OpenaiPlatform),
        tenant: None,
        project: None,
        account: None,
        scopes: std::collections::BTreeSet::new(),
        secret_ref: "env:OPENAI_WORK_API_KEY".into(),
        lifecycle: CredentialLifecycle::default(),
        revocation: Default::default(),
    };
    let named = |name: &str, credential_ref: &str| ProviderEntry::Credentialed {
        provider: CredentialProvider::OpenaiPlatform,
        credential: CredentialBinding {
            credential_ref: credential_ref.into(),
            audience: None,
            tenant: None,
            project: None,
            account: None,
            required_scopes: std::collections::BTreeSet::new(),
        },
        model: Some("gpt-4o".into()),
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some(name.into()),
        reasoning_effort: None,
    };
    let primary = model_config_from_provider(&named("old-name", "work")).unwrap();
    let sibling = model_config_from_provider(&named("broken", "missing")).unwrap();
    let selected = named("renamed", "work");
    let config =
        named_catalog_refresh_config(&primary, &[sibling], 0, &selected, vec![credential.clone()]);
    assert!(config
        .validate()
        .unwrap_err()
        .to_string()
        .contains("missing"));

    let valid = named_catalog_refresh_config(&primary, &[], 0, &selected, vec![credential]);
    valid.validate().unwrap();
    assert_eq!(valid.providers[0].profile_name(), "renamed");
}

#[test]
fn builtin_discovery_profiles_use_provider_specific_urls_and_auth() {
    let claude = model_catalog_profile("claude", "claude-work", "key", None).unwrap();
    assert_eq!(claude.auth, CatalogAuth::AnthropicApiKey);
    assert_eq!(
        claude.endpoints.models_url,
        "https://api.anthropic.com/v1/models"
    );

    let openai = model_catalog_profile("openai", "openai-work", "key", None).unwrap();
    assert_eq!(openai.auth, CatalogAuth::Bearer);
    assert_eq!(
        openai.endpoints.models_url,
        "https://api.openai.com/v1/models"
    );

    let xai = model_catalog_profile("grok", "xai-work", "key", None).unwrap();
    assert_eq!(xai.auth, CatalogAuth::Bearer);
    assert_eq!(xai.endpoints.models_url, "https://api.x.ai/v1/models");

    let mistral = model_catalog_profile("mistral", "mistral-work", "key", None).unwrap();
    assert_eq!(mistral.auth, CatalogAuth::Bearer);
    assert_eq!(
        mistral.endpoints.models_url,
        "https://api.mistral.ai/v1/models"
    );
}

#[test]
fn stale_cross_provider_refresh_is_discarded() {
    let claude_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "claude")
        .unwrap();
    let openai_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "openai")
        .unwrap();
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: claude_idx,
        name: "claude-work".to_string(),
        model: String::new(),
        api_key: Some("claude-key".to_string()),
        focused_field: 0,
        editing_idx: None,
    });
    set_catalog_model_provenance(&mut state, ModelSelectionProvenance::DefaultGenerated);
    let claude = model_catalog_profile("claude", "claude-work", "claude-key", None).unwrap();
    install_completed_catalog_refresh(
        &mut state,
        &claude,
        ModelCatalog {
            provider: "claude".to_string(),
            profile_id: "claude-work".to_string(),
            models_url: claude.endpoints.models_url.clone(),
            models: vec!["claude-account-model".to_string()],
            source: CatalogSource::Discovered,
            refreshed_at: Utc::now(),
        },
    );
    if let Some(SectionState::Models {
        adding_provider,
        catalog_generation,
        ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        *catalog_generation = catalog_generation.wrapping_add(1);
        *adding_provider = Some(AddProviderStep::ConfigureRemote {
            provider_idx: openai_idx,
            name: "openai-work".to_string(),
            model: String::new(),
            api_key: Some("openai-key".to_string()),
            focused_field: 0,
            editing_idx: None,
        });
    }

    advance_catalog_refresh_if_done(&mut state);
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { provider_idx, model, .. })
            if *provider_idx == openai_idx && model.is_empty()
    ));
    assert_eq!(
        catalog_model_provenance(&state),
        ModelSelectionProvenance::DefaultGenerated
    );
    assert!(matches!(
        state.sections.get(&WizardSection::Models),
        Some(SectionState::Models {
            catalog_source: CatalogSource::StaticFallback,
            ..
        })
    ));
}

#[test]
fn only_latest_same_profile_refresh_is_applied() {
    let openai_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "openai")
        .unwrap();
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: openai_idx,
        name: "openai-work".to_string(),
        model: String::new(),
        api_key: Some("openai-key".to_string()),
        focused_field: 2,
        editing_idx: None,
    });
    let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
    install_completed_catalog_refresh(
        &mut state,
        &profile,
        ModelCatalog {
            provider: "openai".to_string(),
            profile_id: "openai-work".to_string(),
            models_url: profile.endpoints.models_url.clone(),
            models: vec!["old-result".to_string()],
            source: CatalogSource::Discovered,
            refreshed_at: Utc::now(),
        },
    );
    if let Some(SectionState::Models {
        catalog_generation, ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        *catalog_generation = catalog_generation.wrapping_add(1);
    }
    advance_catalog_refresh_if_done(&mut state);
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { model, .. }) if model.is_empty()
    ));

    let refreshed_at = Utc::now() - chrono::Duration::minutes(7);
    install_completed_catalog_refresh(
        &mut state,
        &profile,
        ModelCatalog {
            provider: "openai".to_string(),
            profile_id: "openai-work".to_string(),
            models_url: profile.endpoints.models_url.clone(),
            models: vec!["latest-result".to_string()],
            source: CatalogSource::Discovered,
            refreshed_at,
        },
    );
    advance_catalog_refresh_if_done(&mut state);
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { model, .. }) if model == "latest-result"
    ));
    assert!(matches!(
        state.sections.get(&WizardSection::Models),
        Some(SectionState::Models {
            catalog_source: CatalogSource::Discovered,
            catalog_refreshed_at: Some(actual),
            ..
        }) if *actual == refreshed_at
    ));

    let cached_at = refreshed_at - chrono::Duration::hours(2);
    install_completed_catalog_refresh(
        &mut state,
        &profile,
        ModelCatalog {
            provider: "openai".to_string(),
            profile_id: "openai-work".to_string(),
            models_url: profile.endpoints.models_url.clone(),
            models: vec!["cached-result".to_string()],
            source: CatalogSource::Cache,
            refreshed_at: cached_at,
        },
    );
    advance_catalog_refresh_if_done(&mut state);
    assert!(matches!(
        state.sections.get(&WizardSection::Models),
        Some(SectionState::Models {
            catalog_source: CatalogSource::Cache,
            catalog_refreshed_at: Some(actual),
            ..
        }) if *actual == cached_at
    ));
}

#[test]
fn catalog_refresh_time_displays_timestamp_and_age() {
    let refreshed_at = DateTime::parse_from_rfc3339("2026-08-25T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let now = DateTime::parse_from_rfc3339("2026-08-25T10:07:01Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        format_catalog_refresh_time(&refreshed_at, now),
        "2026-08-25 10:00 UTC (7m ago)"
    );
}

#[test]
fn chooser_keeps_chatgpt_subscription_distinct_from_openai_platform() {
    let openai = CLOUD_PROVIDERS
        .iter()
        .find(|(id, ..)| *id == "openai")
        .unwrap();
    assert_eq!(openai.1, "OpenAI API");
    let chatgpt = CLOUD_PROVIDERS
        .iter()
        .find(|(id, ..)| *id == "chatgpt")
        .unwrap();
    assert_eq!(chatgpt.1, "ChatGPT subscription");
    assert_eq!(chatgpt.2, "gpt-5.6-sol");
    assert!(CLOUD_PROVIDERS
        .iter()
        .all(|(id, ..)| *id != "chatgpt_subscription"));

    let step = AddProviderStep::SelectAddType { selected: 0 };
    let rendered = render_card_text(
        add_provider_card(
            CoreMlConfig::default(),
            &step,
            &CatalogSource::StaticFallback,
            false,
            None,
            None,
        ),
        160,
        50,
    );
    assert!(rendered.contains("OpenAI API"), "{rendered}");
    assert!(rendered.contains("ChatGPT subscription"), "{rendered}");
    assert!(
        rendered.contains("Finch-native device sign-in"),
        "{rendered}"
    );
    assert!(!rendered.contains("Codex"), "{rendered}");
    assert!(rendered.contains("platform.openai.com"), "{rendered}");
    assert!(!rendered.contains("GPT-4 (OpenAI)"), "{rendered}");
}

#[test]
fn chatgpt_configuration_has_no_api_key_input_buffer_or_render_path() {
    let mut state = state_with_step(AddProviderStep::SelectAddType { selected: 0 });
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote {
            provider_idx: 0,
            api_key: None,
            focused_field: 1,
            ..
        })
    ));

    handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote {
            api_key: None,
            focused_field: 2,
            ..
        })
    ));

    handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    assert!(
        matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote {
                api_key: Some(_),
                ..
            })
        ),
        "the Console Grok API row after ChatGPT and SuperGrok must expose an API-key buffer; step={:?}",
        get_step(&state)
    );
    for character in "sk-platform-must-not-cross".chars() {
        handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
        handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
        handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
        handle_models_input(&mut state, key(KeyCode::Char(character))).unwrap();
        handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
        handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
        handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
    }
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote {
            api_key: Some(key),
            ..
        }) if key == "sk-platform-must-not-cross"
    ));
    handle_models_input(&mut state, key(KeyCode::Left)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Left)).unwrap();
    let step = get_step(&state).unwrap();
    assert!(matches!(
        step,
        AddProviderStep::ConfigureRemote {
            provider_idx: 0,
            api_key: None,
            ..
        }
    ));

    let rendered = render_card_text(
        add_provider_card(
            CoreMlConfig::default(),
            step,
            &CatalogSource::StaticFallback,
            false,
            None,
            None,
        ),
        180,
        50,
    );
    assert!(rendered.contains("Finch-native device sign-in after save"));
    assert!(!rendered.contains("API Key"), "{rendered}");
    assert!(
        !rendered.contains("sk-platform-must-not-cross"),
        "{rendered}"
    );

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(matches!(
        get_primary(&state),
        Some(ModelConfig::Remote {
            provider,
            api_key,
            ..
        }) if provider == "chatgpt" && api_key.is_empty()
    ));
    state.current_section = WizardSection::Models;
    let rendered = render_wizard_text(&state);
    assert!(rendered.contains("Named device credential"), "{rendered}");
    assert!(!rendered.contains("Paste your API key"), "{rendered}");
    assert!(
        !rendered.contains("sk-platform-must-not-cross"),
        "{rendered}"
    );

    if let Some(SectionState::Models { editing_mode, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *editing_mode = true;
    }
    handle_models_input(&mut state, key(KeyCode::Char('x'))).unwrap();
    assert!(matches!(
        get_primary(&state),
        Some(ModelConfig::Remote {
            provider,
            api_key,
            ..
        }) if provider == "chatgpt" && api_key.is_empty()
    ));
    let rendered = render_wizard_text(&state);
    assert!(rendered.contains("not an API key"), "{rendered}");
    assert!(!rendered.contains("Edit API Key"), "{rendered}");
}

#[test]
fn static_fallback_ui_is_dated_incomplete_and_never_presented_as_fresh() {
    let misleading_runtime_time = Utc::now();
    let label = format_catalog_label(
        &CatalogSource::StaticFallback,
        false,
        Some(&misleading_runtime_time),
        misleading_runtime_time,
    );

    assert!(label.contains("bundled fallback snapshot"), "{label}");
    assert!(label.contains(STATIC_FALLBACK_AS_OF), "{label}");
    assert!(label.contains("incomplete"), "{label}");
    assert!(label.contains("model ID remains editable"), "{label}");
    assert!(!label.contains("provider discovery"), "{label}");
    assert!(!label.contains("local cache"), "{label}");
    assert!(!label.contains("UTC"), "{label}");
    assert!(!label.contains("ago"), "{label}");
    assert!(!label.contains("current"), "{label}");
    assert!(!label.contains("live"), "{label}");

    let openai_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "openai")
        .unwrap();
    let step = AddProviderStep::ConfigureRemote {
        provider_idx: openai_idx,
        name: "openai-work".to_string(),
        model: "gateway-preview-model".to_string(),
        api_key: Some("openai-key".to_string()),
        focused_field: 2,
        editing_idx: None,
    };
    let rendered = render_card_text(
        add_provider_card(
            CoreMlConfig::default(),
            &step,
            &CatalogSource::StaticFallback,
            false,
            Some(&misleading_runtime_time),
            None,
        ),
        180,
        50,
    );
    assert!(rendered.contains("bundled fallback snapshot"), "{rendered}");
    assert!(rendered.contains(STATIC_FALLBACK_AS_OF), "{rendered}");
    assert!(rendered.contains("incomplete"), "{rendered}");
    assert!(!rendered.contains("provider discovery"), "{rendered}");
    assert!(!rendered.contains("local cache"), "{rendered}");
    assert!(!rendered.contains("UTC"), "{rendered}");
}

#[test]
fn manual_openai_id_survives_save_reopen_and_fallback_installation() {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    let cache_dir = directory.path().join("model-catalog-cache");
    let metrics_dir = directory.path().join("metrics");
    let constitution_path = directory.path().join("constitution.md");
    std::fs::write(&constitution_path, "test-only constitution").unwrap();
    let openai_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "openai")
        .unwrap();
    let manual_model = "gateway-preview-model";
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: openai_idx,
        name: "openai-work".to_string(),
        model: String::new(),
        api_key: Some("sk-openai-test-key".to_string()),
        focused_field: 2,
        editing_idx: None,
    });
    state.catalog_cache_dir = Some(cache_dir.clone());
    for character in manual_model.chars() {
        handle_models_input(&mut state, key(KeyCode::Char(character))).unwrap();
    }
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

    let first_save = build_setup_result(&state).unwrap();
    let config = config_from_setup_result_with_paths(
        &first_save,
        metrics_dir.clone(),
        Some(constitution_path.clone()),
    );
    config.save_to(&config_path).unwrap();
    let loaded = crate::config::load_config_from_path_with_paths(
        &config_path,
        metrics_dir.clone(),
        Some(constitution_path.clone()),
    )
    .unwrap();
    assert_eq!(loaded.metrics_dir, metrics_dir);
    assert_eq!(loaded.constitution_path, Some(constitution_path.clone()));
    assert!(matches!(
        loaded.providers.first(),
        Some(ProviderEntry::Openai { model: Some(model), .. }) if model == manual_model
    ));

    let mut reopened =
        WizardState::new_with_catalog_cache_dir(Some(&loaded), Some(cache_dir.clone()));
    handle_models_input(&mut reopened, key(KeyCode::Enter)).unwrap();
    assert!(matches!(
        get_step(&reopened),
        Some(AddProviderStep::ConfigureRemote { model, .. }) if model == manual_model
    ));
    assert_eq!(
        catalog_model_provenance(&reopened),
        ModelSelectionProvenance::Persisted
    );

    let persisted = loaded.providers.first().unwrap();
    let profile = model_catalog_profile(
        "openai",
        "openai-work",
        "sk-openai-test-key",
        Some(persisted),
    )
    .unwrap();
    let mut fallback = fallback_catalog(&profile.provider, &profile.endpoints.models_url);
    fallback.profile_id = profile.profile_id.clone();
    install_completed_catalog_refresh_result(
        &mut reopened,
        &profile,
        fallback,
        Some("fake authenticated catalogue failure".to_string()),
    );
    advance_catalog_refresh_if_done(&mut reopened);
    assert!(matches!(
        get_step(&reopened),
        Some(AddProviderStep::ConfigureRemote { model, .. }) if model == manual_model
    ));
    assert!(matches!(
        reopened.sections.get(&WizardSection::Models),
        Some(SectionState::Models {
            catalog_source: CatalogSource::StaticFallback,
            catalog_error: Some(error),
            ..
        }) if error == "fake authenticated catalogue failure"
    ));

    handle_models_input(&mut reopened, key(KeyCode::Enter)).unwrap();
    let second_save = build_setup_result(&reopened).unwrap();
    config_from_setup_result_with_paths(
        &second_save,
        metrics_dir.clone(),
        Some(constitution_path.clone()),
    )
    .save_to(&config_path)
    .unwrap();
    let reloaded = crate::config::load_config_from_path_with_paths(
        &config_path,
        metrics_dir.clone(),
        Some(constitution_path.clone()),
    )
    .unwrap();
    assert_eq!(reloaded.metrics_dir, metrics_dir);
    assert_eq!(reloaded.constitution_path, Some(constitution_path));
    assert!(matches!(
        reloaded.providers.first(),
        Some(ProviderEntry::Openai { model: Some(model), .. }) if model == manual_model
    ));
    assert!(
        !cache_dir.exists(),
        "the hermetic cache should remain empty unless the test writes it"
    );
}

#[test]
fn failed_refresh_with_stale_cache_renders_age_warning_and_preserves_manual_model() {
    let openai_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "openai")
        .unwrap();
    let manual_model = "gateway-preview-model";
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: openai_idx,
        name: "openai-work".to_string(),
        model: manual_model.to_string(),
        api_key: Some("openai-key".to_string()),
        focused_field: 2,
        editing_idx: None,
    });
    state.current_section = WizardSection::Models;
    let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
    let refreshed_at = Utc::now() - chrono::Duration::hours(2);
    install_completed_catalog_refresh_result(
        &mut state,
        &profile,
        ModelCatalog {
            provider: profile.provider.clone(),
            profile_id: profile.profile_id.clone(),
            models_url: profile.endpoints.models_url.clone(),
            models: vec!["cached-account-model".to_string()],
            source: CatalogSource::Cache,
            refreshed_at,
        },
        Some("fake provider unavailable".to_string()),
    );
    advance_catalog_refresh_if_done(&mut state);

    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { model, .. }) if model == manual_model
    ));
    assert!(matches!(
        state.sections.get(&WizardSection::Models),
        Some(SectionState::Models {
            catalog_source: CatalogSource::Cache,
            catalog_error: Some(error),
            ..
        }) if error == "fake provider unavailable"
    ));
    let rendered = render_wizard_text(&state);
    assert!(rendered.contains("local cache"), "{rendered}");
    assert!(
        rendered.contains(&refreshed_at.format("%Y-%m-%d %H:%M UTC").to_string()),
        "{rendered}"
    );
    assert!(rendered.contains("2h ago"), "{rendered}");
    assert!(
        rendered.contains("Refresh warning: fake provider unavailable"),
        "{rendered}"
    );
    assert!(rendered.contains(manual_model), "{rendered}");
}

#[test]
fn failed_refresh_with_static_fallback_renders_snapshot_warning_and_preserves_manual_model() {
    let openai_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "openai")
        .unwrap();
    let manual_model = "restricted-account-model";
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: openai_idx,
        name: "openai-work".to_string(),
        model: manual_model.to_string(),
        api_key: Some("openai-key".to_string()),
        focused_field: 2,
        editing_idx: None,
    });
    state.current_section = WizardSection::Models;
    let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
    let mut fallback = fallback_catalog(&profile.provider, &profile.endpoints.models_url);
    fallback.profile_id = profile.profile_id.clone();
    install_completed_catalog_refresh_result(
        &mut state,
        &profile,
        fallback,
        Some("fake provider unavailable".to_string()),
    );
    advance_catalog_refresh_if_done(&mut state);

    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { model, .. }) if model == manual_model
    ));
    assert!(matches!(
        state.sections.get(&WizardSection::Models),
        Some(SectionState::Models {
            catalog_source: CatalogSource::StaticFallback,
            catalog_error: Some(error),
            ..
        }) if error == "fake provider unavailable"
    ));
    let rendered = render_wizard_text(&state);
    assert!(rendered.contains("bundled fallback snapshot"), "{rendered}");
    assert!(rendered.contains(STATIC_FALLBACK_AS_OF), "{rendered}");
    assert!(rendered.contains("incomplete"), "{rendered}");
    assert!(
        rendered.contains("Refresh warning: fake provider unavailable"),
        "{rendered}"
    );
    assert!(rendered.contains(manual_model), "{rendered}");
    assert!(!rendered.contains("provider discovery"), "{rendered}");
    assert!(!rendered.contains("local cache"), "{rendered}");
}

#[test]
fn persisted_fallback_id_is_not_replaced_by_refresh() {
    let persisted = ProviderEntry::Openai {
        api_key: "openai-key".to_string(),
        model: Some("gpt-4o".to_string()),
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some("openai-work".to_string()),
        reasoning_effort: None,
    };
    let config = crate::config::Config::with_providers_and_paths(
        vec![persisted.clone()],
        std::path::PathBuf::from("unused-test-metrics"),
        None,
    );
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&config), None);
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert_eq!(
        catalog_model_provenance(&state),
        ModelSelectionProvenance::Persisted
    );
    let profile =
        model_catalog_profile("openai", "openai-work", "openai-key", Some(&persisted)).unwrap();
    install_completed_catalog_refresh(
        &mut state,
        &profile,
        discovered_catalog(&profile, &["aaa-new-default", "gpt-4o"]),
    );
    advance_catalog_refresh_if_done(&mut state);
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { model, .. }) if model == "gpt-4o"
    ));
}

#[test]
fn manually_typed_fallback_id_is_not_replaced_by_refresh() {
    let openai_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "openai")
        .unwrap();
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: openai_idx,
        name: "openai-work".to_string(),
        model: String::new(),
        api_key: Some("openai-key".to_string()),
        focused_field: 2,
        editing_idx: None,
    });
    for character in "gpt-4o".chars() {
        handle_models_input(&mut state, key(KeyCode::Char(character))).unwrap();
    }
    assert_eq!(
        catalog_model_provenance(&state),
        ModelSelectionProvenance::Manual
    );
    let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
    install_completed_catalog_refresh(
        &mut state,
        &profile,
        discovered_catalog(&profile, &["aaa-new-default", "gpt-4o"]),
    );
    advance_catalog_refresh_if_done(&mut state);
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { model, .. }) if model == "gpt-4o"
    ));
}

#[test]
fn discovered_default_may_update_on_later_refresh() {
    let openai_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "openai")
        .unwrap();
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: openai_idx,
        name: "openai-work".to_string(),
        model: String::new(),
        api_key: Some("openai-key".to_string()),
        focused_field: 2,
        editing_idx: None,
    });
    let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
    install_completed_catalog_refresh(
        &mut state,
        &profile,
        discovered_catalog(&profile, &["gpt-4o"]),
    );
    advance_catalog_refresh_if_done(&mut state);
    assert_eq!(
        catalog_model_provenance(&state),
        ModelSelectionProvenance::DefaultGenerated
    );

    install_completed_catalog_refresh(
        &mut state,
        &profile,
        discovered_catalog(&profile, &["aaa-new-default", "gpt-4o"]),
    );
    advance_catalog_refresh_if_done(&mut state);
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { model, .. }) if model == "aaa-new-default"
    ));
}

#[test]
fn cycled_selection_is_not_replaced_by_refresh() {
    let openai_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "openai")
        .unwrap();
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: openai_idx,
        name: "openai-work".to_string(),
        model: String::new(),
        api_key: Some("openai-key".to_string()),
        focused_field: 2,
        editing_idx: None,
    });
    if let Some(SectionState::Models { catalog_models, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *catalog_models = vec!["other-model".to_string(), "gpt-4o".to_string()];
    }
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    assert_eq!(
        catalog_model_provenance(&state),
        ModelSelectionProvenance::Cycled
    );
    let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
    install_completed_catalog_refresh(
        &mut state,
        &profile,
        discovered_catalog(&profile, &["aaa-new-default", "gpt-4o"]),
    );
    advance_catalog_refresh_if_done(&mut state);
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { model, .. }) if model == "gpt-4o"
    ));
}

#[test]
fn completed_discovery_replaces_default_generated_but_not_manual_model() {
    let claude_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "claude")
        .unwrap();
    let fallback = known_models_for("claude")[0].clone();
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: claude_idx,
        name: "claude".to_string(),
        model: fallback,
        api_key: Some("test-key".to_string()),
        focused_field: 2,
        editing_idx: None,
    });
    set_catalog_model_provenance(&mut state, ModelSelectionProvenance::DefaultGenerated);
    let profile = model_catalog_profile("claude", "claude", "test-key", None).unwrap();
    install_completed_catalog_refresh(
        &mut state,
        &profile,
        ModelCatalog {
            provider: "claude".to_string(),
            profile_id: "claude".to_string(),
            models_url: "https://api.anthropic.com/v1/models".to_string(),
            models: vec!["account-visible-model".to_string()],
            source: CatalogSource::Discovered,
            refreshed_at: chrono::Utc::now(),
        },
    );
    advance_catalog_refresh_if_done(&mut state);
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { model, .. })
            if model == "account-visible-model"
    ));

    if let Some(AddProviderStep::ConfigureRemote { model, .. }) = state
        .sections
        .get_mut(&WizardSection::Models)
        .and_then(|section| match section {
            SectionState::Models {
                adding_provider, ..
            } => adding_provider.as_mut(),
            _ => None,
        })
    {
        *model = "manually-entered-model".to_string();
    }
    set_catalog_model_provenance(&mut state, ModelSelectionProvenance::Manual);
    install_completed_catalog_refresh(
        &mut state,
        &profile,
        ModelCatalog {
            provider: "claude".to_string(),
            profile_id: "claude".to_string(),
            models_url: "https://api.anthropic.com/v1/models".to_string(),
            models: vec!["newer-visible-model".to_string()],
            source: CatalogSource::Discovered,
            refreshed_at: chrono::Utc::now(),
        },
    );
    advance_catalog_refresh_if_done(&mut state);
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { model, .. })
            if model == "manually-entered-model"
    ));
}

// ── is_overlay_active ─────────────────────────────────────────────────────

#[test]
fn test_is_overlay_active_false_by_default() {
    let state = WizardState::new(None);
    assert!(!is_overlay_active(&state));
}

#[test]
fn test_is_overlay_active_true_when_configure_local() {
    let state = state_with_step(default_configure_local(0));
    assert!(is_overlay_active(&state));
}

#[test]
fn test_is_overlay_active_true_when_configure_remote() {
    let state = state_with_step(default_configure_remote(0));
    assert!(is_overlay_active(&state));
}

#[test]
fn test_is_overlay_active_true_when_select_add_type() {
    let state = state_with_step(AddProviderStep::SelectAddType { selected: 0 });
    assert!(is_overlay_active(&state));
}

// ── ConfigureLocal: focus navigation ─────────────────────────────────────

#[test]
fn test_configure_local_down_advances_focused_field() {
    let mut state = state_with_step(default_configure_local(0));
    handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
    if let Some(AddProviderStep::ConfigureLocal { focused_field, .. }) = get_step(&state) {
        assert_eq!(*focused_field, 1);
    } else {
        panic!("expected ConfigureLocal");
    }
}

#[test]
fn test_configure_local_up_decrements_focused_field() {
    let mut state = state_with_step(default_configure_local(2));
    handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
    if let Some(AddProviderStep::ConfigureLocal { focused_field, .. }) = get_step(&state) {
        assert_eq!(*focused_field, 1);
    } else {
        panic!("expected ConfigureLocal");
    }
}

#[test]
fn test_configure_local_up_clamps_at_zero() {
    let mut state = state_with_step(default_configure_local(0));
    handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
    if let Some(AddProviderStep::ConfigureLocal { focused_field, .. }) = get_step(&state) {
        assert_eq!(*focused_field, 0, "should not go below 0");
    } else {
        panic!("expected ConfigureLocal");
    }
}

#[test]
fn test_configure_local_down_clamps_at_gguf_path() {
    let mut state = state_with_step(default_configure_local(5));
    handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
    if let Some(AddProviderStep::ConfigureLocal { focused_field, .. }) = get_step(&state) {
        assert_eq!(*focused_field, 5, "should not go past 5 (GGUF path)");
    } else {
        panic!("expected ConfigureLocal");
    }
}

// ── ConfigureLocal: option cycling ───────────────────────────────────────

#[test]
fn test_configure_local_right_cycles_family_forward() {
    let mut state = state_with_step(default_configure_local(1)); // focused on Family
                                                                 // Qwen2 → Gemma2
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    if let Some(AddProviderStep::ConfigureLocal { family, .. }) = get_step(&state) {
        assert_eq!(*family, ModelFamily::Gemma2);
    } else {
        panic!("expected ConfigureLocal");
    }
}

#[test]
fn test_configure_local_left_cycles_family_backward() {
    let mut state = state_with_step(default_configure_local(1)); // Qwen2, focused Family
                                                                 // Qwen2 → wraps to last family (DeepSeek)
    handle_models_input(&mut state, key(KeyCode::Left)).unwrap();
    if let Some(AddProviderStep::ConfigureLocal { family, .. }) = get_step(&state) {
        assert_eq!(*family, ModelFamily::DeepSeek);
    } else {
        panic!("expected ConfigureLocal");
    }
}

#[test]
fn test_configure_local_right_cycles_size_forward() {
    let mut state = state_with_step(default_configure_local(2)); // focused on Size (Medium)
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    if let Some(AddProviderStep::ConfigureLocal { size, .. }) = get_step(&state) {
        assert_eq!(*size, ModelSize::Large);
    } else {
        panic!("expected ConfigureLocal");
    }
}

#[test]
fn test_configure_local_right_cycles_quantization() {
    let mut state = state_with_step(AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        family: ModelFamily::Qwen2,
        size: ModelSize::Medium,
        quantization: GgufQuantization::Q4KM,
        execution: ExecutionTarget::Auto,
        model_path: String::new(),
        focused_field: 3,
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureLocal {
            quantization: GgufQuantization::Q5KM,
            ..
        })
    ));
}

#[test]
fn test_configure_local_right_on_device_field_cycles() {
    let mut state = state_with_step(AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        family: ModelFamily::Qwen2,
        size: ModelSize::Medium,
        quantization: GgufQuantization::Q4KM,
        execution: ExecutionTarget::Auto,
        model_path: test_gguf_path(),
        focused_field: 4, // Device
        editing_idx: None,
    });
    // Auto is first in the list; right should cycle to next (Cpu on non-macOS, CoreML on macOS)
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    if let Some(AddProviderStep::ConfigureLocal { execution, .. }) = get_step(&state) {
        assert_ne!(
            *execution,
            ExecutionTarget::Auto,
            "should have cycled off Auto"
        );
    } else {
        panic!("expected ConfigureLocal");
    }
}

#[test]
fn test_configure_local_right_on_non_focused_field_does_not_affect_others() {
    let mut state = state_with_step(default_configure_local(2)); // focused Size
    let before_family;
    let before_execution;
    if let Some(AddProviderStep::ConfigureLocal {
        family, execution, ..
    }) = get_step(&state)
    {
        before_family = *family;
        before_execution = *execution;
    } else {
        panic!();
    }
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    if let Some(AddProviderStep::ConfigureLocal {
        family, execution, ..
    }) = get_step(&state)
    {
        assert_eq!(
            *family, before_family,
            "family should not change when Size is focused"
        );
        assert_eq!(*execution, before_execution, "execution should not change");
    }
}

// ── ConfigureLocal: Enter commits ─────────────────────────────────────────

#[test]
fn test_configure_local_enter_replaces_empty_primary() {
    // Default state has remote claude with empty key — Enter should replace primary
    let mut state = state_with_step(AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        family: ModelFamily::Phi,
        size: ModelSize::Small,
        quantization: GgufQuantization::Q4KM,
        execution: ExecutionTarget::Cpu,
        model_path: test_gguf_path(),
        focused_field: 0,
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    // overlay should be gone
    assert!(get_step(&state).is_none());
    // primary should now be local
    if let Some(ModelConfig::Local {
        family,
        size,
        execution,
        inference_provider,
        ..
    }) = get_primary(&state)
    {
        assert_eq!(*family, ModelFamily::Phi);
        assert_eq!(*size, ModelSize::Small);
        assert_eq!(*execution, ExecutionTarget::Cpu);
        assert_eq!(*inference_provider, InferenceProvider::LlamaCpp);
    } else {
        panic!("expected Local primary model");
    }
}

#[test]
fn test_gguf_wizard_requires_existing_file_and_keeps_dialog_open() {
    let mut state = state_with_step(AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        family: ModelFamily::Qwen2,
        size: ModelSize::Small,
        quantization: GgufQuantization::Q4KM,
        execution: ExecutionTarget::Auto,
        model_path: "/missing/finch-chat.gguf".to_string(),
        focused_field: 5,
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureLocal { .. })
    ));
    if let Some(SectionState::Models { error, .. }) = state.sections.get(&WizardSection::Models) {
        assert!(error
            .as_deref()
            .unwrap_or_default()
            .contains("existing absolute local .gguf"));
    } else {
        panic!("expected models section");
    }
}

#[test]
fn test_gguf_wizard_only_cycles_auto_and_cpu_targets() {
    let mut state = state_with_step(AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        family: ModelFamily::Qwen2,
        size: ModelSize::Small,
        quantization: GgufQuantization::Q4KM,
        execution: ExecutionTarget::Auto,
        model_path: String::new(),
        focused_field: 4,
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureLocal {
            execution: ExecutionTarget::Cpu,
            ..
        })
    ));
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureLocal {
            execution: ExecutionTarget::Auto,
            ..
        })
    ));
}

#[test]
fn test_gguf_wizard_path_survives_provider_save_and_reopen() {
    let gguf = tempfile::Builder::new().suffix(".gguf").tempfile().unwrap();
    let path = gguf.path().to_path_buf();
    let mut state = state_with_step(AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        family: ModelFamily::Gemma2,
        size: ModelSize::Small,
        quantization: GgufQuantization::Q4KM,
        execution: ExecutionTarget::Cpu,
        model_path: String::new(),
        focused_field: 5,
        editing_idx: None,
    });
    for character in path.to_string_lossy().chars() {
        handle_models_input(&mut state, key(KeyCode::Char(character))).unwrap();
    }
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(get_step(&state).is_none());
    let result = build_setup_result(&state).unwrap();
    assert!(matches!(&result.providers[0], ProviderEntry::Local {
        inference_provider: InferenceProvider::LlamaCpp,
        model_path: Some(saved),
        ..
    } if saved == &path));
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    let metrics_dir = directory.path().join("metrics");
    config_from_setup_result_with_paths(&result, metrics_dir.clone(), None)
        .save_to(&config_path)
        .unwrap();
    let reloaded =
        crate::config::load_config_from_path_with_paths(&config_path, metrics_dir, None).unwrap();
    assert_eq!(reloaded.backend.model_path.as_deref(), Some(path.as_path()));
    let mut reopened = WizardState::new(Some(&reloaded));
    assert!(matches!(get_primary(&reopened), Some(ModelConfig::Local {
        inference_provider: InferenceProvider::LlamaCpp,
        model_path: Some(saved),
        ..
    }) if saved == &path));

    // Editing the reopened primary changes that row, not the provider count.
    handle_models_input(&mut reopened, key(KeyCode::Enter)).unwrap();
    assert!(
        matches!(get_step(&reopened), Some(AddProviderStep::ConfigureLocal {
        editing_idx: Some(0), model_path: shown, ..
    }) if shown.as_str() == path.to_string_lossy().as_ref())
    );
    let replacement = tempfile::Builder::new().suffix(".gguf").tempfile().unwrap();
    for _ in 0..path.to_string_lossy().chars().count() {
        handle_models_input(&mut reopened, key(KeyCode::Backspace)).unwrap();
    }
    for character in replacement.path().to_string_lossy().chars() {
        handle_models_input(&mut reopened, key(KeyCode::Char(character))).unwrap();
    }
    handle_models_input(&mut reopened, key(KeyCode::Enter)).unwrap();
    let edited = build_setup_result(&reopened).unwrap();
    assert_eq!(edited.providers.len(), 1);
    assert!(matches!(&edited.providers[0], ProviderEntry::Local {
        model_path: Some(saved), ..
    } if saved == replacement.path()));
}

#[test]
fn test_managed_gguf_selection_survives_provider_save_and_reopen() {
    let mut state = state_with_step(AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        family: ModelFamily::Qwen2,
        size: ModelSize::Medium,
        quantization: GgufQuantization::Q5KM,
        execution: ExecutionTarget::Auto,
        model_path: String::new(),
        focused_field: 5,
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(get_step(&state).is_none());

    let expected = managed_gguf_artifact(
        ModelFamily::Qwen2,
        ModelSize::Medium,
        GgufQuantization::Q5KM,
    )
    .unwrap();
    let result = build_setup_result(&state).unwrap();
    assert!(matches!(&result.providers[0], ProviderEntry::Local {
        model_path: None,
        managed_artifact: Some(artifact),
        ..
    } if artifact == &expected));

    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    let metrics_dir = directory.path().join("metrics");
    config_from_setup_result_with_paths(&result, metrics_dir.clone(), None)
        .save_to(&config_path)
        .unwrap();
    let reloaded =
        crate::config::load_config_from_path_with_paths(&config_path, metrics_dir, None).unwrap();
    assert_eq!(reloaded.backend.model_path, None);
    assert_eq!(reloaded.backend.managed_artifact.as_ref(), Some(&expected));

    let reopened = WizardState::new(Some(&reloaded));
    assert!(matches!(get_primary(&reopened), Some(ModelConfig::Local {
        model_path: None,
        managed_artifact: Some(artifact),
        ..
    }) if artifact == &expected));
}

#[test]
fn test_unsupported_managed_gguf_keeps_dialog_open_with_actionable_error() {
    let mut state = state_with_step(AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        family: ModelFamily::Phi,
        size: ModelSize::Small,
        quantization: GgufQuantization::Q4KM,
        execution: ExecutionTarget::Auto,
        model_path: String::new(),
        focused_field: 5,
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureLocal { .. })
    ));
    let Some(SectionState::Models {
        error: Some(error), ..
    }) = state.sections.get(&WizardSection::Models)
    else {
        panic!("expected managed GGUF validation error");
    };
    assert!(error.contains("not in Finch's managed GGUF catalog"));
    assert!(error.contains("existing absolute .gguf file"));
}

#[test]
fn test_editing_legacy_local_chat_to_gguf_clears_onnx_repository() {
    let original_path = "/models/old-chat.onnx";
    let config = crate::config::Config::with_providers(vec![ProviderEntry::Local {
        inference_provider: InferenceProvider::LegacyOnnx,
        execution_target: ExecutionTarget::Cpu,
        model_family: ModelFamily::Qwen2,
        model_size: ModelSize::Small,
        model_repo: Some("onnx-community/old-chat".into()),
        model_path: Some(original_path.into()),
        managed_artifact: None,
        enabled: true,
        name: Some("my-local-chat".into()),
    }]);
    let mut state = WizardState::new(Some(&config));
    let unchanged = build_setup_result(&state).unwrap();
    assert!(matches!(&unchanged.providers[0], ProviderEntry::Local {
        inference_provider: InferenceProvider::LegacyOnnx,
        model_repo: Some(repo),
        model_path: Some(saved),
        ..
    } if repo == "onnx-community/old-chat" && saved == std::path::Path::new(original_path)));
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(
        matches!(get_step(&state), Some(AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        model_path,
        focused_field: 5,
        ..
    }) if model_path.is_empty())
    );
    let gguf = tempfile::Builder::new().suffix(".gguf").tempfile().unwrap();
    for character in gguf.path().to_string_lossy().chars() {
        handle_models_input(&mut state, key(KeyCode::Char(character))).unwrap();
    }
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    let result = build_setup_result(&state).unwrap();
    assert_eq!(result.providers.len(), 1);
    assert!(matches!(&result.providers[0], ProviderEntry::Local {
        inference_provider: InferenceProvider::LlamaCpp,
        model_repo: None,
        model_path: Some(saved),
        name: Some(name),
        ..
    } if saved == gguf.path() && name == "my-local-chat"));
}

#[test]
fn test_configure_local_enter_adds_tool_model_when_primary_is_configured() {
    let mut state = state_with_step(default_configure_local(0));
    // Give primary a real API key so it won't be replaced
    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *primary_model = ModelConfig::Remote {
            provider: "claude".to_string(),
            name: "claude".to_string(),
            api_key: "sk-ant-abc123".to_string(),
            model: String::new(),
            enabled: true,
            persisted: None,
        };
    }
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    let tools = get_tool_models(&state);
    assert_eq!(tools.len(), 1);
    if let ModelConfig::Local { family, size, .. } = &tools[0] {
        assert_eq!(*family, ModelFamily::Qwen2);
        assert_eq!(*size, ModelSize::Medium);
    } else {
        panic!("expected Local tool model");
    }
}

// ── ConfigureLocal: Esc goes back ─────────────────────────────────────────

#[test]
fn test_configure_local_esc_closes_overlay() {
    let mut state = state_with_step(default_configure_local(0));
    handle_models_input(&mut state, key(KeyCode::Esc)).unwrap();
    assert!(get_step(&state).is_none());
}

// ── ConfigureRemote: focus navigation ────────────────────────────────────

#[test]
fn test_configure_remote_down_advances_focused_field() {
    let mut state = state_with_step(default_configure_remote(0));
    handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
    if let Some(AddProviderStep::ConfigureRemote { focused_field, .. }) = get_step(&state) {
        assert_eq!(*focused_field, 1);
    } else {
        panic!("expected ConfigureRemote");
    }
}

#[test]
fn test_configure_remote_up_clamps_at_zero() {
    let mut state = state_with_step(default_configure_remote(0));
    handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
    if let Some(AddProviderStep::ConfigureRemote { focused_field, .. }) = get_step(&state) {
        assert_eq!(*focused_field, 0);
    } else {
        panic!("expected ConfigureRemote");
    }
}

#[test]
fn test_configure_remote_down_clamps_at_three() {
    let mut state = state_with_step(default_configure_remote(3));
    handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
    if let Some(AddProviderStep::ConfigureRemote { focused_field, .. }) = get_step(&state) {
        assert_eq!(*focused_field, 3);
    } else {
        panic!("expected ConfigureRemote");
    }
}

// ── ConfigureRemote: provider cycling ────────────────────────────────────

#[test]
fn test_configure_remote_right_cycles_provider_forward() {
    let mut state = state_with_step(default_configure_remote(0)); // focused Provider
    let initial_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, _, _, _)| *id == "grok")
        .unwrap();
    let expected_idx = (initial_idx + 1) % CLOUD_PROVIDERS.len();
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    if let Some(AddProviderStep::ConfigureRemote {
        provider_idx,
        model,
        ..
    }) = get_step(&state)
    {
        assert_eq!(*provider_idx, expected_idx);
        // model should reset to default for new provider
        let expected_model = CLOUD_PROVIDERS[expected_idx].2;
        assert_eq!(model.as_str(), expected_model);
        assert_eq!(
            catalog_model_provenance(&state),
            ModelSelectionProvenance::Blank
        );
    } else {
        panic!("expected ConfigureRemote");
    }
}

#[test]
fn test_configure_remote_left_wraps_provider_to_last() {
    let mut state = state_with_step(default_configure_remote(0));
    let initial_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, _, _, _)| *id == "grok")
        .unwrap();
    let expected_idx = (initial_idx + CLOUD_PROVIDERS.len() - 1) % CLOUD_PROVIDERS.len();
    handle_models_input(&mut state, key(KeyCode::Left)).unwrap();
    if let Some(AddProviderStep::ConfigureRemote {
        provider_idx,
        model,
        ..
    }) = get_step(&state)
    {
        assert_eq!(*provider_idx, expected_idx);
        let expected_model = CLOUD_PROVIDERS[expected_idx].2;
        assert_eq!(model.as_str(), expected_model);
        assert_eq!(
            catalog_model_provenance(&state),
            ModelSelectionProvenance::DefaultGenerated
        );
    } else {
        panic!("expected ConfigureRemote");
    }
}

// ── ConfigureRemote: model cycling ───────────────────────────────────────

#[test]
fn test_configure_remote_right_on_model_field_cycles_to_next_known_model() {
    // OpenAI's deliberately small fallback has multiple choices to cycle.
    let openai_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, _, _, _)| *id == "openai")
        .unwrap();
    let first_model = known_models_for("openai")[0].clone();
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: openai_idx,
        name: "openai".to_string(),
        model: first_model,
        api_key: Some(String::new()),
        focused_field: 2, // Model field
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    if let Some(AddProviderStep::ConfigureRemote { model, .. }) = get_step(&state) {
        let models = known_models_for("openai");
        assert_eq!(model, &models[1]);
    } else {
        panic!("expected ConfigureRemote");
    }
}

#[test]
fn test_configure_remote_left_on_model_field_cycles_backward() {
    let claude_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, _, _, _)| *id == "claude")
        .unwrap();
    let first_model = known_models_for("claude")[0].clone();
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: claude_idx,
        name: "claude".to_string(),
        model: first_model,
        api_key: Some(String::new()),
        focused_field: 2,
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Left)).unwrap();
    if let Some(AddProviderStep::ConfigureRemote { model, .. }) = get_step(&state) {
        let models = known_models_for("claude");
        // wraps from first to last
        assert_eq!(model, &models[models.len() - 1]);
    } else {
        panic!("expected ConfigureRemote");
    }
}

// ── ConfigureRemote: text input on API key / model ────────────────────────

#[test]
fn test_configure_remote_typing_appends_to_api_key_field() {
    let mut state = state_with_step(default_configure_remote(3)); // focused APIKey
    handle_models_input(&mut state, key(KeyCode::Char('s'))).unwrap();
    handle_models_input(&mut state, key(KeyCode::Char('k'))).unwrap();
    if let Some(AddProviderStep::ConfigureRemote { api_key, .. }) = get_step(&state) {
        assert_eq!(api_key.as_deref(), Some("sk"));
    } else {
        panic!("expected ConfigureRemote");
    }
}

#[test]
fn test_configure_remote_typing_appends_to_model_field() {
    let mut state = state_with_step(default_configure_remote(2)); // focused Model
    handle_models_input(&mut state, key(KeyCode::Char('m'))).unwrap();
    handle_models_input(&mut state, key(KeyCode::Char('y'))).unwrap();
    if let Some(AddProviderStep::ConfigureRemote { model, .. }) = get_step(&state) {
        // starts with default model then appends
        assert!(model.ends_with("my"));
    } else {
        panic!("expected ConfigureRemote");
    }
}

#[test]
fn test_configure_remote_backspace_removes_from_api_key() {
    let grok_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, _, _, _)| *id == "grok")
        .unwrap();
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: grok_idx,
        name: "grok".to_string(),
        model: "grok-code-fast-1".to_string(),
        api_key: Some("abc".to_string()),
        focused_field: 3,
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Backspace)).unwrap();
    if let Some(AddProviderStep::ConfigureRemote { api_key, .. }) = get_step(&state) {
        assert_eq!(api_key.as_deref(), Some("ab"));
    } else {
        panic!("expected ConfigureRemote");
    }
}

#[test]
fn test_configure_remote_typing_on_provider_field_is_ignored() {
    let mut state = state_with_step(default_configure_remote(0)); // focused Provider (field 0)
    let before_key;
    if let Some(AddProviderStep::ConfigureRemote { api_key, .. }) = get_step(&state) {
        before_key = api_key.clone();
    } else {
        panic!();
    }
    handle_models_input(&mut state, key(KeyCode::Char('x'))).unwrap();
    if let Some(AddProviderStep::ConfigureRemote { api_key, .. }) = get_step(&state) {
        assert_eq!(
            api_key, &before_key,
            "typing on Provider field should not modify api_key"
        );
    }
}

// ── ConfigureRemote: Enter commits ────────────────────────────────────────

#[test]
fn test_configure_remote_enter_replaces_empty_primary() {
    let grok_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, _, _, _)| *id == "grok")
        .unwrap();
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: grok_idx,
        name: "grok".to_string(),
        model: "grok-code-fast-1".to_string(),
        api_key: Some("xai-test-key".to_string()),
        focused_field: 3,
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(get_step(&state).is_none());
    if let Some(ModelConfig::Remote {
        provider,
        api_key,
        model,
        ..
    }) = get_primary(&state)
    {
        assert_eq!(provider.as_str(), "grok");
        assert_eq!(api_key.as_str(), "xai-test-key");
        assert_eq!(model.as_str(), "grok-code-fast-1");
    } else {
        panic!("expected Remote primary model");
    }
}

#[test]
fn gemini_25_default_survives_save_load_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    let metrics_dir = directory.path().join("metrics");
    let gemini_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, _, _, _)| *id == "gemini")
        .unwrap();
    let canonical_model = "gemini-2.5-flash";
    assert_eq!(CLOUD_PROVIDERS[gemini_idx].2, canonical_model);
    assert_eq!(known_models_for("gemini"), vec![canonical_model]);

    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: gemini_idx,
        name: "gemini".to_string(),
        model: canonical_model.to_string(),
        api_key: Some("gemini-test-key-that-is-long-enough-123456".to_string()),
        focused_field: 3,
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    let result = build_setup_result(&state).unwrap();
    config_from_setup_result_with_paths(&result, metrics_dir.clone(), None)
        .save_to(&config_path)
        .unwrap();

    let loaded =
        crate::config::load_config_from_path_with_paths(&config_path, metrics_dir, None).unwrap();
    assert!(matches!(
        loaded.providers.first(),
        Some(ProviderEntry::Gemini { model: Some(model), .. }) if model == canonical_model
    ));
    let reopened = WizardState::new_with_catalog_cache_dir(Some(&loaded), None);
    assert!(matches!(
        get_primary(&reopened),
        Some(ModelConfig::Remote { provider, model, .. })
            if provider == "gemini" && model == canonical_model
    ));
}

#[test]
fn test_configure_remote_requires_refresh_or_manual_id_when_model_empty() {
    let claude_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, _, _, _)| *id == "claude")
        .unwrap();
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: claude_idx,
        name: "claude".to_string(),
        model: String::new(),
        api_key: Some("sk-ant-key".to_string()),
        focused_field: 3,
        editing_idx: None,
    });
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote {
            model,
            focused_field: 2,
            ..
        }) if model.is_empty()
    ));
    assert!(matches!(
        state.sections.get(&WizardSection::Models),
        Some(SectionState::Models {
            catalog_error: Some(error),
            ..
        }) if error.contains("Ctrl+R")
    ));
}

// ── ConfigureRemote: Esc goes back ────────────────────────────────────────

#[test]
fn test_configure_remote_esc_closes_overlay() {
    let mut state = state_with_step(default_configure_remote(0));
    handle_models_input(&mut state, key(KeyCode::Esc)).unwrap();
    assert!(get_step(&state).is_none());
}

// ── SelectAddType routing ─────────────────────────────────────────────────

#[test]
fn test_select_add_type_enter_on_cloud_opens_configure_remote() {
    let mut state = state_with_step(AddProviderStep::SelectAddType { selected: 0 });
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(
        matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote {
                provider_idx: 0,
                ..
            })
        ),
        "selecting first cloud provider should open ConfigureRemote at index 0"
    );
}

#[test]
fn test_select_add_type_enter_on_local_opens_configure_local() {
    let n_cloud = CLOUD_PROVIDERS.len();
    let mut state = state_with_step(AddProviderStep::SelectAddType { selected: n_cloud });
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(
        matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureLocal { .. })
        ),
        "selecting 'Local model' should open ConfigureLocal"
    );
}

#[test]
fn test_api_key_provider_starts_on_api_key_field() {
    let grok_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "grok")
        .unwrap();
    let mut state = state_with_step(AddProviderStep::SelectAddType { selected: grok_idx });
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    if let Some(AddProviderStep::ConfigureRemote {
        provider_idx,
        api_key,
        focused_field,
        ..
    }) = get_step(&state)
    {
        assert_eq!(*provider_idx, grok_idx);
        assert_eq!(api_key.as_deref(), Some(""));
        assert_eq!(
            *focused_field, 3,
            "API-key providers should open focused on the API key field"
        );
    } else {
        panic!(
            "expected ConfigureRemote, got {:?}",
            get_step(&state).map(|s| format!("{:?}", s))
        );
    }
}

#[test]
fn test_select_add_type_esc_closes_overlay() {
    let mut state = state_with_step(AddProviderStep::SelectAddType { selected: 0 });
    handle_models_input(&mut state, key(KeyCode::Esc)).unwrap();
    assert!(
        get_step(&state).is_none(),
        "Esc on SelectAddType should close overlay"
    );
}

// ── build_setup_result: inference_provider propagation ───────────────────

#[test]
fn test_build_setup_result_uses_inference_provider_from_local_model() {
    let mut state = WizardState::new(None);
    // Set primary to a local model with ONNX provider
    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *primary_model = ModelConfig::Local {
            family: ModelFamily::Llama3,
            size: ModelSize::Large,
            execution: ExecutionTarget::Cpu,
            inference_provider: InferenceProvider::LlamaCpp,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            persisted: None,
        };
    }
    let result = build_setup_result(&state).unwrap();
    assert!(result.backend_enabled);
    assert_eq!(result.inference_provider, InferenceProvider::LlamaCpp);
    assert_eq!(result.model_family, ModelFamily::Llama3);
    assert_eq!(result.model_size, ModelSize::Large);
    assert_eq!(result.execution_target, ExecutionTarget::Cpu);
}

#[test]
fn test_build_setup_result_remote_primary_disables_backend() {
    let mut state = WizardState::new(None);
    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *primary_model = ModelConfig::Remote {
            provider: "claude".to_string(),
            name: "claude".to_string(),
            api_key: "sk-ant-test".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            enabled: true,
            persisted: None,
        };
    }
    let result = build_setup_result(&state).unwrap();
    assert!(!result.backend_enabled);
    assert_eq!(result.claude_api_key, "sk-ant-test");
}

// ── ModelConfig::Local inference_provider field ───────────────────────────

#[test]
fn test_model_config_local_stores_inference_provider() {
    let config = ModelConfig::Local {
        family: ModelFamily::Gemma2,
        size: ModelSize::XLarge,
        execution: ExecutionTarget::Cpu,
        inference_provider: InferenceProvider::LlamaCpp,
        model_path: None,
        managed_artifact: None,
        enabled: true,
        persisted: None,
    };
    if let ModelConfig::Local {
        inference_provider, ..
    } = config
    {
        assert_eq!(inference_provider, InferenceProvider::LlamaCpp);
    } else {
        panic!("unexpected variant");
    }
}

#[test]
fn test_wizard_state_new_loads_inference_provider_from_existing_config() {
    use crate::config::{BackendConfig, Config};
    let mut config = Config::with_providers(vec![]);
    config.backend = BackendConfig {
        enabled: true,
        inference_provider: InferenceProvider::LlamaCpp,
        execution_target: ExecutionTarget::Cpu,
        model_family: ModelFamily::DeepSeek,
        model_size: ModelSize::Large,
        ..Default::default()
    };
    let state = WizardState::new(Some(&config));
    if let Some(ModelConfig::Local {
        inference_provider,
        family,
        ..
    }) = get_primary(&state)
    {
        assert_eq!(*inference_provider, InferenceProvider::LlamaCpp);
        assert_eq!(*family, ModelFamily::DeepSeek);
    } else {
        panic!("expected Local primary when backend is enabled");
    }
}

#[test]
fn test_coreml_policy_survives_wizard_mapping_save_and_reload_for_every_compute_unit() {
    use crate::config::{Config, CoreMlComputeUnits};

    for compute_units in [
        CoreMlComputeUnits::All,
        CoreMlComputeUnits::CpuAndNeuralEngine,
        CoreMlComputeUnits::CpuAndGpu,
        CoreMlComputeUnits::CpuOnly,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let metrics_dir = directory.path().join("metrics");
        let providers = vec![ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: Some("local-coreml-policy-test".to_string()),
        }];
        let mut existing = Config::with_providers_and_paths(providers, metrics_dir.clone(), None);
        existing.backend.coreml = CoreMlConfig {
            compute_units,
            profile_compute_plan: true,
            enable_subgraphs: true,
        };

        let state = WizardState::new_with_catalog_cache_dir(Some(&existing), None);
        let result = build_setup_result(&state).unwrap();
        assert_eq!(result.coreml, existing.backend.coreml);

        config_from_setup_result_with_paths(&result, metrics_dir.clone(), None)
            .save_to(&config_path)
            .unwrap();
        let reloaded =
            crate::config::load_config_from_path_with_paths(&config_path, metrics_dir, None)
                .unwrap();
        assert_eq!(reloaded.backend.coreml, existing.backend.coreml);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn test_setup_coreml_auto_label_is_dispatcher_not_ane_only_or_fastest() {
    let label = ExecutionTarget::CoreML.name();
    let description = ExecutionTarget::CoreML.description();

    assert_eq!(label, "CoreML (Auto: ANE/GPU/CPU)");
    assert!(description.contains("automatic compute-unit selection"));
    assert!(!description.to_ascii_lowercase().contains("fastest"));
    assert!(!description.contains("ANE only"));
}

#[cfg(target_os = "macos")]
#[test]
fn test_reopened_coreml_policy_renders_requested_units_for_every_policy() {
    use crate::config::{Config, CoreMlComputeUnits};

    for (compute_units, expected) in [
        (CoreMlComputeUnits::All, "CoreML (Auto: ANE/GPU/CPU)"),
        (CoreMlComputeUnits::CpuAndNeuralEngine, "CoreML (CPU + ANE)"),
        (CoreMlComputeUnits::CpuAndGpu, "CoreML (CPU + GPU)"),
        (CoreMlComputeUnits::CpuOnly, "CoreML (CPU only)"),
    ] {
        let coreml = CoreMlConfig {
            compute_units,
            ..CoreMlConfig::default()
        };
        let mut reopened = Config::with_providers(vec![ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::CoreML,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: Some("reopened-coreml".to_string()),
        }]);
        reopened.backend.coreml = coreml;
        let state = WizardState::new_with_catalog_cache_dir(Some(&reopened), None);
        assert_eq!(state.coreml, coreml);
        assert_eq!(
            execution_target_display(ExecutionTarget::CoreML, state.coreml),
            expected
        );
    }
}

#[test]
fn test_cloud_primary_keeps_local_qwen_as_tool_model_on_reopen() {
    use crate::config::{Config, ProviderEntry};

    #[cfg(target_os = "macos")]
    let execution_target = ExecutionTarget::CoreML;
    #[cfg(not(target_os = "macos"))]
    let execution_target = ExecutionTarget::Cpu;

    let original = Config::with_providers(vec![
        ProviderEntry::Grok {
            api_key: "xai-test-key".to_string(),
            model: Some("grok-code-fast-1".to_string()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("grok-code-fast-1".to_string()),
        },
        ProviderEntry::Local {
            inference_provider: InferenceProvider::LegacyOnnx,
            execution_target,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Small,
            model_repo: Some("onnx-community/Qwen2.5-Coder-3B-Instruct".to_string()),
            model_path: Some("/models/qwen-coder".into()),
            managed_artifact: None,
            enabled: true,
            name: Some("local-qwen".to_string()),
        },
    ]);

    let state = WizardState::new(Some(&original));
    assert!(matches!(
        get_primary(&state),
        Some(ModelConfig::Remote { provider, .. }) if provider == "grok"
    ));
    assert!(matches!(
        get_tool_models(&state).as_slice(),
        [ModelConfig::Local {
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            execution,
            ..
        }] if *execution == execution_target
    ));

    let saved = build_setup_result(&state).unwrap();
    assert_eq!(saved.providers.len(), 2);
    assert!(matches!(saved.providers[0], ProviderEntry::Grok { .. }));
    assert!(matches!(
        saved.providers[1],
        ProviderEntry::Local {
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Small,
            execution_target: saved_execution_target,
            model_repo: Some(ref repo),
            model_path: Some(ref path),
            name: Some(ref name),
            ..
        } if saved_execution_target == execution_target
            && repo == "onnx-community/Qwen2.5-Coder-3B-Instruct"
            && path == &std::path::PathBuf::from("/models/qwen-coder")
            && name == "local-qwen"
    ));

    let reopened = WizardState::new(Some(&Config::with_providers(saved.providers)));
    assert!(matches!(
        get_tool_models(&reopened).as_slice(),
        [ModelConfig::Local {
            family: ModelFamily::Qwen2,
            ..
        }]
    ));
}

#[test]
fn test_wizard_round_trip_preserves_provider_metadata() {
    use crate::config::{Config, ProviderEntry, ReasoningEffort};

    let providers = vec![
        ProviderEntry::Openai {
            api_key: "openai-key".to_string(),
            model: Some("gpt-test".to_string()),
            base_url: Some("https://compatible.example/api".to_string()),
            chat_path: Some("/custom/chat".to_string()),
            models_path: Some("/custom/models".to_string()),
            name: Some("reasoning-profile".to_string()),
            reasoning_effort: Some(ReasoningEffort::High),
        },
        ProviderEntry::Ollama {
            model: "qwen2.5:7b".to_string(),
            base_url: "http://model-host:11434".to_string(),
            name: Some("office-qwen".to_string()),
        },
        ProviderEntry::RemoteDaemon {
            address: "https://finch-host:11435".to_string(),
            name: Some("build-machine".to_string()),
        },
    ];
    let state = WizardState::new(Some(&Config::with_providers(providers.clone())));
    let saved = build_setup_result(&state).unwrap();

    assert_eq!(saved.providers, providers);
}

#[test]
fn ordinary_remote_edits_preserve_exact_connection_profile_metadata() {
    use crate::config::{Config, ReasoningEffort};

    let cases = vec![
        ProviderEntry::Claude {
            api_key: "old-key".to_string(),
            model: Some("old-model".to_string()),
            base_url: Some("https://claude-compatible.example/v1".to_string()),
            chat_path: Some("https://chat.example/claude?preview=1".to_string()),
            models_path: Some("https://models.example/claude?account=a".to_string()),
            name: Some("claude-old".to_string()),
        },
        ProviderEntry::Openai {
            api_key: "old-key".to_string(),
            model: Some("old-model".to_string()),
            base_url: Some("https://openai-compatible.example/v1".to_string()),
            chat_path: Some("https://chat.example/openai?preview=1".to_string()),
            models_path: Some("https://models.example/openai?account=b".to_string()),
            name: Some("openai-old".to_string()),
            reasoning_effort: Some(ReasoningEffort::High),
        },
        ProviderEntry::Grok {
            api_key: "old-key".to_string(),
            model: Some("old-model".to_string()),
            base_url: Some("https://xai-compatible.example/v1".to_string()),
            chat_path: Some("https://chat.example/xai?preview=1".to_string()),
            models_path: Some("https://models.example/xai?account=c".to_string()),
            name: Some("xai-old".to_string()),
        },
        ProviderEntry::Mistral {
            api_key: "old-key".to_string(),
            model: Some("old-model".to_string()),
            base_url: Some("https://mistral-compatible.example/v1".to_string()),
            chat_path: Some("https://chat.example/mistral?preview=1".to_string()),
            models_path: Some("https://models.example/mistral?account=d".to_string()),
            name: Some("mistral-old".to_string()),
        },
    ];

    for original in cases {
        let expected_type = original.provider_type().to_string();
        let mut state = WizardState::new(Some(&Config::with_providers(vec![original.clone()])));
        edit_primary_remote(&mut state, "renamed", "new-model", "new-key");
        let saved = build_setup_result(&state).unwrap();
        let edited = &saved.providers[0];
        assert_eq!(edited.provider_type(), expected_type);
        match (&original, edited) {
            (
                ProviderEntry::Claude {
                    base_url,
                    chat_path,
                    models_path,
                    ..
                },
                ProviderEntry::Claude {
                    api_key,
                    model,
                    base_url: actual_base,
                    chat_path: actual_chat,
                    models_path: actual_models,
                    name,
                },
            )
            | (
                ProviderEntry::Grok {
                    base_url,
                    chat_path,
                    models_path,
                    ..
                },
                ProviderEntry::Grok {
                    api_key,
                    model,
                    base_url: actual_base,
                    chat_path: actual_chat,
                    models_path: actual_models,
                    name,
                },
            )
            | (
                ProviderEntry::Mistral {
                    base_url,
                    chat_path,
                    models_path,
                    ..
                },
                ProviderEntry::Mistral {
                    api_key,
                    model,
                    base_url: actual_base,
                    chat_path: actual_chat,
                    models_path: actual_models,
                    name,
                },
            ) => {
                assert_eq!(
                    (actual_base, actual_chat, actual_models),
                    (base_url, chat_path, models_path)
                );
                assert_eq!(
                    (api_key.as_str(), model.as_deref(), name.as_deref()),
                    ("new-key", Some("new-model"), Some("renamed"))
                );
            }
            (
                ProviderEntry::Openai {
                    base_url,
                    chat_path,
                    models_path,
                    reasoning_effort,
                    ..
                },
                ProviderEntry::Openai {
                    api_key,
                    model,
                    base_url: actual_base,
                    chat_path: actual_chat,
                    models_path: actual_models,
                    name,
                    reasoning_effort: actual_reasoning,
                },
            ) => {
                assert_eq!(
                    (actual_base, actual_chat, actual_models),
                    (base_url, chat_path, models_path)
                );
                assert_eq!(actual_reasoning, reasoning_effort);
                assert_eq!(
                    (api_key.as_str(), model.as_deref(), name.as_deref()),
                    ("new-key", Some("new-model"), Some("renamed"))
                );
            }
            _ => panic!("provider type changed during edit"),
        }
    }
}

#[test]
fn test_edited_system_prompt_is_included_in_setup_result() {
    let mut state = WizardState::new(None);
    if let Some(SectionState::Personas {
        available_personas,
        selected_idx,
        default_persona,
        ..
    }) = state.sections.get_mut(&WizardSection::Personas)
    {
        *selected_idx = available_personas
            .iter()
            .position(|persona| persona.slug == "default")
            .unwrap();
        *default_persona = "default".to_string();
        available_personas[*selected_idx].system_prompt =
            "Say hello once you've loaded.".to_string();
    } else {
        panic!("expected persona section");
    }

    let result = build_setup_result(&state).unwrap();
    assert_eq!(
        result.custom_system_prompt.as_deref(),
        Some("Say hello once you've loaded.")
    );
    let config = config_from_setup_result(&result);
    assert_eq!(config.active_persona, "default");
}

#[test]
fn chooser_keeps_platform_api_key_distinct_from_native_chatgpt_subscription() {
    assert!(CLOUD_PROVIDERS.iter().any(|(id, ..)| *id == "openai"));
    assert!(CLOUD_PROVIDERS.iter().any(|(id, ..)| *id == "chatgpt"));
    assert!(!CLOUD_PROVIDERS
        .iter()
        .any(|(id, ..)| *id == "chatgpt_subscription"));
}

#[tokio::test]
async fn legacy_chatgpt_setup_fails_before_save() {
    let mut result = build_setup_result(&WizardState::new(None)).unwrap();
    result.providers = vec![ProviderEntry::LegacyChatgptSubscription {
        credential_ref: "codex-app-server:managed".into(),
        model: Some("gpt-5.6-sol".into()),
        name: Some("legacy".into()),
    }];

    let error = validate_and_apply_for(SetupInvocation::Command, &result)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("Legacy chatgpt_subscription profiles are unsupported"));
    assert!(message.contains("finch setup") || message.contains("configure OpenAI Platform"));
}

#[test]
fn named_credential_setup_reopen_and_save_preserves_reference_without_secret() {
    use crate::config::{
        AudienceBinding, CredentialBinding, CredentialKind, CredentialLifecycle,
        CredentialProvider, EndpointFamily, ProviderCredential,
    };
    let credential = ProviderCredential {
        name: "openai-work".into(),
        kind: CredentialKind::ApiKey,
        provider: CredentialProvider::OpenaiPlatform,
        issuer: "openai-platform".into(),
        audience: AudienceBinding::standard(EndpointFamily::OpenaiPlatform),
        tenant: None,
        project: None,
        account: Some("work".into()),
        scopes: std::collections::BTreeSet::new(),
        secret_ref: "env:OPENAI_WORK_API_KEY".into(),
        lifecycle: CredentialLifecycle::default(),
        revocation: Default::default(),
    };
    let profile = ProviderEntry::Credentialed {
        provider: CredentialProvider::OpenaiPlatform,
        credential: CredentialBinding {
            credential_ref: "openai-work".into(),
            audience: None,
            tenant: None,
            project: None,
            account: Some("work".into()),
            required_scopes: std::collections::BTreeSet::new(),
        },
        model: Some("gpt-5.6-sol".into()),
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some("work-reasoning".into()),
        reasoning_effort: Some(crate::config::ReasoningEffort::High),
    };
    let existing = crate::config::Config::with_providers(vec![profile.clone()])
        .with_credentials(vec![credential.clone()]);

    let result = build_setup_result(&WizardState::new(Some(&existing))).unwrap();
    assert_eq!(result.providers, vec![profile]);
    assert_eq!(result.credentials, vec![credential]);
    let saved = config_from_setup_result(&result);
    saved.validate().unwrap();
    let serialized = toml::to_string(&saved.credentials()[0]).unwrap();
    assert!(serialized.contains("env:OPENAI_WORK_API_KEY"));
    assert!(!serialized.contains("sk-"));
}

#[tokio::test]
async fn test_expired_refreshable_chatgpt_grok_local_setup_round_trip_preserves_exact_graph() {
    use crate::config::{
        load_config_from_path_with_paths, AudienceBinding, CredentialBinding, CredentialKind,
        CredentialLifecycle, CredentialProvider, EndpointFamily, ProviderCredential,
        ReasoningEffort,
    };
    use chrono::{DateTime, Utc};
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    let reopened_path = directory.path().join("reopened.toml");
    let metrics_dir = directory.path().join("metrics");
    let scopes = crate::providers::chatgpt_required_scopes();
    let providers = vec![
        ProviderEntry::Credentialed {
            provider: CredentialProvider::ChatgptSubscription,
            credential: CredentialBinding {
                credential_ref: "chatgpt:default".into(),
                audience: Some(AudienceBinding::standard(
                    EndpointFamily::ChatgptSubscription,
                )),
                tenant: None,
                project: None,
                account: None,
                required_scopes: scopes.clone(),
            },
            model: Some("gpt-5.6-sol".into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("ChatGPT Personal".into()),
            reasoning_effort: Some(ReasoningEffort::High),
        },
        ProviderEntry::Grok {
            api_key: "xai-test-preserved".into(),
            model: Some("grok-code-fast-1".into()),
            base_url: Some("https://xai-compatible.example/v1".into()),
            chat_path: Some("/chat/completions?profile=build".into()),
            models_path: Some("/models?profile=build".into()),
            name: Some("Grok Build".into()),
        },
        ProviderEntry::Local {
            inference_provider: InferenceProvider::LegacyOnnx,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: Some("Qwen/Qwen2.5-Coder-7B-Instruct-ONNX".into()),
            model_path: Some(directory.path().join("models/qwen")),
            managed_artifact: None,
            enabled: true,
            name: Some("Local Qwen Medium".into()),
        },
    ];
    let future_expiry: DateTime<Utc> = "2099-01-02T03:04:05Z".parse().unwrap();
    let expired_at: DateTime<Utc> = "2000-01-02T03:04:05Z".parse().unwrap();
    let credential = ProviderCredential {
        name: "chatgpt:default".into(),
        kind: CredentialKind::OauthDevice,
        provider: CredentialProvider::ChatgptSubscription,
        issuer: "openai-chatgpt".into(),
        audience: AudienceBinding::standard(EndpointFamily::ChatgptSubscription),
        tenant: None,
        project: None,
        account: Some("account-123".into()),
        scopes,
        secret_ref: "oauth-store:chatgpt:default".into(),
        lifecycle: CredentialLifecycle::Active {
            expires_at: Some(future_expiry),
            refreshable: true,
        },
        revocation: Default::default(),
    };
    let source = crate::config::Config::with_providers_and_paths(
        providers.clone(),
        metrics_dir.clone(),
        None,
    )
    .with_credentials(vec![credential.clone()]);
    source.save_to(&config_path).unwrap();

    // Model a real short-lived OAuth access lease aging after it was saved.
    let serialized = std::fs::read_to_string(&config_path).unwrap();
    let serialized = serialized
        .replace("2099-01-02T03:04:05Z", "2000-01-02T03:04:05Z")
        .replace("2099-01-02T03:04:05+00:00", "2000-01-02T03:04:05+00:00");
    assert!(
        serialized.contains("2000-01-02T03:04:05"),
        "fixture must contain an expired refreshable lease"
    );
    std::fs::write(&config_path, serialized).unwrap();
    let before_open = std::fs::read(&config_path).unwrap();

    let opened = load_config_from_path_with_paths(&config_path, metrics_dir.clone(), None)
        .expect("refreshable expiry must not make static configuration unloadable");
    let mut expected_credential = credential;
    expected_credential.lifecycle = CredentialLifecycle::Active {
        expires_at: Some(expired_at),
        refreshable: true,
    };
    assert_eq!(opened.providers, providers);
    assert_eq!(opened.credentials(), &[expected_credential.clone()]);

    let mut state = WizardState::new(Some(&opened));
    state.current_section = WizardSection::Models;
    let rendered = render_wizard_text_at(&state, 120, 30);
    assert!(rendered.contains("ChatGPT Personal"), "{rendered}");
    assert!(rendered.contains("Grok Build"), "{rendered}");
    assert!(rendered.contains("Local Qwen"), "{rendered}");
    assert!(!rendered.contains("Claude"), "{rendered}");
    assert!(!rendered.contains("[Not configured]"), "{rendered}");

    assert_eq!(
        handle_wizard_key(
            &mut state,
            modified_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .unwrap(),
        WizardAction::Continue
    );
    assert_eq!(
        handle_wizard_key(&mut state, key(KeyCode::Char('y'))).unwrap(),
        WizardAction::Cancel
    );
    assert_eq!(
        std::fs::read(&config_path).unwrap(),
        before_open,
        "opening and cancelling setup must be byte-for-byte read-only"
    );

    let result = build_setup_result(&WizardState::new(Some(&opened))).unwrap();
    assert_eq!(result.providers, providers);
    assert_eq!(result.credentials, vec![expected_credential.clone()]);
    let authenticator = FakeChatGptSetupAuthenticator::default();
    for invocation in [
        SetupInvocation::FirstRun,
        SetupInvocation::Command,
        SetupInvocation::Repl,
    ] {
        let mut editor = ScriptedRecoveryEditor::new([]);
        let (saved, committed, compensations) =
            run_chatgpt_setup_recovery_loop(invocation, &result, &authenticator, &mut editor)
                .await
                .unwrap()
                .expect("an unchanged valid provider graph must be ready to save");
        assert_eq!(committed.providers, result.providers);
        assert_eq!(committed.credentials, result.credentials);
        assert!(compensations.is_empty());
        assert!(editor.recoveries.is_empty());
        assert!(
            authenticator.calls.lock().unwrap().is_empty(),
            "{invocation:?} must not refresh a persisted credential during setup"
        );

        let invocation_path = reopened_path.with_extension(format!("{invocation:?}.toml"));
        save_chatgpt_setup_config(&saved, &compensations, &authenticator, |config| {
            config.save_to(&invocation_path)
        })
        .unwrap();
        let reopened =
            load_config_from_path_with_paths(&invocation_path, metrics_dir.clone(), None).unwrap();
        assert_eq!(reopened.providers, providers, "{invocation:?}");
        assert_eq!(
            reopened.credentials(),
            &[expected_credential.clone()],
            "{invocation:?}"
        );
    }
}

// ── #419: adding a provider must append, never replace ───────────────────

fn chatgpt_subscription_provider() -> ProviderEntry {
    ProviderEntry::Credentialed {
        provider: crate::config::CredentialProvider::ChatgptSubscription,
        credential: crate::config::CredentialBinding {
            credential_ref: "chatgpt:default".into(),
            audience: Some(crate::config::AudienceBinding::standard(
                crate::config::EndpointFamily::ChatgptSubscription,
            )),
            tenant: None,
            project: None,
            account: None,
            required_scopes: crate::providers::chatgpt_required_scopes(),
        },
        model: Some("gpt-5.6-sol".into()),
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some("ChatGPT Personal".into()),
        reasoning_effort: Some(crate::config::ReasoningEffort::High),
    }
}

fn chatgpt_subscription_credential() -> crate::config::ProviderCredential {
    crate::config::ProviderCredential {
        name: "chatgpt:default".into(),
        kind: crate::config::CredentialKind::OauthDevice,
        provider: crate::config::CredentialProvider::ChatgptSubscription,
        issuer: "openai-chatgpt".into(),
        audience: crate::config::AudienceBinding::standard(
            crate::config::EndpointFamily::ChatgptSubscription,
        ),
        tenant: None,
        project: None,
        account: Some("account-123".into()),
        scopes: crate::providers::chatgpt_required_scopes(),
        secret_ref: "oauth-store:chatgpt:default".into(),
        lifecycle: crate::config::CredentialLifecycle::Active {
            expires_at: Some("2099-01-02T03:04:05Z".parse::<DateTime<Utc>>().unwrap()),
            refreshable: true,
        },
        revocation: Default::default(),
    }
}

fn expected_grok_provider() -> ProviderEntry {
    ProviderEntry::Grok {
        api_key: "xai-test-preserved".into(),
        model: Some("grok-code-fast-1".into()),
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some("grok".into()),
    }
}

fn provider_graph_diagnostics(
    label: &str,
    providers: &[ProviderEntry],
    credentials: &[crate::config::ProviderCredential],
) -> String {
    format!("{label}\nproviders: {providers:#?}\ncredentials: {credentials:#?}")
}

fn cloud_provider_index(provider_id: &str) -> usize {
    CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == provider_id)
        .unwrap_or_else(|| panic!("unknown cloud provider {provider_id}"))
}

/// Drive Models → Add → provider → model → key → confirm through the real reducer.
fn add_cloud_provider_through_reducer(
    state: &mut WizardState,
    provider_id: &str,
    model_id: &str,
    api_key: &str,
) {
    let target = cloud_provider_index(provider_id);
    handle_models_input(state, key(KeyCode::Char('a'))).unwrap();
    for _ in 0..target {
        handle_models_input(state, key(KeyCode::Down)).unwrap();
    }
    handle_models_input(state, key(KeyCode::Enter)).unwrap();

    // Normalize focus at the provider row, then move to model and API key.
    for _ in 0..3 {
        handle_models_input(state, key(KeyCode::Up)).unwrap();
    }
    for _ in 0..2 {
        handle_models_input(state, key(KeyCode::Down)).unwrap();
    }
    for character in model_id.chars() {
        handle_models_input(state, key(KeyCode::Char(character))).unwrap();
    }
    handle_models_input(state, key(KeyCode::Down)).unwrap();
    for character in api_key.chars() {
        handle_models_input(state, key(KeyCode::Char(character))).unwrap();
    }
    handle_models_input(state, key(KeyCode::Enter)).unwrap();
    assert!(
        get_step(state).is_none(),
        "confirming {provider_id} must close the add overlay; remaining step: {:?}",
        get_step(state)
    );
}

fn persisted_subscription_config(
    directory: &std::path::Path,
) -> (
    crate::config::Config,
    ProviderEntry,
    crate::config::ProviderCredential,
) {
    let provider = chatgpt_subscription_provider();
    let credential = chatgpt_subscription_credential();
    let metrics_dir = directory.join("metrics");
    let path = directory.join("before.toml");
    crate::config::Config::with_providers_and_paths(
        vec![provider.clone()],
        metrics_dir.clone(),
        None,
    )
    .with_credentials(vec![credential.clone()])
    .save_to(&path)
    .unwrap();
    let loaded = crate::config::load_config_from_path_with_paths(&path, metrics_dir, None)
        .unwrap_or_else(|error| {
            panic!(
                "the persisted one-provider fixture at {} must load: {error:#}",
                path.display()
            )
        });
    (loaded, provider, credential)
}

fn save_and_reload_wizard_state(
    state: &WizardState,
    directory: &std::path::Path,
    name: &str,
) -> crate::config::Config {
    let path = directory.join(name);
    let result = build_setup_result(state).expect("build setup result after provider change");
    config_from_setup_result_with_paths(&result, directory.join("metrics"), None)
        .save_to(&path)
        .unwrap_or_else(|error| {
            panic!(
                "the provider graph must save to {} after the real reducer: {error:#}\n{:#?}",
                path.display(),
                result.providers
            )
        });
    crate::config::load_config_from_path_with_paths(&path, directory.join("metrics"), None)
        .unwrap_or_else(|error| {
            panic!(
                "the provider graph written to {} must reload: {error:#}",
                path.display()
            )
        })
}

#[test]
fn test_delete_provider_through_reducer_preserves_survivors_after_save_and_reload() {
    let mut second_subscription = chatgpt_subscription_provider();
    if let ProviderEntry::Credentialed {
        credential,
        model,
        name,
        reasoning_effort,
        ..
    } = &mut second_subscription
    {
        credential.credential_ref = "chatgpt:work".into();
        *model = Some("work-model-preview".into());
        *name = Some("ChatGPT Work".into());
        *reasoning_effort = Some(crate::config::ReasoningEffort::Low);
    }
    let mut second_credential = chatgpt_subscription_credential();
    second_credential.name = "chatgpt:work".into();
    second_credential.secret_ref = "oauth-store:chatgpt:work".into();
    second_credential.account = Some("account-work".into());
    let credentials = vec![chatgpt_subscription_credential(), second_credential];
    let providers = [
        expected_grok_provider(),
        chatgpt_subscription_provider(),
        second_subscription,
    ];

    for count in [2, 3] {
        for selected in 0..count {
            for delete_key in ['d', 'D'] {
                let context = format!("{count} providers, selected {selected}, key {delete_key}");
                let directory = tempfile::tempdir().unwrap();
                let original_path = directory.path().join("original.toml");
                let metrics_dir = directory.path().join("metrics");
                crate::config::Config::with_providers_and_paths(
                    providers[..count].to_vec(),
                    metrics_dir.clone(),
                    None,
                )
                .with_credentials(credentials.clone())
                .save_to(&original_path)
                .unwrap();
                let original_bytes = std::fs::read(&original_path).unwrap();
                let loaded = crate::config::load_config_from_path_with_paths(
                    &original_path,
                    metrics_dir,
                    None,
                )
                .unwrap();
                let mut state = WizardState::new_with_catalog_cache_dir(Some(&loaded), None);
                state.current_section = WizardSection::Models;
                for _ in 0..selected {
                    handle_wizard_key(&mut state, key(KeyCode::Down)).unwrap();
                }
                let action = handle_wizard_key(&mut state, key(KeyCode::Char(delete_key))).unwrap();
                let rendered = render_wizard_text(&state);
                let result = build_setup_result(&state).unwrap();
                let mut expected = providers[..count].to_vec();
                let deleted = expected.remove(selected);
                assert_eq!(
                    result.providers, expected,
                    "delete must remove exactly the selected provider and preserve ordered metadata; {context}; action={action:?}; rendered={rendered}"
                );
                assert_eq!(
                    result.credentials, credentials,
                    "provider deletion must retain every credential record; {context}"
                );
                assert_eq!(
                    action,
                    WizardAction::Continue,
                    "delete must stay in setup until explicit save; {context}; rendered={rendered}"
                );
                assert!(
                    !rendered.contains(&deleted.profile_name()),
                    "deleted provider must disappear from the rendered list; {context}; rendered={rendered}"
                );
                for survivor in &expected {
                    assert!(
                        rendered.contains(&survivor.profile_name()),
                        "surviving provider must remain visible; {context}; survivor={survivor:?}; rendered={rendered}"
                    );
                }
                assert!(
                    rendered.contains(&format!("Primary: {}", expected[0].profile_name())),
                    "first surviving provider must render as primary; {context}; rendered={rendered}"
                );
                let expected_selection = selected.min(expected.len() - 1);
                assert!(
                    matches!(state.sections.get(&WizardSection::Models), Some(SectionState::Models { selected_idx, error: None, .. }) if *selected_idx == expected_selection),
                    "selection must remain on the next row or clamp to the last survivor without an error; {context}; section={:?}",
                    state.sections.get(&WizardSection::Models)
                );
                assert_eq!(
                    std::fs::read(&original_path).unwrap(),
                    original_bytes,
                    "delete must not persist before explicit save; {context}"
                );
                assert_eq!(
                    handle_wizard_key(
                        &mut state,
                        modified_key(KeyCode::Char('s'), KeyModifiers::CONTROL)
                    )
                    .unwrap(),
                    WizardAction::Save,
                    "Ctrl+S must accept the provider deletion; {context}"
                );
                let reopened = save_and_reload_wizard_state(&state, directory.path(), "after.toml");
                assert_eq!(
                    reopened.providers, expected,
                    "saved deletion must preserve exact survivor ordering and metadata after reload; {context}"
                );
                assert_eq!(
                    reopened.credentials(), credentials.as_slice(),
                    "saved deletion must leave credential records unchanged after reload; {context}"
                );
                let reopened_state = WizardState::new_with_catalog_cache_dir(Some(&reopened), None);
                assert_eq!(
                    build_setup_result(&reopened_state).unwrap().providers,
                    expected,
                    "reopening setup must not resurrect the deleted provider; {context}"
                );
            }
        }
    }
}

#[test]
fn test_delete_provider_refuses_last_provider_with_visible_recovery_after_reload() {
    let directory = tempfile::tempdir().unwrap();
    let (config, provider, credential) = persisted_subscription_config(directory.path());
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&config), None);
    state.current_section = WizardSection::Models;
    for delete_key in ['d', 'D', 'd'] {
        let action = handle_wizard_key(&mut state, key(KeyCode::Char(delete_key))).unwrap();
        let rendered = render_wizard_text(&state);
        assert_eq!(
            action, WizardAction::Continue,
            "refusing the final deletion must keep setup open; key={delete_key}; rendered={rendered}"
        );
        assert!(
            rendered.contains("Cannot delete the last provider. Press A to add another provider first."),
            "last-provider refusal must explain the recovery in rendered text; key={delete_key}; rendered={rendered}"
        );
        assert!(
            matches!(state.sections.get(&WizardSection::Models), Some(SectionState::Models { selected_idx: 0, tool_models, .. }) if tool_models.is_empty()),
            "refusal must retain the only provider and valid selection; key={delete_key}; section={:?}",
            state.sections.get(&WizardSection::Models)
        );
        let reloaded = save_and_reload_wizard_state(&state, directory.path(), "refused.toml");
        assert_eq!(
            reloaded.providers, vec![provider.clone()],
            "repeated refused deletion must preserve exact provider metadata through save/reload; key={delete_key}"
        );
        assert_eq!(
            reloaded.credentials(), &[credential.clone()],
            "repeated refused deletion must retain credentials through save/reload; key={delete_key}"
        );
        state = WizardState::new_with_catalog_cache_dir(Some(&reloaded), None);
        state.current_section = WizardSection::Models;
    }
}

#[test]
fn test_remote_add_preserves_persisted_subscription_through_save_and_reload() {
    let directory = tempfile::tempdir().unwrap();
    let (loaded, subscription, credential) = persisted_subscription_config(directory.path());
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&loaded), None);
    state.current_section = WizardSection::Models;

    add_cloud_provider_through_reducer(
        &mut state,
        "grok",
        "grok-code-fast-1",
        "xai-test-preserved",
    );
    let reloaded = save_and_reload_wizard_state(&state, directory.path(), "after-grok.toml");
    let diagnostics = provider_graph_diagnostics(
        "remote add beside a persisted ChatGPT subscription",
        &reloaded.providers,
        reloaded.credentials(),
    );

    assert_eq!(
        reloaded.providers,
        vec![subscription, expected_grok_provider()],
        "adding Grok must append without changing the subscription's order, model, name, \
             reasoning effort, or credential binding. {diagnostics}"
    );
    assert_eq!(
        reloaded.credentials(),
        &[credential],
        "adding Grok must preserve the subscription credential metadata. {diagnostics}"
    );
}

#[test]
fn test_local_add_uses_the_same_append_decision() {
    let directory = tempfile::tempdir().unwrap();
    let (loaded, subscription, credential) = persisted_subscription_config(directory.path());
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&loaded), None);
    state.current_section = WizardSection::Models;

    handle_models_input(&mut state, key(KeyCode::Char('a'))).unwrap();
    for _ in 0..CLOUD_PROVIDERS.len() {
        handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
    }
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    for _ in 0..4 {
        handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
    }
    for character in test_gguf_path().chars() {
        handle_models_input(&mut state, key(KeyCode::Char(character))).unwrap();
    }
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

    let reloaded = save_and_reload_wizard_state(&state, directory.path(), "after-local.toml");
    let diagnostics = provider_graph_diagnostics(
        "local add beside a persisted ChatGPT subscription",
        &reloaded.providers,
        reloaded.credentials(),
    );
    assert_eq!(
        reloaded.providers.first(),
        Some(&subscription),
        "the local-add path must not replace the configured subscription. {diagnostics}"
    );
    assert!(
        matches!(reloaded.providers.get(1), Some(ProviderEntry::Local { .. })),
        "the local model must be appended at index 1. {diagnostics}"
    );
    assert_eq!(
        reloaded.providers.len(),
        2,
        "the local-add path must append exactly one provider. {diagnostics}"
    );
    assert_eq!(
        reloaded.credentials(),
        &[credential],
        "the local-add path must preserve credential metadata. {diagnostics}"
    );
}

fn unsaved_remote(provider: &str, model: &str) -> ModelConfig {
    ModelConfig::Remote {
        provider: provider.into(),
        name: provider.into(),
        api_key: String::new(),
        model: model.into(),
        enabled: true,
        persisted: None,
    }
}

#[test]
fn test_placeholder_classification_preserves_keyless_providers() {
    let persisted_chatgpt = model_config_from_provider(&chatgpt_subscription_provider()).unwrap();
    let persisted_finch = model_config_from_provider(&ProviderEntry::RemoteDaemon {
        address: "127.0.0.1:11435".into(),
        name: Some("Finch daemon".into()),
    })
    .unwrap();
    let persisted_ollama = model_config_from_provider(&ProviderEntry::Ollama {
        model: "qwen2.5:7b".into(),
        base_url: "http://127.0.0.1:11434".into(),
        name: Some("Ollama".into()),
    })
    .unwrap();

    let cases = [
        ("persisted ChatGPT", persisted_chatgpt, false),
        (
            "unsaved ChatGPT",
            unsaved_remote("chatgpt", "gpt-5.6-sol"),
            false,
        ),
        ("persisted Finch daemon", persisted_finch, false),
        (
            "unsaved Finch daemon",
            unsaved_remote("finch", "127.0.0.1:11435"),
            false,
        ),
        ("persisted Ollama", persisted_ollama, false),
        (
            "unsaved Ollama",
            unsaved_remote("ollama", "qwen2.5:7b"),
            false,
        ),
        (
            "blank key-required Claude placeholder",
            unsaved_remote("claude", ""),
            true,
        ),
    ];

    for (label, model, expected_placeholder) in cases {
        assert_eq!(
            is_unconfigured_placeholder(&model),
            expected_placeholder,
            "{label} classification controls whether the next add preserves or replaces the \
                 primary provider. Model was: {model:#?}"
        );
    }
}

// ── #418: the dialog must say which operation it is performing ───────────

#[test]
fn test_provider_dialog_is_titled_add_when_adding_and_edit_when_editing() {
    let mut state = state_with_step(AddProviderStep::SelectAddType { selected: 0 });
    state.current_section = WizardSection::Models;
    let adding = render_wizard_text(&state);
    assert!(
        adding.contains("Add AI Provider")
            && !adding.contains("Edit AI Provider")
            && !adding.contains("Add AI provider"),
        "the add overlay must use consistent add-specific wording. Frame was:\n{adding}"
    );

    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: cloud_provider_index("grok"),
        name: "Grok Build".into(),
        model: "grok-code-fast-1".into(),
        api_key: Some("xai-test-preserved".into()),
        focused_field: 1,
        editing_idx: Some(0),
    });
    state.current_section = WizardSection::Models;
    let editing = render_wizard_text(&state);
    assert!(
        editing.contains("Edit AI Provider") && !editing.contains("Add AI Provider"),
        "the edit overlay must not tell the user they are adding a provider. Frame was:\n{editing}"
    );
}

#[test]
fn test_provider_editor_identity_table_matches_catalog() {
    let cases = [
        (crate::config::CredentialProvider::Anthropic, "claude"),
        (crate::config::CredentialProvider::OpenaiPlatform, "openai"),
        (
            crate::config::CredentialProvider::ChatgptSubscription,
            "chatgpt",
        ),
        (
            crate::config::CredentialProvider::GrokSubscription,
            "grok-sub",
        ),
        (crate::config::CredentialProvider::Xai, "grok"),
        (crate::config::CredentialProvider::GeminiAiStudio, "gemini"),
        (crate::config::CredentialProvider::Mistral, "mistral"),
        (crate::config::CredentialProvider::Groq, "groq"),
        (crate::config::CredentialProvider::Openrouter, "openrouter"),
    ];

    assert!(
        !provider_requires_inline_api_key("grok-sub") && provider_requires_inline_api_key("grok"),
        "SuperGrok subscription and xAI Console API-key auth must remain separate wizard choices"
    );
    let sub = CLOUD_PROVIDERS
        .iter()
        .find(|(id, _, _, _)| *id == "grok-sub")
        .expect("grok-sub wizard choice");
    let api = CLOUD_PROVIDERS
        .iter()
        .find(|(id, _, _, _)| *id == "grok")
        .expect("grok API-key wizard choice");
    assert!(
        sub.1.contains("subscription")
            && sub.3.contains("not an xAI API key")
            && api.1.contains("API")
            && api.3.contains("billed separately"),
        "wizard copy must name SuperGrok entitlement versus Console billing: sub={sub:?} api={api:?}"
    );

    for (credential_provider, expected_editor) in cases {
        let provider = ProviderEntry::Credentialed {
            provider: credential_provider,
            credential: crate::config::CredentialBinding {
                credential_ref: format!("{expected_editor}:work"),
                audience: None,
                tenant: None,
                project: None,
                account: None,
                required_scopes: std::collections::BTreeSet::new(),
            },
            model: Some("model-work".into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some(format!("{expected_editor} work")),
            reasoning_effort: None,
        };
        assert_eq!(
            registered_editor_id(&provider),
            Some(expected_editor),
            "persisted {credential_provider:?} must select its registered editor"
        );
        assert!(
            CLOUD_PROVIDERS
                .iter()
                .any(|(editor_id, ..)| *editor_id == expected_editor),
            "mapped editor {expected_editor} must exist in CLOUD_PROVIDERS"
        );
        assert!(
            matches!(model_config_from_provider(&provider), Some(ModelConfig::Remote { provider, .. }) if provider == expected_editor),
            "the rendered model row and edit path must use the same identity mapping for {credential_provider:?}"
        );
    }

    let mapped: std::collections::BTreeSet<_> =
        cases.into_iter().map(|(_, editor)| editor).collect();
    let registered: std::collections::BTreeSet<_> =
        CLOUD_PROVIDERS.iter().map(|(editor, ..)| *editor).collect();
    assert_eq!(
        mapped, registered,
        "every registered cloud editor must have exactly one credentialed-provider identity mapping"
    );
}

#[tokio::test]
async fn test_provider_editor_preserves_chatgpt_named_credential_through_save_and_reload() {
    let directory = tempfile::tempdir().unwrap();
    let mut subscription = chatgpt_subscription_provider();
    let mut credential = chatgpt_subscription_credential();
    if let ProviderEntry::Credentialed {
        credential: binding,
        name,
        ..
    } = &mut subscription
    {
        binding.credential_ref = "chatgpt:work".into();
        *name = Some("ChatGPT Work".into());
    }
    credential.name = "chatgpt:work".into();
    credential.secret_ref = "oauth-store:chatgpt:work".into();
    credential.account = Some("account-work".into());
    let claude = ProviderEntry::Claude {
        api_key: "sk-ant-test-preserved".into(),
        model: Some("claude-sonnet-4-5".into()),
        base_url: Some("https://api.anthropic.test".into()),
        chat_path: None,
        models_path: None,
        name: Some("Claude Review".into()),
    };
    let original_providers = vec![
        expected_grok_provider(),
        subscription.clone(),
        claude.clone(),
    ];

    let original_path = directory.path().join("original.toml");
    let metrics_dir = directory.path().join("metrics");
    crate::config::Config::with_providers_and_paths(
        original_providers.clone(),
        metrics_dir.clone(),
        None,
    )
    .with_credentials(vec![credential.clone()])
    .save_to(&original_path)
    .unwrap();
    let original =
        crate::config::load_config_from_path_with_paths(&original_path, metrics_dir.clone(), None)
            .unwrap();
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&original), None);
    state.current_section = WizardSection::Models;
    handle_models_input(&mut state, key(KeyCode::Down)).unwrap();

    handle_models_input(&mut state, key(KeyCode::Char('e'))).unwrap();
    assert!(
        matches!(get_step(&state), Some(AddProviderStep::ConfigureRemote { provider_idx, editing_idx: Some(1), .. }) if CLOUD_PROVIDERS[*provider_idx].0 == "chatgpt"),
        "the stored ChatGPT subscription identity must open the ChatGPT editor; step={:?}",
        get_step(&state)
    );
    handle_models_input(&mut state, key(KeyCode::Char('!'))).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

    let result = build_setup_result(&state).unwrap();
    let diagnostics = provider_graph_diagnostics(
        "edited ChatGPT subscription",
        &result.providers,
        &result.credentials,
    );
    let mut expected = subscription;
    if let ProviderEntry::Credentialed { name, .. } = &mut expected {
        *name = Some("ChatGPT Work!".into());
    }
    let mut expected_providers = original_providers;
    expected_providers[1] = expected.clone();
    assert_eq!(
        result.providers,
        expected_providers,
        "editing only the profile name must preserve the provider namespace, chatgpt:work binding, model, endpoint and reasoning effort. {diagnostics}"
    );
    assert_eq!(
        result.credentials,
        vec![credential.clone()],
        "editing must preserve the exact credential record. {diagnostics}"
    );

    let authenticator = FakeChatGptSetupAuthenticator::default();
    let mut editor = ScriptedRecoveryEditor::new([]);
    let (saved, committed, compensations) = run_chatgpt_setup_recovery_loop(
        SetupInvocation::Command,
        &result,
        &authenticator,
        &mut editor,
    )
    .await
    .unwrap()
    .expect("the preserved credential graph must require no recovery");
    assert!(
        authenticator.calls.lock().unwrap().is_empty(),
        "editing a profile with a usable chatgpt:work credential must not start device authorization"
    );
    assert!(editor.recoveries.is_empty());
    assert!(compensations.is_empty());
    assert_eq!(committed.providers, result.providers);

    let saved_path = directory.path().join("edited.toml");
    save_chatgpt_setup_config(&saved, &compensations, &authenticator, |config| {
        config.save_to(&saved_path)
    })
    .unwrap();
    let reloaded =
        crate::config::load_config_from_path_with_paths(&saved_path, metrics_dir, None).unwrap();
    assert_eq!(
        reloaded.providers, expected_providers,
        "the edited provider graph must survive save/reload without defaulting its identity"
    );
    assert_eq!(reloaded.credentials(), &[credential]);
}

#[test]
fn test_provider_editor_refuses_unsupported_rows_without_mutation() {
    let unsupported = [
        ProviderEntry::Ollama {
            model: "qwen2.5:7b".into(),
            base_url: "http://127.0.0.1:11434".into(),
            name: Some("Local Ollama".into()),
        },
        ProviderEntry::RemoteDaemon {
            address: "127.0.0.1:11435".into(),
            name: Some("Remote Finch".into()),
        },
        ProviderEntry::Credentialed {
            provider: crate::config::CredentialProvider::GoogleVertex,
            credential: crate::config::CredentialBinding {
                credential_ref: "vertex:work".into(),
                audience: None,
                tenant: None,
                project: Some("project-work".into()),
                account: None,
                required_scopes: std::collections::BTreeSet::new(),
            },
            model: Some("gemini-2.5-pro".into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("Vertex Work".into()),
            reasoning_effort: None,
        },
    ];

    for provider in unsupported {
        let label = provider.display_name().to_string();
        let config = crate::config::Config::with_providers(vec![provider.clone()]);
        let mut state = WizardState::new_with_catalog_cache_dir(Some(&config), None);
        state.current_section = WizardSection::Models;
        let before = build_setup_result(&state).unwrap();

        handle_models_input(&mut state, key(KeyCode::Char('e'))).unwrap();

        let after = build_setup_result(&state).unwrap();
        let rendered = render_wizard_text(&state);
        assert!(
            get_step(&state).is_none(),
            "unsupported {label} must not fall back to the first registered editor; step={:?}",
            get_step(&state)
        );
        assert_eq!(
            after.providers, before.providers,
            "refusing the unsupported {label} editor must not mutate providers; rendered={rendered}"
        );
        assert_eq!(
            after.credentials, before.credentials,
            "refusing the unsupported {label} editor must not mutate credentials; rendered={rendered}"
        );
        assert!(
            rendered.contains("is not available in setup") && rendered.contains(&label),
            "the refusal must name {label} and remain visible; rendered={rendered}"
        );
    }
}

#[test]
fn test_genuinely_empty_setup_alone_renders_unconfigured_claude_default() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Models;
    let rendered = render_wizard_text_at(&state, 100, 24);
    assert!(rendered.contains("claude"), "{rendered}");
    assert!(rendered.contains("[Not configured]"), "{rendered}");
}

#[derive(Default)]
struct FakeChatGptSetupAuthenticator {
    calls: std::sync::Mutex<Vec<String>>,
}

#[derive(Debug, Clone, Copy)]
enum ScriptedChatGptOutcome {
    Cancelled,
    Expired,
    Denied,
    StartDisabledOrUnsupported,
    ProviderRejected(u16),
    Success,
}

struct ScriptedRecoveryAuthenticator {
    outcomes: std::sync::Mutex<std::collections::VecDeque<ScriptedChatGptOutcome>>,
    calls: std::sync::Mutex<Vec<(String, String)>>,
    active: std::sync::atomic::AtomicUsize,
}

#[derive(Clone)]
struct RealRetryVerifier;

#[async_trait::async_trait]
impl crate::providers::OpenAiTokenVerifier for RealRetryVerifier {
    fn preflight(&self) -> Result<()> {
        Ok(())
    }

    async fn verify(
        &self,
        _id_token: Option<&str>,
        _access_token: &str,
        _cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<crate::providers::VerifiedOpenAiClaims> {
        Ok(crate::providers::VerifiedOpenAiClaims {
            issuer: crate::providers::REQUIRED_TOKEN_ISSUER.into(),
            audiences: std::collections::BTreeSet::from([
                crate::providers::OPENAI_PUBLIC_CLIENT_ID.into(),
            ]),
            authorized_party: None,
            subject: "subject-work".into(),
            account_id: Some("acct-work".into()),
            chatgpt_plan_type: Some("plus".into()),
            account_is_fedramp: false,
            nonce: None,
            expires_at: Utc::now() + chrono::TimeDelta::hours(1),
            not_before: None,
        })
    }
}

#[derive(Default)]
struct RealRetryHttpState {
    starts: std::sync::Mutex<Vec<String>>,
    first_poll_finished: std::sync::atomic::AtomicBool,
    second_start_after_first_poll: std::sync::atomic::AtomicBool,
    first_polls: std::sync::atomic::AtomicUsize,
    second_polls: std::sync::atomic::AtomicUsize,
}

struct RealRetryHttpServer {
    origin: String,
    state: std::sync::Arc<RealRetryHttpState>,
    task: tokio::task::JoinHandle<()>,
}

impl RealRetryHttpServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let state = std::sync::Arc::new(RealRetryHttpState::default());
        let app = axum::Router::new()
            .fallback(axum::routing::post(real_retry_http_handler))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            origin,
            state,
            task,
        }
    }
}

impl Drop for RealRetryHttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn real_retry_http_handler(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<RealRetryHttpState>>,
    request: axum::extract::Request,
) -> axum::http::Response<axum::body::Body> {
    use base64::Engine;
    use sha2::Digest;
    let path = request.uri().path().to_string();
    let body = axum::body::to_bytes(request.into_body(), 64 * 1024)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let (status, response) = match path.as_str() {
        "/api/accounts/deviceauth/usercode" => {
            let mut starts = state.starts.lock().unwrap();
            let attempt = starts.len() + 1;
            starts.push(format!("device-{attempt}|CODE-{attempt}"));
            if attempt == 2 {
                state.second_start_after_first_poll.store(
                    state
                        .first_poll_finished
                        .load(std::sync::atomic::Ordering::SeqCst),
                    std::sync::atomic::Ordering::SeqCst,
                );
            }
            (
                axum::http::StatusCode::OK,
                serde_json::json!({
                    "device_auth_id": format!("device-{attempt}"),
                    "user_code": format!("CODE-{attempt}"),
                    "interval": "0"
                }),
            )
        }
        "/api/accounts/deviceauth/token" => {
            match json
                .get("device_auth_id")
                .and_then(serde_json::Value::as_str)
            {
                Some("device-1") => {
                    state
                        .first_polls
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    state
                        .first_poll_finished
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        serde_json::json!({"ignored": "first-body-secret"}),
                    )
                }
                Some("device-2") => {
                    state
                        .second_polls
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let verifier = "second-pkce-verifier";
                    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .encode(sha2::Sha256::digest(verifier.as_bytes()));
                    (
                        axum::http::StatusCode::OK,
                        serde_json::json!({
                            "authorization_code": "second-authorization-code",
                            "code_verifier": verifier,
                            "code_challenge": challenge
                        }),
                    )
                }
                _ => (
                    axum::http::StatusCode::BAD_REQUEST,
                    serde_json::json!({"error": "unknown-device"}),
                ),
            }
        }
        "/oauth/token" => (
            axum::http::StatusCode::OK,
            serde_json::json!({
                "access_token": "successful-access-secret",
                "refresh_token": "successful-refresh-secret",
                "id_token": "successful-id-secret",
                "expires_in": 3600
            }),
        ),
        _ => (
            axum::http::StatusCode::NOT_FOUND,
            serde_json::json!({"error": "unexpected-path"}),
        ),
    };
    axum::http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(response.to_string()))
        .unwrap()
}

struct RealRetryAuthenticator {
    client: crate::oauth::OAuthClient<
        crate::providers::OpenAiChatGptOAuthDialect<RealRetryVerifier>,
        crate::oauth::FileOAuthCredentialStore,
    >,
}

#[async_trait::async_trait]
impl crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator for RealRetryAuthenticator {
    async fn ensure_named_credential(
        &self,
        reference: &str,
        presentation: crate::cli::chatgpt_auth::DeviceLoginPresentation,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::cli::chatgpt_auth::EnsuredChatGptCredential> {
        let credential = crate::cli::chatgpt_auth::login_device_with(
            &self.client,
            reference,
            presentation,
            cancel,
        )
        .await?;
        Ok(crate::cli::chatgpt_auth::EnsuredChatGptCredential {
            credential,
            compensation: None,
        })
    }
}

impl ScriptedRecoveryAuthenticator {
    fn new(outcomes: impl IntoIterator<Item = ScriptedChatGptOutcome>) -> Self {
        Self {
            outcomes: std::sync::Mutex::new(outcomes.into_iter().collect()),
            calls: std::sync::Mutex::new(Vec::new()),
            active: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

struct ActiveCeremony<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for ActiveCeremony<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator for ScriptedRecoveryAuthenticator {
    async fn ensure_named_credential(
        &self,
        reference: &str,
        _presentation: crate::cli::chatgpt_auth::DeviceLoginPresentation,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::cli::chatgpt_auth::EnsuredChatGptCredential> {
        self.active
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _active = ActiveCeremony(&self.active);
        {
            let mut calls = self.calls.lock().unwrap();
            let identity = format!("authorization-{}", calls.len() + 1);
            calls.push((reference.to_string(), identity));
        }
        tokio::task::yield_now().await;
        match self.outcomes.lock().unwrap().pop_front().unwrap() {
            ScriptedChatGptOutcome::Cancelled => {
                Err(crate::oauth::OAuthDeviceAuthorizationError::Cancelled.into())
            }
            ScriptedChatGptOutcome::Expired => {
                Err(crate::oauth::OAuthDeviceAuthorizationError::Expired.into())
            }
            ScriptedChatGptOutcome::Denied => {
                Err(crate::oauth::OAuthDeviceAuthorizationError::Denied.into())
            }
            ScriptedChatGptOutcome::StartDisabledOrUnsupported => {
                Err(crate::providers::ChatGptDeviceEndpointError::StartDisabledOrUnsupported.into())
            }
            ScriptedChatGptOutcome::ProviderRejected(status) => {
                Err(crate::providers::ChatGptDeviceEndpointError::StartRejected(status).into())
            }
            ScriptedChatGptOutcome::Success => {
                Ok(crate::cli::chatgpt_auth::EnsuredChatGptCredential {
                    credential: chatgpt_setup_credential(reference, "acct-isolated"),
                    compensation: None,
                })
            }
        }
    }
}

struct ScriptedRecoveryEditor {
    actions: std::collections::VecDeque<ChatGptSetupRecoveryAction>,
    recoveries: Vec<ChatGptSetupRecovery>,
}

impl ScriptedRecoveryEditor {
    fn new(actions: impl IntoIterator<Item = ChatGptSetupRecoveryAction>) -> Self {
        Self {
            actions: actions.into_iter().collect(),
            recoveries: Vec::new(),
        }
    }
}

impl ChatGptSetupRecoveryEditor for ScriptedRecoveryEditor {
    fn choose(&mut self, recovery: &ChatGptSetupRecovery) -> Result<ChatGptSetupRecoveryAction> {
        self.recoveries.push(recovery.clone());
        self.actions
            .pop_front()
            .context("scripted recovery action missing")
    }
}

#[async_trait::async_trait]
impl crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator for FakeChatGptSetupAuthenticator {
    async fn ensure_named_credential(
        &self,
        reference: &str,
        _presentation: crate::cli::chatgpt_auth::DeviceLoginPresentation,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::cli::chatgpt_auth::EnsuredChatGptCredential> {
        self.calls.lock().unwrap().push(reference.to_string());
        if cancel.is_cancelled() {
            return Err(crate::oauth::OAuthDeviceAuthorizationError::Cancelled.into());
        }
        Ok(crate::cli::chatgpt_auth::EnsuredChatGptCredential {
            credential: chatgpt_setup_credential(
                reference,
                &format!("acct-{}", reference.rsplit(':').next().unwrap()),
            ),
            compensation: None,
        })
    }
}

#[cfg(unix)]
struct DurableSetupAuthenticator {
    root: std::path::PathBuf,
    fail_reference: Option<String>,
    invalid_final_reference: Option<String>,
    replace_before_compensation: Option<String>,
}

#[cfg(unix)]
impl DurableSetupAuthenticator {
    fn store(&self) -> crate::oauth::FileOAuthCredentialStore {
        crate::oauth::FileOAuthCredentialStore::new(self.root.clone())
    }

    fn token_record(reference: &str) -> crate::oauth::OAuthTokenRecord {
        crate::oauth::OAuthTokenRecord {
            dialect_id: "openai_chatgpt_subscription".into(),
            protocol_revision: crate::providers::CHATGPT_OAUTH_PROTOCOL_REVISION.into(),
            provider: crate::config::CredentialProvider::ChatgptSubscription,
            kind: crate::config::CredentialKind::OauthDevice,
            issuer: "openai-chatgpt".into(),
            audience: crate::config::AudienceBinding::standard(
                crate::config::EndpointFamily::ChatgptSubscription,
            ),
            client_id: crate::providers::OPENAI_PUBLIC_CLIENT_ID.into(),
            account: format!("acct-{}", reference.rsplit(':').next().unwrap()),
            tenant: None,
            project: None,
            scopes: crate::providers::chatgpt_required_scopes(),
            access_token: format!("secret-access-{reference}"),
            refresh_token: Some(format!("secret-refresh-{reference}")),
            id_token: Some(format!("secret-identity-{reference}")),
            expires_at: Utc::now() + chrono::TimeDelta::hours(1),
            generation: uuid::Uuid::new_v4().to_string(),
            revoked: false,
            mutation_pending: false,
        }
    }
}

#[cfg(unix)]
#[async_trait::async_trait]
impl crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator for DurableSetupAuthenticator {
    fn compensate_with_tombstone(
        &self,
        handle: &crate::cli::chatgpt_auth::ChatGptCompensationHandle,
    ) -> Result<()> {
        use crate::oauth::OAuthCredentialStore;
        let store = self.store();
        if self.replace_before_compensation.as_deref() == Some(handle.reference()) {
            let current = store
                .load(handle.reference())?
                .context("missing staged credential")?;
            let mut external = current.clone();
            external.generation = uuid::Uuid::new_v4().to_string();
            external.access_token = "external-replacement-sentinel".into();
            store.compare_and_swap(handle.reference(), Some(&current.generation), &external)?;
        }
        let current = store
            .load(handle.reference())?
            .context("missing staged credential")?;
        if current.generation != handle.generation() {
            anyhow::bail!("compensation generation changed; current record left untouched");
        }
        let mut tombstone = current.clone();
        tombstone.access_token.clear();
        tombstone.refresh_token = None;
        tombstone.id_token = None;
        tombstone.generation = uuid::Uuid::new_v4().to_string();
        tombstone.revoked = true;
        tombstone.mutation_pending = false;
        store.compare_and_swap(handle.reference(), Some(handle.generation()), &tombstone)
    }

    async fn ensure_named_credential(
        &self,
        reference: &str,
        _presentation: crate::cli::chatgpt_auth::DeviceLoginPresentation,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::cli::chatgpt_auth::EnsuredChatGptCredential> {
        use crate::oauth::OAuthCredentialStore;
        if self.fail_reference.as_deref() == Some(reference) {
            return Err(crate::oauth::OAuthDeviceAuthorizationError::Denied.into());
        }
        let store = self.store();
        let existing = store.load(reference)?;
        let expected = existing.as_ref().map(|record| record.generation.as_str());
        let replacement = Self::token_record(reference);
        let generation = replacement.generation.clone();
        store.compare_and_swap(reference, expected, &replacement)?;
        let mut credential = replacement.provider_credential(reference);
        if self.invalid_final_reference.as_deref() == Some(reference) {
            credential.issuer = "wrong-issuer".into();
        }
        Ok(crate::cli::chatgpt_auth::EnsuredChatGptCredential {
            credential,
            compensation: Some(crate::cli::chatgpt_auth::ChatGptCompensationHandle::issued(
                reference, generation,
            )),
        })
    }
}

fn chatgpt_setup_credential(reference: &str, account: &str) -> crate::config::ProviderCredential {
    crate::config::ProviderCredential {
        name: reference.into(),
        kind: crate::config::CredentialKind::OauthDevice,
        provider: crate::config::CredentialProvider::ChatgptSubscription,
        issuer: "openai-chatgpt".into(),
        audience: crate::config::AudienceBinding::standard(
            crate::config::EndpointFamily::ChatgptSubscription,
        ),
        tenant: None,
        project: None,
        account: Some(account.into()),
        scopes: crate::providers::chatgpt_required_scopes(),
        secret_ref: format!("oauth-store:{reference}"),
        lifecycle: crate::config::CredentialLifecycle::Active {
            expires_at: Some(Utc::now() + chrono::TimeDelta::hours(1)),
            refreshable: true,
        },
        revocation: Default::default(),
    }
}

fn chatgpt_setup_profile(reference: &str, name: &str, model: &str) -> ProviderEntry {
    ProviderEntry::Credentialed {
        provider: crate::config::CredentialProvider::ChatgptSubscription,
        credential: crate::config::CredentialBinding {
            credential_ref: reference.into(),
            audience: Some(crate::config::AudienceBinding::standard(
                crate::config::EndpointFamily::ChatgptSubscription,
            )),
            tenant: None,
            project: None,
            account: None,
            required_scopes: crate::providers::chatgpt_required_scopes(),
        },
        model: Some(model.into()),
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some(name.into()),
        reasoning_effort: None,
    }
}

fn setup_result_with_profiles(profiles: Vec<ProviderEntry>) -> SetupResult {
    build_setup_result(&WizardState::new(Some(
        &crate::config::Config::with_providers(profiles),
    )))
    .unwrap()
}

#[test]
fn setup_recovery_editor_reprompts_invalid_choice_and_name_without_reflection() {
    let recovery = ChatGptSetupRecovery {
        invocation: SetupInvocation::Command,
        credential_ref: "chatgpt:work".into(),
        cause: ChatGptSetupFailureCause::ProviderRejected,
        summary: chatgpt_setup_failure_summary(ChatGptSetupFailureCause::ProviderRejected),
    };
    let hostile_choice = "invalid-choice-secret";
    let hostile_name = "../invalid-name-secret";
    let script = format!("{hostile_choice}\n2\n{hostile_name}\n2\nchatgpt:replacement\n");
    let mut input = std::io::Cursor::new(script.into_bytes());
    let mut output = Vec::new();
    let action = choose_chatgpt_setup_recovery_with_io(&recovery, &mut input, &mut output).unwrap();
    assert_eq!(
        action,
        ChatGptSetupRecoveryAction::ChangeNamedCredential("chatgpt:replacement".into())
    );
    let rendered = String::from_utf8(output).unwrap();
    assert!(rendered.contains("Invalid selection"), "{rendered}");
    assert!(
        rendered.contains("Named credential is invalid"),
        "{rendered}"
    );
    assert!(!rendered.contains(hostile_choice), "{rendered}");
    assert!(!rendered.contains(hostile_name), "{rendered}");

    let mut eof = std::io::Cursor::new(Vec::<u8>::new());
    let mut eof_output = Vec::new();
    assert_eq!(
        choose_chatgpt_setup_recovery_with_io(&recovery, &mut eof, &mut eof_output).unwrap(),
        ChatGptSetupRecoveryAction::CancelSetup
    );
}

#[test]
fn setup_post_browser_failures_are_stage_specific_and_secret_free() {
    use crate::providers::ChatGptAuthStageError;

    fn wrapped_stage(stage: ChatGptAuthStageError) -> anyhow::Error {
        Err::<(), _>(anyhow::anyhow!("redacted upstream failure"))
            .context(stage)
            .unwrap_err()
    }

    for (error, expected, phrase) in [
        (
            wrapped_stage(ChatGptAuthStageError::PollContract),
            ChatGptSetupFailureCause::PollContract,
            "completed device response",
        ),
        (
            wrapped_stage(ChatGptAuthStageError::TokenExchangeRejected(401)),
            ChatGptSetupFailureCause::TokenExchangeRejected,
            "authorization-code exchange",
        ),
        (
            wrapped_stage(ChatGptAuthStageError::TokenExchangeContract),
            ChatGptSetupFailureCause::TokenExchangeContract,
            "token response",
        ),
        (
            wrapped_stage(ChatGptAuthStageError::IdentityVerification),
            ChatGptSetupFailureCause::IdentityVerification,
            "signed identity",
        ),
        (
            wrapped_stage(ChatGptAuthStageError::ClientBinding),
            ChatGptSetupFailureCause::ClientBinding,
            "public client",
        ),
        (
            wrapped_stage(ChatGptAuthStageError::AccountEntitlement),
            ChatGptSetupFailureCause::AccountEntitlement,
            "account identifier",
        ),
        (
            Err::<(), _>(anyhow::anyhow!("redacted persistence failure"))
                .context(crate::oauth::OAuthCredentialPersistenceError::Commit)
                .unwrap_err(),
            ChatGptSetupFailureCause::Persistence,
            "could not save",
        ),
    ] {
        let cause = chatgpt_setup_failure_cause(&error);
        assert_eq!(cause, expected);
        let summary = chatgpt_setup_failure_summary(cause);
        assert!(summary.contains(phrase), "{summary}");
        for secret in [
            "access-token-sentinel",
            "refresh-token-sentinel",
            "code-sentinel",
        ] {
            assert!(!summary.contains(secret), "{summary}");
        }
    }
}

#[tokio::test]
async fn setup_all_invocations_recover_from_disabled_start_with_fresh_authorization_identity() {
    for invocation in [
        SetupInvocation::FirstRun,
        SetupInvocation::Command,
        SetupInvocation::Repl,
    ] {
        let result = setup_result_with_profiles(vec![chatgpt_setup_profile(
            "chatgpt:work",
            "work",
            "gpt-5.6-sol",
        )]);
        let authenticator = ScriptedRecoveryAuthenticator::new([
            ScriptedChatGptOutcome::StartDisabledOrUnsupported,
            ScriptedChatGptOutcome::Success,
        ]);
        let mut editor = ScriptedRecoveryEditor::new([ChatGptSetupRecoveryAction::RetrySignIn]);
        let (config, committed, _) =
            run_chatgpt_setup_recovery_loop(invocation, &result, &authenticator, &mut editor)
                .await
                .unwrap()
                .unwrap();

        let calls = authenticator.calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "{invocation:?}");
        assert_eq!(calls[0].0, "chatgpt:work");
        assert_eq!(calls[1].0, "chatgpt:work");
        assert_ne!(calls[0].1, calls[1].1, "{invocation:?}");
        assert_eq!(editor.recoveries.len(), 1);
        assert_eq!(editor.recoveries[0].invocation, invocation);
        assert_eq!(
            editor.recoveries[0].cause,
            ChatGptSetupFailureCause::StartDisabledOrUnsupported
        );
        let rendered = format!(
            "{}\n{}",
            editor.recoveries[0].credential_ref, editor.recoveries[0].summary
        );
        for secret in ["one-time-code-sentinel", "token-sentinel", "upstream-body"] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
        assert_eq!(chatgpt_setup_references(&committed).len(), 1);
        assert_eq!(config.credentials().len(), 1);
        config.validate().unwrap();
        assert_eq!(
            authenticator
                .active
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn setup_retry_crosses_real_oauth_boundary_with_fresh_quiescent_ceremony() {
    use crate::oauth::OAuthCredentialStore;
    let server = RealRetryHttpServer::start().await;
    let temporary = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(crate::oauth::FileOAuthCredentialStore::new(
        temporary.path().join("oauth"),
    ));
    let dialect = crate::providers::OpenAiChatGptOAuthDialect::for_test(
        &server.origin,
        std::sync::Arc::new(RealRetryVerifier),
    )
    .unwrap();
    let authenticator = RealRetryAuthenticator {
        client: crate::oauth::OAuthClient::new(std::sync::Arc::new(dialect), store.clone())
            .unwrap(),
    };
    let result = setup_result_with_profiles(vec![chatgpt_setup_profile(
        "chatgpt:work",
        "work",
        "gpt-5.6-sol",
    )]);
    let mut editor = ScriptedRecoveryEditor::new([ChatGptSetupRecoveryAction::RetrySignIn]);
    let (config, _, _) = run_chatgpt_setup_recovery_loop(
        SetupInvocation::Command,
        &result,
        &authenticator,
        &mut editor,
    )
    .await
    .unwrap()
    .unwrap();

    let starts = server.state.starts.lock().unwrap();
    assert_eq!(starts.len(), 2);
    assert_ne!(starts[0], starts[1]);
    drop(starts);
    assert!(server
        .state
        .second_start_after_first_poll
        .load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        server
            .state
            .first_polls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        server
            .state
            .second_polls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(editor.recoveries.len(), 1);
    assert_eq!(
        editor.recoveries[0].cause,
        ChatGptSetupFailureCause::ProviderRejected
    );
    let record = store.load("chatgpt:work").unwrap().unwrap();
    assert_eq!(record.account, "acct-work");
    assert!(!record.revoked && !record.mutation_pending);
    assert_eq!(config.credentials().len(), 1);
    assert_eq!(config.credentials()[0].name, "chatgpt:work");
    assert_eq!(
        config.credentials()[0].account.as_deref(),
        Some("acct-work")
    );
    assert!(store.load("device-1").unwrap().is_none());
    let rendered = format!(
        "{:?}\n{}",
        editor.recoveries[0].cause, editor.recoveries[0].summary
    );
    for secret in [
        "device-1",
        "CODE-1",
        "first-body-secret",
        "successful-access-secret",
        "successful-refresh-secret",
    ] {
        assert!(!rendered.contains(secret), "{rendered}");
    }
}

#[tokio::test]
async fn setup_disabled_expired_denied_and_cancelled_return_secret_free_editor_without_save() {
    for (outcome, expected) in [
        (
            ScriptedChatGptOutcome::StartDisabledOrUnsupported,
            ChatGptSetupFailureCause::StartDisabledOrUnsupported,
        ),
        (
            ScriptedChatGptOutcome::Expired,
            ChatGptSetupFailureCause::Expired,
        ),
        (
            ScriptedChatGptOutcome::Denied,
            ChatGptSetupFailureCause::Denied,
        ),
        (
            ScriptedChatGptOutcome::Cancelled,
            ChatGptSetupFailureCause::Cancelled,
        ),
    ] {
        let result = setup_result_with_profiles(vec![chatgpt_setup_profile(
            "chatgpt:work",
            "work",
            "gpt-5.6-sol",
        )]);
        let authenticator = ScriptedRecoveryAuthenticator::new([outcome]);
        let mut editor = ScriptedRecoveryEditor::new([ChatGptSetupRecoveryAction::CancelSetup]);
        let outcome = run_chatgpt_setup_recovery_loop(
            SetupInvocation::Command,
            &result,
            &authenticator,
            &mut editor,
        )
        .await
        .unwrap();
        assert!(outcome.is_none());
        assert_eq!(editor.recoveries[0].cause, expected);
        assert!(editor.recoveries[0]
            .summary
            .contains("No credential was saved"));
        assert_eq!(
            authenticator
                .active
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}

#[tokio::test]
async fn setup_repeated_retry_is_bounded_and_releases_every_ceremony() {
    let result = setup_result_with_profiles(vec![chatgpt_setup_profile(
        "chatgpt:work",
        "work",
        "gpt-5.6-sol",
    )]);
    let authenticator = ScriptedRecoveryAuthenticator::new(
        (0..MAX_CHATGPT_SETUP_ATTEMPTS).map(|_| ScriptedChatGptOutcome::ProviderRejected(503)),
    );
    let mut editor = ScriptedRecoveryEditor::new(
        (0..MAX_CHATGPT_SETUP_ATTEMPTS).map(|_| ChatGptSetupRecoveryAction::RetrySignIn),
    );
    let error = run_chatgpt_setup_recovery_loop(
        SetupInvocation::Command,
        &result,
        &authenticator,
        &mut editor,
    )
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(error.contains("retry limit"), "{error}");
    assert_eq!(
        authenticator.calls.lock().unwrap().len(),
        MAX_CHATGPT_SETUP_ATTEMPTS
    );
    assert_eq!(
        authenticator
            .active
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn setup_final_allowed_failure_can_remove_sole_provider_without_ninth_ceremony() {
    let result = setup_result_with_profiles(vec![chatgpt_setup_profile(
        "chatgpt:work",
        "work",
        "gpt-5.6-sol",
    )]);
    let authenticator = ScriptedRecoveryAuthenticator::new(
        (0..MAX_CHATGPT_SETUP_ATTEMPTS).map(|_| ScriptedChatGptOutcome::ProviderRejected(503)),
    );
    let mut actions = std::collections::VecDeque::from(vec![
        ChatGptSetupRecoveryAction::RetrySignIn;
        MAX_CHATGPT_SETUP_ATTEMPTS - 1
    ]);
    actions.push_back(ChatGptSetupRecoveryAction::RemoveProvider);
    let mut editor = ScriptedRecoveryEditor::new(actions);
    let (config, committed, compensations) = run_chatgpt_setup_recovery_loop(
        SetupInvocation::Command,
        &result,
        &authenticator,
        &mut editor,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        authenticator.calls.lock().unwrap().len(),
        MAX_CHATGPT_SETUP_ATTEMPTS
    );
    assert!(chatgpt_setup_references(&committed).is_empty());
    assert!(config.providers.is_empty());
    assert!(compensations.is_empty());
    assert_eq!(
        authenticator
            .active
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn setup_recovery_changes_exact_named_credential_or_removes_exact_provider() {
    let result = setup_result_with_profiles(vec![
        chatgpt_setup_profile("chatgpt:a-work", "work", "gpt-5.6-sol"),
        chatgpt_setup_profile("chatgpt:z-other", "other", "gpt-5.6-sol"),
    ]);
    let authenticator = ScriptedRecoveryAuthenticator::new([
        ScriptedChatGptOutcome::Denied,
        ScriptedChatGptOutcome::Success,
        ScriptedChatGptOutcome::Success,
    ]);
    let mut editor =
        ScriptedRecoveryEditor::new([ChatGptSetupRecoveryAction::ChangeNamedCredential(
            "chatgpt:replacement".into(),
        )]);
    let (config, committed, _) = run_chatgpt_setup_recovery_loop(
        SetupInvocation::Command,
        &result,
        &authenticator,
        &mut editor,
    )
    .await
    .unwrap()
    .unwrap();
    let references = chatgpt_setup_references(&committed);
    assert_eq!(
        references,
        std::collections::BTreeSet::from([
            "chatgpt:z-other".to_string(),
            "chatgpt:replacement".to_string(),
        ])
    );
    assert_eq!(config.credentials().len(), 2);

    let remove_auth = ScriptedRecoveryAuthenticator::new([
        ScriptedChatGptOutcome::Denied,
        ScriptedChatGptOutcome::Success,
    ]);
    let mut remove_editor =
        ScriptedRecoveryEditor::new([ChatGptSetupRecoveryAction::RemoveProvider]);
    let (_, removed, _) = run_chatgpt_setup_recovery_loop(
        SetupInvocation::Command,
        &result,
        &remove_auth,
        &mut remove_editor,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        chatgpt_setup_references(&removed),
        std::collections::BTreeSet::from(["chatgpt:z-other".to_string()])
    );

    let sole = setup_result_with_profiles(vec![chatgpt_setup_profile(
        "chatgpt:only",
        "only",
        "gpt-5.6-sol",
    )]);
    let sole_auth = ScriptedRecoveryAuthenticator::new([ScriptedChatGptOutcome::Denied]);
    let mut sole_editor = ScriptedRecoveryEditor::new([ChatGptSetupRecoveryAction::RemoveProvider]);
    let (config, removed, compensations) = run_chatgpt_setup_recovery_loop(
        SetupInvocation::Command,
        &sole,
        &sole_auth,
        &mut sole_editor,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(chatgpt_setup_references(&removed).is_empty());
    assert!(config.providers.is_empty());
    assert!(compensations.is_empty());
}

#[tokio::test]
async fn first_run_command_and_repl_share_one_account_multi_model_setup_boundary() {
    for invocation in [
        SetupInvocation::FirstRun,
        SetupInvocation::Command,
        SetupInvocation::Repl,
    ] {
        let result = setup_result_with_profiles(vec![
            chatgpt_setup_profile("chatgpt:work", "work-fast", "gpt-5.6-sol"),
            chatgpt_setup_profile("chatgpt:work", "work-deep", "gpt-5.6-sol"),
        ]);
        let references = std::collections::BTreeSet::from(["chatgpt:work".to_string()]);
        let authenticator = FakeChatGptSetupAuthenticator::default();
        let config = prepare_chatgpt_setup_config(
            &result,
            &references,
            &authenticator,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            authenticator.calls.lock().unwrap().as_slice(),
            ["chatgpt:work"]
        );
        assert_eq!(config.providers.len(), 2, "{invocation:?}");
        assert_eq!(config.credentials().len(), 1, "{invocation:?}");
        config.validate().unwrap();
    }
}

#[tokio::test]
async fn setup_keeps_two_named_chatgpt_accounts_distinct_without_fallback() {
    let result = setup_result_with_profiles(vec![
        chatgpt_setup_profile("chatgpt:work", "work", "gpt-5.6-sol"),
        chatgpt_setup_profile("chatgpt:personal", "personal", "gpt-5.6-sol"),
    ]);
    let references = std::collections::BTreeSet::from([
        "chatgpt:personal".to_string(),
        "chatgpt:work".to_string(),
    ]);
    let authenticator = FakeChatGptSetupAuthenticator::default();
    let config = prepare_chatgpt_setup_config(
        &result,
        &references,
        &authenticator,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(config.credentials().len(), 2);
    assert_eq!(
        config.credentials()[0].secret_ref,
        "oauth-store:chatgpt:personal"
    );
    assert_eq!(
        config.credentials()[1].secret_ref,
        "oauth-store:chatgpt:work"
    );
}

#[tokio::test]
async fn cancelled_or_unusable_setup_never_returns_a_config_or_tries_another_account() {
    let result = setup_result_with_profiles(vec![chatgpt_setup_profile(
        "chatgpt:work",
        "work",
        "gpt-5.6-sol",
    )]);
    let references = std::collections::BTreeSet::from(["chatgpt:work".to_string()]);
    let authenticator = FakeChatGptSetupAuthenticator::default();
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let save_root = tempfile::tempdir().unwrap();
    let save_path = save_root.path().join("config.toml");
    std::fs::write(&save_path, "unchanged-config-sentinel").unwrap();
    let error = prepare_chatgpt_setup_config(&result, &references, &authenticator, cancel)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("sign-in was cancelled"), "{error}");
    assert!(error.contains("No credential was saved"), "{error}");
    assert!(!error.contains("access_token"), "{error}");
    assert!(!error.contains("refresh_token"), "{error}");
    assert_eq!(
        std::fs::read_to_string(save_path).unwrap(),
        "unchanged-config-sentinel"
    );
    assert_eq!(
        authenticator.calls.lock().unwrap().as_slice(),
        ["chatgpt:work"]
    );

    let mut unusable = result;
    unusable.providers.push(ProviderEntry::Credentialed {
        provider: crate::config::CredentialProvider::OpenaiPlatform,
        credential: crate::config::CredentialBinding {
            credential_ref: "missing-platform".into(),
            audience: None,
            tenant: None,
            project: None,
            account: None,
            required_scopes: Default::default(),
        },
        model: Some("gpt-5.6-sol".into()),
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some("broken-platform".into()),
        reasoning_effort: None,
    });
    let before = authenticator.calls.lock().unwrap().len();
    assert!(prepare_chatgpt_setup_config(
        &unusable,
        &references,
        &authenticator,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .is_err());
    assert_eq!(authenticator.calls.lock().unwrap().len(), before);
}

#[tokio::test]
async fn setup_denial_and_expiry_are_terminal_without_config_or_account_fallback() {
    let result = setup_result_with_profiles(vec![chatgpt_setup_profile(
        "chatgpt:work",
        "work",
        "gpt-5.6-sol",
    )]);
    let references = std::collections::BTreeSet::from(["chatgpt:work".to_string()]);
    for (outcome, expected) in [
        (ScriptedChatGptOutcome::Denied, "sign-in was denied"),
        (ScriptedChatGptOutcome::Expired, "sign-in expired"),
    ] {
        let authenticator = ScriptedRecoveryAuthenticator::new([outcome]);
        let error = prepare_chatgpt_setup_config(
            &result,
            &references,
            &authenticator,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains(expected), "{error}");
        assert_eq!(authenticator.calls.lock().unwrap()[0].0, "chatgpt:work");
    }
}

#[tokio::test]
async fn revoked_same_name_metadata_is_replaced_only_by_exact_account_result() {
    let mut result = setup_result_with_profiles(vec![chatgpt_setup_profile(
        "chatgpt:work",
        "work",
        "gpt-5.6-sol",
    )]);
    let mut stale = chatgpt_setup_credential("chatgpt:work", "acct-old");
    stale.lifecycle = crate::config::CredentialLifecycle::Revoked;
    result.credentials = vec![stale];
    let references = std::collections::BTreeSet::from(["chatgpt:work".to_string()]);
    let authenticator = FakeChatGptSetupAuthenticator::default();
    let config = prepare_chatgpt_setup_config(
        &result,
        &references,
        &authenticator,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        authenticator.calls.lock().unwrap().as_slice(),
        ["chatgpt:work"]
    );
    assert_eq!(config.credentials().len(), 1);
    assert_eq!(
        config.credentials()[0].account.as_deref(),
        Some("acct-work")
    );
    assert!(matches!(
        config.credentials()[0].lifecycle,
        crate::config::CredentialLifecycle::Active { .. }
    ));
}

#[tokio::test]
async fn setup_rejects_wrong_chatgpt_secret_store_before_authentication() {
    let mut result = setup_result_with_profiles(vec![chatgpt_setup_profile(
        "chatgpt:work",
        "work",
        "gpt-5.6-sol",
    )]);
    let mut hostile = chatgpt_setup_credential("chatgpt:work", "acct-work");
    hostile.secret_ref = "keyring:finch/chatgpt/work".into();
    result.credentials = vec![hostile];
    let references = std::collections::BTreeSet::from(["chatgpt:work".to_string()]);
    let authenticator = FakeChatGptSetupAuthenticator::default();

    let error = prepare_chatgpt_setup_config(
        &result,
        &references,
        &authenticator,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(error.contains("exact ChatGPT setup authority"), "{error}");
    assert!(authenticator.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn setup_rejects_chatgpt_credential_without_signed_account_before_authentication() {
    let mut result = setup_result_with_profiles(vec![chatgpt_setup_profile(
        "chatgpt:work",
        "work",
        "gpt-5.6-sol",
    )]);
    let mut incomplete = chatgpt_setup_credential("chatgpt:work", "acct-work");
    incomplete.account = None;
    result.credentials = vec![incomplete];
    let references = std::collections::BTreeSet::from(["chatgpt:work".to_string()]);
    let authenticator = FakeChatGptSetupAuthenticator::default();

    let error = prepare_chatgpt_setup_config(
        &result,
        &references,
        &authenticator,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(error.contains("exact ChatGPT setup authority"), "{error}");
    assert!(authenticator.calls.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn setup_multi_account_terminal_failure_tombstones_prior_issue_and_restart_can_resume() {
    use crate::oauth::OAuthCredentialStore;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("oauth");
    let result = setup_result_with_profiles(vec![
        chatgpt_setup_profile("chatgpt:a", "account-a", "gpt-5.6-sol"),
        chatgpt_setup_profile("chatgpt:b", "account-b", "gpt-5.6-sol"),
    ]);
    let references =
        std::collections::BTreeSet::from(["chatgpt:a".to_string(), "chatgpt:b".to_string()]);
    let failing = DurableSetupAuthenticator {
        root: root.clone(),
        fail_reference: Some("chatgpt:b".into()),
        invalid_final_reference: None,
        replace_before_compensation: None,
    };
    assert!(prepare_chatgpt_setup_config(
        &result,
        &references,
        &failing,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("sign-in was denied"));

    let reopened = crate::oauth::FileOAuthCredentialStore::new(root.clone());
    let first = reopened.load("chatgpt:a").unwrap().unwrap();
    assert!(first.revoked && !first.mutation_pending);
    assert!(first.access_token.is_empty());
    assert!(first.refresh_token.is_none() && first.id_token.is_none());
    assert!(reopened.load("chatgpt:b").unwrap().is_none());

    let resumed = DurableSetupAuthenticator {
        root: root.clone(),
        fail_reference: None,
        invalid_final_reference: None,
        replace_before_compensation: None,
    };
    let config = prepare_chatgpt_setup_config(
        &result,
        &references,
        &resumed,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    config.validate().unwrap();
    assert_eq!(config.credentials().len(), 2);
    for reference in ["chatgpt:a", "chatgpt:b"] {
        let record = reopened.load(reference).unwrap().unwrap();
        assert!(!record.revoked && !record.mutation_pending);
        assert!(record.access_token.starts_with("secret-access-"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn setup_injected_save_failure_compensates_owned_generation_without_config_mutation() {
    use crate::oauth::OAuthCredentialStore;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("oauth");
    let config_path = temporary.path().join("config.toml");
    std::fs::write(&config_path, "prior-config-sentinel").unwrap();
    let result = setup_result_with_profiles(vec![chatgpt_setup_profile(
        "chatgpt:work",
        "work",
        "gpt-5.6-sol",
    )]);
    let authenticator = DurableSetupAuthenticator {
        root: root.clone(),
        fail_reference: None,
        invalid_final_reference: None,
        replace_before_compensation: None,
    };
    let mut editor = ScriptedRecoveryEditor::new([]);
    let (config, _, compensations) = run_chatgpt_setup_recovery_loop(
        SetupInvocation::Command,
        &result,
        &authenticator,
        &mut editor,
    )
    .await
    .unwrap()
    .unwrap();
    let error = save_chatgpt_setup_config(&config, &compensations, &authenticator, |_| {
        anyhow::bail!("forced-save-failure-sentinel")
    })
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("newly issued credentials were rolled back"),
        "{error}"
    );
    assert!(!error.contains("secret-access"), "{error}");
    assert_eq!(
        std::fs::read_to_string(config_path).unwrap(),
        "prior-config-sentinel"
    );
    let record = crate::oauth::FileOAuthCredentialStore::new(root)
        .load("chatgpt:work")
        .unwrap()
        .unwrap();
    assert!(record.revoked && !record.mutation_pending);
    assert!(record.access_token.is_empty());
    assert!(record.refresh_token.is_none() && record.id_token.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn setup_compensation_generation_race_leaves_concurrent_replacement_untouched() {
    use crate::oauth::OAuthCredentialStore;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("oauth");
    let result = setup_result_with_profiles(vec![
        chatgpt_setup_profile("chatgpt:a", "account-a", "gpt-5.6-sol"),
        chatgpt_setup_profile("chatgpt:b", "account-b", "gpt-5.6-sol"),
        chatgpt_setup_profile("chatgpt:c", "account-c", "gpt-5.6-sol"),
    ]);
    let references = std::collections::BTreeSet::from([
        "chatgpt:a".to_string(),
        "chatgpt:b".to_string(),
        "chatgpt:c".to_string(),
    ]);
    let racing = DurableSetupAuthenticator {
        root: root.clone(),
        fail_reference: Some("chatgpt:c".into()),
        invalid_final_reference: None,
        replace_before_compensation: Some("chatgpt:b".into()),
    };
    let error = prepare_chatgpt_setup_config(
        &result,
        &references,
        &racing,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("could not be rolled back safely"), "{error}");
    assert!(!error.contains("terminal second-account denial"), "{error}");
    let reopened = crate::oauth::FileOAuthCredentialStore::new(root);
    let external = reopened.load("chatgpt:b").unwrap().unwrap();
    assert!(!external.revoked && !external.mutation_pending);
    assert_eq!(external.access_token, "external-replacement-sentinel");
    let owned = reopened.load("chatgpt:a").unwrap().unwrap();
    assert!(owned.revoked && !owned.mutation_pending);
    assert!(owned.access_token.is_empty());
    assert!(owned.refresh_token.is_none() && owned.id_token.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn setup_final_validation_preserves_cause_and_compensates_every_owned_generation() {
    use crate::oauth::OAuthCredentialStore;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("oauth");
    let result = setup_result_with_profiles(vec![
        chatgpt_setup_profile("chatgpt:a", "account-a", "gpt-5.6-sol"),
        chatgpt_setup_profile("chatgpt:b", "account-b", "gpt-5.6-sol"),
    ]);
    let references =
        std::collections::BTreeSet::from(["chatgpt:a".to_string(), "chatgpt:b".to_string()]);
    let authenticator = DurableSetupAuthenticator {
        root: root.clone(),
        fail_reference: None,
        invalid_final_reference: Some("chatgpt:b".into()),
        replace_before_compensation: None,
    };

    let error = prepare_chatgpt_setup_config(
        &result,
        &references,
        &authenticator,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("ChatGPT sign-in failed"), "{error}");
    assert!(!error.contains("secret-access"), "{error}");
    assert!(!error.contains("secret-refresh"), "{error}");

    let reopened = crate::oauth::FileOAuthCredentialStore::new(root);
    for reference in ["chatgpt:a", "chatgpt:b"] {
        let owned = reopened.load(reference).unwrap().unwrap();
        assert!(owned.revoked && !owned.mutation_pending);
        assert!(owned.access_token.is_empty());
        assert!(owned.refresh_token.is_none() && owned.id_token.is_none());
    }
}

// ── #424: the OAuth device flow runs when the provider is added ──────────

struct ScriptedAddTimeAuthenticator {
    begin: std::sync::Mutex<
        std::collections::VecDeque<Result<ChatGptNamedCredentialStart, anyhow::Error>>,
    >,
    finish: std::sync::Mutex<
        std::collections::VecDeque<Result<EnsuredChatGptCredential, anyhow::Error>>,
    >,
    begins: std::sync::atomic::AtomicUsize,
    finishes: std::sync::atomic::AtomicUsize,
    last_begin_cancel: std::sync::Mutex<Option<tokio_util::sync::CancellationToken>>,
}

impl ScriptedAddTimeAuthenticator {
    fn new(
        begin: impl IntoIterator<Item = Result<ChatGptNamedCredentialStart, anyhow::Error>>,
        finish: impl IntoIterator<Item = Result<EnsuredChatGptCredential, anyhow::Error>>,
    ) -> Self {
        Self {
            begin: std::sync::Mutex::new(begin.into_iter().collect()),
            finish: std::sync::Mutex::new(finish.into_iter().collect()),
            begins: std::sync::atomic::AtomicUsize::new(0),
            finishes: std::sync::atomic::AtomicUsize::new(0),
            last_begin_cancel: std::sync::Mutex::new(None),
        }
    }

    fn begins(&self) -> usize {
        self.begins.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn finishes(&self) -> usize {
        self.finishes.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator for ScriptedAddTimeAuthenticator {
    async fn ensure_named_credential(
        &self,
        _reference: &str,
        _presentation: crate::cli::chatgpt_auth::DeviceLoginPresentation,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<EnsuredChatGptCredential> {
        anyhow::bail!("the add-time dialog must drive the phased ceremony, not the combined one")
    }

    async fn begin_named_credential(
        &self,
        _reference: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ChatGptNamedCredentialStart> {
        self.begins
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.last_begin_cancel.lock().unwrap() = Some(cancel);
        match self.begin.lock().unwrap().pop_front() {
            Some(outcome) => outcome,
            None => anyhow::bail!("scripted add-time begin missing"),
        }
    }

    async fn finish_named_credential(
        &self,
        _reference: &str,
        _pending: &crate::oauth::DeviceAuthorization,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<EnsuredChatGptCredential> {
        self.finishes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.finish.lock().unwrap().pop_front() {
            Some(outcome) => outcome,
            None => anyhow::bail!("scripted add-time finish missing"),
        }
    }
}

fn chatgpt_provider_idx() -> usize {
    CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "chatgpt")
        .unwrap()
}

fn add_time_device_authorization(user_code: &str) -> crate::oauth::DeviceAuthorization {
    crate::oauth::DeviceAuthorization::issued(
        "device-code-secret".into(),
        user_code.into(),
        "https://auth.openai.com/activate".into(),
        None,
        Duration::from_secs(600),
        Duration::from_secs(0),
    )
    .unwrap()
}

fn ensured_for(reference: &str, account: &str) -> EnsuredChatGptCredential {
    EnsuredChatGptCredential {
        credential: chatgpt_setup_credential(reference, account),
        compensation: Some(crate::cli::chatgpt_auth::ChatGptCompensationHandle::issued(
            reference,
            "generation-1".into(),
        )),
    }
}

fn device_auth_step(outcome: DeviceAuthOutcome) -> AddProviderStep {
    AddProviderStep::DeviceAuth {
        provider_idx: chatgpt_provider_idx(),
        name: "chatgpt".to_string(),
        model: "gpt-5.6-sol".to_string(),
        reference: "chatgpt:default".to_string(),
        editing_idx: None,
        pending: Arc::new(Mutex::new(None)),
        outcome,
        cancel: tokio_util::sync::CancellationToken::new(),
    }
}

/// Wait until `probe` yields a value, bounded coarsely so a hung ceremony
/// fails as hung rather than as a timing mismatch.
fn wait_for<T>(probe: impl Fn() -> Option<T>, label: &str) -> T {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the add-time device ceremony never published {label}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn confirming_a_chatgpt_provider_runs_the_device_exchange_in_the_dialog() {
    let fake = Arc::new(ScriptedAddTimeAuthenticator::new(
        [Ok(ChatGptNamedCredentialStart::AuthorizationRequired(
            add_time_device_authorization("CODE-1234"),
        ))],
        [Ok(ensured_for("chatgpt:default", "acct-work"))],
    ));
    let mut state = state_with_step(AddProviderStep::SelectAddType {
        selected: chatgpt_provider_idx(),
    });
    state.current_section = WizardSection::Models;
    state.chatgpt_authenticator = Some(fake.clone());

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { api_key: None, .. })
    ));
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(
        matches!(get_step(&state), Some(AddProviderStep::DeviceAuth { .. })),
        "confirming a ChatGPT add must open the device dialog instead of adding the row silently; step={:?}",
        get_step(&state)
    );
    assert!(
        get_tool_models(&state).is_empty(),
        "no provider row may appear before the exchange completes; got {:?}",
        get_tool_models(&state)
    );

    let presented = wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { pending, .. }) => pending.lock().unwrap().clone(),
            _ => None,
        },
        "the one-time code",
    );
    assert_eq!(presented.user_code, "CODE-1234");
    assert_eq!(
        presented.verification_uri,
        "https://auth.openai.com/activate"
    );
    wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { outcome, .. }) => {
                outcome.lock().unwrap().is_some().then_some(())
            }
            _ => None,
        },
        "the terminal outcome",
    );

    // The dialog reports the authenticated account before returning to the list.
    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("Signed in as acct-work") && rendered.contains("authenticated"),
        "the dialog must show the provider as authenticated before the list; rendered={rendered}"
    );

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    let primary =
        get_primary(&state).expect("the models section must survive the add-time ceremony");
    assert!(
        matches!(primary, ModelConfig::Remote { provider, .. } if provider == "chatgpt"),
        "the provider must be added after a successful exchange; got {primary:?}"
    );
    assert!(
        get_step(&state).is_none(),
        "the device dialog must close on success; step={:?}",
        get_step(&state)
    );
    assert_eq!(
        state.credentials.len(),
        1,
        "the add-time success must bind the named credential in wizard state; got {:?}",
        state.credentials
    );
    assert_eq!(state.credentials[0].name, "chatgpt:default");
    assert_eq!(state.credentials[0].account.as_deref(), Some("acct-work"));
    assert_eq!(
        state.credentials[0].secret_ref,
        "oauth-store:chatgpt:default"
    );
    assert_eq!(fake.begins(), 1);
    assert_eq!(fake.finishes(), 1);
}

#[test]
fn add_time_device_auth_is_skipped_when_a_named_credential_is_already_active() {
    let fake = Arc::new(ScriptedAddTimeAuthenticator::new(
        [Ok(ChatGptNamedCredentialStart::Ensured(
            EnsuredChatGptCredential {
                credential: chatgpt_setup_credential("chatgpt:default", "acct-existing"),
                compensation: None,
            },
        ))],
        [],
    ));
    let mut state = state_with_step(AddProviderStep::SelectAddType {
        selected: chatgpt_provider_idx(),
    });
    state.current_section = WizardSection::Models;
    state.chatgpt_authenticator = Some(fake.clone());

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { outcome, .. }) => {
                outcome.lock().unwrap().is_some().then_some(())
            }
            _ => None,
        },
        "the reuse outcome",
    );

    let presented = match get_step(&state) {
        Some(AddProviderStep::DeviceAuth { pending, .. }) => pending.lock().unwrap().clone(),
        _ => None,
    };
    assert!(
        presented.is_none(),
        "an already-authenticated provider must not show a device code; got {presented:?}"
    );
    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("Signed in as acct-existing"),
        "the dialog must report the reused account; rendered={rendered}"
    );
    assert!(
        !rendered.contains("One-time code"),
        "the skip path must not render device-code instructions; rendered={rendered}"
    );
    assert_eq!(fake.begins(), 1);
    assert_eq!(fake.finishes(), 0, "reuse must not poll the provider");

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert_eq!(state.credentials.len(), 1);
    assert_eq!(
        state.credentials[0].account.as_deref(),
        Some("acct-existing")
    );
    assert!(get_step(&state).is_none());
}

#[test]
fn add_time_device_auth_failure_surfaces_the_cause_and_retry_completes() {
    let fake = Arc::new(ScriptedAddTimeAuthenticator::new(
        [
            Ok(ChatGptNamedCredentialStart::AuthorizationRequired(
                add_time_device_authorization("CODE-1"),
            )),
            Ok(ChatGptNamedCredentialStart::AuthorizationRequired(
                add_time_device_authorization("CODE-2"),
            )),
        ],
        [
            Err(crate::oauth::OAuthDeviceAuthorizationError::Expired.into()),
            Ok(ensured_for("chatgpt:default", "acct-work")),
        ],
    ));
    let mut state = state_with_step(AddProviderStep::SelectAddType {
        selected: chatgpt_provider_idx(),
    });
    state.current_section = WizardSection::Models;
    state.chatgpt_authenticator = Some(fake.clone());

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { outcome, .. }) => {
                outcome.lock().unwrap().is_some().then_some(())
            }
            _ => None,
        },
        "the failed outcome",
    );

    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("sign-in expired"),
        "the terminal cause must be surfaced in the dialog; rendered={rendered}"
    );
    assert!(
        rendered.contains("Retry sign-in"),
        "the failure panel must offer the retry action; rendered={rendered}"
    );
    assert!(
        !rendered.contains("secret-"),
        "the failure panel must stay secret-free; rendered={rendered}"
    );

    // Enter retries this one provider with a fresh ceremony.
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    let presented = wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { pending, .. }) => pending.lock().unwrap().clone(),
            _ => None,
        },
        "the retried one-time code",
    );
    assert_eq!(presented.user_code, "CODE-2");
    assert_eq!(
        fake.begins(),
        2,
        "retry must start a fresh device authorization"
    );
    wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { outcome, .. }) => {
                outcome.lock().unwrap().is_some().then_some(())
            }
            _ => None,
        },
        "the retried outcome",
    );
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert_eq!(state.credentials.len(), 1);
    assert_eq!(state.credentials[0].account.as_deref(), Some("acct-work"));
    assert_eq!(fake.finishes(), 2);
}

#[test]
fn add_time_device_auth_failure_stays_visible_back_on_the_provider_form() {
    let fake = Arc::new(ScriptedAddTimeAuthenticator::new(
        [Ok(ChatGptNamedCredentialStart::AuthorizationRequired(
            add_time_device_authorization("CODE-1"),
        ))],
        [Err(
            crate::oauth::OAuthDeviceAuthorizationError::Denied.into()
        )],
    ));
    let mut state = state_with_step(AddProviderStep::SelectAddType {
        selected: chatgpt_provider_idx(),
    });
    state.current_section = WizardSection::Models;
    state.chatgpt_authenticator = Some(fake.clone());

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { outcome, .. }) => {
                outcome.lock().unwrap().is_some().then_some(())
            }
            _ => None,
        },
        "the failed outcome",
    );
    handle_models_input(&mut state, key(KeyCode::Esc)).unwrap();

    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureRemote { .. })
    ));
    let error = match state.sections.get(&WizardSection::Models) {
        Some(SectionState::Models { error, .. }) => error.clone(),
        other => panic!("expected models section, got {other:?}"),
    };
    assert!(
        error
            .as_deref()
            .is_some_and(|text| text.contains("sign-in was denied")),
        "the failure cause must stay visible on the provider form; error={error:?}"
    );
    assert!(state.credentials.is_empty());
}

#[test]
fn cancelling_the_device_dialog_returns_to_the_provider_form() {
    let fake = Arc::new(ScriptedAddTimeAuthenticator::new(
        [Ok(ChatGptNamedCredentialStart::AuthorizationRequired(
            add_time_device_authorization("CODE-1"),
        ))],
        [Err(
            crate::oauth::OAuthDeviceAuthorizationError::Cancelled.into()
        )],
    ));
    let mut state = state_with_step(AddProviderStep::SelectAddType {
        selected: chatgpt_provider_idx(),
    });
    state.current_section = WizardSection::Models;
    state.chatgpt_authenticator = Some(fake.clone());

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { pending, .. }) => pending.lock().unwrap().clone(),
            _ => None,
        },
        "the one-time code",
    );

    handle_models_input(&mut state, key(KeyCode::Esc)).unwrap();
    let step = get_step(&state);
    assert!(
        matches!(
            step,
            Some(AddProviderStep::ConfigureRemote {
                name,
                model,
                api_key: None,
                editing_idx: None,
                ..
            }) if name == "chatgpt" && model == "gpt-5.6-sol"
        ),
        "Esc must return to the provider form with its fields intact, without abandoning the wizard; step={step:?}"
    );
    assert!(
        fake.last_begin_cancel
            .lock()
            .unwrap()
            .as_ref()
            .expect("the ceremony must have been started")
            .is_cancelled(),
        "Esc must cancel the in-flight ceremony token"
    );
    assert!(get_tool_models(&state).is_empty());
    assert!(state.credentials.is_empty());
    assert_eq!(state.current_section, WizardSection::Models);
    assert_eq!(
        handle_wizard_key(&mut state, key(KeyCode::Char('n'))).unwrap(),
        WizardAction::Continue,
        "the wizard must still be usable after cancelling the device dialog"
    );
}

#[test]
fn editing_an_existing_chatgpt_provider_does_not_start_the_device_ceremony() {
    let fake = Arc::new(ScriptedAddTimeAuthenticator::new([], []));
    let mut state = state_with_step(AddProviderStep::ConfigureRemote {
        provider_idx: chatgpt_provider_idx(),
        name: "chatgpt".to_string(),
        model: "gpt-5.6-sol".to_string(),
        api_key: None,
        focused_field: 1,
        editing_idx: Some(0),
    });
    state.current_section = WizardSection::Models;
    state.chatgpt_authenticator = Some(fake.clone());

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(
        get_step(&state).is_none(),
        "editing keeps the save-time ceremony; got {:?}",
        get_step(&state)
    );
    assert_eq!(
        fake.begins(),
        0,
        "the add-time device ceremony must not run when editing an existing provider"
    );
    assert!(matches!(
        get_primary(&state),
        Some(ModelConfig::Remote { provider, .. }) if provider == "chatgpt"
    ));
}

#[test]
fn device_auth_dialog_keeps_the_run_loop_polling_instead_of_blocking() {
    let mut state = state_with_step(device_auth_step(Arc::new(Mutex::new(None))));
    state.current_section = WizardSection::Models;
    assert!(
        is_scanning_state(&state),
        "the run loop must treat the device dialog as a polling state so terminal input is never blocked; step={:?}",
        get_step(&state)
    );
}

#[test]
fn add_time_device_dialog_presents_code_and_verification_url_as_text() {
    let outcome: DeviceAuthOutcome = Arc::new(Mutex::new(None));
    let mut state = state_with_step(device_auth_step(outcome));
    state.current_section = WizardSection::Models;
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        if let Some(AddProviderStep::DeviceAuth { pending, .. }) = adding_provider.as_mut() {
            *pending.lock().unwrap() = Some(DeviceAuthPresentation {
                verification_uri: "https://auth.openai.com/activate".into(),
                user_code: "CODE-1234".into(),
                expires_in: Duration::from_secs(600),
            });
        }
    }

    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("One-time code: CODE-1234")
            && rendered.contains("Open: https://auth.openai.com/activate"),
        "the dialog must present the code and verification URL as speakable text; rendered={rendered}"
    );
    assert!(
        rendered.contains("Esc: Cancel"),
        "the dialog must advertise its cancellation key; rendered={rendered}"
    );
}

struct ScriptedGrokAddTimeAuthenticator {
    begin: std::sync::Mutex<
        std::collections::VecDeque<
            Result<crate::cli::grok_auth::GrokNamedCredentialStart, anyhow::Error>,
        >,
    >,
    finish: std::sync::Mutex<
        std::collections::VecDeque<
            Result<crate::cli::grok_auth::EnsuredGrokCredential, anyhow::Error>,
        >,
    >,
    begins: std::sync::atomic::AtomicUsize,
    finishes: std::sync::atomic::AtomicUsize,
}

impl ScriptedGrokAddTimeAuthenticator {
    fn new(
        begin: impl IntoIterator<
            Item = Result<crate::cli::grok_auth::GrokNamedCredentialStart, anyhow::Error>,
        >,
        finish: impl IntoIterator<
            Item = Result<crate::cli::grok_auth::EnsuredGrokCredential, anyhow::Error>,
        >,
    ) -> Self {
        Self {
            begin: std::sync::Mutex::new(begin.into_iter().collect()),
            finish: std::sync::Mutex::new(finish.into_iter().collect()),
            begins: std::sync::atomic::AtomicUsize::new(0),
            finishes: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl crate::cli::grok_auth::GrokCredentialAuthenticator for ScriptedGrokAddTimeAuthenticator {
    async fn ensure_named_credential(
        &self,
        _reference: &str,
        _presentation: crate::cli::grok_auth::DeviceLoginPresentation,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::cli::grok_auth::EnsuredGrokCredential> {
        anyhow::bail!("the add-time dialog must drive the phased ceremony, not the combined one")
    }

    async fn begin_named_credential(
        &self,
        _reference: &str,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::cli::grok_auth::GrokNamedCredentialStart> {
        self.begins
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.begin.lock().unwrap().pop_front() {
            Some(outcome) => outcome,
            None => anyhow::bail!("scripted grok add-time begin missing"),
        }
    }

    async fn finish_named_credential(
        &self,
        _reference: &str,
        _pending: &crate::oauth::DeviceAuthorization,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::cli::grok_auth::EnsuredGrokCredential> {
        self.finishes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.finish.lock().unwrap().pop_front() {
            Some(outcome) => outcome,
            None => anyhow::bail!("scripted grok add-time finish missing"),
        }
    }
}

fn grok_sub_provider_idx() -> usize {
    CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "grok-sub")
        .unwrap()
}

fn grok_setup_credential(reference: &str, account: &str) -> crate::config::ProviderCredential {
    crate::config::ProviderCredential {
        name: reference.into(),
        kind: crate::config::CredentialKind::OauthDevice,
        provider: crate::config::CredentialProvider::GrokSubscription,
        issuer: "xai-grok".into(),
        audience: crate::config::AudienceBinding::standard(
            crate::config::EndpointFamily::GrokSubscription,
        ),
        tenant: None,
        project: None,
        account: Some(account.into()),
        scopes: crate::providers::grok_required_scopes(),
        secret_ref: format!("oauth-store:{reference}"),
        lifecycle: crate::config::CredentialLifecycle::Active {
            expires_at: Some(Utc::now() + chrono::TimeDelta::hours(1)),
            refreshable: true,
        },
        revocation: Default::default(),
    }
}

fn grok_ensured_for(
    reference: &str,
    account: &str,
) -> crate::cli::grok_auth::EnsuredGrokCredential {
    crate::cli::grok_auth::EnsuredGrokCredential {
        credential: grok_setup_credential(reference, account),
        compensation: Some(crate::cli::grok_auth::GrokCompensationHandle::issued(
            reference,
            "generation-1".into(),
        )),
    }
}

fn grok_add_time_device_authorization(user_code: &str) -> crate::oauth::DeviceAuthorization {
    crate::oauth::DeviceAuthorization::issued(
        "device-code-secret".into(),
        user_code.into(),
        "https://accounts.x.ai/sign-in/device".into(),
        None,
        Duration::from_secs(600),
        Duration::from_secs(0),
    )
    .unwrap()
}

#[test]
fn confirming_a_grok_sub_provider_runs_the_device_exchange_in_the_dialog() {
    let fake = Arc::new(ScriptedGrokAddTimeAuthenticator::new(
        [Ok(
            crate::cli::grok_auth::GrokNamedCredentialStart::AuthorizationRequired(
                grok_add_time_device_authorization("GROK-1234"),
            ),
        )],
        [Ok(grok_ensured_for("grok-sub:default", "acct-work"))],
    ));
    let mut state = state_with_step(AddProviderStep::SelectAddType {
        selected: grok_sub_provider_idx(),
    });
    state.current_section = WizardSection::Models;
    state.grok_authenticator = Some(fake);

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(
        matches!(get_step(&state), Some(AddProviderStep::DeviceAuth { .. })),
        "confirming SuperGrok must open the device dialog instead of adding the row silently; step={:?}",
        get_step(&state)
    );

    let presented = wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { pending, .. }) => pending.lock().unwrap().clone(),
            _ => None,
        },
        "the SuperGrok one-time code",
    );
    assert_eq!(presented.user_code, "GROK-1234");
    assert_eq!(
        presented.verification_uri,
        "https://accounts.x.ai/sign-in/device"
    );
    wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { outcome, .. }) => {
                outcome.lock().unwrap().is_some().then_some(())
            }
            _ => None,
        },
        "the SuperGrok terminal outcome",
    );

    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("Grok subscription device sign-in")
            && rendered.contains("Signed in as acct-work"),
        "the dialog must name SuperGrok, not ChatGPT; rendered={rendered}"
    );
    assert!(
        !rendered.contains("ChatGPT device sign-in"),
        "SuperGrok overlay must not reuse ChatGPT copy; rendered={rendered}"
    );

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    let primary =
        get_primary(&state).expect("the models section must survive the SuperGrok ceremony");
    assert!(
        matches!(primary, ModelConfig::Remote { provider, .. } if provider == "grok-sub"),
        "the grok-sub lane must be added after a successful exchange; got {primary:?}"
    );
    assert_eq!(state.credentials.len(), 1);
    assert_eq!(state.credentials[0].name, "grok-sub:default");
    assert_eq!(
        state.credentials[0].provider,
        crate::config::CredentialProvider::GrokSubscription
    );
}

#[test]
fn grok_sub_device_404_fails_closed_without_api_key_fallback() {
    let fake = Arc::new(ScriptedGrokAddTimeAuthenticator::new(
        [Err(
            crate::providers::GrokDeviceEndpointError::StartDisabledOrUnsupported.into(),
        )],
        [],
    ));
    let mut state = state_with_step(AddProviderStep::SelectAddType {
        selected: grok_sub_provider_idx(),
    });
    state.current_section = WizardSection::Models;
    state.grok_authenticator = Some(fake);

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { outcome, .. }) => {
                outcome.lock().unwrap().is_some().then_some(())
            }
            _ => None,
        },
        "the SuperGrok 404 outcome",
    );

    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("disabled or unsupported") && rendered.contains("No credential was"),
        "a missing public device flow must fail closed with no credential; rendered={rendered}"
    );
    assert!(
        rendered.contains("will not invent")
            && rendered.contains("Console API-key billing"),
        "404 copy must refuse a fake OAuth button and Console billing fallback; rendered={rendered}"
    );
    assert!(
        !rendered.to_lowercase().contains("use an xai api key"),
        "API keys bill separately and must not be an automatic fallback; rendered={rendered}"
    );
    assert!(state.credentials.is_empty());
}

#[test]
fn grok_sub_invalid_client_fails_closed_without_api_key_fallback() {
    let fake = Arc::new(ScriptedGrokAddTimeAuthenticator::new(
        [Err(
            crate::providers::GrokDeviceEndpointError::ClientRejected.into(),
        )],
        [],
    ));
    let mut state = state_with_step(AddProviderStep::SelectAddType {
        selected: grok_sub_provider_idx(),
    });
    state.current_section = WizardSection::Models;
    state.grok_authenticator = Some(fake);

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { outcome, .. }) => {
                outcome.lock().unwrap().is_some().then_some(())
            }
            _ => None,
        },
        "the SuperGrok invalid_client outcome",
    );

    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("invalid_client") && rendered.contains("No credential was saved"),
        "independent-client rejection must fail closed with no credential; rendered={rendered}"
    );
    assert!(
        rendered.contains("will not switch to Console API-key billing"),
        "invalid_client copy must refuse Console billing fallback; rendered={rendered}"
    );
    assert!(
        !rendered.to_lowercase().contains("use an xai api key"),
        "API keys bill separately and must not be an automatic fallback; rendered={rendered}"
    );
    assert!(state.credentials.is_empty());
}

#[test]
fn grok_sub_does_not_accept_console_api_key_edits() {
    let grok_sub = ModelConfig::Remote {
        provider: "grok-sub".into(),
        name: "Grok subscription (SuperGrok)".into(),
        api_key: String::new(),
        model: "grok-4.6".into(),
        enabled: true,
        persisted: None,
    };
    let grok_api = ModelConfig::Remote {
        provider: "grok".into(),
        name: "Grok API (xAI Console)".into(),
        api_key: String::new(),
        model: String::new(),
        enabled: true,
        persisted: None,
    };
    assert!(
        !grok_sub.accepts_api_key(),
        "SuperGrok must not take a Console API key; that lane bills separately"
    );
    assert!(
        grok_api.accepts_api_key(),
        "the explicit xAI Console provider still takes an API key"
    );

    let mut state = WizardState::new_with_catalog_cache_dir(None, None);
    state.current_section = WizardSection::Models;
    if let Some(SectionState::Models {
        primary_model,
        editing_mode,
        selected_idx,
        ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        *primary_model = grok_sub;
        *editing_mode = true;
        *selected_idx = 0;
    }
    handle_models_input(&mut state, key(KeyCode::Char('x'))).unwrap();
    assert!(
        matches!(
            get_primary(&state),
            Some(ModelConfig::Remote {
                provider,
                api_key,
                ..
            }) if provider == "grok-sub" && api_key.is_empty()
        ),
        "typing into SuperGrok must not store a Console key; primary={:?}",
        get_primary(&state)
    );
    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("not an API key") || rendered.contains("bill separately"),
        "the editor must refuse Console billing on grok-sub; rendered={rendered}"
    );
    assert!(
        !rendered.contains("Edit API Key"),
        "SuperGrok must not open the API-key editor; rendered={rendered}"
    );
}

#[test]
fn test_wizard_view_borders_are_glyph_runs_of_exact_frame_width_at_80_and_120() {
    // INVARIANT (#926): every boxed border row in every section's frame is a
    // glyph-only run of exactly the frame width — the interior gap is never
    // interpolated as digits, and no border overflows or falls short of the
    // frame. The row-diff blit depends on that exactness.
    for (width, height) in [(80usize, 24usize), (120, 40)] {
        for section in WizardSection::all() {
            let mut state = WizardState::new(None);
            state.current_section = section;
            let view = wizard_view_with_permission_target(&state, "", width, height);
            let frame = crate::cli::tui::plan_wizard_frame(&view, width, height);
            for line in &frame.lines {
                if !(line.starts_with('┌') || line.starts_with('└')) {
                    continue;
                }
                let visible = crate::cli::tui::wizard_visible_length(line);
                assert_eq!(
                    visible,
                    width,
                    "border row of the {} section at {width} columns must fill the frame \
                     exactly; got {line:?}",
                    section.name()
                );
                let digits: Vec<char> = line.chars().filter(|ch| ch.is_ascii_digit()).collect();
                assert!(
                    digits.is_empty(),
                    "border row of the {} section at {width} columns must not carry the \
                     gap count as digits; got {line:?} (digits {digits:?})",
                    section.name()
                );
                assert!(
                    line.trim_end().ends_with('┐') || line.trim_end().ends_with('┘'),
                    "border row of the {} section must close its box; got {line:?}",
                    section.name()
                );
            }
            // A frame that stops short of the terminal height is the stale-row
            // defect class: the previous frame's rows would stay painted below
            // it and the diff would never revisit them.
            let covered = frame
                .row_spans
                .last()
                .map(|(start, rows)| start + rows)
                .unwrap_or(0);
            assert_eq!(
                covered,
                height,
                "the {} section's frame at {width}x{height} must cover every row so the \
                 blit erases the previous frame; covered {covered}",
                section.name()
            );
        }
    }
}

// ── #1140: the styling symptoms stage 4 kills ────────────────────────────────

/// The raw SGR bytes of one planned wizard frame — what the blit prints, not
/// the shadow buffer's stripped text.
fn wizard_frame_bytes(state: &WizardState, width: usize, height: usize) -> String {
    let view = wizard_view_with_permission_target(state, "", width, height);
    let frame = crate::cli::tui::plan_wizard_frame(&view, width, height);
    frame.lines.join("\n")
}

/// REGRESSION (#1140, selection contrast): the selected row wears the
/// selection style — bold bright-white on a black background — not just the
/// `>>>` prefix, and unselected rows carry no selection marking. The
/// pre-migration painter styled selections `bg(Black).fg(White)`; the widget
/// host lost the background channel and selections read grey-on-grey on a
/// grey terminal.
#[test]
fn test_selected_rows_carry_selection_contrast_beyond_the_prefix() {
    // The style at the props boundary: foreground white, background black,
    // bold — the #1140 contrast prop, in one place.
    let selected = crate::cli::tui::wizard_selected(">>> Selected provider <<<");
    let span = &selected.0[0];
    assert_eq!(
        (span.fg, span.bg, span.bold),
        (
            Some(crate::cli::tui::WizardColor::White),
            Some(crate::cli::tui::WizardColor::Black),
            true,
        ),
        "the selection style must be bold white on black; got span {span:?}"
    );
    assert!(
        crate::cli::tui::wizard_line_is_selected(&selected)
            && !crate::cli::tui::wizard_line_is_selected(&crate::cli::tui::wizard_line(
                "plain row",
                crate::cli::tui::WizardColor::Blue,
            )),
        "the selection marker is a styled span, not a prefix; selected={selected:?}"
    );

    // The render boundary: the models section paints the selection SGR run on
    // the selected row and nothing on the unselected ones.
    let state = WizardState::new(None);
    let bytes = wizard_frame_bytes(&state, 80, 24);
    assert!(
        bytes.contains("\x1b[1;97;40m"),
        "a selected wizard row must paint bold bright-white on black; frame: {bytes:?}"
    );
    let unselected_prefix_count = bytes.matches("\x1b[1;97;40m").count();
    assert!(
        unselected_prefix_count == 1,
        "exactly the selected row wears the selection style; count was \
         {unselected_prefix_count} in: {bytes:?}"
    );
}

/// REGRESSION (#1140, tab highlight): the `selected_tab` prop decides which
/// tab wears the active style — bold magenta on black — and moving the
/// marker moves the highlight. The tab row carries the active marker prop at
/// the props/render boundary, so every section shows which pane is active.
#[test]
fn test_active_tab_marker_prop_selects_the_highlighted_tab() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Themes;
    let themes_view = wizard_view_with_permission_target(&state, "", 100, 24);
    let themes_marker = themes_view.selected_tab;
    assert_eq!(
        WizardSection::all()[themes_marker],
        WizardSection::Themes,
        "the view's selected_tab prop must carry the current section; prop was {themes_marker}"
    );

    // Render two frames whose marker props differ; the highlight follows.
    let mut state_second = WizardState::new(None);
    state_second.current_section = WizardSection::Models;
    let bytes_first = wizard_frame_bytes(&state, 100, 24);
    let bytes_second = wizard_frame_bytes(&state_second, 100, 24);
    for (name, tab_title, bytes, other) in [
        ("Themes", "Look & Feel", &bytes_first, &bytes_second),
        ("Models", "Model Setup", &bytes_second, &bytes_first),
    ] {
        let active_run = format!("\x1b[1;35;40m{tab_title}");
        assert!(
            bytes.contains(&active_run),
            "the {name} tab must wear the active style (bold magenta on black) when \
             selected_tab marks it; frame: {bytes:?}"
        );
        assert!(
            !other.contains(&active_run),
            "the {name} tab must not wear the active style while another pane is \
             selected; frame: {other:?}"
        );
    }
}

/// REGRESSION (#1140, context lines): the spinner row shows its value, the
/// ◀/▶ affordance is advertised in the instructions on every platform, and
/// the Left/Right keys actually adjust the value.
#[test]
fn test_context_lines_spinner_value_is_visible_keys_adjust_and_keys_are_advertised() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Features;
    for _ in 0..SETTINGS_CONTEXT_IDX {
        handle_features_input(&mut state, key(KeyCode::Down)).unwrap();
    }
    let before = features_context_lines(&state);
    assert_eq!(before, 4, "the default context-lines value starts at 4");

    // ◀ decrements, ▶ increments, and the value renders (the value IS the
    // row's content — the #1140 report could not set it).
    handle_features_input(&mut state, key(KeyCode::Left)).unwrap();
    assert_eq!(
        features_context_lines(&state),
        3,
        "◀ must decrement the context-lines spinner"
    );
    handle_features_input(&mut state, key(KeyCode::Right)).unwrap();
    handle_features_input(&mut state, key(KeyCode::Right)).unwrap();
    assert_eq!(
        features_context_lines(&state),
        5,
        "▶ must increment the context-lines spinner"
    );
    let rendered = wizard_text_with_permission_target(&state, "", 80, 24);
    assert!(
        rendered.contains("Context lines: 5"),
        "the spinner's current value must be visible; rendered: {rendered}"
    );
    assert!(
        rendered.contains("◀ Context lines: 5 ▶"),
        "the spinner affordances must be visible; rendered: {rendered}"
    );
    assert!(
        rendered.contains("◀/▶: Context lines"),
        "the instructions must advertise the ◀/▶ keys (#1140: the spinner was \
         undiscoverable on macOS); rendered: {rendered}"
    );
    // Bounds hold: 1..=8.
    for _ in 0..10 {
        handle_features_input(&mut state, key(KeyCode::Left)).unwrap();
    }
    assert_eq!(features_context_lines(&state), 1, "the spinner clamps at 1");
    for _ in 0..10 {
        handle_features_input(&mut state, key(KeyCode::Right)).unwrap();
    }
    assert_eq!(features_context_lines(&state), 8, "the spinner clamps at 8");
}

fn features_context_lines(state: &WizardState) -> usize {
    match state.sections.get(&WizardSection::Features) {
        Some(SectionState::Features {
            memory_context_lines,
            ..
        }) => *memory_context_lines,
        other => panic!("features section state must exist; got {other:?}"),
    }
}

/// SCAN REGRESSION (#1141): the wizard view builders construct spans, never
/// SGR bytes — the only escape sequences in the wizard surface live in the
/// host's lowering. Mirrors the component-renderer scan.
#[test]
fn test_wizard_view_builders_construct_no_sgr_bytes() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/setup_wizard/render.rs"
    ))
    .expect("cannot read the wizard view builders under test");
    let offenders: Vec<String> = source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("\\x1b") || line.contains("\\u{1b}"))
        .map(|(index, line)| format!("line {}: {line}", index + 1))
        .collect();
    assert!(
        offenders.is_empty(),
        "wizard view builders must carry no SGR bytes (stage 4 #1141); \
         found escape sequences at:\n{}",
        offenders.join("\n")
    );
}
