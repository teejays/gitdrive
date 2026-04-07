use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::conflict::{ConflictEvent, ConflictResolution, ConflictResolver};
use crate::error::{GitDriveError, Result};
use crate::git::{GitCli, PullResult, PushResult};
use crate::lfs::LfsManager;
use crate::watcher::FileWatcher;

/// Current state of the sync engine.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncState {
    Idle,
    Pushing,
    Pulling,
    Conflicted,
    Error(String),
}

/// The sync engine orchestrates watching, committing, pushing, and pulling.
pub struct SyncEngine {
    git: GitCli,
    config: Config,
    repo_root: PathBuf,
    lfs: LfsManager,
    conflict_resolver: ConflictResolver,
    state: SyncState,
    /// Cached conflict file list from detection, reused during resolution
    pending_conflict_files: Vec<String>,

    conflict_tx: mpsc::Sender<ConflictEvent>,
    resolution_rx: mpsc::Receiver<ConflictResolution>,
}

impl SyncEngine {
    pub fn new(
        config: Config,
        conflict_tx: mpsc::Sender<ConflictEvent>,
        resolution_rx: mpsc::Receiver<ConflictResolution>,
    ) -> Result<Self> {
        let repo_root = config.repo_path.canonicalize()?;
        let git = GitCli::new(repo_root.clone());
        let lfs = LfsManager::new(git.clone());
        let conflict_resolver = ConflictResolver::new(git.clone());
        Ok(Self {
            git,
            config,
            repo_root,
            lfs,
            conflict_resolver,
            state: SyncState::Idle,
            pending_conflict_files: Vec::new(),
            conflict_tx,
            resolution_rx,
        })
    }

    pub fn state(&self) -> &SyncState {
        &self.state
    }

    /// Main sync loop. Runs until the watcher channel closes or a fatal error.
    pub async fn run(&mut self, mut watcher: FileWatcher) -> Result<()> {
        let pull_interval = tokio::time::Duration::from_secs(self.config.pull_interval_secs);
        let mut pull_timer = tokio::time::interval(pull_interval);
        pull_timer.tick().await;

        info!("sync engine started");

        loop {
            tokio::select! {
                batch = watcher.next_batch() => {
                    match batch {
                        Some(paths) => {
                            if self.state == SyncState::Conflicted {
                                warn!("skipping push — conflict pending");
                                continue;
                            }
                            if let Err(e) = self.handle_local_changes(&paths).await {
                                error!("push pipeline error: {e}");
                                self.state = SyncState::Error(e.to_string());
                            }
                        }
                        None => {
                            info!("watcher closed, shutting down sync engine");
                            break;
                        }
                    }
                }

                _ = pull_timer.tick() => {
                    if self.state == SyncState::Conflicted {
                        continue;
                    }
                    if let Err(e) = self.handle_pull().await {
                        error!("pull pipeline error: {e}");
                        self.state = SyncState::Error(e.to_string());
                    }
                }

                resolution = self.resolution_rx.recv() => {
                    if let Some(resolution) = resolution {
                        if let Err(e) = self.handle_resolution(resolution).await {
                            error!("conflict resolution error: {e}");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    // ── push pipeline ────────────────────────────────────────────

    async fn handle_local_changes(&mut self, paths: &[PathBuf]) -> Result<()> {
        self.state = SyncState::Pushing;

        // Auto-track large files with LFS before staging
        self.lfs
            .auto_track_if_needed(paths, self.config.lfs_size_threshold_bytes, &self.repo_root)
            .await?;

        // Stage all changes (modifications + deletions).
        // If the index is locked by a concurrent git op, skip this cycle.
        if let Err(e) = self.git.add_all().await {
            if let GitDriveError::GitCommand { ref stderr, .. } = e {
                if stderr.contains("index.lock") {
                    debug!("index locked, skipping this cycle");
                    self.state = SyncState::Idle;
                    return Ok(());
                }
            }
            return Err(e);
        }

        // Check if there's anything to commit
        let status = self.git.status().await?;
        let has_staged = status.iter().any(|s| s.index != ' ' && s.index != '?');
        if !has_staged {
            self.state = SyncState::Idle;
            return Ok(());
        }

        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        let message = format!(
            "gitdrive: auto-sync from {} at {}",
            self.config.machine_id, timestamp
        );
        self.git.commit(&message).await?;
        self.push_with_retry().await?;

        self.state = SyncState::Idle;
        info!("push complete");
        Ok(())
    }

    async fn push_with_retry(&mut self) -> Result<()> {
        match self.git.push().await? {
            PushResult::Ok => Ok(()),
            PushResult::Rejected => {
                info!("push rejected, pulling first");
                self.do_pull().await?;
                if self.state == SyncState::Conflicted {
                    return Ok(());
                }
                match self.git.push().await? {
                    PushResult::Ok => Ok(()),
                    PushResult::Rejected => {
                        warn!("push still rejected after pull");
                        Err(GitDriveError::Other(
                            "push rejected after pull — manual intervention needed".into(),
                        ))
                    }
                }
            }
        }
    }

    // ── pull pipeline ────────────────────────────────────────────

    async fn handle_pull(&mut self) -> Result<()> {
        // Use `rev-list --left-right --count` to check divergence in a single
        // git command instead of fetch + 3 separate rev-parse/merge-base calls.
        self.git.fetch().await?;

        let out = self
            .git
            .run(&["rev-list", "--left-right", "--count", "HEAD...@{u}"])
            .await?;
        if out.exit_code != 0 {
            // No upstream configured or other issue — skip
            return Ok(());
        }

        let counts: Vec<&str> = out.stdout.split_whitespace().collect();
        if counts.len() != 2 {
            return Ok(());
        }
        let behind: u64 = counts[1].parse().unwrap_or(0);

        if behind == 0 {
            return Ok(());
        }

        self.state = SyncState::Pulling;
        let result = self.do_pull().await;
        if self.state != SyncState::Conflicted {
            self.state = SyncState::Idle;
        }
        result
    }

    async fn do_pull(&mut self) -> Result<()> {
        match self.git.pull_rebase().await? {
            PullResult::Ok => {
                info!("pulled remote changes");
                Ok(())
            }
            PullResult::UpToDate => Ok(()),
            PullResult::Conflict => {
                warn!("merge conflict detected");
                self.state = SyncState::Conflicted;
                if let Some(event) = self.conflict_resolver.detect_conflicts().await? {
                    self.pending_conflict_files = event.conflicted_files.clone();
                    let _ = self.conflict_tx.send(event).await;
                }
                Ok(())
            }
        }
    }

    // ── conflict resolution ──────────────────────────────────────

    async fn handle_resolution(&mut self, resolution: ConflictResolution) -> Result<()> {
        info!(?resolution, "applying conflict resolution");

        let files = std::mem::take(&mut self.pending_conflict_files);

        match resolution {
            ConflictResolution::KeepMine => {
                self.conflict_resolver.resolve_keep_mine(&files).await?;
            }
            ConflictResolution::KeepTheirs => {
                self.conflict_resolver.resolve_keep_theirs(&files).await?;
            }
            ConflictResolution::AbortRebase => {
                self.conflict_resolver.abort_rebase().await?;
            }
        }

        self.state = SyncState::Idle;
        info!("conflict resolved, resuming sync");

        if matches!(
            resolution,
            ConflictResolution::KeepMine | ConflictResolution::KeepTheirs
        ) {
            self.push_with_retry().await?;
        }

        Ok(())
    }
}
