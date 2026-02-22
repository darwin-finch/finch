// Configuration module
// Public interface for configuration loading

mod backend;
mod colors;
pub mod constants;
mod loader;
pub mod persona;
pub mod provider;
mod settings;

pub use backend::{BackendConfig, ExecutionTarget};
#[allow(deprecated)]
pub use backend::BackendDevice; // Deprecated alias for ExecutionTarget
pub use colors::{ColorScheme, ColorSpec, ColorTheme, DialogColors, MessageColors, StatusColors, UiColors};
pub use loader::load_config;
pub use persona::Persona;
pub use provider::ProviderEntry;
pub use settings::{ClientConfig, Config, FeaturesConfig, ServerConfig, TeacherEntry};
