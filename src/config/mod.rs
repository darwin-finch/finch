// Configuration module
// Public interface for configuration loading

mod backend;
mod colors;
mod loader;
pub mod persona;
mod settings;

pub use backend::{BackendConfig, ExecutionTarget};
#[allow(deprecated)]
pub use backend::BackendDevice; // Deprecated alias for ExecutionTarget
pub use colors::{ColorScheme, ColorSpec, DialogColors, MessageColors, StatusColors, UiColors};
pub use loader::load_config;
pub use persona::Persona;
pub use settings::{ClientConfig, Config, FeaturesConfig, ServerConfig, TeacherEntry};
