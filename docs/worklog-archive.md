# Work log archive

Older log lines moved from `docs/worklog.md` (newest first). No game data.

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
