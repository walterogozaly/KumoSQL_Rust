//! The parse pipeline end to end: text -> literals -> rewrites -> sqlparser ->
//! the kumosql AST, and back.
//!
//! The cases are the shapes that discriminate between a BigQuery reader and a
//! generic one, plus the shapes that must be *refused* rather than guessed at.

use kumosql_sql::ast::*;
use kumosql_sql::error::ErrorKind;
use kumosql_sql::parse::{parse_statements, render_statements};

/// Parse one statement, panicking with the error text if it fails.
fn one(sql: &str) -> Statement {
    let statements =
        parse_statements(sql, false).unwrap_or_else(|e| panic!("failed to parse {sql:?}: {e}"));
    assert_eq!(statements.len(), 1, "expected one statement from {sql:?}");
    statements.into_iter().next().unwrap()
}

/// Parse and return the rendered text, for round-trip checks.
fn round_trip(sql: &str) -> String {
    render_statements(&parse_statements(sql, false).expect("parse"))
}

#[test]
fn a_plain_select_parses() {
    let statements = one("SELECT a FROM t");
    let Statement::Query(Query::Select { with, body }) = &statements else {
        panic!("expected a select, got {statements:?}");
    };
    assert!(with.is_none());
    assert_eq!(body.projections.len(), 1);
    assert!(matches!(body.from, Some(TableFactor::Table { .. })));
}

#[test]
fn a_with_clause_becomes_a_with_on_the_query() {
    let statements = one("WITH t AS (SELECT 1) SELECT * FROM t");
    let Statement::Query(Query::Select { with, .. }) = &statements else {
        panic!("expected a select");
    };
    let with = with.as_ref().expect("a WITH clause");
    assert_eq!(with.ctes.len(), 1);
    assert_eq!(with.ctes[0].name, Ident::bare("t"));
}

#[test]
fn a_union_all_keeps_its_quantifier() {
    let statements = one("SELECT 1 UNION ALL SELECT 2");
    let Statement::Query(Query::SetOperation {
        op,
        duplicate_handling,
        ..
    }) = &statements
    else {
        panic!("expected a set operation, got {statements:?}");
    };
    assert_eq!(*op, SetOp::Union);
    assert_eq!(*duplicate_handling, DuplicateHandling::All);
}

#[test]
fn the_aggregate_filter_survives_the_round_trip() {
    // BigQuery's own spelling must come back out, not the temporary standard
    // form the base parser needed.
    assert_eq!(
        round_trip("SELECT COUNT(x WHERE c) FROM t"),
        "SELECT COUNT(x WHERE c) FROM t"
    );
}

#[test]
fn the_aggregate_filter_becomes_a_filter_node() {
    let statements = one("SELECT COUNT(x WHERE c) FROM t");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    match &body.projections[0] {
        Expr::Filter {
            aggregate,
            predicate,
        } => {
            assert!(matches!(**aggregate, Expr::Function { .. }));
            assert!(matches!(**predicate, Expr::Column(_)));
        }
        other => panic!("expected a filter node, got {other:?}"),
    }
}

#[test]
fn a_standard_filter_is_kept_as_a_filter_node_too() {
    let statements = one("SELECT COUNT(x) FILTER (WHERE c) FROM t");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    assert!(matches!(body.projections[0], Expr::Filter { .. }));
}

#[test]
fn unnest_with_offset_keeps_the_offset() {
    let statements = one("SELECT * FROM UNNEST([1, 2]) AS x WITH OFFSET AS o");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    match &body.from {
        Some(TableFactor::Unnest { offset, alias, .. }) => {
            assert_eq!(offset.as_ref().unwrap(), &Some(Ident::bare("o")));
            assert_eq!(alias.as_ref().unwrap(), &Ident::bare("x"));
        }
        other => panic!("expected UNNEST, got {other:?}"),
    }
}

#[test]
fn star_except_becomes_a_modified_star() {
    let statements = one("SELECT * EXCEPT (a, b) FROM t");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    match &body.projections[0] {
        Expr::ModifiedStar(StarModifier::Except(cols)) => {
            assert_eq!(cols, &vec![Ident::bare("a"), Ident::bare("b")]);
        }
        other => panic!("expected * EXCEPT, got {other:?}"),
    }
}

#[test]
fn a_left_join_keeps_its_kind_and_condition() {
    let statements = one("SELECT * FROM a LEFT JOIN b ON a.id = b.id");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    match &body.from {
        Some(TableFactor::Join {
            kind, on, using, ..
        }) => {
            assert_eq!(*kind, JoinKind::Left);
            assert!(on.is_some());
            assert!(using.is_none());
        }
        other => panic!("expected a join, got {other:?}"),
    }
}

#[test]
fn using_is_read_separately_from_on() {
    let statements = one("SELECT * FROM a JOIN b USING (id)");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    match &body.from {
        Some(TableFactor::Join { on, using, .. }) => {
            assert!(on.is_none());
            assert_eq!(using.as_ref().unwrap(), &vec![Ident::bare("id")]);
        }
        other => panic!("expected a join, got {other:?}"),
    }
}

#[test]
fn safe_cast_is_distinguished_from_cast() {
    // They are different functions: SAFE_CAST returns NULL on failure where
    // CAST raises, so the provers must not conflate them.
    let statements = one("SELECT SAFE_CAST(a AS INT64), CAST(a AS INT64) FROM t");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    let flags: Vec<bool> = body
        .projections
        .iter()
        .map(|p| match p {
            Expr::Cast { safe, .. } => *safe,
            other => panic!("expected a cast, got {other:?}"),
        })
        .collect();
    assert_eq!(flags, vec![true, false]);
}

#[test]
fn group_by_all_is_recognised() {
    let statements = one("SELECT a, COUNT(*) FROM t GROUP BY ALL");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    let group_by = body.group_by.as_ref().expect("a GROUP BY");
    assert!(group_by.all);
}

#[test]
fn a_where_in_a_subquery_is_not_taken_for_an_aggregate_filter() {
    let statements = one("SELECT COUNT(*) FROM (SELECT 1 FROM t WHERE c) AS s");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    // The subquery's WHERE must survive as a selection, not become a filter.
    match &body.from {
        Some(TableFactor::Subquery { query, .. }) => {
            let Query::Select { body: inner, .. } = query.as_ref() else {
                panic!("expected an inner select");
            };
            assert!(inner.selection.is_some());
        }
        other => panic!("expected a derived table, got {other:?}"),
    }
}

#[test]
fn multiple_statements_are_split() {
    let statements = parse_statements("SELECT 1; SELECT 2", false).expect("parse");
    assert_eq!(statements.len(), 2);
}

#[test]
fn create_table_as_unwraps_to_its_query() {
    let statements = one("CREATE TABLE t AS SELECT 1");
    let Statement::CreateTableAs { name, query, .. } = &statements else {
        panic!("expected a CTAS, got {statements:?}");
    };
    assert_eq!(name.to_string(), "t");
    assert!(matches!(**query, Query::Select { .. }));
}

#[test]
fn an_unmodelled_statement_is_kept_verbatim_and_flagged() {
    let statements = one("CREATE SCHEMA ds");
    let statement = statements;
    assert!(!statement.is_modelled());
    // The text is preserved so it can be re-emitted, which is what makes the
    // statement unmodelled rather than lost.
    assert_eq!(statement.to_string(), "CREATE SCHEMA ds");
}

#[test]
fn struct_comparison_is_refused_rather_than_guessed() {
    let error = parse_statements("SELECT STRUCT<>()", false).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Rejected);
    assert!(error.is_refusal());
}

#[test]
fn an_unparseable_statement_is_reported_not_swallowed() {
    let error = parse_statements("SELECT FROM WHERE (", false).unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedSyntax);
}

#[test]
fn recovery_does_not_pretend_to_have_recovered() {
    // The recovery fallback is not implemented yet. Saying so is the honest
    // outcome; returning a partial result would look like success.
    let error = parse_statements("SELECT FROM WHERE (", true).unwrap_err();
    assert!(
        error.detail.contains("recovery is not implemented"),
        "unexpected message: {error}"
    );
}

#[test]
fn a_string_literal_is_canonicalised_on_the_way_in() {
    // `"x"` and `'x'` are the same BigQuery string, so both parse to the same
    // literal. This is the pipeline's literals stage doing its job.
    let a = one("SELECT \"x\" AS c FROM t");
    let b = one("SELECT 'x' AS c FROM t");
    assert_eq!(a, b);
}

#[test]
fn a_bytes_literal_is_decoded_to_one_spelling() {
    // `b'\x41'` is the single byte `A`, which canonicalises to `b'A'`.
    let statements = one("SELECT b'\\x41' AS v FROM t");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    match &body.projections[0] {
        Expr::Alias {
            expr,
            alias: Ident { name, .. },
        } => {
            assert_eq!(name, "v");
            match expr.as_ref() {
                Expr::Literal(Literal::Bytes(bytes)) => assert_eq!(bytes, b"A"),
                other => panic!("expected a bytes literal, got {other:?}"),
            }
        }
        other => panic!("expected an alias, got {other:?}"),
    }
}

#[test]
fn a_quoted_identifier_stays_distinct_from_a_bare_one() {
    let quoted = one("SELECT `A` FROM t");
    let bare = one("SELECT a FROM t");
    assert_ne!(quoted, bare);
}

#[test]
fn a_window_function_keeps_its_specification() {
    let statements = one("SELECT ROW_NUMBER() OVER (PARTITION BY a ORDER BY b DESC) FROM t");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    match &body.projections[0] {
        Expr::Window { spec, .. } => {
            assert_eq!(spec.partition_by.len(), 1);
            let order_by = spec.order_by.as_ref().expect("an ORDER BY");
            assert_eq!(order_by.keys[0].descending, Some(true));
        }
        other => panic!("expected a window, got {other:?}"),
    }
}

#[test]
fn a_derived_table_keeps_its_alias() {
    let statements = one("SELECT * FROM (SELECT 1) AS s");
    let Statement::Query(Query::Select { body, .. }) = &statements else {
        panic!("expected a select");
    };
    match &body.from {
        Some(TableFactor::Subquery { alias, .. }) => {
            assert_eq!(alias.as_ref().unwrap(), &Ident::bare("s"));
        }
        other => panic!("expected a derived table, got {other:?}"),
    }
}

#[test]
fn a_rendered_query_is_readable_sql() {
    // Rendering is allowed to differ in spacing from the input; what matters is
    // that it parses back to the same statement.
    let sql = "SELECT t.a AS x, COUNT(*) FROM t WHERE (t.a > 1) GROUP BY x";
    let first = one(sql);
    let second = one(&render_statements(std::slice::from_ref(&first)));
    assert_eq!(first, second);
}
