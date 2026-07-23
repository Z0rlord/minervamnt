# AGENTS.md

## Cursor Cloud specific instructions

Minerva Mint is a single Rust service (`minerva-mint`, axum HTTP API) — an Ark-backed
Cashu mint. There is no separate frontend to run for development (the `landing/`
directory is a static page deployed independently via Cloudflare Pages).

### Toolchain / build gotchas
- Requires **Rust >= 1.88**. `Cargo.lock` pins dependencies (`jsonwebtoken 10.4`,
  `tonic 0.14.6`, `time 0.3.49`, plus `edition2024` crates) that will not build on
  older toolchains. The startup update script runs `rustup default stable`; if a
  build fails with an `edition2024` or "requires rustc 1.88" error, check that the
  active toolchain is stable (`rustc --version`), not the base image's 1.83.
- `protoc` (protobuf-compiler) is required to build `cdk-signatory`. It is installed
  in the VM image; if a build fails with "Could not find `protoc`", install
  `protobuf-compiler` via apt.

### Running the service
- Standard commands are in `README.md` (`cargo build` / `cargo test` / `cargo run`).
  Default dev mode uses a **mock ASP + mock signatory**, so no external services
  (Bitcoin Core, Ark ASP, cdk-signatory) are needed to run or test.
- The app auto-creates the SQLite data dir (`data/` from `config.toml`, gitignored)
  on startup, so no manual `mkdir` is required.
- The server listens on `0.0.0.0:3338` by default; override with `BIND_ADDR=host:port`
  (used by the deploy scripts to bind to a Tailscale IP).
- `/health` reports a `bitcoin_rpc_error` in dev mode because no Bitcoin Core RPC is
  running — this is expected and does not mean the mint is down (`status` is still
  `ok` and `ark_connected` is `true`).

### Lint
- `cargo fmt --check` and `cargo clippy -- -D warnings` (per README) currently report
  pre-existing findings on modern toolchains (rustfmt call-wrapping style diffs and
  clippy `dead_code`/`redundant_closure` warnings). These are pre-existing in the
  tree, not environment issues; do not "fix" them as part of unrelated work.
