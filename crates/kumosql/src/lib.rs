//! Public facade, mirroring the Python package's public API
//!
//! The facade mirrors `src/kumosql/__init__.py`: the functions and types a
//caller reaches for directly, such as `apply_rules`, `apply_rule` and
//`lift_subqueries`, re-exported from the implementation crates so that the
//Rust API reads like the Python one.
