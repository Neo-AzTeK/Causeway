//! `cargo xtask compare`: one run against earlier runs, metric by metric.
//!
//! A report holds medians; the log holds every world line. Speed is judged by
//! the median against the spread (the larger interquartile range of the two
//! runs, with a floor), so a CPU-noisy baseline cannot make a regression out
//! of nothing. Fidelity counters (draws, dropped draws by cause and kind,
//! refused and untranslated shaders, black frames, non-flat pixels) are judged
//! the same way, except counts of whole-run things, where any rise is one.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use clap::Args;

use crate::load::{Record, Summary};
use crate::report::{Baseline, Log, Series, read_baseline};

#[derive(Args)]
pub struct Compare {
    /// The run to judge: its folder (`log.txt`, `report.json`), or a log file.
    run: PathBuf,
    /// An earlier run to compare with, same form; several are allowed.
    #[arg(long = "baseline", required = true)]
    baselines: Vec<PathBuf>,
    /// A metric is a regression only when it is one against every baseline
    /// that has it (the default: against any of them), for a noisy baseline
    /// that must not decide alone.
    #[arg(long)]
    all: bool,
}

/// Fewer world lines than this say nothing about a spread.
const MIN_LINES: usize = 3;

pub fn run(args: &Compare) -> Result<()> {
    let run = Loaded::read(&args.run)?;
    let bases = args.baselines.iter().map(|b| Loaded::read(b)).collect::<Result<Vec<_>>>()?;
    let outcome = compare(&run, &bases, !args.all);
    print!("{}", outcome.text);
    let failures = outcome.failures();
    ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(())
}

/// A run read for comparison.
struct Loaded {
    name: String,
    series: Series,
    metrics: Vec<(&'static str, Option<f64>)>,
    /// What other processes used over the lines measured; `None` when the run
    /// has no `load.txt` for them.
    load: Option<Summary>,
}

impl Loaded {
    fn read(path: &Path) -> Result<Loaded> {
        let (log, report) = if path.is_dir() {
            (path.join("log.txt"), path.join("report.json"))
        } else {
            (path.to_owned(), path.with_file_name("report.json"))
        };
        let bytes = std::fs::read(&log).with_context(|| format!("reading {}", log.display()))?;
        // The non-flat share needs the frames, which the report measured.
        let detail = match read_baseline(&report) {
            Baseline::Metrics(rows) => {
                rows.into_iter().find(|(k, _)| k == "detail_pct").and_then(|(_, v)| v)
            }
            _ => None,
        };
        let name = log.parent().and_then(Path::file_name).map_or_else(
            || log.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let record = Record::beside(&log);
        Ok(Self::from_text(&name, &String::from_utf8_lossy(&bytes), detail, record.as_ref()))
    }

    fn from_text(name: &str, text: &str, detail: Option<f64>, record: Option<&Record>) -> Loaded {
        let log = Log::parse(text);
        let load = record.and_then(|r| r.summary(log.measured_window()));
        Loaded { name: name.to_owned(), series: log.series(), metrics: log.metrics(detail), load }
    }

    fn busy(&self) -> bool {
        self.load.is_some_and(|l| l.busy())
    }

    /// What the table's header says of the load.
    fn load_note(&self) -> String {
        match self.load {
            Some(l) if l.busy() => format!("MACHINE BUSY: {}", l.text()),
            Some(l) => l.text(),
            None => "load not recorded".into(),
        }
    }

    fn lines(&self) -> usize {
        self.series.speed.first().map_or(0, |(_, v)| v.len())
    }

    fn metric(&self, name: &str) -> Option<f64> {
        self.metrics.iter().find(|(k, _)| *k == name).and_then(|(_, v)| *v)
    }
}

/// A metric's values: how many, and where they lie.
#[derive(Clone, Debug, PartialEq)]
struct Dist {
    n: usize,
    median: f64,
    q1: f64,
    q3: f64,
    max: f64,
}

impl Dist {
    fn of(values: &[f64]) -> Dist {
        let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
        sorted.sort_by(f64::total_cmp);
        let at = |p: f64| {
            if sorted.is_empty() {
                return 0.0;
            }
            let pos = p * (sorted.len() - 1) as f64;
            let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
            sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
        };
        Dist {
            n: sorted.len(),
            median: at(0.5),
            q1: at(0.25),
            q3: at(0.75),
            max: sorted.last().copied().unwrap_or(0.0),
        }
    }

    /// One whole-run number, or none.
    fn scalar(value: Option<f64>) -> Dist {
        match value {
            Some(v) => Dist { n: 1, median: v, q1: v, q3: v, max: v },
            None => Dist::of(&[]),
        }
    }

    fn iqr(&self) -> f64 {
        self.q3 - self.q1
    }

    fn text(&self, lines: bool) -> String {
        if self.n == 0 {
            return "n/a".into();
        }
        if !lines {
            return num(self.median);
        }
        format!("{} [{}..{}]", num(self.median), num(self.q1), num(self.q3))
    }
}

fn num(v: f64) -> String {
    if v.abs() < 1.0 && v != 0.0 {
        format!("{v:.2}")
    } else if v.abs() < 100.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.0}")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    Regression,
    Improvement,
    Noise,
    /// The baseline has too few world lines for a spread: it says nothing.
    Few,
    /// The run has too few where the baseline has enough: nothing says it is
    /// no worse, so it fails.
    Short,
    /// The run has no value where the baseline has one: it fails too.
    Missing,
    NoData,
}

impl Verdict {
    fn word(self) -> &'static str {
        match self {
            Verdict::Regression => "REGRESSION",
            Verdict::Improvement => "improvement",
            Verdict::Noise => "within noise",
            Verdict::Few => "too few lines",
            Verdict::Short => "FAIL: run too short",
            Verdict::Missing => "FAIL: missing in run",
            Verdict::NoData => "n/a",
        }
    }
}

/// How a row is judged.
#[derive(Clone, Copy)]
enum Judge {
    /// Many values: the median against the larger spread, `rel` of the
    /// baseline's median, or `abs`, whichever is most.
    Spread { abs: f64, rel: f64 },
    /// One number from many frames (the non-flat share): `rel` of the
    /// baseline's.
    Share { rel: f64 },
    /// A whole-run count: any change is one.
    Count,
    /// Whether the world was reached: not reaching it, when the baseline did,
    /// is a regression.
    Reached,
}

struct Row {
    name: String,
    higher_is_better: bool,
    judge: Judge,
    run: Dist,
    bases: Vec<Dist>,
}

fn verdict(judge: Judge, higher_is_better: bool, run: &Dist, base: &Dist) -> Verdict {
    if let Judge::Reached = judge {
        let reached = |d: &Dist| d.median > 0.0;
        return match (reached(run), reached(base)) {
            (false, true) => Verdict::Regression,
            (true, false) => Verdict::Improvement,
            _ => Verdict::Noise,
        };
    }
    if base.n == 0 {
        return Verdict::NoData;
    }
    if run.n == 0 {
        return Verdict::Missing;
    }
    let delta = run.median - base.median;
    let worse = if higher_is_better { -delta } else { delta };
    let noise = match judge {
        Judge::Spread { abs, rel } => {
            if base.n < MIN_LINES {
                return Verdict::Few;
            }
            if run.n < MIN_LINES {
                return Verdict::Short;
            }
            run.iqr().max(base.iqr()).max(rel * base.median.abs()).max(abs)
        }
        Judge::Share { rel } => rel * base.median.abs(),
        Judge::Count | Judge::Reached => 0.0,
    };
    if worse > noise {
        Verdict::Regression
    } else if worse < -noise {
        Verdict::Improvement
    } else {
        Verdict::Noise
    }
}

/// One verdict over several baselines. What the run lacks fails whatever the
/// others say. A regression against `any` baseline is one (else against every
/// baseline that judged it); an improvement is one only against every one.
fn combine(verdicts: &[Verdict], any: bool) -> Verdict {
    for v in [Verdict::Missing, Verdict::Short] {
        if verdicts.contains(&v) {
            return v;
        }
    }
    // Baselines that say nothing about this metric do not outvote the others.
    let data: Vec<Verdict> = verdicts
        .iter()
        .copied()
        .filter(|v| !matches!(v, Verdict::NoData | Verdict::Few))
        .collect();
    if data.is_empty() {
        return if verdicts.contains(&Verdict::Few) { Verdict::Few } else { Verdict::NoData };
    }
    let every = |v| data.iter().all(|d| *d == v);
    if (any && data.contains(&Verdict::Regression)) || every(Verdict::Regression) {
        return Verdict::Regression;
    }
    if every(Verdict::Improvement) { Verdict::Improvement } else { Verdict::Noise }
}

fn rows(run: &Loaded, bases: &[Loaded]) -> Vec<Row> {
    let spread = |abs, rel| Judge::Spread { abs, rel };
    let from_series = |name: &str, of: &Loaded| {
        let all = of.series.speed.iter().map(|(k, v)| (*k, v));
        let drops = of.series.fidelity.iter().map(|(k, v)| (k.as_str(), v));
        all.chain(drops).find(|(k, _)| *k == name).map_or_else(|| Dist::of(&[]), |(_, v)| Dist::of(v))
    };
    let mut out = Vec::new();
    let mut push = |name: &str, higher_is_better, judge, get: &dyn Fn(&Loaded) -> Dist| {
        let run = get(run);
        let bases: Vec<Dist> = bases.iter().map(get).collect();
        // Drops that no run has are not worth a line; drops the run does not
        // report, where a baseline does, are.
        let lacks = run.n == 0 && bases.iter().any(|b| b.n > 0);
        let silent = !lacks && run.max == 0.0 && bases.iter().all(|b| b.max == 0.0);
        let dropped = name.starts_with("dropped_") || name.starts_with("kind_");
        if !(dropped && silent) {
            out.push(Row { name: name.to_owned(), higher_is_better, judge, run, bases });
        }
    };
    for (name, higher, judge) in [
        ("fps", true, spread(0.05, 0.05)),
        ("ms", false, spread(1.0, 0.05)),
        ("preparing_ms", false, spread(1.0, 0.05)),
        ("gpu_ms", false, spread(1.0, 0.05)),
        ("copy_back_ms", false, spread(1.0, 0.05)),
    ] {
        push(name, higher, judge, &|l| from_series(name, l));
    }
    push("drawn_per_frame", true, spread(1.0, 0.05), &|l| from_series("drawn_per_frame", l));
    for name in [
        "dropped_refused",
        "dropped_compiling",
        "dropped_other",
        "kind_refused",
        "kind_untranslated",
        "kind_indirect",
        "kind_slots",
        "kind_compiling",
        "kind_other",
    ] {
        push(name, false, spread(1.0, 0.05), &|l| from_series(name, l));
    }
    for name in ["refused_pipelines", "untranslated_shaders", "black_frames"] {
        push(name, false, Judge::Count, &|l| Dist::scalar(l.metric(name)));
    }
    push("detail_pct", true, Judge::Share { rel: 0.2 }, &|l| {
        Dist::scalar(l.metric("detail_pct"))
    });
    push("world_lines", true, Judge::Reached, &|l| Dist::scalar(l.metric("world_lines")));
    out
}

struct Outcome {
    text: String,
    regressions: Vec<String>,
    /// Why the run cannot be said to be no worse: what it lacks, or too few
    /// world lines to judge.
    unjudged: Vec<String>,
    /// Why what was measured cannot be believed either way: the machine was
    /// busy during the run, or during every baseline.
    busy: Option<String>,
}

impl Outcome {
    /// What makes the command fail; empty when it passes.
    fn failures(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(why) = &self.busy {
            out.push(format!("machine busy ({why}): run again on a quiet machine; as measured"));
        }
        if !self.regressions.is_empty() {
            out.push(format!("regression in {}", self.regressions.join(", ")));
        }
        if !self.unjudged.is_empty() {
            out.push(format!("cannot judge: {}", self.unjudged.join(", ")));
        }
        out
    }
}

/// Why the machine's load makes the comparison say nothing, if it does.
fn busy_reason(run: &Loaded, bases: &[Loaded]) -> Option<String> {
    let mut why = Vec::new();
    if run.busy() {
        why.push(format!("during {}", run.name));
    }
    if bases.iter().all(Loaded::busy) {
        why.push("during every baseline".to_owned());
    }
    (!why.is_empty()).then(|| why.join(" and "))
}

/// What stops any judgement of speed, whatever the rows say.
fn too_short(run: &Loaded, bases: &[Loaded]) -> Vec<String> {
    let mut out = Vec::new();
    if run.lines() < MIN_LINES {
        out.push(format!("the run has {} world lines (needs {MIN_LINES})", run.lines()));
    }
    if bases.iter().all(|b| b.lines() < MIN_LINES) {
        out.push(format!("no baseline has {MIN_LINES} world lines"));
    }
    out
}

fn compare(run: &Loaded, bases: &[Loaded], any: bool) -> Outcome {
    let rows = rows(run, bases);
    let mut table: Vec<[String; 2]> = Vec::new();
    let mut cells: Vec<Vec<String>> = Vec::new();
    let (mut regressions, mut improvements) = (Vec::new(), Vec::new());
    let mut unjudged = too_short(run, bases);
    for row in &rows {
        let lines = matches!(row.judge, Judge::Spread { .. });
        let verdicts: Vec<Verdict> =
            row.bases.iter().map(|b| verdict(row.judge, row.higher_is_better, &row.run, b)).collect();
        // A busy baseline has its column, and no say while a quiet one has.
        let quiet = bases.iter().any(|b| !b.busy());
        let counted: Vec<Verdict> = verdicts
            .iter()
            .zip(bases)
            .filter(|(_, b)| !(quiet && b.busy()))
            .map(|(v, _)| *v)
            .collect();
        let overall = combine(&counted, any);
        match overall {
            Verdict::Regression => regressions.push(row.name.clone()),
            Verdict::Improvement => improvements.push(row.name.clone()),
            Verdict::Missing => unjudged.push(format!("{} missing", row.name)),
            Verdict::Short => unjudged.push(format!("{} has too few lines", row.name)),
            _ => {}
        }
        table.push([row.name.clone(), row.run.text(lines)]);
        let mut line: Vec<String> = row
            .bases
            .iter()
            .zip(&verdicts)
            .map(|(b, v)| format!("{} {}", b.text(lines), v.word()))
            .collect();
        line.push(overall.word().to_owned());
        cells.push(line);
    }

    let mut text = format!(
        "{}: {} world lines (median [q1..q3] per line); {}\n",
        run.name,
        run.lines(),
        run.load_note()
    );
    for b in bases {
        text += &format!("baseline {}: {} world lines; {}\n", b.name, b.lines(), b.load_note());
    }
    text += &format!(
        "a metric is judged {} the baselines; spread = larger IQR (floor 5% of the baseline)\n",
        if any { "against ANY of" } else { "against ALL of" }
    );
    let head: Vec<String> = ["metric", &run.name]
        .iter()
        .map(|s| (*s).to_owned())
        .chain(bases.iter().map(|b| format!("vs {}", b.name)))
        .chain(["overall".to_owned()])
        .collect();
    let body: Vec<Vec<String>> = table
        .iter()
        .zip(&cells)
        .map(|(t, c)| t.iter().cloned().chain(c.iter().cloned()).collect())
        .collect();
    let widths: Vec<usize> = (0..head.len())
        .map(|i| body.iter().map(|r| r[i].len()).chain([head[i].len()]).max().unwrap_or(0))
        .collect();
    let mut line = |cols: &[String]| {
        let padded: Vec<String> =
            cols.iter().zip(&widths).map(|(c, w)| format!("{c:<w$}")).collect();
        text += padded.join("  ").trim_end();
        text += "\n";
    };
    line(&head);
    for row in &body {
        line(row);
    }
    let busy = busy_reason(run, bases);
    let mut outcome = Outcome { text, regressions, unjudged, busy };
    let mut parts = Vec::new();
    if !outcome.regressions.is_empty() {
        parts.push(format!("REGRESSION in {}", outcome.regressions.join(", ")));
    }
    if !outcome.unjudged.is_empty() {
        parts.push(format!("CANNOT JUDGE ({})", outcome.unjudged.join(", ")));
    }
    if parts.is_empty() {
        parts.push("no regression".to_owned());
    }
    if !improvements.is_empty() {
        parts.push(format!("improvement in {}", improvements.join(", ")));
    }
    let mut verdict = parts.join("; ");
    let ignored: Vec<&str> = bases.iter().filter(|b| b.busy()).map(|b| b.name.as_str()).collect();
    if let Some(why) = &outcome.busy {
        verdict = format!("MACHINE BUSY {why}: run again on a quiet machine. As measured: {verdict}");
    } else if !ignored.is_empty() {
        verdict += &format!(" (not counted, MACHINE BUSY during them: {})", ignored.join(", "));
    }
    outcome.text += &format!("verdict: {verdict}\n");
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::Sample;

    const SPREAD: Judge = Judge::Spread { abs: 1.0, rel: 0.05 };

    fn dist(values: &[f64]) -> Dist {
        Dist::of(values)
    }

    #[test]
    fn quantiles_interpolate() {
        let d = dist(&[4.0, 1.0, 3.0, 2.0, 5.0]);
        assert_eq!((d.n, d.median, d.q1, d.q3, d.max), (5, 3.0, 2.0, 4.0, 5.0));
        assert_eq!(dist(&[1.0, 2.0]).median, 1.5);
        assert_eq!(dist(&[]).n, 0);
    }

    #[test]
    fn speed_regresses_only_beyond_the_spread() {
        let base = dist(&[300.0, 310.0, 320.0, 330.0, 340.0]); // median 320, IQR 20
        let slower = dist(&[360.0, 365.0, 370.0, 375.0, 380.0]); // +50
        let jitter = dist(&[325.0, 330.0, 335.0, 340.0, 345.0]); // +15 < IQR
        let faster = dist(&[250.0, 255.0, 260.0, 265.0, 270.0]);
        assert_eq!(verdict(SPREAD, false, &slower, &base), Verdict::Regression);
        assert_eq!(verdict(SPREAD, false, &jitter, &base), Verdict::Noise);
        assert_eq!(verdict(SPREAD, false, &faster, &base), Verdict::Improvement);
        // The same numbers where higher is better swap the verdicts.
        assert_eq!(verdict(SPREAD, true, &slower, &base), Verdict::Improvement);
    }

    #[test]
    fn a_wide_baseline_spread_hides_a_small_change() {
        let noisy = dist(&[200.0, 250.0, 300.0, 600.0, 700.0]); // IQR 350
        let run = dist(&[400.0, 410.0, 420.0, 430.0, 440.0]); // +120 over median 300
        assert_eq!(verdict(SPREAD, false, &run, &noisy), Verdict::Noise);
    }

    #[test]
    fn few_lines_and_missing_data() {
        let few = dist(&[1.0, 2.0]);
        let many = dist(&[1.0, 2.0, 3.0]);
        assert_eq!(verdict(SPREAD, false, &few, &many), Verdict::Short);
        assert_eq!(verdict(SPREAD, false, &many, &few), Verdict::Few);
        assert_eq!(verdict(SPREAD, false, &dist(&[]), &many), Verdict::Missing);
        assert_eq!(verdict(SPREAD, false, &many, &dist(&[])), Verdict::NoData);
        assert_eq!(verdict(SPREAD, false, &dist(&[]), &dist(&[])), Verdict::NoData);
    }

    #[test]
    fn counts_and_shares() {
        let (two, three) = (Dist::scalar(Some(2.0)), Dist::scalar(Some(3.0)));
        assert_eq!(verdict(Judge::Count, false, &three, &two), Verdict::Regression);
        assert_eq!(verdict(Judge::Count, false, &two, &three), Verdict::Improvement);
        assert_eq!(verdict(Judge::Count, false, &two, &two), Verdict::Noise);
        assert_eq!(verdict(Judge::Count, false, &Dist::scalar(None), &two), Verdict::Missing);
        assert_eq!(verdict(Judge::Count, false, &two, &Dist::scalar(None)), Verdict::NoData);
        let share = Judge::Share { rel: 0.2 };
        assert_eq!(verdict(share, true, &Dist::scalar(None), &two), Verdict::Missing);
        let share = Judge::Share { rel: 0.2 };
        let (base, small, big) =
            (Dist::scalar(Some(3.8)), Dist::scalar(Some(3.3)), Dist::scalar(Some(1.9)));
        assert_eq!(verdict(share, true, &small, &base), Verdict::Noise);
        assert_eq!(verdict(share, true, &big, &base), Verdict::Regression);
    }

    #[test]
    fn world_not_reached_is_a_regression() {
        let (none, some) = (Dist::scalar(Some(0.0)), Dist::scalar(Some(19.0)));
        assert_eq!(verdict(Judge::Reached, true, &none, &some), Verdict::Regression);
        assert_eq!(verdict(Judge::Reached, true, &some, &none), Verdict::Improvement);
        assert_eq!(verdict(Judge::Reached, true, &some, &some), Verdict::Noise);
    }

    #[test]
    fn baselines_combine_by_agreement() {
        use Verdict::*;
        assert_eq!(combine(&[Regression, Regression], false), Regression);
        assert_eq!(combine(&[Regression, Noise], false), Noise);
        assert_eq!(combine(&[Regression, Noise], true), Regression);
        assert_eq!(combine(&[Regression, Improvement], false), Noise);
        assert_eq!(combine(&[Improvement, NoData], false), Improvement);
        assert_eq!(combine(&[NoData], false), NoData);
        assert_eq!(combine(&[Few, Few], false), Few);
        assert_eq!(combine(&[Few, Regression], false), Regression);
        assert_eq!(combine(&[Improvement, Noise], true), Noise);
        assert_eq!(combine(&[Noise, Missing], true), Missing);
        assert_eq!(combine(&[Improvement, Short], false), Short);
    }

    fn same_log(n: usize) -> String {
        log(n, 1.0, 1000.0, 230.0, 650.0, 100.0)
    }

    #[test]
    fn a_run_with_under_three_world_lines_fails() {
        // A tenth of the baseline's frame rate, in two lines.
        let run = loaded("r", &log(2, 0.1, 10_000.0, 2300.0, 6500.0, 100.0));
        let out = compare(&run, &[loaded("a", &same_log(20))], true);
        assert!(!out.failures().is_empty(), "{}", out.text);
        assert!(out.text.contains("CANNOT JUDGE"), "{}", out.text);
        assert!(out.text.contains("FAIL: run too short"), "{}", out.text);
        // Nor may a run that looks the same pass on two lines.
        let out = compare(&loaded("r", &same_log(2)), &[loaded("a", &same_log(20))], true);
        assert!(!out.failures().is_empty(), "{}", out.text);
    }

    #[test]
    fn zero_world_lines_on_both_sides_is_not_no_regression() {
        let out = compare(&loaded("r", &same_log(0)), &[loaded("a", &same_log(0))], true);
        assert!(!out.failures().is_empty(), "{}", out.text);
        assert!(!out.text.contains("no regression"), "{}", out.text);
        // A good run against baselines too short to say anything: the same.
        let out = compare(&loaded("r", &same_log(20)), &[loaded("a", &same_log(2))], true);
        assert!(!out.failures().is_empty(), "{}", out.text);
        // One short baseline beside a good one does not matter.
        let bases = [loaded("a", &same_log(2)), loaded("b", &same_log(20))];
        let out = compare(&loaded("r", &same_log(20)), &bases, true);
        assert!(out.failures().is_empty(), "{}", out.text);
    }

    #[test]
    fn what_the_run_lacks_fails() {
        // No report beside the run: no non-flat share.
        let run = Loaded::from_text("r", &same_log(20), None, None);
        let out = compare(&run, &[loaded("a", &same_log(20))], true);
        assert_eq!(out.unjudged, ["detail_pct missing"], "{}", out.text);
        // Not a diagnostics build's log: black frames are unknown.
        let release = same_log(20).replace("copies per frame: ", "");
        let out = compare(&loaded("r", &release), &[loaded("a", &same_log(20))], true);
        assert_eq!(out.unjudged, ["black_frames missing"], "{}", out.text);
        // No cause lines, where the baseline has drops.
        let silent: String = same_log(20)
            .lines()
            .filter(|l| !l.contains("dropped by cause"))
            .map(|l| format!("{l}\n"))
            .collect();
        let out = compare(&loaded("r", &silent), &[loaded("a", &same_log(20))], true);
        assert!(out.unjudged.contains(&"dropped_refused missing".to_owned()), "{}", out.text);
    }

    /// A run whose every look found other processes at `other` percent. The
    /// synthetic log's times wrap each minute, so looks cover all of one.
    fn measured(name: &str, text: &str, other: f64) -> Loaded {
        let start = 1_767_225_600.0;
        let samples = (0..30)
            .map(|i| Sample { at: start + f64::from(i) * 2.0, total: other + 30.0, other })
            .collect();
        let record = Record { wait: None, samples };
        Loaded::from_text(name, text, Some(3.0), Some(&record))
    }

    #[test]
    fn a_busy_run_says_so_instead_of_a_bare_regression() {
        let base = || [measured("a", &same_log(20), 5.0)];
        let slow = log(20, 0.5, 2000.0, 600.0, 650.0, 100.0);
        let out = compare(&measured("r", &slow, 60.0), &base(), true);
        assert!(out.text.contains("verdict: MACHINE BUSY during r"), "{}", out.text);
        assert!(out.text.contains("As measured: REGRESSION in"), "{}", out.text);
        assert!(out.failures()[0].starts_with("machine busy (during r)"));
        // Busy and no worse is no pass either: load widens the spread.
        let out = compare(&measured("r", &same_log(20), 60.0), &base(), true);
        assert!(!out.failures().is_empty(), "{}", out.text);
        assert!(out.text.contains("As measured: no regression"), "{}", out.text);
        // The same slow run on a quiet machine is a regression, plainly.
        let out = compare(&measured("r", &slow, 5.0), &base(), true);
        assert!(out.text.contains("verdict: REGRESSION in"), "{}", out.text);
        assert!(!out.text.contains("BUSY"), "{}", out.text);
        // A run with no record is not called busy.
        let out = compare(&loaded("r", &same_log(20)), &base(), true);
        assert!(out.failures().is_empty(), "{}", out.text);
        assert!(out.text.contains("load not recorded"), "{}", out.text);
    }

    #[test]
    fn a_busy_baseline_has_no_say_beside_a_quiet_one() {
        let run = || measured("r", &same_log(20), 5.0);
        // The busy baseline drew with far fewer drops: it would fail the run.
        let busy = || measured("b", &log(20, 1.0, 1000.0, 230.0, 650.0, 10.0), 60.0);
        let out = compare(&run(), &[measured("a", &same_log(20), 5.0), busy()], true);
        assert!(out.failures().is_empty(), "{}", out.text);
        assert!(out.text.contains("not counted, MACHINE BUSY during them: b"), "{}", out.text);
        // Alone, it makes the comparison say nothing.
        let out = compare(&run(), &[busy()], true);
        assert!(out.text.contains("MACHINE BUSY during every baseline"), "{}", out.text);
        assert!(!out.failures().is_empty());
    }

    #[test]
    fn by_default_one_baseline_is_enough_for_a_regression() {
        let bases = || {
            [loaded("a", &same_log(20)), loaded("b", &log(20, 1.0, 1000.0, 600.0, 650.0, 100.0))]
        };
        let run = loaded("r", &log(20, 1.0, 1000.0, 400.0, 650.0, 100.0));
        assert_eq!(compare(&run, &bases(), true).regressions, ["preparing_ms"]);
        assert!(compare(&run, &bases(), false).regressions.is_empty());
    }

    /// A log of `n` world lines (900 draws each) and one cause line per line.
    fn log(n: usize, fps: f64, ms: f64, prep: f64, gpu: f64, refused: f64) -> String {
        let mut text = String::from("2026-01-01T00:00:00.0Z WARN victory_gpu::host: copies per frame: 1\n");
        for i in 0..n {
            // A little spread, the same for every run.
            let w = (i % 5) as f64;
            text += &format!(
                "2026-01-01T00:00:{:02}.0Z  INFO victory_gpu::host: {:.1} fps, {:.1} ms/frame \
                 ({:.1} preparing: copies 0.0 ms 0.0 MB, textures 0.0 ms 0.0 MB; {:.1} on the \
                 GPU; 1.0 copying back 0.0 MB), 900 draws/frame, dropped 0 without a host \
                 pipeline (0 being made) and 0 indirect; 1 root-view slots/frame\n",
                i % 60,
                fps + w * 0.01,
                ms + w,
                prep + w,
                gpu + w
            );
            text += &format!(
                "2026-01-01T00:00:{:02}.0Z  INFO victory_gpu::host: dropped by cause: refused \
                 {refused}, compiling 0.0, other 0.0 draws/frame\n",
                i % 60
            );
        }
        text
    }

    fn loaded(name: &str, text: &str) -> Loaded {
        Loaded::from_text(name, text, Some(3.0), None)
    }

    #[test]
    fn a_slower_run_is_a_regression_against_every_baseline() {
        let base_a = loaded("a", &log(20, 1.0, 1000.0, 230.0, 650.0, 100.0));
        let base_b = loaded("b", &log(20, 0.9, 1100.0, 300.0, 700.0, 100.0));
        let run = loaded("r", &log(20, 0.7, 1500.0, 500.0, 700.0, 100.0));
        let out = compare(&run, &[base_a, base_b], false);
        assert!(out.regressions.contains(&"preparing_ms".to_owned()), "{}", out.text);
        assert!(out.regressions.contains(&"fps".to_owned()), "{}", out.text);
        assert!(!out.regressions.contains(&"gpu_ms".to_owned()), "{}", out.text);
        assert!(out.text.contains("verdict: REGRESSION"));
    }

    #[test]
    fn the_same_run_is_within_noise_and_a_faster_one_improves() {
        let base = loaded("a", &log(20, 1.0, 1000.0, 230.0, 650.0, 100.0));
        let same = loaded("s", &log(20, 1.0, 1000.0, 230.0, 650.0, 100.0));
        let out = compare(&same, &[base], false);
        assert!(out.regressions.is_empty(), "{}", out.text);
        assert!(out.text.contains("verdict: no regression\n"), "{}", out.text);

        let base = loaded("a", &log(20, 1.0, 1000.0, 230.0, 650.0, 100.0));
        let faster = loaded("f", &log(20, 1.0, 900.0, 130.0, 650.0, 100.0));
        let out = compare(&faster, &[base], false);
        assert!(out.regressions.is_empty());
        assert!(out.text.contains("improvement in ms, preparing_ms"), "{}", out.text);
    }

    #[test]
    fn more_dropped_draws_is_a_fidelity_regression() {
        let base = loaded("a", &log(20, 1.0, 1000.0, 230.0, 650.0, 100.0));
        let run = loaded("r", &log(20, 1.0, 1000.0, 230.0, 650.0, 400.0));
        let out = compare(&run, &[base], false);
        assert_eq!(out.regressions, ["dropped_refused"], "{}", out.text);
    }

    #[test]
    fn black_frames_and_unreached_world_fail() {
        let base = loaded("a", &log(20, 1.0, 1000.0, 230.0, 650.0, 100.0));
        let mut text = log(20, 1.0, 1000.0, 230.0, 650.0, 100.0);
        text += "2026-01-01T00:01:00.0Z WARN victory_gpu::host: a black frame after a frame \
                 that was not frame=100\n";
        let out = compare(&loaded("r", &text), &[base], false);
        assert_eq!(out.regressions, ["black_frames"], "{}", out.text);

        let base = loaded("a", &log(20, 1.0, 1000.0, 230.0, 650.0, 100.0));
        let out = compare(&loaded("menu", &log(0, 0.0, 0.0, 0.0, 0.0, 0.0)), &[base], false);
        assert!(out.regressions.contains(&"world_lines".to_owned()), "{}", out.text);
    }
}
