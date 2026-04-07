use crate::error::Result;
use crate::git::GitCli;

/// Describes a set of conflicting files detected during pull/rebase.
#[derive(Debug, Clone)]
pub struct ConflictEvent {
    pub conflicted_files: Vec<String>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// How the user wants to resolve the conflict.
#[derive(Debug, Clone)]
pub enum ConflictResolution {
    KeepMine,
    KeepTheirs,
    AbortRebase,
}

/// Detects and resolves merge conflicts.
pub struct ConflictResolver {
    git: GitCli,
}

impl ConflictResolver {
    pub fn new(git: GitCli) -> Self {
        Self { git }
    }

    /// Check for unmerged files after a failed rebase/merge.
    pub async fn detect_conflicts(&self) -> Result<Option<ConflictEvent>> {
        let statuses = self.git.status().await?;
        let unmerged: Vec<String> = statuses
            .iter()
            .filter(|s| s.index == 'U' || s.worktree == 'U' || (s.index == 'A' && s.worktree == 'A'))
            .map(|s| s.path.clone())
            .collect();

        if unmerged.is_empty() {
            Ok(None)
        } else {
            Ok(Some(ConflictEvent {
                conflicted_files: unmerged,
                timestamp: chrono::Utc::now(),
            }))
        }
    }

    /// Resolve by keeping our version of all conflicted files.
    pub async fn resolve_keep_mine(&self, files: &[String]) -> Result<()> {
        if !files.is_empty() {
            let mut args: Vec<&str> = vec!["checkout", "--ours", "--"];
            args.extend(files.iter().map(|f| f.as_str()));
            self.git.run_checked(&args).await?;
        }
        self.git.add_all().await?;
        self.git.run_checked(&["rebase", "--continue"]).await
    }

    /// Resolve by keeping their version of all conflicted files.
    pub async fn resolve_keep_theirs(&self, files: &[String]) -> Result<()> {
        if !files.is_empty() {
            let mut args: Vec<&str> = vec!["checkout", "--theirs", "--"];
            args.extend(files.iter().map(|f| f.as_str()));
            self.git.run_checked(&args).await?;
        }
        self.git.add_all().await?;
        self.git.run_checked(&["rebase", "--continue"]).await
    }

    /// Abort the in-progress rebase entirely.
    pub async fn abort_rebase(&self) -> Result<()> {
        self.git.run_checked(&["rebase", "--abort"]).await
    }
}
