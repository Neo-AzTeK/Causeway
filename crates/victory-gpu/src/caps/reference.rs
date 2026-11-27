//! The reference adapter set: fake caps of each class of device the
//! planners must serve, for tests that need no GPU.
//!
//! A profile stands for a class, not a card, and sets the traits that matter
//! to a planner (wave range, tiers, UMA, memory). So that nobody takes a
//! figure for more than it is, each is listed in [`PROFILES`] with the
//! [`Source`] of its figures. When a device of a class can be read, prefer
//! what it reports and update the source with it; the profile's own comment
//! says which figures that reading did not cover.

use super::{AdapterInfo, Caps, ExistingHeaps, MemoryCaps, ShaderModel, WaveLanes};
use std::collections::BTreeMap;

const GIB: u64 = 1 << 30;

/// Where a profile's figures come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Public vendor and D3D12 documentation, and inference from it: not a
    /// dump of a device.
    Documented,
    /// What a real device wrote to a log; `date` (`YYYY-MM-DD`) is that of
    /// the log. Figures the log lacks are filled in by other means.
    Logged { adapter: &'static str, date: &'static str },
}

/// One entry of the reference set.
pub struct Profile {
    pub caps: fn() -> Caps,
    pub source: Source,
}

const fn documented(caps: fn() -> Caps) -> Profile {
    Profile { caps, source: Source::Documented }
}

/// Every profile and the source of its figures.
pub const PROFILES: &[Profile] = &[
    documented(nvidia_modern),
    documented(nvidia_maxwell1),
    documented(amd_rdna),
    documented(amd_gcn),
    documented(intel_arc),
    documented(intel_igpu),
    documented(warp),
    documented(nvidia_maxwell),
    Profile { caps: nvidia_pascal, source: Source::Logged { adapter: "NVIDIA GeForce GTX 1070", date: "2026-11-25" } },
    documented(nvidia_turing),
    documented(nvidia_ampere),
    documented(nvidia_ada),
    documented(nvidia_blackwell),
];

fn base(name: &str, vendor_id: u32, wave: (u32, u32), tiers: (u8, u8, u8)) -> Caps {
    Caps {
        generation: 0,
        adapter: AdapterInfo { name: name.into(), vendor_id, ..AdapterInfo::default() },
        wave: Some(WaveLanes { min: wave.0, max: wave.1 }),
        shader_model: Some(ShaderModel(0x67)),
        binding_tier: Some(tiers.0),
        tiled_tier: Some(tiers.1),
        mesh_tier: Some(tiers.2),
        typed_atomics64: Some(false),
        heap_atomics64: Some(false),
        aniso_point_mip: Some(false),
        doubles: Some(true),
        barycentrics: Some(true),
        uma: Some(false),
        memory: MemoryCaps { budget: Some(7 * GIB), dedicated: 8 * GIB, shared: 16 * GIB },
        existing_heaps: ExistingHeaps::None,
        formats: BTreeMap::new(),
        workarounds: Vec::new(),
    }
}

/// An NVIDIA card with mesh shaders, the name older tests use (the
/// generations are below).
pub fn nvidia_modern() -> Caps {
    Caps { typed_atomics64: Some(true), heap_atomics64: Some(true), ..base("nvidia-modern", 0x10de, (32, 32), (3, 3, 1)) }
}

// NVIDIA by generation. Every one runs waves of 32 only (no NVIDIA part has
// waves of 64). What the planners read and what they rest on:
// - mesh shader tier 1 comes with feature level 12_2 (Turing and on; the
//   GTX 16 cards are listed at 12_1 and are not profiled here);
// - 64-bit typed atomics: on in the Pascal log, on for every later
//   generation (inferred); off for Maxwell (a guess, the conservative side);
// - shader model: what a current driver reports (6.8 in the Pascal log, later
//   ones inferred, Maxwell 6.7 from Wikipedia's GeForce 700/900 pages). The
//   planners compare only against 6.6;
// - binding tier 3 is a 12_2 requirement and in the Pascal log; on Maxwell 2 it
//   is from driver reports (unverified);
// - tiled tier 3 is a 12_2 requirement; Maxwell's is a guess;
// - memory is a typical card of the generation; anisotropic filtering with
//   point mips is left off (never seen reported).

/// NVIDIA Maxwell 1st generation (GM107: GTX 745/750/750 Ti), feature level
/// 11_0. Reports of its binding tier disagree (2, or 3 on later drivers):
/// this is the pessimistic side, a device translated shaders cannot use.
/// Tiled tier and shader model are guesses.
pub fn nvidia_maxwell1() -> Caps {
    Caps {
        shader_model: Some(ShaderModel(0x67)),
        memory: MemoryCaps { budget: Some(GIB * 3 / 2), dedicated: 2 * GIB, shared: 8 * GIB },
        ..base("nvidia-maxwell1", 0x10de, (32, 32), (2, 1, 0))
    }
}

/// NVIDIA Maxwell 2 (GTX 900), feature level 12_1: no mesh shaders.
pub fn nvidia_maxwell() -> Caps {
    Caps {
        shader_model: Some(ShaderModel(0x67)),
        memory: MemoryCaps { budget: Some(3 * GIB), dedicated: 4 * GIB, shared: 8 * GIB },
        ..base("nvidia-maxwell", 0x10de, (32, 32), (3, 3, 0))
    }
}

/// NVIDIA Pascal, read from a start log (the `caps::probe` summary and the
/// host's path lines). `shared`, the driver version and `aniso_point_mip` are
/// not in that log.
pub fn nvidia_pascal() -> Caps {
    Caps {
        shader_model: Some(ShaderModel(0x68)),
        typed_atomics64: Some(true),
        heap_atomics64: Some(true),
        memory: MemoryCaps { budget: Some(7_653_556_224), dedicated: 8067 << 20, shared: 16 * GIB },
        barycentrics: Some(false),
        ..base("nvidia-pascal", 0x10de, (32, 32), (3, 4, 0))
    }
}

/// NVIDIA Turing (RTX 20), feature level 12_2: mesh shaders tier 1.
pub fn nvidia_turing() -> Caps {
    Caps {
        shader_model: Some(ShaderModel(0x68)),
        typed_atomics64: Some(true),
        heap_atomics64: Some(true),
        ..base("nvidia-turing", 0x10de, (32, 32), (3, 3, 1))
    }
}

/// NVIDIA Ampere (RTX 30), feature level 12_2.
pub fn nvidia_ampere() -> Caps {
    Caps {
        memory: MemoryCaps { budget: Some(10 * GIB), dedicated: 12 * GIB, shared: 16 * GIB },
        adapter: nvidia_adapter("nvidia-ampere"),
        ..nvidia_turing()
    }
}

/// NVIDIA Ada Lovelace (RTX 40), feature level 12_2.
pub fn nvidia_ada() -> Caps {
    Caps {
        memory: MemoryCaps { budget: Some(14 * GIB), dedicated: 16 * GIB, shared: 24 * GIB },
        adapter: nvidia_adapter("nvidia-ada"),
        ..nvidia_turing()
    }
}

/// NVIDIA Blackwell (RTX 50), feature level 12_2.
pub fn nvidia_blackwell() -> Caps {
    Caps {
        memory: MemoryCaps { budget: Some(28 * GIB), dedicated: 32 * GIB, shared: 48 * GIB },
        adapter: nvidia_adapter("nvidia-blackwell"),
        ..nvidia_turing()
    }
}

fn nvidia_adapter(name: &str) -> AdapterInfo {
    AdapterInfo { name: name.into(), vendor_id: 0x10de, ..AdapterInfo::default() }
}

/// The NVIDIA generations, oldest first (`nvidia_modern` is the class name
/// older tests use, `nvidia_maxwell1` a side case of Maxwell).
pub fn nvidia_generations() -> Vec<Caps> {
    vec![nvidia_maxwell(), nvidia_pascal(), nvidia_turing(), nvidia_ampere(), nvidia_ada(), nvidia_blackwell()]
}

/// AMD RDNA2/3 class: wave32 or wave64 at the driver's choice.
pub fn amd_rdna() -> Caps {
    Caps { typed_atomics64: Some(true), heap_atomics64: Some(true), ..base("amd-rdna", 0x1002, (32, 64), (3, 3, 1)) }
}

/// AMD GCN class: wave64 only.
pub fn amd_gcn() -> Caps {
    Caps { shader_model: Some(ShaderModel(0x65)), ..base("amd-gcn", 0x1002, (64, 64), (3, 3, 0)) }
}

/// Intel Arc class: a wide, variable range.
pub fn intel_arc() -> Caps {
    Caps { typed_atomics64: Some(true), ..base("intel-arc", 0x8086, (8, 32), (3, 3, 1)) }
}

/// Intel integrated class: shared memory, a small "dedicated" figure.
pub fn intel_igpu() -> Caps {
    Caps {
        uma: Some(true),
        memory: MemoryCaps { budget: Some(6 * GIB), dedicated: 128 << 20, shared: 16 * GIB },
        ..base("intel-igpu", 0x8086, (8, 32), (3, 1, 0))
    }
}

/// WARP, the software adapter: tiny waves, no real memory.
pub fn warp() -> Caps {
    let mut c = base("warp", 0x1414, (4, 4), (3, 3, 0));
    c.adapter.software = true;
    c.uma = Some(true);
    c.memory = MemoryCaps { budget: Some(8 * GIB), dedicated: 0, shared: 16 * GIB };
    c
}

/// Every class above, by name.
pub fn all() -> Vec<Caps> {
    PROFILES.iter().map(|p| (p.caps)()).collect()
}
