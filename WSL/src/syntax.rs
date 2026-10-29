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
    if matches!(ext, "yaml" | "yml") {
        return tokenize_yaml(text);
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

// ---------------------------------------------------------------------------
// YAML (Ansible playbooks, compose files)
// ---------------------------------------------------------------------------

fn tokenize_yaml(text: &str) -> Vec<Token> {
    let b = text.as_bytes();
    let mut out = Out { tokens: Vec::new() };
    let mut block_parent: Option<usize> = None;
    let mut ls = 0usize;
    loop {
        let nl = text[ls..].find('\n').map(|p| ls + p);
        let le = nl.unwrap_or(text.len());
        let mut ce = le;
        if ce > ls && b[ce - 1] == b'\r' {
            ce -= 1;
        }
        yaml_line(text, ls, ce, &mut block_parent, &mut out);
        out.push(Kind::Normal, ce, le);
        match nl {
            Some(p) => {
                out.push(Kind::Normal, p, p + 1);
                ls = p + 1;
            }
            None => break,
        }
    }
    out.tokens
}

fn skip_blanks(b: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i
}

fn yaml_line(text: &str, ls: usize, ce: usize, bp: &mut Option<usize>, out: &mut Out) {
    let b = text.as_bytes();
    let mut i = skip_blanks(b, ls, ce);
    out.push(Kind::Normal, ls, i);
    let indent = i - ls;
    if i >= ce {
        return;
    }
    if let Some(p) = *bp {
        if indent > p {
            emit_jinja(text, i, ce, Kind::Normal, out);
            return;
        }
        *bp = None;
    }
    if b[i] == b'#' {
        out.push(Kind::Comment, i, ce);
        return;
    }
    if indent == 0
        && (text[i..ce].starts_with("---") || text[i..ce].starts_with("..."))
        && (ce - i == 3 || b[i + 3] == b' ')
    {
        out.push(Kind::Keyword, i, i + 3);
        let j = skip_blanks(b, i + 3, ce);
        out.push(Kind::Normal, i + 3, j);
        yaml_value(text, j, ce, indent, bp, out);
        return;
    }
    while i < ce && b[i] == b'-' && (i + 1 == ce || b[i + 1] == b' ') {
        out.push(Kind::Keyword, i, i + 1);
        let j = skip_blanks(b, i + 1, ce);
        out.push(Kind::Normal, i + 1, j);
        i = j;
    }
    if i >= ce {
        return;
    }
    if let Some((kend, colon)) = yaml_key(text, i, ce) {
        out.push(Kind::Attr, i, kend);
        out.push(Kind::Normal, kend, colon + 1);
        let j = skip_blanks(b, colon + 1, ce);
        out.push(Kind::Normal, colon + 1, j);
        i = j;
    }
    yaml_value(text, i, ce, indent, bp, out);
}

/// `(end of key, index of its colon)` when the text at `i` is `key:`.
fn yaml_key(text: &str, i: usize, ce: usize) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    if b[i] == b'"' || b[i] == b'\'' {
        let end = quote_end(b, i, ce);
        if end > ce || end <= i + 1 || b[end - 1] != b[i] {
            return None;
        }
        let k = skip_blanks(b, end, ce);
        if k < ce && b[k] == b':' && (k + 1 == ce || b[k + 1] == b' ') {
            return Some((end, k));
        }
        return None;
    }
    if matches!(b[i], b'{' | b'[' | b'|' | b'>' | b'&' | b'*' | b'!' | b'%' | b'@' | b'`') {
        return None;
    }
    let mut j = i;
    while j < ce {
        if b[j] == b':' && (j + 1 == ce || b[j + 1] == b' ') {
            let kend = i + text[i..j].trim_end().len();
            return if kend == i { None } else { Some((kend, j)) };
        }
        if b[j] == b'#' && j > i && b[j - 1] == b' ' {
            return None;
        }
        j += 1;
    }
    None
}

/// End (exclusive) of the quoted string starting at `j`; `limit` + 1 never
/// happens: an unterminated string runs to `limit`.
fn quote_end(b: &[u8], j: usize, limit: usize) -> usize {
    let q = b[j];
    let mut k = j + 1;
    while k < limit {
        if q == b'"' && b[k] == b'\\' {
            k += 2;
            continue;
        }
        if b[k] == q {
            if q == b'\'' && k + 1 < limit && b[k + 1] == q {
                k += 2;
                continue;
            }
            return k + 1;
        }
        k += 1;
    }
    limit
}

/// Where a trailing ` # comment` begins (or `ce`), ignoring quoted text.
fn value_end(b: &[u8], i: usize, ce: usize) -> usize {
    let mut j = i;
    while j < ce {
        match b[j] {
            b'"' | b'\'' if j == i || matches!(b[j - 1], b' ' | b'\t' | b'[' | b'{' | b',') => {
                j = quote_end(b, j, ce);
            }
            b'#' if j > i && matches!(b[j - 1], b' ' | b'\t') => return j,
            _ => j += 1,
        }
    }
    ce
}

/// Push `s..e` as `base`, with `{{ }}` / `{% %}` / `{# #}` as `Interp`.
fn emit_jinja(text: &str, s: usize, e: usize, base: Kind, out: &mut Out) {
    let b = text.as_bytes();
    let mut i = s;
    let mut run = s;
    while i < e {
        if b[i] == b'{' && i + 1 < e && matches!(b[i + 1], b'{' | b'%' | b'#') {
            let close = match b[i + 1] {
                b'{' => "}}",
                b'%' => "%}",
                _ => "#}",
            };
            let end = text[i + 2..e].find(close).map_or(e, |p| i + 2 + p + 2);
            out.push(base, run, i);
            out.push(Kind::Interp, i, end);
            i = end;
            run = end;
        } else {
            i += 1;
        }
    }
    out.push(base, run, e);
}

fn yaml_word_kind(word: &str, single: bool) -> Kind {
    let first = word.as_bytes()[0];
    if matches!(first, b'&' | b'*' | b'!') {
        return Kind::Type;
    }
    if single {
        let low = word.to_ascii_lowercase();
        if matches!(
            low.as_str(),
            "true" | "false" | "null" | "~" | "yes" | "no" | "on" | "off"
        ) {
            return Kind::Constant;
        }
        if (first.is_ascii_digit() || matches!(first, b'-' | b'+' | b'.'))
            && word.parse::<f64>().is_ok()
        {
            return Kind::Number;
        }
    }
    Kind::Str
}

fn yaml_value(text: &str, i: usize, ce: usize, indent: usize, bp: &mut Option<usize>, out: &mut Out) {
    let b = text.as_bytes();
    if i >= ce {
        return;
    }
    let vend = value_end(b, i, ce);
    let vt = text[i..vend].trim_end();
    let vend2 = i + vt.len();
    if vt.is_empty() {
        out.push(Kind::Normal, i, vend);
        out.push(Kind::Comment, vend, ce);
        return;
    }
    let vb = vt.as_bytes();
    if matches!(vb[0], b'|' | b'>')
        && vb.len() <= 4
        && vb[1..].iter().all(|c| matches!(c, b'+' | b'-' | b'0'..=b'9'))
    {
        out.push(Kind::Keyword, i, vend2);
        *bp = Some(indent);
        out.push(Kind::Normal, vend2, vend);
        out.push(Kind::Comment, vend, ce);
        return;
    }
    let single = !vt.contains(' ');
    let mut j = i;
    while j < vend2 {
        let c = b[j];
        if c == b' ' || c == b'\t' {
            let k = skip_blanks(b, j, vend2);
            out.push(Kind::Normal, j, k);
            j = k;
        } else if c == b'"' || c == b'\'' {
            let end = quote_end(b, j, vend2);
            emit_jinja(text, j, end, Kind::Str, out);
            j = end;
        } else if c == b'{' && j + 1 < vend2 && matches!(b[j + 1], b'{' | b'%' | b'#') {
            let close = match b[j + 1] {
                b'{' => "}}",
                b'%' => "%}",
                _ => "#}",
            };
            let end = text[j + 2..vend2].find(close).map_or(vend2, |p| j + 2 + p + 2);
            out.push(Kind::Interp, j, end);
            j = end;
        } else if matches!(c, b'[' | b']' | b'{' | b'}' | b',') {
            out.push(Kind::Normal, j, j + 1);
            j += 1;
        } else {
            let mut k = j;
            while k < vend2
                && !matches!(b[k], b' ' | b'\t' | b',' | b'[' | b']' | b'}')
                && !(b[k] == b'{' && k + 1 < vend2 && matches!(b[k + 1], b'{' | b'%' | b'#'))
            {
                k += 1;
            }
            if k == j {
                k = j + 1;
            }
            out.push(yaml_word_kind(&text[j..k], single), j, k);
            j = k;
        }
    }
    out.push(Kind::Normal, vend2, vend);
    out.push(Kind::Comment, vend, ce);
}

// ---------------------------------------------------------------------------
// Indentation guides
// ---------------------------------------------------------------------------

/// Leading-space width of each line, with blank lines taking the smaller of
/// their neighbours' indents so guides run unbroken through gaps.
pub fn effective_indents(text: &str) -> Vec<usize> {
    let raw: Vec<Option<usize>> = text
        .split('\n')
        .map(|l| {
            let l = l.trim_end_matches('\r');
            if l.trim().is_empty() {
                None
            } else {
                Some(l.len() - l.trim_start_matches([' ', '\t']).len())
            }
        })
        .collect();
    let mut next = vec![None; raw.len()];
    let mut n = None;
    for (i, r) in raw.iter().enumerate().rev() {
        if r.is_some() {
            n = *r;
        }
        next[i] = n;
    }
    let mut prev = None;
    raw.iter()
        .enumerate()
        .map(|(i, r)| {
            if r.is_some() {
                prev = *r;
            }
            match r {
                Some(v) => *v,
                None => match (prev, next[i]) {
                    (Some(a), Some(c)) => a.min(c),
                    (Some(a), None) => a,
                    (None, Some(c)) => c,
                    (None, None) => 0,
                },
            }
        })
        .collect()
}

/// The indent step a file uses: its smallest non-zero indent, 2..=8.
pub fn indent_unit(indents: &[usize]) -> usize {
    indents.iter().copied().filter(|&n| n > 0).min().unwrap_or(2).clamp(2, 8)
}

/// The bracket next to byte offset `at` (after it, else before it) and its
/// partner, as byte offsets `(this, partner)`. Brackets inside comments and
/// strings are ignored, as are unmatched ones (`None`).
pub fn matching_bracket(text: &str, tokens: &[Token], at: usize) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    let skip = |k: Kind| matches!(k, Kind::Comment | Kind::Str | Kind::Interp);
    let token_at = |p: usize| tokens.partition_point(|t| t.range.end <= p);
    let cand = [Some(at), at.checked_sub(1)].into_iter().flatten().find(|&p| {
        p < b.len()
            && matches!(b[p], b'(' | b')' | b'[' | b']' | b'{' | b'}')
            && tokens.get(token_at(p)).is_some_and(|t| !skip(t.kind))
    })?;
    let c = b[cand];
    let (open, close, forward) = match c {
        b'(' => (b'(', b')', true),
        b'[' => (b'[', b']', true),
        b'{' => (b'{', b'}', true),
        b')' => (b'(', b')', false),
        b']' => (b'[', b']', false),
        _ => (b'{', b'}', false),
    };
    let first = token_at(cand);
    let mut depth = 0i32;
    if forward {
        for t in &tokens[first..] {
            if skip(t.kind) {
                continue;
            }
            for q in t.range.start.max(cand)..t.range.end {
                if b[q] == open {
                    depth += 1;
                } else if b[q] == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some((cand, q));
                    }
                }
            }
        }
    } else {
        for t in tokens[..=first].iter().rev() {
            if skip(t.kind) {
                continue;
            }
            for q in (t.range.start..t.range.end.min(cand + 1)).rev() {
                if b[q] == close {
                    depth += 1;
                } else if b[q] == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some((cand, q));
                    }
                }
            }
        }
    }
    None
}

/// Indent of the nearest non-blank line above `line` (0 when none): the
/// column a new line should line up with.
pub fn indent_above(text: &str, line: usize) -> usize {
    let lines: Vec<&str> = text.split('\n').take(line).collect();
    for l in lines.iter().rev() {
        let l = l.trim_end_matches('\r');
        if !l.trim().is_empty() {
            return l.len() - l.trim_start_matches([' ', '\t']).len();
        }
    }
    0
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

    fn yaml_kinds(text: &str, ext: &str) -> Vec<(String, Kind)> {
        tokenize(text, ext)
            .into_iter()
            .map(|t| (text[t.range].to_string(), t.kind))
            .collect()
    }

    #[test]
    fn yaml_tokens_are_contiguous() {
        let t = "- name: \"é {{ x }}\"\n  when: a # c\n  shell: |\n    echo {{ y }}\n";
        let mut at = 0;
        for tok in tokenize(t, "yml") {
            assert_eq!(tok.range.start, at);
            at = tok.range.end;
        }
        assert_eq!(at, t.len());
    }

    #[test]
    fn yaml_keys_dashes_strings_and_jinja_get_their_own_colours() {
        let k = yaml_kinds("- name: \"a {{ v }}\"\n  become: true\n  n: 3\n", "yml");
        assert!(k.contains(&("-".into(), Kind::Keyword)));
        assert!(k.contains(&("name".into(), Kind::Attr)));
        assert!(k.contains(&("{{ v }}".into(), Kind::Interp)));
        assert!(k.contains(&("become".into(), Kind::Attr)));
        assert!(k.contains(&("true".into(), Kind::Constant)));
        assert!(k.contains(&("3".into(), Kind::Number)));
    }

    #[test]
    fn yaml_block_scalar_content_is_not_read_as_keys() {
        let k = yaml_kinds("a: |\n  x: y\nb: 1\n", "yaml");
        assert!(!k.iter().any(|(s, kd)| s.as_str() == "x" && *kd == Kind::Attr));
        assert!(k.contains(&("b".into(), Kind::Attr)));
    }

    #[test]
    fn indents_flow_through_blank_lines() {
        assert_eq!(effective_indents("a:\n  b\n\n  c\n"), vec![0, 2, 2, 2, 2]);
        assert_eq!(indent_above("a:\n    b\n\n", 3), 4);
        assert_eq!(indent_above("x", 0), 0);
    }

    #[test]
    fn brackets_pair_across_kinds_and_skip_strings() {
        let text = "a = f(\"(\", [1])\n";
        let toks = tokenize(text, "sh");
        let open = text.find("f(").unwrap() + 1;
        let close = text.rfind(')').unwrap();
        assert_eq!(matching_bracket(text, &toks, open), Some((open, close)));
        assert_eq!(matching_bracket(text, &toks, close + 1), Some((close, open)));
        let tf = tokenize(text, "tf");
        assert_eq!(matching_bracket(text, &tf, open), Some((open, close)));
        assert_eq!(matching_bracket("x )", &tokenize("x )", "sh"), 3), None);
    }

    #[test]
    fn the_indent_unit_follows_the_file() {
        assert_eq!(indent_unit(&[0, 4, 8]), 4);
        assert_eq!(indent_unit(&[0, 2, 4]), 2);
        assert_eq!(indent_unit(&[0, 0]), 2);
    }
}
