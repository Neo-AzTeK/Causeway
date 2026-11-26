//! What else the machine does around a smoke run. A run measured while other
//! things compile reads slower, and its pipelines are still being made or are
//! refused when it draws. So a run waits for a quiet machine before it starts
//! the title, records what processes other than the title use while it plays,
//! and is marked busy when that was much over the lines measured.
//!
//! "Other" is the system's busy time less the title's own (its job's), so a
//! title that fills the processor by itself is not "busy".

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::report::{Area, Status};

/// The record beside a run's `log.txt`.
pub const FILE: &str = "load.txt";
/// The machine is quiet under this percent of all cores, with no compiler
/// running, for `QUIET_FOR`.
const QUIET_CPU: f64 = 30.0;
const QUIET_FOR: Duration = Duration::from_secs(20);
/// A look is busy when processes other than the title use more than this
/// percent of all cores; a run is when more than `BUSY_PART` of its looks are,
/// where the medians and spreads that `compare` reads start to move.
// shortcut: both are estimates; each run records its looks, to tune them.
const BUSY_OTHER: f64 = 20.0;
const BUSY_PART: f64 = 0.25;
/// Processes that mean a build is going on. Not cargo: the one that runs this
/// run, and the queue runner's, wait for it.
const COMPILERS: &[&str] = &["rustc.exe", "clippy-driver.exe", "link.exe", "rust-lld.exe", "cl.exe"];

/// Processor time since boot over all cores, in 100 ns.
#[derive(Clone, Copy, Debug)]
pub struct Times {
    pub busy: u64,
    pub all: u64,
}

#[cfg(windows)]
pub fn system_times() -> Option<Times> {
    use windows_sys::Win32::Foundation::FILETIME;
    let mut times = [FILETIME::default(); 3];
    let [idle, kernel, user] = &mut times;
    // SAFETY: three structures the call fills.
    let read = unsafe { windows_sys::Win32::System::Threading::GetSystemTimes(idle, kernel, user) };
    if read == 0 {
        return None;
    }
    let ticks = |t: &FILETIME| u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime);
    let [idle, kernel, user] = times.each_ref().map(ticks);
    // The kernel's time includes the idle time.
    let all = kernel + user;
    Some(Times { busy: all.saturating_sub(idle), all })
}

#[cfg(not(windows))]
pub fn system_times() -> Option<Times> {
    None
}

/// How many compiler and linker processes run.
#[cfg(windows)]
fn compilers_running() -> usize {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    // SAFETY: a snapshot of every process; checked below, closed at the end.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return 0;
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut found = 0;
    // SAFETY: a live snapshot and an entry that says its size.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    while more != 0 {
        let length = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
        found += usize::from(is_compiler(&name));
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    // SAFETY: the snapshot is ours.
    unsafe { CloseHandle(snapshot) };
    found
}

#[cfg(not(windows))]
fn compilers_running() -> usize {
    0
}

fn is_compiler(name: &str) -> bool {
    COMPILERS.iter().any(|c| name.eq_ignore_ascii_case(c))
}

/// One look while the title played: percent of all cores since the look
/// before.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Unix time, as the log's lines.
    pub at: f64,
    pub total: f64,
    pub other: f64,
}

/// Turns successive counters into samples.
#[derive(Default)]
pub struct Meter {
    last: Option<(Times, u64)>,
}

impl Meter {
    /// `title`: the processor time the title's processes have had, in 100 ns.
    /// Nothing from the first look.
    pub fn sample(&mut self, at: f64, now: Times, title: u64) -> Option<Sample> {
        let (before, titled) = self.last.replace((now, title))?;
        let all = now.all.checked_sub(before.all).filter(|&all| all > 0)?;
        let busy = now.busy.saturating_sub(before.busy);
        let other = busy.saturating_sub(title.saturating_sub(titled));
        let percent = |part: u64| (part as f64 * 100.0 / all as f64).min(100.0);
        Some(Sample { at, total: percent(busy), other: percent(other) })
    }
}

/// What waiting for a quiet machine came to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wait {
    pub seconds: u64,
    /// Else the wait ended at its cap.
    pub quiet: bool,
}

/// Waits until `look` (the time waited so far, the processor's percent since
/// the look before, the compilers running) has been quiet for `QUIET_FOR`, or
/// `cap` has passed. `look` does the pausing.
fn wait_for(cap: Duration, mut look: impl FnMut() -> (Duration, f64, usize)) -> Wait {
    let mut since = None;
    loop {
        let (waited, cpu, compilers) = look();
        if cpu >= QUIET_CPU || compilers > 0 {
            since = None;
        } else if waited - *since.get_or_insert(waited) >= QUIET_FOR {
            return Wait { seconds: waited.as_secs(), quiet: true };
        }
        if waited >= cap {
            return Wait { seconds: waited.as_secs(), quiet: false };
        }
    }
}

/// `wait_for` on this machine, looked at every `every`. `None`: the machine
/// does not say how busy it is.
pub fn wait_quiet(cap: Duration, every: Duration) -> Option<Wait> {
    let mut last = system_times()?;
    let start = Instant::now();
    let mut said = Instant::now();
    Some(wait_for(cap, move || {
        std::thread::sleep(every);
        let now = system_times().unwrap_or(last);
        let all = now.all.saturating_sub(last.all).max(1);
        let cpu = now.busy.saturating_sub(last.busy) as f64 * 100.0 / all as f64;
        last = now;
        let compilers = compilers_running();
        if said.elapsed() >= Duration::from_secs(30) {
            said = Instant::now();
            println!(
                "waiting for a quiet machine: {cpu:.0}% of the CPU, {compilers} compilers running"
            );
        }
        (start.elapsed(), cpu, compilers)
    }))
}

/// A run's `load.txt`: the wait before the title, then its looks.
#[derive(Debug, Default, PartialEq)]
pub struct Record {
    /// `None`: the run did not wait (`--quiet-wait 0`).
    pub wait: Option<Wait>,
    pub samples: Vec<Sample>,
}

impl Record {
    /// The first lines of the file; a `line` for each look follows.
    pub fn header(wait: Option<Wait>) -> String {
        match wait {
            Some(w) => format!("wait_s {}\nquiet {}\n", w.seconds, if w.quiet { "yes" } else { "no" }),
            None => "wait_s none\n".into(),
        }
    }

    pub fn line(s: Sample) -> String {
        format!("look {:.1} {:.1} {:.1}\n", s.at, s.total, s.other)
    }

    pub fn parse(text: &str) -> Record {
        let get = |key: &str| text.lines().find_map(|l| l.strip_prefix(key));
        let seconds = get("wait_s ").and_then(|v| v.parse().ok());
        let wait = seconds.map(|seconds| Wait { seconds, quiet: get("quiet ") == Some("yes") });
        let samples = text.lines().filter_map(|l| {
            let mut fields = l.strip_prefix("look ")?.split(' ').map(|v| v.parse::<f64>().ok());
            Some(Sample { at: fields.next()??, total: fields.next()??, other: fields.next()?? })
        });
        Record { wait, samples: samples.collect() }
    }

    /// The record beside a log, if the run made one.
    pub fn beside(log: &Path) -> Option<Record> {
        std::fs::read_to_string(log.with_file_name(FILE)).ok().map(|text| Record::parse(&text))
    }

    /// The looks inside `window` (Unix times; all of them without one).
    /// `None` when there are none.
    pub fn summary(&self, window: Option<(f64, f64)>) -> Option<Summary> {
        let inside: Vec<&Sample> = self
            .samples
            .iter()
            .filter(|s| window.is_none_or(|(from, to)| s.at >= from && s.at <= to))
            .collect();
        let looks = inside.len();
        (looks > 0).then(|| {
            let mean = |by: fn(&Sample) -> f64| inside.iter().map(|s| by(s)).sum::<f64>() / looks as f64;
            Summary {
                looks,
                total: mean(|s| s.total),
                other: mean(|s| s.other),
                peak: inside.iter().map(|s| s.other).fold(0.0, f64::max),
                over: inside.iter().filter(|s| s.other > BUSY_OTHER).count(),
            }
        })
    }

    /// The report's "machine load" area, over the lines measured: a warning
    /// when the machine was busy, for the run's numbers then say little.
    pub fn area(&self, window: Option<(f64, f64)>) -> Area {
        let waited = match self.wait {
            Some(w) if w.quiet => format!("waited {} s for a quiet machine", w.seconds),
            Some(w) => format!("never quiet in {} s, started anyway", w.seconds),
            None => "did not wait for a quiet machine".into(),
        };
        let over = if window.is_some() { "the lines measured" } else { "the run" };
        let (status, detail) = match self.summary(window) {
            None => (Status::Unknown, format!("{waited}; no look at the load during {over}")),
            Some(s) if s.busy() => {
                (Status::Warn, format!("MACHINE BUSY over {over}: {}; {waited}", s.text()))
            }
            Some(s) => (Status::Pass, format!("over {over}: {}; {waited}", s.text())),
        };
        Area { name: "machine load", status, detail }
    }
}

/// The looks of a stretch of a run.
#[derive(Clone, Copy, Debug)]
pub struct Summary {
    looks: usize,
    /// Mean percent of all cores: everything, and all but the title.
    total: f64,
    other: f64,
    peak: f64,
    /// Looks over `BUSY_OTHER`.
    over: usize,
}

impl Summary {
    pub fn busy(&self) -> bool {
        self.over as f64 > BUSY_PART * self.looks as f64
    }

    pub fn text(&self) -> String {
        let mut text = format!("other processes {:.0}% of the CPU", self.other);
        let _ = write!(
            text,
            " (peak {:.0}%, {} of {} looks over {BUSY_OTHER:.0}%), {:.0}% in all",
            self.peak, self.over, self.looks, self.total
        );
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// A wait over looks every 2 s, each `(cpu, compilers)` from `at`.
    fn waited(cap: u64, at: impl Fn(u64) -> (f64, usize)) -> Wait {
        let mut now = 0;
        wait_for(secs(cap), || {
            now += 2;
            let (cpu, compilers) = at(now);
            (secs(now), cpu, compilers)
        })
    }

    #[test]
    fn a_quiet_machine_is_twenty_quiet_seconds() {
        assert_eq!(waited(600, |_| (5.0, 0)), Wait { seconds: 22, quiet: true });
        // A build ends at 100 s.
        let build = |t| if t <= 100 { (90.0, 3) } else { (10.0, 0) };
        assert_eq!(waited(600, build), Wait { seconds: 122, quiet: true });
        // A compiler on a calm processor is not quiet, nor a busy processor
        // without one.
        let linking = |t| if t <= 40 { (10.0, 1) } else { (10.0, 0) };
        assert_eq!(waited(600, linking).seconds, 62);
        let other = |t| if t <= 40 { (31.0, 0) } else { (29.0, 0) };
        assert_eq!(waited(600, other).seconds, 62);
        // A spike starts the twenty seconds again.
        let spike = |t| if t == 16 { (80.0, 0) } else { (5.0, 0) };
        assert_eq!(waited(600, spike).seconds, 38);
    }

    #[test]
    fn the_wait_ends_at_its_cap() {
        assert_eq!(waited(60, |_| (90.0, 2)), Wait { seconds: 60, quiet: false });
    }

    #[test]
    fn the_titles_own_time_is_not_other_load() {
        let mut meter = Meter::default();
        assert_eq!(meter.sample(1.0, Times { busy: 100, all: 1000 }, 50), None);
        // 800 of 1000 busy, 700 of them the title's.
        let s = meter.sample(3.0, Times { busy: 900, all: 2000 }, 750).unwrap();
        assert_eq!(s, Sample { at: 3.0, total: 80.0, other: 10.0 });
        // Counters read at different moments may put the title above the whole.
        let s = meter.sample(5.0, Times { busy: 1000, all: 3000 }, 900).unwrap();
        assert_eq!(s.other, 0.0);
        assert_eq!(meter.sample(7.0, Times { busy: 1000, all: 3000 }, 900), None);
    }

    fn record(others: &[f64]) -> Record {
        let samples = others
            .iter()
            .enumerate()
            .map(|(i, &other)| Sample { at: 100.0 + i as f64 * 2.0, total: other + 40.0, other })
            .collect();
        Record { wait: Some(Wait { seconds: 22, quiet: true }), samples }
    }

    #[test]
    fn a_run_is_busy_when_over_a_quarter_of_its_looks_are() {
        let quarter = record(&[50.0, 5.0, 5.0, 5.0]);
        assert!(!quarter.summary(None).unwrap().busy());
        let more = record(&[50.0, 50.0, 5.0, 5.0]);
        assert!(more.summary(None).unwrap().busy());
        assert_eq!(more.area(None).status, Status::Warn);
        assert!(more.area(None).detail.starts_with("MACHINE BUSY"));
        // Only the lines measured count: the load before them does not.
        let early = record(&[90.0, 90.0, 5.0, 5.0, 5.0]);
        assert!(early.summary(None).unwrap().busy());
        let window = Some((104.0, 108.0));
        assert!(!early.summary(window).unwrap().busy());
        assert_eq!(early.area(window).status, Status::Pass);
        assert_eq!(early.area(Some((500.0, 600.0))).status, Status::Unknown);
    }

    #[test]
    fn a_record_is_read_back_from_its_file() {
        let written = record(&[1.5, 25.0]);
        let mut text = Record::header(written.wait);
        for s in &written.samples {
            text += &Record::line(*s);
        }
        // A line cut short by a run ended mid-write is left out.
        text += "look 120.0 4";
        assert_eq!(Record::parse(&text), written);
        assert_eq!(Record::parse(&Record::header(None)).wait, None);
        let never = Wait { seconds: 600, quiet: false };
        assert_eq!(Record::parse(&Record::header(Some(never))).wait, Some(never));
    }

    #[test]
    fn compilers_are_known_by_name() {
        assert!(is_compiler("rustc.exe") && is_compiler("LINK.EXE"));
        assert!(!is_compiler("cargo.exe") && !is_compiler("xtask.exe"));
    }

    #[test]
    #[cfg(windows)]
    fn the_machine_says_how_busy_it_is() {
        let first = system_times().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let then = system_times().unwrap();
        assert!(then.all > first.all && then.busy <= then.all);
        compilers_running();
    }
}
