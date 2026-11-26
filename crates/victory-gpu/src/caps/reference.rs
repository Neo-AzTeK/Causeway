//! The reference adapter set: invented caps of each class of device the
//! planners must serve, for tests that need no GPU.
//!
//! Figures are the classes' typical reports written from public vendor and
//! D3D12 documentation, NOT dumps from those devices; only the class traits
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

/// NVIDIA Turing/Ampere/Ada class: wave32 only (the owner's card is this
/// class).
pub fn nvidia_modern() -> Caps {
    Caps { typed_atomics64: Some(true), heap_atomics64: Some(true), ..base("nvidia-modern", 0x10de, (32, 32), (3, 3, 1)) }
}

/// NVIDIA Pascal class: wave32, no mesh shaders, no 64-bit typed atomics,
/// older shader model.
pub fn nvidia_pascal() -> Caps {
    Caps { shader_model: Some(ShaderModel(0x66)), ..base("nvidia-pascal", 0x10de, (32, 32), (3, 3, 0)) }
}

// NVIDIA by generation. Every one runs waves of 32 only. What differs between
// them, and what the planners read: mesh shaders (D3D12 mesh shader tier 1
// exists from Turing, feature level 12_2), 64-bit atomics on typed resources
// (kept off before Ampere, where no report was seen: the conservative side),
// and the shader model a current driver reports (the planners compare only
// against 6.6). Memory is a typical card of the generation. Binding tier 3 and
// tiled resources tier 3 hold from Maxwell 2 (feature level 12_1) on.

/// NVIDIA Maxwell 2 (GTX 900), feature level 12_1: no mesh shaders.
pub fn nvidia_maxwell() -> Caps {
    Caps {
        shader_model: Some(ShaderModel(0x66)),
        memory: MemoryCaps { budget: Some(3 * GIB), dedicated: 4 * GIB, shared: 8 * GIB },
        ..base("nvidia-maxwell", 0x10de, (32, 32), (3, 3, 0))
    }
}

/// NVIDIA Turing (RTX 20), feature level 12_2: mesh shaders tier 1.
pub fn nvidia_turing() -> Caps {
    Caps { shader_model: Some(ShaderModel(0x67)), ..base("nvidia-turing", 0x10de, (32, 32), (3, 3, 1)) }
}

/// NVIDIA Ampere (RTX 30), feature level 12_2.
pub fn nvidia_ampere() -> Caps {
    Caps {
        typed_atomics64: Some(true),
        heap_atomics64: Some(true),
        memory: MemoryCaps { budget: Some(10 * GIB), dedicated: 12 * GIB, shared: 16 * GIB },
        ..base("nvidia-ampere", 0x10de, (32, 32), (3, 3, 1))
    }
}

/// NVIDIA Ada Lovelace (RTX 40), feature level 12_2, shader model 6.8.
pub fn nvidia_ada() -> Caps {
    Caps {
        shader_model: Some(ShaderModel(0x68)),
        typed_atomics64: Some(true),
        heap_atomics64: Some(true),
        memory: MemoryCaps { budget: Some(14 * GIB), dedicated: 16 * GIB, shared: 24 * GIB },
        ..base("nvidia-ada", 0x10de, (32, 32), (3, 3, 1))
    }
}

/// NVIDIA Blackwell (RTX 50), feature level 12_2, shader model 6.8.
pub fn nvidia_blackwell() -> Caps {
    Caps {
        shader_model: Some(ShaderModel(0x68)),
        typed_atomics64: Some(true),
        heap_atomics64: Some(true),
        memory: MemoryCaps { budget: Some(28 * GIB), dedicated: 32 * GIB, shared: 48 * GIB },
        ..base("nvidia-blackwell", 0x10de, (32, 32), (3, 3, 1))
    }
}

/// The NVIDIA generations, oldest first (`nvidia_pascal` is the owner's
/// GTX 1070 class, `nvidia_modern` the class name older tests use).
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
    let mut all = vec![nvidia_modern(), amd_rdna(), amd_gcn(), intel_arc(), intel_igpu(), warp()];
    all.extend(nvidia_generations());
    all
}
