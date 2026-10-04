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
        (WizardSection::LocalHelpers, WizardSection::Models),
        (WizardSection::Personas, WizardSection::LocalHelpers),
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

// ── the provider form must not turn one provider's entry into another's ────

/// A saved configuration shaped like the reported one: a ChatGPT subscription
/// on a non-default model, an API-key provider with its own endpoint, and an
/// OpenAI-compatible endpoint.
fn three_saved_providers_config(metrics_dir: std::path::PathBuf) -> crate::config::Config {
    use crate::config::{
        AudienceBinding, CredentialBinding, CredentialKind, CredentialLifecycle,
        CredentialProvider, EndpointFamily, ProviderCredential,
    };
    let scopes = crate::providers::chatgpt_required_scopes();
    let compatible_url = "https://compatible.example/v1";
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
            reasoning_effort: None,
        },
        ProviderEntry::Grok {
            api_key: "xai-test-preserved".into(),
            model: Some("grok-code-fast-1".into()),
            base_url: Some("https://xai-compatible.example/v1".into()),
            chat_path: None,
            models_path: None,
            name: Some("Grok Build".into()),
        },
        compatible_test_profile("Ciru", "ciru:default", compatible_url),
    ];
    let chatgpt = ProviderCredential {
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
            expires_at: None,
            refreshable: true,
        },
        revocation: Default::default(),
    };
    crate::config::Config::with_providers_and_paths(providers, metrics_dir).with_credentials(vec![
        chatgpt,
        compatible_test_credential("ciru:default", "CIRU_API_KEY", compatible_url),
    ])
}

/// One line per provider: type, name and model, in list order.
fn provider_summary(providers: &[ProviderEntry]) -> Vec<String> {
    providers
        .iter()
        .map(|entry| {
            format!(
                "{} · {} · {}",
                entry.provider_type(),
                entry.profile_name(),
                entry.model().unwrap_or("(none)")
            )
        })
        .collect()
}

/// What the wizard would write to disk for its current state.
fn saved_providers(state: &WizardState, metrics_dir: std::path::PathBuf) -> Vec<ProviderEntry> {
    let result = build_setup_result(state).expect("the wizard state must build a setup result");
    config_from_setup_result_with_paths(&result, metrics_dir).providers
}

fn models_section_error(state: &WizardState) -> Option<String> {
    match state.sections.get(&WizardSection::Models) {
        Some(SectionState::Models { error, .. }) => error.clone(),
        _ => None,
    }
}

#[test]
fn test_editing_a_saved_provider_cannot_turn_it_into_another_provider_with_left_right() {
    let directory = tempfile::tempdir().unwrap();
    let metrics_dir = directory.path().join("metrics");
    let config = three_saved_providers_config(metrics_dir.clone());
    let before = provider_summary(&config.providers);
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&config), None);
    state.current_section = WizardSection::Models;

    // Select the second row ("Grok Build"), open its edit form, move up from
    // the Name row to the Provider row, press Left twice, confirm.
    handle_wizard_key(&mut state, key(KeyCode::Down)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Enter)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Up)).unwrap();
    assert!(
        matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote {
                focused_field: 0,
                editing_idx: Some(1),
                ..
            })
        ),
        "precondition: the edit form for the second row must be open with the Provider row focused; step={:?}",
        get_step(&state)
    );
    handle_wizard_key(&mut state, key(KeyCode::Left)).unwrap();
    let reported = models_section_error(&state);
    let step_after_left = format!("{:?}", get_step(&state));
    let screen_after_left = render_wizard_text(&state);
    handle_wizard_key(&mut state, key(KeyCode::Left)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Enter)).unwrap();

    let saved = saved_providers(&state, metrics_dir);
    let after = provider_summary(&saved);
    assert_eq!(
        after, before,
        "editing a saved entry must not change its provider type: the row named 'Grok Build' \
         must still be the Grok API entry with its own model, not another provider's entry \
         carrying that provider's default model under the old name.\nsaved providers: {after:#?}"
    );
    assert!(
        matches!(
            saved.get(1),
            Some(ProviderEntry::Grok { api_key, base_url, .. })
                if api_key == "xai-test-preserved"
                    && base_url.as_deref() == Some("https://xai-compatible.example/v1")
        ),
        "the edited entry must keep its API key and custom endpoint; saved providers: {after:#?}"
    );
    assert_eq!(
        reported.as_deref(),
        Some(PROVIDER_TYPE_IS_FIXED),
        "pressing Left on a saved entry's Provider row must say why nothing changed; \
         step after Left: {step_after_left}"
    );
    assert!(
        screen_after_left.contains("provider type is fixed"),
        "the reason must be on screen while the edit form is open, not only in wizard state; \
         screen:\n{screen_after_left}"
    );
}

#[test]
fn test_editing_a_saved_provider_keeps_its_model_after_left_right_on_the_provider_row() {
    let directory = tempfile::tempdir().unwrap();
    let metrics_dir = directory.path().join("metrics");
    let config = three_saved_providers_config(metrics_dir.clone());
    let before = provider_summary(&config.providers);
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&config), None);
    state.current_section = WizardSection::Models;

    // Open the ChatGPT subscription's edit form, move up to the Provider row,
    // press Right then Left (back to where it started), confirm.
    handle_wizard_key(&mut state, key(KeyCode::Enter)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Up)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Right)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Left)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Enter)).unwrap();

    let after = provider_summary(&saved_providers(&state, metrics_dir));
    assert_eq!(
        after, before,
        "Right then Left on a saved entry's Provider row must leave the entry as it was: the \
         ChatGPT subscription must keep the model it was saved with (gpt-5.6-sol), not be reset \
         to the setup default for its provider type.\nsaved providers: {after:#?}"
    );
}

#[test]
fn test_add_form_generated_name_follows_the_provider_and_a_typed_name_is_kept() {
    let directory = tempfile::tempdir().unwrap();
    let metrics_dir = directory.path().join("metrics");
    let config = three_saved_providers_config(metrics_dir.clone());

    // `a`, Enter opens the add form for the first provider (ChatGPT
    // subscription) with the generated name "chatgpt"; Up reaches the
    // Provider row; Left selects another provider; Enter adds it.
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&config), None);
    state.current_section = WizardSection::Models;
    handle_wizard_key(&mut state, key(KeyCode::Char('a'))).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Enter)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Up)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Left)).unwrap();
    let Some(AddProviderStep::ConfigureRemote {
        provider_idx, name, ..
    }) = get_step(&state)
    else {
        panic!(
            "the add form must stay open after Left on the Provider row; step={:?}",
            get_step(&state)
        );
    };
    let selected_id = CLOUD_PROVIDERS[*provider_idx].0;
    assert_ne!(
        selected_id, "chatgpt",
        "precondition: Left on the Provider row of an add form must select another provider"
    );
    assert_eq!(
        name, selected_id,
        "a generated name must follow the selected provider: a '{selected_id}' entry must not be \
         named after the provider the form opened with"
    );
    handle_wizard_key(&mut state, key(KeyCode::Enter)).unwrap();
    let saved = saved_providers(&state, metrics_dir);
    let summary = provider_summary(&saved);
    assert!(
        !saved.iter().any(|entry| entry.profile_name() == "chatgpt"),
        "no saved entry may be named 'chatgpt' when no ChatGPT entry was added; \
         saved providers: {summary:#?}"
    );
    assert!(
        saved
            .iter()
            .any(|entry| entry.profile_name() == selected_id),
        "the added entry must be saved under its own provider's name '{selected_id}'; \
         saved providers: {summary:#?}"
    );

    // A name the user typed is theirs: changing the provider leaves it alone.
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&config), None);
    state.current_section = WizardSection::Models;
    handle_wizard_key(&mut state, key(KeyCode::Char('a'))).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Enter)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Char('2'))).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Up)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Left)).unwrap();
    assert!(
        matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { name, .. }) if name == "chatgpt2"
        ),
        "a typed name must survive a provider change; step={:?}",
        get_step(&state)
    );
}

#[test]
fn test_editing_the_unconfigured_placeholder_can_still_choose_a_provider() {
    // The first-run row is an unconfigured placeholder, and Enter on it opens
    // the same form in edit mode. Choosing a provider there must keep working.
    let config = crate::config::Config::with_providers_and_paths(
        vec![ProviderEntry::Claude {
            api_key: String::new(),
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("claude".to_string()),
        }],
        std::path::PathBuf::from("unused-test-metrics"),
    );
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&config), None);
    state.current_section = WizardSection::Models;
    handle_wizard_key(&mut state, key(KeyCode::Enter)).unwrap();
    for _ in 0..3 {
        handle_wizard_key(&mut state, key(KeyCode::Up)).unwrap();
    }
    handle_wizard_key(&mut state, key(KeyCode::Right)).unwrap();
    let Some(AddProviderStep::ConfigureRemote {
        provider_idx,
        name,
        editing_idx: Some(0),
        ..
    }) = get_step(&state)
    else {
        panic!(
            "the placeholder's form must stay open in edit mode; step={:?}",
            get_step(&state)
        );
    };
    let selected_id = CLOUD_PROVIDERS[*provider_idx].0;
    assert_ne!(
        selected_id,
        "claude",
        "Right on the unconfigured placeholder's Provider row must select another provider; \
         error={:?}",
        models_section_error(&state)
    );
    assert_eq!(
        name, selected_id,
        "the placeholder's generated name must follow the selected provider"
    );
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

/// Regression: the Local Helpers toggle must default true (matching
/// `finch_memory::MemoryConfig::default().use_neural_embeddings`), survive
/// Space toggling it off, and reload false from the resulting config -- not
/// silently reset to the default on reopen, which is the exact defect this
/// pins for any wizard field that reads from `existing_config`.
#[test]
fn test_local_helpers_neural_embeddings_toggle_survives_config_mapping_and_reopen() {
    let state = WizardState::new(None);
    assert!(
        matches!(
            state.sections.get(&WizardSection::LocalHelpers),
            Some(SectionState::LocalHelpers {
                use_neural_embeddings: true
            })
        ),
        "default must be on, matching MemoryConfig::default(); got {:?}",
        state.sections.get(&WizardSection::LocalHelpers)
    );

    let mut state = state;
    state.current_section = WizardSection::LocalHelpers;
    assert_eq!(
        handle_wizard_key(&mut state, key(KeyCode::Char(' '))).unwrap(),
        WizardAction::Continue
    );
    assert!(
        matches!(
            state.sections.get(&WizardSection::LocalHelpers),
            Some(SectionState::LocalHelpers {
                use_neural_embeddings: false
            })
        ),
        "Space must toggle it off; got {:?}",
        state.sections.get(&WizardSection::LocalHelpers)
    );

    let result = build_setup_result(&state).unwrap();
    assert!(
        !result.use_neural_embeddings,
        "the off toggle must reach SetupResult"
    );
    let config = config_from_setup_result(&result);
    assert!(
        !config.memory.use_neural_embeddings,
        "the off toggle must reach the saved Config"
    );

    let reopened = WizardState::new(Some(&config));
    assert!(
        matches!(
            reopened.sections.get(&WizardSection::LocalHelpers),
            Some(SectionState::LocalHelpers {
                use_neural_embeddings: false
            })
        ),
        "reopening from a saved config with the toggle off must not silently \
         reset to the true default; got {:?}",
        reopened.sections.get(&WizardSection::LocalHelpers)
    );
}

/// A minimal terminal emulator covering exactly the escape vocabulary
/// `WizardHost::paint` emits (cursor addressing CSI H, erase-in-line/display
/// CSI K/J, CR/LF, deferred autowrap, and `?`-prefixed mode-set/SGR
/// sequences consumed as screen noise) — enough to replay its raw output
/// bytes into the screen a reader would actually see. This is the same
/// end-to-end check the tmux `capture-pane -e -p` byte-level verification
/// that found #1297 performed live, reproduced deterministically here.
struct MiniVt {
    width: usize,
    height: usize,
    screen: Vec<Vec<char>>,
    row: usize,
    col: usize,
    pending_wrap: bool,
}

impl MiniVt {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            screen: vec![vec![' '; width]; height],
            row: 0,
            col: 0,
            pending_wrap: false,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        let chars: Vec<char> = String::from_utf8_lossy(bytes).chars().collect();
        let mut index = 0;
        while index < chars.len() {
            index = self.step(&chars, index);
        }
    }

    fn step(&mut self, chars: &[char], index: usize) -> usize {
        match chars[index] {
            '\x1b' => self.escape(chars, index + 1),
            '\r' => {
                self.col = 0;
                self.pending_wrap = false;
                index + 1
            }
            '\n' => {
                self.line_feed();
                index + 1
            }
            c if (c as u32) < 0x20 => index + 1,
            c => {
                self.put(c);
                index + 1
            }
        }
    }

    fn escape(&mut self, chars: &[char], index: usize) -> usize {
        if chars.get(index) != Some(&'[') {
            return index + 1;
        }
        let mut cursor = index + 1;
        let start = cursor;
        while cursor < chars.len() && !('\u{40}'..='\u{7e}').contains(&chars[cursor]) {
            cursor += 1;
        }
        if cursor >= chars.len() {
            return chars.len();
        }
        let body: String = chars[start..cursor].iter().collect();
        self.csi(&body, chars[cursor]);
        cursor + 1
    }

    fn csi(&mut self, body: &str, final_byte: char) {
        if body.starts_with('?') || body.starts_with('>') {
            return; // mode sets, synchronized updates: screen noise
        }
        let params: Vec<usize> = body
            .split(';')
            .map(|part| part.parse::<usize>().unwrap_or(0))
            .collect();
        let first = params.first().copied().unwrap_or(0);
        match final_byte {
            'H' | 'f' => {
                self.row = first.saturating_sub(1).min(self.height.saturating_sub(1));
                self.col = params
                    .get(1)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(1)
                    .min(self.width.saturating_sub(1));
                self.pending_wrap = false;
            }
            'J' if first >= 2 => {
                self.screen = vec![vec![' '; self.width]; self.height];
            }
            'K' if first == 0 => {
                for column in self.col..self.width {
                    self.screen[self.row][column] = ' ';
                }
                self.pending_wrap = false;
            }
            _ => {}
        }
    }

    fn line_feed(&mut self) {
        self.pending_wrap = false;
        if self.row + 1 < self.height {
            self.row += 1;
        } else {
            self.screen.remove(0);
            self.screen.push(vec![' '; self.width]);
        }
    }

    fn put(&mut self, c: char) {
        if self.pending_wrap {
            self.pending_wrap = false;
            self.col = 0;
            self.line_feed();
        }
        if self.row < self.height && self.col < self.width {
            self.screen[self.row][self.col] = c;
        }
        self.col += 1;
        if self.col >= self.width {
            self.pending_wrap = true;
        }
    }

    /// The visible screen, trailing blanks trimmed per row.
    fn rows(&self) -> Vec<String> {
        self.screen
            .iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect()
    }
}

/// REGRESSION (#1297): toggling the Local Helpers memory-embeddings checkbox
/// OFF shrinks its two-physical-row "On: ..." description to a one-row
/// "Off: ..." description. Live tmux `capture-pane -e -p` verification
/// showed the OLD second row ("recall quality than the fallback below.")
/// surviving underneath the new one-row text. Root cause: the widget host's
/// row-diff blit (`WizardHost::paint`) skips repainting a logical line whose
/// printed *content* is unchanged from the previous frame, without checking
/// whether that line's *absolute terminal row* moved — and it moved here,
/// because the un-wrapped description was one logical `WizardLine` whose
/// physical-row span (via terminal auto-wrap) shrank from 2 to 1, shifting
/// every following line (including a blank padding row whose *text* reads
/// the same in both frames) one row higher. Asserting on `frame.lines` alone
/// would miss this: the computed frame for the OFF state never contains the
/// stale text — only the *blitted* screen does, after two real paints. This
/// test drives the real two-frame paint through a byte-accurate replay of
/// the exact bytes `WizardHost::paint` writes, not a substring check.
#[test]
fn test_local_helpers_toggle_off_does_not_strand_the_on_descriptions_second_row() {
    // 131 columns: wide enough that the OFF description (126 chars) fits on
    // one physical row, but the ON description (170 chars) still needs two
    // -- the long-to-short shrink #1297 depends on -- and the raw terminal's
    // dumb character wrap (this line isn't word-wrapped before printing)
    // happens to break ON's second row exactly on the word boundary this
    // test asserts on. At the file's other tests' usual 100-column width
    // both variants wrap to two rows, which would mask the defect entirely.
    let width = 131;
    let height = 30;
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::LocalHelpers;
    assert!(
        matches!(
            state.sections.get(&WizardSection::LocalHelpers),
            Some(SectionState::LocalHelpers {
                use_neural_embeddings: true
            })
        ),
        "must start ON (the default) so the toggle exercises the long-to-short \
         direction; got {:?}",
        state.sections.get(&WizardSection::LocalHelpers)
    );

    let mut host = crate::cli::tui::WizardHost::new();
    let mut sink: Vec<u8> = Vec::new();

    let view_on = wizard_view_with_permission_target(&state, "", width, height);
    let frame_on = crate::cli::tui::plan_wizard_frame(&view_on, width, height);
    host.paint(&mut sink, &frame_on, width, height).unwrap();
    let mut terminal = MiniVt::new(width, height);
    terminal.feed(&sink);
    assert!(
        terminal
            .rows()
            .iter()
            .any(|row| row.contains("concepts and finds past context more accurately.")),
        "sanity check: the ON description's second wrapped row must actually \
         reach the terminal before the toggle flips, or this test proves \
         nothing; screen:\n{}",
        terminal.rows().join("\n")
    );

    // The real key path (Space toggles the checkbox), not a direct field
    // mutation, so this exercises the same input the live tmux session did.
    assert_eq!(
        handle_wizard_key(&mut state, key(KeyCode::Char(' '))).unwrap(),
        WizardAction::Continue
    );
    let view_off = wizard_view_with_permission_target(&state, "", width, height);
    let frame_off = crate::cli::tui::plan_wizard_frame(&view_off, width, height);
    sink.clear();
    host.paint(&mut sink, &frame_off, width, height).unwrap();
    terminal.feed(&sink);

    let screen = terminal.rows();
    assert!(
        screen
            .iter()
            .any(|row| row.contains("Off: Uses basic keyword matching")),
        "the OFF description must reach the terminal after the toggle; \
         screen:\n{}",
        screen.join("\n")
    );
    assert!(
        !screen
            .iter()
            .any(|row| row.contains("concepts and finds past context more accurately.")),
        "REGRESSION (#1297): the ON description's stale second wrapped row \
         must not survive after toggling to the shorter OFF description; \
         full screen contents:\n{}",
        screen.join("\n")
    );
}

/// REGRESSION (#1305, follow-up from #1297): `models_section_lines`'s
/// primary-provider description varies in physical row count across its
/// provider-specific variants -- confirmed by an exhaustive sweep of every
/// pair of the four variants (no-key, keyed, ChatGPT, Grok subscription)
/// across widths 60-150 comparing each transition's incrementally-blitted
/// screen against an independent from-scratch repaint of the same frame: at
/// 131 columns, going from Grok subscription's primary provider to ChatGPT's
/// (both reachable by changing the primary provider on this tab, no API key
/// required) diverges. Grok's long description wraps to 2 physical rows at
/// that width while ChatGPT's fits in 1, shrinking the block by one row with
/// nothing but `wizard_boxed("AI Providers", ...)` right after it, and the
/// row-diff blit strands both a leftover text fragment from Grok's second
/// row where the box's top border belongs *and* a duplicate of the old
/// "grok-sub" primary-provider row that the real repaint does not show.
/// This test drives the real two-frame `WizardHost::paint` byte stream
/// through the same `MiniVt` replay #1297's fix test uses, then asserts the
/// stronger general invariant: the incrementally-painted screen must match
/// an independent full repaint of the same final frame, not just lack one
/// known-bad substring.
#[test]
fn test_models_section_grok_to_chatgpt_transition_does_not_strand_a_stale_row() {
    let width = 131;
    let height = 30;

    let grok_sub = ModelConfig::Remote {
        provider: "grok-sub".to_string(),
        name: "grok-sub".to_string(),
        api_key: String::new(),
        model: String::new(),
        enabled: true,
        persisted: None,
    };
    let chatgpt = ModelConfig::Remote {
        provider: "chatgpt".to_string(),
        name: "chatgpt".to_string(),
        api_key: String::new(),
        model: String::new(),
        enabled: true,
        persisted: None,
    };

    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Models;
    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *primary_model = grok_sub;
    }

    let mut host = crate::cli::tui::WizardHost::new();
    let mut sink: Vec<u8> = Vec::new();

    let view_grok = wizard_view_with_permission_target(&state, "", width, height);
    let frame_grok = crate::cli::tui::plan_wizard_frame(&view_grok, width, height);
    host.paint(&mut sink, &frame_grok, width, height).unwrap();
    let mut terminal = MiniVt::new(width, height);
    terminal.feed(&sink);
    assert!(
        terminal
            .rows()
            .iter()
            .any(|row| row.contains("tomatically.")),
        "sanity check: Grok's description must actually wrap to a second \
         physical row (the raw terminal's dumb character wrap splits \
         \"automatically.\" into \"au\" + \"tomatically.\" at 131 columns) \
         before the primary provider changes, or this test proves nothing; \
         screen:\n{}",
        terminal.rows().join("\n")
    );

    // The state change a user triggers by switching the primary provider
    // from a Grok subscription to ChatGPT on the Models tab.
    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *primary_model = chatgpt;
    }
    sink.clear();
    let view_chatgpt = wizard_view_with_permission_target(&state, "", width, height);
    let frame_chatgpt = crate::cli::tui::plan_wizard_frame(&view_chatgpt, width, height);
    host.paint(&mut sink, &frame_chatgpt, width, height)
        .unwrap();
    terminal.feed(&sink);
    let incremental = terminal.rows();

    // The general invariant: an incrementally-blitted screen must always
    // equal an independent from-scratch repaint of the same final frame.
    let mut fresh_host = crate::cli::tui::WizardHost::new();
    let mut fresh_sink: Vec<u8> = Vec::new();
    fresh_host
        .paint(&mut fresh_sink, &frame_chatgpt, width, height)
        .unwrap();
    let mut fresh_terminal = MiniVt::new(width, height);
    fresh_terminal.feed(&fresh_sink);
    let expected = fresh_terminal.rows();

    assert_eq!(
        incremental,
        expected,
        "REGRESSION (#1305): switching the primary provider from Grok \
         subscription to ChatGPT must produce the same screen an \
         independent full repaint would, not a screen with a stale \
         leftover row from Grok's longer description or a duplicated \
         primary-provider row; incremental (as actually blitted):\n{}\n\
         expected (independent full repaint):\n{}",
        incremental.join("\n"),
        expected.join("\n")
    );
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

/// REGRESSION (#1298): the Settings tab's compact "GUI automation" row
/// derived its one-line summary with `.strip_prefix("Trust status: ")`, a
/// prefix `gui_automation_status_lines` never emits -- every real outcome
/// starts with "Configured; ..." or a plain disabled/unsupported sentence --
/// so the match could never succeed and the row always fell back to the
/// generic "GUI automation status unavailable" placeholder, whether the
/// process was trusted, untrusted, or in error. Confirmed live: a trusted
/// process (checkbox showing checked) still summarised as "unavailable" even
/// though the full detail view (D) showed the correct status.
///
/// The expected text for both the trusted and not-trusted case comes from
/// the real `gui_automation_status_lines` function, never a mock string, so
/// this pins the row to whatever that function actually says rather than to
/// a copy of its wording.
#[cfg(target_os = "macos")]
#[test]
fn test_gui_automation_settings_summary_shows_real_status_not_generic_placeholder() {
    let width = 140;
    let height = 40;

    for (label, availability_state) in [
        ("trusted", AutomationState::Available),
        ("not trusted", AutomationState::PermissionRequired),
    ] {
        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Features;
        let availability = if let Some(SectionState::Features {
            gui_automation,
            gui_automation_availability,
            gui_automation_prompt,
            gui_automation_prompted,
            gui_automation_last_known_available,
            selected_idx,
            ..
        }) = state.sections.get_mut(&WizardSection::Features)
        {
            *gui_automation = true;
            gui_automation_availability.state = availability_state;
            *gui_automation_prompt = AutomationPromptDisposition::NotNeeded;
            *gui_automation_prompted = false;
            *gui_automation_last_known_available = false;
            // Row 0 ("Live responses"), not the GUI automation row itself:
            // this exercises the compact list summary, not the D-key
            // expanded detail view (already covered elsewhere).
            *selected_idx = 0;
            gui_automation_availability.clone()
        } else {
            panic!("Features section must exist on a fresh WizardState");
        };

        let expected_summary = gui_automation_status_lines(
            true,
            &availability,
            AutomationPromptDisposition::NotNeeded,
            false,
            false,
            "",
            None,
        )
        .first()
        .expect("gui_automation_status_lines must always return at least one line")
        .plain_text();

        let rendered = wizard_text_with_permission_target(&state, "", width, height);
        assert!(
            rendered.contains(&expected_summary),
            "REGRESSION (#1298, {label}): the Settings tab's GUI automation row must show \
             the real status {expected_summary:?} that gui_automation_status_lines returns, \
             not a placeholder; rendered:\n{rendered}"
        );
        assert!(
            !rendered.contains("GUI automation status unavailable"),
            "REGRESSION (#1298, {label}): the generic placeholder must never show once a \
             real status is known; rendered:\n{rendered}"
        );
    }
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
/// Running the test suite must never open a browser. The Gemini add-time
/// flow called the real launcher (`gemini_auth::open_browser`) from its
/// background thread, so every run of
/// `confirming_a_gemini_sub_provider_runs_the_device_exchange_in_the_dialog`
/// — whose fixture authorization points at `https://www.google.com/device` —
/// opened Google's "Connect a device" page in the developer's browser.
/// Setup code launches a browser only through `open_browser_silently`, which
/// compiles to nothing under test.
#[test]
fn test_setup_wizard_launches_a_browser_only_through_the_test_inert_launcher() {
    let module_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli/setup_wizard");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&module_dir).expect("read setup_wizard directory") {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if !name.ends_with(".rs") || name == "tests.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read source file");
        for (number, line) in text.lines().enumerate() {
            let direct_launcher = line.contains("open_browser(");
            let raw_command = name != "input.rs"
                && (line.contains("Command::new(\"open\")") || line.contains("xdg-open"));
            if direct_launcher || raw_command {
                offenders.push(format!("{name}:{}: {}", number + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "setup wizard code must launch a browser only through `open_browser_silently`, \
         which is inert under test; these lines bypass it: {offenders:#?}"
    );

    let launcher = std::fs::read_to_string(module_dir.join("input.rs")).expect("read input.rs");
    let body = launcher
        .split("pub(super) fn open_browser_silently(url: &str) {")
        .nth(1)
        .expect("open_browser_silently must exist in input.rs");
    let guard = body.find("#[cfg(test)]");
    let launch = body.find("Command::new");
    assert!(
        matches!((guard, launch), (Some(guard), Some(launch)) if guard < launch),
        "open_browser_silently must return under #[cfg(test)] before it can spawn a process"
    );
}

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

/// REGRESSION (#1299): the Finish screen's "Ready to go!" summary only ever
/// checked `streaming` and `auto_approve` -- two of the Features section's
/// ~7 toggles -- so every other enabled setting silently never reached the
/// last review screen before saving, including the two with real
/// network/external effect (mDNS advertise, LAN peer discovery). Confirmed
/// live: with Debug logging, GUI automation, Advertise on network, and
/// Discover peers on LAN all showing checked on the Settings tab, the
/// Finish screen read only "Settings: Live responses".
#[test]
fn test_finish_screen_summary_includes_every_enabled_features_toggle() {
    let mut state = WizardState::new(None);
    if let Some(SectionState::Features {
        streaming,
        auto_approve,
        debug,
        #[cfg(target_os = "macos")]
        gui_automation,
        daemon_only_mode,
        mdns_discovery,
        auto_discover,
        ..
    }) = state.sections.get_mut(&WizardSection::Features)
    {
        *streaming = true;
        *auto_approve = false;
        *debug = true;
        #[cfg(target_os = "macos")]
        {
            *gui_automation = true;
        }
        *daemon_only_mode = false;
        *mdns_discovery = true;
        *auto_discover = true;
    }
    state.current_section = WizardSection::Review;

    let rendered = render_wizard_text_at(&state, 160, 30);
    for expected in [
        "Live responses",
        "Debug logging",
        "Advertise on network",
        "Discover peers on LAN",
    ] {
        assert!(
            rendered.contains(expected),
            "REGRESSION (#1299): the Finish screen summary must include every enabled \
             Features toggle, including {expected:?} (mDNS advertise and LAN peer \
             discovery are the two with real network effect); rendered:\n{rendered}"
        );
    }
    #[cfg(target_os = "macos")]
    assert!(
        rendered.contains("GUI automation"),
        "REGRESSION (#1299): GUI automation must appear on the Finish summary when \
         enabled; rendered:\n{rendered}"
    );
    assert!(
        !rendered.contains("Skip permission prompts"),
        "a toggle the user left off (auto_approve) must not appear in the summary; \
         rendered:\n{rendered}"
    );
}

/// A toggle that's off everywhere still summarises as "Defaults", not an
/// empty or malformed line.
#[test]
fn test_finish_screen_summary_falls_back_to_defaults_when_every_toggle_is_off() {
    let mut state = WizardState::new(None);
    if let Some(SectionState::Features {
        streaming,
        auto_approve,
        debug,
        #[cfg(target_os = "macos")]
        gui_automation,
        daemon_only_mode,
        mdns_discovery,
        auto_discover,
        ..
    }) = state.sections.get_mut(&WizardSection::Features)
    {
        *streaming = false;
        *auto_approve = false;
        *debug = false;
        #[cfg(target_os = "macos")]
        {
            *gui_automation = false;
        }
        *daemon_only_mode = false;
        *mdns_discovery = false;
        *auto_discover = false;
    }
    state.current_section = WizardSection::Review;

    let rendered = render_wizard_text_at(&state, 160, 30);
    let settings_row = rendered
        .lines()
        .find(|line| line.contains("Settings:"))
        .unwrap_or_else(|| panic!("no Settings row in rendered Finish screen:\n{rendered}"));
    assert!(
        settings_row.contains("Defaults"),
        "every toggle off must summarise as Defaults, not an empty line; \
         Settings row: {settings_row:?}"
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
        card_text.contains("Click to copy the verification code (CODE5678)")
            && card_text.contains(
                "Click to open the device sign-in page: https://auth.openai.com/activate"
            ),
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
        assert!(
            *id == "openai-compatible" || !models.is_empty(),
            "provider '{}' has no known models",
            id
        );
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
fn generic_openai_compatible_is_visible_and_opens_its_dedicated_editor() {
    let compatible_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "openai-compatible")
        .expect("generic compatible provider must be present in the setup registry");
    let mut state = state_with_step(AddProviderStep::SelectAddType {
        selected: compatible_idx,
    });

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

    assert!(
        matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureCompatibleConnection {
                editing_idx: None,
                ..
            })
        ),
        "selecting Generic OpenAI-compatible must open its endpoint/credential editor; step={:?}",
        get_step(&state)
    );
    let rendered = render_wizard_text_at(&state, 120, 35);
    assert!(
        rendered.contains("Generic OpenAI-compatible")
            || rendered.contains("Compatible Connection")
    );
    assert!(rendered.contains("Protocol compatibility does not attest"));
    assert!(!rendered.contains("sk-live"));
}

fn compatible_test_profile(name: &str, credential_ref: &str, base_url: &str) -> ProviderEntry {
    ProviderEntry::OpenAiCompatible {
        name: name.into(),
        base_url: base_url.into(),
        chat_path: Some("/chat/completions".into()),
        models_path: Some("/models".into()),
        model: "main".into(),
        credential: crate::config::CredentialBinding {
            credential_ref: credential_ref.into(),
            audience: None,
            tenant: None,
            project: None,
            account: None,
            required_scopes: Default::default(),
        },
        capabilities: Default::default(),
        tool_choice: Default::default(),
        strict_tool_schemas: None,
    }
}

fn compatible_test_credential(
    name: &str,
    secret_env: &str,
    base_url: &str,
) -> crate::config::ProviderCredential {
    crate::config::ProviderCredential {
        name: name.into(),
        kind: crate::config::CredentialKind::ApiKey,
        provider: crate::config::CredentialProvider::OpenaiCompatible,
        issuer: "openai-compatible".into(),
        audience: crate::config::required_audience(
            crate::config::CredentialProvider::OpenaiCompatible,
            Some(base_url),
        )
        .unwrap(),
        tenant: None,
        project: None,
        account: None,
        scopes: Default::default(),
        secret_ref: format!("env:{secret_env}"),
        lifecycle: Default::default(),
        revocation: Default::default(),
    }
}

#[test]
fn compatible_wizard_apply_reload_and_factory_resolution_preserve_attested_profile() {
    struct FixtureResolver;
    impl crate::config::CredentialResolver for FixtureResolver {
        fn resolve(
            &self,
            credential: &crate::config::ProviderCredential,
        ) -> Result<crate::config::ResolvedCredential> {
            Ok(crate::config::ResolvedCredential {
                credential_name: credential.name.clone(),
                secret: crate::config::ResolvedSecret::new("fixture-secret")?,
            })
        }
    }

    let mut state = state_with_step(AddProviderStep::ConfigureCompatibleConnection {
        draft: OpenAiCompatibleDraft {
            name: "ciru".into(),
            base_url: "https://dunamis.ciru.ai/v1".into(),
            chat_path: "/chat/completions".into(),
            models_path: "/models".into(),
            model: "main".into(),
            credential_ref: "ciru-key".into(),
            secret_env: "CIRU_API_KEY".into(),
            credential_kind: crate::config::CredentialKind::Bearer,
            streaming: Some(true),
            tools: Some(true),
            parallel_tool_calls: Some(false),
            image_input: Some(false),
            context_window_tokens: "262144".into(),
            max_output_tokens: "32768".into(),
            tool_choice: crate::config::OpenAiCompatibleToolChoice::Auto,
            strict_tool_schemas: Some(false),
            original_profile: None,
            original_credential: None,
        },
        focused_field: 0,
        editing_idx: None,
    });

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureCompatibleCapabilities { .. })
    ));
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(get_step(&state).is_none());

    let result = build_setup_result(&state).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    let metrics_dir = directory.path().join("metrics");
    let config = config_from_setup_result_with_paths(&result, metrics_dir.clone());
    config.save_to(&config_path).unwrap();
    let serialized = std::fs::read_to_string(&config_path).unwrap();
    assert!(serialized.contains("secret_ref = \"env:CIRU_API_KEY\""));
    assert!(!serialized.contains("fixture-secret"));

    let loaded = crate::config::load_config_from_path_with_paths(&config_path, metrics_dir)
        .expect("wizard output must reload through the production config loader");
    let ProviderEntry::OpenAiCompatible {
        name,
        model,
        capabilities,
        tool_choice,
        strict_tool_schemas,
        ..
    } = &loaded.providers[0]
    else {
        panic!(
            "wizard must persist a generic compatible provider: {:?}",
            loaded.providers
        );
    };
    assert_eq!(name, "ciru");
    assert_eq!(model, "main");
    assert_eq!(capabilities.streaming, Some(true));
    assert_eq!(capabilities.tools, Some(true));
    assert_eq!(capabilities.context_window_tokens, Some(262_144));
    assert_eq!(capabilities.max_output_tokens, Some(32_768));
    assert_eq!(
        *tool_choice,
        crate::config::OpenAiCompatibleToolChoice::Auto
    );
    assert_eq!(*strict_tool_schemas, Some(false));

    let provider = crate::providers::create_provider_profile_from_config_with_resolver(
        &loaded,
        "ciru",
        &FixtureResolver,
    )
    .expect("wizard profile must resolve through the production provider factory");
    assert_eq!(provider.name(), "ciru");
    assert!(provider.supports_streaming());
    assert!(provider.supports_tools());
    let resolved_capabilities = provider.capabilities("main");
    assert_eq!(
        resolved_capabilities.streaming.provenance,
        crate::providers::CapabilityProvenance::Configuration
    );
    assert_eq!(
        resolved_capabilities.tools.provenance,
        crate::providers::CapabilityProvenance::Configuration
    );
    assert_eq!(
        resolved_capabilities.context_window.provenance,
        crate::providers::CapabilityProvenance::Configuration
    );
    assert_eq!(
        resolved_capabilities.output_token_limit.provenance,
        crate::providers::CapabilityProvenance::Configuration
    );

    let mut reopened = WizardState::new(Some(&loaded));
    let Some(ModelConfig::Remote { persisted, .. }) = get_primary(&reopened) else {
        panic!("reopened wizard must retain the compatible provider row");
    };
    assert!(matches!(
        persisted,
        Some(ProviderEntry::OpenAiCompatible { .. })
    ));
    handle_models_input(&mut reopened, key(KeyCode::Enter)).unwrap();
    let Some(AddProviderStep::ConfigureCompatibleConnection { draft, .. }) = get_step(&reopened)
    else {
        panic!("editing a compatible row must reopen its dedicated editor");
    };
    assert_eq!(draft.base_url, "https://dunamis.ciru.ai/v1");
    assert_eq!(draft.secret_env, "CIRU_API_KEY");
    assert_eq!(draft.streaming, Some(true));
    assert_eq!(draft.strict_tool_schemas, Some(false));
    handle_models_input(&mut reopened, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut reopened, key(KeyCode::Enter)).unwrap();
    let reopened_result = build_setup_result(&reopened).unwrap();
    assert_eq!(reopened_result.providers, loaded.providers);
    assert_eq!(reopened_result.credentials, loaded.credentials());
}

#[test]
fn compatible_wizard_no_change_edit_preserves_restricted_credential_metadata() {
    let base_url = "https://compatible.example/v1";
    let mut profile = compatible_test_profile("restricted", "restricted-key", base_url);
    let mut credential = compatible_test_credential("restricted-key", "RESTRICTED_KEY", base_url);
    let audience = credential.audience.clone();
    let scopes: std::collections::BTreeSet<String> =
        ["models.read".to_string(), "tools.invoke".to_string()]
            .into_iter()
            .collect();
    if let ProviderEntry::OpenAiCompatible {
        credential: binding,
        ..
    } = &mut profile
    {
        binding.audience = Some(audience);
        binding.tenant = Some("tenant-a".into());
        binding.project = Some("project-a".into());
        binding.account = Some("account-a".into());
        binding.required_scopes = scopes.clone();
    }
    credential.tenant = Some("tenant-a".into());
    credential.project = Some("project-a".into());
    credential.account = Some("account-a".into());
    credential.scopes = scopes;
    credential.lifecycle = crate::config::CredentialLifecycle::Active {
        expires_at: Some("2099-01-01T00:00:00Z".parse().unwrap()),
        refreshable: true,
    };
    let config = crate::config::Config::with_providers(vec![profile.clone()])
        .with_credentials(vec![credential.clone()]);
    config.validate().unwrap();
    let mut state = WizardState::new(Some(&config));

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

    let result = build_setup_result(&state).unwrap();
    assert_eq!(
        result.providers,
        vec![profile.clone()],
        "a no-change wizard edit must preserve every profile-side credential constraint"
    );
    assert_eq!(
        result.credentials,
        vec![credential.clone()],
        "a no-change wizard edit must preserve account, scopes, lifecycle, and revocation state"
    );
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    let metrics_dir = directory.path().join("metrics");
    config_from_setup_result_with_paths(&result, metrics_dir.clone())
        .save_to(&config_path)
        .unwrap();
    let reloaded = crate::config::load_config_from_path_with_paths(&config_path, metrics_dir)
        .expect("restricted compatible metadata must survive production save and reload");
    assert_eq!(reloaded.providers, vec![profile]);
    assert_eq!(reloaded.credentials(), &[credential]);
}

#[test]
fn compatible_wizard_rejects_contradictory_tool_attestations_without_committing() {
    let draft = OpenAiCompatibleDraft {
        name: "hostile".into(),
        base_url: "https://compatible.example/v1".into(),
        model: "main".into(),
        credential_ref: "hostile-key".into(),
        secret_env: "HOSTILE_KEY".into(),
        tools: Some(false),
        tool_choice: crate::config::OpenAiCompatibleToolChoice::Auto,
        ..OpenAiCompatibleDraft::default()
    };
    let mut state = state_with_step(AddProviderStep::ConfigureCompatibleCapabilities {
        draft,
        focused_field: 0,
        editing_idx: None,
    });
    state.current_section = WizardSection::Models;

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureCompatibleCapabilities { .. })
    ));
    let rendered = render_wizard_text_at(&state, 120, 35);
    assert!(
        rendered.contains("tools unsupported") || rendered.contains("tool request fields"),
        "the wizard must explain the contradictory tool contract; rendered:\n{rendered}"
    );
}

#[test]
fn compatible_wizard_does_not_replace_a_same_named_foreign_credential() {
    let draft = OpenAiCompatibleDraft {
        name: "ciru".into(),
        base_url: "https://dunamis.ciru.ai/v1".into(),
        model: "main".into(),
        credential_ref: "shared-key".into(),
        secret_env: "CIRU_API_KEY".into(),
        ..OpenAiCompatibleDraft::default()
    };
    let mut state = state_with_step(AddProviderStep::ConfigureCompatibleCapabilities {
        draft,
        focused_field: 0,
        editing_idx: None,
    });
    state.current_section = WizardSection::Models;
    state.credentials = vec![crate::config::ProviderCredential {
        name: "shared-key".into(),
        kind: crate::config::CredentialKind::ApiKey,
        provider: crate::config::CredentialProvider::OpenaiPlatform,
        issuer: "openai-platform".into(),
        audience: crate::config::AudienceBinding::standard(
            crate::config::EndpointFamily::OpenaiPlatform,
        ),
        tenant: None,
        project: None,
        account: None,
        scopes: Default::default(),
        secret_ref: "env:OPENAI_API_KEY".into(),
        lifecycle: Default::default(),
        revocation: Default::default(),
    }];

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureCompatibleCapabilities { .. })
    ));
    assert_eq!(
        state.credentials[0].provider,
        crate::config::CredentialProvider::OpenaiPlatform,
        "a compatible-provider save must not replace a credential owned by another provider namespace"
    );
    let rendered = render_wizard_text_at(&state, 120, 35);
    assert!(
        rendered.contains("already in use by provider namespace 'openai_platform'"),
        "the wizard must identify the credential collision and its owning namespace; rendered:\n{rendered}"
    );
}

#[test]
fn compatible_wizard_rejects_an_occupied_same_namespace_credential_name() {
    let mut state = state_with_step(AddProviderStep::ConfigureCompatibleCapabilities {
        draft: OpenAiCompatibleDraft {
            name: "new-profile".into(),
            base_url: "https://compatible.example/v1".into(),
            model: "main".into(),
            credential_ref: "occupied-key".into(),
            secret_env: "NEW_SECRET".into(),
            ..OpenAiCompatibleDraft::default()
        },
        focused_field: 0,
        editing_idx: None,
    });
    state.current_section = WizardSection::Models;
    let occupied = compatible_test_credential(
        "occupied-key",
        "EXISTING_SECRET",
        "https://compatible.example/v1",
    );
    state.credentials = vec![occupied.clone()];

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureCompatibleCapabilities { .. })
    ));
    assert_eq!(state.credentials, vec![occupied]);
    let rendered = render_wizard_text_at(&state, 120, 35);
    assert!(
        rendered.contains("already in use by provider namespace 'openai_compatible'"),
        "same-namespace name occupancy must be treated as an ownership collision; rendered:\n{rendered}"
    );
}

#[test]
fn compatible_wizard_rejects_mutating_a_credential_shared_by_two_profiles() {
    let base_url = "https://compatible.example/v1";
    let credential = compatible_test_credential("shared-key", "SHARED_SECRET", base_url);
    let config = crate::config::Config::with_providers(vec![
        compatible_test_profile("primary-compatible", "shared-key", base_url),
        compatible_test_profile("tool-compatible", "shared-key", base_url),
    ])
    .with_credentials(vec![credential.clone()]);
    config.validate().unwrap();
    let mut state = WizardState::new(Some(&config));
    state.current_section = WizardSection::Models;
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    let Some(AddProviderStep::ConfigureCompatibleConnection { draft, .. }) = state
        .sections
        .get_mut(&WizardSection::Models)
        .and_then(|section| {
            if let SectionState::Models {
                adding_provider, ..
            } = section
            {
                adding_provider.as_mut()
            } else {
                None
            }
        })
    else {
        panic!("editing a compatible profile must open the connection editor");
    };
    draft.secret_env = "REPLACEMENT_SECRET".into();

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureCompatibleCapabilities { .. })
    ));
    assert_eq!(
        state.credentials,
        vec![credential],
        "editing one profile must not replace a credential still referenced by another profile"
    );
    let rendered = render_wizard_text_at(&state, 120, 35);
    assert!(
        rendered.contains("shared by 2 compatible profiles"),
        "the shared-dependent rejection must explain how to proceed; rendered:\n{rendered}"
    );
}

#[test]
fn compatible_connection_editor_rejects_invalid_endpoint_and_environment_inputs() {
    let cases = [
        (
            "scheme",
            "ftp://compatible.example/v1",
            "/chat/completions",
            "COMPATIBLE_KEY",
            "endpoint",
        ),
        (
            "cross-origin path",
            "https://compatible.example/v1",
            "https://attacker.example/chat/completions",
            "COMPATIBLE_KEY",
            "endpoint",
        ),
        (
            "environment name",
            "https://compatible.example/v1",
            "/chat/completions",
            "not-a-valid-env-name",
            "requires",
        ),
    ];
    for (case, base_url, chat_path, secret_env, diagnostic) in cases {
        let mut state = state_with_step(AddProviderStep::ConfigureCompatibleConnection {
            draft: OpenAiCompatibleDraft {
                name: "compatible".into(),
                base_url: base_url.into(),
                chat_path: chat_path.into(),
                model: "main".into(),
                credential_ref: "compatible-key".into(),
                secret_env: secret_env.into(),
                ..OpenAiCompatibleDraft::default()
            },
            focused_field: 0,
            editing_idx: None,
        });
        state.current_section = WizardSection::Models;

        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

        assert!(
            matches!(
                get_step(&state),
                Some(AddProviderStep::ConfigureCompatibleConnection { .. })
            ),
            "invalid {case} must remain in the connection editor; step={:?}",
            get_step(&state)
        );
        let rendered = render_wizard_text_at(&state, 120, 35);
        assert!(
            rendered.to_ascii_lowercase().contains(diagnostic),
            "invalid {case} must produce an actionable diagnostic; rendered:\n{rendered}"
        );
    }
}

#[test]
fn compatible_capability_editor_rejects_invalid_token_limits() {
    let cases = [
        ("zero", "0", "", "positive"),
        ("overflow", "4294967296", "", "whole number"),
        ("output above context", "10", "11", "context"),
    ];
    for (case, context_tokens, output_tokens, diagnostic) in cases {
        let mut state = state_with_step(AddProviderStep::ConfigureCompatibleCapabilities {
            draft: OpenAiCompatibleDraft {
                name: "compatible".into(),
                base_url: "https://compatible.example/v1".into(),
                model: "main".into(),
                credential_ref: "compatible-key".into(),
                secret_env: "COMPATIBLE_KEY".into(),
                context_window_tokens: context_tokens.into(),
                max_output_tokens: output_tokens.into(),
                ..OpenAiCompatibleDraft::default()
            },
            focused_field: 0,
            editing_idx: None,
        });
        state.current_section = WizardSection::Models;

        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

        assert!(
            matches!(
                get_step(&state),
                Some(AddProviderStep::ConfigureCompatibleCapabilities { .. })
            ),
            "invalid {case} token limits must remain in the capability editor; step={:?}",
            get_step(&state)
        );
        let rendered = render_wizard_text_at(&state, 120, 35);
        assert!(
            rendered.to_ascii_lowercase().contains(diagnostic),
            "invalid {case} token limits must produce an actionable diagnostic; rendered:\n{rendered}"
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
    assert_eq!(chatgpt.2, "gpt-6.1-sol");
    assert!(CLOUD_PROVIDERS
        .iter()
        .all(|(id, ..)| *id != "chatgpt_subscription"));

    let step = AddProviderStep::SelectAddType { selected: 0 };
    let rendered = render_card_text(
        add_provider_card(
            &step,
            &[],
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
        rendered.contains("Device sign-in starts after the wizard"),
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
        add_provider_card(step, &[], &CatalogSource::StaticFallback, false, None, None),
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

    assert!(label.contains("built-in list"), "{label}");
    assert!(label.contains(STATIC_FALLBACK_AS_OF), "{label}");
    assert!(label.contains("incomplete"), "{label}");
    assert!(label.contains("model name remains editable"), "{label}");
    assert!(!label.contains("bundled fallback snapshot"), "{label}");
    assert!(!label.contains("model ID"), "{label}");
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
            &step,
            &[],
            &CatalogSource::StaticFallback,
            false,
            Some(&misleading_runtime_time),
            None,
        ),
        180,
        50,
    );
    assert!(rendered.contains("built-in list"), "{rendered}");
    assert!(rendered.contains(STATIC_FALLBACK_AS_OF), "{rendered}");
    assert!(rendered.contains("incomplete"), "{rendered}");
    assert!(
        !rendered.contains("bundled fallback snapshot"),
        "{rendered}"
    );
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
    let config = config_from_setup_result_with_paths(&first_save, metrics_dir.clone());
    config.save_to(&config_path).unwrap();
    let loaded =
        crate::config::load_config_from_path_with_paths(&config_path, metrics_dir.clone()).unwrap();
    assert_eq!(loaded.metrics_dir, metrics_dir);
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
    config_from_setup_result_with_paths(&second_save, metrics_dir.clone())
        .save_to(&config_path)
        .unwrap();
    let reloaded =
        crate::config::load_config_from_path_with_paths(&config_path, metrics_dir.clone()).unwrap();
    assert_eq!(reloaded.metrics_dir, metrics_dir);
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
    assert!(rendered.contains("built-in list"), "{rendered}");
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
    // Auto is first in the list; right should cycle to CPU-only execution.
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
fn test_configure_local_enter_preserves_empty_cloud_primary_and_explains_requirement() {
    // A local-only graph cannot start the daemon, so the empty cloud slot stays in place.
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
    assert!(matches!(
        get_step(&state),
        Some(AddProviderStep::ConfigureLocal { .. })
    ));
    assert!(is_unconfigured_placeholder(get_primary(&state).unwrap()));
    let Some(SectionState::Models {
        error: Some(error), ..
    }) = state.sections.get(&WizardSection::Models)
    else {
        panic!("expected cloud fallback validation error");
    };
    assert!(error.contains("cloud provider"));
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
    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *primary_model = ModelConfig::Remote {
            provider: "claude".into(),
            name: "claude".into(),
            api_key: format!("sk-ant-{}", "x".repeat(100)),
            model: "claude-sonnet-4-6".into(),
            enabled: true,
            persisted: None,
        };
    }
    for character in path.to_string_lossy().chars() {
        handle_models_input(&mut state, key(KeyCode::Char(character))).unwrap();
    }
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(get_step(&state).is_none());
    let result = build_setup_result(&state).unwrap();
    assert!(matches!(&result.providers[1], ProviderEntry::Local {
        inference_provider: InferenceProvider::LlamaCpp,
        model_path: Some(saved),
        ..
    } if saved == &path));
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    let metrics_dir = directory.path().join("metrics");
    config_from_setup_result_with_paths(&result, metrics_dir.clone())
        .save_to(&config_path)
        .unwrap();
    let reloaded =
        crate::config::load_config_from_path_with_paths(&config_path, metrics_dir).unwrap();
    assert_eq!(reloaded.backend.model_path.as_deref(), Some(path.as_path()));
    let mut reopened = WizardState::new(Some(&reloaded));
    assert!(
        matches!(get_tool_models(&reopened).last(), Some(ModelConfig::Local {
        inference_provider: InferenceProvider::LlamaCpp,
        model_path: Some(saved),
        ..
    }) if saved == &path)
    );

    // Editing the reopened local row changes that row, not the provider count.
    if let Some(SectionState::Models { selected_idx, .. }) =
        reopened.sections.get_mut(&WizardSection::Models)
    {
        *selected_idx = 1;
    }
    handle_models_input(&mut reopened, key(KeyCode::Enter)).unwrap();
    assert!(
        matches!(get_step(&reopened), Some(AddProviderStep::ConfigureLocal {
            editing_idx: Some(1), model_path: shown, ..
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
    assert_eq!(edited.providers.len(), 2);
    assert!(matches!(&edited.providers[1], ProviderEntry::Local {
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
    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *primary_model = ModelConfig::Remote {
            provider: "claude".into(),
            name: "claude".into(),
            api_key: format!("sk-ant-{}", "x".repeat(100)),
            model: "claude-sonnet-4-6".into(),
            enabled: true,
            persisted: None,
        };
    }
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(get_step(&state).is_none());

    let expected = managed_gguf_artifact(
        ModelFamily::Qwen2,
        ModelSize::Medium,
        GgufQuantization::Q5KM,
    )
    .unwrap();
    let result = build_setup_result(&state).unwrap();
    assert!(matches!(&result.providers[1], ProviderEntry::Local {
        model_path: None,
        managed_artifact: Some(artifact),
        ..
    } if artifact == &expected));

    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.toml");
    let metrics_dir = directory.path().join("metrics");
    config_from_setup_result_with_paths(&result, metrics_dir.clone())
        .save_to(&config_path)
        .unwrap();
    let reloaded =
        crate::config::load_config_from_path_with_paths(&config_path, metrics_dir).unwrap();
    assert_eq!(reloaded.backend.model_path, None);
    assert_eq!(reloaded.backend.managed_artifact.as_ref(), Some(&expected));

    let reopened = WizardState::new(Some(&reloaded));
    assert!(
        matches!(get_tool_models(&reopened).last(), Some(ModelConfig::Local {
        model_path: None,
        managed_artifact: Some(artifact),
        ..
    }) if artifact == &expected)
    );
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
fn test_llama_managed_gguf_can_be_added_from_wizard() {
    let mut state = state_with_step(AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        family: ModelFamily::Llama3,
        size: ModelSize::Medium,
        quantization: GgufQuantization::Q4KM,
        execution: ExecutionTarget::Auto,
        model_path: String::new(),
        focused_field: 5,
        editing_idx: None,
    });
    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *primary_model = ModelConfig::Remote {
            provider: "claude".into(),
            name: "claude".into(),
            api_key: "sk-ant-test".into(),
            model: "claude-sonnet-4-6".into(),
            enabled: true,
            persisted: None,
        };
    }

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

    assert!(get_step(&state).is_none(), "supported Llama must be added");
    assert!(
        matches!(get_tool_models(&state).last(), Some(ModelConfig::Local {
        family: ModelFamily::Llama3,
        size: ModelSize::Medium,
        managed_artifact: Some(artifact),
        ..
    }) if artifact.filename == "Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf")
    );
}

#[test]
fn test_local_model_requires_a_configured_cloud_fallback() {
    let mut state = state_with_step(AddProviderStep::ConfigureLocal {
        inference_provider: InferenceProvider::LlamaCpp,
        family: ModelFamily::Llama3,
        size: ModelSize::Medium,
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
        panic!("expected cloud fallback validation error");
    };
    assert!(error.contains("cloud provider"));
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
    config_from_setup_result_with_paths(&result, metrics_dir.clone())
        .save_to(&config_path)
        .unwrap();

    let loaded =
        crate::config::load_config_from_path_with_paths(&config_path, metrics_dir).unwrap();
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

// ── Models: Shift+Up/Down reorder (#1255) ─────────────────────────────────

fn remote_model(label: &str) -> ModelConfig {
    ModelConfig::Remote {
        provider: "claude".into(),
        name: label.into(),
        api_key: String::new(),
        model: String::new(),
        enabled: true,
        persisted: None,
    }
}

fn state_with_models(primary: &str, tools: &[&str]) -> WizardState {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Models;
    if let Some(SectionState::Models {
        primary_model,
        tool_models,
        ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        *primary_model = remote_model(primary);
        *tool_models = tools.iter().map(|t| remote_model(t)).collect();
    }
    state
}

fn set_selected_idx(state: &mut WizardState, idx: usize) {
    if let Some(SectionState::Models { selected_idx, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *selected_idx = idx;
    }
}

fn selected_idx_of(state: &WizardState) -> usize {
    match state.sections.get(&WizardSection::Models) {
        Some(SectionState::Models { selected_idx, .. }) => *selected_idx,
        other => panic!("expected Models section state, got {other:?}"),
    }
}

fn model_names(state: &WizardState) -> (String, Vec<String>) {
    let name_of = |m: &ModelConfig| match m {
        ModelConfig::Remote { name, .. } => name.clone(),
        ModelConfig::Local { .. } => "local".to_string(),
    };
    match state.sections.get(&WizardSection::Models) {
        Some(SectionState::Models {
            primary_model,
            tool_models,
            ..
        }) => (
            name_of(primary_model),
            tool_models.iter().map(name_of).collect(),
        ),
        other => panic!("expected Models section state, got {other:?}"),
    }
}

#[test]
fn test_shift_down_swaps_two_adjacent_non_primary_tool_models() {
    let mut state = state_with_models("primary", &["a", "b", "c"]);
    set_selected_idx(&mut state, 1); // "a"

    handle_models_input(&mut state, modified_key(KeyCode::Down, KeyModifiers::SHIFT)).unwrap();

    let (primary, tools) = model_names(&state);
    assert_eq!(
        (primary.as_str(), tools.as_slice()),
        (
            "primary",
            ["b".to_string(), "a".to_string(), "c".to_string()].as_slice()
        ),
        "Shift+Down must swap the selected tool_model with its immediate lower \
         neighbor and leave the primary row untouched"
    );
    assert_eq!(
        selected_idx_of(&state),
        2,
        "the cursor should follow the moved entry to its new position"
    );
}

#[test]
fn test_shift_up_swaps_two_adjacent_non_primary_tool_models() {
    let mut state = state_with_models("primary", &["a", "b", "c"]);
    set_selected_idx(&mut state, 3); // "c"

    handle_models_input(&mut state, modified_key(KeyCode::Up, KeyModifiers::SHIFT)).unwrap();

    let (primary, tools) = model_names(&state);
    assert_eq!(
        (primary.as_str(), tools.as_slice()),
        (
            "primary",
            ["a".to_string(), "c".to_string(), "b".to_string()].as_slice()
        ),
        "Shift+Up must swap the selected tool_model with its immediate upper neighbor"
    );
    assert_eq!(selected_idx_of(&state), 2);
}

#[test]
fn test_shift_up_on_first_tool_model_promotes_it_to_primary() {
    // Edge-case decision for issue #1255: swapping the top tool_model further
    // up would move it into the primary slot. Rather than no-op there (which
    // would strand it one step below where Up otherwise walks it), this
    // performs the same swap P already does -- promote to primary -- so
    // repeated Shift+Up keeps moving the entry in one consistent direction.
    let mut state = state_with_models("primary", &["a", "b"]);
    set_selected_idx(&mut state, 1); // "a", the top tool_model

    handle_models_input(&mut state, modified_key(KeyCode::Up, KeyModifiers::SHIFT)).unwrap();

    let (primary, tools) = model_names(&state);
    assert_eq!(
        (primary.as_str(), tools.as_slice()),
        ("a", ["primary".to_string(), "b".to_string()].as_slice()),
        "Shift+Up on the top tool_model promotes it to primary, matching P's swap"
    );
    assert_eq!(
        selected_idx_of(&state),
        0,
        "the cursor should follow the promoted entry onto the primary row"
    );
}

#[test]
fn test_shift_down_on_last_tool_model_is_a_no_op() {
    let mut state = state_with_models("primary", &["a", "b", "c"]);
    set_selected_idx(&mut state, 3); // "c", last entry, no lower neighbor

    handle_models_input(&mut state, modified_key(KeyCode::Down, KeyModifiers::SHIFT)).unwrap();

    let (primary, tools) = model_names(&state);
    assert_eq!(
        (primary.as_str(), tools.as_slice()),
        (
            "primary",
            ["a".to_string(), "b".to_string(), "c".to_string()].as_slice()
        ),
        "Shift+Down on the last tool_model must not panic, reorder, or drop state"
    );
    assert_eq!(
        selected_idx_of(&state),
        3,
        "selection stays put when there is no neighbor below to swap with"
    );
}

#[test]
fn test_shift_up_and_down_on_primary_row_are_no_ops() {
    let mut state = state_with_models("primary", &["a", "b"]);
    // selected_idx defaults to 0, the primary row.
    assert_eq!(selected_idx_of(&state), 0);

    handle_models_input(&mut state, modified_key(KeyCode::Up, KeyModifiers::SHIFT)).unwrap();
    handle_models_input(&mut state, modified_key(KeyCode::Down, KeyModifiers::SHIFT)).unwrap();

    let (primary, tools) = model_names(&state);
    assert_eq!(
        (primary.as_str(), tools.as_slice()),
        ("primary", ["a".to_string(), "b".to_string()].as_slice()),
        "Shift+Up/Down on the primary row is not a supported reorder and must \
         leave state unchanged (use P to move something into the primary slot)"
    );
    assert_eq!(selected_idx_of(&state), 0);
}

#[test]
fn test_promote_key_still_swaps_selected_tool_model_into_primary() {
    // Regression guard: adding Shift+Up/Down must not disturb the pre-existing
    // P (promote) swap at src/cli/setup_wizard/input.rs handle_models_input.
    let mut state = state_with_models("primary", &["a", "b"]);
    set_selected_idx(&mut state, 2); // "b"

    handle_models_input(&mut state, key(KeyCode::Char('p'))).unwrap();

    let (primary, tools) = model_names(&state);
    assert_eq!(
        (primary.as_str(), tools.as_slice()),
        ("b", ["a".to_string(), "primary".to_string()].as_slice()),
        "P must still swap the selected tool_model directly into the primary slot"
    );
}

#[test]
fn test_add_and_enter_keys_still_open_overlays_in_models_section() {
    // Regression guard: adding Shift+Up/Down must not shadow A (add) or Enter
    // (edit) in navigation mode.
    let mut state = state_with_models("primary", &["a"]);

    handle_models_input(&mut state, key(KeyCode::Char('a'))).unwrap();
    assert!(
        matches!(
            get_step(&state),
            Some(AddProviderStep::SelectAddType { selected: 0 })
        ),
        "A must still open the add-provider overlay; step={:?}",
        get_step(&state)
    );
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        *adding_provider = None;
    }

    set_selected_idx(&mut state, 1); // "a", a remote provider
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(
        matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote {
                editing_idx: Some(1),
                ..
            })
        ),
        "Enter must still open the edit overlay for the selected provider; step={:?}",
        get_step(&state)
    );
}

// ── build_setup_result: inference_provider propagation ───────────────────

#[test]
fn test_build_setup_result_uses_inference_provider_from_local_model() {
    let mut state = WizardState::new(None);
    // Set primary to a local llama.cpp model with the daemon's required cloud fallback.
    if let Some(SectionState::Models {
        primary_model,
        tool_models,
        ..
    }) = state.sections.get_mut(&WizardSection::Models)
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
        tool_models.push(ModelConfig::Remote {
            provider: "claude".into(),
            name: "claude".into(),
            api_key: "sk-ant-test".into(),
            model: "claude-sonnet-4-6".into(),
            enabled: true,
            persisted: None,
        });
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
fn test_cloud_primary_keeps_local_qwen_as_tool_model_on_reopen() {
    use crate::config::{Config, ProviderEntry};

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
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Small,
            model_path: Some("/models/qwen-coder.gguf".into()),
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
            model_path: Some(ref path),
            name: Some(ref name),
            ..
        } if saved_execution_target == execution_target
            && path == &std::path::PathBuf::from("/models/qwen-coder.gguf")
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
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_path: Some(directory.path().join("models/qwen.gguf")),
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
    let source =
        crate::config::Config::with_providers_and_paths(providers.clone(), metrics_dir.clone())
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

    let opened = load_config_from_path_with_paths(&config_path, metrics_dir.clone())
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
            load_config_from_path_with_paths(&invocation_path, metrics_dir.clone()).unwrap();
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
    crate::config::Config::with_providers_and_paths(vec![provider.clone()], metrics_dir.clone())
        .with_credentials(vec![credential.clone()])
        .save_to(&path)
        .unwrap();
    let loaded = crate::config::load_config_from_path_with_paths(&path, metrics_dir)
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
    config_from_setup_result_with_paths(&result, directory.join("metrics"))
        .save_to(&path)
        .unwrap_or_else(|error| {
            panic!(
                "the provider graph must save to {} after the real reducer: {error:#}\n{:#?}",
                path.display(),
                result.providers
            )
        });
    crate::config::load_config_from_path_with_paths(&path, directory.join("metrics"))
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
                )
                .with_credentials(credentials.clone())
                .save_to(&original_path)
                .unwrap();
                let original_bytes = std::fs::read(&original_path).unwrap();
                let loaded =
                    crate::config::load_config_from_path_with_paths(&original_path, metrics_dir)
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
        (
            crate::config::CredentialProvider::GeminiSubscription,
            "gemini-sub",
        ),
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

    assert!(
        !provider_requires_inline_api_key("gemini-sub")
            && provider_requires_inline_api_key("gemini"),
        "Gemini subscription and Google AI Studio API-key auth must remain separate wizard choices"
    );
    let gemini_sub = CLOUD_PROVIDERS
        .iter()
        .find(|(id, _, _, _)| *id == "gemini-sub")
        .expect("gemini-sub wizard choice");
    let gemini_api = CLOUD_PROVIDERS
        .iter()
        .find(|(id, _, _, _)| *id == "gemini")
        .expect("gemini API-key wizard choice");
    assert!(
        gemini_sub.1.contains("subscription")
            && gemini_sub.3.contains("not an AI Studio API key")
            && gemini_api.1.contains("Gemini (Google)")
            && gemini_api.3.contains("aistudio.google.com"),
        "wizard copy must name Gemini subscription versus AI Studio key billing: sub={gemini_sub:?} api={gemini_api:?}"
    );

    // A Claude subscription is offered in setup as the Claude CLI bridge. The
    // browser sign-in (`claude-sub`) is kept in the code base with its tests
    // but is not a setup choice, so it has no editor here.
    assert!(
        CLOUD_PROVIDERS.iter().any(|(id, ..)| *id == "claude-cli")
            && !CLOUD_PROVIDERS.iter().any(|(id, ..)| *id == "claude-sub"),
        "setup must offer the Claude CLI bridge and not the browser sign-in; choices={:?}",
        CLOUD_PROVIDERS
            .iter()
            .map(|(id, ..)| *id)
            .collect::<Vec<_>>()
    );
    let cli_bridge = ProviderEntry::ClaudeCliBackend {
        model: None,
        binary: None,
        name: None,
    };
    assert_eq!(
        registered_editor_id(&cli_bridge),
        Some("claude-cli"),
        "a persisted Claude CLI bridge entry must select the claude-cli editor"
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
    let registered: std::collections::BTreeSet<_> = CLOUD_PROVIDERS
        .iter()
        .map(|(editor, ..)| *editor)
        // Neither is a credentialed provider: the generic endpoint has its
        // own entry type, and the Claude CLI bridge holds no credential in
        // Finch (the CLI keeps its own login). Both are asserted separately.
        .filter(|editor| !matches!(*editor, "openai-compatible" | "claude-cli"))
        .collect();
    assert_eq!(
        mapped, registered,
        "every registered cloud editor must have exactly one credentialed-provider identity mapping"
    );
    let compatible = ProviderEntry::OpenAiCompatible {
        name: "compatible".into(),
        base_url: "https://compatible.example/v1".into(),
        chat_path: Some("/chat/completions".into()),
        models_path: Some("/models".into()),
        model: "main".into(),
        credential: crate::config::CredentialBinding {
            credential_ref: "compatible-key".into(),
            audience: None,
            tenant: None,
            project: None,
            account: None,
            required_scopes: Default::default(),
        },
        capabilities: Default::default(),
        tool_choice: Default::default(),
        strict_tool_schemas: None,
    };
    assert_eq!(
        registered_editor_id(&compatible),
        Some("openai-compatible"),
        "generic compatible profiles use their dedicated editor rather than a built-in credentialed-provider mapping"
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
    )
    .with_credentials(vec![credential.clone()])
    .save_to(&original_path)
    .unwrap();
    let original =
        crate::config::load_config_from_path_with_paths(&original_path, metrics_dir.clone())
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
        crate::config::load_config_from_path_with_paths(&saved_path, metrics_dir).unwrap();
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
            }) if name == "chatgpt" && model == "gpt-6.1-sol"
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
        rendered.contains("Click to copy the verification code (CODE1234)")
            && rendered.contains(
                "Click to open the device sign-in page: https://auth.openai.com/activate"
            ),
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

struct ScriptedGeminiAddTimeAuthenticator {
    begin: std::sync::Mutex<
        std::collections::VecDeque<
            Result<crate::cli::gemini_auth::GeminiNamedCredentialStart, anyhow::Error>,
        >,
    >,
    finish: std::sync::Mutex<
        std::collections::VecDeque<
            Result<crate::cli::gemini_auth::EnsuredGeminiCredential, anyhow::Error>,
        >,
    >,
    begins: std::sync::atomic::AtomicUsize,
    finishes: std::sync::atomic::AtomicUsize,
}

impl ScriptedGeminiAddTimeAuthenticator {
    fn new(
        begin: impl IntoIterator<
            Item = Result<crate::cli::gemini_auth::GeminiNamedCredentialStart, anyhow::Error>,
        >,
        finish: impl IntoIterator<
            Item = Result<crate::cli::gemini_auth::EnsuredGeminiCredential, anyhow::Error>,
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
impl crate::cli::gemini_auth::GeminiCredentialAuthenticator for ScriptedGeminiAddTimeAuthenticator {
    async fn ensure_named_credential(
        &self,
        _reference: &str,
        _presentation: crate::cli::gemini_auth::DeviceLoginPresentation,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::cli::gemini_auth::EnsuredGeminiCredential> {
        anyhow::bail!("the add-time dialog must drive the phased ceremony, not the combined one")
    }

    async fn begin_named_credential(
        &self,
        _reference: &str,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::cli::gemini_auth::GeminiNamedCredentialStart> {
        self.begins
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.begin
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted begin outcome")
    }

    async fn finish_named_credential(
        &self,
        _reference: &str,
        _pending: &crate::oauth::DeviceAuthorization,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<crate::cli::gemini_auth::EnsuredGeminiCredential> {
        self.finishes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.finish
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted finish outcome")
    }
}

fn gemini_sub_provider_idx() -> usize {
    CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "gemini-sub")
        .unwrap()
}

fn gemini_setup_credential(reference: &str, account: &str) -> crate::config::ProviderCredential {
    crate::config::ProviderCredential {
        name: reference.into(),
        kind: crate::config::CredentialKind::OauthDevice,
        provider: crate::config::CredentialProvider::GeminiSubscription,
        issuer: "google-gemini".into(),
        audience: crate::config::AudienceBinding::standard(
            crate::config::EndpointFamily::GeminiSubscription,
        ),
        tenant: None,
        project: None,
        account: Some(account.into()),
        scopes: crate::providers::gemini_required_scopes(),
        secret_ref: format!("oauth-store:{reference}"),
        lifecycle: crate::config::CredentialLifecycle::Active {
            expires_at: Some(Utc::now() + chrono::TimeDelta::hours(1)),
            refreshable: true,
        },
        revocation: Default::default(),
    }
}

fn gemini_ensured_for(
    reference: &str,
    account: &str,
) -> crate::cli::gemini_auth::EnsuredGeminiCredential {
    crate::cli::gemini_auth::EnsuredGeminiCredential {
        credential: gemini_setup_credential(reference, account),
        compensation: Some(crate::cli::gemini_auth::GeminiCompensationHandle::issued(
            reference,
            "generation-1".into(),
        )),
    }
}

fn gemini_add_time_device_authorization(user_code: &str) -> crate::oauth::DeviceAuthorization {
    crate::oauth::DeviceAuthorization::issued(
        "device-code-secret".into(),
        user_code.into(),
        "https://www.google.com/device".into(),
        None,
        Duration::from_secs(600),
        Duration::from_secs(0),
    )
    .unwrap()
}

#[test]
fn confirming_a_gemini_sub_provider_runs_the_device_exchange_in_the_dialog() {
    let fake = Arc::new(ScriptedGeminiAddTimeAuthenticator::new(
        [Ok(
            crate::cli::gemini_auth::GeminiNamedCredentialStart::AuthorizationRequired(
                gemini_add_time_device_authorization("GEMINI-1234"),
            ),
        )],
        [Ok(gemini_ensured_for(
            "gemini-sub:default",
            "user@gmail.com",
        ))],
    ));
    let mut state = state_with_step(AddProviderStep::SelectAddType {
        selected: gemini_sub_provider_idx(),
    });
    state.current_section = WizardSection::Models;
    state.gemini_authenticator = Some(fake);

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(
        matches!(get_step(&state), Some(AddProviderStep::DeviceAuth { .. })),
        "confirming Gemini subscription must open the device dialog instead of adding the row silently; step={:?}",
        get_step(&state)
    );

    let presented = wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { pending, .. }) => pending.lock().unwrap().clone(),
            _ => None,
        },
        "the Gemini one-time code",
    );
    assert_eq!(presented.user_code, "GEMINI-1234");
    assert_eq!(presented.verification_uri, "https://www.google.com/device");
    wait_for(
        || match get_step(&state) {
            Some(AddProviderStep::DeviceAuth { outcome, .. }) => {
                outcome.lock().unwrap().is_some().then_some(())
            }
            _ => None,
        },
        "the Gemini terminal outcome",
    );

    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("Gemini subscription device sign-in")
            && rendered.contains("Signed in as user@gmail.com"),
        "the dialog must name Gemini subscription, not ChatGPT or Grok; rendered={rendered}"
    );

    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    let primary = get_primary(&state).expect("the models section must survive the Gemini ceremony");
    assert!(
        matches!(primary, ModelConfig::Remote { provider, .. } if provider == "gemini-sub"),
        "the gemini-sub lane must be added after a successful exchange; got {primary:?}"
    );
    assert_eq!(state.credentials.len(), 1);
    assert_eq!(state.credentials[0].name, "gemini-sub:default");
    assert_eq!(
        state.credentials[0].provider,
        crate::config::CredentialProvider::GeminiSubscription
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

/// REGRESSION: the Local Helpers memory-embeddings checkbox used a bare
/// white foreground with no background (`wizard_bold(_, Color::White)`)
/// instead of the #1140 selection style (`wizard_selected`, bold white on
/// black), so it was invisible on a light terminal theme -- the row painted
/// the same colour as the page background. Every other selected/highlighted
/// row in this file already used `wizard_selected`; this one didn't.
#[test]
fn test_local_helpers_memory_checkbox_carries_selection_contrast() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::LocalHelpers;
    let bytes = wizard_frame_bytes(&state, 100, 30);
    assert!(
        bytes.contains("Smart memory: enable enhanced search"),
        "the checkbox text itself must be present in the rendered frame: {bytes:?}"
    );
    let active_run = "\x1b[1;97;40m>>> ☑ Smart memory: enable enhanced search <<<";
    assert!(
        bytes.contains(active_run),
        "the memory-embeddings checkbox must paint bold bright-white on \
         black (the #1140 selection style), not a bare foreground colour \
         invisible on a light terminal; frame: {bytes:?}"
    );
}

#[test]
fn test_local_helpers_screen_avoids_technical_jargon() {
    let state = WizardState::new(None);
    let view_on = wizard_view_with_permission_target(&state, "", 100, 30);
    let frame_on = crate::cli::tui::plan_wizard_frame(&view_on, 100, 30)
        .lines
        .join("\n");

    let mut state_off = WizardState::new(None);
    if let Some(SectionState::LocalHelpers {
        use_neural_embeddings,
    }) = state_off.sections.get_mut(&WizardSection::LocalHelpers)
    {
        *use_neural_embeddings = false;
    }
    let view_off = wizard_view_with_permission_target(&state_off, "", 100, 30);
    let frame_off = crate::cli::tui::plan_wizard_frame(&view_off, 100, 30)
        .lines
        .join("\n");

    for frame in [&frame_on, &frame_off] {
        assert!(
            !frame.contains("embeddings"),
            "must avoid 'embeddings': {frame}"
        );
        assert!(
            !frame.contains("neural model"),
            "must avoid 'neural model': {frame}"
        );
        assert!(!frame.contains("GGUF"), "must avoid 'GGUF': {frame}");
        assert!(
            !frame.contains("llama.cpp"),
            "must avoid 'llama.cpp': {frame}"
        );
        assert!(!frame.contains("n-gram"), "must avoid 'n-gram': {frame}");
    }
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

/// REGRESSION (#1140 follow-up, "Context lines can't be edited"): the
/// previous spinner test drove `handle_features_input` directly, bypassing
/// the real key-dispatch entry point (`handle_wizard_key`) a keypress
/// actually goes through. That entry point treats bare Left/Right as a
/// global prev/next-section shortcut (arrow keys double for Tab/Shift+Tab)
/// and fired before the section ever saw the key, so on the real interactive
/// path pressing ◀/▶ on the context-lines row silently changed the wizard's
/// tab instead of the value — this is the production-boundary reproduction.
#[test]
fn test_context_lines_spinner_adjusts_through_real_key_dispatch_not_tab_switch() {
    let mut state = WizardState::new(None);
    // Reach the Features ("Settings") tab the way a user does: Tab from the
    // first tab, not by poking `current_section` directly.
    for _ in 0..WizardSection::all()
        .iter()
        .position(|s| *s == WizardSection::Features)
        .unwrap()
    {
        handle_wizard_key(&mut state, key(KeyCode::Tab)).unwrap();
    }
    assert_eq!(state.current_section, WizardSection::Features);

    // Reach the context-lines row with ↓, exactly as advertised in the
    // footer, then via the SAME dispatcher press ◀ to adjust it.
    for _ in 0..SETTINGS_CONTEXT_IDX {
        handle_wizard_key(&mut state, key(KeyCode::Down)).unwrap();
    }
    assert_eq!(
        features_context_lines(&state),
        4,
        "the default context-lines value starts at 4"
    );

    handle_wizard_key(&mut state, key(KeyCode::Left)).unwrap();
    assert_eq!(
        state.current_section,
        WizardSection::Features,
        "◀ on the context-lines row must adjust the spinner, not switch tabs \
         away from Settings"
    );
    assert_eq!(
        features_context_lines(&state),
        3,
        "◀ dispatched through handle_wizard_key must decrement the spinner, \
         the same as calling handle_features_input directly"
    );

    handle_wizard_key(&mut state, key(KeyCode::Right)).unwrap();
    handle_wizard_key(&mut state, key(KeyCode::Right)).unwrap();
    assert_eq!(
        state.current_section,
        WizardSection::Features,
        "▶ on the context-lines row must adjust the spinner, not switch tabs \
         away from Settings"
    );
    assert_eq!(
        features_context_lines(&state),
        5,
        "▶ dispatched through handle_wizard_key must increment the spinner"
    );

    // Off the spinner row, Left/Right still switch tabs (the shortcut is
    // real, just scoped to rows that don't claim the keys themselves).
    for _ in 0..SETTINGS_CONTEXT_IDX {
        handle_wizard_key(&mut state, key(KeyCode::Up)).unwrap();
    }
    handle_wizard_key(&mut state, key(KeyCode::Right)).unwrap();
    assert_eq!(
        state.current_section,
        WizardSection::Review,
        "Right must still act as the tab-navigation shortcut once the \
         selection has moved off the context-lines row"
    );
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

/// The frame the live wizard loop paints: planned, then re-coloured onto the
/// scheme of the theme currently selected (`driver.rs`).
fn themed_wizard_frame_bytes(state: &WizardState, width: usize, height: usize) -> String {
    let view = wizard_view_with_permission_target(state, "", width, height);
    let frame = crate::cli::tui::theme_wizard_frame(
        crate::cli::tui::plan_wizard_frame(&view, width, height),
        &state.selected_scheme(),
    );
    frame.lines.join("\n")
}

/// The Themes section's instruction must describe what the wizard really
/// does (#1300 established this rule for an earlier, wrong "white
/// background" claim). The setup screen is now painted in the theme under
/// the cursor, so the text says exactly that, and the frame proves it: the
/// selected row carries that theme's own highlight pair.
#[test]
fn test_theme_selector_help_text_matches_actual_selection_style() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Themes;
    let bytes = themed_wizard_frame_bytes(&state, 100, 30);

    let help_line = bytes
        .lines()
        .find(|line| line.contains("Press Enter to confirm."))
        .unwrap_or_else(|| panic!("the theme selector's confirm instruction must be in the rendered frame; frame:\n{bytes}"));
    assert!(
        help_line.contains("This whole screen previews the selected theme."),
        "the theme selector help text must describe the live preview; help line: {help_line:?}"
    );

    // WizardState::new(None) selects Light: its highlight is white (#ffffff)
    // on the accent blue (#0969da), bold.
    assert!(
        bytes.contains("\x1b[1;38;2;255;255;255;48;2;9;105;218m"),
        "the selected theme row must paint the selected theme's own highlight pair so the \
         help text is true; frame:\n{bytes}"
    );
}

/// The reported request: changing the theme selection must re-colour the
/// whole setup screen, not only a swatch. Every row of the frame opens on the
/// selected theme's canvas, the wizard's fixed named colours are gone, and
/// moving the selection changes the canvas.
#[test]
fn test_setup_screen_is_painted_in_the_selected_theme_and_follows_the_selection() {
    use crate::theme::ColorTheme;

    let themes = ColorTheme::all();
    let mut canvases = Vec::new();
    for (index, theme) in themes.iter().enumerate() {
        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Themes;
        state.sections.insert(
            WizardSection::Themes,
            SectionState::Themes {
                selected_theme: index,
            },
        );
        let scheme = state.selected_scheme();
        assert_eq!(
            scheme,
            theme.to_scheme(),
            "selecting {} must resolve to that preset",
            theme.name()
        );

        let view = wizard_view_with_permission_target(&state, "", 100, 30);
        let frame = crate::cli::tui::theme_wizard_frame(
            crate::cli::tui::plan_wizard_frame(&view, 100, 30),
            &scheme,
        );
        let canvas = frame
            .canvas
            .unwrap_or_else(|| panic!("{}: a themed frame must carry its canvas", theme.name()));
        let open = format!("{canvas:?}");
        for line in &frame.lines {
            assert!(
                line.starts_with("\x1b["),
                "{}: every row must open on the theme canvas; row={line:?}",
                theme.name()
            );
            // Light and Solarized define every role as RGB, so no ANSI-named
            // colour may survive in their frames. (Dark and High Contrast
            // legitimately use named colours for their own roles.)
            if matches!(theme, ColorTheme::Light | ColorTheme::Solarized) {
                for fixed in [
                    "\x1b[34m",
                    "\x1b[1;34m",
                    "\x1b[36m",
                    "\x1b[33m",
                    "\x1b[1;97;40m",
                    "\x1b[1;35;40m",
                ] {
                    assert!(
                        !line.contains(fixed),
                        "{}: the wizard's fixed colour {fixed:?} must be mapped onto the theme; row={line:?}",
                        theme.name()
                    );
                }
            }
        }
        canvases.push(open);
    }
    canvases.dedup();
    assert!(
        canvases.len() >= 3,
        "moving the theme selection must change the setup screen's canvas; canvases={canvases:?}"
    );
}

/// Saving setup must not erase colours the user overrode by hand in
/// `[colors]`: the wizard picks the theme, the overrides ride on top of it.
#[test]
fn test_setup_save_keeps_hand_written_color_overrides_over_the_chosen_theme() {
    use crate::theme::{ColorSpec, ColorTheme};

    let mut existing = crate::config::Config::with_providers(vec![chatgpt_subscription_provider()]);
    existing.active_theme = "dark".to_string();
    existing.colors = ColorTheme::Dark.to_scheme();
    existing.colors.messages.user = ColorSpec::Rgb(200, 0, 100);

    let mut state = WizardState::new(Some(&existing));
    let light = ColorTheme::all()
        .iter()
        .position(|theme| *theme == ColorTheme::Light)
        .unwrap();
    state.sections.insert(
        WizardSection::Themes,
        SectionState::Themes {
            selected_theme: light,
        },
    );
    let result = build_setup_result(&state).expect("a configured wizard must build a result");
    let saved = config_from_setup_result(&result);

    let mut expected = ColorTheme::Light.to_scheme();
    expected.messages.user = ColorSpec::Rgb(200, 0, 100);
    assert_eq!(saved.active_theme, "light");
    assert_eq!(
        saved.colors, expected,
        "switching Dark to Light in setup must give the light preset with the user's one \
         overridden colour kept"
    );
}

/// REGRESSION (#1300, finding 2): the Local Helpers tab used `[x]`/`[ ]`
/// bracket checkboxes while the Settings tab used a colourful `✅` emoji for
/// "on" and a plain `☐` box for "off" -- two different glyph families for
/// the two states of the same boolean-toggle concept, on top of a third
/// convention (`[x]`/`[ ]`) on a different tab again. The Models tab's own
/// per-tool checkbox already used `☑`/`☐`; every wizard checkbox now uses
/// that one plain-text convention, which (unlike an emoji) renders
/// consistently across terminal fonts.
#[test]
fn test_wizard_checkboxes_use_one_glyph_convention_across_tabs() {
    let mut local_helpers_state = WizardState::new(None);
    local_helpers_state.current_section = WizardSection::LocalHelpers;
    let local_helpers_frame = wizard_frame_bytes(&local_helpers_state, 100, 30);

    let mut features_state = WizardState::new(None);
    features_state.current_section = WizardSection::Features;
    let features_frame = wizard_frame_bytes(&features_state, 100, 30);

    for (name, frame) in [
        ("Local Helpers", &local_helpers_frame),
        ("Settings", &features_frame),
    ] {
        assert!(
            !frame.contains("[x]") && !frame.contains("[ ]"),
            "the {name} tab must not use bracket-style checkboxes now that \
             the wizard is unified on \u{2611}/\u{2610}; frame:\n{frame}"
        );
        assert!(
            !frame.contains('\u{2705}'),
            "the {name} tab must not use the \u{2705} emoji checkbox -- an \
             emoji glyph isn't guaranteed monospaced or available \
             everywhere, unlike the plain-text \u{2611}/\u{2610} pair; \
             frame:\n{frame}"
        );
        assert!(
            frame.contains('\u{2611}') || frame.contains('\u{2610}'),
            "the {name} tab must render at least one checkbox in the \
             unified \u{2611}/\u{2610} convention; frame:\n{frame}"
        );
    }
}

#[test]
fn test_wizard_save_validation_error_shows_card_and_prevents_exit() {
    let mut state = WizardState::new(None);
    // Set an invalid cloud provider (OpenAI with empty key)
    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        *primary_model = ModelConfig::Remote {
            provider: "openai".into(),
            name: "openai".into(),
            api_key: "".into(),
            model: "gpt-4".into(),
            enabled: true,
            persisted: None,
        };
    }

    // Try to save
    let result = handle_save_action(&mut state).unwrap();

    // It should return None because validation failed, and set the error
    assert!(result.is_none());
    assert!(state.save_error.is_some());
    let err = state.save_error.as_ref().unwrap();
    assert!(err.contains("key"), "expected API key error, got: {}", err);

    let rendered = render_wizard_text_at(&state, 100, 24);
    assert!(rendered.contains("Validation Error"));

    // Pressing Enter dismisses it
    let enter_event = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::empty(),
    );
    let action_dismiss = handle_wizard_key(&mut state, enter_event).unwrap();
    assert_eq!(action_dismiss, WizardAction::Continue);
    assert!(state.save_error.is_none());
}

#[test]
fn test_o_key_on_device_dialog_does_not_panic() {
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
                expires_in: std::time::Duration::from_secs(600),
            });
        }
    }

    // Pressing 'o' or 'O' must not panic and must be handled.
    handle_models_input(&mut state, key(KeyCode::Char('o'))).unwrap();
    handle_models_input(&mut state, key(KeyCode::Char('O'))).unwrap();
    // Pressing Enter must not panic and must be handled.
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
}

#[test]
fn test_pressing_e_or_enter_on_unconfigured_provider_focuses_api_key_field() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Models;

    // Press 'E' on the default unconfigured provider
    handle_models_input(&mut state, key(KeyCode::Char('E'))).unwrap();
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get(&WizardSection::Models)
    {
        match adding_provider {
            Some(AddProviderStep::ConfigureRemote { focused_field, .. }) => {
                assert_eq!(
                    *focused_field, 3,
                    "pressing 'E' must focus the API Key field (field 3)"
                );
            }
            other => panic!("expected ConfigureRemote step, got {other:?}"),
        }
    } else {
        panic!("missing Models section state");
    }

    // Dismiss overlay
    handle_models_input(&mut state, key(KeyCode::Esc)).unwrap();

    // Press Enter on the unconfigured provider
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get(&WizardSection::Models)
    {
        match adding_provider {
            Some(AddProviderStep::ConfigureRemote { focused_field, .. }) => {
                assert_eq!(
                    *focused_field, 3,
                    "pressing Enter on empty key must focus API Key field (field 3)"
                );
            }
            other => panic!("expected ConfigureRemote step, got {other:?}"),
        }
    } else {
        panic!("missing Models section state");
    }
}

#[test]
fn test_settings_screen_avoids_technical_jargon() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Features;
    let view = wizard_view_with_permission_target(&state, "", 160, 40);
    let frame = crate::cli::tui::plan_wizard_frame(&view, 160, 40)
        .lines
        .join("\n");

    assert!(
        !frame.contains("debug.log"),
        "must avoid 'debug.log': {frame}"
    );
    assert!(
        !frame.contains("HuggingFace"),
        "must avoid 'HuggingFace': {frame}"
    );
    assert!(
        !frame.contains("Daemon-only"),
        "must avoid 'Daemon-only': {frame}"
    );
    assert!(!frame.contains("REPL"), "must avoid 'REPL': {frame}");
}

#[test]
fn test_device_dialog_advertises_browser_open_controls() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Models;
    let pending = Arc::new(Mutex::new(Some(DeviceAuthPresentation {
        verification_uri: "https://auth.openai.com/activate".into(),
        user_code: "CODE-1234".into(),
        expires_in: Duration::from_secs(600),
    })));
    let outcome = Arc::new(Mutex::new(None));
    let cancel = tokio_util::sync::CancellationToken::new();

    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        *adding_provider = Some(AddProviderStep::DeviceAuth {
            provider_idx: 0,
            name: "test".into(),
            model: "test-model".into(),
            reference: "test:ref".into(),
            editing_idx: None,
            pending,
            outcome,
            cancel,
        });
    }

    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("O / Enter: Open & Copy Code | Esc: Cancel"),
        "the dialog must advertise browser open and cancellation keys; rendered={rendered}"
    );
}

/// The device sign-in card's two links are real click targets: a left press
/// on the "open" row opens the sign-in page, one on the "copy" row copies the
/// code, and a press anywhere else does nothing. The handler this replaces
/// treated a click anywhere on screen as "copy the code and open the browser".
#[test]
fn test_wizard_mouse_click_handles_device_auth_url() {
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Models;
    let pending = Arc::new(Mutex::new(Some(DeviceAuthPresentation {
        verification_uri: "https://example.com/oauth".into(),
        user_code: "AB-123".into(),
        expires_in: Duration::from_secs(300),
    })));
    let outcome = Arc::new(Mutex::new(None));
    let cancel = tokio_util::sync::CancellationToken::new();

    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        *adding_provider = Some(AddProviderStep::DeviceAuth {
            provider_idx: 0,
            name: "test".into(),
            model: "test-model".into(),
            reference: "test:ref".into(),
            editing_idx: None,
            pending,
            outcome,
            cancel,
        });
    }

    let (width, height) = (100usize, 30usize);
    let view = wizard_view_with_permission_target(&state, "", width, height);
    // The frame the live loop paints and hit-tests: planned, then themed.
    let frame = crate::cli::tui::theme_wizard_frame(
        crate::cli::tui::plan_wizard_frame(&view, width, height),
        &state.selected_scheme(),
    );
    let rows = frame.to_shadow_buffer(width, height).rows_as_text();
    let locate = |label: &str| {
        rows.iter()
            .enumerate()
            .find_map(|(row, text)| {
                text.find(label)
                    .map(|byte| (row, text[..byte].chars().count()))
            })
            .unwrap_or_else(|| panic!("{label:?} must be on screen; rows:\n{}", rows.join("\n")))
    };
    let click = |column: usize, row: usize, button| {
        handle_wizard_mouse(
            &frame,
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(button),
                column: column as u16,
                row: row as u16,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )
    };
    let left = crossterm::event::MouseButton::Left;

    let open_label = "Click to open the device sign-in page: https://example.com/oauth";
    let (open_row, open_col) = locate(open_label);
    let copy_label = "Click to copy the verification code (AB123)";
    let (copy_row, copy_col) = locate(copy_label);
    let context = format!("links={:?}\nrows:\n{}", frame.links, rows.join("\n"));

    for column in [open_col, open_col + open_label.chars().count() - 1] {
        assert_eq!(
            click(column, open_row, left),
            Some(WizardLinkAction::OpenUrl("https://example.com/oauth".into())),
            "a left press on the open-page link (column {column}) must open the sign-in page; {context}"
        );
    }
    for column in [copy_col, copy_col + copy_label.chars().count() - 1] {
        assert_eq!(
            click(column, copy_row, left),
            Some(WizardLinkAction::CopyText("AB123".into())),
            "a left press on the copy-code link (column {column}) must copy the code; {context}"
        );
    }
    for (column, row, why) in [
        (
            open_col + open_label.chars().count(),
            open_row,
            "one cell past the link's end",
        ),
        (
            open_col.saturating_sub(1),
            open_row,
            "one cell before the link's start",
        ),
        (open_col, open_row + 2, "a row that holds no link"),
        (0, 0, "the wizard's title border"),
    ] {
        if column == open_col && open_col == 0 {
            continue;
        }
        assert_eq!(
            click(column, row, left),
            None,
            "a press on {why} is not a link click; {context}"
        );
    }
    assert_eq!(
        click(open_col, open_row, crossterm::event::MouseButton::Right),
        None,
        "only the left button activates a link; {context}"
    );
}

/// The reported Gemini sign-in card: the address is a full OAuth authorize
/// URL that wrapped across five rows of plain text, none of it clickable.
/// A long address is not printed; one link opens it and another copies it.
#[test]
fn test_long_sign_in_address_is_two_links_not_wrapped_plain_text() {
    let address = "https://accounts.google.com/o/oauth2/v2/auth?access_type=offline&prompt=consent\
        &response_type=code&client_id=764086051850-6qr4p6gpi6hn506pt8ejuq83di341hur.apps.googleusercontent.com\
        &redirect_uri=http%3A%2F%2F127.0.0.1%3A62729%2Fcallback&scope=email+openid+profile&state=k5FQzOODWwyW5d\
        &code_challenge=bvzipFvoBMhFb_norHwXejtkLptz5OKbbqoKS8z7T0I&code_challenge_method=S256";
    let mut state = WizardState::new(None);
    state.current_section = WizardSection::Models;
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        *adding_provider = Some(AddProviderStep::DeviceAuth {
            provider_idx: 0,
            name: "gemini-sub".into(),
            model: "test-model".into(),
            reference: "test:ref".into(),
            editing_idx: None,
            pending: Arc::new(Mutex::new(Some(DeviceAuthPresentation {
                verification_uri: address.into(),
                user_code: String::new(),
                expires_in: Duration::from_secs(300),
            }))),
            outcome: Arc::new(Mutex::new(None)),
            cancel: tokio_util::sync::CancellationToken::new(),
        });
    }

    let (width, height) = (120usize, 30usize);
    let view = wizard_view_with_permission_target(&state, "", width, height);
    let frame = crate::cli::tui::plan_wizard_frame(&view, width, height);
    let rows = frame.to_shadow_buffer(width, height).rows_as_text();
    let screen = rows.join("\n");
    assert!(
        !screen.contains("accounts.google.com"),
        "a long sign-in address must not be printed across wrapped rows; screen:\n{screen}"
    );
    let click_on = |label: &str| {
        let (row, column) = rows
            .iter()
            .enumerate()
            .find_map(|(row, text)| {
                text.find(label)
                    .map(|byte| (row, text[..byte].chars().count()))
            })
            .unwrap_or_else(|| panic!("{label:?} must be on screen; screen:\n{screen}"));
        handle_wizard_mouse(
            &frame,
            crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: column as u16,
                row: row as u16,
                modifiers: crossterm::event::KeyModifiers::empty(),
            },
        )
    };
    assert_eq!(
        click_on("Click to open the device sign-in page"),
        Some(WizardLinkAction::OpenUrl(address.into())),
        "the open link must carry the whole address; links={:?}",
        frame.links
    );
    assert_eq!(
        click_on("Click to copy the sign-in address"),
        Some(WizardLinkAction::CopyText(address.into())),
        "the copy link must carry the whole address; links={:?}",
        frame.links
    );
}

// ── choosing a model from a visible list ──────────────────────────────────
//
// The provider form used to take a model identifier from memory: the built-in
// choices were reachable only by cycling ←→ blind, a ChatGPT subscription
// could not list its account's models at all, and an API-key provider listed
// them only after Ctrl+R. These tests drive the real key handler, the real
// background refresh and the real frame planner.

/// A scripted ChatGPT account listing: what the account offers, or why the
/// listing failed. Records how often it was asked and for which credential,
/// so a test can prove setup did not ask without a signed-in credential.
struct ScriptedChatGptAccountModels {
    outcome: Mutex<Result<Vec<String>, String>>,
    asked_for: Mutex<Vec<String>>,
}

impl ScriptedChatGptAccountModels {
    fn offering(models: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(Ok(models.iter().map(|model| model.to_string()).collect())),
            asked_for: Mutex::new(Vec::new()),
        })
    }

    fn failing(reason: &str) -> Arc<Self> {
        Arc::new(Self {
            outcome: Mutex::new(Err(reason.to_string())),
            asked_for: Mutex::new(Vec::new()),
        })
    }

    fn asked_for(&self) -> Vec<String> {
        self.asked_for.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl crate::providers::ChatGptAccountModels for ScriptedChatGptAccountModels {
    async fn account_models(
        &self,
        credential: &crate::config::ProviderCredential,
    ) -> anyhow::Result<Vec<String>> {
        self.asked_for.lock().unwrap().push(credential.name.clone());
        self.outcome
            .lock()
            .unwrap()
            .clone()
            .map_err(|reason| anyhow::anyhow!(reason))
    }
}

/// The named credential a completed ChatGPT sign-in leaves in the wizard:
/// metadata only, no token material.
fn signed_in_chatgpt_credential() -> crate::config::ProviderCredential {
    use crate::config::{
        AudienceBinding, CredentialKind, CredentialLifecycle, CredentialProvider, EndpointFamily,
    };
    crate::config::ProviderCredential {
        name: "chatgpt:default".into(),
        kind: CredentialKind::OauthDevice,
        provider: CredentialProvider::ChatgptSubscription,
        issuer: "openai-chatgpt".into(),
        audience: AudienceBinding::standard(EndpointFamily::ChatgptSubscription),
        tenant: None,
        project: None,
        account: Some("account-123".into()),
        scopes: crate::providers::chatgpt_required_scopes(),
        secret_ref: "oauth-store:chatgpt:default".into(),
        lifecycle: CredentialLifecycle::Active {
            expires_at: Some("2099-01-02T03:04:05Z".parse().unwrap()),
            refreshable: true,
        },
        revocation: Default::default(),
    }
}

/// A wizard on the Models tab that behaves like the live one: it lists models
/// on its own, and asks `source` for a ChatGPT account's list.
fn live_like_models_state(
    source: Arc<ScriptedChatGptAccountModels>,
    signed_in: bool,
) -> WizardState {
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
    );
    let mut state = WizardState::new_with_catalog_cache_dir(Some(&hermetic_config), None);
    state.current_section = WizardSection::Models;
    state.auto_catalog_refresh = true;
    state.chatgpt_account_models = Some(source as Arc<dyn crate::providers::ChatGptAccountModels>);
    if signed_in {
        state.credentials.push(signed_in_chatgpt_credential());
    }
    state
}

/// Open the add-provider overlay and choose its first entry, the ChatGPT
/// subscription, exactly as a user does: `a`, then Enter.
fn open_chatgpt_add_form(state: &mut WizardState) {
    assert_eq!(
        CLOUD_PROVIDERS[0].0, "chatgpt",
        "this helper relies on the ChatGPT subscription being the first add-provider choice"
    );
    handle_models_input(state, key(KeyCode::Char('a'))).unwrap();
    handle_models_input(state, key(KeyCode::Enter)).unwrap();
    assert!(
        matches!(
            get_step(state),
            Some(AddProviderStep::ConfigureRemote {
                provider_idx: 0,
                ..
            })
        ),
        "choosing the ChatGPT subscription must open its provider form; step={:?}",
        get_step(state)
    );
}

fn catalog_refresh_in_flight(state: &WizardState) -> bool {
    matches!(
        state.sections.get(&WizardSection::Models),
        Some(SectionState::Models {
            catalog_refresh: Some(_),
            ..
        })
    )
}

/// Let a started background refresh finish and be applied, as the run loop's
/// tick does. The bound is a liveness guard only: reaching it means the
/// refresh hung, not that it was slow.
fn settle_catalog_refresh(state: &mut WizardState) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while catalog_refresh_in_flight(state) {
        advance_catalog_refresh_if_done(state);
        assert!(
            std::time::Instant::now() < deadline,
            "the model list refresh never completed: the background refresh hung"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn form_model(state: &WizardState) -> String {
    match get_step(state) {
        Some(AddProviderStep::ConfigureRemote { model, .. }) => model.clone(),
        other => panic!("expected the provider form to be open; step={other:?}"),
    }
}

fn catalog_state_summary(state: &WizardState) -> String {
    match state.sections.get(&WizardSection::Models) {
        Some(SectionState::Models {
            catalog_models,
            catalog_source,
            catalog_error,
            catalog_refresh,
            ..
        }) => format!(
            "models={catalog_models:?} source={catalog_source:?} error={catalog_error:?} refreshing={}",
            catalog_refresh.is_some()
        ),
        _ => "no models section".to_string(),
    }
}

#[test]
fn test_chatgpt_form_lists_the_signed_in_accounts_models_without_a_keypress() {
    // The account offers the `gpt-5.6` alias and `gpt-6.1-sol`, which differs
    // from Finch's built-in pair (`gpt-5.6-sol`, `gpt-6.1-sol`).
    let source = ScriptedChatGptAccountModels::offering(&["gpt-5.6", "gpt-6.1-sol"]);
    let mut state = live_like_models_state(source.clone(), true);

    open_chatgpt_add_form(&mut state);
    assert!(
        catalog_refresh_in_flight(&state),
        "opening the form for a signed-in ChatGPT subscription must start listing the \
         account's models on its own, with no Ctrl+R; {}",
        catalog_state_summary(&state)
    );
    settle_catalog_refresh(&mut state);

    assert_eq!(
        source.asked_for(),
        vec!["chatgpt:default".to_string()],
        "the account must be asked exactly once, for the credential the profile is bound to"
    );
    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("provider discovery"),
        "the list must be labelled as fetched from the provider; {}\n{rendered}",
        catalog_state_summary(&state)
    );
    assert!(
        rendered.contains("Model choices (2)"),
        "the form must show the account's choices as a visible list; {}\n{rendered}",
        catalog_state_summary(&state)
    );
    assert!(
        rendered.contains("    gpt-5.6 ") && !rendered.contains("gpt-5.6-sol"),
        "the list must be the account's models, not Finch's built-in pair; {}\n{rendered}",
        catalog_state_summary(&state)
    );
    assert!(
        rendered.contains("→ gpt-6.1-sol  (selected)"),
        "the selected model must be marked in the list in words; {}\n{rendered}",
        catalog_state_summary(&state)
    );
    assert_eq!(
        form_model(&state),
        "gpt-6.1-sol",
        "a default model the account offers must stay selected instead of being replaced \
         by whichever identifier sorts first; {}",
        catalog_state_summary(&state)
    );
}

#[test]
fn test_chatgpt_form_without_a_sign_in_shows_the_builtin_list_and_says_why() {
    let source = ScriptedChatGptAccountModels::offering(&["gpt-6.1-sol"]);
    let mut state = live_like_models_state(source.clone(), false);

    open_chatgpt_add_form(&mut state);
    assert!(
        !catalog_refresh_in_flight(&state),
        "with no signed-in credential there is nothing to list with, so no refresh may \
         start; {}",
        catalog_state_summary(&state)
    );
    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("built-in list") && rendered.contains("incomplete"),
        "the fallback must be labelled as Finch's dated, incomplete built-in list; \n{rendered}"
    );
    assert!(
        rendered.contains("    gpt-5.6-sol") && rendered.contains("→ gpt-6.1-sol  (selected)"),
        "the built-in choices must be visible with the default marked; \n{rendered}"
    );
    assert!(
        rendered.contains("this account's own list needs a signed-in subscription"),
        "the form must say why it is not showing the account's own list; \n{rendered}"
    );
    assert!(
        !rendered.contains("Refresh warning"),
        "not being signed in yet is not a failure and must not be shown as one before the \
         user asks for a refresh; \n{rendered}"
    );

    handle_models_input(
        &mut state,
        modified_key(KeyCode::Char('r'), KeyModifiers::CONTROL),
    )
    .unwrap();
    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("Refresh warning: Sign in to ChatGPT first"),
        "Ctrl+R with no sign-in must say sign-in is what is missing; {}\n{rendered}",
        catalog_state_summary(&state)
    );
    assert!(
        source.asked_for().is_empty(),
        "setup must never ask for an account's models without a signed-in credential; \
         asked_for={:?}",
        source.asked_for()
    );
}

#[test]
fn test_chatgpt_listing_failure_falls_back_to_the_builtin_list_with_the_reason() {
    let source = ScriptedChatGptAccountModels::failing(
        "ChatGPT subscription model discovery failed (HTTP 503 Service Unavailable)",
    );
    let mut state = live_like_models_state(source.clone(), true);

    open_chatgpt_add_form(&mut state);
    settle_catalog_refresh(&mut state);

    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("Refresh warning:") && rendered.contains("HTTP 503"),
        "a failed listing must show its reason in the form; {}\n{rendered}",
        catalog_state_summary(&state)
    );
    assert!(
        rendered.contains("built-in list")
            && rendered.contains("incomplete")
            && !rendered.contains("provider discovery"),
        "after a failed listing the choices must be labelled as the built-in list, never as \
         fetched; {}\n{rendered}",
        catalog_state_summary(&state)
    );
    assert!(
        rendered.contains("    gpt-5.6-sol") && rendered.contains("→ gpt-6.1-sol  (selected)"),
        "the built-in choices must stay visible and selectable after a failed listing; \
         \n{rendered}"
    );

    // Moving around the form must not hammer a failing listing.
    handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    assert!(
        !catalog_refresh_in_flight(&state) && source.asked_for().len() == 1,
        "a failed listing must not be retried by navigation; asked_for={:?} {}",
        source.asked_for(),
        catalog_state_summary(&state)
    );

    // Ctrl+R is the retry.
    handle_models_input(
        &mut state,
        modified_key(KeyCode::Char('r'), KeyModifiers::CONTROL),
    )
    .unwrap();
    settle_catalog_refresh(&mut state);
    assert_eq!(
        source.asked_for().len(),
        2,
        "Ctrl+R must retry the listing; {}",
        catalog_state_summary(&state)
    );
}

#[test]
fn test_typed_model_missing_from_the_fetched_list_is_flagged_before_saving() {
    let source = ScriptedChatGptAccountModels::offering(&["gpt-5.6", "gpt-6.1-sol"]);
    let mut state = live_like_models_state(source, true);
    open_chatgpt_add_form(&mut state);
    settle_catalog_refresh(&mut state);

    // Name is focused when the form opens; Down reaches Model.
    handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
    for _ in 0.."gpt-6.1-sol".len() {
        handle_models_input(&mut state, key(KeyCode::Backspace)).unwrap();
    }
    for c in "gpt-9".chars() {
        handle_models_input(&mut state, key(KeyCode::Char(c))).unwrap();
    }
    assert_eq!(
        form_model(&state),
        "gpt-9",
        "typing a model identifier must stay possible when a list is shown"
    );
    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("'gpt-9' is not in this list"),
        "an identifier the fetched list does not contain must be flagged in the form, \
         not left to fail at the first query; {}\n{rendered}",
        catalog_state_summary(&state)
    );
    assert!(
        !rendered.contains("(selected)"),
        "no listed model may be marked selected while a different identifier is typed; \
         \n{rendered}"
    );

    // ←→ picks from the list, which clears the warning.
    handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
    let picked = form_model(&state);
    let rendered = render_wizard_text(&state);
    assert!(
        ["gpt-5.6", "gpt-6.1-sol"].contains(&picked.as_str()),
        "→ on the Model row must pick a listed model; picked={picked:?}"
    );
    assert!(
        rendered.contains(&format!("→ {picked}  (selected)")) && !rendered.contains("is not in"),
        "picking a listed model must mark it and clear the warning; picked={picked:?}\n{rendered}"
    );
}

#[test]
fn test_api_key_form_lists_models_once_the_key_is_entered_and_never_per_keystroke() {
    let mut server = mockito::Server::new();
    let listing = server
        .mock("GET", "/v1/models")
        .match_header("authorization", "Bearer sk-typed-0123456789")
        .with_status(200)
        .with_body(r#"{"data":[{"id":"gpt-b"},{"id":"gpt-a"}]}"#)
        .expect(1)
        .create();
    // Any request sent while the key was still being typed would land here.
    let partial_key = server
        .mock("GET", "/v1/models")
        .match_header(
            "authorization",
            mockito::Matcher::Regex("^Bearer .{0,18}$".into()),
        )
        .with_status(401)
        .expect(0)
        .create();
    let cache = tempfile::tempdir().unwrap();
    let config = crate::config::Config::with_providers_and_paths(
        vec![ProviderEntry::Openai {
            api_key: String::new(),
            model: None,
            base_url: Some(server.url()),
            chat_path: Some("/v1/chat/completions".into()),
            models_path: Some("/v1/models".into()),
            name: Some("openai-work".to_string()),
            reasoning_effort: None,
        }],
        std::path::PathBuf::from("unused-test-metrics"),
    );
    let mut state =
        WizardState::new_with_catalog_cache_dir(Some(&config), Some(cache.path().to_path_buf()));
    state.current_section = WizardSection::Models;
    state.auto_catalog_refresh = true;

    // Open the editor for the keyless OpenAI row: focus lands on API Key.
    handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
    assert!(
        matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote {
                focused_field: 3,
                ..
            })
        ),
        "a keyless API provider must open on its API Key row; step={:?}",
        get_step(&state)
    );
    assert!(
        !catalog_refresh_in_flight(&state),
        "with no key there is nothing to list with; {}",
        catalog_state_summary(&state)
    );
    for c in "sk-typed-0123456789".chars() {
        handle_models_input(&mut state, key(KeyCode::Char(c))).unwrap();
        assert!(
            !catalog_refresh_in_flight(&state),
            "typing the key must not send a request per keystroke (after {c:?}); {}",
            catalog_state_summary(&state)
        );
    }

    // Leaving the key for the Model row is when the form has what it needs.
    handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
    assert!(
        catalog_refresh_in_flight(&state),
        "reaching the Model row with a key entered must list models without Ctrl+R; {}",
        catalog_state_summary(&state)
    );
    settle_catalog_refresh(&mut state);

    let rendered = render_wizard_text(&state);
    assert!(
        rendered.contains("provider discovery") && rendered.contains("Model choices (2)"),
        "the key's models must be shown as a fetched, visible list; {}\n{rendered}",
        catalog_state_summary(&state)
    );
    assert!(
        rendered.contains("→ gpt-a  (selected)") && rendered.contains("    gpt-b"),
        "a blank model must be filled with a listed one and both choices shown; {}\n{rendered}",
        catalog_state_summary(&state)
    );
    assert!(
        !rendered.contains("sk-typed-0123456789"),
        "the form must never display the whole key; \n{rendered}"
    );
    listing.assert();
    partial_key.assert();
}

#[test]
fn test_model_choice_block_keeps_a_fixed_height_and_windows_a_long_list() {
    let many: Vec<String> = (0..100).map(|index| format!("model-{index:03}")).collect();
    let cases: Vec<(&str, Vec<String>, &str, CatalogSource)> = vec![
        ("empty", Vec::new(), "", CatalogSource::StaticFallback),
        (
            "two built-in",
            vec!["a".into(), "b".into()],
            "b",
            CatalogSource::StaticFallback,
        ),
        (
            "exactly a window",
            many[..MODEL_CHOICE_WINDOW].to_vec(),
            "model-003",
            CatalogSource::Discovered,
        ),
        (
            "one more than a window",
            many[..MODEL_CHOICE_WINDOW + 1].to_vec(),
            "model-006",
            CatalogSource::Discovered,
        ),
        (
            "long, selected deep",
            many.clone(),
            "model-057",
            CatalogSource::Discovered,
        ),
        (
            "long, selected last",
            many.clone(),
            "model-099",
            CatalogSource::Cache,
        ),
        (
            "long, typed id",
            many.clone(),
            "not-listed",
            CatalogSource::Discovered,
        ),
    ];
    for (name, models, current, source) in cases {
        let lines = model_choice_lines(&models, current, &source, false);
        let text: Vec<String> = lines.iter().map(|line| line.plain_text()).collect();
        assert_eq!(
            lines.len(),
            MODEL_CHOICE_ROWS,
            "the model list must occupy the same number of rows whatever the catalogue \
             holds, so nothing beneath it moves (case {name:?}); lines={text:#?}"
        );
        if models.iter().any(|model| model == current) {
            assert!(
                text.iter()
                    .any(|line| line == &format!("  → {current}  (selected)")),
                "the selected model must always be inside the visible window \
                 (case {name:?}); lines={text:#?}"
            );
        }
        if models.len() > MODEL_CHOICE_WINDOW {
            assert!(
                text[0].contains(&format!("of {}", models.len())),
                "a windowed list must say how many models there are in all \
                 (case {name:?}); lines={text:#?}"
            );
        }
    }

    // In the frame: a two-entry list and a hundred-entry list leave the
    // controls row on the same screen row.
    let openai_idx = CLOUD_PROVIDERS
        .iter()
        .position(|(id, ..)| *id == "openai")
        .unwrap();
    let controls_row = |models: &[String], model: &str| {
        let step = AddProviderStep::ConfigureRemote {
            provider_idx: openai_idx,
            name: "openai-work".to_string(),
            model: model.to_string(),
            api_key: Some("openai-key".to_string()),
            focused_field: 2,
            editing_idx: None,
        };
        let rendered = render_card_text(
            add_provider_card(&step, models, &CatalogSource::Discovered, false, None, None),
            120,
            40,
        );
        rendered
            .lines()
            .position(|row| row.contains("Enter adds"))
            .unwrap_or_else(|| panic!("the controls row must be on screen:\n{rendered}"))
    };
    let short = controls_row(&["a".to_string(), "b".to_string()], "a");
    let long = controls_row(&many, "model-057");
    assert_eq!(
        short, long,
        "the form's controls row must not move when the model list grows from 2 to 100 \
         entries (row with 2 entries, row with 100 entries)"
    );
}

/// Which setup list a #1651 walk drives.
#[derive(Clone, Copy, Debug)]
enum ScrolledSetupList {
    Themes,
    AddProvider,
}

/// One painted step of a list walk: what was selected, the screen the real
/// incremental blit left, and an independent from-scratch repaint of the
/// same frame.
struct ListWalkStep {
    label: String,
    selected: String,
    incremental: Vec<String>,
    fresh: Vec<String>,
}

/// Open `list` through the real key handling, walk the selection down to the
/// last entry and back up to the first, and paint every step through one
/// `WizardHost` into a `MiniVt` (the real frame builder and row-diff blit).
fn walk_setup_list(list: ScrolledSetupList, width: usize, height: usize) -> Vec<ListWalkStep> {
    let entries: Vec<String> = match list {
        ScrolledSetupList::Themes => crate::theme::ColorTheme::all()
            .iter()
            .map(|theme| format!(">>> {} - ", theme.name()))
            .collect(),
        ScrolledSetupList::AddProvider => CLOUD_PROVIDERS
            .iter()
            .map(|(_, display_name, _, _)| format!(">>> {display_name} <<<"))
            .chain([
                ">>> Local model <<<".to_string(),
                ">>> Scan local network <<<".to_string(),
            ])
            .collect(),
    };

    let mut state = WizardState::new(None);
    assert_eq!(state.current_section, WizardSection::Themes);
    if matches!(list, ScrolledSetupList::AddProvider) {
        handle_wizard_key(&mut state, key(KeyCode::Tab)).unwrap();
        assert_eq!(state.current_section, WizardSection::Models);
        handle_wizard_key(&mut state, key(KeyCode::Char('a'))).unwrap();
    }
    // The theme list opens on the theme the environment suggests; start the
    // walk from the first entry whatever that was.
    for _ in &entries {
        handle_wizard_key(&mut state, key(KeyCode::Up)).unwrap();
    }

    let mut host = crate::cli::tui::WizardHost::new();
    let mut terminal = MiniVt::new(width, height);
    let mut steps = Vec::new();
    let mut paint = |state: &WizardState, label: String, selected: &str| {
        let view = wizard_view_with_permission_target(state, "", width, height);
        let frame = crate::cli::tui::plan_wizard_frame(&view, width, height);
        let mut sink: Vec<u8> = Vec::new();
        host.paint(&mut sink, &frame, width, height).unwrap();
        terminal.feed(&sink);

        let mut fresh_sink: Vec<u8> = Vec::new();
        crate::cli::tui::WizardHost::new()
            .paint(&mut fresh_sink, &frame, width, height)
            .unwrap();
        let mut fresh_terminal = MiniVt::new(width, height);
        fresh_terminal.feed(&fresh_sink);
        steps.push(ListWalkStep {
            label,
            selected: selected.to_string(),
            incremental: terminal.rows(),
            fresh: fresh_terminal.rows(),
        });
    };

    paint(&state, format!("{list:?} opened"), &entries[0]);
    for (index, entry) in entries.iter().enumerate().skip(1) {
        handle_wizard_key(&mut state, key(KeyCode::Down)).unwrap();
        paint(&state, format!("{list:?} after Down x{index}"), entry);
    }
    for (index, entry) in entries.iter().enumerate().rev().skip(1) {
        handle_wizard_key(&mut state, key(KeyCode::Up)).unwrap();
        paint(&state, format!("{list:?} back Up to entry {index}"), entry);
    }
    steps
}

/// REGRESSION (#1651, setup lists do not scroll): at 80x24 and 60x15 the Add
/// AI Provider list and the theme list were cut at the window bottom, so the
/// selection moved onto rows that were never drawn and the only hint was
/// "more lines — resize window". Every entry, the last included, must be on
/// screen with its `>>>` marker while it is selected, walking down and back
/// up through the real key handling.
#[test]
fn test_setup_lists_scroll_to_keep_the_selected_row_on_screen() {
    for (width, height) in [(80usize, 24usize), (60, 15)] {
        for list in [ScrolledSetupList::Themes, ScrolledSetupList::AddProvider] {
            let steps = walk_setup_list(list, width, height);
            for step in &steps {
                let screen = step.incremental.join("\n");
                assert!(
                    step.incremental
                        .iter()
                        .any(|row| row.contains(&step.selected)),
                    "the selected setup-list entry and its marker must be drawn \
                     ({label}, {width}x{height}): expected a row containing \
                     {selected:?}; screen:\n{screen}",
                    label = step.label,
                    selected = step.selected,
                );
            }
            for step in &steps {
                let screen = step.incremental.join("\n");
                assert!(
                    !screen.contains("resize window"),
                    "a list that scrolls must not tell the user to resize the \
                     window ({label}, {width}x{height}); screen:\n{screen}",
                    label = step.label,
                );
            }
            if matches!(list, ScrolledSetupList::AddProvider) {
                let last = &steps[steps.len() / 2];
                let screen = last.incremental.join("\n");
                assert!(
                    last.selected.contains("Scan local network")
                        && screen.contains("more lines above")
                        && screen.contains("Esc: Cancel"),
                    "with the last provider entry selected the card must say in \
                     words that entries are hidden above and keep its controls \
                     row ({label}, {width}x{height}); screen:\n{screen}",
                    label = last.label,
                );
                let first = &steps[0];
                assert!(
                    first.incremental.join("\n").contains("more lines below"),
                    "with the first provider entry selected the card must say in \
                     words that entries are hidden below ({width}x{height}); \
                     screen:\n{}",
                    first.incremental.join("\n")
                );
            }
        }
    }
}

/// INVARIANT (#1651, setup lists do not scroll; the wizard's row-diff blit):
/// when the visible window of a scrolling setup list changes between two
/// frames, the incrementally painted screen must equal a from-scratch repaint
/// of the same frame at every step, so no row of the previous window is left
/// behind.
#[test]
fn test_setup_list_scrolling_leaves_no_stale_row_between_frames() {
    for (width, height) in [(80usize, 24usize), (60, 15)] {
        for list in [ScrolledSetupList::Themes, ScrolledSetupList::AddProvider] {
            let steps = walk_setup_list(list, width, height);
            let distinct: std::collections::HashSet<&Vec<String>> =
                steps.iter().map(|step| &step.fresh).collect();
            assert!(
                distinct.len() > 1,
                "sanity check: the walk must change the screen or this test \
                 proves nothing ({list:?}, {width}x{height}); screen:\n{}",
                steps[0].fresh.join("\n")
            );
            for step in &steps {
                assert_eq!(
                    step.incremental,
                    step.fresh,
                    "scrolling a setup list must leave the same screen an \
                     independent full repaint would ({label}, {width}x{height}); \
                     incremental (as actually blitted):\n{incremental}\n\
                     expected (independent full repaint):\n{fresh}",
                    label = step.label,
                    incremental = step.incremental.join("\n"),
                    fresh = step.fresh.join("\n"),
                );
            }
        }
    }
}
