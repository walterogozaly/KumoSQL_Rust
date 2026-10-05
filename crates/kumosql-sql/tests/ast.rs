//! The AST's identifier folding and rendering.
//!
//! These are the rules the Python original's tests lean on: BigQuery folds
//! unquoted identifiers to lower case and reads backticked ones literally, so
//! ``A`` and ``a`` are the same column but `` `A` `` and `` `a` `` are not.
//! `same_table` in `ast_utils.py` depends on exactly this.

use kumosql_sql::ast::*;

#[test]
fn unquoted_identifiers_fold_to_lower_case() {
    // `folded()` is the semantic comparison. The derived `PartialEq` is
    // structural and stays case-sensitive on purpose, so a rewrite can still
    // tell how a name was written.
    assert_eq!(Ident::bare("Orders").folded(), "orders");
    assert_eq!(Ident::bare("orders").folded(), "orders");
    assert_ne!(Ident::bare("Orders"), Ident::bare("orders"));
}

#[test]
fn quoted_identifiers_do_not_fold() {
    assert_eq!(Ident::quoted("A").folded(), "A");
    assert_ne!(Ident::quoted("A").folded(), Ident::quoted("a").folded());
    assert_ne!(Ident::quoted("Orders"), Ident::quoted("orders"));
}

#[test]
fn bare_identifiers_render_unquoted() {
    assert_eq!(Ident::bare("orders").to_string(), "orders");
    assert_eq!(Ident::bare("col_1").to_string(), "col_1");
}

#[test]
fn identifiers_needing_quotes_are_quoted_again() {
    // A leading digit, a non-word character, or the empty name cannot be bare.
    assert_eq!(Ident::bare("1st").to_string(), "`1st`");
    assert_eq!(Ident::bare("has space").to_string(), "`has space`");
    assert_eq!(Ident::bare("").to_string(), "``");
}

#[test]
fn a_quoted_identifier_stays_quoted() {
    // `Orders` is a valid bare name too, but it was written quoted, so it means
    // something different and must stay quoted.
    assert_eq!(Ident::quoted("Orders").to_string(), "`Orders`");
}

#[test]
fn a_backtick_inside_a_quoted_name_is_escaped() {
    assert_eq!(Ident::quoted("a`b").to_string(), "`a\\`b`");
}

#[test]
fn a_table_spelled_differently_is_the_same_table() {
    // The behaviour `same_table` relies on: a table spelled `A` on one side and
    // `a` on the other is one table.
    assert_eq!(
        ObjectName::new([Ident::bare("A"), Ident::bare("Orders")]).folded(),
        ObjectName::new([Ident::bare("a"), Ident::bare("orders")]).folded()
    );
}

#[test]
fn a_quoted_table_part_is_distinct_only_when_the_case_differs() {
    // `` `a` `` and `a` name the same table: both are lower-case `a`.
    // Backticks only stop the folding, so the difference shows for `A`.
    assert_eq!(
        ObjectName::new([Ident::quoted("a")]).folded(),
        ObjectName::new([Ident::bare("a")]).folded()
    );
    assert_ne!(
        ObjectName::new([Ident::quoted("A")]).folded(),
        ObjectName::new([Ident::bare("a")]).folded()
    );
}

#[test]
fn object_names_render_dotted() {
    let name = ObjectName::new([
        Ident::bare("project"),
        Ident::bare("dataset"),
        Ident::bare("table"),
    ]);
    assert_eq!(name.to_string(), "project.dataset.table");
}

#[test]
fn literals_round_trip_through_rendering() {
    assert_eq!(Literal::Null.to_string(), "NULL");
    assert_eq!(Literal::Boolean(true).to_string(), "TRUE");
    assert_eq!(Literal::Number("1e400".into()).to_string(), "1e400");
    assert_eq!(Literal::String("it's".into()).to_string(), "'it\\'s'");
    assert_eq!(
        Literal::Parameter("session_id".into()).to_string(),
        "@session_id"
    );
}

#[test]
fn a_bytes_literal_quotes_and_backslashes_as_hex() {
    // Only `'` (39) and `\` (92) are escaped; a double quote is printable
    // inside `b'..'` and is kept. This matches `_bytes_literal` in the Python
    // original, which is what keeps `b'q\x5C"'` the canonical spelling of
    // `Rb'q\"'`.
    assert_eq!(Literal::Bytes(b"A".to_vec()).to_string(), "b'A'");
    assert_eq!(Literal::Bytes(vec![0x5C, 0x22]).to_string(), "b'\\x5C\"'");
    assert_eq!(Literal::Bytes(vec![0x27]).to_string(), "b'\\x27'");
}

#[test]
fn a_literal_is_compared_by_its_value_not_its_spelling() {
    // `Literal` keeps the number's text, so two spellings of the same number
    // are *not* equal here. That is deliberate at this layer: canonicalising
    // numbers is the numeric-value layer's job, not the AST's, and a rewrite
    // must not assume two spellings are one value without proving it.
    assert_ne!(Literal::Number("0x10".into()), Literal::Number("16".into()));
    assert_eq!(Literal::Number("16".into()), Literal::Number("16".into()));
    assert_ne!(
        Literal::String("a\"b".into()),
        Literal::String("a\\\"b".into())
    );
}

#[test]
fn conjunction_and_disjunction_render_flat() {
    let and = Expr::And(vec![
        Expr::Column(ObjectName::single("a")),
        Expr::Column(ObjectName::single("b")),
        Expr::Column(ObjectName::single("c")),
    ]);
    assert_eq!(and.to_string(), "(a AND b AND c)");

    let or = Expr::Or(vec![
        Expr::Column(ObjectName::single("a")),
        Expr::Column(ObjectName::single("b")),
    ]);
    assert_eq!(or.to_string(), "(a OR b)");
}

#[test]
fn select_renders_its_clauses_in_bigquery_order() {
    let select = Select {
        projections: vec![
            Expr::Alias {
                expr: Box::new(Expr::Column(ObjectName::new([
                    Ident::bare("t"),
                    Ident::bare("c"),
                ]))),
                alias: Ident::bare("x"),
            },
            Expr::Function {
                name: ObjectName::single("COUNT"),
                args: vec![Expr::Star],
                distinct: DuplicateHandling::All,
                star: true,
                order_by: None,
                limit: None,
            },
        ],
        from: Some(TableFactor::Table {
            name: ObjectName::single("t"),
            alias: None,
            options: None,
        }),
        selection: Some(Expr::Binary {
            op: BinaryOp::Gt,
            left: Box::new(Expr::Column(ObjectName::new([
                Ident::bare("t"),
                Ident::bare("c"),
            ]))),
            right: Box::new(Expr::Literal(Literal::Number("1".into()))),
        }),
        group_by: None,
        having: None,
        qualify: None,
        windows: None,
        order_by: None,
        limit: None,
        offset: None,
        ..Default::default()
    };
    assert_eq!(
        select.to_string(),
        "SELECT t.c AS x, COUNT(*) FROM t WHERE (t.c > 1)"
    );
}

#[test]
fn a_with_clause_renders_before_its_query() {
    let query = Query::Select {
        with: Some(With {
            recursive: false,
            ctes: vec![Cte {
                name: Ident::bare("t"),
                columns: None,
                query: Box::new(Query::Select {
                    with: None,
                    body: Box::new(Select {
                        projections: vec![Expr::Star],
                        ..Default::default()
                    }),
                }),
            }],
        }),
        body: Box::new(Select {
            projections: vec![Expr::Star],
            from: Some(TableFactor::Table {
                name: ObjectName::single("t"),
                alias: None,
                options: None,
            }),
            ..Default::default()
        }),
    };
    assert_eq!(
        RenderQuery(&query).to_string(),
        "WITH t AS (SELECT *) SELECT * FROM t"
    );
}

#[test]
fn a_union_all_carries_all_and_a_bare_union_does_not() {
    let branch = || {
        Box::new(Query::Select {
            with: None,
            body: Box::new(Select {
                projections: vec![Expr::Star],
                ..Default::default()
            }),
        })
    };
    let union = Query::SetOperation {
        with: None,
        op: SetOp::Union,
        duplicate_handling: DuplicateHandling::All,
        left: branch(),
        right: branch(),
        order_by: None,
        limit: None,
    };
    assert_eq!(
        RenderQuery(&union).to_string(),
        "SELECT * UNION ALL SELECT *"
    );
}

#[test]
fn star_modifiers_render() {
    let except = Expr::ModifiedStar(StarModifier::Except(vec![
        Ident::bare("a"),
        Ident::bare("b"),
    ]));
    assert_eq!(except.to_string(), "* EXCEPT (a, b)");

    let both = Expr::ModifiedStar(StarModifier::Both {
        except: vec![Ident::bare("a")],
        replace: vec![Expr::Alias {
            expr: Box::new(Expr::Literal(Literal::Number("1".into()))),
            alias: Ident::bare("a"),
        }],
    });
    assert_eq!(both.to_string(), "* EXCEPT (a) REPLACE (1 AS a)");
}

#[test]
fn top_level_query_unwraps_create_and_insert_only() {
    let query = Query::Select {
        with: None,
        body: Box::new(Select {
            projections: vec![Expr::Star],
            ..Default::default()
        }),
    };

    let created = Statement::CreateTableAs {
        name: ObjectName::single("t"),
        query: Box::new(query.clone()),
        replace: false,
        if_not_exists: false,
    };
    assert!(created.top_level_query().is_some());
    assert!(created.is_modelled());

    let updated = Statement::Update {
        table: ObjectName::single("t"),
        assignments: None,
        selection: None,
    };
    assert!(updated.top_level_query().is_none());
}

#[test]
fn a_command_statement_is_preserved_but_not_modelled() {
    // The text is kept so it can be re-emitted, but the tree must not be used
    // to reason about it -- that is what `is_modelled` reports.
    let command = Statement::Command {
        keyword: "CREATE SCHEMA".into(),
        text: "CREATE SCHEMA ds".into(),
    };
    assert!(!command.is_modelled());
    assert!(command.top_level_query().is_none());
    assert_eq!(command.to_string(), "CREATE SCHEMA ds");
}

#[test]
fn a_constant_query_is_recognised() {
    let constant = Query::Select {
        with: None,
        body: Box::new(Select {
            projections: vec![Expr::Literal(Literal::Number("1".into()))],
            ..Default::default()
        }),
    };
    assert!(constant.is_constant());

    let reading = Query::Select {
        with: None,
        body: Box::new(Select {
            projections: vec![Expr::Star],
            from: Some(TableFactor::Table {
                name: ObjectName::single("t"),
                alias: None,
                options: None,
            }),
            ..Default::default()
        }),
    };
    assert!(!reading.is_constant());
}
