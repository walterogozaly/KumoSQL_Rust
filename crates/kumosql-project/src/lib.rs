//! Change reports, verified refactoring and project minimization
//!
//! Ported from `change_report.py`, `ci_check.py`, `refactor.py`,
//`consolidate.py`, `table_minimizer.py`, `project_reduction.py`,
//`evidence_summary.py` and the saved scopes / equivalence declaration stores.
//
//Refactors are proof-gated: a proposal that is not proven equivalent is
//reported as such and never presented as a successful refactor.
