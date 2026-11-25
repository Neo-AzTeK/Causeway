//! Filling [`Caps`] from a D3D12 device. Every query is allowed to fail: a
//! failure leaves its field `None` (Microsoft's advice for older runtimes
//! that do not know a feature structure).

use super::{AdapterInfo, Caps, MemoryCaps, ShaderModel, WORKAROUNDS, WaveLanes};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

/// One feature query into a zeroed structure; `None` when it fails.
fn query<T: Default>(device: &ID3D12Device, feature: D3D12_FEATURE) -> Option<T> {
    let mut data = T::default();
    // SAFETY: `data` is a live structure of the size passed; the runtime
    // writes at most that many bytes.
    unsafe { device.CheckFeatureSupport(feature, (&raw mut data).cast(), size_of::<T>() as u32) }
        .ok()
        .map(|()| data)
}

/// The highest shader model the runtime and driver accept, asked from the
/// newest down (a runtime that does not know a model fails the query).
fn shader_model(device: &ID3D12Device) -> Option<ShaderModel> {
    [0x69, 0x68, 0x67, 0x66, 0x65, 0x64, 0x63, 0x62, 0x61, 0x60].into_iter().find_map(|asked| {
        let mut m = D3D12_FEATURE_DATA_SHADER_MODEL { HighestShaderModel: D3D_SHADER_MODEL(asked) };
        // SAFETY: as in `query`.
        unsafe {
            device.CheckFeatureSupport(
                D3D12_FEATURE_SHADER_MODEL,
                (&raw mut m).cast(),
                size_of_val(&m) as u32,
            )
        }
        .ok()
        .map(|()| ShaderModel(m.HighestShaderModel.0 as u32))
    })
}

impl Caps {
    /// Ask `device` everything the planners use, once. `formats` are the
    /// formats the caller will choose between (see `format_choice`); a
    /// format left out is reported unsupported. Applies [`WORKAROUNDS`] and
    /// takes a new generation: call it again for every device made, never
    /// reuse a `Caps` across a device loss.
    pub fn probe(device: &ID3D12Device, formats: &[DXGI_FORMAT]) -> Caps {
        let options = query::<D3D12_FEATURE_DATA_D3D12_OPTIONS>(device, D3D12_FEATURE_D3D12_OPTIONS);
        let waves = query::<D3D12_FEATURE_DATA_D3D12_OPTIONS1>(device, D3D12_FEATURE_D3D12_OPTIONS1);
        let o7 = query::<D3D12_FEATURE_DATA_D3D12_OPTIONS7>(device, D3D12_FEATURE_D3D12_OPTIONS7);
        let o9 = query::<D3D12_FEATURE_DATA_D3D12_OPTIONS9>(device, D3D12_FEATURE_D3D12_OPTIONS9);
        let o11 = query::<D3D12_FEATURE_DATA_D3D12_OPTIONS11>(device, D3D12_FEATURE_D3D12_OPTIONS11);
        let o19 = query::<D3D12_FEATURE_DATA_D3D12_OPTIONS19>(device, D3D12_FEATURE_D3D12_OPTIONS19);
        let arch = query::<D3D12_FEATURE_DATA_ARCHITECTURE1>(device, D3D12_FEATURE_ARCHITECTURE1);
        let (adapter, memory) = adapter_info(device);
        let mut caps = Caps {
            generation: Caps::next_generation(),
            adapter,
            wave: waves
                .filter(|o| o.WaveOps.as_bool() && o.WaveLaneCountMin > 0)
                .map(|o| WaveLanes { min: o.WaveLaneCountMin, max: o.WaveLaneCountMax }),
            shader_model: shader_model(device),
            binding_tier: options.map(|o| o.ResourceBindingTier.0 as u8),
            tiled_tier: options.map(|o| o.TiledResourcesTier.0 as u8),
            // MESH_SHADER_TIER_1 is 10 in D3D12.
            mesh_tier: o7.map(|o| u8::from(o.MeshShaderTier != D3D12_MESH_SHADER_TIER_NOT_SUPPORTED)),
            typed_atomics64: o9.map(|o| o.AtomicInt64OnTypedResourceSupported.as_bool()),
            heap_atomics64: o11.map(|o| o.AtomicInt64OnDescriptorHeapResourceSupported.as_bool()),
            aniso_point_mip: o19.map(|o| o.AnisoFilterWithPointMipSupported.as_bool()),
            uma: arch.map(|a| a.UMA.as_bool()),
            memory,
            formats: formats
                .iter()
                .filter_map(|&f| {
                    let mut s = D3D12_FEATURE_DATA_FORMAT_SUPPORT { Format: f, ..Default::default() };
                    // SAFETY: as in `query`.
                    unsafe {
                        device.CheckFeatureSupport(
                            D3D12_FEATURE_FORMAT_SUPPORT,
                            (&raw mut s).cast(),
                            size_of_val(&s) as u32,
                        )
                    }
                    .ok()
                    .map(|()| (f.0 as u32, s.Support1.0 as u32))
                })
                .collect(),
            workarounds: Vec::new(),
        };
        caps.apply_workarounds(WORKAROUNDS);
        tracing::info!("host caps (generation {}): {}", caps.generation, caps.summary());
        caps
    }
}

/// Who the adapter is and what memory it reports. Failures leave defaults.
fn adapter_info(device: &ID3D12Device) -> (AdapterInfo, MemoryCaps) {
    let mut info = AdapterInfo::default();
    let mut memory = MemoryCaps::default();
    // SAFETY: plain DXGI queries on interfaces this function owns.
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory4>() else {
            return (info, memory);
        };
        let Ok(adapter) = factory.EnumAdapterByLuid::<IDXGIAdapter1>(device.GetAdapterLuid()) else {
            return (info, memory);
        };
        if let Ok(d) = adapter.GetDesc1() {
            let len = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
            info.name = String::from_utf16_lossy(&d.Description[..len]);
            info.vendor_id = d.VendorId;
            info.device_id = d.DeviceId;
            info.software = d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
            memory.dedicated = d.DedicatedVideoMemory as u64;
            memory.shared = d.SharedSystemMemory as u64;
        }
        if let Ok(v) = adapter.CheckInterfaceSupport(&IDXGIDevice::IID) {
            info.driver_version = u64::try_from(v).ok();
        }
        if let Ok(a3) = adapter.cast::<IDXGIAdapter3>() {
            let mut m = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
            if a3.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut m).is_ok() {
                memory.budget = Some(m.Budget);
            }
        }
    }
    (info, memory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;

    /// The probe on WARP (software, no GPU launch): it fills what WARP
    /// answers and the planners accept the result. Skipped where WARP or
    /// D3D12 is missing.
    #[test]
    fn probe_on_warp_fills_caps() {
        // SAFETY: plain DXGI and device creation.
        let device = unsafe {
            let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory4>() else { eprintln!("skip: no DXGI"); return };
            let Ok(warp) = factory.EnumWarpAdapter::<IDXGIAdapter>() else { eprintln!("skip: no WARP"); return };
            let mut device: Option<ID3D12Device> = None;
            if D3D12CreateDevice(&warp, D3D_FEATURE_LEVEL_12_0, &mut device).is_err() {
                return;
            }
            device
        };
        let Some(device) = device else { return };
        let caps = Caps::probe(&device, &[windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM]);
        assert!(caps.generation > 0 && caps.wave.is_some() && caps.binding_tier.is_some());
        assert!(caps.formats.contains_key(&28));
    }
}
