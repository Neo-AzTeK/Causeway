//! The world lines of one log, one value per line, for `cargo xtask compare`:
//! the report holds medians; a comparison needs the spread too.

use super::*;

/// Drop kinds, as `classify` tags them.
const KINDS: &[&str] = &["refused", "untranslated", "indirect", "slots", "compiling", "other"];

/// Values per steady world line (speed, draws) or per steady world log
/// interval (drops). The warm-up's lines are only reported.
#[derive(Default)]
pub struct Series {
    /// fps, ms, preparing, gpu, copy-back: one value per steady line.
    pub speed: Vec<(&'static str, Vec<f64>)>,
    /// Draws per frame, one value per steady line; dropped draws per frame by
    /// cause and by kind, one value per steady log interval.
    pub fidelity: Vec<(String, Vec<f64>)>,
    /// The warm-up's world lines: their frame rates.
    pub warm_up_fps: Vec<f64>,
}

impl Log {
    pub fn series(&self) -> Series {
        let (world, warm_up): (Vec<&Rate>, Vec<&Rate>) =
            self.world().partition(|r| self.is_steady(unix_time(&r.time)));
        let column = |by: fn(&Rate) -> f64| world.iter().map(|r| by(r)).collect::<Vec<_>>();
        let speed = vec![
            ("fps", column(|r| r.fps)),
            ("ms", column(|r| r.ms)),
            ("preparing_ms", column(|r| r.preparing)),
            ("gpu_ms", column(|r| r.gpu)),
            ("copy_back_ms", column(|r| r.copy_back)),
        ];
        let mut fidelity = vec![("drawn_per_frame".to_owned(), column(|r| r.draws))];
        let steady = |b: &DropBlock| b.world && self.is_steady(b.at);
        let causes: Vec<&[f64; 3]> =
            self.causes.iter().zip(&self.blocks).filter(|(_, b)| steady(b)).map(|(c, _)| c).collect();
        for (i, name) in ["refused", "compiling", "other"].into_iter().enumerate() {
            let values = causes.iter().map(|c| c[i]).collect();
            fidelity.push((format!("dropped_{name}"), values));
        }
        let blocks: Vec<&DropBlock> = self.blocks.iter().filter(|b| steady(b)).collect();
        for kind in KINDS {
            let values = blocks
                .iter()
                .map(|b| {
                    let lost = b.why.iter().map(|(why, n)| self.classify(why, *n));
                    lost.filter(|l| l.tag == *kind).map(|l| l.per_frame).sum()
                })
                .collect();
            fidelity.push((format!("kind_{kind}"), values));
        }
        let warm_up_fps = warm_up.iter().map(|r| r.fps).collect();
        Series { speed, fidelity, warm_up_fps }
    }
}
