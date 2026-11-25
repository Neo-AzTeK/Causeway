//! The comment rules a machine can hold (CONTRIBUTING.md, "Comments"), run
//! as part of `cargo xtask policy` and so by the workspace tests:
//!
//! - a comment line `// invariant(<test>): why` must name a test that exists,
//!   so the proof of a must-hold rule cannot be deleted or renamed unnoticed;
//! - comments citing a numbered decision point outside the repository, so
//!   their count may only go down.
//!
//! shortcut: a named test is only checked to exist, not to prove the claim;
//! that is the reviewer's part (`.claude/agents/reviewer.md`).

use std::collections::HashSet;

/// References to numbered decisions still in comments. Lower it when one goes.
pub const DECISION_REFS: usize = 36;

/// What is wrong in `sources`, the repository's Rust files as (path, text),
/// expecting `decision_refs` references to numbered decisions.
pub fn check(sources: &[(String, String)], decision_refs: usize) -> Vec<String> {
    let tests: HashSet<&str> = sources.iter().flat_map(|(_, text)| tests(text)).collect();
    let mut problems = Vec::new();
    let mut found = 0;
    for (path, text) in sources {
        for (number, line) in text.lines().enumerate() {
            let line = line.trim_start();
            if !line.starts_with("//") {
                continue;
            }
            found += decisions(line);
            // Only plain comments mark; doc comments may describe the marker.
            let Some(marker) = line.strip_prefix("// invariant") else {
                continue;
            };
            let at = format!("{path}:{}", number + 1);
            match marker.strip_prefix('(').and_then(|m| m.split_once("):")) {
                None => problems.push(format!("{at}: write `// invariant(<test>): why`")),
                Some((name, _)) if !tests.contains(name) => {
                    problems.push(format!("{at}: invariant names no test `{name}`"));
                }
                Some(_) => {}
            }
        }
    }
    if found > decision_refs {
        problems.push(format!(
            "{found} comment references to numbered decisions, {decision_refs} allowed: those \
             files are not in the repository, so say the reason in the comment"
        ));
    } else if found < decision_refs {
        problems.push(format!(
            "only {found} references to numbered decisions left: lower DECISION_REFS in \
             xtask/src/comments.rs to {found}"
        ));
    }
    problems
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
            let digits = line[at + word.len()..].bytes().take_while(u8::is_ascii_digit);
            digits.count() == 4
        })
        .count()
}

#[cfg(test)]
mod tests_of_the_check {
    use super::check;

    fn run(text: &str, refs: usize) -> Vec<String> {
        check(&[("a.rs".to_owned(), text.to_owned())], refs)
    }

    #[test]
    fn an_invariant_must_name_an_existing_test() {
        let held = "// invariant(holds): why\n#[test]\n#[cfg(windows)]\nfn holds() {}\n";
        assert!(run(held, 0).is_empty());
        let gone = "    // invariant(holds): why\nfn holds() {}\n";
        assert_eq!(run(gone, 0).len(), 1, "a function that is no test proves nothing");
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
