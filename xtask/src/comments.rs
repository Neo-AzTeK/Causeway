//! The comment rules a machine can hold (CONTRIBUTING.md, "Comments"), run
//! as part of `cargo xtask policy` and so by the workspace tests:
//!
//! - a comment line `// invariant(<test>): why` must name a test that exists,
//!   so the proof of a must-hold rule cannot be deleted or renamed unnoticed;
//! - comments that go stale ([`KINDS`]) are counted across the tree, so their
//!   number may only go down: none is added, and each goes when its code is
//!   changed anyway, never in a sweep;
//! - no comment names a shader of the title (rule #1) or splits the work by
//!   owner, session or AI (a split once kept the earliest wrong step unfixed).
//!
//! shortcut: a named test is only checked to exist, not to prove the claim;
//! that is the reviewer's part (`.claude/agents/reviewer.md`). Whether a
//! comment still says what its code does is the reviewer's part too.

use std::collections::HashSet;

/// A kind of comment line that goes stale, with how often it occurs in one.
struct Kind {
    name: &'static str,
    fix: &'static str,
    count: fn(&str) -> usize,
}

/// The kinds counted, in the order of [`LIMITS`].
const KINDS: [Kind; 3] = [
    Kind {
        name: "references to numbered decisions",
        fix: "those files are not in the repository, so say the reason in the comment",
        count: decisions,
    },
    Kind {
        name: "dated comments",
        fix: "a date or a run goes stale: say the rule and why; measured figures go in \
              docs/adapters.md or run notes outside the repository",
        count: dated,
    },
    Kind {
        name: "\"for now\" comments",
        fix: "mark the limit `// shortcut:` and say what it is",
        count: for_now,
    },
];

/// How many of each of [`KINDS`] are left in comments. Lower one when it drops.
pub const LIMITS: [usize; KINDS.len()] = [35, 2, 2];

/// Words that split the work by who does it; descriptor work, say, is ours.
const SPLITS: [&str; 4] = [
    "another owner",
    "other owner",
    "another ai",
    "someone else's",
];

/// The prefixes of the title's shader names (`CS_Name`): such a name is title
/// data, as its figures are.
const SHADER_PREFIXES: [&str; 8] = ["CS_", "PS_", "VS_", "GS_", "HS_", "DS_", "MS_", "AS_"];

/// This check's own file, as `sources` names it.
const THIS_FILE: &str = "xtask/src/comments.rs";

/// What is wrong in `sources`, the repository's Rust files as (path, text),
/// expecting `limits` of each of [`KINDS`].
pub fn check(sources: &[(String, String)], limits: &[usize; KINDS.len()]) -> Vec<String> {
    let tests: HashSet<&str> = sources.iter().flat_map(|(_, text)| tests(text)).collect();
    let mut problems = Vec::new();
    let mut found = [0; KINDS.len()];
    for (path, text) in sources {
        for (number, line) in text.lines().enumerate() {
            let line = line.trim_start();
            if !line.starts_with("//") {
                continue;
            }
            for (kind, found) in KINDS.iter().zip(&mut found) {
                *found += (kind.count)(line);
            }
            let at = format!("{path}:{}", number + 1);
            let lower = line.to_ascii_lowercase();
            // This file names what it rejects.
            let own = path == THIS_FILE;
            if let Some(words) = SPLITS.iter().find(|w| !own && lower.contains(*w)) {
                problems.push(format!(
                    "{at}: \"{words}\": no work is someone else's; say what is to be done"
                ));
            }
            if let Some(name) = shader_name(line).filter(|_| !own) {
                problems.push(format!(
                    "{at}: `{name}` names a shader of the title (rule #1): describe what it does"
                ));
            }
            // Only plain comments mark; doc comments may describe the marker.
            let Some(marker) = line.strip_prefix("// invariant") else {
                continue;
            };
            match marker.strip_prefix('(').and_then(|m| m.split_once("):")) {
                None => problems.push(format!("{at}: write `// invariant(<test>): why`")),
                Some((name, _)) if !tests.contains(name) => {
                    problems.push(format!("{at}: invariant names no test `{name}`"));
                }
                Some(_) => {}
            }
        }
    }
    for ((kind, found), allowed) in KINDS.iter().zip(found).zip(limits) {
        let name = kind.name;
        if found > *allowed {
            problems.push(format!("{found} {name}, {allowed} allowed: {}", kind.fix));
        } else if found < *allowed {
            problems.push(format!(
                "only {found} {name} left: lower its entry in LIMITS (xtask/src/comments.rs) \
                 to {found}"
            ));
        }
    }
    problems
}

/// Dates written `YYYY-MM-DD` in `line`; a timestamp (`...T12:00Z`) is a
/// format's example, not when something was seen.
fn dated(line: &str) -> usize {
    let bytes = line.as_bytes();
    let shape = b"dddd-dd-dd";
    (0..bytes.len().saturating_sub(shape.len() - 1))
        .filter(|&at| {
            let digit = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
            let fits = shape.iter().enumerate().all(|(i, &s)| {
                if s == b'd' {
                    digit(at + i)
                } else {
                    bytes[at + i] == s
                }
            });
            let next = bytes.get(at + shape.len());
            fits && (at == 0 || !digit(at - 1))
                && !next.is_some_and(|b| b.is_ascii_digit() || *b == b'T')
        })
        .count()
}

/// "for now" on a line not marked `shortcut:`.
fn for_now(line: &str) -> usize {
    let lower = line.to_ascii_lowercase();
    usize::from(!lower.contains("shortcut:") && lower.contains("for now"))
}

/// The first title shader name in `line`: a [`SHADER_PREFIXES`] prefix at the
/// start of a word, then a capital and a small letter (`CS_CopyThing`).
fn shader_name(line: &str) -> Option<&str> {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.split(|c: char| !word(c)).find(|w| {
        SHADER_PREFIXES.iter().any(|p| {
            w.strip_prefix(p).is_some_and(|rest| {
                let mut chars = rest.chars();
                chars.next().is_some_and(|c| c.is_ascii_uppercase())
                    && chars.next().is_some_and(|c| c.is_ascii_lowercase())
            })
        })
    })
}

/// The names of the functions under a `#[test]` attribute.
fn tests(text: &str) -> impl Iterator<Item = &str> {
    let mut marked = false;
    text.lines().filter_map(move |line| {
        let line = line.trim();
        if line == "#[test]" {
            marked = true;
            return None;
        }
        // Further attributes and comments may stand between.
        if !marked || line.starts_with('#') || line.starts_with("//") {
            return None;
        }
        marked = false;
        let (_, rest) = line.split_once("fn ")?;
        rest.split(['(', '<']).next()
    })
}

fn decisions(line: &str) -> usize {
    line.match_indices("decision ")
        .filter(|(at, word)| {
            let digits = line[at + word.len()..]
                .bytes()
                .take_while(u8::is_ascii_digit);
            digits.count() == 4
        })
        .count()
}

#[cfg(test)]
mod tests_of_the_check {
    use super::check;

    /// The check of one file, with `refs` numbered decisions and nothing else
    /// counted allowed.
    fn run(text: &str, refs: usize) -> Vec<String> {
        check(&[("a.rs".to_owned(), text.to_owned())], &[refs, 0, 0])
    }

    #[test]
    fn dated_and_for_now_comments_only_go_down() {
        let dated = "// seen in a run (2026-11-24)\n";
        assert_eq!(run(dated, 0).len(), 1);
        assert!(check(&[("a.rs".to_owned(), dated.to_owned())], &[0, 1, 0]).is_empty());
        assert!(run("// 12026-11-245 is no date\nlet d = \"2026-11-24\";\n", 0).is_empty());
        assert_eq!(run("// Refused for now.\n", 0).len(), 1);
        assert!(run("/// A log line's time (`2026-01-31T12:00:00Z`).\n", 0).is_empty());
        assert!(run("// shortcut: refused for now, ...\n", 0).is_empty());
    }

    #[test]
    fn no_comment_splits_the_work_by_owner() {
        assert_eq!(run("/// This code has another owner.\n", 0).len(), 1);
        assert_eq!(run("// Another AI does this.\n", 0).len(), 1);
        assert!(run("/// An address that is not ours.\n", 0).is_empty());
    }

    #[test]
    fn no_comment_names_a_shader_of_the_title() {
        assert_eq!(run("/// (CS_CopyThing) writes it.\n", 0).len(), 1);
        assert_eq!(run("// see PS_Draw.\n", 0).len(), 1);
        for fine in [
            "// VS_OUTPUT, D3D12_CS_Thing.\n",
            "let n = \"CS_CopyThing\";\n",
        ] {
            assert!(run(fine, 0).is_empty(), "{fine}");
        }
    }

    #[test]
    fn an_invariant_must_name_an_existing_test() {
        let held = "// invariant(holds): why\n#[test]\n#[cfg(windows)]\nfn holds() {}\n";
        assert!(run(held, 0).is_empty());
        let gone = "    // invariant(holds): why\nfn holds() {}\n";
        assert_eq!(
            run(gone, 0).len(),
            1,
            "a function that is no test proves nothing"
        );
        assert_eq!(run("// invariant: why\n", 0).len(), 1);
        assert!(run("//! Write `// invariant(x): why`.\n", 0).is_empty());
    }

    #[test]
    fn references_to_numbered_decisions_only_go_down() {
        let one = format!("/// See {} 0042.\n", "decision");
        assert!(run(&one, 1).is_empty());
        assert_eq!(run(&one, 0).len(), 1);
        assert_eq!(run("", 1).len(), 1);
    }
}
