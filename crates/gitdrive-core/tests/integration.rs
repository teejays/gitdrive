use gitdrive_core::config::Config;
use gitdrive_core::conflict::{ConflictEvent, ConflictResolution, ConflictResolver};
use gitdrive_core::git::{Divergence, GitCli, PullResult, PushResult};
use gitdrive_core::lfs::LfsManager;
use gitdrive_core::sync::SyncEngine;
use gitdrive_core::watcher::FileWatcher;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tokio::sync::mpsc;

/// Set up a bare remote + local clone pair for testing.
async fn setup_repo_pair() -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new().unwrap();
    let remote_path = dir.path().join("remote.git");
    let local_path = dir.path().join("local");

    std::fs::create_dir_all(&remote_path).unwrap();
    std::fs::create_dir_all(&local_path).unwrap();

    // Init bare remote
    let out = tokio::process::Command::new("git")
        .args(["init", "--bare"])
        .current_dir(&remote_path)
        .output()
        .await
        .unwrap();
    assert!(out.status.success(), "failed to init bare repo");

    // Init local and set remote
    let git = GitCli::new(local_path.clone());
    git.init().await.unwrap();
    git.add_remote(remote_path.to_str().unwrap()).await.unwrap();

    // Initial commit + push to establish upstream
    std::fs::write(local_path.join(".gitkeep"), "").unwrap();
    git.add_all().await.unwrap();
    git.commit("initial commit").await.unwrap();
    git.push().await.unwrap();

    (dir, remote_path, local_path)
}

/// Clone the remote into a second local directory (simulates another machine).
async fn clone_to(remote_path: &Path, parent: &Path, name: &str) -> PathBuf {
    let clone_path = parent.join(name);
    let out = tokio::process::Command::new("git")
        .args([
            "clone",
            "--branch",
            "main",
            remote_path.to_str().unwrap(),
            clone_path.to_str().unwrap(),
        ])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    clone_path
}

// ── Push pipeline tests ──────────────────────────────────────

#[tokio::test]
async fn test_push_new_file() {
    let (_dir, _remote, local) = setup_repo_pair().await;
    let git = GitCli::new(local.clone());

    std::fs::write(local.join("hello.txt"), "world").unwrap();
    git.add_all().await.unwrap();
    git.commit("add hello").await.unwrap();

    let result = git.push().await.unwrap();
    assert_eq!(result, PushResult::Ok);
}

#[tokio::test]
async fn test_push_modify_file() {
    let (_dir, _remote, local) = setup_repo_pair().await;
    let git = GitCli::new(local.clone());

    // Create and push
    std::fs::write(local.join("data.txt"), "v1").unwrap();
    git.add_all().await.unwrap();
    git.commit("v1").await.unwrap();
    git.push().await.unwrap();

    // Modify and push
    std::fs::write(local.join("data.txt"), "v2").unwrap();
    git.add_all().await.unwrap();
    git.commit("v2").await.unwrap();

    let result = git.push().await.unwrap();
    assert_eq!(result, PushResult::Ok);
}

#[tokio::test]
async fn test_push_delete_file() {
    let (_dir, _remote, local) = setup_repo_pair().await;
    let git = GitCli::new(local.clone());

    std::fs::write(local.join("temp.txt"), "temp").unwrap();
    git.add_all().await.unwrap();
    git.commit("add temp").await.unwrap();
    git.push().await.unwrap();

    std::fs::remove_file(local.join("temp.txt")).unwrap();
    git.add_all().await.unwrap();
    git.commit("delete temp").await.unwrap();

    let result = git.push().await.unwrap();
    assert_eq!(result, PushResult::Ok);
}

// ── Pull pipeline tests ─────────────────────────────────────

#[tokio::test]
async fn test_pull_remote_changes() {
    let (dir, remote, local) = setup_repo_pair().await;
    let local_git = GitCli::new(local.clone());

    // Clone to machine2 and push a change
    let machine2 = clone_to(&remote, dir.path(), "machine2").await;
    std::fs::write(machine2.join("from_m2.txt"), "hello from m2").unwrap();
    let m2_git = GitCli::new(machine2.clone());
    m2_git.add_all().await.unwrap();
    m2_git.commit("from machine2").await.unwrap();
    m2_git.push().await.unwrap();

    // Pull on local
    local_git.fetch().await.unwrap();
    let result = local_git.pull_rebase().await.unwrap();
    assert_eq!(result, PullResult::Ok);

    // Verify file arrived
    assert!(local.join("from_m2.txt").exists());
    assert_eq!(
        std::fs::read_to_string(local.join("from_m2.txt")).unwrap(),
        "hello from m2"
    );
}

#[tokio::test]
async fn test_pull_already_up_to_date() {
    let (_dir, _remote, local) = setup_repo_pair().await;
    let git = GitCli::new(local.clone());

    git.fetch().await.unwrap();
    let result = git.pull_rebase().await.unwrap();
    assert_eq!(result, PullResult::UpToDate);
}

// ── Divergence detection ────────────────────────────────────

#[tokio::test]
async fn test_divergence_up_to_date() {
    let (_dir, _remote, local) = setup_repo_pair().await;
    let git = GitCli::new(local.clone());

    git.fetch().await.unwrap();
    let div = git.merge_base_check().await.unwrap();
    assert_eq!(div, Divergence::UpToDate);
}

#[tokio::test]
async fn test_divergence_ahead() {
    let (_dir, _remote, local) = setup_repo_pair().await;
    let git = GitCli::new(local.clone());

    std::fs::write(local.join("new.txt"), "x").unwrap();
    git.add_all().await.unwrap();
    git.commit("local only").await.unwrap();

    git.fetch().await.unwrap();
    let div = git.merge_base_check().await.unwrap();
    assert_eq!(div, Divergence::Ahead);
}

#[tokio::test]
async fn test_divergence_behind() {
    let (dir, remote, local) = setup_repo_pair().await;
    let local_git = GitCli::new(local.clone());

    // Push from machine2
    let machine2 = clone_to(&remote, dir.path(), "machine2").await;
    let m2_git = GitCli::new(machine2.clone());
    std::fs::write(machine2.join("m2.txt"), "x").unwrap();
    m2_git.add_all().await.unwrap();
    m2_git.commit("from m2").await.unwrap();
    m2_git.push().await.unwrap();

    local_git.fetch().await.unwrap();
    let div = local_git.merge_base_check().await.unwrap();
    assert_eq!(div, Divergence::Behind);
}

#[tokio::test]
async fn test_divergence_diverged() {
    let (dir, remote, local) = setup_repo_pair().await;
    let local_git = GitCli::new(local.clone());

    // Push from machine2
    let machine2 = clone_to(&remote, dir.path(), "machine2").await;
    let m2_git = GitCli::new(machine2.clone());
    std::fs::write(machine2.join("m2.txt"), "from m2").unwrap();
    m2_git.add_all().await.unwrap();
    m2_git.commit("m2 change").await.unwrap();
    m2_git.push().await.unwrap();

    // Local commit (different file, no conflict but diverged)
    std::fs::write(local.join("local.txt"), "from local").unwrap();
    local_git.add_all().await.unwrap();
    local_git.commit("local change").await.unwrap();

    local_git.fetch().await.unwrap();
    let div = local_git.merge_base_check().await.unwrap();
    assert_eq!(div, Divergence::Diverged);
}

// ── Push rejection + auto-rebase ────────────────────────────

#[tokio::test]
async fn test_push_rejected_when_behind() {
    let (dir, remote, local) = setup_repo_pair().await;
    let local_git = GitCli::new(local.clone());

    // Push from machine2
    let machine2 = clone_to(&remote, dir.path(), "machine2").await;
    let m2_git = GitCli::new(machine2.clone());
    std::fs::write(machine2.join("m2.txt"), "x").unwrap();
    m2_git.add_all().await.unwrap();
    m2_git.commit("from m2").await.unwrap();
    m2_git.push().await.unwrap();

    // Local commit on different file
    std::fs::write(local.join("local.txt"), "y").unwrap();
    local_git.add_all().await.unwrap();
    local_git.commit("from local").await.unwrap();

    // Push should be rejected
    let result = local_git.push().await.unwrap();
    assert_eq!(result, PushResult::Rejected);

    // Pull rebase should succeed (no conflict — different files)
    let pull_result = local_git.pull_rebase().await.unwrap();
    assert_eq!(pull_result, PullResult::Ok);

    // Now push should work
    let result = local_git.push().await.unwrap();
    assert_eq!(result, PushResult::Ok);
}

// ── Conflict detection ──────────────────────────────────────

#[tokio::test]
async fn test_conflict_detection() {
    let (dir, remote, local) = setup_repo_pair().await;
    let local_git = GitCli::new(local.clone());

    // Both machines edit the same file
    std::fs::write(local.join("shared.txt"), "original").unwrap();
    local_git.add_all().await.unwrap();
    local_git.commit("add shared").await.unwrap();
    local_git.push().await.unwrap();

    let machine2 = clone_to(&remote, dir.path(), "machine2").await;
    let m2_git = GitCli::new(machine2.clone());
    std::fs::write(machine2.join("shared.txt"), "machine2 version").unwrap();
    m2_git.add_all().await.unwrap();
    m2_git.commit("m2 edit").await.unwrap();
    m2_git.push().await.unwrap();

    // Local edits the same file differently
    std::fs::write(local.join("shared.txt"), "local version").unwrap();
    local_git.add_all().await.unwrap();
    local_git.commit("local edit").await.unwrap();

    // Pull should result in conflict
    local_git.fetch().await.unwrap();
    let result = local_git.pull_rebase().await.unwrap();
    assert_eq!(result, PullResult::Conflict);

    // Conflict resolver should detect it
    let resolver = ConflictResolver::new(local_git.clone());
    let event = resolver.detect_conflicts().await.unwrap();
    assert!(event.is_some());
    let event = event.unwrap();
    assert!(event
        .conflicted_files
        .iter()
        .any(|f| f.contains("shared.txt")));

    // Abort to clean up
    resolver.abort_rebase().await.unwrap();
}

#[tokio::test]
async fn test_conflict_resolve_keep_mine() {
    let (dir, remote, local) = setup_repo_pair().await;
    let local_git = GitCli::new(local.clone());

    std::fs::write(local.join("file.txt"), "base").unwrap();
    local_git.add_all().await.unwrap();
    local_git.commit("base").await.unwrap();
    local_git.push().await.unwrap();

    let machine2 = clone_to(&remote, dir.path(), "machine2").await;
    let m2_git = GitCli::new(machine2.clone());
    std::fs::write(machine2.join("file.txt"), "theirs").unwrap();
    m2_git.add_all().await.unwrap();
    m2_git.commit("their edit").await.unwrap();
    m2_git.push().await.unwrap();

    std::fs::write(local.join("file.txt"), "mine").unwrap();
    local_git.add_all().await.unwrap();
    local_git.commit("my edit").await.unwrap();

    local_git.fetch().await.unwrap();
    let result = local_git.pull_rebase().await.unwrap();
    assert_eq!(result, PullResult::Conflict);

    let resolver = ConflictResolver::new(local_git.clone());
    let event = resolver.detect_conflicts().await.unwrap().unwrap();
    resolver
        .resolve_keep_mine(&event.conflicted_files)
        .await
        .unwrap();

    let content = std::fs::read_to_string(local.join("file.txt")).unwrap();
    assert_eq!(content, "mine");
}

#[tokio::test]
async fn test_conflict_resolve_keep_theirs() {
    let (dir, remote, local) = setup_repo_pair().await;
    let local_git = GitCli::new(local.clone());

    std::fs::write(local.join("file.txt"), "base").unwrap();
    local_git.add_all().await.unwrap();
    local_git.commit("base").await.unwrap();
    local_git.push().await.unwrap();

    let machine2 = clone_to(&remote, dir.path(), "machine2").await;
    let m2_git = GitCli::new(machine2.clone());
    std::fs::write(machine2.join("file.txt"), "theirs").unwrap();
    m2_git.add_all().await.unwrap();
    m2_git.commit("their edit").await.unwrap();
    m2_git.push().await.unwrap();

    std::fs::write(local.join("file.txt"), "mine").unwrap();
    local_git.add_all().await.unwrap();
    local_git.commit("my edit").await.unwrap();

    local_git.fetch().await.unwrap();
    local_git.pull_rebase().await.unwrap();

    let resolver = ConflictResolver::new(local_git.clone());
    let event = resolver.detect_conflicts().await.unwrap().unwrap();
    resolver
        .resolve_keep_theirs(&event.conflicted_files)
        .await
        .unwrap();

    let content = std::fs::read_to_string(local.join("file.txt")).unwrap();
    assert_eq!(content, "theirs");
}

// ── LFS tests ───────────────────────────────────────────────

#[tokio::test]
async fn test_lfs_init_default_tracking() {
    let dir = TempDir::new().unwrap();
    let git = GitCli::new(dir.path().to_path_buf());
    git.init().await.unwrap();

    let lfs = LfsManager::new(git.clone());
    // Skip if git lfs isn't available
    if lfs.ensure_lfs_installed().await.is_err() {
        eprintln!("skipping LFS test — git lfs not installed");
        return;
    }
    lfs.install().await.unwrap();
    lfs.init_default_tracking(dir.path()).await.unwrap();

    let content = std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
    assert!(content.contains("*.png filter=lfs"));
    assert!(content.contains("*.mp4 filter=lfs"));
    assert!(content.contains("*.pdf filter=lfs"));
    assert!(content.contains("*.zip filter=lfs"));
}

#[tokio::test]
async fn test_lfs_init_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let git = GitCli::new(dir.path().to_path_buf());
    git.init().await.unwrap();

    let lfs = LfsManager::new(git.clone());
    if lfs.ensure_lfs_installed().await.is_err() {
        return;
    }
    lfs.install().await.unwrap();

    lfs.init_default_tracking(dir.path()).await.unwrap();
    let content1 = std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap();

    lfs.init_default_tracking(dir.path()).await.unwrap();
    let content2 = std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap();

    // Running twice should not duplicate entries
    assert_eq!(content1, content2);
}

#[tokio::test]
async fn test_lfs_auto_track_large_file() {
    let dir = TempDir::new().unwrap();
    let git = GitCli::new(dir.path().to_path_buf());
    git.init().await.unwrap();

    let lfs = LfsManager::new(git.clone());
    if lfs.ensure_lfs_installed().await.is_err() {
        return;
    }
    lfs.install().await.unwrap();

    // Create a file with an unusual extension that exceeds the threshold
    let big_file = dir.path().join("data.xyz");
    let data = vec![0u8; 1024]; // 1KB — use a low threshold for testing
    std::fs::write(&big_file, &data).unwrap();

    let tracked = lfs
        .auto_track_if_needed(&[big_file], 512, dir.path()) // 512 byte threshold
        .await
        .unwrap();

    assert!(tracked.contains(&"xyz".to_string()));

    let content = std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
    assert!(content.contains("*.xyz filter=lfs"));
}

#[tokio::test]
async fn test_lfs_auto_track_skips_small_files() {
    let dir = TempDir::new().unwrap();
    let git = GitCli::new(dir.path().to_path_buf());
    git.init().await.unwrap();

    let lfs = LfsManager::new(git.clone());
    if lfs.ensure_lfs_installed().await.is_err() {
        return;
    }
    lfs.install().await.unwrap();

    let small_file = dir.path().join("small.abc");
    std::fs::write(&small_file, "tiny").unwrap();

    let tracked = lfs
        .auto_track_if_needed(&[small_file], 10 * 1024 * 1024, dir.path())
        .await
        .unwrap();

    assert!(tracked.is_empty());
}

// ── Sync engine tests ───────────────────────────────────────

#[tokio::test]
async fn test_sync_engine_push_pipeline() {
    let (_dir, _remote, local) = setup_repo_pair().await;

    let config = Config::new(local.clone(), "unused".into(), "main".into());

    let (conflict_tx, _conflict_rx) = mpsc::channel::<ConflictEvent>(8);
    let (_resolution_tx, resolution_rx) = mpsc::channel::<ConflictResolution>(8);

    let mut engine = SyncEngine::new(config, conflict_tx, resolution_rx).unwrap();

    let git = GitCli::new(local.clone());
    let watcher = FileWatcher::new(&local, 50, git.clone()).unwrap();

    // Start engine first, then create files so FSEvents fires
    let local_clone = local.clone();
    let engine_handle = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(10), engine.run(watcher)).await
    });

    // Wait for watcher to be ready, then create files
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    std::fs::write(local_clone.join("a.txt"), "aaa").unwrap();
    std::fs::write(local_clone.join("b.txt"), "bbb").unwrap();

    // Wait for the file events to be processed and committed
    tokio::time::sleep(std::time::Duration::from_secs(7)).await;
    engine_handle.abort();

    // The engine should have committed the files.
    // Check that HEAD moved beyond the initial commit.
    let log_out = tokio::process::Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&local)
        .output()
        .await
        .unwrap();
    let log = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        log.contains("gitdrive: auto-sync"),
        "expected auto-sync commit in log:\n{log}"
    );
}

#[tokio::test]
async fn test_sync_engine_detects_deletion() {
    let (_dir, _remote, local) = setup_repo_pair().await;
    let git = GitCli::new(local.clone());

    // Create and commit a file first
    std::fs::write(local.join("to_delete.txt"), "bye").unwrap();
    git.add_all().await.unwrap();
    git.commit("add file").await.unwrap();
    git.push().await.unwrap();

    let config = Config::new(local.clone(), "unused".into(), "main".into());
    let (conflict_tx, _) = mpsc::channel::<ConflictEvent>(8);
    let (_, resolution_rx) = mpsc::channel::<ConflictResolution>(8);
    let mut engine = SyncEngine::new(config, conflict_tx, resolution_rx).unwrap();

    let watcher = FileWatcher::new(&local, 50, git.clone()).unwrap();

    // Delete the file
    std::fs::remove_file(local.join("to_delete.txt")).unwrap();

    let engine_handle = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(8), engine.run(watcher)).await
    });

    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    engine_handle.abort();

    // Verify the deletion was committed
    let status = git.status().await.unwrap();
    assert!(status.is_empty(), "working tree should be clean after sync");
    assert!(!local.join("to_delete.txt").exists());
}

// ── Multi-machine sync test ─────────────────────────────────

#[tokio::test]
async fn test_two_machine_roundtrip() {
    let (dir, remote, local) = setup_repo_pair().await;
    let local_git = GitCli::new(local.clone());

    // Machine 1 pushes a file
    std::fs::write(local.join("from_m1.txt"), "hello from m1").unwrap();
    local_git.add_all().await.unwrap();
    local_git.commit("m1 push").await.unwrap();
    local_git.push().await.unwrap();

    // Machine 2 clones and sees the file
    let machine2 = clone_to(&remote, dir.path(), "machine2").await;
    assert!(machine2.join("from_m1.txt").exists());
    assert_eq!(
        std::fs::read_to_string(machine2.join("from_m1.txt")).unwrap(),
        "hello from m1"
    );

    // Machine 2 pushes a file back
    let m2_git = GitCli::new(machine2.clone());
    std::fs::write(machine2.join("from_m2.txt"), "hello from m2").unwrap();
    m2_git.add_all().await.unwrap();
    m2_git.commit("m2 push").await.unwrap();
    m2_git.push().await.unwrap();

    // Machine 1 pulls and sees the file
    local_git.fetch().await.unwrap();
    local_git.pull_rebase().await.unwrap();
    assert!(local.join("from_m2.txt").exists());
    assert_eq!(
        std::fs::read_to_string(local.join("from_m2.txt")).unwrap(),
        "hello from m2"
    );
}
