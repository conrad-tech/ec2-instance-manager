//! Lightweight syntax tokenizer for the in-app file editor.
//!
//! Pure text in, token ranges out, so the GUI only has to map `Kind` to a
//! colour and so the parts that can be wrong are settled by tests.
//!
//! * One linear pass, no per-character allocation: the editor re-runs this on
//!   every edit, on the UI thread that also draws the terminal.
//! * Tokens are contiguous and cover the whole text, so a layout job built by
//!   appending each slice has the same byte offsets as the source (the find
//!   highlights rely on that).
//! * Bracket pairing is checked for code-like extensions only. In prose or
//!   shell (`case x in a)`) an unmatched bracket is normal, and underlining it
//!   would be noise.

use std::ops::Range;

/// What a token is, for colouring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Normal,
    Comment,
    Str,
    /// `${...}` / `%{...}` inside a Terraform string.
    Interp,
    Number,
    Keyword,
    /// `true`, `false`, `null`.
    Constant,
    Type,
    /// A block type or nested block name: `resource`, `ingress`.
    Block,
    /// An attribute name: the `name` in `name = "x"`.
    Attr,
    Func,
    /// A reference root: `var`, `local`, `module`, `each`...
    Builtin,
    /// A paired bracket; the value is its nesting depth modulo 3.
    Bracket(u8),
    /// A bracket with no partner.
    BadBracket,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    pub kind: Kind,
}

/// Files past this size are not tokenized (one plain token).
pub const MAX_HIGHLIGHT_BYTES: usize = 1_000_000;

/// A broad union of keywords across common languages. Good enough for
/// at-a-glance highlighting without per-language grammars.
pub const KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "pub", "use", "mod", "impl", "struct", "enum",
    "trait", "match", "if", "else", "for", "while", "loop", "return",
    "break", "continue", "const", "static", "ref", "as", "where", "self",
    "super", "crate", "move", "async", "await", "dyn", "type", "in",
    "def", "class", "import", "from", "lambda", "pass", "with", "try",
    "except", "finally", "raise", "yield", "global", "nonlocal", "and",
    "or", "not", "is", "None", "True", "False", "var", "function",
    "func", "package", "interface", "public", "private", "protected",
    "void", "int", "bool", "string", "new", "null", "nil", "true",
    "false", "then", "do", "end", "echo", "export", "local", "elif",
    "fi", "esac", "case", "default", "switch", "extends", "implements",
];

const TF_BLOCKS: &[&str] = &[
    "resource", "data", "variable", "output", "locals", "module", "provider",
    "terraform", "moved", "import", "check", "removed", "dynamic", "lifecycle",
    "provisioner", "connection", "backend", "required_providers",
];
const TF_KEYWORDS: &[&str] = &["for", "in", "if", "else", "endif", "endfor"];
const TF_CONSTANTS: &[&str] = &["true", "false", "null"];
const TF_TYPES: &[&str] = &[
    "string", "number", "bool", "list", "map", "set", "object", "tuple", "any",
];
const TF_ROOTS: &[&str] = &[
    "var", "local", "module", "data", "each", "count", "self", "path",
    "terraform",
];

/// Comment prefixes for a file extension.
pub fn comment_prefixes(ext: &str) -> &'static [&'static str] {
    match ext {
        "rs" | "c" | "cc" | "cpp" | "h" | "hpp" | "js" | "ts" | "jsx" | "tsx"
        | "go" | "java" | "kt" | "swift" | "json" | "css" | "scss" => &["//"],
        "py" | "sh" | "bash" | "zsh" | "yaml" | "yml" | "toml" | "conf"
        | "ini" | "cfg" | "rb" | "pl" | "tf" | "tfvars" | "hcl"
        | "dockerfile" => &["#"],
        "sql" | "lua" | "hs" => &["--"],
        _ => &["#", "//"],
    }
}

/// Terraform / HCL files get the richer tokenizer.
pub fn is_terraform(ext: &str) -> bool {
    matches!(ext, "tf" | "tfvars" | "hcl")
}

fn has_block_comments(ext: &str) -> bool {
    is_terraform(ext)
        || matches!(
            ext,
            "rs" | "c" | "cc" | "cpp" | "h" | "hpp" | "js" | "ts" | "jsx"
                | "tsx" | "go" | "java" | "kt" | "swift" | "css" | "scss"
        )
}

/// Extensions where bracket pairing is reliable enough to colour and flag.
pub fn checks_brackets(ext: &str) -> bool {
    is_terraform(ext)
        || matches!(
            ext,
            "json" | "rs" | "js" | "ts" | "jsx" | "tsx" | "go" | "java" | "c"
                | "cc" | "cpp" | "h" | "hpp" | "css" | "scss" | "kt" | "swift"
        )
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Move `j` forward to the next char boundary.
fn to_boundary(text: &str, j: &mut usize) {
    while *j < text.len() && !text.is_char_boundary(*j) {
        *j += 1;
    }
}

fn line_end(b: &[u8], from: usize) -> usize {
    b[from.min(b.len())..]
        .iter()
        .position(|&c| c == b'\n')
        .map_or(b.len(), |p| from + p)
}

/// End (exclusive) of a Terraform string that starts at the opening quote
/// `i`: past the closing quote, or at the end of the line when unclosed.
fn skip_string_tf(text: &str, i: usize) -> usize {
    let b = text.as_bytes();
    let n = b.len();
    let mut j = i + 1;
    while j < n {
        match b[j] {
            b'\\' if j + 1 < n && b[j + 1] != b'\n' => {
                j += 2;
                to_boundary(text, &mut j);
            }
            b'"' => return j + 1,
            b'\n' => return j,
            c @ (b'$' | b'%')
                if j + 1 < n && b[j + 1] == b'{' && !(j > 0 && b[j - 1] == c) =>
            {
                j = skip_interp(text, j);
            }
            _ => j += 1,
        }
    }
    n
}

/// End (exclusive) of a `${...}` that starts at `i` (the `$`), counting
/// nested braces and skipping nested strings. Unclosed stops at end of line.
fn skip_interp(text: &str, i: usize) -> usize {
    let b = text.as_bytes();
    let n = b.len();
    let mut depth = 1usize;
    let mut k = i + 2;
    while k < n {
        match b[k] {
            b'\n' => return k,
            b'{' => {
                depth += 1;
                k += 1;
            }
            b'}' => {
                depth -= 1;
                k += 1;
                if depth == 0 {
                    return k;
                }
            }
            b'"' => k = skip_string_tf(text, k),
            _ => k += 1,
        }
    }
    n
}

struct Out {
    tokens: Vec<Token>,
}

impl Out {
    fn push(&mut self, kind: Kind, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let mergeable = !matches!(kind, Kind::Bracket(_) | Kind::BadBracket);
        if mergeable {
            if let Some(last) = self.tokens.last_mut() {
                if last.kind == kind && last.range.end == start {
                    last.range.end = end;
                    return;
                }
            }
        }
        self.tokens.push(Token { range: start..end, kind });
    }
}

/// Tokenize `text` (`ext` lower-case, no dot).
pub fn tokenize(text: &str, ext: &str) -> Vec<Token> {
    let n = text.len();
    if n > MAX_HIGHLIGHT_BYTES {
        return vec![Token { range: 0..n, kind: Kind::Normal }];
    }
    let tf = is_terraform(ext);
    let marks = comment_prefixes(ext);
    let block_comments = has_block_comments(ext);
    let check = checks_brackets(ext);
    let b = text.as_bytes();
    let mut out = Out { tokens: Vec::new() };
    // (opening byte, index of its token)
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut at_line_start = true;
    let mut i = 0usize;

    while i < n {
        let c = b[i];

        if c == b'\n' {
            out.push(Kind::Normal, i, i + 1);
            i += 1;
            at_line_start = true;
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\r' {
            let mut j = i;
            while j < n && matches!(b[j], b' ' | b'\t' | b'\r') {
                j += 1;
            }
            out.push(Kind::Normal, i, j);
            i = j;
            continue;
        }

        // Comments.
        if block_comments && c == b'/' && b.get(i + 1) == Some(&b'*') {
            let end = text[i + 2..].find("*/").map_or(n, |p| i + 2 + p + 2);
            out.push(Kind::Comment, i, end);
            i = end;
            at_line_start = false;
            continue;
        }
        if marks.iter().any(|m| b[i..].starts_with(m.as_bytes())) {
            let j = line_end(b, i);
            out.push(Kind::Comment, i, j);
            i = j;
            continue;
        }

        // Terraform heredoc: <<EOT ... EOT
        if tf && c == b'<' && b.get(i + 1) == Some(&b'<') {
            let mut j = i + 2;
            if b.get(j) == Some(&b'-') {
                j += 1;
            }
            let id_start = j;
            while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if j > id_start && !b[id_start].is_ascii_digit() {
                let ident = &text[id_start..j];
                let mut end = n;
                let mut ls = line_end(b, j) + 1;
                while ls <= n {
                    let le = line_end(b, ls);
                    if text[ls.min(n)..le].trim() == ident {
                        end = le;
                        break;
                    }
                    if le >= n {
                        break;
                    }
                    ls = le + 1;
                }
                out.push(Kind::Str, i, end);
                i = end;
                at_line_start = false;
                continue;
            }
        }

        // Strings.
        if c == b'"' && tf {
            let end = skip_string_tf(text, i);
            // Split out interpolations so they can be coloured separately.
            let mut seg = i;
            let mut j = i + 1;
            while j < end {
                let cj = b[j];
                if (cj == b'$' || cj == b'%')
                    && j + 1 < end
                    && b[j + 1] == b'{'
                    && !(j > 0 && b[j - 1] == cj)
                {
                    let e = skip_interp(text, j).min(end);
                    out.push(Kind::Str, seg, j);
                    out.push(Kind::Interp, j, e);
                    seg = e;
                    j = e;
                } else if cj == b'\\' && j + 1 < end {
                    j += 2;
                    to_boundary(text, &mut j);
                } else {
                    j += 1;
                }
            }
            out.push(Kind::Str, seg, end);
            i = end;
            at_line_start = false;
            continue;
        }
        if c == b'"' || (c == b'\'' && !tf) {
            let quote = c;
            let mut j = i + 1;
            while j < n {
                if b[j] == b'\\' && j + 1 < n && b[j + 1] != b'\n' {
                    j += 2;
                    to_boundary(text, &mut j);
                } else if b[j] == quote {
                    j += 1;
                    break;
                } else if b[j] == b'\n' {
                    break;
                } else {
                    j += 1;
                }
            }
            let j = j.min(n);
            out.push(Kind::Str, i, j);
            i = j;
            at_line_start = false;
            continue;
        }

        // Brackets.
        if check && matches!(c, b'(' | b'[' | b'{') {
            out.tokens.push(Token {
                range: i..i + 1,
                kind: Kind::Bracket((stack.len() % 3) as u8),
            });
            stack.push((c, out.tokens.len() - 1));
            i += 1;
            at_line_start = false;
            continue;
        }
        if check && matches!(c, b')' | b']' | b'}') {
            let want = match c {
                b')' => b'(',
                b']' => b'[',
                _ => b'{',
            };
            match stack.iter().rposition(|&(o, _)| o == want) {
                Some(p) => {
                    // Anything opened after the partner was never closed.
                    for &(_, idx) in &stack[p + 1..] {
                        out.tokens[idx].kind = Kind::BadBracket;
                    }
                    stack.truncate(p);
                    out.tokens.push(Token {
                        range: i..i + 1,
                        kind: Kind::Bracket((p % 3) as u8),
                    });
                }
                None => out.tokens.push(Token {
                    range: i..i + 1,
                    kind: Kind::BadBracket,
                }),
            }
            i += 1;
            at_line_start = false;
            continue;
        }

        // Numbers.
        if c.is_ascii_digit() {
            let mut j = i;
            while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'.' || b[j] == b'_') {
                j += 1;
            }
            out.push(Kind::Number, i, j);
            i = j;
            at_line_start = false;
            continue;
        }

        // Identifiers.
        if is_ident_start(c) {
            let mut j = i;
            while j < n {
                if is_ident_byte(b[j]) {
                    j += 1;
                } else if tf
                    && b[j] == b'-'
                    && j + 1 < n
                    && (b[j + 1].is_ascii_alphanumeric() || b[j + 1] == b'_')
                {
                    j += 1;
                } else {
                    break;
                }
            }
            let word = &text[i..j];
            let kind = if tf {
                classify_tf(text, word, j, at_line_start)
            } else if KEYWORDS.contains(&word) {
                Kind::Keyword
            } else {
                Kind::Normal
            };
            out.push(kind, i, j);
            i = j;
            at_line_start = false;
            continue;
        }

        // Anything else: one char of plain text.
        let mut j = i + 1;
        to_boundary(text, &mut j);
        out.push(Kind::Normal, i, j);
        i = j;
        at_line_start = false;
    }

    for &(_, idx) in &stack {
        out.tokens[idx].kind = Kind::BadBracket;
    }
    out.tokens
}

fn classify_tf(text: &str, word: &str, end: usize, at_line_start: bool) -> Kind {
    let b = text.as_bytes();
    let mut k = end;
    while k < b.len() && matches!(b[k], b' ' | b'\t') {
        k += 1;
    }
    let next = b.get(k).copied();
    let after = b.get(k + 1).copied();

    // `name = value` (but not `==` or `=>`).
    if next == Some(b'=') && after != Some(b'=') && after != Some(b'>') {
        return Kind::Attr;
    }
    if at_line_start {
        let le = line_end(b, end);
        let rest = text[end..le].trim_end();
        if rest.ends_with('{') && !rest.contains('=') {
            return Kind::Block;
        }
        if TF_BLOCKS.contains(&word) && next.is_none_or(|c| c == b'\n' || c == b'"' || c == b'{') {
            return Kind::Block;
        }
    }
    if next == Some(b'(') {
        return Kind::Func;
    }
    if TF_KEYWORDS.contains(&word) {
        return Kind::Keyword;
    }
    if TF_CONSTANTS.contains(&word) {
        return Kind::Constant;
    }
    if TF_TYPES.contains(&word) {
        return Kind::Type;
    }
    if TF_ROOTS.contains(&word) && next == Some(b'.') && k == end {
        return Kind::Builtin;
    }
    Kind::Normal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds_of<'a>(text: &'a str, ext: &str) -> Vec<(&'a str, Kind)> {
        tokenize(text, ext)
            .into_iter()
            .map(|t| (&text[t.range], t.kind))
            .collect()
    }

    fn assert_covers(text: &str, ext: &str) {
        let toks = tokenize(text, ext);
        let mut at = 0;
        for t in &toks {
            assert_eq!(t.range.start, at, "gap or overlap in {ext}: {text:?}");
            assert!(text.is_char_boundary(t.range.start) && text.is_char_boundary(t.range.end));
            at = t.range.end;
        }
        assert_eq!(at, text.len(), "tokens stop short in {ext}: {text:?}");
    }

    fn bad_brackets(text: &str, ext: &str) -> Vec<usize> {
        tokenize(text, ext)
            .into_iter()
            .filter(|t| t.kind == Kind::BadBracket)
            .map(|t| t.range.start)
            .collect()
    }

    #[test]
    fn tokens_always_cover_the_text() {
        for ext in ["tf", "rs", "sh", "json", "txt"] {
            for text in [
                "",
                "a = \"x\\\ny\" # c\n",
                "résumé = \"héllo \\é\" { ( [",
                "x = \"${lookup(var.m, \"k\")}\"\n",
                "<<EOT\nbody\nEOT\nafter",
                "/* open",
                "\"unterminated",
                "'\\",
            ] {
                assert_covers(text, ext);
            }
        }
    }

    #[test]
    fn balanced_brackets_are_not_flagged() {
        assert!(bad_brackets("a { b = [1, (2)] }\n", "tf").is_empty());
    }

    #[test]
    fn an_unclosed_opener_is_flagged() {
        let text = "resource \"x\" \"y\" {\n  a = [1, 2\n}\n";
        let bad = bad_brackets(text, "tf");
        assert_eq!(bad, vec![text.find('[').unwrap()]);
    }

    #[test]
    fn a_closer_with_no_opener_is_flagged() {
        let text = "a = 1 }\n";
        assert_eq!(bad_brackets(text, "tf"), vec![text.find('}').unwrap()]);
    }

    #[test]
    fn a_closer_skips_unclosed_openers_above_its_partner() {
        let text = "{ ( }";
        // `(` was never closed; `{`/`}` pair.
        assert_eq!(bad_brackets(text, "tf"), vec![2]);
    }

    #[test]
    fn brackets_in_strings_and_comments_do_not_count() {
        assert!(bad_brackets("a = \"{\" # (\n/* [ */\n", "tf").is_empty());
    }

    #[test]
    fn brackets_inside_an_interpolation_do_not_count() {
        assert!(bad_brackets("a = \"${var.x}\"\n", "tf").is_empty());
    }

    #[test]
    fn depth_colours_cycle_and_a_pair_shares_one() {
        let toks: Vec<Kind> = tokenize("{{{{}}}}", "tf").into_iter().map(|t| t.kind).collect();
        assert_eq!(
            toks,
            vec![
                Kind::Bracket(0), Kind::Bracket(1), Kind::Bracket(2), Kind::Bracket(0),
                Kind::Bracket(0), Kind::Bracket(2), Kind::Bracket(1), Kind::Bracket(0),
            ]
        );
    }

    #[test]
    fn prose_and_shell_are_not_bracket_checked() {
        assert!(bad_brackets("case $x in a) echo ;;\n", "sh").is_empty());
        assert!(bad_brackets("smile :)\n", "txt").is_empty());
    }

    #[test]
    fn terraform_block_attr_and_references() {
        let k = kinds_of("resource \"aws_instance\" \"a\" {\n  ami = var.ami\n}\n", "tf");
        assert!(k.contains(&("resource", Kind::Block)));
        assert!(k.contains(&("ami", Kind::Attr)));
        assert!(k.contains(&("var", Kind::Builtin)));
    }

    #[test]
    fn a_nested_block_name_is_a_block() {
        let k = kinds_of("  ingress {\n  }\n", "tf");
        assert!(k.contains(&("ingress", Kind::Block)));
    }

    #[test]
    fn equality_is_not_an_attribute() {
        let k = kinds_of("x = a == b\n", "tf");
        let attrs: Vec<_> = k.iter().filter(|(_, kind)| *kind == Kind::Attr).collect();
        assert_eq!(attrs, vec![&("x", Kind::Attr)]);
    }

    #[test]
    fn functions_types_and_constants() {
        let k = kinds_of("a = lookup(m, \"k\")\nt = string\nb = true\n", "tf");
        assert!(k.contains(&("lookup", Kind::Func)));
        assert!(k.contains(&("string", Kind::Type)));
        assert!(k.contains(&("true", Kind::Constant)));
    }

    #[test]
    fn a_quoted_string_inside_an_interpolation_does_not_end_the_string() {
        let text = "a = \"x ${lookup(m, \"k\")} y\"\nb = 1\n";
        let k = kinds_of(text, "tf");
        assert!(k.contains(&("${lookup(m, \"k\")}", Kind::Interp)));
        assert!(k.contains(&("b", Kind::Attr)), "{k:?}");
    }

    #[test]
    fn an_escaped_interpolation_is_plain_string() {
        let k = kinds_of("a = \"$${x}\"\n", "tf");
        assert!(!k.iter().any(|(_, kind)| *kind == Kind::Interp));
    }

    #[test]
    fn a_heredoc_is_one_string_and_brackets_inside_are_ignored() {
        let text = "a = <<-EOT\n  { ( [\n  EOT\nb = 1\n";
        let k = kinds_of(text, "tf");
        assert!(k.iter().any(|(s, kind)| *kind == Kind::Str && s.contains("{ ( [")));
        assert!(k.contains(&("b", Kind::Attr)));
        assert!(bad_brackets(text, "tf").is_empty());
    }

    #[test]
    fn block_comments_span_lines() {
        let k = kinds_of("a /* x\n{ */ b", "tf");
        assert!(k.iter().any(|(s, kind)| *kind == Kind::Comment && s.contains('{')));
    }

    #[test]
    fn an_unterminated_string_stops_at_the_line_end() {
        let k = kinds_of("a = \"oops\nb = 1\n", "tf");
        assert!(k.contains(&("b", Kind::Attr)));
    }

    #[test]
    fn generic_languages_keep_keywords_and_numbers() {
        let k = kinds_of("fn main() { let x = 42; }", "rs");
        assert!(k.contains(&("fn", Kind::Keyword)));
        assert!(k.contains(&("42", Kind::Number)));
    }

    #[test]
    fn a_large_file_tokenizes_quickly_and_linearly() {
        let line = "resource \"aws_instance\" \"web\" { ami = \"${var.ami}\" # test (x)\n";
        let text = line.repeat(18_000); // just over the cap
        let text = &text[..MAX_HIGHLIGHT_BYTES - 10];
        let start = std::time::Instant::now();
        let toks = tokenize(text, "tf");
        assert!(!toks.is_empty());
        assert!(start.elapsed().as_secs() < 5, "tokenizer is not linear");
    }
}
