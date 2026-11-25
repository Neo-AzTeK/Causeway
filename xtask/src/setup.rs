//! `cargo xtask setup`: what one clone needs once. Git hooks, `gh`'s default
//! repository, and the two git-ignored files about this machine:
//! `victory.toml` (where the title is; written once, then yours) and
//! `victory.hardware.toml` (its GPUs; generated, rewritten on each run).

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};

/// The user's configuration, read by `victory_core::Config`.
const CONFIG: &str = "victory.toml";
/// Apart from [`CONFIG`], whose loader rejects sections it does not know.
const HARDWARE: &str = "victory.hardware.toml";

pub fn run(root: &Path, game_dir: Option<&Path>) -> Result<()> {
    install_hooks(root)?;
    set_gh_repo(root)?;
    if let Some(dir) = game_dir {
        victory_core::ensure_outside_repo(dir)?;
        ensure!(dir.is_dir(), "{} is not a folder", dir.display());
    }
    let config = root.join(CONFIG);
    if write_config(&config, game_dir)? {
        println!("wrote {CONFIG}");
    } else if game_dir.is_some() {
        println!("{CONFIG} exists and was left as is: edit [paths].game_dir yourself");
    }
    let text = std::fs::read_to_string(&config).with_context(|| format!("reading {CONFIG}"))?;
    if !text.lines().any(|l| l.trim_start().starts_with("game_dir")) {
        println!("no game folder in {CONFIG}: set [paths].game_dir (see victory.example.toml)");
    }
    let gpus = gpus();
    std::fs::write(root.join(HARDWARE), hardware(&gpus))
        .with_context(|| format!("writing {HARDWARE}"))?;
    println!("wrote {HARDWARE} ({} GPUs listed)", gpus.len());
    Ok(())
}

fn install_hooks(root: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["config", "core.hooksPath", ".githooks"])
        .current_dir(root)
        .status()
        .context("running git")?;
    ensure!(status.success(), "git config failed (is this a git repository?)");
    println!("hooks installed: commits now run `cargo xtask policy --staged`");
    Ok(())
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
    ensure!(status.success(), "git config {key} failed");
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

/// Writes the configuration if there is none: whether it did.
// invariant(setup_never_overwrites_the_users_configuration): the file holds
// the user's own paths.
fn write_config(path: &Path, game_dir: Option<&Path>) -> Result<bool> {
    let game = match game_dir {
        Some(dir) => {
            let dir = dir.display().to_string();
            // A TOML literal string takes backslashes as they are, not `'`.
            ensure!(!dir.contains('\''), "{dir}: write this path in {CONFIG} by hand");
            format!("game_dir = '{dir}'")
        }
        None => r"# game_dir = 'D:\victory-data\game'".to_owned(),
    };
    let text = format!(
        "# This machine's paths, ignored by git. See victory.example.toml.\n[paths]\n{game}\n"
    );
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(path);
    match file {
        Ok(mut file) => {
            std::io::Write::write_all(&mut file, text.as_bytes())?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e).with_context(|| format!("creating {}", path.display())),
    }
}

/// This machine's display adapters as (name, driver version); none where they
/// cannot be listed.
// shortcut: names and drivers only. What a GPU can do is what a run logs
// ("host D3D12 caps"); probing it here would build the GPU crate for a hook.
fn gpus() -> Vec<(String, String)> {
    let script = "Get-CimInstance Win32_VideoController | \
                  ForEach-Object { $_.Name + '|' + $_.DriverVersion }";
    let out = Command::new("powershell").args(["-NoProfile", "-Command", script]).output();
    let Some(out) = out.ok().filter(|o| o.status.success()) else {
        return Vec::new();
    };
    parse_gpus(&String::from_utf8_lossy(&out.stdout))
}

fn parse_gpus(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| line.trim().split_once('|'))
        .filter(|(name, _)| !name.is_empty())
        .map(|(name, driver)| (name.to_owned(), driver.to_owned()))
        .collect()
}

fn hardware(gpus: &[(String, String)]) -> String {
    let mut text = String::from(
        "# This machine's GPUs, ignored by git. Generated by `cargo xtask setup`:\n\
         # run it again after a hardware or driver change. Capabilities (wave\n\
         # size, tiers) are in a run's log, line \"host D3D12 caps\".\n",
    );
    if gpus.is_empty() {
        text += "# No GPU could be listed.\n";
    }
    let quoted = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    for (name, driver) in gpus {
        text += &format!(
            "\n[[gpu]]\nname = \"{}\"\ndriver = \"{}\"\n",
            quoted(name),
            quoted(driver)
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{causeway_remote, hardware, parse_gpus, write_config};

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

    #[test]
    fn setup_never_overwrites_the_users_configuration() {
        let path = std::env::temp_dir().join(format!("causeway-setup-test-{}", std::process::id()));
        assert!(write_config(&path, None).unwrap());
        assert!(std::fs::read_to_string(&path).unwrap().contains("[paths]\n# game_dir"));
        std::fs::write(&path, "mine").unwrap();
        assert!(!write_config(&path, Some(std::path::Path::new("elsewhere"))).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn listed_gpus_become_the_hardware_file() {
        let gpus = parse_gpus("Some GPU \"X\"|1.2.3\r\n\r\nnoise\r\n|4\r\n");
        assert_eq!(gpus.len(), 1);
        let text = hardware(&gpus);
        assert!(text.contains("[[gpu]]\nname = \"Some GPU \\\"X\\\"\"\ndriver = \"1.2.3\""));
        assert!(hardware(&[]).contains("No GPU could be listed"));
    }
}
