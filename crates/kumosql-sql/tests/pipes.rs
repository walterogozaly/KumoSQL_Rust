//! Pipe operators.
//!
//! `sqlparser` 0.59 models `|>` natively -- including `SET` and `DROP` -- so
//! unlike the Python original this port needs no text rewrite: each stage
//! becomes a node. These tests pin that, and pin the one thing that matters
//! most about a pipe, which is that a stage changes what the query reads.

use kumosql_sql::ast::*;
use kumosql_sql::parse::{parse_statements, render_statements};

/// The stages of the single query in `sql`.
fn stages(sql: &str) -> Vec<PipeStage> {
    let statements =
        parse_statements(sql, false).unwrap_or_else(|e| panic!("failed to parse {sql:?}: {e}"));
    let query = statements[0].top_level_query().expect("a query");
    let Query::Pipe { stages, .. } = query else {
        panic!("{sql:?} is not a piped query: {query:?}");
    };
    stages.clone()
}

#[test]
fn a_pipe_becomes_its_own_query_kind() {
    let statements = parse_statements("SELECT * FROM t |> WHERE a = 1", false).expect("parse");
    let query = statements[0].top_level_query().expect("a query");
    assert!(matches!(query, Query::Pipe { .. }), "got {query:?}");
}

#[test]
fn a_where_stage_is_modelled() {
    let found = stages("SELECT * FROM t |> WHERE a = 1");
    assert_eq!(found.len(), 1);
    match &found[0] {
        PipeStage::Where(expr) => {
            assert!(matches!(
                expr,
                Expr::Binary {
                    op: BinaryOp::Eq,
                    ..
                }
            ));
        }
        other => panic!("expected a WHERE stage, got {other:?}"),
    }
    assert!(found[0].is_modelled());
}

#[test]
fn a_set_stage_is_modelled_without_a_rewrite() {
    // The Python original rewrites `|> SET c = 1` into `|> SELECT * REPLACE`
    // because sqlglot cannot read it. sqlparser reads it directly, so the stage
    // keeps its own shape here.
    let found = stages("SELECT * FROM t |> SET c = 1");
    match &found[0] {
        PipeStage::Set(pairs) => {
            assert_eq!(pairs.len(), 1);
            assert_eq!(pairs[0].0, Ident::bare("c"));
            assert!(matches!(pairs[0].1, Expr::Literal(Literal::Number(_))));
        }
        other => panic!("expected a SET stage, got {other:?}"),
    }
}

#[test]
fn a_drop_stage_is_modelled() {
    let found = stages("SELECT * FROM t |> DROP a, b");
    match &found[0] {
        PipeStage::Drop(columns) => {
            assert_eq!(columns, &vec![Ident::bare("a"), Ident::bare("b")]);
        }
        other => panic!("expected a DROP stage, got {other:?}"),
    }
}

#[test]
fn several_stages_keep_their_order() {
    let found = stages("SELECT * FROM t |> WHERE a = 1 |> SET c = 2 |> ORDER BY c");
    let keywords: Vec<&str> = found.iter().map(|s| s.keyword()).collect();
    assert_eq!(keywords, vec!["WHERE", "SET", "ORDER BY"]);
}

#[test]
fn an_unmodelled_stage_keeps_its_text_and_says_so() {
    // `|> AGGREGATE` is not one this port reasons about, but the text survives
    // so the query can be re-emitted and reported rather than truncated.
    let found = stages("SELECT * FROM t |> AGGREGATE SUM(x) AS total");
    match &found[0] {
        PipeStage::Other { keyword, text } => {
            assert_eq!(keyword, "AGGREGATE");
            assert!(text.contains("SUM"), "{text}");
        }
        other => panic!("expected an unmodelled stage, got {other:?}"),
    }
    assert!(!found[0].is_modelled());
}

#[test]
fn an_unmodelled_stage_makes_the_piped_query_unmodelled() {
    // A rule must not treat `|> AGGREGATE` as if it were not there.
    let found = stages("SELECT * FROM t |> WHERE a = 1 |> AGGREGATE SUM(x) AS total");
    assert!(
        found.iter().any(|s| !s.is_modelled()),
        "the AGGREGATE stage should be reported unmodelled"
    );
}

#[test]
fn a_piped_query_round_trips() {
    for sql in [
        "SELECT * FROM t |> WHERE a = 1",
        "SELECT * FROM t |> SET c = 1",
        "SELECT * FROM t |> DROP a, b",
        "SELECT * FROM t |> SELECT c",
        "SELECT * FROM t |> AS s",
    ] {
        let first = parse_statements(sql, false).expect("parse");
        let rendered = render_statements(&first);
        let second = parse_statements(&rendered, false)
            .unwrap_or_else(|e| panic!("{rendered:?} should parse again: {e}"));
        assert_eq!(first, second, "round trip changed {sql:?} -> {rendered:?}");
    }
}

#[test]
fn a_pipe_attaches_to_the_query_it_follows() {
    let statements = parse_statements("SELECT a FROM t |> WHERE a = 1", false).expect("parse");
    let query = statements[0].top_level_query().expect("a query");
    let Query::Pipe { base, .. } = query else {
        panic!("expected a pipe");
    };
    assert!(
        matches!(base.as_ref(), Query::Select { .. }),
        "got {base:?}"
    );
}

#[test]
fn a_piped_query_is_never_constant() {
    // A stage reads the rows before it, so a piped query never returns nothing
    // on empty input by definition.
    let statements = parse_statements("SELECT 1 |> WHERE TRUE", false).expect("parse");
    let query = statements[0].top_level_query().expect("a query");
    assert!(!query.is_constant());
}

#[test]
fn a_limit_stage_keeps_both_bounds() {
    let found = stages("SELECT * FROM t |> LIMIT 10");
    match &found[0] {
        PipeStage::Limit { limit, offset } => {
            assert!(limit.is_some());
            assert!(offset.is_none());
        }
        other => panic!("expected a LIMIT stage, got {other:?}"),
    }
}

#[test]
fn a_query_without_a_pipe_is_not_a_pipe() {
    let statements = parse_statements("SELECT a FROM t", false).expect("parse");
    let query = statements[0].top_level_query().expect("a query");
    assert!(matches!(query, Query::Select { .. }), "got {query:?}");
}
