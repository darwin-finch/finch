// Tokenizer module stub (Phase 4: Candle-based implementation removed)
//
// The original tokenizer module used Candle's tensor operations.
// For ONNX-based models, use the tokenizers crate directly with LoadedOnnxModel.

use anyhow::Result;

/// Text tokenizer (stub for compatibility)
///
/// Phase 4: This is a stub. Use tokenizers crate directly with ONNX models:
///
/// ```text
/// use tokenizers::Tokenizer;
/// let tokenizer = Tokenizer::from_file("tokenizer.json")?;
/// let encoding = tokenizer.encode(text, true)?;
/// let tokens = encoding.get_ids();
/// ```
#[derive(Debug, Clone)]
pub struct TextTokenizer;

impl TextTokenizer {
    pub fn new(_vocab_size: usize) -> Result<Self> {
        anyhow::bail!(
            "TextTokenizer removed in Phase 4 (Candle-based).\n\
             Use tokenizers crate directly:\n\
             \n\
             use tokenizers::Tokenizer;\n\
             let tokenizer = Tokenizer::from_file(\"tokenizer.json\")?;"
        )
    }

    pub fn stub() -> Result<Self> {
        // Phase 4: Return dummy instance for compatibility
        // The tokenizer is only used for training features which are stubbed
        Ok(Self)
    }

    pub fn encode(&self, _text: &str, _add_special_tokens: bool) -> Result<Vec<u32>> {
        anyhow::bail!("TextTokenizer removed in Phase 4 (Candle-based)")
    }

    pub fn decode(&self, _tokens: &[u32], _skip_special_tokens: bool) -> Result<String> {
        anyhow::bail!("TextTokenizer removed in Phase 4 (Candle-based)")
    }
}
