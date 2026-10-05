# Port roadmap

The goal is feature parity with the Python original. This file is the running
status board. **Update it in the same change that lands a task** -- a task is not
done until it says so here.

Status values: `todo`, `in progress`, `done`, `blocked`, `partial`.

## Task status

| # | Task | Status | Notes |
| --- | --- | --- | --- |
| 1 | `toolchain-workspace` | done | Rust 1.99.0 + VS Build Tools 2022 installed; 12 crates scaffolded; see [toolchain.md](toolchain.md) for the Application Control workaround |
| 2 | `sql-ast-parser` | in progress | `string_literals` ported and passing (5 tests, 11 generated cases). AST, hybrid parse pipeline and renderer still to do |
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

Task 2 has started. Ported so far:

* **`string_literals`** (`kumosql-sql/src/literals.rs`) -- `canonical_literals`
  and `invalid_literal`, from `src/kumosql/string_literals.py`.
* **`error`** (`kumosql-sql/src/error.rs`) -- the typed errors that stand in for
  `UnmodeledConstruct` and `LossySql`, plus the `Rejected` case. The kind is
  what lets a caller separate "we cannot reason about this" from "this is not a
  query", which are different claims.
* **`ast`** (`kumosql-sql/src/ast.rs`) -- the syntax tree and its BigQuery
  rendering. Enums rather than sqlglot's node classes, so an unhandled construct
  is a compile error instead of a silently dropped node.

25 tests pass. Four of the AST tests failed on first run and caught real
problems: a renderer bug that put `ALL` on the left operand of a `UNION ALL`,
and three test expectations of mine that were wrong (structural identifier
equality is case-sensitive by design; `` `a` `` and `a` name the same table; and
`"` is printable inside a bytes literal). All four are recorded in the tests.

The case table is **generated**, not transcribed: `tools/dump_literal_cases.py`
reads `tests/test_string_literals.py` out of the reference repo with `ast`,
renders each case as a Rust literal, and self-checks the whole table against the
Python implementation before writing it. A mismatch in the Rust tests therefore
means the port is wrong, not the table. Hand-transcribing the cases corrupted two
of them on the first attempt, which is why the generator exists.

The six prover- and DuckDB-dependent tests in that Python file are listed as
`pending` in `crates/kumosql-sql/tests/literals.rs` with the task each needs,
rather than dropped.

* **`token`** (`kumosql-sql/src/token.rs`) -- a BigQuery-aware tokenizer, used
  only by the rewrites. It exists because probing `sqlparser-rs` 0.59 showed it
  reads most BigQuery but refuses `COUNT(x WHERE c)`, `STRUCT()`,
  `GROUP BY ALL` and script statements -- the same class of gap the Python
  original works around for sqlglot.
* **`rewrite`** (`kumosql-sql/src/rewrite.rs`) -- the text rewrites that close
  that gap: the aggregate filter in both directions, and an outright refusal of
  `STRUCT<>()`, which both parsers would otherwise read as a comparison.

43 tests pass across the crate.

The rewrite tests caught four real bugs on first run: a doubled `)`, `FROM (`
mistaken for a call so a subquery's `WHERE` got rewritten, a broken `FILTER`
reconstruction, and BigQuery's doubled-backtick quoting being misread. The
`__KUMO_AGG_FILTER__` marker is also restored, for the same reason the Python
original marks its rewrites -- without it, a query that already used the
standard `FILTER` is indistinguishable from one this module produced.

Still to do for task 2: wiring the pipeline together (`parse_statements`:
literals -> rewrites -> `sqlparser-rs` -> AST), `GROUP BY ALL`, script
statements, and `parse_check`.

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