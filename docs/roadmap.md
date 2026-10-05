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

* **`parse`** (`kumosql-sql/src/parse.rs`) -- the pipeline end to end:
  text -> `canonical_literals` -> `rewrite_all` -> `sqlparser-rs` -> the
  kumosql AST, and back out through `render_statements`. The conversion step is
  what makes the modelled subset explicit: an expression outside it becomes
  `Unmodeled` naming the construct, rather than a node that survives into a
  rewrite and quietly changes what the rule meant.

67 tests pass across the workspace.

Three real bugs the parse tests caught, all now pinned by tests:

1. `rewrite_all` rewrote the aggregate filter and then immediately resolved it,
   undoing its own work before the parser ran. Rewriting and resolving are now
   separate functions.
2. The marker leaked into the AST, so rendering emitted
   `COUNT(x WHERE c) (WHERE __KUMO_AGG_FILTER__(c))`. Since the AST already
   models the filter as a node, `Expr::Filter` now renders BigQuery's own
   spelling and the marker is purely an internal parse trick, unwrapped during
   conversion.
3. `"text"` was read as a quoted *identifier*, because `GenericDialect` treats
   double quotes as identifier quotes. This is the one case where `sqlparser`'s
   reading is the **opposite** of BigQuery's rather than merely a gap, so the
   delimiter is rewritten before parsing.

`sqlparser-rs` 0.59 moved several things relative to 0.5x, which cost real time
to discover: `ORDER BY`/`LIMIT` live on `Query` not `Select`, `FILTER` is a
field on `Function` not a node, windows live on `Function.over`, `ObjectName`
parts are an enum, and `Query` is a struct not an enum.

* **`normalize`** (`kumosql-sql/src/normalize.rs`) -- `expand_group_by_all`,
  which spells `GROUP BY ALL` as the keys it infers before anything reasons
  about it, plus the expression predicates it needs (aggregate / window /
  subquery / column / unknown-function).
* **`scripts`** (`kumosql-sql/src/scripts.rs`) -- the BigQuery script
  splitter. A script is not SQL (`DECLARE`, `SET`, `BEGIN ... END`,
  `IF ... THEN`, `FOR ... IN`, `CREATE PROCEDURE`), so it is split first and
  each statement parsed on its own. Splitting is not flattening: a statement
  inside `IF` or a procedure carries that on its `ScriptPart`.

* **`rewrites`** (`kumosql-sql/src/rewrites.rs`) -- the remaining
  `bigquery_syntax.py` rewrites: `LIKE ALL/SOME UNNEST`, the `WITH(a AS 1, ...)`
  named-expression form, a `TABLE name` argument to a table-valued function, and
  `DROP TABLE FUNCTION`. New AST nodes came with them: `LikeQuantifier`,
  `Expr::WithExpr`, `Expr::TableArg`, `TableFactor::TableFunction`.

117 tests pass across the workspace.

**`sqlparser` silently drops the arguments of a table function in FROM.**
`FROM ds.fn(arg, ...)` parses as a bare table named `ds.fn` with the call's
arguments discarded and *no error reported*. That is the worst failure mode
available to this project: a truncated query that looks fine. The parser
therefore compares whether the `TABLE` marker survived conversion and refuses
the query when it did not, rather than returning a table it silently trimmed.
Recorded as a known gap below.

* **`parse_check`** (`kumosql-sql/src/parse_check.rs`) -- checks that the parse
  accounts for the whole query, in the spirit of `parse_check.py`.

  The Python module is ~2,000 lines because it compares `sqlglot`'s reading with
  a hand-written precedence parser for three dialects. This port keeps the
  property that module exists to protect and checks it against our own tree: the
  **atoms** of the query (identifiers, numbers, strings) are collected from the
  source tokens and from the parsed tree, and the two multisets must be equal.

  That catches the two failures that produce a query which *looks* fine:

  1. a **dropped token** -- the parser read part of the query and discarded the
     rest without complaining, which is exactly the table-function bug above;
  2. an **invented name** -- the tree holds a name the source never spells.

  The comparison runs on the *rewritten* text, so a marker or a canonicalised
  literal on one side only would not read as a disagreement; `ParseCheck::note`
  says which text was compared. `Unchecked` is kept distinct from `Disagree`,
  because "we could not look at this" is a different claim from "these differ",
  and `disagrees()` never reports the first as the second.

132 tests pass in `kumosql-sql`.

Still to do for task 2: the pipe operator rewrites.

The script tests are ported from `tests/test_scripts.py` **with its exact
expected statement texts and conditional flags**, because a splitter one
statement off hands a caller a different set of queries to verify. Five real
bugs were caught that way, none of which my own reading had predicted:

1. statement text included the trailing `;` -- Python's `simple()` stops before it
2. a comment between statements was absorbed into the next one, shifting both
   its text and its reported line. The script lexer drops comments, which is why
   the Python original's does; the shared tokenizer keeps them, which is right
   for the rewrite layer
3. `BEGIN TRANSACTION` was read as a block rather than a statement
4. a procedure's `OPTIONS(...)`, and a `LANGUAGE js` body with no `BEGIN`,
   left the splitter inside the procedure forever
5. labels, a missing final semicolon, and stray keywords at top level

Still to do for task 2: `parse_check` (`check_query` / `reading`) and the pipe
operator rewrites.

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
- **`sqlparser` cannot represent `DROP TABLE FUNCTION`.** It reads the shape as a
  drop of the table named `FUNCTION` and then refuses the real name, so the
  rewrite that works for `sqlglot` is a no-op here. The statement has to come
  through the command/splitter path instead of the parse. Declined for now.
- **`sqlparser` drops a table function's arguments in FROM.** See above. The
  parser refuses rather than returning the truncated query; the proper fix is a
  different strategy for that shape, not a looser check.