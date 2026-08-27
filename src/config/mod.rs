// Configuration module
// Public interface for configuration loading

mod backend;
mod colors;
pub mod constants;
mod loader;
pub mod persona;
pub mod provider;
mod settings;

#[allow(deprecated)]
pub use backend::BackendDevice; // Deprecated alias for ExecutionTarget
pub use backend::{BackendConfig, ExecutionTarget};
pub use colors::{
    ColorScheme, ColorSpec, ColorTheme, DialogColors, MessageBand, MessageColors, StatusColors,
    UiColors,
};
pub use loader::load_config;
#[cfg(test)]
pub(crate) use loader::load_config_from_path;
pub use persona::Persona;
pub use provider::{ProviderEntry, ReasoningEffort};
pub use settings::{
    ClientConfig, Config, FeaturesConfig, LicenseConfig, LicenseType, ServerConfig, TeacherEntry,
};
