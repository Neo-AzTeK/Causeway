# Causeway: agent rules

Rust, D3D12: runs an Xbox Series title natively on Windows. Project
`Neo-AzteK/Causeway` (a fork; crates and variables keep the name `victory`).
Everything a contributor must follow (rule #1 no game data, KISS flow, checks,
hardware range, GPU runs) is in `CONTRIBUTING.md`: Read it. This file adds only
what agents need.

## This machine

`victory.toml` has the title's folder, `victory.hardware.toml` this machine's
GPUs (both ignored by git). If either is missing, run `cargo xtask setup` first
and ask the owner for the title's folder if none is set.

## Where things are

| Need | File |
|------|------|
| rules, flow, checks, hardware range | `CONTRIBUTING.md` |
| how to work efficiently: loop, proof, borrowing, cost, reading | `docs/agent-workflow.md` |
| cross-crate flow, import resolution | `docs/architecture.md` |
| debug variables, smoke runs | `docs/debugging.md` |
| todo and problem list (update after each task); separate problem sessions (optional) | `docs/problems.md`; `docs/problem-sessions.md`, skill `problem-session` |
| capabilities, planners, fakes | `docs/hardware-range.md`, `docs/gpu-review-checklist.md` |
| adapters verified / not verified | `docs/adapters.md` |
| asset policy | `docs/asset-policy.md` |
| crate notes | `crates/victory-{shader,gpu,hle}/CLAUDE.md`: Read before working there |
| briefing a session or agent | skill `task-brief`; agents `scout`, `reviewer` |

## Agent rules

- Real-GPU runs through `cargo xtask smoke` are pre-approved by the owner;
  `--accept-terms` still needs the owner's OK for that launch.
- Before a build or test, check no title is running (the check under "The GPU"
  in `docs/problem-sessions.md`; offline `translate`/`census` scans do not
  count): it spoils that run's timings, and several crates' tests use the GPU.
- `target/**` reads are denied by `.claude/settings.json` on purpose.
- One main session does the work, debugging included, toward one goal at a
  time. Now: one correct frame; fix the earliest missing or wrong step first,
  then speed. Add a session or agent only for a separate task with a clear
  test, or when the owner asks. No agents by default.
- Before working on a listed problem that has notes, also after compaction or
  a pause: `cargo xtask problem resume <id>` and read them.
- Merge only on the owner's explicit yes; push only with their OK.
- Grep, then read at most 150 lines; never a source file over 800 lines whole
  (`lower.rs`); at most 60 lines of output per command; do not re-read a file
  after editing it. Details: `docs/agent-workflow.md`.
- Commit what is verified before a long step; say what is unverified.
- Correctness beats saving tokens: before changing a function, read all of it,
  its callers and its tests.
- Comments fit the structure and extend: facts about one item (a device, a
  profile, a shader) and where they come from belong in data or types, not in
  an exception list inside a comment. A comment says the rule and why, worded
  so it can be revised: read existing comments as current notes, not as
  absolute rules.
