//! A TOML file's text with the spans of every key, so findings can point at
//! `file:line`.

use crate::error::Evidence;
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use toml::de::{DeTable, DeValue};

/// A parsed TOML source.
#[derive(Clone, Debug)]
pub struct Source {
    /// The file.
    pub path: PathBuf,
    /// Its text.
    pub text: String,
    line_starts: Vec<usize>,
    /// `dotted.path` (arrays as `path[i]`) → (key span, value span).
    spans: BTreeMap<String, (Range<usize>, Range<usize>)>,
}

impl Source {
    /// Indexes a file's text. A syntax error is reported by the caller's
    /// deserializer; the span index is then empty.
    pub fn new(path: &Path, text: String) -> Source {
        let mut line_starts = vec![0];
        line_starts.extend(
            text.char_indices()
                .filter(|(_, c)| *c == '\n')
                .map(|(index, _)| index + 1),
        );

        let mut spans = BTreeMap::new();
        if let Ok(table) = DeTable::parse(&text) {
            walk("", table.get_ref(), &mut spans);
        }

        Source {
            path: path.to_path_buf(),
            text,
            line_starts,
            spans,
        }
    }

    /// The 1-based line and column of a byte offset.
    pub fn line_col(&self, offset: usize) -> (u32, u32) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        };
        let start = self.line_starts.get(line).copied().unwrap_or(0);
        let col = self
            .text
            .get(start..offset)
            .map_or(0, |s| s.chars().count());
        (line as u32 + 1, col as u32 + 1)
    }

    /// The text of a 1-based line.
    pub fn line_text(&self, line: u32) -> &str {
        let index = (line as usize).saturating_sub(1);
        let start = self.line_starts.get(index).copied().unwrap_or(0);
        let end = self
            .line_starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.text.len());
        self.text
            .get(start..end)
            .unwrap_or("")
            .trim_end_matches(['\n', '\r'])
    }

    /// The span of a key's value (`app.id`, `app.platforms[1]`).
    pub fn value_span(&self, path: &str) -> Option<Range<usize>> {
        self.spans.get(path).map(|(_, value)| value.clone())
    }

    /// The span of a key itself.
    pub fn key_span(&self, path: &str) -> Option<Range<usize>> {
        self.spans.get(path).map(|(key, _)| key.clone())
    }

    /// Every indexed path under a table (direct children only).
    pub fn children(&self, table: &str) -> Vec<String> {
        let prefix = if table.is_empty() {
            String::new()
        } else {
            format!("{table}.")
        };
        self.spans
            .keys()
            .filter(|path| {
                path.starts_with(&prefix)
                    && !path[prefix.len()..].contains(['.', '['])
                    && path.len() > prefix.len()
            })
            .cloned()
            .collect()
    }

    /// `path:line:col` for a span.
    pub fn location(&self, span: &Range<usize>) -> String {
        let (line, col) = self.line_col(span.start);
        format!("{}:{line}:{col}", crate::paths::display(&self.path))
    }

    /// Evidence for a span: the file, the line and the line's text.
    pub fn evidence(&self, span: &Range<usize>) -> Evidence {
        let (line, _) = self.line_col(span.start);
        Evidence::line(&self.path, line, self.line_text(line).trim())
    }

    /// Evidence for a dotted path: its value, else its key, else the
    /// nearest indexed parent, else the file.
    pub fn evidence_for(&self, path: &str) -> Evidence {
        let mut candidate = path.to_string();
        loop {
            if let Some(span) = self.value_span(&candidate) {
                return self.evidence(&span);
            }
            match candidate.rfind(['.', '[']) {
                Some(index) => candidate.truncate(index),
                None => return Evidence::file(&self.path),
            }
        }
    }

    /// `file:line:col` for a dotted path, falling back like
    /// [`Source::evidence_for`].
    pub fn location_for(&self, path: &str) -> String {
        let evidence = self.evidence_for(path);
        match evidence.line {
            Some(line) => format!("{}:{line}", evidence.path),
            None => evidence.path,
        }
    }
}

fn walk(
    prefix: &str,
    table: &DeTable<'_>,
    out: &mut BTreeMap<String, (Range<usize>, Range<usize>)>,
) {
    for (key, value) in table.iter() {
        let path = if prefix.is_empty() {
            key.get_ref().to_string()
        } else {
            format!("{prefix}.{}", key.get_ref())
        };
        let _ = out.insert(path.clone(), (key.span(), value.span()));

        match value.get_ref() {
            DeValue::Table(inner) => walk(&path, inner, out),
            DeValue::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    let item_path = format!("{path}[{index}]");
                    let _ = out.insert(item_path.clone(), (item.span(), item.span()));
                    if let DeValue::Table(inner) = item.get_ref() {
                        walk(&item_path, inner, out);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_resolve_to_lines() {
        let text = "schema = 1\n\n[app]\nname = \"Notes\"\nplatforms = [\"web\", \"tv\"]\n\n[ios.privacy]\ntracking = false\n";
        let source = Source::new(Path::new("/x/icm.toml"), text.to_string());

        let span = source.value_span("app.name").unwrap();
        assert_eq!(source.line_col(span.start), (4, 8));
        assert_eq!(source.line_text(4), "name = \"Notes\"");

        let evidence = source.evidence_for("app.platforms[1]");
        assert_eq!(evidence.line, Some(5));

        assert_eq!(source.evidence_for("ios.privacy.tracking").line, Some(8));
        // A missing key falls back to its table.
        assert_eq!(source.evidence_for("app.id").line, Some(3));
        assert_eq!(source.evidence_for("nothing.here").line, None);

        let mut children = source.children("app");
        children.sort();
        assert_eq!(children, vec!["app.name", "app.platforms"]);
    }
}
