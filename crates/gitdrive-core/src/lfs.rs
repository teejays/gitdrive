use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::error::{GitDriveError, Result};
use crate::git::GitCli;

/// Common binary file extensions that should always be tracked by LFS.
const DEFAULT_LFS_EXTENSIONS: &[&str] = &[
    // Images
    "png", "jpg", "jpeg", "gif", "bmp", "tiff", "tif", "webp", "ico", "heic", "heif", "raw",
    "cr2", "nef", "arw",
    // Video
    "mp4", "mov", "avi", "mkv", "webm", "flv", "wmv", "m4v",
    // Audio
    "mp3", "wav", "flac", "aac", "ogg", "m4a", "wma",
    // Archives
    "zip", "tar", "gz", "bz2", "7z", "rar", "xz", "zst",
    // Documents
    "pdf", "psd", "ai", "sketch", "fig", "xd",
    // Binaries
    "exe", "dll", "dylib", "so", "app", "dmg", "pkg", "msi",
    // Fonts
    "ttf", "otf", "woff", "woff2", "eot",
    // Data
    "sqlite", "db",
];

/// Manages Git LFS setup, tracking, and auto-detection.
pub struct LfsManager {
    git: GitCli,
}

impl LfsManager {
    pub fn new(git: GitCli) -> Self {
        Self { git }
    }

    /// Verify that `git lfs` is available.
    pub async fn ensure_lfs_installed(&self) -> Result<()> {
        let out = self.git.run(&["lfs", "version"]).await?;
        if out.exit_code != 0 {
            return Err(GitDriveError::LfsNotInstalled);
        }
        debug!("git lfs is available: {}", out.stdout.trim());
        Ok(())
    }

    /// Run `git lfs install` to set up hooks and filters for this repo.
    pub async fn install(&self) -> Result<()> {
        self.git.run_checked(&["lfs", "install"]).await?;
        info!("git lfs installed");
        Ok(())
    }

    /// Write default `.gitattributes` with LFS tracking for common binary extensions.
    /// Only adds entries that aren't already present.
    pub async fn init_default_tracking(&self, repo_path: &Path) -> Result<()> {
        let gitattributes_path = repo_path.join(".gitattributes");

        let existing = if gitattributes_path.exists() {
            std::fs::read_to_string(&gitattributes_path)?
        } else {
            String::new()
        };

        let existing_extensions = parse_lfs_extensions(&existing);
        let mut additions = Vec::new();

        for ext in DEFAULT_LFS_EXTENSIONS {
            if !existing_extensions.contains(*ext) {
                additions.push(format!(
                    "*.{ext} filter=lfs diff=lfs merge=lfs -text"
                ));
            }
        }

        if additions.is_empty() {
            debug!("all default LFS extensions already tracked");
            return Ok(());
        }

        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str("# GitDrive: auto-tracked LFS extensions\n");
        for line in &additions {
            content.push_str(line);
            content.push('\n');
        }

        std::fs::write(&gitattributes_path, &content)?;
        info!(
            count = additions.len(),
            "added LFS tracking for default extensions"
        );

        // Stage .gitattributes
        self.git
            .add(&[gitattributes_path.as_path()])
            .await?;

        Ok(())
    }

    /// Scan a set of paths for files that exceed the LFS size threshold and whose
    /// extension is not yet tracked by LFS. Auto-adds tracking for new extensions.
    ///
    /// Returns the list of extensions that were newly tracked.
    pub async fn auto_track_if_needed(
        &self,
        paths: &[PathBuf],
        threshold_bytes: u64,
        repo_path: &Path,
    ) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Ok(vec![]);
        }

        // First pass: collect extensions of large files (no file I/O on .gitattributes yet)
        let mut candidate_extensions: HashSet<String> = HashSet::new();
        for path in paths {
            let metadata = match std::fs::metadata(path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !metadata.is_file() || metadata.len() < threshold_bytes {
                continue;
            }
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                candidate_extensions.insert(ext.to_lowercase());
            }
        }

        if candidate_extensions.is_empty() {
            return Ok(vec![]);
        }

        // Only read .gitattributes when we actually have large files to check
        let gitattributes_path = repo_path.join(".gitattributes");
        let existing = if gitattributes_path.exists() {
            std::fs::read_to_string(&gitattributes_path)?
        } else {
            String::new()
        };
        let tracked_extensions = parse_lfs_extensions(&existing);

        let new_extensions: HashSet<String> = candidate_extensions
            .into_iter()
            .filter(|ext| !tracked_extensions.contains(ext.as_str()))
            .collect();

        if new_extensions.is_empty() {
            return Ok(vec![]);
        }

        // Track each new extension
        let mut tracked = Vec::new();
        for ext in &new_extensions {
            let pattern = format!("*.{ext}");
            match self
                .git
                .run_checked(&["lfs", "track", &pattern])
                .await
            {
                Ok(()) => {
                    info!(ext = %ext, "auto-tracked new LFS extension");
                    tracked.push(ext.clone());
                }
                Err(e) => {
                    warn!(ext = %ext, error = %e, "failed to track LFS extension");
                }
            }
        }

        // Stage the updated .gitattributes
        if !tracked.is_empty() {
            self.git
                .add(&[gitattributes_path.as_path()])
                .await?;
        }

        Ok(tracked)
    }
}

/// Parse existing `.gitattributes` content and extract extensions that are already
/// tracked by LFS (lines matching `*.EXT filter=lfs ...`).
fn parse_lfs_extensions(content: &str) -> HashSet<&str> {
    let mut extensions = HashSet::new();
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        // Match patterns like: *.png filter=lfs diff=lfs merge=lfs -text
        if line.contains("filter=lfs") {
            if let Some(pattern) = line.split_whitespace().next() {
                if let Some(ext) = pattern.strip_prefix("*.") {
                    extensions.insert(ext);
                }
            }
        }
    }
    extensions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_lfs_extensions() {
        let content = r#"
# Some comment
*.png filter=lfs diff=lfs merge=lfs -text
*.jpg filter=lfs diff=lfs merge=lfs -text
*.txt text
# Another comment
*.mp4 filter=lfs diff=lfs merge=lfs -text
"#;
        let exts = parse_lfs_extensions(content);
        assert!(exts.contains("png"));
        assert!(exts.contains("jpg"));
        assert!(exts.contains("mp4"));
        assert!(!exts.contains("txt"));
        assert_eq!(exts.len(), 3);
    }

    #[test]
    fn test_parse_empty() {
        let exts = parse_lfs_extensions("");
        assert!(exts.is_empty());
    }
}
