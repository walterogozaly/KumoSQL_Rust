//! `lift_subqueries`, and the parentheses rule's documented no-op.
//!
//! Ported from `tests/test_lift_subqueries.py` and the rule table in the Python
//! README.

use kumosql_rules::RewriteRule;
use kumosql_rules::lift::{LiftSubqueries, RemoveRedundantParentheses};
use kumosql_rules::{apply_rule, apply_rule_to_sql};

/// Apply the rule and return the text.
fn lift(sql: &str) -> String {
    let output = apply_rule(&LiftSubqueries, sql);
    assert!(
        !output.diagnostics.iter().any(|d| d.code == "parse_error"),
        "{sql:?} failed to parse: {:?}",
        output.diagnostics
    );
    output.sql
}

#[test]
fn a_derived_table_is_lifted_into_a_cte() {
    let out = lift("SELECT * FROM (SELECT a FROM t) AS s");
    assert!(out.starts_with("WITH _subquery_1 AS ("), "{out}");
    assert!(out.contains("FROM _subquery_1 AS s"), "{out}");
    // The body is unchanged.
    assert!(out.contains("SELECT a FROM t"), "{out}");
}

#[test]
fn a_derived_table_in_a_join_is_lifted() {
    let out = lift("SELECT * FROM a JOIN (SELECT id FROM b) AS s ON a.id = s.id");
    assert!(
        out.contains("WITH _subquery_1 AS (SELECT id FROM b)"),
        "{out}"
    );
    assert!(out.contains("JOIN _subquery_1 AS s"), "{out}");
}

#[test]
fn several_derived_tables_get_distinct_names() {
    let out = lift("SELECT * FROM (SELECT a FROM t) AS x JOIN (SELECT b FROM u) AS y ON x.a = y.b");
    assert!(out.contains("_subquery_1 AS"), "{out}");
    assert!(out.contains("_subquery_2 AS"), "{out}");
    assert!(out.contains("FROM _subquery_1 AS x"), "{out}");
    assert!(out.contains("JOIN _subquery_2 AS y"), "{out}");
}

#[test]
fn a_lifted_name_does_not_shadow_an_existing_one() {
    // The query already binds `_subquery_1`, so the lifted CTE takes another
    // name rather than colliding with it.
    let out = lift("WITH _subquery_1 AS (SELECT 0) SELECT * FROM (SELECT a FROM t) AS s");
    assert!(out.contains("_subquery_2 AS (SELECT a FROM t)"), "{out}");
}

#[test]
fn a_query_with_no_derived_table_is_unchanged() {
    let sql = "SELECT a FROM t";
    assert_eq!(lift(sql), sql);
}

#[test]
fn a_subquery_in_a_predicate_is_left_alone() {
    // An `IN (SELECT ...)` is not a derived table; lifting it would change where
    // it reads from.
    let sql = "SELECT a FROM t WHERE a IN (SELECT b FROM u)";
    assert_eq!(lift(sql), sql);
}

#[test]
fn a_derived_table_with_its_own_with_stays_and_is_reported() {
    // The names the inner `WITH` binds are not visible to a CTE lifted to the
    // top, so lifting it would change the query. Reported, not done quietly.
    let sql = "SELECT * FROM (WITH inner_cte AS (SELECT 1) SELECT * FROM inner_cte) AS s";
    let output = apply_rule(&LiftSubqueries, sql);
    assert!(
        output.sql.contains("(WITH inner_cte AS (SELECT 1)"),
        "the derived table should stay: {}",
        output.sql
    );
    let diagnostic = output
        .diagnostic("correlated_subquery_kept")
        .expect("a correlated_subquery_kept diagnostic");
    assert!(diagnostic.message.contains("nested WITH"), "{diagnostic:?}");
}

#[test]
fn a_survivor_is_a_failure_not_a_partial_success() {
    // A rule that lifted some subqueries and gave up on others has not done its
    // job. `count_remaining` is what makes that visible.
    let output = apply_rule(&LiftSubqueries, "SELECT * FROM (SELECT a FROM t) AS s");
    assert_eq!(output.remaining, 0, "a fully lifted query has nothing left");
    assert!(
        output.success(),
        "a completed lift should succeed; diagnostics were {:?}",
        output
            .diagnostics
            .iter()
            .map(|d| &d.code)
            .collect::<Vec<_>>()
    );

    // And the rule reports what it left alone.
    let kept = apply_rule(
        &LiftSubqueries,
        "SELECT * FROM (WITH x AS (SELECT 1) SELECT * FROM x) AS s",
    );
    assert_eq!(
        kept.remaining, 0,
        "the kept subquery is not liftable, so it is not 'remaining'"
    );
}

#[test]
fn lift_reports_how_many_it_lifted() {
    let output = apply_rule(
        &LiftSubqueries,
        "SELECT * FROM (SELECT a FROM t) AS x JOIN (SELECT b FROM u) AS y ON x.a = y.b",
    );
    assert_eq!(output.changed_statements, 1);
    assert_eq!(output.changes, 2, "two derived tables lifted");
}

#[test]
fn lifting_is_idempotent() {
    // Running the rule on its own output changes nothing: there are no derived
    // tables left to lift.
    let once = lift("SELECT * FROM (SELECT a FROM t) AS x");
    let twice = apply_rule_to_sql(&LiftSubqueries, &once).sql;
    assert_eq!(once, twice);
}

#[test]
fn lifting_is_idempotent_for_several_subqueries() {
    let once =
        lift("SELECT * FROM (SELECT a FROM t) AS x JOIN (SELECT b FROM u) AS y ON x.a = y.b");
    let twice = apply_rule_to_sql(&LiftSubqueries, &once).sql;
    assert_eq!(once, twice);
}

#[test]
fn a_derived_table_with_no_alias_still_lifts() {
    let out = lift("SELECT * FROM (SELECT a FROM t)");
    assert!(out.contains("WITH _subquery_1 AS ("), "{out}");
    assert!(out.contains("FROM _subquery_1"), "{out}");
}

// ------------------------------------------------------- redundant parentheses

#[test]
fn removing_redundant_parentheses_is_a_no_op_here() {
    // The AST has no parenthesis node, so there is nothing to remove. That is
    // the safe direction, and it is deliberate -- see the rule's documentation.
    let sql = "SELECT * FROM t WHERE ((a = 1) AND (b = 2))";
    let output = apply_rule_to_sql(&RemoveRedundantParentheses, sql);
    assert_eq!(output.sql, sql);
    assert_eq!(output.changes, 0);
    assert!(output.success());
}

#[test]
fn the_parentheses_rule_leaves_meaning_alone() {
    // The rendered output parenthesises more than the Python original, but a
    // comparison's meaning must not depend on that.
    let sql = "SELECT * FROM t WHERE (a OR b) AND c";
    let before = apply_rule_to_sql(&RemoveRedundantParentheses, sql).sql;
    let after = apply_rule_to_sql(
        &RemoveRedundantParentheses,
        &apply_rule_to_sql(&RemoveRedundantParentheses, &before).sql,
    )
    .sql;
    assert_eq!(before, after);
}

#[test]
fn the_rule_is_registered_under_its_documented_name() {
    assert_eq!(
        RemoveRedundantParentheses.name(),
        "remove_redundant_parentheses"
    );
    assert_eq!(LiftSubqueries.name(), "lift_subqueries");
    assert!(!LiftSubqueries.summary().is_empty());
}
