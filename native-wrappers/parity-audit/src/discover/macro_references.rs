use std::collections::BTreeSet;

use syn::buffer::{Cursor, TokenBuffer};

use super::NamePath;

/// Literal macro calls in rule transcribers, without interpreting matchers or
/// expanding tokens. Unsupported computed paths are rejected when reached.
#[derive(Default)]
pub(super) struct MacroReferences {
    paths: BTreeSet<NamePath>,
    error: Option<String>,
}

impl MacroReferences {
    pub fn collect(mac: &syn::Macro) -> Self {
        let buffer = TokenBuffer::new2(mac.tokens.clone());
        let mut references = Self::default();
        references.rules(buffer.begin());
        references
    }

    pub fn paths(&self) -> Result<&BTreeSet<NamePath>, &str> {
        self.error.as_deref().map_or(Ok(&self.paths), Err)
    }

    fn rules(&mut self, mut cursor: Cursor<'_>) {
        while !cursor.eof() {
            let rule = cursor.any_group().and_then(|(_, _, _, after_matcher)| {
                let arrow = punct(after_matcher, '=').and_then(|cursor| punct(cursor, '>'))?;
                arrow.any_group()
            });
            let Some((transcriber, _, _, rest)) = rule else {
                self.error = Some("unsupported macro rule syntax".into());
                return;
            };
            self.scan(transcriber);
            cursor = punct(rest, ';')
                .or_else(|| punct(rest, ','))
                .unwrap_or(rest);
        }
    }

    fn scan(&mut self, mut cursor: Cursor<'_>) {
        let mut repetition = false;
        let mut computed_prefix = false;
        let mut previous_group = false;
        while !cursor.eof() {
            // A repeated token sequence can synthesize a path ($($part)::*!).
            // Follow the groups for literal nested calls, but never accept the
            // synthesized path as an ordinary unqualified helper name.
            if let Some(group) = punct(cursor, '$')
                && let Some((inside, _, _, rest)) = group.any_group()
            {
                self.scan(inside);
                computed_prefix |= ends_in_separator(inside);
                cursor = rest;
                repetition = true;
                continue;
            }
            if let Some((path, computed, rest)) = macro_path(cursor)
                && let Some(after_bang) = punct(rest, '!')
            {
                if path.segments == ["macro_rules"]
                    && let Some((_, after_name)) = after_bang.ident()
                    && let Some((rules, _, _, rest)) = after_name.any_group()
                {
                    self.rules(rules);
                    cursor = rest;
                    continue;
                }
                // Arguments may themselves be a captured token group, as in
                // helper! $arguments. The callee still selects a dependency.
                if computed || computed_prefix || path.segments == ["macro_rules"] {
                    self.error = Some("macro-variable expansion paths cannot be resolved; use literal helper paths".into());
                } else {
                    self.paths.insert(path);
                }
                cursor = after_bang;
                repetition = false;
                computed_prefix = false;
                previous_group = false;
                continue;
            }
            if (repetition || previous_group) && punct(cursor, '!').is_some() {
                self.error = Some(
                    "macro-variable expansion paths cannot be resolved; use literal helper paths"
                        .into(),
                );
            }
            if let Some((inside, _, _, rest)) = cursor.any_group() {
                self.scan(inside);
                cursor = rest;
                previous_group = true;
            } else {
                if cursor
                    .punct()
                    .is_none_or(|(punct, _)| !matches!(punct.as_char(), ':' | '*' | '+' | '?'))
                {
                    repetition = false;
                    computed_prefix = false;
                }
                cursor = cursor.token_tree().unwrap().1;
                previous_group = false;
            }
        }
    }
}

fn ends_in_separator(mut cursor: Cursor<'_>) -> bool {
    let mut colons = 0;
    while let Some((_, rest)) = cursor.token_tree() {
        if cursor
            .punct()
            .is_some_and(|(punct, _)| punct.as_char() == ':')
        {
            colons += 1;
        } else {
            colons = 0;
        }
        cursor = rest;
    }
    colons >= 2
}

fn punct(cursor: Cursor<'_>, expected: char) -> Option<Cursor<'_>> {
    let (punct, rest) = cursor.punct()?;
    (punct.as_char() == expected).then_some(rest)
}

fn separator(cursor: Cursor<'_>) -> Option<Cursor<'_>> {
    punct(cursor, ':').and_then(|cursor| punct(cursor, ':'))
}

fn macro_path(mut cursor: Cursor<'_>) -> Option<(NamePath, bool, Cursor<'_>)> {
    let mut path = NamePath::default();
    if let Some(rest) = separator(cursor) {
        path.absolute = true;
        cursor = rest;
    }
    let mut computed = false;
    loop {
        let dollar = punct(cursor, '$');
        let (name, rest) = dollar.unwrap_or(cursor).ident()?;
        let name = name.to_string();
        computed |= dollar.is_some() && (name != "crate" || !path.segments.is_empty());
        path.segments.push(name);
        cursor = rest;
        if let Some(rest) = separator(cursor) {
            cursor = rest;
        } else {
            return Some((path, computed, cursor));
        }
    }
}
