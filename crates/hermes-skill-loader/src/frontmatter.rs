//! Strip the YAML frontmatter from a SKILL.md file body.

use serde_yaml::Value;

/// Parse `---`-fenced YAML frontmatter from the start of a markdown file.
///
/// Returns `Some((frontmatter, body))` when the file starts with a
/// (possibly preceded by blank lines) `---` fence that closes with a
/// standalone `---` line. Returns `None` when the file has no frontmatter.
///
/// On YAML parse failure, returns `Some((Value::Null, original_text))` so
/// callers can surface a precise warning (the caller decides whether
/// invalid YAML is a "skip and warn" condition).
pub fn parse(raw: &str) -> Option<(Value, String)> {
    // Skip leading blank lines (spec §5.2).
    let trimmed_start = raw.trim_start_matches('\n');
    let after_skip = trimmed_start
        .strip_prefix("---\n")
        .or_else(|| trimmed_start.strip_prefix("---\r\n"))?;

    // Find the closing `---` on its own line. We require a newline
    // before AND after the closing fence so that `---` mid-line in the
    // body does not falsely close.
    let close_marker = "\n---";
    let close_offset = after_skip.find(close_marker)?;
    // The closing fence is `\n---` followed by end-of-line or EOF.
    let after_close_idx = close_offset + close_marker.len();
    let tail = &after_skip[after_close_idx..];
    if !(tail.is_empty() || tail.starts_with('\n') || tail.starts_with("\r\n")) {
        return None;
    }

    let yaml_text = &after_skip[..close_offset];
    // Strip exactly one newline (the one immediately after the closing fence).
    // Any further blank lines within the body are preserved.
    let body = tail
        .strip_prefix("\r\n")
        .or_else(|| tail.strip_prefix('\n'))
        .unwrap_or(tail)
        .to_string();

    let frontmatter = match serde_yaml::from_str::<Value>(yaml_text) {
        Ok(v) => v,
        Err(_) => Value::Null,
    };

    Some((frontmatter, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_strips_it_from_body() {
        let raw = "---\nname: rust\ndescription: \"Rust style\"\n---\n\n# Body\n";
        let (fm, body) = parse(raw).expect("frontmatter should parse");
        assert_eq!(fm.get("name").and_then(|v| v.as_str()), Some("rust"));
        assert_eq!(body, "\n# Body\n");
    }

    #[test]
    fn tolerates_leading_blank_lines_before_opening_fence() {
        let raw = "\n\n---\nname: rust\ndescription: x\n---\nbody\n";
        let (fm, _) = parse(raw).expect("leading blanks should be tolerated");
        assert_eq!(fm.get("name").and_then(|v| v.as_str()), Some("rust"));
    }

    #[test]
    fn returns_none_when_no_opening_fence() {
        let raw = "# Just a markdown file\nwith no frontmatter\n";
        assert!(parse(raw).is_none());
    }

    #[test]
    fn returns_none_when_frontmatter_is_unterminated() {
        let raw = "---\nname: rust\ndescription: x\n";
        assert!(parse(raw).is_none());
    }

    #[test]
    fn returns_null_frontmatter_on_invalid_yaml() {
        // Unclosed flow sequence — `serde_yaml` will fail to parse this.
        let raw = "---\nname: [unclosed\ndescription: x\n---\nbody\n";
        let (fm, body) = parse(raw).expect("frontmatter fence is present");
        assert!(fm.is_null(), "invalid YAML should fall back to Null");
        assert_eq!(body, "body\n", "body should still be extractable");
    }

    #[test]
    fn empty_body_is_returned_when_frontmatter_is_last() {
        let raw = "---\nname: rust\ndescription: x\n---\n";
        let (_, body) = parse(raw).expect("frontmatter present");
        assert_eq!(body, "");
    }

    #[test]
    fn handles_crlf_body_line_endings() {
        // Closing fence is `---\r\n` (CRLF, no preceding newline).
        // The body should be returned with the `\r` stripped.
        let raw = "---\nname: rust\ndescription: \"x\"\n---\r\nbody\n";
        let (_, body) = parse(raw).expect("frontmatter should parse");
        assert_eq!(body, "body\n");
        assert!(!body.contains('\r'), "body should not contain a stray \\r");
    }
}
