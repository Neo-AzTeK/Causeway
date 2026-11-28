# Problem list and todo

What to do next, and the ranked problems behind it. **Update it whenever a task
ends:** tick or drop what is done, add what was found, and move what comes next
to the top. Measured figures, run folders and shader hashes stay on the
developer's machine, never here (rule #1).

Goal: **one correct frame** (`docs/agent-workflow.md`).

## Todo, next first

Each item names its problem in the ranked table below.

- [ ] Windows stops with 0x10E (`host-bugcheck`), on watch: all three
      stops came right after a GPU reset or stall, in the video memory
      manager's eviction (a failed page lock). Resets stopped: none in 56
      runs since 2026-11-27 17:50 (TdrDelay 10 s was set at 17:13), against
      a device removal in 60 of ~183 runs before. Guards over views the
      runtime did not report: none in the run that stopped (weak lead). The
      branch's cap on host copies is not merged (no way to show it helps
      without resets). On the next 0x10E or device removal: copy the dump
      and find which allocation the lock lacked before any fix

- [ ] With in-place sections on, the title's code still writes half or f32
      infinities (empty bounding boxes, after two -1 quadwords) over its
      own objects, then reads a pointer or an index from them and faults
      (`inplace-float-overwrite`). Two in-place holes fixed and merged (H11,
      H12, tests fail without), not sufficient: 4 of 6 runs crashed on them.
      In-place off shows the family too (rb-ip0c-2, 1 of 3; 9 of 11 clean
      off, about 5 of 11 on), so in-place is not the only source. No empty
      bounds appeared during in-place GPU writes (`VICTORY_PROBE_EMPTY_BOUNDS`,
      rb-eb-2). Next: who writes the title objects whose pointer fields hold
      floats or -1 (all five faulting sites load one)
- [ ] The title's code wrote to memory reserved but never committed and the
      run stopped (0xc0000005 in the title's image, one quiet run of `main`
      before the `descriptor-slots` merge; `image-uncommitted-write`). Not a
      missing commit: a bytecode interpreter in the title stored through an
      out-of-range operand index into a register file on its own stack, past
      the stack's top. Find where the bad index came from; the crash report
      now names the reservation, the thread's stack and what the registers
      point at, for the next time it happens

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
- [ ] Texture slots shaders pick by index stay empty because memory runs
      out (`placed-texture-slots`): probes show the waiting slots need about
      as much video memory again as the textures already resident, which
      are all mapped (holding only mapped levels would not free room) and
      kept resident only because the heap views them. Read bits are built:
      shaders mark each host slot they read by index (`view_reads`, bits
      after the write bits in the marks bitmap), and the texture queue makes
      read slots first, up to the critical memory level; the full shader
      scan lost no program. Next: a world run's "read marks since the last
      queue line" figures (how many read slots wait null, how many get made)
      and its frames against the last run; if read slots then fit, stop
      keeping unread heap-viewed textures resident (null their slots first)
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
- [ ] Waves of 64 (`wave64-refusals`): census 0 programs require them
      (2026-11-28). The last 2 (the triangle clipper's `CollectTriangles`,
      whose LDS fills the host's 32 KiB) borrow the top of their own LDS for
      the halves' slots, kept and put back around each exchange (3 more
      barriers each). Their run as halves is not seen yet: neither was bound
      in the gate run. Not shown by a test: that a wave that is on gets a
      load's result after the code guarded for off waves (the new test's
      load is unused). Left: "Done when" to agree with the owner. The
      unmerged branch with several console waves per group is superseded by
      `linear` on `main` (owner's call to drop it)
- [ ] Refusals with no fix found yet (`untranslated-shaders`): structured
      buffer of unknown stride; a descriptor read as data (may need the
      descriptor heap's contents; that work is ours too)
- [ ] Reach one fixed test scene that every run can repeat (`scene-blocker`)
- [ ] When shader changes stop: make pipelines ahead of time again, with the
      same build that plays (`compile-backlog`)
- [ ] Frame replay matches the run (branch `frame-record`, not merged;
      `docs/frame-replay.md`). Now on the replay: descriptors translated,
      compute-written textures and base colour on people come through,
      GPU-written counters start as in the run, the run's host copies of
      title memory are made. Still wrong: no world geometry (emulated mesh
      draws) reaches depth. Two replay lists read ~43,000 pages of tiled
      memory (where the title maps page-pool pages) that no host copy of the
      run covered, so they read zeros and withhold all their writes: the
      replay computes addresses the run never read. Next: find the first
      dispatch whose reads leave the run's copies, and which input differs
- [ ] Recording runs crash the title at `+0x23cf683` (write to 0), most
      likely when a snapshot stalls the submitting thread for seconds
      (9 recordings, no other run); being fixed on its own branch
- [ ] Speed and memory (`frame-preparing-cost`): the design agreed with the
      owner is `docs/memory-design.md` (rewritten 2026-11-27): each byte
      lives where its users are; assets stream from disk to video memory,
      GPU-only memory lives only there, the title's pages for both stay
      unbacked and guarded, and textures are made when read and evicted
      when not. Next: step 0, one run measuring memory by class and owner,
      CPU reads of loaded data, and GPU bytes per frame. Built: sections
      used in place (median frame 500 ms against ~880-980, but 6.1 GB
      committed up front, to decide after step 0). Remaining hypotheses in
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
      - [x] H0.4 streaming volume is real: ~4.4 GB of DirectStorage loads per
            ~2 minutes standing still, ~18% reloads of the same data
      - [x] H0.5 our memory answers make the title evict and reload: mostly
            refuted (it never asks for working-set statistics)
      - [ ] H0.6 the title's own streaming churns (not retries: tested);
            parked until its streaming inputs are known
      - [ ] H0.3 CPU reads of pages the GPU writes
      - [x] H1.2 our own write-backs refreshed back in: small (~1% of pages)
      - [ ] H1.3 one title page in several host copies
      - [ ] H1.1 changed 64-byte pieces per copied page
      - [ ] H3.1 / H3.2 changed and equal-rewritten heap slots per page
      - [ ] H5.1 views per frame seen the frame before
      - [ ] H1.4 kernel read against plain copy (offline benchmark)
      - [ ] H1.5 / H5.4 the same scene with the release build
      - [x] H6.1 buffers in video memory: mostly refuted (GPU time about
            unchanged with every copy in video memory)
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

- [x] Pointers of all ones read from loaded resources (`title-read-minus-one`):
      seven holes fixed, each with a test (lazy pages opened before their
      bytes were written, DirectStorage reads started on unwritten page
      lists, heap slots over in-place memory kept after a remap, lists
      recorded before a remap or an unmap, a copy opened during an unmap,
      undeclared writes to remapped pages); all merged into main (273d94d),
      7 of 9 default 300 s runs clean on them (1 of 8 before). The crash
      left is problem 20 (`inplace-float-overwrite`)
- [x] In-place sections left their size charged to system commit after
      exit (`inplace-commit-leak`): a section opened for the GPU by handle
      stays charged (probe `inplace::tests::section_commit_after_exit`);
      section heaps are opened by address over a view of our own (e28cfd7,
      merged); two title runs left system commit flat
- [x] `descriptor-slots` confirmed on `main`: two quiet world runs lose no
      draw for lack of slots, and the merge did not raise refusals (problem
      notes, "Confirm: refusal shift")
- [x] `victory-shader` barycentrics test skips on a host without them
      (5ce24f1, `Host::has_barycentrics`)
- [x] Forks `avatarmanz/VIctory-7900XTX` and `DexNom/VIctory` scanned
      (2026-11-27): leads in the developer's `fork-scan.md`; x7900's
      `431cab5` ported and merged into `main` (`wave64-refusals`)
- [x] GPU timing times every command of long lists (`gpu-timing-stamps`):
      timestamps in chunks of 4,096 made as a list notes more commands; a
      command left without an end stamp is counted as not timed, never
      charged with the rest of the list
- [x] With `VICTORY_PROBE_VRAM_COPIES=1`, slow-list warnings and "removed
      while list N ran alone" named the list before the one that ran: the
      probe's copy is submission group 0 (`host::run_of_group`, unit test)
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
overall problem; any session may work on any number of them
(`docs/problem-sessions.md`). `cargo xtask problem list` adds each one's state
and branch on this machine. Ids and ranks change only when the owner re-ranks, or a diff adds,
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
| 3 | `wave64-refusals` | compute needing waves of 64 is refused: most refused pipelines in world runs | census 42 -> 0 programs (first-lane reads of console-wave-alike values; groups of several console waves run memory accesses where a wave is off; slots borrowed from a full LDS); world run: 25 refused pipelines -> 0, no dispatch dropped as refused (was 24 per frame), most of them the tiled shadow renderer; picture effect not seen (3D scene not shown yet) |
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
| 15 | `gpu-timing-stamps` | with `VICTORY_GPU_TIMING`, a list of more than 4,095 noted commands charged the rest of the list to its last timed command, so slow-command lines misled on long lists (rank not confirmed by the owner) | seen once: a dropped dispatch charged 4.9 s of a 5 s list; fixed in `main` (timestamps in chunks made on demand, each command timed; tests), not yet seen in a title run |
| 16 | `host-bugcheck` | Windows itself stops (bug check 0x10E, VIDEO_MEMORY_MANAGEMENT_INTERNAL, first parameter 0x37: the video memory manager's own check) during GPU runs, ending every session on the machine; seen during a game run and during a frame replay (rank not confirmed by the owner) | seen three times on 2026-11-27 on the GTX 1070, each right after a GPU reset or stall; dumps: `dxgmms2!VIDMM_GLOBAL::EnsureLockedPages` after a failed lock of our process's pages, during an eviction from a system-memory segment; which pages, not known; no reset and no stop in 56 runs since 2026-11-27 17:50 (on watch) |
| 17 | `image-uncommitted-write` | the title's code wrote to an address reserved but never committed and the run stopped with an access violation (rank not confirmed by the owner) | seen once on 2026-11-27, in a quiet run of `main`; a wild index in the title's interpreter, its source unknown; crash report extended to catch it |
| 18 | `title-read-minus-one` | the title's code reads a pointer of all ones (or zero) from a loaded resource's memory and the run stops; most world runs on `main` since lazy disk loads (rank not confirmed by the owner) | seven holes fixed (lazy fill order, stale page lists, slots over in-place memory after a remap, lists recorded before a remap or an unmap, a copy opened during an unmap, undeclared writes to remapped pages); the first three merged into main, then crashes again in 7 of 8 runs; the last four merged into main (273d94d): 7 of 9 default 300 s runs clean on them (1 of 8 before), the crash left is problem 20; in-place off 7 of 8 clean |
| 19 | `inplace-commit-leak` | each run with in-place sections leaves the sections' size charged to system commit after it exits, so a session's later runs fail to allocate (rank not confirmed by the owner) | cause shown by a probe (a section opened for the GPU by handle stays charged after exit); fixed by opening a view of it by address (e28cfd7, merged); two title runs left commit flat |
| 20 | `inplace-float-overwrite` | with in-place sections on, the title's code writes float infinities over its own objects and faults reading them; about 1 run of 6 on the fixes of problem 18 (rank not confirmed by the owner) | split from problem 18 after seven in-place holes were fixed. Merged into main: H11 (pages disk loads left without bytes filled for in-place copies, 126278a, 957b59d) and H12 (what shaders write in place is a write for trackers at the page's other mappings, 8725ea7, marked after copy-backs f35fa53, cost 453f641), tests fail without; not sufficient: 4 of 6 runs crashed on them (rb-h12b-*), and in-place off crashes the same way too (rb-ip0c-2: a pointer field holding floats), 9 of 11 clean off against about 5 of 11 on. All five sites disassembled load a pointer or index from a title object that holds floats or -1. Next: who writes those objects (scope wider than in-place, owner to rank); open perf: `Shadow::stale` in place takes the guard lock per copy (+60 ms preparing, rb-merge-1 vs rb-base-main-1) |

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
