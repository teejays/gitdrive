# gitdrive

[![CI](https://github.com/teejays/gitdrive/actions/workflows/ci.yml/badge.svg)](https://github.com/teejays/gitdrive/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/teejays/gitdrive)](https://github.com/teejays/gitdrive/releases)

Dropbox-like file sync backed by Git + Git LFS. Watches a local folder, auto-commits changes, and syncs to a GitHub repo — giving you cloud sync with full version history.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/teejays/gitdrive/main/install.sh | sh
```

Supports macOS (Apple Silicon + Intel) and Linux (x86_64). Installs to `/usr/local/bin/gitdrive`.

### Prerequisites

- [Git](https://git-scm.com/)
- [Git LFS](https://git-lfs.com/) — `brew install git-lfs` (macOS) or `apt install git-lfs` (Linux)
- [GitHub CLI](https://cli.github.com/) — for auto-detecting your GitHub username during init
- SSH key configured with GitHub

### Build from source

```bash
git clone git@github.com:teejays/gitdrive.git
cd gitdrive
cargo install --path crates/gitdrive-cli
```

## Usage

### Initialize

```bash
gitdrive init
```

This will:
- Detect your GitHub username via `gh` CLI
- Create `~/gitdrive/` if it doesn't exist
- Set up the git repo with `github.com/<you>/gitdrive` as the remote
- Configure Git LFS for 59 common binary extensions
- Push an initial commit

You can also specify a username explicitly:

```bash
gitdrive init --user myusername
```

### Start syncing

```bash
gitdrive watch
```

Runs a background daemon that:
- Watches `~/gitdrive/` for file changes (create, modify, delete)
- Auto-commits ~100ms after edits stop (debounced)
- Pushes to GitHub immediately
- Pulls remote changes every 20 seconds
- Prompts for conflict resolution when needed

For verbose output: `RUST_LOG=gitdrive=debug gitdrive watch`

### Check status

```bash
gitdrive status
```

Shows pending local changes and sync state with the remote.

## Multi-machine setup

On a second machine:

```bash
git clone git@github.com:<you>/gitdrive.git ~/gitdrive
gitdrive init
gitdrive watch
```

Both machines sync through the same GitHub repo.

## Conflict resolution

If the same file is edited on two machines, the daemon pauses and prompts:

```
--- MERGE CONFLICT ---
  - path/to/file.txt
  [1] Keep mine
  [2] Keep theirs
  [3] Abort rebase
  Choice:
```

Sync resumes after you choose.

## Git LFS

Large and binary files are handled automatically via Git LFS:

- 59 binary extensions tracked by default (images, video, audio, archives, fonts, PDFs, etc.)
- Files over 10MB with untracked extensions are auto-detected and added
- All config lives in `.gitattributes` in the repo

## Configuration

Stored at `~/.gitdrive/config.toml`:

```toml
repo_path = "/Users/you/gitdrive"
remote_url = "git@github.com:you/gitdrive.git"
branch = "main"
pull_interval_secs = 20
debounce_ms = 100
lfs_size_threshold_bytes = 10485760
machine_id = "my-macbook"
```

## How it works

gitdrive shells out to the `git` CLI (not libgit2) so that Git LFS works natively. The sync engine uses a `tokio::select!` loop over three event sources:

1. **File watcher** (FSEvents/inotify via `notify` crate) — detects local changes, debounces, filters `.gitignore`
2. **Pull timer** — fetches and rebases remote changes every N seconds
3. **Conflict channel** — receives user resolution decisions

Commits are tagged with the machine ID and timestamp for traceability.
