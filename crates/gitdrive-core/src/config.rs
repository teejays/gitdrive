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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_new_defaults() {
        let config = Config::new("/tmp/test".into(), "git@github.com:user/repo.git".into(), "main".into());
        assert_eq!(config.repo_path, PathBuf::from("/tmp/test"));
        assert_eq!(config.remote_url, "git@github.com:user/repo.git");
        assert_eq!(config.branch, "main");
        assert_eq!(config.pull_interval_secs, 20);
        assert_eq!(config.debounce_ms, 100);
        assert_eq!(config.lfs_size_threshold_bytes, 10 * 1024 * 1024);
        assert!(config.sparse_paths.is_none());
        assert!(!config.machine_id.is_empty());
    }

    #[test]
    fn test_config_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        let config = Config::new("/tmp/test".into(), "git@github.com:user/repo.git".into(), "main".into());
        config.save(&config_path).unwrap();

        let loaded = Config::load(&config_path).unwrap();
        assert_eq!(loaded.repo_path, config.repo_path);
        assert_eq!(loaded.remote_url, config.remote_url);
        assert_eq!(loaded.branch, config.branch);
        assert_eq!(loaded.pull_interval_secs, config.pull_interval_secs);
        assert_eq!(loaded.debounce_ms, config.debounce_ms);
        assert_eq!(loaded.lfs_size_threshold_bytes, config.lfs_size_threshold_bytes);
    }

    #[test]
    fn test_config_load_missing_file() {
        let result = Config::load(Path::new("/nonexistent/config.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_config_load_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        // Minimal config — serde defaults should fill in the rest
        std::fs::write(&config_path, r#"
            repo_path = "/tmp/test"
            remote_url = "git@github.com:user/repo.git"
        "#).unwrap();

        let loaded = Config::load(&config_path).unwrap();
        assert_eq!(loaded.branch, "main");
        assert_eq!(loaded.pull_interval_secs, 20);
        assert_eq!(loaded.debounce_ms, 100);
        assert_eq!(loaded.lfs_size_threshold_bytes, 10 * 1024 * 1024);
    }

    #[test]
    fn test_config_save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("nested").join("deep").join("config.toml");

        let config = Config::new("/tmp/test".into(), "url".into(), "main".into());
        config.save(&config_path).unwrap();

        assert!(config_path.exists());
    }

    #[test]
    fn test_config_load_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "this is not valid toml {{{{").unwrap();

        let result = Config::load(&config_path);
        assert!(result.is_err());
    }
}
