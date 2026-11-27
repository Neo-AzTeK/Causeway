# Debugging

Reference for diagnostics builds; the [README](../README.md) has the basics.
A release build has no debugging aids. Build them in with

```powershell
cargo build --profile diag -p victory --features diagnostics   # target\diag\victory.exe
```

| Variable | Effect |
|----------|--------|
| `VICTORY_LOG=<target>=debug,...` | debug and trace output |
| `VICTORY_STACKS=20,60` | every thread's stack at those seconds, with function names (diagnostics build); `cargo xtask stacks <run>` lists the title's threads in our code and tallies where they ran or waited |
| `VICTORY_WATCH=<rva>,...` | logs a title u32 when it changes |
| `VICTORY_DRAW_BUDGET_MS=33` | test mode: skips draws past what should fit in that many ms (small draws, compute and copies run); H in the game window draws one whole frame in full (a few more while pipelines are being made) and holds the game at it until H is pressed again |
| `VICTORY_PROFILE=<file>` | writes a CSV line of times and draw counts for each presented frame (outside the repository) |
| `VICTORY_KEYS=30:Enter,12-280/4:Enter,285-300:W` | presses keys at those seconds, every 4 s in a range, or holds them through one, focus or not |
| `VICTORY_CRASH_SAVE=<rva>:<len>,...` | saves title memory to the cache folder on a crash |
| `VICTORY_CAPTURE=<dir>` | saves presented frames (powers of two, or `VICTORY_CAPTURE_FRAMES=<list>`) |
| `VICTORY_DUMP_TARGETS=1` | with the two above, saves every render target of those frames; read them with `cargo xtask dumps <run> --frame <n>` (list, `--only <part of a name>`), or measure one dump split by another under masks (`--measure <part> --by <part> --mask <part>:<min>`) |
| `VICTORY_D3D12_DEBUG=1` | host debug layer |
| `VICTORY_D3D12_GBV=1` | with the above, GPU-based validation (bad descriptor use reported, not faulted; slow) |
| `VICTORY_D3D12_DRED=1` | on a GPU failure, the allocation hit and the commands in flight; on by default in smoke runs, it formats a note per draw (about 1.5x the draw cost): `--env VICTORY_D3D12_DRED=0` for timing runs |
| `VICTORY_IN_PLACE=1` | the GPU uses section views the runtime maps in place (`OpenExistingHeapFromAddress`, where the device has it), without host copies, refreshes or copy-backs; off by default: in a world run the title unmapped views its copies still held, Windows refused, and the title crashed (warning "refuses the unmap") |
| `VICTORY_SURVIVE_REMOVAL=1` | a device removal after a submission is logged and the run goes on without the GPU (nothing draws after it); `cargo xtask smoke` sets it |
| `VICTORY_SKIP_PIPELINES=<hash>,...` | omit draws and dispatches, including indirect ones, whose pipeline contains a listed 16-digit shader hash |
| `VICTORY_DUMP_SHADERS=<dir>` | writes each translated program's layout, resources and decoded instructions to `<dir>\<hash>.txt` (only the hashes in `VICTORY_DUMP_HASHES=<h>,...` when set); derived from the title: keep `<dir>` outside the repository |
| `VICTORY_PROBE_WINDOW=<hash>:<user data index>[:big]` | in listed frames, for dispatches of that program, logs the words of each table buffer over 64 bytes (title memory and host copy) at the byte offset that user data holds; with `:big` in every frame, only where the loop counts at +4 / +0x374 exceed 4096 or the window lies past the view |
| `VICTORY_PROBE_COUNT=<hash>[,<hash>…]` | for every dispatch of those programs, logs (`count …`) the group count, root constants and the first 16 bytes of each root view: title memory when recorded, title memory and host copy after the list ran. For programs that loop over a count read from memory (`docs/frame-cost.md` H6.2) |
| `VICTORY_PROBE_CLASSES=1` | keeps the console bits the title allocated each range with (virtual protection above the host's, physical-page flags, `XMemAlloc` attributes) and every 60 frames logs (`classes, …`) tracked and CPU-written 64 KiB pages per frame by them (`docs/frame-cost.md` H0.2); every 100 frames also `writers …`: MB of title memory tracked or written in the window, by class and by who wrote it (w1 the title's code, w2 the kernel, w4 GPU work, w0 nobody; H0.1), and `loads …`: per load path (DirectStorage, `ReadFile`, `fread`) calls, bytes asked and got, pages, loads repeated to the same destination or of the same data (H0.4); and `memory asked`: the title's `XMemGetWorkingSetStatistics` calls and our answers, one line per 10 s (H0.5) |
| `VICTORY_GPU_TIMING=1` | with the above, each list of a submission run and timed on its own; slow lists logged with their slowest commands |
| `VICTORY_D3D12_WARP=1` | host device on the software adapter (slow; cannot reset the driver) |
| `VICTORY_SHADER_LOOP_LIMIT=<n>` | translated shader loops of every stage stop after `n` iterations per lane (own shader cache entries) instead of the stage limits: pixel 128, vertex and mesh 256, compute 4,096, lower in deeply nested loops; 2^20 itself means the stage limits; if a lower `n` makes world frames much faster, loops hitting the limit are what makes them slow |
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
`--log <file>` judges an existing log instead.
`--world-lines <n>` ends the run once it has `n` steady world lines (one about
every 5 s): world lines logged 90 s or more after the first, the warm-up. The log
is read every 2 s and the title is ended one look after the `n`th, as at the end
of the time; `--seconds` stays the cap, and a run that ends short of `n` fails the
"steady world lines" area, which says how many it had.

A run measured on a busy machine reads slower and can lose draws to pipelines
still being made. So after its build, a run waits for a quiet machine before it
starts the title: no compiler or linker running (`rustc`, `clippy-driver`, `link`,
`rust-lld`, `cl`), under 30% of all cores, at least 12 GB of commit free and at
least 4 GB of RAM free (`--quiet-commit-gb`, `--quiet-ram-gb`; 0 asks for none;
the title holds ~32 GB private), all for 20 s, for 600 s at most
(`--quiet-wait`; 0 starts at once; `log.txt` is made when the title starts). A
machine short of memory is *starved*: the title boots, translates and loads at
a crawl and may never reach the world. The report reads that from the log (the
pipeline library load over 1000 ms against 30 ms, cached shader translations
that take 20 times their CPU time on the clock, or the story000 pack opened over
120 s after boot) and then says **STARVED machine, rerun on a quiet machine**:
in "world reached" when there is no world, and first in the digest's "Next". Such
a run is run again, not debugged. While
the title plays it records in `load.txt`, every 2 s, the CPU in all, the CPU of
processes other than the title's (the system's busy time less the title's job),
and the compilers running. The "machine load" area gives the wait, the load over
the lines measured (the steady ones; all world lines if fewer than 3 are), and how
many looks a build overlapped, in and outside them. It warns **MACHINE BUSY** when
over a quarter of those looks have other processes above 20% of all cores. These
two numbers are estimates: tune them on the recorded looks. It also writes `digest.md` (at most 60
lines: run header with the switches the game saw, delta against `--baseline <report.json>`,
frame cost, draws lost by cause and pipeline, refusals ranked by draws lost, a contact
sheet and the top 3 next items). `--env NAME=VALUE` passes a `VICTORY_*` switch to the
game (the runner removes all others) and the digest warns when the log does not show it.
It exits with an error when an
area failed; what a log does not show is "unknown".

On a real GPU this can reset the display driver, so only one smoke run at a
time starts on a machine, and none while another `victory.exe`, the testbed or
any cargo test binary runs (it may be on the GPU; `victory census` and
`victory translate` without `--pipelines` are not). A run or `cargo xtask alone --
<command>` (which holds the same lock around GPU tests) that finds the lock held or
a GPU process running waits, not refuses: it logs `waiting (N s so far): <holder>`
at once and then every 60 s, and starts when both are free. It never ends or breaks
the other. After 60 minutes it refuses, naming the holder, and a queued request
records that in its `.done`. `--quiet` also refuses to start
beside a compiler, a linker or such a scan: use it when timings matter. Each launched run
is sampled every 5 s; `run.txt` and the digest header say `machine: quiet`,
`NOT QUIET: ...` or `unknown` (by process name: other load is not seen).
`--problem <id>` writes the run into that problem's log
(`docs/problem-sessions.md`). Scripted keys can tick the title's legal terms on its
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
cargo run -q --target-dir target\queue-runner -p xtask -- queue run --exe E:\GTA\game_scarlett_master.exe   # once, in the background
cargo xtask queue status
```

`add` writes `<name>.toml`: the worktree (this one by default), its commit (HEAD
by default; the runner refuses a worktree at another commit, with uncommitted
tracked changes, or that `git worktree list` does not show), `seconds`, `world_lines`
(`--world-lines`; the worktree's own `xtask` must know it), `env`
(`VICTORY_*` only), `baseline` (a run's name, or an absolute `report.json`),
`requester`, and `data_dir`, `data_from`, `keys`. A smoke run on the default keys
(no `--keys`, no `--accept-terms`) also stops early when
no world line comes 150 s after the story000 pack opened (the log line
`dstorage: file opened ... story000`; a starved boot gets those 150 s): smoke writes
`rerun.txt` into the run's folder and the `.done` gets `error = "rerun: ..."`,
unless the log has a crash or a removed device. Run it again if the digest says
STARVED; otherwise read the log (a hang while loading looks the same). A baseline named by run must
have a report, or be queued or running (`add` refuses it otherwise); if it still
has none when this run's turn comes, the run goes on and its `.done` has a
`warning`. Any other field, or a switch
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
a run that still finds the lock held when it starts goes back to the head (its request is
untouched), and after 150 such tries (five minutes) it is given up on in its `.done`. A
run that starts waits for the lock and for GPU processes itself (see above), so a
request is refused only after that wait's 60 minutes, with the holder in its `.done`.
`status` prints what runs (and for how long), then the queue in the order it runs, then
the last finished.

The runner gets its own target directory (the command above), or a request for the
worktree it was built from could not rebuild `xtask` while it runs. A compile
beside the booting title starves it (a black screen of minutes, or no world), so while
a run boots the runner builds nothing. Once the title has reached its world and
settled (10 s after the first world line) the runner builds the next request's
worktree (`xtask`, then the `diag` title build; output in `<runs>\<name>.build.txt`),
at idle priority on a quarter of the cores; it logs `runner: build ahead held while
<name> boots` once before. No build reaches the lines measured: 75 s after the first
world line, 15 s before the first steady one, a build still going is ended, and it is
taken up again after the run (cargo keeps the crates it finished). The runner waits for
it before it starts the next run. It skips a request for the playing run's worktree. The
queue, the smoke lock and the run are looked at every 2 s.

Before any cargo build, test or clippy, a session runs `cargo xtask queue wait-idle`
(`--timeout <s>`, 900 by default): it returns at once when no run is booting, else it
waits. A run boots from its `.running` file (so through its wait for a quiet machine
and its own build) until 10 s after its first world line. A run stuck without a
world no longer counts: 10 minutes after its log was made, or 30 minutes after it
started when it has no log. It protects the boot only: a compile after it can still
disturb the lines measured (the digest then says MACHINE BUSY), so prefer
`-j 2` while a run plays. After the timeout it exits with an error naming the run:
do not compile then. Already-started compiles are not stopped, and a smoke run
started by hand is not seen (it has no `.running`).

```powershell
cargo xtask compare E:\Causeway-tools\runs\mesh-9 --baseline E:\Causeway-tools\runs\smoke-16 --baseline E:\Causeway-tools\runs\smoke-15
```

`compare` reads each run's `log.txt` (`report.json` beside it for the non-flat
pixel share, `load.txt` for the load) and judges the **steady** world lines only,
on both sides (90 s or more after the first world line); the warm-up's frame rates
are one more row, for information. It prints, per metric, the median and
`[q1..q3]` of the run and of each baseline, a verdict against each (regression /
improvement / within noise), and an overall one. Speed (fps, ms, preparing, gpu,
copy-back) and drawn/dropped draws per frame (by cause, and by kind of named
reason) regress when the median is worse by more than the larger interquartile
range of the two runs (floor: 5% of the baseline, and 0.05 fps, 1 ms or 1 draw);
fewer than 3 steady lines say nothing. Counts of the whole run (refused
pipelines, untranslated shaders, black frames) regress on any rise; the non-flat
share, on a drop of more than 20%; a run that never reached the world regresses.
A regression against any baseline is one (`--all` asks for every baseline, for a
noisy one that must not decide alone); an improvement counts only against every
one. It exits with an error on any regression, and when it cannot judge: the run
has fewer than 3 steady lines, no baseline has 3, or the run lacks a value a
baseline has (no `report.json` for the non-flat share, no black-frame count, no
drop lines). When the machine was busy during the run, or during every baseline, the
verdict says **MACHINE BUSY** with what was measured after it, and the command
fails: run again (a run that looks no worse then proves nothing, since load widens
the spread). A busy baseline beside a quiet one is shown and not counted. A run
without `load.txt` is "load not recorded", not busy. A drop reason renamed between builds moves its draws between
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
