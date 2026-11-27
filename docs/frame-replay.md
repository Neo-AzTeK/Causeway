# Frame replay

Plan for a tool that records one frame of a title run once and replays it
without the title. Status: **design, nothing built**. Replaces "start the
title, reach the scene by hand, read the log" as the way to test a change on
one frame.

## Why

- A title run takes minutes, reaching the scene is manual, and no two runs
  behave the same, so one change needs several runs before we can judge it.
- Logs only show counts. They don't show which step of a frame first goes
  wrong, so guesses from them have been wrong before.
- A replay gives the same inputs every time. We can rerun it after each
  change and dump targets after any draw to find the first wrong one.

What a replay cannot tell: what the console would show (we still need a
reference, by eye or from a capture we trust); problems that depend on the
running title (CPU reads of GPU results, timing, streaming, the pipeline
compile backlog); and other frames.

## Where to record

Host D3D12 list calls are made as the title calls the D3D12.X entry points
(`list.rs`, decision 0007). There is no list of commands of our own to
save, so a replay records at the **title's call boundary**: every D3D12.X
call the title makes (device, list, queue, allocator, fence, heap and
resource tables, `object.rs`), with the structures its arguments point to,
plus the title memory the GPU reads. Replay calls the same entry points
with the same arguments, so command translation, shader translation,
shadows, pipelines and copy-back all run again. A change to any of them shows
up in the replay.

The inline fast paths can't bypass this: `List::put` is zero, so every inline
check fails and the title calls us.

## What a recording holds

A folder outside the repository (rule #1), by default
`E:\Causeway-tools\captures\<name>\`:

| Part | Recorded when | Content |
|------|---------------|---------|
| `objects` | from boot, always (cheap) | each creation call through the device table: resources (description, heap, address), heaps, pipelines (stream bytes), root and command signatures, descriptor heaps, query heaps, fences; releases |
| `calls` | the recorded frame | every list, allocator, queue and fence call in order, with thread, sequence number and copied arguments; object pointers as recorded ids |
| `memory` | each submission of the frame | the title memory the submission reads, as content-addressed chunks: the ranges its `Deferred` steps name (`Refresh`, `Fill`, `Texture`, `Heap`, `Memory`), upload memory, descriptor heap memory, indirect arguments |
| `textures` | frame start | host texture contents the frame reads before writing (history buffers, previous targets), from `capture::read_back` |
| `meta` | always | build commit, adapter, caps, switches, frame number |

Memory is snapshotted at **each submission**, not once per frame, because
title threads write it while the frame is built. Equal chunks are stored once
(hash), so repeats cost nothing.

## Replay

`victory replay <capture> [--dump-after <draw>] [--skip <hash>,...] [--out <dir>]`:

1. Make the host device with the recorded caps (check the adapter matches;
   a mismatch is a warning).
2. Reserve and commit the recorded title ranges **at the same addresses**,
   because a resource's GPU address is its CPU address. A range the replay
   process already uses is a hard error that names it.
3. Run the `objects` calls through the same entry points and map recorded
   ids to new objects.
4. Restore the `textures`. Then, for each recorded submission: write its
   `memory` chunks, run its calls, and execute.
5. Write the presented image, every target (`capture::dump_targets`), the
   usual drawn, dropped and refused counts, and GPU time per submission.
   Pipelines are made before the replay starts, so the compile backlog does
   not affect timing.

`--dump-after <n>` saves the targets after draw or dispatch `n`. Bisecting
over `n` finds the first draw whose output is wrong.

## Phases

Each phase ends in something we can test on its own.

1. **Recorder, objects and calls** (diagnostics builds only,
   `VICTORY_RECORD=<dir>`, `VICTORY_RECORD_FRAME=<n>` or the
   `VICTORY_CAPTURE_WORLD` offsets). Done when a recording of a world frame
   lists every call, and a test checks that each table entry has a recorder
   (no call goes unrecorded).
2. **Recorder, memory and textures.** Done when the recorded chunks cover
   every range the submissions' steps read (the recorder checks this and
   reports what it missed).
3. **Replay.** Done when replaying a world recording twice gives targets
   that match to the byte (determinism), and the presented image matches the
   live run's capture of the same frame by eye.
4. **Stepping and bisecting** (`--dump-after`, `--skip`).
5. **In the merge gate.** A short list of blessed recordings on this machine.
   A change must keep their targets, or explain the difference, and must not
   slow their GPU time beyond the spread. This adds to the smoke run and does
   not replace it.

Risks to check early, in phase 1: address collisions in the replay process
(the title's ranges against our own images and heap); recording size (one
world frame's textures may be hundreds of MB, which is acceptable outside the
repository); descriptors the title writes straight into heap memory (covered
by the heap memory snapshot; descriptor work belongs to another AI, so the
recorder only reads heaps).

## Cheaper step that works today

PIX or RenderDoc on the **host** side can capture `victory.exe`'s own D3D12
frame and replay it with per-draw stepping and GPU timings. It doesn't rerun
our translation, so it can't test a change, but it can show which draw first
writes a wrong target now. Not yet tried on this title: whether it attaches
cleanly is unverified.

## A project to compare: Xenia

Xenia (Xbox 360 emulator, D3D12 back end) solved the same four problems.
The points below are from memory of its source and have not been checked
against the current tree. Check them before relying on them.

| Question | Xenia | Us |
|----------|-------|----|
| sharing memory between CPU and GPU | one GPU buffer mirrors guest memory; pages are watched by memory protection, and only dirty pages are uploaded before use; GPU writes are tracked so they aren't overwritten | per-resource host copies (`shadow.rs`, `watch.rs`), refreshed per submission, with copy-back of changed bytes |
| keeping CPU and GPU busy | submissions are fenced and the CPU waits only when it needs a result or runs out of frames in flight | `host.rs` waits for every submission |
| translating shaders and feeding them data | translated at first draw, keyed by shader and the state it depends on; shared memory bound as one raw buffer | translated at pipeline creation; descriptors traced to root parameters (`resource.rs` in `victory-shader`) |
| testing changes | a GPU trace writer (command stream and memory reads) and a trace player/viewer and dumper that replay a frame without the game, used to compare output after a change | smoke runs of the title and `compare` |

The last row is the model for this plan. The second row is the next speed
question once the frame is correct.
