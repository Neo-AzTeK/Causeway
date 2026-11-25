//! Repository maintenance. Run with `cargo xtask <command>`.

mod comments;
mod policy;
mod report;
mod smoke;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
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
    /// One-time clone setup: point git at .githooks/ so every commit runs the
    /// policy check, and make `gh` target Causeway rather than its parent.
    InstallHooks,
    /// Run a title unattended with the diagnostics build, skip its menus and
    /// judge each graphics area. On a real GPU this can reset the driver.
    Smoke(Box<smoke::Smoke>),
}

fn main() -> Result<()> {
    let root = workspace_root();
    match Cli::parse().command {
        Cmd::Policy { staged } => policy::run(&root, staged),
        Cmd::Smoke(args) => smoke::run(&root, &args),
        Cmd::InstallHooks => {
            let status = Command::new("git")
                .args(["config", "core.hooksPath", ".githooks"])
                .current_dir(&root)
                .status()
                .context("running git")?;
            anyhow::ensure!(
                status.success(),
                "git config failed (is this a git repository?)"
            );
            println!("hooks installed: commits now run `cargo xtask policy --staged`");
            set_gh_repo(&root)
        }
    }
}

/// Marks Causeway's remote as `gh`'s default repository (what `gh repo
/// set-default` writes): on a fork, `gh` may otherwise target the parent.
fn set_gh_repo(root: &Path) -> Result<()> {
    let git = |args: &[&str]| Command::new("git").args(args).current_dir(root).output();
    let remotes = git(&["remote", "-v"]).context("running git")?;
    let remotes = String::from_utf8_lossy(&remotes.stdout);
    let Some(name) = causeway_remote(&remotes) else {
        println!("no Neo-AzteK/Causeway remote: add one and rerun, or `gh` may target the parent");
        return Ok(());
    };
    let key = format!("remote.{name}.gh-resolved");
    let status = git(&["config", &key, "base"]).context("running git")?.status;
    anyhow::ensure!(status.success(), "git config {key} failed");
    println!("`gh` targets Neo-AzteK/Causeway (remote {name})");
    Ok(())
}

/// The name of the remote in `git remote -v` output whose URL is Causeway's.
fn causeway_remote(remotes: &str) -> Option<&str> {
    remotes.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let (name, url) = (words.next()?, words.next()?.to_ascii_lowercase());
        let url = url.trim_end_matches('/').trim_end_matches(".git");
        let host = url.strip_suffix("neo-aztek/causeway")?;
        host.ends_with(['/', ':']).then_some(name)
    })
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace")
        .into()
}

#[cfg(test)]
mod tests {
    use super::causeway_remote;

    #[test]
    fn the_causeway_remote_is_found_by_url() {
        let parent = "origin\thttps://github.com/Marvisak/VIctory.git (fetch)\n";
        let ssh = "up\tgit@github.com:Neo-AzTeK/Causeway.git (push)\n";
        let https = "origin\thttps://github.com/Neo-AzteK/Causeway (fetch)";
        let look_alike = "o\thttps://github.com/notneo-aztek/causeway (fetch)";
        assert_eq!(causeway_remote(&format!("{parent}{ssh}")), Some("up"));
        assert_eq!(causeway_remote(https), Some("origin"));
        assert_eq!(causeway_remote(parent), None);
        assert_eq!(causeway_remote(look_alike), None);
    }
}
