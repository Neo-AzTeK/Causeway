# Title memory on a PC: design

Agreed with the owner, 2026-11-27. Replaces the earlier proposal (host
copies moved to video memory). Nothing below is built yet except where it
says so. It rests on the measurements in `docs/frame-cost.md` (H0.1-H0.6).
Live work stays in `docs/problems.md`.

## The problem

The console gives a title about 13.5 GB that its CPU and GPU share. We run
the title's memory as host memory in our process, and add our own on top:
buffer copies the PC GPU reads, detiled textures, snapshots, pipeline
caches. Our process holds about 32 GB of private memory, and video memory
is full. The title's texture slots read empty because the textures they
name do not fit.

What the memory we track holds, by who writes it (H0.1, two minutes in one
world scene, 64 KiB pages):

| written by | share | likely contents |
|------------|-------|-----------------|
| disk loads (DirectStorage, file reads) | 52% | textures, meshes, other assets loaded for the GPU |
| nobody, for minutes | 44% | GPU-only memory (render targets, GPU outputs) and loaded data at rest |
| the title's CPU code | 3% | constants, command data, state the GPU reads |
| our write-backs of GPU results | 1% | |

So about 96% of what the GPU uses is never written by the title's CPU. That
memory needs to exist only on the GPU.

## Principles

1. **The console's budget is the target.** The title's own memory counts
   once. Everything we add has a stated budget and a reason.
2. **Each byte lives where its users are.** Data only the GPU uses lives
   only in video memory. Data the CPU writes lives in RAM, and the GPU gets
   it in place or as an upload per change.
3. **Assets come from disk, not from RAM.** Loaded data is streamed from
   its file to video memory when the GPU first uses it. The title's pages
   for it stay empty.
4. **The title never sees a difference.** Its pages that we leave empty are
   guarded; a CPU access fills them first (from disk, or from the GPU).
5. **Lazy, one frame late at most.** What shaders pick at run time is made
   when seen used and appears a frame later; what was not used for a while
   is evicted. PC games stream textures the same way.
6. **Every card works.** Placement follows `Caps` and the live budget
   (`vram.rs`). What does not fit in video memory goes to system memory:
   slower, never missing.
7. **Caches are bounded.** No cache grows with play time (the pipeline
   library reached 2.2 GB and was copied whole to save it).

## The design

### Disk loads keep their source

Our DirectStorage (`victory-hle` `dstorage`) knows, for each request, the
file, offset, size, compression and destination address. It records that
as the destination's **source**, per page. A load decodes into the
destination as today until step 2 lands; after it, a load into memory the
GPU will use writes nothing and only records the source.

### The GPU's copy is made from the source

When a texture or buffer is made over memory with a source, the bytes come
from the file: read, decompressed (BCPack, zlib, as `dstorage` does now),
detiled for textures, and uploaded to video memory. That copy is the only
one. The title's pages there stay unbacked and guarded.

### GPU-only memory lives only on the GPU

Memory the title allocates for its GPU (physical pages, GPU-flagged pools)
and that no CPU code writes, such as render targets and GPU outputs, has
its home in video memory. Its title pages stay unbacked and guarded.

### A CPU access fills the page first

Guarded pages fault on access (the guard machinery in `watch.rs`). The
handler fills the page and lets the access go on:

- a page with a source: read from the file again;
- a page the GPU wrote: copied back from video memory (a readback of that
  page, waited on);
- a page never written: zeros.

A CPU write afterwards makes the page CPU-written: it then stays in RAM, and
the GPU's copy takes the change as an upload. Pages that fault often are
kept backed (a share of frames, not a fixed time).

### Textures are made when used, evicted when not

Shaders pick most textures by index at run time, so the CPU cannot know
beforehand which a draw reads. Shaders mark what they read (read marks,
built, today opt-in: `VICTORY_READ_MARKS=1`; they must get cheaper, such as
one mark per draw and slot, not per pixel). A slot read empty is made for
the next frame from its source; a texture no shader read for a while is
evicted and its slot made empty again. Over budget, textures go to system
memory instead of being left out.

### CPU-written memory stays in RAM

The ~3% the title's CPU writes keeps today's path: the GPU uses it in place
where the device opens its memory (sections, built), else a copy refreshed
per change.

## Steps, each with what it must show

| step | change | must show |
|------|--------|-----------|
| 0 | measure: bytes per class at one moment, process memory by owner, whether the title's CPU reads loaded data, and bytes the GPU uses per frame | the numbers below are known |
| 1 | record sources of disk loads, per page | every loaded page has a source; a re-read from it equals what was loaded |
| 2 | textures from sources, title pages left unbacked and guarded | the picture is not worse; process memory falls by the texture bytes |
| 3 | buffers from sources the same way | as step 2, for buffers |
| 4 | GPU-only memory in video memory only, filled back on CPU access | copy-back and snapshots fall; no title read sees stale bytes |
| 5 | textures made when read, evicted when not; system memory when over budget | no slot reads empty for more than a frame; video memory under budget |
| 6 | prepare the next submission while the GPU runs (H7.1) | frame time below preparing + GPU |

## Built so far

- **Physical-page sections used in place** (`inplace.rs`, `xmem.rs`): the
  device opens committed sections under 4 GiB only (probe
  `sections_the_device_opens`), so physical pages are committed sections of
  1 GiB at most. One world run: median frame 500 ms against ~880-980.
  **Conflicts with principle 1:** the sections' 6.1 GB are committed up
  front, and a boot met this machine's commit limit. To decide after step 0:
  keep them for CPU-written memory only, or reserved by default.
- **Pipeline library** saved less often when big, and never by aborting.
- **Read marks** in shaders, opt-in.

## Unknowns that decide the order

- How much of the 52% loaded memory is textures, and how much buffers.
- Whether the title's CPU reads loaded data (headers, CPU-side copies of
  meshes): if often, guarding it costs faults.
- Bytes the GPU uses in one frame: whether one frame fits in this card's
  ~7.2 GB budget, and how much falls to system memory.
- How much of the 6.1 GB of physical pages the title touches.

## Risks

- **Aliases.** The title maps one physical page at several addresses; a
  fill must reach every view (`xmem::aliases` knows them).
- **Data changed after loading.** The title may patch loaded data in place;
  the guard sees the write, and the page becomes CPU-written.
- **Disk throughput.** Re-reads from files cost time; a texture is read once
  per making, and eviction must not make the same texture again every frame.
- **Fault cost.** Each fill stops a title thread; pages that fault often
  stay backed.
