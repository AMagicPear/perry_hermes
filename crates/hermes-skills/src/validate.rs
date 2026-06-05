//! Strict validators for frontmatter field values.
//!
//! These are pure string checks. The validation rules come from
//! spec §5.3, §5.4, and §5.5.

const MAX_NAME_LEN: usize = 64;
const MAX_DESC_LEN: usize = 1024;
const RESERVED: &[&str] = &["anthropic", "claude"];

fn is_shaped_like_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_NAME_LEN
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.contains('<')
        && !s.contains('>')
        && !RESERVED.contains(&s)
}

/// True iff `name` is a valid skill name: 1..=64 chars from
/// [a-z0-9-], no XML bracket characters, not a reserved word.
pub fn is_valid_name(name: &str) -> bool {
    is_shaped_like_name(name)
}

/// True iff `description` is non-empty, ≤ 1024 chars, no XML brackets.
pub fn is_valid_description(description: &str) -> bool {
    !description.is_empty()
        && description.len() <= MAX_DESC_LEN
        && !description.contains('<')
        && !description.contains('>')
}

/// True iff `category` passes the same shape checks as `name` (but
/// the `RESERVED` list is the only extra constraint).
pub fn is_valid_category(category: &str) -> bool {
    is_shaped_like_name(category)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_accepts_valid_lowercase_digits_hyphens() {
        assert!(is_valid_name("rust-core-style"));
        assert!(is_valid_name("abc"));
        assert!(is_valid_name("a1-b2-c3"));
        assert!(is_valid_name(&"a".repeat(MAX_NAME_LEN)));
    }

    #[test]
    fn name_rejects_empty() {
        assert!(!is_valid_name(""));
    }

    #[test]
    fn name_rejects_various_invalid_inputs() {
        // Each case exercises a different clause of the validator:
        // uppercase / underscores / over-length / XML brackets / reserved words.
        assert!(!is_valid_name("Rust"), "uppercase should be rejected");
        assert!(!is_valid_name("rust_core"), "underscore should be rejected");
        assert!(
            !is_valid_name(&"a".repeat(MAX_NAME_LEN + 1)),
            "over-length should be rejected"
        );
        assert!(!is_valid_name("foo<bar>baz"), "XML brackets should be rejected");
        assert!(!is_valid_name("anthropic"), "reserved word should be rejected");
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
    fn category_uses_same_rules_as_name() {
        assert!(is_valid_category("software-engineering"));
        assert!(!is_valid_category("Software_Engineering"));
        assert!(!is_valid_category("anthropic"));
    }
}