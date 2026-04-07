use std::path::{Path, PathBuf};
use tokio::process::Command;
use tracing::{debug, instrument};

use crate::error::{GitDriveError, Result};

/// Raw output from a git command.
#[derive(Debug)]
pub struct GitOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Result of a push operation.
#[derive(Debug, PartialEq)]
pub enum PushResult {
    Ok,
    Rejected,
}

/// Result of a pull --rebase operation.
#[derive(Debug, PartialEq)]
pub enum PullResult {
    Ok,
    UpToDate,
    Conflict,
}

/// How HEAD relates to the upstream tracking branch.
#[derive(Debug, PartialEq)]
pub enum Divergence {
    UpToDate,
    Ahead,
    Behind,
    Diverged,
    NoUpstream,
}

/// Status of a single file in the working tree.
#[derive(Debug, Clone)]
pub struct FileStatus {
    pub index: char,
    pub worktree: char,
    pub path: String,
}

/// Wrapper around the `git` CLI.
///
/// All operations shell out to `git` so we get full LFS support for free.
#[derive(Debug, Clone)]
pub struct GitCli {
    repo_path: PathBuf,
}

impl GitCli {
    pub fn new(repo_path: PathBuf) -> Self {
        Self { repo_path }
    }

    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    // ── core operations ───────────────────────────────────────────

    /// `git status --porcelain`
    #[instrument(skip(self))]
    pub async fn status(&self) -> Result<Vec<FileStatus>> {
        let out = self.run(&["status", "--porcelain"]).await?;
        let statuses = out
            .stdout
            .lines()
            .filter(|l| l.len() >= 3)
            .map(|line| {
                let bytes = line.as_bytes();
                FileStatus {
                    index: bytes[0] as char,
                    worktree: bytes[1] as char,
                    path: line[3..].to_string(),
                }
            })
            .collect();
        Ok(statuses)
    }

    /// `git add <paths>`
    #[instrument(skip(self))]
    pub async fn add(&self, paths: &[&Path]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut args: Vec<&str> = vec!["add", "--"];
        let path_strs: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let refs: Vec<&str> = path_strs.iter().map(|s| s.as_str()).collect();
        args.extend(refs);
        self.run_checked(&args).await
    }

    /// `git add -A` — stage everything (used for conflict resolution)
    #[instrument(skip(self))]
    pub async fn add_all(&self) -> Result<()> {
        self.run_checked(&["add", "-A"]).await
    }

    /// `git commit -m <message>`
    #[instrument(skip(self))]
    pub async fn commit(&self, message: &str) -> Result<()> {
        self.run_checked(&["commit", "-m", message]).await
    }

    /// `git push -u origin HEAD`. Returns `Rejected` if the push was rejected (needs pull first).
    #[instrument(skip(self))]
    pub async fn push(&self) -> Result<PushResult> {
        let out = self.run(&["push", "-u", "origin", "HEAD"]).await?;
        if out.exit_code == 0 {
            Ok(PushResult::Ok)
        } else if out.stderr.contains("rejected")
            || out.stderr.contains("non-fast-forward")
            || out.stderr.contains("fetch first")
        {
            Ok(PushResult::Rejected)
        } else {
            Err(GitDriveError::GitCommand {
                command: "push".into(),
                stderr: out.stderr,
                exit_code: out.exit_code,
            })
        }
    }

    /// `git fetch origin`
    #[instrument(skip(self))]
    pub async fn fetch(&self) -> Result<()> {
        self.run_checked(&["fetch", "origin"]).await
    }

    /// `git pull --rebase`. Returns `Conflict` if there were merge conflicts.
    #[instrument(skip(self))]
    pub async fn pull_rebase(&self) -> Result<PullResult> {
        let out = self.run(&["pull", "--rebase"]).await?;
        if out.exit_code == 0 {
            if out.stdout.contains("Already up to date") {
                Ok(PullResult::UpToDate)
            } else {
                Ok(PullResult::Ok)
            }
        } else if out.stderr.contains("CONFLICT") || out.stderr.contains("could not apply") {
            Ok(PullResult::Conflict)
        } else {
            Err(GitDriveError::GitCommand {
                command: "pull --rebase".into(),
                stderr: out.stderr,
                exit_code: out.exit_code,
            })
        }
    }

    /// Check how HEAD relates to the upstream tracking branch.
    #[instrument(skip(self))]
    pub async fn merge_base_check(&self) -> Result<Divergence> {
        // Get local HEAD
        let local = match self.run(&["rev-parse", "HEAD"]).await {
            Ok(o) if o.exit_code == 0 => o.stdout.trim().to_string(),
            _ => return Ok(Divergence::NoUpstream),
        };

        // Get upstream ref
        let remote = match self.run(&["rev-parse", "@{u}"]).await {
            Ok(o) if o.exit_code == 0 => o.stdout.trim().to_string(),
            _ => return Ok(Divergence::NoUpstream),
        };

        if local == remote {
            return Ok(Divergence::UpToDate);
        }

        // Get merge base
        let base_out = self.run(&["merge-base", &local, &remote]).await?;
        if base_out.exit_code != 0 {
            return Ok(Divergence::Diverged);
        }
        let base = base_out.stdout.trim();

        if base == local {
            Ok(Divergence::Behind)
        } else if base == remote {
            Ok(Divergence::Ahead)
        } else {
            Ok(Divergence::Diverged)
        }
    }

    /// `git diff --name-only <from> <to>`
    #[instrument(skip(self))]
    pub async fn diff_name_only(&self, from: &str, to: &str) -> Result<Vec<String>> {
        let out = self.run(&["diff", "--name-only", from, to]).await?;
        Ok(out
            .stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect())
    }

    /// `git rev-parse HEAD` — returns the current commit hash.
    #[instrument(skip(self))]
    pub async fn head_hash(&self) -> Result<String> {
        let out = self.run(&["rev-parse", "HEAD"]).await?;
        if out.exit_code != 0 {
            return Err(GitDriveError::GitCommand {
                command: "rev-parse HEAD".into(),
                stderr: out.stderr,
                exit_code: out.exit_code,
            });
        }
        Ok(out.stdout.trim().to_string())
    }

    /// `git check-ignore --stdin` — returns which paths are ignored.
    #[instrument(skip(self))]
    pub async fn check_ignore(&self, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
        if paths.is_empty() {
            return Ok(vec![]);
        }
        let stdin_data: String = paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        let output = Command::new("git")
            .args(["check-ignore", "--stdin"])
            .current_dir(&self.repo_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        use tokio::io::AsyncWriteExt;
        let mut child = output;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_data.as_bytes()).await?;
            // Drop stdin to signal EOF
        }

        let out = child.wait_with_output().await?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        Ok(stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| PathBuf::from(l.trim()))
            .collect())
    }

    /// Verify that `git` is available and the repo exists.
    pub async fn verify(&self) -> Result<()> {
        // Check git is on PATH
        let out = Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        match out {
            Ok(child) => {
                let o = child.wait_with_output().await?;
                if !o.status.success() {
                    return Err(GitDriveError::GitNotFound);
                }
            }
            Err(_) => return Err(GitDriveError::GitNotFound),
        }

        // Check repo path is a git repo
        let out = self.run(&["rev-parse", "--git-dir"]).await?;
        if out.exit_code != 0 {
            return Err(GitDriveError::NotARepo(self.repo_path.clone()));
        }

        Ok(())
    }

    /// Initialize a new git repo at the configured path.
    pub async fn init(&self) -> Result<()> {
        std::fs::create_dir_all(&self.repo_path)?;
        self.run_checked(&["init"]).await?;
        self.run_checked(&["checkout", "-b", "main"]).await.ok(); // ignore if already on main
        Ok(())
    }

    /// Add a remote origin.
    pub async fn add_remote(&self, url: &str) -> Result<()> {
        // Remove existing origin if any, then add
        let _ = self.run(&["remote", "remove", "origin"]).await;
        self.run_checked(&["remote", "add", "origin", url]).await
    }

    // ── internal ──────────────────────────────────────────────────

    /// Run a git command and return its raw output (does not fail on non-zero exit).
    pub(crate) async fn run(&self, args: &[&str]) -> Result<GitOutput> {
        debug!(args = ?args, "running git command");
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.repo_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        debug!(exit_code, %stdout, %stderr, "git command completed");

        Ok(GitOutput {
            stdout,
            stderr,
            exit_code,
        })
    }

    /// Run a git command and return an error if it fails.
    pub(crate) async fn run_checked(&self, args: &[&str]) -> Result<()> {
        let out = self.run(args).await?;
        if out.exit_code != 0 {
            return Err(GitDriveError::GitCommand {
                command: args.join(" "),
                stderr: out.stderr,
                exit_code: out.exit_code,
            });
        }
        Ok(())
    }
}
