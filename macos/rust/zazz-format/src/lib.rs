// The .zazz file format: archives, manifests, packing and script ids.
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]

pub mod manifest;
pub mod pack;
pub mod sig;
pub mod zip;

// Returns the script id of a .zazz file: "b3:" and the BLAKE3 hash of its exact bytes.
pub fn script_id(bytes: &[u8]) -> String {
    format!("b3:{}", blake3::hash(bytes).to_hex())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // Checks the id format and that it changes with any byte.
    #[test]
    fn test_script_id() {
        let a = script_id(b"abc");
        assert!(a.starts_with("b3:") && a.len() == 67);
        assert_ne!(a, script_id(b"abd"));
    }
}
