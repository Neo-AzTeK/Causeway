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
`cargo xtask install-hooks`) enforces it. Details: `docs/asset-policy.md`.

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
- `decision NNNN` and `TODO.md` in comments point at files that are not in this
  repo; do not guess what they say.

## Checks

While iterating, only the crate you changed:

    cargo clippy -q -p <crate> --all-targets --locked -- -D warnings
    cargo test -q -p <crate> --locked <filter>

Before merging, what CI runs, plus the policy check:

    cargo clippy -q --workspace --all-targets --locked -- -D warnings
    cargo clippy -q --workspace --all-targets --all-features --locked -- -D warnings
    cargo test -q --workspace --locked
    cargo xtask policy

A bug fix has a test that fails without it. A performance claim has before and
after numbers. Tests never need game files. Report what was not verified and
which GPU adapter ran: passing tests do not show the title renders.

## Hardware range

A fix must hold on other GPUs, vendors and drivers, not only the primary dev GPU
(wave32 only). Query capabilities through `Caps` instead of hardcoding, keep a
clean refusal where a device lacks a feature, test both paths (`caps::reference`
gives fake caps, no GPU needed), and say which adapters were and were not
verified. A review of a GPU-facing diff answers: which capability does this
assume, and where is the fallback tested?
How: `docs/hardware-range.md`, `docs/gpu-review-checklist.md`; adapters seen so
far and not yet verified: `docs/adapters.md` (update its table when a new
adapter runs).

## Running the title on a real GPU

It can reset the display driver. Run one at a time, with diagnostics on and the
full log kept (`cargo xtask smoke`, see `docs/debugging.md`). Scripted menu
skipping must not tick the game's legal terms without the owner's OK for that
launch: use a data directory already set up.

