# Work log: what is done, what is next

Kept up to date by whoever merges or finishes a task (rule in `CLAUDE.md`).
Newest state first inside each section. No game data here (rule #1): counts,
error kinds, branches and commits only.

## State (update at every merge)
- 2026-11-25 (GTX 1070, Pascal, wave32, no mesh shaders, 1,000,000-slot view
  heap): main has view slots past a capped host heap taken from the pool
  (2863ff3), host textures resident only once the GPU may reach them
  (32f83df), the typed-store check for unordered views in no format, plain
  committed memory never guarded inside a tracked range. On that GPU the title reaches
  the world at ~1 fps; world pipelines with mesh shaders are refused there.
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
00. Texture refresh at ~3 MB/s: fixed on branch claude/frosty-satoshi-7332c1
   (not merged yet, see log): menus hold 30 fps.
0. (frame-wait hunt, see log) Real cost is GPU time before the removal
   (4.6 s/frame) and 266 MB/frame of copies; item 1 below was mis-attributed.
1. Frame time: 2.4 s per world frame with GPU/prep near 0: find the wait
   (present, pacing, fences). Biggest lever on everything else.
2. Dropped draws per frame: indirect kind 10 "prepare failed (mesh)" 468,
   indirect kind 2 "no host pipeline" 358, still compiling 230, "title memory
   without a view heap or page table" 168.
3. Program `a1f6cdf8874960a9` (compute, loops; smoke 11 skipped it: no removal,
   slow submissions 4.5 s -> 250-285 ms): find why it runs so long on the host
   (loop bound, s_barrier lane-uniformity, wave32 lowering) and fix it, then
   stop skipping it. One error, one session.
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
- Whether to emulate mesh shaders as compute for GPUs without them (GTX 1070
  class); design written, nothing built.
- Pass the other AI the review of 2863ff3, 32f83df, 433ecf7.
- Whether a device removal should recreate the device (device lifetime).

## Parked
Switch-trace bug (fallback and wait switches left no trace), preparing cost,
prewarm, fmt-only commit, `lower.rs` split, journal hot-path cost, whether
the guest `s_barrier` in a loop is lane-uniform.

## Log (newest first; one line per merge or finding)
- 2026-11-25: smoke 12's slow menus: `watch::plain_pages` (dc31df2) called
  `VirtualQuery` on every alias page it guards. The aliases sit in the
  runtime's ~5.5 GiB committed pool view, and `VirtualQuery` walks every page
  to the end of a committed region (~10 us/MiB): 62 ms CPU per call, ~300
  calls/frame, all under the guard lock. Fix: `QueryVirtualMemoryInformation`
  (1-2 us) first; views are skipped whole. GTX 1070, 250 s: textures <= 5.1
  ms/frame (was 2300-5400), protecting 2.2 ms for 396 calls, menus 30 fps
  throughout; world not reached (select screen not pressed). Branch
  claude/frosty-satoshi-7332c1. Other `VirtualQuery` callers on that view
  (`texture::committed`, `copystat`, `watch::writable`) cost the same if hot.
- 2026-11-25: smoke 12 (main be98212, GTX 1070, 300 s): menus 30-36 fps for
  ~40 s, then 0.3-0.4 fps to the end: 2.3-2.9 s/frame in texture preparation
  (`Staging::refresh`, 7.6-9.5 MB/frame read, ~3 MB/s; 5-7 draws, GPU 4 ms).
  At that rate scripted 0.2 s presses are missed: never reached the world (the
  "A Player" select screen needs Enter by hand). 84 refused, 16 untranslated,
  no removal. Proposed its own session (texture-refresh-speed).
- 2026-11-25: merged from the GTX 1070 sessions: fits() checks the texture's
  format for an unordered view in no format (#1754 path; unit test); watch:
  plain committed pages inside a tracked 4 MiB window are not guarded
  (victory-gpu lib tests x20 on GTX 1070: 9 hangs -> 0, one zero-read left in
  draw root_views). Not merged: claude/vigilant-wu-adb5cb (wave-wide
  structuring of one-wave64 groups on wave32) breaks main's
  a_wave64_skip_over_a_lane_read_is_left_out_for_two_waves_of_32 (now needs
  32, test expects 64); back to its session. Adapters verified: GTX 1070
  (tests only, no title run on this merge). One workspace run failed
  bindless::tests::heap_views_are_current (round 0 dword 0 read 0) while
  another session used the GPU; 5/5 lib runs pass on the merge and on main,
  and a second workspace run passed: same open zero-read under load.
- 2026-11-25: finding (GTX 1070, release, 2 runs): world ~1 fps is GPU time;
  two compute programs take about half (GPU 520-980 ms/frame, ~290 ms without
  them); ~200 ms/frame of CPU preparation does not overlap the GPU. Matches
  Next item 0. Residency (32f83df): loading screen GPU 229 -> 3 ms, process
  memory dedicated/shared 6.9/8.6 -> 5.4/4.5 GB; world fps unchanged.
- 2026-11-25: finding (GTX 1070, first title run, 300 s, menus): the title
  asks 1,245,184 view slots; the host heap caps at 1,000,000 (fixed by
  2863ff3). Mesh shader emulation as compute designed in a session
  (`mesh-emulation-design.md`, not in the repo), not implemented: open decision.
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
