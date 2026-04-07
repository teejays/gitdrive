# GitDrive

Dropbox-like file sync CLI backed by Git + Git LFS. Watches `~/gitdrive/`, auto-commits, and syncs to `github.com/<user>/gitdrive`.

## Project Structure

Cargo workspace with two crates:

- `crates/gitdrive-core/` — Library. All sync logic, no UI.
- `crates/gitdrive-cli/` — Binary. CLI commands (`init`, `watch`, `status`), depends on core.

## Core Modules (`gitdrive-core/src/`)

- `git.rs` — Git CLI wrapper. Shells out to `git` (not libgit2) for full LFS support. All git operations go through `run()` / `run_checked()` which are `pub(crate)`.
- `sync.rs` — Sync engine. `tokio::select!` loop over file watcher events, pull timer, and conflict resolution channel. Owns the push/pull pipelines.
- `watcher.rs` — FSEvents file watcher via `notify` crate. Debounces events (100ms default), filters `.gitignore` and `.git/` internal paths.
- `lfs.rs` — LFS management. Default `.gitattributes` with 59 binary extensions. Auto-detects large files (>10MB) and adds LFS tracking by extension.
- `conflict.rs` — Conflict detection (unmerged files from `git status`) and resolution (keep mine/theirs/abort). Communicates with the UI layer via channels.
- `sparse.rs` — Sparse checkout wrapper (cone mode). Not yet wired into the CLI.
- `config.rs` — `~/.gitdrive/config.toml` parsing with serde. `Config::new()` fills defaults.
- `error.rs` — `GitDriveError` enum with `thiserror`.

## Key Design Decisions

- **Shells out to `git` CLI** rather than using `git2`/`gitoxide` because neither supports Git LFS natively.
- **Single `main` branch** across all machines. No per-device branches. Machine identity is tracked via `machine_id` in commit messages.
- **Standardized paths**: local folder is always `~/gitdrive/`, remote is always `github.com/<user>/gitdrive`.
- **`SyncEngine::new()` returns `Result`** because it canonicalizes the repo path at construction time (once, not per-cycle).
- **Conflict resolution via channels**: `SyncEngine` sends `ConflictEvent` out, receives `ConflictResolution` back. The CLI uses stdin/stderr; a future Tauri UI would use notifications.

## Building & Testing

```bash
cargo build                    # build both crates
cargo test                     # run unit tests
cargo run --bin gitdrive-cli   # run the CLI
RUST_LOG=gitdrive=debug cargo run --bin gitdrive-cli -- watch  # verbose
```

## Releasing

Tag a version to trigger the GitHub Actions release workflow:

```bash
git tag v0.1.0
git push origin v0.1.0
```

Builds macOS (arm64 + x86_64) and Linux (x86_64) binaries, attaches to a GitHub Release.

## Style

- No unnecessary abstractions. Helpers only when used in 2+ places.
- `pub(crate)` for internal cross-module access, not duplicate methods.
- Errors should be structured (`GitDriveError` variants), not stringly-typed.
- Avoid spawning git processes on hot paths when a cheaper check exists (e.g. `rev-list --left-right --count` instead of 3 separate rev-parse calls).
