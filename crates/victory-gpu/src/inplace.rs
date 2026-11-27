//! Title memory the GPU uses where it is, without a host copy: a probe of
//! what the host device allows (`ID3D12Device3::OpenExistingHeapFromAddress`
//! and the sized `ID3D12Device13` form), for the plan in `docs/problems.md`
//! ("Ideas from xcl"). Host copies of buffers live in system memory
//! (`crate::shadow`), so the GPU reads such memory at the same cost as
//! today; what goes away is the refresh, the snapshot and the copy-back.

#[cfg(test)]
mod tests {
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::core::Interface;
    use windows_sys::Win32::System::Memory::{
        GetWriteWatch, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, ResetWriteWatch, VirtualAlloc,
        VirtualFree,
    };

    const MIB: usize = 1 << 20;
    const MEM_WRITE_WATCH: u32 = 0x20_0000;

    fn buffer_over(device: &ID3D12Device, heap: &ID3D12Heap, len: usize) -> windows::core::Result<ID3D12Resource> {
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: len as u64,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER,
            ..Default::default()
        };
        let mut made: Option<ID3D12Resource> = None;
        // SAFETY: a buffer placed at the start of our heap, as large as it.
        unsafe { device.CreatePlacedResource(heap, 0, &desc, D3D12_RESOURCE_STATE_COMMON, None, &mut made) }?;
        Ok(made.expect("made"))
    }

    /// A write-watched, wholly committed allocation (as the runtime's GPU
    /// pools are made) opened as a heap: the GPU copies one half into the
    /// other, and the CPU reads the result in the same memory. The CPU's own
    /// writes stay watched.
    #[test]
    fn title_style_memory_is_used_by_the_gpu_in_place() {
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let len = 2 * MIB;
        // SAFETY: a fresh allocation of our own, released below.
        let at = unsafe { VirtualAlloc(std::ptr::null(), len, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE) };
        assert!(!at.is_null());
        let words = at.cast::<u32>();
        for i in 0..(64 << 10) / 4 {
            // SAFETY: inside the allocation.
            unsafe { words.add(i).write(0xC0DE_0000 | i as u32) };
        }
        let device3: ID3D12Device3 = host.device.cast().expect("ID3D12Device3");
        // SAFETY: the start of a committed allocation that outlives the heap.
        let heap: windows::core::Result<ID3D12Heap> = unsafe { device3.OpenExistingHeapFromAddress(at) };
        let heap = match heap {
            Ok(heap) => heap,
            Err(e) => panic!("the device does not open title-style memory: {e}"),
        };
        // SAFETY: plain query of our heap.
        let desc = unsafe { heap.GetDesc() };
        eprintln!(
            "opened {} bytes: heap type {:?}, page {:?}, pool {:?}, flags {:?}",
            desc.SizeInBytes,
            desc.Properties.Type,
            desc.Properties.CPUPageProperty,
            desc.Properties.MemoryPoolPreference,
            desc.Flags
        );
        assert_eq!(desc.SizeInBytes as usize, len);
        let buffer = buffer_over(&host.device, &heap, len).expect("a buffer placed over it");
        // What opening the heap marked written (the driver pins the pages)
        // is cleared, then the CPU writes one page again.
        let watched = |at: *mut core::ffi::c_void| {
            let mut pages = [std::ptr::null_mut::<core::ffi::c_void>(); 512];
            let (mut count, mut granularity) = (pages.len(), 0u32);
            // SAFETY: room for `count` addresses; no reset.
            let r = unsafe { GetWriteWatch(0, at, len, pages.as_mut_ptr(), &mut count, &mut granularity) };
            assert_eq!(r, 0);
            pages[..count].iter().map(|&p| (p as usize - at as usize) / 4096).collect::<Vec<usize>>()
        };
        eprintln!("write watch after opening: {} pages", watched(at).len());
        // SAFETY: our watched allocation.
        assert_eq!(unsafe { ResetWriteWatch(at, len) }, 0);
        // SAFETY: inside the allocation.
        unsafe { words.add(5).write(0xC0DE_0005) };
        let list = host.new_list().unwrap();
        // SAFETY: an open list; both ranges inside the buffer, apart.
        unsafe { list.list.CopyBufferRegion(&buffer, MIB as u64, &buffer, 0, 64 << 10) };
        list.close();
        host.execute(&[&list]);
        let half = MIB / 4;
        for i in [0usize, 1, 4095, (64 << 10) / 4 - 1] {
            // SAFETY: inside the allocation; the GPU finished (execute waits).
            let got = unsafe { words.add(half + i).read() };
            assert_eq!(got, 0xC0DE_0000 | i as u32, "dword {i} of the GPU's copy");
        }
        // The CPU's write after the reset is seen; whether the GPU's are is
        // only reported.
        let seen = watched(at);
        assert!(seen.contains(&0), "{seen:?}");
        let gpu_seen = seen.iter().filter(|&&p| p >= MIB / 4096).count();
        eprintln!("write watch after the copy: {} pages, {gpu_seen} in the GPU's 16", seen.len());
        drop(buffer);
        drop(heap);
        // SAFETY: ours; nothing uses it now.
        unsafe { VirtualFree(at, 0, MEM_RELEASE) };
    }

    /// Part of an allocation: the plain form refuses an address that is not
    /// an allocation's start (xcl's note); the sized form, where the device
    /// has it, is asked for the second megabyte only.
    #[test]
    fn part_of_an_allocation_is_opened_or_refused() {
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let len = 4 * MIB;
        // SAFETY: a fresh allocation of our own, released below.
        let at = unsafe { VirtualAlloc(std::ptr::null(), len, MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH, PAGE_READWRITE) };
        assert!(!at.is_null());
        let inner = at.wrapping_byte_add(MIB);
        let device3: ID3D12Device3 = host.device.cast().expect("ID3D12Device3");
        // SAFETY: inside a committed allocation that outlives the heap.
        let plain: windows::core::Result<ID3D12Heap> = unsafe { device3.OpenExistingHeapFromAddress(inner) };
        // SAFETY: plain query.
        let plain_size = plain.as_ref().map(|h| unsafe { h.GetDesc() }.SizeInBytes);
        eprintln!("plain form at +1 MiB: {plain_size:?}");
        match host.device.cast::<ID3D12Device13>() {
            Ok(device13) => {
                // SAFETY: as above, one megabyte inside the allocation.
                let sized: windows::core::Result<ID3D12Heap> = unsafe { device13.OpenExistingHeapFromAddress1(inner, MIB) };
                // SAFETY: plain query.
                let size = sized.as_ref().map(|h| unsafe { h.GetDesc() }.SizeInBytes);
                eprintln!("sized form, 1 MiB at +1 MiB: {size:?}");
            }
            Err(e) => eprintln!("no ID3D12Device13: {e}"),
        }
        drop(plain);
        // SAFETY: ours; nothing uses it now.
        unsafe { VirtualFree(at, 0, MEM_RELEASE) };
    }

    /// Physical pages as the runtime makes them (`victory_hle` xmem): a
    /// pagefile section reserved, its view put in a placeholder, its pages
    /// committed on use. Whether the device opens such a view decides if
    /// most of the world's copies could go (they copy views like this).
    #[test]
    fn a_physical_page_view_is_opened_or_refused() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER, MEM_RESERVE_PLACEHOLDER,
            MapViewOfFile3, SEC_RESERVE, UnmapViewOfFile2, VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let Some(host) = crate::host::host() else {
            eprintln!("skipped: no host GPU");
            return;
        };
        let len = 2 * MIB;
        // SAFETY: plain section creation, closed below.
        let section =
            unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, std::ptr::null(), PAGE_READWRITE | SEC_RESERVE, 0, len as u32, std::ptr::null()) };
        assert!(!section.is_null());
        // SAFETY: a placeholder of our own, replaced by the view.
        let place = unsafe {
            VirtualAlloc2(GetCurrentProcess(), std::ptr::null(), len, MEM_RESERVE | MEM_RESERVE_PLACEHOLDER, 0x01, std::ptr::null_mut(), 0)
        };
        assert!(!place.is_null());
        // SAFETY: the whole placeholder replaced by the whole section.
        let view = unsafe {
            MapViewOfFile3(section, GetCurrentProcess(), place, 0, len, MEM_REPLACE_PLACEHOLDER, PAGE_READWRITE, std::ptr::null_mut(), 0)
        };
        assert_eq!(view.Value, place);
        // SAFETY: commits the view's pages, as a first use does.
        assert!(!unsafe { VirtualAlloc(place, len, MEM_COMMIT, PAGE_READWRITE) }.is_null());
        // SAFETY: inside the committed view.
        unsafe { place.cast::<u32>().write(0xC0DE_0001) };
        let device3: ID3D12Device3 = host.device.cast().expect("ID3D12Device3");
        // SAFETY: the view outlives the heaps.
        let plain: windows::core::Result<ID3D12Heap> = unsafe { device3.OpenExistingHeapFromAddress(place) };
        eprintln!("plain form on a section view: {:?}", plain.as_ref().map(|_| ()).map_err(|e| e.code()));
        let sized = host.device.cast::<ID3D12Device13>().ok().map(|d13| {
            // SAFETY: as above.
            let heap: windows::core::Result<ID3D12Heap> = unsafe { d13.OpenExistingHeapFromAddress1(place, len) };
            heap
        });
        eprintln!(
            "sized form on a section view: {:?}",
            sized.as_ref().map(|r| r.as_ref().map(|_| ()).map_err(|e| e.code()))
        );
        // A heap that opened is used: the GPU copies dword 0 to 1 MiB.
        if let Some(heap) = plain.ok().or_else(|| sized.and_then(Result::ok)) {
            let buffer = buffer_over(&host.device, &heap, len).expect("a buffer placed over it");
            let list = host.new_list().unwrap();
            // SAFETY: an open list; both ranges inside the buffer.
            unsafe { list.list.CopyBufferRegion(&buffer, MIB as u64, &buffer, 0, 4) };
            list.close();
            host.execute(&[&list]);
            // SAFETY: inside the view; the GPU finished.
            let got = unsafe { place.cast::<u32>().wrapping_byte_add(MIB).read() };
            assert_eq!(got, 0xC0DE_0001, "the GPU's copy in the view");
            eprintln!("the GPU used the section view in place");
        }
        // SAFETY: ours, unused now.
        unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), view, MEM_PRESERVE_PLACEHOLDER);
            VirtualFree(place, 0, MEM_RELEASE);
            CloseHandle(section);
        }
    }
}
