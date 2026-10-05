# Repository instructions for coding agents

## The Python repository is read-only

`C:/Users/walte/Desktop/Repos/KumoSQL` is the reference implementation. **Do not
edit it, do not run formatters over it, and do not commit into it.** Read it,
copy out of it, and leave it alone. Before declaring any task complete, run:

```shell
git -C ../KumoSQL status --porcelain
```

It must be empty. A dirty reference repo invalidates the parity evidence for
every task in the batch, because it becomes impossible to tell a deliberate
port difference from an accidental edit.

## The goal contract

Work is organised as the 21 tasks in [docs/roadmap.md](docs/roadmap.md). Each
task states its own verification contract; the overall gate is
[README.md](README.md)'s success criteria plus
[docs/parity-notes.md](docs/parity-notes.md).

If something blocks you, **do not stop the session.** Record the blocker (what
is blocked, the exact error, what would unblock it) in
[docs/parity-notes.md](docs/parity-notes.md), commit the partial progress, and
continue on every task that is not blocked. Only halt if genuinely nothing can
proceed. Never invent evidence to get around a blocker.

## Correctness rules

These come from the Python project's own safeguards and are checked by its test
suite. Violating them is the failure mode this whole project exists to avoid.

- **No proof without evidence.** `proven` requires an established proof.
  `planner_checked` is not proof and must not be reported as such. A failed
  planner check is reported inside `unproven`, never as a pass.
- **No silent approximation.** Unsupported SQL, unmodelled semantics and
  missing credentials produce an explicit `unsupported` / `unproven` status or
  a typed error. Never degrade to a plausible-looking guess.
- **Counterexamples must replay.** A reported difference is re-executed, and
  confirmed with the engine's optimizer disabled, before it counts.
- **`unknown` stays `unknown`.** Do not promote an unknown to proven or demote
  it to refuted to make a score look better.
- **No fabricated dry runs.** Credentialed BigQuery features skip with a clear
  message when credentials are absent.
- **Scoreboards are generated, never typed.** The benchmark table comes from
  `benchmarks/results/*.json` by script. Editing the table by hand is a defect.

## Tests

- Tests must be parallel-safe: no shared files, no module-level mutable state.
  The Python suite enforces this and its CI runs `-n auto`.
- Port tests alongside behaviour, in the same change. A ported module with no
  ported test is unfinished.
- If a Python test has no Rust counterpart, say so explicitly in
  [docs/parity-notes.md](docs/parity-notes.md). Do not quietly drop it.

## Style

- Rust stable only. No nightly features without asking.
- Exact dependency versions are pinned in `Cargo.lock`; the notable pins are
  recorded in [docs/toolchain.md](docs/toolchain.md).
- `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings` and
  `cargo fmt --check` must all pass before a task is called done.
- Every crate's `src/lib.rs` names the Python modules it owns. Keep that current.
- Public items are documented (`missing_docs` is set to `warn`); the docs are
  load-bearing, not decoration.