//! Command-line interface
//!
//! Ported from `cli.py` and `console.py`.
//
//Exit codes are part of the contract:
//
//  0   success
//  2   a rule failed fatally; nothing is written
//  3   output written but not proven equivalent (override: --allow-unproven)
//  4   --check-idempotence found a rule changing its own output
//
//All 24 Python console-script commands and their `python -m kumosql`
//dispatch forms have a Rust equivalent, with matching flags and `--help`.
