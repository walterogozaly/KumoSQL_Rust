//! Promoting derived tables to top-level CTEs.
//!
//! Ported from `src/kumosql/lift_subqueries.py`.
//!
//! # What is lifted, and what is not
//!
//! A derived table in `FROM` or a `JOIN` becomes a CTE named `_subquery_N`. A
//! derived table whose body carries **its own `WITH`** is left exactly where it
//! is: the names that `WITH` binds are not visible to a CTE lifted to the top,
//! so lifting it would change what the query means. That is reported through
//! the `correlated_subquery_kept` diagnostic rather than done quietly.
//!
//! # Why a survivor is a failure
//!
//! `count_remaining` reports how many liftable derived tables are left, and a
//! non-zero count makes [`crate::RuleOutput::success`] false. A rule that lifted
//! some subqueries and gave up on others has not done its job, and reporting
//! that as a partial success is how a query quietly keeps a shape nobody
//! verified.

use crate::{RewriteRule, RuleDiagnostic};
use kumosql_sql::ast::*;

/// Promote every `FROM`/`JOIN` derived table into a uniquely named top-level
/// CTE.
pub struct LiftSubqueries;

impl RewriteRule for LiftSubqueries {
    fn name(&self) -> &'static str {
        "lift_subqueries"
    }
    fn summary(&self) -> &'static str {
        "Lift FROM/JOIN subqueries into named top-level CTEs"
    }

    fn count_remaining(&self, statements: &[Statement]) -> usize {
        statements
            .iter()
            .filter_map(Statement::top_level_query)
            .map(count_liftable)
            .sum()
    }

    fn rewrite_statement(
        &self,
        statement: &mut Statement,
        index: usize,
    ) -> Result<(usize, Vec<RuleDiagnostic>), String> {
        let Some(query) = statement.top_level_query_mut() else {
            return Ok((0, Vec::new()));
        };

        let before = count_liftable(query);
        let mut fresh = FreshNames::new(query);
        let mut ctes: Vec<Cte> = Vec::new();
        let mut lifted = 0usize;
        let mut kept = 0usize;

        lift_in_query(query, &mut fresh, &mut ctes, &mut lifted, &mut kept);

        if !ctes.is_empty() {
            // The lifted CTEs go in front of any that were already there: a CTE
            // may read another only if that one is defined first.
            let existing = query.with_clause().cloned().unwrap_or_default();
            let recursive = existing.recursive;
            let mut all = ctes;
            all.extend(existing.ctes);
            query.set_with_clause(Some(With {
                recursive,
                ctes: all,
            }));
        }

        let after = count_liftable(query);
        let mut diagnostics = Vec::new();
        if before > 0 && after > 0 {
            diagnostics.push(RuleDiagnostic::at(
                index as i64,
                "inline_subqueries_remaining",
                format!("{after} relational subquery/subqueries remain after transformation"),
            ));
        }
        if kept > 0 {
            diagnostics.push(RuleDiagnostic::at(
                index as i64,
                "correlated_subquery_kept",
                format!(
                    "{kept} subquery/subqueries left in place: they read a relation of the enclosing \\
                     query or a name defined by a nested WITH, which a top-level CTE cannot see"
                ),
            ));
        }
        Ok((lifted, diagnostics))
    }
}

/// Remove parentheses that cannot change how an expression parses.
///
/// **A no-op in this port, by construction.** The AST has no parenthesis node:
/// [`Expr::Binary`], [`Expr::And`], [`Expr::Or`] and [`Expr::Subquery`] each
/// carry their own delimiters, so `(a)` and `a` are the same tree and there is
/// nothing here to remove.
///
/// That is the safe direction -- an expression's meaning never depends on a
/// parenthesis the tree dropped -- and it is why
/// [`crate::cleanup::is_self_delimited`] exists for the rules that rebuild a
/// fragment. The cost is cosmetic: rendering always parenthesises a comparison
/// or a conjunction, so the output carries more parentheses than the Python
/// original leaves.
///
/// Giving this rule teeth would mean an `Expr::Paren` node that the parser
/// records and every other rule has to see through. Recorded in
/// `docs/parity-notes.md` as a deliberate divergence, not an oversight.
pub struct RemoveRedundantParentheses;

impl RewriteRule for RemoveRedundantParentheses {
    fn name(&self) -> &'static str {
        "remove_redundant_parentheses"
    }
    fn summary(&self) -> &'static str {
        "Remove parentheses that do not change how an expression parses"
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

/// Picks CTE names that collide with nothing already bound in the query.
struct FreshNames {
    taken: Vec<String>,
    next: usize,
}

impl FreshNames {
    fn new(query: &Query) -> Self {
        let mut taken = Vec::new();
        collect_names(query, &mut taken);
        FreshNames { taken, next: 1 }
    }

    /// A naming run for a nested query.
    ///
    /// A derived table cannot see the statement's top-level CTEs, so it starts
    /// its own run rather than reusing the outer one and producing a name that
    /// means two different things.
    fn nested(&self) -> Self {
        FreshNames {
            taken: self.taken.clone(),
            next: self.next,
        }
    }

    fn next_name(&mut self) -> Ident {
        loop {
            let candidate = Ident::bare(format!("_subquery_{}", self.next));
            self.next += 1;
            if !self.taken.contains(&candidate.folded()) {
                self.taken.push(candidate.folded());
                return candidate;
            }
        }
    }
}

/// Every name the query already binds, so a lifted CTE cannot shadow one.
fn collect_names(query: &Query, out: &mut Vec<String>) {
    if let Some(with) = query.with_clause() {
        for cte in &with.ctes {
            out.push(cte.name.folded());
            collect_names(&cte.query, out);
        }
    }
    match query {
        Query::SetOperation { left, right, .. } => {
            collect_names(left, out);
            collect_names(right, out);
        }
        Query::Pipe { base, .. } => collect_names(base, out),
        _ => {}
    }
}

/// Whether a derived table may be lifted to the top of its statement.
fn is_liftable(query: &Query) -> bool {
    // A nested `WITH` binds names a top-level CTE cannot see, so its derived
    // table stays where it is.
    !contains_nested_with(query)
}

fn contains_nested_with(query: &Query) -> bool {
    query.with_clause().is_some()
        || query
            .as_select()
            .and_then(|select| select.from.as_ref())
            .is_some_and(factor_contains_nested_with)
}

fn factor_contains_nested_with(factor: &TableFactor) -> bool {
    match factor {
        TableFactor::Subquery { query, .. } => contains_nested_with(query),
        TableFactor::Join {
            left, right, on, ..
        } => {
            factor_contains_nested_with(left)
                || factor_contains_nested_with(right)
                || on.as_ref().is_some_and(expr_contains_nested_with)
        }
        TableFactor::TableFunction { args, .. } => args.iter().any(expr_contains_nested_with),
        TableFactor::Unnest { array, .. } => expr_contains_nested_with(array),
        TableFactor::Table { .. } => false,
    }
}

fn expr_contains_nested_with(expr: &Expr) -> bool {
    match expr {
        Expr::Subquery(query) | Expr::Exists { query, .. } => contains_nested_with(query),
        Expr::In {
            expr, query, list, ..
        } => {
            expr_contains_nested_with(expr)
                || query.as_deref().is_some_and(contains_nested_with)
                || list.iter().flatten().any(expr_contains_nested_with)
        }
        Expr::Function { args, .. } => args.iter().any(expr_contains_nested_with),
        _ => false,
    }
}

/// Replace every liftable derived table under `query` with a fresh CTE name.
fn lift_in_query(
    query: &mut Query,
    fresh: &mut FreshNames,
    ctes: &mut Vec<Cte>,
    lifted: &mut usize,
    kept: &mut usize,
) {
    match query {
        Query::Select { with, body } => {
            if let Some(with) = with {
                for cte in &mut with.ctes {
                    let mut inner = fresh.nested();
                    lift_in_query(&mut cte.query, &mut inner, ctes, lifted, kept);
                }
            }
            lift_in_maybe_factor(&mut body.from, fresh, ctes, lifted, kept);
            for projection in &mut body.projections {
                lift_in_expr(projection, fresh, ctes, lifted, kept);
            }
            if let Some(selection) = &mut body.selection {
                lift_in_expr(selection, fresh, ctes, lifted, kept);
            }
            if let Some(having) = &mut body.having {
                lift_in_expr(having, fresh, ctes, lifted, kept);
            }
        }
        Query::SetOperation {
            with, left, right, ..
        } => {
            if let Some(with) = with {
                for cte in &mut with.ctes {
                    lift_in_query(&mut cte.query, fresh, ctes, lifted, kept);
                }
            }
            lift_in_query(left, fresh, ctes, lifted, kept);
            lift_in_query(right, fresh, ctes, lifted, kept);
        }
        Query::Pipe { with, base, .. } => {
            if let Some(with) = with {
                for cte in &mut with.ctes {
                    lift_in_query(&mut cte.query, fresh, ctes, lifted, kept);
                }
            }
            lift_in_query(base, fresh, ctes, lifted, kept);
        }
        Query::Values { with, .. } => {
            if let Some(with) = with {
                for cte in &mut with.ctes {
                    lift_in_query(&mut cte.query, fresh, ctes, lifted, kept);
                }
            }
        }
    }
}

fn lift_in_maybe_factor(
    factor: &mut Option<TableFactor>,
    fresh: &mut FreshNames,
    ctes: &mut Vec<Cte>,
    lifted: &mut usize,
    kept: &mut usize,
) {
    if let Some(factor) = factor {
        lift_in_factor(factor, fresh, ctes, lifted, kept);
    }
}

fn lift_in_factor(
    factor: &mut TableFactor,
    fresh: &mut FreshNames,
    ctes: &mut Vec<Cte>,
    lifted: &mut usize,
    kept: &mut usize,
) {
    if let TableFactor::Subquery { query, alias, .. } = factor {
        if is_liftable(query) {
            let name = fresh.next_name();
            let body = *std::mem::replace(
                query,
                Box::new(Query::Values {
                    with: None,
                    rows: Vec::new(),
                }),
            );
            ctes.push(Cte {
                name: name.clone(),
                columns: None,
                query: Box::new(body),
            });
            *lifted += 1;
            *factor = TableFactor::Table {
                name: ObjectName::new([name]),
                alias: alias.clone(),
                options: None,
            };
            return;
        }
        // The derived table stays, but anything liftable inside it still is.
        *kept += 1;
        lift_in_query(query, fresh, ctes, lifted, kept);
        return;
    }

    match factor {
        TableFactor::Join {
            left, right, on, ..
        } => {
            // A `Box`ed join side is moved out, rewritten, and put back, because
            // the recursion needs a plain `&mut TableFactor`.
            // A `Box`ed join side is moved out, rewritten and put back, because
            // the recursion wants a plain `&mut TableFactor`.
            let mut left_owned = std::mem::replace(
                left,
                Box::new(TableFactor::Table {
                    name: ObjectName::default(),
                    alias: None,
                    options: None,
                }),
            );
            lift_in_factor(&mut left_owned, fresh, ctes, lifted, kept);
            **left = *left_owned;

            let mut right_owned = std::mem::replace(
                right,
                Box::new(TableFactor::Table {
                    name: ObjectName::default(),
                    alias: None,
                    options: None,
                }),
            );
            lift_in_factor(&mut right_owned, fresh, ctes, lifted, kept);
            **right = *right_owned;

            if let Some(on) = on {
                lift_in_expr(on, fresh, ctes, lifted, kept);
            }
        }
        TableFactor::Unnest { array, .. } => lift_in_expr(array, fresh, ctes, lifted, kept),
        TableFactor::TableFunction { args, .. } => {
            for arg in args {
                lift_in_expr(arg, fresh, ctes, lifted, kept);
            }
        }
        TableFactor::Table { .. } => {}
        TableFactor::Subquery { .. } => {}
    }
}

fn lift_in_expr(
    expr: &mut Expr,
    fresh: &mut FreshNames,
    ctes: &mut Vec<Cte>,
    lifted: &mut usize,
    kept: &mut usize,
) {
    match expr {
        Expr::Subquery(query) | Expr::Exists { query, .. } => {
            lift_in_query(query, fresh, ctes, lifted, kept)
        }
        Expr::In {
            expr, query, list, ..
        } => {
            lift_in_expr(expr, fresh, ctes, lifted, kept);
            if let Some(query) = query {
                lift_in_query(query, fresh, ctes, lifted, kept);
            }
            for item in list.iter_mut().flatten() {
                lift_in_expr(item, fresh, ctes, lifted, kept);
            }
        }
        Expr::Function { args, .. } => {
            for arg in args {
                lift_in_expr(arg, fresh, ctes, lifted, kept);
            }
        }
        Expr::Window { function, spec } => {
            lift_in_expr(function, fresh, ctes, lifted, kept);
            for expr in &mut spec.partition_by {
                lift_in_expr(expr, fresh, ctes, lifted, kept);
            }
        }
        Expr::Filter {
            aggregate,
            predicate,
        } => {
            lift_in_expr(aggregate, fresh, ctes, lifted, kept);
            lift_in_expr(predicate, fresh, ctes, lifted, kept);
        }
        Expr::Binary { left, right, .. } => {
            lift_in_expr(left, fresh, ctes, lifted, kept);
            lift_in_expr(right, fresh, ctes, lifted, kept);
        }
        Expr::Unary { expr, .. } => lift_in_expr(expr, fresh, ctes, lifted, kept),
        Expr::And(items) | Expr::Or(items) => {
            for item in items {
                lift_in_expr(item, fresh, ctes, lifted, kept);
            }
        }
        Expr::Alias { expr, .. } => lift_in_expr(expr, fresh, ctes, lifted, kept),
        Expr::Cast { expr, .. } => lift_in_expr(expr, fresh, ctes, lifted, kept),
        Expr::Between {
            expr, low, high, ..
        } => {
            lift_in_expr(expr, fresh, ctes, lifted, kept);
            lift_in_expr(low, fresh, ctes, lifted, kept);
            lift_in_expr(high, fresh, ctes, lifted, kept);
        }
        Expr::IsNull { expr, .. } | Expr::IsBool { expr, .. } => {
            lift_in_expr(expr, fresh, ctes, lifted, kept)
        }
        Expr::Like { expr, pattern, .. } => {
            lift_in_expr(expr, fresh, ctes, lifted, kept);
            lift_in_expr(pattern, fresh, ctes, lifted, kept);
        }
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => {
            if let Some(operand) = operand {
                lift_in_expr(operand, fresh, ctes, lifted, kept);
            }
            for (when, then) in whens {
                lift_in_expr(when, fresh, ctes, lifted, kept);
                lift_in_expr(then, fresh, ctes, lifted, kept);
            }
            if let Some(otherwise) = otherwise {
                lift_in_expr(otherwise, fresh, ctes, lifted, kept);
            }
        }
        Expr::Collate { expr, .. } => lift_in_expr(expr, fresh, ctes, lifted, kept),
        Expr::Interval { value, .. } => lift_in_expr(value, fresh, ctes, lifted, kept),
        Expr::Array(items) | Expr::Struct { fields: items } => {
            for item in items {
                lift_in_expr(item, fresh, ctes, lifted, kept);
            }
        }
        Expr::WithExpr { variables, body } => {
            for (_, value) in variables {
                lift_in_expr(value, fresh, ctes, lifted, kept);
            }
            lift_in_expr(body, fresh, ctes, lifted, kept);
        }
        Expr::Column(_)
        | Expr::Literal(_)
        | Expr::Star
        | Expr::ModifiedStar(_)
        | Expr::TableArg(_)
        | Expr::Verbatim { .. } => {}
    }
}

/// How many derived tables in `query` could still be lifted.
fn count_liftable(query: &Query) -> usize {
    let mut total = 0usize;
    count_liftable_in_query(query, &mut total);
    total
}

fn count_liftable_in_query(query: &Query, total: &mut usize) {
    match query {
        Query::Select { with, body } => {
            if let Some(with) = with {
                for cte in &with.ctes {
                    count_liftable_in_query(&cte.query, total);
                }
            }
            if let Some(from) = &body.from {
                count_liftable_in_factor(from, total);
            }
            for projection in &body.projections {
                count_liftable_in_expr(projection, total);
            }
            for clause in [&body.selection, &body.having].into_iter().flatten() {
                count_liftable_in_expr(clause, total);
            }
        }
        Query::SetOperation {
            with, left, right, ..
        } => {
            if let Some(with) = with {
                for cte in &with.ctes {
                    count_liftable_in_query(&cte.query, total);
                }
            }
            count_liftable_in_query(left, total);
            count_liftable_in_query(right, total);
        }
        Query::Pipe { with, base, .. } => {
            if let Some(with) = with {
                for cte in &with.ctes {
                    count_liftable_in_query(&cte.query, total);
                }
            }
            count_liftable_in_query(base, total);
        }
        Query::Values { with, .. } => {
            if let Some(with) = with {
                for cte in &with.ctes {
                    count_liftable_in_query(&cte.query, total);
                }
            }
        }
    }
}

fn count_liftable_in_factor(factor: &TableFactor, total: &mut usize) {
    match factor {
        TableFactor::Subquery { query, .. } => {
            if is_liftable(query) {
                *total += 1;
            }
            count_liftable_in_query(query, total);
        }
        TableFactor::Join {
            left, right, on, ..
        } => {
            count_liftable_in_factor(left, total);
            count_liftable_in_factor(right, total);
            if let Some(on) = on {
                count_liftable_in_expr(on, total);
            }
        }
        TableFactor::Unnest { array, .. } => count_liftable_in_expr(array, total),
        TableFactor::TableFunction { args, .. } => {
            for arg in args {
                count_liftable_in_expr(arg, total);
            }
        }
        TableFactor::Table { .. } => {}
    }
}

fn count_liftable_in_expr(expr: &Expr, total: &mut usize) {
    match expr {
        Expr::Subquery(query) | Expr::Exists { query, .. } => count_liftable_in_query(query, total),
        Expr::In {
            expr, query, list, ..
        } => {
            count_liftable_in_expr(expr, total);
            if let Some(query) = query {
                count_liftable_in_query(query, total);
            }
            for item in list.iter().flatten() {
                count_liftable_in_expr(item, total);
            }
        }
        Expr::Function { args, .. } => {
            for arg in args {
                count_liftable_in_expr(arg, total);
            }
        }
        Expr::Window { function, spec } => {
            count_liftable_in_expr(function, total);
            for expr in &spec.partition_by {
                count_liftable_in_expr(expr, total);
            }
        }
        Expr::Filter {
            aggregate,
            predicate,
        } => {
            count_liftable_in_expr(aggregate, total);
            count_liftable_in_expr(predicate, total);
        }
        Expr::Binary { left, right, .. } => {
            count_liftable_in_expr(left, total);
            count_liftable_in_expr(right, total);
        }
        Expr::Unary { expr, .. } => count_liftable_in_expr(expr, total),
        Expr::And(items) | Expr::Or(items) => {
            for item in items {
                count_liftable_in_expr(item, total);
            }
        }
        Expr::Alias { expr, .. } => count_liftable_in_expr(expr, total),
        Expr::Cast { expr, .. } => count_liftable_in_expr(expr, total),
        Expr::Between {
            expr, low, high, ..
        } => {
            count_liftable_in_expr(expr, total);
            count_liftable_in_expr(low, total);
            count_liftable_in_expr(high, total);
        }
        Expr::IsNull { expr, .. } | Expr::IsBool { expr, .. } => {
            count_liftable_in_expr(expr, total)
        }
        Expr::Like { expr, pattern, .. } => {
            count_liftable_in_expr(expr, total);
            count_liftable_in_expr(pattern, total);
        }
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => {
            if let Some(operand) = operand {
                count_liftable_in_expr(operand, total);
            }
            for (when, then) in whens {
                count_liftable_in_expr(when, total);
                count_liftable_in_expr(then, total);
            }
            if let Some(otherwise) = otherwise {
                count_liftable_in_expr(otherwise, total);
            }
        }
        Expr::Collate { expr, .. } => count_liftable_in_expr(expr, total),
        Expr::Interval { value, .. } => count_liftable_in_expr(value, total),
        Expr::Array(items) | Expr::Struct { fields: items } => {
            for item in items {
                count_liftable_in_expr(item, total);
            }
        }
        Expr::WithExpr { variables, body } => {
            for (_, value) in variables {
                count_liftable_in_expr(value, total);
            }
            count_liftable_in_expr(body, total);
        }
        Expr::Column(_)
        | Expr::Literal(_)
        | Expr::Star
        | Expr::ModifiedStar(_)
        | Expr::TableArg(_)
        | Expr::Verbatim { .. } => {}
    }
}
