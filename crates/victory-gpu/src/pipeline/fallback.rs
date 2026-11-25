//! `VICTORY_REFUSED_FALLBACK=1` (off by default): a graphics pipeline whose
//! pixel program is refused (it does not translate, or the host refuses
//! the pipeline) is made again with a flat-colour substitute pixel shader
//! (`victory_shader::emit::flat_pixel_shader`), so its geometry is still
//! drawn, with the title's blend, depth and raster state. Only the
//! shading is wrong.
//!
//! The refusal is still logged and still counts as one (`refused`); the
//! substitute adds `pipelines` and `draws`. A vertex or mesh program that
//! is refused has no substitute: that pipeline stays without a host
//! pipeline and its draws are dropped, as without the switch. Compute
//! dispatches are never substituted; they are dropped and counted there.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Dxgi::Common::*;

use super::host::GraphicsState;
use super::Shader;

/// Tests turn the switch on without the variable.
static FORCED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn enabled() -> bool {
    static ENV: OnceLock<bool> = OnceLock::new();
    FORCED.load(Relaxed)
        || *ENV.get_or_init(|| {
            crate::switch::on("VICTORY_REFUSED_FALLBACK")
        })
}

/// Pixel programs refused and replaced, host pipelines made with a
/// substitute, draws run with one.
static REFUSED: AtomicU64 = AtomicU64::new(0);
static PIPELINES: AtomicU64 = AtomicU64::new(0);
static DRAWS: AtomicU64 = AtomicU64::new(0);

/// Refused pixel pipelines that got a substitute, host pipelines made with
/// one, and draws run with one, since the start.
pub fn counts() -> (u64, u64, u64) {
    (REFUSED.load(Relaxed), PIPELINES.load(Relaxed), DRAWS.load(Relaxed))
}

/// A draw ran with a substitute.
pub(crate) fn drew() {
    DRAWS.fetch_add(1, Relaxed);
}

/// Notes (and logs, per pipeline) that `ps` was refused and replaced.
pub(crate) fn made(ps: &Shader) {
    REFUSED.fetch_add(1, Relaxed);
    PIPELINES.fetch_add(1, Relaxed);
    tracing::warn!(
        pixel = %ps.hash,
        "pixel program refused: drawn with a flat substitute (VICTORY_REFUSED_FALLBACK)"
    );
}

/// Whether a render target format takes a floating-point colour (unorm,
/// snorm, float, sRGB): an integer or unknown one gets no substitute
/// output.
fn float_target(f: DXGI_FORMAT) -> bool {
    !matches!(
        f,
        DXGI_FORMAT_UNKNOWN
            | DXGI_FORMAT_R32G32B32A32_UINT
            | DXGI_FORMAT_R32G32B32A32_SINT
            | DXGI_FORMAT_R32G32B32_UINT
            | DXGI_FORMAT_R32G32B32_SINT
            | DXGI_FORMAT_R16G16B16A16_UINT
            | DXGI_FORMAT_R16G16B16A16_SINT
            | DXGI_FORMAT_R32G32_UINT
            | DXGI_FORMAT_R32G32_SINT
            | DXGI_FORMAT_R10G10B10A2_UINT
            | DXGI_FORMAT_R8G8B8A8_UINT
            | DXGI_FORMAT_R8G8B8A8_SINT
            | DXGI_FORMAT_R16G16_UINT
            | DXGI_FORMAT_R16G16_SINT
            | DXGI_FORMAT_R32_UINT
            | DXGI_FORMAT_R32_SINT
            | DXGI_FORMAT_R8G8_UINT
            | DXGI_FORMAT_R8G8_SINT
            | DXGI_FORMAT_R16_UINT
            | DXGI_FORMAT_R16_SINT
            | DXGI_FORMAT_R8_UINT
            | DXGI_FORMAT_R8_SINT
    )
}

/// The render target slots of `g` the substitute writes.
fn slots(g: &GraphicsState) -> Vec<u32> {
    (0..g.targets.NumRenderTargets.min(8))
        .filter(|&i| float_target(g.targets.RTFormats[i as usize]))
        .collect()
}

/// The substitute pixel shader for `g`'s render targets, one per set of
/// slots.
pub(crate) fn pixel_shader(g: &GraphicsState) -> Vec<u8> {
    static CACHE: Mutex<Option<HashMap<Vec<u32>, Vec<u8>>>> = Mutex::new(None);
    let slots = slots(g);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .get_or_insert_with(HashMap::new)
        .entry(slots)
        .or_insert_with_key(|s| victory_shader::emit::flat_pixel_shader(s))
        .clone()
}

#[cfg(all(test, feature = "diagnostics"))]
pub(crate) fn force(on: bool) {
    FORCED.store(on, Relaxed);
}
