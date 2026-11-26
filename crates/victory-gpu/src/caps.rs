//! What the host GPU can do, asked once, decided by pure functions.
//!
//! **The rule:** only this module asks the host GPU what it can do. Code
//! elsewhere reads a [`Caps`] field or a planner; the one question a snapshot
//! cannot answer, about a single resource, is in `per_resource`.
//! `cargo xtask policy` rejects a query made anywhere else; the few calls it
//! allows elsewhere, and why, are data in `xtask/src/capability.rs`.
//!
//! [`Caps`] is a snapshot of one device, filled when the device is created
//! (`Caps::probe`, Windows only) and never changed after. Every field that a
//! query can fail to fill is an `Option`: `None` means "unknown" and every
//! planner treats it as "not supported". The planners (`wave_plan`,
//! `binding_plan`, `memory_plan`, ...) are pure functions of `&Caps`; they
//! never see the vendor or the driver, so the whole decision table runs in
//! tests with invented caps and no GPU (`reference` holds the adapter classes
//! tests use). How to add a capability, test its fallback and review a
//! change: `docs/hardware-range.md`.
//!
//! Vendor, device and driver are data for the log. The one door to a
//! vendor-specific behaviour is [`WORKAROUNDS`]: an entry names a bug and a
//! bounded driver range, and corrects the caps the driver reported, so the
//! planners stay vendor-free.
//!
//! Not here: live figures (video memory in use changes every frame; the
//! budget policy samples it itself) and descriptor or heap code (which does not
//! call [`binding_plan`] today).

#[cfg(windows)]
pub(crate) mod per_resource;
mod planner;
#[cfg(windows)]
mod probe;
pub mod reference;
#[cfg(test)]
mod tests;

pub use planner::{
    BindingClass, BindingPlan, BudgetSource, FALLBACK_BUDGET, MemoryPlan, WaveNeed, WavePlan, aniso_point_mip,
    atomics64_heap, binding_plan, format_choice, heap_indexing, memory_plan, mesh_shaders, min_lod_clamp,
    wave64_native, wave_plan,
};

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Lane counts of one wave the host can run (`WaveLaneCountMin`/`Max`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaveLanes {
    pub min: u32,
    pub max: u32,
}

/// A shader model in D3D's own encoding: `0x66` is 6.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ShaderModel(pub u32);

impl ShaderModel {
    pub const SM6_6: Self = Self(0x66);
}

/// Memory as the adapter reports it when the device is made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryCaps {
    /// The OS budget for this process, local segment
    /// (`QueryVideoMemoryInfo`), when the query worked.
    pub budget: Option<u64>,
    /// `DedicatedVideoMemory` (small and meaningless on a UMA part).
    pub dedicated: u64,
    /// `SharedSystemMemory`.
    pub shared: u64,
}

/// Who the adapter is. For logs and [`WORKAROUNDS`] only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdapterInfo {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    /// `CheckInterfaceSupport` user-mode driver version (a packed 64-bit
    /// number), when the call worked.
    pub driver_version: Option<u64>,
    /// A software adapter (WARP).
    pub software: bool,
}

/// What a device reports it supports, for one device creation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caps {
    /// Increases with every probe in the process: a holder that keeps
    /// something derived from a `Caps` compares it to know it is stale after
    /// the device was lost and made again.
    pub generation: u64,
    pub adapter: AdapterInfo,
    /// `None` also when the device reports no wave operations.
    pub wave: Option<WaveLanes>,
    pub shader_model: Option<ShaderModel>,
    /// `ResourceBindingTier`: 1, 2 or 3.
    pub binding_tier: Option<u8>,
    /// `TiledResourcesTier`: 0 (none) to 4.
    pub tiled_tier: Option<u8>,
    /// `MeshShaderTier`: 0 (none) or 1.
    pub mesh_tier: Option<u8>,
    /// `AtomicInt64OnTypedResourceSupported` (OPTIONS9).
    pub typed_atomics64: Option<bool>,
    /// `AtomicInt64OnDescriptorHeapResourceSupported` (OPTIONS11).
    pub heap_atomics64: Option<bool>,
    /// `AnisoFilterWithPointMipSupported` (OPTIONS19).
    pub aniso_point_mip: Option<bool>,
    /// The adapter shares memory with the CPU (`CacheCoherentUMA` is not
    /// required).
    pub uma: Option<bool>,
    pub memory: MemoryCaps,
    /// `D3D12_FORMAT_SUPPORT1` bits of each format the probe was asked for,
    /// by `DXGI_FORMAT` value. A format that was not asked for is absent,
    /// and absent means unsupported.
    pub formats: BTreeMap<u32, u32>,
    /// Ids of the [`WORKAROUNDS`] entries applied to these caps.
    pub workarounds: Vec<&'static str>,
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

impl Caps {
    /// Nothing known: every query failed. The planners' most conservative
    /// input.
    pub fn unknown() -> Self {
        Self::default()
    }

    /// A fresh generation number (probes use it; tests may too).
    pub fn next_generation() -> u64 {
        NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
    }

    /// Whether `format` was probed with every bit of `flags` supported.
    pub fn format_supports(&self, format: u32, flags: u32) -> bool {
        self.formats.get(&format).is_some_and(|have| have & flags == flags)
    }

    /// Correct what the driver reported using `table`, once, at probe time.
    /// Returns how many entries applied.
    pub fn apply_workarounds(&mut self, table: &[Workaround]) -> usize {
        let mut applied = 0;
        for w in table {
            if w.matches(&self.adapter) {
                (w.fix)(self);
                self.workarounds.push(w.id);
                applied += 1;
            }
        }
        applied
    }

    /// The device's figures as the run digest shows them (`cargo xtask`
    /// reads the line they are logged in): `n/a` where the device did not
    /// say.
    pub fn digest(&self) -> String {
        let n = |v: Option<u8>| v.map_or_else(|| "n/a".to_owned(), |v| v.to_string());
        format!(
            "wave {}, binding tier {}, mesh tier {}, shader model {}",
            self.wave.map_or_else(|| "n/a".to_owned(), |w| format!("{}-{}", w.min, w.max)),
            n(self.binding_tier),
            n(self.mesh_tier),
            self.shader_model.map_or_else(|| "n/a".to_owned(), |m| format!("{:#x}", m.0)),
        )
    }

    /// One line for the log: what was found, what is unknown, who the
    /// adapter is (data, never a branch).
    pub fn summary(&self) -> String {
        fn o<T: fmt::Display>(v: Option<T>) -> String {
            v.map_or_else(|| "?".into(), |v| v.to_string())
        }
        format!(
            "{} (vendor {:#06x}, device {:#06x}{}): waves {}, SM {}, binding tier {}, \
             tiled tier {}, mesh tier {}, UMA {}, budget {}, workarounds {:?}",
            self.adapter.name,
            self.adapter.vendor_id,
            self.adapter.device_id,
            if self.adapter.software { ", software" } else { "" },
            self.wave.map_or_else(|| "?".into(), |w| format!("{}..{}", w.min, w.max)),
            self.shader_model.map_or_else(|| "?".into(), |m| format!("{:#x}", m.0)),
            o(self.binding_tier),
            o(self.tiled_tier),
            o(self.mesh_tier),
            o(self.uma),
            o(self.memory.budget),
            self.workarounds,
        )
    }
}

/// A clean refusal: what the host lacks, what was needed, what it has. Its
/// `Display` is the log line; it names no vendor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub feature: &'static str,
    pub need: String,
    pub have: String,
}

impl Refusal {
    pub(crate) fn new(feature: &'static str, need: impl Into<String>, have: impl Into<String>) -> Self {
        Self { feature, need: need.into(), have: have.into() }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the host GPU lacks {}: needs {}, has {}", self.feature, self.need, self.have)
    }
}

/// A known driver bug, corrected in the caps. Every entry names the bug and a
/// bounded driver range: an entry without both is rejected by a test.
pub struct Workaround {
    pub id: &'static str,
    pub vendor_id: u32,
    /// Matches this device only, or every device of the vendor.
    pub device_id: Option<u32>,
    /// Matches driver versions in `[from, below)`; both bounds required.
    pub driver_from: u64,
    pub driver_below: u64,
    /// The bug, with a reference a reader can follow.
    pub bug: &'static str,
    /// Overwrites the wrong field (for example clears a feature the driver
    /// claims but breaks).
    pub fix: fn(&mut Caps),
}

impl Workaround {
    fn matches(&self, a: &AdapterInfo) -> bool {
        a.vendor_id == self.vendor_id
            && self.device_id.is_none_or(|d| d == a.device_id)
            && a.driver_version.is_some_and(|v| (self.driver_from..self.driver_below).contains(&v))
    }
}

/// Known bugs. Empty until a real device is seen to lie: add an entry with
/// its evidence (`docs/hardware-range.md`).
pub static WORKAROUNDS: &[Workaround] = &[];
