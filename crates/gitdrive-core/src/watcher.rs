use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::error::Result;
use crate::git::GitCli;

/// Watches a directory for file changes, debounces events, and emits batched paths.
pub struct FileWatcher {
    rx: mpsc::Receiver<Vec<PathBuf>>,
    _watcher: RecommendedWatcher,
}

impl FileWatcher {
    /// Start watching `watch_path` recursively. Changed paths are debounced and
    /// emitted as batches on the returned receiver.
    pub fn new(watch_path: &Path, debounce_ms: u64, git: GitCli) -> Result<Self> {
        let (event_tx, mut event_rx) = mpsc::channel::<Event>(1024);
        let (batch_tx, batch_rx) = mpsc::channel::<Vec<PathBuf>>(64);

        let tx = event_tx.clone();
        let mut watcher = RecommendedWatcher::new(
            move |res: std::result::Result<Event, notify::Error>| match res {
                Ok(event) => {
                    let _ = tx.blocking_send(event);
                }
                Err(e) => warn!("watcher error: {e}"),
            },
            notify::Config::default(),
        )?;

        watcher.watch(watch_path, RecursiveMode::Recursive)?;

        // Pre-compute the .git directory path. Canonicalize to handle symlinks
        // (e.g. /tmp -> /private/tmp on macOS) so starts_with() matches correctly.
        let git_dir = watch_path.join(".git")
            .canonicalize()
            .unwrap_or_else(|_| watch_path.join(".git"));

        tokio::spawn(async move {
            let debounce = tokio::time::Duration::from_millis(debounce_ms);
            let mut pending: HashSet<PathBuf> = HashSet::new();

            loop {
                let event = match event_rx.recv().await {
                    Some(e) => e,
                    None => break,
                };
                collect_paths(&event, &mut pending);

                loop {
                    match tokio::time::timeout(debounce, event_rx.recv()).await {
                        Ok(Some(event)) => {
                            collect_paths(&event, &mut pending);
                        }
                        Ok(None) => return,
                        Err(_) => break,
                    }
                }

                if pending.is_empty() {
                    continue;
                }

                let paths: Vec<PathBuf> = pending.drain().collect();
                let ignored = git.check_ignore(&paths).await.unwrap_or_default();
                let ignored_set: HashSet<&PathBuf> = ignored.iter().collect();

                let filtered: Vec<PathBuf> = paths
                    .into_iter()
                    .filter(|p| !ignored_set.contains(p))
                    .filter(|p| !p.starts_with(&git_dir))
                    .collect();

                if !filtered.is_empty() {
                    debug!(count = filtered.len(), "emitting debounced file batch");
                    if batch_tx.send(filtered).await.is_err() {
                        break;
                    }
                }
            }
        });

        Ok(Self {
            rx: batch_rx,
            _watcher: watcher,
        })
    }

    /// Wait for the next batch of changed file paths.
    pub async fn next_batch(&mut self) -> Option<Vec<PathBuf>> {
        self.rx.recv().await
    }
}

fn collect_paths(event: &Event, set: &mut HashSet<PathBuf>) {
    for path in &event.paths {
        set.insert(path.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_git_internal(path: &Path, git_dir: &Path) -> bool {
        path.starts_with(git_dir)
    }

    #[test]
    fn test_collect_paths_deduplicates() {
        let event = Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            paths: vec![PathBuf::from("/a/b.txt"), PathBuf::from("/a/b.txt")],
            attrs: Default::default(),
        };
        let mut set = HashSet::new();
        collect_paths(&event, &mut set);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn test_collect_paths_multiple() {
        let event = Event {
            kind: notify::EventKind::Create(notify::event::CreateKind::File),
            paths: vec![
                PathBuf::from("/a/1.txt"),
                PathBuf::from("/a/2.txt"),
                PathBuf::from("/a/3.txt"),
            ],
            attrs: Default::default(),
        };
        let mut set = HashSet::new();
        collect_paths(&event, &mut set);
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn test_is_git_internal() {
        let git_dir = PathBuf::from("/repo/.git");
        assert!(is_git_internal(Path::new("/repo/.git/index"), &git_dir));
        assert!(is_git_internal(Path::new("/repo/.git/objects/abc"), &git_dir));
        assert!(!is_git_internal(Path::new("/repo/src/main.rs"), &git_dir));
        assert!(!is_git_internal(Path::new("/repo/.gitignore"), &git_dir));
    }

    #[tokio::test]
    async fn test_watcher_detects_file_creation() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().to_path_buf();

        // Need a git repo for check-ignore to work
        let git = crate::git::GitCli::new(git_dir.clone());
        git.init().await.unwrap();

        let mut watcher = FileWatcher::new(dir.path(), 50, git).unwrap();

        // Create a file
        std::fs::write(dir.path().join("test.txt"), "hello").unwrap();

        let batch = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            watcher.next_batch(),
        )
        .await
        .expect("timeout waiting for watcher")
        .expect("watcher closed");

        let names: Vec<String> = batch
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"test.txt".to_string()));
    }

    #[tokio::test]
    async fn test_watcher_filters_git_internal() {
        let dir = tempfile::tempdir().unwrap();
        let git = crate::git::GitCli::new(dir.path().to_path_buf());
        git.init().await.unwrap();

        let mut watcher = FileWatcher::new(dir.path(), 50, git).unwrap();

        // Write to .git/ and to a regular file simultaneously
        std::fs::write(dir.path().join(".git").join("test_internal"), "x").unwrap();
        std::fs::write(dir.path().join("visible.txt"), "y").unwrap();

        // Collect all batches within a window — the .git write and visible write
        // may arrive in separate batches
        let mut all_paths = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, watcher.next_batch()).await {
                Ok(Some(batch)) => {
                    all_paths.extend(batch);
                    // Give a short window for any more batches
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                _ => break,
            }
            if all_paths.iter().any(|p| {
                p.file_name().map(|f| f == "visible.txt").unwrap_or(false)
            }) {
                break;
            }
        }

        let names: Vec<String> = all_paths
            .iter()
            .filter_map(|p| p.file_name().map(|f| f.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"visible.txt".to_string()));
        // .git internal paths should be filtered out
        assert!(!names.contains(&"test_internal".to_string()));
    }
}
