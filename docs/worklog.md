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

- Smoke 10 (main 162672f, 300 s, survive-removal on): ran to the end, in the
  world: 0.4 fps (2.4 s/frame, mostly waits), 1743 draws/frame, lost per frame:
  refused 377, compiling 230, other 1245. One removal at ~3.5 min (submission
  7982 of 1 list, 40 compute programs, journal in smoke-out10/run/log.txt);
  after it only 56 programs refused as "device suspended". Only 0.4% of
  pixels non-flat in the saved frame: no 3D scene yet.

## Next, ranked by effect on the picture
1. Frame time: 2.4 s per world frame with GPU/prep near 0: find the wait
   (present, pacing, fences). Biggest lever on everything else.
2. Dropped draws per frame: indirect kind 10 "prepare failed (mesh)" 468,
   indirect kind 2 "no host pipeline" 358, still compiling 230, "title memory
   without a view heap or page table" 168.
3. Removal in submission 7982 (40 compute programs in one list, GPU timeout?):
   bisect which program hangs; one error, one session.
4. Refused pipelines: 63 need 64 lanes (the skips cannot be left out); lower
   them on wave32 or fall back.
5. Untranslated: descriptor read only as data (pixel, compute), lane mask read
   as data, barycentrics read directly.
6. Descriptor table full ("no free dynamic descriptor slots", "no free pool
   slot"): the other AI owns descriptors and heaps; report to it.
7. Caps wave 2: emit `Options` plumbing for the wave plan; shader-facing caps
   and the cache key from Caps; device lifetime (new device after loss).
8. Unimplemented device methods (two slots seen in smoke 9).

## Open decisions (owner)
- Pass the other AI the review of 2863ff3, 32f83df, 433ecf7.
- Whether a device removal should recreate the device (device lifetime).

## Parked
Switch-trace bug (fallback and wait switches left no trace), preparing cost,
prewarm, fmt-only commit, `lower.rs` split, journal hot-path cost, whether
the guest `s_barrier` in a loop is lane-uniform.

## Log (newest first; one line per merge or finding)
- 2026-11-25: finding (offline, smoke 10 log): the removal's breadcrumb list
  stopped at command 14 of 129, a 141x1x1 dispatch of compute `a1f6cdf8874960a9`
  (commands 2..22 are all that program, 2..569 groups). 24 of 26 "slow host
  submission" lines (4.5 s each) and smoke 9's only one name it: a hang by a
  long/looping kernel, not a one-off. Journal text spans several log lines
  (grep the next lines, not `-m1` alone). Single run: `VICTORY_SKIP_PIPELINES=a1f6cdf8874960a9`
  must give no slow submission and no removal; `VICTORY_DUMP_HASHES` for its loop.
- 2026-11-25: smoke 10 reached the world (0.4 fps, one removal survived).
- 2026-11-25: merged check-health, run-digest, device-removal, wave64-refusals,
  caps wave plan, caps memory plan; added `VICTORY_SURVIVE_REMOVAL`.
