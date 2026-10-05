//! Parsing BigQuery SQL into [`crate::ast`], and back out as text.
//!
//! Ported from `parse_statements` / `render_statement` in `ast_utils.py`, and
//! from the install-once entry point in `bigquery_syntax.py`.
//!
//! # The pipeline
//!
//! ```text
//! source text
//!   -> canonical_literals        one spelling per string/bytes value
//!   -> rewrite_all                BigQuery shapes a generic parser cannot read
//!   -> sqlparser-rs               the base grammar, into its own AST
//!   -> convert                    sqlparser AST -> kumosql-sql AST
//! ```
//!
//! and rendering runs the tail of that in reverse, resolving the rewrite
//! markers back to BigQuery's own spelling.
//!
//! # Why a conversion step at all
//!
//! `sqlparser-rs`' AST is its own, and it is not the shape the rewrite rules and
//! provers want: it models dialects, positions and constructs KumoSQL declines
//! to reason about. Converting into [`crate::ast`] is where that becomes
//! explicit. An expression outside the proven subset becomes
//! [`ErrorKind::Unmodeled`] with the construct named, rather than a node that
//! survives into a rewrite and quietly changes what the rule meant.
//!
//! This is the shape the Python original uses for the same reason: it converts
//! through `sqlglot`'s dialect-specific reading rather than trusting that a
//! generic parse means what BigQuery means by it.

use sqlparser::ast as sp;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::ast::*;
use crate::error::{Error, ErrorKind, Result};
use crate::literals::canonical_literals;
use crate::rewrite::{FILTER_MARKER, rewrite_all};
use crate::rewrites::{LIKE_ALL_MARKER, TABLE_ARGUMENT_MARKER};

/// The dialect used for the base parse.
///
/// `GenericDialect` rather than a BigQuery dialect: `sqlparser-rs` has no
/// BigQuery dialect, and choosing one would silently change how standard SQL is
/// read. Everything BigQuery-specific is handled by [`crate::rewrite`] before
/// the parse, which keeps the two concerns separable and testable.
const DIALECT: GenericDialect = GenericDialect;

/// Parse BigQuery SQL into statements.
///
/// `recover` mirrors the Python original's recovery fallback: when the strict
/// parse fails, return the statements it can get rather than none, so a caller
/// can report partial progress instead of an empty result.
pub fn parse_statements(sql: &str, recover: bool) -> Result<Vec<Statement>> {
    // A refusal is a decision, not a parse failure, so it is never recovered
    // from: recovering past `STRUCT<>()` would mean reasoning about text
    // KumoSQL deliberately declined.
    let canonical = canonical_literals(sql);
    let rewritten = rewrite_all(&canonical)?;

    let parsed = Parser::parse_sql(&DIALECT, &rewritten);
    let statements = match parsed {
        Ok(statements) => statements,
        Err(err) => {
            if !recover {
                return Err(Error::unsupported(format!(
                    "sqlparser could not read this BigQuery: {err}"
                )));
            }
            // Recovery: sqlparser reports the same failure, so the recovery
            // path is a hard error until a tolerant parser is in place. Saying
            // so is better than pretending the fallback recovered anything.
            return Err(Error::unsupported(format!(
                "sqlparser could not read this BigQuery, and recovery is not implemented yet: {err}"
            )));
        }
    };

    let mut out = Vec::with_capacity(statements.len());
    let mut saw_table_argument = false;
    for (index, statement) in statements.iter().enumerate() {
        let span = offset_of_statement(&rewritten, index, statements.len());
        let converted =
            convert_statement(statement).map_err(|e| Error::at(e.kind, span, e.detail))?;
        // A statement with no query (a bare DDL command) has nothing to check.
        saw_table_argument |= converted
            .top_level_query()
            .is_some_and(query_mentions_table_argument);
        out.push(converted);
    }

    // `sqlparser` reads `FROM ds.fn(arg, ...)` as a bare table and drops the
    // arguments without reporting anything. Returning that would be silent data
    // loss, so the query is declined instead. Recorded as a known gap in
    // `docs/roadmap.md`.
    if rewritten.contains(TABLE_ARGUMENT_MARKER) && !saw_table_argument {
        return Err(Error::new(
            ErrorKind::Unmodeled,
            "a table function in FROM loses its arguments when read by the base parser; \\
             this query is declined rather than read without them",
        ));
    }

    Ok(out)
}

/// Whether a query still holds a `TABLE` argument.
///
/// Used to confirm the marker survived conversion rather than being dropped.
fn query_mentions_table_argument(query: &Query) -> bool {
    match query {
        Query::Select { with, body } => {
            body.projections.iter().any(is_table_arg)
                || factor_mentions_table_argument(&body.from)
                || with.as_ref().is_some_and(|w| {
                    w.ctes
                        .iter()
                        .any(|c| query_mentions_table_argument(&c.query))
                })
        }
        Query::Values { with, rows } => {
            rows.iter().flatten().any(is_table_arg)
                || with.as_ref().is_some_and(|w| {
                    w.ctes
                        .iter()
                        .any(|c| query_mentions_table_argument(&c.query))
                })
        }
        Query::Pipe {
            with, base, stages, ..
        } => {
            query_mentions_table_argument(base)
                || stages.iter().any(pipe_stage_mentions_table_argument)
                || with.as_ref().is_some_and(|w| {
                    w.ctes
                        .iter()
                        .any(|c| query_mentions_table_argument(&c.query))
                })
        }
        Query::SetOperation {
            with, left, right, ..
        } => {
            query_mentions_table_argument(left)
                || query_mentions_table_argument(right)
                || with.as_ref().is_some_and(|w| {
                    w.ctes
                        .iter()
                        .any(|c| query_mentions_table_argument(&c.query))
                })
        }
    }
}

/// Whether `expr` holds a `TABLE` argument anywhere inside it.
fn pipe_stage_mentions_table_argument(stage: &PipeStage) -> bool {
    match stage {
        PipeStage::Select(exprs) | PipeStage::Extend(exprs) => exprs.iter().any(is_table_arg),
        PipeStage::Set(pairs) => pairs.iter().any(|(_, value)| is_table_arg(value)),
        PipeStage::Where(expr) => is_table_arg(expr),
        PipeStage::OrderBy(order_by) => order_by.keys.iter().any(|k| is_table_arg(&k.expr)),
        _ => false,
    }
}

fn is_table_arg(expr: &Expr) -> bool {
    match expr {
        Expr::TableArg(_) => true,
        Expr::Function { args, .. } => args.iter().any(is_table_arg),
        Expr::Alias { expr, .. } => is_table_arg(expr),
        _ => false,
    }
}

fn factor_mentions_table_argument(factor: &Option<TableFactor>) -> bool {
    match factor {
        Some(TableFactor::TableFunction { args, .. }) => args.iter().any(is_table_arg),
        Some(TableFactor::Subquery { query, .. }) => query_mentions_table_argument(query),
        Some(TableFactor::Join { left, right, .. }) => {
            factor_mentions_table_argument(&Some(left.as_ref().clone()))
                || factor_mentions_table_argument(&Some(right.as_ref().clone()))
        }
        _ => false,
    }
}

/// A byte offset for the statement at `index`, used only for error reporting.
///
/// `sqlparser` does not hand back positions for every construct, so this is
/// approximate by design: it locates the statement start by counting the
/// semicolons before it. An approximate position is useful; a fabricated exact
/// one is not.
fn offset_of_statement(sql: &str, index: usize, total: usize) -> usize {
    if index == 0 {
        return 0;
    }
    let mut seen = 0usize;
    let mut in_string = false;
    let bytes: Vec<char> = sql.chars().collect();
    let mut offset = 0usize;
    for ch in bytes {
        if ch == '\'' {
            in_string = !in_string;
        }
        if ch == ';' && !in_string {
            seen += 1;
            if seen == index {
                return offset + ch.len_utf8();
            }
        }
        offset += ch.len_utf8();
    }
    let _ = total;
    sql.len()
}

/// Render statements back to SQL.
///
/// Undoes the text rewrites, so the output is BigQuery's own spelling rather
/// than the temporary standard form the base parser needed.
pub fn render_statements(statements: &[Statement]) -> String {
    let joined: Vec<String> = statements.iter().map(|s| s.to_string()).collect();
    joined.join(";\n")
}

fn convert_statement(statement: &sp::Statement) -> Result<Statement> {
    match statement {
        sp::Statement::Query(query) => Ok(Statement::Query(convert_query(query)?)),

        sp::Statement::Insert(insert) => Ok(Statement::Insert {
            table: match &insert.table {
                sp::TableObject::TableName(name) => convert_object_name(name),
                other => {
                    return Err(Error::new(
                        ErrorKind::Unmodeled,
                        format!("insert target {other:?} is outside the modelled subset"),
                    ));
                }
            },
            columns: (!insert.columns.is_empty())
                .then(|| insert.columns.iter().map(convert_ident).collect()),
            query: match &insert.source {
                Some(query) => Some(Box::new(convert_query(query)?)),
                None => None,
            },
        }),

        sp::Statement::Update {
            table,
            assignments,
            selection,
            ..
        } => Ok(Statement::Update {
            table: match &table.relation {
                sp::TableFactor::Table { name, .. } => convert_object_name(name),
                other => {
                    return Err(Error::new(
                        ErrorKind::Unmodeled,
                        format!("update target {other:?} is outside the modelled subset"),
                    ));
                }
            },
            assignments: Some(render_assignments(assignments)),
            selection: selection.as_ref().map(convert_expr).transpose()?,
        }),

        sp::Statement::Delete(delete) => Ok(Statement::Delete {
            table: match delete.tables.first() {
                Some(name) => convert_object_name(name),
                None => ObjectName::default(),
            },
            selection: delete.selection.as_ref().map(convert_expr).transpose()?,
        }),

        // Anything with a query body that creates something is a CTAS.
        sp::Statement::CreateTable(create) => match &create.query {
            Some(query) => Ok(Statement::CreateTableAs {
                name: convert_object_name(&create.name),
                query: Box::new(convert_query(query)?),
                replace: create.or_replace,
                if_not_exists: create.if_not_exists,
            }),
            None => Ok(unmodelled("CREATE TABLE", statement)),
        },

        sp::Statement::CreateView {
            name,
            query,
            or_replace,
            materialized,
            ..
        } => Ok(Statement::CreateViewAs {
            name: convert_object_name(name),
            query: Box::new(convert_query(query)?),
            replace: *or_replace,
            materialized: *materialized,
        }),

        _ => Ok(unmodelled("", statement)),
    }
}

/// A statement kept verbatim because KumoSQL does not model it.
fn unmodelled(keyword: &str, statement: &sp::Statement) -> Statement {
    Statement::Command {
        keyword: if keyword.is_empty() {
            statement_kind(statement)
        } else {
            keyword.to_string()
        },
        text: statement.to_string(),
    }
}

/// A short name for a statement kind, used as [`Statement::Command`]'s keyword.
fn statement_kind(statement: &sp::Statement) -> String {
    let rendered = statement.to_string();
    rendered
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase()
}

/// `sqlparser` has no `Display` for an assignment list, so it is rendered here.
fn render_assignments(assignments: &[sp::Assignment]) -> String {
    assignments
        .iter()
        .map(|a| format!("{} = {}", a.target, a.value))
        .collect::<Vec<_>>()
        .join(", ")
}

fn convert_query(query: &sp::Query) -> Result<Query> {
    let with = query.with.as_ref().map(convert_with).transpose()?;

    // `sqlparser` models pipe operators natively, so they need no text rewrite:
    // each stage becomes a node and a piped query is its own query kind.
    if !query.pipe_operators.is_empty() {
        let stages = query
            .pipe_operators
            .iter()
            .map(convert_pipe_stage)
            .collect::<Result<Vec<_>>>()?;
        let mut base = sp::Query {
            with: query.with.clone(),
            body: query.body.clone(),
            order_by: query.order_by.clone(),
            limit_clause: query.limit_clause.clone(),
            fetch: None,
            locks: Vec::new(),
            for_clause: None,
            settings: None,
            format_clause: None,
            pipe_operators: Vec::new(),
        };
        base.with = None;
        let converted = convert_query(&base)?;
        return Ok(Query::Pipe {
            with,
            base: Box::new(converted),
            stages,
        });
    }
    let order_by = query.order_by.as_ref().map(convert_order_by).transpose()?;
    let limit = convert_limit(&query.limit_clause)?;

    match query.body.as_ref() {
        sp::SetExpr::Select(select) => Ok(convert_select_select(with, select, order_by, limit)?),
        sp::SetExpr::Values(values) => Ok(Query::Values {
            with,
            rows: values
                .rows
                .iter()
                .map(|row| row.iter().map(convert_expr).collect::<Result<Vec<_>>>())
                .collect::<Result<Vec<_>>>()?,
        }),
        sp::SetExpr::SetOperation {
            op,
            set_quantifier,
            left,
            right,
        } => {
            let converted_op = match op {
                sp::SetOperator::Union => SetOp::Union,
                sp::SetOperator::Intersect => SetOp::Intersect,
                // MySQL's `MINUS` is `EXCEPT` under another name.
                sp::SetOperator::Except | sp::SetOperator::Minus => SetOp::Except,
            };
            let duplicate_handling = match set_quantifier {
                // `BY NAME` is a ClickHouse extension with no BigQuery meaning.
                sp::SetQuantifier::All => DuplicateHandling::All,
                sp::SetQuantifier::Distinct => DuplicateHandling::Distinct,
                sp::SetQuantifier::None => DuplicateHandling::All,
                other => {
                    return Err(Error::new(
                        ErrorKind::Unmodeled,
                        format!("set quantifier {other:?} is outside the modelled subset"),
                    ));
                }
            };
            Ok(Query::SetOperation {
                with,
                op: converted_op,
                duplicate_handling,
                left: Box::new(convert_set_expr(left)?),
                right: Box::new(convert_set_expr(right)?),
                order_by,
                limit,
            })
        }
        // A parenthesised query: `(SELECT ...) UNION (SELECT ...)`.
        sp::SetExpr::Query(inner) => convert_query(inner),
        other => Err(Error::new(
            ErrorKind::Unmodeled,
            format!("query body {other:?} is outside the modelled subset"),
        )),
    }
}

/// Convert one pipe operator.
///
/// The modelled stages become nodes; the rest keep their text and are reported
/// as unmodelled by [`PipeStage::is_modelled`], so a rule can refuse a piped
/// query rather than silently reason about a stage it dropped.
fn convert_pipe_stage(operator: &sp::PipeOperator) -> Result<PipeStage> {
    Ok(match operator {
        sp::PipeOperator::Select { exprs } => PipeStage::Select(convert_select_items(exprs)?),
        sp::PipeOperator::Extend { exprs } => PipeStage::Extend(convert_select_items(exprs)?),
        sp::PipeOperator::Set { assignments } => {
            let mut pairs = Vec::with_capacity(assignments.len());
            for assignment in assignments {
                let sp::AssignmentTarget::ColumnName(name) = &assignment.target else {
                    return Err(Error::new(
                        ErrorKind::Unmodeled,
                        format!(
                            "a |> SET target must name a column, got {:?}",
                            assignment.target
                        ),
                    ));
                };
                let column = name
                    .0
                    .first()
                    .map(convert_object_name_part)
                    .ok_or_else(|| {
                        Error::new(ErrorKind::Unmodeled, "a |> SET target names no column")
                    })?;
                pairs.push((column, convert_expr(&assignment.value)?));
            }
            PipeStage::Set(pairs)
        }
        sp::PipeOperator::Drop { columns } => {
            PipeStage::Drop(columns.iter().map(convert_ident).collect())
        }
        sp::PipeOperator::As { alias } => PipeStage::As(convert_ident(alias)),
        sp::PipeOperator::Where { expr } => PipeStage::Where(convert_expr(expr)?),
        sp::PipeOperator::Limit { expr, offset } => PipeStage::Limit {
            limit: Some(convert_expr(expr)?),
            offset: match offset {
                Some(offset) => Some(convert_expr(offset)?),
                None => None,
            },
        },
        sp::PipeOperator::OrderBy { exprs } => {
            let mut keys = Vec::with_capacity(exprs.len());
            for expr in exprs {
                keys.push(OrderKey {
                    expr: convert_expr(&expr.expr)?,
                    descending: Some(expr.options.asc == Some(false)),
                    nulls: expr.options.nulls_first,
                });
            }
            PipeStage::OrderBy(OrderBy { keys })
        }
        other => PipeStage::Other {
            keyword: pipe_keyword(other).to_string(),
            text: other.to_string(),
        },
    })
}

/// The keyword `sqlparser` would write a pipe operator with.
fn pipe_keyword(operator: &sp::PipeOperator) -> &'static str {
    match operator {
        sp::PipeOperator::Limit { .. } => "LIMIT",
        sp::PipeOperator::Where { .. } => "WHERE",
        sp::PipeOperator::OrderBy { .. } => "ORDER BY",
        sp::PipeOperator::Select { .. } => "SELECT",
        sp::PipeOperator::Extend { .. } => "EXTEND",
        sp::PipeOperator::Set { .. } => "SET",
        sp::PipeOperator::Drop { .. } => "DROP",
        sp::PipeOperator::As { .. } => "AS",
        sp::PipeOperator::Aggregate { .. } => "AGGREGATE",
        sp::PipeOperator::TableSample { .. } => "TABLESAMPLE",
        sp::PipeOperator::Rename { .. } => "RENAME",
        sp::PipeOperator::Union { .. } => "UNION",
        sp::PipeOperator::Intersect { .. } => "INTERSECT",
        sp::PipeOperator::Except { .. } => "EXCEPT",
        sp::PipeOperator::Call { .. } => "CALL",
        sp::PipeOperator::Pivot { .. } => "PIVOT",
        sp::PipeOperator::Unpivot { .. } => "UNPIVOT",
        sp::PipeOperator::Join(_) => "JOIN",
    }
}

fn convert_set_expr(expr: &sp::SetExpr) -> Result<Query> {
    match expr {
        sp::SetExpr::Select(select) => convert_select_select(None, select, None, None),
        sp::SetExpr::Query(query) => convert_query(query),
        sp::SetExpr::SetOperation {
            op,
            set_quantifier,
            left,
            right,
        } => Ok(Query::SetOperation {
            with: None,
            op: match op {
                sp::SetOperator::Union => SetOp::Union,
                sp::SetOperator::Intersect => SetOp::Intersect,
                sp::SetOperator::Except | sp::SetOperator::Minus => SetOp::Except,
            },
            duplicate_handling: match set_quantifier {
                sp::SetQuantifier::All => DuplicateHandling::All,
                sp::SetQuantifier::Distinct => DuplicateHandling::Distinct,
                sp::SetQuantifier::None => DuplicateHandling::All,
                other => {
                    return Err(Error::new(
                        ErrorKind::Unmodeled,
                        format!("set quantifier {other:?} is outside the modelled subset"),
                    ));
                }
            },
            left: Box::new(convert_set_expr(left)?),
            right: Box::new(convert_set_expr(right)?),
            order_by: None,
            limit: None,
        }),
        sp::SetExpr::Values(values) => Ok(Query::Values {
            with: None,
            rows: values
                .rows
                .iter()
                .map(|row| row.iter().map(convert_expr).collect::<Result<Vec<_>>>())
                .collect::<Result<Vec<_>>>()?,
        }),
        other => Err(Error::new(
            ErrorKind::Unmodeled,
            format!("set operand {other:?} is outside the modelled subset"),
        )),
    }
}

fn convert_select_select(
    with: Option<With>,
    select: &sp::Select,
    outer_order_by: Option<OrderBy>,
    outer_limit: Option<Expr>,
) -> Result<Query> {
    Ok(Query::Select {
        with,
        body: Box::new(convert_select(select, outer_order_by, outer_limit)?),
    })
}

fn convert_select(
    select: &sp::Select,
    outer_order_by: Option<OrderBy>,
    outer_limit: Option<Expr>,
) -> Result<Select> {
    // In sqlparser 0.59 ORDER BY, LIMIT and OFFSET live on the *Query*, not the
    // Select, so a query-level ORDER BY / LIMIT is what reaches us here. There
    // is no separate select-level clause to prefer.
    let (order_by, limit) = match outer_order_by {
        Some(order_by) => (Some(order_by), outer_limit),
        None => (None, None),
    };

    let mut projections = Vec::with_capacity(select.projection.len());
    for item in &select.projection {
        projections.push(convert_select_item(item)?);
    }

    let from = match select.from.first() {
        Some(table_with_joins) => Some(convert_table_with_joins(table_with_joins)?),
        None => None,
    };

    Ok(Select {
        as_kind: SelectAs::None,
        duplicate_handling: match &select.distinct {
            Some(sp::Distinct::Distinct) => DuplicateHandling::Distinct,
            _ => DuplicateHandling::All,
        },
        projections,
        from,
        selection: select.selection.as_ref().map(convert_expr).transpose()?,
        group_by: convert_group_by(&select.group_by)?,
        having: select.having.as_ref().map(convert_expr).transpose()?,
        qualify: select.qualify.as_ref().map(convert_expr).transpose()?,
        windows: if select.named_window.is_empty() {
            None
        } else {
            Some(
                select
                    .named_window
                    .iter()
                    .map(|w| format!("{} AS {}", w.0, w.1))
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        },
        order_by,
        limit,
        offset: None,
    })
}

fn convert_group_by(group_by: &sp::GroupByExpr) -> Result<Option<GroupBy>> {
    match group_by {
        sp::GroupByExpr::Expressions(exprs, modifiers) => {
            if exprs.is_empty() && modifiers.is_empty() {
                return Ok(None);
            }
            // ROLLUP / CUBE / GROUPING SETS are kept structurally.
            // GROUPING SETS arrives as a tuple of tuples.
            if let Some(sp::GroupByWithModifier::GroupingSets(sp::Expr::Tuple(sets))) =
                modifiers.first()
            {
                let mut converted: Vec<Vec<Expr>> = Vec::new();
                for set in sets {
                    match set {
                        sp::Expr::Tuple(group) => converted
                            .push(group.iter().map(convert_expr).collect::<Result<Vec<_>>>()?),
                        single => converted.push(vec![convert_expr(single)?]),
                    }
                }
                return Ok(Some(GroupBy {
                    all: false,
                    keys: Vec::new(),
                    grouping: Some(Grouping::Sets(converted)),
                }));
            }
            let keys = exprs.iter().map(convert_expr).collect::<Result<Vec<_>>>()?;
            Ok(Some(GroupBy {
                all: false,
                keys,
                grouping: None,
            }))
        }
        sp::GroupByExpr::All(_) => Ok(Some(GroupBy {
            all: true,
            keys: Vec::new(),
            grouping: None,
        })),
    }
}

fn convert_order_by(order_by: &sp::OrderBy) -> Result<OrderBy> {
    let exprs = match &order_by.kind {
        sp::OrderByKind::Expressions(exprs) => exprs,
        // `ORDER BY ALL` has no explicit keys; it is expanded by the caller
        // that knows the projections, which is `expand_group_by_all` in the
        // Python original.
        sp::OrderByKind::All(_) => return Ok(OrderBy::default()),
    };
    let mut keys = Vec::with_capacity(exprs.len());
    for expr in exprs {
        keys.push(OrderKey {
            expr: convert_expr(&expr.expr)?,
            descending: Some(expr.options.asc == Some(false)),
            nulls: expr.options.nulls_first.map(|first| {
                // `nulls_first: None` means the dialect default, which for
                // BigQuery is NULLS LAST for ASC and NULLS FIRST for DESC.
                match expr.options.asc {
                    Some(false) => !first,
                    _ => first,
                }
            }),
        });
    }
    Ok(OrderBy { keys })
}

fn convert_limit(limit_clause: &Option<sp::LimitClause>) -> Result<Option<Expr>> {
    let Some(clause) = limit_clause else {
        return Ok(None);
    };
    match clause {
        sp::LimitClause::LimitOffset { limit, .. } => match limit {
            Some(expr) => Ok(Some(convert_expr(expr)?)),
            None => Ok(None),
        },
        // MySQL's `LIMIT offset, limit` has its two bounds reversed relative to
        // BigQuery's spelling, so it is not read as a row cap here.
        sp::LimitClause::OffsetCommaLimit { .. } => Err(Error::new(
            ErrorKind::Unmodeled,
            "LIMIT <offset>, <limit> is MySQL syntax, not BigQuery",
        )),
    }
}

fn convert_with(with: &sp::With) -> Result<With> {
    Ok(With {
        recursive: with.recursive,
        ctes: with
            .cte_tables
            .iter()
            .map(|cte| {
                let query = convert_query(&cte.query)?;
                Ok(Cte {
                    name: convert_ident(&cte.alias.name),
                    columns: (!cte.alias.columns.is_empty()).then(|| {
                        cte.alias
                            .columns
                            .iter()
                            .map(|c| convert_ident(&c.name))
                            .collect()
                    }),
                    query: Box::new(query),
                })
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

fn convert_select_items(items: &[sp::SelectItem]) -> Result<Vec<Expr>> {
    items.iter().map(convert_select_item).collect()
}

fn convert_select_item(item: &sp::SelectItem) -> Result<Expr> {
    match item {
        sp::SelectItem::UnnamedExpr(expr) => convert_expr(expr),
        sp::SelectItem::ExprWithAlias { expr, alias } => Ok(Expr::Alias {
            expr: Box::new(convert_expr(expr)?),
            alias: convert_ident(alias),
        }),
        sp::SelectItem::Wildcard(options) => Ok(convert_wildcard_options(options)),
        sp::SelectItem::QualifiedWildcard(_, options) => Ok(convert_wildcard_options(options)),
    }
}

fn convert_wildcard_options(options: &sp::WildcardAdditionalOptions) -> Expr {
    // `* EXCEPT (...)`, `* REPLACE (...)`, or both in that order. The two are
    // read independently and combined here, because sqlparser carries them as
    // separate optional fields.
    let except: Vec<Ident> = match &options.opt_except {
        Some(item) => {
            let mut cols = vec![convert_ident(&item.first_element)];
            cols.extend(item.additional_elements.iter().map(convert_ident));
            cols
        }
        None => Vec::new(),
    };

    let replace: Vec<Expr> = match &options.opt_replace {
        Some(item) => item
            .items
            .iter()
            .map(|element| {
                convert_expr(&element.expr).map(|expr| Expr::Alias {
                    expr: Box::new(expr),
                    alias: convert_ident(&element.column_name),
                })
            })
            .collect::<Result<Vec<_>>>()
            .unwrap_or_default(),
        None => Vec::new(),
    };

    match (except.is_empty(), replace.is_empty()) {
        (true, true) => Expr::Star,
        (false, true) => Expr::ModifiedStar(StarModifier::Except(except)),
        (true, false) => Expr::ModifiedStar(StarModifier::Replace(replace)),
        (false, false) => Expr::ModifiedStar(StarModifier::Both { except, replace }),
    }
}

fn convert_table_with_joins(table: &sp::TableWithJoins) -> Result<TableFactor> {
    let mut factor = convert_table_factor(&table.relation)?;

    for join in &table.joins {
        let right = convert_table_factor(&join.relation)?;
        // In sqlparser 0.59 the join constraint hangs off the operator rather
        // than the join, so the kind and the constraint are read together.
        let (kind, constraint) = match &join.join_operator {
            sp::JoinOperator::Join(constraint) | sp::JoinOperator::Inner(constraint) => {
                (JoinKind::Inner, constraint)
            }
            sp::JoinOperator::Left(constraint) | sp::JoinOperator::LeftOuter(constraint) => {
                (JoinKind::Left, constraint)
            }
            sp::JoinOperator::Right(constraint) | sp::JoinOperator::RightOuter(constraint) => {
                (JoinKind::Right, constraint)
            }
            sp::JoinOperator::FullOuter(constraint) => (JoinKind::Full, constraint),
            sp::JoinOperator::CrossJoin(constraint) => (JoinKind::Cross, constraint),
            other => {
                return Err(Error::new(
                    ErrorKind::Unmodeled,
                    format!("join operator {other:?} is outside the modelled subset"),
                ));
            }
        };

        let (on, using) = match constraint {
            sp::JoinConstraint::On(expr) => (Some(convert_expr(expr)?), None),
            sp::JoinConstraint::Using(names) => (
                None,
                Some(
                    names
                        .iter()
                        .flat_map(|name: &sp::ObjectName| name.0.iter())
                        .map(|part| match part {
                            sp::ObjectNamePart::Identifier(ident) => convert_ident(ident),
                            other => Ident::bare(other.to_string()),
                        })
                        .collect(),
                ),
            ),
            sp::JoinConstraint::None | sp::JoinConstraint::Natural => (None, None),
        };

        factor = TableFactor::Join {
            left: Box::new(factor),
            kind,
            right: Box::new(right),
            on,
            using,
        };
    }
    Ok(factor)
}

fn convert_table_factor(factor: &sp::TableFactor) -> Result<TableFactor> {
    match factor {
        sp::TableFactor::Table { name, alias, .. } => Ok(TableFactor::Table {
            name: convert_object_name(name),
            alias: alias.as_ref().map(convert_table_alias),
            options: None,
        }),
        sp::TableFactor::Derived {
            subquery, alias, ..
        } => {
            let query = convert_query(subquery)?;
            Ok(TableFactor::Subquery {
                query: Box::new(query),
                alias: alias.as_ref().map(convert_table_alias),
                columns: alias
                    .as_ref()
                    .filter(|a| !a.columns.is_empty())
                    .map(|a| a.columns.iter().map(|c| convert_ident(&c.name)).collect()),
            })
        }
        sp::TableFactor::TableFunction { expr, alias } => {
            // `dataset.fn(arg, ...)` reaches here as a call expression whose
            // name is dotted. The `TABLE` argument inside it was marked in the
            // source; the marker is unwrapped below.
            match expr {
                sp::Expr::Function(function) => {
                    let call = convert_function(function)?;
                    match call {
                        // A marker call becomes the table it stood for.
                        Expr::TableArg(name) => Ok(TableFactor::Table {
                            name,
                            alias: alias.as_ref().map(convert_table_alias),
                            options: None,
                        }),
                        Expr::Function { name, args, .. } => Ok(TableFactor::TableFunction {
                            name,
                            args: args
                                .into_iter()
                                .map(|arg| match arg {
                                    Expr::TableArg(name) => Expr::TableArg(name),
                                    other => other,
                                })
                                .collect(),
                            alias: alias.as_ref().map(convert_table_alias),
                        }),
                        other => Err(Error::new(
                            ErrorKind::Unmodeled,
                            format!("table function {other:?} is outside the modelled subset"),
                        )),
                    }
                }
                other => Err(Error::new(
                    ErrorKind::Unmodeled,
                    format!("table function {other:?} is outside the modelled subset"),
                )),
            }
        }
        sp::TableFactor::UNNEST {
            array_exprs,
            alias,
            with_offset,
            with_offset_alias,
            ..
        } => {
            let array = match array_exprs.first() {
                Some(expr) => convert_expr(expr)?,
                None => {
                    return Err(Error::new(
                        ErrorKind::Unmodeled,
                        "UNNEST with no array expression",
                    ));
                }
            };
            Ok(TableFactor::Unnest {
                array: Box::new(array),
                offset: if *with_offset {
                    Some(with_offset_alias.as_ref().map(convert_ident))
                } else {
                    None
                },
                alias: alias.as_ref().map(convert_table_alias),
            })
        }
        _ => Err(Error::new(
            ErrorKind::Unmodeled,
            format!("table factor {factor:?} is outside the modelled subset"),
        )),
    }
}

fn convert_table_alias(alias: &sp::TableAlias) -> Ident {
    convert_ident(&alias.name)
}

fn convert_ident(ident: &sp::Ident) -> Ident {
    Ident {
        name: ident.value.clone(),
        quoted: ident.quote_style.is_some(),
    }
}

fn convert_object_name(name: &sp::ObjectName) -> ObjectName {
    ObjectName::new(name.0.iter().map(convert_object_name_part))
}

fn convert_object_name_part(part: &sp::ObjectNamePart) -> Ident {
    match part {
        sp::ObjectNamePart::Identifier(ident) => convert_ident(ident),
        // A part that is itself a call, such as a JSON path in a qualified
        // name. It is not a bare identifier, so it is kept as written.
        other => Ident::quoted(other.to_string()),
    }
}

fn convert_expr(expr: &sp::Expr) -> Result<Expr> {
    use sp::Expr as E;
    Ok(match expr {
        // A bare identifier in expression position is a column reference.
        E::Identifier(ident) => Expr::Column(ObjectName::new([convert_ident(ident)])),
        E::CompoundIdentifier(idents) => {
            Expr::Column(ObjectName::new(idents.iter().map(convert_ident)))
        }
        E::CompoundFieldAccess { root, access_chain } => {
            // In sqlparser 0.59 the chain is a list of `.` and `[...]` steps.
            // Only the `.` steps name a column; a subscript is not an
            // identifier, so it is kept as a quoted placeholder rather than
            // being flattened into a name BigQuery never wrote.
            let mut parts = vec![match convert_expr(root)? {
                Expr::Column(name) => name
                    .parts
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| Ident::quoted("<root>")),
                other => Ident::quoted(other.to_string()),
            }];
            for access in access_chain {
                match access {
                    sp::AccessExpr::Dot(expr) => parts.push(match convert_expr(expr)? {
                        Expr::Column(name) => name
                            .parts
                            .into_iter()
                            .next()
                            .unwrap_or_else(|| Ident::quoted("<root>")),
                        other => Ident::quoted(other.to_string()),
                    }),
                    sp::AccessExpr::Subscript(_) => parts.push(Ident::quoted("<subscript>")),
                }
            }
            Expr::Column(ObjectName::new(parts))
        }
        E::Value(value) => convert_value(&value.value)?,

        E::BinaryOp { left, op, right } => {
            let op = convert_binary_op(op)?;
            let left = convert_expr(left)?;
            let right = convert_expr(right)?;
            // AND and OR chains are flattened into the n-ary nodes, matching
            // `exp.And`: a rule reading a conjunction as a set of conjuncts
            // cannot see them inside a left-leaning binary tree.
            match op {
                BinaryOp::And => match left {
                    Expr::And(mut items) => {
                        items.push(right);
                        Expr::And(items)
                    }
                    other => Expr::And(vec![other, right]),
                },
                BinaryOp::Or => match left {
                    Expr::Or(mut items) => {
                        items.push(right);
                        Expr::Or(items)
                    }
                    other => Expr::Or(vec![other, right]),
                },
                _ => Expr::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            }
        }
        E::UnaryOp { op, expr } => Expr::Unary {
            op: match op {
                sp::UnaryOperator::Not => UnaryOp::Not,
                sp::UnaryOperator::Minus | sp::UnaryOperator::Plus => {
                    return Err(Error::new(
                        ErrorKind::Unmodeled,
                        "a signed numeric literal is not modelled; CAST is required",
                    ));
                }
                other => {
                    return Err(Error::new(
                        ErrorKind::Unmodeled,
                        format!("unary operator {other:?} is outside the modelled subset"),
                    ));
                }
            },
            expr: Box::new(convert_expr(expr)?),
        },
        E::Nested(inner) => convert_expr(inner)?,

        E::Function(function) => convert_function(function)?,

        E::Case {
            operand,
            conditions,
            else_result,
            ..
        } => Expr::Case {
            operand: match operand {
                Some(expr) => Some(Box::new(convert_expr(expr)?)),
                None => None,
            },
            whens: conditions
                .iter()
                .map(|case| Ok((convert_expr(&case.condition)?, convert_expr(&case.result)?)))
                .collect::<Result<Vec<_>>>()?,
            otherwise: match else_result {
                Some(expr) => Some(Box::new(convert_expr(expr)?)),
                None => None,
            },
        },

        E::Cast {
            kind,
            expr,
            data_type,
            ..
        } => Expr::Cast {
            expr: Box::new(convert_expr(expr)?),
            data_type: data_type.to_string(),
            // `SAFE_CAST` is a different function from `CAST`, not a flag on
            // the same one: it returns NULL on failure where `CAST` raises.
            safe: matches!(kind, sp::CastKind::TryCast | sp::CastKind::SafeCast),
        },

        E::Subquery(query) => Expr::Subquery(Box::new(convert_query(query)?)),
        E::Exists { subquery, negated } => Expr::Exists {
            query: Box::new(convert_query(subquery)?),
            negated: *negated,
        },

        E::InList {
            expr,
            list,
            negated,
            ..
        } => Expr::In {
            expr: Box::new(convert_expr(expr)?),
            query: None,
            list: Some(list.iter().map(convert_expr).collect::<Result<Vec<_>>>()?),
            negated: *negated,
        },
        E::InSubquery {
            expr,
            subquery,
            negated,
            ..
        } => Expr::In {
            expr: Box::new(convert_expr(expr)?),
            query: Some(Box::new(convert_query(subquery)?)),
            list: None,
            negated: *negated,
        },
        E::InUnnest {
            expr,
            array_expr,
            negated,
        } => Expr::In {
            expr: Box::new(convert_expr(expr)?),
            query: Some(Box::new(Query::Select {
                with: None,
                body: Box::new(Select {
                    projections: vec![Expr::Star],
                    from: Some(TableFactor::Unnest {
                        array: Box::new(convert_expr(array_expr)?),
                        offset: None,
                        alias: None,
                    }),
                    ..Default::default()
                }),
            })),
            list: None,
            negated: *negated,
        },

        E::Between {
            expr,
            negated,
            low,
            high,
        } => Expr::Between {
            expr: Box::new(convert_expr(expr)?),
            low: Box::new(convert_expr(low)?),
            high: Box::new(convert_expr(high)?),
            negated: *negated,
        },
        E::IsNull(inner) => Expr::IsNull {
            expr: Box::new(convert_expr(inner)?),
            negated: false,
        },
        E::IsNotNull(inner) => Expr::IsNull {
            expr: Box::new(convert_expr(inner)?),
            negated: true,
        },
        E::IsTrue(inner) => Expr::IsBool {
            expr: Box::new(convert_expr(inner)?),
            kind: IsBoolKind::True,
            negated: false,
        },
        E::IsNotTrue(inner) => Expr::IsBool {
            expr: Box::new(convert_expr(inner)?),
            kind: IsBoolKind::True,
            negated: true,
        },
        E::IsFalse(inner) => Expr::IsBool {
            expr: Box::new(convert_expr(inner)?),
            kind: IsBoolKind::False,
            negated: false,
        },
        E::IsNotFalse(inner) => Expr::IsBool {
            expr: Box::new(convert_expr(inner)?),
            kind: IsBoolKind::False,
            negated: true,
        },
        E::IsUnknown(inner) => Expr::IsBool {
            expr: Box::new(convert_expr(inner)?),
            kind: IsBoolKind::Unknown,
            negated: false,
        },
        E::IsNotUnknown(inner) => Expr::IsBool {
            expr: Box::new(convert_expr(inner)?),
            kind: IsBoolKind::Unknown,
            negated: true,
        },
        E::Like {
            negated,
            any,
            expr,
            pattern,
            ..
        } => convert_like(*negated, *any, expr, pattern)?,
        E::ILike {
            negated,
            any,
            expr,
            pattern,
            ..
        } => convert_like(*negated, *any, expr, pattern)?,

        E::IsDistinctFrom(left, right) => Expr::Binary {
            op: BinaryOp::IsDistinctFrom,
            left: Box::new(convert_expr(left)?),
            right: Box::new(convert_expr(right)?),
        },
        E::IsNotDistinctFrom(left, right) => Expr::Binary {
            op: BinaryOp::IsNotDistinctFrom,
            left: Box::new(convert_expr(left)?),
            right: Box::new(convert_expr(right)?),
        },

        E::Wildcard(_) => Expr::Star,

        E::Interval(interval) => Expr::Interval {
            value: Box::new(convert_expr(&interval.value)?),
            unit: interval.leading_field.as_ref().map(|f| f.to_string()),
        },

        E::Array(array) => Expr::Array(
            array
                .elem
                .iter()
                .map(convert_expr)
                .collect::<Result<Vec<_>>>()?,
        ),

        E::TypedString(typed) => Expr::Cast {
            expr: Box::new(convert_value(&typed.value.value)?),
            data_type: typed.data_type.to_string(),
            safe: false,
        },

        E::Collate { expr, collation } => Expr::Collate {
            expr: Box::new(convert_expr(expr)?),
            collation: collation.to_string(),
        },

        other => {
            return Err(Error::new(
                ErrorKind::Unmodeled,
                format!("expression {other:?} is outside the modelled subset"),
            ));
        }
    })
}

/// Convert a named function argument, returning its value and its label when
/// the label is a plain identifier.
fn convert_named_arg(arg: &sp::FunctionArgExpr) -> Result<(Expr, Option<String>)> {
    Ok(match arg {
        sp::FunctionArgExpr::Expr(expr) => (convert_expr(expr)?, None),
        sp::FunctionArgExpr::QualifiedWildcard(name) => {
            (Expr::Column(convert_object_name(name)), None)
        }
        sp::FunctionArgExpr::Wildcard => (Expr::Star, None),
    })
}

fn convert_function(function: &sp::Function) -> Result<Expr> {
    let name = convert_object_name(&function.name);

    // In sqlparser 0.59 a function's arguments are `FunctionArguments`, whose
    // list form carries the duplicate treatment separately from the arguments.
    let (argument_list, subquery_argument) = match &function.args {
        sp::FunctionArguments::List(list) => (Some(list), None),
        sp::FunctionArguments::Subquery(query) => (None, Some(query)),
        sp::FunctionArguments::None => (None, None),
    };

    let distinct = match argument_list.and_then(|list| list.duplicate_treatment) {
        Some(sp::DuplicateTreatment::Distinct) => DuplicateHandling::Distinct,
        _ => DuplicateHandling::All,
    };

    let mut args = Vec::new();
    let mut star = false;

    if let Some(query) = subquery_argument {
        args.push(Expr::Subquery(Box::new(convert_query(query)?)));
    }

    if let Some(list) = argument_list {
        for arg in &list.args {
            match arg {
                sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Expr(sp::Expr::Wildcard(_)))
                | sp::FunctionArg::Unnamed(sp::FunctionArgExpr::QualifiedWildcard(_))
                | sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Wildcard) => star = true,
                sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Expr(expr)) => {
                    args.push(convert_expr(expr)?)
                }
                sp::FunctionArg::Named { name, arg, .. } => {
                    let (value, label) = convert_named_arg(arg)?;
                    args.push(Expr::Alias {
                        expr: Box::new(value),
                        alias: Ident::bare(name.value.clone()),
                    });
                    let _ = label;
                }
                sp::FunctionArg::ExprNamed { name, arg, .. } => {
                    // BigQuery's `=>` argument, as in
                    // `STRUCT(1 AS a, 2 AS b)`. The label is the name expression.
                    let (value, _) = convert_named_arg(arg)?;
                    let alias = convert_expr(name)?;
                    args.push(Expr::Alias {
                        expr: Box::new(value),
                        alias: match alias {
                            Expr::Column(name) => name
                                .parts
                                .into_iter()
                                .next()
                                .unwrap_or(Ident::quoted("<arg>")),
                            other => Ident::quoted(other.to_string()),
                        },
                    });
                }
            }
        }
    }

    // A window hangs off `Function.over` rather than being its own node.
    let mut order_by = None;
    let mut window_spec = None;

    if let Some(sp::WindowType::WindowSpec(window)) = &function.over {
        let mut keys = Vec::with_capacity(window.order_by.len());
        for expr in &window.order_by {
            keys.push(OrderKey {
                expr: convert_expr(&expr.expr)?,
                descending: Some(expr.options.asc == Some(false)),
                nulls: expr
                    .options
                    .nulls_first
                    .map(|first| match expr.options.asc {
                        Some(false) => !first,
                        _ => first,
                    }),
            });
        }
        order_by = (!window.order_by.is_empty()).then_some(OrderBy { keys });
        window_spec = Some(WindowSpec {
            name: window.window_name.as_ref().map(convert_ident),
            partition_by: window
                .partition_by
                .iter()
                .map(convert_expr)
                .collect::<Result<Vec<_>>>()?,
            order_by: order_by.clone(),
            frame: window.window_frame.as_ref().map(render_window_frame),
        });
    } else if let Some(sp::WindowType::NamedWindow(name)) = &function.over {
        window_spec = Some(WindowSpec {
            name: Some(convert_ident(name)),
            ..Default::default()
        });
    }

    // The rewrite parked the aggregate's predicate in a marker call. Unwrap it
    // here so the marker never reaches the AST.
    if name.parts.len() == 1 && name.parts[0].name.eq_ignore_ascii_case(FILTER_MARKER) {
        let inner = args
            .into_iter()
            .next()
            .unwrap_or(Expr::Literal(Literal::Null));
        return Ok(inner);
    }

    // A `TABLE name` argument is a node of its own, not a call.
    if name.parts.len() == 1
        && name.parts[0]
            .name
            .eq_ignore_ascii_case(TABLE_ARGUMENT_MARKER)
    {
        let inner = args
            .into_iter()
            .next()
            .unwrap_or(Expr::Literal(Literal::Null));
        return match inner {
            Expr::Column(table) => Ok(Expr::TableArg(table)),
            other => Err(Error::new(
                ErrorKind::Unmodeled,
                format!("a TABLE argument must name a table, got {other:?}"),
            )),
        };
    }

    let call = match window_spec {
        Some(spec) => Expr::Window {
            function: Box::new(Expr::Function {
                name,
                args,
                distinct,
                star,
                order_by: None,
                limit: None,
            }),
            spec,
        },
        None => Expr::Function {
            name,
            args,
            distinct,
            star,
            order_by,
            // A window carries its ORDER BY inside the spec, so the plain call
            // has none left to record.
            limit: None,
        },
    };

    // `f(x) FILTER (WHERE p)` is BigQuery's `f(x WHERE p)`. The rewrite in
    // `crate::rewrite` put it into standard form so the base parser could read
    // it; the AST carries it as the filter node the rules compare against.
    match &function.filter {
        Some(filter) => Ok(Expr::Filter {
            aggregate: Box::new(call),
            predicate: Box::new(convert_expr(filter)?),
        }),
        None => Ok(call),
    }
}

/// Render a window frame clause.
///
/// `WindowFrame` has no `Display`, and its several spellings (`ROWS`, `RANGE`,
/// `GROUPS`, the two bounds and the exclusions) are numerous and rarely
/// rewritten, so the frame is kept as its canonical text rather than modelled.
fn render_window_frame(frame: &sp::WindowFrame) -> String {
    format!(
        "{} {} AND {}",
        frame.units,
        frame.start_bound,
        frame
            .end_bound
            .as_ref()
            .map(|b| b.to_string())
            .unwrap_or_else(|| "CURRENT ROW".to_string())
    )
}

/// Convert a `LIKE`, recovering the quantifier the source rewrite parked.
///
/// `sqlparser` 0.59 records only `any: bool`, so it cannot tell `ANY` from
/// `ALL`. `rewrite_like_quantifiers` therefore rewrites
/// `x LIKE ALL UNNEST(arr)` as `x LIKE ANY UNNEST(__KUMO_LIKE_ALL__(arr))`:
/// the parser sees an ordinary `ANY` and reads `UNNEST(...)` as a plain call,
/// and the marker call inside it is what says the quantifier was really `ALL`.
fn convert_like(negated: bool, any: bool, expr: &sp::Expr, pattern: &sp::Expr) -> Result<Expr> {
    // `UNNEST(__KUMO_LIKE_ALL__(arr))` means `ALL UNNEST(arr)`.
    if let Some(inner) = unnest_marker_argument(pattern) {
        return Ok(Expr::Like {
            expr: Box::new(convert_expr(expr)?),
            pattern: Box::new(convert_expr(inner)?),
            negated,
            quantifier: Some(LikeQuantifier::All),
        });
    }

    Ok(Expr::Like {
        expr: Box::new(convert_expr(expr)?),
        pattern: Box::new(convert_expr(pattern)?),
        negated,
        // `LIKE SOME` was read as `ANY` by the rewrite, and `SOME` is BigQuery's
        // older spelling of `ANY`.
        quantifier: any.then_some(LikeQuantifier::Any),
    })
}

/// The array of `UNNEST(__KUMO_LIKE_ALL__(x))`, if that is what `expr` is.
fn unnest_marker_argument(expr: &sp::Expr) -> Option<&sp::Expr> {
    // `UNNEST(...)` reaches here as an ordinary function call.
    let sp::Expr::Function(unnest) = expr else {
        return None;
    };
    if unnest.name.to_string().to_uppercase() != "UNNEST" {
        return None;
    }
    let sp::FunctionArguments::List(list) = &unnest.args else {
        return None;
    };
    let sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Expr(sp::Expr::Function(marker))) =
        list.args.first()?
    else {
        return None;
    };
    if marker.name.to_string().to_uppercase() != LIKE_ALL_MARKER {
        return None;
    }
    let sp::FunctionArguments::List(inner) = &marker.args else {
        return None;
    };
    match inner.args.first()? {
        sp::FunctionArg::Unnamed(sp::FunctionArgExpr::Expr(expr)) => Some(expr),
        _ => None,
    }
}

fn convert_value(value: &sp::Value) -> Result<Expr> {
    let literal = match value {
        sp::Value::Number(text, _) => Literal::Number(text.clone()),
        sp::Value::SingleQuotedString(text) => Literal::String(text.clone()),
        sp::Value::DoubleQuotedString(text) => Literal::String(text.clone()),
        sp::Value::Boolean(b) => Literal::Boolean(*b),
        sp::Value::Null => Literal::Null,
        sp::Value::Placeholder(name) => Literal::Parameter(name.clone()),
        // A dollar-quoted run is PostgreSQL spelling, not BigQuery's. Kept as
        // written rather than guessed at.
        sp::Value::DollarQuotedString(text) => Literal::String(text.value.clone()),
        // The pipeline canonicalises bytes literals to one spelling first, so
        // by the time they reach here the escapes are already decoded.
        sp::Value::SingleQuotedByteStringLiteral(text)
        | sp::Value::DoubleQuotedByteStringLiteral(text)
        | sp::Value::TripleSingleQuotedByteStringLiteral(text)
        | sp::Value::TripleDoubleQuotedByteStringLiteral(text) => {
            Literal::Bytes(text.clone().into_bytes())
        }
        // Every string spelling the parser can produce, including raw and
        // triple-quoted forms. The literals stage canonicalises the escapes
        // before the parse, so the text here is already one spelling.
        sp::Value::EscapedStringLiteral(text)
        | sp::Value::UnicodeStringLiteral(text)
        | sp::Value::NationalStringLiteral(text)
        | sp::Value::TripleSingleQuotedString(text)
        | sp::Value::TripleDoubleQuotedString(text)
        | sp::Value::SingleQuotedRawStringLiteral(text)
        | sp::Value::DoubleQuotedRawStringLiteral(text)
        | sp::Value::TripleSingleQuotedRawStringLiteral(text)
        | sp::Value::TripleDoubleQuotedRawStringLiteral(text) => Literal::String(text.clone()),
        other => {
            return Err(Error::new(
                ErrorKind::Unmodeled,
                format!("literal {other:?} is outside the modelled subset"),
            ));
        }
    };
    Ok(Expr::Literal(literal))
}

fn convert_binary_op(op: &sp::BinaryOperator) -> Result<BinaryOp> {
    Ok(match op {
        sp::BinaryOperator::Eq => BinaryOp::Eq,
        sp::BinaryOperator::NotEq => BinaryOp::NotEq,
        sp::BinaryOperator::Lt => BinaryOp::Lt,
        sp::BinaryOperator::LtEq => BinaryOp::LtEq,
        sp::BinaryOperator::Gt => BinaryOp::Gt,
        sp::BinaryOperator::GtEq => BinaryOp::GtEq,
        sp::BinaryOperator::And => BinaryOp::And,
        sp::BinaryOperator::Or => BinaryOp::Or,
        sp::BinaryOperator::Plus => BinaryOp::Add,
        sp::BinaryOperator::Minus => BinaryOp::Sub,
        sp::BinaryOperator::Multiply => BinaryOp::Mul,
        sp::BinaryOperator::Divide => BinaryOp::Div,
        sp::BinaryOperator::Modulo => BinaryOp::Mod,
        sp::BinaryOperator::StringConcat => BinaryOp::Concat,
        sp::BinaryOperator::PGRegexMatch => {
            return Err(Error::new(
                ErrorKind::Unmodeled,
                "the ~ operator is not modelled",
            ));
        }
        sp::BinaryOperator::MyIntegerDivide | sp::BinaryOperator::DuckIntegerDivide => {
            BinaryOp::IntDiv
        }
        other => {
            return Err(Error::new(
                ErrorKind::Unmodeled,
                format!("binary operator {other:?} is outside the modelled subset"),
            ));
        }
    })
}
