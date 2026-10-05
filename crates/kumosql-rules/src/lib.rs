//! Rewrite-rule framework, rule registry and shared driver
//!
//! Ported from the rule infrastructure in `engine.py` and the documented cleanup
//rules.
//
//The nine documented rules land here first (task 5):
//  `lift_subqueries`, `inline_single_use_ctes`, `remove_trivial_predicates`,
//  `remove_redundant_parentheses`, `deduplicate_ctes`, `remove_unused_ctes`,
//  `remove_redundant_distinct`, `qualify_columns`, `format_sql`.
//
//The driver owns the behaviour the Python driver owns: SQLX block and
//`${...}` interpolation handling, strict parsing with a visible recovery
//fallback, formatting, byte-for-byte no-op detection and CTE dependency
//checks. `canonical_rule_order()` returns a fixed point, and
//`check_idempotence` backs the CLI's exit code 4.
