// Configuration module
// Public interface for configuration loading

mod backend;
mod colors;
pub mod constants;
pub mod credential;
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
    notice_state::claim_notice_showing(
        &path,
        legacy_suppress_until,
        today,
        chrono::Duration::days(7),
    )
}
pub mod persona;
pub mod provider;
mod settings;

#[allow(deprecated)]
pub use backend::BackendDevice; // Deprecated alias for ExecutionTarget
pub use backend::{BackendConfig, CoreMlComputeUnits, CoreMlConfig, ExecutionTarget};
pub use colors::{
    ColorScheme, ColorSpec, ColorTheme, DialogColors, MessageBand, MessageColors, StatusColors,
    UiColors,
};
pub use credential::{
    AudienceBinding, CredentialBinding, CredentialKind, CredentialLifecycle, CredentialProvider,
    CredentialResolver, EndpointFamily, EnvironmentCredentialResolver, ProviderCredential,
    ResolvedCredential, ResolvedSecret,
};
pub use loader::{load_config, load_persisted_config};
#[cfg(test)]
pub(crate) use loader::{load_config_from_path, load_config_from_path_with_paths};
pub use persona::Persona;
pub use provider::{ProviderEntry, ReasoningEffort};
pub use settings::{
    ClientConfig, Config, FeaturesConfig, LicenseConfig, LicenseType, ServerConfig, TeacherEntry,
};
