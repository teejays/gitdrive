use tracing::{debug, info};

use crate::error::Result;
use crate::git::GitCli;

/// Manages git sparse-checkout for selective sync on a per-machine basis.
pub struct SparseCheckout {
    git: GitCli,
}

impl SparseCheckout {
    pub fn new(git: GitCli) -> Self {
        Self { git }
    }

    /// Initialize sparse-checkout in cone mode (fast, directory-based).
    pub async fn enable_cone_mode(&self) -> Result<()> {
        self.git
            .run_checked(&["sparse-checkout", "init", "--cone"])
            .await?;
        info!("sparse-checkout enabled in cone mode");
        Ok(())
    }

    /// Set the sparse-checkout to exactly these paths (replaces any previous selection).
    pub async fn set_paths(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            // Setting to "." means check out everything
            self.git
                .run_checked(&["sparse-checkout", "set", "."])
                .await?;
        } else {
            let mut args: Vec<&str> = vec!["sparse-checkout", "set"];
            let refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
            args.extend(refs);
            self.git.run_checked(&args).await?;
        }
        info!(paths = ?paths, "sparse-checkout paths set");
        Ok(())
    }

    /// Add additional paths to the sparse-checkout (keeps existing selection).
    pub async fn add_path(&self, path: &str) -> Result<()> {
        self.git
            .run_checked(&["sparse-checkout", "add", path])
            .await?;
        info!(path = %path, "added to sparse-checkout");
        Ok(())
    }

    /// List currently checked-out sparse paths.
    pub async fn list_paths(&self) -> Result<Vec<String>> {
        let out = self.git.run(&["sparse-checkout", "list"]).await?;
        if out.exit_code != 0 {
            // sparse-checkout might not be enabled
            debug!("sparse-checkout list returned non-zero — may not be enabled");
            return Ok(vec![]);
        }
        Ok(out
            .stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect())
    }

    /// List all directories in the repo (including those not checked out).
    /// Useful for showing the user what's available to sync.
    pub async fn list_available_dirs(&self) -> Result<Vec<String>> {
        let out = self
            .git
            .run(&["ls-tree", "-d", "--name-only", "-r", "HEAD"])
            .await?;
        if out.exit_code != 0 {
            return Ok(vec![]);
        }
        Ok(out
            .stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect())
    }

    /// Disable sparse-checkout (check out everything).
    pub async fn disable(&self) -> Result<()> {
        self.git
            .run_checked(&["sparse-checkout", "disable"])
            .await?;
        info!("sparse-checkout disabled");
        Ok(())
    }
}
