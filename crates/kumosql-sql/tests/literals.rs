//! BigQuery string literals and quoted names get one spelling before the
//! provers and the execution checks read them.
//!
//! Ported from `tests/test_string_literals.py` in the Python original.
//!
//! Note on literal syntax: several cases contain both `'` and `"`, so they are
//! written as `r#"..."#`. A plain `r"..."` would end at the first `"` inside
//! the SQL.
//!
//! The cases that need a prover or a DuckDB connection are ported in tasks 7,
//! 9 and 12, and are marked `pending` below with the Python test they come
//! from. They are listed rather than dropped: see `docs/parity-notes.md`.

use kumosql_sql::literals::{canonical_literals, invalid_literal};

// The case table is generated from the Python original's own test file by
// tools/dump_literal_cases.py, so it cannot drift from the reference. That
// generator also self-checks its cases against the Python implementation, so a
// mismatch here means the Rust port is wrong, not the table.
include!("generated/literal_cases.rs");

#[test]
fn canonical_literals_matches_the_python_original() {
    for (sql, expected) in CANONICAL_CASES {
        assert_eq!(
            &canonical_literals(sql),
            expected,
            "canonical_literals({sql:?})"
        );
    }
}

#[test]
fn canonical_literals_is_idempotent() {
    for (sql, _) in CANONICAL_CASES {
        let once = canonical_literals(sql);
        assert_eq!(
            canonical_literals(&once),
            once,
            "not idempotent for {sql:?}"
        );
    }
}

#[test]
fn invalid_literal_rejects_a_broken_string() {
    // BigQuery rejects an unclosed literal; only a triple-quoted string may
    // span lines.
    assert!(invalid_literal("SELECT 'a\nb' AS v FROM t"));
    assert!(!invalid_literal("SELECT '''a\nb''' AS v FROM t"));
}

#[test]
fn invalid_literal_ignores_newlines_outside_literals() {
    assert!(!invalid_literal("SELECT 1\nFROM t"));
    assert!(!invalid_literal("SELECT 1 -- a comment\nFROM t"));
    assert!(!invalid_literal("SELECT 1 /* a\ncomment */ FROM t"));
}

#[test]
fn a_backtick_name_may_span_lines() {
    // A quoted name is not a string literal, so a newline inside one is not
    // the invalid-literal case.
    assert!(!invalid_literal("SELECT `a\nb` FROM t"));
}

// pending: `test_a_string_broken_over_two_lines_is_not_proven_equal_to_its_escaped_form`
//   needs `prove_equivalent` (task 7) and `prove_equivalent_algebraic` (task 9).
// pending: `test_the_prover_agrees_with_bigquery_on_escaped_strings` (task 9).
// pending: `test_execution_check_reads_escapes_like_bigquery` (needs DuckDB, task 12).
// pending: `test_equal_strings_spelled_differently_are_proved_equal` (task 7).
// pending: `test_different_strings_are_not_proved_equal` (task 7).
// pending: `test_bytes_are_compared_by_value` (task 7).
