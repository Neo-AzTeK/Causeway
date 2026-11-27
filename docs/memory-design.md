# Title memory on a discrete GPU: design

Proposal, 2026-11-27. Step 1 built. Steps 2-3 held: the probe for step 2 (H6.1)
found no GPU-time gain on a discrete card; step 4 not built. It rests on the tested hypotheses in
`docs/frame-cost.md` (H0.1-H0.6, H1.2, H6.2). Live work stays in
`docs/problems.md`.

## The problem in one paragraph

Every GPU-visible title buffer has a host copy in system memory. Before each
use, every declared read range is checked for title writes (~160,000 checks a
frame), changed pages are copied in whole (~0.3 GB a frame), and GPU writes
are copied back after each submission. All of it is serialized with the GPU.
Tests show what that machinery serves:

- the title's code writes ~3% of the memory we track;
- ~44% is written by nobody for minutes;
- ~52% is written by loads (DirectStorage), once per load;
- one class the title marks (protection bit 0x800, up to ~1 GB) is never
  written after allocation;
- GPU results written back touch ~1% of pages.

So we pay per-use costs on memory that changes rarely. The GPU then reads all
of it over PCIe, because the copies live in system memory.

## Principles

1. **Pay per change, not per use.** A check or a copy happens because
   something wrote, never because a list reads.
2. **Each byte has one home per side.** Title memory stays on the CPU side.
   On the GPU side the home is video memory where the budget allows, system
   memory otherwise (the UMA adapters and over-budget cases).
3. **Writes travel as uploads and readbacks**, recorded on the GPU like any
   other copy: not as CPU copies into mapped GPU memory.
4. **Nothing is decided from one adapter.** Placement follows `Caps` and the
   live budget (`vram.rs`), as textures do already.
5. **Correctness first.** Every step keeps today's tests, and adds one that
   fails if a title write is missed (`crates/victory-gpu/CLAUDE.md`).

## The design

### Change detection stays; checks become free when nothing changed

The guards (write-protected pages, faults, the write watch) already record
every title write per 64 KiB page, in each tracker's `Changed` set. A check
today takes a lock and walks a range of that set, even when the set is empty.

- Each tracker gets an atomic count of its changed pages, kept with the set.
  A refresh range on a tracker whose count is zero is skipped with one
  atomic load: no lock, no batch entry. Most of the ~160,000 checks a frame
  are of this kind (H0.1: nearly half the memory is quiet for minutes).
- Optional: a frame-wide "nothing written since the last flush" counter
  skips a whole batch.

### Host copies move to video memory, filled by uploads

Each host copy becomes a default-heap buffer when the budget has room
(`vram::room`, the same budget textures use). Otherwise it stays as today.
A flush of changed pages no longer writes the mapped copy:

- the changed byte ranges (`Tracked::take`, as today) are read from title
  memory into an upload ring, still with the kernel read, safe on unmapped
  pages;
- the submission records `CopyBufferRegion`s from the ring into the copy,
  before the title's lists, in one list (as texture conversions are done
  today: `Batch::convert`);
- loads (DirectStorage, file reads) need no special path: they mark pages
  like any write, and the next use uploads them once.

The GPU then reads every buffer from video memory. This is also the test of
H6.1: the copy programs' time should fall.

### GPU writes come back through readback, only where marked

Copy-back today reads the mapped copy after the wait. With copies in video
memory:

- pages shaders marked (the view-mark bits) and declared write ranges are
  copied, in the same submission after the title's lists, into a readback
  ring;
- after the wait, ring to title memory, as `copy_back` does now, through
  `gpu_writing`. The order stays: copy-back first, then bottom-of-pipe
  writes.

Before-images (snapshots) exist to tell GPU writes from title writes in a
page both touch. They stay for pages that are changed and also written by
shaders in one submission; everywhere else there are none.

### Quiet memory skips the machinery

A tracker with no title write for a long while (a share of frames, not a
fixed time) keeps its guard armed and costs nothing per use (first part).
Memory the title marked GPU-side (bit 0x800, H0.2) is just a case of that;
no special path, since its bit's meaning is not known.

### Overlap follows (later)

When copy-back only reads rings, the host need not wait for each
submission before preparing the next: rings are per submission, released at
its fence. This is H7.1. It is a separate step after the others are proven.

## Steps, each with its test

| step | change | test (must fail before the fix) | done when |
|------|--------|-------------------------------|-----------|
| 1 | atomic changed-page count per tracker; skip zero-count refreshes without the lock | a title write between two lists is still seen (`host.rs`, "the next list after a title write"); a new test: a write to a tracker with count 0 makes the next refresh copy | refresh batches in a world window cost at least 50% less, by the `copystat` medians |
| 2 | copies in video memory with uploads, under the budget, falling back per copy | a GPU read after a title write sees it, for both placements (fake caps, `caps::reference`); upload ring wrap | GPU time of the copy programs falls (H6.1); picture not worse |
| 3 | copy-back through readback rings | marked GPU writes reach title memory; a page both sides wrote keeps both (existing before-image tests, both placements) | copy-back time not worse; `marked pages written back` unchanged |
| 4 | prepare the next submission while the GPU runs | fence order: rings reused only after their fence; bottom-of-pipe writes after copy-back | frame time below preparing + GPU |

Each step lands alone, behind no switch once its tests pass on both
placements (the hook's capability-query rule applies to steps 2-4).

## Risks and unknowns

- **Budget.** Tracked buffers were up to ~3.6 GB of 64 KiB pages in one
  frame, next to textures. On an 8 GB card some copies will stay in system
  memory. The fallback is today's path, so that is slower, not wrong.
- **Views point at copies.** A copy that moves between heaps changes its GPU
  address, so every view and descriptor naming it must be made again. So
  placement is decided once, when a copy is made, not migrated (first
  version).
- **Upload volume.** ~0.3 GB a frame is copied today; most of it is
  repeated ring writes by the title and loads. Uploads move the same bytes
  through PCIe once, instead of the GPU reading them over PCIe on every use.
  Page grain stays the multiplier (H1.1), and changed 64-byte pieces would
  cut it later.
- **The streaming churn (H0.6)** is the title's own. This design makes each
  reload cost one upload, and does not stop it.

## Not in this design

The page-table rewrite of descriptors (owner's permission needed), heap
descriptor sync (H3.x), recording cost (H5.x), and the picture blockers in
`docs/problems.md`.
