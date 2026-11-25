# Causeway: agent rules

Rust, D3D12: runs an Xbox Series title natively on Windows. Project
`Neo-AzteK/Causeway` (a fork; crates and variables keep the name `victory`).
Everything a contributor must follow (rule #1 no game data, KISS flow, checks,
hardware range, GPU runs) is in `CONTRIBUTING.md`: Read it. This file adds only
what agents need.

## Where things are

| Need | File |
|------|------|
| rules, flow, checks, hardware range | `CONTRIBUTING.md` |
| current state, next work, owner decisions (local, ignored) | `.local/handoff.md` |
| run log, recipes (local, ignored) | `.local/log.md`, `.local/recipes.md` |
| cost, delegation, reading, handoff workflow | `docs/agent-workflow.md` |
| cross-crate flow, import resolution | `docs/architecture.md` |
| debug variables, smoke runs | `docs/debugging.md` |
| capabilities, planners, fakes | `docs/hardware-range.md`, `docs/gpu-review-checklist.md` |
| adapters verified / not verified | `docs/adapters.md` |
| asset policy | `docs/asset-policy.md` |
| crate notes | `crates/victory-{shader,gpu,hle}/CLAUDE.md`: Read before working there |
| briefing a session or agent | skill `task-brief`; agents `scout`, `reviewer` |

Start of a session: read `.local/handoff.md` before choosing work. In a
worktree, the local files are in the main checkout (`git rev-parse
--git-common-dir`, its parent folder), not in the worktree.

## Agent rules

- This is a fork: always pass `-R Neo-AzteK/Causeway` to `gh` (its default can
  be the parent repo).
- Standing approval for real-GPU runs (owner, 2026-11-25): one at a time, any
  time limit, diagnostics on, full log kept, `cargo xtask smoke` defaults.
  Never accept the game's legal terms by script without the owner's OK for that
  launch.
- `target/**` reads are denied by `.claude/settings.json` on purpose.
- The main session coordinates and does not debug: a hunt goes to its own
  session with a `task-brief` (one error, one session). No agents by default.
- Grep, then read at most 150 lines; never a source file over 800 lines whole
  (`lower.rs`); at most 60 lines of output per command; do not re-read a file
  after editing it. Details: `docs/agent-workflow.md`.
- End early, before context fills: commit what is verified, update
  `.local/handoff.md`, say what is unverified, tell the owner to wipe context.
- Correctness beats saving tokens: before changing a function, read all of it,
  its callers and its tests.
