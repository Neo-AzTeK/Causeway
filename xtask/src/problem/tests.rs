//! Each test has its own registry and its own git repository.

use super::*;

struct World {
    base: PathBuf,
}

impl World {
    fn new(name: &str) -> World {
        let base =
            std::env::temp_dir().join(format!("causeway-problem-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("repo/docs")).unwrap();
        std::fs::create_dir_all(base.join("registry")).unwrap();
        let world = World { base };
        world.git(&["init", "-q", "-b", "main"]);
        world.write(
            "docs/problems.md",
            "| rank | id | effect |\n|---|---|---|\n| 1 | `first` | e |\n",
        );
        world.commit("start");
        world.git(&["checkout", "-q", "-b", "work"]);
        world
    }

    fn root(&self) -> PathBuf {
        self.base.join("repo")
    }

    fn git(&self, args: &[&str]) {
        let mut all = vec!["-c", "user.name=t", "-c", "user.email=t@localhost"];
        all.extend(["-c", "core.hooksPath=none", "-c", "commit.gpgsign=false"]);
        all.extend(args);
        git(&self.root(), &all).unwrap();
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.root().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    fn session(&self, name: &str, now: u64) -> Registry {
        Registry {
            dir: self.base.join("registry"),
            here: Checkout {
                root: self.root(),
                session: name.into(),
                now,
            },
        }
    }

    fn folder(&self, id: &str) -> PathBuf {
        self.base.join("registry").join(id)
    }

    /// Fills the sections of a file, as a session would.
    fn fill(&self, id: &str, name: &str, sections: &[&str], text: &str) {
        let body: String = sections
            .iter()
            .map(|s| format!("## {s}\n\n{text}\n\n"))
            .collect();
        std::fs::write(self.folder(id).join(name), body).unwrap();
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // Git's objects are read-only; what stays is in the temp folder.
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

const T: u64 = 1_800_000_000;

fn claim(id: &str, files: &[&str]) -> Problem {
    Problem::Claim {
        id: id.into(),
        files: files.iter().map(|f| (*f).to_owned()).collect(),
        accept_overlap: None,
        take_over: false,
        owner: None,
    }
}

fn take_over(id: &str, owner: Option<&str>) -> Problem {
    Problem::Claim {
        id: id.into(),
        files: vec![],
        accept_overlap: None,
        take_over: true,
        owner: owner.map(str::to_owned),
    }
}

fn error(registry: &Registry, command: &Problem) -> String {
    format!("{:#}", registry.apply(command).unwrap_err())
}

fn accepting(id: &str, file: &str, why: &str) -> Problem {
    Problem::Claim {
        id: id.into(),
        files: vec![file.into()],
        accept_overlap: Some(why.into()),
        take_over: false,
        owner: None,
    }
}

fn new(registry: &Registry, id: &str) {
    registry.apply(&Problem::New { id: id.into() }).unwrap();
}

#[test]
fn one_session_one_problem_and_one_holder() {
    let world = World::new("claim");
    let (a, b) = (world.session("aaa", T), world.session("bbb", T + 60));
    new(&a, "first");
    new(&a, "second");
    a.apply(&claim("first", &[])).unwrap();
    assert!(error(&b, &claim("first", &[])).contains("claimed by session aaa"));
    assert!(error(&a, &claim("second", &[])).contains("one problem per session"));
    let log = Problem::Log {
        id: "first".into(),
        text: "x".into(),
    };
    assert!(error(&b, &log).contains("does not hold"));
    a.apply(&claim("first", &["xtask"])).unwrap();
    assert_eq!(a.state("first").unwrap().files, ["xtask"]);
}

#[test]
fn a_dead_sessions_claim_is_taken_over_and_a_live_one_is_not() {
    let world = World::new("stale");
    let a = world.session("aaa", T);
    new(&a, "first");
    a.apply(&claim("first", &["xtask"])).unwrap();
    let soon = world.session("bbb", T + STALE);
    assert!(error(&soon, &take_over("first", None)).contains("not stale"));
    let late = world.session("bbb", T + STALE + 1);
    assert!(error(&late, &claim("first", &[])).contains("--take-over"));
    late.apply(&take_over("first", None)).unwrap();
    let state = late.state("first").unwrap();
    assert_eq!((state.session.as_str(), state.files.len()), ("bbb", 1));
    assert!(
        late.file("first", "log.md")
            .unwrap()
            .contains("taken over from session aaa")
    );
    // The old session, should it come back, is told to stop.
    let back = world.session("aaa", T + STALE + 2);
    assert!(error(&back, &Problem::Check { id: "first".into() }).contains("does not hold"));
}

#[test]
fn a_vanished_worktree_is_stale_at_once_but_a_blocked_claim_needs_the_owner() {
    let world = World::new("blocked");
    let a = world.session("aaa", T);
    new(&a, "first");
    a.apply(&claim("first", &[])).unwrap();
    a.apply(&Problem::Block {
        id: "first".into(),
        question: "q?".into(),
    })
    .unwrap();
    let late = world.session("bbb", T + 100 * STALE);
    assert!(error(&late, &take_over("first", None)).contains("only the owner"));
    late.apply(&take_over("first", Some("yes"))).unwrap();
    assert_eq!(late.state("first").unwrap().status, Status::Blocked);

    let mut state = late.state("first").unwrap();
    state.status = Status::Active;
    state.worktree = world.base.join("gone").display().to_string();
    late.save("first", &state).unwrap();
    world
        .session("ccc", T + 100 * STALE)
        .apply(&take_over("first", None))
        .unwrap();
}

#[test]
fn only_the_owner_lets_a_problem_go_and_release_frees_it() {
    let world = World::new("release");
    let (a, b) = (world.session("aaa", T), world.session("bbb", T));
    new(&a, "first");
    a.apply(&claim("first", &[])).unwrap();
    let release = Problem::Release {
        id: "first".into(),
        reason: "r".into(),
        owner: "go".into(),
    };
    a.apply(&release).unwrap();
    assert!(
        a.file("first", "log.md")
            .unwrap()
            .contains("owner allowed the release: go")
    );
    b.apply(&claim("first", &[])).unwrap();
    a.apply(&release).unwrap();
    assert_eq!(a.state("first").unwrap().session, "");
    assert!(error(&a, &release).contains("not claimed"));
}

#[test]
fn overlapping_files_are_refused() {
    let world = World::new("overlap");
    let (a, b) = (world.session("aaa", T), world.session("bbb", T));
    new(&a, "first");
    new(&a, "second");
    a.apply(&claim("first", &["crates/victory-gpu/src"]))
        .unwrap();
    let wanted = claim("second", &["crates\\victory-gpu\\src\\Host.rs"]);
    assert!(error(&b, &wanted).contains("overlap"));
    assert_eq!(b.state("second").unwrap().session, "");
    b.apply(&claim("second", &["crates/victory-gpu/src2"]))
        .unwrap();
    let host = "crates/victory-gpu/src/host.rs";
    b.apply(&accepting("second", host, "only the log line"))
        .unwrap();
    // Accepted by one of the two is accepted for both.
    let state = a.state("first").unwrap();
    assert!(
        a.overlaps("first", &state, &state.files)
            .unwrap()
            .is_empty()
    );

    // It ends when the other problem changes hands.
    let release = Problem::Release {
        id: "first".into(),
        reason: "r".into(),
        owner: "go".into(),
    };
    a.apply(&release).unwrap();
    let d = world.session("ddd", T);
    assert!(error(&d, &claim("first", &["crates/victory-gpu/src"])).contains("overlap"));
    a.apply(&claim("first", &[])).unwrap();
    a.apply(&claim("first", &["crates/victory-gpu/src"]))
        .unwrap();

    // It does not cover a third claim.
    let c = world.session("ccc", T);
    new(&c, "third");
    c.apply(&claim("third", &["xtask"])).unwrap();
    assert!(error(&b, &claim("second", &["xtask/src"])).contains("meets xtask of third"));

    for bad in ["../x", "C:/x", "a/*", "/x"] {
        assert!(normal(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_damaged_state_file_is_named_not_guessed() {
    let world = World::new("damaged");
    let a = world.session("aaa", T);
    new(&a, "first");
    for damaged in [
        "",
        "status=active\nsession=x\n",
        "status=???\nend=\n",
        "junk",
    ] {
        std::fs::write(world.folder("first").join("state.txt"), damaged).unwrap();
        assert!(
            error(&a, &claim("first", &[])).contains("is damaged"),
            "{damaged:?}"
        );
        assert!(a.apply(&Problem::List).unwrap().contains("DAMAGED"));
    }
    assert!(a.apply(&Problem::New { id: "../x".into() }).is_err());
}

#[test]
fn a_state_file_keeps_every_field() {
    let state = State {
        status: Status::Proven,
        session: "s".into(),
        worktree: "C:\\w t".into(),
        branch: "b".into(),
        heartbeat: 7,
        files: vec!["a".into(), "b/c".into()],
        accepted: vec!["other".into()],
        proven_commit: "abc".into(),
        proven_by: "s".into(),
    };
    assert_eq!(State::read(&state.write()).unwrap(), state);
}

#[test]
fn fixed_takes_the_agreed_test_every_sub_cause_the_proof_the_merge_and_another_session() {
    let world = World::new("fixed");
    let a = world.session("aaa", T);
    new(&a, "first");
    a.apply(&claim("first", &["src"])).unwrap();
    let agree = Problem::Agree {
        id: "first".into(),
        owner: "agreed".into(),
    };
    assert!(error(&a, &agree).contains("not filled"));
    world.fill("first", "problem.md", DEFINITION, "ten frames show it");
    let proven = Problem::Proven { id: "first".into() };
    assert!(error(&a, &proven).contains("not agreed"));
    a.apply(&agree).unwrap();

    // The session cannot make the test easier on its own, in place, by adding
    // after it, or by narrowing the scope.
    let agreed_text = std::fs::read_to_string(world.folder("first").join("problem.md")).unwrap();
    for weaker in [
        agreed_text.replace("## Done when\n\nten", "## Done when\n\none"),
        format!("{agreed_text}## Amendment\n\none frame is enough\n"),
        agreed_text.replace("## Scope\n\nten frames show it", "## Scope\n\nhalf of it"),
        // A heading spelled otherwise, or a section the agreement would miss.
        agreed_text.replace("## Done when\n\nten", "##  Done when\n\none"),
        agreed_text.replace("## Done when", "## Exceptions\n\nhalf\n\n## Done when"),
    ] {
        std::fs::write(world.folder("first").join("problem.md"), weaker).unwrap();
        assert!(error(&a, &proven).contains("differs from what the owner agreed"));
        assert!(
            a.apply(&Problem::Resume { id: "first".into() })
                .unwrap()
                .contains("CHANGED")
        );
    }
    std::fs::write(world.folder("first").join("problem.md"), &agreed_text).unwrap();
    assert!(!error(&a, &proven).contains("differs"));

    world.write("src/fix.rs", "fixed\n");
    world.write("other.rs", "not claimed\n");
    let refused = error(&a, &proven);
    for wanted in [
        "no sub-cause",
        "proof.md: \"Before\"",
        "uncommitted",
        "other.rs: changed",
    ] {
        assert!(refused.contains(wanted), "{wanted}: {refused}");
    }
    std::fs::remove_file(world.root().join("other.rs")).unwrap();
    world.commit("fix");
    world.fill("first", "proof.md", PROOF_SECTIONS, "shown");
    let notes = NOTES.replace("## Hypotheses", "- [x] one\n- [ ] two\n\n## Hypotheses");
    std::fs::write(world.folder("first").join("notes.md"), &notes).unwrap();
    assert!(error(&a, &proven).contains("sub-cause open: - [ ] two"));
    for hidden in [
        "* [ ] two",
        "1. [ ] two",
        "two is still open",
        "  - [ ] two",
    ] {
        let text = notes.replace("- [ ] two", hidden);
        std::fs::write(world.folder("first").join("notes.md"), text).unwrap();
        assert!(error(&a, &proven).contains("two"), "{hidden}");
    }
    std::fs::write(
        world.folder("first").join("notes.md"),
        notes.replace("[ ]", "[x]"),
    )
    .unwrap();
    a.apply(&proven).unwrap();
    assert!(error(&a, &agree).contains("is proven"));

    let confirm = Problem::Confirm {
        id: "first".into(),
        evidence: "ran".into(),
        owner: None,
    };
    assert!(error(&a, &confirm).contains("does not confirm"));
    let b = world.session("bbb", T);
    assert!(error(&b, &confirm).contains("is not in main"));

    // A test changed after the proof is not what was proven.
    let path = world.folder("first").join("problem.md");
    std::fs::write(
        &path,
        agreed_text.replace("## Done when\n\nten", "## Done when\n\none"),
    )
    .unwrap();
    assert!(error(&b, &confirm).contains("changed since it was proven"));
    std::fs::write(&path, &agreed_text).unwrap();

    // A proof found wanting goes back to its session, never to nobody.
    let reopen = Problem::Reopen {
        id: "first".into(),
        evidence: "fails on main".into(),
    };
    b.apply(&reopen).unwrap();
    let state = b.state("first").unwrap();
    assert_eq!(
        (state.status, state.session.as_str()),
        (Status::Active, "aaa")
    );
    assert!(error(&b, &claim("first", &[])).contains("claimed by session aaa"));
    world.fill(
        "first",
        "proof.md",
        PROOF_SECTIONS,
        "shown again; see TODO.md",
    );
    a.apply(&proven).unwrap();
    world.git(&["checkout", "-q", "main"]);
    world.git(&["merge", "-q", "--no-ff", "-m", "merge", "work"]);
    b.apply(&confirm).unwrap();
    assert!(error(&b, &claim("first", &[])).contains("is fixed"));

    b.apply(&Problem::Reopen {
        id: "first".into(),
        evidence: "back in run 9".into(),
    })
    .unwrap();
    assert_eq!(b.state("first").unwrap().branch, "work");
    b.apply(&claim("first", &[])).unwrap();
    assert!(b.file("first", "proof.md").unwrap().contains("TODO"));
    assert!(b.file("first", "agreed.md").unwrap().contains("ten frames"));
}

#[test]
fn nothing_is_proven_without_a_change_main_lacks() {
    let world = World::new("empty");
    let a = world.session("aaa", T);
    new(&a, "first");
    a.apply(&claim("first", &["src"])).unwrap();
    let proven = Problem::Proven { id: "first".into() };
    assert!(error(&a, &proven).contains("changes nothing that main lacks"));
    world.git(&["checkout", "-q", "main"]);
    assert!(error(&a, &proven).contains("on its own branch"));
}

#[test]
fn notes_stay_short_and_whole() {
    let world = World::new("notes");
    let a = world.session("aaa", T);
    new(&a, "first");
    a.apply(&claim("first", &[])).unwrap();
    let long = NOTES.replace("## Next step", &"x\n".repeat(NOTE_LINES));
    std::fs::write(world.folder("first").join("notes.md"), long).unwrap();
    let refused = error(&a, &Problem::Check { id: "first".into() });
    assert!(refused.contains("at most 150"), "{refused}");
    assert!(refused.contains("no \"## Next step\""), "{refused}");
}

#[test]
fn the_tracked_list_ranks_and_dates_read() {
    let rows = ranked("| rank | id | owner |\n|---|---|---|\n| 3 | `x-y` | here | z |\ntext\n");
    assert_eq!(
        (rows.len(), rows[0].rank, rows[0].id.as_str()),
        (1, 3, "x-y")
    );
    assert_eq!(date(0), "1970-01-01 00:00Z");
    assert_eq!(date(951_782_400 + 3_660), "2000-02-29 01:01Z");
}

#[test]
fn the_repository_list_is_ranked_once_each_with_valid_ids() {
    let text = std::fs::read_to_string(crate::workspace_root().join("docs/problems.md")).unwrap();
    let rows = ranked(&text);
    assert!(!rows.is_empty());
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.rank as usize, i + 1, "{}", row.id);
        valid_id(&row.id).unwrap();
        assert!(
            !rows[..i].iter().any(|r| r.id == row.id),
            "{} twice",
            row.id
        );
    }
}

#[test]
fn the_templates_have_every_section_and_start_unfilled() {
    assert_eq!(unfilled(PROBLEM, DEFINITION).len(), DEFINITION.len());
    assert_eq!(unfilled(PROOF, PROOF_SECTIONS).len(), PROOF_SECTIONS.len());
    for name in NOTE_SECTIONS {
        assert!(section(NOTES, name).is_some(), "{name}");
    }
    assert_eq!(section(NOTES, "Sub-causes").unwrap(), "");
}
