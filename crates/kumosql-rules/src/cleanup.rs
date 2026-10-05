//! The cleanup rules.
//!
//! Ported from `src/kumosql/cleanup.py`, starting with the always-true
//! predicate fold and the CTE rules.
//!
//! # What "always true" means here
//!
//! [`truth_value`] answers a predicate's truth only when it can be decided with
//! certainty. Two INT64 literals compare exactly; any other numeric pair is
//! decided only when the texts are identical, because BigQuery may coerce INT64
//! to FLOAT64 and lose precision -so `9007199254740993 = 9007199254740992` is
//! *not* folded here even though an i64 says it should be. Two string literals
//! are equal only when their text is identical and carries no escapes.
//!
//! Everything else returns `None`, and `None` means *leave it alone*. A rule
//! that folded a comparison it was not sure about would rewrite a query whose
//! meaning it had guessed at.

use crate::{RewriteRule, RuleDiagnostic};
use kumosql_sql::ast::*;

/// `2^63 - 1`, the largest INT64. A literal above it is not an INT64, so its
/// comparison is not decided exactly.
const INT64_MAX: u64 = i64::MAX as u64;

/// The truth value of a predicate built only from literals, or `None`.
///
/// `None` is a real answer: it means the predicate cannot be decided, and the
/// caller must leave it as written.
pub fn truth_value(expr: &Expr) -> Option<bool> {
    match expr {
        Expr::Literal(Literal::Boolean(value)) => Some(*value),
        Expr::Unary {
            op: UnaryOp::Not,
            expr,
        } => truth_value(expr).map(|value| !value),
        Expr::Binary { op, left, right } => {
            if !is_foldable_comparison(*op) {
                return None;
            }
            let left = unparen(left);
            let right = unparen(right);
            if let Some(folded) = literal_compare(*op, left, right) {
                return Some(folded);
            }
            // Two string literals are only known equal when their text is
            // identical and carries no escapes; anything else is left alone.
            if matches!(op, BinaryOp::Eq | BinaryOp::NotEq)
                && let (Expr::Literal(Literal::String(a)), Expr::Literal(Literal::String(b))) =
                    (&left, &right)
                && a == b
                && !a.contains('\\')
            {
                return Some(matches!(op, BinaryOp::Eq));
            }
            None
        }
        _ => None,
    }
}

/// Whether an operator's result is one [`truth_value`] can fold.
fn is_foldable_comparison(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::Gt
            | BinaryOp::GtEq
            | BinaryOp::Lt
            | BinaryOp::LtEq
    )
}

/// Compare two numeric literals the way BigQuery would, when certain.
///
/// Two INT64 literals compare exactly. Any other pair is decided only when the
/// texts are identical, because BigQuery may coerce INT64 to FLOAT64 and lose
/// precision.
fn literal_compare(op: BinaryOp, left: &Expr, right: &Expr) -> Option<bool> {
    let (Expr::Literal(Literal::Number(a)), Expr::Literal(Literal::Number(b))) = (left, right)
    else {
        return None;
    };

    let (x, y) = if is_decimal_digits(a) && is_decimal_digits(b) {
        // A literal above INT64_MAX is not an INT64, so the comparison is not
        // exact and is left alone.
        let x: u64 = a.parse().ok()?;
        let y: u64 = b.parse().ok()?;
        if x > INT64_MAX || y > INT64_MAX {
            return None;
        }
        (x as i128, y as i128)
    } else if a == b {
        (0, 0)
    } else {
        return None;
    };

    Some(match op {
        BinaryOp::Eq => x == y,
        BinaryOp::NotEq => x != y,
        BinaryOp::Gt => x > y,
        BinaryOp::GtEq => x >= y,
        BinaryOp::Lt => x < y,
        BinaryOp::LtEq => x <= y,
        _ => return None,
    })
}

fn is_decimal_digits(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
}

/// Strip redundant parentheses: this AST keeps none, so there is nothing to do.
///
/// The parenthesised *shape* is carried by [`Expr::And`]/[`Expr::Or`] and by
/// [`Expr::Binary`], which both render their own delimiters.
fn unparen(expr: &Expr) -> &Expr {
    expr
}

/// Drop `TRUE` from AND chains and `FALSE` from OR chains, folding literals.
///
/// The walk only descends through AND, OR and NOT, so it stays in boolean
/// context: it never rewrites a comparison as if it were a predicate.
///
/// Returns the simplified predicate and the number of changes.
pub fn simplify_predicate(expr: &Expr) -> (Expr, usize) {
    match expr {
        Expr::And(items) => {
            let mut simplified = Vec::with_capacity(items.len());
            let mut changes = 0usize;
            for item in items {
                let (item, item_changes) = simplify_predicate(item);
                changes += item_changes;
                // `x AND TRUE` is `x`; `x AND FALSE` is not, because `x` may
                // raise an error BigQuery would not have reached.
                match truth_value(&item) {
                    Some(true) => changes += 1,
                    Some(false) => simplified.push(item),
                    None => simplified.push(item),
                }
            }
            match simplified.len() {
                0 => (Expr::Literal(Literal::Boolean(false)), changes),
                1 => (simplified.into_iter().next().expect("one item"), changes),
                _ => (Expr::And(simplified), changes),
            }
        }
        Expr::Or(items) => {
            let mut simplified = Vec::with_capacity(items.len());
            let mut changes = 0usize;
            for item in items {
                let (item, item_changes) = simplify_predicate(item);
                changes += item_changes;
                match truth_value(&item) {
                    Some(false) => changes += 1,
                    Some(true) => simplified.push(item),
                    None => simplified.push(item),
                }
            }
            match simplified.len() {
                0 => (Expr::Literal(Literal::Boolean(true)), changes),
                1 => (simplified.into_iter().next().expect("one item"), changes),
                _ => (Expr::Or(simplified), changes),
            }
        }
        Expr::Unary {
            op: UnaryOp::Not,
            expr,
        } => {
            let (inner, changes) = simplify_predicate(expr);
            match truth_value(&Expr::Unary {
                op: UnaryOp::Not,
                expr: Box::new(inner.clone()),
            }) {
                Some(value) => (Expr::Literal(Literal::Boolean(value)), changes + 1),
                None => (
                    Expr::Unary {
                        op: UnaryOp::Not,
                        expr: Box::new(inner),
                    },
                    changes,
                ),
            }
        }
        other => match truth_value(other) {
            // A bare TRUE is already canonical, so folding it is not a change.
            Some(_) if !matches!(other, Expr::Literal(Literal::Boolean(_))) => {
                (Expr::Literal(Literal::Boolean(true)), 1)
            }
            _ => (other.clone(), 0),
        },
    }
}

/// An expression whose rendering cannot be split by a neighbouring operator.
///
/// Dropping parentheses around anything else could change how it parses: `a OR
/// b AND c` is not `(a OR b) AND c`.
pub fn is_self_delimited(expr: &Expr) -> bool {
    match expr {
        Expr::Column(_)
        | Expr::Literal(_)
        | Expr::Star
        | Expr::ModifiedStar(_)
        | Expr::Binary { .. }
        | Expr::And(_)
        | Expr::Or(_)
        | Expr::Function { .. }
        | Expr::Filter { .. }
        | Expr::Case { .. }
        | Expr::Cast { .. }
        | Expr::Subquery(_)
        | Expr::Exists { .. }
        | Expr::Between { .. }
        | Expr::IsNull { .. }
        | Expr::IsBool { .. }
        | Expr::WithExpr { .. }
        | Expr::TableArg(_) => true,
        Expr::In { query: Some(_), .. } => true,
        // An `IN` list is bracketed, but `x NOT IN (...)` reads as a comparison,
        // so it is safe.
        Expr::In { list: Some(_), .. } => true,
        Expr::Alias { .. } | Expr::Collate { .. } | Expr::Interval { .. } => true,
        // `a = (b OR c)` -- the operand of a comparison is delimited by the
        // comparison itself, but `-x` is not.
        _ => false,
    }
}

/// Remove `WHERE 1 = 1`, `AND TRUE` and similar no-op predicates.
pub struct RemoveTrivialPredicates;

impl RewriteRule for RemoveTrivialPredicates {
    fn name(&self) -> &'static str {
        "remove_trivial_predicates"
    }
    fn summary(&self) -> &'static str {
        "Remove always-true filters such as WHERE 1 = 1 and AND TRUE"
    }

    fn keep_sqlx_expressions(&self) -> bool {
        true
    }

    fn rewrite_statement(
        &self,
        statement: &mut Statement,
        _index: usize,
    ) -> Result<(usize, Vec<RuleDiagnostic>), String> {
        // UPDATE, DELETE and MERGE predicates are left intact: the verifier
        // cannot prove those statements, so a changed DML statement could not
        // be shown equivalent -- and dropping `UPDATE ... WHERE TRUE` would
        // drop a clause BigQuery requires.
        if matches!(
            statement,
            Statement::Update { .. } | Statement::Delete { .. } | Statement::Merge(_)
        ) {
            return Ok((0, Vec::new()));
        }

        let Some(query) = statement.top_level_query_mut() else {
            return Ok((0, Vec::new()));
        };
        let mut changes = 0usize;

        // WHERE, then HAVING (only with a GROUP BY), then QUALIFY.
        // A `WHERE TRUE` is not "WHERE TRUE"; the clause goes.
        let drop_selection = query
            .selection_mut()
            .map(|selection| {
                let (simplified, count) = simplify_predicate(selection);
                changes += count;
                *selection = simplified;
                truth_value(selection) == Some(true)
            })
            .unwrap_or(false);
        if drop_selection && let Some(select) = query.as_select_mut() {
            select.clear_selection();
            changes += 1;
        }

        // `HAVING TRUE` is only dropped when a `GROUP BY` is present: without
        // one, `HAVING` is what makes the query an aggregate.
        let drop_having = query
            .having_mut()
            .map(|having| {
                let (simplified, count) = simplify_predicate(having);
                changes += count;
                *having = simplified;
                truth_value(having) == Some(true)
            })
            .unwrap_or(false);
        if drop_having
            && let Some(select) = query.as_select_mut()
            && select.group_by.is_some()
        {
            select.clear_having();
            changes += 1;
        }

        if let Some(qualify) = query.qualify_mut() {
            let (simplified, count) = simplify_predicate(qualify);
            changes += count;
            *qualify = simplified;
        }

        // An ON TRUE is kept: an INNER JOIN needs a condition.
        for factor in query.factors_mut() {
            if let TableFactor::Join { on, .. } = factor
                && let Some(condition) = on
            {
                let (simplified, count) = simplify_predicate(condition);
                changes += count;
                *on = Some(simplified);
            }
        }

        Ok((changes, Vec::new()))
    }
}

/// Remove root CTEs that no query or other CTE references.
pub struct RemoveUnusedCtes;

impl RewriteRule for RemoveUnusedCtes {
    fn name(&self) -> &'static str {
        "remove_unused_ctes"
    }
    fn summary(&self) -> &'static str {
        "Remove root CTEs that are never referenced"
    }

    fn keep_sqlx_expressions(&self) -> bool {
        true
    }

    fn rewrite_statement(
        &self,
        statement: &mut Statement,
        _index: usize,
    ) -> Result<(usize, Vec<RuleDiagnostic>), String> {
        let Some(query) = statement.top_level_query_mut() else {
            return Ok((0, Vec::new()));
        };
        Ok((remove_unused_ctes(query), Vec::new()))
    }
}

/// Drop every CTE in `query` that nothing outside it references.
fn remove_unused_ctes(query: &mut Query) -> usize {
    let mut removed = 0usize;
    // Repeated, because removing one CTE can make another unreferenced.
    while let Some(ctes) = query.with_clause().map(|with| with.ctes.clone()) {
        let unused: Vec<usize> = ctes
            .iter()
            .enumerate()
            .filter(|(_, cte)| !cte_is_referenced(query, &cte.name))
            .map(|(index, _)| index)
            .collect();
        if unused.is_empty() {
            break;
        }
        if let Some(with) = query.with_clause_mut() {
            for index in unused.into_iter().rev() {
                with.ctes.remove(index);
                removed += 1;
            }
        }
    }

    // A `WITH` left with no definitions is dropped rather than kept empty.
    if query.with_clause().is_some_and(|with| with.ctes.is_empty()) {
        query.set_with_clause(None);
    }
    removed
}

/// Whether any part of `query` other than `cte`'s own body reads `name`.
fn cte_is_referenced(query: &Query, name: &Ident) -> bool {
    let target = name.folded();
    let mut found = false;
    walk_query_except(query, &target, &mut found);
    found
}

/// Note every table read of `target` in `query`, skipping the CTE called
/// `skip`.
fn walk_query_except(query: &Query, skip: &str, found: &mut bool) {
    if let Some(with) = query.with_clause() {
        for cte in &with.ctes {
            // A CTE's own body is not a reference *from outside* it.
            if cte.name.folded() == skip {
                continue;
            }
            walk_query_except(&cte.query, skip, found);
        }
    }
    match query {
        Query::Select { body, .. } => {
            if let Some(from) = &body.from {
                walk_factor_except(from, skip, found);
            }
            for projection in &body.projections {
                walk_expr_tables(projection, skip, found);
            }
        }
        Query::SetOperation { left, right, .. } => {
            walk_query_except(left, skip, found);
            walk_query_except(right, skip, found);
        }
        Query::Pipe { base, .. } => walk_query_except(base, skip, found),
        Query::Values { .. } => {}
    }
}

/// Note a table read of `skip` in a `FROM` factor and everything inside it.
fn walk_factor_except(factor: &TableFactor, skip: &str, found: &mut bool) {
    match factor {
        TableFactor::Table { name, .. } => {
            if name.parts.last().is_some_and(|p| p.folded() == skip) {
                *found = true;
            }
        }
        TableFactor::Subquery { query, .. } => walk_query_except(query, skip, found),
        TableFactor::Join { left, right, .. } => {
            walk_factor_except(left, skip, found);
            walk_factor_except(right, skip, found);
        }
        TableFactor::TableFunction { name, args, .. } => {
            if name.parts.last().is_some_and(|p| p.folded() == skip) {
                *found = true;
            }
            for arg in args {
                walk_expr_tables(arg, skip, found);
            }
        }
        TableFactor::Unnest { array, .. } => walk_expr_tables(array, skip, found),
    }
}

/// Note a table read of `skip` in an expression, such as a table-valued call.
fn walk_expr_tables(expr: &Expr, skip: &str, found: &mut bool) {
    if let Expr::Function { name, args, .. } = expr {
        if name.parts.last().is_some_and(|p| p.folded() == skip) {
            *found = true;
        }
        for arg in args {
            walk_expr_tables(arg, skip, found);
        }
    }
}

/// Replace each root CTE referenced exactly once with an inline subquery.
pub struct InlineSingleUseCtes;

impl RewriteRule for InlineSingleUseCtes {
    fn name(&self) -> &'static str {
        "inline_single_use_ctes"
    }
    fn summary(&self) -> &'static str {
        "Replace each root CTE referenced exactly once with an inline subquery"
    }

    fn rewrite_statement(
        &self,
        statement: &mut Statement,
        _index: usize,
    ) -> Result<(usize, Vec<RuleDiagnostic>), String> {
        let Some(query) = statement.top_level_query_mut() else {
            return Ok((0, Vec::new()));
        };
        Ok((inline_single_use_ctes(query), Vec::new()))
    }
}

/// Inline every CTE that exactly one place reads.
fn inline_single_use_ctes(query: &mut Query) -> usize {
    let Some(with) = query.with_clause() else {
        return 0;
    };
    // Anything that makes inlining wrong, checked before touching anything.
    if with.recursive {
        return 0;
    }

    let ctes: Vec<Cte> = with.ctes.clone();
    let mut inlined = 0usize;

    for cte in &ctes {
        // A CTE with a column alias list has its columns renamed by the
        // reference, which the inline subquery would lose.
        if cte.columns.is_some() {
            continue;
        }
        // An `ORDER BY` or `LIMIT` inside a CTE that has `WITH OFFSET` or a
        // sibling reading it twice changes the reference's meaning.
        let references = count_references(query, &cte.name);
        if references != 1 {
            continue;
        }
        if !inline_reference(query, &cte.name, &cte.query) {
            continue;
        }
        inlined += 1;
        remove_cte(query, &cte.name);
    }

    if query.with_clause().is_some_and(|with| with.ctes.is_empty()) {
        query.set_with_clause(None);
    }
    inlined
}

/// How many times `name` is read in `query`, counting other CTEs' bodies.
///
/// A reference that inlining could not safely replace -- a dotted name, or a
/// reference carrying something beyond a bare alias -- counts as many times, so
/// the caller's `== 1` test rejects it.
fn count_references(query: &Query, name: &Ident) -> usize {
    let target = name.folded();
    let mut count = 0usize;
    count_query_references(query, &target, &mut count);
    count
}

fn count_query_references(query: &Query, target: &str, count: &mut usize) {
    if let Some(with) = query.with_clause() {
        for cte in &with.ctes {
            if cte.name.folded() == target {
                continue;
            }
            count_query_references(&cte.query, target, count);
        }
    }
    match query {
        Query::Select { body, .. } => {
            if let Some(from) = &body.from {
                count_factor_references(from, target, count);
            }
            for projection in &body.projections {
                count_expr_references(projection, target, count);
            }
        }
        Query::SetOperation { left, right, .. } => {
            count_query_references(left, target, count);
            count_query_references(right, target, count);
        }
        Query::Pipe { base, .. } => count_query_references(base, target, count),
        Query::Values { .. } => {}
    }
}

fn count_factor_references(factor: &TableFactor, target: &str, count: &mut usize) {
    match factor {
        TableFactor::Table { name, .. } => {
            if name.parts.last().is_some_and(|p| p.folded() == target) {
                *count += 1;
                // A qualified reference is not the bare name inlining replaces.
                if name.parts.len() > 1 {
                    *count += 1000;
                }
            }
        }
        TableFactor::Subquery { query, .. } => count_query_references(query, target, count),
        TableFactor::Join { left, right, .. } => {
            count_factor_references(left, target, count);
            count_factor_references(right, target, count);
        }
        TableFactor::TableFunction { args, .. } => {
            for arg in args {
                count_expr_references(arg, target, count);
            }
        }
        TableFactor::Unnest { array, .. } => count_expr_references(array, target, count),
    }
}

fn count_expr_references(expr: &Expr, target: &str, count: &mut usize) {
    match expr {
        Expr::Function { name, args, .. } => {
            if name.parts.last().is_some_and(|p| p.folded() == target) {
                *count += 1;
            }
            for arg in args {
                count_expr_references(arg, target, count);
            }
        }
        Expr::Alias { expr, .. } => count_expr_references(expr, target, count),
        Expr::Binary { left, right, .. } => {
            count_expr_references(left, target, count);
            count_expr_references(right, target, count);
        }
        Expr::And(items) | Expr::Or(items) => {
            for item in items {
                count_expr_references(item, target, count);
            }
        }
        Expr::Window { function, spec } => {
            count_expr_references(function, target, count);
            for expr in &spec.partition_by {
                count_expr_references(expr, target, count);
            }
        }
        _ => {}
    }
}

/// Replace the one reference to `name` with `body`, keeping its alias.
fn inline_reference(query: &mut Query, name: &Ident, body: &Query) -> bool {
    let target = name.folded();
    let Some(select) = query.as_select_mut() else {
        return false;
    };
    let Some(from) = select.from_mut() else {
        return false;
    };
    inline_in_factor(from, &target, body)
}

fn inline_in_factor(factor: &mut TableFactor, target: &str, body: &Query) -> bool {
    match factor {
        TableFactor::Table {
            name,
            alias,
            options,
        } => {
            if name.parts.len() == 1 && name.parts[0].folded() == target {
                let mut replacement = TableFactor::Subquery {
                    query: Box::new(body.clone()),
                    alias: alias.clone(),
                    columns: None,
                };
                if let TableFactor::Subquery { columns, .. } = &mut replacement {
                    // A reference to a bare name keeps no column list.
                    *columns = None;
                }
                let _ = options;
                *factor = replacement;
                true
            } else {
                false
            }
        }
        TableFactor::Subquery { query, .. } => inline_in_query(query, target, body),
        TableFactor::Join { left, right, .. } => {
            let a = inline_in_factor(left, target, body);
            let b = inline_in_factor(right, target, body);
            a || b
        }
        _ => false,
    }
}

fn inline_in_query(query: &mut Query, target: &str, body: &Query) -> bool {
    match query {
        Query::Select { body: select, .. } => match &mut select.from {
            Some(from) => inline_in_factor(from, target, body),
            None => false,
        },
        Query::SetOperation { left, right, .. } => {
            let a = inline_in_query(left, target, body);
            let b = inline_in_query(right, target, body);
            a || b
        }
        Query::Pipe { base, .. } => inline_in_query(base, target, body),
        Query::Values { .. } => false,
    }
}

/// Remove the CTE called `name` from `query`, and from any CTE nested in it.
fn remove_cte(query: &mut Query, name: &Ident) {
    let target = name.folded();
    if let Some(with) = query.with_clause_mut() {
        with.ctes.retain(|cte| cte.name.folded() != target);
        // A later CTE may have read the one just removed.
        for cte in &mut with.ctes {
            remove_cte(&mut cte.query, name);
        }
    }
}
