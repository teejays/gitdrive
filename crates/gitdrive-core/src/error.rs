use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum GitDriveError {
    #[error("git command failed: `git {command}` (exit code {exit_code})\n{stderr}")]
    GitCommand {
        command: String,
        stderr: String,
        exit_code: i32,
    },

    #[error("merge conflict in: {}", files.join(", "))]
    MergeConflict { files: Vec<String> },

    #[error("git not found on PATH — install git and try again")]
    GitNotFound,

    #[error("not a git repository: {0}")]
    NotARepo(PathBuf),

    #[error("git lfs not installed — run `git lfs install`")]
    LfsNotInstalled,

    #[error("config error: {0}")]
    Config(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("watcher error: {0}")]
    Watcher(#[from] notify::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, GitDriveError>;
