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
0. (frame-wait hunt, see log) Real cost is GPU time before the removal
   (4.6 s/frame) and 266 MB/frame of copies; item 1 below was mis-attributed.
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
- 2026-11-25: frame-wait hunt (from smoke-out10 log, no run): the 2.4 s "other"
  median was a digest artifact. Before the removal (12:36:32) world lines show
  GPU 4-27 s/frame (4.6 s at 546 draws, 0.76 s at 5114 draws, preparing 0.4-0.5 s,
  copies 266 MB/frame): the wait is GPU time, i.e. the load that ended in the
  removal. After it, lines read P0 G0 C0 at 1.3-4.2 s/frame with 1500-4000
  draws/frame: the host records without a GPU, cause not named (log spam of
  ~1000 lines/s is a suspect). The digest now leaves post-removal lines out of
  world lines. Pipeline wait cannot be it: capped at 500 ms/frame.
- 2026-11-25: smoke 10 reached the world (0.4 fps, one removal survived).
- 2026-11-25: merged check-health, run-digest, device-removal, wave64-refusals,
  caps wave plan, caps memory plan; added `VICTORY_SURVIVE_REMOVAL`.
