//! BigQuery/GoogleSQL AST, hybrid parser and SQL rendering
//!
//! Ported from the Python modules `ast_utils.py`, `parse_check.py`, `scripts.py`,
//`bigquery_syntax.py`, `formatting.py` and `js_literals.py`.
//
//Parsing is hybrid, per the project contract: `sqlparser-rs` provides the base
//SQL grammar, and custom extensions handle the BigQuery/GoogleSQL shapes it
//cannot read:
//
//  * `UNNEST` and `WITH OFFSET`
//  * STRUCT field access and typed struct literals (`STRUCT(1 AS a)`)
//  * `SAFE_`-prefixed functions and the BigQuery function catalogue
//  * `SELECT * EXCEPT (...)` / `SELECT * REPLACE (...)`
//  * BigQuery literal spelling: raw `r''`/`b''` prefixes, `0x`/`0b` literals,
//    triple-quoted strings, and GoogleSQL escape rules
//  * BigQuery script blocks (`DECLARE`, `SET`, `BEGIN ... END`, `IF`/`WHILE`/`FOR`)
//  * interval, range and array syntax, and `QUALIFY`
//
//Any construct the parser cannot read must produce an explicit, typed error.
//Silently approximating an unsupported shape is a defect: the whole point of
//this project is that a rewrite is either proven or reported as unproven.
