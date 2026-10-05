# Parity notes

The running log of differences between this Rust port and the Python original,
and of decisions taken during the port.

Nothing is removed from this file. If a difference turns out to be a mistake on
the Rust side, the entry stays and is updated to say so -- the point is an
honest record, not a clean-looking one.

## Known environment differences

### The Application Control policy blocks execution under Desktop

Building succeeds; executing a binary built under `C:\Users\walte\Desktop\...`
fails with `An Application Control policy has blocked this file. (os error
4551)`. Workaround: `.cargo/config.toml` redirects cargo's output directory to
`%TEMP%`. See [toolchain.md](toolchain.md).

### `static.rust-lang.org` is unreliable on this host

Connections are forcibly closed partway through downloads (`os error 10054`).
`rustup` operations need retries, and a repository-level
`rust-toolchain.toml` makes every cargo invocation attempt a channel sync that
can fail. Hence no `rust-toolchain.toml`; the version is pinned in
[toolchain.md](toolchain.md) and `Cargo.lock`.

### `crates.io`'s API endpoint is blocked; the registry itself is not

`https://crates.io/api/v1/crates/<name>` does not respond on this host, but
`cargo add` and normal dependency resolution work. Versions are therefore
resolved by cargo and read from `Cargo.lock`, not queried by hand.

## Design decisions that create behavioural difference

### Parsing is hybrid rather than a sqlglot equivalent

**Status:** decided, 2026-10-05.

The Python original pins `sqlglot==30.21.0` and treats that pin as a hard
contract. No Rust crate is a drop-in for sqlglot, and the whole Python codebase
is built on sqlglot's BigQuery AST and its exact reading of GoogleSQL. This
port uses `sqlparser-rs` as the base grammar and adds custom BigQuery
extensions for what it cannot read.

Consequence: where `sqlparser-rs` and `sqlglot` read the same text differently,
this port follows `sqlparser-rs` and **must** document each such case rather than
tolerate a silent divergence. The Python project has already hit several of
these by hand -- for example the Python translator now explicitly declines
comparison chains such as `t.x = -1 IS NULL` without parentheses, because
sqlglot and BigQuery read them differently. Expect more of these.

### No `sqlfluff` peer, so `format_sql` is a reimplementation

**Status:** decided, 2026-10-05.

The Python `format_sql` is sqlfluff behind a wrapper, and its eval page says so
plainly: reproducing sqlfluff's fix mostly tests the wrapper. The Rust port has
to implement formatting itself plus the saved-preferences layer and the
independent `proof_format` layout check.

Consequence: formatted output will differ from Python's in cosmetic ways that
are expected and numerous. The contract is behaviour parity, so this is
acceptable -- but it must be visible, and the layout check must stay genuinely
independent of the formatter so that it can still catch a formatter that
changes meaning.

### CLI exit codes are part of the interface

0 success, 2 fatal rule failure (nothing written), 3 output written but not
proven (`--allow-unproven` overrides), 4 `--check-idempotence` violation. These
are asserted by the task 18 parity matrix.

## Python-only components

Recorded here so they are never silently dropped. Each must end up either
ported or explicitly declared unported with a reason.

| Component | Python location | Disposition |
| --- | --- | --- |
| Playwright laptop-replica smoke test | `smoke.py`, `pyproject.toml` `smoke` extra | Undecided (task 19) |
| `sqlglot` dialect behaviour | dependency | Superseded by the hybrid parser, see above |
| `sqlfluff` formatting engine | dependency | Reimplemented, see above |
| Python `z3-solver` bindings | dependency | Reimplemented against the `z3` crate (task 8) |

## Log

| Date | Entry |
| --- | --- |
| 2026-10-05 | Repository created. Workspace and 12 crate skeletons in place. No behaviour ported yet. |
| 2026-10-05 | Ported `string_literals` (task 2, partial). `canonical_literals` and `invalid_literal` match the Python original on all 11 of its own cases. |
| 2026-10-05 | **Method note.** Test cases are generated from the reference repo, never transcribed by hand. `tools/dump_literal_cases.py` parses the Python test file with `ast` and self-checks each case against the Python implementation. Hand-transcribing `tests/test_string_literals.py` corrupted two cases silently -- `Rb'q\"'` lost its backslash, and `SELECT "a\"b", "x\"` lost its trailing one. Both produced plausible-looking but wrong test data. Any further ported test table should be generated the same way. |
| 2026-10-05 | The Python original indexes `str` by character, and its literal scanning depends on that (a `\\` advances two characters). The Rust port scans a `Vec<char>` for that reason, so non-ASCII input behaves identically. A byte-slice port would have diverged. |
| 2026-10-05 | **`"x"` is a string in BigQuery and a quoted identifier in `GenericDialect`.** Unlike the other divergences, which are gaps (a parser cannot read a shape), this one is the *opposite reading*: trusting the dialect would model a different query without any error. `rewrite_double_quoted_strings` converts the delimiter before parsing. Worth checking for any other construct where `sqlparser-rs` and BigQuery actively disagree. |
| 2026-10-05 | `FILTER (WHERE p)` is a field on `Function` in `sqlparser-rs` 0.59, not an expression node, and the window specification lives on `Function.over`. So both are folded into kumosql-sql's own nodes during conversion. Recorded because the 0.5x shape differs and any later port reading older examples will get this wrong. |
| 2026-10-05 | Recovery fallback is **not implemented**. `parse_statements(sql, recover = true)` returns an error saying so rather than a partial result, because a partial result would look like success. `parse_check`'s recovery behaviour is still to come. |