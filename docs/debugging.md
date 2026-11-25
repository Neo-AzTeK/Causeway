# Debugging

Reference for diagnostics builds; the [README](../README.md) has the basics.
A release build has no debugging aids. Build them in with

```powershell
cargo build --profile diag -p victory --features diagnostics   # target\diag\victory.exe
```

| Variable | Effect |
|----------|--------|
| `VICTORY_LOG=<target>=debug,...` | debug and trace output |
| `VICTORY_STACKS=20,60` | every thread's stack at those seconds |
| `VICTORY_WATCH=<rva>,...` | logs a title u32 when it changes |
| `VICTORY_KEYS=30:Enter,12-280/4:Enter,285-300:W` | presses keys at those seconds, every 4 s in a range, or holds them through one, focus or not |
| `VICTORY_CRASH_SAVE=<rva>:<len>,...` | saves title memory to the cache folder on a crash |
| `VICTORY_CAPTURE=<dir>` | saves presented frames (powers of two, or `VICTORY_CAPTURE_FRAMES=<list>`) |
| `VICTORY_DUMP_TARGETS=1` | with the two above, saves every render target of those frames |
| `VICTORY_D3D12_DEBUG=1` | host debug layer |
| `VICTORY_D3D12_GBV=1` | with the above, GPU-based validation (bad descriptor use reported, not faulted; slow) |
| `VICTORY_D3D12_DRED=1` | on a GPU failure, the allocation hit and the commands in flight |
| `VICTORY_SURVIVE_REMOVAL=1` | a device removal after a submission is logged and the run goes on without the GPU (nothing draws after it); `cargo xtask smoke` sets it |
| `VICTORY_SKIP_PIPELINES=<hash>,...` | omit draws and dispatches, including indirect ones, whose pipeline contains a listed 16-digit shader hash |
| `VICTORY_DUMP_SHADERS=<dir>` | writes each translated program's layout, resources and decoded instructions to `<dir>\<hash>.txt` (only the hashes in `VICTORY_DUMP_HASHES=<h>,...` when set); derived from the title: keep `<dir>` outside the repository |
| `VICTORY_GPU_TIMING=1` | with the above, each list of a submission run and timed on its own; slow lists logged with their slowest commands |
| `VICTORY_D3D12_WARP=1` | host device on the software adapter (slow; cannot reset the driver) |
| `VICTORY_SHADER_LOOP_LIMIT=<n>` | translated shader loops stop after `n` iterations per lane instead of 2^20 (own shader cache entries); if a lower `n` makes world frames much faster, loops hitting the limit are what makes them slow |
| `VICTORY_AUDIO_CAPTURE=<file.wav>` | records what the title plays (our process only) |
| `VICTORY_XMA_DUMP=<dir>` | saves the first 32 XMA voices' context and input |

Every build appends crashes and host GPU failures to `crashes.log` in the
title's data folder.

## Smoke runs

`cargo xtask smoke --exe <title exe> --out <folder outside the repository>`
runs a title unattended with this build (300 s, `--seconds`), skips its menus
with `VICTORY_KEYS` (`--keys` replaces them; they do nothing while an XInput pad is
connected), and judges each graphics area in `report.txt` and `report.json`.
`--log <file>` judges an existing log instead. It also writes `digest.md` (at most 60
lines: run header with the switches the game saw, delta against `--baseline <report.json>`,
frame cost, draws lost by cause and pipeline, refusals ranked by draws lost, a contact
sheet and the top 3 next items). `--env NAME=VALUE` passes a `VICTORY_*` switch to the
game (the runner removes all others) and the digest warns when the log does not show it.
It exits with an error when an
area failed; what a log does not show is "unknown".

On a real GPU this can reset the display driver. The default keys press
through the title's first-start screens and can tick its legal terms: where
that is not wanted, pass a `--data-dir` already set up, and `--keys` that
leave time to look at each screen.

## Testbed (no game, no title run)

`victory-testbed` checks the translation layer headlessly:

    cargo run -q -p victory-testbed -- hle list
    cargo run -q -p victory-testbed -- hle call xmem.dll XMemIsToolsMemAvailable
    cargo run -q -p victory-testbed -- alu 4a000302 --in 40,2,0,0 [--hardware]

`alu` takes GFX10 instruction words in hex. They run as one thread after a
prologue that copies the four `--in` dwords to `v1..v4`; the result is `v0`.
WARP is the default; `--hardware` runs on the real GPU (CLAUDE.md: ask the owner first). `hle call` passes up to four
integers: call only exports that need no running title.
