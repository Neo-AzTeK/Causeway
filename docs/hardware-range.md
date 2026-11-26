# Hardware range: how a fix holds on other GPUs

Rule (CLAUDE.md): a fix must hold on other GPUs, vendors and drivers, not only
the primary dev GPU (NVIDIA, wave32 only; `docs/adapters.md`). This page is how. The code
is `crates/victory-gpu/src/caps.rs` (`Caps`, the planners, `reference`).

## The mechanism in four lines

1. `Caps::probe(&device, &formats)` asks the device once, when it is made.
   Every query may fail; a failed field is `None` and means "not supported".
2. A planner is a pure function `&Caps -> decision` (`wave_plan`,
   `binding_plan`, `memory_plan`, `min_lod_clamp`, `mesh_shaders`,
   `wave64_native`, `atomics64_heap`, `aniso_point_mip`, `format_choice`). It never sees vendor
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
| waves of 64 or two of 32 | `wave` | OPTIONS1 | `wave64_native` |
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

`reference::all()`: the NVIDIA generations (`nvidia_maxwell`, `nvidia_pascal`
= the owner's GTX 1070, `nvidia_turing`, `nvidia_ampere`, `nvidia_ada`,
`nvidia_blackwell`; `nvidia_modern` is the class name older tests use), `amd_rdna` (waves 32 to 64), `amd_gcn` (wave64 only),
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

## NVIDIA: each path the GTX 1070 takes, and the other side

The owner's card is Pascal (`nvidia_pascal`): wave32, no mesh shaders. Each
row is a decision made from `Caps` (or its creation failure, where marked);
the generations differ only in the fields shown, and no code branches on
vendor. Tests: `caps/tests.rs` unless a path is given.

| Decision | GTX 1070 path | Modern NVIDIA path (Turing and on) | Covered by |
|----------|---------------|------------------------------------|------------|
| mesh shaders (`mesh_shaders`: tier >= 1 and SM 6.6) | tier 0: mesh programs are made as compute and drawn indexed | native mesh pipeline | `nvidia_generations_take_the_paths_their_feature_level_gives`, `a_gtx_1070_and_a_modern_card_differ_only_in_what_the_caps_say`, `shader_cache::mesh_as_compute_is_part_of_the_key` |
| console wave of 64 (`wave64_native`) | two waves of 32 stand in for one (`[WaveSize(32)]`) | same: every NVIDIA part has waves of 32 only | `waves_of_64_are_asked_for_only_where_waves_of_32_do_not_exist`, `victory-shader` lane test `a_host_without_waves_of_32_is_asked_for_waves_of_64`, `shader_cache::wave64_native_is_part_of_the_key` |
| wave plan of a program that asks for 32 or 64 (`wave_plan`) | 32 native; 64 refused | same | `wave_plan_serves_each_reference_class`, `nvidia_generations_take_the_paths_...`, `pipeline::host` `wave_sizes_the_host_lacks_are_refused` |
| least-level clamp (`min_lod_clamp`) | tiled tier 3: native | same | `tiers_are_compared_with_at_least` |
| 64-bit atomics on heap resources (`atomics64_heap`) | off: such pipelines are refused | on from Ampere (kept off for Turing: no report seen) | `heap_atomics_need_both_caps`, `nvidia_generations_take_the_paths_...` |
| binding model (`binding_plan`) | tier 3, bindless | same | `tiers_are_compared_with_at_least`, `view_heap_limits_are_the_specifications_on_every_tier` |
| view heap size (creation failure, `PORTABLE_VIEWS`) | the host refuses a heap past 1,000,000 views: capped heap with a pool for the title's slots | a driver that accepts the larger heap keeps the title's slots (RTX 2060 SUPER, 2026-11-25) | `descriptor::host::tests::title_slots_past_a_capped_heap_take_pool_slots` (forced limit, needs a GPU); the large path runs wherever the heap is accepted (not on the 1070) |
| view copies on 64 KiB heap blocks (`shadow::placed_buffer`) | `ALLOW_ONLY_BUFFERS` heap, valid on resource heap tiers 1 and 2 | same: no tier is read | `shadow::tests` (refused heap, refused placement: copies take a buffer of their own) |
| memory budget (`memory_plan`) | 8 GiB card: the OS's figure | the card's own figure | `nvidia_memory_budgets_follow_the_card_not_a_constant`, `memory_budget_is_chosen_from_the_figures_it_has` |
| formats a host cannot store to or render | asked per resource (`per_resource`) | same query | no fake: needs the device |

The `Caps` figures of these generations are class profiles from public
documentation, not dumps. Not shown by any fake, so it needs a real modern
card: the heap limit a driver accepts (differs by driver, not generation), the
64-bit typed atomics of Turing, and the speed of native mesh against emulation.
