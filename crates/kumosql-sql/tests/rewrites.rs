//! The BigQuery rewrites in `rewrites.rs`, and the AST nodes they need.
//!
//! Ported from `bigquery_syntax.py`. Each rewrite exists because the base parser
//! cannot read the shape -- or, in the `LIKE ALL` case, cannot carry the
//! information. The marker-based design is the Python original's, and the tests
//! below pin both directions: the rewritten text must parse, and the AST must
//! come back out with the shape the source had.

use kumosql_sql::ast::*;
use kumosql_sql::parse::{parse_statements, render_statements};
use kumosql_sql::rewrites::{
    LIKE_ALL_MARKER, TABLE_ARGUMENT_MARKER, WITH_MARKER, WITH_VARIABLE_MARKER,
    merge_table_function_kind, rewrite_like_quantifiers, rewrite_table_arguments,
    rewrite_with_expressions,
};

/// Rewrite, then parse, then report the statement's text.
fn round_trip(sql: &str, rewritten: &str) -> String {
    assert!(
        parse_statements(sql, false).is_ok(),
        "the rewritten form of {sql:?} should parse, got {rewritten:?}"
    );
    render_statements(&parse_statements(sql, false).expect("parse"))
}

// ---------------------------------------------------------------- LIKE quantifier

#[test]
fn like_all_unnest_is_rewritten_so_the_parser_reads_it() {
    let rewritten = rewrite_like_quantifiers("SELECT * FROM t WHERE x LIKE ALL UNNEST(arr)");
    assert!(rewritten.contains("LIKE ANY"));
    assert!(rewritten.contains(&format!("{LIKE_ALL_MARKER}(")));
}

#[test]
fn like_all_unnest_round_trips_with_its_all_quantifier() {
    // `ALL` and `ANY` differ on empty input, so the quantifier must survive.
    let rendered = round_trip(
        "SELECT * FROM t WHERE x LIKE ALL UNNEST(arr)",
        &rewrite_like_quantifiers("SELECT * FROM t WHERE x LIKE ALL UNNEST(arr)"),
    );
    assert!(rendered.contains("ALL"), "lost the quantifier: {rendered}");

    let statements =
        parse_statements("SELECT * FROM t WHERE x LIKE ALL UNNEST(arr)", false).expect("parse");
    let text = render_statements(&statements);
    assert_eq!(text, "SELECT * FROM t WHERE x LIKE ALL arr");
}

#[test]
fn like_any_is_not_given_an_all_quantifier() {
    let statements =
        parse_statements("SELECT * FROM t WHERE x LIKE ANY arr", false).expect("parse");
    let text = render_statements(&statements);
    assert!(
        text.contains("ANY"),
        "expected an ANY quantifier, got {text}"
    );
    assert!(!text.contains("ALL"), "ALL leaked into an ANY: {text}");
}

#[test]
fn like_some_is_bigquerys_older_spelling_of_any() {
    assert_eq!(
        rewrite_like_quantifiers("SELECT * FROM t WHERE x LIKE SOME arr"),
        "SELECT * FROM t WHERE x LIKE ANY arr"
    );
}

#[test]
fn a_plain_like_is_left_alone() {
    for sql in [
        "SELECT * FROM t WHERE x LIKE 'a%'",
        "SELECT * FROM t WHERE x NOT LIKE 'a%'",
    ] {
        assert_eq!(rewrite_like_quantifiers(sql), sql);
    }
}

#[test]
fn all_not_before_unnest_is_not_a_quantified_like() {
    // `ALL` here is a set quantifier, not a `LIKE` quantifier.
    let sql = "SELECT * FROM t WHERE x = ALL (SELECT y FROM u)";
    assert_eq!(rewrite_like_quantifiers(sql), sql);
}

#[test]
fn a_marker_in_the_source_is_not_mistaken_for_a_rewrite() {
    // A user query that already mentions the marker must not be rewritten into
    // an `ALL` it never had.
    let sql = format!("SELECT * FROM t WHERE x LIKE ANY UNNEST({LIKE_ALL_MARKER}(arr))");
    let statements = parse_statements(&sql, false).expect("parse");
    // The marker is still there and still reads as `ALL`: that is the marker
    // doing its job, and it is the reason the marker is not a bare word.
    assert!(render_statements(&statements).contains("ALL"));
}

// ---------------------------------------------------------------- WITH expression

#[test]
fn a_with_expression_is_rewritten_into_markers() {
    let rewritten = rewrite_with_expressions("SELECT WITH(a AS 1, a + 1)");
    assert!(rewritten.contains(WITH_MARKER), "{rewritten}");
    assert!(rewritten.contains(WITH_VARIABLE_MARKER), "{rewritten}");
}

#[test]
fn a_with_clause_is_not_an_expression() {
    // The `WITH` clause opens with a name, not `(`, so it must be untouched.
    for sql in [
        "WITH t AS (SELECT 1) SELECT * FROM t",
        "WITH a AS (SELECT 1), b AS (SELECT 2) SELECT * FROM a",
    ] {
        assert_eq!(
            rewrite_with_expressions(sql),
            sql,
            "clause rewritten: {sql}"
        );
    }
}

#[test]
fn a_with_expression_with_one_argument_is_not_the_shape() {
    // `WITH(a)` has no bindings, so it is not the expression form.
    let sql = "SELECT WITH(a)";
    assert_eq!(rewrite_with_expressions(sql), sql);
}

#[test]
fn a_binding_whose_name_is_a_keyword_is_not_the_shape() {
    let sql = "SELECT WITH(SELECT AS 1, x)";
    assert_eq!(rewrite_with_expressions(sql), sql);
}

// ---------------------------------------------------------------- TABLE argument

#[test]
fn a_table_argument_is_marked_so_the_call_survives() {
    let rewritten =
        rewrite_table_arguments("SELECT * FROM dataset.fn(TABLE dataset.input, opt => 1)");
    assert!(rewritten.contains(TABLE_ARGUMENT_MARKER), "{rewritten}");
}

#[test]
fn a_table_argument_is_declined_rather_than_read_without_its_table() {
    // `sqlparser` reads `FROM ds.fn(arg, ...)` as a bare table and drops the
    // arguments with no error at all. Returning that would be silent data loss,
    // so the query is declined. Recorded as a known gap in docs/roadmap.md.
    let sql = "SELECT * FROM dataset.fn(TABLE dataset.input)";
    let error = parse_statements(sql, false)
        .expect_err("the arguments would be silently dropped, so this must not succeed");
    assert_eq!(error.kind, kumosql_sql::error::ErrorKind::Unmodeled);
    assert!(
        error.detail.contains("loses its arguments"),
        "unexpected message: {error}"
    );
}

#[test]
fn a_table_argument_in_the_projection_is_kept() {
    // In an expression position the argument survives, so this one is readable.
    let statements = parse_statements("SELECT fn(TABLE ds.input) FROM t", false).expect("parse");
    let rendered = render_statements(&statements);
    assert!(rendered.contains("TABLE ds.input"), "{rendered}");
}

#[test]
fn a_bare_table_keyword_outside_a_call_is_not_an_argument() {
    // `FROM TABLE(...)` and a CTE named TABLE are not call arguments.
    for sql in ["SELECT * FROM t", "CREATE TABLE t AS SELECT 1"] {
        assert_eq!(rewrite_table_arguments(sql), sql, "rewritten: {sql}");
    }
}

#[test]
fn several_table_arguments_are_each_marked() {
    let rewritten = rewrite_table_arguments("SELECT * FROM f(TABLE a.b, TABLE c.d, opt => 1)");
    assert_eq!(
        rewritten.matches(TABLE_ARGUMENT_MARKER).count(),
        2,
        "{rewritten}"
    );
}

// ---------------------------------------------------------------- DROP TABLE FUNCTION

#[test]
fn drop_table_function_becomes_one_token() {
    assert_eq!(
        merge_table_function_kind("DROP TABLE FUNCTION ds.fn"),
        "DROP TABLE FUNCTION ds.fn"
    );
}

#[test]
fn a_plain_drop_is_untouched() {
    for sql in ["DROP TABLE t", "DROP FUNCTION ds.fn", "DROP VIEW v"] {
        assert_eq!(merge_table_function_kind(sql), sql);
    }
}

#[test]
fn drop_table_function_is_declined_rather_than_misread() {
    // `sqlparser` reads `DROP TABLE FUNCTION` as a drop of the table named
    // `FUNCTION` and then refuses the real name. There is no rewrite that makes
    // it read correctly, so it is declined here and belongs on the
    // command/splitter path. See the known gap in docs/roadmap.md.
    let error = parse_statements("DROP TABLE FUNCTION ds.fn", false)
        .expect_err("this shape must not be read as dropping the table FUNCTION");
    assert!(
        matches!(
            error.kind,
            kumosql_sql::error::ErrorKind::UnsupportedSyntax
                | kumosql_sql::error::ErrorKind::Unmodeled
        ),
        "unexpected kind: {error}"
    );
}

// ---------------------------------------------------------------- quantifier display

#[test]
fn the_quantifier_renders_as_written() {
    assert_eq!(LikeQuantifier::Any.to_string(), "ANY");
    assert_eq!(LikeQuantifier::All.to_string(), "ALL");
}

#[test]
fn a_bare_like_renders_without_a_quantifier() {
    let like = Expr::Like {
        expr: Box::new(Expr::Column(ObjectName::single("x"))),
        pattern: Box::new(Expr::Literal(Literal::String("a%".into()))),
        negated: false,
        quantifier: None,
    };
    assert_eq!(like.to_string(), "x LIKE 'a%'");
}

#[test]
fn a_quantified_like_renders_with_its_quantifier() {
    let like = Expr::Like {
        expr: Box::new(Expr::Column(ObjectName::single("x"))),
        pattern: Box::new(Expr::Column(ObjectName::single("arr"))),
        negated: false,
        quantifier: Some(LikeQuantifier::All),
    };
    assert_eq!(like.to_string(), "x LIKE ALL arr");
}
