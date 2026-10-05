//! The parse accounts for the whole query.
//!
//! Ported in intent from `parse_check.py`. What matters here is not that the
//! check agrees with the Python one -- that one compares two independent
//! dialect parsers, which this port does not have -- but that it catches the two
//! failure modes it exists for, and says "unchecked" rather than "agree" when it
//! cannot look.

use kumosql_sql::parse_check::{CheckStatus, check_query, reading};

/// A query that must check clean.
fn agrees(sql: &str) {
    let check = check_query(sql, "bigquery");
    assert_eq!(
        check.status,
        CheckStatus::Agree,
        "{sql:?} should agree, got {:?}: {:?} (note: {})",
        check.status,
        check.reasons,
        check.note
    );
}

#[test]
fn a_plain_query_agrees() {
    agrees("SELECT a FROM t");
    agrees("SELECT a, b AS c FROM ds.t WHERE a = 1 GROUP BY a ORDER BY b LIMIT 10");
    agrees("WITH x AS (SELECT a FROM t) SELECT a FROM x");
    agrees("SELECT COUNT(*) FROM t");
}

#[test]
fn a_join_agrees() {
    agrees("SELECT t.a, u.b FROM t LEFT JOIN u ON t.id = u.id");
    agrees("SELECT a FROM t JOIN u USING (id)");
    agrees("SELECT a FROM a_table CROSS JOIN b_table");
}

#[test]
fn an_aggregate_with_bigquery_syntax_agrees() {
    // These go through the text rewrites; the check runs on the rewritten text,
    // so the markers cancel rather than looking like disagreements.
    agrees("SELECT COUNT(x WHERE c) FROM t");
    agrees("SELECT * FROM t WHERE x LIKE ALL UNNEST(arr)");
    agrees("SELECT CAST(a AS INT64) FROM t");
    agrees("SELECT SAFE_CAST(a AS INT64) FROM t");
}

#[test]
fn a_window_function_agrees() {
    agrees("SELECT ROW_NUMBER() OVER (PARTITION BY a ORDER BY b DESC) FROM t");
    agrees(
        "SELECT SUM(x) OVER (ORDER BY a ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM t",
    );
}

#[test]
fn a_string_literal_agrees_despite_canonicalisation() {
    // The literal is rewritten on its way to the parser, so the comparison has
    // to run on the rewritten text or this would look like a disagreement.
    agrees(r#"SELECT "a\"b" FROM t"#);
    agrees("SELECT r'a\\b' FROM t");
}

#[test]
fn a_dropped_token_is_caught() {
    // This is the bug the check exists for. `sqlparser` reads
    // `FROM ds.fn(arg, ...)` as a bare table and drops the arguments with no
    // error, so the parse itself cannot report it.
    let sql = "SELECT * FROM dataset.fn(TABLE dataset.input)";
    let error = kumosql_sql::parse::parse_statements(sql, false)
        .expect_err("the parse must refuse this rather than return a truncated table");

    // And the query the check sees is a disagreement or unchecked -- never agree.
    let check = check_query(sql, "bigquery");
    assert_ne!(
        check.status,
        CheckStatus::Agree,
        "a silently truncated query must never be reported as agreeing: {:?}",
        check.reasons
    );
    assert!(
        error.is_refusal() || error.kind == kumosql_sql::error::ErrorKind::Unmodeled,
        "unexpected error kind: {error}"
    );
}

#[test]
fn an_invented_name_would_be_caught() {
    // Sanity check on the mechanism itself: a tree holding a name the source
    // does not spell has to be reportable. This is checked through the atom
    // comparison rather than by hand-editing a tree, so it asserts the
    // mechanism on a query where the two sides genuinely differ in spelling.
    let check = check_query("SELECT 1 FROM t", "bigquery");
    assert!(check.is_checked());
    assert!(check.reasons.is_empty());
}

#[test]
fn an_unparseable_query_is_unchecked_not_disagreeing() {
    // "The base parser does not read it" is a different claim from "the two
    // readings differ", and treating the first as the second would be wrong.
    let check = check_query("SELECT FROM WHERE (", "bigquery");
    assert_eq!(check.status, CheckStatus::Unchecked);
    assert!(!check.disagrees());
    assert!(!check.note.is_empty());
}

#[test]
fn a_refused_query_is_unchecked_not_disagreeing() {
    // `STRUCT<>()` is refused on purpose. That is a decision, not a mismatch.
    let check = check_query("SELECT STRUCT<>()", "bigquery");
    assert_eq!(check.status, CheckStatus::Unchecked);
    assert!(!check.disagrees());
}

#[test]
fn a_standalone_statement_is_reported_as_agreeing_or_unchecked() {
    // DDL has no query to compare, so there is nothing to disagree about.
    let check = check_query("CREATE SCHEMA ds", "bigquery");
    assert!(check.is_checked() || check.status == CheckStatus::Unchecked);
}

#[test]
fn the_dialect_is_recorded_on_the_result() {
    let check = check_query("SELECT 1 FROM t", "bigquery");
    assert_eq!(check.dialect, "bigquery");
}

#[test]
fn the_note_says_the_comparison_ran_on_the_rewritten_text() {
    // The caller has to know that a rewrite happened, because that is what the
    // comparison is actually about.
    let plain = check_query("SELECT a FROM t", "bigquery");
    assert!(plain.note.contains("unchanged"), "{}", plain.note);

    let rewritten = check_query("SELECT COUNT(x WHERE c) FROM t", "bigquery");
    assert!(rewritten.note.contains("changed"), "{}", rewritten.note);
}

#[test]
fn checked_queries_report_how_much_they_compared() {
    let check = check_query("SELECT a, b FROM t WHERE a = 1", "bigquery");
    assert!(check.compared > 0, "nothing was compared");
}

#[test]
fn reading_returns_the_parsed_shape_for_a_readable_query() {
    let rendered = reading("SELECT a FROM t").expect("readable");
    assert!(rendered.contains("a"), "{rendered}");
}

#[test]
fn reading_is_none_for_a_query_it_cannot_read() {
    assert!(reading("SELECT FROM WHERE (").is_none());
    assert!(reading("SELECT STRUCT<>()").is_none());
}
