# Problem list and todo

What to do next, and the ranked problems behind it. **Update it whenever a task
ends:** tick or drop what is done, add what was found, and move what comes next
to the top. Measured figures, run folders and shader hashes stay on the
developer's machine, never here (rule #1).

Goal: **one correct frame** (`docs/agent-workflow.md`).

## Todo, next first

Each item names its problem in the ranked table below.

- [ ] Keep write guards (`watch.rs` `protect`) off mapped views that are
      not the title's memory (`host-bugcheck`). Both dumps have one stack:
      the video memory manager evicting a system-memory allocation locks its
      pages in our process for write, the lock fails, and Windows stops.
      `protect` makes any committed non-private view in a loose copy's 4 MiB
      window read-only, and the D3D12 runtime's mapped allocations are such
      views; our fault handler hides the user-mode writes, the kernel's lock
      is not caught. Not proven: first log any guard on a view the runtime
      did not report; low memory could also fail the lock

- [x] `descriptor-slots` confirmed on `main`: two quiet world runs lose no
      draw for lack of slots. The merge did not raise refusals. The pixel
      programs refused for "a descriptor read only as data" are counted
      under their own reason on every path, before and after the merge, and
      lose the same per frame whenever they are on screen. Runs differ only
      in how long that content stays on screen (same commit: short in one
      run, long in the next). Refused work without them is the same on
      both sides (problem notes, "Confirm: refusal shift")
- [ ] The title's code wrote to memory reserved but never committed and the
      run stopped (0xc0000005 in the title's image, one quiet run of `main`
      before the `descriptor-slots` merge; `image-uncommitted-write`). Find
      which reservation and who should have committed it
- [ ] `victory-shader` test `direct_barycentrics_weigh_the_second_and_third_vertex`
      fails on a host without barycentrics (GTX 1070: making its pipeline is
      refused) instead of skipping; seen on `main` and branches alike (no
      ranked problem: test hygiene, `docs/hardware-range.md`)

- [ ] Find the pass that should put lit people and ground into the scene's
      colour before tonemap (`scene-blocker`). Traced on the newest build:
      most surfaces have no base colour in the main pass, the earliest wrong
      step, as before (empty texture slots, the descriptor side). Surfaces
      that do have colour stay near black in the scene's colour before
      tonemap, while one lighting pass draws them lit: on near geometry that
      pass lights, the scene's colour is hundreds of times darker, almost
      everywhere. Tonemap and upscale keep the picture they get, so this
      second loss is in lighting or composition. Measured in one frame of
      one shot; not pinned to one pass, no causal test yet
- [ ] Owner decides how the original project's later updates come in: one
      commit at a time, or squashed; cleaned either way
- [ ] Game run that includes the newest shader fix (lane mask joined with data)
      and shows the picture: ran on 2026-11-27 with dumps; surfaces still
      black (item above); its own effect not separated
- [ ] Run the `victory-gpu` tests with the new shader changes (`gpu-tests-hang`
      may get in the way)
- [ ] Mesh programs whose outputs pass the host's size limit: see whether
      mesh-as-compute, which has no such limit, can be chosen per program
      (`untranslated-shaders`). Lowering the declared maxima was tried and
      does not help
- [ ] Remaining shader refusals with a lead (`untranslated-shaders`): lane mask
      read as data (the AMD fork's `d1f883e`; its switch must be in the shader
      cache key, or go), `v_permlanex16_b32` (`fxceless/victory-fork`)
- [ ] Waves of 64: the unmerged branch with several console waves per group
      (`wave64-refusals`; needs the owner's OK to take it over)
- [ ] Refusals with no fix found yet (`untranslated-shaders`): structured
      buffer of unknown stride; a descriptor read as data (may need the
      descriptor heap's contents; that work is ours too)
- [ ] Look for fixes in forks not yet checked: `avatarmanz/VIctory-7900XTX`,
      `DexNom/VIctory`
- [ ] Reach one fixed test scene that every run can repeat (`scene-blocker`)
- [ ] When shader changes stop: make pipelines ahead of time again, with the
      same build that plays (`compile-backlog`)
- [ ] Frame replay tool, if the owner chooses it
- [ ] Speed, after the picture: look at waiting for the GPU after every batch
      sent, and at the copies between the two views of memory; the gain is not
      measured (`frame-preparing-cost`)

## Done lately

Not seen to change the picture yet. Where each ran: `docs/adapters.md`.

- [x] Loop limits by shader stage (`gpu-long-dispatch`)
- [x] Shader flag for UAVs outside pixel and compute (validator refusals)
- [x] Compute shaders with implicit derivatives pass the validator
- [x] Lane mask joined with data at a uniform join
- [x] Render-target dumps no longer stop the game: a submission kept the
      lock of the GPU detiling's lists while the dumps at its end submitted
      again (it stopped the title at the first captured frame since
      `a69a361`); one game run with dumps ran its full time

## Ranked problems

Ranked by effect on the picture, then stability, then speed. One row is one
overall problem, worked by at most one session
(`docs/problem-sessions.md`). `cargo xtask problem list` adds who holds what on
this machine. Ids and ranks change only when the owner re-ranks, or a diff adds,
splits or closes a problem; `cargo xtask problem` reads this table, so keep its
first two columns and number the rows from 1.

Picture before speed: speed problems (`compile-backlog`,
`frame-preparing-cost`, `texture-refresh-menus`) wait until one world frame is
correct, unless they keep runs from reaching the world at all.

**No rank below 1 is proven.** Nothing yet shows which loss keeps the 3D scene
from showing; the order is a guess from dropped-work counts, which do not
measure what the picture loses.

| rank | id | effect | how sure |
|------|----|--------|----------|
| 1 | `scene-blocker` | which loss keeps the 3D scene from showing is unknown; the answer ranks the rest | unknown |
| 2 | `placed-texture-slots` | heap-indexing shaders read an empty slot where the title placed a texture: most 3D surfaces have no base colour | causal by white substitution (main-pass black 65% to 39%, 49% to 10%); real textures not tried |
| 3 | `wave64-refusals` | compute needing waves of 64 is refused: most refused pipelines in world runs | counts measured; picture effect a guess |
| 4 | `descriptor-slots` | draws and dispatches dropped for lack of descriptor slots; a large share of drops | fixed in `main`, confirmed on the GTX 1070 (capped heap); an uncapped host (AMD) not run; picture effect a guess |
| 5 | `mesh-pipeline-invalid` | a mesh pipeline the host rejects loses many indirect draws in some runs, almost none in others | cause unknown |
| 6 | `gpu-long-dispatch` | evicting idle host textures that bindless shaders still write hung the GPU seconds into most world runs (fixed on main by fdc72fc: idle textures are lowered in priority, not evicted) | shown by 600 s runs: no such hang in 11 fix runs; main 0 of 2 past 11 s |
| 7 | `gpu-long-lists` | emulated mesh draws and the ray-tracing BVH builder's compute run for seconds in one list and trip the GPU timeout, in about half the world runs once the eviction hang is gone | GPU times and pixel counts measured; lead: loops cut at the 64-iteration limit make layered full-screen draws (one run without the cap had none) |
| 8 | `untranslated-shaders` | some programs do not translate or are refused by the validator (several reasons); one reason may need data only the descriptor heap has | refusals counted; picture effect a guess |
| 9 | `title-crash-breakpoint` | the title stopped itself on a failed internal check once, during a long world run | seen once, not reproduced since |
| 10 | `compile-backlog` | on a cold shader cache, work is dropped for a long stretch while pipelines are made | measured on cold cache only; warm effect small |
| 11 | `frame-preparing-cost` | CPU preparation is a large share of each world frame | measured, some runs not on a quiet machine |
| 12 | `texture-refresh-menus` | on the GTX 1070 slow texture refresh stalls the menus, so unattended runs miss the world | measured on that adapter, older build |
| 13 | `gpu-tests-hang` | `victory-gpu` tests can hang or read zeros on a real GPU, which weakens every proof | may be mostly fixed (guess) |
| 14 | `device-methods-missing` | the title calls device methods that are not implemented; what it loses is unknown (rank not confirmed by the owner) | seen in run reports, effect unknown |
| 15 | `gpu-timing-stamps` | with `VICTORY_GPU_TIMING`, a list of more than 4,095 noted commands charges the rest of the list to its last timed command, so slow-command lines mislead on long lists (rank not confirmed by the owner) | seen once: a dropped dispatch charged 4.9 s of a 5 s list |
| 16 | `host-bugcheck` | Windows itself stops (bug check 0x10E, VIDEO_MEMORY_MANAGEMENT_INTERNAL, first parameter 0x37: the video memory manager's own check) during GPU runs, ending every session on the machine; seen during a game run and during a frame replay (rank not confirmed by the owner) | seen twice on 2026-11-27 on the GTX 1070; both dumps: `dxgmms2!VIDMM_GLOBAL::EnsureLockedPages` after a failed lock of our process's pages, during an eviction from a system-memory segment; which pages, not known |
| 17 | `image-uncommitted-write` | the title's code wrote to an address reserved but never committed and the run stopped with an access violation (rank not confirmed by the owner) | seen once on 2026-11-27, in a quiet run of `main`; cause unknown |

## Finding what keeps the 3D scene from showing

The method for `scene-blocker`. It is a question, so its "Done when" is an answer
the owner accepts, not a visible scene.

1. Name one reference frame or moment that should show 3D geometry, and how a
   run reaches it. Say whether it is the intro or the playable world: geometry
   in one does not prove the other.
2. Capture it with all its render targets (`VICTORY_CAPTURE_FRAMES`,
   `VICTORY_DUMP_TARGETS=1`; dumps stay outside the repository). Once the
   frame replay exists (`docs/frame-replay.md`), record it once and do steps
   3 to 5 on the replay.
3. Walk back from the presented image to the earliest target that is flat or
   wrong although the title writes it. Whether the log can say which draws and
   dispatches write each target is not verified; adding that is part of the work.
4. List what was dropped or substituted among those writers and their inputs.
   Rule out the others: clears, formats, copies, culling inputs, constants.
5. A candidate counts only with causal evidence: restoring it changes the
   target, or removing equivalent working work reproduces the loss. A large
   dropped-draw count is not evidence.
6. Re-rank this table by what the chain shows, and mark what is still a guess.
