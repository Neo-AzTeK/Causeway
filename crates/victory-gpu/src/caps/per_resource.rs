//! Queries about one resource's description, which no snapshot of the device
//! answers: the title picks its formats while it runs. They live here because
//! only `caps/` asks the host GPU (`cargo xtask policy` holds that).
//!
//! What the device as a whole can do is never asked here: that is a [`Caps`]
//! field.
//!
//! [`Caps`]: super::Caps

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;

/// The uses `device` has for `format`; `None` when the query fails, which the
/// caller must decide the meaning of.
pub(crate) fn format_support(device: &ID3D12Device, format: DXGI_FORMAT) -> Option<D3D12_FORMAT_SUPPORT1> {
    let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT { Format: format, ..Default::default() };
    // SAFETY: the query writes its own structure of the given size.
    unsafe {
        device.CheckFeatureSupport(
            D3D12_FEATURE_FORMAT_SUPPORT,
            (&raw mut support).cast(),
            size_of_val(&support) as u32,
        )
    }
    .ok()
    .map(|()| support.Support1)
}
