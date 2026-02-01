use std::collections::HashSet;

use crate::utils::truncate_at_boundary;

pub struct TextCompressor;

impl TextCompressor {
    pub fn new() -> Self {
        Self
    }

    pub fn compress(&self, text: &str, max_chars: usize) -> String {
        if text.len() <= max_chars {
            return text.to_string();
        }

        let sentences: Vec<&str> = text
            .split(['.', '\n'])
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();

        if sentences.is_empty() {
            return self.truncate(text, max_chars);
        }

        // Store (original_index, score, sentence) to avoid O(n²) lookup
        let mut scored: Vec<(usize, usize, &str)> = sentences
            .iter()
            .enumerate()
            .map(|(i, s)| (i, self.score_sentence(s, i, sentences.len()), *s))
            .collect();

        // Sort by score descending (highest first)
        scored.sort_by(|a, b| b.1.cmp(&a.1));

        // Collect indices of sentences that fit within budget
        let mut included_indices: Vec<usize> = Vec::new();
        let mut total_len = 0;

        for (original_idx, _score, sentence) in &scored {
            let additional_len = if included_indices.is_empty() {
                sentence.len()
            } else {
                sentence.len() + 2 // ". " separator
            };

            if total_len + additional_len <= max_chars {
                included_indices.push(*original_idx);
                total_len += additional_len;
            }
        }

        // Sort by original position to maintain text order
        included_indices.sort_unstable();

        // Build result
        let mut result = String::with_capacity(total_len);
        for (i, &idx) in included_indices.iter().enumerate() {
            if i > 0 {
                result.push_str(". ");
            }
            result.push_str(sentences[idx]);
        }

        if result.len() > max_chars {
            self.truncate(&result, max_chars)
        } else {
            result
        }
    }

    /// Score a sentence for summarization priority using structural signals.
    fn score_sentence(&self, sentence: &str, position: usize, total: usize) -> usize {
        let mut score = 0;

        // Position-based scoring (structural signal)
        if position == 0 {
            score += 20; // First sentence often has context
        } else if position == total - 1 {
            score += 15; // Last sentence often has conclusion
        }

        // Structural markers (language-agnostic)
        if sentence.contains(':') || sentence.contains('-') {
            score += 5;
        }

        // Length bonus for substantive sentences (language-aware)
        let char_count = sentence.chars().count();
        let (min_chars, max_chars) = Self::substantive_length_range(sentence);
        if char_count > min_chars && char_count < max_chars {
            score += 10;
        }

        score
    }

    /// Determine substantive length range based on script detection.
    ///
    /// CJK (Chinese, Japanese, Korean) characters carry more information per character
    /// than Latin scripts. A 10-character Chinese sentence can convey as much as
    /// a 30-character English sentence.
    ///
    /// Returns (min_chars, max_chars) for "substantive" sentence detection.
    fn substantive_length_range(text: &str) -> (usize, usize) {
        let cjk_ratio = Self::cjk_character_ratio(text);

        if cjk_ratio > 0.3 {
            // Predominantly CJK text: lower thresholds
            // CJK characters are ~3x more information-dense
            (7, 80)
        } else if cjk_ratio > 0.1 {
            // Mixed text: intermediate thresholds
            (12, 120)
        } else {
            // Predominantly Latin/alphabetic text
            (20, 200)
        }
    }

    /// Calculate the ratio of CJK characters in the text.
    ///
    /// CJK ranges (approximate):
    /// - CJK Unified Ideographs: U+4E00 - U+9FFF
    /// - CJK Extension A: U+3400 - U+4DBF
    /// - Hangul Syllables: U+AC00 - U+D7AF
    /// - Hiragana: U+3040 - U+309F
    /// - Katakana: U+30A0 - U+30FF
    fn cjk_character_ratio(text: &str) -> f32 {
        let total_chars = text.chars().count();
        if total_chars == 0 {
            return 0.0;
        }

        let cjk_count = text
            .chars()
            .filter(|c| {
                let c = *c;
                // CJK Unified Ideographs
                ('\u{4E00}'..='\u{9FFF}').contains(&c) ||
                // CJK Extension A
                ('\u{3400}'..='\u{4DBF}').contains(&c) ||
                // Hangul Syllables
                ('\u{AC00}'..='\u{D7AF}').contains(&c) ||
                // Hiragana
                ('\u{3040}'..='\u{309F}').contains(&c) ||
                // Katakana
                ('\u{30A0}'..='\u{30FF}').contains(&c)
            })
            .count();

        cjk_count as f32 / total_chars as f32
    }

    fn truncate(&self, text: &str, max_chars: usize) -> String {
        truncate_at_boundary(text, max_chars)
    }

    pub fn deduplicate(&self, items: &[String], similarity_threshold: f32) -> Vec<String> {
        if items.is_empty() {
            return Vec::new();
        }

        let mut result: Vec<String> = Vec::new();

        for item in items {
            let is_duplicate = result
                .iter()
                .any(|existing| self.similarity(existing, item) >= similarity_threshold);

            if !is_duplicate {
                result.push(item.clone());
            }
        }

        result
    }

    fn similarity(&self, a: &str, b: &str) -> f32 {
        let words_a: HashSet<&str> = a.split_whitespace().collect();
        let words_b: HashSet<&str> = b.split_whitespace().collect();

        if words_a.is_empty() && words_b.is_empty() {
            return 1.0;
        }

        if words_a.is_empty() || words_b.is_empty() {
            return 0.0;
        }

        let intersection = words_a.intersection(&words_b).count();
        let union = words_a.union(&words_b).count();

        intersection as f32 / union as f32
    }
}

impl Default for TextCompressor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_short_text() {
        let compressor = TextCompressor::new();
        let text = "Short text.";
        assert_eq!(compressor.compress(text, 50), text);
    }

    #[test]
    fn test_compress_long_text() {
        let compressor = TextCompressor::new();
        let text = "First sentence with important info. Second sentence with details. Third sentence with more context. Fourth sentence with additional information.";
        let result = compressor.compress(text, 50);
        assert!(result.len() <= 50);
    }

    #[test]
    fn test_deduplicate() {
        let compressor = TextCompressor::new();
        let items = vec![
            "Use async await for API calls".to_string(),
            "Use async await for HTTP requests".to_string(),
            "Implement proper error handling".to_string(),
        ];
        let result = compressor.deduplicate(&items, 0.5);
        assert!(result.len() < items.len());
    }

    #[test]
    fn test_similarity() {
        let compressor = TextCompressor::new();
        assert!(compressor.similarity("hello world", "hello world") == 1.0);
        assert!(compressor.similarity("hello world", "goodbye world") > 0.0);
        assert!(compressor.similarity("hello world", "completely different") < 0.5);
    }

    #[test]
    fn test_cjk_character_ratio() {
        // Pure English
        assert_eq!(TextCompressor::cjk_character_ratio("hello world"), 0.0);

        // Pure Chinese
        assert!(TextCompressor::cjk_character_ratio("你好世界") > 0.9);

        // Pure Korean
        assert!(TextCompressor::cjk_character_ratio("안녕하세요") > 0.9);

        // Pure Japanese (hiragana + kanji)
        assert!(TextCompressor::cjk_character_ratio("こんにちは世界") > 0.9);

        // Mixed content
        let mixed = "Hello 你好 World";
        let ratio = TextCompressor::cjk_character_ratio(mixed);
        assert!(ratio > 0.0 && ratio < 0.5);

        // Empty string
        assert_eq!(TextCompressor::cjk_character_ratio(""), 0.0);
    }

    #[test]
    fn test_substantive_length_range() {
        // English text
        let (min, max) = TextCompressor::substantive_length_range("This is English text");
        assert_eq!((min, max), (20, 200));

        // Chinese text
        let (min, max) = TextCompressor::substantive_length_range("这是中文文本测试");
        assert_eq!((min, max), (7, 80));

        // Korean text
        let (min, max) = TextCompressor::substantive_length_range("한국어 텍스트 테스트입니다");
        assert_eq!((min, max), (7, 80));

        // Mixed text (moderate CJK ratio)
        let (min, max) = TextCompressor::substantive_length_range("Hello World 你好 Testing 测试");
        // Should be intermediate thresholds
        assert!(min > 7 && min < 20);
        assert!(max > 80 && max < 200);
    }

    #[test]
    fn test_compress_cjk_text() {
        let compressor = TextCompressor::new();

        // Chinese text compression
        let chinese_text = "这是第一句话。这是第二句话有很多细节。这是第三句结论。";
        let result = compressor.compress(chinese_text, 30);
        assert!(result.chars().count() <= 30);

        // Korean text compression
        let korean_text = "첫 번째 문장입니다. 두 번째 문장은 세부사항입니다. 세 번째 결론입니다.";
        let result = compressor.compress(korean_text, 30);
        assert!(result.chars().count() <= 30);
    }
}
