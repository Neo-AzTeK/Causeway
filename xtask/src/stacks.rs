//! `cargo xtask stacks`: where the title's threads were in our code at each
//! stack dump of a run (`VICTORY_STACKS`), and a tally over all dumps: which
//! of our functions they ran in or waited in, and on what.
//!
//! A title thread is one started in the title's image (`victory-image`). A
//! dump frame reads `#<i> <module>+<rva>[ <function>+<offset>]`; functions
//! come from the diagnostics build's PDB (`victory-loader` `stacks.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;

#[derive(Args)]
pub struct Stacks {
    /// The run's folder (its `log.txt`) or a log file.
    run: PathBuf,
    /// Our frames shown per thread, innermost first.
    #[arg(long, default_value_t = 6)]
    depth: usize,
    /// Print only the tally, not each dump.
    #[arg(long)]
    tally: bool,
}

/// One title thread at one dump.
#[derive(Debug, PartialEq)]
struct Sample {
    dump: usize,
    thread: String,
    /// Waiting on one of our locks, waiting on something else, or running.
    state: &'static str,
    /// Our functions, innermost first, without the standard library's.
    ours: Vec<String>,
}

pub fn run(args: &Stacks) -> Result<()> {
    let path = if args.run.is_dir() {
        args.run.join("log.txt")
    } else {
        args.run.clone()
    };
    let samples = samples(&read(&path)?);
    if !args.tally {
        for s in &samples {
            let shown: Vec<&str> = s.ours.iter().take(args.depth).map(String::as_str).collect();
            println!(
                "dump {} thread {} {}: {}",
                s.dump,
                s.thread,
                s.state,
                shown.join(" < ")
            );
        }
    }
    let mut tally: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for s in &samples {
        *tally
            .entry((s.state, s.ours.first().map_or("?", String::as_str)))
            .or_default() += 1;
    }
    let mut rows: Vec<_> = tally.into_iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    let dumps = samples.iter().map(|s| s.dump).max().unwrap_or(0);
    println!(
        "tally over {dumps} dumps, {} title-thread samples in our code:",
        samples.len()
    );
    for ((state, function), n) in rows {
        println!("  {n:>3}  {state:<12} {function}");
    }
    Ok(())
}

fn read(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Removes terminal colour codes (`ESC [ ... m`).
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            chars.by_ref().find(|&c| c == 'm');
        } else {
            out.push(c);
        }
    }
    out
}

fn samples(log: &str) -> Vec<Sample> {
    let mut out = Vec::new();
    let mut dump = 0;
    // The thread being read: its id, whether it is the title's, its frames.
    let mut current: Option<(String, bool, Vec<String>)> = None;
    let finish =
        |current: &mut Option<(String, bool, Vec<String>)>, dump: usize, out: &mut Vec<Sample>| {
            if let Some((thread, true, frames)) = current.take()
                && let Some(sample) = sample(dump, thread, &frames)
            {
                out.push(sample);
            }
        };
    for line in log.lines().map(plain) {
        let trimmed = line.trim_start();
        if line.contains("VIctory: stacks of") {
            finish(&mut current, dump, &mut out);
            dump += 1;
        } else if let Some(rest) = trimmed.strip_prefix("thread ") {
            finish(&mut current, dump, &mut out);
            let id = rest.split_whitespace().next().unwrap_or("").to_string();
            let title = rest
                .split(" start ")
                .nth(1)
                .is_some_and(|s| s.starts_with("victory-image"));
            current = Some((id, title, Vec::new()));
        } else if trimmed.starts_with('#') && line.starts_with("    ") {
            if let Some((_, _, frames)) = &mut current {
                frames.push(
                    trimmed
                        .split_whitespace()
                        .skip(1)
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        } else if !trimmed.is_empty() {
            finish(&mut current, dump, &mut out);
        }
    }
    finish(&mut current, dump, &mut out);
    out
}

/// A thread's sample if it is in our code (`victory.exe` frames).
fn sample(dump: usize, thread: String, frames: &[String]) -> Option<Sample> {
    let ours_at = frames.iter().position(|f| f.starts_with("victory.exe"))?;
    let function = |f: &String| f.split_once(' ').map(|(_, name)| strip_offset(name));
    let lock = frames[ours_at..]
        .iter()
        .take(3)
        .any(|f| f.contains("lock_contended"));
    let waits = frames[..ours_at]
        .iter()
        .any(|f| f.starts_with("ntdll.dll") || f.starts_with("KERNELBASE.dll"));
    let state = match (lock, waits) {
        (true, _) => "waits lock",
        (false, true) => "waits other",
        (false, false) => "runs",
    };
    let ours = frames[ours_at..]
        .iter()
        .filter(|f| f.starts_with("victory.exe"))
        .filter_map(function)
        .filter(|name| {
            !name.starts_with("std::")
                && !name.starts_with("core::")
                && !name.starts_with("alloc::")
        })
        .collect();
    Some(Sample {
        dump,
        thread,
        state,
        ours,
    })
}

/// `name<generics>+0x1f` -> `name<..>`.
fn strip_offset(name: &str) -> String {
    let name = name.rsplit_once("+0x").map_or(name, |(n, _)| n);
    name.find('<')
        .map_or(name.to_string(), |at| format!("{}<..>", &name[..at]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
VIctory: stacks of 3 threads
  thread 10 \"\" start victory-image-ab.dll+0x10 priority 8 (base 8)
    #0  ntdll.dll+0x16a054 ZwWaitForAlertByThreadId+0x14
    #1  KERNELBASE.dll+0xddd18 WaitOnAddress+0x38
    #2  victory.exe+0x60c884 std::sys::sync::mutex::futex::Mutex::lock_contended+0x80
    #3  victory.exe+0x20ac15 victory_gpu::descriptor::host::HostHeap::dynamic+0x55
    #4  victory.exe+0x2a02dd victory_gpu::draw::State::bind+0x1d
    #5  victory-image-ab.dll+0x99
  thread 11 \"pipelines\" start victory.exe+0x5b4b40 priority 8 (base 8)
    #0  victory.exe+0x1 victory_gpu::pipeline::make+0x1
  thread 12 \"\" start victory-image-ab.dll+0x10 priority 8 (base 8)
    #0  \x1b[2mvictory.exe+0x20aaa2 victory_gpu::descriptor::host::HostHeap::dynamic+0x22\x1b[0m
    #1  victory.exe+0x2a0e64 victory_gpu::draw::State::bind+0x44
2026-11-27T10:00:00Z INFO something else
VIctory: stacks of 1 threads
  thread 12 \"\" start victory-image-ab.dll+0x10 priority 8 (base 8)
    #0  victory-image-ab.dll+0x5
";

    #[test]
    fn title_threads_in_our_code_are_sampled_with_their_state() {
        let got = samples(LOG);
        let dynamic = "victory_gpu::descriptor::host::HostHeap::dynamic".to_string();
        let bind = "victory_gpu::draw::State::bind".to_string();
        assert_eq!(
            got,
            [
                Sample {
                    dump: 1,
                    thread: "10".into(),
                    state: "waits lock",
                    ours: vec![dynamic.clone(), bind.clone()]
                },
                Sample {
                    dump: 1,
                    thread: "12".into(),
                    state: "runs",
                    ours: vec![dynamic, bind]
                },
            ]
        );
    }
}
