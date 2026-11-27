# Where a world frame's time goes: facts, assumptions, hypotheses

From world runs of one scene on an RTX 2060 SUPER (diagnostics build, Series X
profile), 2026-11-27. Figures are rounded per-frame medians over world frames.
Run folders and shader hashes stay on the developer's machine (rule #1). Live
work stays in `docs/problems.md`.

**How to read this.** Each slow part has three layers, kept apart on purpose:

- **Measured:** what the counters say.
- **Assumed:** what the code or an earlier explanation takes for granted. These
  go stale first; each says what would show it wrong.
- **Hypotheses (H):** possible causes, each with the evidence so far and a test
  that would confirm or refute it. Kind: **impl** (our implementation),
  **missing** (a function or feature we lack), **design** (a choice that
  several parts depend on).

None of the hypotheses below is tested yet. When one is tested, write the
result next to it, and move it out of the hypotheses when it is settled.

## The frame

| part | ms/frame | where |
|------|----------|-------|
| preparing, in all | ~500-700 | submitting thread |
| of it, refresh batches (title memory copied into host copies) | ~300 | submitting thread |
| of it, descriptor heap sync | ~55 | submitting thread |
| of it, texture uploads for bindless heaps | ~30 | submitting thread |
| GPU | ~550 | GPU |
| copy-back, including pages shaders marked (~40) | 50-110 | submitting thread |
| recording draws, summed over title threads | ~1,170 for ~6,500 draws | title threads, parallel |

The host waits for the GPU after each of ~40 submissions per frame, holding the
submission lock, so a frame is about preparing + GPU + copy-back. Dropped work
(below) makes all of these smaller than the full frame would be.

## The assumption under most of it

**Assumed:** "Memory is unified on the console and separate on the host, so all
GPU-visible memory needs a host copy kept in step with title memory."

That is only true of memory both processors use. Most GPU memory is probably
never touched by the CPU after it is filled: render targets, culling outputs,
meshlet lists, indirect arguments the GPU writes for itself. The code does not
tell the two kinds apart. Every buffer gets a system-memory host copy, a write
watch, change checks on every declared read, before-images and copy-back.

- **H0.1 (design): most of the bytes we track and copy are GPU-only.** If so,
  refresh checks, snapshots, copy-back and part of the GPU time would all fall
  for that memory if it lived only on the GPU side. Evidence: lists declare
  ~22 GB/frame of reads, but only ~5 GB lie in pages flagged as changed, and
  ~0.3 GB is actually copied. So most declared memory is not written by the
  CPU between uses. Test: per host copy, record over a run whether the CPU
  writes it after the first fill and whether the GPU writes it. Then sum bytes
  by class (CPU→GPU, GPU-only, both, neither).
- **H0.2 (missing): the title already says which memory is GPU-only, and we
  drop it.** Heaps and committed resources come with a heap kind (default,
  upload, readback). `resource.rs` uses it only for the title-side
  allocation's attributes (`Memory::for_resource`); `shadow::register` keeps
  only address and length, so host placement never sees it. Caution: on the console a "default" heap is still
  CPU-addressable, so the kind is a hint the title may break. Test: count CPU
  writes (guard faults, write watch) into buffers on default heaps. If there
  are none or almost none, the kind can drive placement, with a fallback for
  pages the CPU does touch.
  **Restated and tested, 2026-11-27** (`VICTORY_PROBE_CLASSES=1`, one 200 s
  world run). Heap kinds say little: most buffers are placed at addresses
  the title maps itself, which carry no kind (we label them default). The
  title does give its own memory calls console bits (above the host's
  protection) that `xmem` drops. So the probe tallies tracked and written
  64 KiB pages per frame by those bits, per 60 frames:
  - virtual memory with protection bit 0x800: up to ~15,000 pages (~0.9
    GB) tracked, **no page written in any line of the run**;
  - physical pages (two flag sets, the big GPU heap): up to ~30,000 and
    ~12,000 pages tracked, 1-3% written in the busiest lines;
  - virtual memory with bits 0x2400 or 0x22000: up to ~12,000 and ~800
    pages, a few % written.
  "Written" here counts every write the watch sees, kernel writes and our
  own copy-back included, so CPU writes are at most these. Result: **partly
  confirmed**. One class the title marks (bit 0x800) is never written
  after allocation and could live on the GPU side with no refresh. The big
  GPU heap is one class, written in a few % of its pages, so the title's
  bits cannot split it; that needs per-buffer classes (H0.1). Not yet
  known: what bit 0x800 means on the console, and whether the CPU reads it.
- **H0.3 (design): the CPU rarely reads what the GPU writes.** Copy-back
  assumes it might, everywhere. Test: guard a sample of pages after copy-back
  against reads (not only writes) and count faults per buffer.

If H0.1 and H0.2 hold, the largest lever is buffer placement: video memory for
GPU-only buffers, system memory and copies only where the CPU writes or reads.
It touches the preparing, copy-back and GPU rows at once.

## Refresh batches (~300 ms)

**Measured:** ~5,300 refreshes run per frame, and ~160,000 skip as unchanged.
Of ~5 GB of changed-flagged pages checked, ~0.3 GB is copied. Moving bytes
takes ~120 ms (kernel reads, ~2.6 GB/s). Watches take ~45 ms, page protection
~30 ms and the guard fault handler ~70 ms. ~3,300 tracked pages are written per
frame, ~3,600 are marked through another mapping of the same memory, and
~1,400 of them were also written the frame before.

**Assumed:** "What is copied is what the title's CPU wrote."

- **H1.1 (impl): 64 KiB pages magnify small writes.** The title writes a few
  constants and we copy the whole page. The ~0.3 GB/frame copied looks large
  next to what a console title's CPU writes in a frame. Test: when a page is
  copied, compare it with the copy's old bytes in 64-byte pieces (as xcl
  does) and count the pieces that differ.
- **H1.2 (design): our own write-backs come back as "CPU writes".** Copy-back
  writes GPU results into title memory (~75 MB/frame through write marks).
  That marks the pages changed, so the other host copies of the same memory
  copy them in again. GPU work then pays twice, and it is counted as CPU work.
  Test: keep the set of pages our copy-back wrote this frame, and count how
  much of the next refreshes falls in it.
- **H1.3 (design): one title page lives in several host copies.** These are the
  whole buffer, a 4 MiB loose window, single-view copies and page-table
  copies. Each is refreshed separately. Earlier probe notes: ~1.6 copies per
  page taken, ~25 trackers per page. Test: bytes refreshed per frame against
  distinct title bytes refreshed.
- **H1.4 (impl): kernel reads are slow.** The kernel read (`ReadProcessMemory`)
  avoids a fault on unmapped pages, but runs at ~2.6 GB/s where a plain copy
  runs at several times that. Test: an offline benchmark of both on mapped
  memory. A plain copy with a fault handler (or a check of the mapping,
  cached) would keep the safety.
- **H1.5 (impl): checking costs more than copying.** About 30 checks run for
  each copy actually made, and the diagnostics timers sit on that path
  (earlier notes: ~470 ns per skipped check in the diagnostics build against
  ~120 in release). Test: the same scene with the release build. Run counts
  will need a release-side counter.

## Shader-write step (~150 ms, previously misread)

**Measured:** this step's time includes the refresh flush it forces before each
snapshot. Snapshots themselves are ~300 MB in ~32 ms per frame.

The earlier version of this document read the ~150 ms as snapshot cost. That
was wrong: most of it is refresh work that would run anyway, only earlier.

- **H2.1 (impl): splitting batches makes refreshes cost more.** Each forced
  flush ends a batch early, so fewer ranges are merged and pages are taken
  more than once. Test: refresh time per byte, in submissions with and
  without shader writes.
- **H2.2 (design): before-images exist only because there are two copies.**
  Memory with a single home (H0.1) needs none.

## Descriptor heap sync (~55 ms)

**Measured:** ~660,000 heap slots are checked per frame, and ~50,000 are made.

**Assumed:** "The title writes descriptors into its heaps itself, so each
written page must be re-read."

- **H3.1 (impl): page granularity.** A written 64 KiB page holds 2,048 slots.
  The title changes a few, and all 2,048 are rechecked: ~92% of checks find
  nothing. Test: count changed slots per written page.
- **H3.2 (impl): the title rewrites equal descriptors every frame**, for
  example tables in a ring. Comparing the slot's 32 bytes with the last
  translated value would skip the translation. Test: count rewrites with
  unchanged bytes.
- **H3.3 (missing): host shaders cannot read console descriptors themselves.**
  This is a real difference between the machines: we must translate them.
  The translation cost is ours, the need is not.

## Copy-back (50-110 ms)

**Measured:** copy-back of snapshot ranges finds ~0 MB changed. The real
traffic is pages shaders marked: ~75 MB/frame written back in ~40-65 ms.

**Assumed:** "GPU writes must reach title memory."

- **H4.1 (design): most marked writes go to memory the CPU never reads** (see
  H0.3). They are written back only so that other host copies of the same
  memory see them (H1.3), and those copies then refresh again (H1.2). This is
  a cycle the copy design makes, not something the title needs.
- **H4.2: some writes are read by the CPU** (query results, GPU-driven streaming
  feedback, readback buffers). Those must stay; the readback heap kind marks
  some of them (H0.2).

## Recording draws (~180 µs per draw, title threads)

**Measured:** per frame, `for_view` ~100 ms, placed-buffer view making ~50 ms and
bindless records ~45 ms. ~23-28k placed-buffer slots are made per frame, all
behind one heap lock.

**Assumed:** "Each draw needs host views of its buffers."

- **H5.1 (impl): the same views are made again every frame.** The title binds
  the same buffers frame after frame. A cache keyed by (address, length,
  stride, format) would skip most of the work. Test: count views per frame
  whose key was seen the frame before.
- **H5.2 (impl): one heap lock serialises the title's recording threads.** The
  slot search under it was fixed already; the stack tallies still show waits
  in `mirror`. Test: share of title-thread samples waiting on that lock.
- **H5.3 (missing): host shaders cannot take a buffer's address from its
  console descriptor.** So every buffer needs a host view. Translating
  buffer descriptors to address reads through the title-memory page table
  (an xcl idea) would remove most of these views.
- **H5.4 (impl): diagnostics cost.** DRED alone is ~1.5x per draw, and the timing
  runs keep it off. Other diagnostics bookkeeping is on the same path.

## GPU (~550 ms)

**Measured:** per-command timestamps (`VICTORY_GPU_TIMING=1`, which inflates
totals by waiting after each list) rank first two small compute programs that
copy buffer to buffer. Each lane copies 16 bytes per loop turn, and the byte
count is read from a buffer. At the same group count, one dispatch takes 1 ms
and another 1.3-1.5 s (that run later lost its device, see H6.6; a run without
the loss ranks a wave-64 compute program with group memory first, then these
copies, all under 30 ms each). Next come a mesh culling program written for waves of
64 (up to 1.7 s per indirect draw) and a wave-64 compute program using group
memory (up to 250 ms).

**Assumed:** "The GPU is slow because the host GPU is slower than the console's."

- **H6.1 (design): buffers in system memory, across PCIe.** Every host copy is
  in system memory (custom heap, write-back, L0). The copy programs read and
  write only buffers, and a console moves those at GDDR6 speed. Test: run
  one heavy pass with its buffers placed in video memory and compare its
  timestamps.
- **H6.2 (missing, correctness): the copy count is wrong.** The count is read
  from a buffer. If its host copy is stale (an earlier pass wrote the count
  into another copy of the same memory, H1.3), or holds garbage, the program
  copies far too much. It runs until the compute loop limit (4,096 turns per
  lane), and its output is also cut short. This fits times that vary
  1,000-fold at the same group count. Test: log the count, read from the
  copy the GPU used, after the submission.
  **Refuted, 2026-11-27** (`VICTORY_PROBE_COUNT`, one 200 s world run):
  ~950 dispatches of both programs. Every count is sane: the largest copy is
  under 8 MB, under 1 MB for most, and the loop stride constant is 64 KiB.
  A dozen counts are all ones, which wrap to zero bytes (no work, not a long
  loop). The slowest of these dispatches took under 30 ms, against 1.0-1.5 s
  in the earlier run. Side finding: the counts are in the host copy only,
  and title memory reads zero there. The GPU writes them for itself and the
  CPU never sees them, which fits H0.1.
- **H6.6 (impl or driver): second-long commands are device trouble, not the
  program.** In the earlier run, commands of 0.2-1.7 s came from five
  unrelated programs during the last minute before the device was removed.
  In the new run, with no removal, one dispatch of yet another program took
  2.7 s once (10 groups; 3-6 ms elsewhere). So a one-off stall lands on
  whatever runs at that moment. Candidates: residency or paging of
  system-memory heaps, a page fault on the GPU, or the device drifting toward
  removal. Consequence: the per-program ranking above came from a failing
  device. It must be redone from runs without removal, and with outliers
  counted apart. Test: the times of the stalls against Windows' GPU paging
  events (a GPUView or PIX capture), and against the device-removed reason
  when there is one.
- **H6.3 (missing): waves of 64 run as two waves of 32.** The host has no wave
  size of 64, and exchanges between lanes cost extra in translation. The
  share is unknown. Test: the same programs on an adapter with waves of 64
  (AMD).
- **H6.4 (impl): translated code does more per access** (page-table lookups
  for computed addresses, bounds handling). Test: compare instruction counts
  of the host shader and the console program for the top passes.
- **H6.5 (impl): measured GPU time includes idle gaps.** It is wall time from
  submission to fence, ~40 times a frame. Test: timestamps at each
  submission's start and end against the wall time.

## Waiting after every submission

**Assumed:** "The host must wait for each submission before preparing the next."

- **H7.1 (design): the wait exists for copy-back and snapshots.** Memory that
  has a single home (H0.1) needs neither, so the next submission could be
  prepared while the GPU runs, hiding up to the smaller of preparing and GPU
  time. The order "copy-back, then bottom-of-pipe writes" must stay
  (`crates/victory-gpu/CLAUDE.md`).

## Missing functionality hides cost

| missing | now | once added |
|---------|-----|------------|
| dynamic descriptor slots on a capped view heap (`descriptor-slots`) | ~1,000 (median) to ~3,900 draws/frame dropped, mostly indirect mesh draws | more recording, refreshes and GPU |
| waves of 64 on wave-32 hosts (`wave64-refusals`) | 26 pipelines refused | more GPU |
| untranslated programs (`untranslated-shaders`) | a few per frame | small |
| shader writes to memory with no host copy | ~7,700 ranges/frame withheld | correctness |
| two device methods | unknown | unknown |

So the frame is both slow and incomplete. The hypotheses above are ranked by
the bytes they touch, not by measured savings.

## Tried

- **Title memory used by the GPU in place** (`VICTORY_IN_PLACE=1`, off). It
  serves H0.1 from the other side. The runtime maps title memory as 64 KiB
  views, so the big GPU heap never opened whole. An open heap also pins its
  view, so Windows refused the title's unmaps, and the title crashed. Leads:
  - open the section rather than the view (`docs/problems.md`);
  - for pools the title allocates whole, open them at allocation time and
    release before its free (needs a hook on the free).
- **Heap slot search** replaced by a bitmap: recording sums fell, frame time
  did not.
- **Earlier preparing work:** −38% of refresh-check cost. Estimate then: another
  50% is not reachable without changing the page-table rewrite (problem
  notes of `frame-preparing-cost`). That estimate assumed the copy design;
  H0.1-H0.3 question the design itself.

## Where the evidence comes from

- Preparing parts: `copystat` lines in a smoke run with
  `--env VICTORY_D3D12_DRED=0`, world frames only.
- GPU ranking: `--env VICTORY_GPU_TIMING=1`, `slow list command` lines summed
  per program.
- Console code of a program: `VICTORY_DUMP_SHADERS` / `VICTORY_DUMP_HASHES` on
  a run (outside the repository).
- Title-thread waits: `VICTORY_STACKS`, `cargo xtask stacks --tally`.
