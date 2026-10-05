//! BigQuery integration, dry-run and Dataform repositories
//!
//! Ported from `dryrun.py`, `bigquery_catalog.py`, `catalogs.py`,
//`workflow_configs.py`, `git_repo.py`, `github_repo.py` and the Dataform
//repository connection features.
//
//Every credentialed path must degrade to an explicit, clear skip message
//when credentials are absent. It must never fabricate a dry-run result.
