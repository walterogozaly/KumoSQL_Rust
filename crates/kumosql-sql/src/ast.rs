//! The BigQuery/GoogleSQL syntax tree, and its rendering back to SQL.
//!
//! Ported in shape from `sqlglot`'s expression model, which the Python original
//! is built on. Where `sqlglot` distinguishes node kinds by class, this uses
//! enums, which makes the set of supported constructs explicit and lets the
//! compiler reject an unhandled case rather than silently dropping it.
//!
//! # Scope
//!
//! This is the *proven subset*, not all of GoogleSQL. A construct outside it
//! is reported as [`ErrorKind::Unmodeled`] rather than approximated -- see
//! [`crate::error`] for why that distinction is load-bearing.
//!
//! `sqlglot` node kinds are named in each item's documentation so the two can be
//! read side by side.
//!
//! # Identifiers
//!
//! [`Ident`] stores an identifier's *decoded* name plus whether it was written
//! in backticks. Rendering re-quotes when the name is not a bare
//! `[A-Za-z_][A-Za-z0-9_]*`, because BigQuery folds unquoted identifiers to
//! lower case and treats backticked ones literally. Case is preserved on the
//! decoded name, so `A` and `a` stay distinguishable -- a distinction several
//! of the Python tests depend on.

use std::fmt;

/// A possibly backticked identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ident {
    /// The name as written, with backticks removed and escapes decoded.
    pub name: String,
    /// Whether it was written in backticks.
    pub quoted: bool,
}

impl Ident {
    /// An unquoted identifier, which BigQuery reads case-insensitively.
    pub fn bare(name: impl Into<String>) -> Self {
        Ident {
            name: name.into(),
            quoted: false,
        }
    }

    /// A quoted identifier, which BigQuery reads literally.
    pub fn quoted(name: impl Into<String>) -> Self {
        Ident {
            name: name.into(),
            quoted: true,
        }
    }

    /// The name folded to lower case, which is how BigQuery compares two
    /// unquoted identifiers.
    ///
    /// A quoted identifier is *not* folded: `` `A` `` and `` `a` `` are
    /// different columns.
    pub fn folded(&self) -> String {
        if self.quoted {
            self.name.clone()
        } else {
            self.name.to_lowercase()
        }
    }

    /// Whether the name can be written without backticks.
    fn is_bare(&self) -> bool {
        !self.quoted
            && !self.name.is_empty()
            && !self.name.chars().next().is_some_and(|c| c.is_ascii_digit())
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_bare() {
            f.write_str(&self.name)
        } else {
            write!(f, "`{}`", self.name.replace('`', "\\`"))
        }
    }
}

/// A dotted table or column path, such as `project.dataset.table`.
///
/// Kept as parts rather than a string because BigQuery addresses each part
/// separately, and `project.dataset` is a valid table name as well as a prefix
/// of a longer one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ObjectName {
    /// The dot-separated parts.
    pub parts: Vec<Ident>,
}

impl ObjectName {
    /// A single-part name.
    pub fn single(name: impl Into<String>) -> Self {
        ObjectName {
            parts: vec![Ident::bare(name)],
        }
    }

    /// A dotted name.
    pub fn new(parts: impl IntoIterator<Item = Ident>) -> Self {
        ObjectName {
            parts: parts.into_iter().collect(),
        }
    }

    /// The name as BigQuery compares it: every part folded to lower case.
    ///
    /// Used by `same_table` in the Python original, which treats a table
    /// spelled `A` on one side and `a` on the other as one table.
    pub fn folded(&self) -> Vec<String> {
        self.parts.iter().map(Ident::folded).collect()
    }
}

impl fmt::Display for ObjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered: Vec<String> = self.parts.iter().map(|p| p.to_string()).collect();
        f.write_str(&rendered.join("."))
    }
}

/// A literal value, as written.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    /// A number, kept as written so BigQuery's exact spelling survives.
    ///
    /// The Python original is careful about this too: `1e400` overflowing and
    /// `2**53` are both traps its numeric tests cover.
    Number(String),
    /// A single-quoted string, already canonicalised by
    /// [`crate::literals::canonical_literals`].
    String(String),
    /// A bytes literal.
    Bytes(Vec<u8>),
    /// `TRUE` / `FALSE`.
    Boolean(bool),
    /// `NULL`.
    Null,
    /// A named or session parameter such as `@name`.
    Parameter(String),
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::Number(text) => f.write_str(text),
            Literal::String(text) => {
                write!(f, "'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
            }
            Literal::Bytes(bytes) => {
                let text: String = bytes
                    .iter()
                    .map(|b| {
                        if (32..127).contains(b) && *b != 39 && *b != 92 {
                            (*b as char).to_string()
                        } else {
                            format!("\\x{b:02X}")
                        }
                    })
                    .collect();
                write!(f, "b'{text}'")
            }
            Literal::Boolean(value) => f.write_str(if *value { "TRUE" } else { "FALSE" }),
            Literal::Null => f.write_str("NULL"),
            Literal::Parameter(name) => write!(f, "@{name}"),
        }
    }
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `=`
    Eq,
    /// `!=` and `<>`
    NotEq,
    /// `<`
    Lt,
    /// `<=`
    LtEq,
    /// `>`
    Gt,
    /// `>=`
    GtEq,
    /// `AND`
    And,
    /// `OR`
    Or,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `DIV`
    IntDiv,
    /// `%` and `MOD`
    Mod,
    /// `||`
    Concat,
    /// `IS DISTINCT FROM`
    IsDistinctFrom,
    /// `IS NOT DISTINCT FROM`
    IsNotDistinctFrom,
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BinaryOp::Eq => "=",
            BinaryOp::NotEq => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::LtEq => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::GtEq => ">=",
            BinaryOp::And => "AND",
            BinaryOp::Or => "OR",
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::IntDiv => "DIV",
            BinaryOp::Mod => "%",
            BinaryOp::Concat => "||",
            BinaryOp::IsDistinctFrom => "IS DISTINCT FROM",
            BinaryOp::IsNotDistinctFrom => "IS NOT DISTINCT FROM",
        })
    }
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    /// `NOT`
    Not,
    /// `-`
    Minus,
    /// `+`
    Plus,
    /// `~`
    BitNot,
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            UnaryOp::Not => "NOT ",
            UnaryOp::Minus => "-",
            UnaryOp::Plus => "+",
            UnaryOp::BitNot => "~",
        })
    }
}

/// A join kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JoinKind {
    /// `INNER JOIN`, and a bare `JOIN`.
    Inner,
    /// `LEFT JOIN` / `LEFT OUTER JOIN`
    Left,
    /// `RIGHT JOIN` / `RIGHT OUTER JOIN`
    Right,
    /// `FULL JOIN` / `FULL OUTER JOIN`
    Full,
    /// `CROSS JOIN`
    Cross,
}

impl fmt::Display for JoinKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            JoinKind::Inner => "INNER JOIN",
            JoinKind::Left => "LEFT JOIN",
            JoinKind::Right => "RIGHT JOIN",
            JoinKind::Full => "FULL JOIN",
            JoinKind::Cross => "CROSS JOIN",
        })
    }
}

/// A set operation kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SetOp {
    /// `UNION`
    Union,
    /// `INTERSECT`
    Intersect,
    /// `EXCEPT`
    Except,
}

/// Whether a `SELECT` or set operation keeps duplicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DuplicateHandling {
    /// `DISTINCT`, or an aggregate that de-duplicates.
    Distinct,
    /// `ALL`: `UNION ALL`, `ARRAY_AGG(DISTINCT ...)`, and the default.
    #[default]
    All,
}

/// A modifier on a projected `*`.
///
/// `Eq` is deliberately not derived: [`Expr`] carries floating-point-ish
/// literal text and is only `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub enum StarModifier {
    /// `SELECT * EXCEPT (a, b)`
    Except(Vec<Ident>),
    /// `SELECT * REPLACE (expr AS a)`
    Replace(Vec<Expr>),
    /// `SELECT * EXCEPT (a) REPLACE (expr AS b)` -- both, in that order.
    Both {
        /// Columns to drop.
        except: Vec<Ident>,
        /// Columns to replace.
        replace: Vec<Expr>,
    },
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A bare column or alias, such as `t.col` (`exp.Column`).
    Column(ObjectName),
    /// A constant.
    Literal(Literal),
    /// `*` (`exp.Star`).
    Star,
    /// `* EXCEPT (...)` / `* REPLACE (...)` (`exp.Star` with a modifier).
    ModifiedStar(StarModifier),
    /// `a + b`, `a = b`, `a AND b` (`exp.Binary`).
    Binary {
        /// The operator.
        op: BinaryOp,
        /// Left operand.
        left: Box<Expr>,
        /// Right operand.
        right: Box<Expr>,
    },
    /// `NOT a` (`exp.Not`), `-a` (`exp.Neg`).
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        expr: Box<Expr>,
    },
    /// `a AND b AND c`, flattened (`exp.And` chains).
    ///
    /// Flattening rather than nesting matches how the provers read a
    /// conjunction as a set of conjuncts.
    And(Vec<Expr>),
    /// `a OR b OR c`, flattened.
    Or(Vec<Expr>),
    /// A function call (`exp.Anonymous`, `exp.Func`).
    Function {
        /// The written name, preserved: `SAFE_CAST` is not `CAST`.
        name: ObjectName,
        /// Positional arguments.
        args: Vec<Expr>,
        /// Whether the call de-duplicates.
        distinct: DuplicateHandling,
        /// `COUNT(*)` as the argument, which is not the same as an expression.
        star: bool,
        /// `ORDER BY` inside the call.
        order_by: Option<OrderBy>,
        /// `LIMIT` inside the call.
        limit: Option<Box<Expr>>,
    },
    /// `FILTER (WHERE p)` -- the standard form of BigQuery's `COUNT(x WHERE p)`.
    Filter {
        /// The aggregate the filter applies to.
        aggregate: Box<Expr>,
        /// The predicate.
        predicate: Box<Expr>,
    },
    /// A `CASE` expression (`exp.Case`).
    Case {
        /// The operand of a simple `CASE`.
        operand: Option<Box<Expr>>,
        /// `(condition, result)` pairs.
        whens: Vec<(Expr, Expr)>,
        /// `ELSE`.
        otherwise: Option<Box<Expr>>,
    },
    /// `CAST(x AS t)` (`exp.Cast`).
    Cast {
        /// The value being cast.
        expr: Box<Expr>,
        /// The target type as written.
        data_type: String,
        /// `SAFE_CAST` is the same shape with a different name; kept distinct
        /// because it is a different function.
        safe: bool,
    },
    /// A subquery used as a value (`exp.Subquery`).
    Subquery(Box<Query>),
    /// `(SELECT ...)` in an `EXISTS` predicate.
    Exists {
        /// The subquery.
        query: Box<Query>,
        /// `NOT EXISTS`.
        negated: bool,
    },
    /// An alias (`exp.Alias`).
    Alias {
        /// The expression being named.
        expr: Box<Expr>,
        /// The name.
        alias: Ident,
    },
    /// `x IN (...)` (`exp.In`).
    In {
        /// The value tested.
        expr: Box<Expr>,
        /// The subquery side, when written as a subquery.
        query: Option<Box<Query>>,
        /// An explicit value list, when written as one.
        list: Option<Vec<Expr>>,
        /// `NOT IN`.
        negated: bool,
    },
    /// `x BETWEEN a AND b` (`exp.Between`).
    Between {
        /// The value tested.
        expr: Box<Expr>,
        /// Lower bound.
        low: Box<Expr>,
        /// Upper bound.
        high: Box<Expr>,
        /// `NOT BETWEEN`.
        negated: bool,
    },
    /// `x IS NULL`, `x IS NOT NULL` (`exp.Is`).
    IsNull {
        /// The value tested.
        expr: Box<Expr>,
        /// `IS NOT NULL`.
        negated: bool,
    },
    /// `x IS TRUE` / `IS FALSE` / `IS UNKNOWN`.
    IsBool {
        /// The value tested.
        expr: Box<Expr>,
        /// Which value is tested for.
        kind: IsBoolKind,
        /// `IS NOT TRUE`.
        negated: bool,
    },
    /// `x LIKE p` (`exp.Like`).
    Like {
        /// The value tested.
        expr: Box<Expr>,
        /// The pattern.
        pattern: Box<Expr>,
        /// `NOT LIKE`.
        negated: bool,
        /// `ANY` or `ALL`, for the quantifier over an array: `x LIKE ANY y`.
        ///
        /// `None` means the bare `LIKE`. `SOME` is BigQuery's older spelling
        /// of `ANY` and is read as `ANY`.
        quantifier: Option<LikeQuantifier>,
    },
    /// A `WITH(expr AS name, ...)` expression, BigQuery's named-expression
    /// form.
    ///
    /// Distinct from a `WITH` *clause*: this binds a name to a value inside one
    /// expression. `WITH(a AS 1, a + 1)` is the whole expression.
    WithExpr {
        /// The `(name AS value)` bindings, in order.
        variables: Vec<(Ident, Expr)>,
        /// The expression that uses them.
        body: Box<Expr>,
    },
    /// A `TABLE name` argument to a table-valued function.
    ///
    /// `FROM dataset.fn(TABLE dataset.input, option => value)` passes a whole
    /// table to the function. A generic parser stops at `TABLE`, so this is
    /// marked in the source and resolved here.
    TableArg(ObjectName),
    /// `x IS [NOT] TRUE|FALSE|UNKNOWN` is separate from `IsNull` because BigQuery
    /// reads them differently and the provers must not conflate them.
    Collate {
        /// The value.
        expr: Box<Expr>,
        /// The collation name.
        collation: String,
    },
    /// An interval, date or similar typed literal (`exp.Interval`, `exp.Cast`).
    Interval {
        /// The value.
        value: Box<Expr>,
        /// The unit, as written.
        unit: Option<String>,
    },
    /// An array constructor: `[1, 2]` (`exp.Array`).
    Array(Vec<Expr>),
    /// `STRUCT(1 AS a, 2 AS b)` (`exp.Struct`).
    Struct {
        /// The fields, in order.
        fields: Vec<Expr>,
    },
    /// `expr COLLATE` is separate from `Collate`; this is the keyword form.
    /// A window function: `f(...) OVER (...)` (`exp.Window`).
    Window {
        /// The function being windowed.
        function: Box<Expr>,
        /// The partitioning, ordering and frame.
        spec: WindowSpec,
    },
    /// A call to an unknown or vendor function kept as written.
    ///
    /// This is the escape hatch the Python original uses for `GRAPH_TABLE`,
    /// whose arguments are a language of their own.
    Verbatim {
        /// The name as written.
        name: ObjectName,
        /// The argument text, kept verbatim.
        text: String,
    },
}

/// The quantifier on a quantified `LIKE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LikeQuantifier {
    /// `x LIKE ANY y` -- true when the pattern matches at least one element.
    Any,
    /// `x LIKE ALL y` -- true when the pattern matches every element.
    ///
    /// On empty input this is true and `ANY` is false, which is exactly the
    /// difference the provers must not blur.
    All,
}

impl fmt::Display for LikeQuantifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LikeQuantifier::Any => "ANY",
            LikeQuantifier::All => "ALL",
        })
    }
}

/// The value an `IS` test looks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IsBoolKind {
    /// `IS TRUE`
    True,
    /// `IS FALSE`
    False,
    /// `IS UNKNOWN`
    Unknown,
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Column(name) => write!(f, "{name}"),
            Expr::Literal(literal) => write!(f, "{literal}"),
            Expr::Star => f.write_str("*"),
            Expr::ModifiedStar(StarModifier::Except(cols)) => {
                write!(f, "* EXCEPT ({})", join_idents(cols))
            }
            Expr::ModifiedStar(StarModifier::Replace(exprs)) => {
                write!(f, "* REPLACE ({})", join_commas(exprs))
            }
            Expr::ModifiedStar(StarModifier::Both { except, replace }) => write!(
                f,
                "* EXCEPT ({}) REPLACE ({})",
                join_idents(except),
                join_commas(replace)
            ),
            Expr::Binary { op, left, right } => write!(f, "({left} {op} {right})"),
            Expr::Unary { op, expr } => write!(f, "{op}({expr})"),
            Expr::And(items) => write!(f, "({})", join_and(items)),
            Expr::Or(items) => write!(f, "({})", join_or(items)),
            Expr::Function {
                name,
                args,
                distinct,
                star,
                order_by,
                limit,
            } => {
                write!(f, "{name}(")?;
                if *star {
                    f.write_str("*")?;
                } else {
                    if matches!(distinct, DuplicateHandling::Distinct) {
                        f.write_str("DISTINCT ")?;
                    }
                    f.write_str(&join_commas(args))?;
                }
                f.write_str(")")?;
                if let Some(order_by) = order_by {
                    write!(f, " {order_by}")?;
                }
                if let Some(limit) = limit {
                    write!(f, " LIMIT {limit}")?;
                }
                Ok(())
            }
            Expr::Filter {
                aggregate,
                predicate,
            } => {
                // BigQuery spells an aggregate's filter inside the parentheses.
                // Rendering the standard `FILTER (WHERE ...)` instead would be
                // valid SQL but not BigQuery's spelling, and this node exists
                // precisely because BigQuery's own form needed a rewrite to
                // parse in the first place.
                match aggregate.as_ref() {
                    Expr::Function {
                        name,
                        args,
                        distinct,
                        star: false,
                        ..
                    } => {
                        if matches!(distinct, DuplicateHandling::Distinct) {
                            f.write_str("DISTINCT ")?;
                        }
                        write!(f, "{name}({} WHERE {predicate})", join_commas(args))
                    }
                    other => write!(f, "{other} FILTER (WHERE {predicate})"),
                }
            }
            Expr::Case {
                operand,
                whens,
                otherwise,
            } => {
                f.write_str("CASE")?;
                if let Some(operand) = operand {
                    write!(f, " {operand}")?;
                }
                for (when, then) in whens {
                    write!(f, " WHEN {when} THEN {then}")?;
                }
                if let Some(otherwise) = otherwise {
                    write!(f, " ELSE {otherwise}")?;
                }
                f.write_str(" END")
            }
            Expr::Cast {
                expr,
                data_type,
                safe,
            } => write!(
                f,
                "{}({} AS {})",
                if *safe { "SAFE_CAST" } else { "CAST" },
                expr,
                data_type
            ),
            Expr::Subquery(query) => write!(f, "({})", RenderQuery(query)),
            Expr::Exists { query, negated } => write!(
                f,
                "{}({})",
                if *negated { "NOT EXISTS" } else { "EXISTS" },
                RenderQuery(query)
            ),
            Expr::Alias { expr, alias } => write!(f, "{expr} AS {alias}"),
            Expr::In {
                expr,
                query,
                list,
                negated,
            } => {
                write!(f, "{expr} {}IN ", if *negated { "NOT " } else { "" })?;
                match (query, list) {
                    (Some(query), _) => write!(f, "({})", RenderQuery(query)),
                    (None, Some(list)) => write!(f, "({})", join_commas(list)),
                    (None, None) => f.write_str("()"),
                }
            }
            Expr::Between {
                expr,
                low,
                high,
                negated,
            } => write!(
                f,
                "{expr} {}BETWEEN {low} AND {high}",
                if *negated { "NOT " } else { "" }
            ),
            Expr::IsNull { expr, negated } => {
                write!(f, "{expr} IS {}NULL", if *negated { "NOT " } else { "" })
            }
            Expr::IsBool {
                expr,
                kind,
                negated,
            } => {
                let word = match kind {
                    IsBoolKind::True => "TRUE",
                    IsBoolKind::False => "FALSE",
                    IsBoolKind::Unknown => "UNKNOWN",
                };
                write!(f, "{expr} IS {}{word}", if *negated { "NOT " } else { "" })
            }
            Expr::Like {
                expr,
                pattern,
                negated,
                quantifier,
            } => {
                write!(f, "{expr} {}LIKE ", if *negated { "NOT " } else { "" })?;
                if let Some(quantifier) = quantifier {
                    write!(f, "{quantifier} ")?;
                }
                write!(f, "{pattern}")
            }
            Expr::WithExpr { variables, body } => {
                let bindings: Vec<String> = variables
                    .iter()
                    .map(|(name, value)| format!("{name} AS {value}"))
                    .collect();
                write!(f, "WITH({})", bindings.join(", ")).and_then(|_| write!(f, ", {body}"))
            }
            Expr::TableArg(name) => write!(f, "TABLE {name}"),
            Expr::Collate { expr, collation } => write!(f, "{expr} COLLATE {collation}"),
            Expr::Interval { value, unit } => match unit {
                Some(unit) => write!(f, "INTERVAL {value} {unit}"),
                None => write!(f, "{value}"),
            },
            Expr::Array(items) => write!(f, "[{}]", join_commas(items)),
            Expr::Struct { fields } => write!(f, "STRUCT({})", join_commas(fields)),
            Expr::Window { function, spec } => write!(f, "{function} OVER {spec}"),
            Expr::Verbatim { name, text } => write!(f, "{name}({text})"),
        }
    }
}

/// `a`, `b`, `c`
fn join_commas(items: &[Expr]) -> String {
    items
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `a, b, c`
fn join_idents(items: &[Ident]) -> String {
    items
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `a AND b AND c`
fn join_and(items: &[Expr]) -> String {
    items
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// `a OR b OR c`
fn join_or(items: &[Expr]) -> String {
    items
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// An `ORDER BY` clause.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderBy {
    /// The keys, each optionally with a direction.
    pub keys: Vec<OrderKey>,
}

/// One `ORDER BY` key.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderKey {
    /// The expression being ordered by.
    pub expr: Expr,
    /// `ASC` or `DESC`. `None` means as written, which is `ASC`.
    pub descending: Option<bool>,
    /// `NULLS FIRST` / `NULLS LAST`, when written.
    pub nulls: Option<bool>,
}

impl fmt::Display for OrderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.expr)?;
        if let Some(descending) = self.descending {
            f.write_str(if descending { " DESC" } else { " ASC" })?;
        }
        if let Some(nulls_first) = self.nulls {
            f.write_str(if nulls_first {
                " NULLS FIRST"
            } else {
                " NULLS LAST"
            })?;
        }
        Ok(())
    }
}

impl fmt::Display for OrderBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys: Vec<String> = self.keys.iter().map(|k| k.to_string()).collect();
        write!(f, "ORDER BY {}", keys.join(", "))
    }
}

/// A window specification: `PARTITION BY ... ORDER BY ... <frame>`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowSpec {
    /// A named window this one is copied from.
    pub name: Option<Ident>,
    /// `PARTITION BY`.
    pub partition_by: Vec<Expr>,
    /// `ORDER BY`.
    pub order_by: Option<OrderBy>,
    /// The frame clause, kept as written.
    ///
    /// Kept verbatim because the several spellings (`ROWS`, `RANGE`, `GROUPS`,
    /// their bounds and exclusions) are numerous and rarely rewritten.
    pub frame: Option<String>,
}

impl fmt::Display for WindowSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = &self.name {
            return write!(f, "{name}");
        }
        if !self.partition_by.is_empty() {
            write!(f, "PARTITION BY {}", join_commas(&self.partition_by))?;
        }
        if let Some(order_by) = &self.order_by {
            if !self.partition_by.is_empty() {
                f.write_str(" ")?;
            }
            write!(f, "{order_by}")?;
        }
        if let Some(frame) = &self.frame {
            f.write_str(" ")?;
            f.write_str(frame)?;
        }
        Ok(())
    }
}

/// One table in a `FROM` clause.
#[derive(Debug, Clone, PartialEq)]
pub enum TableFactor {
    /// A named table.
    Table {
        /// The table name.
        name: ObjectName,
        /// Its alias.
        alias: Option<Ident>,
        /// `FOR SYSTEM_TIME AS OF ...` and similar.
        options: Option<String>,
    },
    /// A derived table: `(SELECT ...) AS t`.
    Subquery {
        /// The query.
        query: Box<Query>,
        /// The alias, which BigQuery requires.
        alias: Option<Ident>,
        /// The column alias list, when written.
        columns: Option<Vec<Ident>>,
    },
    /// A table-valued function: `dataset.fn(arg, ...)`.
    ///
    /// Needed because `dataset.fn(TABLE dataset.input, option => value)` passes
    /// a whole table to the function, which is the shape the `TABLE` marker in
    /// [`crate::rewrites`] exists for.
    TableFunction {
        /// The function name, usually dotted.
        name: ObjectName,
        /// Its arguments, in order.
        args: Vec<Expr>,
        /// Its alias.
        alias: Option<Ident>,
    },
    /// `UNNEST(expr)`, with or without `WITH OFFSET`.
    Unnest {
        /// The array expression.
        array: Box<Expr>,
        /// `WITH OFFSET [AS name]`.
        offset: Option<Option<Ident>>,
        /// The alias.
        alias: Option<Ident>,
    },
    /// `t JOIN u ON ...`.
    Join {
        /// The left input.
        left: Box<TableFactor>,
        /// The join kind.
        kind: JoinKind,
        /// The right input.
        right: Box<TableFactor>,
        /// `ON` condition.
        on: Option<Expr>,
        /// `USING (a, b)`.
        using: Option<Vec<Ident>>,
    },
}

impl fmt::Display for TableFactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TableFactor::Table {
                name,
                alias,
                options,
            } => {
                write!(f, "{name}")?;
                if let Some(options) = options {
                    write!(f, " {options}")?;
                }
                if let Some(alias) = alias {
                    write!(f, " AS {alias}")?;
                }
                Ok(())
            }
            TableFactor::Subquery {
                query,
                alias,
                columns,
            } => {
                write!(f, "({})", RenderQuery(query))?;
                if let Some(columns) = columns {
                    write!(f, " ({})", join_idents(columns))?;
                }
                if let Some(alias) = alias {
                    write!(f, " AS {alias}")?;
                }
                Ok(())
            }
            TableFactor::TableFunction { name, args, alias } => {
                write!(f, "{name}({})", join_commas(args))?;
                if let Some(alias) = alias {
                    write!(f, " AS {alias}")?;
                }
                Ok(())
            }
            TableFactor::Unnest {
                array,
                offset,
                alias,
            } => {
                write!(f, "UNNEST({array})")?;
                if let Some(offset) = offset {
                    f.write_str(" WITH OFFSET")?;
                    if let Some(name) = offset {
                        write!(f, " AS {name}")?;
                    }
                }
                if let Some(alias) = alias {
                    write!(f, " AS {alias}")?;
                }
                Ok(())
            }
            TableFactor::Join {
                left,
                kind,
                right,
                on,
                using,
            } => {
                write!(f, "{left} {kind} {right}")?;
                if let Some(on) = on {
                    write!(f, " ON {on}")?;
                }
                if let Some(using) = using {
                    write!(f, " USING ({})", join_idents(using))?;
                }
                Ok(())
            }
        }
    }
}

/// A `GROUP BY` clause.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GroupBy {
    /// `GROUP BY ALL`, which groups by every projected non-aggregate.
    pub all: bool,
    /// The keys, as written (positions included: `GROUP BY 1`).
    pub keys: Vec<Expr>,
    /// `ROLLUP(...)`, `CUBE(...)` or `GROUPING SETS(...)`.
    pub grouping: Option<Grouping>,
}

/// A `ROLLUP`, `CUBE` or `GROUPING SETS` clause.
#[derive(Debug, Clone, PartialEq)]
pub enum Grouping {
    /// `ROLLUP(a, b)`
    Rollup(Vec<Expr>),
    /// `CUBE(a, b)`
    Cube(Vec<Expr>),
    /// `GROUPING SETS((a), (b), ())`
    Sets(Vec<Vec<Expr>>),
}

impl fmt::Display for GroupBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.all {
            return f.write_str("GROUP BY ALL");
        }
        match &self.grouping {
            Some(Grouping::Rollup(items)) => {
                write!(f, "ROLLUP({})", join_commas(items))
            }
            Some(Grouping::Cube(items)) => write!(f, "CUBE({})", join_commas(items)),
            Some(Grouping::Sets(sets)) => {
                let rendered: Vec<String> = sets
                    .iter()
                    .map(|s| format!("({})", join_commas(s)))
                    .collect();
                write!(f, "GROUPING SETS({})", rendered.join(", "))
            }
            None => write!(f, "GROUP BY {}", join_commas(&self.keys)),
        }
    }
}

/// A `SELECT` body.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Select {
    /// `SELECT`, or `SELECT AS STRUCT` / `SELECT AS VALUE`.
    pub as_kind: SelectAs,
    /// `DISTINCT` or `ALL`.
    pub duplicate_handling: DuplicateHandling,
    /// The projected expressions, before aliases are stripped into
    /// [`Expr::Alias`].
    pub projections: Vec<Expr>,
    /// The `FROM` clause.
    pub from: Option<TableFactor>,
    /// `WHERE`.
    pub selection: Option<Expr>,
    /// `GROUP BY`.
    pub group_by: Option<GroupBy>,
    /// `HAVING`.
    pub having: Option<Expr>,
    /// `QUALIFY`.
    pub qualify: Option<Expr>,
    /// `WINDOW` definitions, kept as written.
    pub windows: Option<String>,
    /// `ORDER BY`.
    pub order_by: Option<OrderBy>,
    /// `LIMIT`.
    pub limit: Option<Expr>,
    /// `OFFSET`.
    pub offset: Option<Expr>,
}

/// Whether a `SELECT` produces a struct or a bare value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SelectAs {
    /// A plain `SELECT`.
    #[default]
    None,
    /// `SELECT AS STRUCT`.
    Struct,
    /// `SELECT AS VALUE`.
    Value,
}

impl fmt::Display for Select {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SELECT")?;
        match self.as_kind {
            SelectAs::None => {}
            SelectAs::Struct => f.write_str(" AS STRUCT")?,
            SelectAs::Value => f.write_str(" AS VALUE")?,
        }
        match self.duplicate_handling {
            DuplicateHandling::Distinct => f.write_str(" DISTINCT")?,
            DuplicateHandling::All => {}
        }
        write!(f, " {}", join_commas(&self.projections))?;
        if let Some(from) = &self.from {
            write!(f, " FROM {from}")?;
        }
        if let Some(selection) = &self.selection {
            write!(f, " WHERE {selection}")?;
        }
        if let Some(group_by) = &self.group_by {
            write!(f, " {group_by}")?;
        }
        if let Some(having) = &self.having {
            write!(f, " HAVING {having}")?;
        }
        if let Some(qualify) = &self.qualify {
            write!(f, " QUALIFY {qualify}")?;
        }
        if let Some(windows) = &self.windows {
            write!(f, " WINDOW {windows}")?;
        }
        if let Some(order_by) = &self.order_by {
            write!(f, " {order_by}")?;
        }
        if let Some(limit) = &self.limit {
            write!(f, " LIMIT {limit}")?;
        }
        if let Some(offset) = &self.offset {
            write!(f, " OFFSET {offset}")?;
        }
        Ok(())
    }
}

/// A common table expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Cte {
    /// The name it is bound to.
    pub name: Ident,
    /// The optional column alias list.
    pub columns: Option<Vec<Ident>>,
    /// The query.
    pub query: Box<Query>,
}

impl fmt::Display for Cte {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(columns) = &self.columns {
            write!(f, " ({})", join_idents(columns))?;
        }
        write!(f, " AS ({})", RenderQuery(&self.query))
    }
}

/// A query.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    /// A plain `SELECT`, possibly with a `WITH` clause in front.
    Select {
        /// The `WITH` clause, when present.
        with: Option<With>,
        /// The body.
        body: Box<Select>,
    },
    /// `UNION` / `INTERSECT` / `EXCEPT`, possibly with a `WITH` clause.
    SetOperation {
        /// The `WITH` clause, when present.
        with: Option<With>,
        /// Which operation.
        op: SetOp,
        /// Whether duplicates are kept.
        duplicate_handling: DuplicateHandling,
        /// Left operand.
        left: Box<Query>,
        /// Right operand.
        right: Box<Query>,
        /// A trailing `ORDER BY`, which binds to the whole set operation.
        order_by: Option<OrderBy>,
        /// A trailing `LIMIT`.
        limit: Option<Expr>,
    },
    /// `VALUES (...), (...)`.
    Values {
        /// The `WITH` clause, when present.
        with: Option<With>,
        /// The rows.
        rows: Vec<Vec<Expr>>,
    },
}

impl Query {
    /// The `WITH` clause, if this query has one.
    pub fn with_clause(&self) -> Option<&With> {
        match self {
            Query::Select { with, .. }
            | Query::SetOperation { with, .. }
            | Query::Values { with, .. } => with.as_ref(),
        }
    }

    /// The `SELECT` body, when this query is a plain select.
    pub fn as_select(&self) -> Option<&Select> {
        match self {
            Query::Select { body, .. } => Some(body),
            _ => None,
        }
    }

    /// The `SELECT` body, mutably, when this query is a plain select.
    ///
    /// A set operation and `VALUES` have no single body to hand back, so they
    /// return `None` rather than a fabricated one.
    pub fn as_select_mut(&mut self) -> Option<&mut Select> {
        match self {
            Query::Select { body, .. } => Some(body),
            _ => None,
        }
    }

    /// Whether this query reads no tables at all: a `SELECT` of literals, or
    /// `VALUES` with no `WITH`.
    ///
    /// The provers use this to decline pairs whose reading depends on data they
    /// cannot see.
    pub fn is_constant(&self) -> bool {
        match self {
            Query::Values { with, .. } => with.is_none(),
            Query::Select { with, body } => {
                with.is_none() && body.from.is_none() && body.selection.is_none()
            }
            Query::SetOperation { with, .. } => with.is_none(),
        }
    }
}

/// A `WITH` clause.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct With {
    /// `WITH RECURSIVE`.
    pub recursive: bool,
    /// The definitions, in order.
    pub ctes: Vec<Cte>,
}

impl fmt::Display for With {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WITH ")?;
        if self.recursive {
            f.write_str("RECURSIVE ")?;
        }
        let rendered: Vec<String> = self.ctes.iter().map(|c| c.to_string()).collect();
        f.write_str(&rendered.join(", "))
    }
}

/// Renders a query with its `WITH` clause, which [`Query`]'s own `Display`
/// cannot do because the clause lives in each variant.
pub struct RenderQuery<'a>(pub &'a Query);

impl fmt::Display for RenderQuery<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let query = self.0;
        if let Some(with) = query.with_clause() {
            write!(f, "{with} ")?;
        }
        match query {
            Query::Select { body, .. } => write!(f, "{body}"),
            Query::Values { rows, .. } => {
                let rendered: Vec<String> = rows
                    .iter()
                    .map(|row| format!("({})", join_commas(row)))
                    .collect();
                write!(f, "VALUES {}", rendered.join(", "))
            }
            Query::SetOperation {
                op,
                duplicate_handling,
                left,
                right,
                order_by,
                limit,
                ..
            } => {
                let keyword = match op {
                    SetOp::Union => "UNION",
                    SetOp::Intersect => "INTERSECT",
                    SetOp::Except => "EXCEPT",
                };
                let all = match duplicate_handling {
                    DuplicateHandling::All => " ALL",
                    DuplicateHandling::Distinct => " DISTINCT",
                };
                write!(f, "{} ", RenderQuery(left))?;
                // The right operand carries no `WITH`: the clause already
                // applies to the whole set operation.
                write!(f, "{keyword}{all} {}", RenderRight(right))?;
                if let Some(order_by) = order_by {
                    write!(f, " {order_by}")?;
                }
                if let Some(limit) = limit {
                    write!(f, " LIMIT {limit}")?;
                }
                Ok(())
            }
        }
    }
}

/// Renders a set operation's right operand without its own `WITH`, which would
/// be a syntax error there.
struct RenderRight<'a>(&'a Query);

impl fmt::Display for RenderRight<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Query::SetOperation {
                op,
                duplicate_handling,
                left,
                right,
                order_by,
                limit,
                ..
            } => {
                let keyword = match op {
                    SetOp::Union => "UNION",
                    SetOp::Intersect => "INTERSECT",
                    SetOp::Except => "EXCEPT",
                };
                let all = match duplicate_handling {
                    DuplicateHandling::All => " ALL",
                    DuplicateHandling::Distinct => " DISTINCT",
                };
                write!(
                    f,
                    "{} {keyword}{all} {}",
                    RenderQuery(left),
                    RenderRight(right)
                )?;
                if let Some(order_by) = order_by {
                    write!(f, " {order_by}")?;
                }
                if let Some(limit) = limit {
                    write!(f, " LIMIT {limit}")?;
                }
                Ok(())
            }
            other => write!(f, "{}", RenderQuery(other)),
        }
    }
}

/// A statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    /// A bare query: `SELECT`, `WITH ...`, `UNION ...`, `VALUES ...`.
    Query(Query),
    /// `CREATE TABLE ... AS SELECT ...`
    CreateTableAs {
        /// The table being created.
        name: ObjectName,
        /// The query producing it.
        query: Box<Query>,
        /// `OR REPLACE`.
        replace: bool,
        /// `IF NOT EXISTS`.
        if_not_exists: bool,
    },
    /// `CREATE [OR REPLACE] VIEW ... AS SELECT ...`
    CreateViewAs {
        /// The view being created.
        name: ObjectName,
        /// The query defining it.
        query: Box<Query>,
        /// `OR REPLACE`.
        replace: bool,
        /// `MATERIALIZED`.
        materialized: bool,
    },
    /// `INSERT INTO ... SELECT ...`
    Insert {
        /// The target table.
        table: ObjectName,
        /// The target columns.
        columns: Option<Vec<Ident>>,
        /// The query supplying rows.
        query: Option<Box<Query>>,
    },
    /// `UPDATE ... SET ... WHERE ...`
    Update {
        /// The target table.
        table: ObjectName,
        /// The assignments, as written.
        assignments: Option<String>,
        /// The `WHERE` clause, as written.
        selection: Option<Expr>,
    },
    /// `DELETE FROM ... WHERE ...`
    Delete {
        /// The target table.
        table: ObjectName,
        /// The `WHERE` clause, as written.
        selection: Option<Expr>,
    },
    /// `MERGE`, kept as written.
    Merge(String),
    /// A statement KumoSQL does not model, kept verbatim.
    ///
    /// Used for DDL, script statements and anything else outside the proven
    /// subset. The text is preserved so it can be re-emitted unchanged; the
    /// tree must not be used to reason about it.
    Command {
        /// The statement's tag, e.g. `CREATE SCHEMA`.
        keyword: String,
        /// The full statement text as written.
        text: String,
    },
}

impl fmt::Display for Statement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Statement::Query(query) => write!(f, "{}", RenderQuery(query)),
            Statement::CreateTableAs {
                name,
                query,
                replace,
                if_not_exists,
            } => {
                f.write_str("CREATE TABLE")?;
                if *replace {
                    f.write_str(" OR REPLACE")?;
                }
                if *if_not_exists {
                    f.write_str(" IF NOT EXISTS")?;
                }
                write!(f, " {name} AS {}", RenderQuery(query))
            }
            Statement::CreateViewAs {
                name,
                query,
                replace,
                materialized,
            } => {
                f.write_str("CREATE ")?;
                if *replace {
                    f.write_str("OR REPLACE ")?;
                }
                if *materialized {
                    f.write_str("MATERIALIZED ")?;
                }
                write!(f, "VIEW {name} AS {}", RenderQuery(query))
            }
            Statement::Insert {
                table,
                columns,
                query,
            } => {
                write!(f, "INSERT INTO {table}")?;
                if let Some(columns) = columns {
                    write!(f, " ({})", join_idents(columns))?;
                }
                if let Some(query) = query {
                    write!(f, " {}", RenderQuery(query))?;
                }
                Ok(())
            }
            Statement::Update {
                table,
                assignments,
                selection,
            } => {
                write!(f, "UPDATE {table}")?;
                if let Some(assignments) = assignments {
                    write!(f, " SET {assignments}")?;
                }
                if let Some(selection) = selection {
                    write!(f, " WHERE {selection}")?;
                }
                Ok(())
            }
            Statement::Delete { table, selection } => {
                write!(f, "DELETE FROM {table}")?;
                if let Some(selection) = selection {
                    write!(f, " WHERE {selection}")?;
                }
                Ok(())
            }
            Statement::Merge(text) => f.write_str(text),
            Statement::Command { text, .. } => f.write_str(text),
        }
    }
}

impl Statement {
    /// The query this statement is ultimately about, if it has one.
    ///
    /// Corresponds to `top_level_query` in the Python original: `CREATE ... AS`
    /// and `INSERT ... SELECT` unwrap to their query, everything else does not.
    pub fn top_level_query(&self) -> Option<&Query> {
        match self {
            Statement::Query(query) => Some(query),
            Statement::CreateTableAs { query, .. } => Some(query),
            Statement::CreateViewAs { query, .. } => Some(query),
            Statement::Insert { query, .. } => query.as_deref(),
            Statement::Update { .. } | Statement::Delete { .. } => None,
            Statement::Merge(_) | Statement::Command { .. } => None,
        }
    }

    /// Whether this statement's tree can be used for a rewrite.
    ///
    /// `false` for [`Statement::Command`]: the text is preserved but the shape
    /// is not modelled, so a rewrite must be declined rather than attempted.
    pub fn is_modelled(&self) -> bool {
        !matches!(self, Statement::Command { .. } | Statement::Merge(_))
    }
}
