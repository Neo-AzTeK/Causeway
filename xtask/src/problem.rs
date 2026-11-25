//! Problem sessions (docs/problem-sessions.md): which session owns which
//! problem, and the gates a problem passes to count as fixed.
//!
//! Claims and notes are one folder per problem in a machine-local registry,
//! shared by every worktree: they are derived from runs of the title, so they
//! never enter the repository. The ranked list (`docs/problems.md`) is tracked.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use clap::Subcommand;

/// Overrides where the registry is (tests, dry runs).
const DIR_VARIABLE: &str = "CAUSEWAY_PROBLEMS_DIR";
/// A claim whose session wrote nothing for this long may be taken over.
const STALE: u64 = 8 * 3600;
/// Notes must stay cheap to read back; older detail goes to the log.
const NOTE_LINES: usize = 150;
/// Another AI owns these (owner, 2026-11-25).
const EXTERNAL_PATHS: &[&str] = &[
    "crates/victory-gpu/src/descriptor",
    "crates/victory-gpu/src/descriptor.rs",
];
/// Changed by any problem's diff without being claimed.
const SHARED_PATHS: &[&str] = &["docs", "Cargo.lock"];

const PROBLEM: &str = include_str!("../../.claude/skills/problem-session/problem.md");
const NOTES: &str = include_str!("../../.claude/skills/problem-session/notes.md");
const PROOF: &str = include_str!("../../.claude/skills/problem-session/proof.md");

const DEFINITION: &[&str] = &["Symptom", "Evidence", "Effect", "Scope", "Done when"];
const NOTE_SECTIONS: &[&str] = &[
    "Now",
    "Next step",
    "Sub-causes",
    "Hypotheses",
    "Ruled out",
    "Commits and branch",
    "Runs",
    "Questions for owner",
];
const PROOF_SECTIONS: &[&str] = &[
    "Before",
    "After",
    "Same scene",
    "Test",
    "Checks",
    "Review",
    "Portability",
    "Not verified",
];

#[derive(Subcommand)]
pub enum Problem {
    /// The ranked list with each problem's state and holder.
    List,
    /// Make a problem's folder from the templates.
    New { id: String },
    /// Take a problem for this session, or add files to your claim.
    Claim {
        id: String,
        /// Repository paths this problem will change.
        #[arg(long, value_delimiter = ',')]
        files: Vec<String>,
        /// Claim although another claim covers the same files: say how the
        /// two are kept apart.
        #[arg(long)]
        accept_overlap: Option<String>,
        /// Take a claim whose session is dead.
        #[arg(long)]
        take_over: bool,
        /// The owner's words allowing a take-over of a claim not stale.
        #[arg(long, requires = "take_over")]
        owner: Option<String>,
    },
    /// Where a problem stands; start here, also after compaction.
    Resume { id: String },
    /// Record the owner's agreement to "Scope" and "Done when" in problem.md.
    Agree {
        id: String,
        /// The owner's words.
        #[arg(long)]
        owner: String,
    },
    /// Add a dated line to the log and keep the claim alive.
    Log { id: String, text: String },
    /// Format, agreed test, notes and changed files.
    Check { id: String },
    /// Stop on a question only the owner can answer.
    Block {
        id: String,
        #[arg(long)]
        question: String,
    },
    /// Go on with the owner's answer.
    Unblock {
        id: String,
        #[arg(long)]
        answer: String,
    },
    /// The fix is proven on this branch and waits for the owner's merge.
    Proven { id: String },
    /// The proven commit is in `main` and the "Done when" passed there.
    Confirm {
        id: String,
        /// What was run on `main` and what it gave.
        #[arg(long)]
        evidence: String,
        /// The owner's words, to confirm from the session that fixed it.
        #[arg(long)]
        owner: Option<String>,
    },
    /// A fixed problem is back.
    Reopen {
        id: String,
        #[arg(long)]
        evidence: String,
    },
    /// Give a problem up without calling it fixed.
    Release {
        id: String,
        #[arg(long)]
        reason: String,
        /// The owner's words: only they let a problem go unfixed.
        #[arg(long)]
        owner: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Status {
    #[default]
    Open,
    Active,
    Blocked,
    Proven,
    Fixed,
}

impl Status {
    const ALL: [(Status, &str); 5] = [
        (Status::Open, "open"),
        (Status::Active, "active"),
        (Status::Blocked, "blocked"),
        (Status::Proven, "proven"),
        (Status::Fixed, "fixed"),
    ];

    fn name(self) -> &'static str {
        Status::ALL
            .iter()
            .find(|(s, _)| *s == self)
            .map_or("?", |(_, n)| n)
    }
}

/// `state.txt`. A claim is a non-empty `session`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    status: Status,
    session: String,
    worktree: String,
    branch: String,
    heartbeat: u64,
    files: Vec<String>,
    /// Claims whose files this one may share: `problem@session`, so that it
    /// ends when the other problem changes hands.
    accepted: Vec<String>,
    proven_commit: String,
    proven_by: String,
}

impl State {
    fn write(&self) -> String {
        format!(
            "status={}\nsession={}\nworktree={}\nbranch={}\nheartbeat={}\nfiles={}\n\
             accepted={}\nproven_commit={}\nproven_by={}\nend=\n",
            self.status.name(),
            self.session,
            self.worktree,
            self.branch,
            self.heartbeat,
            self.files.join(","),
            self.accepted.join(","),
            self.proven_commit,
            self.proven_by,
        )
    }

    fn read(text: &str) -> Result<State> {
        let mut state = State::default();
        let mut keys = Vec::new();
        for line in text.lines() {
            let (key, value) = line
                .split_once('=')
                .with_context(|| format!("line {line:?}"))?;
            let value = value.to_owned();
            match key {
                "status" => {
                    state.status = Status::ALL
                        .iter()
                        .find(|(_, n)| *n == value)
                        .with_context(|| format!("status {value:?}"))?
                        .0;
                }
                "session" => state.session = value,
                "worktree" => state.worktree = value,
                "branch" => state.branch = value,
                "heartbeat" => state.heartbeat = value.parse().context("heartbeat")?,
                "files" => state.files = list(&value),
                "accepted" => state.accepted = list(&value),
                "proven_commit" => state.proven_commit = value,
                "proven_by" => state.proven_by = value,
                "end" => {}
                other => bail!("unknown key {other:?}"),
            }
            keys.push(key);
        }
        ensure!(
            keys.first() == Some(&"status") && keys.last() == Some(&"end"),
            "cut short"
        );
        Ok(state)
    }
}

fn list(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect()
}

fn pair(id: &str, session: &str) -> String {
    format!("{id}@{session}")
}

/// What the commands need to know of the checkout they run in.
pub struct Checkout {
    pub root: PathBuf,
    /// This session: kept through compaction, new for a new session.
    pub session: String,
    pub now: u64,
}

struct Registry {
    dir: PathBuf,
    here: Checkout,
}

/// One writer of the registry at a time; freed when dropped or on exit.
fn lock(dir: &Path) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 0);
    let path = dir.join(".lock");
    let mut tries = 0;
    loop {
        match options.open(&path) {
            Ok(file) => return Ok(file),
            Err(_) if tries < 100 => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e).with_context(|| format!("{} stays taken", path.display())),
        }
        tries += 1;
    }
}

/// Where the registry is on this machine. Not under `AppData`, which a
/// packaged app may redirect for itself.
fn registry_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(DIR_VARIABLE) {
        return Ok(dir.into());
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .with_context(|| format!("no home folder; set {DIR_VARIABLE}"))?;
    Ok(PathBuf::from(home).join(".causeway").join("problems"))
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .context("running git")?;
    ensure!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
}

fn open(root: &Path) -> Result<Registry> {
    let dir = registry_dir()?;
    victory_core::ensure_outside_repo(&dir)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let session = std::env::var("CLAUDE_CODE_SESSION_ID")
        .or_else(|_| std::env::var("CAUSEWAY_SESSION"))
        .unwrap_or_else(|_| format!("worktree:{}", root.display()));
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(Registry {
        dir,
        here: Checkout {
            root: root.into(),
            session,
            now,
        },
    })
}

pub fn run(root: &Path, command: &Problem) -> Result<()> {
    let registry = open(root)?;
    let _writer = lock(&registry.dir)?;
    println!("{}", registry.apply(command)?);
    Ok(())
}

/// For `smoke --problem`: writes a run into the problem's log. Refused unless
/// this session holds the claim, which it keeps alive.
pub fn record_run(root: &Path, id: &str, text: &str) -> Result<()> {
    run(
        root,
        &Problem::Log {
            id: id.into(),
            text: text.into(),
        },
    )
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn heading(line: &str) -> Option<&str> {
    line.strip_prefix("## ").map(str::trim)
}

/// Whether `problem.md` has the definition's sections, in order and no other,
/// so that all of what follows "Scope" is agreed.
fn whole_definition(text: &str) -> bool {
    text.lines()
        .filter_map(heading)
        .eq(DEFINITION.iter().copied())
}

/// The text under `## name`, comments out, trimmed.
fn section(text: &str, name: &str) -> Option<String> {
    let mut lines = text.lines();
    lines.find(|l| heading(l) == Some(name))?;
    let body: Vec<_> = lines
        .take_while(|l| !l.starts_with("## "))
        .filter(|l| !(l.trim_start().starts_with("<!--") && l.trim_end().ends_with("-->")))
        .map(str::trim_end)
        .collect();
    Some(body.join("\n").trim().to_owned())
}

/// What the owner agrees to: "Scope", and "Done when" to the end of the file,
/// so that nothing added after it goes unseen.
fn agreed_part(definition: &str) -> String {
    let scope = section(definition, "Scope").unwrap_or_default();
    let mut lines = definition.lines();
    lines.find(|l| heading(l) == Some("Done when"));
    let test: Vec<_> = lines.map(str::trim_end).collect();
    format!(
        "## Scope\n\n{scope}\n\n## Done when\n\n{}\n",
        test.join("\n").trim()
    )
}

/// The sections of `names` that are missing, empty or still `TODO`.
fn unfilled(text: &str, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter(|name| {
            section(text, name).is_none_or(|body| {
                // The word, so that a file named TODO.md can be spoken of.
                body.is_empty() || body.split_whitespace().any(|w| w == "TODO" || w == "TODO:")
            })
        })
        .map(|name| (*name).to_owned())
        .collect()
}

fn valid_id(id: &str) -> Result<()> {
    ensure!(
        (3..=48).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "{id:?}: a problem id is 3-48 of a-z, 0-9 and -"
    );
    Ok(())
}

fn normal(path: &str) -> Result<String> {
    let path = path.trim().replace('\\', "/");
    let path = path.trim_start_matches("./").trim_end_matches('/');
    ensure!(
        !path.is_empty()
            && !path.contains([':', '*', ',', '='])
            && !path.starts_with('/')
            && !path.split('/').any(|part| part == ".." || part == "."),
        "{path:?}: a plain path inside the repository, a file or a folder"
    );
    Ok(path.to_owned())
}

/// Whether `path` is `folder` or inside it.
fn within(path: &str, folder: &str) -> bool {
    // Windows does not tell case apart.
    let (path, folder) = (path.to_ascii_lowercase(), folder.to_ascii_lowercase());
    path.strip_prefix(&folder)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

struct Ranked {
    rank: u32,
    id: String,
    owner: String,
}

/// The table rows `| rank | id | owner | ...` of `docs/problems.md`.
fn ranked(text: &str) -> Vec<Ranked> {
    text.lines()
        .filter_map(|line| {
            let mut cells = line.trim().strip_prefix('|')?.split('|').map(str::trim);
            Some(Ranked {
                rank: cells.next()?.parse().ok()?,
                id: cells.next()?.trim_matches('`').to_owned(),
                owner: cells.next()?.to_owned(),
            })
        })
        .collect()
}

impl Registry {
    fn folder(&self, id: &str) -> Result<PathBuf> {
        valid_id(id)?;
        Ok(self.dir.join(id))
    }

    fn state(&self, id: &str) -> Result<State> {
        let path = self.folder(id)?.join("state.txt");
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!("no problem {id:?} here ({}): `problem new`", path.display())
        })?;
        State::read(&text).with_context(|| {
            format!(
                "{} is damaged; mend it by hand, from log.md",
                path.display()
            )
        })
    }

    fn save(&self, id: &str, state: &State) -> Result<()> {
        let folder = self.folder(id)?;
        let temporary = folder.join("state.tmp");
        std::fs::write(&temporary, state.write()).context("writing state.tmp")?;
        std::fs::rename(&temporary, folder.join("state.txt")).context("replacing state.txt")
    }

    fn file(&self, id: &str, name: &str) -> Result<String> {
        let path = self.folder(id)?.join(name);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(text.replace("\r\n", "\n"))
    }

    fn append(&self, id: &str, text: &str) -> Result<()> {
        use std::io::Write;
        let path = self.folder(id)?.join("log.md");
        let mut log = File::options().create(true).append(true).open(path)?;
        let session: String = self.here.session.chars().take(8).collect();
        writeln!(
            log,
            "- {} [{session}] {}",
            date(self.here.now),
            one_line(text)
        )?;
        Ok(())
    }

    /// Every problem with a folder here.
    fn ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if entry.path().join("state.txt").is_file() {
                ids.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn table(&self) -> Vec<Ranked> {
        std::fs::read_to_string(self.here.root.join("docs/problems.md"))
            .map(|t| ranked(&t))
            .unwrap_or_default()
    }

    /// Who else owns a problem, by this checkout's list or `main`'s: a branch
    /// cannot take one over by editing its copy.
    fn external(&self, id: &str) -> Result<Option<String>> {
        let main = git(&self.here.root, &["show", "main:docs/problems.md"])
            .map(|t| ranked(&t))
            .unwrap_or_default();
        let mut rows = self.table();
        rows.extend(main);
        ensure!(
            !rows.is_empty(),
            "no table in docs/problems.md, here or in main"
        );
        Ok(rows
            .into_iter()
            .find(|r| r.id == id && r.owner != "here")
            .map(|r| r.owner))
    }

    fn stale(&self, state: &State) -> bool {
        self.here.now.saturating_sub(state.heartbeat) > STALE
            || (!state.worktree.is_empty() && !Path::new(&state.worktree).exists())
    }

    /// The state of a problem this session holds.
    fn mine(&self, id: &str) -> Result<State> {
        let state = self.state(id)?;
        ensure!(
            state.session == self.here.session,
            "this session does not hold {id}: {}",
            if state.session.is_empty() {
                "claim it first".to_owned()
            } else {
                format!("session {} in {} does; stop", state.session, state.worktree)
            }
        );
        Ok(state)
    }

    // A damaged state file fails this on purpose: its claim might meet any.
    /// Other claims whose files meet `files`, unless one of the two accepted
    /// it: each other problem's id and where they meet.
    fn overlaps(&self, id: &str, state: &State, files: &[String]) -> Result<Vec<(String, String)>> {
        let mut found = Vec::new();
        for other in self.ids()?.into_iter().filter(|o| o != id) {
            let theirs = self.state(&other)?;
            if theirs.session.is_empty()
                || state.accepted.contains(&pair(&other, &theirs.session))
                || theirs.accepted.contains(&pair(id, &self.here.session))
            {
                continue;
            }
            for mine in files {
                for their in &theirs.files {
                    if within(mine, their) || within(their, mine) {
                        found.push((other.clone(), format!("{mine} meets {their} of {other}")));
                    }
                }
            }
        }
        Ok(found)
    }

    /// Whether "Scope" and "Done when" are as agreed; `None` before `agree`.
    fn as_agreed(&self, id: &str) -> Result<Option<bool>> {
        if !self.folder(id)?.join("agreed.md").exists() {
            return Ok(None);
        }
        let agreed = self.file(id, "agreed.md")?;
        let definition = self.file(id, "problem.md")?;
        let now = agreed_part(&definition);
        Ok(Some(
            whole_definition(&definition)
                && agreed
                    .split_once("\n\n")
                    .is_some_and(|(_, part)| part == now),
        ))
    }

    /// What the branch and the working tree change against `main`.
    fn changed(&self) -> Result<Vec<String>> {
        let root = &self.here.root;
        let mut files: Vec<String> = Vec::new();
        for args in [
            &["diff", "--name-only", "--no-renames", "main...HEAD"][..],
            &["diff", "--name-only", "--no-renames", "HEAD"][..],
            &["ls-files", "--others", "--exclude-standard"][..],
        ] {
            files.extend(git(root, args)?.lines().map(str::to_owned));
        }
        files.sort();
        files.dedup();
        Ok(files)
    }

    fn problems(&self, id: &str, state: &State, changed: &[String]) -> Result<Vec<String>> {
        let mut found = Vec::new();
        let definition = self.file(id, "problem.md")?;
        if !whole_definition(&definition) {
            found.push(format!(
                "problem.md: its \"## \" headings must be {}, in that order and no other",
                DEFINITION.join(", ")
            ));
        }
        for name in unfilled(&definition, DEFINITION) {
            found.push(format!("problem.md: \"{name}\" is not filled"));
        }
        match self.as_agreed(id)? {
            None => found.push("\"Done when\" is not agreed by the owner (`agree`)".into()),
            Some(false) => found.push(
                "\"Scope\" or \"Done when\" differs from what the owner agreed: put it back, or \
                 ask them and `agree` again"
                    .into(),
            ),
            Some(true) => {}
        }
        let notes = self.file(id, "notes.md")?;
        for name in NOTE_SECTIONS {
            if section(&notes, name).is_none() {
                found.push(format!("notes.md: no \"## {name}\""));
            }
        }
        if section(&notes, "Next step").is_some_and(|s| s.is_empty()) {
            found.push("notes.md: \"Next step\" is empty".into());
        }
        let lines = notes.lines().count();
        if lines > NOTE_LINES {
            found.push(format!(
                "notes.md: {lines} lines, at most {NOTE_LINES}: move old detail to log.md"
            ));
        }
        for file in changed {
            if EXTERNAL_PATHS.iter().any(|e| within(file, e)) {
                found.push(format!("{file}: descriptor work belongs to another AI"));
            } else if !SHARED_PATHS
                .iter()
                .copied()
                .chain(state.files.iter().map(String::as_str))
                .any(|f| within(file, f))
            {
                found.push(format!("{file}: changed but not claimed (`claim --files`)"));
            }
        }
        found.extend(
            self.overlaps(id, state, &state.files)?
                .into_iter()
                .map(|(_, at)| at),
        );
        Ok(found)
    }

    fn apply(&self, command: &Problem) -> Result<String> {
        match command {
            Problem::List => self.list(),
            Problem::New { id } => self.new_problem(id),
            Problem::Claim {
                id,
                files,
                accept_overlap,
                take_over,
                owner,
            } => self.claim(
                id,
                files,
                accept_overlap.as_deref(),
                *take_over,
                owner.as_deref(),
            ),
            Problem::Resume { id } => self.resume(id),
            Problem::Agree { id, owner } => {
                let mut state = self.mine(id)?;
                ensure!(
                    matches!(state.status, Status::Active | Status::Blocked),
                    "{id} is {}: what was proven was proven against the old agreement",
                    state.status.name()
                );
                let definition = self.file(id, "problem.md")?;
                ensure!(
                    whole_definition(&definition),
                    "problem.md: its \"## \" headings must be {}, in that order and no other",
                    DEFINITION.join(", ")
                );
                let missing = unfilled(&definition, DEFINITION);
                ensure!(
                    missing.is_empty(),
                    "problem.md: not filled: {}",
                    missing.join(", ")
                );
                if let Ok(old) = self.file(id, "agreed.md") {
                    self.append(id, &format!("replaced agreement: {old}"))?;
                }
                let agreed = format!(
                    "Owner, {}: {}\n\n{}",
                    date(self.here.now),
                    one_line(owner),
                    agreed_part(&definition)
                );
                std::fs::write(self.folder(id)?.join("agreed.md"), agreed)?;
                self.append(id, &format!("agreed \"Done when\"; owner: {owner}"))?;
                state.heartbeat = self.here.now;
                self.save(id, &state)?;
                Ok(format!("{id}: \"Done when\" recorded as agreed"))
            }
            Problem::Log { id, text } => {
                let mut state = self.mine(id)?;
                self.append(id, text)?;
                state.heartbeat = self.here.now;
                self.save(id, &state)?;
                Ok(format!("{id}: logged; keep notes.md current too"))
            }
            Problem::Check { id } => {
                let mut state = self.mine(id)?;
                state.heartbeat = self.here.now;
                self.save(id, &state)?;
                let found = self.problems(id, &state, &self.changed()?)?;
                ensure!(found.is_empty(), "{id}:\n  {}", found.join("\n  "));
                Ok(format!("{id}: check ok"))
            }
            Problem::Block { id, question } => {
                let mut state = self.mine(id)?;
                ensure!(
                    state.status == Status::Active,
                    "{id} is {}",
                    state.status.name()
                );
                state.status = Status::Blocked;
                state.heartbeat = self.here.now;
                self.append(id, &format!("blocked, asks the owner: {question}"))?;
                self.save(id, &state)?;
                Ok(format!("{id}: blocked. Ask the owner, then end the turn."))
            }
            Problem::Unblock { id, answer } => {
                let mut state = self.mine(id)?;
                ensure!(
                    state.status == Status::Blocked,
                    "{id} is {}",
                    state.status.name()
                );
                state.status = Status::Active;
                state.heartbeat = self.here.now;
                self.append(id, &format!("unblocked; owner: {answer}"))?;
                self.save(id, &state)?;
                Ok(format!("{id}: active"))
            }
            Problem::Proven { id } => self.proven(id),
            Problem::Confirm {
                id,
                evidence,
                owner,
            } => self.confirm(id, evidence, owner.as_deref()),
            Problem::Reopen { id, evidence } => {
                let mut state = self.state(id)?;
                ensure!(
                    matches!(state.status, Status::Fixed | Status::Proven),
                    "{id} is {}: only a fixed or proven problem reopens",
                    state.status.name()
                );
                // The old proof proved too little.
                let folder = self.folder(id)?;
                let kept = folder.join(format!("proof-{}.md", self.here.now));
                std::fs::rename(folder.join("proof.md"), kept).context("keeping the old proof")?;
                std::fs::write(folder.join("proof.md"), PROOF)?;
                self.append(id, &format!("reopened: {evidence}"))?;
                let said = if state.status == Status::Proven {
                    // Still its session's, with time to answer.
                    state.status = Status::Active;
                    state.heartbeat = self.here.now;
                    state.proven_commit.clear();
                    state.proven_by.clear();
                    "active again, held by its session"
                } else {
                    state = State {
                        branch: state.branch,
                        ..State::default()
                    };
                    "open again"
                };
                self.save(id, &state)?;
                Ok(format!("{id}: {said}; the agreed \"Done when\" stands"))
            }
            Problem::Release { id, reason, owner } => {
                let mut state = self.state(id)?;
                ensure!(!state.session.is_empty(), "{id} is not claimed");
                self.append(id, &format!("owner allowed the release: {owner}"))?;
                self.append(id, &format!("released ({}): {reason}", state.status.name()))?;
                // The branch stays named: the next session goes on from it.
                state = State {
                    branch: state.branch,
                    ..State::default()
                };
                self.save(id, &state)?;
                Ok(format!("{id}: released, open, not fixed"))
            }
        }
    }

    fn list(&self) -> Result<String> {
        let mut table = self.table();
        table.sort_by_key(|r| r.rank);
        let here = self.ids()?;
        let unranked = here.iter().filter(|id| !table.iter().any(|r| r.id == **id));
        let rows = table
            .iter()
            .map(|r| (r.rank.to_string(), r.id.clone(), r.owner.clone()))
            .chain(unranked.map(|id| ("-".into(), id.clone(), "here".into())));
        let mut out = format!("registry: {}", self.dir.display());
        for (rank, id, owner) in rows {
            let what = if owner != "here" {
                format!("external ({owner})")
            } else if here.contains(&id) {
                match self.state(&id) {
                    Err(e) => format!("DAMAGED: {e:#}"),
                    Ok(s) if s.session.is_empty() => s.status.name().into(),
                    Ok(s) => format!(
                        "{}{} by {:.8} on {}, quiet {} min",
                        s.status.name(),
                        if s.status == Status::Active && self.stale(&s) {
                            " (STALE)"
                        } else {
                            ""
                        },
                        s.session,
                        s.branch,
                        self.here.now.saturating_sub(s.heartbeat) / 60
                    ),
                }
            } else {
                "no folder here (`problem new`)".into()
            };
            out += &format!("\n{rank:>3} {id:<32} {what}");
        }
        Ok(out)
    }

    fn new_problem(&self, id: &str) -> Result<String> {
        let folder = self.folder(id)?;
        ensure!(!folder.exists(), "{} is there already", folder.display());
        std::fs::create_dir_all(&folder)?;
        for (name, text) in [
            ("problem.md", PROBLEM),
            ("notes.md", NOTES),
            ("proof.md", PROOF),
        ] {
            std::fs::write(folder.join(name), text)?;
        }
        self.save(id, &State::default())?;
        self.append(id, "made")?;
        Ok(format!("{id}: {}", folder.display()))
    }

    fn claim(
        &self,
        id: &str,
        files: &[String],
        accept_overlap: Option<&str>,
        take_over: bool,
        owner: Option<&str>,
    ) -> Result<String> {
        if let Some(who) = self.external(id)? {
            bail!("{id} belongs to {who} (docs/problems.md): do not work on it");
        }
        let mut state = self.state(id)?;
        ensure!(
            state.status != Status::Fixed,
            "{id} is fixed; a regression is `reopen`ed first"
        );
        // One problem per session.
        for other in self.ids()?.iter().filter(|o| *o != id) {
            ensure!(
                self.state(other)?.session != self.here.session,
                "this session holds {other}: one problem per session"
            );
        }
        let files = files
            .iter()
            .map(|f| normal(f))
            .collect::<Result<Vec<_>>>()?;
        for file in &files {
            ensure!(
                !EXTERNAL_PATHS.iter().any(|e| within(file, e)),
                "{file}: descriptor work belongs to another AI"
            );
        }
        let held = state.session.clone();
        if !held.is_empty() && held != self.here.session {
            ensure!(
                take_over,
                "{id} is claimed by session {held} in {} ({}{}); a dead session's claim is taken \
                 with --take-over",
                state.worktree,
                state.status.name(),
                if self.stale(&state) { ", stale" } else { "" }
            );
            let dead = state.status == Status::Active && self.stale(&state);
            ensure!(
                dead || owner.is_some(),
                "{id} is {}{}: only the owner can give it away (--owner \"their words\")",
                state.status.name(),
                if self.stale(&state) {
                    ""
                } else {
                    ", not stale"
                }
            );
        }
        let mut all = state.files.clone();
        all.extend(files);
        all.sort();
        all.dedup();
        if held != self.here.session {
            // What the earlier holder accepted was theirs to keep apart.
            state.accepted.clear();
        }
        let overlaps = self.overlaps(id, &state, &all)?;
        if !overlaps.is_empty() {
            let met: Vec<_> = overlaps.iter().map(|(_, at)| at.as_str()).collect();
            let why = accept_overlap.with_context(|| {
                format!(
                    "files overlap another claim:\n  {}\nnarrow --files, or say how the two are \
                     kept apart with --accept-overlap",
                    met.join("\n  ")
                )
            })?;
            self.append(id, &format!("accepted overlap ({}): {why}", met.join("; ")))?;
            for (other, _) in &overlaps {
                let session = self.state(other)?.session;
                state.accepted.push(pair(other, &session));
            }
            state.accepted.sort();
            state.accepted.dedup();
        }
        let branch = git(&self.here.root, &["branch", "--show-current"]).unwrap_or_default();
        let mut said = format!("{id}: claimed by this session");
        if held != self.here.session {
            if !held.is_empty() {
                let words = owner.map(|o| format!("; owner: {o}")).unwrap_or_default();
                self.append(id, &format!("taken over from session {held}{words}"))?;
            }
            if !state.branch.is_empty() && state.branch != branch {
                said += &format!(
                    "\nthe work so far is on branch {}: build on it (and check its worktree for \
                     uncommitted files: {})",
                    state.branch,
                    if state.worktree.is_empty() {
                        "released"
                    } else {
                        &state.worktree
                    }
                );
            }
        }
        if state.status == Status::Open {
            state.status = Status::Active;
        }
        state.session = self.here.session.clone();
        state.worktree = self.here.root.display().to_string();
        state.branch = branch;
        state.heartbeat = self.here.now;
        state.files = all;
        self.append(
            id,
            &format!(
                "claimed on {} for files [{}]",
                state.branch,
                state.files.join(", ")
            ),
        )?;
        self.save(id, &state)?;
        for (_, overlap) in overlaps {
            said += &format!("\naccepted overlap: {overlap}");
        }
        Ok(said)
    }

    fn resume(&self, id: &str) -> Result<String> {
        let mut state = self.state(id)?;
        let folder = self.folder(id)?;
        let mine = state.session == self.here.session;
        if mine {
            state.heartbeat = self.here.now;
            self.save(id, &state)?;
        }
        let holder = match &*state.session {
            "" => "nobody (claim it before working)".to_owned(),
            _ if mine => "this session".to_owned(),
            other => format!("session {other} in {}: do NOT work on it", state.worktree),
        };
        let mut out = format!(
            "{id}: {}; held by {holder}\nfolder: {}\nbranch: {}\nfiles: {}",
            state.status.name(),
            folder.display(),
            if state.branch.is_empty() {
                "-"
            } else {
                &state.branch
            },
            state.files.join(", ")
        );
        match self.as_agreed(id)? {
            None => out += "\n\"Done when\" NOT AGREED: define the problem and ask the owner first",
            Some(false) => out += "\n\"Scope\" or \"Done when\" CHANGED since the owner agreed it",
            Some(true) => {}
        }
        let notes = self.file(id, "notes.md")?;
        for name in ["Now", "Next step"] {
            out += &format!(
                "\n\n## {name}\n{}",
                section(&notes, name).unwrap_or_default()
            );
        }
        let log = self.file(id, "log.md")?;
        let lines: Vec<_> = log.lines().collect();
        out += "\n\nlast log lines:";
        for line in &lines[lines.len().saturating_sub(8)..] {
            out += &format!("\n{}", line.chars().take(200).collect::<String>());
        }
        out += "\n\nnow read problem.md, agreed.md and notes.md in that folder";
        Ok(out)
    }

    fn proven(&self, id: &str) -> Result<String> {
        let mut state = self.mine(id)?;
        ensure!(
            state.status == Status::Active,
            "{id} is {}",
            state.status.name()
        );
        let mut found = self.problems(id, &state, &self.changed()?)?;
        let notes = self.file(id, "notes.md")?;
        let causes = section(&notes, "Sub-causes").unwrap_or_default();
        let mut ticked = 0;
        for line in causes.lines().filter(|l| !l.trim().is_empty()) {
            let done = line.starts_with("- [x] ") || line.starts_with("- [X] ");
            ticked += u32::from(done);
            if line.contains("[ ]") {
                found.push(format!("sub-cause open: {}", line.trim()));
            } else if !done && !line.starts_with(char::is_whitespace) {
                found.push(format!("notes.md: not a `- [x]` sub-cause line: {line}"));
            }
        }
        if ticked == 0 {
            found.push("notes.md: no sub-cause listed and ticked (`- [x] ...`)".into());
        }
        let branch = git(&self.here.root, &["branch", "--show-current"])?;
        if branch.is_empty() || branch == "main" {
            found.push("a fix is proven on its own branch, before the owner merges it".into());
        }
        if git(&self.here.root, &["diff", "--name-only", "main...HEAD"])?.is_empty() {
            found.push("the branch changes nothing that main lacks".into());
        }
        for name in unfilled(&self.file(id, "proof.md")?, PROOF_SECTIONS) {
            found.push(format!("proof.md: \"{name}\" is not filled"));
        }
        if !git(&self.here.root, &["status", "--porcelain"])?.is_empty() {
            found.push("the working tree has uncommitted files".into());
        }
        ensure!(
            found.is_empty(),
            "{id} is not proven:\n  {}",
            found.join("\n  ")
        );
        state.status = Status::Proven;
        state.proven_commit = git(&self.here.root, &["rev-parse", "HEAD"])?;
        state.proven_by = self.here.session.clone();
        state.branch = branch;
        state.heartbeat = self.here.now;
        self.append(
            id,
            &format!("proven at {} on {}", state.proven_commit, state.branch),
        )?;
        self.save(id, &state)?;
        Ok(format!(
            "{id}: proven at {:.10}. Not fixed yet: ask the owner to merge; then `confirm`.",
            state.proven_commit
        ))
    }

    fn confirm(&self, id: &str, evidence: &str, owner: Option<&str>) -> Result<String> {
        let mut state = self.state(id)?;
        ensure!(
            state.status == Status::Proven,
            "{id} is {}, not proven",
            state.status.name()
        );
        ensure!(
            state.proven_by != self.here.session || owner.is_some(),
            "the session that fixed {id} does not confirm it: another session, or --owner"
        );
        ensure!(
            self.as_agreed(id)? == Some(true),
            "{id}: \"Scope\" or \"Done when\" changed since it was proven: `reopen`"
        );
        git(
            &self.here.root,
            &["merge-base", "--is-ancestor", &state.proven_commit, "main"],
        )
        .with_context(|| format!("{} is not in main", state.proven_commit))?;
        let words = owner.map(|o| format!("; owner: {o}")).unwrap_or_default();
        self.append(id, &format!("confirmed fixed on main: {evidence}{words}"))?;
        state = State {
            status: Status::Fixed,
            proven_commit: state.proven_commit,
            proven_by: state.proven_by,
            branch: state.branch,
            ..State::default()
        };
        self.save(id, &state)?;
        Ok(format!("{id}: fixed"))
    }
}

/// `YYYY-MM-DD HH:MMZ`.
fn date(unix: u64) -> String {
    let (days, seconds) = (unix / 86400, unix % 86400);
    // Howard Hinnant's civil_from_days, for days since 1970.
    let z = days + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}Z",
        seconds / 3600,
        seconds % 3600 / 60
    )
}

#[cfg(test)]
#[path = "problem/tests.rs"]
mod tests;
