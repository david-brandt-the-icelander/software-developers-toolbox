// Token estimation: a cheap bytes-per-token heuristic used for budgets and session windows.

// Estimates tokens for a byte count at about 3.5 bytes per token.
pub fn from_bytes(n: usize) -> usize {
    (n * 2).div_ceil(7)
}

// Estimates tokens for a string.
pub fn estimate(text: &str) -> usize {
    from_bytes(text.len())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks the estimate rounds up and scales with length.
    #[test]
    fn test_from_bytes_rounds_up() {
        assert_eq!(from_bytes(0), 0);
        assert_eq!(from_bytes(1), 1);
        assert_eq!(from_bytes(7), 2);
        assert_eq!(from_bytes(35), 10);
    }

    // Checks the string wrapper uses the byte length.
    #[test]
    fn test_estimate_uses_byte_length() {
        assert_eq!(estimate("héllo"), from_bytes(6));
    }
}
