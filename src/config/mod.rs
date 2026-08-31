// Configuration module
// Public interface for configuration loading

mod backend;
mod colors;
pub mod constants;
pub mod credential;
mod loader;
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
