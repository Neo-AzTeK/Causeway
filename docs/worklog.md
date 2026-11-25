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
3. Program `a1f6cdf8874960a9` (compute, loops; smoke 11 skipped it: no removal,
   slow submissions 4.5 s -> 250-285 ms): find why it runs so long on the host
   and fix it, then stop skipping it. Not the barrier (it has none; loops are
   uniform, see log). Next: one launch with `VICTORY_SHADER_LOOP_LIMIT=64` (loop
   count), and its end-of-program aggregated atomic per half. One error, one
   session.
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
- 2026-11-25: finding (offline, local decoded dump, no launch, no code change):
  `a1f6cdf8874960a9` has two do-while loops (`s_add` counter, `s_cmp_eq` against
  a count, exit on `scc0`), each guarded by a count-is-zero skip. Count 1 is
  one dword of a constant buffer (`s_buffer_load` at user-SGPR base + 4); the
  loop body reads one 16-byte vector per step from the same buffer, count 2
  likewise (6 vectors per group of 5). A constant buffer holds at most 4096
  such vectors, so a legitimate count cannot run for seconds; a count that
  hits the 2^20 limit is garbage (stale or unwritten data, or a view larger
  than the guest's buffer), not a large real loop. Not shown: which buffer, and
  which word. One run needed: log, at the first slow submission of this program,
  the constant-buffer descriptor (base, size) and the dword at base + s7 + 4,
  with `VICTORY_SHADER_LOOP_LIMIT=64` for safety. No adapter verified.
- 2026-11-25: smoke 12 (69b004e, RTX 2060 SUPER wave32, 300 s, no skip,
  `VICTORY_SHADER_LOOP_LIMIT=64`, owner-approved single run): no hang, no
  removal; 7 slow submissions of 251-271 ms, none naming `a1f6cdf8874960a9`
  (23 log lines mention it, so it ran). 0.6 fps, 1088 drawn/frame, lost
  per frame: refused 346, compiling 760, other 2494; 0.0% non-flat. So the
  hang is a loop trip count, as suspected; not shown whether the count is
  garbage on the host (a wrong constant-buffer read) or legitimately large.
  Next: run it at the default limit with the count of its loops logged, or
  find the count's source. The limit also changes every other loop: this run
  proves the cause, not a fix.
- 2026-11-25: halves gate (`emit/halves.rs`, branch claude/kind-chebyshev-d936c6,
  offline, no launch): two-halves only if no `s_barrier` can be skipped or
  repeated by a branch on a lane-derived scalar (compare masks, EXEC, scalar
  ALU on them; lane reads are exchanged by the lowering), and no scalar load
  address or non-bitwise EXEC write comes from one. Synthetic tests: lane exit
  + barrier refused; uniform exit, no barrier, barrier after the loop accepted.
  Census (local files): 223 compute programs still need 64 lanes, as before
  the gate, so all 56 enabled programs survive it; 931 wave64 programs run on
  waves of 32 (two halves or lanes that fit; the counter cannot tell them
  apart). Finding: `a1f6cdf8874960a9` has no `s_barrier` and no LDS; its two
  backward branches test `s_cmp` of scalars loaded from a constant buffer
  (uniform), so the barrier hypothesis is refuted for it and the gate accepts
  it. Not shown: why it runs long. Candidates: loop count read as 0 or garbage
  (2^20 per-lane limit), and a wave-aggregated atomic at its end (`s_bcnt1`,
  `s_ff1` on EXEC, `v_readfirstlane`, `v_cmpx` on a lane index) that picks a
  leader per half. Keep skipping it. No adapter verified (no launch).
- 2026-11-25: smoke 11 (main 4f4befe, skip a1f6cdf8874960a9, 300 s): no removal,
  0.7 fps (prep 609, GPU 239, copy-back 137, other 483 ms), 1376 drawn,
  dropped 299 refused + 130 compiling + 461 other; still no visible 3D (0.0%
  non-flat). One run, so the program is the likely cause, not proven.
- 2026-11-25: frame-wait hunt (from smoke-out10 log, no run): the 2.4 s "other"
  median was a digest artifact. Before the removal (12:36:32) world lines show
  GPU 4-27 s/frame (4.6 s at 546 draws, 0.76 s at 5114 draws, preparing 0.4-0.5 s,
  copies 266 MB/frame): the wait is GPU time, i.e. the load that ended in the
  removal. After it, lines read P0 G0 C0 at 1.3-4.2 s/frame with 1500-4000
  draws/frame: the host records without a GPU, cause not named (log spam of
  ~1000 lines/s is a suspect). The digest now leaves post-removal lines out of
  world lines. Pipeline wait cannot be it: capped at 500 ms/frame.
- 2026-11-25: finding (offline, smoke 10 log): the removal's breadcrumb list
  stopped at command 14 of 129, a 141x1x1 dispatch of compute `a1f6cdf8874960a9`
  (commands 2..22 are all that program, 2..569 groups). 24 of 26 "slow host
  submission" lines (4.5 s each) and smoke 9's only one name it: a hang by a
  long/looping kernel, not a one-off. Journal text spans several log lines
  (grep the next lines, not `-m1` alone). Single run: `VICTORY_SKIP_PIPELINES=a1f6cdf8874960a9`
  must give no slow submission and no removal; `VICTORY_DUMP_HASHES` for its loop.
- 2026-11-25: finding (smoke 10 log, no launch): indirect kind 2 "no host
  pipeline" = compute programs refused for wave size (needs 64, host has 32;
  e.g. one program 6x/frame): same as Next #4, not a compile race. Kind 10
  "prepare failed (mesh)" has an empty reason: `root_view` returned None
  silently; trace shows its root view's bound heap as "null" (no host heap),
  likely `heap.host()?`: descriptor side (other AI). `root_view` now names
  its reasons (diagnostics); confirm on next run.
- 2026-11-25: smoke 10 reached the world (0.4 fps, one removal survived).
- 2026-11-25: merged check-health, run-digest, device-removal, wave64-refusals,
  caps wave plan, caps memory plan; added `VICTORY_SURVIVE_REMOVAL`.
