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

/// The files of the D3D12 test host. `victory-gpu` depends on their crate, so
/// they cannot read `Caps`; only here may [`TEST_HOST_MARKER`] excuse a call.
/// Not all of it is test code: the title builds root signatures through it.
const TEST_HOST_FILES: [&str; 2] =
    ["crates/victory-shader/src/emit/host.rs", "crates/victory-shader/src/emit/mod.rs"];

/// Excuses the one query call within [`MARKER_REACH`] lines below it.
const TEST_HOST_MARKER: &str = "// test-host-query:";
const MARKER_REACH: usize = 6;

const FIX: &str = "read a `Caps` field or a planner; for one format use \
                   `caps::per_resource` (docs/hardware-range.md)";

const TEST_HOST_FIX: &str = "in the test host, put `// test-host-query: why` above \
                             the call (docs/hardware-range.md)";

/// What is wrong in `sources`, the repository's Rust files as (path, text).
pub fn check(sources: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, text) in sources {
        if path.starts_with(CHECKER_DIR) || path.starts_with(CAPS_DIR) {
            continue;
        }
        let unmarked = unmarked(path, text, &mut problems);
        let fix = if TEST_HOST_FILES.contains(&path.as_str()) { TEST_HOST_FIX } else { FIX };
        problems.extend(
            unmarked
                .iter()
                .map(|line| format!("{path}:{line}: the host GPU is asked outside `caps/`: {fix}")),
        );
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
            if !TEST_HOST_FILES.contains(&path) {
                problems.push(format!(
                    "{path}:{number}: only the test host ({TEST_HOST_FILES:?}) may be excused: {FIX}"
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
        let code = before_comment(line);
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

/// `line` up to its comment: a `//` inside a string literal is not one.
// shortcut: reads one line; a `'"'` before a call on that line could hide it.
// invariant(a_slash_pair_in_a_string_hides_no_query): a URL or path in a string
// must not excuse the code after it.
fn before_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let (mut at, mut quoted) = (0, false);
    while at < bytes.len() {
        match bytes[at] {
            b'\\' if quoted => at += 1,
            b'"' => quoted = !quoted,
            b'/' if !quoted && bytes.get(at + 1) == Some(&b'/') => return &line[..at],
            _ => {}
        }
        at += 1;
    }
    line
}

#[cfg(test)]
mod tests {
    use super::{CAPS_DIR, MARKER_REACH, QUERY_CALLS, TEST_HOST_FILES, TEST_HOST_MARKER, check};

    const GPU: &str = "crates/victory-gpu/src/host.rs";

    fn run(path: &str, text: &str) -> Vec<String> {
        check(&[(path.to_owned(), text.to_owned())])
    }

    fn call() -> String {
        format!("    device.{}(feature, data, size)\n", QUERY_CALLS[0])
    }

    fn host_file() -> String {
        TEST_HOST_FILES[0].to_owned()
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
    fn a_slash_pair_in_a_string_hides_no_query() {
        let line = format!("let p = \"a//b\\\"\"; {}", call());
        assert_eq!(run(GPU, &line).len(), 1);
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
}
