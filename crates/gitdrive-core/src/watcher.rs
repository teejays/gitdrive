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

        // Pre-compute the .git directory path to avoid per-path allocation
        let git_dir = watch_path.join(".git");

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
