# local — public interface

Generated from [`src/local/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/local/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Generated response with metadata
pub struct GeneratedResponse { … }
/// Local generation system that coordinates pattern classification and response generation
pub struct LocalGenerator { … }
impl LocalGenerator {
    /// Get the model adapter for cleaning
    pub fn get_adapter(&self) -> Box<dyn LocalModelAdapter>;
    /// Check if enabled
    pub fn is_enabled(&self) -> bool;
    /// Learn from a Claude response
    pub fn learn_from_claude(&mut self, query: &str, response: &str, quality_score: f64, batch_trainer: Option<&Arc<RwLock<BatchTrainer>>>);
    /// Load local generator from file
    pub fn load<P: AsRef<std::path::Path>>(path: P) -> Result<Self>;
    /// Return the name of the currently-configured local model.
    pub fn model_name(&self) -> &str;
    /// Create new local generator without neural models
    pub fn new() -> Self;
    /// Get pattern classifier
    pub fn pattern_classifier(&self) -> &PatternClassifier;
    /// Get response generator
    pub fn response_generator(&mut self) -> &mut TemplateGenerator;
    /// Save local generator to file
    pub fn save<P: AsRef<std::path::Path>>(&self, path: P) -> Result<()>;
    /// Enable/disable local generation
    pub fn set_enabled(&mut self, enabled: bool);
    /// Try to generate a local response from patterns
    pub fn try_generate_from_pattern(&mut self, query: &str) -> Result<Option<String>>;
    /// Try to generate a response with streaming callback  Calls the callback for each generated token with (token_id, token_text).
    pub fn try_generate_from_pattern_streaming<F>(&mut self, messages: &[Message], token_callback: F) -> Result<Option<GeneratorResponse>> where F: FnMut(u32, &str) + Send + 'static,;
    /// Try to generate a response from patterns with tools  This method is used by the daemon to support tool execution.
    pub fn try_generate_from_pattern_with_tools(&mut self, messages: &[Message], _tools: Option<Vec<ToolDefinition>>) -> Result<Option<GeneratorResponse>>;
    /// Create local generator with optional neural models
    pub fn with_models(neural_generator: Option<Arc<RwLock<GeneratorModel>>>) -> Self;
}
/// Pattern classifier that learns from Claude's responses
pub struct PatternClassifier { … }
impl PatternClassifier {
    /// Classify a query based on learned patterns
    pub fn classify(&self, query: &str) -> (QueryPattern, f64);
    /// Create new pattern classifier
    pub fn new() -> Self;
}
/// Query patterns learned from Claude's responses
pub enum QueryPattern { Greeting, Definition, HowTo, Explanation, Code, Debugging, Comparison, Opinion, Complex, Other }
impl QueryPattern {
    /// Convert to string
    pub fn as_str(&self) -> &str;
}
/// Template-based generator that creates local responses from learned patterns
pub struct TemplateGenerator { … }
impl TemplateGenerator {
    /// Generate a response for a query
    pub fn generate(&mut self, query: &str) -> Result<GeneratedResponse>;
    /// Generate a response with streaming callback  Calls the callback for each generated token with (token_id, token_text).
    pub fn generate_streaming<F>(&mut self, messages: &[crate::providers::Message], token_callback: F) -> Result<Option<crate::generators::GeneratorResponse>> where F: FnMut(u32, &str) + Send + 'static,;
    /// Get the model adapter for external use (e.g., streaming cleaning)
    pub fn get_adapter(&self) -> Box<dyn LocalModelAdapter>;
    /// Learn from a Claude response
    pub fn learn_from_claude(&mut self, query: &str, response: &str, quality_score: f64, batch_trainer: Option<&Arc<RwLock<BatchTrainer>>>);
    /// Create new response generator without neural models
    pub fn new(pattern_classifier: PatternClassifier) -> Self;
    /// Create response generator with optional neural models
    pub fn with_models(pattern_classifier: PatternClassifier, neural_generator: Option<Arc<RwLock<GeneratorModel>>>, model_name: &str) -> Self;
}
```
