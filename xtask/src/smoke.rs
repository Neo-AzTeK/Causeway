//! Runs a title unattended with the diagnostics build, skips its menus with
//! scripted keys, and reports what the log says (see `report`). Everything
//! it writes is derived from the title, so it stays outside the repository.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use clap::Args;

use crate::load;
use crate::report::sheet;
use crate::report::{Baseline, Context as Inputs, End, Log, Status, flat_png, read_baseline};

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
    /// Ends the run once this many world lines are logged after the world's
    /// warm-up. --seconds stays the cap: a run that ends short of them fails
    /// the "steady world lines" area.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=100000))]
    world_lines: Option<u64>,
    /// Before the title starts, waits at most this long for a quiet machine
    /// (no compiler running, under 30% of the CPU and enough free memory, see
    /// --quiet-commit-gb and --quiet-ram-gb, for 20 s); 0 starts at
    /// once. What other processes use while it plays goes to `load.txt`.
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(0..=86400))]
    quiet_wait: u64,
    /// A quiet machine also has this much commit free, in GB (what the system
    /// can still promise; the title holds ~32 GB private); 0 asks for none.
    #[arg(long, default_value_t = load::QUIET_COMMIT_GB, value_parser = clap::value_parser!(u64).range(0..=4096))]
    quiet_commit_gb: u64,
    /// ... and this much physical RAM free, in GB; 0 asks for none.
    #[arg(long, default_value_t = load::QUIET_RAM_GB, value_parser = clap::value_parser!(u64).range(0..=4096))]
    quiet_ram_gb: u64,
    /// The title's saves and settings; a folder in --out by default, so
    /// every run goes through the first-start menus (see --accept-terms).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// A data folder already set up, copied into --out/data for this run; the
    /// source is not changed.
    #[arg(long, conflicts_with_all = ["data_dir", "log"])]
    data_from: Option<PathBuf>,
    /// Lets scripted keys run on a data folder not set up yet, where they can
    /// tick the title's legal terms. Needs the owner's OK for that launch.
    #[arg(long, conflicts_with = "log")]
    accept_terms: bool,
    /// `VICTORY_KEYS` instead of the menu-skipping keys.
    #[arg(long)]
    keys: Option<String>,
    /// A diagnostics build to run instead of building one.
    #[arg(long)]
    exe_victory: Option<PathBuf>,
    /// Judge this log; nothing runs.
    #[arg(long, conflicts_with_all = ["exe", "keys", "exe_victory", "data_dir"])]
    log: Option<PathBuf>,
    /// `NAME=VALUE`: a `VICTORY_*` switch the game process gets (the runner
    /// otherwise removes them, see `INHERITED`). With `--log`: a switch the
    /// run is said to have had, which the digest checks against the log.
    #[arg(long = "env", value_name = "NAME=VALUE", value_parser = parse_env)]
    envs: Vec<(String, String)>,
    /// An earlier run's report.json: the digest shows each metric against it.
    #[arg(long)]
    baseline: Option<PathBuf>,
}

fn parse_env(text: &str) -> Result<(String, String), String> {
    let (name, value) = text.split_once('=').ok_or("expected NAME=VALUE")?;
    let name = name.trim().to_ascii_uppercase();
    if !name.starts_with("VICTORY_") {
        return Err(format!("{name}: only VICTORY_* switches pass through"));
    }
    Ok((name, value.to_owned()))
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
    let log_path = log;
    let mut log = Log::parse(&String::from_utf8_lossy(&bytes));
    log.ended = end.map(|(_, at)| at);
    let mut areas = log.judge(end.map(|(end, _)| end), |png| flat_png(&std::fs::read(png).ok()?));
    if let Some(asked) = args.world_lines {
        areas.push(log.steady_area(asked as usize, end.map(|(end, _)| end)));
    }
    if let Some(record) = load::Record::beside(&log_path) {
        areas.push(record.area(log.measured_window()));
    }
    let text = log.text(&areas);
    create(&args.out)?;

    // The digest: pictures first, for the metrics hold their measure.
    let frames: Vec<PathBuf> =
        log.world_frames().into_iter().map(|f| beside(&log_path, f)).collect();
    let visual = sheet::build(&frames, &args.out, |p| std::fs::read(p).ok());
    let (build, mut requested) = run_info(root, &log_path);
    requested.extend(args.envs.iter().cloned());
    let baseline = match &args.baseline {
        Some(json) => read_baseline(json),
        None => Baseline::Off,
    };
    let metrics = log.metrics(visual.mean().map(|m| m * 100.0));
    let digest = log.digest(&areas, &Inputs { build, requested, baseline, visual });

    std::fs::write(args.out.join("report.txt"), &text).context("writing report.txt")?;
    std::fs::write(args.out.join("report.json"), log.json(&areas, &metrics))
        .context("writing report.json")?;
    std::fs::write(args.out.join("digest.md"), digest).context("writing digest.md")?;
    print!("{text}");
    let failed = areas.iter().filter(|a| a.status == Status::Fail).count();
    ensure!(failed == 0, "{failed} areas failed");
    Ok(())
}

/// A frame the log names, found where the log is if it moved.
fn beside(log: &Path, frame: PathBuf) -> PathBuf {
    if frame.exists() {
        return frame;
    }
    let moved = frame.file_name().map(|n| log.with_file_name("frames").join(n));
    moved.filter(|m| m.exists()).unwrap_or(frame)
}

/// What the run's `run.txt` says (commit, branch, switches it asked for), as
/// the digest's build line and requested switches.
fn run_info(root: &Path, log: &Path) -> (String, Vec<(String, String)>) {
    let text = std::fs::read_to_string(log.with_file_name("run.txt")).unwrap_or_default();
    let get = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).map(str::to_owned);
    let requested: Vec<_> = text
        .lines()
        .filter_map(|l| l.strip_prefix("env ")?.split_once('='))
        .map(|(n, v)| (n.to_owned(), v.to_owned()))
        .collect();
    let build = match (get("commit "), get("branch ")) {
        (Some(commit), branch) => format!(
            "build commit: {commit} branch: {}{}",
            branch.as_deref().unwrap_or("?"),
            if get("dirty ").as_deref() == Some("yes") { " (uncommitted changes)" } else { "" }
        ),
        _ => {
            let (commit, branch) = git_state(root);
            format!(
                "build commit: unknown (judged an existing log); judged by {commit} on {branch}"
            )
        }
    };
    (build, requested)
}

/// The checkout's commit, branch, and whether it has uncommitted changes.
fn git_state(root: &Path) -> (String, String) {
    let git = |args: &[&str]| {
        let out = Command::new("git").args(args).current_dir(root).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    let commit = git(&["rev-parse", "--short=10", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let branch = git(&["branch", "--show-current"]).unwrap_or_else(|| "unknown".into());
    (commit, branch)
}

/// Written beside the log: what the digest needs of the build and of what
/// the runner set. Switch values are not secrets; they stay out of the repo.
fn write_run_info(root: &Path, out: &Path, env: &[(String, OsString)]) -> Result<()> {
    let (commit, branch) = git_state(root);
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(root)
        .output()
        .is_ok_and(|o| !o.stdout.is_empty());
    let mut text = format!("commit {commit}\nbranch {branch}\ndirty {}\n", if dirty { "yes" } else { "no" });
    for (name, value) in env {
        text += &format!("env {name}={}\n", value.to_string_lossy().replace(['\r', '\n'], " "));
    }
    std::fs::write(out.join("run.txt"), text).context("writing run.txt")
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
    if let Some(from) = &args.data_from {
        victory_core::ensure_outside_repo(from)?;
        ensure!(from.is_dir(), "{} is not a folder", from.display());
    }
    // invariant(scripted_keys_need_a_set_up_data_folder_or_the_terms_accepted): a
    // script never accepts the title's terms unasked.
    let set_up = args.data_from.as_ref().or(args.data_dir.as_ref());
    check_terms(&keys, set_up.map(PathBuf::as_path), args.accept_terms)?;
    // Held until the title ends: two runs can reset the display driver.
    let _only = one_run(&std::env::temp_dir().join("causeway-smoke.lock"))?;
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
    if let Some(from) = &args.data_from {
        copy_dir(from, &data)?;
    }

    // After the build and the copies, so that nothing of ours is going on when
    // the title starts. `log.txt` is made after it: it says the title started.
    let wait = match args.quiet_wait {
        0 => None,
        cap => {
            println!("waiting for a quiet machine ({cap} s at most)");
            let need = load::Need::gb(args.quiet_commit_gb, args.quiet_ram_gb);
            let wait = load::wait_quiet(Duration::from_secs(cap), LOOK, need);
            if let Some(w) = wait {
                let how = if w.quiet { "quiet" } else { "never quiet, starting anyway" };
                println!("{how} after {} s", w.seconds);
            }
            wait
        }
    };
    let record_path = out.join(load::FILE);
    std::fs::write(&record_path, load::Record::header(wait)).context("writing load.txt")?;
    let mut record = std::fs::OpenOptions::new().append(true).open(&record_path)?;

    let log = out.join("log.txt");
    let file = File::create(&log).context("creating log.txt")?;
    let mut command = Command::new(&victory);
    clean(&mut command, std::env::vars_os().map(|(name, _)| name));
    // What the game process is meant to see: the runner's own, then the
    // owner's `--env` (which win); the digest checks the log against these.
    let mut set: Vec<(String, OsString)> = vec![
        ("VICTORY_LOG".into(), "info".into()),
        // Device-removed data costs little; without it a removal in an
        // unwatched run is "no breadcrumbs" (`clean` drops the caller's).
        ("VICTORY_D3D12_DRED".into(), "1".into()),
        // A removal is logged (the digest still reports it) and the run goes
        // on to its end, so the rest of the menus and the log are kept.
        ("VICTORY_SURVIVE_REMOVAL".into(), "1".into()),
        ("VICTORY_KEYS".into(), keys.clone().into()),
        ("VICTORY_CAPTURE".into(), frames.clone().into_os_string()),
        ("VICTORY_CAPTURE_FRAMES".into(), capture_frames(args.seconds).into()),
        ("VICTORY_DATA_DIR".into(), std::path::absolute(&data)?.into_os_string()),
    ];
    for (name, value) in &args.envs {
        set.retain(|(n, _)| n != name);
        set.push((name.clone(), value.into()));
    }
    write_run_info(root, &out, &set)?;
    for (name, value) in &set {
        command.env(name, value);
    }
    command
        .arg("run")
        .arg(std::path::absolute(exe)?)
        .arg("--execute")
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file);
    let mut watch = args.world_lines.map(|asked| Watch::new(&log, asked as usize));
    let mut meter = load::Meter::default();
    let look = |title: Option<u64>| {
        let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64());
        if let (Ok(at), Some(now), Some(title)) = (at, load::system_times(), title)
            && let Some(mut sample) = meter.sample(at, now, title)
        {
            sample.compilers = load::compilers_running();
            // A look that cannot be written is one less.
            let _ = record.write_all(load::Record::line(sample).as_bytes());
        }
        watch.as_mut().is_some_and(Watch::enough)
    };
    let end = run_for(command, Duration::from_secs(args.seconds), look)
        .with_context(|| format!("running {}", victory.display()))?;
    Ok((log, end))
}

/// What `one_run` says when it fails: the queue runner puts a run back for it.
pub(crate) const LOCK_BUSY: &str = "is not free";

/// Whether no smoke run holds the machine's lock now.
pub(crate) fn lock_free() -> bool {
    one_run(&std::env::temp_dir().join("causeway-smoke.lock")).is_ok()
}

/// The lock one smoke run at a time holds on this machine: free once dropped
/// or once the process is gone.
// shortcut: a title started by hand (`victory run`) does not take it.
fn one_run(path: &Path) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    // invariant(one_smoke_run_at_a_time): no sharing, so a second open fails
    // while the first handle lives.
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 0);
    options
        .open(path)
        .with_context(|| format!("is another smoke run on? {} {LOCK_BUSY}", path.display()))
}

/// Refuses scripted keys where they could tick the title's legal terms: on a
/// data folder that is missing or empty.
// shortcut: a folder with any file counts as set up, so one left at the terms
// screen passes.
fn check_terms(keys: &str, data: Option<&Path>, accepted: bool) -> Result<()> {
    let set_up = data
        .and_then(|d| std::fs::read_dir(d).ok())
        .is_some_and(|mut entries| entries.next().is_some());
    ensure!(
        accepted || set_up || keys.trim().is_empty(),
        "scripted keys on a data folder not set up can tick the title's legal terms: \
         pass --data-from or --data-dir with one set up, --keys \"\" to play by hand, \
         or --accept-terms with the owner's OK for this launch"
    );
    Ok(())
}

/// Copies the files and folders under `from` into `to`.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.context("reading --data-from")?;
        let target = to.join(entry.path().strip_prefix(from)?);
        if entry.file_type().is_dir() {
            create(&target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
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

/// The diagnostics build a run makes (`queue run` makes it ahead of one).
pub(crate) const DIAG_BUILD: &[&str] =
    &["build", "--locked", "--profile", "diag", "-p", "victory", "--features", "diagnostics"];

/// Builds the diagnostics build; its path.
fn build(root: &Path) -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let built = Command::new(cargo)
        .args(DIAG_BUILD)
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

/// How often a running title's log is read.
const LOOK: Duration = Duration::from_secs(2);

/// A log read as it grows.
pub(crate) struct Follow {
    log_file: PathBuf,
    /// Bytes read, and the last line while it is not whole.
    at: u64,
    rest: Vec<u8>,
    log: Log,
}

impl Follow {
    pub(crate) fn new(log_file: &Path) -> Follow {
        Follow { log_file: log_file.to_owned(), at: 0, rest: Vec::new(), log: Log::default() }
    }

    /// The log's whole lines so far; what cannot be read yet is not in it.
    pub(crate) fn read(&mut self) -> &Log {
        let mut new = Vec::new();
        if let Ok(mut file) = File::open(&self.log_file)
            && file.seek(SeekFrom::Start(self.at)).is_ok()
            && file.read_to_end(&mut new).is_ok()
        {
            self.at += new.len() as u64;
            self.rest.append(&mut new);
        }
        if let Some(end) = self.rest.iter().rposition(|&b| b == b'\n') {
            let whole: Vec<u8> = self.rest.drain(..=end).collect();
            self.log.feed(&String::from_utf8_lossy(&whole));
        }
        &self.log
    }
}

/// Follows the growing log for `--world-lines`.
struct Watch {
    log: Follow,
    asked: usize,
    reached: bool,
}

impl Watch {
    fn new(log_file: &Path, asked: usize) -> Watch {
        Watch { log: Follow::new(log_file), asked, reached: false }
    }

    /// Whether the lines asked for were there at the look before this one: the
    /// run goes on for one more, so that what a world line brings with it
    /// (its drops, by cause and reason) is in the log. A log that cannot be
    /// read is never enough: the run goes to its cap.
    fn enough(&mut self) -> bool {
        let steady = self.log.read().steady_lines();
        std::mem::replace(&mut self.reached, steady >= self.asked)
    }
}

/// Runs a process for at most `time`, or until `enough` says so, then ends it
/// and what it started. `enough` is asked every `LOOK`, and given the processor
/// time its processes have had (100 ns). Also when it ended, or was ended
/// (Unix time).
fn run_for(
    mut command: Command,
    time: Duration,
    mut enough: impl FnMut(Option<u64>) -> bool,
) -> Result<(End, f64)> {
    let now = || SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64());
    let group = Group::new()?;
    let mut child = command.spawn()?;
    if let Err(e) = group.add(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    let start = Instant::now();
    let mut looked = start;
    let mut end = End::TimedOut;
    while start.elapsed() < time {
        // A title that relaunches itself goes on in a child.
        if let Some(status) = child.try_wait()?
            && !group.running()?
        {
            return Ok((End::Exited(status.code()), now()?));
        }
        if looked.elapsed() >= LOOK {
            looked = Instant::now();
            if enough(group.cpu_time()) {
                end = End::Enough;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Before ending it, which takes a while for a process of this size.
    let ended = now()?;
    group.kill(&mut child)?;
    child.wait()?;
    Ok((end, ended))
}

/// A job: the process and what it starts, and nothing else. They end with
/// this process too.
#[cfg(windows)]
pub(crate) struct Group(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Group {
    pub(crate) fn new() -> Result<Group> {
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
    pub(crate) fn add(&self, child: &Child) -> Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        // SAFETY: both handles are live.
        let added = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) };
        ensure!(added != 0, std::io::Error::last_os_error());
        Ok(())
    }

    fn running(&self) -> Result<bool> {
        Ok(self.accounting()?.ActiveProcesses > 0)
    }

    /// The processor time of everything that is or was in the job, in 100 ns.
    fn cpu_time(&self) -> Option<u64> {
        let info = self.accounting().ok()?;
        u64::try_from(info.TotalUserTime + info.TotalKernelTime).ok()
    }

    fn accounting(
        &self,
    ) -> Result<windows_sys::Win32::System::JobObjects::JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>
    {
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
        Ok(info)
    }

    pub(crate) fn kill(&self, _: &mut Child) -> Result<()> {
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
pub(crate) struct Group;

#[cfg(not(windows))]
impl Group {
    pub(crate) fn new() -> Result<Group> {
        Ok(Group)
    }

    pub(crate) fn add(&self, _: &Child) -> Result<()> {
        Ok(())
    }

    fn running(&self) -> Result<bool> {
        Ok(false)
    }

    fn cpu_time(&self) -> Option<u64> {
        None
    }

    pub(crate) fn kill(&self, child: &mut Child) -> Result<()> {
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
    #[cfg(windows)]
    fn one_smoke_run_at_a_time() {
        let path = std::env::temp_dir().join(format!("causeway-lock-test-{}", std::process::id()));
        let first = one_run(&path).unwrap();
        assert!(one_run(&path).is_err());
        drop(first);
        drop(one_run(&path).unwrap());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn scripted_keys_need_a_set_up_data_folder_or_the_terms_accepted() {
        let dir = std::env::temp_dir().join(format!("causeway-terms-test-{}", std::process::id()));
        let keys = menu_keys(60);
        assert!(check_terms(&keys, None, false).is_err());
        std::fs::create_dir_all(&dir).unwrap();
        assert!(check_terms(&keys, Some(&dir), false).is_err());
        check_terms("", Some(&dir), false).unwrap();
        check_terms(&keys, Some(&dir), true).unwrap();
        std::fs::write(dir.join("settings"), "x").unwrap();
        check_terms(&keys, Some(&dir), false).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_out_inside_the_repository_is_refused() {
        let root = crate::workspace_root();
        let args = Smoke {
            exe: None,
            out: root.join("smoke-out"),
            seconds: 1,
            world_lines: None,
            quiet_wait: 0,
            quiet_commit_gb: 0,
            quiet_ram_gb: 0,
            data_dir: None,
            data_from: None,
            accept_terms: false,
            keys: None,
            exe_victory: None,
            log: Some(root.join("README.md")),
            envs: vec![],
            baseline: None,
        };
        let error = run(&root, &args).unwrap_err();
        assert!(error.is::<victory_core::PolicyError>(), "{error:#}");
        assert!(!args.out.exists());
    }

    /// A world line `at` seconds in.
    fn world_line(at: u32) -> String {
        format!(
            "2026-01-01T00:{:02}:{:02}Z  INFO victory_gpu::host: 1.0 fps, 1000.0 ms/frame (1.0 \
             preparing: copies 0.0 ms 0.0 MB, textures 0.0 ms 0.0 MB; 2.0 on the GPU; 0.1 copying \
             back 0.0 MB), 900 draws/frame, dropped 0 without a host pipeline (0 being made) and \
             0 indirect\n",
            at / 60,
            at % 60
        )
    }

    #[test]
    fn a_growing_log_is_enough_one_look_after_its_last_line_asked_for() {
        let path = std::env::temp_dir().join(format!("xtask-watch-{}", std::process::id()));
        let mut watch = Watch::new(&path, 2);
        assert!(!watch.enough(), "no log yet");
        let append = |text: &str| {
            use std::io::Write as _;
            let mut file =
                std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap();
            file.write_all(text.as_bytes()).unwrap();
        };
        std::fs::write(&path, world_line(0)).unwrap();
        append(&world_line(50));
        append(&world_line(95)); // The first steady one.
        assert!(!watch.enough());
        assert!(!watch.enough(), "one steady line of two");
        // Half a line counts for nothing.
        let second = world_line(100);
        let (head, tail) = second.split_at(60);
        append(head);
        assert!(!watch.enough());
        assert!(!watch.enough());
        append(tail);
        assert!(!watch.enough(), "the look that sees it: what follows it may not be there");
        assert!(watch.enough());
        assert_eq!(watch.log.read().steady_lines(), 2);
        std::fs::remove_file(&path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_run_with_its_lines_is_ended_before_its_time() {
        let mut command = Command::new("cmd");
        command.args(["/d", "/c", "ping -n 60 127.0.0.1"]).stdout(Stdio::null());
        let start = Instant::now();
        let mut looks = 0;
        let mut times = Vec::new();
        let enough = |cpu: Option<u64>| {
            looks += 1;
            times.push(cpu.unwrap());
            looks == 2
        };
        let (end, _) = run_for(command, Duration::from_secs(60), enough).unwrap();
        // The job's processor time only grows.
        assert!(times[1] >= times[0]);
        assert_eq!((end, looks), (End::Enough, 2));
        assert!(start.elapsed() >= LOOK * 2 && start.elapsed() < Duration::from_secs(30));
    }

    #[cfg(windows)]
    #[test]
    fn what_outlives_its_time_is_ended_with_its_children() {
        let shell = |script: &str| {
            let mut c = Command::new("cmd");
            c.args(["/d", "/c", script]).stdout(Stdio::null());
            c
        };
        let exited = run_for(shell("exit 3"), Duration::from_secs(60), |_| false).unwrap();
        assert_eq!(exited.0, End::Exited(Some(3)));

        // The grandchild holds the pipe: reading ends only when it is gone.
        let (mut read, write) = std::io::pipe().unwrap();
        let mut command = shell("ping -n 60 127.0.0.1");
        command.stdout(write);
        let start = Instant::now();
        assert_eq!(run_for(command, Duration::from_secs(1), |_| false).unwrap().0, End::TimedOut);
        std::io::Read::read_to_end(&mut read, &mut Vec::new()).unwrap();
        assert!(start.elapsed() < Duration::from_secs(30));
    }
}
