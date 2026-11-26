//! The reference adapter set: invented caps of each class of device the
//! planners must serve, for tests that need no GPU.
//!
//! Figures are the classes' typical reports written from public vendor and
//! D3D12 documentation, NOT dumps from those devices (except `nvidia_pascal`,
//! which holds the figures a GTX 1070 logged); only the class traits
//! that matter to a planner (wave range, tiers, UMA, memory) are set. When
//! someone owns a device, replace its entry with that device's `{:#?}` of
//! `Caps` and say so in the commit. Names say what class, not what card.

use super::{AdapterInfo, Caps, MemoryCaps, ShaderModel, WaveLanes};
use std::collections::BTreeMap;

const GIB: u64 = 1 << 30;

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
        uma: Some(false),
        memory: MemoryCaps { budget: Some(7 * GIB), dedicated: 8 * GIB, shared: 16 * GIB },
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
// - Pascal's figures are the GTX 1070's own start log (below);
// - 64-bit typed atomics: on, as that log says, for every later generation
//   (inferred); off for Maxwell (a guess, the conservative side);
// - shader model: what a current driver reports (Pascal 6.8 seen, later ones
//   inferred, Maxwell 6.7 from Wikipedia's GeForce 700/900 pages). The
//   planners compare only against 6.6;
// - binding tier 3 is a 12_2 requirement and seen on Pascal; on Maxwell 2 it
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

/// NVIDIA Pascal: the owner's GTX 1070 as its start log reports it
/// (2026-11, `caps::probe` summary and the host's path lines). `shared`,
/// the driver version and `aniso_point_mip` are not in the log.
pub fn nvidia_pascal() -> Caps {
    Caps {
        shader_model: Some(ShaderModel(0x68)),
        typed_atomics64: Some(true),
        heap_atomics64: Some(true),
        memory: MemoryCaps { budget: Some(7_653_556_224), dedicated: 8067 << 20, shared: 16 * GIB },
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

/// The NVIDIA generations, oldest first (`nvidia_pascal` is the owner's
/// GTX 1070, `nvidia_modern` the class name older tests use,
/// `nvidia_maxwell1` a side case of Maxwell).
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
    let mut all = vec![nvidia_modern(), nvidia_maxwell1(), amd_rdna(), amd_gcn(), intel_arc(), intel_igpu(), warp()];
    all.extend(nvidia_generations());
    all
}
