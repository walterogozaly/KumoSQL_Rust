//! Equivalence verification stack
//!
//! Ported from `equivalence.py`, `smt_equivalence.py`,
//`algebraic_equivalence.py`, `bounded_equivalence.py`,
//`conditional_equivalence.py`, `counterexample.py`, `executed_refutation.py`,
//`canonical.py` and `canonical_rules.py`.
//
//Five backends, tried in the documented order:
//
//  1. structural   -- canonical-form comparison (task 7)
//  2. smt          -- z3, with reported assumptions (task 8)
//  3. algebraic    -- algebraic facts, then SQLSolver (task 9)
//  4. bounded      -- z3 with a row bound; not a proof (task 10)
//  5. executed     -- counterexample search on real databases (task 11)
//
//The status vocabulary is fixed by the contract and must match the Python
//original exactly: `unchanged`, `proven`, `planner_checked`, `unproven`,
//`failed`, each with `checks[]` entries carrying `kind`, `outcome` and
//`detail`. Only `unchanged` and `proven` are trusted.
