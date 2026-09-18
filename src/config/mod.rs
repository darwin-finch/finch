// Configuration module
// Public interface for configuration loading

mod atomic_write;
mod backend;
mod constants;
mod loader;
mod notice_state;

/// Decide whether the licence notice is due, recording the decision in the
/// runtime-state file rather than in `config.toml` (#76).
///
/// `legacy_suppress_until` is the value that used to live in the config. It is
/// still honoured so an existing installation's suppression survives, and is
/// never written back.
pub fn claim_notice_showing_now(
    legacy_suppress_until: Option<&str>,
    today: chrono::NaiveDate,
) -> bool {
    let Ok(path) = notice_state::notice_state_path() else {
        // No home directory: show the notice rather than guess, and still
        // write nothing.
        return true;
    };
    // Delegate to the injectable form rather than duplicating the decision.
    // These were two parallel paths, and the regression test exercised the one
    // production did not call, so restoring the real defect left every test
    // green (#329 review).
    notice_state::claim_notice_showing_for_paths(&path, legacy_suppress_until, today)
}

/// Forget any recorded notice suppression, so the next start shows it.
///
/// Called when a licence is removed: that used to un-suppress the notice as a
/// side effect of writing `notice_suppress_until: None` to the config, and the
/// move to a state file silently stopped it working (#329 review).
pub fn forget_notice_suppression() {
    if let Ok(path) = notice_state::notice_state_path() {
        notice_state::forget_recorded_suppression(&path);
    }
}

mod persona;
mod provider;
mod settings;

#[allow(deprecated)]
pub use backend::BackendDevice; // Deprecated alias for ExecutionTarget
pub use backend::{BackendConfig, CoreMlComputeUnits, CoreMlConfig, ExecutionTarget};
// Colours are a rendering vocabulary, not a configuration one: `crate::theme` defines what a
// scheme is and this module's job is turning a config file into one. Re-exported so callers that
// think of it as configuration keep working.
pub use crate::theme::{
    ColorScheme, ColorSpec, ColorTheme, DialogColors, MessageBand, MessageColors, StatusColors,
    UiColors,
};
pub use constants::{
    DEFAULT_BRAIN_TLS_PORT, DEFAULT_DAEMON_ADDR, DEFAULT_HTTP_ADDR, DEFAULT_MAX_TOKENS,
    DEFAULT_WORKER_ADDR,
};
pub use finch_providers::{
    credential_dependencies, credential_index, normalize_origin, required_audience,
    validate_authenticated_endpoints, validate_binding, AudienceBinding, CredentialBinding,
    CredentialKind, CredentialLifecycle, CredentialProvider, CredentialResolver, EndpointFamily,
    EnvironmentCredentialResolver, LifecycleRevocation, ProviderCredential, ReasoningEffort,
    ResolvedCredential, ResolvedSecret, DEFAULT_CLAUDE_MODEL,
};
pub use loader::{load_config, load_persisted_config};
#[cfg(test)]
pub(crate) use loader::{load_config_from_path, load_config_from_path_with_paths};
pub use persona::Persona;
pub use provider::ProviderEntry;
pub use settings::{
    ClientConfig, Config, FeaturesConfig, LicenseConfig, LicenseType, ServerConfig, TeacherEntry,
};
