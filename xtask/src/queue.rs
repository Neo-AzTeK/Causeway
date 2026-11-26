//! `cargo xtask queue`: smoke runs requested by many sessions, run one at a
//! time by one runner.
//!
//! Only one real-GPU run may run on a machine, so sessions do not launch it:
//! each adds a request (a small TOML file in a folder outside the repository),
//! and the runner, started once, takes the oldest and runs exactly
//! `cargo xtask smoke` for it in the requester's worktree. Nothing else is
//! ever run: a request is parsed into a fixed set of fields, and a field it
//! does not know, or a switch that is not `VICTORY_*`, refuses it. A request
//! ends with `<name>.done` beside it (exit code, digest path), refused ones
//! too, so a requester only ever waits for that file.

use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};

use crate::smoke;

const DEFAULT_QUEUE: &str = r"E:\Causeway-tools\queue";
const DEFAULT_RUNS: &str = r"E:\Causeway-tools\runs";
const LOCK: &str = "runner.lock";
/// How often the queue, the smoke lock and a run are looked at.
const POLL: Duration = Duration::from_secs(2);
/// A request whose run stops at the smoke lock this many times (five minutes)
/// is given up on.
const MAX_BUSY: u32 = 150;

#[derive(Args)]
pub struct Queue {
    #[command(subcommand)]
    cmd: Cmd,
    /// Where requests wait (outside the repository). Default: `VICTORY_QUEUE_DIR`,
    /// else E:\Causeway-tools\queue.
    #[arg(long, global = true)]
    queue_dir: Option<PathBuf>,
    /// Where runs go, one folder per request name. Default: `VICTORY_RUNS_DIR`,
    /// else E:\Causeway-tools\runs.
    #[arg(long, global = true)]
    runs_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ask for a smoke run of a commit of this worktree (or `--worktree`).
    Add(Box<Add>),
    /// Run the requests one at a time, oldest first. Start it once, in the
    /// background: it only runs `cargo xtask smoke` for valid requests.
    Run(RunArgs),
    /// Requests waiting, running and done lately.
    Status,
}

#[derive(Args)]
struct Add {
    /// The run's name: its folder in the runs folder, and `<name>.done` here.
    #[arg(long)]
    name: String,
    /// A worktree of this repository; this one by default.
    #[arg(long)]
    worktree: Option<PathBuf>,
    /// Commit the worktree must be at (7 to 40 hex digits); its HEAD by default.
    #[arg(long)]
    commit: Option<String>,
    #[arg(long, default_value_t = 300)]
    seconds: u64,
    /// `smoke --world-lines`: end the run once it has these steady world
    /// lines; --seconds stays the cap.
    #[arg(long)]
    world_lines: Option<u64>,
    /// `NAME=VALUE`: a `VICTORY_*` switch for the game.
    #[arg(long = "env", value_name = "NAME=VALUE")]
    envs: Vec<String>,
    /// An earlier run, by name in the runs folder (or an absolute report.json).
    #[arg(long)]
    baseline: Option<String>,
    /// The session to tell (`local_...`), for whoever reads the queue.
    #[arg(long)]
    requester: Option<String>,
    /// `smoke --data-dir`.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// `smoke --data-from`.
    #[arg(long)]
    data_from: Option<PathBuf>,
    /// `smoke --keys`.
    #[arg(long)]
    keys: Option<String>,
    /// Wait for `<name>.done`; exit with an error unless the run exited 0.
    #[arg(long)]
    wait: bool,
}

#[derive(Args)]
struct RunArgs {
    /// The title executable (`smoke --exe`).
    #[arg(long)]
    exe: PathBuf,
    /// Run what is queued and exit, instead of waiting for more.
    #[arg(long)]
    once: bool,
}

pub fn run(root: &Path, args: &Queue) -> Result<()> {
    let dirs = Dirs {
        queue: args.queue_dir.clone().unwrap_or_else(|| default_dir("VICTORY_QUEUE_DIR", DEFAULT_QUEUE)),
        runs: args.runs_dir.clone().unwrap_or_else(|| default_dir("VICTORY_RUNS_DIR", DEFAULT_RUNS)),
    };
    victory_core::ensure_outside_repo(&dirs.queue)?;
    victory_core::ensure_outside_repo(&dirs.runs)?;
    match &args.cmd {
        Cmd::Add(add) => add_request(root, &dirs, add),
        Cmd::Run(run_args) => runner(root, &dirs, run_args),
        Cmd::Status => status(&dirs),
    }
}

fn default_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map_or_else(|| PathBuf::from(fallback), PathBuf::from)
}

struct Dirs {
    queue: PathBuf,
    runs: PathBuf,
}

impl Dirs {
    fn file(&self, name: &str, ext: &str) -> PathBuf {
        self.queue.join(format!("{name}.{ext}"))
    }
}

/// One request, as a file holds it.
#[derive(Debug, PartialEq)]
struct Request {
    name: String,
    worktree: PathBuf,
    commit: String,
    seconds: u64,
    world_lines: Option<u64>,
    env: Vec<(String, String)>,
    baseline: Option<String>,
    requester: Option<String>,
    data_dir: Option<PathBuf>,
    data_from: Option<PathBuf>,
    keys: Option<String>,
}

const FIELDS: &[&str] = &[
    "name", "worktree", "commit", "seconds", "world_lines", "env", "baseline", "requester", "data_dir",
    "data_from", "keys",
];

/// Letters, digits and `._-`, not starting with a dot: a file name and a
/// folder name that cannot leave their folder.
fn plain_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && !text.starts_with('.')
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn no_control(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

/// `NAME=VALUE` of a `VICTORY_*` switch, nothing else.
fn parse_switch(text: &str) -> Result<(String, String)> {
    let (name, value) = text.split_once('=').with_context(|| format!("{text:?}: expected NAME=VALUE"))?;
    let name = name.trim();
    ensure!(
        name.starts_with("VICTORY_")
            && name.len() > "VICTORY_".len()
            && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
        "{name}: only VICTORY_* switches pass through"
    );
    ensure!(no_control(value), "{name}: the value has a control character");
    Ok((name.to_owned(), value.to_owned()))
}

impl Request {
    /// A request from its file's text; refuses what is not exactly the
    /// fields above, in the forms above. Nothing here looks at the disk.
    fn parse(text: &str) -> Result<Request> {
        let table: toml::Table = text.parse().context("not a TOML file")?;
        if let Some(unknown) = table.keys().find(|k| !FIELDS.contains(&k.as_str())) {
            bail!("unknown field {unknown:?}: a request holds only {}", FIELDS.join(", "));
        }
        let string = |key: &str| -> Result<Option<String>> {
            match table.get(key) {
                None => Ok(None),
                Some(toml::Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => bail!("{key} must be a string"),
            }
        };
        let need = |key: &str| string(key)?.with_context(|| format!("{key} is missing"));
        let name = need("name")?;
        ensure!(plain_name(&name), "name {name:?}: letters, digits and ._- only, at most 64");
        let commit = need("commit")?.to_ascii_lowercase();
        ensure!(
            (7..=40).contains(&commit.len()) && commit.bytes().all(|b| b.is_ascii_hexdigit()),
            "commit {commit:?}: 7 to 40 hex digits"
        );
        let seconds = match table.get("seconds") {
            None => 300,
            Some(toml::Value::Integer(n)) if (1..=86400).contains(n) => *n as u64,
            Some(_) => bail!("seconds must be a whole number from 1 to 86400"),
        };
        let world_lines = match table.get("world_lines") {
            None => None,
            Some(toml::Value::Integer(n)) if (1..=100_000).contains(n) => Some(*n as u64),
            Some(_) => bail!("world_lines must be a whole number from 1 to 100000"),
        };
        let env = match table.get("env") {
            None => Vec::new(),
            Some(toml::Value::Array(list)) => list
                .iter()
                .map(|v| match v {
                    toml::Value::String(s) => parse_switch(s),
                    _ => bail!("env holds strings, \"NAME=VALUE\""),
                })
                .collect::<Result<_>>()?,
            Some(_) => bail!("env must be a list of \"NAME=VALUE\""),
        };
        let path = |key: &str| -> Result<Option<PathBuf>> {
            let Some(p) = string(key)? else { return Ok(None) };
            ensure!(no_control(&p) && Path::new(&p).is_absolute(), "{key} must be an absolute path");
            Ok(Some(p.into()))
        };
        let baseline = string("baseline")?;
        if let Some(b) = &baseline {
            ensure!(
                plain_name(b) || (Path::new(b).is_absolute() && b.ends_with("report.json") && no_control(b)),
                "baseline {b:?}: a run's name, or an absolute path to its report.json"
            );
        }
        let requester = string("requester")?;
        if let Some(r) = &requester {
            ensure!(plain_name(r), "requester {r:?}: a session id");
        }
        let keys = string("keys")?;
        if let Some(k) = &keys {
            ensure!(no_control(k) && k.len() <= 4000, "keys: too long, or a control character");
        }
        Ok(Request {
            name,
            worktree: path("worktree")?.context("worktree is missing")?,
            commit,
            seconds,
            world_lines,
            env,
            baseline,
            requester,
            data_dir: path("data_dir")?,
            data_from: path("data_from")?,
            keys,
        })
    }

    fn to_toml(&self) -> String {
        use toml::Value::{Array, Integer, String as Str};
        let mut table = toml::Table::new();
        let mut put = |key: &str, value| {
            table.insert(key.to_owned(), value);
        };
        put("name", Str(self.name.clone()));
        put("worktree", Str(self.worktree.display().to_string()));
        put("commit", Str(self.commit.clone()));
        put("seconds", Integer(self.seconds as i64));
        if let Some(n) = self.world_lines {
            put("world_lines", Integer(n as i64));
        }
        let env = self.env.iter().map(|(n, v)| Str(format!("{n}={v}"))).collect();
        put("env", Array(env));
        for (key, value) in [
            ("baseline", self.baseline.clone()),
            ("requester", self.requester.clone()),
            ("data_dir", self.data_dir.as_ref().map(|p| p.display().to_string())),
            ("data_from", self.data_from.as_ref().map(|p| p.display().to_string())),
            ("keys", self.keys.clone()),
        ] {
            if let Some(v) = value {
                put(key, Str(v));
            }
        }
        table.to_string()
    }

    /// The arguments after `cargo` (always `xtask smoke ...`): the title's
    /// executable and the run's folder are the runner's, not the request's.
    fn smoke_args(&self, dirs: &Dirs, exe: &Path) -> Vec<OsString> {
        let mut args: Vec<OsString> = ["xtask", "smoke", "--exe"].map(Into::into).into();
        args.push(exe.into());
        args.extend(["--out".into(), dirs.runs.join(&self.name).into()]);
        args.extend(["--seconds".into(), self.seconds.to_string().into()]);
        if let Some(n) = self.world_lines {
            args.extend(["--world-lines".into(), n.to_string().into()]);
        }
        for (flag, path) in [("--data-dir", &self.data_dir), ("--data-from", &self.data_from)] {
            if let Some(p) = path {
                args.extend([flag.into(), p.into()]);
            }
        }
        if let Some(k) = &self.keys {
            args.extend(["--keys".into(), k.into()]);
        }
        if let Some(b) = &self.baseline {
            let json = if plain_name(b) { dirs.runs.join(b).join("report.json") } else { b.into() };
            args.extend(["--baseline".into(), json.into()]);
        }
        for (name, value) in &self.env {
            args.extend(["--env".into(), format!("{name}={value}").into()]);
        }
        args
    }

    /// What the disk must show: the worktree is one of this repository's, at
    /// the commit, with nothing tracked changed (the run would not be of the
    /// commit), and the baseline and the run's folder are as they should be.
    fn check(&self, root: &Path, dirs: &Dirs) -> Result<()> {
        let listed = git(root, &["worktree", "list", "--porcelain"])?;
        let known = listed
            .lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .any(|p| same_path(Path::new(p), &self.worktree));
        ensure!(known, "{} is not a worktree of this repository", self.worktree.display());
        ensure!(
            self.worktree.join("xtask").join("Cargo.toml").is_file(),
            "{} has no xtask",
            self.worktree.display()
        );
        ensure!(
            self.world_lines.is_none() || knows_world_lines(&self.worktree),
            "{}: its xtask predates --world-lines (the runner runs the worktree's own): merge main \
             there",
            self.worktree.display()
        );
        let head = git(&self.worktree, &["rev-parse", "HEAD"])?;
        ensure!(
            head.starts_with(&self.commit),
            "the worktree is at {}, the request names {}",
            &head[..head.len().min(10)],
            self.commit
        );
        let dirty = git(&self.worktree, &["status", "--porcelain", "--untracked-files=no"])?;
        ensure!(dirty.is_empty(), "the worktree has uncommitted changes: commit them first");
        if let Some(b) = self.baseline.as_deref().filter(|b| plain_name(b)) {
            let json = dirs.runs.join(b).join("report.json");
            ensure!(json.is_file(), "baseline {b}: no {}", json.display());
        }
        ensure!(!dirs.runs.join(&self.name).exists(), "{} exists already", dirs.runs.join(&self.name).display());
        Ok(())
    }
}

/// Whether a worktree's `smoke` takes `--world-lines`.
fn knows_world_lines(worktree: &Path) -> bool {
    let smoke = worktree.join("xtask").join("src").join("smoke.rs");
    fs::read_to_string(smoke).is_ok_and(|text| text.contains("world_lines: Option<u64>"))
}

/// The same place, whatever the slashes, case and `\\?\` prefix.
fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let text = p.to_string_lossy().replace('\\', "/").to_lowercase();
        text.trim_start_matches("//?/").trim_end_matches('/').to_owned()
    };
    norm(a) == norm(b)
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("running git in {}", dir.display()))?;
    ensure!(out.status.success(), "git {} failed in {}", args.join(" "), dir.display());
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn add_request(root: &Path, dirs: &Dirs, add: &Add) -> Result<()> {
    let worktree = add.worktree.clone().unwrap_or_else(|| root.to_owned());
    let commit = match &add.commit {
        Some(c) => c.clone(),
        None => git(&worktree, &["rev-parse", "HEAD"])?,
    };
    // What is queued is what the file parses back to, so what is checked is what runs.
    let request = Request {
        name: add.name.clone(),
        worktree,
        commit,
        seconds: add.seconds,
        world_lines: add.world_lines,
        env: add.envs.iter().map(|e| parse_switch(e)).collect::<Result<_>>()?,
        baseline: add.baseline.clone(),
        requester: add.requester.clone(),
        data_dir: add.data_dir.clone(),
        data_from: add.data_from.clone(),
        keys: add.keys.clone(),
    };
    let text = request.to_toml();
    let request = Request::parse(&text)?;
    request.check(root, dirs)?;
    fs::create_dir_all(&dirs.queue).with_context(|| format!("creating {}", dirs.queue.display()))?;
    publish(dirs, &request.name, &text)?;
    let done = dirs.file(&request.name, "done");
    println!("queued {} ({}); wait for {}", request.name, length(&request), done.display());
    if add.wait {
        while !done.is_file() {
            std::thread::sleep(POLL);
        }
        let text = fs::read_to_string(&done)?;
        print!("{text}");
        let table: toml::Table = text.parse()?;
        let code = table.get("exit").and_then(toml::Value::as_integer);
        ensure!(code == Some(0), "the run did not exit 0 (exit = {code:?})");
    }
    Ok(())
}

/// Puts a request in the queue under a name nobody has. Of two adds of one
/// name only one can: the `.tmp` is the claim (`create_new`), held until the
/// whole `.toml` is in place, so the runner never reads half a file.
fn publish(dirs: &Dirs, name: &str, text: &str) -> Result<()> {
    let tmp = dirs.file(name, "tmp");
    let mut claim = fs::OpenOptions::new().write(true).create_new(true).open(&tmp).with_context(|| {
        format!(
            "{}: pick another name (another add has it; delete it if that one died)",
            tmp.display()
        )
    })?;
    let taken = ["toml", "done", "running"].map(|ext| dirs.file(name, ext));
    if let Some(path) = taken.iter().find(|p| p.exists()) {
        drop(claim);
        fs::remove_file(&tmp)?;
        bail!("{} exists: pick another name", path.display());
    }
    claim.write_all(text.as_bytes())?;
    drop(claim);
    fs::rename(&tmp, dirs.file(name, "toml"))?;
    Ok(())
}

fn write_done(dirs: &Dirs, name: &str, exit: i64, digest: Option<&Path>, error: Option<&str>) -> Result<()> {
    let mut table = toml::Table::new();
    table.insert("name".into(), name.to_owned().into());
    if let Some(who) = requester_of(dirs, name) {
        table.insert("requester".into(), who.into());
    }
    table.insert("exit".into(), exit.into());
    table.insert("finished_unix".into(), (unix_now() as i64).into());
    table.insert("out".into(), dirs.runs.join(name).display().to_string().into());
    if let Some(d) = digest {
        table.insert("digest".into(), d.display().to_string().into());
    }
    table.insert("console".into(), dirs.runs.join(format!("{name}.console.txt")).display().to_string().into());
    if let Some(e) = error {
        table.insert("error".into(), e.to_owned().into());
    }
    // Whole or absent: a requester polls for this file.
    let tmp = dirs.file(name, "done.tmp");
    fs::write(&tmp, table.to_string())?;
    fs::rename(&tmp, dirs.file(name, "done"))?;
    let _ = fs::remove_file(dirs.file(name, "running"));
    Ok(())
}

/// Names of requests with no `.done` and not running, oldest first.
fn waiting(dirs: &Dirs) -> Vec<String> {
    let Ok(entries) = fs::read_dir(&dirs.queue) else { return Vec::new() };
    let mut found: Vec<(SystemTime, String)> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let path = e.path();
            if path.extension()? != "toml" {
                return None;
            }
            let name = path.file_stem()?.to_str()?.to_owned();
            let taken = dirs.file(&name, "done").exists() || dirs.file(&name, "running").exists();
            if taken { return None; }
            Some((e.metadata().ok()?.modified().ok()?, name))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, n)| n).collect()
}

fn runner(root: &Path, dirs: &Dirs, args: &RunArgs) -> Result<()> {
    ensure!(args.exe.is_file(), "{} is not a file", args.exe.display());
    fs::create_dir_all(&dirs.queue)?;
    fs::create_dir_all(&dirs.runs)?;
    // Held until this process ends.
    let _only = runner_lock(&dirs.queue.join(LOCK))?;
    recover(dirs)?;
    println!("runner: queue {}, runs {}", dirs.queue.display(), dirs.runs.display());
    serve(root, dirs, args)
}

/// The lock one runner at a time holds, with its process id. Others may only
/// read it, and the system frees it when the process is gone: a file a dead
/// runner left blocks nobody, and a live runner cannot be replaced.
fn runner_lock(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    // invariant(one_queue_runner_at_a_time): only reading is shared
    // (`FILE_SHARE_READ`), so a second writer fails while the first lives.
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 1);
    let mut file = options.open(path).with_context(|| {
        format!(
            "another runner is running (two would run two real-GPU runs at once): {} is not free",
            path.display()
        )
    })?;
    file.set_len(0)?;
    writeln!(file, "pid {}\nstarted_unix {}", std::process::id(), unix_now())?;
    Ok(file)
}

/// What `status` says of the runner: its first lock line while one holds it.
fn runner_state(lock: &Path) -> String {
    let Ok(text) = fs::read_to_string(lock) else { return "not running".to_owned() };
    // Opening it for writing fails while a runner holds it.
    if fs::OpenOptions::new().write(true).open(lock).is_ok() {
        return "not running".to_owned();
    }
    format!("running, {}", text.lines().next().unwrap_or("?"))
}

/// Ends the runs a dead runner left `.running`: they will not finish. Only
/// the holder of the runner lock may call it.
fn recover(dirs: &Dirs) -> Result<()> {
    let entries = fs::read_dir(&dirs.queue)
        .with_context(|| format!("reading {}", dirs.queue.display()))?;
    for path in entries.filter_map(|e| Some(e.ok()?.path())) {
        if path.extension().is_some_and(|e| e == "running")
            && let Some(name) = path.file_stem().and_then(|n| n.to_str())
        {
            if dirs.file(name, "done").exists() {
                fs::remove_file(&path)?;
            } else {
                write_done(dirs, name, -1, None, Some("the runner stopped while it ran"))?;
            }
        }
    }
    Ok(())
}

fn serve(root: &Path, dirs: &Dirs, args: &RunArgs) -> Result<()> {
    let mut busy: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut said = false;
    let mut builder = Builder::default();
    loop {
        let Some(name) = waiting(dirs).into_iter().next() else {
            if args.once {
                builder.finish();
                return Ok(());
            }
            std::thread::sleep(POLL);
            continue;
        };
        // A run started by hand, or one that has not let go, holds the smoke
        // lock: the head of the queue waits, it is not lost.
        if !smoke::lock_free() {
            if !said {
                println!("runner: the smoke lock is held; {name} waits at the head of the queue");
                said = true;
            }
            std::thread::sleep(POLL);
            continue;
        }
        said = false;
        // No build may go on into the next run.
        builder.finish();
        fs::write(dirs.file(&name, "running"), format!("started_unix {}\n", unix_now()))?;
        let outcome = fs::read_to_string(dirs.file(&name, "toml"))
            .context("reading the request")
            .and_then(|text| Request::parse(&text))
            .and_then(|request| {
                ensure!(request.name == name, "the request names {}, its file {name}", request.name);
                request.check(root, dirs)?;
                println!("runner: start {name} ({})", length(&request));
                launch(dirs, &request, &args.exe, || builder.tick(root, dirs, &request.worktree))
            });
        match outcome {
            Ok(Ran::Exit(code)) => {
                let digest = dirs.runs.join(&name).join("digest.md");
                write_done(dirs, &name, code.into(), digest.is_file().then_some(digest.as_path()), None)?;
                println!("runner: done {name} exit={code}");
            }
            Ok(Ran::LockBusy) => {
                // The smoke run did not start: back to the head, its request
                // untouched (it is still the oldest).
                let tries = busy.entry(name.clone()).or_default();
                *tries += 1;
                if *tries >= MAX_BUSY {
                    let why = format!("the smoke lock was busy {MAX_BUSY} times in a row");
                    write_done(dirs, &name, -1, None, Some(&why))?;
                    println!("runner: gave up on {name}: {why}");
                } else {
                    let _ = fs::remove_file(dirs.file(&name, "running"));
                    println!("runner: {name} did not start (smoke lock busy); it stays at the head");
                    std::thread::sleep(POLL);
                }
            }
            Err(e) => {
                write_done(dirs, &name, -1, None, Some(&format!("{e:#}")))?;
                println!("runner: refused {name}: {e:#}");
            }
        }
    }
}

/// How a launch ended.
enum Ran {
    Exit(i32),
    /// `cargo xtask smoke` stopped at the smoke lock, before making anything.
    LockBusy,
}

/// Whether a smoke run failed only because another holds the lock.
fn stopped_at_the_lock(code: i32, console: &str, out_exists: bool) -> bool {
    code != 0 && !out_exists && console.contains(smoke::LOCK_BUSY)
}

/// How long a request runs, for people.
fn length(request: &Request) -> String {
    match request.world_lines {
        Some(n) => format!("{n} world lines, {} s at most", request.seconds),
        None => format!("{} s", request.seconds),
    }
}

/// Builds what the next run will need while the current one plays, so that no
/// build sits between two runs. The run being played measures CPU time, so
/// this runs below normal priority, on half the cores.
// shortcut: it still shares cores, memory and disk with the title; a cold
// build can reach into the steady lines. An incremental one ends during boot.
#[derive(Default)]
struct Builder {
    running: Option<Build>,
    /// Requests built for already, whether or not the build worked.
    tried: HashSet<String>,
}

/// The build under way and the steps after it.
struct Build {
    name: String,
    log: PathBuf,
    child: Child,
    after: VecDeque<Command>,
}

impl Builder {
    /// Moves the build on, or starts the one the next run needs.
    fn tick(&mut self, root: &Path, dirs: &Dirs, playing: &Path) {
        self.advance();
        if self.running.is_some() {
            return;
        }
        let Some(request) = next_build(dirs, playing, &self.tried) else { return };
        self.tried.insert(request.name.clone());
        // A request that will be refused is not built for; its turn says why.
        if request.check(root, dirs).is_err() {
            return;
        }
        let log = dirs.runs.join(format!("{}.build.txt", request.name));
        // The runner of that worktree first (`cargo xtask`), then the title's build.
        let steps = [&["build", "--locked", "-p", "xtask"][..], smoke::DIAG_BUILD];
        let steps = steps.map(|step| build_command(&request.worktree, step)).into();
        println!("runner: building ahead for {}", request.name);
        self.start(request.name, log, steps);
    }

    /// Starts the first of `steps`, its output added to `log`. A step that
    /// cannot start ends the build: the run builds for itself.
    fn start(&mut self, name: String, log: PathBuf, mut steps: VecDeque<Command>) {
        let Some(mut command) = steps.pop_front() else { return };
        let spawned = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .and_then(|out| {
                command.stdin(Stdio::null()).stdout(out.try_clone()?).stderr(out);
                command.spawn()
            });
        match spawned {
            Ok(child) => self.running = Some(Build { name, log, child, after: steps }),
            Err(e) => println!("runner: the build for {name} did not start: {e}"),
        }
    }

    /// Takes a step that has ended: the next one, or the build is over.
    fn advance(&mut self) {
        let Some(build) = &mut self.running else { return };
        let status = match build.child.try_wait() {
            Ok(None) => return,
            Ok(Some(status)) => status.success(),
            Err(_) => false,
        };
        let Some(Build { name, log, after, .. }) = self.running.take() else { return };
        if !status {
            println!("runner: the build ahead for {name} failed: {}", log.display());
        } else if after.is_empty() {
            println!("runner: built ahead for {name}");
        } else {
            self.start(name, log, after);
        }
    }

    /// Waits for what is being built.
    fn finish(&mut self) {
        if let Some(build) = &self.running {
            println!("runner: waiting for the build for {} to end", build.name);
        }
        while let Some(build) = &mut self.running {
            let _ = build.child.wait();
            self.advance();
        }
    }
}

/// The request to build for: the next to run, once. Not one in the worktree
/// of the run going on, where cargo is that run's.
fn next_build(dirs: &Dirs, playing: &Path, tried: &HashSet<String>) -> Option<Request> {
    let name = waiting(dirs).into_iter().next()?;
    if tried.contains(&name) {
        return None;
    }
    let text = fs::read_to_string(dirs.file(&name, "toml")).ok()?;
    let request = Request::parse(&text).ok().filter(|r| r.name == name)?;
    (!same_path(&request.worktree, playing)).then_some(request)
}

/// A build step in a worktree that leaves the processor to the run playing.
fn build_command(worktree: &Path, step: &[&str]) -> Command {
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    let mut command = Command::new("cargo");
    command.args(step).args(["-j", &(cores / 2).max(1).to_string()]).current_dir(worktree);
    // What it starts (rustc, the linker) takes the same class.
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(
        &mut command,
        windows_sys::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS,
    );
    command
}

/// Runs `cargo xtask smoke` for a checked request; its console goes to
/// `<runs>/<name>.console.txt`. `meanwhile` is called every `POLL` it runs.
fn launch(
    dirs: &Dirs,
    request: &Request,
    exe: &Path,
    mut meanwhile: impl FnMut(),
) -> Result<Ran> {
    let console = dirs.runs.join(format!("{}.console.txt", request.name));
    let out = fs::File::create(&console).with_context(|| format!("creating {}", console.display()))?;
    let err = out.try_clone()?;
    let mut child = Command::new("cargo")
        .args(request.smoke_args(dirs, exe))
        .current_dir(&request.worktree)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .context("starting cargo")?;
    let status = loop {
        if let Some(status) = child.try_wait().context("waiting for cargo")? {
            break status;
        }
        meanwhile();
        std::thread::sleep(POLL);
    };
    let code = status.code().unwrap_or(-1);
    let text = fs::read(&console).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    if stopped_at_the_lock(code, &text, dirs.runs.join(&request.name).exists()) {
        return Ok(Ran::LockBusy);
    }
    Ok(Ran::Exit(code))
}

/// The session a request names, if its file reads.
fn requester_of(dirs: &Dirs, name: &str) -> Option<String> {
    let text = fs::read_to_string(dirs.file(name, "toml")).ok()?;
    Request::parse(&text).ok()?.requester
}

fn status(dirs: &Dirs) -> Result<()> {
    println!("runner: {}", runner_state(&dirs.queue.join(LOCK)));
    let Ok(entries) = fs::read_dir(&dirs.queue) else {
        println!("no queue folder yet ({})", dirs.queue.display());
        return Ok(());
    };
    let mut names: Vec<(SystemTime, String)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
        .filter_map(|e| {
            let name = e.path().file_stem()?.to_str()?.to_owned();
            Some((e.metadata().and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH), name))
        })
        .collect();
    names.sort();
    let (mut running, mut queued, mut finished) = (Vec::new(), Vec::new(), Vec::new());
    for (_, name) in names {
        let parsed = fs::read_to_string(dirs.file(&name, "toml")).ok().and_then(|t| Request::parse(&t).ok());
        let what = parsed.as_ref().map_or("unreadable".to_owned(), |r| {
            let who = r.requester.as_deref().map_or(String::new(), |q| format!(", {q}"));
            format!("{}, {}{who}", length(r), &r.commit[..r.commit.len().min(8)])
        });
        if let Ok(done) = fs::read_to_string(dirs.file(&name, "done")) {
            let table: toml::Table = done.parse().unwrap_or_default();
            let code = table.get("exit").and_then(toml::Value::as_integer).unwrap_or(-1);
            let state = match table.get("error").and_then(toml::Value::as_str) {
                Some(e) => format!("refused: {}", e.chars().take(80).collect::<String>()),
                None => format!("done, exit {code}"),
            };
            finished.push(format!("{name:<26} {state}; {what}"));
        } else if let Ok(started) = fs::read_to_string(dirs.file(&name, "running")) {
            let at = started.split_whitespace().nth(1).and_then(|s| s.parse::<u64>().ok());
            let for_s = at.map_or(0, |at| unix_now().saturating_sub(at));
            running.push(format!("{name:<26} RUNNING for {for_s} s; {what}"));
        } else {
            queued.push(format!("{name:<26} {what}"));
        }
    }
    for line in &running {
        println!("{line}");
    }
    for (i, line) in queued.iter().enumerate() {
        println!("#{} {line}", i + 1);
    }
    for line in finished.iter().rev().take(10) {
        println!("{line}");
    }
    println!(
        "{} running, {} queued (in the order they run), {} finished",
        running.len(),
        queued.len(),
        finished.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
name = "mesh-emu-8"
worktree = 'E:\Causeway\.claude\worktrees\mesh'
commit = "fb5003eea8"
seconds = 420
env = ["VICTORY_SHADER_LOOP_LIMIT=64", "VICTORY_D3D12_DRED=1"]
baseline = "smoke-16"
requester = "local_d9f48aa8-e1b5-46aa-aeb1-92d20980a9a3"
"#;

    fn refused(text: &str) -> String {
        format!("{:#}", Request::parse(text).unwrap_err())
    }

    fn with(line: &str) -> String {
        format!("{GOOD}{line}\n")
    }

    #[test]
    fn a_valid_request_is_read_and_written_back() {
        let r = Request::parse(GOOD).unwrap();
        assert_eq!(r.name, "mesh-emu-8");
        assert_eq!(r.seconds, 420);
        assert_eq!(r.env[0], ("VICTORY_SHADER_LOOP_LIMIT".to_owned(), "64".to_owned()));
        assert_eq!(Request::parse(&r.to_toml()).unwrap(), r);
    }

    #[test]
    fn only_victory_switches_pass() {
        for bad in ["PATH=/x", "victory_log=info", "VICTORY_=1", "VICTORY_A B=1", "RUSTFLAGS=-C", "NOEQUALS"] {
            let text = GOOD.replace("VICTORY_D3D12_DRED=1", bad);
            assert!(Request::parse(&text).is_err(), "{bad} passed");
        }
        assert!(refused(&GOOD.replace("VICTORY_D3D12_DRED=1", "PATH=/x")).contains("only VICTORY_"));
        assert!(Request::parse(&GOOD.replace("DRED=1", "DRED=1\\nx")).is_err());
    }

    #[test]
    fn unknown_fields_and_wrong_types_are_refused() {
        assert!(refused(&with("command = \"calc\"")).contains("unknown field"));
        assert!(refused(&with("exe = 'C:\\x.exe'")).contains("unknown field"));
        assert!(refused(&GOOD.replace("seconds = 420", "seconds = \"420\"")).contains("seconds"));
        assert!(refused(&GOOD.replace("seconds = 420", "seconds = 0")).contains("seconds"));
        assert!(refused(&GOOD.replace("seconds = 420", "seconds = 90000")).contains("seconds"));
        assert!(refused(&GOOD.replace("env = [", "env = [1, ")).contains("env"));
        assert!(refused("not toml [").contains("TOML"));
    }

    #[test]
    fn names_commits_and_paths_are_confined() {
        for bad in ["../x", "a/b", "a\\b", ".hidden", "", "a b", "x:y"] {
            assert!(Request::parse(&GOOD.replace("mesh-emu-8", bad)).is_err(), "name {bad:?}");
        }
        assert!(Request::parse(&GOOD.replace("fb5003eea8", "fb5003")).is_err());
        assert!(Request::parse(&GOOD.replace("fb5003eea8", "main")).is_err());
        assert!(Request::parse(&GOOD.replace(r"E:\Causeway\.claude\worktrees\mesh", "mesh")).is_err());
        assert!(Request::parse(&GOOD.replace("\"smoke-16\"", "\"..\\\\x\"")).is_err());
        assert!(Request::parse(&GOOD.replace("\"smoke-16\"", "'E:\\runs\\smoke-16\\report.json'")).is_ok());
        assert!(Request::parse(&GOOD.replace("\"smoke-16\"", "'E:\\runs\\x.exe'")).is_err());
    }

    #[test]
    fn smoke_gets_only_known_flags() {
        let dirs = Dirs { queue: "Q".into(), runs: "R".into() };
        let mut r = Request::parse(GOOD).unwrap();
        r.keys = Some("8:Enter".into());
        r.data_dir = Some(PathBuf::from("D"));
        let args: Vec<String> =
            r.smoke_args(&dirs, Path::new("title.exe")).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(&args[..5], ["xtask", "smoke", "--exe", "title.exe", "--out"]);
        let flags: Vec<&str> = args.iter().filter(|a| a.starts_with("--")).map(String::as_str).collect();
        assert_eq!(
            flags,
            ["--exe", "--out", "--seconds", "--data-dir", "--keys", "--baseline", "--env", "--env"]
        );
        assert!(args.contains(&"VICTORY_SHADER_LOOP_LIMIT=64".to_owned()));
        let base = args.iter().position(|a| a == "--baseline").unwrap();
        assert!(args[base + 1].ends_with("report.json") && args[base + 1].contains("smoke-16"));
        // Never the terms switch.
        assert!(!args.iter().any(|a| a == "--accept-terms"));
    }

    #[test]
    fn commits_match_by_prefix_and_paths_by_place() {
        assert!(same_path(Path::new("E:/Causeway/x/"), Path::new(r"e:\causeway\X")));
        assert!(same_path(Path::new(r"\\?\E:\a"), Path::new("E:/a")));
        assert!(!same_path(Path::new("E:/a"), Path::new("E:/b")));
    }

    #[test]
    fn the_oldest_waiting_request_goes_first() {
        let dir = std::env::temp_dir().join(format!("xtask-queue-{}", std::process::id()));
        let dirs = Dirs { queue: dir.clone(), runs: dir.join("runs") };
        fs::create_dir_all(&dir).unwrap();
        for name in ["b", "a", "c"] {
            fs::write(dirs.file(name, "toml"), "x").unwrap();
            std::thread::sleep(Duration::from_millis(30));
        }
        fs::write(dirs.file("a", "done"), "x").unwrap();
        fs::write(dirs.file("c", "running"), "x").unwrap();
        assert_eq!(waiting(&dirs), ["b"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_a_start_stopped_at_the_lock_is_put_back() {
        let busy = format!("Error: is another smoke run on? C:/t/x.lock {}", smoke::LOCK_BUSY);
        assert!(stopped_at_the_lock(1, &busy, false));
        assert!(!stopped_at_the_lock(0, &busy, false));
        // A run that made its folder started, whatever it printed.
        assert!(!stopped_at_the_lock(1, &busy, true));
        assert!(!stopped_at_the_lock(1, "Error: 3 areas failed", false));
    }

    #[test]
    fn a_done_file_names_the_requester() {
        let dir = std::env::temp_dir().join(format!("xtask-done-{}", std::process::id()));
        let dirs = Dirs { queue: dir.clone(), runs: dir.join("runs") };
        fs::create_dir_all(&dir).unwrap();
        fs::write(dirs.file("mesh-emu-8", "toml"), GOOD).unwrap();
        write_done(&dirs, "mesh-emu-8", 0, None, None).unwrap();
        let done: toml::Table = fs::read_to_string(dirs.file("mesh-emu-8", "done")).unwrap().parse().unwrap();
        assert_eq!(done["requester"].as_str(), Some("local_d9f48aa8-e1b5-46aa-aeb1-92d20980a9a3"));
        assert_eq!(done["exit"].as_integer(), Some(0));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A queue folder of its own for one test.
    fn scratch(name: &str) -> Dirs {
        let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Dirs { queue: dir.clone(), runs: dir.join("runs") }
    }

    fn done(dirs: &Dirs, name: &str) -> toml::Table {
        fs::read_to_string(dirs.file(name, "done")).unwrap().parse().unwrap()
    }

    #[test]
    fn a_run_left_running_is_ended_by_the_next_runner() {
        let dirs = scratch("recover");
        fs::write(dirs.file("mesh-emu-8", "toml"), GOOD).unwrap();
        fs::write(dirs.file("mesh-emu-8", "running"), "started_unix 1\n").unwrap();
        // One that finished but kept its marker: its result stays.
        fs::write(dirs.file("old", "done"), "exit = 0\n").unwrap();
        fs::write(dirs.file("old", "running"), "started_unix 1\n").unwrap();
        recover(&dirs).unwrap();
        let ended = done(&dirs, "mesh-emu-8");
        assert_eq!(ended["exit"].as_integer(), Some(-1));
        assert!(ended["error"].as_str().unwrap().contains("runner stopped"));
        assert!(ended.contains_key("requester"));
        assert_eq!(done(&dirs, "old")["exit"].as_integer(), Some(0));
        for name in ["mesh-emu-8", "old"] {
            assert!(!dirs.file(name, "running").exists());
        }
        assert!(waiting(&dirs).is_empty());
        fs::remove_dir_all(&dirs.queue).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn one_queue_runner_at_a_time() {
        let dirs = scratch("lock");
        let lock = dirs.queue.join(LOCK);
        // A file a dead runner left blocks nobody.
        fs::write(&lock, "pid 1\nstarted_unix 1\nleft over\n").unwrap();
        assert_eq!(runner_state(&lock), "not running");
        let first = runner_lock(&lock).unwrap();
        assert!(runner_lock(&lock).is_err(), "a live runner was replaced");
        let pid = format!("pid {}", std::process::id());
        assert_eq!(runner_state(&lock), format!("running, {pid}"));
        assert!(!fs::read_to_string(&lock).unwrap().contains("left over"));
        drop(first);
        assert_eq!(runner_state(&lock), "not running");
        drop(runner_lock(&lock).unwrap());
        fs::remove_dir_all(&dirs.queue).unwrap();
    }

    #[test]
    fn world_lines_pass_through_to_smoke() {
        let r = Request::parse(&with("world_lines = 40")).unwrap();
        assert_eq!(r.world_lines, Some(40));
        assert_eq!(Request::parse(&r.to_toml()).unwrap(), r);
        let dirs = Dirs { queue: "Q".into(), runs: "R".into() };
        let args = r.smoke_args(&dirs, Path::new("title.exe"));
        let at = args.iter().position(|a| a == "--world-lines").unwrap();
        assert_eq!(args[at + 1], "40");
        // The cap stays.
        assert!(args.iter().any(|a| a == "--seconds"));
        assert_eq!(length(&r), "40 world lines, 420 s at most");
        // Not asked for: not passed.
        let plain = Request::parse(GOOD).unwrap().smoke_args(&dirs, Path::new("title.exe"));
        assert!(!plain.iter().any(|a| a == "--world-lines"));
        for bad in ["0", "-1", "\"40\"", "1.5", "100001"] {
            assert!(refused(&with(&format!("world_lines = {bad}"))).contains("world_lines"));
        }
    }

    #[test]
    fn a_worktree_without_the_flag_is_told_apart() {
        assert!(knows_world_lines(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()));
        assert!(!knows_world_lines(Path::new("no/such/worktree")));
    }

    #[test]
    fn the_next_run_is_built_for_once_and_not_where_a_run_plays() {
        let dirs = scratch("next-build");
        let playing = Path::new(r"E:\Causeway\.claude\worktrees\playing");
        assert!(next_build(&dirs, playing, &HashSet::new()).is_none());
        // The one playing is not waiting.
        for (name, at) in [("now", "playing"), ("next", "mesh"), ("later", "other")] {
            let text = GOOD.replace("mesh-emu-8", name).replace(r"worktrees\mesh", &format!(r"worktrees\{at}"));
            fs::write(dirs.file(name, "toml"), text).unwrap();
            std::thread::sleep(Duration::from_millis(30));
        }
        fs::write(dirs.file("now", "running"), "started_unix 1\n").unwrap();
        let mut tried = HashSet::new();
        assert_eq!(next_build(&dirs, playing, &tried).unwrap().name, "next");
        // Once; and never one further down the queue.
        tried.insert("next".to_owned());
        assert!(next_build(&dirs, playing, &tried).is_none());
        let same = Path::new("E:/Causeway/.claude/worktrees/mesh");
        assert!(next_build(&dirs, same, &HashSet::new()).is_none());
        fs::remove_dir_all(&dirs.queue).unwrap();
    }

    #[test]
    fn the_build_ahead_leaves_the_cores_to_the_run() {
        let command = build_command(Path::new("w"), smoke::DIAG_BUILD);
        let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(&args[..args.len() - 2], smoke::DIAG_BUILD);
        assert_eq!(args[args.len() - 2], "-j");
        let cores = std::thread::available_parallelism().unwrap().get();
        assert!(args.last().unwrap().parse::<usize>().unwrap() <= cores.div_ceil(2));
        assert_eq!(command.get_current_dir(), Some(Path::new("w")));
    }

    #[test]
    #[cfg(windows)]
    fn the_steps_of_a_build_run_in_turn_until_one_fails() {
        let dirs = scratch("builder");
        let step = |script: &str| {
            let mut command = Command::new("cmd");
            command.args(["/d", "/c", script]);
            command
        };
        let log = dirs.queue.join("x.build.txt");
        let mut builder = Builder::default();
        let steps = [step("echo one"), step("echo two & exit 1"), step("echo three")];
        builder.start("x".into(), log.clone(), steps.into_iter().collect());
        builder.finish();
        assert!(builder.running.is_none());
        let said = fs::read_to_string(&log).unwrap();
        assert!(said.contains("one") && said.contains("two"), "{said}");
        assert!(!said.contains("three"), "{said}");
        fs::remove_dir_all(&dirs.queue).unwrap();
    }

    #[test]
    fn a_name_is_queued_once() {
        let dirs = scratch("publish");
        publish(&dirs, "a", "first").unwrap();
        assert!(publish(&dirs, "a", "second").is_err());
        assert_eq!(fs::read_to_string(dirs.file("a", "toml")).unwrap(), "first");
        assert!(!dirs.file("a", "tmp").exists());
        // Another add that holds the claim.
        fs::write(dirs.file("b", "tmp"), "theirs").unwrap();
        assert!(publish(&dirs, "b", "ours").is_err());
        assert!(!dirs.file("b", "toml").exists());
        assert_eq!(fs::read_to_string(dirs.file("b", "tmp")).unwrap(), "theirs");
        fs::remove_dir_all(&dirs.queue).unwrap();
    }
}
