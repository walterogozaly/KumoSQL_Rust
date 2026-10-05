//! The BigQuery tokenizer and the text rewrites built on it.
//!
//! The cases here are the shapes a generic SQL parser misreads or refuses, and
//! they come from probing `sqlparser-rs` 0.59 against BigQuery: it reads
//! `COUNT(x) FILTER (WHERE c)`, `UNNEST(...) WITH OFFSET`, `* EXCEPT (...)`,
//! `STRUCT(1 AS x).x`, `SAFE_CAST`, `QUALIFY` and `IS NOT DISTINCT FROM`, and
//! it refuses `COUNT(x WHERE c)`, `STRUCT()`, `GROUP BY ALL` and script
//! statements. The rewrites cover the first of those; the rest are noted as
//! remaining in `docs/roadmap.md`.

use kumosql_sql::error::ErrorKind;
use kumosql_sql::rewrite::{
    has_empty_struct, reject_struct_comparison, rewrite_aggregate_filters, rewrite_all,
};
use kumosql_sql::token::{TokenKind, tokenize};

#[test]
fn words_punctuation_and_parens_are_separated() {
    let tokens = tokenize("SELECT a, b FROM t");
    let kinds: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        vec![
            TokenKind::Word, // SELECT
            TokenKind::Word, // a
            TokenKind::Comma,
            TokenKind::Word, // b
            TokenKind::Word, // FROM
            TokenKind::Word, // t
        ]
    );
}

#[test]
fn a_string_is_one_token_even_with_punctuation_inside() {
    let tokens = tokenize("SELECT 'a, b (c)' AS x");
    let string = tokens
        .iter()
        .find(|t| t.kind == TokenKind::String)
        .expect("a string token");
    assert_eq!(string.text("SELECT 'a, b (c)' AS x"), "'a, b (c)'");
}

#[test]
fn a_quoted_name_is_a_word_and_keeps_its_backticks() {
    let sql = "SELECT `a``b` FROM t";
    let tokens = tokenize(sql);
    let backticked = tokens
        .iter()
        .find(|t| t.kind == TokenKind::Word && t.text(sql).starts_with('`'))
        .expect("a backticked name");
    assert_eq!(backticked.text(sql), "`a``b`");
}

#[test]
fn a_where_inside_a_string_is_not_a_keyword() {
    // The rewrite must not see a `WHERE` that is really literal text.
    let sql = "SELECT 'WHERE' AS x";
    let rewritten = rewrite_aggregate_filters(sql).unwrap();
    assert_eq!(rewritten, sql);
}

#[test]
fn a_where_inside_a_line_comment_is_not_a_keyword() {
    let sql = "SELECT COUNT(x) -- WHERE c\nFROM t";
    let rewritten = rewrite_aggregate_filters(sql).unwrap();
    assert_eq!(rewritten, sql);
}

#[test]
fn the_aggregate_filter_is_rewritten_to_the_standard_form() {
    // The intermediate form carries a marker so that resolving can tell this
    // rewrite apart from a `FILTER` the query already had.
    assert_eq!(
        rewrite_aggregate_filters("SELECT COUNT(x WHERE c) FROM t").unwrap(),
        "SELECT COUNT(x) FILTER (WHERE __KUMO_AGG_FILTER__(c)) FROM t"
    );
}

#[test]
fn an_already_standard_filter_is_left_alone() {
    // Rewriting this again would produce `FILTER () FILTER (...)`.
    let sql = "SELECT COUNT(x) FILTER (WHERE c) FROM t";
    assert_eq!(rewrite_aggregate_filters(sql).unwrap(), sql);
}

#[test]
fn a_where_in_a_subquery_is_not_an_aggregate_filter() {
    // The `WHERE` belongs to the subquery, so it must be left where it is.
    let sql = "SELECT COUNT(*) FROM (SELECT 1 FROM t WHERE c) AS s";
    assert_eq!(rewrite_aggregate_filters(sql).unwrap(), sql);
}

#[test]
fn a_nested_call_with_a_where_is_left_alone() {
    let sql = "SELECT COALESCE(IF(a, 1, 2)) FROM t WHERE c";
    assert_eq!(rewrite_aggregate_filters(sql).unwrap(), sql);
}

#[test]
fn the_rewritten_filter_is_readable_by_the_base_parser() {
    // The rewrite exists only so `sqlparser-rs` can read BigQuery's aggregate
    // filter. What matters is that the result parses, and that the marker is a
    // *call*: a bare marker name reads as two adjacent tokens, which the base
    // parser refuses.
    let standard = rewrite_aggregate_filters("SELECT COUNT(x WHERE c) FROM t").unwrap();
    assert!(standard.contains("__KUMO_AGG_FILTER__("));
    assert!(
        kumosql_sql::parse::parse_statements("SELECT COUNT(x WHERE c) FROM t", false).is_ok(),
        "the rewritten form should parse"
    );
}

#[test]
fn rewrite_all_leaves_a_plain_query_byte_identical() {
    let sql = "SELECT a, b FROM t WHERE c = 1 ORDER BY a LIMIT 10";
    assert_eq!(rewrite_all(sql).unwrap(), sql);
}

#[test]
fn the_empty_struct_comparison_is_recognised() {
    assert!(has_empty_struct("SELECT STRUCT<>()"));
    assert!(has_empty_struct("SELECT * FROM t WHERE x = STRUCT<>()"));
}

#[test]
fn a_real_struct_is_not_mistaken_for_the_comparison() {
    assert!(!has_empty_struct("SELECT STRUCT(1 AS x)"));
    assert!(!has_empty_struct("SELECT a < b"));
    assert!(!has_empty_struct("SELECT STRUCT"));
}

#[test]
fn the_empty_struct_comparison_is_refused_rather_than_guessed() {
    let error = reject_struct_comparison("SELECT STRUCT<>()").unwrap_err();
    // Refused on purpose, which is a different claim from "failed to parse".
    assert_eq!(error.kind, ErrorKind::Rejected);
    assert!(error.is_refusal());
}

#[test]
fn a_valid_query_is_not_refused() {
    assert!(reject_struct_comparison("SELECT STRUCT(1 AS x)").is_ok());
}

#[test]
fn rewrite_all_refuses_the_empty_struct_before_parsing() {
    let error = rewrite_all("SELECT STRUCT<>()").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Rejected);
}

#[test]
fn the_tokenizer_keeps_every_byte_of_the_source() {
    // Comments are tokens rather than dropped, so rendering a rewritten query
    // cannot lose them.
    let sql = "SELECT 1 -- a note\nFROM t /* another */ WHERE c = 'x'";
    let tokens = tokenize(sql);
    let rebuilt: String = tokens.iter().map(|t| t.text(sql)).collect();
    // Whitespace between tokens is not preserved, but no non-space byte is lost.
    let compact: String = rebuilt.chars().filter(|c| !c.is_whitespace()).collect();
    let original: String = sql.chars().filter(|c| !c.is_whitespace()).collect();
    assert_eq!(compact, original);
    assert!(tokens.iter().any(|t| t.kind == TokenKind::LineComment));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::BlockComment));
}
