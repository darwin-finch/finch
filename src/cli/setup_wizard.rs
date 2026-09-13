//! First-run configuration wizard: the tabbed UI the user sees on first launch,
//! on `finch setup`, and on `/setup`.
//!
//! This file is the module's facade and entry point. The wizard itself lives in sibling
//! files declared below — Rust treats them as the same module, so `use super::*` reaches
//! private items across all of them:
//!
//! | file | what it is |
//! |------|------------|
//! | [`catalog`] | what Finch knows about providers and models before the user picks one |
//! | [`state`] | the wizard's own state: sections, selections, and the `SetupResult` it produces |
//! | [`driver`] | the run loop: tick polling, overlay state, top-level key dispatch |
//! | [`input`] | key handling, one function per wizard section |
//! | [`render`] | drawing, one function per wizard section and overlay |
//! | [`apply`] | from wizard state to a saved `crate::config::Config` |
//! | [`chatgpt_recovery`] | the ChatGPT credential ceremony and its recovery loop |

mod apply;
mod catalog;
mod chatgpt_recovery;
mod driver;
mod input;
mod render;
mod state;

#[cfg(test)]
mod tests;

use crate::config::{CoreMlConfig, ExecutionTarget, ProviderEntry, TeacherEntry};
use crate::models::compatibility;
use crate::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};
use crate::providers::endpoints::ProviderEndpoints;
use crate::providers::model_catalog::{
    self, CatalogAuth, CatalogSource, ModelCatalog, ModelCatalogProfile,
};
use crate::service::discovery_client::{DiscoveredService, ServiceDiscoveryClient};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    Frame,
};
use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(target_os = "macos")]
use crate::runtime::automation::{
    permission_context_key, permission_target_description, AutomationAvailability,
    AutomationBroker, AutomationPermissionResult, AutomationPromptContext,
    AutomationPromptDisposition, AutomationState,
};

use apply::*;
use catalog::*;
use driver::*;
use input::*;
use render::*;
use state::*;

// The module's public surface. Everything else in the wizard is `pub(super)` and
// reaches no further than these seven files.
pub use apply::{
    apply_daemon_api_key, validate_and_apply, validate_command_and_apply,
    validate_first_run_and_apply, validate_repl_and_apply,
};
pub use catalog::ModelConfig;
pub use chatgpt_recovery::validate_and_apply_for;
pub use state::SetupResult;

/// Restore the terminal to normal state after the wizard exits.
fn cleanup_terminal(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let raw_result = crossterm::terminal::disable_raw_mode();
    let screen_result = crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    );
    let cursor_result = terminal.show_cursor();
    raw_result?;
    screen_result?;
    cursor_result?;
    Ok(())
}

/// Show first-run setup wizard and return configuration
pub fn show_setup_wizard() -> Result<SetupResult> {
    // `None` is reserved for a genuinely absent file. Existing configuration
    // failures must stop setup before it can render or save an empty fallback.
    let existing_config = crate::config::load_persisted_config().context(
        "Existing Finch configuration could not be loaded; setup was not opened because saving an empty wizard would overwrite it",
    )?;
    if let Some(config) = existing_config.as_ref() {
        tracing::debug!(
            providers = config.providers.len(),
            credentials = config.credentials().len(),
            "Successfully loaded existing setup configuration"
        );
    }

    // Set up terminal
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    // Run the NEW tabbed wizard
    let result = run_tabbed_wizard(&mut terminal, existing_config.as_ref());

    // ALWAYS restore terminal, even if wizard was cancelled or errored
    // Prioritize cleanup to ensure terminal is always restored
    cleanup_terminal(&mut terminal)?;

    // Return the wizard result after cleanup is guaranteed
    result
}

/// Public entry point used by `/setup` command — runs the wizard and returns
/// `Some(result)` on completion or `None` if the user cancelled.
pub fn run_setup_wizard() -> Result<Option<SetupResult>> {
    match show_setup_wizard() {
        Ok(result) => Ok(Some(result)),
        Err(e) if e.to_string().contains("Setup cancelled") => Ok(None),
        Err(e) => Err(e),
    }
}

/// Apply a `SetupResult` to a new `Config` and save it to disk.
///
/// Used both by `main.rs` (first-run) and by the `/setup` REPL command.
pub fn apply_and_save(result: &SetupResult) -> Result<()> {
    config_from_setup_result(result).save()?;
    if let Some(prompt) = result.custom_system_prompt.as_deref() {
        crate::config::Persona::save_system_prompt_override(&result.default_persona, prompt)?;
    }
    Ok(())
}

/// Result of the shared first-run, `finch setup`, and `/setup` commit ceremony.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupApplyOutcome {
    /// Authentication and model validation succeeded and configuration was saved.
    Saved,
    /// The user explicitly chose not to save the wizard changes.
    Cancelled,
}

/// Entry point invoking the shared post-wizard authentication and commit ceremony.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupInvocation {
    /// Automatic setup because no Finch configuration exists yet.
    FirstRun,
    /// Explicit `finch setup` command.
    Command,
    /// In-session `/setup` command.
    Repl,
}
