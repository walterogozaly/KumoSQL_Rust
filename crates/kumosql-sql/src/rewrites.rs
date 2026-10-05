//! Further BigQuery syntax that a generic SQL parser does not read.
//!
//! Ported from `bigquery_syntax.py`. These are separate from
//! [`crate::rewrite`] because they each need `matching_paren` and `apply_edits`
//! from there; keeping the token-walking helpers in one place stops the two
//! modules drifting apart on how a span or a parenthesis depth is computed.
//!
//! | Shape | Why the base parser cannot read it |
//! | --- | --- |
//! | `x LIKE ALL UNNEST(arr)` | `sqlparser` records only "is this `ANY`", so it cannot tell `ANY` from `ALL` |
//! | `WITH(a AS 1, ...)` | a named-expression form with no node at all |
//! | `fn(TABLE ds.input)` | the parser stops at the `TABLE` keyword and loses the call |
//! | `DROP TABLE FUNCTION` | the kind of a `DROP` is read from the token after it |

use crate::error::Result;
use crate::rewrite::{apply_edits, matching_paren};
use crate::token::{Token, TokenKind, tokenize};

/// Parks the array of a `LIKE ALL UNNEST(arr)`.
///
/// `LIKE ALL` and `LIKE ANY` differ on empty input -- `ALL` is true, `ANY` is
/// false -- so the quantifier must survive the parse exactly. `sqlparser`
/// cannot carry it, so it is parked in a marker call inside the `UNNEST`.
pub const LIKE_ALL_MARKER: &str = "__KUMO_LIKE_ALL__";

/// Parks a whole `WITH(a AS 1, ...)` expression.
pub const WITH_MARKER: &str = "__KUMO_WITH__";

/// Parks one `(name AS value)` binding inside a `WITH(...)` expression.
pub const WITH_VARIABLE_MARKER: &str = "__KUMO_WITH_VARIABLE__";

/// Parks a `TABLE name` argument to a table-valued function.
pub const TABLE_ARGUMENT_MARKER: &str = "__KUMO_TABLE_ARGUMENT__";

/// `x LIKE ALL UNNEST(arr)` -> `x LIKE ANY UNNEST(__KUMO_LIKE_ALL__(arr))`.
///
/// `LIKE SOME` becomes `LIKE ANY`, since `SOME` is BigQuery's older spelling.
pub fn rewrite_like_quantifiers(sql: &str) -> String {
    let tokens = tokenize(sql);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();

    if tokens.len() < 3 {
        return sql.to_string();
    }
    for index in 0..tokens.len() - 2 {
        let like = tokens[index].text(sql).to_uppercase();
        if like != "LIKE" && like != "ILIKE" {
            continue;
        }
        let quantifier = &tokens[index + 1];
        let word = quantifier.text(sql).to_uppercase();
        let after = &tokens[index + 2];

        if word == "SOME" {
            edits.push((
                quantifier.span.start,
                quantifier.span.end,
                "ANY".to_string(),
            ));
            continue;
        }

        let is_all_unnest = word == "ALL"
            && after.text(sql).to_uppercase() == "UNNEST"
            && tokens.get(index + 3).map(|t| t.kind) == Some(TokenKind::LParen);

        if !is_all_unnest {
            continue;
        }
        let Some(close) = matching_paren(&tokens, index + 3) else {
            continue;
        };

        edits.push((
            quantifier.span.start,
            quantifier.span.end,
            "ANY".to_string(),
        ));
        // Open the marker just inside the `UNNEST(`.
        edits.push((
            tokens[index + 3].span.end,
            tokens[index + 3].span.end,
            format!("{LIKE_ALL_MARKER}("),
        ));
        // And close it just before the `)`.
        edits.push((
            tokens[close].span.start,
            tokens[close].span.start,
            ")".to_string(),
        ));
    }

    apply_edits(sql, edits)
}

/// `WITH(a AS 1, a + 1)` -> `__KUMO_WITH__(__KUMO_WITH_VARIABLE__('a', 1), a + 1)`.
///
/// An outer expression holds the text of an inner one, so the inner rewrite is
/// dropped here and picked up on the next pass.
pub fn rewrite_with_expressions(sql: &str) -> String {
    let tokens = tokenize(sql);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();

    if tokens.len() < 3 {
        return sql.to_string();
    }
    for index in 1..tokens.len() - 1 {
        let token = &tokens[index];
        if !token.is_keyword(sql, "WITH")
            || tokens[index + 1].kind != TokenKind::LParen
            // A `WITH` after a `;` opens a clause, not an expression.
            || tokens[index - 1].text(sql) == ";"
        {
            continue;
        }
        let Some(close) = matching_paren(&tokens, index + 1) else {
            continue;
        };

        let Some(items) = split_on_top_level_commas(&tokens, index + 2, close) else {
            continue;
        };
        if items.len() < 2 || items.iter().any(Vec::is_empty) {
            continue;
        }

        // Every item but the last must read `name AS value`.
        let mut parts: Vec<String> = Vec::new();
        let mut usable = true;
        for item in &items[..items.len() - 1] {
            let name = &tokens[item[0]];
            if item.len() < 3
                || name.kind != TokenKind::Word
                || is_keyword_word(name.text(sql))
                || !tokens[item[1]].is_keyword(sql, "AS")
            {
                usable = false;
                break;
            }
            let last = *item.last().expect("non-empty");
            let value = &sql[tokens[item[2]].span.start..tokens[last].span.end];
            parts.push(format!(
                "{WITH_VARIABLE_MARKER}('{}', {value})",
                name.text(sql)
            ));
        }
        if !usable {
            continue;
        }

        let body = &items[items.len() - 1];
        let first = body[0];
        let last = *body.last().expect("non-empty");
        parts.push(sql[tokens[first].span.start..tokens[last].span.end].to_string());
        edits.push((
            token.span.start,
            tokens[close].span.end,
            format!("{WITH_MARKER}({})", parts.join(", ")),
        ));
    }

    // Keep only the outermost rewrites: an outer one has already replaced the
    // text the inner one was looking at.
    let outermost: Vec<(usize, usize, String)> = edits
        .iter()
        .filter(|(start, _, _)| {
            !edits
                .iter()
                .any(|(outer_start, outer_end, _)| *start > *outer_start && *start < *outer_end)
        })
        .cloned()
        .collect();

    apply_edits(sql, outermost)
}

/// Split token indices `open..close` on top-level commas.
///
/// `None` when a bracket is left open, which means the caller mis-guessed a
/// parenthesis and the text should be left alone.
fn split_on_top_level_commas(
    tokens: &[Token],
    open: usize,
    close: usize,
) -> Option<Vec<Vec<usize>>> {
    let mut items: Vec<Vec<usize>> = vec![Vec::new()];
    let mut depth = 0i32;
    for (offset, token) in tokens[open..close].iter().enumerate() {
        let position = open + offset;
        match token.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => depth -= 1,
            TokenKind::Comma if depth == 0 => {
                items.push(Vec::new());
                continue;
            }
            _ => {}
        }
        items.last_mut().expect("seeded").push(position);
    }
    if depth != 0 {
        return None;
    }
    Some(items)
}

/// Whether a word is a reserved keyword rather than a name.
///
/// A `WITH(name AS value)` binding's name must not be one of these, or the
/// shape is not the expression form at all.
fn is_keyword_word(text: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "SELECT",
        "FROM",
        "WHERE",
        "GROUP",
        "ORDER",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "JOIN",
        "ON",
        "AND",
        "OR",
        "NOT",
        "AS",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        "WITH",
        "BY",
        "WHEN",
        "THEN",
        "ELSE",
        "END",
        "CASE",
        "IN",
        "IS",
        "LIKE",
        "BETWEEN",
        "DISTINCT",
        "ALL",
        "ANY",
        "NULL",
        "TRUE",
        "FALSE",
        "ASC",
        "DESC",
        "INNER",
        "LEFT",
        "RIGHT",
        "FULL",
        "CROSS",
        "QUALIFY",
        "WINDOW",
        "VALUES",
        "SET",
        "INTO",
        "USING",
    ];
    KEYWORDS.iter().any(|k| text.eq_ignore_ascii_case(k))
}

/// `fn(TABLE dataset.input, ...)` -> `fn(__KUMO_TABLE_ARGUMENT__(dataset.input), ...)`.
///
/// Without this the parser stops at `TABLE` and reports the whole call
/// unparseable, losing the table with it. With it the table reads like any other
/// and the SQL prints back unchanged.
pub fn rewrite_table_arguments(sql: &str) -> String {
    let tokens = tokenize(sql);
    let mut out = String::with_capacity(sql.len());
    let mut cursor = 0usize;
    let mut index = 0usize;

    while index < tokens.len() {
        let token = &tokens[index];

        // Only a `TABLE` directly inside a call's arguments: right after a `(`
        // or a `,`.
        let follows_open =
            index > 0 && matches!(tokens[index - 1].kind, TokenKind::LParen | TokenKind::Comma);

        let is_table_argument = follows_open
            && token.is_keyword(sql, "TABLE")
            && tokens
                .get(index + 1)
                .is_some_and(|t| t.kind != TokenKind::Comma && t.kind != TokenKind::RParen);

        if is_table_argument {
            // The argument runs to the next top-level comma or closing paren.
            let mut depth = 0i32;
            let mut end = index + 1;
            while end < tokens.len() {
                match tokens[end].kind {
                    TokenKind::LParen => depth += 1,
                    TokenKind::RParen => {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    }
                    TokenKind::Comma if depth == 0 => break,
                    _ => {}
                }
                end += 1;
            }

            if end < tokens.len() {
                out.push_str(&sql[cursor..token.span.start]);
                out.push_str(&format!("{TABLE_ARGUMENT_MARKER}("));
                out.push_str(&sql[tokens[index + 1].span.start..tokens[end - 1].span.end]);
                out.push(')');
                cursor = tokens[end - 1].span.end;
                index = end;
                continue;
            }
        }

        index += 1;
    }

    out.push_str(&sql[cursor..]);
    out
}

/// `DROP TABLE FUNCTION` -> a `DROP` followed by one token spelled
/// `TABLE FUNCTION`.
///
/// A parser takes the kind of a `DROP` from the token after it.
///
/// **Known gap:** this is written for a parser whose tokens include trailing
/// whitespace, and `sqlparser` 0.59 cannot represent the shape at all -- it
/// reads `DROP TABLE FUNCTION` as a drop of the table named `FUNCTION` and then
/// refuses the real name. The rewrite is therefore a no-op there, and the shape
/// has to come through the command/splitter path instead of the parse. See the
/// open risks in `docs/roadmap.md`.
pub fn merge_table_function_kind(sql: &str) -> String {
    let tokens = tokenize(sql);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();

    if tokens.len() < 3 {
        return sql.to_string();
    }
    for index in 1..tokens.len() - 1 {
        if tokens[index].is_keyword(sql, "TABLE")
            && tokens[index - 1].is_keyword(sql, "DROP")
            && tokens[index + 1].is_keyword(sql, "FUNCTION")
        {
            edits.push((
                tokens[index].span.start,
                tokens[index + 1].span.end,
                "TABLE FUNCTION".to_string(),
            ));
        }
    }

    apply_edits(sql, edits)
}

/// Apply every rewrite in this module, in the order they must run.
///
/// Split out from [`crate::rewrite::rewrite_all`] so the two can be tested
/// independently; `rewrite_all` calls this.
pub fn rewrite_all(sql: &str) -> Result<String> {
    // Two passes over the `WITH(...)` rewrite: it nests, and an outer rewrite
    // absorbs the inner one, so the inner is only seen on the second pass.
    let once = rewrite_with_expressions(sql);
    let twice = rewrite_with_expressions(&once);
    Ok(rewrite_with_expressions(&twice)).map(|text| {
        rewrite_table_arguments(&merge_table_function_kind(&rewrite_like_quantifiers(&text)))
    })
}
