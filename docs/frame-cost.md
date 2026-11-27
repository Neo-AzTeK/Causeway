# Where a world frame's time goes, and why

Written 2026-11-27 from world runs of the same scene on an RTX 2060 SUPER
(diagnostics build, Series X profile). It answers one question for each slow
part: is it slow because of how we implemented it, or because something is
missing? Figures are rounded per-frame medians over world frames. Run folders
and shader hashes stay on the developer's machine (rule #1). Live work stays in
`docs/problems.md`.

## Short answer

- **CPU preparation (about 500-700 ms) is mostly our implementation.** It is the
  cost of keeping host copies of the title's memory up to date, and of
  following what the title writes. Little of it comes from a missing function.
- **GPU time (about 550 ms) is mostly where memory lives.** Every buffer the GPU
  reads or writes is a host copy in system memory, reached over PCIe. A smaller,
  unmeasured share is waves of 64 being run as two waves of 32.
- **Missing functionality mostly hides cost; it does not add it.** Between about
  1,000 and 3,900 draws per frame are dropped (nearly all for lack of
  descriptor slots), and some pipelines are refused. Today's frame is both slow
  and incomplete. Each missing part we add makes the frame heavier before it
  gets faster.
- **The parts run one after another.** The host waits for the GPU after every
  submission (about 40 per frame), so a frame costs about preparing + GPU +
  copy-back, with no overlap.

## The frame, part by part

| part | ms/frame | thread | cause | kind |
|------|----------|--------|-------|------|
| refresh batches: title writes copied into host copies | ~300 | submitting | memory is unified on the console, separate on the host: every write the title makes to GPU-visible memory must be found and copied | implementation (design) |
| of it, moving bytes (kernel reads) | ~120 | submitting | copies use a kernel read so an unmapped page cannot fault | implementation |
| of it, finding what changed (watches, page protection, guard faults) | ~80 | submitting | changes are tracked per 64 KiB page | implementation |
| shader-write snapshots and the flushes they force | ~150 | submitting | copy-back must know what the GPU wrote, so a before-image is taken | implementation (design) |
| heap sync: title-written descriptor slots | ~55 (660k slots checked) | submitting | console descriptors are plain memory the title writes; we re-read and translate them | both: hardware difference, made costly by re-checking |
| written-back pages that shaders marked | ~40 | submitting | shaders write title memory through computed addresses | implementation |
| texture uploads for bindless heaps | ~30 | submitting | textures are host objects, refreshed when their memory changes | implementation |
| copy-back after the GPU | 50-110 | submitting | GPU writes in host copies go back to title memory | implementation (design) |
| recording draws (summed over title threads) | ~1,170 for ~6,500 draws (~180 µs each) | title threads, parallel | one host buffer view per placed buffer per draw (`for_view` ~100, view making ~50, bindless ~45), behind one heap lock | implementation |
| GPU | ~550 | GPU | see below | mostly memory placement |

Preparing and recording overlap with each other, but neither overlaps with the
GPU: the submitting thread holds the submission lock through the GPU wait.

## GPU time

Per-command GPU timestamps (`VICTORY_GPU_TIMING=1`; it waits after each list,
which inflates the total, so use it for ranking, not totals) rank the commands:

1. **A small compute program that copies buffer to buffer.** It is 68 console
   instructions: each lane copies 16 bytes per loop turn, and the byte count is
   read from a buffer. With the same group count, one dispatch takes 1 ms and
   another up to 1.3 s. Its cost is the bytes it moves divided by bandwidth,
   and both buffers are host copies in system memory. A console copies such
   memory at GDDR6 speed, this host over PCIe. **Kind: memory placement.**
   Not verified: whether the largest counts are the title's real sizes, or a
   wrong count that runs until the compute loop limit (4,096 turns per lane).
   If it is the limit, the copy is also cut short, which is a correctness loss.
   Next check: log the count it reads.
2. **A second copy program of the same shape**, with the same pattern: up to 1.5
   s per dispatch. **Kind: memory placement.**
3. **A mesh culling program written for waves of 64** (65 loop branches, 83
   registers), up to 1.7 s per indirect draw. On a host with waves of 32 it
   runs as two halves. **Kind: missing hardware feature, so translation
   cost**, together with memory placement. The share of each is not measured.
4. **A wave-64 compute program using group memory**, up to 250 ms. Same kind
   as 3.
5. Further down, many 5-30 ms dispatches of large compute passes. They read and
   write big buffers in system memory.

Long dispatches of this kind are also what removes the device
(`gpu-long-dispatch`): one timing run lost the device 90 s into the world.

## Missing functionality, and what it hides

| what is missing | effect now | effect on cost once added |
|-----------------|------------|---------------------------|
| dynamic descriptor slots on a capped view heap (`descriptor-slots`) | ~1,000 (median) to 3,900 draws/frame dropped, mostly indirect mesh draws | more recording, refreshes and GPU work |
| waves of 64 on wave-32 hosts (`wave64-refusals`) | 26 pipelines refused | more GPU work |
| shaders not translated (`untranslated-shaders`) | a few programs per frame dropped | small |
| shader writes to memory with no host copy | ~7,700 ranges/frame withheld, ~450 MB of views kept | correctness, then copy-back cost |
| two device methods | unknown | unknown |

## Tried and ruled out

- **Title memory used by the GPU in place** (idea from xcl-dev/xcl,
  `VICTORY_IN_PLACE=1`, off by default). It works for a whole view, but the
  runtime maps title memory as 64 KiB views, so the big GPU heap never opens
  whole. An open heap also pins its pages, and Windows then refuses the
  title's unmaps: the title crashed in a world run. Lead: open the section
  instead of the view (`docs/problems.md`).
- **Heap slot search** (a linear scan under the heap lock) was the top lock
  wait on title threads. Replaced by a bitmap: draw recording sums fell, frame
  time did not move.
- Earlier preparing work cut refresh-check cost by about 38%. The estimate
  then was that another 50% is not reachable without changing the page-table
  rewrite (problem notes of `frame-preparing-cost`).

## What this means for the work order

1. Keep correctness first: dropped draws and refused pipelines make the frame
   both wrong and look cheaper than it is.
2. The biggest single lever is **where buffer memory lives**: video memory
   for buffers that only the GPU uses (copy targets, culling outputs), with
   copy-back only where the title reads them. This attacks both the GPU copies
   and the snapshots.
3. **Overlap the CPU and the GPU**: prepare the next submission while the GPU
   runs the current one. Copy-back ordering must stay (`victory-gpu`
   CLAUDE.md).
4. Then the per-draw buffer views (descriptors read as addresses, xcl idea
   2), and refresh batches.

## Gaps in measurement

- No per-command GPU time without the per-list wait, which changes the totals.
- The size each GPU copy program is asked to move is not logged.
- Every figure comes from the diagnostics build; the release build is not
  measured. DRED alone costs about 1.5x per draw (`docs/debugging.md`).
- One adapter only (RTX 2060 SUPER); host copies sit in system memory on every
  discrete GPU, so the GPU conclusion should hold there. Integrated GPUs share
  memory and are not measured.

## How to repeat

- Preparing parts: a smoke run with `--env VICTORY_D3D12_DRED=0`; the
  `copystat` lines (`copies per frame`, `recording per frame`,
  `placed buffer parts per frame`) in the log, world frames only.
- GPU ranking: `--env VICTORY_GPU_TIMING=1`; the `slow list command` lines,
  summed per program.
- A program's console code: `VICTORY_DUMP_SHADERS=<dir>` and
  `VICTORY_DUMP_HASHES=<h,...>` on a run (outside the repository).
- Where title threads wait: `VICTORY_STACKS` and `cargo xtask stacks --tally`.
