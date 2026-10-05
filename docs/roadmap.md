# Port roadmap

The goal is feature parity with the Python original. This file is the running
status board. **Update it in the same change that lands a task** -- a task is not
done until it says so here.

Status values: `todo`, `in progress`, `done`, `blocked`, `partial`.

## Task status

| # | Task | Status | Notes |
| --- | --- | --- | --- |
| 1 | `toolchain-workspace` | done | Rust 1.99.0 + VS Build Tools 2022 installed; 12 crates scaffolded; see [toolchain.md](toolchain.md) for the Application Control workaround |
| 2 | `sql-ast-parser` | todo | Hybrid `sqlparser-rs` + BigQuery extensions |
| 3 | `sqlx-and-scripts` | todo | |
| 4 | `rule-framework` | todo | |
| 5 | `cleanup-rules` | todo | The nine documented rules |
| 6 | `formatting-rules` | todo | No sqlfluff peer; Rust formatter + `proof_format` check |
| 7 | `structural-prover` | todo | |
| 8 | `z3-smt-prover` | todo | Adds the `z3` crate (native C++) |
| 9 | `algebraic-prover` | todo | |
| 10 | `bounded-verification` | todo | |
| 11 | `executed-verification` | todo | |
| 12 | `duckdb-execution` | todo | Adds the `duckdb` crate (native C++) |
| 13 | `lineage-graph` | todo | |
| 14 | `pipeline-analysis` | todo | |
| 15 | `change-and-refactor` | todo | |
| 16 | `cost-and-join-order` | todo | |
| 17 | `bigquery-integration` | todo | |
| 18 | `cli-parity` | todo | Exit codes 0 / 2 / 3 / 4 |
| 19 | `browser-ui` | todo | |
| 20 | `eval-harness-and-evals` | todo | Scoreboard generated, never hand-edited |
| 21 | `docs-and-parity-gate` | todo | Final gate |

## What has actually landed

Nothing functional yet. As of 2026-10-05 the repository contains the workspace,
12 empty crate skeletons with documented responsibilities, the build/toolchain
documentation, and no ported behaviour. No rewrite runs, no proof is attempted,
and no benchmark number in this repository means anything yet.

## Sequencing notes

`duckdb` (task 12) and `z3` (task 8) compile native C++ and take minutes to
build. They are deliberately **not** declared in the workspace yet, so that the
fast tasks 2-11 do not pay for a C++ build on every `cargo test`. Each is added
to the specific crate that needs it when its task begins, and its resolved
version is recorded in [toolchain.md](toolchain.md).

Task 18 (`cli-parity`) is where the exit-code contract becomes testable, but it
depends on the commands existing. Its parity matrix test can be scaffolded early
with the not-yet-ported commands marked as pending, so that each later task
tightens it rather than writing it once at the end.

## Open risks

- **Parser divergence.** `sqlparser-rs` reads some BigQuery differently from
  `sqlglot`. The Python project has already fixed several such disagreements by
  hand (for example `t.x = -1 IS NULL`, which sqlglot and BigQuery read
  differently). Expect this to be the largest source of parity work, and log
  each divergence in [parity-notes.md](parity-notes.md).
- **`sqlfluff` has no Rust peer.** `format_sql` will be a reimplementation. It
  is the one place where output text is likely to differ from Python in ways
  that are cosmetic but numerous.
- **Benchmark corpora are not vendored.** Several evals need external checkouts
  downloaded at pinned commits. If those are unavailable, those evals report as
  not-run rather than as passing.