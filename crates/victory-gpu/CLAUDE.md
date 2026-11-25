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

- Memory a test hands to `shadow`/`watch` (a buffer view, `for_view`, `find`,
  `register`) is never a `Vec` or a stack array. `shadow::loose` copies the
  whole 4 MiB window around an address and write-protects it; over the host
  heap that faults the allocator into the guard handler on the thread holding
  `GUARDS`, which waits for itself (a hang about every second run, in a later
  test). Use `watch::tests::pool` (write-watched) or
  `watch::tests::owned_window` (alone in its window).
- A test that hangs must not wait for the 15-minute chain limit: run the
  test binary directly with `timeout 60 <exe> --test-threads=1 <filters>`;
  single-threaded libtest prints `test name ...` before each test, so the
  last line names the stuck one. Narrow with two filters (culprit + victim).
- No wall-clock bounds near a tick or a few milliseconds, and no "consecutive"
  assertions on scheduled events: a loaded host (other sessions build and use
  the GPU) wakes threads 15-30 ms late. Assert order and "not the time-out".
  Check a timing test under load before merging: 24 busy loops, 80 runs.
- A test that needs the host GPU returns when `host()` is `None`; say so
  (`eprintln!("skipped: no host GPU")`) in new tests so a silent pass is
  visible with `--nocapture`.
