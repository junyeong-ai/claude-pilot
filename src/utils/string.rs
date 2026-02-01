/// Find the largest valid UTF-8 boundary at or before the given byte index.
/// Returns the byte index that is safe to slice at.
#[inline]
fn safe_byte_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    // Find the character boundary at or before max_bytes
    s.char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= max_bytes)
        .last()
        .unwrap_or(0)
}

/// Truncate a string with a marker if it exceeds the maximum length (UTF-8 safe).
///
/// Returns an owned String. The max_len is in bytes, but truncation respects
/// UTF-8 character boundaries to avoid panics with multi-byte characters.
#[inline]
pub fn truncate_with_marker(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let boundary = safe_byte_boundary(s, max_len);
        format!("{}...[truncated]", &s[..boundary])
    }
}

/// Truncate a string to maximum length, returning a borrowed slice (UTF-8 safe).
///
/// Use this when you don't need the marker suffix and want to avoid allocation.
/// The max_len is in bytes, but truncation respects UTF-8 character boundaries.
#[inline]
pub fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        let boundary = safe_byte_boundary(s, max_len);
        &s[..boundary]
    }
}

/// Truncate a string to maximum character count (UTF-8 safe).
///
/// This function is O(n) where n is the character count, but guarantees
/// correct handling of multi-byte UTF-8 characters.
/// Adds "..." suffix if truncated.
#[inline]
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{}...", truncated)
}

/// Truncate a string at a word boundary with "..." suffix (UTF-8 safe).
///
/// Attempts to truncate at whitespace, period, or comma for cleaner output.
/// Falls back to character boundary truncation if no word boundary is found.
#[inline]
pub fn truncate_at_boundary(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let truncate_at = safe_byte_boundary(s, max_len.saturating_sub(3));
    let boundary = s[..truncate_at]
        .rfind(|c: char| c.is_whitespace() || c == '.' || c == ',')
        .unwrap_or(truncate_at);
    format!("{}...", &s[..boundary])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_with_marker_short() {
        let result = truncate_with_marker("hello", 10);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_with_marker_exact() {
        let result = truncate_with_marker("hello", 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_with_marker_long() {
        let result = truncate_with_marker("hello world", 5);
        assert_eq!(result, "hello...[truncated]");
    }

    #[test]
    fn test_truncate_str_short() {
        let result = truncate_str("hello", 10);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_str_long() {
        let result = truncate_str("hello world", 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_chars_short() {
        let result = truncate_chars("hello", 10);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_chars_long() {
        let result = truncate_chars("hello world", 8);
        assert_eq!(result, "hello...");
    }

    #[test]
    fn test_truncate_chars_unicode() {
        // "안녕하세요" = 5 characters
        let result = truncate_chars("안녕하세요 세계", 6);
        assert_eq!(result, "안녕하...");
    }

    #[test]
    fn test_truncate_at_boundary_short() {
        let result = truncate_at_boundary("hello", 10);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_at_boundary_at_word() {
        let result = truncate_at_boundary("hello world today", 12);
        // Should truncate at word boundary before "today"
        assert!(result.ends_with("..."));
        assert!(result.len() <= 12);
    }

    #[test]
    fn test_truncate_with_marker_unicode() {
        // "안녕하세요" = 5 characters, 15 bytes
        let korean = "안녕하세요 세계입니다";
        // Truncate at byte 10 (middle of "세")
        let result = truncate_with_marker(korean, 10);
        // Should not panic and should end at a character boundary
        assert!(result.ends_with("...[truncated]"));
        assert!(!result.contains('\u{FFFD}')); // No replacement characters
    }

    #[test]
    fn test_truncate_str_unicode() {
        let korean = "안녕하세요";
        // Each Korean char is 3 bytes, so truncating at 7 bytes should give "안녕" (6 bytes)
        let result = truncate_str(korean, 7);
        assert_eq!(result, "안녕");
    }

    #[test]
    fn test_truncate_at_boundary_unicode() {
        let mixed = "Hello 안녕하세요 World";
        let result = truncate_at_boundary(mixed, 15);
        // Should truncate at word boundary, respecting UTF-8
        assert!(result.ends_with("..."));
        assert!(!result.contains('\u{FFFD}')); // No replacement characters
    }
}
