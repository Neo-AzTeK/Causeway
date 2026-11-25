# Hardware range: how a fix holds on other GPUs

Rule (CLAUDE.md): a fix must hold on other GPUs, vendors and drivers, not only
the primary dev GPU (NVIDIA, wave32 only; `docs/adapters.md`). This page is how. The code
is `crates/victory-gpu/src/caps.rs` (`Caps`, the planners, `reference`).

## The mechanism in four lines

1. `Caps::probe(&device, &formats)` asks the device once, when it is made.
   Every query may fail; a failed field is `None` and means "not supported".
2. A planner is a pure function `&Caps -> decision` (`wave_plan`,
   `binding_plan`, `memory_plan`, `min_lod_clamp`, `mesh_shaders`,
   `atomics64_heap`, `aniso_point_mip`, `format_choice`). It never sees vendor
   or driver.
3. A planner that cannot serve the device returns a `Refusal`, which prints as
   `the host GPU lacks <feature>: needs <x>, has <y>`.
4. Tests run every planner over the fake caps in `caps::reference` and over
   the wave/tier/failure grid in `caps/tests.rs`. No GPU.

## What to query, and where it lives

| Need | `Caps` field | D3D12 query | Planner |
|------|--------------|-------------|---------|
| lane counts of a wave | `wave` | OPTIONS1 `WaveLaneCountMin/Max` | `wave_plan` |
| wave pinning, mesh shaders | `shader_model` | `SHADER_MODEL`, asked 6.8 down | `wave_plan`, `mesh_shaders` |
| descriptor limits | `binding_tier` | OPTIONS `ResourceBindingTier` | `binding_plan` |
| least-level clamp | `tiled_tier` | OPTIONS `TiledResourcesTier` | `min_lod_clamp` |
| mesh shaders | `mesh_tier` | OPTIONS7 | `mesh_shaders` |
| 64-bit image atomics | `typed_atomics64`, `heap_atomics64` | OPTIONS9, OPTIONS11 | `atomics64_heap` |
| aniso with point mips | `aniso_point_mip` | OPTIONS19 | `aniso_point_mip` |
| memory | `memory`, `uma` | `GetDesc1`, `QueryVideoMemoryInfo`, ARCHITECTURE1 | `memory_plan` |
| format use | `formats` (probed list) | `FORMAT_SUPPORT` | `format_choice` |

Descriptor and heap code belongs to another owner: it reads `binding_plan`
(class, `max_views`, `max_samplers`, per-stage limits at tier 1) and nothing
else from here. Limits are the specification's per tier; a driver may allow
more. Where a tier sets no bound (`max_views: None`), try the size wanted and
treat a creation failure as the refusal.

Wave caveats: `wave` is `None` when the device reports no wave ops; a pin (`[WaveSize]`) exists only in compute, mesh and amplification stages (`WaveNeed.pinnable`), so a wave64 pixel shader on a 32-to-64 host is a refusal, not a pin. Reported ranges can be wrong (Intel): the answer is a `WORKAROUNDS` entry, not a branch.

Not in `Caps`: figures that change while running (video memory in use; `memory.budget` is a creation-time hint). The
budget policy samples those itself; `memory_plan` gives only the starting
budget.

## Adding a capability

1. Field in `Caps` (an `Option` if the query can fail), query in `probe.rs`.
2. A planner in `caps/planner.rs` that follows its three rules: unknown is
   unsupported; compare tiers with `>=`; a refusal carries need and have.
3. Fake caps for each side of the decision in the matrix; extend the grid in
   `caps/tests.rs` with a rule-written oracle (not a copy of the planner).
4. The call site takes the plan from the `Caps` held by the host, never
   queries the device itself.

## Testing a path without the hardware

- Fallback path: build caps (`reference::amd_gcn()`, or
  `Caps { wave: .., ..Caps::unknown() }`) and assert the plan. Run it through
  the code that acts on the plan where that code takes plain values (the shader
  translator takes `emit::Options`, not a device).
- Query failure: `Caps::unknown()` is every query failing.
- Real API on a second device: WARP (`probe_on_warp_fills_caps`). WARP has the
  API but not vendor behaviour (waves of 4, its own tiers); it can pass what
  hardware fails and the reverse. It shows the probe works, not that a plan is
  right.
- A new device you own: log line `host caps (generation N): ...` at start
  shows the summary; paste its `{:#?}` into `reference.rs` as a new class.

## The reference adapter set

`reference::all()`: `nvidia_modern` (wave32, the owner's class),
`nvidia_pascal`, `amd_rdna` (waves 32 to 64), `amd_gcn` (wave64 only),
`intel_arc` (8 to 32), `intel_igpu` (UMA, 128 MiB "dedicated"), `warp`.
These are class profiles written from public documentation, not dumps; the
code says so. A change is verified on a class only through its fake or WARP.
Every brief and report says which adapters ran for real, which only as fakes.

## Vendor and driver

They are data for the log. A planner never branches on them, and a test
(`plans_do_not_depend_on_vendor_or_driver`) fails if one does. The one door is
`WORKAROUNDS`: an entry names the bug (with a link or capture), a vendor, and
a driver range with both ends, and `fix` corrects the caps the driver
reported; the planners then see the corrected caps. The table is empty today.
An entry without a bug or a bounded range fails a test. Add one only for a
bug seen on a device, not for a rumour.

Drivers also misreport. A caps bit is a hint: pipeline, heap and resource
creation can still fail, and that failure must end as a clean refusal too
(Dawn, for one, treats Intel's reported minimum wave size as unreliable).

## Device loss

`Caps` is immutable and belongs to one device. When the device is made again
(device removal, a different adapter), probe again, take new plans, and drop
anything derived from the old caps (pipelines, heaps sized from limits).
`Caps::generation` rises with each probe so a holder can tell. Do not keep a
planner result in a `static`/`OnceLock`; that is stale after the first loss.

## Review checklist

Answer each in the commit message or the review:

- Which capability does this assume (wave size, tier, shader model, format,
  memory size, UMA)? Is it a `Caps` field, or a constant?
- What does the code do when it is `None`, and when it is worse than the
  owner's card (wave64 only, tier 1, 2 GiB, no mesh shaders)?
- Where is the fallback or refusal tested, and does the test fail without it?
- Is the refusal text of the form `lacks <feature>: needs <x>, has <y>`?
- Which adapters ran for real, which as fakes, which not at all?
- Does anything branch on vendor or driver outside `WORKAROUNDS`?
- Is a result cached across device creation?

## Who may ask the device

Only `caps/`. `cargo xtask policy` (`xtask/src/capability.rs`) rejects a
`CheckFeatureSupport` or `CheckInterfaceSupport` in any other Rust file (the
checker's own, under `xtask/`, aside):

| Kind of question | Where it goes |
|------------------|---------------|
| what the device can do | a `Caps` field and a planner |
| about one resource (a format the title picks while it runs) | a function in `caps/per_resource.rs` |
| the test host (`victory-shader/src/emit/`, below the crate that has `Caps`) | `// test-host-query: why`, one per call |

Part of the test host also runs in the title (`fuses_fmad`, which asks
OPTIONS19 and uses no sampler).

Still open, all in descriptor and heap code (other owner): one `FORMAT_SUPPORT`
query in `descriptor/host.rs` (`typed_store`, which also caches its answers
across devices), counted in `OTHER_OWNER` so that no other is added; heap sizing
by `binding_plan`. `format_choice` and `Caps::formats` have no caller. The
`Host` keeps one `Caps` and nothing replaces it after a device loss.

## Prior art (ideas only; no code taken)

wgpu keeps one adapter-caps struct with a downlevel tier and `wgpu-info`
dumps it for replay (MIT/Apache-2.0); Dolphin's `DriverDetails` tests a bug
flag from a version-ranged table, never a vendor name (GPL-2.0+); Magnum lists
each workaround by id (MIT); Microsoft's `CheckFeatureSupport` guidance:
zero the structure, treat failure as unsupported, compare tiers with `>=`;
Dawn's notes on Intel wave sizes. A replay format for dumps is not built
here; add it when a second person has a device to dump.
