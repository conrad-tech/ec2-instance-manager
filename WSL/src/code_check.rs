//! Syntax checks for the file editor: parse the text and report where it
//! stops making sense. Parsing only — nothing here runs `terraform` or any
//! other tool, and it cannot see semantic errors (an unknown variable, a
//! wrong attribute name). It is the offline, as-you-type half of validation.
//!
//! The parsers stop at the first error, so at most one problem is reported;
//! fixing it and re-checking surfaces the next.

use std::ops::Range;

/// Files past this size are not checked (the check re-runs on every edit).
pub const MAX_CHECK_BYTES: usize = 1_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// 0-based line.
    pub line: usize,
    /// 0-based byte offset into that line.
    pub col: usize,
    pub message: String,
}

/// Extensions `check` knows how to parse.
pub fn can_check(ext: &str) -> bool {
    matches!(ext, "tf" | "tfvars" | "hcl" | "json" | "yaml" | "yml")
}

/// Parse `text` as `ext` and return the first syntax error, if any.
pub fn check(text: &str, ext: &str) -> Option<Problem> {
    if text.len() > MAX_CHECK_BYTES || text.trim().is_empty() {
        return None;
    }
    match ext {
        "tf" | "tfvars" | "hcl" => check_hcl(text),
        "json" => check_json(text),
        "yaml" | "yml" => check_yaml(text),
        _ => None,
    }
}

fn check_hcl(text: &str) -> Option<Problem> {
    match hcl::parse(text) {
        Ok(_) => None,
        Err(hcl::Error::Parse(e)) => {
            let loc = e.location();
            Some(Problem {
                line: loc.line().saturating_sub(1),
                col: loc.column().saturating_sub(1),
                message: e.message().to_string(),
            })
        }
        Err(e) => Some(Problem {
            line: 0,
            col: 0,
            message: e.to_string(),
        }),
    }
}

fn check_yaml(text: &str) -> Option<Problem> {
    match yaml_rust2::YamlLoader::load_from_str(text) {
        Ok(_) => None,
        Err(e) => {
            // Work from the byte index: the scanner's own column counts
            // chars, and everything downstream wants bytes.
            let mut at = e.marker().index().min(text.len());
            while at > 0 && !text.is_char_boundary(at) {
                at -= 1;
            }
            let line = text[..at].matches('\n').count();
            let col = at - text[..at].rfind('\n').map_or(0, |p| p + 1);
            Some(Problem {
                line,
                col,
                message: e.info().to_string(),
            })
        }
    }
}

fn check_json(text: &str) -> Option<Problem> {
    match serde_json::from_str::<serde::de::IgnoredAny>(text) {
        Ok(_) => None,
        Err(e) => Some(Problem {
            line: e.line().saturating_sub(1),
            col: e.column().saturating_sub(1),
            message: e.to_string(),
        }),
    }
}

/// The byte range to underline for `p`: from the reported column to the end
/// of the token there (at least one char). An error reported at or past the
/// end of its line (an unexpected newline) underlines the line's content
/// instead. `None` when there is nothing visible to underline.
pub fn problem_range(text: &str, p: &Problem) -> Option<Range<usize>> {
    let mut start = 0usize;
    for _ in 0..p.line {
        start += text[start..].find('\n')? + 1;
    }
    let end_of_line = text[start..].find('\n').map_or(text.len(), |q| start + q);
    let line = text[start..end_of_line].trim_end_matches('\r');
    let line_end = start + line.len();

    let mut at = start + p.col.min(line.len());
    while at < line_end && !text.is_char_boundary(at) {
        at += 1;
    }
    if at >= line_end {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lead = line.len() - line.trim_start().len();
        return Some(start + lead..start + lead + trimmed.len());
    }

    let first = text[at..].chars().next()?;
    let mut end = at + first.len_utf8();
    if first.is_alphanumeric() || first == '_' {
        while end < line_end {
            let c = text[end..].chars().next()?;
            if !(c.is_alphanumeric() || c == '_' || c == '-') {
                break;
            }
            end += c.len_utf8();
        }
    }
    Some(at..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_terraform_has_no_problem() {
        let tf = "resource \"aws_instance\" \"a\" {\n  ami = var.ami\n  tags = { Name = \"x\" }\n}\n";
        assert_eq!(check(tf, "tf"), None);
    }

    #[test]
    fn a_missing_closing_brace_is_reported() {
        let p = check("resource \"a\" \"b\" {\n  x = 1\n", "tf").expect("unclosed block");
        assert!(!p.message.is_empty());
    }

    #[test]
    fn a_missing_equals_is_reported_on_its_line() {
        let p = check("a = 1\nb 2\nc = 3\n", "tf").expect("bad attribute");
        assert_eq!(p.line, 1, "{p:?}");
    }

    #[test]
    fn blank_and_unsupported_files_are_not_checked() {
        assert_eq!(check("  \n", "tf"), None);
        assert_eq!(check("{{{", "sh"), None);
        assert!(!can_check("sh"));
        assert!(can_check("tfvars"));
    }

    #[test]
    fn json_trailing_comma_is_reported() {
        let p = check("{\n  \"a\": 1,\n}\n", "json").expect("trailing comma");
        assert_eq!(p.line, 2, "{p:?}");
    }

    #[test]
    fn valid_ansible_has_no_problem() {
        let y = "- hosts: all\n  tasks:\n    - name: x\n      debug:\n        msg: \"{{ var }}\"\n";
        assert_eq!(check(y, "yml"), None);
        assert_eq!(check("a: 1\n---\nb: 2\n", "yaml"), None);
    }

    #[test]
    fn bad_yaml_indentation_is_reported() {
        let p = check("a:\n  b: 1\n c: 2\n", "yaml").expect("bad indent");
        assert!(p.line >= 1, "{p:?}");
    }

    #[test]
    fn an_unquoted_jinja_value_is_a_known_blind_spot() {
        // `{{ var }}` is valid YAML (a flow mapping used as a key); Ansible
        // rejects it later, at the data level. This is a syntax check only,
        // so it must NOT claim to catch it.
        assert_eq!(check("msg: {{ var }}\n", "yml"), None);
    }

    #[test]
    fn a_tab_in_yaml_is_reported_with_a_valid_range() {
        let text = "a:\n\tb: 1\n";
        let p = check(text, "yaml").expect("tab indent");
        assert_eq!(p.line, 1, "{p:?}");
        if let Some(r) = problem_range(text, &p) {
            assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end));
        }
    }

    #[test]
    fn non_ascii_yaml_errors_do_not_panic() {
        let text = "é: 1\nü: [1, 2\n";
        let p = check(text, "yaml").expect("unclosed flow");
        if let Some(r) = problem_range(text, &p) {
            assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end));
        }
    }

    #[test]
    fn valid_json_has_no_problem() {
        assert_eq!(check("{\"a\": [1, 2, {\"b\": null}]}", "json"), None);
    }

    #[test]
    fn the_range_covers_the_token_at_the_error() {
        let text = "a = 1\nb 2\n";
        let p = Problem { line: 1, col: 2, message: String::new() };
        assert_eq!(&text[problem_range(text, &p).unwrap()], "2");
        let p = Problem { line: 0, col: 0, message: String::new() };
        assert_eq!(&text[problem_range(text, &p).unwrap()], "a");
    }

    #[test]
    fn an_error_past_the_line_end_underlines_the_line() {
        let text = "  a = (\nb = 1\n";
        let p = Problem { line: 0, col: 99, message: String::new() };
        assert_eq!(&text[problem_range(text, &p).unwrap()], "a = (");
    }

    #[test]
    fn a_column_inside_a_multibyte_char_does_not_panic() {
        let text = "é = \"ü\"\n";
        for col in 0..12 {
            let p = Problem { line: 0, col, message: String::new() };
            if let Some(r) = problem_range(text, &p) {
                assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end));
            }
        }
    }

    #[test]
    fn a_line_past_the_end_is_none() {
        let p = Problem { line: 9, col: 0, message: String::new() };
        assert_eq!(problem_range("a\n", &p), None);
    }

    #[test]
    fn non_ascii_terraform_errors_map_to_valid_ranges() {
        let text = "é = 1\nü 2\n";
        let p = check(text, "tf").expect("bad attribute");
        if let Some(r) = problem_range(text, &p) {
            assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end));
        }
    }
}
