# generators — public interface

Generated from [`src/generators/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/generators/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Claude API generator implementation
pub struct ClaudeGenerator { … }
impl ClaudeGenerator {
    /// The instruction files found at construction and what happened to each.
    pub fn instruction_sources(&self) -> &InstructionSources;
    pub fn new(client: Arc<ClaudeClient>) -> Self;
    /// Build a generator whose instructions are collected from `cwd`, reading user-level files under `home`.
    pub fn new_in(client: Arc<ClaudeClient>, cwd: Option<PathBuf>, home: Option<PathBuf>) -> Self;
}
/// Presents the daemon-owned local model through the same interface used by cloud providers.
pub struct DaemonLocalGenerator { … }
impl DaemonLocalGenerator {
    pub fn new(client: Arc<DaemonClient>, profile_name: impl Into<String>) -> Self;
}
/// Generator capabilities (what features are supported)
pub struct GeneratorCapabilities { … }
/// Unified response format
pub struct GeneratorResponse { … }
/// Associates a configured profile name with a generator without changing its provider-specific response metadata.
pub struct ProfiledGenerator { … }
impl ProfiledGenerator {
    pub fn new(profile_name: impl Into<String>, inner: std::sync::Arc<dyn Generator>) -> Self;
}
/// Qwen local generator implementation
pub struct QwenGenerator { … }
impl QwenGenerator {
    pub fn new(local_generator: Arc<RwLock<LocalGenerator>>, tokenizer: Arc<TextTokenizer>, tool_executor: Option<Arc<tokio::sync::Mutex<ToolExecutor>>>) -> Self;
}
pub struct ResponseMetadata { … }
/// Streaming chunk (text delta or complete block)
pub enum StreamChunk { TextDelta, ContentBlockComplete, ResponseMetadata, Usage, Allowance }
/// Tool use request from generator
pub struct ToolUse { … }
impl ToolUse {
    /// Convert to ContentBlock for conversation history
    pub fn to_content_block(&self) -> ContentBlock;
}
```

## Traits

```rust
/// Unified generator interface for Claude, Qwen, and future generators
pub trait Generator: Send + Sync {
    fn capabilities(&self) -> &GeneratorCapabilities;
    fn name(&self) -> &str;
    fn model_name(&self) -> &str;
}
```

## Functions

```rust
pub(crate) fn validate_response_model(model: &str) -> Result<()> { … }
```

## Constants

```rust
pub const CODING_SYSTEM_PROMPT: &str = "You are the software-engineering reasoning provider inside \ Finch. You are not the Finch application or terminal UI, and you do not impersonate either one. \ Use the host tools Finch exposes to inspect and modify the user's codebase autonomously, like a \ senior engineer pairing at the terminal. A transport-specific execution/output contract may follow \ this coding policy;
pub(crate) const MAX_RESPONSE_MODEL_BYTES: usize = 256;
```
