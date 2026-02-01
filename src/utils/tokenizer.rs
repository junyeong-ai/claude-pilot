//! Token counting utilities for context size estimation.
//!
//! Provides configurable token counting using tiktoken encodings.
//! Note: Claude uses a proprietary tokenizer. These OpenAI-based
//! encodings are approximations suitable for budget planning.

use std::sync::OnceLock;

use tiktoken_rs::{CoreBPE, cl100k_base, o200k_base, p50k_base};

use crate::config::TokenEncoding;

static CL100K: OnceLock<CoreBPE> = OnceLock::new();
static O200K: OnceLock<CoreBPE> = OnceLock::new();
static P50K: OnceLock<CoreBPE> = OnceLock::new();

fn get_cl100k() -> &'static CoreBPE {
    CL100K.get_or_init(|| cl100k_base().expect("Failed to load cl100k_base tokenizer"))
}

fn get_o200k() -> &'static CoreBPE {
    O200K.get_or_init(|| o200k_base().expect("Failed to load o200k_base tokenizer"))
}

fn get_p50k() -> &'static CoreBPE {
    P50K.get_or_init(|| p50k_base().expect("Failed to load p50k_base tokenizer"))
}

/// Estimates token count using the specified encoding.
///
/// # Arguments
/// * `text` - The text to tokenize
/// * `encoding` - The encoding strategy to use
/// * `heuristic_chars_per_token` - Chars per token for heuristic mode
///
/// # Returns
/// Estimated token count
pub fn estimate_tokens_with_encoding(
    text: &str,
    encoding: TokenEncoding,
    heuristic_chars_per_token: usize,
) -> usize {
    match encoding {
        TokenEncoding::Cl100kBase => get_cl100k().encode_with_special_tokens(text).len(),
        TokenEncoding::O200kBase => get_o200k().encode_with_special_tokens(text).len(),
        TokenEncoding::P50kBase => get_p50k().encode_with_special_tokens(text).len(),
        TokenEncoding::Heuristic => heuristic_estimate(text, heuristic_chars_per_token),
    }
}

/// Fast heuristic token estimation based on character count.
///
/// This is a simple approximation: tokens ≈ chars / chars_per_token.
/// Useful when exact counting isn't critical or for performance.
fn heuristic_estimate(text: &str, chars_per_token: usize) -> usize {
    let chars_per_token = chars_per_token.max(1);
    text.len().div_ceil(chars_per_token)
}

/// Default token estimation using cl100k_base encoding.
///
/// This is the recommended encoding for Claude context estimation.
/// Provides a good balance of accuracy and performance.
pub fn estimate_tokens(text: &str) -> usize {
    estimate_tokens_with_encoding(text, TokenEncoding::Cl100kBase, 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens_basic() {
        let text = "Hello, world!";
        let tokens = estimate_tokens(text);
        assert!(tokens > 0);
        assert!(tokens < text.len()); // Tokens should be fewer than chars
    }

    #[test]
    fn test_different_encodings() {
        let text = "The quick brown fox jumps over the lazy dog.";

        let cl100k = estimate_tokens_with_encoding(text, TokenEncoding::Cl100kBase, 4);
        let o200k = estimate_tokens_with_encoding(text, TokenEncoding::O200kBase, 4);
        let p50k = estimate_tokens_with_encoding(text, TokenEncoding::P50kBase, 4);
        let heuristic = estimate_tokens_with_encoding(text, TokenEncoding::Heuristic, 4);

        // All should produce reasonable results
        assert!(cl100k > 0);
        assert!(o200k > 0);
        assert!(p50k > 0);
        assert!(heuristic > 0);
    }

    #[test]
    fn test_heuristic_estimate() {
        let text = "twelve chars"; // 12 chars
        let estimate = heuristic_estimate(text, 4);
        assert_eq!(estimate, 3); // 12 / 4 = 3
    }

    #[test]
    fn test_empty_string() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(heuristic_estimate("", 4), 0);
    }

    #[test]
    fn test_unicode() {
        let text = "こんにちは世界"; // Japanese: "Hello World"
        let tokens = estimate_tokens(text);
        assert!(tokens > 0);
    }
}
