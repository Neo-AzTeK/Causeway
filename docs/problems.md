# Problem list and todo

What to do next, and the ranked problems behind it. **Update it whenever a task
ends:** tick or drop what is done, add what was found, and move what comes next
to the top. Measured figures, run folders and shader hashes stay on the
developer's machine, never here (rule #1).

Goal: **one correct frame** (`docs/agent-workflow.md`).

## Todo, next first

Each item names its problem in the ranked table below.

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
- [ ] Speed (`frame-preparing-cost`): test the hypotheses in
      `docs/frame-cost.md`, one at a time, writing each result there. Order
      (cheap tests first, then by bytes touched):
      - [x] H6.2 the slow GPU copy programs read a wrong or stale count:
            refuted (counts sane); second-long commands are H6.6
            (also correctness)
      - [x] H0.2 the title marks GPU-only memory: partly (protection bit 0x800
            never written; the GPU heap is one class). Old wording: CPU
            writes into buffers on default heaps
      - [x] H0.1 bytes by writer: confirmed (title code writes ~3% of
            tracked memory; ~44% written by nobody in two minutes)
      - [ ] H0.4 ~5.7 GB of distinct heap pages streamed in two minutes of
            a still scene: re-requested loads?
      - [ ] H0.3 (rest of the old item:)
            CPU reads of written-back pages
      - [ ] H1.2 our own write-backs refreshed back in as CPU writes
      - [ ] H1.3 one title page in several host copies
      - [ ] H1.1 changed 64-byte pieces per copied page
      - [ ] H3.1 / H3.2 changed and equal-rewritten heap slots per page
      - [ ] H5.1 views per frame seen the frame before
      - [ ] H1.4 kernel read against plain copy (offline benchmark)
      - [ ] H1.5 / H5.4 the same scene with the release build
      - [ ] H6.1 one heavy pass with its buffers in video memory
      - [ ] H6.6 second-long one-off stalls (paging, GPU faults, device
            drift); redo the GPU ranking from runs without removal
      - [ ] H6.3 / H6.4 / H6.5 wave 64, translated code, idle gaps
      - [ ] H2.1 refresh cost per byte with and without shader writes
      - [ ] H7.1 overlap preparing with the GPU, if H0.x hold
- [ ] Ideas from xcl-dev/xcl (GPL-2.0-only: ideas only, our own code), by
      measured cost (world frame ~1.3 s: 709 ms preparing on the submitting
      thread, 550 ms GPU): (1) changed-bytes upload: copy only 64-byte chunks
      that differ from what was last sent, on a copy queue the title's queue
      waits on in the GPU, so preparing leaves the submitting thread; (2)
      buffer descriptors read as their title address through the page table,
      not one host view each (23-28k placed-buffer slots a frame, 190-645 ms,
      `for_view` up to 1.1 s, on title threads, all behind one heap lock);
      (3) title memory the GPU uses in place (`OpenExistingHeapFromAddress`):
      host copies of buffers are system memory already, so the GPU reads at
      today's cost and refresh, snapshot and copy-back go. Probes
      (`inplace.rs`, RTX 2060 SUPER) open committed pools and physical-page
      section views in placeholders, GPU writes land in place; the sized form
      opens part of an allocation. Tried (`VICTORY_IN_PLACE=1`, off by
      default): the runtime maps title memory as 64 KiB views, so the big GPU
      heap never opened whole; and an open heap pins its view, so the
      title's unmaps were refused while lists or slots held the copy (167 in
      one world run), then the title crashed. Next lead: open the section
      itself (`OpenExistingHeapFromFileMapping`), which pins no addresses;
      title addresses then need their section offset, contiguous only where
      the title maps pages in order

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

**No rank below 1 is proven.** Nothing yet shows which loss keeps the 3D scene
from showing; the order is a guess from dropped-work counts, which do not
measure what the picture loses.

| rank | id | effect | how sure |
|------|----|--------|----------|
| 1 | `scene-blocker` | which loss keeps the 3D scene from showing is unknown; the answer ranks the rest | unknown |
| 2 | `placed-texture-slots` | heap-indexing shaders read an empty slot where the title placed a texture: most 3D surfaces have no base colour | causal by white substitution (main-pass black 65% to 39%, 49% to 10%); real textures not tried |
| 3 | `wave64-refusals` | compute needing waves of 64 is refused: most refused pipelines in world runs | counts measured; picture effect a guess |
| 4 | `descriptor-slots` | draws and dispatches dropped for lack of descriptor slots; a large share of drops | counts measured (some mislabelled as pipeline losses); picture effect a guess |
| 5 | `mesh-pipeline-invalid` | a mesh pipeline the host rejects loses many indirect draws in some runs, almost none in others | cause unknown |
| 6 | `gpu-long-dispatch` | long dispatches remove the GPU; held off by loop limits, a skip switch, and by leaving a fix for indirect dispatches reverted | skip proven to avoid one removal; the second link strongly suggested by one run; loop limits' effect not recorded |
| 7 | `untranslated-shaders` | some programs do not translate or are refused by the validator (several reasons); one reason may need data only the descriptor heap has | refusals counted; picture effect a guess |
| 8 | `title-crash-breakpoint` | the title stopped itself on a failed internal check once, during a long world run | seen once, not reproduced since |
| 9 | `compile-backlog` | on a cold shader cache, work is dropped for a long stretch while pipelines are made | measured on cold cache only; warm effect small |
| 10 | `frame-preparing-cost` | CPU preparation is a large share of each world frame | measured, some runs not on a quiet machine |
| 11 | `texture-refresh-menus` | on the GTX 1070 slow texture refresh stalls the menus, so unattended runs miss the world | measured on that adapter, older build |
| 12 | `gpu-tests-hang` | `victory-gpu` tests can hang or read zeros on a real GPU, which weakens every proof | may be mostly fixed (guess) |
| 13 | `device-methods-missing` | the title calls device methods that are not implemented; what it loses is unknown (rank not confirmed by the owner) | seen in run reports, effect unknown |

## Finding what keeps the 3D scene from showing

The method for `scene-blocker`. It is a question, so its "Done when" is an answer
the owner accepts, not a visible scene.

1. Name one reference frame or moment that should show 3D geometry, and how a
   run reaches it. Say whether it is the intro or the playable world: geometry
   in one does not prove the other.
2. Capture it with all its render targets (`VICTORY_CAPTURE_FRAMES`,
   `VICTORY_DUMP_TARGETS=1`; dumps stay outside the repository).
3. Walk back from the presented image to the earliest target that is flat or
   wrong although the title writes it. Whether the log can say which draws and
   dispatches write each target is not verified; adding that is part of the work.
4. List what was dropped or substituted among those writers and their inputs.
   Rule out the others: clears, formats, copies, culling inputs, constants.
5. A candidate counts only with causal evidence: restoring it changes the
   target, or removing equivalent working work reproduces the loss. A large
   dropped-draw count is not evidence.
6. Re-rank this table by what the chain shows, and mark what is still a guess.
