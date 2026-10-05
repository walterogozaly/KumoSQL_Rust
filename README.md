# KumoSQL (Rust)

A Rust port of [KumoSQL](https://github.com/walterogozaly/KumoSQL), the Python
tool for deterministic, test-verified transformations of BigQuery SQL.

> **Status: port in progress.** The workspace builds, tests, lints and formats
> clean, and the crate boundaries are in place, but no KumoSQL functionality has
> been ported yet. Nothing here rewrites or proves SQL today. See
> [docs/roadmap.md](docs/roadmap.md) for what has landed and what has not, and
> do not treat any number in this repository as a result until the corresponding
> task in the roadmap is done.

## Relationship to the Python original

The Python repository at `C:/Users/walte/Desktop/Repos/KumoSQL` is a **read-only
reference**. It is never modified by work in this repository. Where the two
disagree, the disagreement is recorded rather than silently resolved -- see
[docs/parity-notes.md](docs/parity-notes.md) for the log of known differences.

The port targets **behaviour and result-JSON parity**: the same rewrite
decisions, the same verification status vocabulary, the same CLI exit codes and
the same JSON shapes. It does not promise byte-identical SQL text, because two
of the Python tool's dependencies (`sqlfluff` for formatting, `sqlglot` for
rendering) have no Rust peer. Where output text differs but meaning does not,
that difference is documented.

## Layout

| Crate | Ported from |
| --- | --- |
| `kumosql-sql` | AST, parser and rendering (`ast_utils.py`, `parse_check.py`, `scripts.py`, `bigquery_syntax.py`, `formatting.py`) |
| `kumosql-rules` | Rewrite-rule framework and the documented cleanup rules (`engine.py`) |
| `kumosql-verify` | Structural / SMT / algebraic / bounded / executed verification (`equivalence.py` and siblings) |
| `kumosql-exec` | DuckDB execution and BigQuery-to-DuckDB translation |
| `kumosql-pipeline` | Lineage, graph, pipeline analysis, incremental models |
| `kumosql-project` | Change reports, verified refactoring, minimization |
| `kumosql-cost` | Cost model and join ordering (`cost_model.py`, `joinorder/`) |
| `kumosql-bq` | BigQuery dry-run, catalogs, Dataform repositories |
| `kumosql-eval` | Benchmark harness and scoreboard generation |
| `kumosql-ui` | Local browser UI server |
| `kumosql-cli` | Command-line interface and exit-code contract |
| `kumosql` | Public facade, mirroring the Python package's public API |

Each crate's `src/lib.rs` documents the Python modules it is responsible for.

## Building

```shell
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

**This machine needs a workaround.** A Windows Application Control policy blocks
executing binaries built under the Desktop tree, so cargo's output directory is
redirected outside it in [.cargo/config.toml](.cargo/config.toml). If you move
the repository, or work on a machine without that policy, read the comments in
that file first. Toolchain details and dependency pins are in
[docs/toolchain.md](docs/toolchain.md).

## The rules that shape every change

These are not stylistic preferences. They are the project's correctness
guarantees, and they are what the Python repository's own tests enforce.

1. **Never claim a proof you did not establish.** A rewrite is `proven` only
   when equivalence was actually established. `planner_checked` is not a proof
   and is never treated as one. Unproven output is unproven output.
2. **Never silently approximate.** A construct the parser or a prover cannot
   read produces an explicit `unsupported` / `unproven` status or a typed
   error. Guessing is a defect, because a guessed answer looks identical to a
   correct one from the outside.
3. **A counterexample is not a counterexample until it replays.** Every
   reported difference must be re-run, and confirmed with the engine's
   optimizer disabled, before it is reported.
4. **`unknown` is a real answer.** Where the Python original reports `unknown`,
   this port reports `unknown`. Do not convert an unknown into a pass or a fail.
5. **Behaviour parity, not byte parity.** Semantic equivalence is required;
   identical rendering is required only where the Python rendering is
   reproducible. Document every difference.
6. **The reference repo is read-only.** Verify
   `git -C ../KumoSQL status --porcelain` is empty before declaring a task done.