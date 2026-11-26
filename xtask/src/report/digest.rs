//! The run digest: what to work on next, from one run's log. At most
//! `MAX_LINES` lines; a number the log does not give is "n/a", never
//! guessed. It holds counts and hashes only, so it stays outside the repo
//! with the rest of a run's output.

use super::sheet::Visual;
use super::*;

pub const MAX_LINES: usize = 60;
/// Rows of the tables, and of the delta (metrics per line).
const PIPELINES: usize = 10;
const KINDS: usize = 5;
const SWITCH_LINES: usize = 3;

/// What a digest needs besides the log.
pub struct Context {
    /// Commit and branch of the build that ran, or what is known.
    pub build: String,
    /// Switches the runner meant the game to see: name, value.
    pub requested: Vec<(String, String)>,
    pub baseline: Baseline,
    pub visual: Visual,
}

pub enum Baseline {
    /// `--baseline` was not given.
    Off,
    /// It was, and gave nothing to compare with.
    Unusable(String),
    Metrics(Vec<(String, Option<f64>)>),
}

/// The metrics of an earlier run, from its `report.json`, or from the
/// `log.txt` beside it when that report predates the metrics.
pub fn read_baseline(json: &Path) -> Baseline {
    let Ok(text) = std::fs::read_to_string(json) else {
        return Baseline::Unusable(format!("cannot read {}", json.display()));
    };
    if let Some((_, body)) = text.split_once("\"metrics\": {") {
        let body = body.split_once("\n  }").map_or(body, |(b, _)| b);
        let rows = body.lines().filter_map(|l| {
            let (name, value) = l.trim().trim_end_matches(',').split_once("\": ")?;
            Some((name.trim_start_matches('"').to_owned(), value.parse::<f64>().ok()))
        });
        return Baseline::Metrics(rows.collect());
    }
    let beside = json.with_file_name("log.txt");
    match std::fs::read(&beside) {
        Ok(bytes) => {
            let log = Log::parse(&String::from_utf8_lossy(&bytes));
            // Its frames are not read again: no detail measure.
            let metrics = log.metrics(None).into_iter().map(|(k, v)| (k.to_owned(), v));
            Baseline::Metrics(metrics.collect())
        }
        Err(_) => Baseline::Unusable(
            "baseline has no metrics (older report) and no log.txt beside it".into(),
        ),
    }
}

/// Better when higher; everything else is better lower.
const HIGHER_IS_BETTER: &[&str] = &["fps", "world_lines", "drawn_per_frame", "detail_pct"];

/// Where a draw was lost: what the host named, and who would fix it.
pub(super) struct Lost {
    /// Hash or verb: what names it.
    id: String,
    kind: String,
    reason: String,
    /// Merges what one fix removes.
    group: String,
    pub(super) tag: &'static str,
    area: &'static str,
    pub(super) per_frame: f64,
}

fn area_of(reason: &str) -> &'static str {
    let any = |words: &[&str]| words.iter().any(|w| reason.contains(w));
    if any(&["wave"]) {
        "wave64"
    } else if any(&["not supported", "GFX10", "translate", "unsupported", "lowering"]) {
        "shader lowering"
    } else if any(&["descriptor", "heap", "slots"]) {
        "descriptors (other AI)"
    } else {
        "pipeline"
    }
}

fn hashes(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .filter(|t| t.len() == 16 && t.bytes().all(|b| b.is_ascii_hexdigit()))
}

impl Log {
    /// Interval blocks of the world, or of every interval when the run never
    /// reached it.
    fn blocks_used(&self) -> (Vec<&DropBlock>, bool) {
        let world: Vec<_> = self.blocks.iter().filter(|b| b.world).collect();
        if world.is_empty() { (self.blocks.iter().collect(), false) } else { (world, true) }
    }

    /// Dropped draws per frame by cause (refused, compiling, other): mean
    /// of the intervals in `blocks_used`.
    fn cause_means(&self) -> Option<[f64; 3]> {
        let picked: Vec<_> = self
            .causes
            .iter()
            .zip(&self.blocks)
            .filter(|(_, b)| b.world)
            .map(|(c, _)| c)
            .collect();
        let picked: Vec<&[f64; 3]> =
            if picked.is_empty() { self.causes.iter().collect() } else { picked };
        (!picked.is_empty()).then(|| {
            let n = picked.len() as f64;
            [0, 1, 2].map(|i| picked.iter().map(|c| c[i]).sum::<f64>() / n)
        })
    }

    pub(super) fn classify(&self, why: &str, per_frame: f64) -> Lost {
        let unknown = "reason not logged".to_owned();
        let mut lost = Lost {
            id: why.split_whitespace().last().unwrap_or("?").to_owned(),
            kind: "?".into(),
            reason: why.to_owned(),
            group: clip(why),
            tag: "other",
            area: "pipeline",
            per_frame,
        };
        if why == "being made" {
            lost.id = "-".into();
            lost.kind = "any".into();
            lost.reason = "still compiling".into();
            lost.group = "still compiling".into();
            lost.tag = "compiling";
        } else if let Some(key) = why.strip_prefix("refused ") {
            lost.id = hashes(key).next().unwrap_or("?").to_owned();
            lost.kind = key.trim_start_matches('[').split_whitespace().next().unwrap_or("?").into();
            lost.tag = "refused";
            let bad = hashes(key).find_map(|h| self.untranslated.get(h));
            let reason = match bad {
                Some((_, error)) => {
                    lost.tag = "untranslated";
                    format!("does not translate: {}", error.split(" cached=").next().unwrap_or(error))
                }
                None => self.refused.get(key).cloned().unwrap_or(unknown),
            };
            lost.area = if bad.is_some() { "shader lowering" } else { area_of(&reason) };
            lost.group = reason.clone();
            lost.reason = reason;
        } else if let Some(rest) = why.strip_prefix("untranslated ") {
            let (stage, hash) = rest.split_once(' ').unwrap_or((rest, "?"));
            let error = self.untranslated.get(hash.trim()).map(|(_, e)| e.clone());
            lost.id = hash.trim().to_owned();
            lost.kind = stage.to_owned();
            lost.tag = "untranslated";
            lost.area = "shader lowering";
            lost.reason = format!("does not translate: {}", error.as_deref().and_then(|e| e.split(" cached=").next()).unwrap_or("?"));
            lost.group = lost.reason.clone();
        } else if why.starts_with("indirect kind") {
            let (head, reason) = why.split_once(": ").unwrap_or((why, "no host pipeline"));
            lost.id = head.trim_start_matches("indirect ").to_owned();
            lost.kind = "indirect".into();
            lost.tag = "indirect";
            lost.reason = reason.to_owned();
            lost.group = format!("indirect: {reason}");
        } else if why.contains("no dynamic slots") {
            lost.id = why.split(':').next().unwrap_or("?").to_owned();
            lost.kind = lost.id.clone();
            lost.tag = "slots";
            lost.area = "descriptors (other AI)";
            lost.reason = "no dynamic descriptor slots".into();
            lost.group = lost.reason.clone();
        }
        lost
    }

    /// What the host named as lost, per frame, largest first; and whether
    /// those are of the world.
    fn lost(&self) -> (Vec<Lost>, bool) {
        let (blocks, world) = self.blocks_used();
        let mut sums = BTreeMap::<&str, f64>::new();
        for (why, n) in blocks.iter().flat_map(|b| b.why.iter()) {
            *sums.entry(why.as_str()).or_default() += n;
        }
        // A reason a window does not list lost less than the list's least.
        let windows = blocks.len().max(1) as f64;
        let mut lost: Vec<Lost> =
            sums.into_iter().map(|(why, sum)| self.classify(why, sum / windows)).collect();
        lost.sort_by(|a, b| b.per_frame.total_cmp(&a.per_frame).then(a.id.cmp(&b.id)));
        (lost, world)
    }

    /// Numbers for the delta and for `report.json`. `None`: the log does not
    /// give it.
    pub fn metrics(&self, detail_pct: Option<f64>) -> Vec<(&'static str, Option<f64>)> {
        let world: Vec<&Rate> = self.world().collect();
        let med = |by: fn(&Rate) -> f64| median(&world, by).map(by);
        let other = |r: &Rate| (r.ms - r.preparing - r.gpu - r.copy_back).max(0.0);
        let causes = self.cause_means();
        let cause = |i: usize| causes.map(|c| c[i]);
        let black = self.black.iter().filter(|&&f| f >= STARTUP_FRAMES).count();
        vec![
            ("fps", med(|r| r.fps)),
            ("ms", med(|r| r.ms)),
            ("preparing_ms", med(|r| r.preparing)),
            ("gpu_ms", med(|r| r.gpu)),
            ("copy_back_ms", med(|r| r.copy_back)),
            ("other_ms", median(&world, other).map(other)),
            ("drawn_per_frame", med(|r| r.draws)),
            ("dropped_refused", cause(0)),
            ("dropped_compiling", cause(1)),
            ("dropped_other", cause(2)),
            ("refused_pipelines", Some(self.refused.len() as f64)),
            ("untranslated_shaders", Some(self.untranslated.len() as f64)),
            ("black_frames", self.diagnostics.then_some(black as f64)),
            ("world_lines", Some(world.len() as f64)),
            ("detail_pct", detail_pct),
        ]
    }

    pub fn digest(&self, areas: &[Area], ctx: &Context) -> String {
        let mut sections = vec![
            self.run_section(ctx),
            self.delta_section(ctx),
            self.cost_section(),
            self.draws_section(),
            self.kinds_section(),
            self.visual_section(ctx),
            self.stability_section(areas),
        ];
        let next = self.next_section(areas, ctx);
        // Cut the longest section's tail first: "Next" stays whole.
        let total = |s: &[Vec<String>], n: &[String]| s.iter().map(Vec::len).sum::<usize>() + n.len() + 1;
        while total(&sections, &next) > MAX_LINES {
            let longest = sections.iter_mut().max_by_key(|s| s.len()).unwrap();
            if longest.len() <= 2 {
                break;
            }
            longest.pop();
            *longest.last_mut().unwrap() += " ...";
        }
        let mut lines = vec!["# Run digest".to_owned()];
        lines.extend(sections.into_iter().flatten());
        lines.extend(next);
        lines.truncate(MAX_LINES);
        lines.join("\n") + "\n"
    }

    fn run_section(&self, ctx: &Context) -> Vec<String> {
        let mut out = vec!["## 1. Run".to_owned(), ctx.build.clone()];
        let adapter = self.adapter_text.clone().unwrap_or_else(|| self.adapter().to_string());
        let caps = self.caps.as_deref().unwrap_or("not logged (this build predates the caps line)");
        out.push(format!("adapter: {adapter}; caps: {caps}"));
        let mut warnings = Vec::new();
        match &self.switches {
            None => {
                out.push("switches seen: not logged (this build predates the switch echo)".into());
                for (name, _) in ctx.requested.iter().filter(|(n, _)| !is_runner(n)) {
                    warnings.push(format!("WARNING: {name} requested; the log cannot confirm it"));
                }
            }
            Some(seen) => {
                let shown: Vec<String> = seen
                    .iter()
                    .map(|(n, v)| format!("{}={}", n.trim_start_matches("VICTORY_"), cut(v, 28)))
                    .collect();
                out.push(format!("switches seen by the game ({}), VICTORY_ omitted:", seen.len()));
                let mut line = String::new();
                let mut lines = Vec::new();
                for item in shown {
                    if line.len() + item.len() > 110 {
                        lines.push(std::mem::take(&mut line));
                    }
                    line += &format!("{item} ");
                }
                lines.push(line);
                let more = lines.len() > SWITCH_LINES;
                out.extend(lines.into_iter().take(SWITCH_LINES).map(|l| l.trim_end().to_owned()));
                if more {
                    out.push("(more switches not shown)".into());
                }
                for (name, value) in &ctx.requested {
                    let want: String = value.chars().take(80).collect();
                    match seen.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
                        None => warnings.push(format!("WARNING: {name} was requested, not seen")),
                        Some((_, got)) if *got != want => warnings
                            .push(format!("WARNING: {name} seen with another value than requested")),
                        _ => {}
                    }
                }
            }
        }
        out.extend(warnings.into_iter().take(4));
        out
    }

    fn delta_section(&self, ctx: &Context) -> Vec<String> {
        let mut out = vec!["## 2. Delta vs baseline".to_owned()];
        let old = match &ctx.baseline {
            Baseline::Off => {
                out.push("no --baseline given".into());
                return out;
            }
            Baseline::Unusable(why) => {
                out.push(why.clone());
                return out;
            }
            Baseline::Metrics(old) => old,
        };
        let shown = |v: Option<f64>| v.map_or("n/a".to_owned(), |v| format!("{v:.1}"));
        let rows: Vec<String> = self
            .metrics(ctx.visual.mean().map(|m| m * 100.0))
            .into_iter()
            .map(|(name, new)| {
                let was = old.iter().find(|(n, _)| n == name).and_then(|(_, v)| *v);
                let verdict = match (was, new) {
                    (Some(a), Some(b)) => {
                        let scale = a.abs().max(b.abs()).max(1e-9);
                        if (b - a).abs() <= 0.01 * scale {
                            "= same"
                        } else if (b > a) == HIGHER_IS_BETTER.contains(&name) {
                            if b > a { "↑ better" } else { "↓ better" }
                        } else if b > a {
                            "↑ worse"
                        } else {
                            "↓ worse"
                        }
                    }
                    _ => "n/a",
                };
                format!("{name} {} → {} {verdict}", shown(was), shown(new))
            })
            .collect();
        for pair in rows.chunks(2) {
            out.push(pair.join("   |   "));
        }
        out
    }

    fn cost_section(&self) -> Vec<String> {
        let mut out = vec!["## 3. Frame cost (median world line)".to_owned()];
        let world: Vec<&Rate> = self.world().collect();
        let Some(r) = median(&world, |r| r.ms) else {
            out.push("n/a: no world frame-rate lines".into());
            return out;
        };
        let other = (r.ms - r.preparing - r.gpu - r.copy_back).max(0.0);
        let phases = [
            ("preparing", r.preparing),
            ("GPU", r.gpu),
            ("copy-back", r.copy_back),
            ("not in these three (waits, present, pacing)", other),
        ];
        let big = phases.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        out.push(format!(
            "{:.1} ms ({:.1} fps): preparing {:.1}, GPU {:.1}, copy-back {:.1}, other {:.1}",
            r.ms, r.fps, r.preparing, r.gpu, r.copy_back, other
        ));
        out.push(format!("largest: {} ({:.1} ms, {:.0}%)", big.0, big.1, 100.0 * big.1 / r.ms.max(1e-9)));
        out
    }

    fn draws_section(&self) -> Vec<String> {
        let mut out = vec!["## 4. Draws".to_owned()];
        let world: Vec<&Rate> = self.world().collect();
        let drawn = median(&world, |r| r.draws).map_or("n/a".to_owned(), |r| format!("{:.0}", r.draws));
        match self.cause_means() {
            Some([a, b, c]) => out.push(format!(
                "per frame: {drawn} drawn; dropped: refused {a:.1}, still compiling {b:.1}, other {c:.1}"
            )),
            None => out.push(format!("per frame: {drawn} drawn; dropped by cause: n/a (no cause lines)")),
        }
        let (lost, world) = self.lost();
        if lost.is_empty() {
            out.push("draws lost by pipeline/program: none named in the log".into());
            return out;
        }
        out.push(format!(
            "top {} lost per frame ({}; the host names its 40 largest per 5 s):",
            lost.len().min(PIPELINES),
            if world { "world lines" } else { "menus only, the world was not reached" }
        ));
        for l in lost.iter().take(PIPELINES) {
            out.push(format!("{:>7.1}  {:<9} {:<16} {}", l.per_frame, l.kind, l.id, cut(&l.reason, 70)));
        }
        out
    }

    fn kinds_section(&self) -> Vec<String> {
        let mut out = vec!["## 5. Refusals and untranslated, by draws or dispatches lost".to_owned()];
        let (lost, _) = self.lost();
        // reason -> (tag, area, programs, per frame, compute, other kinds)
        let mut groups = BTreeMap::<String, (&str, &str, usize, f64, bool, bool)>::new();
        for l in lost.iter().filter(|l| ["refused", "untranslated"].contains(&l.tag)) {
            let g = groups.entry(l.group.clone()).or_insert((l.tag, l.area, 0, 0.0, false, false));
            g.2 += 1;
            g.3 += l.per_frame;
            g.4 |= l.kind == "compute";
            g.5 |= l.kind != "compute";
        }
        let mut groups: Vec<_> = groups.into_iter().collect();
        groups.sort_by(|a, b| b.1.3.total_cmp(&a.1.3));
        for (reason, (tag, _, n, per_frame, compute, draw)) in groups.iter().take(KINDS) {
            let unit = match (compute, draw) {
                (true, false) => "dispatches",
                (false, true) => "draws",
                _ => "draws+dispatches",
            };
            out.push(format!("{per_frame:>7.1} {unit}/frame, {n} programs [{tag}]: {}", cut(reason, 80)));
        }
        if groups.len() > KINDS {
            out.push(format!("... and {} more kinds", groups.len() - KINDS));
        }
        let named: BTreeSet<&str> = lost.iter().map(|l| l.id.as_str()).collect();
        let quiet = |keys: Vec<&str>| {
            keys.iter().filter(|k| !hashes(k).any(|h| named.contains(h))).count()
        };
        out.push(format!(
            "logged: {} refused, {} untranslated; with no loss named: {} and {} (counts only)",
            self.refused.len(),
            self.untranslated.len(),
            quiet(self.refused.keys().map(String::as_str).collect()),
            quiet(self.untranslated.keys().map(String::as_str).collect()),
        ));
        out
    }

    fn visual_section(&self, ctx: &Context) -> Vec<String> {
        let v = &ctx.visual;
        let mut out = vec!["## 6. Visual evidence".to_owned(), format!("contact sheet: {}", v.sheet)];
        match v.mean() {
            None => out.push("detail: n/a (no frame could be measured)".into()),
            Some(m) => {
                let each: Vec<String> =
                    v.shares.iter().map(|(n, s)| format!("{n}: {:.1}%", s * 100.0)).collect();
                out.push(format!(
                    "pixels neither black nor flat, mean {:.1}% (frame: share) {}; not proof of geometry",
                    m * 100.0,
                    each.join(", ")
                ));
            }
        }
        out
    }

    fn stability_section(&self, areas: &[Area]) -> Vec<String> {
        let mut out = vec!["## 7. Stability".to_owned()];
        for a in areas.iter().filter(|a| {
            ["crash", "hang", "device removed", "host GPU failure", STEADY_AREA].contains(&a.name)
        }) {
            out.push(format!("{}: {} - {}", a.name, a.status.name(), cut(&a.detail, 90)));
        }
        match self.journal.first() {
            Some(first) => out.push(format!(
                "DRED/journal: {} lines, first: {}",
                self.journal.len(),
                cut(first, 80)
            )),
            None => out.push("DRED/journal: no such line in this log".into()),
        }
        out
    }

    fn next_section(&self, areas: &[Area], ctx: &Context) -> Vec<String> {
        struct Item {
            text: String,
            area: String,
            draws: f64,
            frames: usize,
            stability: bool,
        }
        let mut items = Vec::new();
        for a in areas.iter().filter(|a| {
            a.status == Status::Fail
                && ["crash", "hang", "device removed", "host GPU failure"].contains(&a.name)
        }) {
            items.push(Item {
                text: format!("{}: {}", a.name, cut(&a.detail, 70)),
                area: "stability (memory / device)".into(),
                draws: 0.0,
                frames: 0,
                stability: true,
            });
        }
        let (lost, _) = self.lost();
        let mut groups = BTreeMap::<String, (usize, f64, &str, bool)>::new();
        for l in &lost {
            let area = if l.tag == "other" { "pipeline" } else { l.area };
            let g = groups.entry(format!("{area}|{}", l.group)).or_insert((0, 0.0, area, true));
            g.0 += 1;
            g.1 += l.per_frame;
            g.3 &= l.kind == "compute";
        }
        for (key, (n, per_frame, area, compute)) in groups {
            let reason = key.split_once('|').map_or("", |(_, r)| r);
            let unit = if compute { "dispatches" } else { "draws" };
            items.push(Item {
                text: format!("{n} programs, {per_frame:.0} {unit}/frame lost: {}", cut(reason, 60)),
                area: area.into(),
                draws: per_frame,
                frames: 0,
                stability: false,
            });
        }
        let black = self.black.iter().filter(|&&f| f >= STARTUP_FRAMES).count();
        let empty = ctx.visual.shares.iter().filter(|s| s.1 < 0.001).count();
        if black + empty > 0 {
            items.push(Item {
                text: format!(
                    "{black} black frames after start, {empty} of {} sampled frames without detail",
                    ctx.visual.shares.len()
                ),
                area: "pipeline (what draws the picture)".into(),
                draws: 0.0,
                frames: black + empty,
                stability: false,
            });
        }
        items.sort_by(|a, b| {
            b.stability
                .cmp(&a.stability)
                .then(b.draws.total_cmp(&a.draws))
                .then(b.frames.cmp(&a.frames))
        });
        let mut out = vec![
            "## 8. Next".to_owned(),
            "rule: a failed stability area first (the run stopped), then draws or dispatches lost per frame, then frames lost; same area and reason merge".to_owned(),
        ];
        if items.is_empty() {
            out.push("nothing lost is named in this log".into());
        }
        for (i, item) in items.iter().take(3).enumerate() {
            out.push(format!("{}. [{}] {}", i + 1, item.area, item.text));
        }
        out
    }
}

/// Switches the runner always sets itself.
fn is_runner(name: &str) -> bool {
    ["VICTORY_LOG", "VICTORY_KEYS", "VICTORY_CAPTURE", "VICTORY_CAPTURE_FRAMES", "VICTORY_DATA_DIR"]
        .contains(&name)
}

fn cut(text: &str, longest: usize) -> String {
    match text.char_indices().nth(longest) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "2026-01-01T00:00:00Z";

    fn rate(draws: u32) -> String {
        format!(
            "{T}  INFO victory_gpu::host: 30.0 fps, 33.3 ms/frame (1.5 preparing: copies 0.1 ms \
             0.2 MB, textures 5.4 ms 12.7 MB; 3.9 on the GPU; 0.4 copying back 0.0 MB), {draws} \
             draws/frame, dropped 0 without a host pipeline (7 being made) and 0 indirect; 1 \
             root-view slots/frame\n"
        )
    }

    fn cause(refused: f64, compiling: f64, other: f64, why: &[(&str, f64)]) -> String {
        let mut s = format!(
            "{T}  INFO victory_gpu::host: dropped by cause: refused {refused}, compiling \
             {compiling}, other {other} draws/frame\n"
        );
        for (w, n) in why {
            s += &format!("{T}  INFO victory_gpu::host: dropped {n}/frame without a host pipeline: {w}\n");
        }
        s
    }

    fn window(why: &[(&str, f64)]) -> String {
        rate(2000) + &cause(10.0, 5.0, 1.0, why)
    }

    fn refusal(hash: &str, reason: &str) -> String {
        format!(
            "{T}  WARN victory_gpu::pipeline::host: host pipeline refused: wave size \
             shaders=[compute {hash}] wave=\"{reason}\"\n"
        )
    }

    fn ctx(requested: &[(&str, &str)], baseline: Baseline) -> Context {
        Context {
            build: "build commit: test".into(),
            requested: requested.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
            baseline,
            visual: Visual::none("none: test"),
        }
    }

    fn digest(text: &str, c: &Context) -> String {
        let log = Log::parse(text);
        log.digest(&log.judge(None, |_| None), c)
    }

    #[test]
    fn switches_round_trip_and_a_missing_one_warns() {
        let seen = format!(
            "{T}  INFO victory_gpu::host: process switches (2): VICTORY_A=\"1\" VICTORY_B=\"a \\\"q\\\" \\\\x\"\n"
        );
        let log = Log::parse(&seen);
        assert_eq!(
            log.switches,
            Some(vec![("VICTORY_A".into(), "1".into()), ("VICTORY_B".into(), "a \"q\" \\x".into())])
        );
        let d = digest(&seen, &ctx(&[("VICTORY_A", "1"), ("VICTORY_C", "2")], Baseline::Off));
        assert!(d.contains("WARNING: VICTORY_C was requested, not seen"), "{d}");
        assert!(!d.contains("VICTORY_A was requested"), "{d}");
        let d = digest(&seen, &ctx(&[("VICTORY_A", "9")], Baseline::Off));
        assert!(d.contains("VICTORY_A seen with another value"), "{d}");
    }

    #[test]
    fn a_log_without_the_echo_cannot_confirm_a_switch() {
        let d = digest(&window(&[]), &ctx(&[("VICTORY_X", "1")], Baseline::Off));
        assert!(d.contains("predates the switch echo"), "{d}");
        assert!(d.contains("WARNING: VICTORY_X requested; the log cannot confirm it"), "{d}");
    }

    #[test]
    fn losses_merge_by_reason_and_rank_by_draws() {
        let two = "requires waves of 64 to 64 lanes, the host has 32 to 32";
        let text = refusal("00000000000000a1", two)
            + &refusal("00000000000000a2", two)
            + &format!(
                "{T}  WARN victory_gpu::pipeline: shader does not translate; its pipelines' draws \
                 are dropped stage=\"pixel\" hash=00000000000000b1 error=GFX10 code at 0x4: odd \
                 thing is not supported cached=false ms=1\n"
            )
            + &(0..3)
                .map(|_| {
                    window(&[
                        ("refused [compute 00000000000000a1]", 100.0),
                        ("refused [compute 00000000000000a2]", 50.0),
                        ("untranslated pixel 00000000000000b1", 30.0),
                        ("being made", 400.0),
                        ("indirect kind 2: no host pipeline", 20.0),
                    ])
                })
                .collect::<String>();
        let d = digest(&text, &ctx(&[], Baseline::Off));
        // The two wave refusals are one item, with their dispatches added.
        assert!(d.contains("150.0 dispatches/frame, 2 programs [refused]"), "{d}");
        assert!(d.contains("30.0 draws/frame, 1 programs [untranslated]"), "{d}");
        let next: Vec<&str> = d.lines().skip_while(|l| !l.starts_with("## 8")).collect();
        assert!(next[2].starts_with("1. [pipeline]") && next[2].contains("400 draws"), "{d}");
        assert!(next[3].starts_with("2. [wave64] 2 programs, 150 dispatches"), "{d}");
        assert!(next[4].starts_with("3. [shader lowering] 1 programs, 30 draws"), "{d}");
    }

    #[test]
    fn a_failed_stability_area_comes_first() {
        let text = window(&[("being made", 5.0)])
            + &format!("{T}  ERROR victory_gpu::host: host GPU device removed: reason\n");
        let d = digest(&text, &ctx(&[], Baseline::Off));
        let next: Vec<&str> = d.lines().skip_while(|l| !l.starts_with("## 8")).collect();
        assert!(next[2].starts_with("1. [stability"), "{d}");
    }

    #[test]
    fn the_digest_never_passes_sixty_lines_and_keeps_its_next_section() {
        let mut text = window(&[]);
        let mut whys = Vec::new();
        for i in 0..80 {
            let hash = format!("{i:016x}");
            text += &refusal(&hash, &format!("reason number {i}"));
            whys.push((format!("refused [compute {hash}]"), 1.0 + f64::from(i)));
        }
        let whys: Vec<(&str, f64)> = whys.iter().map(|(w, n)| (w.as_str(), *n)).collect();
        for _ in 0..3 {
            text += &window(&whys);
        }
        let old = Baseline::Metrics(vec![("fps".into(), Some(10.0)); 1]);
        let d = digest(&text, &ctx(&[("VICTORY_A", "1"); 6], old));
        assert!(d.lines().count() <= MAX_LINES, "{}", d.lines().count());
        assert!(d.contains("## 8. Next"), "{d}");
        assert_eq!(d.lines().filter(|l| l.starts_with(|c: char| c.is_ascii_digit()) && l.contains(". [")).count(), 3, "{d}");
    }

    #[test]
    fn a_delta_has_arrows_and_what_the_baseline_lacks_is_n_a() {
        let text = window(&[]);
        let old = Baseline::Metrics(vec![("fps".into(), Some(15.0)), ("ms".into(), Some(33.3))]);
        let d = digest(&text, &ctx(&[], old));
        assert!(d.contains("fps 15.0 → 30.0 ↑ better"), "{d}");
        assert!(d.contains("ms 33.3 → 33.3 = same"), "{d}");
        assert!(d.contains("gpu_ms n/a → 3.9 n/a"), "{d}");
        let d = digest(&text, &ctx(&[], Baseline::Unusable("baseline has no metrics".into())));
        assert!(d.contains("baseline has no metrics"), "{d}");
    }

    #[test]
    fn metrics_survive_report_json() {
        let log = Log::parse(&(window(&[]) + &window(&[]) + &window(&[])));
        let json = log.json(&log.judge(None, |_| None), &log.metrics(None));
        let dir = std::env::temp_dir().join(format!("victory-base-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("report.json"), json).unwrap();
        let Baseline::Metrics(read) = read_baseline(&dir.join("report.json")) else { panic!() };
        let now = log.metrics(None);
        assert_eq!(read.len(), now.len());
        assert_eq!(read[0], ("fps".to_owned(), Some(30.0)));
        assert!(read.iter().any(|(n, v)| n == "detail_pct" && v.is_none()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
