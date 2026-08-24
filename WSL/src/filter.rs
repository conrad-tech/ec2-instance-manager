use crate::models::Instance;

#[derive(Clone, Debug, Default)]
pub struct Filters {
    pub includes: Vec<String>,
    pub excludes: Vec<String>,
    pub states: Vec<String>,
    pub only_ssm_managed: bool,
}

#[derive(Clone, Debug)]
enum SearchMatcher {
    Substring(String),
    RegexLite(Vec<Pattern>),
}

#[derive(Clone, Debug)]
struct Pattern {
    anchored_start: bool,
    anchored_end: bool,
    atoms: Vec<Atom>,
}

#[derive(Clone, Debug)]
struct Atom {
    kind: AtomKind,
    quantifier: Quantifier,
}

#[derive(Clone, Debug)]
enum AtomKind {
    Literal(char),
    Any,
}

#[derive(Clone, Debug)]
enum Quantifier {
    One,
    ZeroOrOne,
    ZeroOrMore,
    OneOrMore,
}

pub fn apply_filters(instances: &[Instance], filters: &Filters) -> Vec<Instance> {
    let include_matchers = build_matchers(&filters.includes);
    let exclude_matchers = build_matchers(&filters.excludes);

    let state_set: Vec<String> = filters
        .states
        .iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    instances
        .iter()
        .filter(|instance| {
            if filters.only_ssm_managed && !instance.ssm_managed {
                return false;
            }

            if !state_set.is_empty()
                && !state_set
                    .iter()
                    .any(|expected| instance.state.eq_ignore_ascii_case(expected))
            {
                return false;
            }

            if !include_matchers.is_empty() || !exclude_matchers.is_empty() {
                let searchable = searchable_text(instance);

                if !include_matchers
                    .iter()
                    .all(|matcher| matcher_matches(matcher, &searchable))
                {
                    return false;
                }

                if exclude_matchers
                    .iter()
                    .any(|matcher| matcher_matches(matcher, &searchable))
                {
                    return false;
                }
            }

            true
        })
        .cloned()
        .collect()
}

pub fn matching_tags(instance: &Instance, terms: &[String]) -> Vec<(String, String)> {
    let matchers = build_matchers(terms);
    if matchers.is_empty() {
        return Vec::new();
    }

    instance
        .tags
        .iter()
        .filter(|(key, value)| {
            let mut searchable = String::new();
            append(&mut searchable, key);
            append(&mut searchable, value);
            matchers
                .iter()
                .any(|matcher| matcher_matches(matcher, &searchable))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Split a `Key: value` search term into its tag key and the lowercased
/// value it looks for. `None` for anything that is not one — a plain term,
/// or a colon with nothing on one side of it.
///
/// Both the tag filter and the Match Tag column read a term through this,
/// so the column cannot end up reporting a different match than the filter
/// made.
pub fn parse_tag_term(term: &str) -> Option<(String, String)> {
    let (key, value) = term.trim().split_once(':')?;
    let key = key.trim();
    let value = value.trim().to_ascii_lowercase();
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key.to_string(), value))
}

/// The tags on `instance` that a `Key: value` term matches — the key
/// case-insensitively, the value as a substring — returned as the instance
/// spells them, which is what the Match Tag column has to show. Empty means
/// the term does not match this instance.
pub fn tag_term_matches(instance: &Instance, key: &str, value: &str) -> Vec<(String, String)> {
    instance
        .tags
        .iter()
        .filter(|(k, v)| k.eq_ignore_ascii_case(key) && v.to_ascii_lowercase().contains(value))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn build_matcher(raw: &str) -> SearchMatcher {
    let query = raw.trim().to_ascii_lowercase();
    if query.is_empty() {
        return SearchMatcher::Substring(String::new());
    }

    if looks_like_regex(&query) {
        let patterns = split_alternatives(&query)
            .into_iter()
            .map(|part| parse_pattern(&part))
            .collect();
        SearchMatcher::RegexLite(patterns)
    } else {
        SearchMatcher::Substring(query)
    }
}

fn build_matchers(values: &[String]) -> Vec<SearchMatcher> {
    values
        .iter()
        .map(|value| build_matcher(value))
        .filter(|matcher| !matches!(matcher, SearchMatcher::Substring(v) if v.is_empty()))
        .collect()
}

fn looks_like_regex(query: &str) -> bool {
    query
        .chars()
        .any(|ch| matches!(ch, '*' | '+' | '?' | '^' | '$' | '|'))
}

fn split_alternatives(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut escaped = false;

    for ch in query.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if ch == '\\' {
            escaped = true;
            current.push(ch);
            continue;
        }

        if ch == '|' {
            out.push(current);
            current = String::new();
            continue;
        }

        current.push(ch);
    }

    out.push(current);
    out
}

fn parse_pattern(raw: &str) -> Pattern {
    let mut chars = raw.chars().peekable();
    let mut atoms = Vec::new();
    let mut anchored_start = false;
    let mut anchored_end = false;

    if matches!(chars.peek(), Some('^')) {
        anchored_start = true;
        chars.next();
    }

    let mut token_chars: Vec<char> = chars.collect();
    if let Some(last) = token_chars.last() {
        if *last == '$' {
            anchored_end = true;
            token_chars.pop();
        }
    }

    let mut i = 0usize;
    while i < token_chars.len() {
        let ch = token_chars[i];

        let kind = if ch == '\\' {
            i += 1;
            let escaped = token_chars.get(i).copied().unwrap_or('\\');
            AtomKind::Literal(escaped)
        } else if ch == '.' {
            AtomKind::Any
        } else {
            AtomKind::Literal(ch)
        };

        let quantifier = if let Some(next) = token_chars.get(i + 1) {
            match next {
                '*' => {
                    i += 1;
                    Quantifier::ZeroOrMore
                }
                '+' => {
                    i += 1;
                    Quantifier::OneOrMore
                }
                '?' => {
                    i += 1;
                    Quantifier::ZeroOrOne
                }
                _ => Quantifier::One,
            }
        } else {
            Quantifier::One
        };

        atoms.push(Atom { kind, quantifier });
        i += 1;
    }

    Pattern {
        anchored_start,
        anchored_end,
        atoms,
    }
}

fn matcher_matches(matcher: &SearchMatcher, searchable: &str) -> bool {
    match matcher {
        SearchMatcher::Substring(query) => searchable.contains(query),
        SearchMatcher::RegexLite(patterns) => {
            for line in searchable.lines() {
                if patterns
                    .iter()
                    .any(|pattern| pattern_matches_line(pattern, line))
                {
                    return true;
                }
            }
            false
        }
    }
}

fn pattern_matches_line(pattern: &Pattern, line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();

    if pattern.anchored_start {
        return matches_from(pattern, &chars, 0, 0);
    }

    for start in 0..=chars.len() {
        if matches_from(pattern, &chars, 0, start) {
            return true;
        }
    }

    false
}

fn matches_from(pattern: &Pattern, input: &[char], atom_idx: usize, pos: usize) -> bool {
    if atom_idx >= pattern.atoms.len() {
        return if pattern.anchored_end {
            pos == input.len()
        } else {
            true
        };
    }

    let atom = &pattern.atoms[atom_idx];

    match atom.quantifier {
        Quantifier::One => {
            if pos >= input.len() || !atom_matches(&atom.kind, input[pos]) {
                return false;
            }
            matches_from(pattern, input, atom_idx + 1, pos + 1)
        }
        Quantifier::ZeroOrOne => {
            if matches_from(pattern, input, atom_idx + 1, pos) {
                return true;
            }
            if pos < input.len() && atom_matches(&atom.kind, input[pos]) {
                return matches_from(pattern, input, atom_idx + 1, pos + 1);
            }
            false
        }
        Quantifier::ZeroOrMore => {
            let mut max = pos;
            while max < input.len() && atom_matches(&atom.kind, input[max]) {
                max += 1;
            }
            for next_pos in (pos..=max).rev() {
                if matches_from(pattern, input, atom_idx + 1, next_pos) {
                    return true;
                }
            }
            false
        }
        Quantifier::OneOrMore => {
            if pos >= input.len() || !atom_matches(&atom.kind, input[pos]) {
                return false;
            }
            let mut max = pos + 1;
            while max < input.len() && atom_matches(&atom.kind, input[max]) {
                max += 1;
            }
            for next_pos in ((pos + 1)..=max).rev() {
                if matches_from(pattern, input, atom_idx + 1, next_pos) {
                    return true;
                }
            }
            false
        }
    }
}

fn atom_matches(atom: &AtomKind, ch: char) -> bool {
    match atom {
        AtomKind::Literal(expected) => *expected == ch,
        AtomKind::Any => true,
    }
}

fn searchable_text(instance: &Instance) -> String {
    let mut out = String::new();
    append(&mut out, &instance.instance_id);
    append_opt(&mut out, instance.name.as_deref());
    append_opt(&mut out, instance.private_ip.as_deref());
    // The table shows a Private DNS column, so it has to be searchable too;
    // without it `ip-10-1-2-3` only ever matched the handful of boxes with
    // that string in a Name tag.
    append_opt(&mut out, instance.private_dns.as_deref());
    append_opt(&mut out, instance.image_id.as_deref());

    for (k, v) in &instance.tags {
        append(&mut out, k);
        append(&mut out, v);
    }

    out
}

fn append(out: &mut String, value: &str) {
    out.push_str(&value.to_ascii_lowercase());
    out.push('\n');
}

fn append_opt(out: &mut String, value: Option<&str>) {
    if let Some(v) = value {
        append(out, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Instance;

    #[test]
    fn search_and_ssm_filters() {
        let mut a = Instance::new("i-a".to_string(), "running".to_string());
        a.name = Some("orders-api".to_string());
        a.ssm_managed = true;

        let mut b = Instance::new("i-b".to_string(), "stopped".to_string());
        b.name = Some("legacy-worker".to_string());
        b.ssm_managed = false;

        let all = vec![a, b];

        let filtered = apply_filters(
            &all,
            &Filters {
                includes: vec!["orders".to_string()],
                states: vec!["running".to_string()],
                only_ssm_managed: true,
                ..Filters::default()
            },
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].instance_id, "i-a");
    }

    /// Searching the private DNS name — the short host part, the fully
    /// qualified form, or a prefix — has to find the box even when nothing
    /// in its name or tags mentions it.
    #[test]
    fn search_matches_private_dns() {
        let mut a = Instance::new("i-a".to_string(), "running".to_string());
        a.name = Some("orders-api".to_string());
        a.private_ip = Some("10.20.30.40".to_string());
        a.private_dns = Some("ip-10-20-30-40.ec2.internal".to_string());

        let mut b = Instance::new("i-b".to_string(), "running".to_string());
        b.name = Some("billing-worker".to_string());
        b.private_dns = Some("ip-10-20-31-99.ec2.internal".to_string());

        let all = vec![a, b];
        let hits = |term: &str| {
            apply_filters(
                &all,
                &Filters {
                    includes: vec![term.to_string()],
                    ..Filters::default()
                },
            )
        };

        assert_eq!(hits("ip-10-20-30-40")[0].instance_id, "i-a");
        assert_eq!(
            hits("ip-10-20-30-40.ec2.internal")[0].instance_id,
            "i-a"
        );
        assert_eq!(hits("IP-10-20-30-40")[0].instance_id, "i-a");
        // A prefix common to both still returns both.
        assert_eq!(hits("ip-10-20-3").len(), 2);
        assert!(hits("ip-10-99-0-1").is_empty());
    }

    #[test]
    fn regex_search_matches_name() {
        let mut a = Instance::new("i-a".to_string(), "running".to_string());
        a.name = Some("orders-api-01".to_string());

        let mut b = Instance::new("i-b".to_string(), "running".to_string());
        b.name = Some("billing-worker".to_string());

        let filtered = apply_filters(
            &[a, b],
            &Filters {
                includes: vec!["^orders-.*$".to_string()],
                ..Filters::default()
            },
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].instance_id, "i-a");
    }

    #[test]
    fn regex_search_matches_tags() {
        let mut a = Instance::new("i-a".to_string(), "running".to_string());
        a.tags
            .insert("Team".to_string(), "platform-core".to_string());

        let mut b = Instance::new("i-b".to_string(), "running".to_string());
        b.tags.insert("Team".to_string(), "payments".to_string());

        let filtered = apply_filters(
            &[a, b],
            &Filters {
                includes: vec!["platform.*".to_string()],
                ..Filters::default()
            },
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].instance_id, "i-a");
    }

    #[test]
    fn regex_alternation_works() {
        let mut a = Instance::new("i-a".to_string(), "running".to_string());
        a.name = Some("orders-api".to_string());

        let mut b = Instance::new("i-b".to_string(), "running".to_string());
        b.name = Some("legacy-worker".to_string());

        let filtered = apply_filters(
            &[a, b],
            &Filters {
                includes: vec!["orders|legacy".to_string()],
                ..Filters::default()
            },
        );

        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn dotted_query_is_literal_text() {
        let mut a = Instance::new("i-a".to_string(), "running".to_string());
        a.private_ip = Some("10.0.0.1".to_string());

        let mut b = Instance::new("i-b".to_string(), "running".to_string());
        b.private_ip = Some("10x0x0x1".to_string());

        let filtered = apply_filters(
            &[a, b],
            &Filters {
                includes: vec!["10.0.0.1".to_string()],
                ..Filters::default()
            },
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].instance_id, "i-a");
    }

    #[test]
    fn include_and_exclude_terms_are_composed() {
        let mut a = Instance::new("i-a".to_string(), "running".to_string());
        a.name = Some("orders-api".to_string());
        a.tags.insert("Team".to_string(), "platform".to_string());

        let mut b = Instance::new("i-b".to_string(), "running".to_string());
        b.name = Some("orders-legacy".to_string());
        b.tags.insert("Team".to_string(), "platform".to_string());

        let filtered = apply_filters(
            &[a, b],
            &Filters {
                includes: vec!["orders".to_string(), "platform".to_string()],
                excludes: vec!["legacy".to_string()],
                ..Filters::default()
            },
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].instance_id, "i-a");
    }

    #[test]
    fn matching_tags_finds_key_or_value_matches() {
        let mut instance = Instance::new("i-1".to_string(), "running".to_string());
        instance
            .tags
            .insert("Team".to_string(), "cwfm-admin".to_string());

        let by_value = matching_tags(&instance, &vec!["cwfm".to_string()]);
        assert_eq!(by_value.len(), 1);
        assert_eq!(by_value[0].0, "Team");

        let by_key = matching_tags(&instance, &vec!["team".to_string()]);
        assert_eq!(by_key.len(), 1);
        assert_eq!(by_key[0].1, "cwfm-admin");
    }

    /// The tag filter and the Match Tag column must agree on what a
    /// `Key: value` term means, so both read it through this one parse.
    #[test]
    fn parse_tag_term_splits_key_and_value() {
        assert_eq!(
            parse_tag_term("Application: Reaper"),
            Some(("Application".to_string(), "reaper".to_string()))
        );
        assert_eq!(parse_tag_term("reaper"), None);
        assert_eq!(parse_tag_term(": reaper"), None);
        assert_eq!(parse_tag_term("Application:"), None);
    }

    /// The column reports the tag as the instance spells it, not as the
    /// user typed it: the key matches case-insensitively and the value is
    /// a substring, so neither is the tag's own text.
    #[test]
    fn tag_term_matches_reports_the_tag_in_its_own_casing() {
        let mut instance = Instance::new("i-1".to_string(), "running".to_string());
        instance
            .tags
            .insert("Application".to_string(), "Reaper-v2".to_string());
        instance
            .tags
            .insert("Team".to_string(), "platform".to_string());

        assert_eq!(
            tag_term_matches(&instance, "application", "reaper"),
            vec![("Application".to_string(), "Reaper-v2".to_string())]
        );
        assert!(tag_term_matches(&instance, "application", "scavenger").is_empty());
        assert!(tag_term_matches(&instance, "service", "reaper").is_empty());
    }

    #[test]
    fn matching_tags_supports_regex_lite() {
        let mut instance = Instance::new("i-1".to_string(), "running".to_string());
        instance
            .tags
            .insert("Service".to_string(), "billing-api".to_string());

        let matched = matching_tags(&instance, &vec!["^bill.*$".to_string()]);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].0, "Service");
    }
}
