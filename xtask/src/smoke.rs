//! Runs a title unattended with the diagnostics build, skips its menus with
//! scripted keys, and reports what the log says (see `report`). Everything
//! it writes is derived from the title, so it stays outside the repository.

use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use clap::Args;

use crate::report::{End, Log, Status, flat_png};

#[derive(Args)]
pub struct Smoke {
    /// The title executable.
    #[arg(long, required_unless_present = "log")]
    exe: Option<PathBuf>,
    /// Where the report, log, frames and saves go: outside the repository,
    /// and new or empty unless only judging a log.
    #[arg(long)]
    out: PathBuf,
    /// How long the title runs.
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=86400))]
    seconds: u64,
    /// The title's saves and settings; a folder in --out by default, so
    /// every run goes through the first-start menus.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// `VICTORY_KEYS` instead of the menu-skipping keys.
    #[arg(long)]
    keys: Option<String>,
    /// A diagnostics build to run instead of building one.
    #[arg(long)]
    exe_victory: Option<PathBuf>,
    /// Judge this log; nothing runs.
    #[arg(long, conflicts_with_all = ["exe", "keys", "exe_victory", "data_dir"])]
    log: Option<PathBuf>,
}

/// A frame is saved this often, at the rate the title presents at most.
const CAPTURE_EVERY: u64 = 100;
const CAPTURE_FPS: u64 = 30;
/// Frames saved at most: an environment variable holds 32,767 characters,
/// and these numbers have 7 digits.
const CAPTURES: u64 = 3000;

/// Our variables the run still takes from the environment: where its
/// configuration and caches are, and the adapter. Any other may change what
/// runs or what is logged.
const INHERITED: &[&str] =
    &["VICTORY_CONFIG", "VICTORY_GAME_DIR", "VICTORY_CACHE_DIR", "VICTORY_D3D12_WARP"];

pub fn run(root: &Path, args: &Smoke) -> Result<()> {
    victory_core::ensure_outside_repo(&args.out)?;
    let (log, end) = match &args.log {
        Some(log) => (log.clone(), None),
        None => {
            let (log, end) = launch(root, args)?;
            (log, Some(end))
        }
    };
    let bytes = std::fs::read(&log).with_context(|| format!("reading {}", log.display()))?;
    let mut log = Log::parse(&String::from_utf8_lossy(&bytes));
    log.ended = end.map(|(_, at)| at);
    let areas = log.judge(end.map(|(end, _)| end), |png| flat_png(&std::fs::read(png).ok()?));
    let text = log.text(&areas);
    create(&args.out)?;
    std::fs::write(args.out.join("report.txt"), &text).context("writing report.txt")?;
    std::fs::write(args.out.join("report.json"), log.json(&areas))
        .context("writing report.json")?;
    print!("{text}");
    let failed = areas.iter().filter(|a| a.status == Status::Fail).count();
    ensure!(failed == 0, "{failed} areas failed");
    Ok(())
}

fn create(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))
}

fn launch(root: &Path, args: &Smoke) -> Result<(PathBuf, (End, f64))> {
    let exe = args.exe.as_ref().context("--exe is required")?;
    ensure!(exe.is_file(), "{} is not a file", exe.display());
    let data = args.data_dir.clone().unwrap_or_else(|| args.out.join("data"));
    victory_core::ensure_outside_repo(&data)?;
    let keys = args.keys.clone().unwrap_or_else(|| menu_keys(args.seconds));
    check_keys(&keys)?;
    create(&args.out)?;
    // An old run's frames and saves would pass for this one's.
    ensure!(
        std::fs::read_dir(&args.out)?.next().is_none(),
        "{} is not empty",
        args.out.display()
    );
    let victory = match &args.exe_victory {
        Some(path) => path.clone(),
        None => build(root)?,
    };
    let out = std::path::absolute(&args.out)?;
    let frames = out.join("frames");
    create(&frames)?;
    create(&data)?;

    let log = out.join("log.txt");
    let file = File::create(&log).context("creating log.txt")?;
    let mut command = Command::new(&victory);
    clean(&mut command, std::env::vars_os().map(|(name, _)| name));
    command
        .arg("run")
        .arg(std::path::absolute(exe)?)
        .arg("--execute")
        .env("VICTORY_LOG", "info")
        // Device-removed data costs little; without it a removal in an
        // unwatched run is "no breadcrumbs" (`clean` drops the caller's).
        .env("VICTORY_D3D12_DRED", "1")
        .env("VICTORY_KEYS", &keys)
        .env("VICTORY_CAPTURE", &frames)
        .env("VICTORY_CAPTURE_FRAMES", capture_frames(args.seconds))
        .env("VICTORY_DATA_DIR", std::path::absolute(&data)?)
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file);
    let end = run_for(command, Duration::from_secs(args.seconds))
        .with_context(|| format!("running {}", victory.display()))?;
    Ok((log, end))
}

/// Takes out of what a run inherits what would change it or its log.
fn clean(command: &mut Command, names: impl Iterator<Item = OsString>) {
    for name in names {
        // Windows does not tell case apart.
        let upper = name.to_string_lossy().to_ascii_uppercase();
        let ours = upper.starts_with("VICTORY_") && !INHERITED.contains(&upper.as_str());
        if ours || upper == "RUST_LOG" {
            command.env_remove(name);
        }
    }
}

/// `VICTORY_CAPTURE_FRAMES` for a run this long.
fn capture_frames(seconds: u64) -> String {
    let frames = seconds * CAPTURE_FPS;
    let every = CAPTURE_EVERY.max(frames.div_ceil(CAPTURES));
    let listed: Vec<_> = (1..=frames / every).map(|i| (i * every).to_string()).collect();
    listed.join(",")
}

/// Builds the diagnostics build; its path.
fn build(root: &Path) -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let built = Command::new(cargo)
        .args(["build", "--locked", "--profile", "diag", "-p", "victory"])
        .args(["--features", "diagnostics"])
        .args(["--message-format", "json-render-diagnostics"])
        .current_dir(root)
        .stderr(Stdio::inherit())
        .output()
        .context("running cargo")?;
    ensure!(built.status.success(), "the diagnostics build failed");
    let messages = String::from_utf8(built.stdout).context("cargo's messages")?;
    executable(&messages).context("cargo named no executable")
}

/// The executable in cargo's JSON messages (one is built, named last).
fn executable(messages: &str) -> Option<PathBuf> {
    let key = "\"executable\":\"";
    let text = messages.lines().rev().find_map(|l| Some(&l[l.find(key)? + key.len()..]))?;
    let mut path = String::new();
    let mut chars = text.chars();
    loop {
        match chars.next()? {
            '"' => return Some(path.into()),
            // A path on Windows has no other escapes.
            '\\' => path.push(chars.next()?),
            c => path.push(c),
        }
    }
}

/// Runs a process for at most `time`, then ends it and what it started.
/// Also when it ended, or was ended (Unix time).
fn run_for(mut command: Command, time: Duration) -> Result<(End, f64)> {
    let now = || SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64());
    let group = Group::new()?;
    let mut child = command.spawn()?;
    if let Err(e) = group.add(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    let start = Instant::now();
    while start.elapsed() < time {
        // A title that relaunches itself goes on in a child.
        if let Some(status) = child.try_wait()?
            && !group.running()?
        {
            return Ok((End::Exited(status.code()), now()?));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Before ending it, which takes a while for a process of this size.
    let ended = now()?;
    group.kill(&mut child)?;
    child.wait()?;
    Ok((End::TimedOut, ended))
}

/// A job: the process and what it starts, and nothing else. They end with
/// this process too.
#[cfg(windows)]
struct Group(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Group {
    fn new() -> Result<Group> {
        use windows_sys::Win32::System::JobObjects::*;
        // SAFETY: plain creation, no name or security attributes.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        ensure!(!job.is_null(), std::io::Error::last_os_error());
        let group = Group(job);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the structure of this class, with its size.
        let set = unsafe {
            SetInformationJobObject(
                group.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of_val(&limits) as u32,
            )
        };
        ensure!(set != 0, std::io::Error::last_os_error());
        Ok(group)
    }

    // shortcut: what the process starts before this is not in the job; it
    // is still loading its DLLs.
    fn add(&self, child: &Child) -> Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        // SAFETY: both handles are live.
        let added = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) };
        ensure!(added != 0, std::io::Error::last_os_error());
        Ok(())
    }

    fn running(&self) -> Result<bool> {
        use windows_sys::Win32::System::JobObjects::*;
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: the structure of this class, with its size.
        let read = unsafe {
            QueryInformationJobObject(
                self.0,
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast(),
                size_of_val(&info) as u32,
                std::ptr::null_mut(),
            )
        };
        ensure!(read != 0, std::io::Error::last_os_error());
        Ok(info.ActiveProcesses > 0)
    }

    fn kill(&self, _: &mut Child) -> Result<()> {
        // SAFETY: a live job.
        let ended =
            unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1) };
        ensure!(ended != 0, std::io::Error::last_os_error());
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for Group {
    fn drop(&mut self) {
        // SAFETY: the job is ours; closing it ends what is still in it.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

// shortcut: elsewhere only the process itself is watched and ended; titles
// run on Windows only.
#[cfg(not(windows))]
struct Group;

#[cfg(not(windows))]
impl Group {
    fn new() -> Result<Group> {
        Ok(Group)
    }

    fn add(&self, _: &Child) -> Result<()> {
        Ok(())
    }

    fn running(&self) -> Result<bool> {
        Ok(false)
    }

    fn kill(&self, child: &mut Child) -> Result<()> {
        Ok(child.kill()?)
    }
}

/// Enter, for screens that it passes, then Down, Down, Enter for lists whose
/// third entry goes on (there Enter alone picks the first for ever). Every
/// 8 seconds, from 8 seconds after input starts.
fn menu_keys(seconds: u64) -> String {
    // A range that is never empty, however short the run.
    let end = seconds.max(60);
    [("8", "Enter"), ("12", "Down"), ("12.6", "Down"), ("13.2", "Enter")]
        .map(|(from, key)| format!("{from}-{end}/8:{key}"))
        .join(",")
}

/// Rejects what the runtime would skip without a word. The syntax is
/// `parse_script`'s in `victory-hle`'s gameinput.rs.
fn check_keys(list: &str) -> Result<()> {
    for entry in list.split(',').filter(|e| !e.trim().is_empty()) {
        check_key(entry).with_context(|| format!("key entry {entry:?}"))?;
    }
    Ok(())
}

fn check_key(entry: &str) -> Result<()> {
    let (when, key) = entry.split_once(':').context("expected <when>:<key>")?;
    let key = key.trim();
    let named = ["Enter", "Esc", "Tab", "Back", "Up", "Down", "Left", "Right"].contains(&key);
    let single = key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric();
    ensure!(named || single, "unknown key {key:?}");
    let seconds = |s: &str| -> Result<f64> {
        let s: f64 = s.trim().parse().context("expected seconds")?;
        Duration::try_from_secs_f64(s).context("expected seconds")?;
        Ok(s)
    };
    let Some((from, rest)) = when.split_once('-') else {
        seconds(when)?;
        return Ok(());
    };
    let (to, every) = match rest.split_once('/') {
        Some((to, every)) => (to, Some(every)),
        None => (rest, None),
    };
    ensure!(seconds(from)? < seconds(to)?, "an empty range");
    if let Some(every) = every {
        ensure!(seconds(every)? >= 0.4, "a period under 0.4 s");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_lists_are_checked() {
        for good in ["", "30:x", "12-280/4:Enter,285-300:W", " 1.5-2 : Down"] {
            assert!(check_keys(good).is_ok(), "{good}");
        }
        for bad in ["30", "30:Space", "x:A", "-1:A", "9-5:A", "5-9/0.1:A", "5-9/q:A", "1:A,junk"] {
            assert!(check_keys(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn menu_keys_are_valid_and_interleaved() {
        check_keys(&menu_keys(1)).unwrap();
        assert_eq!(
            menu_keys(300),
            "8-300/8:Enter,12-300/8:Down,12.6-300/8:Down,13.2-300/8:Enter"
        );
        check_keys(&menu_keys(300)).unwrap();
    }

    #[test]
    fn inherited_debug_variables_are_taken_out() {
        let mut command = Command::new("x");
        let names = [
            "VICTORY_LOG",
            "RUST_LOG",
            "victory_dump_targets",
            "VICTORY_SKIP_PIPELINES",
            "PATH",
            "VICTORY_D3D12_WARP",
            "VICTORY_CACHE_DIR",
        ];
        clean(&mut command, names.iter().map(OsString::from));
        let mut removed: Vec<_> =
            command.get_envs().map(|(n, _)| n.to_string_lossy().to_uppercase()).collect();
        removed.sort();
        assert_eq!(
            removed,
            ["RUST_LOG", "VICTORY_DUMP_TARGETS", "VICTORY_LOG", "VICTORY_SKIP_PIPELINES"]
        );
    }

    #[test]
    fn captured_frames_fit_a_variable() {
        assert_eq!(capture_frames(10), "100,200,300");
        assert_eq!(capture_frames(1), "");
        assert!((10_000..32_000).contains(&capture_frames(86400).len()));
    }

    #[test]
    fn cargo_names_the_executable() {
        let messages = r#"{"executable":null}
{"a":1,"executable":"C:\\my t\\victory.exe","fresh":true}
{"success":true}"#;
        assert_eq!(executable(messages), Some(r"C:\my t\victory.exe".into()));
        assert_eq!(executable("{}"), None);
    }

    #[test]
    fn an_out_inside_the_repository_is_refused() {
        let root = crate::workspace_root();
        let args = Smoke {
            exe: None,
            out: root.join("smoke-out"),
            seconds: 1,
            data_dir: None,
            keys: None,
            exe_victory: None,
            log: Some(root.join("README.md")),
        };
        let error = run(&root, &args).unwrap_err();
        assert!(error.is::<victory_core::PolicyError>(), "{error:#}");
        assert!(!args.out.exists());
    }

    #[cfg(windows)]
    #[test]
    fn what_outlives_its_time_is_ended_with_its_children() {
        let shell = |script: &str| {
            let mut c = Command::new("cmd");
            c.args(["/d", "/c", script]).stdout(Stdio::null());
            c
        };
        let exited = run_for(shell("exit 3"), Duration::from_secs(60)).unwrap();
        assert_eq!(exited.0, End::Exited(Some(3)));

        // The grandchild holds the pipe: reading ends only when it is gone.
        let (mut read, write) = std::io::pipe().unwrap();
        let mut command = shell("ping -n 60 127.0.0.1");
        command.stdout(write);
        let start = Instant::now();
        assert_eq!(run_for(command, Duration::from_secs(1)).unwrap().0, End::TimedOut);
        std::io::Read::read_to_end(&mut read, &mut Vec::new()).unwrap();
        assert!(start.elapsed() < Duration::from_secs(30));
    }
}
