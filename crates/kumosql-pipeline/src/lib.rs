//! Lineage, pipeline graph, pipeline analysis and incremental models
//!
//! Ported from `graph.py`, the lineage modules, `pipeline.py`,
//`pipeline_loading.py`, `table_profile.py`, `incremental.py`,
//`incremental_rules.py`, `incremental_scan.py`, `containment.py`,
//`model_reuse.py` and shared-model behaviour.
//
//The unknown outcome is load-bearing: where the Python original reports
//`unknown`, this crate reports `unknown`. A lineage edge the reference tool
//does not claim must not be claimed here either.
