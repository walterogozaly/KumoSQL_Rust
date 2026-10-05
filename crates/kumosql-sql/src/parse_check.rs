//! Checking that the parse accounts for the whole query.
//!
//! Ported in intent from `parse_check.py`, which compares `sqlglot`'s reading
//! of a query with an independent dialect parser. That module is ~2,000 lines
//! with a hand-written precedence parser for three dialects; this port takes
//! the property it exists to protect and checks it against our own tree.
//!
//! # The property
//!
//! Two failures, both of which produce a query that *looks* fine:
//!
//! 1. **A dropped token.** The parser read part of the query and discarded the
//!    rest without complaining. This is not hypothetical: `sqlparser` 0.59
//!    reads `FROM ds.fn(arg, ...)` as a bare table named `ds.fn` and drops the
//!    call's arguments with no error at all.
//! 2. **An invented name.** The tree holds a name or literal that the source
//!    does not contain.
//!
//! A parse that only fails loudly cannot cause either. A parse that succeeds
//! can, and everything downstream -- rules, provers, results -- would then work
//! on a query that was never written.
//!
//! # How it is checked
//!
//! The *atoms* of the query are collected from the source tokens (identifiers,
//! numbers, strings) and from the parsed tree (every name, literal, function
//! name and type name the tree holds). The two multisets must be equal.
//!
//! The comparison runs on the **rewritten** text -- the text the parser
//! actually saw -- because a marker or a canonicalised literal in one side and
//! not the other is a false disagreement, not a real one. The Python original
//! takes the opposite choice and declines such queries as unchecked; running on
//! the rewritten text is the more useful of the two, and the caveat is stated
//! in the returned [`ParseCheck::note`].

use std::collections::BTreeMap;

use crate::ast::*;
use crate::error::{Error, ErrorKind};
use crate::literals::canonical_literals;
use crate::parse::parse_statements;
use crate::rewrite::rewrite_all;
use crate::token::{TokenKind, tokenize};

/// The outcome of checking a parse against its source.
///
/// Mirrors `ParseCheck` in the Python original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckStatus {
    /// Every token in the source is accounted for, and the tree invents nothing.
    Agree,
    /// The two readings differ. `reasons` says how.
    Disagree,
    /// The query was not checked, and `note` says why.
    ///
    /// Separate from `Disagree` on purpose: "we could not check this" is a
    /// different claim from "these disagree", and a caller must be able to tell
    /// them apart.
    Unchecked,
}

/// The outcome of checking one query's parse against its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseCheck {
    /// The dialect checked, as the caller named it.
    pub dialect: String,
    /// Whether the two readings agree.
    pub status: CheckStatus,
    /// Why they disagree, one entry per difference found.
    pub reasons: Vec<String>,
    /// How many atoms were compared.
    pub compared: usize,
    /// Why the query was left unchecked, when it was.
    pub note: String,
}

impl ParseCheck {
    /// Whether the check found a disagreement.
    ///
    /// An unchecked query is *not* a disagreement: it is a query nobody looked
    /// at, and treating that as agreement is exactly the mistake this project
    /// exists to avoid.
    pub fn disagrees(&self) -> bool {
        self.status == CheckStatus::Disagree
    }

    /// Whether the query was actually checked.
    pub fn is_checked(&self) -> bool {
        matches!(self.status, CheckStatus::Agree | CheckStatus::Disagree)
    }
}

/// Reserved words that are structure rather than a name.
///
/// A word on this list is not an atom: `SELECT` appears in the source and in no
/// tree, and treating it as an atom would make every query disagree.
const RESERVED: &[&str] = &[
    "ALL",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BETWEEN",
    "BY",
    "CASE",
    "CAST",
    "CROSS",
    "DESC",
    "DISTINCT",
    "ELSE",
    "END",
    "EXCEPT",
    "FALSE",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "IF",
    "ILIKE",
    "IN",
    "INNER",
    "INTERSECT",
    "IS",
    "JOIN",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "RIGHT",
    "SAFE_CAST",
    "SELECT",
    "SET",
    "SOME",
    "STRUCT",
    "THEN",
    "TRUE",
    "UNION",
    "USING",
    "VALUES",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
    "QUALIFY",
    "ARRAY",
    "OVER",
    "PARTITION",
    "PRECEDING",
    "FOLLOWING",
    "UNBOUNDED",
    "CURRENT",
    "ROW",
    "ROWS",
    "RANGE",
    "AND",
    "EXCEPT",
    // `FILTER` is standard SQL, introduced by the aggregate-filter rewrite when
    // the source did not spell it. `UNNEST` is the wrapper the `LIKE ALL`
    // rewrite introduces and then unwraps during conversion.
    "FILTER",
    "UNNEST",
];

/// The prefix every marker this port parks text under carries.
///
/// A marker exists only between the rewrite and the conversion, so it is never
/// an atom of the query the user wrote.
const MARKER_PREFIX: &str = "__KUMO_";

/// The quote characters a literal may be spelled with.
const QUOTES: [char; 2] = ['\'', '"'];

/// The text inside a quoted literal, without quotes or a `r`/`b` prefix.
fn inner_literal_text(text: &str) -> String {
    let body = text
        .trim_start_matches(['r', 'b'])
        .trim_start_matches(QUOTES)
        .trim_end_matches(QUOTES);
    body.to_string()
}

/// Whether a word is SQL structure rather than a name.
fn is_reserved(word: &str) -> bool {
    RESERVED.iter().any(|r| word.eq_ignore_ascii_case(r))
}

/// Check that `sql` parses, and that the parse accounts for the whole query.
///
/// `dialect` is recorded and reported; the check itself is BigQuery-specific
/// because the atom comparison uses BigQuery's reading of identifiers and
/// literals.
pub fn check_query(sql: &str, dialect: &str) -> ParseCheck {
    let mut check = ParseCheck {
        dialect: dialect.to_string(),
        status: CheckStatus::Unchecked,
        reasons: Vec::new(),
        compared: 0,
        note: String::new(),
    };

    // The check runs on the text the parser saw, so a marker or a canonicalised
    // literal cannot look like a disagreement.
    let canonical = canonical_literals(sql);
    let rewritten = match rewrite_all(&canonical) {
        Ok(text) => text,
        Err(Error { kind, detail, .. }) => {
            // A refusal is a decision, not a disagreement.
            check.note = format!("not checked: {kind}: {detail}");
            return check;
        }
    };

    let statements = match parse_statements(&rewritten, false) {
        Ok(statements) => statements,
        Err(error) => {
            check.note = format!("the base parser does not read it: {}", error.detail);
            return check;
        }
    };

    let source = atoms_in_source(&rewritten);
    let tree = atoms_in_statements(&statements);

    let mut reasons = Vec::new();
    let mut compared = 0usize;

    // Source atoms the tree has nothing for: a dropped token.
    for (atom, count) in &source {
        let found = tree.get(atom).copied().unwrap_or(0);
        compared += count.max(&found);
        if found < *count {
            reasons.push(format!(
                "the source has {atom} but the tree accounts for it {} time(s), not {count}",
                found
            ));
        }
    }
    // Names the tree holds that the source does not: an invented name.
    for (atom, count) in &tree {
        let found = source.get(atom).copied().unwrap_or(0);
        if found < *count {
            reasons.push(format!(
                "the tree holds {atom}, which the source does not spell {count} time(s)"
            ));
        }
    }

    check.compared = compared;
    check.note = format!(
        "compared on the rewritten text ({}); atoms not canonicalised identically \
         are compared as the parser saw them",
        if rewritten == sql {
            "unchanged"
        } else {
            "changed"
        }
    );

    if reasons.is_empty() {
        check.status = CheckStatus::Agree;
    } else {
        check.status = CheckStatus::Disagree;
        check.reasons = reasons;
    }
    check
}

/// The atoms the source text contains: identifiers, numbers and strings.
///
/// Structure keywords are excluded, since no tree holds them.
fn atoms_in_source(sql: &str) -> BTreeMap<String, usize> {
    let mut atoms: BTreeMap<String, usize> = BTreeMap::new();
    for token in tokenize(sql) {
        let text = token.text(sql);
        let atom = match token.kind {
            TokenKind::Word => {
                // A backticked name keeps its case; a bare word folds.
                if text.starts_with('`') {
                    text.trim_matches('`').to_string()
                } else if is_reserved(text) || text.starts_with(MARKER_PREFIX) {
                    continue;
                } else {
                    text.to_lowercase()
                }
            }
            TokenKind::Number => text.to_lowercase(),
            // A string or bytes atom is its *inner* text, so an `r'..'` prefix
            // or a different quoting does not read as a disagreement. The value
            // is what matters, not its spelling.
            TokenKind::String | TokenKind::Bytes => inner_literal_text(text),
            _ => continue,
        };
        if atom.is_empty() {
            continue;
        }
        *atoms.entry(atom).or_insert(0) += 1;
    }
    atoms
}

/// The atoms the parsed tree holds.
fn atoms_in_statements(statements: &[Statement]) -> BTreeMap<String, usize> {
    let mut atoms: BTreeMap<String, usize> = BTreeMap::new();
    for statement in statements {
        if let Some(query) = statement.top_level_query() {
            collect_query_atoms(query, &mut atoms);
        }
    }
    atoms
}

fn add(atoms: &mut BTreeMap<String, usize>, atom: &str) {
    if !atom.is_empty() {
        *atoms.entry(atom.to_string()).or_insert(0) += 1;
    }
}

fn collect_query_atoms(query: &Query, atoms: &mut BTreeMap<String, usize>) {
    match query {
        Query::Select { with, body } => {
            if let Some(with) = with {
                collect_with_atoms(with, atoms);
            }
            for projection in &body.projections {
                collect_expr_atoms(projection, atoms);
            }
            if let Some(from) = &body.from {
                collect_factor_atoms(from, atoms);
            }
            for expr in [
                body.selection.as_ref(),
                body.having.as_ref(),
                body.qualify.as_ref(),
                body.limit.as_ref(),
                body.offset.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                collect_expr_atoms(expr, atoms);
            }
            if let Some(group_by) = &body.group_by {
                for key in &group_by.keys {
                    collect_expr_atoms(key, atoms);
                }
                if let Some(grouping) = &group_by.grouping {
                    collect_grouping_atoms(grouping, atoms);
                }
            }
            if let Some(order_by) = &body.order_by {
                for key in &order_by.keys {
                    collect_expr_atoms(&key.expr, atoms);
                }
            }
        }
        Query::Values { with, rows } => {
            if let Some(with) = with {
                collect_with_atoms(with, atoms);
            }
            for row in rows {
                for value in row {
                    collect_expr_atoms(value, atoms);
                }
            }
        }
        Query::Pipe {
            with, base, stages, ..
        } => {
            collect_query_atoms(base, atoms);
            if let Some(with) = with {
                collect_with_atoms(with, atoms);
            }
            for stage in stages {
                collect_pipe_stage_atoms(stage, atoms);
            }
        }
        Query::SetOperation {
            with,
            left,
            right,
            order_by,
            ..
        } => {
            if let Some(with) = with {
                collect_with_atoms(with, atoms);
            }
            collect_query_atoms(left, atoms);
            collect_query_atoms(right, atoms);
            if let Some(order_by) = order_by {
                for key in &order_by.keys {
                    collect_expr_atoms(&key.expr, atoms);
                }
            }
        }
    }
}

fn collect_pipe_stage_atoms(stage: &PipeStage, atoms: &mut BTreeMap<String, usize>) {
    match stage {
        PipeStage::Select(exprs) | PipeStage::Extend(exprs) => {
            for expr in exprs {
                collect_expr_atoms(expr, atoms);
            }
        }
        PipeStage::Set(pairs) => {
            for (name, value) in pairs {
                add(atoms, &name.folded());
                collect_expr_atoms(value, atoms);
            }
        }
        PipeStage::Drop(columns) => {
            for column in columns {
                add(atoms, &column.folded());
            }
        }
        PipeStage::As(alias) => add(atoms, &alias.folded()),
        PipeStage::Where(expr) => collect_expr_atoms(expr, atoms),
        PipeStage::OrderBy(order_by) => {
            for key in &order_by.keys {
                collect_expr_atoms(&key.expr, atoms);
            }
        }
        PipeStage::Limit { limit, offset } => {
            for expr in [limit, offset].into_iter().flatten() {
                collect_expr_atoms(expr, atoms);
            }
        }
        // An unmodelled stage keeps its text, so its names are not atoms the
        // check can account for; the stage itself is what makes it unmodelled.
        PipeStage::Other { .. } => {}
    }
}

fn collect_with_atoms(with: &With, atoms: &mut BTreeMap<String, usize>) {
    for cte in &with.ctes {
        add(atoms, &cte.name.folded());
        for column in cte.columns.iter().flatten() {
            add(atoms, &column.folded());
        }
        collect_query_atoms(&cte.query, atoms);
    }
}

fn collect_grouping_atoms(grouping: &Grouping, atoms: &mut BTreeMap<String, usize>) {
    match grouping {
        Grouping::Rollup(items) | Grouping::Cube(items) => {
            for expr in items {
                collect_expr_atoms(expr, atoms);
            }
        }
        Grouping::Sets(sets) => {
            for set in sets {
                for expr in set {
                    collect_expr_atoms(expr, atoms);
                }
            }
        }
    }
}

fn collect_factor_atoms(factor: &TableFactor, atoms: &mut BTreeMap<String, usize>) {
    match factor {
        TableFactor::Table { name, alias, .. } => {
            collect_name_atoms(name, atoms);
            if let Some(alias) = alias {
                add(atoms, &alias.folded());
            }
        }
        TableFactor::Subquery {
            query,
            alias,
            columns,
        } => {
            collect_query_atoms(query, atoms);
            if let Some(alias) = alias {
                add(atoms, &alias.folded());
            }
            for column in columns.iter().flatten() {
                add(atoms, &column.folded());
            }
        }
        TableFactor::Unnest { array, alias, .. } => {
            collect_expr_atoms(array, atoms);
            if let Some(alias) = alias {
                add(atoms, &alias.folded());
            }
        }
        TableFactor::TableFunction { name, args, alias } => {
            collect_name_atoms(name, atoms);
            for arg in args {
                collect_expr_atoms(arg, atoms);
            }
            if let Some(alias) = alias {
                add(atoms, &alias.folded());
            }
        }
        TableFactor::Join {
            left,
            right,
            on,
            using,
            ..
        } => {
            collect_factor_atoms(left, atoms);
            collect_factor_atoms(right, atoms);
            if let Some(on) = on {
                collect_expr_atoms(on, atoms);
            }
            for column in using.iter().flatten() {
                add(atoms, &column.folded());
            }
        }
    }
}

fn collect_name_atoms(name: &ObjectName, atoms: &mut BTreeMap<String, usize>) {
    for part in &name.parts {
        add(atoms, &part.folded());
    }
}

fn collect_expr_atoms(expr: &Expr, atoms: &mut BTreeMap<String, usize>) {
    match expr {
        Expr::Column(name) => collect_name_atoms(name, atoms),
        Expr::Literal(literal) => match literal {
            Literal::Number(text) => add(atoms, &text.to_lowercase()),
            // The source token keeps its quotes, so compare against that.
            Literal::String(text) => add(atoms, text),
            Literal::Bytes(bytes) => add(
                atoms,
                &inner_literal_text(&Literal::Bytes(bytes.clone()).to_string()),
            ),
            // TRUE / FALSE / NULL are reserved, so they contribute nothing.
            Literal::Boolean(_) | Literal::Null => {}
            Literal::Parameter(name) => add(atoms, &format!("@{name}")),
        },
        Expr::Star => {}
        Expr::ModifiedStar(StarModifier::Except(cols)) => {
            for column in cols {
                add(atoms, &column.folded());
            }
        }
        Expr::ModifiedStar(StarModifier::Replace(exprs)) => {
            for expr in exprs {
                collect_expr_atoms(expr, atoms);
            }
        }
        Expr::ModifiedStar(StarModifier::Both { except, replace }) => {
            for column in except {
                add(atoms, &column.folded());
            }
            for expr in replace {
                collect_expr_atoms(expr, atoms);
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_expr_atoms(left, atoms);
            collect_expr_atoms(right, atoms);
        }
        Expr::Unary { expr, .. } => collect_expr_atoms(expr, atoms),
        Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_expr_atoms(item, atoms);
            }
        }
        Expr::Function {
            name,
            args,
            order_by,
            limit,
            ..
        } => {
            collect_name_atoms(name, atoms);
            for arg in args {
                collect_expr_atoms(arg, atoms);
            }
            if let Some(order_by) = order_by {
                for key in &order_by.keys {
                    collect_expr_atoms(&key.expr, atoms);
                }
            }
            if let Some(limit) = limit {
                collect_expr_atoms(limit, atoms);
            }
        }
        Expr::Filter {
            aggregate,
            predicate,
        } => {
            collect_expr_atoms(aggregate, atoms);
            collect_expr_atoms(predicate, atoms);
        }
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => {
            if let Some(operand) = operand {
                collect_expr_atoms(operand, atoms);
            }
            for (when, then) in whens {
                collect_expr_atoms(when, atoms);
                collect_expr_atoms(then, atoms);
            }
            if let Some(otherwise) = otherwise {
                collect_expr_atoms(otherwise, atoms);
            }
        }
        Expr::Cast {
            expr, data_type, ..
        } => {
            collect_expr_atoms(expr, atoms);
            // The target type is a name in the source and a string in the tree.
            add(atoms, &data_type.to_lowercase());
        }
        Expr::Subquery(query) => collect_query_atoms(query, atoms),
        Expr::Exists { query, .. } => collect_query_atoms(query, atoms),
        Expr::Alias { expr, alias } => {
            collect_expr_atoms(expr, atoms);
            add(atoms, &alias.folded());
        }
        Expr::In {
            expr, query, list, ..
        } => {
            collect_expr_atoms(expr, atoms);
            if let Some(query) = query {
                collect_query_atoms(query, atoms);
            }
            for item in list.iter().flatten() {
                collect_expr_atoms(item, atoms);
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_expr_atoms(expr, atoms);
            collect_expr_atoms(low, atoms);
            collect_expr_atoms(high, atoms);
        }
        Expr::IsNull { expr, .. } | Expr::IsBool { expr, .. } => collect_expr_atoms(expr, atoms),
        Expr::Like { expr, pattern, .. } => {
            collect_expr_atoms(expr, atoms);
            collect_expr_atoms(pattern, atoms);
        }
        Expr::Collate { expr, collation } => {
            collect_expr_atoms(expr, atoms);
            add(atoms, &collation.to_lowercase());
        }
        Expr::Interval { value, unit } => {
            collect_expr_atoms(value, atoms);
            if let Some(unit) = unit {
                add(atoms, &unit.to_lowercase());
            }
        }
        Expr::Array(items) | Expr::Struct { fields: items } => {
            for item in items {
                collect_expr_atoms(item, atoms);
            }
        }
        Expr::Window { function, spec } => {
            collect_expr_atoms(function, atoms);
            if let Some(name) = &spec.name {
                add(atoms, &name.folded());
            }
            for expr in &spec.partition_by {
                collect_expr_atoms(expr, atoms);
            }
            if let Some(order_by) = &spec.order_by {
                for key in &order_by.keys {
                    collect_expr_atoms(&key.expr, atoms);
                }
            }
        }
        Expr::WithExpr { variables, body } => {
            for (name, value) in variables {
                add(atoms, &name.folded());
                collect_expr_atoms(value, atoms);
            }
            collect_expr_atoms(body, atoms);
        }
        Expr::TableArg(name) => collect_name_atoms(name, atoms),
        Expr::Verbatim { name, .. } => collect_name_atoms(name, atoms),
    }
}

/// `sql` with its atoms grouped in parentheses, for reading a check's reasons.
///
/// The Python original's `reading` does this for its independent parser. Here it
/// is the parsed shape, rendered, which is what a caller needs to see when a
/// check reports a disagreement.
pub fn reading(sql: &str) -> Option<String> {
    match parse_statements(sql, false) {
        Ok(statements) => Some(crate::parse::render_statements(&statements)),
        Err(error) if error.kind == ErrorKind::Rejected => None,
        Err(_) => None,
    }
}
