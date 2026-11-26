//! Repository maintenance. Run with `cargo xtask <command>`.

mod capability;
mod comments;
mod compare;
mod policy;
mod queue;
mod report;
mod setup;
mod smoke;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Enforce the asset policy: the repository holds only small text files
    /// we wrote. Checks tracked and untracked-but-not-ignored files.
    Policy {
        /// Check only what is staged for commit (used by the pre-commit hook).
        #[arg(long)]
        staged: bool,
    },
    /// Set up a clone: git hooks, `gh`'s default repository, and the ignored
    /// files naming the title's folder and this machine's GPUs.
    #[command(alias = "install-hooks")]
    Setup {
        /// The title's folder, for a `victory.toml` not made yet.
        #[arg(long)]
        game_dir: Option<PathBuf>,
    },
    /// Run a title unattended with the diagnostics build, skip its menus and
    /// judge each graphics area. On a real GPU this can reset the driver.
    Smoke(Box<smoke::Smoke>),
    /// Judge one smoke run against earlier runs, per world line: speed against
    /// the spread, fidelity counters; exits with an error on a regression.
    Compare(compare::Compare),
    /// Smoke runs requested by many sessions, run one at a time by one runner:
    /// `queue add`, `queue run`, `queue status`.
    Queue(Box<queue::Queue>),
}

fn main() -> Result<()> {
    let root = workspace_root();
    match Cli::parse().command {
        Cmd::Policy { staged } => policy::run(&root, staged),
        Cmd::Smoke(args) => smoke::run(&root, &args),
        Cmd::Compare(args) => compare::run(&args),
        Cmd::Queue(args) => queue::run(&root, &args),
        Cmd::Setup { game_dir } => setup::run(&root, game_dir.as_deref()),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace")
        .into()
}
