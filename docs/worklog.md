# Work log: what is done, what is next

Kept up to date by whoever merges or finishes a task (rule in `CLAUDE.md`).
Newest state first inside each section. No game data here (rule #1): counts,
error kinds, branches and commits only.

## State (update at every merge)
- 2026-11-25: main has Caps (`caps.rs`, `docs/hardware-range.md`), wave plan
  and memory plan read from Caps, test-gate hang fixed (tests own their
  4 MiB window), smoke digest, DRED in smoke, `VICTORY_SURVIVE_REMOVAL=1`
  (set by smoke), wave64 compute run as two waves of 32 where skips allow.
- Last real-GPU smoke (RTX 2060 SUPER, wave32): menus only, device removed
  (0x887A0005, then ended) about 3 minutes in; 64 pipelines refused, 63 for
  wave 64. The world's 3D scene is not reached yet.

## Next, ranked by effect on the picture
1. Reach the world without a removal: log of a survive-removal smoke names the
   submission (DRED, journal). One error, one session.
2. Refused pipelines: 63 need 64 lanes (the skips cannot be left out); lower
   them on wave32 or fall back.
3. Untranslated: descriptor read only as data (pixel, compute), lane mask read
   as data, barycentrics read directly.
4. Descriptor table full ("no free dynamic descriptor slots", "no free pool
   slot"): the other AI owns descriptors and heaps; report to it.
5. Caps wave 2: emit `Options` plumbing for the wave plan; shader-facing caps
   and the cache key from Caps; device lifetime (new device after loss).
6. Unimplemented device methods (two slots seen in smoke 9).

## Open decisions (owner)
- Pass the other AI the review of 2863ff3, 32f83df, 433ecf7.
- Whether a device removal should recreate the device (device lifetime).

## Parked
Switch-trace bug (fallback and wait switches left no trace), preparing cost,
prewarm, fmt-only commit, `lower.rs` split, journal hot-path cost, whether
the guest `s_barrier` in a loop is lane-uniform.

## Log (newest first; one line per merge or finding)
- 2026-11-25: merged check-health, run-digest, device-removal, wave64-refusals,
  caps wave plan, caps memory plan; added `VICTORY_SURVIVE_REMOVAL`.
