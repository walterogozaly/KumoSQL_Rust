//! Normalisations the provers need before they can reason about a query.
//!
//! Ported from `ast_utils.py`, starting with [`expand_group_by_all`].
//!
//! # Why this lives in its own pass
//!
//! `GROUP BY ALL` is an instruction to infer the grouping keys from the select
//! list, which is exactly the kind of thing a prover must not do implicitly. If
//! a prover silently ignored the `ALL` flag it would read an aggregate-only
//! `GROUP BY ALL` as a *grouped* query with no row on empty input, and would
//! then prove `COUNT(*) ... GROUP BY ALL` equal to `GROUP BY a` -- both provers
//! did exactly that before this pass existed. Spelling the keys out first makes
//! the inference explicit and checkable.
//!
//! Every shape whose keys depend on the engine or on an outer scope is
//! **declined**, not guessed. The refusals below are the interesting half.

use crate::ast::*;
use crate::error::{Error, ErrorKind, Result};

/// BigQuery aggregate functions whose presence makes a select item an aggregate.
///
/// A select item holding one of these is an aggregate; anything else that is a
/// function must be a known scalar, or the query is refused -- an unrecognised
/// function may be an aggregate wearing a different name, and treating it as a
/// key would silently change the grouping.
///
/// This list has to grow with BigQuery's function catalogue. It is a copy
/// rather than a lookup because there is no Rust peer for `sqlglot`'s function
/// metadata.
pub const AGGREGATES: &[&str] = &[
    "ANY_VALUE",
    "APPROX_COUNT_DISTINCT",
    "APPROX_QUANTILES",
    "APPROX_TOP_COUNT",
    "APPROX_TOP_SUM",
    "ARRAY_AGG",
    "ARRAY_CONCAT_AGG",
    "AVG",
    "BIT_AND",
    "BIT_OR",
    "BIT_XOR",
    "COUNT",
    "COUNTIF",
    "CORR",
    "COVAR_POP",
    "COVAR_SAMP",
    "GROUPING",
    "LOGICAL_AND",
    "LOGICAL_OR",
    "MAX",
    "MAX_BY",
    "MIN",
    "MIN_BY",
    "PERCENTILE_CONT",
    "PERCENTILE_DISC",
    "STDDEV",
    "STDDEV_POP",
    "STDDEV_SAMP",
    "STRING_AGG",
    "SUM",
    "VAR_POP",
    "VAR_SAMP",
    "VARIANCE",
];

/// Scalar functions that are definitely not aggregates.
///
/// Only needed so that a *known* non-aggregate function is accepted. Anything
/// outside both lists is refused, which is the safe direction.
pub const SCALARS: &[&str] = &[
    "ABS",
    "ACOS",
    "ANY_TYPE",
    "APPROX_TOP_N",
    "ARRAY",
    "ARRAY_LENGTH",
    "ASCII",
    "ASIN",
    "ATAN",
    "ATAN2",
    "BIT_COUNT",
    "BOOL",
    "BYTE_LENGTH",
    "CEIL",
    "CEILING",
    "CHAR_LENGTH",
    "CHR",
    "CODE_POINTS_TO_BYTES",
    "CODE_POINTS_TO_STRING",
    "COALESCE",
    "CONCAT",
    "CORRECT",
    "COS",
    "COSH",
    "COT",
    "COTH",
    "COUNTIF",
    "CURRENT_DATE",
    "CURRENT_DATETIME",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "DATE",
    "DATE_ADD",
    "DATE_DIFF",
    "DATE_FROM_UNIX_DATE",
    "DATE_SUB",
    "DATE_TRUNC",
    "DATETIME",
    "DATETIME_ADD",
    "DATETIME_DIFF",
    "DATETIME_SUB",
    "DATETIME_TRUNC",
    "DECODE",
    "DEGREES",
    "DIV",
    "ENDS_WITH",
    "EXP",
    "EXTRACT",
    "FARM_FINGERPRINT",
    "FIRST_VALUE",
    "FLOOR",
    "FORMAT",
    "FORMAT_DATE",
    "FORMAT_DATETIME",
    "FORMAT_TIME",
    "FORMAT_TIMESTAMP",
    "FROM_BASE32",
    "FROM_BASE64",
    "FROM_HEX",
    "GENERATE_ARRAY",
    "GENERATE_DATE_ARRAY",
    "GREATEST",
    "IF",
    "IFNULL",
    "INSTR",
    "IS_INF",
    "IS_NAN",
    "JSON_EXTRACT",
    "JSON_EXTRACT_SCALAR",
    "JSON_QUERY",
    "JSON_VALUE",
    "LAST_VALUE",
    "LEAST",
    "LEFT",
    "LENGTH",
    "LN",
    "LOG",
    "LOG10",
    "LOGICAL_AND",
    "LOGICAL_OR",
    "LOWER",
    "LPAD",
    "LTRIM",
    "MAKE_INTERVAL",
    "MAX_BY",
    "MIN_BY",
    "MOD",
    "NORMALIZE",
    "NORMALIZE_AND_CASE_FOLD",
    "NULLIF",
    "PARSE_DATE",
    "PARSE_DATETIME",
    "PARSE_TIME",
    "PARSE_TIMESTAMP",
    "POW",
    "POWER",
    "RADIANS",
    "RANGE_BUCKET",
    "REGEXP_CONTAINS",
    "REGEXP_EXTRACT",
    "REGEXP_EXTRACT_ALL",
    "REGEXP_INSTR",
    "REGEXP_REPLACE",
    "REPEAT",
    "REPLACE",
    "REVERSE",
    "RIGHT",
    "ROUND",
    "RPAD",
    "RTRIM",
    "SAFE_ADD",
    "SAFE_DIVIDE",
    "SAFE_MULTIPLY",
    "SAFE_NEGATE",
    "SAFE_SUBTRACT",
    "SIGN",
    "SIN",
    "SINH",
    "SPLIT",
    "SQRT",
    "STARTS_WITH",
    "ST_ASCII",
    "ST_UTF8",
    "STRPOS",
    "SUBSTR",
    "SUBSTRING",
    "TIMESTAMP",
    "TIMESTAMP_ADD",
    "TIMESTAMP_DIFF",
    "TIMESTAMP_MICROS",
    "TIMESTAMP_MILLIS",
    "TIMESTAMP_SECONDS",
    "TIMESTAMP_SUB",
    "TIMESTAMP_TRUNC",
    "TO_BASE64",
    "TO_CODE_POINTS",
    "TO_HEX",
    "TO_JSON_STRING",
    "TRIM",
    "TRUNC",
    "UNIX_DATE",
    "UNIX_MICROS",
    "UNIX_MILLIS",
    "UNIX_SECONDS",
    "UPPER",
    "VALUE",
    "VAR_POP",
];

/// Whether `name` is a known BigQuery aggregate.
pub fn is_aggregate_name(name: &str) -> bool {
    AGGREGATES.iter().any(|a| name.eq_ignore_ascii_case(a))
}

/// Whether `expr` holds a window function.
///
/// A window makes the inferred keys ambiguous, because a windowed expression
/// may reference grouping columns that are not themselves keys.
pub fn contains_window(expr: &Expr) -> bool {
    match expr {
        Expr::Window { .. } => true,
        Expr::Function { args, .. } => args.iter().any(contains_window),
        Expr::Binary { left, right, .. } => contains_window(left) || contains_window(right),
        Expr::Unary { expr, .. } => contains_window(expr),
        Expr::And(items) | Expr::Or(items) => items.iter().any(contains_window),
        Expr::Alias { expr, .. } => contains_window(expr),
        Expr::In { expr, list, .. } => {
            contains_window(expr) || list.as_ref().is_some_and(|l| l.iter().any(contains_window))
        }
        Expr::Between {
            expr, low, high, ..
        } => contains_window(expr) || contains_window(low) || contains_window(high),
        Expr::IsNull { expr, .. } | Expr::IsBool { expr, .. } => contains_window(expr),
        Expr::Like { expr, pattern, .. } => contains_window(expr) || contains_window(pattern),
        Expr::Collate { expr, .. } => contains_window(expr),
        Expr::Interval { value, .. } => contains_window(value),
        Expr::Array(items) => items.iter().any(contains_window),
        Expr::Struct { fields } => fields.iter().any(contains_window),
        Expr::Filter {
            aggregate,
            predicate,
        } => contains_window(aggregate) || contains_window(predicate),
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => {
            operand.as_deref().is_some_and(contains_window)
                || whens
                    .iter()
                    .any(|(c, t)| contains_window(c) || contains_window(t))
                || otherwise.as_deref().is_some_and(contains_window)
        }
        Expr::Cast { expr, .. } => contains_window(expr),
        Expr::Star
        | Expr::ModifiedStar(_)
        | Expr::Literal(_)
        | Expr::Column(_)
        | Expr::TableArg(_) => false,
        Expr::WithExpr { body, .. } => contains_window(body),
        // A subquery is refused separately; do not also treat it as a window.
        Expr::Subquery(_) | Expr::Exists { .. } | Expr::Verbatim { .. } => false,
    }
}

/// Whether `expr` holds a subquery in any position.
pub fn contains_subquery(expr: &Expr) -> bool {
    match expr {
        Expr::Subquery(_) | Expr::Exists { .. } => true,
        Expr::Function { args, .. } => args.iter().any(contains_subquery),
        Expr::Binary { left, right, .. } => contains_subquery(left) || contains_subquery(right),
        Expr::Unary { expr, .. } => contains_subquery(expr),
        Expr::And(items) | Expr::Or(items) => items.iter().any(contains_subquery),
        Expr::Alias { expr, .. } => contains_subquery(expr),
        Expr::In { expr, list, .. } => {
            contains_subquery(expr)
                || list
                    .as_ref()
                    .is_some_and(|l| l.iter().any(contains_subquery))
        }
        Expr::Between {
            expr, low, high, ..
        } => contains_subquery(expr) || contains_subquery(low) || contains_subquery(high),
        Expr::IsNull { expr, .. } | Expr::IsBool { expr, .. } => contains_subquery(expr),
        Expr::Like { expr, pattern, .. } => contains_subquery(expr) || contains_subquery(pattern),
        Expr::Collate { expr, .. } => contains_subquery(expr),
        Expr::WithExpr { variables, body } => {
            variables.iter().any(|(_, value)| contains_subquery(value)) || contains_subquery(body)
        }
        Expr::Interval { value, .. } => contains_subquery(value),
        Expr::Array(items) => items.iter().any(contains_subquery),
        Expr::Struct { fields } => fields.iter().any(contains_subquery),
        Expr::Filter {
            aggregate,
            predicate,
        } => contains_subquery(aggregate) || contains_subquery(predicate),
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => {
            operand.as_deref().is_some_and(contains_subquery)
                || whens
                    .iter()
                    .any(|(c, t)| contains_subquery(c) || contains_subquery(t))
                || otherwise.as_deref().is_some_and(contains_subquery)
        }
        Expr::Cast { expr, .. } => contains_subquery(expr),
        Expr::Window { function, .. } => contains_subquery(function),
        Expr::Star
        | Expr::ModifiedStar(_)
        | Expr::Literal(_)
        | Expr::Column(_)
        | Expr::TableArg(_) => false,
        Expr::Verbatim { .. } => false,
    }
}

/// Whether `expr` is, or holds, an aggregate call.
pub fn contains_aggregate(expr: &Expr) -> bool {
    match expr {
        Expr::Function { name, args, .. } => {
            is_aggregate_name(&name.to_string()) || args.iter().any(contains_aggregate)
        }
        Expr::Filter { aggregate, .. } => contains_aggregate(aggregate),
        Expr::Window { function, .. } => contains_aggregate(function),
        Expr::Binary { left, right, .. } => contains_aggregate(left) || contains_aggregate(right),
        Expr::Unary { expr, .. } => contains_aggregate(expr),
        Expr::And(items) | Expr::Or(items) => items.iter().any(contains_aggregate),
        Expr::Alias { expr, .. } => contains_aggregate(expr),
        Expr::In { expr, list, .. } => {
            contains_aggregate(expr)
                || list
                    .as_ref()
                    .is_some_and(|l| l.iter().any(contains_aggregate))
        }
        Expr::Between {
            expr, low, high, ..
        } => contains_aggregate(expr) || contains_aggregate(low) || contains_aggregate(high),
        Expr::IsNull { expr, .. } | Expr::IsBool { expr, .. } => contains_aggregate(expr),
        Expr::Like { expr, pattern, .. } => contains_aggregate(expr) || contains_aggregate(pattern),
        Expr::Collate { expr, .. } => contains_aggregate(expr),
        Expr::WithExpr { variables, body } => {
            variables.iter().any(|(_, value)| contains_aggregate(value)) || contains_aggregate(body)
        }
        Expr::Interval { value, .. } => contains_aggregate(value),
        Expr::Array(items) => items.iter().any(contains_aggregate),
        Expr::Struct { fields } => fields.iter().any(contains_aggregate),
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => {
            operand.as_deref().is_some_and(contains_aggregate)
                || whens
                    .iter()
                    .any(|(c, t)| contains_aggregate(c) || contains_aggregate(t))
                || otherwise.as_deref().is_some_and(contains_aggregate)
        }
        Expr::Cast { expr, .. } => contains_aggregate(expr),
        Expr::Star
        | Expr::ModifiedStar(_)
        | Expr::Literal(_)
        | Expr::Column(_)
        | Expr::TableArg(_) => false,
        Expr::Subquery(_) | Expr::Exists { .. } | Expr::Verbatim { .. } => false,
    }
}

/// Whether `expr` references a column.
pub fn contains_column(expr: &Expr) -> bool {
    match expr {
        Expr::Column(_) => true,
        Expr::Function { args, .. } => args.iter().any(contains_column),
        Expr::Binary { left, right, .. } => contains_column(left) || contains_column(right),
        Expr::Unary { expr, .. } => contains_column(expr),
        Expr::And(items) | Expr::Or(items) => items.iter().any(contains_column),
        Expr::Alias { expr, .. } => contains_column(expr),
        Expr::In { expr, list, .. } => {
            contains_column(expr) || list.as_ref().is_some_and(|l| l.iter().any(contains_column))
        }
        Expr::Between {
            expr, low, high, ..
        } => contains_column(expr) || contains_column(low) || contains_column(high),
        Expr::IsNull { expr, .. } | Expr::IsBool { expr, .. } => contains_column(expr),
        Expr::Like { expr, pattern, .. } => contains_column(expr) || contains_column(pattern),
        Expr::Collate { expr, .. } => contains_column(expr),
        Expr::WithExpr { variables, body } => {
            variables.iter().any(|(_, value)| contains_column(value)) || contains_column(body)
        }
        Expr::Interval { value, .. } => contains_column(value),
        Expr::Array(items) => items.iter().any(contains_column),
        Expr::Struct { fields } => fields.iter().any(contains_column),
        Expr::Filter {
            aggregate,
            predicate,
        } => contains_column(aggregate) || contains_column(predicate),
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => {
            operand.as_deref().is_some_and(contains_column)
                || whens
                    .iter()
                    .any(|(c, t)| contains_column(c) || contains_column(t))
                || otherwise.as_deref().is_some_and(contains_column)
        }
        Expr::Cast { expr, .. } => contains_column(expr),
        Expr::Window { function, .. } => contains_column(function),
        Expr::Star | Expr::ModifiedStar(_) | Expr::Literal(_) | Expr::TableArg(_) => false,
        Expr::Subquery(_) | Expr::Exists { .. } | Expr::Verbatim { .. } => false,
    }
}

/// Whether `expr` calls a function that is neither a known aggregate nor a known
/// scalar.
///
/// Such a call may be an aggregate under a name this port does not know, which
/// would make the inferred keys wrong, so the query is refused.
pub fn contains_unknown_function(expr: &Expr) -> bool {
    match expr {
        Expr::Function { name, args, .. } => {
            let rendered = name.to_string();
            if !is_aggregate_name(&rendered)
                && !SCALARS.iter().any(|s| s.eq_ignore_ascii_case(&rendered))
            {
                return true;
            }
            args.iter().any(contains_unknown_function)
        }
        Expr::Filter {
            aggregate,
            predicate,
        } => contains_unknown_function(aggregate) || contains_unknown_function(predicate),
        Expr::Window { function, .. } => contains_unknown_function(function),
        Expr::Binary { left, right, .. } => {
            contains_unknown_function(left) || contains_unknown_function(right)
        }
        Expr::Unary { expr, .. } => contains_unknown_function(expr),
        Expr::And(items) | Expr::Or(items) => items.iter().any(contains_unknown_function),
        Expr::Alias { expr, .. } => contains_unknown_function(expr),
        Expr::In { expr, list, .. } => {
            contains_unknown_function(expr)
                || list
                    .as_ref()
                    .is_some_and(|l| l.iter().any(contains_unknown_function))
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            contains_unknown_function(expr)
                || contains_unknown_function(low)
                || contains_unknown_function(high)
        }
        Expr::IsNull { expr, .. } | Expr::IsBool { expr, .. } => contains_unknown_function(expr),
        Expr::Like { expr, pattern, .. } => {
            contains_unknown_function(expr) || contains_unknown_function(pattern)
        }
        Expr::Collate { expr, .. } => contains_unknown_function(expr),
        Expr::WithExpr { variables, body } => {
            variables
                .iter()
                .any(|(_, value)| contains_unknown_function(value))
                || contains_unknown_function(body)
        }
        Expr::Interval { value, .. } => contains_unknown_function(value),
        Expr::Array(items) => items.iter().any(contains_unknown_function),
        Expr::Struct { fields } => fields.iter().any(contains_unknown_function),
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => {
            operand.as_deref().is_some_and(contains_unknown_function)
                || whens
                    .iter()
                    .any(|(c, t)| contains_unknown_function(c) || contains_unknown_function(t))
                || otherwise.as_deref().is_some_and(contains_unknown_function)
        }
        Expr::Cast { expr, .. } => contains_unknown_function(expr),
        Expr::Star
        | Expr::ModifiedStar(_)
        | Expr::Literal(_)
        | Expr::Column(_)
        | Expr::TableArg(_) => false,
        Expr::Subquery(_) | Expr::Exists { .. } => false,
        Expr::Verbatim { .. } => true,
    }
}

/// Spell `GROUP BY ALL` as the grouping keys it infers.
///
/// The keys are the select items that reference a column and hold no
/// aggregate, written as positions. With no keys the query has one group even
/// on empty input, so an aggregate-only select becomes a plain global aggregate.
///
/// Correlated selects, stars, windows, subqueries, unknown functions (which may
/// be aggregates) and a select of only constants (engines differ on whether a
/// constant is a key) are all declined.
///
/// Mutates `select` in place and returns `Ok(())`, or returns
/// [`ErrorKind::Unmodeled`] naming the shape that was declined.
pub fn expand_group_by_all(select: &mut Select) -> Result<()> {
    let Some(group_by) = &select.group_by else {
        return Ok(());
    };
    if !group_by.all {
        return Ok(());
    }

    // A `GROUP BY ALL` written alongside explicit keys is not this construct.
    if !group_by.keys.is_empty() || group_by.grouping.is_some() {
        return Err(Error::unmodeled(
            0,
            "GROUP BY ALL is not modelled next to explicit keys",
        ));
    }

    let mut keys: Vec<usize> = Vec::new();
    let mut key_sql: Vec<String> = Vec::new();
    let mut aggregated: Vec<&Expr> = Vec::new();
    let mut constants = false;

    for (position, item) in select.projections.iter().enumerate() {
        let value = match item {
            Expr::Alias { expr, .. } => expr.as_ref(),
            other => other,
        };

        if matches!(value, Expr::Star | Expr::ModifiedStar(_)) {
            return Err(Error::unmodeled(
                0,
                "GROUP BY ALL with a star select is not modelled",
            ));
        }
        if contains_window(value) {
            return Err(Error::unmodeled(
                0,
                "GROUP BY ALL next to a window is not modelled",
            ));
        }
        if contains_subquery(value) {
            return Err(Error::unmodeled(
                0,
                "GROUP BY ALL next to a subquery is not modelled",
            ));
        }

        if contains_aggregate(value) {
            aggregated.push(value);
        } else if contains_unknown_function(value) {
            return Err(Error::unmodeled(
                0,
                "GROUP BY ALL with an unknown function is not modelled",
            ));
        } else if contains_column(value) {
            keys.push(position + 1);
            key_sql.push(value.to_string());
        } else {
            constants = true;
        }
    }

    // A column outside the aggregates must be a key, or the query is invalid.
    for value in &aggregated {
        for column in top_level_columns(value) {
            if !key_sql.contains(&column) {
                return Err(Error::unmodeled(
                    0,
                    "GROUP BY ALL with an ungrouped column is not modelled",
                ));
            }
        }
    }

    if !keys.is_empty() {
        let group_by = select.group_by.as_mut().expect("checked above");
        group_by.all = false;
        group_by.keys = keys
            .into_iter()
            .map(|position| Expr::Literal(Literal::Number(position.to_string())))
            .collect();
    } else if !aggregated.is_empty() && !constants {
        // An aggregate-only select with no keys: one group even on empty input,
        // which is exactly the global aggregate, so the clause goes away.
        select.group_by = None;
    } else {
        return Err(Error::unmodeled(
            0,
            "GROUP BY ALL without grouping keys is not modelled",
        ));
    }

    Ok(())
}

/// The columns an expression reads outside any aggregate call it sits in.
///
/// `COUNT(*) + a` references `a` outside the aggregate, so `a` must be a key.
/// The Python original uses `value.find_all(exp.Column)` filtered by "not
/// inside an `AggFunc`", which is the same thing.
fn top_level_columns(expr: &Expr) -> Vec<String> {
    let mut found = Vec::new();
    collect_top_level_columns(expr, &mut found);
    found
}

fn collect_top_level_columns(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Column(name) => out.push(name.to_string()),
        // Inside an aggregate, columns are the aggregate's business.
        Expr::Function { .. } | Expr::Filter { .. } | Expr::Window { .. } => {}
        Expr::Binary { left, right, .. } => {
            collect_top_level_columns(left, out);
            collect_top_level_columns(right, out);
        }
        Expr::Unary { expr, .. } => collect_top_level_columns(expr, out),
        Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_top_level_columns(item, out);
            }
        }
        Expr::Alias { expr, .. } => collect_top_level_columns(expr, out),
        Expr::In { expr, list, .. } => {
            collect_top_level_columns(expr, out);
            for item in list.iter().flatten() {
                collect_top_level_columns(item, out);
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_top_level_columns(expr, out);
            collect_top_level_columns(low, out);
            collect_top_level_columns(high, out);
        }
        Expr::IsNull { expr, .. } | Expr::IsBool { expr, .. } => {
            collect_top_level_columns(expr, out)
        }
        Expr::Like { expr, pattern, .. } => {
            collect_top_level_columns(expr, out);
            collect_top_level_columns(pattern, out);
        }
        Expr::Collate { expr, .. } => collect_top_level_columns(expr, out),
        Expr::WithExpr { variables, body } => {
            for (_, value) in variables {
                collect_top_level_columns(value, out);
            }
            collect_top_level_columns(body, out);
        }
        Expr::Interval { value, .. } => collect_top_level_columns(value, out),
        Expr::Array(items) => {
            for item in items {
                collect_top_level_columns(item, out);
            }
        }
        Expr::Struct { fields } => {
            for field in fields {
                collect_top_level_columns(field, out);
            }
        }
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => {
            if let Some(operand) = operand {
                collect_top_level_columns(operand, out);
            }
            for (when, then) in whens {
                collect_top_level_columns(when, out);
                collect_top_level_columns(then, out);
            }
            if let Some(otherwise) = otherwise {
                collect_top_level_columns(otherwise, out);
            }
        }
        Expr::Cast { expr, .. } => collect_top_level_columns(expr, out),
        Expr::Star | Expr::ModifiedStar(_) | Expr::Literal(_) | Expr::TableArg(_) => {}
        Expr::Subquery(_) | Expr::Exists { .. } | Expr::Verbatim { .. } => {}
    }
}

/// Expand every `GROUP BY ALL` in a query, including inside derived tables and
/// CTEs.
///
/// A refusal inside a nested query is not propagated here -- the enclosing
/// expansion reports its own verdict -- but a `GROUP BY ALL` that is *reached*
/// is always expanded.
pub fn expand_group_by_all_in_query(query: &mut Query) -> Result<()> {
    match query {
        Query::Select { with, body } => {
            expand_group_by_all_in_with(with)?;
            expand_group_by_all(body)?;
            expand_group_by_all_in_maybe_factor(&mut body.from);
            for projection in &mut body.projections {
                expand_group_by_all_in_expr(projection);
            }
            expand_group_by_all_in_maybe(&mut body.selection);
            expand_group_by_all_in_maybe(&mut body.having);
            expand_group_by_all_in_maybe(&mut body.qualify);
            if let Some(order_by) = &mut body.order_by {
                for key in &mut order_by.keys {
                    expand_group_by_all_in_expr(&mut key.expr);
                }
            }
        }
        Query::Values { with, rows } => {
            expand_group_by_all_in_with(with)?;
            for row in rows {
                for value in row {
                    expand_group_by_all_in_expr(value);
                }
            }
        }
        Query::Pipe { with, base, stages } => {
            expand_group_by_all_in_with(with)?;
            expand_group_by_all_in_query(base)?;
            for stage in stages {
                match stage {
                    PipeStage::Select(exprs) | PipeStage::Extend(exprs) => {
                        for expr in exprs {
                            expand_group_by_all_in_expr(expr);
                        }
                    }
                    PipeStage::Set(pairs) => {
                        for (_, value) in pairs {
                            expand_group_by_all_in_expr(value);
                        }
                    }
                    PipeStage::Where(expr) => expand_group_by_all_in_expr(expr),
                    PipeStage::OrderBy(order_by) => {
                        for key in &mut order_by.keys {
                            expand_group_by_all_in_expr(&mut key.expr);
                        }
                    }
                    PipeStage::Limit { limit, offset } => {
                        for expr in [limit, offset].into_iter().flatten() {
                            expand_group_by_all_in_expr(expr);
                        }
                    }
                    PipeStage::Drop(_) | PipeStage::As(_) | PipeStage::Other { .. } => {}
                }
            }
        }
        Query::SetOperation {
            with, left, right, ..
        } => {
            expand_group_by_all_in_with(with)?;
            expand_group_by_all_in_query(left)?;
            expand_group_by_all_in_query(right)?;
        }
    }
    Ok(())
}

fn expand_group_by_all_in_with(with: &mut Option<With>) -> Result<()> {
    let Some(with) = with else { return Ok(()) };
    for cte in &mut with.ctes {
        expand_group_by_all_in_query(&mut cte.query)?;
    }
    Ok(())
}

fn expand_group_by_all_in_maybe(expr: &mut Option<Expr>) {
    if let Some(expr) = expr {
        expand_group_by_all_in_expr(expr);
    }
}

fn expand_group_by_all_in_maybe_factor(factor: &mut Option<TableFactor>) {
    if let Some(factor) = factor {
        expand_group_by_all_in_factor(factor);
    }
}

fn expand_group_by_all_in_factor(factor: &mut TableFactor) {
    match factor {
        TableFactor::Subquery { query, .. } => {
            let _ = expand_group_by_all_in_query(query);
        }
        TableFactor::Join {
            left, right, on, ..
        } => {
            expand_group_by_all_in_factor(left);
            expand_group_by_all_in_factor(right);
            expand_group_by_all_in_maybe(on);
        }
        TableFactor::TableFunction { args, .. } => {
            for arg in args {
                expand_group_by_all_in_expr(arg);
            }
        }
        TableFactor::Unnest { array, .. } => expand_group_by_all_in_expr(array),
        TableFactor::Table { .. } => {}
    }
}

/// Walk into `expr`, expanding any `GROUP BY ALL` in a query it contains.
fn expand_group_by_all_in_expr(expr: &mut Expr) {
    match expr {
        Expr::Subquery(query) => {
            let _ = expand_group_by_all_in_query(query);
        }
        Expr::Exists { query, .. } => {
            let _ = expand_group_by_all_in_query(query);
        }
        Expr::In {
            expr, query, list, ..
        } => {
            expand_group_by_all_in_expr(expr);
            if let Some(query) = query {
                let _ = expand_group_by_all_in_query(query);
            }
            for item in list.iter_mut().flatten() {
                expand_group_by_all_in_expr(item);
            }
        }
        Expr::Function { args, .. } => {
            for arg in args {
                expand_group_by_all_in_expr(arg);
            }
        }
        Expr::Window { function, .. } => expand_group_by_all_in_expr(function),
        Expr::Filter {
            aggregate,
            predicate,
        } => {
            expand_group_by_all_in_expr(aggregate);
            expand_group_by_all_in_expr(predicate);
        }
        Expr::Binary { left, right, .. } => {
            expand_group_by_all_in_expr(left);
            expand_group_by_all_in_expr(right);
        }
        Expr::Unary { expr, .. } => expand_group_by_all_in_expr(expr),
        Expr::And(items) | Expr::Or(items) => {
            for item in items {
                expand_group_by_all_in_expr(item);
            }
        }
        Expr::Alias { expr, .. } => expand_group_by_all_in_expr(expr),
        Expr::Cast { expr, .. } => expand_group_by_all_in_expr(expr),
        Expr::IsNull { expr, .. } | Expr::IsBool { expr, .. } => expand_group_by_all_in_expr(expr),
        Expr::Like { expr, pattern, .. } => {
            expand_group_by_all_in_expr(expr);
            expand_group_by_all_in_expr(pattern);
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            expand_group_by_all_in_expr(expr);
            expand_group_by_all_in_expr(low);
            expand_group_by_all_in_expr(high);
        }
        Expr::Case {
            operand,
            whens,
            otherwise,
        } => {
            if let Some(operand) = operand {
                expand_group_by_all_in_expr(operand);
            }
            for (when, then) in whens {
                expand_group_by_all_in_expr(when);
                expand_group_by_all_in_expr(then);
            }
            if let Some(otherwise) = otherwise {
                expand_group_by_all_in_expr(otherwise);
            }
        }
        Expr::Collate { expr, .. } => expand_group_by_all_in_expr(expr),
        Expr::WithExpr { variables, body } => {
            for (_, value) in variables {
                expand_group_by_all_in_expr(value);
            }
            expand_group_by_all_in_expr(body);
        }
        Expr::Interval { value, .. } => expand_group_by_all_in_expr(value),
        Expr::Array(items) | Expr::Struct { fields: items } => {
            for item in items {
                expand_group_by_all_in_expr(item);
            }
        }
        Expr::Star
        | Expr::ModifiedStar(_)
        | Expr::Literal(_)
        | Expr::Column(_)
        | Expr::TableArg(_) => {}
        Expr::Verbatim { .. } => {}
    }
}

/// The error a caller sees when `GROUP BY ALL` could not be expanded.
///
/// Kept as a constructor so every refusal names the construct, which is what
/// the Python tests assert on.
pub fn group_by_all_refusal() -> Error {
    Error::new(ErrorKind::Unmodeled, "GROUP BY ALL is not modelled here")
}
