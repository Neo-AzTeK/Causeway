# Agent workflow: cost, delegation, reading, output

Everything in context is paid for again on every later call, so what enters
early and stays long costs most. Agents were the largest cost measured (64% of
one evening, one agent 32%).

## Sessions
- One goal per session. Let the context compact instead of handing off: no
  handoff files; durable facts go to the tracked docs (adapters, design, debug
  switches), the rest stays in the conversation and the commit messages.
- A game problem gets a problem session, which stays until it is fixed or the
  owner must decide, however long that takes, and keeps running notes outside
  the repository (`docs/problem-sessions.md`; the notes are a lab book, not a
  handoff). The reading and output limits below keep it affordable.
- Any other goal needs an end, and a stop where the owner must decide. Waiting
  on the owner, answer a hook in one line; do not redo or restate work.
- Before a long step, commit what is verified and say what is unverified.

## Delegating (owner, 2026-11-25)
- The main session coordinates; it does not debug. A hunt that needs more than
  a few runs or reads (a hang, a refusal origin, a speed problem) goes to a
  problem session, via `start_session` with the prompt `/problem-session <id>`.
- One overall problem, one session: do a run, group the errors by cause (count,
  do not print), match them to `docs/problems.md`, and give an unlisted one a
  row (the owner ranks it). Sub-causes stay with their problem; do not split
  them into short sessions. The main session runs, triages, and merges and
  pushes when the owner says so. Never two real-GPU launches at once.
- A question that is not a problem to fix (a lookup, a measurement) goes with a
  `task-brief`.
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
