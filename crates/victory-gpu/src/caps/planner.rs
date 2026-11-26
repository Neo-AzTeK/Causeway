//! Pure planners: `&Caps` in, a decision out. No vendor, no driver, no GPU.
//!
//! Rules every planner here follows (a test enforces the first two):
//! 1. Unknown (`None`) is "not supported", never a guess.
//! 2. A better device never gets a worse plan: tiers are compared with
//!    `>=`, never `==`.
//! 3. A refusal carries what was needed and what the host has.

use super::{Caps, Refusal, ShaderModel};

/// Lane counts a shader needs to give the same results as on the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveNeed {
    /// The shader does not depend on how many lanes a wave has.
    Any,
    /// It needs a wave of `min` to `max` lanes (the console's wave64
    /// programs: 64 to 64).
    Lanes {
        min: u32,
        max: u32,
        /// The stage can pin a wave size (`[WaveSize]`: compute, mesh and
        /// amplification shaders only).
        pinnable: bool,
    },
}

/// How to run a shader that has a [`WaveNeed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WavePlan {
    /// The host's wave size is what the shader needs; nothing to ask for.
    Native,
    /// The host can run several sizes: pin this one (`[WaveSize(n)]`,
    /// shader model 6.6).
    Pinned(u32),
    Refuse(Refusal),
}

/// Which wave size serves `need` on this host.
pub fn wave_plan(caps: &Caps, need: WaveNeed) -> WavePlan {
    let WaveNeed::Lanes { min, max, pinnable } = need else {
        return WavePlan::Native;
    };
    let wanted = format!("waves of {min} to {max} lanes");
    let Some(host) = caps.wave else {
        return WavePlan::Refuse(Refusal::new("wave size", wanted, "unknown"));
    };
    let have = format!("{} to {}", host.min, host.max);
    let (lo, hi) = (min.max(host.min), max.min(host.max));
    if lo > hi {
        return WavePlan::Refuse(Refusal::new("wave size", wanted, have));
    }
    if host.min == host.max {
        return WavePlan::Native;
    }
    // Any size the host may pick is acceptable: nothing to ask for.
    if min <= host.min && host.max <= max {
        return WavePlan::Native;
    }
    // The host picks its own size within a range: only a pin is sure.
    if !pinnable {
        return WavePlan::Refuse(Refusal::new(
            "a wave size this stage can pin",
            format!("{wanted}, host range {have}"),
            "no pinning in this stage",
        ));
    }
    if caps.shader_model.is_none_or(|m| m < ShaderModel::SM6_6) {
        return WavePlan::Refuse(Refusal::new(
            "wave size pinning (shader model 6.6)",
            format!("{wanted}, host range {have}"),
            caps.shader_model.map_or_else(|| "unknown".into(), |m| format!("{:#x}", m.0)),
        ));
    }
    // Sizes are powers of two: the largest one inside the overlap.
    match (lo..=hi).rev().find(|n| n.is_power_of_two()) {
        Some(n) => WavePlan::Pinned(n),
        None => WavePlan::Refuse(Refusal::new("wave size", wanted, have)),
    }
}

/// Whether a console wave of 64 must be asked for as waves of 64: the host
/// cannot run waves of 32 (so two of them cannot stand in for one) and can run
/// 64. When false (also with an unknown range), a program the translator can
/// place runs as two waves of 32, which any host that offers 32 lanes takes.
pub fn wave64_native(caps: &Caps) -> bool {
    caps.wave.is_some_and(|w| w.min > 32 && w.max >= 64)
}

/// Whether pixel shaders may read `SV_Barycentrics`: a translated program
/// that does is refused where they may not.
pub fn barycentrics(caps: &Caps) -> bool {
    caps.barycentrics == Some(true)
}

/// Whether shaders may compute in double precision: a translated program
/// that does is refused where they may not.
pub fn doubles(caps: &Caps) -> bool {
    caps.doubles == Some(true)
}

/// Whether sampling with a least-level clamp is native (tiled resources
/// tier 2 or better).
pub fn min_lod_clamp(caps: &Caps) -> bool {
    caps.tiled_tier.is_some_and(|t| t >= 2)
}

/// Whether the host runs mesh shaders as translated ones need them (mesh
/// shader tier 1 and shader model 6.6).
pub fn mesh_shaders(caps: &Caps) -> bool {
    caps.mesh_tier.is_some_and(|t| t >= 1) && caps.shader_model.is_some_and(|m| m >= ShaderModel::SM6_6)
}

/// Whether 64-bit atomics work on typed resources reached through the
/// descriptor heap (both caps must say so).
pub fn atomics64_heap(caps: &Caps) -> bool {
    caps.typed_atomics64 == Some(true) && caps.heap_atomics64 == Some(true)
}

/// Whether a sampler may filter anisotropically with point mip selection;
/// where not, translated samplers take the nearest filter the host has.
pub fn aniso_point_mip(caps: &Caps) -> bool {
    caps.aniso_point_mip == Some(true)
}

/// Whether translated shaders can reach their descriptors. All of them index
/// the descriptor heaps directly (SM 6.6 `ResourceDescriptorHeap`, root
/// signatures flagged heap-directly-indexed), which D3D12 guarantees only
/// with shader model 6.6 and resource binding tier 3 together (DirectX-Specs,
/// "HLSL Dynamic Resources"). There is no fallback binding path.
pub fn heap_indexing(caps: &Caps) -> Result<(), Refusal> {
    if caps.shader_model.is_some_and(|m| m >= ShaderModel::SM6_6) && caps.binding_tier.is_some_and(|t| t >= 3) {
        return Ok(());
    }
    Err(Refusal::new(
        "heap-indexed descriptors",
        "shader model 6.6 and resource binding tier 3",
        format!(
            "shader model {}, binding tier {}",
            caps.shader_model.map_or_else(|| "unknown".into(), |m| format!("{:#x}", m.0)),
            caps.binding_tier.map_or_else(|| "unknown".into(), |t| t.to_string()),
        ),
    ))
}

/// How much the binding model allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingClass {
    /// Tier 3: no limit on the descriptors a table may span.
    Bindless,
    /// Tier 2: unbounded SRV tables, bounded CBV/UAV counts.
    Tables,
    /// Tier 1, or unknown: fixed counts per stage; assume the least.
    Slots,
}

/// What descriptor heap and root signature code may rely on. Limits are the
/// D3D12 specification's per tier; a driver may allow more, never less.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingPlan {
    pub class: BindingClass,
    /// Views in one shader-visible heap the tier guarantees; `None`: the
    /// tier sets no bound, so try the size wanted and treat a creation
    /// failure as the refusal.
    pub max_views: Option<u32>,
    /// Samplers in one shader-visible heap (every tier).
    pub max_samplers: u32,
}

pub fn binding_plan(caps: &Caps) -> BindingPlan {
    match caps.binding_tier {
        Some(t) if t >= 3 => BindingPlan {
            class: BindingClass::Bindless,
            max_views: None,
            max_samplers: 2048,
        },
        Some(2) => BindingPlan {
            class: BindingClass::Tables,
            max_views: Some(1_000_000),
            max_samplers: 2048,
        },
        _ => BindingPlan {
            class: BindingClass::Slots,
            max_views: Some(1_000_000),
            max_samplers: 2048,
        },
    }
}

/// Where a memory budget came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSource {
    /// The OS's budget for this process.
    Queried,
    /// Dedicated video memory (a discrete card, query failed).
    Dedicated,
    /// Half the shared memory (a UMA part, query failed).
    SharedHalf,
    /// Nothing known: [`FALLBACK_BUDGET`].
    Fallback,
}

/// The budget when the adapter reports nothing (a guess).
pub const FALLBACK_BUDGET: u64 = 4 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryPlan {
    pub budget: u64,
    pub source: BudgetSource,
}

/// The video memory budget at device creation. A UMA part's dedicated figure
/// (often 128 MiB) is not its memory: its shared figure is.
pub fn memory_plan(caps: &Caps) -> MemoryPlan {
    let m = &caps.memory;
    let plan = |budget, source| MemoryPlan { budget, source };
    if let Some(b) = m.budget.filter(|&b| b > 0) {
        return plan(b, BudgetSource::Queried);
    }
    if caps.uma == Some(true) && m.shared > 0 {
        return plan(m.shared / 2, BudgetSource::SharedHalf);
    }
    if caps.uma == Some(false) && m.dedicated > 0 {
        return plan(m.dedicated, BudgetSource::Dedicated);
    }
    // UMA unknown: the larger figure (a small dedicated one may be a UMA
    // part's window, not its memory).
    if caps.uma.is_none() && m.dedicated > m.shared / 2 {
        return plan(m.dedicated, BudgetSource::Dedicated);
    }
    if m.shared > 0 {
        return plan(m.shared / 2, BudgetSource::SharedHalf);
    }
    plan(FALLBACK_BUDGET, BudgetSource::Fallback)
}

/// The first of `candidates` (`(DXGI_FORMAT, required support bits)`) the
/// host supports; the order is the preference, the last is the fallback.
pub fn format_choice(caps: &Caps, candidates: &[(u32, u32)]) -> Result<u32, Refusal> {
    candidates
        .iter()
        .find(|&&(f, flags)| caps.format_supports(f, flags))
        .map(|&(f, _)| f)
        .ok_or_else(|| {
            Refusal::new(
                "a usable format",
                format!("one of {:?}", candidates.iter().map(|c| c.0).collect::<Vec<_>>()),
                "none probed as supported",
            )
        })
}
