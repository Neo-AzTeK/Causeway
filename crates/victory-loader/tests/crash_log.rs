#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;

use windows_sys::Win32::Foundation::EXCEPTION_BREAKPOINT;
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
    EXCEPTION_POINTERS, RaiseException,
};

const TEST_LOG: &str = "VICTORY_TEST_CRASH_LOG";

#[test]
fn first_chance_breakpoint_logging() {
    if let Some(log) = std::env::var_os(TEST_LOG) {
        victory_loader::log_crashes_to(PathBuf::from(log));
        victory_loader::note_title_output("synthetic title output");
        victory_loader::report_crashes();

        unsafe extern "system" fn handle_breakpoint(info: *mut EXCEPTION_POINTERS) -> i32 {
            // SAFETY: the system passes valid exception pointers.
            if unsafe { (*(*info).ExceptionRecord).ExceptionCode } == EXCEPTION_BREAKPOINT {
                EXCEPTION_CONTINUE_EXECUTION
            } else {
                EXCEPTION_CONTINUE_SEARCH
            }
        }

        // SAFETY: register after VIctory's vectored handler. This synthetic
        // breakpoint is continuable and the handler needs no extra data.
        unsafe {
            assert!(!AddVectoredExceptionHandler(0, Some(handle_breakpoint)).is_null());
            RaiseException(EXCEPTION_BREAKPOINT as u32, 0, 0, std::ptr::null());
        }
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    for opt_in in [false, true] {
        let log = dir.path().join(format!("breakpoint-{opt_in}.log"));
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "first_chance_breakpoint_logging", "--nocapture"])
            .env(TEST_LOG, &log)
            .env_remove("VICTORY_CRASH_BREAKPOINTS");
        if opt_in {
            child.env("VICTORY_CRASH_BREAKPOINTS", "1");
        }
        let output = child.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{}\n{stderr}", output.status);
        assert!(stderr.contains("first-chance exception 0x80000003"));

        if cfg!(feature = "diagnostics") && opt_in {
            let report = std::fs::read_to_string(&log).unwrap();
            assert!(report.contains("first-chance exception 0x80000003 at "));
            assert!(report.contains("  #0 "));
            assert!(report.contains("  title said: synthetic title output"));
            assert!(!report.contains("unhandled exception"));
        } else {
            assert!(!log.exists());
        }
    }
}

const TEST_ENTRIES: &str = "VICTORY_TEST_CRASH_ENTRIES";

/// Several threads fault at once when a title stops itself, and each must
/// leave its own entry.
#[test]
fn concurrent_entries_do_not_interleave() {
    const THREADS: usize = 16;
    const EACH: usize = 50;
    if let Some(log) = std::env::var_os(TEST_ENTRIES) {
        victory_loader::log_crashes_to(PathBuf::from(log));
        let start = std::sync::Barrier::new(THREADS);
        std::thread::scope(|s| {
            for t in 0..THREADS {
                let start = &start;
                s.spawn(move || {
                    start.wait();
                    for i in 0..EACH {
                        victory_loader::log_crash(&format!("entry {t} {i} end"));
                    }
                });
            }
        });
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("crashes.log");
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "concurrent_entries_do_not_interleave"])
        .env(TEST_ENTRIES, &log)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", output.status);
    let report = std::fs::read_to_string(&log).unwrap();
    let mut lines = report.lines();
    let mut entries = 0;
    while let Some(header) = lines.next() {
        let whole = header.starts_with("unix time ")
            && header.matches("unix time").count() == 1
            && header.matches(", process ").count() == 1;
        assert!(whole, "header: {header:?}");
        let body = lines.next().unwrap_or_default();
        assert!(
            body.starts_with("entry ") && body.ends_with(" end"),
            "after {header:?}: {body:?}"
        );
        // The host line, then the blank line that ends the entry.
        let host = lines.next().unwrap_or_default();
        assert!(host.starts_with("  host: "), "{host:?}");
        assert_eq!(lines.next(), Some(""));
        entries += 1;
    }
    assert_eq!(entries, THREADS * EACH);
}
