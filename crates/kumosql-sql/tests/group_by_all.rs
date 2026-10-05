//! `GROUP BY ALL` is spelled out as its inferred keys before anything reasons
//! about it.
//!
//! Ported from `tests/test_soundness_group_by_all.py`.
//!
//! The Python file's assertions mostly go through the SMT and algebraic provers
//! and DuckDB. Those are ported in tasks 8, 9 and 12; the expansion itself and
//! its refusals are pure tree work, so they are tested here directly and the
//! prover-dependent tests are listed as pending below.

use kumosql_sql::ast::*;
use kumosql_sql::error::ErrorKind;
use kumosql_sql::normalize::{
    expand_group_by_all, expand_group_by_all_in_query, is_aggregate_name,
};
use kumosql_sql::parse::parse_statements;

/// Parse, expand every `GROUP BY ALL`, and hand back the single query.
fn expanded(sql: &str) -> Query {
    let mut statements =
        parse_statements(sql, false).unwrap_or_else(|e| panic!("failed to parse {sql:?}: {e}"));
    assert_eq!(statements.len(), 1);
    let mut query = statements
        .remove(0)
        .top_level_query()
        .cloned()
        .unwrap_or_else(|| panic!("no query in {sql:?}"));
    expand_group_by_all_in_query(&mut query)
        .unwrap_or_else(|e| panic!("failed to expand {sql:?}: {e}"));
    query
}

/// Parse, expand, and expect the refusal instead.
fn refused(sql: &str) -> kumosql_sql::error::Error {
    let mut statements =
        parse_statements(sql, false).unwrap_or_else(|e| panic!("failed to parse {sql:?}: {e}"));
    let mut query = statements
        .remove(0)
        .top_level_query()
        .cloned()
        .expect("a query");
    let error =
        expand_group_by_all_in_query(&mut query).expect_err("expected this shape to be declined");
    assert_eq!(
        error.kind,
        ErrorKind::Unmodeled,
        "unexpected error for {sql:?}: {error}"
    );
    assert!(
        error.detail.contains("GROUP BY ALL"),
        "the refusal should name the construct, got: {error}"
    );
    error
}

/// The `GROUP BY` clause of the statement's outermost select, as text.
fn group_by_text(sql: &str) -> String {
    let query = expanded(sql);
    let Query::Select { body, .. } = &query else {
        panic!("expected a select");
    };
    match &body.group_by {
        Some(group_by) => group_by.to_string(),
        None => "(none)".to_string(),
    }
}

#[test]
fn group_by_all_groups_by_the_non_aggregate_items() {
    assert_eq!(
        group_by_text("SELECT a, COUNT(*) AS n FROM t GROUP BY ALL"),
        "GROUP BY 1"
    );
    assert_eq!(
        group_by_text("SELECT a + 1 AS k, b, SUM(c) AS s FROM t GROUP BY ALL"),
        "GROUP BY 1, 2"
    );
}

#[test]
fn a_constant_is_not_a_key() {
    // Engines disagree on whether a constant is a grouping key, so it is simply
    // not one here and does not shift the positions.
    assert_eq!(
        group_by_text("SELECT 'x' AS tag, a, COUNT(*) AS n FROM t GROUP BY ALL"),
        "GROUP BY 2"
    );
}

#[test]
fn an_aggregate_only_select_becomes_a_global_aggregate() {
    // With no keys the query has one group even on empty input, so `GROUP BY
    // ALL` and no `GROUP BY` at all are the same query. Leaving the clause in
    // place is what made `COUNT(*) ... GROUP BY ALL` provable against a
    // genuinely grouped query.
    assert_eq!(
        group_by_text("SELECT COUNT(*) AS n FROM t GROUP BY ALL"),
        "(none)"
    );
}

#[test]
fn a_key_column_may_also_appear_next_to_an_aggregate() {
    // `a` is a key, so its appearance inside `COUNT(*) + a` is fine.
    assert_eq!(
        group_by_text("SELECT a, COUNT(*) + a AS n FROM t GROUP BY ALL"),
        "GROUP BY 1"
    );
}

#[test]
fn group_by_all_is_expanded_inside_ctes_and_derived_tables() {
    let query = expanded(
        "WITH c AS (SELECT a, b, COUNT(*) AS n FROM t GROUP BY ALL) \
         SELECT x.a FROM (SELECT a, MAX(n) AS m FROM c GROUP BY ALL) AS x",
    );
    let Query::Select { with, body } = &query else {
        panic!("expected a select");
    };
    let with = with.as_ref().expect("a WITH clause");
    let Query::Select { body: cte_body, .. } = with.ctes[0].query.as_ref() else {
        panic!("expected a CTE select");
    };
    assert_eq!(
        cte_body.group_by.as_ref().expect("a GROUP BY").to_string(),
        "GROUP BY 1, 2"
    );

    // And the derived table in the outer select: only `a` is a key there, since
    // `MAX(n)` is an aggregate. Together the two spell out `GROUP BY 1, 2` and
    // `GROUP BY 1`, which is what the Python test asserts.
    let Some(TableFactor::Subquery { query, .. }) = &body.from else {
        panic!("expected a derived table");
    };
    let Query::Select { body: inner, .. } = query.as_ref() else {
        panic!("expected an inner select");
    };
    assert_eq!(
        inner.group_by.as_ref().expect("a GROUP BY").to_string(),
        "GROUP BY 1"
    );
}

#[test]
fn an_explicit_group_by_is_left_alone() {
    assert_eq!(
        group_by_text("SELECT a, COUNT(*) AS n FROM t GROUP BY a"),
        "GROUP BY a"
    );
}

#[test]
fn group_by_all_beside_explicit_keys_is_declined() {
    // `GROUP BY ALL, b` never reaches this code: `sqlparser-rs` refuses the
    // syntax outright, so the query is already declined upstream. This test
    // covers the tree-level guard by setting both flags directly, which is the
    // shape the Python original refuses with `group.expressions`.
    let mut statements = parse_statements("SELECT a FROM t GROUP BY ALL", false).unwrap();
    let mut query = statements.remove(0).top_level_query().cloned().unwrap();
    let select = query.as_select_mut().expect("a select");
    let group_by = select.group_by.as_mut().expect("a GROUP BY");
    group_by.keys.push(Expr::Column(ObjectName::single("b")));

    let error = expand_group_by_all(select).expect_err("expected a refusal");
    assert_eq!(error.kind, ErrorKind::Unmodeled);
    assert!(error.detail.contains("GROUP BY ALL"));
}

#[test]
fn a_star_select_is_declined() {
    refused("SELECT * FROM t GROUP BY ALL");
}

#[test]
fn a_window_next_to_group_by_all_is_declined() {
    refused("SELECT a, SUM(b) OVER () AS w FROM t GROUP BY ALL");
}

#[test]
fn a_subquery_next_to_group_by_all_is_declined() {
    refused("SELECT a, (SELECT 1) AS s, COUNT(*) AS n FROM t GROUP BY ALL");
}

#[test]
fn an_unknown_function_is_declined() {
    // An unrecognised function may be an aggregate, which would change the keys.
    refused("SELECT a, MY_UDF(b) AS u, COUNT(*) AS n FROM t GROUP BY ALL");
}

#[test]
fn an_ungrouped_column_inside_an_aggregate_is_declined() {
    // `b` is read outside the aggregate but is not a key, so the query is not
    // valid as written and the keys cannot be inferred safely.
    refused("SELECT b, COUNT(*) + a AS n FROM t GROUP BY ALL");
}

#[test]
fn a_constant_only_select_is_declined() {
    // A key in DuckDB, not in BigQuery.
    refused("SELECT 1 AS k, COUNT(*) AS n FROM t GROUP BY ALL");
    refused("SELECT 1 AS k FROM t GROUP BY ALL");
}

#[test]
fn an_aggregate_catalogue_entry_is_recognised() {
    assert!(is_aggregate_name("COUNT"));
    assert!(is_aggregate_name("count"));
    assert!(is_aggregate_name("APPROX_COUNT_DISTINCT"));
    assert!(!is_aggregate_name("COALESCE"));
    assert!(!is_aggregate_name("MY_UDF"));
}

#[test]
fn expand_group_by_all_does_nothing_without_the_all_flag() {
    let mut statements = parse_statements("SELECT a, COUNT(*) FROM t GROUP BY a", false).unwrap();
    let mut query = statements.remove(0).top_level_query().cloned().unwrap();
    expand_group_by_all(query.as_select_mut().expect("a select")).unwrap();
}

#[test]
fn a_query_with_no_group_by_is_untouched() {
    let mut statements = parse_statements("SELECT 1", false).unwrap();
    let mut query = statements.remove(0).top_level_query().cloned().unwrap();
    expand_group_by_all(query.as_select_mut().expect("a select")).unwrap();
    let Query::Select { body, .. } = &query else {
        panic!("expected a select");
    };
    assert!(body.group_by.is_none());
}

// pending: `test_aggregate_only_group_by_all_is_not_proved_equal_to_a_grouped_query`
//   (needs the SMT and algebraic provers, tasks 8 and 9, plus DuckDB, task 12).
// pending: `test_aggregate_only_group_by_all_is_not_a_constant_key` (tasks 8, 9).
// pending: `test_aggregate_only_group_by_all_is_the_global_aggregate` (tasks 8, 9).
// pending: `test_group_by_all_shapes_that_are_declined` -- the provers half (tasks 8, 9);
//   the expand_group_by_all half is ported above.
// pending: `test_a_key_column_may_also_appear_next_to_an_aggregate` (tasks 8, 9).
// pending: `test_bounded_checker_reads_group_by_all_keys` (task 10).
