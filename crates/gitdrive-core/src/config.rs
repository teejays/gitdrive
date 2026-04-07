use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::{GitDriveError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Local folder to sync
    pub repo_path: PathBuf,

    /// GitHub remote URL (SSH or HTTPS)
    pub remote_url: String,

    /// Branch to sync (default: "main")
    #[serde(default = "default_branch")]
    pub branch: String,

    /// How often to pull from remote, in seconds
    #[serde(default = "default_pull_interval")]
    pub pull_interval_secs: u64,

    /// Debounce window for file watcher, in milliseconds
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,

    /// Files larger than this (bytes) get auto-tracked by LFS
    #[serde(default = "default_lfs_threshold")]
    pub lfs_size_threshold_bytes: u64,

    /// Optional: only sync these paths (sparse checkout)
    pub sparse_paths: Option<Vec<String>>,

    /// Machine identifier for commit messages
    #[serde(default = "default_machine_id")]
    pub machine_id: String,
}

fn default_branch() -> String {
    "main".into()
}

fn default_pull_interval() -> u64 {
    20
}

fn default_debounce_ms() -> u64 {
    100
}

fn default_lfs_threshold() -> u64 {
    10 * 1024 * 1024 // 10 MB
}

fn default_machine_id() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".into())
}

impl Config {
    /// Create a new config with sensible defaults for the given repo path and remote.
    pub fn new(repo_path: PathBuf, remote_url: String, branch: String) -> Self {
        Self {
            repo_path,
            remote_url,
            branch,
            pull_interval_secs: default_pull_interval(),
            debounce_ms: default_debounce_ms(),
            lfs_size_threshold_bytes: default_lfs_threshold(),
            sparse_paths: None,
            machine_id: default_machine_id(),
        }
    }

    /// Default config directory: ~/.gitdrive/
    pub fn dir() -> PathBuf {
        dirs::home_dir()
            .expect("could not determine home directory")
            .join(".gitdrive")
    }

    /// Default config file path: ~/.gitdrive/config.toml
    pub fn default_path() -> PathBuf {
        Self::dir().join("config.toml")
    }

    /// Load config from a TOML file
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            GitDriveError::Config(format!("failed to read {}: {e}", path.display()))
        })?;
        toml::from_str(&contents)
            .map_err(|e| GitDriveError::Config(format!("invalid config: {e}")))
    }

    /// Save config to a TOML file
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)
            .map_err(|e| GitDriveError::Config(format!("serialize error: {e}")))?;
        std::fs::write(path, contents)?;
        Ok(())
    }
}
