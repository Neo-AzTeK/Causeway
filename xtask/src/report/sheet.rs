//! The run digest's pictures: a contact sheet of world frames and a cheap
//! measure of whether they show anything. Frames are the 8-bit RGB PNGs the
//! diagnostics build writes; any other file is "not read", never guessed.

use std::path::{Path, PathBuf};

/// Frames on the sheet at most, its tiles' size, and its columns.
const SHEET_FRAMES: usize = 6;
const TILE: (usize, usize) = (480, 270);
const COLUMNS: usize = 3;
/// A pixel whose strongest channel is at most this is black.
const BLACK: u8 = 16;
/// A neighbour that differs by more than this in a channel is an edge.
const EDGE: u8 = 8;

pub struct Image {
    pub width: usize,
    pub height: usize,
    /// Rows of RGB, no padding.
    pub rgb: Vec<u8>,
}

/// What the digest says about its pictures.
#[derive(Default, Debug)]
pub struct Visual {
    /// The sheet's file name in the out folder, or why there is none.
    pub sheet: String,
    /// `(frame number, share)` for each frame read.
    pub shares: Vec<(u64, f64)>,
    /// Frames asked for, that could not be read.
    pub unreadable: usize,
}

impl Visual {
    pub fn none(why: &str) -> Visual {
        Visual { sheet: why.to_owned(), ..Visual::default() }
    }

    /// Mean share of pixels with detail, if any frame was read.
    pub fn mean(&self) -> Option<f64> {
        (!self.shares.is_empty())
            .then(|| self.shares.iter().map(|s| s.1).sum::<f64>() / self.shares.len() as f64)
    }
}

/// The frame number in `frame-<n>.png`.
fn number(path: &Path) -> Option<u64> {
    path.file_name()?.to_str()?.strip_prefix("frame-")?.strip_suffix(".png")?.parse().ok()
}

/// Up to `SHEET_FRAMES` of `frames`, evenly spread over them.
fn spread(frames: &[PathBuf]) -> Vec<&PathBuf> {
    let n = frames.len().min(SHEET_FRAMES);
    (0..n).map(|i| &frames[if n == 1 { 0 } else { i * (frames.len() - 1) / (n - 1) }]).collect()
}

/// Reads the world frames named, writes `contact-sheet.png` into `out` and
/// measures them. `read` gives a file's bytes (`None`: gone).
pub fn build(frames: &[PathBuf], out: &Path, read: impl Fn(&Path) -> Option<Vec<u8>>) -> Visual {
    if frames.is_empty() {
        return Visual::none("none: no world frame was saved");
    }
    let mut visual = Visual::default();
    let mut tiles = Vec::new();
    for path in spread(frames) {
        match read(path).as_deref().and_then(decode) {
            Some(image) => {
                if let Some(n) = number(path) {
                    visual.shares.push((n, detail_share(&image)));
                }
                tiles.push(tile(&image));
            }
            None => visual.unreadable += 1,
        }
    }
    if tiles.is_empty() {
        visual.sheet = "none: no saved frame could be read (8-bit RGB PNG expected)".into();
        return visual;
    }
    let rows = tiles.len().div_ceil(COLUMNS);
    let (width, height) = (COLUMNS * TILE.0, rows * TILE.1);
    let mut sheet = vec![0u8; width * height * 3];
    for (i, t) in tiles.iter().enumerate() {
        let (cx, cy) = ((i % COLUMNS) * TILE.0, (i / COLUMNS) * TILE.1);
        for y in 0..TILE.1 {
            let to = ((cy + y) * width + cx) * 3;
            sheet[to..to + TILE.0 * 3].copy_from_slice(&t[y * TILE.0 * 3..(y + 1) * TILE.0 * 3]);
        }
    }
    let png = encode(width, height, &sheet);
    visual.sheet = match std::fs::write(out.join("contact-sheet.png"), png) {
        Ok(()) => format!("contact-sheet.png ({} frames)", tiles.len()),
        Err(e) => format!("none: writing it failed: {e}"),
    };
    visual
}

/// A tile of the image, nearest neighbour.
fn tile(image: &Image) -> Vec<u8> {
    let mut out = Vec::with_capacity(TILE.0 * TILE.1 * 3);
    for y in 0..TILE.1 {
        let sy = y * image.height / TILE.1;
        for x in 0..TILE.0 {
            let sx = x * image.width / TILE.0;
            let at = (sy * image.width + sx) * 3;
            out.extend_from_slice(&image.rgb[at..at + 3]);
        }
    }
    out
}

/// The share of pixels that are neither black nor flat: not black, and
/// differing from the pixel to the right or below by more than `EDGE`.
/// Detail, not proof of geometry: noise and a UI count too.
pub fn detail_share(image: &Image) -> f64 {
    let (w, h) = (image.width, image.height);
    let px = |x: usize, y: usize| &image.rgb[(y * w + x) * 3..(y * w + x) * 3 + 3];
    let far = |a: &[u8], b: &[u8]| a.iter().zip(b).any(|(p, q)| p.abs_diff(*q) > EDGE);
    let mut count = 0usize;
    for y in 0..h {
        for x in 0..w {
            let p = px(x, y);
            if p.iter().all(|&c| c <= BLACK) {
                continue;
            }
            let right = x + 1 < w && far(p, px(x + 1, y));
            let below = y + 1 < h && far(p, px(x, y + 1));
            count += usize::from(right || below);
        }
    }
    count as f64 / (w * h).max(1) as f64
}

/// 8-bit RGB, filter 0 rows, one or more IDAT chunks.
pub fn decode(png: &[u8]) -> Option<Image> {
    let mut rest = png.strip_prefix(b"\x89PNG\r\n\x1a\n")?;
    let (mut width, mut height) = (0usize, 0usize);
    let mut data = Vec::new();
    while rest.len() >= 12 {
        let len = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
        let body = rest.get(8..8usize.checked_add(len)?)?;
        match &rest[4..8] {
            b"IHDR" => {
                width = u32::from_be_bytes(body.get(..4)?.try_into().ok()?) as usize;
                height = u32::from_be_bytes(body.get(4..8)?.try_into().ok()?) as usize;
                if body.get(8..13)? != [8, 2, 0, 0, 0] {
                    return None;
                }
            }
            b"IDAT" => data.extend_from_slice(body),
            _ => {}
        }
        rest = rest.get(len.checked_add(12)?..)?;
    }
    let row = width.checked_mul(3)?.checked_add(1)?;
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&data).ok()?;
    if width == 0 || height == 0 || raw.len() != row.checked_mul(height)? {
        return None;
    }
    let mut rgb = Vec::with_capacity(width * height * 3);
    for line in raw.chunks_exact(row) {
        if line[0] != 0 {
            return None;
        }
        rgb.extend_from_slice(&line[1..]);
    }
    Some(Image { width, height, rgb })
}

fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for byte in parts.iter().flat_map(|p| p.iter()) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

pub fn encode(width: usize, height: usize, rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width * 3 + 1) * height);
    for line in rgb.chunks_exact(width * 3) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut header = Vec::new();
    header.extend_from_slice(&(width as u32).to_be_bytes());
    header.extend_from_slice(&(height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        png.extend_from_slice(&(body.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(body);
        png.extend_from_slice(&crc32(&[kind, body]).to_be_bytes());
    };
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6));
    chunk(b"IEND", &[]);
    png
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Image {
        let f = &f;
        let rgb = (0..h).flat_map(move |y| (0..w).flat_map(move |x| f(x, y)));
        Image { width: w, height: h, rgb: rgb.collect() }
    }

    #[test]
    fn png_round_trips() {
        let i = image(5, 3, |x, y| [x as u8 * 40, y as u8 * 90, 7]);
        let back = decode(&encode(5, 3, &i.rgb)).unwrap();
        assert_eq!((back.width, back.height, back.rgb), (5, 3, i.rgb));
        assert!(decode(b"not a png").is_none());
    }

    #[test]
    fn black_and_flat_frames_show_no_detail_and_a_pattern_does() {
        assert_eq!(detail_share(&image(32, 32, |_, _| [0, 0, 0])), 0.0);
        assert_eq!(detail_share(&image(32, 32, |_, _| [90, 120, 200])), 0.0);
        let checker = image(32, 32, |x, y| if (x + y) % 2 == 0 { [200; 3] } else { [60; 3] });
        assert!(detail_share(&checker) > 0.9);
    }

    #[test]
    fn a_sheet_is_written_from_up_to_six_frames_and_reports_unreadable_ones() {
        let dir = std::env::temp_dir().join(format!("victory-sheet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<PathBuf> = (1..=8).map(|n| format!("frame-{n}00.png").into()).collect();
        let flat = encode(64, 36, &vec![120u8; 64 * 36 * 3]);
        let v = build(&paths, &dir, |p| (p != Path::new("frame-100.png")).then(|| flat.clone()));
        assert_eq!(v.sheet, "contact-sheet.png (5 frames)");
        assert_eq!((v.shares.len(), v.unreadable), (5, 1));
        assert_eq!(v.mean(), Some(0.0));
        let sheet = decode(&std::fs::read(dir.join("contact-sheet.png")).unwrap()).unwrap();
        assert_eq!((sheet.width, sheet.height), (3 * TILE.0, 2 * TILE.1));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(build(&[], &dir, |_| None).sheet.starts_with("none"));
    }
}
