//! Splitting a BigQuery script into its statements.
//!
//! Ported from `tests/test_scripts.py`. The expectations are taken from that
//! file directly, because the exact statement texts and the exact sequence of
//! conditional flags are the contract here: a splitter that is one statement off
//! would hand a caller a different set of queries to verify.
//!
//! The lexing and `string_value` tests at the end of the Python file cover
//! `string_literals`, which is ported and tested in `literals.rs`.

use kumosql_sql::scripts::{
    has_blocks, leaf_statements, parse_script, split_script, split_statements,
};

/// The text of every statement, in order.
fn texts(parts: &[kumosql_sql::scripts::ScriptPart]) -> Vec<String> {
    parts.iter().map(|p| p.text.clone()).collect()
}

#[test]
fn blocks_are_opened_not_returned_and_branches_are_conditional() {
    let parts = split_script(
        "DECLARE x INT64 DEFAULT 1;
         BEGIN
           IF x > 0 THEN SELECT 1; ELSEIF x < 0 THEN SELECT 2; ELSE SELECT IF(x = 0, 3, 4); END IF;
           WHILE x < 3 DO SET x = x + 1; END WHILE;
           FOR r IN (SELECT a FROM t) DO INSERT INTO u SELECT r.a; END FOR;
           LOOP SELECT 5; BREAK; END LOOP;
           REPEAT SET x = x - 1; UNTIL x < 0 END REPEAT;
           CASE x WHEN 1 THEN SELECT 6; ELSE SELECT 7; END CASE;
         EXCEPTION WHEN ERROR THEN SELECT 'err; ok';
         END;
         SELECT 'after'",
    );

    assert_eq!(
        texts(&parts),
        vec![
            "DECLARE x INT64 DEFAULT 1",
            "SELECT 1",
            "SELECT 2",
            "SELECT IF(x = 0, 3, 4)",
            "SET x = x + 1",
            "INSERT INTO u SELECT r.a",
            "SELECT 5",
            "BREAK",
            "SET x = x - 1",
            "SELECT 6",
            "SELECT 7",
            "SELECT 'err; ok'",
            "SELECT 'after'",
        ]
    );

    let conditional: Vec<bool> = parts.iter().map(|p| p.conditional).collect();
    assert_eq!(
        conditional,
        [false]
            .into_iter()
            .chain([true].repeat(11))
            .chain([false])
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_semicolon_inside_a_string_is_not_a_statement_boundary() {
    // A string, a quoted name, a triple-quoted string, a raw string and a
    // dotted quoted name each hold a `;` that must not split the script.
    for sql in [
        "SELECT 'a;b'",
        r#"SELECT "x;y" AS s"#,
        "SELECT '''multi;\n    line;''' AS m",
        r"SELECT r'raw;' AS r",
        "SELECT `p.d.t;x` FROM t",
    ] {
        assert_eq!(
            split_statements(sql),
            vec![sql.to_string()],
            "a semicolon inside {sql:?} split the statement"
        );
    }
}

#[test]
fn case_expressions_and_if_functions_do_not_open_blocks() {
    let sql = "SELECT CASE WHEN a THEN 1 END AS c, IF(b, 1, 2) AS d FROM t; SELECT 2";
    assert_eq!(
        split_statements(sql),
        vec![
            "SELECT CASE WHEN a THEN 1 END AS c, IF(b, 1, 2) AS d FROM t",
            "SELECT 2",
        ]
    );
}

#[test]
fn transactions_are_statements_not_blocks() {
    assert_eq!(
        split_statements("BEGIN TRANSACTION; INSERT INTO t SELECT 1; COMMIT TRANSACTION;"),
        vec![
            "BEGIN TRANSACTION",
            "INSERT INTO t SELECT 1",
            "COMMIT TRANSACTION",
        ]
    );
}

#[test]
fn a_procedure_body_is_marked_and_a_function_body_is_one_statement() {
    let parts = split_script(
        "CREATE OR REPLACE PROCEDURE `p.d.proc`(IN a STRING)
         OPTIONS(description='x; y')
         BEGIN
           SELECT 1;
           INSERT INTO t SELECT CASE WHEN a = 'x' THEN 1 END
         END;
         CREATE TEMP FUNCTION f(x INT64) RETURNS INT64 LANGUAGE js AS r'''return x; ''';
         SELECT f(1)",
    );

    let marked: Vec<(String, String)> = parts
        .iter()
        .map(|p| (p.text.chars().take(20).collect(), p.in_procedure.clone()))
        .collect();
    assert_eq!(
        marked,
        vec![
            ("SELECT 1".to_string(), "p.d.proc".to_string()),
            ("INSERT INTO t SELECT".to_string(), "p.d.proc".to_string()),
            ("CREATE TEMP FUNCTION".to_string(), String::new()),
            ("SELECT f(1)".to_string(), String::new()),
        ]
    );
}

#[test]
fn labels_missing_final_semicolons_and_empty_text() {
    assert_eq!(
        split_statements("lbl: BEGIN SELECT 1; END lbl; SELECT 2;"),
        vec!["SELECT 1", "SELECT 2"]
    );
    // No final semicolon before `END`.
    assert_eq!(
        split_statements("BEGIN SELECT 1; SELECT 2 END"),
        vec!["SELECT 1", "SELECT 2"]
    );
    assert!(split_statements("").is_empty());
    assert!(split_statements(" ;; ").is_empty());
}

#[test]
fn statement_lines_are_reported() {
    let parts = split_script("SELECT 1;\n\n-- c\nSELECT 2;");
    let lines: Vec<usize> = parts.iter().map(|p| p.line).collect();
    assert_eq!(lines, vec![1, 4]);
}

#[test]
fn has_blocks_distinguishes_a_plain_script_from_a_wrapped_one() {
    assert!(!has_blocks("SELECT 1; SELECT 2;"));
    assert!(has_blocks("BEGIN SELECT 1; END"));
    assert!(has_blocks("IF a THEN SELECT 1; END IF"));
    assert!(has_blocks("CREATE PROCEDURE p() BEGIN SELECT 1; END"));
}

#[test]
fn leaf_statements_carry_their_offsets() {
    let sql = "SELECT 1; SELECT 2";
    let leaves = leaf_statements(sql);
    assert_eq!(leaves.len(), 2);
    // "SELECT 1; SELECT 2": the second statement starts after `; `, so at 10.
    assert_eq!(leaves[0].start, 0);
    assert_eq!(leaves[1].start, 10);
    assert_eq!(&sql[leaves[0].start..leaves[0].end], "SELECT 1");
    assert_eq!(&sql[leaves[1].start..leaves[1].end], "SELECT 2");
}

#[test]
fn a_for_loop_records_its_variable_and_query() {
    let nodes = parse_script("FOR r IN (SELECT a FROM t) DO SELECT 1; END FOR;");
    let for_node = nodes
        .iter()
        .find(|n| n.kind == kumosql_sql::scripts::NodeKind::For)
        .expect("a FOR node");
    assert_eq!(for_node.name, "r");
    // The loop's own parentheses are stripped from the query.
    assert_eq!(for_node.query, "SELECT a FROM t");
}

#[test]
fn an_if_block_records_every_condition_that_picks_a_path() {
    let nodes = parse_script("IF a THEN SELECT 1; ELSEIF b THEN SELECT 2; ELSE SELECT 3; END IF;");
    let if_node = nodes
        .iter()
        .find(|n| n.kind == kumosql_sql::scripts::NodeKind::If)
        .expect("an IF node");
    // Three branches: THEN, ELSEIF, ELSE.
    assert_eq!(if_node.branches.len(), 3);
    assert_eq!(if_node.conditions, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn a_case_statement_records_each_when() {
    let nodes =
        parse_script("CASE x WHEN 1 THEN SELECT 1; WHEN 2 THEN SELECT 2; ELSE SELECT 3; END CASE;");
    let case_node = nodes
        .iter()
        .find(|n| n.kind == kumosql_sql::scripts::NodeKind::Case)
        .expect("a CASE node");
    assert_eq!(case_node.conditions, vec!["x", "1", "2"]);
    assert_eq!(case_node.branches.len(), 3);
}
