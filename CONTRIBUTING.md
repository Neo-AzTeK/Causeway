# Contributing to Causeway

Causeway runs an Xbox Series title's x86-64 code natively on Windows (Rust,
D3D12). The project is `Neo-AzteK/Causeway`; crates, binary and variables keep
the name `victory` from the project it was forked from. Humans and agents follow
the same rules; agents also read `CLAUDE.md`.

Goal: the title (your local copy) works fully. Current milestone: the world's 3D
scene is visible.

## Rule #1: no game data

Nothing from the game, or derived from it, enters the repo: no files, dumps,
disassembly, screenshots or snapshots. No new policy exceptions without the
owner's decision. `cargo xtask policy` (also the pre-commit hook,
`cargo xtask setup`) enforces it. Details: `docs/asset-policy.md`.

Run `cargo xtask setup --game-dir <folder>` once per clone: it installs that
hook, makes `gh` target this repository rather than the parent of the fork, and
writes the ignored `victory.toml` (the title's folder, never overwritten) and
`victory.hardware.toml` (this machine's GPUs; run it again after a hardware or
driver change).

## Flow (KISS, owner's decision 2026-11-24)

- No stacks, no PRs. A task is a short branch (or worktree) off `main`, one task
  per branch, small diff, no drive-by refactors or formatting (`cargo fmt
  --check` already fails on `main`).
- When its checks pass, merge it into `main` and push `main`, after each merge
  or milestone. Never force-push or rewrite `main`. Push other branches or open
  PRs only when the owner asks.
- Mark a deliberate simplification `// shortcut:` with its limit.
- A new dependency needs a measured gain or a correctness reason, and a
  GPL-3.0-compatible licence.

## Comments

If a change contradicts a comment, either the change is wrong or the comment is
updated in the same diff. Match a comment to how binding it is:

| The comment | What to do |
|---|---|
| must always hold, and code can check it | make it a test, an assert or a type |
| must hold, and a test proves it | `// invariant(<test_name>): why` |
| explains why (hardware quirk, rejected approach) | keep it, short |
| marks a deliberate limit | `// shortcut:` with the limit |
| restates the code, or is stale | delete it in code you already change, never in a sweep |

`cargo xtask policy` (pre-commit, and the workspace tests) fails when an
invariant names no existing test, and when the number of `decision NNNN`
references grows: those documents are not in this repo (nor is `TODO.md`), so do
not guess what they say. Keep `// SAFETY:` on `unsafe` blocks. What a machine
cannot see (a weakened test, a comment the diff makes false) is the `reviewer`
agent's job: run it, or review by its list, before merging a code change.

## Checks

While iterating, only the crate you changed:

    cargo clippy -q -p <crate> --all-targets --locked -- -D warnings
    cargo test -q -p <crate> --locked <filter>

Before merging, the full chain. There is no CI: nobody else runs it.

    cargo clippy -q --workspace --all-targets --locked -- -D warnings
    cargo clippy -q --workspace --all-targets --all-features --locked -- -D warnings
    cargo test -q --workspace --locked
    cargo xtask policy

A bug fix has a test that fails without it. A performance claim has before and
after numbers. Tests never need game files. Report what was not verified and
which GPU adapter ran: passing tests do not show the title renders.

## Hardware range

A fix must hold on other GPUs, vendors and drivers, not only the primary dev GPU
(wave32 only). Query capabilities through `Caps` instead of hardcoding (only
`caps/` asks the device, apart from the counted exceptions in
`docs/hardware-range.md`; `cargo xtask policy` rejects it elsewhere), keep a
clean refusal where a device lacks a feature, test both paths (`caps::reference`
gives fake caps, no GPU needed), and say which adapters were and were not
verified. A review of a GPU-facing diff answers: which capability does this
assume, and where is the fallback tested?
How: `docs/hardware-range.md`, `docs/gpu-review-checklist.md`; adapters seen so
far and not yet verified: `docs/adapters.md` (update its table when a new
adapter runs).

## Running the title on a real GPU

It can reset the display driver. Use `cargo xtask smoke` (see
`docs/debugging.md`): diagnostics build, full log, one run at a time, and no
scripted keys on a data folder not set up unless `--accept-terms` is passed,
which needs the owner's OK for that launch.

