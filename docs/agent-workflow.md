# Agent workflow: cost, delegation, reading, output

Everything in context is paid for again on every later call, so what enters
early and stays long costs most. Agents were the largest cost measured (64% of
one evening, one agent 32%).

## Sessions
- One milestone per session; at its end update `docs/worklog.md` and tell the
  owner to wipe context. Machine-local notes (paths, tools) stay outside the
  repo.
- Goals need an end, and a stop where the owner must decide. Waiting on the
  owner, answer a hook in one line; do not redo or restate work.
- End early, before context fills: finish what is verified, commit it, update
  the worklog, say what is unverified. Do not push a long investigation to its
  end.

## Delegating (owner, 2026-11-25)
- The main session coordinates; it does not debug. A hunt that needs more than
  a few runs or reads (a hang, a refusal origin, a speed problem) goes to a
  session with one clear question and an end, via `start_session`.
- One error, one session: do a run, group the errors by type (count, do not
  print), start a fresh session per distinct error with a `task-brief` (error
  text, evidence, a "done when" test). The main session runs, triages, merges
  and pushes. Never two real-GPU launches at once.
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
