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

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};

const DEFAULT_QUEUE: &str = r"E:\Causeway-tools\queue";
const DEFAULT_RUNS: &str = r"E:\Causeway-tools\runs";
const LOCK: &str = "runner.lock";
const POLL: Duration = Duration::from_secs(5);

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
    /// Start although `runner.lock` exists (its runner is gone).
    #[arg(long)]
    replace_lock: bool,
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
    env: Vec<(String, String)>,
    baseline: Option<String>,
    requester: Option<String>,
    data_dir: Option<PathBuf>,
    data_from: Option<PathBuf>,
    keys: Option<String>,
}

const FIELDS: &[&str] = &[
    "name", "worktree", "commit", "seconds", "env", "baseline", "requester", "data_dir",
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
    for ext in ["toml", "done", "running"] {
        ensure!(!dirs.file(&request.name, ext).exists(), "{} exists: pick another name", dirs.file(&request.name, ext).display());
    }
    let tmp = dirs.file(&request.name, "tmp");
    fs::write(&tmp, &text)?;
    fs::rename(&tmp, dirs.file(&request.name, "toml"))?;
    let done = dirs.file(&request.name, "done");
    println!("queued {} ({} s); wait for {}", request.name, request.seconds, done.display());
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

fn write_done(dirs: &Dirs, name: &str, exit: i64, digest: Option<&Path>, error: Option<&str>) -> Result<()> {
    let mut table = toml::Table::new();
    table.insert("name".into(), name.to_owned().into());
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
    let lock = dirs.queue.join(LOCK);
    if args.replace_lock {
        let _ = fs::remove_file(&lock);
    }
    match fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
        Ok(mut f) => {
            use std::io::Write as _;
            writeln!(f, "pid {}\nstarted_unix {}", std::process::id(), unix_now())?;
        }
        Err(_) => bail!(
            "{} exists: a runner is running (two would run two real-GPU runs at once). \
             If it is gone, start with --replace-lock",
            lock.display()
        ),
    }
    // A run a dead runner left running will not finish.
    if let Ok(entries) = fs::read_dir(&dirs.queue) {
        for path in entries.filter_map(|e| Some(e.ok()?.path())) {
            if path.extension().is_some_and(|e| e == "running")
                && let Some(name) = path.file_stem().and_then(|n| n.to_str())
                && !dirs.file(name, "done").exists()
            {
                write_done(dirs, name, -1, None, Some("the runner stopped while it ran"))?;
            }
        }
    }
    println!("runner: queue {}, runs {}", dirs.queue.display(), dirs.runs.display());
    let result = serve(root, dirs, args);
    let _ = fs::remove_file(&lock);
    result
}

fn serve(root: &Path, dirs: &Dirs, args: &RunArgs) -> Result<()> {
    loop {
        let Some(name) = waiting(dirs).into_iter().next() else {
            if args.once {
                return Ok(());
            }
            std::thread::sleep(POLL);
            continue;
        };
        fs::write(dirs.file(&name, "running"), format!("started_unix {}\n", unix_now()))?;
        let outcome = fs::read_to_string(dirs.file(&name, "toml"))
            .context("reading the request")
            .and_then(|text| Request::parse(&text))
            .and_then(|request| {
                ensure!(request.name == name, "the request names {}, its file {name}", request.name);
                request.check(root, dirs)?;
                println!("runner: start {name} ({} s)", request.seconds);
                launch(dirs, &request, &args.exe)
            });
        match outcome {
            Ok(code) => {
                let digest = dirs.runs.join(&name).join("digest.md");
                write_done(dirs, &name, code.into(), digest.is_file().then_some(digest.as_path()), None)?;
                println!("runner: done {name} exit={code}");
            }
            Err(e) => {
                write_done(dirs, &name, -1, None, Some(&format!("{e:#}")))?;
                println!("runner: refused {name}: {e:#}");
            }
        }
    }
}

/// Runs `cargo xtask smoke` for a checked request; its console goes to
/// `<runs>/<name>.console.txt`. The exit code, or -1 when it was a signal.
fn launch(dirs: &Dirs, request: &Request, exe: &Path) -> Result<i32> {
    let console = dirs.runs.join(format!("{}.console.txt", request.name));
    let out = fs::File::create(&console).with_context(|| format!("creating {}", console.display()))?;
    let err = out.try_clone()?;
    let status = Command::new("cargo")
        .args(request.smoke_args(dirs, exe))
        .current_dir(&request.worktree)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .status()
        .context("starting cargo")?;
    Ok(status.code().unwrap_or(-1))
}

fn status(dirs: &Dirs) -> Result<()> {
    let lock = fs::read_to_string(dirs.queue.join(LOCK)).ok();
    println!(
        "runner: {}",
        lock.map_or("not running".to_owned(), |l| l.lines().next().unwrap_or("running").to_owned())
    );
    let Ok(entries) = fs::read_dir(&dirs.queue) else {
        println!("no queue folder yet ({})", dirs.queue.display());
        return Ok(());
    };
    let mut rows: Vec<(SystemTime, String, String)> = Vec::new();
    for e in entries.filter_map(Result::ok) {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "toml") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
        let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
        let who = fs::read_to_string(&path)
            .ok()
            .and_then(|t| Request::parse(&t).ok())
            .map(|r| format!("{} s, {}{}", r.seconds, &r.commit[..r.commit.len().min(8)], r.requester.map_or(String::new(), |q| format!(", {q}"))))
            .unwrap_or_else(|| "unreadable".into());
        let state = if let Ok(done) = fs::read_to_string(dirs.file(&name, "done")) {
            let table: toml::Table = done.parse().unwrap_or_default();
            let code = table.get("exit").and_then(toml::Value::as_integer).unwrap_or(-1);
            match table.get("error").and_then(toml::Value::as_str) {
                Some(e) => format!("refused: {}", e.chars().take(80).collect::<String>()),
                None => format!("done, exit {code}"),
            }
        } else if dirs.file(&name, "running").exists() {
            "RUNNING".to_owned()
        } else {
            "queued".to_owned()
        };
        rows.push((modified, name, format!("{state}; {who}")));
    }
    rows.sort();
    // Waiting and running first, then the latest finished.
    let (open, done): (Vec<_>, Vec<_>) =
        rows.into_iter().partition(|(_, _, s)| s.starts_with("queued") || s.starts_with("RUNNING"));
    for (_, name, state) in open.iter().chain(done.iter().rev().take(10)) {
        println!("{name:<28} {state}");
    }
    println!("{} open, {} finished", open.len(), done.len());
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
}
