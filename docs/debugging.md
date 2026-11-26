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
title's data folder. After the title's own crash filter returns, its latest debug
output is added (where it saved its report, if it did). A title that stops
itself on a breakpoint (`int3`) is often at a failed check: diagnostics builds
also log, for the unhandled report, up to eight quadwords at what each register
but `rsp` points at, loaded images left out (title memory, in the log and the
console: do not copy it into the repository).

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

On a real GPU this can reset the display driver, so only one smoke run at a
time starts on a machine. Scripted keys can tick the title's legal terms on its
first-start screens, so the runner refuses them unless the data folder is
already set up (`--data-from <folder>` copies one for the run, `--data-dir
<folder>` uses it in place). `--keys ""` plays by hand; `--accept-terms` allows
a fresh folder and needs the owner's OK for that launch.

### Sharing the GPU: `queue`, and judging a run: `compare`

Sessions do not launch real-GPU runs; they queue them. Requests are small TOML
files in `E:\Causeway-tools\queue` (`--queue-dir`, or `VICTORY_QUEUE_DIR`); runs go
to `E:\Causeway-tools\runs` (`--runs-dir`, `VICTORY_RUNS_DIR`). Both stay outside
the repository.

```powershell
cargo xtask queue add --name mesh-9 --seconds 420 --env VICTORY_SHADER_LOOP_LIMIT=64 --baseline smoke-15 --requester local_... --wait
cargo xtask queue run --exe E:\GTA\game_scarlett_master.exe   # once, in the background
cargo xtask queue status
```

`add` writes `<name>.toml`: the worktree (this one by default), its commit (HEAD
by default; the runner refuses a worktree at another commit, with uncommitted
tracked changes, or that `git worktree list` does not show), `seconds`, `env`
(`VICTORY_*` only), `baseline` (a run's name, or an absolute `report.json`),
`requester`, and `data_dir`, `data_from`, `keys`. Any other field, or a switch
that is not `VICTORY_*`, refuses the request; `--accept-terms` is not one. `run`
takes the oldest request and runs exactly `cargo xtask smoke --exe <its --exe> --out
<runs>\<name> ...` in that worktree, one at a time (a runner holds `runner.lock`
open, so a second cannot start while it lives, and the file a dead one left blocks
nobody; the next runner ends what it left running; `--once` to stop when the queue
is empty). The console goes to `<runs>\<name>.console.txt`. It then writes
`<name>.done` (`exit`, `out`, `digest`, `console`, and `error` for a refused
request, `exit = -1`, and the `requester`) beside the request: wait for that file, or
pass `--wait`. There is one runner and it owns the order (oldest request first); never
start a waiter per request. While the smoke lock is held (a run by hand) the head waits;
a run that still stops at the lock goes back to the head (its request is untouched), and
after 60 such tries it is given up on in its `.done`. `status` prints what runs (and for
how long), then the queue in the order it runs, then the last finished.

```powershell
cargo xtask compare E:\Causeway-tools\runs\mesh-9 --baseline E:\Causeway-tools\runs\smoke-16 --baseline E:\Causeway-tools\runs\smoke-15
```

`compare` reads each run's `log.txt` (every world line; `report.json` beside it
for the non-flat pixel share) and prints, per metric, the median and
`[q1..q3]` of the run and of each baseline, a verdict against each (regression /
improvement / within noise), and an overall one. Speed (fps, ms, preparing, gpu,
copy-back) and drawn/dropped draws per frame (by cause, and by kind of named
reason) regress when the median is worse by more than the larger interquartile
range of the two runs (floor: 5% of the baseline, and 0.05 fps, 1 ms or 1 draw);
fewer than 3 world lines say nothing. Counts of the whole run (refused
pipelines, untranslated shaders, black frames) regress on any rise; the non-flat
share, on a drop of more than 20%; a run that never reached the world regresses.
A regression against any baseline is one (`--all` asks for every baseline, for a
noisy one that must not decide alone); an improvement counts only against every
one. It exits with an error on any regression, and when it cannot judge: the run
has fewer than 3 world lines, no baseline has 3, or the run lacks a value a
baseline has (no `report.json` for the non-flat share, no black-frame count, no
drop lines). A drop reason renamed between builds moves its draws between
`kind_*` rows: read those against the `dropped_*` rows, which are by cause.

## Testbed (no game, no title run)

`victory-testbed` checks the translation layer headlessly:

    cargo run -q -p victory-testbed -- hle list
    cargo run -q -p victory-testbed -- hle call xmem.dll XMemIsToolsMemAvailable
    cargo run -q -p victory-testbed -- alu 4a000302 --in 40,2,0,0 [--hardware]

`alu` takes GFX10 instruction words in hex. They run as one thread after a
prologue that copies the four `--in` dwords to `v1..v4`; the result is `v0`.
WARP is the default; `--hardware` runs on the real GPU (CLAUDE.md: ask the owner first). `hle call` passes up to four
integers: call only exports that need no running title.
