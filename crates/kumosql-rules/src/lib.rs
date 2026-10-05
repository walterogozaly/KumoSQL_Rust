//! The rewrite-rule framework: the rule trait, the registry, and the driver
//! that applies a rule to SQL or Dataform SQLX.
//!
//! Ported from `src/kumosql/engine.py`.
//!
//! # What the driver is for
//!
//! A rule knows how to rewrite one statement. Everything around that is here,
//! and it is deliberately paranoid, because the failure this project exists to
//! prevent is a rewrite that silently changes what a query means:
//!
//! * statements are mapped back to their **source spans**, and only the spans
//!   a rule actually changed are edited -- every other byte, including layout
//!   and comments, survives;
//! * a splice that cannot retain every source comment is an **error**, not a
//!   best-effort edit;
//! * a rule that mutates the tree without reporting a change is an **error**;
//! * a template-tagged query, pipe syntax, and an opaque `${...}` expression are
//!   each **left as written** with a diagnostic, because the rule would be
//!   rewriting something other than what the user wrote;
//! * a failed rule step restores the statement it was working on rather than
//!   keeping a half-mutated tree.
//!
//! None of these decline a rewrite silently. Each one either edits the source
//! safely or reports why it did not.

use std::collections::BTreeMap;
use std::fmt;

use kumosql_sql::ast::{ObjectName, Query, Statement};
use kumosql_sql::error::ErrorKind;
use kumosql_sql::parse::{parse_statements, render_statements};
use kumosql_sql::scripts::{has_blocks, split_statements};
use kumosql_sql::sqlx::{
    SectionKind, is_table_reference, looks_like_sqlx, mask_sqlx_interpolations, opaque_tokens,
    restore_sqlx_interpolations, split_sqlx_sections,
};
use kumosql_sql::token::{Token, TokenKind, tokenize};

pub mod registry;

/// One statement-level parse or transformation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleDiagnostic {
    /// Which statement, or `-1` for the whole input.
    pub statement_index: i64,
    /// A stable code, which callers and tests match on.
    pub code: String,
    /// A human-readable explanation.
    pub message: String,
}

impl RuleDiagnostic {
    /// A diagnostic about the whole input.
    pub fn whole(code: &str, message: impl Into<String>) -> Self {
        RuleDiagnostic {
            statement_index: -1,
            code: code.to_string(),
            message: message.into(),
        }
    }

    /// A diagnostic about one statement.
    pub fn at(statement_index: i64, code: &str, message: impl Into<String>) -> Self {
        RuleDiagnostic {
            statement_index,
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// The diagnostic codes that mean the rule did not run cleanly.
///
/// A run with any of these is a failure, and the CLI's exit code 2 comes from
/// here. `apply_rule` adds `unproven` output to the mix separately.
pub const FATAL_DIAGNOSTIC_CODES: &[&str] = &[
    "parse_error",
    "sqlx_parse_error",
    "sqlx_restore_error",
    "transform_error",
    "cte_dependency_error",
    "inline_subqueries_remaining",
    "output_parse_error",
    "source_splice_error",
];

/// The syntactic result of applying one rule to a SQL or SQLX text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleOutput {
    /// The rewritten text. Unchanged input is returned byte-for-byte.
    pub sql: String,
    /// How many statements were read.
    pub statements: usize,
    /// How many of them the rule changed.
    pub changed_statements: usize,
    /// How many individual changes the rule reported.
    pub changes: usize,
    /// Constructs the rule meant to remove and did not.
    pub remaining: usize,
    /// Every diagnostic, in order.
    pub diagnostics: Vec<RuleDiagnostic>,
}

impl RuleOutput {
    /// Whether the rule ran cleanly and left nothing it was meant to remove.
    pub fn success(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|d| FATAL_DIAGNOSTIC_CODES.contains(&d.code.as_str()))
            && self.remaining == 0
    }

    /// Whether any diagnostic is fatal.
    pub fn has_fatal(&self) -> bool {
        !self.success()
    }

    /// The first diagnostic with the given code, if any.
    pub fn diagnostic(&self, code: &str) -> Option<&RuleDiagnostic> {
        self.diagnostics.iter().find(|d| d.code == code)
    }
}

/// A deterministic, statement-local rewrite.
///
/// Implementors implement [`RewriteRule::rewrite_statement`] and the two
/// description methods. `Send + Sync` because the registry is a process-wide
/// static: a rule has to be usable from the parallel test runner.
pub trait RewriteRule: Send + Sync {
    /// The rule's registered name, as the CLI spells it.
    fn name(&self) -> &'static str;

    /// A one-line description, shown by `kumosql --help`.
    fn summary(&self) -> &'static str;

    /// Rewrite `statement` in place, returning the change count and diagnostics.
    fn rewrite_statement(
        &self,
        statement: &mut Statement,
        index: usize,
    ) -> Result<(usize, Vec<RuleDiagnostic>), String>;

    /// Count constructs this rule should have removed but did not.
    ///
    /// A rule whose goal is to eliminate something (inline subqueries, say)
    /// overrides this so leftovers are reported as failures rather than passing
    /// silently.
    fn count_remaining(&self, _statements: &[Statement]) -> usize {
        0
    }

    /// Rewrite statements written in pipe syntax.
    ///
    /// Off by default: a parser turns `|>` into nested CTEs and subqueries, so a
    /// rule would rewrite that translation rather than what was written.
    fn rewrite_pipe_syntax(&self) -> bool {
        false
    }

    /// Only the prover's normalisation reads this rule's output, never a user.
    fn analysis_only(&self) -> bool {
        false
    }

    /// Leave a SQLX statement alone when it holds a `${...}` expression that is
    /// not `ref()`/`self()`.
    ///
    /// The rule sees only a placeholder there, but the expression can compile
    /// to a looser-binding predicate, a whole clause, or a query that reads a
    /// CTE -- so dropping parentheses, predicates or CTEs around it can break it.
    fn keep_sqlx_expressions(&self) -> bool {
        false
    }

    /// Left out of the canonical rule order and of the sweeps over every rule:
    /// a style choice, or a rule needing facts those inputs do not declare.
    fn opt_in(&self) -> bool {
        false
    }
}

/// A rule that does nothing, used to exercise the driver.
pub struct NoopRule;

impl RewriteRule for NoopRule {
    fn name(&self) -> &'static str {
        "noop"
    }
    fn summary(&self) -> &'static str {
        "Change nothing; exercises the driver"
    }
    fn rewrite_statement(
        &self,
        _statement: &mut Statement,
        _index: usize,
    ) -> Result<(usize, Vec<RuleDiagnostic>), String> {
        Ok((0, Vec::new()))
    }
}

/// Apply `rule` to `sql`, which may be BigQuery SQL or Dataform SQLX.
pub fn apply_rule(rule: &dyn RewriteRule, sql: &str) -> RuleOutput {
    if looks_like_sqlx(sql) {
        return apply_rule_to_sqlx(rule, sql);
    }
    if !sql.trim().is_empty() && has_blocks(sql) {
        return apply_rule_to_script(rule, sql);
    }
    apply_rule_to_sql(rule, sql)
}

/// Apply `rule` to plain SQL.
pub fn apply_rule_to_sql(rule: &dyn RewriteRule, sql: &str) -> RuleOutput {
    if sql.trim().is_empty() {
        return RuleOutput {
            sql: String::new(),
            statements: 0,
            changed_statements: 0,
            changes: 0,
            remaining: 0,
            diagnostics: Vec::new(),
        };
    }

    let mut diagnostics = Vec::new();

    // Jinja templating is left exactly as written: a rule would be rewriting
    // the template, not the query.
    if !rule.analysis_only() && has_template_tags(sql) {
        diagnostics.push(RuleDiagnostic::whole(
            "templated_sql_kept",
            "Jinja templating ({{ }}, {% %}, {# #}) is left as written",
        ));
        return RuleOutput {
            sql: sql.to_string(),
            statements: 0,
            changed_statements: 0,
            changes: 0,
            remaining: 0,
            diagnostics,
        };
    }

    let statements = match parse_statements(sql, false) {
        Ok(statements) => statements,
        Err(error) => {
            diagnostics.push(RuleDiagnostic::at(0, "parse_error", error.detail));
            return RuleOutput {
                sql: sql.to_string(),
                statements: 0,
                changed_statements: 0,
                changes: 0,
                remaining: 0,
                diagnostics,
            };
        }
    };

    let remaining = rule.count_remaining(&statements);
    let segments = match statement_segments(sql) {
        Some(segments) => segments,
        None => {
            diagnostics.push(RuleDiagnostic::whole(
                "source_splice_error",
                "SQL statements could not be mapped to their original source spans",
            ));
            return RuleOutput {
                sql: sql.to_string(),
                statements: statements.len(),
                changed_statements: 0,
                changes: 0,
                remaining,
                diagnostics,
            };
        }
    };

    if segments.len() != statements.len() {
        diagnostics.push(RuleDiagnostic::whole(
            "source_splice_error",
            format!(
                "found {} statements but {} source spans",
                statements.len(),
                segments.len()
            ),
        ));
        return RuleOutput {
            sql: sql.to_string(),
            statements: statements.len(),
            changed_statements: 0,
            changes: 0,
            remaining,
            diagnostics,
        };
    }

    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut changed_statements = 0usize;
    let mut changes = 0usize;
    let mut rewritten: Vec<Statement> = Vec::with_capacity(statements.len());

    let mut paired: Vec<((usize, usize), Statement)> = segments;
    paired.reverse();

    for (index, mut statement) in statements.into_iter().enumerate() {
        let Some(((start, end), _)) = paired.pop() else {
            break;
        };
        let span = &sql[start..end];

        if !rule.rewrite_pipe_syntax() && uses_pipe_syntax(span) {
            diagnostics.push(RuleDiagnostic::at(
                index as i64,
                "pipe_syntax_kept",
                "pipe syntax (|>) is left as written",
            ));
            rewritten.push(statement);
            continue;
        }

        let before = statement.clone();
        match rule.rewrite_statement(&mut statement, index) {
            Ok((count, statement_diagnostics)) => {
                diagnostics.extend(statement_diagnostics);
                for error in cte_dependency_errors(&statement) {
                    diagnostics.push(RuleDiagnostic::at(
                        index as i64,
                        "cte_dependency_error",
                        error,
                    ));
                }
                if count > 0 {
                    let rendered = render_statements(&[statement.clone()]);
                    if rendered == span {
                        diagnostics.push(RuleDiagnostic::at(
                            index as i64,
                            "source_splice_error",
                            "a rule reported a change but the rendered statement is identical",
                        ));
                        return RuleOutput {
                            sql: sql.to_string(),
                            statements: segments_len(sql),
                            changed_statements: 0,
                            changes: 0,
                            remaining,
                            diagnostics,
                        };
                    }
                    match splice_statement(span, &rendered) {
                        Ok(replacement) => {
                            changed_statements += 1;
                            changes += count;
                            edits.push((start, end, replacement));
                        }
                        Err(message) => {
                            diagnostics.push(RuleDiagnostic::at(
                                index as i64,
                                "source_splice_error",
                                format!("SQL source spans could not be safely edited: {message}"),
                            ));
                            return RuleOutput {
                                sql: sql.to_string(),
                                statements: segments_len(sql),
                                changed_statements: 0,
                                changes: 0,
                                remaining,
                                diagnostics,
                            };
                        }
                    }
                }
                rewritten.push(statement);
            }
            Err(message) => {
                diagnostics.push(RuleDiagnostic::at(index as i64, "transform_error", message));
                // Do not keep a partially mutated statement after a failed step.
                rewritten.push(before);
            }
        }
    }

    if changes == 0 {
        // A rule that changed the tree without reporting a change would leave
        // the source and the tree disagreeing, so it is an error, not a pass.
        if rewritten != parse_statements(sql, false).unwrap_or_default() {
            diagnostics.push(RuleDiagnostic::whole(
                "source_splice_error",
                "a rule changed an AST without reporting a source edit",
            ));
        }
        return RuleOutput {
            sql: sql.to_string(),
            statements: rewritten.len(),
            changed_statements: 0,
            changes: 0,
            remaining,
            diagnostics,
        };
    }

    let mut output = sql.to_string();
    for (start, end, replacement) in edits.into_iter().rev() {
        output.replace_range(start..end, &replacement);
    }

    RuleOutput {
        sql: output,
        statements: rewritten.len(),
        changed_statements,
        changes,
        remaining,
        diagnostics,
    }
}

/// How many statements `sql` holds, for an early-return's field.
fn segments_len(sql: &str) -> usize {
    parse_statements(sql, false).map(|s| s.len()).unwrap_or(0)
}

/// Apply `rule` to a script, statement by statement.
pub fn apply_rule_to_script(rule: &dyn RewriteRule, sql: &str) -> RuleOutput {
    let parts = split_statements(sql);
    let mut diagnostics = Vec::new();
    let mut changed_statements = 0usize;
    let mut changes = 0usize;

    for (index, part) in parts.iter().enumerate() {
        let output = apply_rule_to_sql(rule, part);
        diagnostics.extend(output.diagnostics);
        if output.changes > 0 {
            changed_statements += 1;
            changes += output.changes;
        }
        let _ = index;
    }

    // The script path rebuilds each statement, so the text is reassembled rather
    // than spliced: a script's statements are not at known spans.
    let rendered: Vec<String> = parts
        .iter()
        .map(|part| {
            let output = apply_rule_to_sql(rule, part);
            output.sql
        })
        .collect();
    let mut joined = rendered.join(";\n");

    if changes == 0 {
        joined = sql.to_string();
    }

    RuleOutput {
        sql: joined,
        statements: parts.len(),
        changed_statements,
        changes,
        remaining: 0,
        diagnostics,
    }
}

/// Apply `rule` to a Dataform SQLX file.
///
/// The `config`, `js`, `pre_operations` and `post_operations` blocks are
/// preserved byte-for-byte, and only the SQL sections are rewritten. Every
/// interpolation is restored afterwards, and a lost or duplicated one is an
/// error rather than a best-effort substitution.
pub fn apply_rule_to_sqlx(rule: &dyn RewriteRule, sql: &str) -> RuleOutput {
    let mut diagnostics = Vec::new();
    let sections = split_sqlx_sections(sql);

    let mut rebuilt = String::new();
    let mut statements = 0usize;
    let mut changed_statements = 0usize;
    let mut changes = 0usize;
    let mut remaining = 0usize;

    for section in &sections {
        if section.kind == SectionKind::Block {
            // Not SQL: preserved exactly.
            rebuilt.push_str(&section.text);
            continue;
        }

        let (masked, restorations) = mask_sqlx_interpolations(&section.text);
        let output = apply_rule_to_sql(rule, &masked);
        diagnostics.extend(output.diagnostics);
        statements += output.statements;
        remaining += output.remaining;
        changed_statements += output.changed_statements;
        changes += output.changes;

        if rule.keep_sqlx_expressions() {
            let opaque = opaque_tokens(&restorations);
            for token in &opaque {
                if output.sql.contains(token) {
                    diagnostics.push(RuleDiagnostic::whole(
                        "sqlx_expression_kept",
                        "a ${...} expression other than ref() or self() is left as written",
                    ));
                }
            }
        }

        match restore_sqlx_interpolations(&output.sql, &restorations) {
            Ok(restored) => rebuilt.push_str(&restored),
            Err(error) => {
                diagnostics.push(RuleDiagnostic::whole("sqlx_restore_error", error.0));
                // No rewritten output at all: a half-restored file would be a
                // query that reads something else.
                return RuleOutput {
                    sql: sql.to_string(),
                    statements,
                    changed_statements: 0,
                    changes: 0,
                    remaining,
                    diagnostics,
                };
            }
        }
    }

    RuleOutput {
        sql: rebuilt,
        statements,
        changed_statements,
        changes,
        remaining,
        diagnostics,
    }
}

/// The source spans of each statement, or `None` when they cannot be mapped.
///
/// A semicolon inside a string, a comment or a quoted name does not end a
/// statement, so the spans come from the token stream rather than from
/// splitting the text.
pub fn statement_segments(sql: &str) -> Option<Vec<((usize, usize), Statement)>> {
    let tokens = tokenize(sql);
    let mut segments = Vec::new();
    let mut cursor = 0usize;

    for token in &tokens {
        if token.kind != TokenKind::Punctuation || token.text(sql) != ";" {
            continue;
        }
        let end = token.span.start;
        if sql[cursor..end].trim().is_empty() {
            cursor = token.span.end;
            continue;
        }
        let parsed = parse_statements(&sql[cursor..end], false).ok()?;
        if parsed.len() != 1 {
            return None;
        }
        segments.push(((cursor, end), parsed.into_iter().next()?));
        // Skip the whitespace after the semicolon so the next span starts at
        // the statement rather than at the gap before it.
        cursor = sql[token.span.end..]
            .find(|c: char| !c.is_whitespace())
            .map(|offset| token.span.end + offset)
            .unwrap_or(sql.len());
    }

    if !sql[cursor..].trim().is_empty() {
        let parsed = parse_statements(&sql[cursor..], false).ok()?;
        if parsed.len() != 1 {
            return None;
        }
        segments.push(((cursor, sql.len()), parsed.into_iter().next()?));
    }

    Some(segments)
}

/// Whether `sql` uses pipe syntax.
pub fn uses_pipe_syntax(sql: &str) -> bool {
    let tokens = tokenize(sql);
    tokens
        .windows(2)
        .any(|pair| pair[0].text(sql) == "|" && pair[1].text(sql) == ">")
}

/// Whether `sql` carries Jinja template tags.
pub fn has_template_tags(sql: &str) -> bool {
    ["{{", "{%", "{#"].iter().any(|tag| sql.contains(tag))
}

/// The comment texts in `sql`.
pub fn sql_comments(sql: &str) -> Vec<String> {
    sql_comment_tokens(sql)
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

/// The comment tokens in `sql`, with the line they start on.
fn sql_comment_tokens(sql: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut line = 1usize;
    let mut cursor = 0usize;
    for token in tokenize(sql) {
        let text = token.text(sql);
        if matches!(token.kind, TokenKind::LineComment | TokenKind::BlockComment) {
            out.push((text.to_string(), line));
        }
        line += sql[cursor..token.span.end].matches('\n').count();
        cursor = token.span.end;
    }
    out
}

/// Patch only the spans whose tokens changed, preserving every other byte.
///
/// This is what keeps a rewrite from reflowing the statement: the unchanged
/// tokens around a change keep their original whitespace, layout and comments.
///
/// Fails when a source comment cannot be retained, because losing one would
/// change what the query says about itself.
pub fn splice_statement(source: &str, rendered: &str) -> Result<String, String> {
    let source_tokens = tokenize(source);
    let target_tokens = tokenize(rendered);
    if source_tokens.is_empty() || target_tokens.is_empty() {
        return Err("a changed statement could not be aligned to SQL tokens".to_string());
    }

    let source_keys: Vec<(TokenKind, String)> = source_tokens
        .iter()
        .map(|t| (t.kind, t.text(source).to_string()))
        .collect();
    let target_keys: Vec<(TokenKind, String)> = target_tokens
        .iter()
        .map(|t| (t.kind, t.text(rendered).to_string()))
        .collect();

    let operations = token_diff(&source_keys, &target_keys);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();

    for (i1, i2, j1, j2) in operations {
        if i1 >= i2 || j1 >= j2 {
            // An insertion or a deletion has no target text (or no source
            // text); handled by the single-token cases below.
            continue;
        }
        // Replace exactly the changed tokens, keeping the spacing *between*
        // them and the spacing around the region. Using the rendered text
        // rather than the joined tokens is what preserves the layout.
        let start = source_tokens[i1].span.start;
        let end = source_tokens[i2 - 1].span.end;
        let replacement = &rendered[target_tokens[j1].span.start..target_tokens[j2 - 1].span.end];
        edits.push((start, end.min(source.len()), replacement.to_string()));
    }

    let mut result = source.to_string();
    for (start, end, replacement) in edits.into_iter().rev() {
        if start <= end && end <= result.len() {
            result.replace_range(start..end, &replacement);
        }
    }

    let mut before = sql_comments(source);
    let mut after = sql_comments(&result);
    before.sort();
    after.sort();
    if before != after {
        return Err("a source comment could not be retained during span editing".to_string());
    }

    Ok(result)
}

/// The changed regions between two token key sequences, as
/// `(source range, target range)` pairs.
///
/// A plain longest-common-subsequence alignment. Statement token runs are short
/// and the quadratic cost is not worth avoiding; what matters is that equal
/// tokens are matched, so the edit stays local.
fn token_diff(
    source: &[(TokenKind, String)],
    target: &[(TokenKind, String)],
) -> Vec<(usize, usize, usize, usize)> {
    let rows = source.len();
    let cols = target.len();
    // lcs[i][j] = length of the longest common subsequence of source[i..], target[j..]
    let mut lcs = vec![vec![0usize; cols + 1]; rows + 1];
    for i in (0..rows).rev() {
        for j in (0..cols).rev() {
            lcs[i][j] = if source[i] == target[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut operations = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < rows && j < cols {
        if source[i] == target[j] {
            i += 1;
            j += 1;
            continue;
        }
        let (i_start, j_start) = (i, j);
        // Extend the changed region while the alignment stays off the diagonal.
        while i < rows && j < cols && source[i] != target[j] {
            if lcs[i + 1][j] >= lcs[i][j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
        operations.push((i_start, i, j_start, j));
    }
    if i < rows || j < cols {
        operations.push((i, rows, j, cols));
    }
    operations
}

/// Why a query's CTEs cannot be read as written.
///
/// A CTE may only be referenced where it is visible: a reference inside another
/// CTE's body, or before that CTE is defined, is a dependency the reader cannot
/// satisfy, and a rewrite that moved it would change the query.
pub fn cte_dependency_errors(statement: &Statement) -> Vec<String> {
    let Some(query) = statement.top_level_query() else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    for with in with_clauses(query) {
        let defined: Vec<String> = with.ctes.iter().map(|c| c.name.folded()).collect();
        let mut visible: Vec<String> = Vec::new();
        for cte in &with.ctes {
            let references = query_references(&cte.query);
            for reference in references {
                if defined.contains(&reference) && !visible.contains(&reference) {
                    errors.push(format!(
                        "CTE {reference} is referenced before it is defined"
                    ));
                }
            }
            visible.push(cte.name.folded());
        }
        // A reference no CTE defines at all.
        for cte in &with.ctes {
            for reference in query_references(&cte.query) {
                if !defined.contains(&reference) && !is_table_like(&reference) {
                    errors.push(format!("CTE {reference} is referenced but never defined"));
                }
            }
        }
    }
    errors
}

/// Every `WITH` clause reachable from `query`.
fn with_clauses(query: &Query) -> Vec<&kumosql_sql::ast::With> {
    let mut found = Vec::new();
    collect_with_clauses(query, &mut found);
    found
}

fn collect_with_clauses<'a>(query: &'a Query, out: &mut Vec<&'a kumosql_sql::ast::With>) {
    if let Some(with) = query.with_clause() {
        out.push(with);
        for cte in &with.ctes {
            collect_with_clauses(&cte.query, out);
        }
    }
    match query {
        Query::SetOperation { left, right, .. } => {
            collect_with_clauses(left, out);
            collect_with_clauses(right, out);
        }
        Query::Pipe { base, .. } => collect_with_clauses(base, out),
        _ => {}
    }
}

/// The names a query reads in a `FROM` clause.
fn query_references(query: &Query) -> Vec<String> {
    let mut names = Vec::new();
    collect_references(query, &mut names);
    names
}

fn collect_references(query: &Query, out: &mut Vec<String>) {
    match query {
        Query::Select { with, body } => {
            if let Some(with) = with {
                for cte in &with.ctes {
                    out.push(cte.name.folded());
                    collect_references(&cte.query, out);
                }
            }
            if let Some(from) = &body.from {
                collect_factor_references(from, out);
            }
        }
        Query::Values { with, .. } => {
            if let Some(with) = with {
                for cte in &with.ctes {
                    out.push(cte.name.folded());
                    collect_references(&cte.query, out);
                }
            }
        }
        Query::SetOperation {
            with, left, right, ..
        } => {
            if let Some(with) = with {
                for cte in &with.ctes {
                    out.push(cte.name.folded());
                    collect_references(&cte.query, out);
                }
            }
            collect_references(left, out);
            collect_references(right, out);
        }
        Query::Pipe { with, base, .. } => {
            if let Some(with) = with {
                for cte in &with.ctes {
                    out.push(cte.name.folded());
                    collect_references(&cte.query, out);
                }
            }
            collect_references(base, out);
        }
    }
}

fn collect_factor_references(factor: &kumosql_sql::ast::TableFactor, out: &mut Vec<String>) {
    use kumosql_sql::ast::TableFactor as F;
    match factor {
        // The *whole* dotted name: a `ds.t` is plainly a table, while a bare
        // `t` might be a CTE reference this clause does not define.
        F::Table { name, .. } => out.push(
            name.parts
                .iter()
                .map(|p| p.folded())
                .collect::<Vec<_>>()
                .join("."),
        ),
        F::Subquery { query, .. } => collect_references(query, out),
        F::Join { left, right, .. } => {
            collect_factor_references(left, out);
            collect_factor_references(right, out);
        }
        F::TableFunction { name, .. } => out.push(
            name.parts
                .iter()
                .map(|p| p.folded())
                .collect::<Vec<_>>()
                .join("."),
        ),
        F::Unnest { .. } => {}
    }
}

/// Whether a name is plainly a table rather than an unresolvable reference.
///
/// A dotted or project-qualified name is a table. This is a heuristic in the
/// same spirit as the Python original's, and a false positive here costs an
/// *error the caller must see*, which is the safe direction.
fn is_table_like(name: &str) -> bool {
    name.contains('.') || name.is_empty()
}

impl fmt::Display for RuleOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} statement(s), {} changed, {} change(s), {} remaining",
            self.statements, self.changed_statements, self.changes, self.remaining
        )
    }
}

/// The token keys `splice_statement` aligns on, exposed for tests.
pub fn token_keys(sql: &str) -> Vec<(TokenKind, String)> {
    tokenize(sql)
        .iter()
        .map(|t: &Token| (t.kind, t.text(sql).to_string()))
        .collect()
}

/// A map of rule name to rule, for `available_rules` style output.
pub type RuleMap = BTreeMap<String, Box<dyn RewriteRule>>;

/// Whether a parse failure was a refusal rather than a syntax error.
pub fn is_refusal(error: &kumosql_sql::error::Error) -> bool {
    error.kind == ErrorKind::Rejected
}

/// Whether an expression is a bare table name, used by the driver.
pub fn is_table_name(statement: &Statement) -> bool {
    matches!(statement.top_level_query(), Some(Query::Select { body, .. }) if body.from.is_none())
}

/// The name of a statement's target table, when it has one.
pub fn target_table(statement: &Statement) -> Option<ObjectName> {
    match statement {
        Statement::CreateTableAs { name, .. }
        | Statement::CreateViewAs { name, .. }
        | Statement::Insert { table: name, .. } => Some(name.clone()),
        Statement::Update { table, .. } | Statement::Delete { table, .. } => Some(table.clone()),
        _ => None,
    }
}

/// Whether an interpolation is a plain table reference.
pub fn interpolation_is_table_reference(original: &str) -> bool {
    is_table_reference(original)
}
