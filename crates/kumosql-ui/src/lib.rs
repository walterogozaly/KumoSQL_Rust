//! Local browser UI server
//!
//! Ported from the Python `ui` module and `static/`. The static assets are
//ported as-is (they are already JavaScript, CSS and HTML); the server is
//rewritten in Rust.
//
//Python-only Playwright laptop-replica smoke tests are documented as
//intentionally unported, or replaced by an equivalent Rust check -- never
//silently dropped.
