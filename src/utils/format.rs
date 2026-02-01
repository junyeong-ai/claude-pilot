//! Formatting utilities for display values.

pub fn ratio_to_percent_u8(ratio: f64) -> u8 {
    (ratio * 100.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ratio_to_percent_u8() {
        assert_eq!(ratio_to_percent_u8(0.75), 75);
        assert_eq!(ratio_to_percent_u8(1.0), 100);
        assert_eq!(ratio_to_percent_u8(0.0), 0);
    }
}
