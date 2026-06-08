//! Strict validators for frontmatter field values.
//!
//! These are pure string checks. The validation rules come from
//! spec §5.3, §5.4, and §5.5.

const MAX_NAME_LEN: usize = 64;
const MAX_DESC_LEN: usize = 1024;

/// True iff `name` is a valid skill or category name: 1..=64 chars from
/// [a-z0-9-], no XML bracket characters.
pub fn is_valid_category(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.contains('<')
        && !name.contains('>')
}

/// True iff `description` is non-empty, ≤ 1024 chars, no XML brackets.
pub fn is_valid_description(description: &str) -> bool {
    !description.is_empty()
        && description.len() <= MAX_DESC_LEN
        && !description.contains('<')
        && !description.contains('>')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_accepts_valid_lowercase_digits_hyphens() {
        assert!(is_valid_category("rust-core-style"));
        assert!(is_valid_category("abc"));
        assert!(is_valid_category("a1-b2-c3"));
        assert!(is_valid_category(&"a".repeat(MAX_NAME_LEN)));
    }

    #[test]
    fn name_rejects_empty() {
        assert!(!is_valid_category(""));
    }

    #[test]
    fn name_rejects_various_invalid_inputs() {
        // Each case exercises a different clause of the validator:
        // uppercase / underscores / over-length / XML brackets.
        assert!(!is_valid_category("Rust"), "uppercase should be rejected");
        assert!(
            !is_valid_category("rust_core"),
            "underscore should be rejected"
        );
        assert!(
            !is_valid_category(&"a".repeat(MAX_NAME_LEN + 1)),
            "over-length should be rejected"
        );
        assert!(
            !is_valid_category("foo<bar>baz"),
            "XML brackets should be rejected"
        );
    }

    #[test]
    fn description_accepts_normal_text() {
        assert!(is_valid_description("Rust style guide"));
        assert!(is_valid_description(&"a".repeat(MAX_DESC_LEN)));
    }

    #[test]
    fn description_rejects_invalid_inputs() {
        // Each case exercises a different clause of the validator.
        assert!(!is_valid_description(""), "empty should be rejected");
        assert!(
            !is_valid_description(&"a".repeat(MAX_DESC_LEN + 1)),
            "over-length should be rejected"
        );
        assert!(
            !is_valid_description("a <b> tag"),
            "XML brackets should be rejected"
        );
    }

    #[test]
    fn category_accepts_common_package_like_names() {
        assert!(is_valid_category("software-engineering"));
        assert!(is_valid_category("anthropic"));
        assert!(is_valid_category("claude"));
        assert!(!is_valid_category("Software_Engineering"));
    }
}
