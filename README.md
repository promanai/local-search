# LocalSearch

LocalSearch is a Windows-first local search engine for fast filename, path, and explicitly opt-in
plaintext content search across large filesystems.

The v0.1 implementation includes:

- a durable SQLite filesystem graph and independent Tantivy catalog/content indexes;
- restart-safe projection, reconciliation, bounded maintenance, and resource governance;
- a secured per-user Agent API used by the CLI, MCP adapter, and Tauri desktop client;
- an authenticated metadata-only Windows filesystem broker for explicit development-mode USN
  observation; public v0.1 discovery remains under the current user's token;
- reproducible Windows packaging, guarded install/uninstall operations, and redacted diagnostics.

The project is in release hardening. Core engineering gates and the fail-closed multi-user policy
pass, while physical Windows lifecycle, long-running load, second-user isolation, signed-artifact,
and UX evidence remain conditional. No prebuilt release artifact is published yet.

## Build and verify

Prerequisites are Windows, the Rust toolchain declared in `rust-toolchain.toml`, Node.js for Desktop
frontend tests, and PowerShell 5.1 or newer for package contracts.

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
npm test --prefix crates/desktop
.\packaging\test-windows-package.ps1
node .\packaging\test-dependency-policy.mjs
```

## Documentation

- [Architecture](ARCHITECTURE.md)
- [Implementation plan](IMPLEMENTATION_PLAN.md)
- [Design and evidence index](docs/README.md)
- [Project readiness checklist](docs/PROJECT-READINESS-CHECKLIST.md)
- [Windows packaging lifecycle](docs/START-018-WINDOWS-PACKAGING.md)

## Privacy and publication policy

LocalSearch indexes local metadata and explicitly selected content. Search source text, snippets,
backend scores, local databases, generated indexes, and private benchmark reports are not part of
the public repository or public API.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
