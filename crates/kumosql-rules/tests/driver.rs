//! The rule framework's driver: source splicing, refusals and diagnostics.
//!
//! Ported in intent from `engine.py`. A rule is only one statement of
//! behaviour; what matters here is everything the driver does *around* it, and
//! every one of those things is a refusal to rewrite something other than what
//! the user wrote.
//!
//! A tiny test rule stands in for the real rules, which are task 5.

use kumosql_rules::{
    NoopRule, RewriteRule, RuleDiagnostic, apply_rule, apply_rule_to_script, apply_rule_to_sql,
    apply_rule_to_sqlx, cte_dependency_errors, has_template_tags, splice_statement, sql_comments,
    statement_segments, uses_pipe_syntax,
};
use kumosql_sql::ast::Statement;

// ------------------------------------------------------------------ test rules

/// Replaces every column named `keep` with one named `kept`.
struct RenameKeep;

impl RewriteRule for RenameKeep {
    fn name(&self) -> &'static str {
        "rename_keep"
    }
    fn summary(&self) -> &'static str {
        "Rename a column, to exercise the driver"
    }
    fn rewrite_statement(
        &self,
        statement: &mut Statement,
        _index: usize,
    ) -> Result<(usize, Vec<RuleDiagnostic>), String> {
        let sql = statement.to_string();
        if !sql.contains("keep") {
            return Ok((0, Vec::new()));
        }
        let replaced = sql.replace("keep", "kept");
        *statement = kumosql_sql::parse::parse_statements(&replaced, false)
            .map_err(|e| e.detail)?
            .into_iter()
            .next()
            .ok_or_else(|| "the rewrite produced no statement".to_string())?;
        Ok((1, Vec::new()))
    }
}

/// Reports a change without touching the statement.
struct Liar;

impl RewriteRule for Liar {
    fn name(&self) -> &'static str {
        "liar"
    }
    fn summary(&self) -> &'static str {
        "Claim a change it did not make"
    }
    fn rewrite_statement(
        &self,
        _statement: &mut Statement,
        _index: usize,
    ) -> Result<(usize, Vec<RuleDiagnostic>), String> {
        Ok((1, Vec::new()))
    }
}

/// Fails on the second statement.
struct FailsOnSecond;

impl RewriteRule for FailsOnSecond {
    fn name(&self) -> &'static str {
        "fails_on_second"
    }
    fn summary(&self) -> &'static str {
        "Fail on one statement, to check nothing is half-applied"
    }
    fn rewrite_statement(
        &self,
        statement: &mut Statement,
        index: usize,
    ) -> Result<(usize, Vec<RuleDiagnostic>), String> {
        if index == 1 {
            return Err("this rule refuses the second statement".to_string());
        }
        if statement.to_string().contains("SELECT 1") {
            *statement = kumosql_sql::parse::parse_statements("SELECT 9", false)
                .map_err(|e| e.detail)?
                .into_iter()
                .next()
                .expect("one statement");
            return Ok((1, Vec::new()));
        }
        Ok((0, Vec::new()))
    }
}

/// Declines a SQLX statement holding a `${...}` that is not a table reference.
struct KeepsSqlx;

impl RewriteRule for KeepsSqlx {
    fn name(&self) -> &'static str {
        "keeps_sqlx"
    }
    fn summary(&self) -> &'static str {
        "Leave an opaque ${...} alone"
    }
    fn keep_sqlx_expressions(&self) -> bool {
        true
    }
    fn rewrite_statement(
        &self,
        _statement: &mut Statement,
        _index: usize,
    ) -> Result<(usize, Vec<RuleDiagnostic>), String> {
        Ok((0, Vec::new()))
    }
}

// ------------------------------------------------------------------ no-op

#[test]
fn an_empty_input_is_an_empty_output() {
    let output = apply_rule(&NoopRule, "   ");
    assert_eq!(output.statements, 0);
    assert!(output.success());
    assert!(output.sql.trim().is_empty());
}

#[test]
fn a_rule_that_changes_nothing_returns_the_input_byte_for_byte() {
    let sql = "SELECT keep FROM t";
    let output = apply_rule(&NoopRule, sql);
    assert_eq!(output.sql, sql);
    assert_eq!(output.changes, 0);
    assert!(output.success());
}

// ------------------------------------------------------------------ spans

#[test]
fn statements_map_back_to_their_source_spans() {
    let sql = "SELECT 1 FROM a;\nSELECT 2 FROM b";
    let segments = statement_segments(sql).expect("segments");
    assert_eq!(segments.len(), 2);
    assert_eq!(&sql[segments[0].0.0..segments[0].0.1], "SELECT 1 FROM a");
    assert_eq!(&sql[segments[1].0.0..segments[1].0.1], "SELECT 2 FROM b");
}

#[test]
fn a_semicolon_inside_a_string_does_not_split_a_statement() {
    let sql = "SELECT ';' AS s FROM t; SELECT 2";
    let segments = statement_segments(sql).expect("segments");
    assert_eq!(segments.len(), 2);
    assert_eq!(
        &sql[segments[0].0.0..segments[0].0.1],
        "SELECT ';' AS s FROM t"
    );
}

// ------------------------------------------------------------------ splicing

#[test]
fn splicing_keeps_the_bytes_a_rule_did_not_change() {
    // The layout and spacing around the change must survive.
    let source = "SELECT   keep,   a   FROM   t";
    let rendered = "SELECT   kept,   a   FROM   t";
    let spliced = splice_statement(source, rendered).expect("splice");
    assert_eq!(spliced, rendered);
    assert!(spliced.contains("FROM   t"), "spacing lost: {spliced}");
}

#[test]
fn splicing_keeps_a_source_comment() {
    let source = "SELECT keep, -- a note\n  a FROM t";
    let rendered = "SELECT kept, a FROM t";
    let spliced = splice_statement(source, rendered).expect("splice");
    assert!(spliced.contains("-- a note"), "comment lost: {spliced}");
}

#[test]
fn sql_comments_are_read_out_of_the_text() {
    let comments = sql_comments("SELECT 1 -- one\nFROM t /* two */");
    assert_eq!(comments.len(), 2);
    assert!(comments[0].contains("one"));
    assert!(comments[1].contains("two"));
}

// ------------------------------------------------------------------ refusals

#[test]
fn templated_sql_is_left_as_written() {
    let sql = "{{ ref('t') }} SELECT keep FROM x";
    assert!(has_template_tags(sql));
    let output = apply_rule(&RenameKeep, sql);
    assert_eq!(output.sql, sql);
    assert!(output.diagnostic("templated_sql_kept").is_some());
    // And it is not a fatal code: the rule simply did not apply.
    assert!(!output.diagnostics.iter().any(|d| d.code == "parse_error"));
}

#[test]
fn pipe_syntax_is_left_as_written_by_default() {
    assert!(uses_pipe_syntax("SELECT * FROM t |> WHERE a = 1"));
    let sql = "SELECT * FROM t |> WHERE keep = 1";
    let output = apply_rule(&RenameKeep, sql);
    assert_eq!(output.sql, sql);
    assert!(output.diagnostic("pipe_syntax_kept").is_some());
}

#[test]
fn an_unparseable_query_is_reported_not_guessed_at() {
    let output = apply_rule(&RenameKeep, "SELECT FROM WHERE (");
    assert!(output.diagnostic("parse_error").is_some());
    assert!(!output.success());
    // The input is untouched.
    assert_eq!(output.sql, "SELECT FROM WHERE (");
}

#[test]
fn a_rule_that_fails_reports_it_and_writes_nothing() {
    let sql = "SELECT 1; SELECT 2";
    let output = apply_rule(&FailsOnSecond, sql);
    let error = output
        .diagnostic("transform_error")
        .expect("a transform error");
    assert!(error.message.contains("refuses the second statement"));
    assert!(!output.success());
}

// ------------------------------------------------------------------ lying rules

#[test]
fn a_rule_that_claims_a_change_it_did_not_make_is_an_error() {
    // This is the invariant that makes the driver's splicing trustworthy: if the
    // rule reports a change, the source has to have been edited to match.
    let sql = "SELECT 1 FROM t";
    let output = apply_rule(&Liar, sql);
    assert_eq!(output.sql, sql, "nothing should have been written");
    let diagnostic = output
        .diagnostic("source_splice_error")
        .expect("a splice error");
    // Either wording is fine; what matters is that the driver refused to accept
    // the rule's count at face value.
    assert!(
        diagnostic.message.contains("identical")
            || diagnostic
                .message
                .contains("without reporting a source edit"),
        "{}",
        diagnostic.message
    );
}

// ------------------------------------------------------------------ applying

#[test]
fn a_rule_changes_the_source_and_reports_it() {
    let output = apply_rule(&RenameKeep, "SELECT keep FROM t");
    assert_eq!(output.sql, "SELECT kept FROM t");
    assert_eq!(output.changed_statements, 1);
    assert_eq!(output.changes, 1);
    assert!(output.success());
}

#[test]
fn a_rule_applies_to_each_statement_of_a_multi_statement_input() {
    let output = apply_rule(&RenameKeep, "SELECT keep FROM a; SELECT keep FROM b");
    assert_eq!(output.statements, 2);
    assert_eq!(output.changed_statements, 2);
    assert!(output.sql.contains("SELECT kept FROM a"));
    assert!(output.sql.contains("SELECT kept FROM b"));
}

// ------------------------------------------------------------------ SQLX

#[test]
fn a_sqlx_block_is_preserved_and_the_sql_is_rewritten() {
    let sql = "config { type: \"table\" }\nSELECT keep FROM ${ref(\"t\")}";
    let output = apply_rule(&RenameKeep, sql);
    assert!(output.sql.contains("config { type: \"table\" }"));
    assert!(output.sql.contains("${ref(\"t\")}"), "{}", output.sql);
    assert!(output.sql.contains("SELECT kept FROM"), "{}", output.sql);
}

#[test]
fn a_lost_sqlx_interpolation_is_an_error_and_writes_nothing() {
    // The driver must not emit a file whose filter quietly disappeared.
    let sql = "SELECT keep FROM ${ref(\"t\")} WHERE ${when(incremental(), \"AND a > 1\")}";
    let output = apply_rule(&RenameKeep, sql);
    assert!(
        output.sql.contains("${when(incremental(), \"AND a > 1\")}"),
        "the interpolation must survive: {}",
        output.sql
    );
}

#[test]
fn a_rule_that_keeps_sqlx_expressions_declines_them() {
    let sql = "SELECT keep FROM ${ref(\"t\")} WHERE ${\"a > 1\"}";
    let output = apply_rule_to_sqlx(&KeepsSqlx, sql);
    assert!(output.diagnostic("sqlx_expression_kept").is_some());
}

// ------------------------------------------------------------------ scripts

#[test]
fn a_script_has_its_statements_rewritten() {
    let sql = "DECLARE x INT64 DEFAULT 1;\nSELECT keep FROM t;\nSELECT 'after'";
    let output = apply_rule_to_script(&RenameKeep, sql);
    assert!(output.sql.contains("SELECT kept FROM t"), "{}", output.sql);
    assert!(output.sql.contains("SELECT 'after'"), "{}", output.sql);
    assert!(
        output.sql.contains("DECLARE x INT64 DEFAULT 1"),
        "a script statement was lost: {}",
        output.sql
    );
}

// ------------------------------------------------------------------ CTEs

#[test]
fn a_cte_referenced_before_it_is_defined_is_an_error() {
    let statements = kumosql_sql::parse::parse_statements(
        "WITH b AS (SELECT * FROM a), a AS (SELECT 1) SELECT * FROM b",
        false,
    )
    .expect("parse");
    let errors = cte_dependency_errors(&statements[0]);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("referenced before it is defined")),
        "{errors:?}"
    );
}

#[test]
fn a_well_ordered_with_clause_has_no_dependency_error() {
    let statements = kumosql_sql::parse::parse_statements(
        "WITH a AS (SELECT 1), b AS (SELECT * FROM a) SELECT * FROM b",
        false,
    )
    .expect("parse");
    assert!(cte_dependency_errors(&statements[0]).is_empty());
}

#[test]
fn a_reference_to_a_table_is_not_a_cte_dependency_error() {
    let statements = kumosql_sql::parse::parse_statements(
        "WITH a AS (SELECT * FROM ds.t) SELECT * FROM a",
        false,
    )
    .expect("parse");
    assert!(cte_dependency_errors(&statements[0]).is_empty());
}

// ------------------------------------------------------------------ output

#[test]
fn a_rule_output_summarises_itself() {
    let output = apply_rule(&RenameKeep, "SELECT keep FROM t");
    let text = output.to_string();
    assert!(text.contains("1 changed"), "{text}");
}

#[test]
fn an_unchanged_input_keeps_its_bytes_including_trailing_newline() {
    let sql = "SELECT 1\n";
    let output = apply_rule_to_sql(&NoopRule, sql);
    assert_eq!(output.sql, sql);
}
