//! Translated shaders kept on disk (decision 0003): under the configured
//! cache folder, keyed by the container's content and by what its
//! translation depends on: the translator's build
//! ([`victory_shader::BUILD_HASH`]), the loop limit and this file, which
//! encodes the entries. A rebuild that changes none of these keeps the
//! cache. Entries go through `victory-pipeline`'s content-addressed cache,
//! which writes them atomically; an entry that does not read back is
//! translated again.
//!
//! Shaders that do not translate are kept too, with the reason: translation
//! is a pure function of the container, so they fail again. Failures to
//! read or write the cache are never kept.
//!
//! The entry is our own encoding of [`HostShader`], after its BLAKE3: a
//! field added to it must be added to `encode` and `decode` too.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use victory_pipeline::{Cache, Converter};
use victory_shader::emit::{Caps, HostShader, StaticSampler, UserData};
use victory_shader::{HwStage, RootType};

static CACHE: OnceLock<Cache> = OnceLock::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);

/// Keeps translated shaders in `dir` from now on.
pub fn use_shader_cache(dir: PathBuf) {
    match Cache::open(dir) {
        Ok(cache) => {
            let _ = CACHE.set(cache);
        }
        Err(e) => tracing::warn!(error = %e, "no shader cache"),
    }
}

/// Shaders found in the cache and translated since the start.
pub(crate) fn counts() -> (u64, u64) {
    (HITS.load(Ordering::Relaxed), MISSES.load(Ordering::Relaxed))
}

/// What a translation depends on beside the container.
fn build(caps: Caps) -> &'static [u8; 32] {
    static BUILD: [OnceLock<[u8; 32]>; 2] = [OnceLock::new(), OnceLock::new()];
    BUILD[usize::from(caps.min_lod_clamp)].get_or_init(|| {
        let limit = victory_shader::emit::loop_limit();
        if limit != victory_shader::emit::DEFAULT_LOOP_LIMIT {
            tracing::warn!(
                limit,
                "VICTORY_SHADER_LOOP_LIMIT: translated loops stop sooner"
            );
        }
        build_key(victory_shader::BUILD_HASH, limit, caps)
    })
}

/// The BLAKE3 of the translator's build hash, the loop limit (which a
/// diagnostics build can change at run time) and this file.
fn build_key(translator: &str, loop_limit: u32, caps: Caps) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&(translator.len() as u64).to_le_bytes());
    h.update(translator.as_bytes());
    h.update(&loop_limit.to_le_bytes());
    h.update(&[u8::from(caps.min_lod_clamp)]);
    h.update(include_bytes!("shader_cache.rs"));
    *h.finalize().as_bytes()
}

/// The translation of a console shader container, from the cache when
/// there is one.
pub(crate) fn host_shader(container: &[u8], caps: Caps) -> Result<HostShader, String> {
    let translate = |c: &[u8]| victory_shader::emit::translate_with(c, &caps).map_err(|e| e.to_string());
    match CACHE.get() {
        Some(cache) => cached(cache, build(caps), container, translate),
        None => translate(container),
    }
}

/// The cache entry's converter: the build and the container in, the
/// encoded translation or its failure out. `ran` tells a miss from a hit.
struct Translate<F> {
    translate: F,
    ran: AtomicBool,
}

impl<F: Fn(&[u8]) -> Result<HostShader, String> + Send + Sync> Converter for Translate<F> {
    fn id(&self) -> &'static str {
        "shader.dxil"
    }
    fn version(&self) -> u32 {
        // The build fingerprint in the input does the versioning.
        1
    }
    fn convert(&self, input: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.ran.store(true, Ordering::Relaxed);
        Ok(encode_entry(&(self.translate)(&input[32..])))
    }
}

fn cached(
    cache: &Cache,
    build: &[u8; 32],
    container: &[u8],
    translate: impl Fn(&[u8]) -> Result<HostShader, String> + Send + Sync,
) -> Result<HostShader, String> {
    let input = [&build[..], container].concat();
    let converter = Translate {
        translate,
        ran: AtomicBool::new(false),
    };
    for _ in 0..2 {
        let path = match cache.get_or_convert(&converter, &input) {
            Ok(path) => path,
            Err(e) => {
                // The store failed, not the translation.
                tracing::warn!(error = format!("{e:#}"), "shader cache entry not written");
                break;
            }
        };
        if let Some(result) = std::fs::read(&path).ok().and_then(|b| decode_entry(&b)) {
            let (counter, what) = if converter.ran.load(Ordering::Relaxed) {
                (&MISSES, "translated and cached")
            } else {
                (&HITS, "from the cache")
            };
            counter.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(path = %path.display(), "shader {what}");
            return result;
        }
        tracing::debug!(path = %path.display(), "unreadable shader cache entry; translating again");
        let _ = std::fs::remove_file(&path);
    }
    MISSES.fetch_add(1, Ordering::Relaxed);
    (converter.translate)(container)
}

/// A translation or the reason there is none, after the BLAKE3 of the rest.
fn encode_entry(result: &Result<HostShader, String>) -> Vec<u8> {
    let body = match result {
        Ok(shader) => encode(shader),
        Err(why) => [&FAILED[..], why.as_bytes()].concat(),
    };
    [&blake3::hash(&body).as_bytes()[..], &body].concat()
}

fn decode_entry(bytes: &[u8]) -> Option<Result<HostShader, String>> {
    let (hash, body) = bytes.split_at_checked(32)?;
    if hash != &blake3::hash(body).as_bytes()[..] {
        return None;
    }
    match body.strip_prefix(FAILED) {
        Some(why) => Some(Err(String::from_utf8(why.to_vec()).ok()?)),
        None => decode(body).map(Ok),
    }
}

const FAILED: &[u8; 4] = b"VSHF";
const MAGIC: &[u8; 4] = b"VSH1";
const STAGES: [HwStage; 8] = [
    HwStage::Ls,
    HwStage::Hs,
    HwStage::Es,
    HwStage::Gs,
    HwStage::Vs,
    HwStage::Ps,
    HwStage::Ms,
    HwStage::Cs,
];

fn root_code(r: RootType) -> u32 {
    match r {
        RootType::Constants => 0,
        RootType::Descriptor => 1,
        RootType::Table => 2,
        RootType::ImmediateSampler => 3,
        RootType::ResourceHeap => 4,
        RootType::SamplerHeap => 5,
        RootType::Other(x) => 6 + x,
    }
}

fn root_type(code: u32) -> RootType {
    match code {
        0 => RootType::Constants,
        1 => RootType::Descriptor,
        2 => RootType::Table,
        3 => RootType::ImmediateSampler,
        4 => RootType::ResourceHeap,
        5 => RootType::SamplerHeap,
        x => RootType::Other(x - 6),
    }
}

/// Little-endian words and length-prefixed lists.
fn encode(s: &HostShader) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    let word = |out: &mut Vec<u8>, v: u32| out.extend(v.to_le_bytes());
    let stage = STAGES.iter().position(|&x| x == s.stage).unwrap_or(0) as u32;
    word(&mut out, stage);
    word(&mut out, s.dxil.len() as u32);
    out.extend(&s.dxil);
    word(&mut out, s.user_data.len() as u32);
    for u in &s.user_data {
        for v in [u.index, u32::from(u.slot), root_code(u.root), u.dword] {
            word(&mut out, v);
        }
    }
    word(&mut out, s.samplers.len() as u32);
    for sampler in &s.samplers {
        for &v in sampler {
            word(&mut out, v);
        }
    }
    word(&mut out, s.static_samplers.len() as u32);
    for t in &s.static_samplers {
        let [u, v, w] = t.address;
        let words = [
            t.filter,
            u,
            v,
            w,
            t.mip_lod_bias.to_bits(),
            t.max_anisotropy,
            t.comparison,
            t.border,
            t.min_lod.to_bits(),
            t.max_lod.to_bits(),
        ];
        for v in words {
            word(&mut out, v);
        }
    }
    for slots in [&s.root_cbvs, &s.root_srvs] {
        word(&mut out, slots.len() as u32);
        out.extend(slots);
    }
    out
}

fn decode(bytes: &[u8]) -> Option<HostShader> {
    struct Reader<'a>(&'a [u8]);
    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            let (head, rest) = self.0.split_at_checked(n)?;
            self.0 = rest;
            Some(head)
        }
        fn word(&mut self) -> Option<u32> {
            Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
        }
        fn list(&mut self) -> Option<Vec<u8>> {
            let n = self.word()? as usize;
            Some(self.take(n)?.to_vec())
        }
    }
    let mut r = Reader(bytes);
    if r.take(4)? != MAGIC {
        return None;
    }
    let stage = *STAGES.get(r.word()? as usize)?;
    let dxil = r.list()?;
    let n = r.word()? as usize;
    let mut user_data = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        user_data.push(UserData {
            index: r.word()?,
            slot: u8::try_from(r.word()?).ok()?,
            root: root_type(r.word()?),
            dword: r.word()?,
        });
    }
    let n = r.word()? as usize;
    let mut samplers = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        samplers.push([r.word()?, r.word()?, r.word()?, r.word()?]);
    }
    let n = r.word()? as usize;
    let mut static_samplers = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        static_samplers.push(StaticSampler {
            filter: r.word()?,
            address: [r.word()?, r.word()?, r.word()?],
            mip_lod_bias: f32::from_bits(r.word()?),
            max_anisotropy: r.word()?,
            comparison: r.word()?,
            border: r.word()?,
            min_lod: f32::from_bits(r.word()?),
            max_lod: f32::from_bits(r.word()?),
        });
    }
    let root_cbvs = r.list()?;
    let root_srvs = r.list()?;
    r.0.is_empty().then_some(HostShader {
        stage,
        dxil,
        user_data,
        samplers,
        static_samplers,
        root_cbvs,
        root_srvs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shader() -> HostShader {
        HostShader {
            stage: HwStage::Ps,
            dxil: b"DXBC and the rest".to_vec(),
            user_data: vec![UserData {
                index: 8,
                slot: 1,
                root: RootType::Table,
                dword: 0,
            }],
            samplers: vec![[0x400, 0xc0_0000, 0x18f0_0000, 0]],
            static_samplers: vec![StaticSampler::from_words([
                0x400,
                0xc0_0000,
                0x18f0_0000,
                0,
            ])],
            root_cbvs: vec![2],
            root_srvs: vec![0, 3],
        }
    }

    #[test]
    fn translations_come_back_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let build = [7u8; 32];
        let container = b"a console container".as_slice();
        let runs = AtomicU64::new(0);
        let translate = |_: &[u8]| {
            runs.fetch_add(1, Ordering::Relaxed);
            Ok(shader())
        };
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 1, "the second came from disk");

        // Another build translates again.
        assert_eq!(cached(&cache, &[8; 32], container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 2);

        // A damaged entry is a miss, translated and stored again.
        for e in walk(dir.path()) {
            std::fs::write(e, b"VSH1 broken").unwrap();
        }
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 3);
        assert_eq!(cached(&cache, &build, container, translate), Ok(shader()));
        assert_eq!(runs.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn failures_come_back_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let runs = AtomicU64::new(0);
        let get = || {
            cached(&cache, &[7; 32], b"container", |_| {
                runs.fetch_add(1, Ordering::Relaxed);
                Err("an opcode is not supported".to_owned())
            })
        };
        let failure = Err("an opcode is not supported".to_owned());
        assert_eq!(get(), failure);
        assert_eq!(get(), failure);
        assert_eq!(runs.load(Ordering::Relaxed), 1, "the second came from disk");

        // Damaged (here in the reason only), it is translated again.
        let files = walk(dir.path());
        let mut bytes = std::fs::read(&files[0]).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&files[0], &bytes).unwrap();
        assert_eq!(get(), failure);
        assert_eq!(runs.load(Ordering::Relaxed), 2);
        // Truncated or emptied, too.
        for len in [0, 31, 32, bytes.len() - 1] {
            std::fs::write(&files[0], &bytes[..len]).unwrap();
            assert_eq!(get(), failure);
        }
        assert_eq!(runs.load(Ordering::Relaxed), 6);
        assert_eq!(get(), failure);
        assert_eq!(runs.load(Ordering::Relaxed), 6, "stored again");
    }

    #[test]
    fn a_store_failure_neither_fails_the_shader_nor_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let get = || cached(&cache, &[7; 32], b"container", |_| Ok(shader()));
        assert_eq!(get(), Ok(shader()));
        // A folder in the entry's place cannot be written over.
        let path = walk(dir.path()).remove(0);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(get(), Ok(shader()));
        assert!(walk(dir.path()).is_empty());
    }

    fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                files.extend(walk(&e.path()));
            } else {
                files.push(e.path());
            }
        }
        files
    }

    #[test]
    fn the_build_key_follows_the_loop_limit_and_the_translator() {
        let key = build_key("a", 1 << 20, Caps::default());
        assert_eq!(build_key("a", 1 << 20, Caps::default()), key, "same source, same key");
        assert_ne!(build_key("a", 1 << 19, Caps::default()), key);
        let clamp = Caps { min_lod_clamp: true };
        assert_ne!(build_key("a", 1 << 20, clamp), key, "the device caps are keyed");
        assert_ne!(build_key("b", 1 << 20, Caps::default()), key);
        assert_eq!(victory_shader::BUILD_HASH.len(), 64);
    }

    #[test]
    fn another_device_capability_misses() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let runs = AtomicU64::new(0);
        let get = |min_lod_clamp| {
            let key = build_key(victory_shader::BUILD_HASH, 64, Caps { min_lod_clamp });
            cached(&cache, &key, b"container", |_| {
                runs.fetch_add(1, Ordering::Relaxed);
                Ok(shader())
            })
        };
        for (clamp, expected) in [(false, 1), (false, 1), (true, 2), (true, 2), (false, 2)] {
            get(clamp).unwrap();
            assert_eq!(runs.load(Ordering::Relaxed), expected, "clamp {clamp}");
        }
    }

    #[test]
    fn same_source_hits_and_another_loop_limit_misses() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let runs = AtomicU64::new(0);
        let get = |limit| {
            let key = build_key(victory_shader::BUILD_HASH, limit, Caps::default());
            cached(&cache, &key, b"container", |_| {
                runs.fetch_add(1, Ordering::Relaxed);
                Ok(shader())
            })
        };
        for (limit, expected) in [(64, 1), (64, 1), (65, 2), (64, 2)] {
            assert_eq!(get(limit), Ok(shader()));
            assert_eq!(runs.load(Ordering::Relaxed), expected);
        }
    }

    #[test]
    fn entries_round_trip_and_reject_damage() {
        let bytes = encode(&shader());
        assert_eq!(decode(&bytes), Some(shader()));
        assert_eq!(decode(&bytes[..bytes.len() - 1]), None);
        assert_eq!(decode(&[bytes.as_slice(), &[0]].concat()), None);
        assert_eq!(decode(b"VSH0"), None);

        let mut entry = encode_entry(&Ok(shader()));
        assert_eq!(decode_entry(&entry), Some(Ok(shader())));
        entry[60] ^= 1; // in the DXIL
        assert_eq!(decode_entry(&entry), None);
    }
}
