//! `icm logs --grep`: one matcher for every platform. The pattern is a list
//! of alternatives separated by `|`; a record matches when its tag or
//! message contains any of them, ignoring case. It is not a regular
//! expression (the CLI has no regex dependency): `panic|error` finds
//! either word, `.*` finds a literal `.*`.

use crate::catalogue::CheckId;
use crate::context::Ctx;
use crate::error::Check;

/// A parsed `--grep`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grep {
    /// The alternatives, lowercased; never empty.
    alternatives: Vec<String>,
}

impl Grep {
    /// The matcher for `--grep`, or `None` when there is nothing to match
    /// (no pattern, or only `|`).
    pub fn new(pattern: Option<&str>) -> Option<Grep> {
        let alternatives: Vec<String> = pattern?
            .split('|')
            .map(|part| part.trim().to_lowercase())
            .filter(|part| !part.is_empty())
            .collect();
        (!alternatives.is_empty()).then_some(Grep { alternatives })
    }

    /// Whether any of `texts` (a record's tag, its message) contains any
    /// alternative, ignoring case.
    pub fn matches(&self, texts: &[&str]) -> bool {
        texts.iter().any(|text| {
            let text = text.to_lowercase();
            self.alternatives
                .iter()
                .any(|alternative| text.contains(alternative.as_str()))
        })
    }
}

/// Matches `texts` against an optional `--grep` (no pattern keeps all).
pub fn keeps(pattern: Option<&str>, texts: &[&str]) -> bool {
    Grep::new(pattern).is_none_or(|grep| grep.matches(texts))
}

/// When a pattern that looks like a regular expression (or an alternation)
/// matched nothing, says how `--grep` reads it: an agent that wrote
/// `panic|error` or `^E.*` must not conclude there were no such lines.
pub fn warn_unmatched(ctx: &Ctx, pattern: Option<&str>, matched: usize, total: usize) {
    let Some(pattern) = pattern else {
        return;
    };
    if matched > 0 || !pattern.contains(['|', '.', '*', '+', '?', '^', '$', '[', '(', '\\']) {
        return;
    }
    ctx.rep.check(Check::warn(
        CheckId::UsageBadArgs,
        format!(
            "--grep `{pattern}` matched none of {total} record(s); it takes substrings separated by `|`, matched ignoring case, not a regular expression"
        ),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternatives_are_case_insensitive_substrings() {
        let grep = Grep::new(Some("panic|ERROR")).unwrap();
        assert!(grep.matches(&["", "thread 'main' panicked at src/lib.rs"]));
        assert!(grep.matches(&["Error", "x"]));
        assert!(!grep.matches(&["iced", "all quiet"]));
        assert!(
            Grep::new(Some("added"))
                .unwrap()
                .matches(&["", "ADDED \"x\""])
        );
        assert_eq!(Grep::new(Some(" | ")), None);
        assert_eq!(Grep::new(None), None);
        assert!(keeps(None, &["anything"]));
        // Not a regex: the dot is literal.
        assert!(!keeps(Some("a.c"), &["abc"]));
        assert!(keeps(Some("a.c"), &["a.c"]));
    }
}
