//! `cargo xtask dumps`: what a frame's dumped render targets hold, read from
//! the files a run with `VICTORY_DUMP_TARGETS=1` wrote
//! (`frame-<n>[-s<submission>]-<i>-<address>-<w>x<h>-f<format>[-by-<writer>][-black].png`).
//!
//! Dumps are 8-bit pictures, each scaled to its own range (`capture::to_rgba`):
//! a value here is a share of that dump's high (its 99th percentile for
//! colour), not an absolute one. "Zero" is a pixel whose three channels are 0,
//! that is below 1/255 of the dump's high.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use clap::Args;

use crate::report::sheet::{Image, decode};

#[derive(Args)]
pub struct Dumps {
    /// The run's folder (dumps in its `frames/`), or a folder of dumps.
    run: PathBuf,
    /// The frame whose dumps are read.
    #[arg(long)]
    frame: u64,
    /// Only dumps whose name holds this: a submission (`s8611-`), an
    /// address, a writer's hash.
    #[arg(long, allow_hyphen_values = true)]
    only: Option<String>,
    /// Measure the dump whose name holds this, instead of listing.
    #[arg(long, allow_hyphen_values = true)]
    measure: Option<String>,
    /// Split the measured pixels by this dump's strongest channel: 0, 1..15,
    /// 16 and up.
    #[arg(long, requires = "measure", allow_hyphen_values = true)]
    by: Option<String>,
    /// Keep only pixels where a dump's strongest channel is at least `min`:
    /// `<part of its name>:<min>`; may be repeated.
    #[arg(long = "mask", requires = "measure", allow_hyphen_values = true)]
    masks: Vec<String>,
}

pub fn run(args: &Dumps) -> Result<()> {
    let folder = if args.run.join("frames").is_dir() {
        args.run.join("frames")
    } else {
        args.run.clone()
    };
    let names = dumps_of(&folder, args.frame)?;
    let Some(measured) = &args.measure else {
        for name in names
            .iter()
            .filter(|n| args.only.as_ref().is_none_or(|o| n.contains(o.as_str())))
        {
            let image = read(&folder, name)?;
            println!("{}  {}", summary(&image), short(name));
        }
        return Ok(());
    };
    let one = |part: &str| -> Result<(String, Image)> {
        let name = pick(&names, part)?;
        let image = read(&folder, &name)?;
        Ok((name, image))
    };
    let (name, image) = one(measured)?;
    let by = args.by.as_deref().map(one).transpose()?;
    let mut masks = Vec::new();
    for mask in &args.masks {
        let (part, min) = mask
            .rsplit_once(':')
            .with_context(|| format!("--mask {mask}: wanted <part of a name>:<min>"))?;
        let min: u8 = min
            .parse()
            .with_context(|| format!("--mask {mask}: min is 0..255"))?;
        masks.push((one(part)?, min));
    }
    for other in by
        .iter()
        .map(|(n, i)| (n, i))
        .chain(masks.iter().map(|((n, i), _)| (n, i)))
    {
        ensure!(
            (other.1.width, other.1.height) == (image.width, image.height),
            "{} is {}x{}, {} is {}x{}",
            short(other.0),
            other.1.width,
            other.1.height,
            short(&name),
            image.width,
            image.height
        );
    }
    println!("measured {}", short(&name));
    if let Some((n, _)) = &by {
        println!("split by {}", short(n));
    }
    for ((n, _), min) in &masks {
        println!("mask     {} >= {min}", short(n));
    }
    let mask_images: Vec<(&Image, u8)> = masks.iter().map(|((_, i), m)| (i, *m)).collect();
    for line in split(&image, by.as_ref().map(|(_, i)| i), &mask_images) {
        println!("{line}");
    }
    Ok(())
}

/// The dumps of `frame` in `folder`, by name (not the presented frame).
fn dumps_of(folder: &Path, frame: u64) -> Result<Vec<String>> {
    let prefix = format!("frame-{frame}-");
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .with_context(|| format!("reading {}", folder.display()))?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with(&prefix) && n.ends_with(".png"))
        .collect();
    names.sort_by_key(|n| order(n));
    ensure!(
        !names.is_empty(),
        "no dumps of frame {frame} in {}",
        folder.display()
    );
    Ok(names)
}

/// Submissions in order, each one's dumps by index; the end of the frame
/// (no submission) last.
fn order(name: &str) -> (u64, u64, String) {
    let mut parts = name.split('-').skip(2);
    let first = parts.next().unwrap_or("");
    let (sub, index) = match first.strip_prefix('s').and_then(|s| s.parse().ok()) {
        Some(s) => (s, parts.next().and_then(|i| i.parse().ok()).unwrap_or(0)),
        None => (u64::MAX, first.parse().unwrap_or(0)),
    };
    (sub, index, name.to_owned())
}

/// The one name that holds `part`.
fn pick(names: &[String], part: &str) -> Result<String> {
    let found: Vec<&String> = names.iter().filter(|n| n.contains(part)).collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("no dump of this frame has {part:?} in its name"),
        many => bail!(
            "{} dumps have {part:?} in their name (first {}): give more of it",
            many.len(),
            short(many[0])
        ),
    }
}

fn read(folder: &Path, name: &str) -> Result<Image> {
    let path: PathBuf = folder.join(name);
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    decode(&bytes).with_context(|| format!("{name}: not an 8-bit RGB PNG with plain rows"))
}

/// The name without `frame-<n>-` and `.png`.
fn short(name: &str) -> &str {
    let rest = name.trim_end_matches(".png");
    rest.splitn(3, '-').nth(2).unwrap_or(rest)
}

/// A pixel's strongest channel.
fn strongest(image: &Image, i: usize) -> u8 {
    let p = &image.rgb[i * 3..i * 3 + 3];
    p[0].max(p[1]).max(p[2])
}

/// Size, the share of zero pixels, and the median and 99th percentile of the
/// others' strongest channel.
fn summary(image: &Image) -> String {
    let n = image.width * image.height;
    let mut lit: Vec<u8> = (0..n)
        .map(|i| strongest(image, i))
        .filter(|v| *v != 0)
        .collect();
    lit.sort_unstable();
    let at = |q: f64| {
        lit.get(((lit.len().max(1) - 1) as f64 * q) as usize)
            .copied()
            .unwrap_or(0)
    };
    format!(
        "{:>4}x{:<4} zero {:>5.1}%  p50 {:>3}  p99 {:>3}",
        image.width,
        image.height,
        100.0 * (n - lit.len()) as f64 / n.max(1) as f64,
        at(0.5),
        at(0.99)
    )
}

/// The measured pixels that pass every mask, per group of `by` (or all in
/// one): how many, the share of the frame, their median and the share that
/// is zero.
fn split(image: &Image, by: Option<&Image>, masks: &[(&Image, u8)]) -> Vec<String> {
    const GROUPS: [&str; 3] = ["by = 0", "by 1..15", "by >= 16"];
    let n = image.width * image.height;
    let mut groups: Vec<Vec<u8>> = vec![Vec::new(); 3];
    for i in 0..n {
        if masks.iter().any(|(m, min)| strongest(m, i) < *min) {
            continue;
        }
        let group = by.map_or(0, |b| match strongest(b, i) {
            0 => 0,
            1..=15 => 1,
            _ => 2,
        });
        groups[group].push(strongest(image, i));
    }
    let names: &[&str] = if by.is_some() { &GROUPS } else { &["all"] };
    names
        .iter()
        .zip(groups.iter_mut())
        .map(|(name, values)| {
            values.sort_unstable();
            let zero = values.iter().filter(|v| **v == 0).count();
            format!(
                "{name:<9} {:>8} px ({:>5.2}% of frame) | median {:>3}/255 | zero {:>5.1}%",
                values.len(),
                100.0 * values.len() as f64 / n.max(1) as f64,
                values.get(values.len() / 2).copied().unwrap_or(0),
                100.0 * zero as f64 / values.len().max(1) as f64
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(values: &[u8]) -> Image {
        Image {
            width: values.len(),
            height: 1,
            rgb: values.iter().flat_map(|v| [0, *v, 0]).collect(),
        }
    }

    #[test]
    fn submissions_come_in_order_then_the_end_of_frame() {
        let mut names = [
            "frame-7-03-0x1-4x4-f10.png".to_owned(),
            "frame-7-s20-01-0x1-4x4-f10.png".to_owned(),
            "frame-7-s3-12-0x1-4x4-f10.png".to_owned(),
            "frame-7-s3-2-0x1-4x4-f10.png".to_owned(),
        ];
        names.sort_by_key(|n| order(n));
        assert_eq!(
            names.iter().map(|n| short(n)).collect::<Vec<_>>(),
            [
                "s3-2-0x1-4x4-f10",
                "s3-12-0x1-4x4-f10",
                "s20-01-0x1-4x4-f10",
                "03-0x1-4x4-f10"
            ]
        );
    }

    #[test]
    fn a_part_must_name_one_dump() {
        let names = [
            "frame-7-s3-01-0xa.png".to_owned(),
            "frame-7-s3-02-0xb.png".to_owned(),
        ];
        assert_eq!(pick(&names, "0xb").unwrap(), names[1]);
        assert!(
            pick(&names, "s3-")
                .unwrap_err()
                .to_string()
                .contains("2 dumps")
        );
        assert!(pick(&names, "0xc").is_err());
    }

    #[test]
    fn split_groups_masked_pixels_by_the_other_dump() {
        // Pixel:     0   1   2   3   4
        let measured = image(&[0, 9, 0, 200, 7]);
        let by = image(&[0, 0, 5, 40, 40]);
        let mask = image(&[30, 30, 30, 30, 10]);
        let lines = split(&measured, Some(&by), &[(&mask, 24)]);
        assert!(
            lines[0].contains("2 px") && lines[0].contains("zero  50.0%"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("1 px") && lines[1].contains("zero 100.0%"),
            "{lines:?}"
        );
        // Pixel 4 is masked out: only pixel 3 is left with by >= 16.
        assert!(
            lines[2].contains("1 px") && lines[2].contains("median 200"),
            "{lines:?}"
        );
        let all = split(&measured, None, &[]);
        assert_eq!(all.len(), 1);
        assert!(
            all[0].contains("5 px") && all[0].contains("zero  40.0%"),
            "{all:?}"
        );
    }

    #[test]
    fn summary_counts_zero_pixels_and_the_rest() {
        let s = summary(&image(&[0, 0, 10, 20]));
        assert!(s.contains("zero  50.0%") && s.contains("p50  10"), "{s}");
    }
}
