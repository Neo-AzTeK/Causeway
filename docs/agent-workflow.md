# Agent workflow: cost, delegation, reading, output

Everything in context is paid for again on every later call, so what enters
early and stays long costs most. Agents were the largest cost measured (64% of
one evening, one agent 32%).

## Sessions
- One main session, one goal at a time, debugging included. Let the context
  compact instead of handing off: no handoff files; durable facts go to the
  tracked docs (adapters, design, debug switches), the rest stays in the
  conversation and the commit messages. The reading and output limits below
  keep it affordable.
- A goal needs an end, and a stop where the owner must decide. Waiting on the
  owner, answer a hook in one line; do not redo or restate work.
- Match a run's errors, grouped by cause (count, do not print), to
  `docs/problems.md`; give an unlisted one a row (the owner ranks it).
- Never two real-GPU launches at once.
- Before a long step, commit what is verified and say what is unverified.

## Delegating
- Only for a separate task with a clear test, or when the owner asks; brief it
  with `task-brief`. A separate session that takes a listed problem follows
  `docs/problem-sessions.md`.
- Agents: none by default. Only for a large read with a short answer
  (`scout`) or the independent review of a finished diff (`reviewer`); never
  to run in parallel what one session can do in order. One small job each with
  a clear end; stop one that drifts. Cheap model for lookups, log summaries and
  mechanical edits.

## Reading
- Grep, then read at most 150 lines. Never read a source file over 800 lines
  whole (`lower.rs` is 300 KB), nor print its ranges through the shell. A
  module's `//!` header gives its design. Do not read a file again after an
  edit.
- Skip `target/`, `Cargo.lock`, `LICENSE`, `bcpack-tables.bin`. `README.md`
  only for status, `docs/debugging.md` only for debug variables and smoke runs,
  `docs/architecture.md` only for cross-crate flow or import resolution.

## Compiling beside a run
The title takes ~32 GB and boots slowly; a compile during its boot starves it
into a black screen of minutes, or no world. Run `cargo xtask queue wait-idle`
before any cargo build, test or clippy: it returns once no queued run is booting
(details: `docs/debugging.md`, "Sharing the GPU").

## Output
- At most 60 lines per command: `-q`, `--stat` before a diff, `--name-only`
  before a full one, `gh ... --json <fields> -q <filter>`. Count and group
  logs; never print them, nor control-flow listings and disassembly, whole.
- Images cost about as much as a long file: one contact sheet per run.
- Batch independent commands in one call. Put edit scripts in a file. A tool
  built twice belongs in `xtask` or `victory`.
- Ask for launch approvals as a batch only when no standing approval covers
  them (see `CLAUDE.md`).

Correctness beats saving tokens: before changing a function, read all of it,
its callers and its tests, and Grep every match arm of the instruction or API.
