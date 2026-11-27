# Debugging

Reference for diagnostics builds; [README](../README.md) covers basics. Release builds omit debugging aids; compile with:

```powershell
cargo build --profile diag -p victory --features diagnostics   # target\diag\victory.exe
```

| Variable | Effect |
| --- | --- |
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
| `VICTORY_IN_PLACE=0` or `=1` | by default the GPU uses title memory mapped from physical-page sections in place: each section is opened whole (`OpenExistingHeapFromFileMapping`) and a buffer is placed over each run of consecutive pages, without host copies, refreshes, snapshots or copy-backs (log "section opened for the GPU in place"). `=0` copies all of it as before. `=1` also opens other section views by address (`OpenExistingHeapFromAddress`): in a world run the title then unmapped views its copies still held, Windows refused, and the title crashed (warning "refuses the unmap") |
| `VICTORY_LAZY_LOADS=0` | by default DirectStorage file loads leave whole 64 KiB pages of their destination without bytes and without access; our reads of title memory take those bytes from the file (compressed loads decoded once, 96 MiB cache), and a title access or any write fills the page first (`crate::lazy`, `watch`). `=0` writes every load as before. The line "memory: private ..." every 5 s shows bytes left so, filled, and read from files |
| `VICTORY_SURVIVE_REMOVAL=1` | a device removal after a submission is logged and the run goes on without the GPU (nothing draws after it); `cargo xtask smoke` sets it |
| `VICTORY_SKIP_PIPELINES=<hash>,...` | omit draws and dispatches, including indirect ones, whose pipeline contains a listed 16-digit shader hash |
| `VICTORY_DUMP_SHADERS=<dir>` | writes each translated program's layout, resources and decoded instructions to `<dir>\<hash>.txt` (only the hashes in `VICTORY_DUMP_HASHES=<h>,...` when set); derived from the title: keep `<dir>` outside the repository |
| `VICTORY_PROBE_WINDOW=<hash>:<user data index>[:big]` | in listed frames, for dispatches of that program, logs the words of each table buffer over 64 bytes (title memory and host copy) at the byte offset that user data holds; with `:big` in every frame, only where the loop counts at +4 / +0x374 exceed 4096 or the window lies past the view |
| `VICTORY_PROBE_COUNT=<hash>[,<hash>…]` | for every dispatch of those programs, logs (`count …`) the group count, root constants and the first 16 bytes of each root view: title memory when recorded, title memory and host copy after the list ran. For programs that loop over a count read from memory (`docs/frame-cost.md` H6.2) |
| `VICTORY_PROBE_CLASSES=1` | keeps the console bits the title allocated each range with (virtual protection above the host's, physical-page flags, `XMemAlloc` attributes) and every 60 frames logs (`classes, …`) tracked and CPU-written 64 KiB pages per frame by them (`docs/frame-cost.md` H0.2); every 100 frames also `writers …`: MB of title memory tracked or written in the window, by class and by who wrote it (w1 the title's code, w2 the kernel, w4 GPU work, w0 nobody; H0.1), and `loads …`: per load path (DirectStorage, `ReadFile`, `fread`) calls, bytes asked and got, pages, loads repeated to the same destination or of the same data (H0.4); and `memory asked`: the title's `XMemGetWorkingSetStatistics` calls and our answers, one line per 10 s (H0.5) |
| `VICTORY_PROBE_VRAM_COPIES=1` | host copies of title buffers (not of single views) go in video memory while the budget has room; refreshes still write the system copy, and one list per submission copies what they wrote to video memory. GPU writes are not copied back, so the picture may be wrong: it measures GPU time only (`docs/frame-cost.md` H6.1). Logs `video-memory copies (probe): …` (copies made, MB per frame copied) |
| `VICTORY_PROBE_LEVELS=1` | with each host textures line: of the resident host textures made for title memory, the host bytes in mip levels the title has not mapped now, and how many hold no level (`levels.rs`; problem `placed-texture-slots`). Also, with the texture queue line, the distinct textures its slots wait for and the host bytes making them would add (`texture queue (probe)`). Diagnostics builds always log `levels not mapped (probe), at made / at first upload` |
| `VICTORY_GPU_TIMING=1` | with the above, each list of a submission run and timed on its own; slow lists logged with their slowest commands |
| `VICTORY_D3D12_WARP=1` | host device on the software adapter (slow; cannot reset the driver) |
| `VICTORY_READ_MARKS=1` | translated shaders mark the heap slots they read by index (own shader cache entries), and the texture queue makes read slots first; "read marks since the last queue line" counts them. Off by default: they cost GPU work on every heap read |
| `VICTORY_SHADER_LOOP_LIMIT=<n>` | translated shader loops of every stage stop after `n` iterations per lane (own shader cache entries) instead of the stage limits: pixel 128, vertex and mesh 256, compute 4,096, lower in deeply nested loops; 2^20 itself means the stage limits; if a lower `n` makes world frames much faster, loops hitting the limit are what makes them slow |
| `VICTORY_AUDIO_CAPTURE=<file.wav>` | records what the title plays (our process only) |
| `VICTORY_XMA_DUMP=<dir>` | saves the first 32 XMA voices' context and input |

Crashes and host GPU failures append to `crashes.log` in the title's data folder after the title's crash filter returns, followed by latest debug output and report path if saved. An access violation's report names the reservation an uncommitted address lies in and the faulting thread's stack range. On `int3` breakpoints and access violations in diagnostics builds, unhandled reports also log up to eight quadwords at each register target except `rsp` (excluding loaded images; title memory in log/console—do not copy into repo).

## Smoke runs

```powershell
cargo xtask smoke --exe <title exe> --out <folder outside the repository>
```

Runs a title unattended (`--seconds`, default `300`), drives menus via `VICTORY_KEYS` (`--keys` overrides; ineffective with XInput connected), and evaluates graphics areas in `report.txt` and `report.json` (`--log <file>` evaluates an existing log). `--world-lines <n>` stops one poll after reaching `n` **steady world lines** (logged every ~5 s, >= 90 s after the first/warm-up line), capped by `--seconds`; fewer than `n` fails "steady world lines".

To avoid slow reads and pipeline-compile draw losses, post-build runs wait up to `--quiet-wait` (default `600` s; `0` starts immediately) for 20 s of quiet state: no active compilers/linkers (`rustc`, `clippy-driver`, `link`, `rust-lld`, `cl`), total CPU < 30% of all cores, free commit >= `--quiet-commit-gb` (default `12` GB), and free RAM >= `--quiet-ram-gb` (default `4` GB; `0` disables either; title uses ~32 GB private). `log.txt` is created at title start. Memory-starved runs (pipeline library load > 1000 ms vs. ~30 ms, cached shader translation wall time > 20x CPU time, or `story000` pack opened > 120 s post-boot) report **STARVED machine, rerun on a quiet machine** in "world reached" (if no world) and first in digest "Next"—rerun rather than debug. `load.txt` samples total CPU, non-title CPU (system busy minus title job), and running compilers every 2 s. "machine load" reports wait time, load across measured lines (steady lines, or all world lines if < 3), and build overlaps in/outside them, warning **MACHINE BUSY** when > 25% of those looks have other processes > 20% of all cores (tune thresholds against recorded looks). Also writes `digest.md` (<= 60 lines: header with game-seen switches, `--baseline <report.json>` delta, frame cost, draws lost by cause/pipeline, refusals ranked by draws lost, contact sheet, top 3 next items). `--env NAME=VALUE` forwards only `VICTORY_*` variables (runner clears all others) and warns in the digest if unlogged. Exits non-zero on failed areas; missing log data is `"unknown"`.

Only one smoke run starts at a time, and none while another `victory.exe`, testbed, or `cargo test` binary runs (`victory census` and `victory translate` without `--pipelines` excluded). `cargo xtask smoke` and `cargo xtask alone -- <command>` (GPU test lock) wait rather than fail when locked or GPU-busy, logging `waiting (N s so far): <holder>` immediately and every 60 s without killing holders, refusing after 60 minutes with holder recorded in `.done`. `--quiet` also waits on compilers, linkers, or GPU scans for timing accuracy. Active runs are sampled every 5 s; `run.txt` and digest header log `machine: quiet`, `NOT QUIET: ...`, or `unknown` (by process name only). `--problem <id>` appends to `docs/problem-sessions.md`. Scripted keys can accept first-run legal terms, so `--data-from <folder>` (copies) or `--data-dir <folder>` (in-place) is required unless `--keys ""` (manual) or `--accept-terms` (requires owner approval).

### Sharing the GPU: `queue`, and judging a run: `compare`

Sessions queue real-GPU runs instead of launching them directly. Queue TOMLs live in `E:\Causeway-tools\queue` (`--queue-dir` / `VICTORY_QUEUE_DIR`) and outputs in `E:\Causeway-tools\runs` (`--runs-dir` / `VICTORY_RUNS_DIR`), both outside the repository.

```powershell
cargo xtask queue add --name mesh-9 --seconds 420 --env VICTORY_SHADER_LOOP_LIMIT=64 --baseline smoke-15 --requester local_... --wait
cargo run -q --target-dir target\queue-runner -p xtask -- queue run --exe E:\GTA\game_scarlett_master.exe   # once, in the background
cargo xtask queue status
```

`add` records `<name>.toml`: worktree (default current), commit (default `HEAD`; runner rejects non-`HEAD`, dirty tracked files, or worktrees outside `git worktree list`), `seconds`, `world_lines` (`--world-lines`, known to worktree `xtask`), `env` (`VICTORY_*` only; `--accept-terms` rejected), `baseline` (run name or absolute `report.json`), `requester`, `data_dir`, `data_from`, and `keys`. Default-key runs (no `--keys`, no `--accept-terms`) stop early if no world line appears within 150 s of `dstorage: file opened ... story000`, writing `rerun.txt` and setting `.done` `error = "rerun: ..."` unless a crash or device removal occurred (rerun if STARVED; otherwise inspect log for hang). Named baselines must exist, be queued, or be running at `add` time; if still missing when run, execution proceeds with a `.done` `warning`. Unknown fields or non-`VICTORY_*` switches are rejected. `run` executes the oldest request via `cargo xtask smoke --exe <its --exe> --out <runs>\<name> ...` in its worktree sequentially (holds `runner.lock` open; stale locks do not block, dead runners' children are terminated by next runner, `--once` exits when empty), writing console output to `<runs>\<name>.console.txt` and `<name>.done` (`exit`, `out`, `digest`, `console`, `requester`, plus `error` with `exit = -1` on refusal). Never spawn per-request waiters. If the smoke lock is held at start, the head requeues untouched and gives up in `.done` after 150 retries (5 minutes); lock/GPU contention during a started run follows the 60-minute limit. `status` lists active runs (with elapsed time), queued order, and most recent completion.

Use a separate `--target-dir` for the runner so rebuilding `xtask` for a request does not collide with the running runner. The runner does not build while a run boots (prevents multi-minute black screens or no world). Once settled (10 s after first world line), it prebuilds the next eligible worktree (`xtask` then `diag` title to `<runs>\<name>.build.txt`) at idle priority on one-quarter of cores, logging `runner: build ahead held while <name> boots` once beforehand; any still-running build is stopped 75 s after the first world line (15 s before steady lines) and resumed later (completed crates stay cached), awaited before starting the next run, and skipped if matching the active run's worktree. Queue, lock, and run state poll every 2 s.

Before any `cargo build`, `test`, or `clippy`, sessions run `cargo xtask queue wait-idle` (`--timeout <s>`, default `900`). It returns immediately unless a run is booting (from `.running` creation until 10 s after first world line; stuck runs without a world expire 10 minutes after log creation or 30 minutes after start if no log). Post-boot builds can still cause **MACHINE BUSY**, so prefer `-j 2` during active runs. On timeout, it exits with an error naming the run—do not compile. Pre-existing compiles are not stopped, and manual smoke runs lack `.running`.

```powershell
cargo xtask compare E:\Causeway-tools\runs\mesh-9 --baseline E:\Causeway-tools\runs\smoke-16 --baseline E:\Causeway-tools\runs\smoke-15
```

`compare` reads `log.txt`, `report.json` (non-flat pixel share), and `load.txt`, evaluating **steady world lines** (>= 90 s after first world line) on both run and baselines (warm-up FPS is informational only). It reports median and `[q1..q3]` per metric plus per-baseline and overall verdicts (`regression` / `improvement` / `within noise`). Speed (`fps`, `ms`, `preparing`, `gpu`, `copy-back`) and per-frame drawn/dropped draws (by cause and named-reason `kind_*`) regress when median is worse by > max IQR of the two runs, subject to floors (`5%` of baseline, and `0.05 fps`, `1 ms`, or `1 draw`); < 3 steady lines produce no verdict. Whole-run counts (refused pipelines, untranslated shaders, black frames) regress on any increase; non-flat share on a drop > 20%; failure to reach world is a regression. Regression against any baseline triggers overall regression (`--all` requires every baseline); improvement requires beating every baseline. Exits non-zero on regression or when unjudgeable (< 3 steady lines in run or no baseline with >= 3, or run missing a metric present in baseline such as `report.json` non-flat share, black frames, or drop lines). If the machine was busy during the run or during every baseline, verdict reports **MACHINE BUSY** with post-busy metrics and fails (rerun, as load inflates spread). A busy baseline alongside a quiet one is displayed without counting; missing `load.txt` is `"load not recorded"`, not busy. Renamed drop reasons shift draws among `kind_*` rows—compare against `dropped_*` cause totals.

## Testbed (no game, no title run)

`victory-testbed` tests the translation layer headlessly:

```powershell
cargo run -q -p victory-testbed -- hle list
cargo run -q -p victory-testbed -- hle call xmem.dll XMemIsToolsMemAvailable
cargo run -q -p victory-testbed -- alu 4a000302 --in 40,2,0,0 [--hardware]
```

`alu` runs GFX10 hex dwords for one thread after copying four `--in` dwords to `v1..v4`, returning result in `v0`. Defaults to WARP; `--hardware` targets real GPU (ask owner first per `CLAUDE.md`). `hle call` takes up to four integers—call only exports that do not require a running title.
