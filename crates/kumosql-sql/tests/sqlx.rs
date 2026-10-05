//! SQLX sections and `${...}` masking.
//!
//! Ported from `tests/test_sqlx.py` and `tests/test_sqlx_opaque_expressions.py`.
//! The end-to-end tests there run through the rule engine, which is tasks 4 and
//! 5; what is ported here is the masking layer those tests depend on, with the
//! same examples.

use kumosql_sql::sqlx::{
    SectionKind, SqlxRestorationError, is_table_reference, looks_like_sqlx,
    mask_sqlx_interpolations, opaque_tokens, restore_sqlx_interpolations, split_sqlx_sections,
    sql_comment_spans,
};

// ------------------------------------------------------------------ sections

#[test]
fn a_config_block_is_split_out_and_kept_byte_for_byte() {
    let source = "config {\n  type: \"table\"\n}\n\nSELECT c.id\nFROM ${ref(\"customers\")} AS c";
    let sections = split_sqlx_sections(source);
    let block = sections
        .iter()
        .find(|s| s.kind == SectionKind::Block)
        .expect("a config block");
    assert_eq!(block.text, "config {\n  type: \"table\"\n}");
    // And the SQL around it is still there.
    assert!(
        sections
            .iter()
            .any(|s| s.kind == SectionKind::Sql
                && s.text.contains("FROM ${ref(\"customers\")} AS c"))
    );
}

#[test]
fn every_block_kind_is_recognised() {
    for (header, body) in [
        ("config { type: \"table\" }", "config { type: \"table\" }"),
        ("js { const x = 1; }", "js { const x = 1; }"),
        (
            "pre_operations {\n  DECLARE run_id INT64 DEFAULT 1;\n}",
            "pre_operations {\n  DECLARE run_id INT64 DEFAULT 1;\n}",
        ),
        (
            "post_operations {\n  GRANT `roles/x` ON TABLE ${self()} TO \"group:a\";\n}",
            "post_operations {\n  GRANT `roles/x` ON TABLE ${self()} TO \"group:a\";\n}",
        ),
        (
            "input \"a { b\" {\n  SELECT 1;\n}",
            "input \"a { b\" {\n  SELECT 1;\n}",
        ),
    ] {
        let sections = split_sqlx_sections(body);
        let block = sections
            .iter()
            .find(|s| s.kind == SectionKind::Block)
            .unwrap_or_else(|| panic!("{header} should open a block: {body:?}"));
        assert_eq!(block.text, body, "block not preserved byte for byte");
    }
}

#[test]
fn a_brace_inside_a_block_does_not_close_it_early() {
    // The `js` block contains an object literal; the block must not end there.
    let source = "js { const o = { a: 1 }; }\nSELECT 1";
    let sections = split_sqlx_sections(source);
    let block = sections
        .iter()
        .find(|s| s.kind == SectionKind::Block)
        .expect("a js block");
    assert_eq!(block.text, "js { const o = { a: 1 }; }");
}

#[test]
fn a_quote_or_comment_inside_a_block_is_not_structure() {
    for source in [
        "js { const s = \"}\"; }\nSELECT 1",
        "js { // }\nconst x = 1; }\nSELECT 1",
        "js { /* } */ const x = 1; }\nSELECT 1",
    ] {
        let sections = split_sqlx_sections(source);
        let block = sections
            .iter()
            .find(|s| s.kind == SectionKind::Block)
            .unwrap_or_else(|| panic!("no block found in {source:?}"));
        assert!(block.text.ends_with('}'), "{:?}", block.text);
        assert!(
            sections
                .iter()
                .any(|s| s.kind == SectionKind::Sql && s.text.contains("SELECT 1")),
            "the SQL after the block was lost for {source:?}"
        );
    }
}

#[test]
fn a_malformed_block_is_not_claimed_as_a_block() {
    // An unterminated block must not silently swallow the rest as if it were
    // well-formed.
    let sections = split_sqlx_sections("config {\n  type: \"table\"\n\nSELECT 1");
    assert!(
        !sections.iter().any(|s| s.kind == SectionKind::Block),
        "an unterminated block must not be reported as one"
    );
}

#[test]
fn plain_sql_has_one_sql_section_and_no_blocks() {
    let sections = split_sqlx_sections("SELECT 1 FROM t");
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].kind, SectionKind::Sql);
    assert_eq!(sections[0].text, "SELECT 1 FROM t");
}

#[test]
fn looks_like_sqlx_distinguishes_sql_from_sqlx() {
    assert!(looks_like_sqlx("SELECT ${ref(\"t\")} FROM x"));
    assert!(looks_like_sqlx("config { type: \"table\" }\nSELECT 1"));
    assert!(!looks_like_sqlx("SELECT 1 FROM t"));
}

// ------------------------------------------------------------------ comments

#[test]
fn sql_comments_are_found() {
    assert_eq!(sql_comment_spans("SELECT 1 -- note\nFROM t"), vec![(9, 16)]);
    assert_eq!(sql_comment_spans("SELECT /* note */ 1"), vec![(7, 17)]);
}

#[test]
fn a_comment_marker_inside_a_string_is_not_a_comment() {
    assert!(sql_comment_spans("SELECT '-- not a comment'").is_empty());
    assert!(sql_comment_spans("SELECT '/* nor this */'").is_empty());
}

#[test]
fn an_interpolation_is_skipped_when_looking_for_comments() {
    // A comment marker inside `${...}` must not open a comment.
    let spans = sql_comment_spans("SELECT ${a -- b} FROM t -- real");
    // `-- real` starts at 24: `SELECT ${a -- b} FROM t ` is 24 characters.
    assert_eq!(spans, vec![(24, 31)]);
}

// ------------------------------------------------------------------ table refs

#[test]
fn table_references_are_recognised() {
    for original in [
        "${ref(\"t\")}",
        "${ref(\"dataset\", \"t\")}",
        "${resolve(\"t\")}",
        "${self()}",
    ] {
        assert!(
            is_table_reference(original),
            "{original} should be a table reference"
        );
    }
}

#[test]
fn other_expressions_are_not_table_references() {
    // These compile to arbitrary SQL, so a rule may not reason past them.
    for original in [
        "${\"x OR y\"}",
        "${when(incremental(), \"AND b > 1\")}",
        "${column(\"a\")}",
        "${ref(a)}",
        "${ref(\"a\", \"b\", \"c\")}",
        "not an interpolation",
        "${}",
    ] {
        assert!(
            !is_table_reference(original),
            "{original} should not be a table reference"
        );
    }
}

// ------------------------------------------------------------------ masking

#[test]
fn an_interpolation_becomes_a_sentinel() {
    let (masked, restorations) = mask_sqlx_interpolations("SELECT * FROM ${ref(\"t\")}");
    assert_eq!(masked, "SELECT * FROM __sqlx_token_000__");
    assert_eq!(restorations.len(), 1);
    assert_eq!(restorations[0].original, "${ref(\"t\")}");
}

#[test]
fn a_when_expression_that_continues_a_condition_keeps_its_connective() {
    // `... WHERE a > 0 ${when(incremental(), "AND b > 1")}`: the expression
    // continues the condition, so the sentinel needs the AND in front of it for
    // the SQL to parse.
    let (masked, restorations) = mask_sqlx_interpolations(
        "SELECT * FROM t WHERE a > 0 ${when(incremental(), \"AND b > 1\")}",
    );
    assert_eq!(masked, "SELECT * FROM t WHERE a > 0 AND __sqlx_token_000__");
    assert_eq!(restorations[0].connective.as_deref(), Some("AND"));
    assert!(!restorations[0].whole);
}

#[test]
fn a_when_expression_that_opens_a_condition_keeps_its_trailing_connective() {
    // `WHERE ${when(incremental(), "a > 1 AND")} b > 2`: the expression starts
    // the condition and ends with its connective, so the two are one unit and a
    // rewrite that separated them could not be restored.
    let (masked, restorations) =
        mask_sqlx_interpolations("SELECT * FROM t WHERE ${when(incremental(), `a > 1 AND`)} b > 2");
    assert!(
        masked.contains("WHERE __sqlx_token_000__ AND b > 2"),
        "{masked}"
    );
    assert!(restorations[0].whole);
    assert_eq!(restorations[0].connective.as_deref(), Some("AND"));
}

#[test]
fn an_expression_that_opens_a_clause_of_its_own_keeps_the_clause() {
    let (masked, restorations) = mask_sqlx_interpolations(
        "SELECT * FROM ${ref(\"customers\")} ${when(incremental(), `WHERE id > 1`, ``)}",
    );
    assert!(masked.contains("WHERE __sqlx_token_001__"), "{masked}");
    assert_eq!(restorations[1].connective.as_deref(), Some("WHERE"));
}

#[test]
fn several_interpolations_get_distinct_sentinels() {
    let (masked, restorations) =
        mask_sqlx_interpolations("SELECT ${ref(\"a\")}, ${ref(\"b\")} FROM t");
    assert_eq!(
        masked,
        "SELECT __sqlx_token_000__, __sqlx_token_001__ FROM t"
    );
    assert_eq!(restorations.len(), 2);
    assert_ne!(restorations[0].token, restorations[1].token);
}

#[test]
fn a_sentinel_already_in_the_source_is_not_reused() {
    let source = "SELECT __sqlx_token_000__, ${ref(\"a\")}";
    let (masked, restorations) = mask_sqlx_interpolations(source);
    assert_ne!(restorations[0].token, "__sqlx_token_000__");
    assert!(masked.contains("__sqlx_token_000__"), "{masked}");
}

#[test]
fn an_interpolation_inside_a_comment_is_left_alone() {
    // Dataform leaves it as text there, so it must not be masked.
    let source = "SELECT 1 -- ${ref(\"t\")}\nFROM t";
    let (masked, restorations) = mask_sqlx_interpolations(source);
    assert!(masked.contains("${ref(\"t\")}"), "{masked}");
    assert!(restorations.is_empty());
}

// ------------------------------------------------------------------ opaque

#[test]
fn opaque_tokens_are_the_non_table_references() {
    let (_, restorations) = mask_sqlx_interpolations("SELECT ${ref(\"t\")}, ${\"x OR y\"} FROM u");
    let opaque = opaque_tokens(&restorations);
    assert_eq!(opaque.len(), 1, "{opaque:?}");
    assert_eq!(opaque[0], restorations[1].token);
}

// ------------------------------------------------------------------ restoring

#[test]
fn masking_then_restoring_returns_the_original() {
    for source in [
        "SELECT * FROM ${ref(\"t\")}",
        "SELECT * FROM t WHERE a > 0 ${when(incremental(), \"AND b > 1\")}",
        "SELECT * FROM t WHERE ${when(incremental(), `a > 1 AND`)} b > 2",
        "SELECT ${ref(\"a\")}, ${\"x OR y\"} FROM t",
    ] {
        let (masked, restorations) = mask_sqlx_interpolations(source);
        let restored = restore_sqlx_interpolations(&masked, &restorations)
            .unwrap_or_else(|e| panic!("{source:?} should restore: {e}"));
        assert_eq!(restored, source, "round trip changed {source:?}");
    }
}

#[test]
fn a_lost_sentinel_is_an_error_not_a_substitution() {
    // This is the safety property: a rewrite that dropped the expression must
    // not produce valid-looking SQL that silently means something else.
    let (_, restorations) = mask_sqlx_interpolations("SELECT * FROM ${ref(\"t\")}");
    let result = restore_sqlx_interpolations("SELECT * FROM t", &restorations);
    match result {
        Err(SqlxRestorationError(message)) => {
            assert!(message.contains("lost or duplicated"), "{message}");
        }
        Ok(restored) => panic!("a lost sentinel must not restore: {restored}"),
    }
}

#[test]
fn a_duplicated_sentinel_is_an_error() {
    let (masked, restorations) = mask_sqlx_interpolations("SELECT * FROM ${ref(\"t\")}");
    let doubled = masked.replace(
        "__sqlx_token_000__",
        "__sqlx_token_000__ , __sqlx_token_000__",
    );
    assert!(restore_sqlx_interpolations(&doubled, &restorations).is_err());
}

#[test]
fn separating_an_expression_from_its_connective_is_an_error() {
    // The whole-interpolation case: the sentinel plus its connective must move
    // together, or restoring would change the meaning.
    let (masked, restorations) =
        mask_sqlx_interpolations("SELECT * FROM t WHERE ${when(incremental(), `a > 1 AND`)} b > 2");
    assert!(restorations[0].whole);
    let separated = masked.replace("__sqlx_token_000__ AND", "__sqlx_token_000__");
    let result = restore_sqlx_interpolations(&separated, &restorations);
    match result {
        Err(SqlxRestorationError(message)) => {
            assert!(message.contains("separated"), "{message}");
        }
        Ok(restored) => panic!("a separated expression must not restore: {restored}"),
    }
}

#[test]
fn a_backslash_in_an_expression_is_not_replacement_syntax() {
    // `r'\d'` must come back as written, not have `\d` read as a replacement.
    let source = r"SELECT ${r'\d'} FROM t";
    let (masked, restorations) = mask_sqlx_interpolations(source);
    let restored = restore_sqlx_interpolations(&masked, &restorations).expect("restore");
    assert_eq!(restored, source);
}

#[test]
fn nothing_to_restore_is_a_no_op() {
    let (masked, restorations) = mask_sqlx_interpolations("SELECT 1 FROM t");
    assert_eq!(masked, "SELECT 1 FROM t");
    assert!(restorations.is_empty());
    assert_eq!(
        restore_sqlx_interpolations(&masked, &restorations).expect("restore"),
        "SELECT 1 FROM t"
    );
}
