//! Problem sessions (docs/problem-sessions.md): each problem's notes, and
//! the gates it passes to count as fixed. Problems are not owned or locked:
//! any session may work on any number of them, so one is never stuck behind
//! a session that stopped.
//!
//! Notes are one folder per problem in a machine-local registry, shared by
//! every worktree: they are derived from runs of the title, so they never
//! enter the repository. The ranked list (`docs/problems.md`) is tracked.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use clap::Subcommand;

/// Overrides where the registry is (tests, dry runs).
const DIR_VARIABLE: &str = "CAUSEWAY_PROBLEMS_DIR";
/// Notes must stay cheap to read back; older detail goes to the log.
const NOTE_LINES: usize = 150;
/// Keys of `state.txt` from when problems had holders: read and dropped.
const OLD_KEYS: &[&str] = &["session", "worktree", "heartbeat", "files", "accepted"];

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
    /// The ranked list with each problem's state and branch.
    List,
    /// Make a problem's folder from the templates.
    New { id: String },
    /// Where a problem stands; start here, also after compaction.
    Resume { id: String },
    /// Record the owner's agreement to "Scope" and "Done when" in problem.md.
    Agree {
        id: String,
        /// The owner's words.
        #[arg(long)]
        owner: String,
    },
    /// Add a dated line to the log.
    Log { id: String, text: String },
    /// Format, agreed test and notes.
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

/// `state.txt`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    status: Status,
    /// Where the work was last logged from.
    branch: String,
    proven_commit: String,
    /// The session that proved it: another one (or the owner) confirms it.
    proven_by: String,
}

impl State {
    fn write(&self) -> String {
        format!(
            "status={}\nbranch={}\nproven_commit={}\nproven_by={}\nend=\n",
            self.status.name(),
            self.branch,
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
                "branch" => state.branch = value,
                old if OLD_KEYS.contains(&old) => {}
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

/// For `smoke --problem`: writes a run into the problem's log.
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

struct Ranked {
    rank: u32,
    id: String,
}

/// The table rows `| rank | id | ...` of `docs/problems.md`.
fn ranked(text: &str) -> Vec<Ranked> {
    text.lines()
        .filter_map(|line| {
            let mut cells = line.trim().strip_prefix('|')?.split('|').map(str::trim);
            Some(Ranked {
                rank: cells.next()?.parse().ok()?,
                id: cells.next()?.trim_matches('`').to_owned(),
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

    /// The state of a problem being worked on: an open one becomes active,
    /// on this checkout's branch; a fixed one is `reopen`ed first.
    fn working(&self, id: &str) -> Result<State> {
        let mut state = self.state(id)?;
        ensure!(
            state.status != Status::Fixed,
            "{id} is fixed; a regression is `reopen`ed first"
        );
        if state.status == Status::Open {
            state.status = Status::Active;
        }
        let branch = git(&self.here.root, &["branch", "--show-current"]).unwrap_or_default();
        if !branch.is_empty() && state.status == Status::Active {
            state.branch = branch;
        }
        Ok(state)
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

    fn problems(&self, id: &str) -> Result<Vec<String>> {
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
        Ok(found)
    }

    fn apply(&self, command: &Problem) -> Result<String> {
        match command {
            Problem::List => self.list(),
            Problem::New { id } => self.new_problem(id),
            Problem::Resume { id } => self.resume(id),
            Problem::Agree { id, owner } => {
                let state = self.working(id)?;
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
                self.save(id, &state)?;
                Ok(format!("{id}: \"Done when\" recorded as agreed"))
            }
            Problem::Log { id, text } => {
                let state = self.working(id)?;
                self.append(id, text)?;
                self.save(id, &state)?;
                Ok(format!("{id}: logged; keep notes.md current too"))
            }
            Problem::Check { id } => {
                let found = self.problems(id)?;
                ensure!(found.is_empty(), "{id}:\n  {}", found.join("\n  "));
                Ok(format!("{id}: check ok"))
            }
            Problem::Block { id, question } => {
                let mut state = self.working(id)?;
                ensure!(
                    state.status == Status::Active,
                    "{id} is {}",
                    state.status.name()
                );
                state.status = Status::Blocked;
                self.append(id, &format!("blocked, asks the owner: {question}"))?;
                self.save(id, &state)?;
                Ok(format!("{id}: blocked. Ask the owner, then end the turn."))
            }
            Problem::Unblock { id, answer } => {
                let mut state = self.working(id)?;
                ensure!(
                    state.status == Status::Blocked,
                    "{id} is {}",
                    state.status.name()
                );
                state.status = Status::Active;
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
                    state.status = Status::Active;
                    state.proven_commit.clear();
                    state.proven_by.clear();
                    "active again"
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
        }
    }

    fn list(&self) -> Result<String> {
        let mut table = self.table();
        table.sort_by_key(|r| r.rank);
        let here = self.ids()?;
        let unranked = here.iter().filter(|id| !table.iter().any(|r| r.id == **id));
        let rows = table
            .iter()
            .map(|r| (r.rank.to_string(), r.id.clone()))
            .chain(unranked.map(|id| ("-".into(), id.clone())));
        let mut out = format!("registry: {}", self.dir.display());
        for (rank, id) in rows {
            let what = if here.contains(&id) {
                match self.state(&id) {
                    Err(e) => format!("DAMAGED: {e:#}"),
                    Ok(s) if s.branch.is_empty() => s.status.name().into(),
                    Ok(s) => format!("{} on {}", s.status.name(), s.branch),
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

    fn resume(&self, id: &str) -> Result<String> {
        let state = self.state(id)?;
        let folder = self.folder(id)?;
        let mut out = format!(
            "{id}: {}\nfolder: {}\nbranch: {}",
            state.status.name(),
            folder.display(),
            if state.branch.is_empty() {
                "-"
            } else {
                &state.branch
            },
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
        let mut state = self.working(id)?;
        ensure!(
            state.status == Status::Active,
            "{id} is {}",
            state.status.name()
        );
        let mut found = self.problems(id)?;
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
        state.status = Status::Fixed;
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
