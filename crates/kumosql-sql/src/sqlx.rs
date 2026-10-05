//! Dataform SQLX: block sections and `${...}` interpolation masking.
//!
//! Ported from `src/kumosql/sqlx.py`.
//!
//! # Why masking
//!
//! A `${...}` expression is not a value in the SQL sense -- it compiles to
//! *arbitrary SQL text*. `${"x OR y"}` is a whole predicate, `when(incremental(),
//! "AND b > 1")` binds looser than its neighbours, and `${ref("t")}` is a table
//! name. So a rule cannot read an interpolation as an expression; it has to see
//! an opaque name in its place.
//!
//! Masking replaces each interpolation with a sentinel, and **carries the
//! connective around it** so the surrounding SQL still parses:
//!
//! ```text
//! WHERE a > 0 ${when(incremental(), "AND b > 1")}   ->  WHERE a > 0 AND __sqlx_token_000__
//! WHERE ${when(incremental(), "a > 1 AND")} b > 2   ->  WHERE __sqlx_token_000__ AND b > 2
//! SELECT ${"x AS y"} FROM t                          ->  SELECT __sqlx_token_000__ FROM t
//! ```
//!
//! # Why restore verifies
//!
//! A parse that succeeded does **not** prove the reader kept every expression. A
//! rewrite could drop a sentinel, duplicate one, or separate an expression from
//! the connective it was written with -- and each of those turns an incremental
//! filter into *valid-looking SQL that filters something else*. So every sentinel
//! is counted before any is restored, and a count other than one is an error
//! rather than a best-effort substitution.

use std::fmt;

/// The parts of a SQLX file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    /// SQL.
    Sql,
    /// A `config { ... }`, `js { ... }`, `pre_operations { ... }` or
    /// `post_operations { ... }` block, or a Dataform test's `input "name" { ... }`.
    ///
    /// Preserved byte-for-byte: it is not SQL and must not be read as SQL.
    Block,
}

/// One piece of a SQLX file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Whether this is SQL or a preserved block.
    pub kind: SectionKind,
    /// The text as written.
    pub text: String,
}

/// Raised when masking or restoring a SQLX interpolation would change meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlxRestorationError(pub String);

impl fmt::Display for SqlxRestorationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SqlxRestorationError {}

/// One masked interpolation, and how to put it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restoration {
    /// The sentinel the interpolation was replaced with.
    pub token: String,
    /// The original `${...}` text.
    pub original: String,
    /// The connective the sentinel was given, if any: `AND`, `OR`, `WHERE`,
    /// `QUALIFY`, `HAVING` or `ORDER BY`.
    pub connective: Option<String>,
    /// Whether the sentinel stands for the expression *and* its connective.
    ///
    /// When it does, restoring it anywhere else would change the meaning, so a
    /// rewrite that separated them cannot be restored.
    pub whole: bool,
}

/// Whether `sql` looks like a SQLX file rather than plain SQL.
///
/// True when it has a SQLX block or any `${...}`.
pub fn looks_like_sqlx(sql: &str) -> bool {
    sql.contains("${") || find_block_start(sql, 0).is_some()
}

/// The index of the `{` opening a SQLX block at or after `from`, if any.
///
/// A block header is a line whose first word is `config`, `js`,
/// `pre_operations` or `post_operations`, or `input "name"` / `input 'name'`,
/// followed by whitespace and `{`.
fn find_block_start(sql: &str, from: usize) -> Option<usize> {
    let bytes = sql.as_bytes();
    let mut index = from;
    while index < bytes.len() {
        // Only a line start can begin a block header.
        let at_line_start = index == 0 || bytes[index - 1] == b'\n';
        if !at_line_start {
            index += 1;
            continue;
        }

        let mut cursor = index;
        while cursor < bytes.len() && (bytes[cursor] == b' ' || bytes[cursor] == b'\t') {
            cursor += 1;
        }
        let word_start = cursor;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
        {
            cursor += 1;
        }
        let word = &sql[word_start..cursor];

        // `input "name" {` and `input 'name' {`
        if word.eq_ignore_ascii_case("input") {
            let mut probe = cursor;
            while probe < bytes.len() && bytes[probe].is_ascii_whitespace() {
                probe += 1;
            }
            if probe < bytes.len() && (bytes[probe] == b'"' || bytes[probe] == b'\'') {
                let quote = bytes[probe];
                probe += 1;
                while probe < bytes.len() && bytes[probe] != quote && bytes[probe] != b'\n' {
                    probe += 1;
                }
                if probe < bytes.len() && bytes[probe] == quote {
                    probe += 1;
                }
                while probe < bytes.len() && bytes[probe].is_ascii_whitespace() {
                    probe += 1;
                }
                if probe < bytes.len() && bytes[probe] == b'{' {
                    return Some(probe);
                }
            }
            index += 1;
            continue;
        }

        if matches!(
            word.to_ascii_lowercase().as_str(),
            "config" | "js" | "pre_operations" | "post_operations"
        ) {
            let mut probe = cursor;
            while probe < bytes.len() && bytes[probe].is_ascii_whitespace() {
                probe += 1;
            }
            if probe < bytes.len() && bytes[probe] == b'{' {
                return Some(probe);
            }
        }
        index += 1;
    }
    None
}

/// The index of the `}` closing the `{` at `opening`, or `None` if unbalanced.
///
/// Quotes, `--` and `/* */` comments and JavaScript `//` comments are all
/// skipped, because a brace or an apostrophe inside one is not structure.
fn find_balanced_brace(sql: &str, opening: usize) -> Option<usize> {
    let bytes = sql.as_bytes();
    let mut depth = 1usize;
    let mut index = opening + 1;
    let mut quote: Option<u8> = None;
    let mut line_comment = false;
    let mut block_comment = false;

    while index < bytes.len() {
        let ch = bytes[index];
        let next = if index + 1 < bytes.len() {
            bytes[index + 1]
        } else {
            0
        };

        if line_comment {
            if ch == b'\r' || ch == b'\n' {
                line_comment = false;
            }
        } else if block_comment {
            if ch == b'*' && next == b'/' {
                block_comment = false;
                index += 1;
            }
        } else if let Some(open) = quote {
            if ch == b'\\' {
                index += 1;
            } else if ch == open {
                // A doubled quote is an escaped quote, not the close.
                if next == open {
                    index += 1;
                } else {
                    quote = None;
                }
            }
        } else if ch == b'-' && next == b'-' {
            line_comment = true;
            index += 1;
        } else if ch == b'/' && next == b'*' {
            block_comment = true;
            index += 1;
        } else if ch == b'/' && next == b'/' {
            // A JavaScript comment: its apostrophes and braces are not code.
            line_comment = true;
            index += 1;
        } else if ch == b'\'' || ch == b'"' || ch == b'`' {
            quote = Some(ch);
        } else if ch == b'{' {
            depth += 1;
        } else if ch == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

/// Split a SQLX file into SQL sections and preserved blocks, in order.
pub fn split_sqlx_sections(sql: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut cursor = 0usize;

    while let Some(opening) = find_block_start(sql, cursor) {
        if opening > cursor {
            sections.push(Section {
                kind: SectionKind::Sql,
                text: sql[cursor..opening].to_string(),
            });
        }
        // The block header is included, so re-emitting is byte-for-byte.
        let header_start = sql[..opening]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let Some(closing) = find_balanced_brace(sql, opening) else {
            // An unterminated block is kept as the rest of the text rather than
            // silently dropping it.
            sections.push(Section {
                kind: SectionKind::Sql,
                text: sql[header_start..].to_string(),
            });
            return sections;
        };
        sections.push(Section {
            kind: SectionKind::Block,
            text: sql[header_start..=closing].to_string(),
        });
        cursor = closing + 1;
    }

    sections.push(Section {
        kind: SectionKind::Sql,
        text: sql[cursor..].to_string(),
    });
    sections
}

/// The index of the `}` closing a `${` interpolation opening at `opening`.
///
/// `None` if it never closes.
fn find_interpolation_end(sql: &str, opening: usize) -> Option<usize> {
    let bytes = sql.as_bytes();
    let mut depth = 1usize;
    let mut index = opening + 2;
    let mut quote: Option<u8> = None;

    while index < bytes.len() {
        let ch = bytes[index];
        let next = if index + 1 < bytes.len() {
            bytes[index + 1]
        } else {
            0
        };
        if let Some(open) = quote {
            if ch == b'\\' {
                index += 1;
            } else if ch == open {
                if next == open {
                    index += 1;
                } else {
                    quote = None;
                }
            }
        } else if ch == b'\'' || ch == b'"' || ch == b'`' {
            quote = Some(ch);
        } else if ch == b'{' {
            depth += 1;
        } else if ch == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

/// The byte ranges of every `--` and `/* */` comment in `sql`.
///
/// A `${...}` inside a comment is literal text: Dataform neither evaluates it
/// nor treats it as a dependency. A `#` comment is *not* counted, because
/// `@dataform/cli` evaluates `${ref(...)}` after one, and neither is a `--`
/// inside a string.
pub fn sql_comment_spans(sql: &str) -> Vec<(usize, usize)> {
    let bytes = sql.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0usize;

    while index < bytes.len() {
        if bytes[index] == b'$' && bytes.get(index + 1) == Some(&b'{') {
            // Skip the whole expression: a comment marker inside it is not a
            // comment.
            match find_interpolation_end(sql, index) {
                Some(end) => index = end + 1,
                None => return spans,
            }
        } else if bytes[index] == b'-' && bytes.get(index + 1) == Some(&b'-') {
            let start = index;
            while index < bytes.len() && bytes[index] != b'\r' && bytes[index] != b'\n' {
                index += 1;
            }
            spans.push((start, index));
        } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let start = index;
            index += 2;
            while index < bytes.len()
                && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
            {
                index += 1;
            }
            if index < bytes.len() {
                index += 2;
            }
            spans.push((start, index));
        } else if bytes[index] == b'\'' || bytes[index] == b'"' || bytes[index] == b'`' {
            // A string: its contents cannot open a comment.
            let quote = bytes[index];
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index += 2;
                    continue;
                }
                if bytes[index] == quote {
                    index += 1;
                    break;
                }
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    spans
}

/// Whether `original` is a plain table reference: `${ref("t")}`,
/// `${ref("dataset", "t")}`, `${resolve("t")}` or `${self()}`.
///
/// Such an expression is one table name once compiled, so a rule may reason
/// about it. Anything else compiles to arbitrary SQL and is opaque.
pub fn is_table_reference(original: &str) -> bool {
    let body = original
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .map(str::trim);
    let Some(body) = body else { return false };
    if body.is_empty() {
        return false;
    }

    if body == "self()" {
        return true;
    }

    // `ref("t")` or `ref("dataset", "t")`, and the same for `resolve`.
    let Some(open) = body.find('(') else {
        return false;
    };
    let name = body[..open].trim();
    if !name.eq_ignore_ascii_case("ref") && !name.eq_ignore_ascii_case("resolve") {
        return false;
    }
    let args = body[open + 1..].trim();
    let args = args.strip_suffix(')').unwrap_or(args).trim();

    let parts: Vec<&str> = args.split(',').map(str::trim).collect();
    if parts.is_empty() || parts.len() > 2 {
        return false;
    }
    parts.iter().all(|part| {
        let part = part.trim();
        part.len() >= 2
            && ((part.starts_with('"') && part.ends_with('"'))
                || (part.starts_with('\'') && part.ends_with('\'')))
    })
}

/// Replace every `${...}` interpolation in `sql` with a SQL-safe sentinel.
///
/// Returns the masked text and what is needed to restore it.
///
/// An interpolation inside a SQL comment is left alone: it is literal text
/// there, not an expression.
pub fn mask_sqlx_interpolations(sql: &str) -> (String, Vec<Restoration>) {
    let comments = sql_comment_spans(sql);
    let mut output = String::with_capacity(sql.len());
    let mut restorations: Vec<Restoration> = Vec::new();
    let mut cursor = 0usize;
    let mut ordinal = 0usize;

    while let Some(opening) = find_from(sql, "${", cursor) {
        // Inside a comment it is text, not an expression.
        if comments
            .iter()
            .any(|(start, end)| *start <= opening && opening < *end)
        {
            // Copy the text across: it is comment text, not an expression, so
            // it must survive into the masked output unchanged.
            output.push_str(&sql[cursor..opening + 2]);
            cursor = opening + 2;
            continue;
        }
        let Some(closing) = find_interpolation_end(sql, opening) else {
            break;
        };

        output.push_str(&sql[cursor..opening]);
        let original = sql[opening..=closing].to_string();

        // A sentinel that already occurs in the source would be ambiguous.
        let mut token = format!("__sqlx_token_{ordinal:03}__");
        while sql.contains(&token) {
            ordinal += 1;
            token = format!("__sqlx_token_{ordinal:03}__");
        }
        ordinal += 1;

        let prefix = output.trim_end().to_string();
        let body = &original[2..original.len() - 1];
        let preceding_clause = ends_with_clause(&prefix);
        let preceding_condition = ends_with_condition(&prefix);

        let mut replacement = token.clone();
        let mut connective = None;
        let mut whole = false;

        // `when(cond, `a > 1 AND`)`: the expression opens a condition and ends
        // with its connective.
        if let Some(keyword) = when_trailing_connective(body)
            && (preceding_clause || preceding_condition)
        {
            replacement = format!("{token} {keyword}");
            connective = Some(keyword.to_string());
            whole = true;
        } else if let Some(keyword) = when_leading_connective(body)
            && !clause_keyword(body).is_some()
            && !preceding_clause
            && !preceding_condition
        {
            // `... WHERE a > 0 ${when(incremental(), "AND b > 1")}`: the
            // expression continues the condition.
            replacement = format!("{keyword} {token}");
            connective = Some(keyword.to_string());
        } else if let Some(keyword) = clause_keyword(body)
            && !preceding_clause
            && !preceding_condition
        {
            // The expression opens a clause of its own.
            if matches!(keyword, "WHERE" | "QUALIFY" | "HAVING") {
                replacement = format!("{keyword} {token}");
            } else {
                replacement = format!("ORDER BY {token}");
            }
            connective = Some(keyword.to_string());
        }

        output.push_str(&replacement);
        restorations.push(Restoration {
            token,
            original,
            connective,
            whole,
        });
        cursor = closing + 1;
    }

    output.push_str(&sql[cursor..]);
    (output, restorations)
}

/// The next occurrence of `needle` at or after `from`.
fn find_from(sql: &str, needle: &str, from: usize) -> Option<usize> {
    if from >= sql.len() {
        return None;
    }
    sql[from..].find(needle).map(|offset| from + offset)
}

/// The clause keyword the text ends with, if any.
fn ends_with_clause(prefix: &str) -> bool {
    ["WHERE", "QUALIFY", "HAVING"]
        .iter()
        .any(|keyword| ends_with_word(prefix, keyword))
        || ends_with_word(prefix, "ORDER") && prefix.to_uppercase().ends_with("ORDER BY")
}

/// Whether the text ends with `AND` or `OR` as a whole word.
fn ends_with_condition(prefix: &str) -> bool {
    ends_with_word(prefix, "AND") || ends_with_word(prefix, "OR")
}

fn ends_with_word(text: &str, word: &str) -> bool {
    let upper = text.to_uppercase();
    let trimmed = upper.trim_end();
    if !trimmed.ends_with(word) {
        return false;
    }
    let before = &trimmed[..trimmed.len() - word.len()];
    // A word boundary: the character before must not be part of a word.
    match before.chars().next_back() {
        None => true,
        Some(c) => !(c.is_alphanumeric() || c == '_'),
    }
}

/// The clause keyword inside an interpolation's body, if it has one.
fn clause_keyword(body: &str) -> Option<&'static str> {
    let upper = body.to_uppercase();
    let mut best: Option<(usize, &'static str)> = None;
    for keyword in ["WHERE", "QUALIFY", "HAVING"] {
        if let Some(index) = find_word(&upper, keyword)
            && best.is_none_or(|(position, _)| index < position)
        {
            best = Some((index, keyword));
        }
    }
    // `ORDER BY` is two words, so it is looked for as a phrase.
    if let Some(index) = upper.find("ORDER BY")
        && best.is_none_or(|(position, _)| index < position)
    {
        best = Some((index, "ORDER BY"));
    }
    best.map(|(_, keyword)| keyword)
}

/// The index of `word` in `upper` as a whole word.
fn find_word(upper: &str, word: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(offset) = upper[from..].find(word) {
        let index = from + offset;
        let before_ok = index == 0
            || !upper[..index]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        let after = index + word.len();
        let after_ok = after >= upper.len()
            || !upper[after..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if before_ok && after_ok {
            return Some(index);
        }
        from = index + word.len();
    }
    None
}

/// The connective in `when(cond, "AND ...")` -- the expression *continues* a
/// condition that has already started.
fn when_leading_connective(body: &str) -> Option<&'static str> {
    let (arguments, _quote) = when_arguments(body)?;
    let upper = arguments.to_uppercase();
    let keyword = if upper.starts_with("AND") {
        "AND"
    } else if upper.starts_with("OR") {
        "OR"
    } else {
        return None;
    };
    Some(keyword)
}

/// The connective in `when(cond, `a > 1 AND`)` -- the expression *starts* a
/// condition and ends with its connective.
fn when_trailing_connective(body: &str) -> Option<&'static str> {
    let (arguments, quote) = when_arguments(body)?;
    let trimmed = arguments.trim_start();
    // The connective is the last word before the closing quote, so the quote
    // and anything after it is trimmed off first.
    // The closing paren of `when(...)` sits between the closing quote and the
    // end of the expression, so both go before the connective is read.
    let after_parens = trimmed
        .trim_end()
        .strip_suffix(')')
        .unwrap_or(trimmed.trim_end());
    let without_quote = after_parens.trim_end().strip_suffix(quote)?;
    let upper = without_quote.trim_end().to_uppercase();
    if upper.ends_with(" AND") {
        Some("AND")
    } else if upper.ends_with(" OR") {
        Some("OR")
    } else {
        None
    }
}

/// The text between `when(`'s first top-level comma and its closing quote, and
/// the quote character.
fn when_arguments(body: &str) -> Option<(&str, char)> {
    let trimmed = body.trim_start();
    if trimmed.len() < 4 || !trimmed[..4].eq_ignore_ascii_case("when") {
        return None;
    }
    let after_keyword = &trimmed[4..];
    let after_keyword = after_keyword.trim_start();
    if !after_keyword.starts_with('(') {
        return None;
    }
    let inner = &after_keyword[1..];

    // Find the first top-level comma.
    let mut depth = 0i32;
    let mut index = 0usize;
    let bytes = inner.as_bytes();
    let mut comma = None;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
            }
            b',' if depth == 0 => {
                comma = Some(index);
                break;
            }
            _ => {}
        }
        index += 1;
    }
    let comma = comma?;
    let after_comma = &inner[comma + 1..];

    // The first argument after the comma is a quoted string.
    let after_comma = after_comma.trim_start();
    let quote = after_comma.chars().next()?;
    if quote != '\'' && quote != '"' && quote != '`' {
        return None;
    }
    Some((&after_comma[quote.len_utf8()..], quote))
}

/// The sentinels of interpolations that are *not* plain table references.
///
/// Such an expression compiles to arbitrary SQL: a clause, a predicate that
/// binds looser than its neighbours, or a query reading a CTE. A rule sees only
/// an opaque name in its place and must not reason past it.
pub fn opaque_tokens(restorations: &[Restoration]) -> Vec<String> {
    restorations
        .iter()
        .filter(|item| !is_table_reference(&item.original))
        .map(|item| item.token.clone())
        .collect()
}

/// Put every masked interpolation back.
///
/// **Verifies first that each sentinel appears exactly once.** A parse that
/// succeeded does not prove the reader kept every expression: a rewrite could
/// drop one, duplicate one, or separate an expression from the connective it was
/// written with, and each of those turns an incremental filter into
/// valid-looking SQL that filters something else.
///
/// An error is raised rather than a best-effort substitution.
pub fn restore_sqlx_interpolations(
    sql: &str,
    restorations: &[Restoration],
) -> Result<String, SqlxRestorationError> {
    for item in restorations {
        let count = sql.matches(&item.token).count();
        if count != 1 {
            return Err(SqlxRestorationError(format!(
                "SQLX interpolation placeholder was lost or duplicated during rewriting \
                 ({count} copies found; expected one)"
            )));
        }
    }

    let mut restored = sql.to_string();
    for item in restorations {
        if item.whole {
            // The sentinel plus its connective must still be together.
            let Some(keyword) = &item.connective else {
                return Err(SqlxRestorationError(
                    "a whole interpolation has no connective to keep it attached to".to_string(),
                ));
            };
            let needle = format!("{} {}", item.token, keyword);
            let found = match_case_insensitive(&restored, &needle);
            if found != 1 {
                return Err(SqlxRestorationError(
                    "a rewrite separated a SQLX interpolation from the connective it ends with"
                        .to_string(),
                ));
            }
        }

        // The connective belongs to the sentinel: masking put them together
        // because the surrounding SQL needs it, and restoring has to take both
        // out again. Leaving it behind would duplicate the connective.
        if let Some(keyword) = &item.connective {
            let pair = if item.whole {
                format!("{} {keyword}", item.token)
            } else {
                format!("{keyword} {}", item.token)
            };
            restored = replace_after(&restored, &pair, &item.original);
        }
        restored = replace_after(&restored, &item.token, &item.original);
        // A sentinel inside a quoted table reference may have been quoted.
        restored = replace_after(&restored, &format!("`{}`", item.token), &item.original);
    }
    Ok(restored)
}

/// How many times `needle` occurs in `text`, ignoring case.
fn match_case_insensitive(text: &str, needle: &str) -> usize {
    let upper_text = text.to_uppercase();
    let upper_needle = needle.to_uppercase();
    upper_text.matches(&upper_needle).count()
}

/// Replace `from` with `to`, treating the replacement as literal text.
///
/// A function replacement would read the backslashes in `r'\d'` or `\1` as
/// replacement syntax, so the substitution is done by hand.
fn replace_after(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(offset) = text[cursor..].find(from) {
        let start = cursor + offset;
        out.push_str(&text[cursor..start]);
        out.push_str(to);
        cursor = start + from.len();
    }
    out.push_str(&text[cursor..]);
    out
}
