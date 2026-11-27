//! Disk loads the title's memory is left without (`docs/memory-design.md`,
//! "Assets come from disk"). The runtime's DirectStorage hands each load
//! over as a [`Load`]: where its bytes come from, decoded on demand. Whole
//! 64 KiB pages of its destination are then left as they are, without
//! access (`watch`), and nothing is written there:
//!
//! - our own reads of title memory (copies the GPU uses, texture uploads)
//!   decode the bytes from the load instead (`watch::read_lazy`);
//! - a title access faults, and the page is filled from the load first
//!   (`watch`'s handler), then behaves as any other page;
//! - our own writes, and the runtime's, fill the page before they land.
//!
//! What the GPU never needs and the title never reads again costs no
//! memory at all. Decoded compressed loads are kept in a small cache, so a
//! texture read page by page decodes once.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

/// Where a load's bytes come from.
pub trait Source: Send + Sync {
    /// Decoded bytes of the load.
    fn size(&self) -> usize;
    /// Whether any part needs the whole load decoded (compressed data).
    fn whole(&self) -> bool;
    /// Fills `out` with decoded bytes `[at, at + out.len())`; false if the
    /// file cannot be read or decoded.
    fn read(&self, at: usize, out: &mut [u8]) -> bool;
}

/// One load: its source, named by a number for the decode cache.
pub struct Load {
    id: u64,
    source: Box<dyn Source>,
}

impl Load {
    pub fn new(source: Box<dyn Source>) -> Arc<Load> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Arc::new(Load { id: NEXT.fetch_add(1, Relaxed), source })
    }

    /// Decoded bytes `[at, at + out.len())`.
    pub fn bytes(&self, at: usize, out: &mut [u8]) -> bool {
        if at.checked_add(out.len()).is_none_or(|end| end > self.source.size()) {
            return false;
        }
        if !self.source.whole() {
            crate::ledger::add(crate::ledger::Owner::SourceRead, out.len() as i64);
            return self.source.read(at, out);
        }
        let Some(decoded) = self.decoded() else {
            return false;
        };
        out.copy_from_slice(&decoded[at..at + out.len()]);
        true
    }

    /// The whole load decoded, from the cache or decoded now.
    fn decoded(&self) -> Option<Arc<Vec<u8>>> {
        if let Some(found) = cache().iter().find(|(id, _)| *id == self.id) {
            return Some(found.1.clone());
        }
        let mut bytes = vec![0; self.source.size()];
        crate::ledger::add(crate::ledger::Owner::SourceRead, bytes.len() as i64);
        if !self.source.read(0, &mut bytes) {
            return None;
        }
        let bytes = Arc::new(bytes);
        let mut cache = cache();
        cache.push_front((self.id, bytes.clone()));
        let mut total = 0;
        cache.retain(|(_, b)| {
            total += b.len();
            total <= CACHE_BYTES
        });
        crate::ledger::set(crate::ledger::Owner::DecodeCache, total.min(CACHE_BYTES) as i64);
        Some(bytes)
    }
}

/// Bytes of decoded loads kept: a few of the largest textures.
const CACHE_BYTES: usize = 96 << 20;

#[allow(clippy::type_complexity)]
static CACHE: Mutex<VecDeque<(u64, Arc<Vec<u8>>)>> = Mutex::new(VecDeque::new());

fn cache() -> std::sync::MutexGuard<'static, VecDeque<(u64, Arc<Vec<u8>>)>> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether loads leave title memory without bytes (`VICTORY_LAZY_LOADS=0`:
/// every load writes its destination, as before).
pub fn wanted() -> bool {
    static WANTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WANTED.get_or_init(|| std::env::var("VICTORY_LAZY_LOADS").map_or(true, |v| v != "0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counting(Vec<u8>, bool, std::sync::atomic::AtomicUsize);

    impl Source for Counting {
        fn size(&self) -> usize {
            self.0.len()
        }
        fn whole(&self) -> bool {
            self.1
        }
        fn read(&self, at: usize, out: &mut [u8]) -> bool {
            self.2.fetch_add(1, Relaxed);
            out.copy_from_slice(&self.0[at..at + out.len()]);
            true
        }
    }

    #[test]
    fn compressed_loads_decode_once_for_many_reads() {
        let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let load = Load::new(Box::new(Counting(data.clone(), true, Default::default())));
        let mut out = [0u8; 10];
        for at in [0, 100, 990] {
            assert!(load.bytes(at, &mut out));
            assert_eq!(out, data[at..at + 10]);
        }
        assert!(!load.bytes(995, &mut out), "past the end");
        let reads = cache().iter().filter(|(id, _)| *id == load.id).count();
        assert_eq!(reads, 1);
    }

    #[test]
    fn raw_loads_read_only_what_is_asked() {
        let load = Load::new(Box::new(Counting(vec![7; 100], false, Default::default())));
        let mut out = [0u8; 4];
        assert!(load.bytes(96, &mut out));
        assert_eq!(out, [7; 4]);
        assert!(!cache().iter().any(|(id, _)| *id == load.id));
    }
}
