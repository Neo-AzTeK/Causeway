# Causeway: agent rules

Runs an Xbox Series title's x86-64 code natively on Windows: Rust, D3D12. The
project is `Neo-AzteK/Causeway`; crates, binary and variables keep the name
`victory` from the project it was forked from. This file holds what every task needs. Three crates have
their own `CLAUDE.md` (`victory-shader`, `victory-gpu`, `victory-hle`): Read it
before working there.

## Rules

- **Rule #1:** nothing from the game, or derived from it, enters the repo
  (files, dumps, disassembly, screenshots, snapshots). No new policy exceptions
  without the owner's decision. Details: `docs/asset-policy.md`.
- **Work on Causeway.** Branches, PRs and searches target it: pass
  `-R Neo-AzteK/Causeway` to `gh`, whose default for a fork can be the parent.
  Use the original project (`Marvisak/VIctory`) only when the owner asks; code
  taken from it keeps its authorship and licence.
- KISS flow (owner's decision, 2026-11-24): no stacks, no PRs. A task is a
  short branch (or worktree) off `main`; once its checks pass (below) merge it
  into `main` directly, and push `main` to `origin` (Neo-AzteK/Causeway) after
  each merge or at least each milestone. Never force-push, rewrite `main`, or
  push other branches. Comment on or open PRs only when the owner asks.
- One task per branch, small diff, no drive-by refactors or formatting
  (`cargo fmt --check` already fails on `main`).
- Mark a deliberate simplification `// shortcut:` with its limit.
- New dependency: only for a measured gain or correctness, GPL-3.0-compatible.
- `decision NNNN` and `TODO.md` in comments are not in this repo. Do not guess
  what they say.
- Goal: the title (the owner's local copy, path given by the owner) works
  fully; current milestone: the world's 3D scene is visible.
- Running the title on a real GPU can reset the display driver. Ask first; the
  owner allows several launches per task, one at a time. A script may accept
  the game's legal terms only with the owner's OK for that launch.
- `target/**` reads are denied by `.claude/settings.json` on purpose.
- Hardware range (owner's rule, 2026-11-25): a fix must hold on other GPUs,
  vendors and drivers, not only the owner's RTX 2060 SUPER (wave32 only). Query
  capabilities (wave size, binding tier, heap and format limits) instead of
  hardcoding; keep a clean refusal where a device lacks a feature; test both
  paths; say which adapters were and were not verified. Every brief states it.
  Every review of a GPU-facing diff answers one fixed question: which
  capability does this assume, and where is the fallback tested? Prefer the
  conservative portable path unless a capability is proven.

## Work log

`docs/worklog.md` is the shared record of state, ranked next work, open
decisions and a log line per merge or finding (no game data). Whoever merges,
finishes a task or runs the game updates it in the same change, and reads its
"Next" section before choosing work.

## Spending tokens

Everything in context is paid for again on every later call, so what enters
early and stays long costs most. Agents were the largest cost measured
(64% of one evening, one agent 32%).

Sessions
- One milestone per session; at its end update `HANDOFF.md` (local, ignored by
  git; at most 40 lines, current state only) and tell the owner to wipe context.
- Goals need an end, and a stop where the owner must decide. Waiting on the
  owner, answer a hook in one line; do not redo or restate work.
- Ask for launch approvals as a batch ("up to 3 runs, 5 minutes each").

Delegating (owner's rule, 2026-11-25)
- The main session coordinates; it does not debug. A hunt that needs more than
  a few runs or reads (a hang, a refusal origin, a speed problem) goes to a
  session with one clear question and an end, via `start_session`; the main
  session keeps merging, pushing and the handoff.
- One error, one session (owner's strategy, 2026-11-25): do a run, group the
  errors by type (count, do not print), then start a fresh session per
  distinct error via `start_session` with a `task-brief`: the error text, the
  evidence and a "done when" test. The main session only runs, triages, merges
  and pushes. Never two real-GPU launches at once.
- End the session early, before context fills: finish what is verified,
  commit it, update `HANDOFF.md`, say what is unverified, and tell the owner
  to wipe context. Do not push through a long investigation to its end.

Agents
- None by default. Only for a large read with a short answer (`scout`) or the
  independent review of a finished diff (`reviewer`); never to run in parallel
  what one session can do in order.
- One small job each with a clear end, such as one refusal type; stop one that
  drifts. Brief with the `task-brief` skill. Cheap model for lookups, log
  summaries and mechanical edits.

Reading
- Grep, then read at most 150 lines. Never read a source file over 800 lines
  whole (`lower.rs` is 300 KB), nor print its ranges through the shell.
  A module's `//!` header gives its design. Do not read a file again
  after an edit.
- Skip `target/`, `Cargo.lock`, `LICENSE` and `bcpack-tables.bin`. `README.md`
  only for status, `docs/debugging.md` only for debug variables and smoke runs,
  `docs/architecture.md` only for cross-crate flow or import resolution.

Output
- At most 60 lines per command: `-q`, `--stat` before a diff, `--name-only`
  before a full one, `gh ... --json <fields> -q <filter>`. Count and group
  logs; never print them, nor control-flow listings and disassembly, whole.
- Images cost about as much as a long file: one contact sheet per run.
- Batch independent commands in one call. Put edit scripts in a file, not in
  quoted shell text. A tool built twice belongs in `xtask` or `victory`.

Correctness beats saving tokens: before changing a function, read all of it,
its callers and its tests, and Grep every match arm of the instruction or API.

## Checks

While iterating, only the crate you changed:

    cargo clippy -q -p <crate> --all-targets --locked -- -D warnings
    cargo test -q -p <crate> --locked <filter>

Before calling it done, what CI runs, plus the policy check:

    cargo clippy -q --workspace --all-targets --locked -- -D warnings
    cargo clippy -q --workspace --all-targets --all-features --locked -- -D warnings
    cargo test -q --workspace --locked
    cargo xtask policy

Test a bug fix with a test that fails without it. Back performance claims with
before and after numbers. Report what was not verified, and which GPU adapter
ran: passing tests do not show the title renders.
