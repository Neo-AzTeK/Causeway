//! What else on the machine would spoil a run: a title or GPU test already on
//! the GPU, and builds or tests that take its CPU (a run timed beside them has
//! given wrong numbers).
// shortcut: judged by process name, so other load (a browser, a game, an
// indexer) is not seen.

use std::collections::{BTreeMap, BTreeSet};

pub struct Process {
    pub id: u32,
    pub parent: u32,
    pub name: String,
}

/// Compilers and linkers. Not `cargo` itself: alone it takes next to nothing,
/// and every `cargo xtask` has one.
const BUILDERS: &[&str] = &[
    "rustc.exe",
    "rustdoc.exe",
    "clippy-driver.exe",
    "link.exe",
    "lld-link.exe",
    "rust-lld.exe",
    "cl.exe",
];

/// A title, the testbed, or any cargo test: several crates test on the real
/// GPU, and an integration test's binary is named after its file alone.
fn uses_gpu(name: &str) -> bool {
    ["victory.exe", "victory-testbed.exe", "testbed.exe"].contains(&name)
        || test_binary(name).is_some()
}

/// The name of a test binary as cargo makes it: `<name>-<16 hex>.exe`.
fn test_binary(name: &str) -> Option<&str> {
    let (test, hash) = name.strip_suffix(".exe")?.rsplit_once('-')?;
    (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(test)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Others {
    pub gpu: BTreeSet<String>,
    pub busy: BTreeSet<String>,
}

/// What runs that is not this process, what started it, what it started, or
/// the title (`title`: the ids in its job, which keeps one that relaunched).
// shortcut: parents are ids, which are reused: a process left by a dead parent
// whose id this process now has would pass for its own.
pub fn others(me: u32, title: &[u32], processes: &[Process]) -> Others {
    let mut started = BTreeSet::from([me]);
    // Until no more are found: the list is in no order.
    loop {
        let before = started.len();
        for p in processes {
            if started.contains(&p.parent) {
                started.insert(p.id);
            }
        }
        if started.len() == before {
            break;
        }
    }
    let parents: BTreeMap<_, _> = processes.iter().map(|p| (p.id, p.parent)).collect();
    let mut above = BTreeSet::new();
    let mut at = me;
    // A reused id can close a loop; `insert` ends it.
    while let Some(&parent) = parents.get(&at) {
        if !above.insert(parent) {
            break;
        }
        at = parent;
    }
    let mut found = Others::default();
    for p in processes {
        if started.contains(&p.id) || above.contains(&p.id) || title.contains(&p.id) {
            continue;
        }
        let name = p.name.to_ascii_lowercase();
        if uses_gpu(&name) {
            found.gpu.insert(name.clone());
        }
        if uses_gpu(&name) || BUILDERS.contains(&name.as_str()) {
            found.busy.insert(name);
        }
    }
    found
}

/// Every process on the machine.
#[cfg(windows)]
pub fn processes() -> anyhow::Result<Vec<Process>> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    // SAFETY: plain call; the result is checked.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    anyhow::ensure!(
        snapshot != INVALID_HANDLE_VALUE,
        std::io::Error::last_os_error()
    );
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut all = Vec::new();
    // SAFETY: a live snapshot and an entry with its size set.
    let mut more = unsafe { Process32FirstW(snapshot, &raw mut entry) };
    while more != 0 {
        let length = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        all.push(Process {
            id: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            name: String::from_utf16_lossy(&entry.szExeFile[..length]),
        });
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot, &raw mut entry) };
    }
    // SAFETY: the snapshot is ours.
    unsafe { CloseHandle(snapshot) };
    anyhow::ensure!(!all.is_empty(), "the process list is empty");
    Ok(all)
}

// shortcut: titles run on Windows only.
#[cfg(not(windows))]
pub fn processes() -> anyhow::Result<Vec<Process>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(entries: &[(u32, u32, &str)]) -> Vec<Process> {
        entries
            .iter()
            .map(|&(id, parent, name)| Process {
                id,
                parent,
                name: name.into(),
            })
            .collect()
    }

    fn names(set: &BTreeSet<String>) -> Vec<&str> {
        set.iter().map(String::as_str).collect()
    }

    #[test]
    fn only_other_builds_tests_and_titles_count() {
        let all = list(&[
            (5, 1, "cargo.exe"),
            (10, 5, "xtask.exe"),
            (21, 20, "VIctory.exe"),
            (20, 10, "victory.exe"),
            (30, 2, "cargo.exe"),
            (31, 30, "rustc.exe"),
            (32, 30, "victory_shader-0123456789abcdef.exe"),
            (33, 30, "xtask-0123456789abcdef.exe"),
            (34, 30, "fidelityfx-0123456789abcdef.exe"),
            (40, 3, "victory.exe"),
            (50, 3, "explorer.exe"),
            (51, 3, "tool-v2.exe"),
        ]);
        let found = others(10, &[], &all);
        let gpu = [
            "fidelityfx-0123456789abcdef.exe",
            "victory.exe",
            "victory_shader-0123456789abcdef.exe",
            "xtask-0123456789abcdef.exe",
        ];
        assert_eq!(names(&found.gpu), gpu);
        assert_eq!(
            names(&found.busy),
            [gpu[0], "rustc.exe", gpu[1], gpu[2], gpu[3]]
        );
    }

    #[test]
    fn a_relaunched_title_stays_ours_and_a_reused_id_does_not() {
        // The title (20) relaunched as 21 and ended; another build got id 20.
        let all = list(&[
            (10, 1, "xtask.exe"),
            (21, 20, "victory.exe"),
            (20, 3, "cargo.exe"),
            (22, 20, "rustc.exe"),
        ]);
        assert_eq!(names(&others(10, &[21], &all).busy), ["rustc.exe"]);
    }

    #[cfg(windows)]
    #[test]
    fn this_process_is_listed() {
        assert!(
            processes()
                .unwrap()
                .iter()
                .any(|p| p.id == std::process::id())
        );
    }
}
