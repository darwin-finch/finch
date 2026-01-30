// Tokenizer - Text to token IDs conversion
// Uses BPE tokenizer from the tokenizers crate

use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor};
use std::path::{Path, PathBuf};
use tokenizers::Tokenizer;

/// Wrapper around the tokenizers crate
pub struct TextTokenizer {
    tokenizer: Tokenizer,
    vocab_size: usize,
    pad_token_id: u32,
    bos_token_id: u32,
    eos_token_id: u32,
}

impl TextTokenizer {
    /// Create a new tokenizer from a tokenizer.json file
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let tokenizer = Tokenizer::from_file(path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer: {}", e))?;

        let vocab_size = tokenizer.get_vocab_size(true);

        Ok(Self {
            tokenizer,
            vocab_size,
            pad_token_id: 0, // Standard PAD token
            bos_token_id: 1, // Standard BOS (beginning of sequence)
            eos_token_id: 2, // Standard EOS (end of sequence)
        })
    }

    /// Create a default tokenizer (simplified for initial testing)
    pub fn default() -> Result<Self> {
        // For now, create a simple BPE tokenizer
        // In production, this should load from ~/.shammah/tokenizer.json
        let tokenizer = Self::create_simple_bpe_tokenizer()?;

        Ok(Self {
            tokenizer,
            vocab_size: 50_000,
            pad_token_id: 0,
            bos_token_id: 1,
            eos_token_id: 2,
        })
    }

    /// Encode text to token IDs
    pub fn encode(&self, text: &str, add_special_tokens: bool) -> Result<Vec<u32>> {
        // Try to use the tokenizer
        match self.tokenizer.encode(text, add_special_tokens) {
            Ok(encoding) => {
                let ids = encoding.get_ids().to_vec();
                // If we get an empty result, fall back to character-level encoding
                if ids.is_empty() && !text.is_empty() {
                    Ok(self.char_level_encode(text, add_special_tokens))
                } else {
                    Ok(ids)
                }
            }
            Err(_) => {
                // Fallback to character-level encoding if tokenizer fails
                Ok(self.char_level_encode(text, add_special_tokens))
            }
        }
    }

    /// Simple character-level encoding as fallback
    fn char_level_encode(&self, text: &str, add_special_tokens: bool) -> Vec<u32> {
        let mut ids = Vec::new();

        if add_special_tokens {
            ids.push(self.bos_token_id);
        }

        // Convert each character to an ID (simple: char as u32 % vocab_size)
        for ch in text.chars().take(self.vocab_size - 10) {
            // Leave room for special tokens
            let id = ((ch as u32) % (self.vocab_size as u32 - 10)) + 10; // Start from 10 to avoid special tokens
            ids.push(id);
        }

        if add_special_tokens {
            ids.push(self.eos_token_id);
        }

        ids
    }

    /// Encode text and return as Tensor
    pub fn encode_as_tensor(&self, text: &str, device: &Device) -> Result<Tensor> {
        let ids = self.encode(text, true)?;
        let len = ids.len();

        // Create tensor with shape (1, seq_len) for batch processing
        Tensor::from_vec(ids, (1, len), device).context("Failed to create tensor from token IDs")
    }

    /// Encode with padding to fixed length
    pub fn encode_padded(&self, text: &str, max_length: usize, device: &Device) -> Result<Tensor> {
        let mut ids = self.encode(text, true)?;

        // Truncate if too long
        if ids.len() > max_length {
            ids.truncate(max_length);
            // Replace last token with EOS
            if let Some(last) = ids.last_mut() {
                *last = self.eos_token_id;
            }
        }

        // Pad if too short
        while ids.len() < max_length {
            ids.push(self.pad_token_id);
        }

        Tensor::from_vec(ids, (1, max_length), device).context("Failed to create padded tensor")
    }

    /// Decode token IDs back to text
    pub fn decode(&self, ids: &[u32], skip_special_tokens: bool) -> Result<String> {
        // Try tokenizer decode first
        match self.tokenizer.decode(ids, skip_special_tokens) {
            Ok(text) if !text.is_empty() => Ok(text),
            _ => {
                // Fallback to character-level decode
                Ok(self.char_level_decode(ids, skip_special_tokens))
            }
        }
    }

    /// Simple character-level decoding as fallback
    fn char_level_decode(&self, ids: &[u32], skip_special_tokens: bool) -> String {
        let mut text = String::new();

        for &id in ids {
            // Skip special tokens if requested
            if skip_special_tokens
                && (id == self.pad_token_id || id == self.bos_token_id || id == self.eos_token_id)
            {
                continue;
            }

            // Convert ID back to character (reverse of encoding)
            if id >= 10 {
                let ch = char::from_u32((id - 10) as u32).unwrap_or('?');
                text.push(ch);
            }
        }

        text
    }

    /// Decode from Tensor
    pub fn decode_tensor(&self, tensor: &Tensor, skip_special_tokens: bool) -> Result<String> {
        // Get the token IDs from the tensor
        let ids = if tensor.dims().len() == 1 {
            tensor.to_vec1::<u32>()?
        } else if tensor.dims().len() == 2 {
            // Take first batch element
            tensor.to_vec2::<u32>()?[0].clone()
        } else {
            anyhow::bail!("Tensor must be 1D or 2D, got {}D", tensor.dims().len());
        };

        self.decode(&ids, skip_special_tokens)
    }

    /// Get vocabulary size
    pub fn vocab_size(&self) -> usize {
        self.vocab_size
    }

    /// Get special token IDs
    pub fn special_tokens(&self) -> (u32, u32, u32) {
        (self.pad_token_id, self.bos_token_id, self.eos_token_id)
    }

    /// Create a simple BPE tokenizer for testing
    fn create_simple_bpe_tokenizer() -> Result<Tokenizer> {
        use tokenizers::models::bpe::BPE;
        use tokenizers::normalizers::Lowercase;
        use tokenizers::pre_tokenizers::byte_level::ByteLevel;
        use tokenizers::{AddedToken, NormalizerWrapper, PreTokenizerWrapper};

        // Create a simple BPE model with empty vocabulary
        // In production, this should be trained on actual data
        let bpe = BPE::default();

        let mut tokenizer = Tokenizer::new(bpe);

        // Add lowercase normalizer
        tokenizer.with_normalizer(Some(NormalizerWrapper::Lowercase(Lowercase)));

        // Add byte-level pre-tokenizer
        tokenizer.with_pre_tokenizer(Some(PreTokenizerWrapper::ByteLevel(ByteLevel::default())));

        // Add special tokens
        let special_tokens = vec![
            AddedToken::from("[PAD]", true),
            AddedToken::from("[BOS]", true),
            AddedToken::from("[EOS]", true),
            AddedToken::from("[UNK]", true),
        ];

        // add_special_tokens returns the number of tokens added (usize)
        let _num_added = tokenizer.add_special_tokens(&special_tokens);

        Ok(tokenizer)
    }

    /// Save tokenizer to file
    pub fn save<P: AsRef<Path>>(&self, path: P, pretty: bool) -> Result<()> {
        self.tokenizer
            .save(path.as_ref(), pretty)
            .map_err(|e| anyhow::anyhow!("Failed to save tokenizer: {}", e))
    }

    /// Get the tokenizer file path for the user
    pub fn tokenizer_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        let path = home.join(".shammah").join("tokenizer.json");
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::Device;

    #[test]
    fn test_tokenizer_creation() {
        let tokenizer = TextTokenizer::default();
        assert!(tokenizer.is_ok());
    }

    #[test]
    fn test_encode_decode() -> Result<()> {
        let tokenizer = TextTokenizer::default()?;
        let text = "Hello, world!";

        let ids = tokenizer.encode(text, true)?;
        assert!(!ids.is_empty());

        let decoded = tokenizer.decode(&ids, false)?;
        // Note: decoded text might not exactly match due to normalization
        assert!(!decoded.is_empty());

        Ok(())
    }

    #[test]
    fn test_encode_as_tensor() -> Result<()> {
        let tokenizer = TextTokenizer::default()?;
        let device = Device::Cpu;
        let text = "Test encoding";

        let tensor = tokenizer.encode_as_tensor(text, &device)?;
        assert_eq!(tensor.dims().len(), 2); // (batch_size, seq_len)
        assert_eq!(tensor.dims()[0], 1); // batch_size = 1

        Ok(())
    }

    #[test]
    fn test_encode_padded() -> Result<()> {
        let tokenizer = TextTokenizer::default()?;
        let device = Device::Cpu;
        let text = "Short";
        let max_length = 50;

        let tensor = tokenizer.encode_padded(text, max_length, &device)?;
        assert_eq!(tensor.dims(), &[1, max_length]);

        Ok(())
    }

    #[test]
    fn test_vocab_size() -> Result<()> {
        let tokenizer = TextTokenizer::default()?;
        assert_eq!(tokenizer.vocab_size(), 50_000);

        Ok(())
    }

    #[test]
    fn test_special_tokens() -> Result<()> {
        let tokenizer = TextTokenizer::default()?;
        let (pad, bos, eos) = tokenizer.special_tokens();
        assert_eq!(pad, 0);
        assert_eq!(bos, 1);
        assert_eq!(eos, 2);

        Ok(())
    }
}
