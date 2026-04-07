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
            .filter(|s| {
                s.index == 'U' || s.worktree == 'U' || (s.index == 'A' && s.worktree == 'A')
            })
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

    /// Resolve by keeping the local (user's) version of all conflicted files.
    /// During rebase, local commits are "theirs" (being replayed onto the upstream).
    pub async fn resolve_keep_mine(&self, files: &[String]) -> Result<()> {
        if !files.is_empty() {
            let mut args: Vec<&str> = vec!["checkout", "--theirs", "--"];
            args.extend(files.iter().map(|f| f.as_str()));
            self.git.run_checked(&args).await?;
        }
        self.git.add_all().await?;
        self.git.run_checked(&["rebase", "--continue"]).await
    }

    /// Resolve by keeping the remote version of all conflicted files.
    /// During rebase, the upstream is "ours" (the branch being rebased onto).
    pub async fn resolve_keep_theirs(&self, files: &[String]) -> Result<()> {
        if !files.is_empty() {
            let mut args: Vec<&str> = vec!["checkout", "--ours", "--"];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_detect_no_conflicts_clean_repo() {
        let dir = tempfile::tempdir().unwrap();
        let git = GitCli::new(dir.path().to_path_buf());
        git.init().await.unwrap();

        std::fs::write(dir.path().join("f.txt"), "x").unwrap();
        git.add_all().await.unwrap();
        git.commit("c1").await.unwrap();

        let resolver = ConflictResolver::new(git);
        let result = resolver.detect_conflicts().await.unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_conflict_resolution_variants() {
        // Ensure all variants can be constructed and debugged
        let variants = vec![
            ConflictResolution::KeepMine,
            ConflictResolution::KeepTheirs,
            ConflictResolution::AbortRebase,
        ];
        for v in &variants {
            let _ = format!("{:?}", v);
        }
    }

    #[test]
    fn test_conflict_event_clone() {
        let event = ConflictEvent {
            conflicted_files: vec!["a.txt".into(), "b.txt".into()],
            timestamp: chrono::Utc::now(),
        };
        let cloned = event.clone();
        assert_eq!(cloned.conflicted_files, event.conflicted_files);
    }
}
