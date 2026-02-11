// Configuration module
// Public interface for configuration loading

mod backend;
mod loader;
mod settings;

pub use backend::{BackendConfig, BackendDevice};
pub use loader::load_config;
pub use settings::{Config, ServerConfig, TeacherEntry};
