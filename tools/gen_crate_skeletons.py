"""Generate the KumoSQL_Rust crate skeletons."""

import os

CRATES = {
    "kumosql-sql": (
        "BigQuery/GoogleSQL AST, hybrid parser and SQL rendering",
        [
            "sqlparser = { workspace = true }",
            "serde = { workspace = true }",
            "thiserror = { workspace = true }",
        ],
        """
Ported from the Python modules `ast_utils.py`, `parse_check.py`, `scripts.py`,
`bigquery_syntax.py`, `formatting.py` and `js_literals.py`.

Parsing is hybrid, per the project contract: `sqlparser-rs` provides the base
SQL grammar, and custom extensions handle the BigQuery/GoogleSQL shapes it
cannot read:

  * `UNNEST` and `WITH OFFSET`
  * STRUCT field access and typed struct literals (`STRUCT(1 AS a)`)
  * `SAFE_`-prefixed functions and the BigQuery function catalogue
  * `SELECT * EXCEPT (...)` / `SELECT * REPLACE (...)`
  * BigQuery literal spelling: raw `r''`/`b''` prefixes, `0x`/`0b` literals,
    triple-quoted strings, and GoogleSQL escape rules
  * BigQuery script blocks (`DECLARE`, `SET`, `BEGIN ... END`, `IF`/`WHILE`/`FOR`)
  * interval, range and array syntax, and `QUALIFY`

Any construct the parser cannot read must produce an explicit, typed error.
Silently approximating an unsupported shape is a defect: the whole point of
this project is that a rewrite is either proven or reported as unproven.
""",
    ),
    "kumosql-rules": (
        "Rewrite-rule framework, rule registry and shared driver",
        [
            "kumosql-sql = { workspace = true }",
            "kumosql-verify = { workspace = true }",
            "serde = { workspace = true }",
            "thiserror = { workspace = true }",
            "anyhow = { workspace = true }",
        ],
        """
Ported from the rule infrastructure in `engine.py` and the documented cleanup
rules.

The nine documented rules land here first (task 5):
  `lift_subqueries`, `inline_single_use_ctes`, `remove_trivial_predicates`,
  `remove_redundant_parentheses`, `deduplicate_ctes`, `remove_unused_ctes`,
  `remove_redundant_distinct`, `qualify_columns`, `format_sql`.

The driver owns the behaviour the Python driver owns: SQLX block and
`${...}` interpolation handling, strict parsing with a visible recovery
fallback, formatting, byte-for-byte no-op detection and CTE dependency
checks. `canonical_rule_order()` returns a fixed point, and
`check_idempotence` backs the CLI's exit code 4.
""",
    ),
    "kumosql-verify": (
        "Equivalence verification stack",
        [
            "kumosql-sql = { workspace = true }",
            "kumosql-exec = { workspace = true }",
            "serde = { workspace = true }",
            "serde_json = { workspace = true }",
            "thiserror = { workspace = true }",
            "anyhow = { workspace = true }",
            "rayon = { workspace = true }",
        ],
        """
Ported from `equivalence.py`, `smt_equivalence.py`,
`algebraic_equivalence.py`, `bounded_equivalence.py`,
`conditional_equivalence.py`, `counterexample.py`, `executed_refutation.py`,
`canonical.py` and `canonical_rules.py`.

Five backends, tried in the documented order:

  1. structural   -- canonical-form comparison (task 7)
  2. smt          -- z3, with reported assumptions (task 8)
  3. algebraic    -- algebraic facts, then SQLSolver (task 9)
  4. bounded      -- z3 with a row bound; not a proof (task 10)
  5. executed     -- counterexample search on real databases (task 11)

The status vocabulary is fixed by the contract and must match the Python
original exactly: `unchanged`, `proven`, `planner_checked`, `unproven`,
`failed`, each with `checks[]` entries carrying `kind`, `outcome` and
`detail`. Only `unchanged` and `proven` are trusted.
""",
    ),
    "kumosql-exec": (
        "DuckDB execution, BigQuery-to-DuckDB translation, result comparison",
        [
            "kumosql-sql = { workspace = true }",
            "serde = { workspace = true }",
            "serde_json = { workspace = true }",
            "thiserror = { workspace = true }",
            "anyhow = { workspace = true }",
        ],
        """
Ported from `duckdb_load.py`, `bigquery_duckdb.py`, `bigquery_on_duckdb.py`,
`data_sources.py` and the sample-database loaders.

Comparison is bag (multiset) semantics with documented tie handling, and
every result difference must be confirmed with DuckDB's optimizer disabled
before it is reported.
""",
    ),
    "kumosql-pipeline": (
        "Lineage, pipeline graph, pipeline analysis and incremental models",
        [
            "kumosql-sql = { workspace = true }",
            "kumosql-exec = { workspace = true }",
            "serde = { workspace = true }",
            "serde_json = { workspace = true }",
            "thiserror = { workspace = true }",
            "anyhow = { workspace = true }",
            "walkdir = { workspace = true }",
            "indexmap = { workspace = true }",
        ],
        """
Ported from `graph.py`, the lineage modules, `pipeline.py`,
`pipeline_loading.py`, `table_profile.py`, `incremental.py`,
`incremental_rules.py`, `incremental_scan.py`, `containment.py`,
`model_reuse.py` and shared-model behaviour.

The unknown outcome is load-bearing: where the Python original reports
`unknown`, this crate reports `unknown`. A lineage edge the reference tool
does not claim must not be claimed here either.
""",
    ),
    "kumosql-project": (
        "Change reports, verified refactoring and project minimization",
        [
            "kumosql-sql = { workspace = true }",
            "kumosql-verify = { workspace = true }",
            "kumosql-pipeline = { workspace = true }",
            "serde = { workspace = true }",
            "serde_json = { workspace = true }",
            "thiserror = { workspace = true }",
            "anyhow = { workspace = true }",
            "walkdir = { workspace = true }",
        ],
        """
Ported from `change_report.py`, `ci_check.py`, `refactor.py`,
`consolidate.py`, `table_minimizer.py`, `project_reduction.py`,
`evidence_summary.py` and the saved scopes / equivalence declaration stores.

Refactors are proof-gated: a proposal that is not proven equivalent is
reported as such and never presented as a successful refactor.
""",
    ),
    "kumosql-cost": (
        "Cost model, cardinality estimation and join ordering",
        [
            "kumosql-sql = { workspace = true }",
            "serde = { workspace = true }",
            "serde_json = { workspace = true }",
            "thiserror = { workspace = true }",
            "anyhow = { workspace = true }",
        ],
        """
Ported from `cost_model.py`, `costs.py`, `cost_rules.py` and the Python
`joinorder/` package (`planner.py`, `estimator.py`, `stats.py`,
`predicates.py`, `query.py`, `view_cost.py`).
""",
    ),
    "kumosql-bq": (
        "BigQuery integration, dry-run and Dataform repositories",
        [
            "kumosql-sql = { workspace = true }",
            "kumosql-exec = { workspace = true }",
            "serde = { workspace = true }",
            "serde_json = { workspace = true }",
            "thiserror = { workspace = true }",
            "anyhow = { workspace = true }",
            "walkdir = { workspace = true }",
        ],
        """
Ported from `dryrun.py`, `bigquery_catalog.py`, `catalogs.py`,
`workflow_configs.py`, `git_repo.py`, `github_repo.py` and the Dataform
repository connection features.

Every credentialed path must degrade to an explicit, clear skip message
when credentials are absent. It must never fabricate a dry-run result.
""",
    ),
    "kumosql-eval": (
        "Benchmark harness, results and scoreboard generation",
        [
            "kumosql = { workspace = true }",
            "serde = { workspace = true }",
            "serde_json = { workspace = true }",
            "anyhow = { workspace = true }",
            "walkdir = { workspace = true }",
            "rayon = { workspace = true }",
        ],
        """
Ported from `tools/scoreboard.py`, `tools/eval_diff.py` and the per-eval
harnesses under `benchmarks/` and `tools/`.

The scoreboard is generated from `benchmarks/results/*.json` and is never
hand-edited. Where the Rust port scores differently from the Python
original, the difference is recorded with its reason rather than smoothed
over.
""",
    ),
    "kumosql-ui": (
        "Local browser UI server",
        [
            "kumosql-pipeline = { workspace = true }",
            "kumosql-project = { workspace = true }",
            "axum = { workspace = true }",
            "tokio = { workspace = true }",
            "serde = { workspace = true }",
            "serde_json = { workspace = true }",
            "anyhow = { workspace = true }",
            "dirs = { workspace = true }",
        ],
        """
Ported from the Python `ui` module and `static/`. The static assets are
ported as-is (they are already JavaScript, CSS and HTML); the server is
rewritten in Rust.

Python-only Playwright laptop-replica smoke tests are documented as
intentionally unported, or replaced by an equivalent Rust check -- never
silently dropped.
""",
    ),
    "kumosql-cli": (
        "Command-line interface",
        [
            "kumosql = { workspace = true }",
            "kumosql-bq = { workspace = true }",
            "kumosql-cost = { workspace = true }",
            "kumosql-eval = { workspace = true }",
            "kumosql-project = { workspace = true }",
            "kumosql-ui = { workspace = true }",
            "clap = { workspace = true }",
            "serde_json = { workspace = true }",
            "anyhow = { workspace = true }",
            "walkdir = { workspace = true }",
        ],
        """
Ported from `cli.py` and `console.py`.

Exit codes are part of the contract:

  0   success
  2   a rule failed fatally; nothing is written
  3   output written but not proven equivalent (override: --allow-unproven)
  4   --check-idempotence found a rule changing its own output

All 24 Python console-script commands and their `python -m kumosql`
dispatch forms have a Rust equivalent, with matching flags and `--help`.
""",
    ),
    "kumosql": (
        "Public facade, mirroring the Python package's public API",
        [
            "kumosql-bq = { workspace = true }",
            "kumosql-cost = { workspace = true }",
            "kumosql-exec = { workspace = true }",
            "kumosql-pipeline = { workspace = true }",
            "kumosql-project = { workspace = true }",
            "kumosql-rules = { workspace = true }",
            "kumosql-sql = { workspace = true }",
            "kumosql-verify = { workspace = true }",
            "serde = { workspace = true }",
            "serde_json = { workspace = true }",
            "anyhow = { workspace = true }",
        ],
        """
The facade mirrors `src/kumosql/__init__.py`: the functions and types a
caller reaches for directly, such as `apply_rules`, `apply_rule` and
`lift_subqueries`, re-exported from the implementation crates so that the
Rust API reads like the Python one.
""",
    ),
}

BIN = {"kumosql-cli"}


def main() -> None:
    for name, (role, deps, body) in CRATES.items():
        d = os.path.join("crates", name)
        os.makedirs(os.path.join(d, "src"), exist_ok=True)
        dep_lines = "\n".join("  " + x for x in deps)
        extra = ""
        if name in BIN:
            extra = '\n[[bin]]\nname = "kumosql"\npath = "src/main.rs"\n'
        with open(os.path.join(d, "Cargo.toml"), "w") as f:
            f.write(
                "[package]\n"
                f'name = "{name}"\n'
                "version.workspace = true\n"
                "edition.workspace = true\n"
                "rust-version.workspace = true\n"
                "license.workspace = true\n"
                "repository.workspace = true\n"
                f'description = "{role}"\n'
                "\n[dependencies]\n"
                f"{dep_lines}\n"
                f"{extra}\n"
                "[lints.rust]\n"
                'missing_docs = "warn"\n'
            )
        with open(os.path.join(d, "src", "lib.rs"), "w") as f:
            # Every body line needs its own `//!` continuation, otherwise the
            # prose is parsed as Rust code.
            doc = "\n".join(
                ("//! " if i == 0 else "//") + line
                for i, line in enumerate(body.strip().splitlines())
            )
            f.write(f"//! {role}\n//!\n{doc}\n")
        if name in BIN:
            with open(os.path.join(d, "src", "main.rs"), "w") as f:
                f.write(
                    "//! `kumosql` command-line entry point.\n//!\n"
                    "//! Subcommands are added incrementally as the underlying crates\n"
                    "//! land; see this crate's module documentation for the exit-code\n"
                    "//! contract.\n\n"
                    "fn main() {\n"
                    '    println!("kumosql 0.1.0 (Rust port)");\n'
                    '    println!("subcommands are being ported; see docs/roadmap.md");\n'
                    "}\n"
                )
    print(f"created {len(CRATES)} crates")


if __name__ == "__main__":
    main()