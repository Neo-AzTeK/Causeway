//! The hardware-range rule a machine can hold (docs/hardware-range.md), run
//! as part of `cargo xtask policy`: only `victory-gpu/src/caps/` asks the
//! host GPU what it can do. Elsewhere, code reads `Caps` or a planner.
//!
//! shortcut: the query calls are found by name in the source text, so a
//! macro that builds the name would pass.

/// The calls that ask a device or adapter what it supports.
const QUERY_CALLS: [&str; 2] = ["CheckFeatureSupport", "CheckInterfaceSupport"];

/// This tool names the calls and talks to no GPU.
const CHECKER_DIR: &str = "xtask/";

/// Where those calls belong.
const CAPS_DIR: &str = "crates/victory-gpu/src/caps/";

/// The D3D12 test host. `victory-gpu` depends on its crate, so it cannot read
/// `Caps`; only here may [`TEST_HOST_MARKER`] excuse a call.
const TEST_HOST_DIR: &str = "crates/victory-shader/src/emit/";

/// Excuses the one query call within [`MARKER_REACH`] lines below it.
const TEST_HOST_MARKER: &str = "// test-host-query:";
const MARKER_REACH: usize = 6;

/// Descriptor and heap code has another owner. Its unmarked calls are counted
/// here, exactly, so that none is added and the number only goes down.
pub const OTHER_OWNER: &[(&str, usize)] = &[("crates/victory-gpu/src/descriptor/host.rs", 1)];

const FIX: &str = "read a `Caps` field or a planner; for one format use \
                   `caps::per_resource` (docs/hardware-range.md)";

const TEST_HOST_FIX: &str = "in the test host, put `// test-host-query: why` above \
                             the call (docs/hardware-range.md)";

/// What is wrong in `sources`, the repository's Rust files as (path, text),
/// with `other_owner` calls left in files this check may not change.
pub fn check(sources: &[(String, String)], other_owner: &[(&str, usize)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, text) in sources {
        if path.starts_with(CHECKER_DIR) || path.starts_with(CAPS_DIR) {
            continue;
        }
        let unmarked = unmarked(path, text, &mut problems);
        if other_owner.iter().all(|(p, _)| p != path) {
            let fix = if path.starts_with(TEST_HOST_DIR) { TEST_HOST_FIX } else { FIX };
            problems.extend(unmarked.iter().map(|line| {
                format!("{path}:{line}: the host GPU is asked outside `caps/`: {fix}")
            }));
        }
    }
    for &(path, allowed) in other_owner {
        let found = sources
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(p, text)| unmarked(p, text, &mut Vec::new()).len())
            .sum::<usize>();
        if found > allowed {
            problems.push(format!(
                "{path}: {found} queries of the host GPU, {allowed} allowed: {FIX}"
            ));
        } else if found < allowed {
            problems.push(format!(
                "{path}: only {found} queries of the host GPU left: lower its count in \
                 OTHER_OWNER (xtask/src/capability.rs) to {found}"
            ));
        }
    }
    problems
}

/// The line numbers of the query calls in `text` that no marker excuses;
/// misused markers go to `problems`.
fn unmarked(path: &str, text: &str, problems: &mut Vec<String>) -> Vec<usize> {
    let mut unmarked = Vec::new();
    // The line of a marker still looking for its call.
    let mut marker: Option<usize> = None;
    let unused = |at: usize| format!("{path}:{at}: no query below this marker: delete it");
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if let Some(at) = marker.filter(|at| number - at > MARKER_REACH) {
            problems.push(unused(at));
            marker = None;
        }
        let line = line.trim_start();
        if let Some(why) = line.strip_prefix(TEST_HOST_MARKER) {
            if !path.starts_with(TEST_HOST_DIR) {
                problems.push(format!(
                    "{path}:{number}: only the test host ({TEST_HOST_DIR}) may be excused: {FIX}"
                ));
                continue;
            }
            if why.trim().is_empty() {
                problems.push(format!("{path}:{number}: write `{TEST_HOST_MARKER} why`"));
            }
            if let Some(at) = marker.replace(number) {
                problems.push(unused(at));
            }
            continue;
        }
        let code = line.split("//").next().unwrap_or_default();
        let calls: usize = QUERY_CALLS.iter().map(|call| code.matches(call).count()).sum();
        for _ in 0..calls {
            if marker.take().is_none() {
                unmarked.push(number);
            }
        }
    }
    problems.extend(marker.map(unused));
    unmarked
}

#[cfg(test)]
mod tests {
    use super::{CAPS_DIR, MARKER_REACH, QUERY_CALLS, TEST_HOST_DIR, TEST_HOST_MARKER, check};

    const GPU: &str = "crates/victory-gpu/src/host.rs";

    fn run(path: &str, text: &str) -> Vec<String> {
        check(&[(path.to_owned(), text.to_owned())], &[])
    }

    fn call() -> String {
        format!("    device.{}(feature, data, size)\n", QUERY_CALLS[0])
    }

    fn host_file() -> String {
        format!("{TEST_HOST_DIR}host.rs")
    }

    #[test]
    fn a_query_outside_caps_is_rejected_with_its_place() {
        for name in QUERY_CALLS {
            let found = run(GPU, &format!("fn f() {{\n    adapter.{name}(x);\n}}\n"));
            assert_eq!(found.len(), 1, "{name}");
            assert!(found[0].starts_with(&format!("{GPU}:2: ")), "{}", found[0]);
        }
        assert_eq!(run("crates/victory-hle/src/x.rs", &call()).len(), 1);
    }

    #[test]
    fn the_test_host_is_told_about_its_marker() {
        let found = run(&host_file(), &call());
        assert!(found[0].contains(TEST_HOST_MARKER), "{}", found[0]);
        assert!(!run(GPU, &call())[0].contains(TEST_HOST_MARKER));
    }

    #[test]
    fn caps_comments_and_this_tool_may_name_queries() {
        assert!(run(&format!("{CAPS_DIR}probe.rs"), &call()).is_empty());
        assert!(run(GPU, &format!("/// `{}` is asked once.\n", QUERY_CALLS[0])).is_empty());
        assert!(run(GPU, &format!("f(); // not {}\n", QUERY_CALLS[0])).is_empty());
        assert!(run("xtask/src/x.rs", &call()).is_empty());
        for path in ["apps/victory/src/x.rs", "tests/fixtures/x/src/lib.rs"] {
            assert_eq!(run(path, &call()).len(), 1, "{path}");
        }
    }

    #[test]
    fn a_marker_excuses_one_call_of_the_test_host_within_its_reach() {
        let marked = |gap: usize| format!("{TEST_HOST_MARKER} why\n{}{}", "x\n".repeat(gap), call());
        assert!(run(&host_file(), &marked(MARKER_REACH - 1)).is_empty());
        // Too far: the call is unmarked and the marker unused.
        assert_eq!(run(&host_file(), &marked(MARKER_REACH)).len(), 2);
        // One marker, two calls.
        assert_eq!(run(&host_file(), &format!("{}{}", marked(0), call())).len(), 1);
        assert_eq!(run(&host_file(), &format!("{TEST_HOST_MARKER} why\n")).len(), 1);
        assert_eq!(run(&host_file(), &format!("{TEST_HOST_MARKER}\n{}", call())).len(), 1);
    }

    #[test]
    fn no_marker_excuses_code_outside_the_test_host() {
        let found = run(GPU, &format!("{TEST_HOST_MARKER} why\n{}", call()));
        assert_eq!(found.len(), 2, "the marker and the call: {found:?}");
    }

    #[test]
    fn another_owners_calls_are_counted_exactly() {
        let owned = [("crates/a.rs", 1)];
        let found = |text: String| check(&[("crates/a.rs".to_owned(), text)], &owned).len();
        assert_eq!(found(call()), 0);
        assert_eq!(found(call().repeat(2)), 1);
        assert_eq!(found(String::new()), 1);
    }
}
