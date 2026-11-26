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
    /// A `victory.exe` whose command line shows an offline scan
    /// ([`offline_scan`]): it takes CPU but never opens the GPU.
    pub offline: bool,
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

/// A `victory` command line that never makes a GPU device: `census`, or
/// `translate` without `--pipelines`. Anything else, a line that cannot be
/// read among them, may use the GPU.
// shortcut: by the words of the line, so `--pipelines` inside a path counts
// too (waits when it need not).
fn offline_scan(command: &str) -> bool {
    let command = command.trim_start();
    // The program: quoted, or up to the first space.
    let rest = match command.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').map(|(_, rest)| rest),
        None => command.split_once(char::is_whitespace).map(|(_, rest)| rest),
    };
    let mut words = rest.unwrap_or("").split_whitespace();
    match words.next() {
        Some("census") => true,
        // Windows drops quotes inside a word (`--pipe""lines`): so do we.
        Some("translate") => !words.any(|word| word.replace('"', "").contains("--pipelines")),
        _ => false,
    }
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
        if uses_gpu(&name) && !p.offline {
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
        let id = entry.th32ProcessID;
        let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
        // Only the title's binary has offline commands.
        let offline = name.eq_ignore_ascii_case("victory.exe")
            && command_line(id).is_some_and(|line| offline_scan(&line));
        all.push(Process {
            id,
            parent: entry.th32ParentProcessID,
            name,
            offline,
        });
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot, &raw mut entry) };
    }
    // SAFETY: the snapshot is ours.
    unsafe { CloseHandle(snapshot) };
    anyhow::ensure!(!all.is_empty(), "the process list is empty");
    Ok(all)
}

/// The command line of process `id`; `None` if it cannot be read.
#[cfg(windows)]
fn command_line(id: u32) -> Option<String> {
    use windows_sys::Wdk::System::Threading::NtQueryInformationProcess;
    use windows_sys::Win32::Foundation::{CloseHandle, UNICODE_STRING};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    /// `ProcessCommandLineInformation`: a `UNICODE_STRING`, then its text.
    const COMMAND_LINE: i32 = 60;
    // SAFETY: plain call; the result is checked.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, id) };
    if process.is_null() {
        return None;
    }
    // Room for the longest command line (32767 UTF-16 units), aligned.
    let mut buffer = vec![0u64; 8448];
    let size = size_of_val(&buffer[..]) as u32;
    // SAFETY: a live handle and a writable buffer of the size passed.
    let status = unsafe {
        NtQueryInformationProcess(
            process,
            COMMAND_LINE,
            buffer.as_mut_ptr().cast(),
            size,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: the handle is ours.
    unsafe { CloseHandle(process) };
    if status < 0 {
        return None;
    }
    // SAFETY: on success the buffer starts with the string's header.
    let header = unsafe { &*buffer.as_ptr().cast::<UNICODE_STRING>() };
    if header.Buffer.is_null() {
        return None;
    }
    // SAFETY: the text is `Length` bytes inside the buffer, which lives.
    let text = unsafe { std::slice::from_raw_parts(header.Buffer, usize::from(header.Length) / 2) };
    Some(String::from_utf16_lossy(text))
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
                offline: false,
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

    #[test]
    fn only_a_scan_that_makes_no_gpu_device_is_offline() {
        for line in [
            r#""C:\a b\victory.exe" translate C:\files --list"#,
            r"victory.exe translate --jobs 4",
            r"C:\w\victory.exe census",
            r#""victory.exe" translate"#,
        ] {
            assert!(offline_scan(line), "{line}");
        }
        for line in [
            r"victory.exe translate C:\files --pipelines --list",
            r"victory.exe translate --cache --pipelines",
            r#"victory.exe translate "--pipelines""#,
            r#"victory.exe translate --pipe""lines"#,
            r#"victory.exe translate "--"pipelines"#,
            r"victory.exe run C:\title.exe --execute",
            r#"victory.exe run "C:\a translate census\title.exe""#,
            r#""C:\my census dir\victory.exe" run title.exe"#,
            r"victory.exe translated",
            r"victory.exe",
            r#""unclosed translate"#,
            "",
        ] {
            assert!(!offline_scan(line), "{line}");
        }
    }

    #[test]
    fn an_offline_scan_leaves_the_gpu_free_and_the_machine_busy() {
        let mut all = list(&[(10, 1, "xtask.exe"), (40, 3, "victory.exe")]);
        all[1].offline = true;
        let found = others(10, &[], &all);
        assert!(found.gpu.is_empty(), "{found:?}");
        assert_eq!(names(&found.busy), ["victory.exe"]);
        // A title beside it still holds the GPU.
        all.extend(list(&[(41, 3, "victory.exe")]));
        assert_eq!(names(&others(10, &[], &all).gpu), ["victory.exe"]);
    }

    #[cfg(windows)]
    #[test]
    fn a_command_line_is_read() {
        let line = command_line(std::process::id()).expect("our own command line");
        let me = std::env::current_exe().unwrap();
        let name = me.file_stem().unwrap().to_string_lossy();
        assert!(line.contains(&*name), "{line}");
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
