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
        doubles: Some(true),
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
    vec![nvidia_modern(), nvidia_pascal(), amd_rdna(), amd_gcn(), intel_arc(), intel_igpu(), warp()]
}
