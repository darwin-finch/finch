// Tool execution system for local tool use
//
// Enables Shammah to execute tools (WebFetch, Bash, Read, etc.) locally
// instead of only generating text responses.

pub mod executor;
pub mod implementations;
pub mod pattern_matcher;
pub mod permissions;
pub mod registry;
pub mod types;

pub use executor::ToolExecutor;
pub use pattern_matcher::ToolPatternMatcher;
pub use permissions::{PermissionCheck, PermissionManager, PermissionRule};
pub use registry::{Tool, ToolRegistry};
pub use types::{ContentBlock, ToolDefinition, ToolInputSchema, ToolResult, ToolUse};
