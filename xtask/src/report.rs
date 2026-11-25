//! What a run's log says about each graphics area. Everything is read from
//! the log (and the frames it names), so a log can be judged again later;
//! what a log does not show is "unknown", never a pass.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

mod digest;
pub use digest::{Baseline, Context, read_baseline};
pub mod sheet;

/// Draws and dispatches asked for per frame that mean the menus draw. The
/// title's first screens ask for 3 to 7.
const MENU_DRAWS: f64 = 20.0;
/// Saved frames it takes to judge what the menus show.
const MENU_FRAMES: usize = 5;
/// The same for the world: menus ask for 60 at most, the world thousands.
const WORLD_DRAWS: f64 = 300.0;
/// World log lines (one per 5 s or slower) that anything passes from.
const WORLD_LINES: usize = 3;
/// The console's frame rate, less measuring noise; then playable.
const FPS_PASS: f64 = 27.0;
const FPS_WARN: f64 = 15.0;
/// Share of the draws and dispatches asked for that were dropped.
const DROPPED_PASS: f64 = 0.001;
const DROPPED_WARN: f64 = 0.05;
/// Distinct problems of one kind that still warn instead of failing.
const FEW: usize = 3;
/// Frames before this one are black while the title starts.
const STARTUP_FRAMES: u64 = 60;
/// Black frames in an otherwise drawn run that cuts and fades explain.
const BLACK_WARN: usize = 5;
/// A run whose last frame-rate line is older than this, in seconds, at its
/// end has hung (the lines are 5 s apart, and a frame longer at worst).
const HANG: f64 = 15.0;
/// A channel range this small over a whole frame is one flat color.
const FLAT_RANGE: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Unknown,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Warn => "warn",
            Status::Fail => "fail",
            Status::Unknown => "unknown",
        }
    }
}

pub struct Area {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
}

/// How the process ended, when we ran it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum End {
    /// Still running when its time was up.
    TimedOut,
    Exited(Option<i32>),
}

/// The log's "fps, ms/frame" line.
#[derive(Clone, PartialEq, Debug, Default)]
struct Rate {
    time: String,
    fps: f64,
    ms: f64,
    preparing: f64,
    gpu: f64,
    copy_back: f64,
    draws: f64,
    dropped: f64,
    making: f64,
    indirect: f64,
    /// Logged after the host GPU device was removed: frames of a run that
    /// goes on without the GPU, not a measure of the world.
    removed: bool,
}

impl Rate {
    fn parse(line: &str) -> Option<Rate> {
        Some(Rate {
            time: line.split_whitespace().next()?.to_owned(),
            fps: before(line, " fps, ")?,
            ms: before(line, " ms/frame (")?,
            preparing: before(line, " preparing: ")?,
            gpu: before(line, " on the GPU; ")?,
            copy_back: before(line, " copying back ")?,
            draws: before(line, " draws/frame, dropped ")?,
            dropped: before(line, " without a host pipeline (")?,
            making: before(line, " being made)")?,
            indirect: before(line, " indirect")?,
            removed: false,
        })
    }

    fn asked(&self) -> f64 {
        self.draws + self.dropped + self.indirect
    }

    fn dropped_share(&self) -> f64 {
        (self.dropped + self.indirect) / self.asked().max(1.0)
    }
}

/// A log line's time (`2026-01-31T12:00:00.5Z`), in Unix time.
fn unix_time(text: &str) -> Option<f64> {
    let (date, time) = text.strip_suffix('Z')?.split_once('T')?;
    let mut date = date.splitn(3, '-').map(|n| n.parse::<u32>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let mut time = time.splitn(3, ':').map(|n| n.parse::<f64>().ok());
    let (hour, minute, second) = (time.next()??, time.next()??, time.next()??);
    if !(1970..10000).contains(&year) || !(1..=12).contains(&month) || day > 31 {
        return None;
    }
    // Years start in March: the leap day is their last.
    let (y, m) = if month > 2 { (year, month - 3) } else { (year - 1, month + 9) };
    let days = y * 365 + y / 4 - y / 100 + y / 400 + (m * 153 + 2) / 5 + day;
    let days = f64::from(days) - 719_469.0;
    Some(((days * 24.0 + hour) * 60.0 + minute) * 60.0 + second)
}

/// Whether a saved image is a presented frame: `frame-<n>.png`, and not a
/// render target dumped with it.
fn presented(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let number = name.strip_prefix("frame-").and_then(|n| n.strip_suffix(".png"));
    number.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The number that ends where `marker` starts.
fn before(line: &str, marker: &str) -> Option<f64> {
    let head = &line[..line.find(marker)?];
    let start = head.rfind(|c: char| !c.is_ascii_digit() && c != '.').map_or(0, |i| i + 1);
    // A space or bracket: one byte.
    head.get(start..)?.parse().ok()
}

/// The value of a `key=value` field; quotes and brackets are kept whole.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(&format!(" {key}="))? + key.len() + 2;
    let rest = &line[start..];
    if let Some(quoted) = rest.strip_prefix('"') {
        return quoted.split('"').next();
    }
    if rest.starts_with('[') {
        return Some(&rest[..=rest.find(']')?]);
    }
    rest.split_whitespace().next()
}

/// The `wave` field: why a pipeline was refused before the host was asked.
/// It is the line's last field, and its text (any `Display`) is not quoted.
fn wave_reason(line: &str) -> Option<&str> {
    let value = line.split_once(" wave=")?.1.trim();
    Some(value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value))
}

/// Without terminal escape sequences (`ESC [ ... letter`).
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find("\x1b[") {
        out.push_str(&rest[..i]);
        rest = &rest[i + 2..];
        rest = rest.find(|c: char| c.is_ascii_alphabetic()).map_or("", |end| &rest[end + 1..]);
    }
    out.push_str(rest);
    out
}

#[derive(Default)]
pub struct Log {
    rates: Vec<Rate>,
    /// Draws per frame dropped as refused, still being made, other.
    causes: Vec<[f64; 3]>,
    /// The highest frame a line names.
    frames: u64,
    /// A host device, and a diagnostics build, were seen.
    device: bool,
    diagnostics: bool,
    /// The host device is the software adapter's.
    warp: bool,
    /// When the run ended, if we ran it (Unix time).
    pub ended: Option<f64>,
    /// The last time a line has.
    last: Option<f64>,
    /// Modules seen to log at info: their warnings are in the log too.
    informed: BTreeSet<String>,
    removed: Vec<String>,
    host_failures: Vec<String>,
    crashes: Vec<String>,
    /// Shaders: the error.
    refused: BTreeMap<String, String>,
    /// Shader hash: stage, error.
    untranslated: BTreeMap<String, (String, String)>,
    black: Vec<u64>,
    methods: BTreeSet<String>,
    other_methods: BTreeSet<String>,
    resources: BTreeSet<String>,
    /// Frames saved, and whether the world had been reached.
    captures: Vec<(PathBuf, bool)>,
    /// The stand-in pad, and a real one, were connected.
    stand_in: bool,
    pad: bool,
    /// For the digest: the adapter and its caps as logged, the switches the
    /// process saw (`None`: no such line), draws lost per log interval,
    /// and lines that name a removal journal.
    adapter_text: Option<String>,
    caps: Option<String>,
    switches: Option<Vec<(String, String)>>,
    blocks: Vec<DropBlock>,
    journal: Vec<String>,
}

/// One log interval's dropped draws: when it was in the world, the counts
/// by cause, and what the host named as lost (at most 40 reasons).
#[derive(Default)]
struct DropBlock {
    world: bool,
    why: Vec<(String, f64)>,
}

const UNHEARD: &str = "none, but nothing shows that the log would have them";

/// Messages of `victory_gpu` about a resource or view that was not made.
const NOT_MADE: &[&str] = &[
    "no host texture",
    "no size for this resource",
    "no footprint for this resource",
    "placed resource does not fit",
    "no host descriptor heap",
    "title slot past the host heap",
    "no free dynamic descriptor slots",
    "no upload buffer",
    "no shared buffer",
    "no swap chain",
    "cannot present this format",
    "texture not detiled",
    "view the host does not allow",
    "view format of another family",
    "view not on an element of its host copy",
    "unordered access to a buffer whose host copy shaders cannot write",
    "constant buffer not 256-byte aligned",
    "texture view not mirrored",
];

impl Log {
    /// Draws per frame dropped as refused, still compiling and other, from
    /// a "dropped by cause" line of the host.
    fn causes(line: &str) -> Option<[f64; 3]> {
        let (_, rest) = line.split_once("dropped by cause: refused ")?;
        let (refused, rest) = rest.split_once(", compiling ")?;
        let (compiling, rest) = rest.split_once(", other ")?;
        let other = rest.split_whitespace().next()?;
        Some([refused, compiling, other].map(|n| n.parse().unwrap_or(0.0)))
    }

    /// The mean of the cause lines, as a table row for the drops area.
    fn cause_table(&self) -> Option<String> {
        if self.causes.is_empty() {
            return None;
        }
        let n = self.causes.len() as f64;
        let mean = |i: usize| self.causes.iter().map(|c| c[i]).sum::<f64>() / n;
        Some(format!(
            "dropped draws per frame by cause (mean of {} lines): refused {:.1}, \
             still compiling {:.1}, other {:.1}",
            self.causes.len(),
            mean(0),
            mean(1),
            mean(2)
        ))
    }

    pub fn parse(text: &str) -> Log {
        let mut log = Log::default();
        for line in text.lines() {
            log.line(&plain(line));
        }
        log
    }

    fn line(&mut self, line: &str) {
        if self.journal.len() < 20 && (line.contains("DRED") || line.contains("removal journal")) {
            self.journal.push(clip(line));
        }
        if line.contains("VIctory: unhandled exception")
            || (line.contains("VIctory: unimplemented import") && line.contains(" called"))
            || (line.starts_with("thread '") && line.contains(" panicked at"))
        {
            self.crashes.push(line.to_owned());
        }
        if line.contains("device removed:") || line.contains("host GPU device removed") {
            self.removed.push(line.to_owned());
        } else if line.contains(" ERROR victory_gpu::host:") {
            self.host_failures.push(line.to_owned());
        }
        if let Some(at) = line.split_whitespace().next().and_then(unix_time) {
            self.last = Some(at);
        }
        if let Some((_, rest)) = line.split_once(" INFO ")
            && let Some((module, _)) = rest.trim_start().split_once(": ")
            && module.starts_with("victory_")
        {
            self.informed.insert(module.to_owned());
        }
        let warn = line.contains(" WARN ");
        if !warn && !line.contains(" INFO ") {
            return;
        }
        if line.contains("victory_gpu::host: ")
            && let Some(mut rate) = Rate::parse(line)
        {
            rate.removed = !self.removed.is_empty();
            self.rates.push(rate);
            return;
        }
        if line.contains("victory_hle::modules::gameinput: input device device=")
            && line.contains("connected=true")
        {
            // Devices 0 to 3 are the XInput slots, 4 the stand-in pad.
            match field(line, "device").and_then(|d| d.parse::<u32>().ok()) {
                Some(0..4) => self.pad = true,
                Some(4) => self.stand_in = true,
                _ => {}
            }
        }
        if line.contains("victory_gpu::capture: frame saved") {
            // The line's last field: a path may hold spaces.
            if let Some((_, path)) = line.split_once(" path=")
                && presented(path.trim())
            {
                let world = self.world().next().is_some();
                self.captures.push((path.trim().into(), world));
            }
            return;
        }
        if !line.contains(" victory_gpu::") {
            if warn && line.contains("implemented") {
                self.other_methods.insert(message(line));
            }
            return;
        }
        if let Some(frame) = field(line, "frame").and_then(|f| f.parse().ok()) {
            self.frames = self.frames.max(frame);
        }
        self.device |= line.contains("host D3D12 device ready");
        self.warp |= line.contains("on the software adapter (WARP)");
        self.diagnostics |= line.contains("copies per frame: ");
        if let Some(causes) = Self::causes(line) {
            self.causes.push(causes);
            let world = self.rates.last().is_some_and(|r| r.asked() >= WORLD_DRAWS && !r.removed);
            self.blocks.push(DropBlock { world, why: Vec::new() });
        }
        if let Some((_, why)) = line.split_once("/frame without a host pipeline: ")
            && let Some(n) = before(line, "/frame without a host pipeline: ")
            && let Some(block) = self.blocks.last_mut()
        {
            block.why.push((why.trim().to_owned(), n));
        }
        if let Some((_, rest)) = line.split_once("host D3D12 adapter adapter=") {
            let (name, rest) = rest.split_once(" vendor=").unwrap_or((rest, ""));
            let vram = rest.split_once("vram_mb=").map_or("?", |(_, v)| v.split_whitespace().next().unwrap_or("?"));
            self.adapter_text = Some(format!("{} ({vram} MB)", name.trim()));
        }
        if let Some((_, caps)) = line.split_once("host D3D12 caps: ") {
            self.caps = Some(caps.trim().to_owned());
        }
        if let Some((_, rest)) = line.split_once("process switches (")
            && let Some((_, list)) = rest.split_once("):")
        {
            self.switches = Some(parse_switches(list));
        }
        // Some refusals are logged once per reason; the drops name each.
        if let Some((_, shaders)) = line.split_once("without a host pipeline: refused ") {
            let reason = || "reason not logged".to_owned();
            self.refused.entry(shaders.trim().to_owned()).or_insert_with(reason);
        }
        if !warn {
            return;
        }
        if line.contains("no host D3D12 device") || line.contains("no host GPU runs them") {
            self.host_failures.push(line.to_owned());
        } else if line.contains("pipeline refused") || line.contains("root signature refused") {
            let shaders = field(line, "shaders").unwrap_or("?").to_owned();
            // A refusal made before the host was asked has no `error`: its
            // reason is the `wave` field, or else the message.
            let reason = match wave_reason(line) {
                Some(wave) if !line.contains(" error=") => clip(wave),
                _ if line.contains(" error=") => error(line),
                _ => message(line),
            };
            self.refused.insert(shaders, reason);
        } else if line.contains("no host pipeline: ") {
            let shaders = field(line, "shaders").unwrap_or("?").to_owned();
            self.refused.insert(shaders, message(line));
        } else if line.contains("shader does not translate") {
            let get = |key| field(line, key).unwrap_or("?").to_owned();
            self.untranslated.insert(get("hash"), (get("stage"), error(line)));
        } else if line.contains("a black frame after a frame that was not") {
            self.black.extend(field(line, "frame").and_then(|f| f.parse::<u64>().ok()));
        } else if line.contains("implemented") {
            self.methods.insert(message(line));
        } else if NOT_MADE.iter().any(|m| line.contains(m)) {
            self.resources.insert(message(line));
        }
    }

    /// Whether a warning of these modules would be in the log. We run the
    /// title with everything at info; what filtered another log is unknown.
    fn heard(&self, modules: &[&str]) -> bool {
        self.ended.is_some()
            || (!modules.is_empty() && modules.iter().all(|&m| self.informed.contains(m)))
    }

    /// Seconds without a frame-rate line at the end of the run, or of the
    /// log if the run's end is not known.
    fn silence(&self) -> Option<f64> {
        let rate = unix_time(&self.rates.last()?.time)?;
        Some(self.ended.or(self.last)? - rate)
    }

    /// Saved frames from the world, in the order saved.
    pub fn world_frames(&self) -> Vec<PathBuf> {
        self.captures.iter().filter(|c| c.1).map(|c| c.0.clone()).collect()
    }

    fn world(&self) -> impl Iterator<Item = &Rate> {
        self.rates.iter().filter(|r| r.asked() >= WORLD_DRAWS && !r.removed)
    }

    /// The report's areas. `flat` says whether a saved frame is one color
    /// (`None`: it cannot be read).
    pub fn judge(&self, end: Option<End>, flat: impl Fn(&Path) -> Option<bool>) -> Vec<Area> {
        let world: Vec<&Rate> = self.world().collect();
        // A clean log of less of the world than this shows nothing yet.
        let settled = world.len() >= WORLD_LINES;
        let silence = self.silence();
        let hung = silence.is_some_and(|s| s > HANG);
        let mut areas = Vec::new();
        let mut add = |name, (status, detail)| areas.push(Area { name, status, detail });

        add("menu reached", match self.rates.iter().find(|r| r.asked() >= MENU_DRAWS) {
            Some(r) => (Status::Pass, format!("drawing at {} (inferred from draw counts)", r.time)),
            None if self.rates.is_empty() => (Status::Unknown, "no frame-rate lines".into()),
            None => (Status::Fail, "never more draws than the first screens".into()),
        });

        let most = self.rates.iter().map(Rate::asked).fold(0.0, f64::max);
        add("world reached", match world.first() {
            Some(r) => (
                Status::Pass,
                format!("at {}, {} lines (inferred from draw counts)", r.time, world.len()),
            ),
            None if self.rates.is_empty() => (Status::Unknown, "no frame-rate lines".into()),
            None => (Status::Fail, format!("at most {most:.0} draws/frame asked for")),
        });

        add("hang", match silence {
            None => (Status::Unknown, "no frame-rate lines".into()),
            Some(s) if hung => (Status::Fail, format!("no frame-rate line for the last {s:.0} s")),
            Some(_) if self.ended.is_none() => {
                (Status::Unknown, "the log does not say when the run ended".into())
            }
            Some(s) => (Status::Pass, format!("last frame-rate line {s:.0} s before the end")),
        });

        add("crash", match (self.crashes.first(), end) {
            (Some(first), _) => (Status::Fail, clip(first)),
            (None, Some(End::TimedOut)) if silence.is_none() => {
                (Status::Unknown, "no crash line, but no frame-rate line either".into())
            }
            (None, Some(End::TimedOut)) if hung => {
                (Status::Unknown, "no crash line, but it hung".into())
            }
            (None, Some(End::TimedOut)) => {
                (Status::Pass, "running until its time was up".into())
            }
            (None, Some(End::Exited(code))) => {
                let code = code.map_or("no code".into(), |c| format!("{c:#x}"));
                (Status::Fail, format!("ended by itself, exit code {code}"))
            }
            (None, None) => {
                (Status::Unknown, "no crash line; the log does not say how it ended".into())
            }
        });

        add("device removed", match self.removed.first() {
            Some(first) => (Status::Fail, clip(first)),
            None if !self.device => (Status::Unknown, "no host device line".into()),
            None if hung => (Status::Unknown, "not said, but it hung".into()),
            None if !self.heard(&["victory_gpu::host"]) => (Status::Unknown, UNHEARD.into()),
            None if !settled => (Status::Unknown, "no, but too little of the world ran".into()),
            None => (Status::Pass, "no".into()),
        });

        add("host GPU failure", match self.host_failures.first() {
            Some(first) => (Status::Fail, clip(first)),
            None if hung => (Status::Unknown, "not said, but it hung".into()),
            None if !self.heard(&["victory_gpu::host", "victory_gpu::queue"]) => {
                (Status::Unknown, UNHEARD.into())
            }
            None if !settled => (Status::Unknown, "none, but too little of the world ran".into()),
            None => (Status::Pass, "none".into()),
        });

        add("world frame rate", match median(&world, |r| r.fps) {
            None => (Status::Unknown, "no world frames".into()),
            Some(r) => {
                let few = !settled;
                let status = match r.fps {
                    _ if few || hung => Status::Unknown,
                    fps if fps >= FPS_PASS => Status::Pass,
                    fps if fps >= FPS_WARN => Status::Warn,
                    _ => Status::Fail,
                };
                let detail = format!(
                    "{:.1} fps, {:.1} ms: {:.1} preparing, {:.1} GPU, {:.1} copying back \
                     (median of {} world lines{})",
                    r.fps,
                    r.ms,
                    r.preparing,
                    r.gpu,
                    r.copy_back,
                    world.len(),
                    match (hung, few) {
                        (true, _) => ", before it hung",
                        (_, true) => ", too few to judge",
                        _ => "",
                    }
                );
                (status, detail)
            }
        });

        let (status, mut detail) = self.drops(&world);
        if let Some(table) = self.cause_table() {
            detail = format!("{detail}; {table}");
        }
        add("drawn vs dropped", (status, detail));

        // What is clean passes only if the log has those modules' warnings.
        let count = |n: usize, detail: String, modules: &[&str]| match n {
            0 if !self.heard(modules) => (Status::Unknown, UNHEARD.into()),
            0 if settled => (Status::Pass, "none".into()),
            0 => (Status::Unknown, "none, but too little of the world ran".into()),
            n if n <= FEW => (Status::Warn, format!("{n}: {detail}")),
            n => (Status::Fail, format!("{n}: {detail}")),
        };
        // Most of these are logged once per reason, however often they happen.
        let set = |s: &BTreeSet<String>, modules: &[&str]| {
            let reasons = format!("distinct reasons: {}", tally(s.iter().map(String::as_str)));
            count(s.len(), reasons, modules)
        };

        let reasons = tally(self.refused.values().map(String::as_str));
        add("pipelines refused", count(self.refused.len(), reasons, &["victory_gpu::pipeline::host"]));

        const STAGES: [(&str, &str); 4] = [
            ("vertex", "untranslated vertex"),
            ("pixel", "untranslated pixel"),
            ("mesh", "untranslated mesh"),
            ("compute", "untranslated compute"),
        ];
        let untranslated = |keep: &dyn Fn(&str) -> bool| {
            let errors = self.untranslated.values().filter(|(s, _)| keep(s));
            let errors: Vec<&str> = errors.map(|(_, e)| e.as_str()).collect();
            count(errors.len(), tally(errors.iter().copied()), &["victory_gpu::pipeline"])
        };
        for (stage, name) in STAGES {
            add(name, untranslated(&|s| s == stage));
        }
        add("untranslated other", untranslated(&|s| STAGES.iter().all(|(stage, _)| s != *stage)));

        let black: Vec<u64> =
            self.black.iter().copied().filter(|&f| f >= STARTUP_FRAMES).collect();
        add("black frames", match black.len() {
            0 if !self.diagnostics => (Status::Unknown, "not a diagnostics build's log".into()),
            0 if !self.heard(&["victory_gpu::queue"]) => (Status::Unknown, UNHEARD.into()),
            0 if !settled => (Status::Unknown, "none, but too little of the world ran".into()),
            0 => (Status::Pass, "none".into()),
            n => (
                if n <= BLACK_WARN { Status::Warn } else { Status::Fail },
                format!("{n} after a drawn frame (cuts, fades or glitches): {}", list(&black)),
            ),
        });

        let judged: Vec<bool> =
            self.captures.iter().filter(|c| c.1).filter_map(|c| flat(&c.0)).collect();
        let n = judged.iter().filter(|&&f| f).count();
        let status = match n {
            _ if judged.is_empty() => Status::Unknown,
            0 => Status::Pass,
            n if 2 * n <= judged.len() => Status::Warn,
            _ => Status::Fail,
        };
        add("flat frames", (status, format!("{n} of {} frames saved in the world", judged.len())));

        // The first half holds the black frames of the start.
        let early: Vec<_> = self.captures.iter().filter(|c| !c.1).collect();
        let shown: Vec<bool> =
            early[early.len() / 2..].iter().filter_map(|c| flat(&c.0)).collect();
        let n = shown.iter().filter(|&&f| f).count();
        let status = match n {
            _ if shown.len() < MENU_FRAMES => Status::Unknown,
            n if 2 * n > shown.len() => Status::Fail,
            _ => Status::Pass,
        };
        let detail = format!("{n} of the last {} frames saved before the world are flat", shown.len());
        add("menu picture", (status, detail));

        add("unimplemented graphics", set(&self.methods, &["victory_gpu::object", "victory_gpu::list"]));
        // Not graphics (and from any module, so heard in no other log), so never a failure, but a missing call explains much.
        add("unimplemented other", match set(&self.other_methods, &[]) {
            (Status::Fail, detail) => (Status::Warn, detail),
            other => other,
        });
        let modules = ["victory_gpu::resource", "victory_gpu::descriptor::host"];
        add("textures/views not made", set(&self.resources, &modules));
        areas
    }

    fn drops(&self, world: &[&Rate]) -> (Status, String) {
        let detail = |r: &Rate| {
            format!(
                "{:.0} drawn, {:.0} dropped without a pipeline ({:.0} pipelines being made), \
                 {:.0} indirect dropped per frame ({:.1}%)",
                r.draws,
                r.dropped,
                r.making,
                r.indirect,
                100.0 * r.dropped_share()
            )
        };
        if let Some(r) = median(world, Rate::dropped_share) {
            let status = match r.dropped_share() {
                share if share > DROPPED_WARN => Status::Fail,
                share if share > DROPPED_PASS => Status::Warn,
                _ if world.len() >= WORLD_LINES => Status::Pass,
                _ => Status::Unknown,
            };
            return (status, detail(r));
        }
        let worst = |a: &&Rate, b: &&Rate| a.dropped_share().total_cmp(&b.dropped_share());
        match self.rates.iter().max_by(worst) {
            // Of the menus the worst line: what they drop for a moment is
            // worth a look, not a failure.
            Some(r) if r.dropped_share() > DROPPED_PASS => {
                (Status::Warn, format!("menus at worst: {}", detail(r)))
            }
            Some(_) => (Status::Unknown, "none dropped, but only menus were drawn".into()),
            None => (Status::Unknown, "no frame-rate lines".into()),
        }
    }

    /// Scripted keys become pad buttons only through the stand-in pad.
    fn pad(&self) -> &'static str {
        match (self.pad, self.stand_in) {
            (true, _) => "connected: scripted keys did not become buttons while it was",
            (false, true) => "none: the stand-in pad took the keys",
            (false, false) => "unknown: the log names no pad",
        }
    }

    fn adapter(&self) -> &'static str {
        if self.warp {
            "software (WARP)"
        } else if self.device {
            "not the software one; the log does not name it"
        } else {
            "unknown: the log does not name it"
        }
    }

    pub fn text(&self, areas: &[Area]) -> String {
        let count = |s| areas.iter().filter(|a| a.status == s).count();
        let mut out = format!(
            "{} fail, {} warn, {} unknown, {} pass; last frame named {}\n",
            count(Status::Fail),
            count(Status::Warn),
            count(Status::Unknown),
            count(Status::Pass),
            self.frames
        );
        let _ = writeln!(out, "XInput pad: {}", self.pad());
        let _ = writeln!(out, "adapter: {}", self.adapter());
        for a in areas {
            let _ = writeln!(out, "{:<8} {:<24} {}", a.status.name(), a.name, a.detail);
        }
        out
    }

    pub fn json(&self, areas: &[Area], metrics: &[(&str, Option<f64>)]) -> String {
        let objects = |rows: Vec<Vec<(&str, &str)>>| {
            let rows = rows.iter().map(|row| {
                let fields: Vec<_> =
                    row.iter().map(|(k, v)| format!("{}: {}", quote(k), quote(v))).collect();
                format!("\n    {{{}}}", fields.join(", "))
            });
            rows.collect::<Vec<_>>().join(",")
        };
        let judged = areas.iter().map(|a| {
            vec![("area", a.name), ("status", a.status.name()), ("detail", a.detail.as_str())]
        });
        let untranslated = self.untranslated.iter().map(|(hash, (stage, error))| {
            vec![("hash", hash.as_str()), ("stage", stage), ("error", error)]
        });
        let refused = self.refused.iter().map(|(shaders, error)| {
            vec![("shaders", shaders.as_str()), ("error", error)]
        });
        // One per line, for the digest's baseline reader (`null`: unavailable).
        let metrics: Vec<String> = metrics
            .iter()
            .map(|(k, v)| format!("\n    {}: {}", quote(k), v.map_or("null".into(), |v| v.to_string())))
            .collect();
        format!(
            "{{\n  \"frames\": {},\n  \"xinput_pad\": {},\n  \"adapter\": {},\n  \"metrics\": \
             {{{}\n  }},\n  \"areas\": \
             [{}\n  ],\n  \"untranslated\": [{}\n  ],\n  \"refused\": [{}\n  ]\n}}\n",
            self.frames,
            quote(self.pad()),
            quote(self.adapter()),
            metrics.join(","),
            objects(judged.collect()),
            objects(untranslated.collect()),
            objects(refused.collect())
        )
    }
}

/// `NAME="value" NAME="value"` as the host logs its switches (values in
/// Rust debug quoting).
fn parse_switches(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = list.trim();
    while let Some((name, tail)) = rest.split_once("=\"") {
        let mut value = String::new();
        let mut chars = tail.char_indices();
        let mut end = tail.len();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    end = i + 1;
                    break;
                }
                '\\' => {
                    if let Some((_, e)) = chars.next() {
                        value.push(if e == 'n' { '\n' } else { e });
                    }
                }
                c => value.push(c),
            }
        }
        out.push((name.trim().to_owned(), value));
        rest = tail[end..].trim_start();
    }
    out
}

fn median<'a>(rates: &[&'a Rate], by: impl Fn(&Rate) -> f64) -> Option<&'a Rate> {
    let mut sorted = rates.to_vec();
    sorted.sort_by(|a, b| by(a).total_cmp(&by(b)));
    sorted.get(sorted.len() / 2).copied()
}

/// What follows the target, without the time.
fn message(line: &str) -> String {
    let start = line.find(" victory_").unwrap_or(0);
    let rest = line[start..].trim();
    clip(rest.split_once(": ").map_or(rest, |(_, m)| m))
}

/// The `error` field, without the place in the code or the fields after.
fn error(line: &str) -> String {
    let Some((_, text)) = line.split_once(" error=") else {
        return "?".into();
    };
    let text = text.split_once(" shaders=").map_or(text, |(e, _)| e);
    let text = match text.split_once(" code at 0x") {
        Some((_, rest)) => rest.split_once(": ").map_or(text, |(_, e)| e),
        None => text,
    };
    clip(text)
}

fn clip(text: &str) -> String {
    const LONGEST: usize = 200;
    match text.char_indices().nth(LONGEST) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_owned(),
    }
}

/// The most common entries, with their counts.
fn tally<'a>(entries: impl Iterator<Item = &'a str>) -> String {
    const SHOWN: usize = 4;
    let mut counts = BTreeMap::<&str, usize>::new();
    for e in entries {
        *counts.entry(e).or_default() += 1;
    }
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    let shown = counts.iter().take(SHOWN).map(|(e, n)| match n {
        1 => (*e).to_owned(),
        n => format!("{n}x {e}"),
    });
    let mut text = shown.collect::<Vec<_>>().join("; ");
    if counts.len() > SHOWN {
        let _ = write!(text, "; {} more kinds", counts.len() - SHOWN);
    }
    text
}

fn list(frames: &[u64]) -> String {
    const SHOWN: usize = 10;
    let shown: Vec<_> = frames.iter().take(SHOWN).map(u64::to_string).collect();
    shown.join(", ") + if frames.len() > SHOWN { ", ..." } else { "" }
}

/// A JSON string.
fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' | '\\' => out.extend(['\\', c]),
            c if c < ' ' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether a frame saved by the diagnostics build is one color; `None` for
/// anything but the 8-bit RGB, filter 0, single chunk files it writes.
pub fn flat_png(png: &[u8]) -> Option<bool> {
    let mut rest = png.strip_prefix(b"\x89PNG\r\n\x1a\n")?;
    let (mut width, mut data) = (0, None);
    while rest.len() >= 12 {
        let len = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
        let body = rest.get(8..8usize.checked_add(len)?)?;
        match &rest[4..8] {
            b"IHDR" => {
                width = u32::from_be_bytes(body.get(..4)?.try_into().ok()?) as usize;
                if body.get(8..13)? != [8, 2, 0, 0, 0] {
                    return None;
                }
            }
            b"IDAT" if data.is_none() => data = Some(body),
            b"IDAT" => return None,
            _ => {}
        }
        rest = rest.get(len.checked_add(12)?..)?;
    }
    let row = width.checked_mul(3)?.checked_add(1)?;
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(data?).ok()?;
    if width == 0 || raw.is_empty() || raw.len() % row != 0 {
        return None;
    }
    let (mut low, mut high) = ([u8::MAX; 3], [0; 3]);
    for line in raw.chunks_exact(row) {
        if line[0] != 0 {
            return None;
        }
        for pixel in line[1..].as_chunks::<3>().0 {
            for c in 0..3 {
                low[c] = low[c].min(pixel[c]);
                high[c] = high[c].max(pixel[c]);
            }
        }
    }
    Some((0..3).all(|c| high[c] - low[c] <= FLAT_RANGE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(fps: f64, draws: u32, dropped: u32, indirect: u32) -> String {
        format!(
            "2026-01-01T00:00:00Z \x1b[32m INFO\x1b[0m victory_gpu::host: {fps:.1} fps, 33.3 \
             ms/frame (1.5 preparing: copies 0.1 ms 0.2 MB, textures 5.4 ms 12.7 MB; 3.9 on the \
             GPU; 0.4 copying back 0.0 MB), {draws} draws/frame, dropped {dropped} without a host \
             pipeline (7 being made) and {indirect} indirect\n"
        )
    }

    fn gpu(level: &str, text: &str) -> String {
        format!("2026-01-01T00:00:00Z {level} victory_gpu::{text}\n")
    }

    /// When every synthetic line was logged.
    const THEN: f64 = 1_767_225_600.0;

    /// As we run it, up to `end`; or the log of another run.
    fn judge(text: &str, end: Option<End>) -> Vec<Area> {
        judge_at(text, end, end.map(|_| THEN + 5.0))
    }

    fn judge_at(text: &str, end: Option<End>, ended: Option<f64>) -> Vec<Area> {
        let mut log = Log::parse(text);
        log.ended = ended;
        log.judge(end, |_| None)
    }

    fn status(areas: &[Area], name: &str) -> Status {
        areas.iter().find(|a| a.name == name).unwrap().status
    }

    #[test]
    fn frames_after_a_device_removal_are_not_world_lines() {
        let text = rate(30.0, 600, 0, 0)
            + &gpu("ERROR", "host: host GPU device removed after a submission of 1 lists")
            + &rate(0.4, 600, 0, 0);
        let log = Log::parse(&text);
        assert_eq!(log.world().count(), 1);
    }

    fn healthy() -> String {
        gpu("INFO", "host: host D3D12 device ready")
            + &gpu("INFO", "copystat: copies per frame: 1.0 submissions; 1 title copies")
            + &rate(30.0, 39, 0, 0)
            + &rate(29.9, 2000, 0, 0).repeat(3)
    }

    #[test]
    fn a_rate_line_is_read_whole() {
        let r = Rate::parse(&plain(&rate(24.3, 598, 1867, 3264))).unwrap();
        assert_eq!((r.fps, r.ms, r.preparing, r.gpu, r.copy_back), (24.3, 33.3, 1.5, 3.9, 0.4));
        assert_eq!((r.draws, r.dropped, r.making, r.indirect), (598.0, 1867.0, 7.0, 3264.0));
        assert_eq!(Rate::parse("x INFO victory_gpu::host: 1.0 fps, whatever"), None);
    }

    #[test]
    fn a_healthy_run_passes_and_a_silent_log_is_unknown() {
        for a in judge(&healthy(), Some(End::TimedOut)) {
            // No frame was saved, so nothing shows that they are not flat.
            let unknown = ["flat frames", "menu picture"].contains(&a.name);
            let wanted = if unknown { Status::Unknown } else { Status::Pass };
            assert_eq!(a.status, wanted, "{}: {}", a.name, a.detail);
        }
        for a in judge("", None) {
            assert_eq!(a.status, Status::Unknown, "{}", a.name);
        }
    }

    #[test]
    fn menus_alone_pass_nothing_about_the_world() {
        for a in judge(&rate(30.0, 39, 0, 0).repeat(9), None) {
            let wanted = match a.name {
                "menu reached" => Status::Pass,
                "world reached" => Status::Fail,
                _ => Status::Unknown,
            };
            assert_eq!(a.status, wanted, "{}", a.name);
        }
        let first = judge(&rate(30.0, 6, 0, 0), None);
        assert_eq!(status(&first, "menu reached"), Status::Fail);
        let drops = judge(&rate(30.0, 39, 5, 0), None);
        assert_eq!(status(&drops, "drawn vs dropped"), Status::Warn);
    }

    #[test]
    fn rates_and_drops_are_judged_by_the_median() {
        let run = |fps: [f64; 3], dropped: u32| {
            let lines: String = fps.iter().map(|&f| rate(f, 1000, dropped, 0)).collect();
            judge(&lines, None)
        };
        let fps = |fps| status(&run(fps, 0), "world frame rate");
        assert_eq!(fps([1.0, 30.0, 30.0]), Status::Pass);
        assert_eq!(fps([30.0, 20.0, 1.0]), Status::Warn);
        assert_eq!(fps([30.0, 2.0, 1.0]), Status::Fail);
        let one = judge(&rate(1.0, 1000, 0, 0), None);
        assert_eq!(status(&one, "world frame rate"), Status::Unknown);
        assert_eq!(status(&run([30.0; 3], 10), "drawn vs dropped"), Status::Warn);
        assert_eq!(status(&run([30.0; 3], 100), "drawn vs dropped"), Status::Fail);
        // A world of which nothing is drawn is still the world.
        let undrawn = judge(&rate(2.0, 0, 0, 900), None);
        assert_eq!(status(&undrawn, "drawn vs dropped"), Status::Fail);
    }

    #[test]
    fn crashes_and_a_removed_device_fail() {
        for line in [
            "VIctory: unhandled exception 0xc0000005 at title+0x1 (read 0x0: unmapped)\n",
            "VIctory: unimplemented import a.dll!Missing called\n",
            "thread 'main' panicked at src/x.rs:1:2:\n",
        ] {
            let areas = judge(&(healthy() + line), Some(End::TimedOut));
            assert_eq!(status(&areas, "crash"), Status::Fail, "{line}");
        }
        let first_chance = healthy() + "VIctory: first-chance exception 0x1 at title+0x1\n";
        let crash = |text: &str, end| status(&judge(text, end), "crash");
        assert_eq!(crash(&first_chance, Some(End::TimedOut)), Status::Pass);
        assert_eq!(crash(&healthy(), Some(End::Exited(Some(0)))), Status::Fail);
        assert_eq!(crash(&healthy(), None), Status::Unknown);
        let removed = healthy() + &gpu("ERROR", "host: host GPU device removed after a submission");
        assert_eq!(status(&judge(&removed, None), "device removed"), Status::Fail);
    }

    #[test]
    fn problems_are_counted_once_each() {
        let refused = "pipeline::host: host pipeline refused error=The parameter is incorrect. \
                       (0x80070057) shaders=[compute 01]";
        let untranslated = |hash: &str| {
            let text = format!(
                "pipeline: shader does not translate; its pipelines' draws are dropped \
                 stage=\"pixel\" hash={hash} error=GFX10 code at 0x1{hash}: op is not supported"
            );
            gpu("WARN", &text)
        };
        let text = healthy()
            + &gpu("WARN", refused).repeat(5)
            + &["a", "b", "c", "d"].map(untranslated).concat()
            + &gpu("WARN", "object: method not implemented object=\"device\" slot=0x218")
            + &gpu("WARN", "resource: no host texture error=x desc=ResourceDesc { width: 1 }")
            + &gpu("WARN", "descriptor::host: view the host does not allow format=3");
        let mut log = Log::parse(&text);
        log.ended = Some(THEN);
        let areas = log.judge(Some(End::TimedOut), |_| None);
        for (name, wanted, detail) in [
            ("pipelines refused", Status::Warn, "1: The parameter is incorrect. (0x80070057)"),
            ("untranslated pixel", Status::Fail, "4: 4x op is not supported"),
            ("untranslated compute", Status::Pass, "none"),
            ("textures/views not made", Status::Warn, "2: distinct reasons: "),
            ("unimplemented graphics", Status::Warn, "1: distinct reasons: method not implemented"),
        ] {
            let a = areas.iter().find(|a| a.name == name).unwrap();
            assert_eq!(a.status, wanted, "{name}");
            assert!(a.detail.starts_with(detail), "{name}: {}", a.detail);
        }
        let json = log.json(&areas, &[]);
        assert!(json.contains(r#"{"shaders": "[compute 01]", "error": "The parameter"#), "{json}");
        assert!(json.contains(r#"object=\"device\""#), "{json}");
    }

    #[test]
    fn black_frames_are_judged_past_startup() {
        let black = |frame: u64| {
            let text =
                format!("queue: a black frame after a frame that was not frame={frame} draws=1");
            gpu("WARN", &text)
        };
        let with = |lines: &str| status(&judge(&(healthy() + lines), Some(End::TimedOut)), "black frames");
        assert_eq!(with(&black(1)), Status::Pass);
        assert_eq!(with(&black(900)), Status::Warn);
        assert_eq!(with(&(900..910).map(black).collect::<String>()), Status::Fail);
        // A release build does not look for them.
        let release = judge(&rate(30.0, 2000, 0, 0).repeat(3), Some(End::TimedOut));
        assert_eq!(status(&release, "black frames"), Status::Unknown);
    }

    #[test]
    fn only_frames_saved_in_the_world_are_looked_at() {
        let saved = |name: &str| {
            gpu("INFO", &format!("capture: frame saved path=/a b/frame-{name}.png"))
        };
        let log = Log::parse(&(saved("1") + &healthy() + &saved("2") + &saved("3")));
        let flat = |flat: &[&str]| {
            let areas = log.judge(None, |p| Some(flat.iter().any(|f| p.ends_with(f))));
            status(&areas, "flat frames")
        };
        assert_eq!(flat(&["frame-1.png"]), Status::Pass);
        assert_eq!(flat(&["frame-2.png"]), Status::Warn);
        assert_eq!(flat(&["frame-2.png", "frame-3.png"]), Status::Fail);
        assert_eq!(status(&log.judge(None, |_| None), "flat frames"), Status::Unknown);
    }

    /// Menus that draw but show nothing: the last frames are flat, not only
    /// those of the start.
    #[test]
    fn flat_menus_fail() {
        let saved = |n: u32| gpu("INFO", &format!("capture: frame saved path=/a/frame-{n}.png"));
        let picture = |frames: u32, flat_to: u32| {
            let text = rate(30.0, 39, 0, 0) + &(1..=frames).map(saved).collect::<String>();
            let flat = |p: &Path| {
                let n: u32 = p.file_stem()?.to_str()?.strip_prefix("frame-")?.parse().ok()?;
                Some(n <= flat_to)
            };
            status(&Log::parse(&text).judge(None, flat), "menu picture")
        };
        assert_eq!(picture(20, 10), Status::Pass);
        assert_eq!(picture(20, 19), Status::Fail);
        assert_eq!(picture(4, 4), Status::Unknown);
    }

    #[test]
    fn dumped_targets_are_not_frames() {
        let dump = "capture: frame saved path=C:\\a b\\frame-64-03-0x1f0-512x512-f28.png";
        let text = healthy() + &gpu("INFO", dump);
        let flat = |text: &str| {
            status(&Log::parse(text).judge(None, |_| Some(true)), "flat frames")
        };
        assert_eq!(flat(&text), Status::Unknown);
        let frame = gpu("INFO", "capture: frame saved path=C:\\a b\\frame-64.png");
        assert_eq!(flat(&(text + &frame)), Status::Fail);
    }

    #[test]
    fn a_log_passes_only_what_it_would_have_said() {
        // The frame-rate lines are the host module's.
        let shown = [
            "menu reached",
            "world reached",
            "world frame rate",
            "drawn vs dropped",
            "device removed",
        ];
        for a in judge(&healthy(), None) {
            let wanted = if shown.contains(&a.name) { Status::Pass } else { Status::Unknown };
            assert_eq!(a.status, wanted, "{}", a.name);
        }
        let areas = judge(&(healthy() + &gpu("INFO", "pipeline: first bind pipeline=0x1")), None);
        assert_eq!(status(&areas, "untranslated pixel"), Status::Pass);
        // Another module, though its name starts the same.
        assert_eq!(status(&areas, "pipelines refused"), Status::Unknown);
    }

    #[test]
    fn a_run_that_stops_presenting_passes_nothing_about_its_end() {
        let end = Some(End::TimedOut);
        let areas = judge_at(&healthy(), end, Some(THEN + 60.0));
        assert_eq!(status(&areas, "hang"), Status::Fail);
        for name in ["crash", "device removed", "host GPU failure", "world frame rate"] {
            assert_eq!(status(&areas, name), Status::Unknown, "{name}");
        }
        assert_eq!(status(&judge_at(&healthy(), end, Some(THEN + 14.0)), "hang"), Status::Pass);
        // A log whose other lines go on shows it too.
        let later = healthy() + "2026-01-01T00:01:00.5Z  INFO victory_hle::modules::net: x\n";
        assert_eq!(status(&judge(&later, None), "hang"), Status::Fail);
        assert_eq!(status(&judge(&healthy(), None), "hang"), Status::Unknown);
    }

    #[test]
    fn times_are_read() {
        assert_eq!(unix_time("2026-01-01T00:00:00Z"), Some(THEN));
        assert_eq!(unix_time("1970-01-01T00:00:01.5Z"), Some(1.5));
        assert_eq!(unix_time("2024-03-01T00:00:00Z"), Some(1_709_251_200.0));
        assert_eq!(unix_time("2024-02-29T01:00:00Z"), Some(1_709_168_400.0));
        for bad in ["", "Z", "bound", "2024-13-01T00:00:00Z", "2024-01T00:00:00Z"] {
            assert_eq!(unix_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn other_host_failures_are_not_a_removed_device() {
        let end = Some(End::TimedOut);
        let closed = healthy() + &gpu("ERROR", "host: a host command list did not close (x)");
        let areas = judge(&closed, end);
        assert_eq!(status(&areas, "host GPU failure"), Status::Fail);
        assert_eq!(status(&areas, "device removed"), Status::Pass);
        let none = gpu("WARN", "host: no host D3D12 device; command lists will not run error=x");
        let areas = judge(&none, end);
        assert_eq!(status(&areas, "host GPU failure"), Status::Fail);
        assert_eq!(status(&areas, "device removed"), Status::Unknown);
        // Another failure can be the removal's.
        let both = gpu("ERROR", "host: a host command list did not reset (x); device removed: y");
        assert_eq!(status(&judge(&both, end), "device removed"), Status::Fail);
    }

    #[test]
    fn refusals_logged_once_are_counted_by_their_drops() {
        let drop = |hash: u32| {
            let shaders = format!("[vertex {hash}, pixel 9]");
            gpu("INFO", &format!("host: dropped 2.0/frame without a host pipeline: refused {shaders}"))
        };
        let once = "pipeline::host: no host pipeline: x (logged once) shaders=[vertex 1, pixel 9]";
        let text = healthy() + &gpu("WARN", once) + &[1, 2, 3, 4, 4].map(drop).concat();
        let areas = judge(&text, None);
        let a = areas.iter().find(|a| a.name == "pipelines refused").unwrap();
        assert_eq!(a.status, Status::Fail);
        assert!(a.detail.starts_with("4: "), "{}", a.detail);
    }

    #[test]
    fn drops_are_reported_by_cause() {
        let line = gpu("INFO", "host: dropped by cause: refused 3.0, compiling 9.0, other 0.5 draws/frame");
        let text = healthy() + &line + &line.replace("compiling 9.0", "compiling 5.0");
        let areas = judge(&text, None);
        let a = areas.iter().find(|a| a.name == "drawn vs dropped").unwrap();
        assert!(a.detail.contains("refused 3.0, still compiling 7.0, other 0.5"), "{}", a.detail);
    }

    #[test]
    fn a_refusal_before_the_host_names_its_wave_reason() {
        let text = healthy()
            + &gpu(
                "WARN",
                "pipeline::host: host pipeline refused: wave size shaders=[compute 01] \
                 wave=the host GPU lacks widgets: needs a mode=2, has none",
            );
        let areas = judge(&text, None);
        let a = areas.iter().find(|a| a.name == "pipelines refused").unwrap();
        let whole = "the host GPU lacks widgets: needs a mode=2, has none";
        assert!(a.detail.contains(whole), "{}", a.detail);
    }

    #[test]
    fn flat_pictures_are_told_apart() {
        let png = |raw: &[u8]| {
            let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
            let zlib = miniz_oxide::deflate::compress_to_vec_zlib(raw, 3);
            let header = [0, 0, 0, 2, 0, 0, 0, 1, 8, 2, 0, 0, 0];
            for (kind, data) in [(b"IHDR", &header[..]), (b"IDAT", &zlib), (b"IEND", &[])] {
                png.extend((data.len() as u32).to_be_bytes());
                png.extend(kind);
                png.extend(data);
                png.extend([0; 4]);
            }
            png
        };
        assert_eq!(flat_png(&png(&[0, 9, 200, 9, 12, 201, 9])), Some(true));
        assert_eq!(flat_png(&png(&[0, 9, 200, 9, 90, 200, 9])), Some(false));
        assert_eq!(flat_png(&png(&[1, 9, 200, 9, 9, 200, 9])), None);
        assert_eq!(flat_png(b"\x89PNG\r\n\x1a\n\xff\xff\xff\xffIDAT----"), None);
        assert_eq!(flat_png(b"junk"), None);
    }

    #[test]
    fn the_pad_that_took_the_keys_is_named() {
        let pad = |devices: &[u32]| {
            let lines = devices.iter().map(|i| {
                format!(
                    "T  INFO victory_hle::modules::gameinput: input device device={i} \
                     connected=true\n"
                )
            });
            Log::parse(&lines.collect::<String>()).pad()
        };
        assert!(pad(&[4, 5]).starts_with("none"));
        assert!(pad(&[0, 5]).starts_with("connected"));
        assert!(pad(&[5]).starts_with("unknown"));
    }
}
