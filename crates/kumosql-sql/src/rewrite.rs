//! BigQuery syntax that a generic SQL parser does not read, rewritten in the
//! source text before parsing.
//!
//! Ported from `bigquery_syntax.py` in the Python original, which does the same
//! thing for `sqlglot`.
//!
//! # Why rewrite the text instead of the tree
//!
//! Both parsers are good standard-SQL readers and poor BigQuery readers. The
//! Python original's solution is to rewrite the source into shapes the parser
//! *does* read -- usually an ordinary function call with a marker name -- and
//! resolve the marker back into the real node afterwards. That works on every
//! build of the parser, compiled or pure, which a parser-subclass approach does
//! not.
//!
//! # The rewrites
//!
//! * [`rewrite_aggregate_filters`] -- `COUNT(x WHERE c)`, the aggregate filter
//!   BigQuery spells inside the parentheses, becomes the standard
//!   `COUNT(x) FILTER (WHERE c)`, and [`resolve_aggregate_filters`] turns it
//!   back.
//! * [`reject_struct_comparison`] -- `STRUCT<>()`, which both parsers read as
//!   the comparison `STRUCT <> ()`, is *refused* rather than guessed at.
//!
//! Every rewrite is text-preserving: the tokens it does not touch are emitted
//! unchanged, so a query with none of these shapes comes back byte-identical.

use crate::error::{Error, Result};
use crate::token::{TokenKind, tokenize};

/// The call the rewritten aggregate filter is parked in while the base parser
/// reads it.
///
/// It must be a *call* rather than a bare name: a bare identifier reads as two
/// adjacent tokens, and the base parser refuses that. It never reaches the AST
/// -- [`crate::parse`] unwraps it back into the predicate -- so the marker is an
/// internal parse trick, not a spelling of the query.
pub(crate) const FILTER_MARKER: &str = "__KUMO_AGG_FILTER__";

/// Words that are never an aggregate's name, even though a `(` follows them.
///
/// `FROM (SELECT ...)` and `IN (1, 2)` both look like a call to the token walk.
/// Getting this wrong rewrites a subquery's `WHERE`, which would change the
/// query rather than its spelling.
const NOT_A_CALL: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "OUTER",
    "ON",
    "USING",
    "AS",
    "AND",
    "OR",
    "NOT",
    "BY",
    "WHEN",
    "THEN",
    "ELSE",
    "CASE",
    "OVER",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "ALL",
    "ANY",
    "SOME",
    "EXISTS",
    "WITH",
    "HAVING",
    "QUALIFY",
    "LIMIT",
    "OFFSET",
    "GROUP",
    "ORDER",
    "PARTITION",
    "VALUES",
    "BETWEEN",
    "LIKE",
    "IN",
    "RETURNING",
    // A `FILTER (WHERE c)` that was already written is the *output* of this
    // rewrite, so it must never be read as a call to rewrite again.
    "FILTER",
];

/// `COUNT(x WHERE c)` -> `COUNT(x) FILTER (WHERE c)`.
///
/// Only applies to a `WHERE` directly inside a call's parentheses, which is the
/// one position BigQuery allows it in. A `WHERE` belonging to a subquery, a
/// window frame or a nested call is left alone.
///
/// Returns the text unchanged when there is nothing to do.
pub fn rewrite_aggregate_filters(sql: &str) -> Result<String> {
    let tokens = tokenize(sql);
    let mut out = String::with_capacity(sql.len());
    let mut cursor = 0usize;

    let mut i = 0usize;
    while i < tokens.len() {
        let token = &tokens[i];

        // Only a `(` whose preceding word could be a function name.
        let is_call = token.kind == TokenKind::LParen
            && i > 0
            && tokens[i - 1].kind == TokenKind::Word
            && !NOT_A_CALL.iter().any(|k| tokens[i - 1].is_keyword(sql, k));

        if is_call {
            let Some(close) = matching_paren(&tokens, i) else {
                break;
            };
            // A parenthesised `SELECT` is a subquery, not a call's arguments.
            let is_subquery = tokens
                .get(i + 1)
                .is_some_and(|t| t.is_keyword(sql, "SELECT") || t.is_keyword(sql, "WITH"));
            let where_at = if is_subquery {
                None
            } else {
                top_level_where(&tokens, sql, i, close)
            };

            let Some(where_at) = where_at else {
                i += 1;
                continue;
            };

            // Everything up to the `WHERE` is the argument list, with the
            // whitespace before `WHERE` trimmed so the call closes tidily.
            let mut args_end = tokens[where_at].span.start;
            while args_end > cursor && sql.as_bytes()[args_end - 1].is_ascii_whitespace() {
                args_end -= 1;
            }
            // Everything from the `WHERE` to the `)` is the predicate.
            let predicate = sql[tokens[where_at].span.end..tokens[close].span.start]
                .trim()
                .to_string();

            out.push_str(&sql[cursor..args_end]);
            out.push_str(&format!(") FILTER (WHERE {FILTER_MARKER}({predicate}))"));

            // The original `)` is consumed here, so the copy resumes after it.
            cursor = tokens[close].span.end;
            i = close + 1;
            continue;
        }

        i += 1;
    }

    out.push_str(&sql[cursor..]);
    Ok(out)
}

/// The position of a `)` closing the `(` at `open`.
pub(crate) fn matching_paren(tokens: &[crate::token::Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        match token.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The index of a `WHERE` directly inside the parentheses, not nested in a
/// subquery or another call.
///
/// `sql` is needed because a keyword is compared against the token's text.
fn top_level_where(
    tokens: &[crate::token::Token],
    sql: &str,
    open: usize,
    close: usize,
) -> Option<usize> {
    let mut depth = 0i32;
    for (offset, token) in tokens[open + 1..close].iter().enumerate() {
        match token.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => depth -= 1,
            TokenKind::Word if depth == 0 && token.is_keyword(sql, "WHERE") => {
                return Some(open + 1 + offset);
            }
            _ => {}
        }
    }
    None
}

/// `COUNT(x) FILTER (WHERE c)` -> `COUNT(x WHERE c)`, undoing
/// [`rewrite_aggregate_filters`].
///
/// Idempotent: text that was never rewritten comes back unchanged.
/// Whether `sql` reads `STRUCT<>` as the comparison `STRUCT <> ()`.
///
/// `STRUCT<>` is not valid BigQuery, and a parser that read it as a comparison
/// would happily rewrite a query BigQuery rejects. This is the check
/// `_check_empty_struct` performs in the Python original.
pub fn has_empty_struct(sql: &str) -> bool {
    let tokens = tokenize(sql);
    let text = |i: usize| tokens.get(i).map(|t| t.text(sql)).unwrap_or("");
    tokens.iter().enumerate().any(|(i, token)| {
        token.is_keyword(sql, "STRUCT")
            && text(i + 1) == "<"
            && text(i + 2) == ">"
            && text(i + 3) == "("
            && text(i + 4) == ")"
    })
}

/// Refuse `STRUCT<>()`, which is a comparison a generic parser invents.
///
/// Returns [`crate::error::ErrorKind::Rejected`] rather than a parse error: the
/// text is refused *on purpose*, and a caller should be able to tell that apart
/// from a query that merely failed to parse.
pub fn reject_struct_comparison(sql: &str) -> Result<()> {
    if has_empty_struct(sql) {
        return Err(Error::rejected(
            0,
            "STRUCT<>() reads as the comparison STRUCT <> (), not an empty struct",
        ));
    }
    Ok(())
}

/// `"text"` -> `'text'`, because BigQuery reads a double-quoted run as a
/// string.
///
/// `GenericDialect` reads `"x"` as a *quoted identifier* instead. This is the
/// one place its reading of BigQuery is not merely a gap but the opposite of
/// BigQuery's: ``SELECT "x"`` is a string in BigQuery and a column named `x`
/// elsewhere, so a parse that trusted the dialect would model a different query.
///
/// Backslash handling is left to [`crate::literals::canonical_literals`], which
/// runs afterwards and normalises escapes in either spelling.
pub fn rewrite_double_quoted_strings(sql: &str) -> String {
    let tokens = tokenize(sql);
    let mut out = String::with_capacity(sql.len());
    let mut cursor = 0usize;
    for token in &tokens {
        if token.kind != TokenKind::String {
            continue;
        }
        let text = token.text(sql);
        // Only a double-quoted run; single quotes and backticks are untouched.
        if !text.starts_with('"') {
            continue;
        }
        out.push_str(&sql[cursor..token.span.start]);
        // Swap the delimiters, leaving the body (and its escapes) as written.
        out.push('\'');
        out.push_str(&text[1..text.len() - 1]);
        out.push('\'');
        cursor = token.span.end;
    }
    if cursor == 0 {
        return sql.to_string();
    }
    out.push_str(&sql[cursor..]);
    out
}

/// Rewrite the BigQuery shapes a generic parser cannot read, leaving the
/// markers in place so the base parser can read the result.
///
/// The markers are resolved by [`resolve_all`] when the text is rendered back.
/// They must **not** be resolved here: the parser runs in between, and
/// resolving first would undo the rewrite before it was ever needed.
pub fn rewrite_all(sql: &str) -> Result<String> {
    reject_struct_comparison(sql)?;
    let rewritten = crate::rewrites::rewrite_all(sql)?;
    Ok(rewrite_double_quoted_strings(&rewrite_aggregate_filters(
        &rewritten,
    )?))
}

/// Apply `(start, end, replacement)` edits over `sql`.
///
/// Edits are applied left to right and an edit starting inside an already
/// applied span is skipped, so overlapping edits cannot corrupt the text.
pub(crate) fn apply_edits(sql: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    if edits.is_empty() {
        return sql.to_string();
    }
    edits.sort_by_key(|(start, _, _)| *start);

    let mut out = String::with_capacity(sql.len());
    let mut cursor = 0usize;
    for (start, end, replacement) in edits {
        if start < cursor {
            continue;
        }
        out.push_str(&sql[cursor..start]);
        out.push_str(&replacement);
        cursor = end;
    }
    out.push_str(&sql[cursor..]);
    out
}
