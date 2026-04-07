use clap::{Parser, Subcommand};
use gitdrive_core::config::Config;
use gitdrive_core::conflict::{ConflictEvent, ConflictResolution};
use gitdrive_core::git::GitCli;
use gitdrive_core::lfs::LfsManager;
use gitdrive_core::sync::SyncEngine;
use gitdrive_core::watcher::FileWatcher;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Standard local folder: ~/gitdrive/
fn default_repo_path() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join("gitdrive")
}

/// Standard repo name on GitHub.
const GITHUB_REPO_NAME: &str = "gitdrive";

#[derive(Parser)]
#[command(
    name = "gitdrive",
    about = "Dropbox-like file sync backed by Git + LFS"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize GitDrive (syncs ~/gitdrive/ to github.com/<user>/gitdrive)
    Init {
        /// GitHub username (auto-detected from `gh` CLI if omitted)
        #[arg(short, long)]
        user: Option<String>,

        /// Branch name (default: main)
        #[arg(short, long, default_value = "main")]
        branch: String,
    },

    /// Start watching and syncing
    Watch {
        /// Path to config file (default: ~/.gitdrive/config.toml)
        #[arg(short, long)]
        config: Option<PathBuf>,
    },

    /// Show current sync status
    Status {
        /// Path to config file
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "gitdrive=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { user, branch } => {
            if let Err(e) = cmd_init(user, branch).await {
                error!("init failed: {e}");
                std::process::exit(1);
            }
        }
        Commands::Watch { config } => {
            if let Err(e) = cmd_watch(config).await {
                error!("watch failed: {e}");
                std::process::exit(1);
            }
        }
        Commands::Status { config } => {
            if let Err(e) = cmd_status(config).await {
                error!("status failed: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// Detect GitHub username from the `gh` CLI.
async fn detect_github_user() -> Option<String> {
    let output = tokio::process::Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let login = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if login.is_empty() {
        None
    } else {
        Some(login)
    }
}

async fn cmd_init(user: Option<String>, branch: String) -> gitdrive_core::error::Result<()> {
    // Resolve GitHub username
    let username = match user {
        Some(u) => u,
        None => {
            info!("detecting GitHub username via `gh` CLI...");
            detect_github_user().await.ok_or_else(|| {
                gitdrive_core::error::GitDriveError::Config(
                    "could not detect GitHub username. Install `gh` CLI and run `gh auth login`, or pass --user <username>".into(),
                )
            })?
        }
    };

    let path = default_repo_path();
    let remote = format!("git@github.com:{username}/{GITHUB_REPO_NAME}.git");

    info!("GitHub user: {username}");
    info!("local folder: {}", path.display());
    info!("remote: {remote}");

    let git = GitCli::new(path.clone());

    git.init().await?;
    git.add_remote(&remote).await?;

    // Set up Git LFS
    let lfs = LfsManager::new(git.clone());
    match lfs.ensure_lfs_installed().await {
        Ok(()) => {
            lfs.install().await?;
            lfs.init_default_tracking(&path).await?;
            info!("LFS configured with default binary extensions");
        }
        Err(e) => {
            warn!("git lfs not available ({e}) — large file tracking disabled. Install with: brew install git-lfs");
        }
    }

    let config = Config::new(path, remote, branch);

    let config_path = Config::default_path();
    config.save(&config_path)?;
    info!("config saved to {}", config_path.display());

    // Make an initial commit if the repo has no commits yet
    let has_commits = git.head_hash().await.is_ok();
    if !has_commits {
        git.add_all().await?;
        git.commit("gitdrive: initial commit").await?;
        git.push().await?;
        info!("pushed initial commit to remote");
    }

    info!("GitDrive initialized. Run `gitdrive watch` to start syncing.");
    Ok(())
}

async fn cmd_watch(config_path: Option<PathBuf>) -> gitdrive_core::error::Result<()> {
    let config_path = config_path.unwrap_or_else(Config::default_path);
    let config = Config::load(&config_path)?;
    info!("loaded config from {}", config_path.display());

    let git = GitCli::new(config.repo_path.clone());
    git.verify().await?;

    let (conflict_tx, mut conflict_notify_rx) = mpsc::channel::<ConflictEvent>(8);
    let (resolution_tx, resolution_rx) = mpsc::channel::<ConflictResolution>(8);

    tokio::spawn(async move {
        while let Some(event) = conflict_notify_rx.recv().await {
            warn!("CONFLICT in: {}", event.conflicted_files.join(", "));
            eprintln!("\n--- MERGE CONFLICT ---");
            for f in &event.conflicted_files {
                eprintln!("  - {f}");
            }
            eprintln!("  [1] Keep mine");
            eprintln!("  [2] Keep theirs");
            eprintln!("  [3] Abort rebase");
            eprint!("  Choice: ");

            let mut input = String::new();
            if std::io::stdin().read_line(&mut input).is_ok() {
                let resolution = match input.trim() {
                    "1" => ConflictResolution::KeepMine,
                    "2" => ConflictResolution::KeepTheirs,
                    "3" => ConflictResolution::AbortRebase,
                    _ => {
                        eprintln!("  Invalid choice, aborting rebase");
                        ConflictResolution::AbortRebase
                    }
                };
                let _ = resolution_tx.send(resolution).await;
            }
        }
    });

    let watcher = FileWatcher::new(
        &config.repo_path,
        config.debounce_ms,
        GitCli::new(config.repo_path.clone()),
    )?;

    let mut engine = SyncEngine::new(config, conflict_tx, resolution_rx)?;

    info!("watching for changes... (press Ctrl+C to stop)");
    engine.run(watcher).await
}

async fn cmd_status(config_path: Option<PathBuf>) -> gitdrive_core::error::Result<()> {
    let config_path = config_path.unwrap_or_else(Config::default_path);
    let config = Config::load(&config_path)?;

    let git = GitCli::new(config.repo_path.clone());
    git.verify().await?;

    let status = git.status().await?;
    if status.is_empty() {
        println!("Clean — no pending changes");
    } else {
        println!("Pending changes:");
        for s in &status {
            println!("  {}{} {}", s.index, s.worktree, s.path);
        }
    }

    let divergence = git.merge_base_check().await?;
    println!("Remote: {:?}", divergence);

    Ok(())
}
