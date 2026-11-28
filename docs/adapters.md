# Adapters: what ran where

The record of which GPUs have really run this code. How a fix is made to hold
on the others (`Caps`, the planners, the fake classes, vendor workarounds) is
`docs/hardware-range.md`; the review questions are in
`docs/gpu-review-checklist.md`. No game data here.

A pass on one adapter says nothing about another. A fake class or the shader
validator is not an adapter.

## Real adapters

| Adapter | Wave | Mesh shaders | Binding tier | View heap | Title last run | Tests last run |
|---------|------|--------------|--------------|-----------|----------------|----------------|
| NVIDIA RTX 2060 SUPER (Turing, 8 GB), driver 32.0.16.1088 | 32 only | native (tier 1) | 3 | large (title's slots kept) | 2026-11-27, diagnostics build, not through smoke; result not recorded | `victory-shader` 2026-11-27; `victory-gpu` not run with that day's changes |
| NVIDIA GTX 1070 (Pascal, 8 GB), driver 32.0.15.8278 | 32 only | none: made as compute | 3 | capped at 1,000,000 slots | 2026-11-28 (smoke) | workspace 2026-11-28 |

The RTX 2060 SUPER is the adapter of the current development machine
(`victory.hardware.toml`, ignored by git, lists a machine's own). The driver
version is that of 2026-11-27.

The software adapter (WARP) is not a row: it runs some shader tests
(`VICTORY_WARP=1`) and the caps probe. It has its own wave size and tiers, so
it shows neither vendor behaviour nor speed.

## What this means per path

| Path | Ran for real on | Not shown |
|------|-----------------|-----------|
| native mesh shaders | RTX 2060 SUPER | any other vendor or generation |
| mesh programs made as compute | GTX 1070, while the path was still being built | any other card without mesh shaders |
| large view heap | RTX 2060 SUPER | whether other drivers accept it |
| capped view heap and slot pool | GTX 1070 | other small-heap drivers |
| console wave of 64 as two waves of 32 | RTX 2060 SUPER and GTX 1070 (shader tests) | a host with real waves of 64 |
| programs that require a wave of 64 | refused on both | any host that can run them |
| 64-bit atomics on heap resources | GTX 1070 reports them | inferred for the others: no caps dump of the RTX 2060 SUPER is kept |

## Newer changes, and where they have run

Drop a row once the title has run with it on a real adapter and the result is
written down.

| Change | Checked by | Not yet shown |
|--------|------------|---------------|
| root constants read whatever their dword number | a full 6-minute title run, 2026-11-27, RTX 2060 SUPER: no device removal; a second run of the same code removed the device about 3 minutes in (cause not read, `gpu-long-dispatch` not ruled out) | effect on the picture; other adapters; whether the removal comes back |
| loop limits by shader stage | tests; part of the 2026-11-27 title run | the run's result; whether a real loop is cut short |
| "UAVs at every stage" shader flag | tests, validator; part of that run | effect in the title |
| compute shaders with implicit derivatives ask for validator 1.8 | tests, validator; part of that run | effect in the title; whether an older Windows D3D12 runtime accepts such a module |
| lane mask joined with data keeps its number | tests, one of them on the GPU (RTX 2060 SUPER); validator; part of the same full title run | effect in the title |
| first-lane reads of values alike in a console wave take each host wave's own lane | tests, three of them on the GPU (GTX 1070); validator and census (3 programs gained, none lost); a 300 s title run on the GTX 1070, 2026-11-28 (`wave64-gate-2`): no regression | effect in the title: the 3 programs gained were not refused in that run's scene on `main` either; the RTX 2060 SUPER |

The validator in these rows is the Windows SDK's `dxil.dll` (10.0.26100.0).

## Fake classes only

`caps::reference` has fakes for NVIDIA Maxwell (both generations), Pascal,
Turing, Ampere, Ada and Blackwell, AMD GCN and RDNA, Intel Arc and integrated,
and WARP. Each entry of `reference::PROFILES` says where its figures come from;
many are documentation and inference. What each NVIDIA path does:
`docs/hardware-range.md`, "NVIDIA: each path the GTX 1070 takes".
First-generation Maxwell at binding tier 2 cannot run translated shaders: it is
refused, and that refusal has run only against fake caps.

## Never run (assume nothing)

- AMD, any generation, wave64 or a 32-to-64 range
- Intel (reported wave ranges can be wrong)
- NVIDIA other than the two cards above
- binding tier 1 or 2
- shared-memory (UMA) devices and cards under 8 GB

## Updating

Update this page in the same diff as a GPU-facing change, and after a title
run on a new adapter or driver:

1. Give each real adapter's last title run and last tests, with the date.
   "Result not recorded" is better than an assumed pass.
2. Add a change to "Newer changes" with what has **not** been shown.
3. A new adapter: add its row, and add its caps to `caps::reference` as
   `docs/hardware-range.md` describes ("Testing a path without the hardware").
