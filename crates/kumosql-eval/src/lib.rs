//! Benchmark harness, results and scoreboard generation
//!
//! Ported from `tools/scoreboard.py`, `tools/eval_diff.py` and the per-eval
//harnesses under `benchmarks/` and `tools/`.
//
//The scoreboard is generated from `benchmarks/results/*.json` and is never
//hand-edited. Where the Rust port scores differently from the Python
//original, the difference is recorded with its reason rather than smoothed
//over.
