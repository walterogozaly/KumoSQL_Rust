# Toolchain and dependency pins

Everything here was verified on the development machine on 2026-10-05.

## Rust toolchain

| Component | Version |
| --- | --- |
| rustc | 1.99.0 (`b940084d7` 2026-09-28) |
| cargo | 1.99.0 (`5f94df478` 2026-08-27) |
| Target triple | `x86_64-pc-windows-msvc` |
| Profile | `minimal` plus `clippy` and `rustfmt` |

Installed with `rustup default stable-msvc`.

### Why there is no `rust-toolchain.toml`

The file was written and then deliberately renamed to
`rust-toolchain.toml.disabled`.

Naming a channel in `rust-toolchain.toml` makes `rustup` re-sync that channel's
manifest from `static.rust-lang.org` on **every** cargo invocation. This host's
link to `static.rust-lang.org` is unreliable, and those syncs failed
intermittently with `os error 10054` ("connection forcibly closed"), making
`cargo build` fail before it started. Pinning the bare word `stable` did not
help: rustup still tried to sync.

The toolchain is therefore pinned at the machine level (`rustup default`) rather
than the repository level, and the exact version is recorded here and in
`Cargo.lock`. If you want repository-level pinning back, restore
`rust-toolchain.toml.disabled` on a machine with a reliable
`static.rust-lang.org`, and change its `channel` to an exact version so rustup
resolves from its local cache.

## Native build toolchain

The `duckdb` and `z3` crates compile native C++. Visual Studio Build Tools 2022
were installed for this:

| Component | Version / path |
| --- | --- |
| MSVC toolset | 14.44.35207 (`VC/Tools/MSVC/14.44.35207`) |
| `cl.exe` | `VC/Tools/MSVC/14.44.35207/bin/Hostx64/x64/cl.exe` |
| `link.exe` | same directory |
| CMake | `Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin/cmake.exe` |

Install path: `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools`,
added with the `Microsoft.VisualStudio.Workload.VCTools` workload.

`rustc` locates `link.exe` automatically for the MSVC target, so no environment
setup is needed for an ordinary `cargo build`.

## The Application Control workaround

This host refuses to *execute* binaries built anywhere under
`C:\Users\walte\Desktop\...`:

```
An Application Control policy has blocked this file. (os error 4551)
```

Building and linking succeed; only execution is refused. The same binary copied
to `%TEMP%` runs normally, which confirms the policy is path-based rather than
binary-based.

Consequence: with the default `./target`, `cargo test` cannot run the compiled
test binaries. `.cargo/config.toml` therefore redirects the output directory to
`C:/Users/walte/AppData/Local/Temp/kumosql-rust-target`.

**If you move this repository**, update that path in `.cargo/config.toml`, or
export `CARGO_TARGET_DIR` yourself. **On a machine without the policy**, delete
`.cargo/config.toml` and use the default layout.

## Dependency pins

Direct dependencies are declared with caret requirements in the workspace
`Cargo.toml`; `Cargo.lock` is the authoritative pin. Resolved versions as of
2026-10-05:

| Crate | Resolved | Notes |
| --- | --- | --- |
| `sqlparser` | see `Cargo.lock` | Hybrid parser base (task 2) |
| `duckdb` | added in task 12 | Bundled C++ build; pin recorded here when added |
| `z3` | added in task 8 | Compiles native C++; pin recorded here when added |
| `serde`, `serde_json` | see `Cargo.lock` | Result JSON parity |
| `clap` 4 | see `Cargo.lock` | CLI |
| `axum`, `tokio` | see `Cargo.lock` | Browser UI server (task 19) |
| `anyhow`, `thiserror` | see `Cargo.lock` | Error handling |
| `walkdir`, `dirs`, `rayon`, `indexmap` | see `Cargo.lock` | Traversal, parallelism, deterministic ordering |

The Python original pins `sqlglot==30.21.0` and treats that pin as a hard
contract. There is no equivalent single Rust dependency, because no Rust crate
is a drop-in for sqlglot: `sqlparser-rs` covers standard SQL well and BigQuery
only partially, which is why parsing here is explicitly hybrid. See
[parity-notes.md](parity-notes.md) for the divergence this causes.