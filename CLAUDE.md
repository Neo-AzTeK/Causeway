# Causeway: agent rules

Runs an Xbox Series title's x86-64 code natively on Windows: Rust, D3D12. The
project is `Neo-AzteK/Causeway`; crates, binary and variables keep the name
`victory` from the project it was forked from. Crates `victory-shader`,
`victory-gpu` and `victory-hle` have their own `CLAUDE.md`: Read it before
working there. Before choosing work, read the "State" and "Next" sections of
`docs/worklog.md`. Humans: `CONTRIBUTING.md`.

## Rules

- **Rule #1:** nothing from the game, or derived from it, enters the repo
  (files, dumps, disassembly, screenshots, snapshots). No new policy exceptions
  without the owner's decision. Details: `docs/asset-policy.md`.
- **Work on Causeway.** Branches and searches target it: pass
  `-R Neo-AzteK/Causeway` to `gh`. Use `Marvisak/VIctory` only when the owner
  asks; code taken from it keeps its authorship and licence.
- KISS flow (owner, 2026-11-24): no stacks, no PRs. A task is a short branch
  (or worktree) off `main`; once its checks pass, merge it into `main` and push
  `main` after each merge or milestone. Never force-push or rewrite `main`.
  Push other branches or open PRs only when the owner asks.
- One task per branch, small diff, no drive-by refactors or formatting
  (`cargo fmt --check` already fails on `main`).
- Mark a deliberate simplification `// shortcut:` with its limit.
- New dependency: only for a measured gain or correctness, GPL-3.0-compatible.
- `decision NNNN` and `TODO.md` in comments are not in this repo; do not guess.
- Goal: the title (the owner's local copy, path given by the owner) works
  fully; milestone: the world's 3D scene is visible.
- Real-GPU runs can reset the display driver. Standing approval (owner,
  2026-11-25): runs are approved, one at a time, never two at once; any time
  limit; the owner may exit early. Log everything (diagnostics on, keep the
  log), skip configuration (`cargo xtask smoke` defaults), decide the workloads
  afterwards. A script accepts the game's legal terms only with the owner's OK
  for that launch.
- `target/**` reads are denied by `.claude/settings.json` on purpose.
- **Hardware range** (owner, 2026-11-25): a fix must hold on other GPUs,
  vendors and drivers, not only the owner's RTX 2060 SUPER (wave32 only). Query
  capabilities through `Caps`; keep a clean refusal where a feature is missing;
  test both paths; say which adapters were and were not verified. Every brief
  states it. Every review of a GPU-facing diff answers: which capability does
  this assume, and where is the fallback tested? How: `docs/hardware-range.md`,
  `docs/gpu-review-checklist.md`, `docs/adapters.md`.

## Work log

`docs/worklog.md` is the shared record: state, ranked next work, open
decisions, one log line per merge or finding (no game data). Whoever merges,
finishes a task or runs the game updates it in the same change. Old log lines
move to `docs/worklog-archive.md` when the log passes ~60 lines.

## Working cheaply

Cost, delegation, reading and output rules: `docs/agent-workflow.md`. Short
form: the main session coordinates and does not debug; a hunt goes to its own
session with a `task-brief`; one error, one session; no agents by default;
Grep then read at most 150 lines, never a file over 800 lines whole
(`lower.rs`); at most 60 lines of output per command; end the session early,
update the worklog and tell the owner to wipe context.

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
