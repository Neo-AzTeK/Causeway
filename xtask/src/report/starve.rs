//! Whether the machine was starved while the title ran, from its log alone.
//! A starved run (free RAM and commit near zero) boots, loads and translates
//! at a crawl and often never reaches the world: it says nothing about the
//! code under test, so the report says "rerun on a quiet machine" instead of a
//! bare "no world". Each sign is the same work taking far longer on the clock
//! than it does on a quiet machine.

use super::{field, unix_time};

/// The pipeline library loads in ~30 ms from the page cache; read from disk
/// (396 MB at ~150 MB/s) it takes 2400 ms or more.
const LIBRARY_MS: f64 = 1000.0;
/// Cached shader translations take 0.6 to 12 times their CPU time on the
/// clock (a busy but healthy machine is at 12); a starved one 100 to 440 times.
/// Only a sum of 10 s or more is judged, so that a few slow lines say nothing.
const TRANSLATE_RATIO: f64 = 40.0;
const TRANSLATE_WALL_MS: f64 = 10_000.0;
/// From the first log line to the first story000 pack opened: ~30 s healthy,
/// 176 s and 264 s starved. A cold shader cache (a build that changed the
/// translator) also boots slowly, on a healthy machine: with this many shaders
/// translated afresh, the pack's time says nothing.
const PACK_AFTER_BOOT: f64 = 120.0;
const COLD_CACHE: usize = 100;

#[derive(Default)]
pub struct Starve {
    /// The first time a line has, and the story000 pack's open (Unix time).
    first: Option<f64>,
    pack: Option<f64>,
    library_ms: Option<f64>,
    /// Cached shader translations: how many, the clock time they took, and
    /// the CPU time (user and kernel), in ms.
    translated: usize,
    /// Shaders translated afresh, not from the cache.
    afresh: usize,
    wall_ms: f64,
    cpu_ms: f64,
}

impl Starve {
    /// One log line, without terminal escapes.
    pub fn read(&mut self, line: &str) {
        let at = || line.split_whitespace().next().and_then(unix_time);
        if self.first.is_none() {
            self.first = at();
        }
        if !line.contains(" INFO ") {
            return;
        }
        let ms = |key| field(line, key).and_then(|v| v.parse::<f64>().ok());
        if line.contains("host pipeline library loaded") {
            self.library_ms = self.library_ms.or(ms("ms"));
        } else if line.contains("shader translated") && field(line, "cached") == Some("false") {
            self.afresh += 1;
        } else if line.contains("shader translated") && field(line, "cached") == Some("true") {
            self.translated += 1;
            self.wall_ms += ms("ms").unwrap_or(0.0);
            self.cpu_ms += ms("user_ms").unwrap_or(0.0) + ms("kernel_ms").unwrap_or(0.0);
        } else if self.pack.is_none()
            && line.contains("dstorage: file opened")
            && line.contains("story000")
        {
            self.pack = at();
        }
    }

    /// What in the log shows the machine was starved, one sign each. A sign
    /// alone can be a cold disk cache: `Log::starved` weighs them.
    pub fn signs(&self) -> Vec<String> {
        let mut signs = Vec::new();
        if let Some(ms) = self.library_ms.filter(|&ms| ms > LIBRARY_MS) {
            signs.push(format!("pipeline library load took {ms:.0} ms (30 ms when quiet)"));
        }
        if self.wall_ms >= TRANSLATE_WALL_MS && self.wall_ms > TRANSLATE_RATIO * self.cpu_ms {
            signs.push(format!(
                "{} cached shader translations took {:.0} s on the clock for {:.1} s of CPU",
                self.translated,
                self.wall_ms / 1000.0,
                self.cpu_ms / 1000.0
            ));
        }
        if let Some((first, pack)) = self.first.zip(self.pack)
            && self.afresh < COLD_CACHE
            && pack - first > PACK_AFTER_BOOT
        {
            signs.push(format!("story000 pack opened {:.0} s after boot (30 s when quiet)", pack - first));
        }
        signs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn starve(log: &str) -> Vec<String> {
        let mut starve = Starve::default();
        for line in log.lines() {
            starve.read(line);
        }
        starve.signs()
    }

    /// A quiet machine's log: library in 30 ms, 3000 translations on 2.5 s of
    /// CPU in 1.6 s, the pack opened 30 s after the first line.
    fn quiet() -> String {
        let mut log = String::from("build          Series 1.0.2.1 (b99f)\n");
        log += "2026-11-26T03:12:28.1Z  INFO victory_hle::modules::fidelityfx: no FidelityFX folder\n";
        log += "2026-11-26T03:12:32.1Z  INFO victory_gpu::pipeline::library: host pipeline library loaded bytes=393198600 ms=30\n";
        for _ in 0..3000 {
            log += &translated("true", 1, 1, 0);
        }
        log += "2026-11-26T03:12:58.1Z  INFO victory_hle::modules::dstorage: dstorage: file opened path=\"G:\\\\pack\\\\story000\\\\story000_script.rpf\"\n";
        log
    }

    fn translated(cached: &str, ms: u32, user: u32, kernel: u32) -> String {
        format!(
            "2026-11-26T03:12:40.0Z  INFO victory_gpu::pipeline: shader translated stage=\"pixel\" hash=9594 user_data=2 samplers=0 cached={cached} ms={ms} user_ms={user} kernel_ms={kernel}\n"
        )
    }

    #[test]
    fn a_quiet_machines_log_shows_no_starving() {
        assert_eq!(starve(&quiet()), Vec::<String>::new());
        // No line at all, or only the start of a run, shows nothing.
        assert_eq!(starve(""), Vec::<String>::new());
        assert_eq!(starve("build x\n2026-11-26T03:12:28.1Z  INFO a: b\n"), Vec::<String>::new());
    }

    #[test]
    fn a_slow_library_load_shows_it() {
        let slow = quiet().replace("ms=30\n", "ms=2679\n");
        let signs = starve(&slow);
        assert_eq!(signs.len(), 1, "{signs:?}");
        assert!(signs[0].contains("2679 ms"), "{signs:?}");
        // At the limit it is not slow.
        assert!(starve(&quiet().replace("ms=30\n", "ms=1000\n")).is_empty());
    }

    #[test]
    fn translations_that_wait_much_longer_than_they_compute_show_it() {
        // 2584 lines, 466 s on the clock for 2.6 s of CPU.
        let mut log = String::new();
        for i in 0..2584 {
            log += &translated("true", if i < 2 { 233_000 } else { 0 }, 1, 0);
        }
        let signs = starve(&log);
        assert_eq!(signs.len(), 1, "{signs:?}");
        assert!(signs[0].starts_with("2584 cached shader translations took 466 s"), "{signs:?}");
        // A long clock time over as much CPU is a long translation, not a starved one.
        let mut busy = String::new();
        for _ in 0..100 {
            busy += &translated("true", 300, 150, 150);
        }
        assert!(starve(&busy).is_empty());
        // A few slow lines under the sum asked for say nothing.
        assert!(starve(&translated("true", 9000, 0, 0)).is_empty());
        // Nor do translations that were not cached.
        assert!(starve(&translated("false", 500_000, 0, 0)).is_empty());
    }

    #[test]
    fn a_pack_that_opens_late_shows_it() {
        let late = quiet().replace("03:12:58.1Z", "03:15:24.1Z");
        let signs = starve(&late);
        assert_eq!(signs.len(), 1, "{signs:?}");
        assert!(signs[0].contains("176 s after boot"), "{signs:?}");
        // Only the first story000 open counts, and 120 s is not late.
        let again = quiet().replace("03:12:58.1Z", "03:14:28.1Z")
            + "2026-11-26T03:30:00.0Z  INFO victory_hle::modules::dstorage: dstorage: file opened path=\"story000_bvh4\"\n";
        assert!(starve(&again).is_empty());
    }

    #[test]
    fn a_cold_shader_cache_boots_slowly_on_a_healthy_machine() {
        let late = quiet().replace("03:12:58.1Z", "03:16:00.1Z");
        assert_eq!(starve(&late).len(), 1);
        // 1652 shaders translated afresh before the pack opened: not starving.
        let cold = translated("false", 40, 40, 0).repeat(1652) + &late;
        assert!(starve(&cold).is_empty(), "{:?}", starve(&cold));
    }

    #[test]
    fn a_busy_but_healthy_translation_is_not_starved() {
        // 12 times the CPU time on the clock, 30 s in all.
        let busy = translated("true", 1200, 50, 50).repeat(25);
        assert!(starve(&busy).is_empty());
        // 100 times is.
        assert_eq!(starve(&translated("true", 10_000, 50, 50).repeat(3)).len(), 1);
    }

    #[test]
    fn every_sign_is_named() {
        let all = quiet().replace("ms=30\n", "ms=2400\n").replace("03:12:58.1Z", "03:20:00.1Z")
            + &translated("true", 600_000, 1, 0);
        assert_eq!(starve(&all).len(), 3);
    }
}
