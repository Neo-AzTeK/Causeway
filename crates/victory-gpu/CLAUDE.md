# victory-gpu: D3D12.X objects and GPU memory to host D3D12

| Topic | File in `src/` |
|-------|------|
| object header, function tables | `object.rs` |
| device, adapter, console model | `device.rs`, `dxgi.rs`, `console.rs` |
| host device, submission, per-list host state | `host.rs` |
| queues, fences, frame pacing | `queue.rs`, `fence.rs`, `frame.rs` |
| command lists, draw state, indirect | `list.rs`, `draw.rs`, `indirect.rs`, `signature.rs` |
| pipelines (made by workers, kept on disk) | `pipeline.rs`, `pipeline/` |
| translated-shader cache | `shader_cache.rs` (store: `victory-pipeline`) |
| descriptors, samplers, root signatures | `descriptor.rs`, `descriptor/host.rs`, `sampler.rs`, `root.rs` |
| heaps, resources | `resource.rs` |
| host copies of buffers, textures | `shadow.rs`, `texture.rs`; title writes: `watch.rs` |
| primitive types the host lacks, queries | `primitive.rs`, `query.rs` |
| debug captures, copy statistics | `capture.rs`, `copystat.rs` |

- A resource's GPU address is its CPU address. Title threads write that memory
  while the GPU runs: `Shadow::copy_back` must write back only the bytes
  shaders changed.
- `host.rs` waits for every submission today. A change to that must keep the
  order: copy-back first, then bottom-of-pipe writes.
- Layouts the title's inlined code reads are fixed: never reorder their fields
  or table slots.
- Debug code sits behind `feature = "diagnostics"`: also run clippy with
  `--all-features`. Its variables are in `docs/debugging.md`.

## Writing tests here

- Memory handed to `shadow`/`watch` (buffer view, `for_view`, `find`,
  `register`) is never a `Vec` or stack array: `shadow::loose` write-protects
  the 4 MiB window around it, and over the host heap that hangs (about every
  second run). Use `watch::tests::pool` or `watch::tests::owned_window`.
- A hanging test: run the binary directly, `timeout 60 <exe> --test-threads=1
  <filters>`; the last `test name ...` line names the stuck one.
- No wall-clock bounds near a few milliseconds and no "consecutive" assertions
  on scheduled events (loaded hosts wake threads 15-30 ms late). Assert order
  and "not the time-out"; check a timing test under load (24 busy loops, 80
  runs) before merging.
- A test that needs the host GPU returns when `host()` is `None` and prints
  `skipped: no host GPU`. Capability paths use fake caps (`caps::reference`),
  never the real adapter.
