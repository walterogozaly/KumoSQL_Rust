//! DuckDB execution, BigQuery-to-DuckDB translation, result comparison
//!
//! Ported from `duckdb_load.py`, `bigquery_duckdb.py`, `bigquery_on_duckdb.py`,
//`data_sources.py` and the sample-database loaders.
//
//Comparison is bag (multiset) semantics with documented tie handling, and
//every result difference must be confirmed with DuckDB's optimizer disabled
//before it is reported.
