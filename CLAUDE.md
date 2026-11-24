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
- Do not push, comment or open PRs unless the owner asks.
- One task per branch, small diff, no drive-by refactors or formatting
  (`cargo fmt --check` already fails on `main`).
- Mark a deliberate simplification `// shortcut:` with its limit.
- New dependency: only for a measured gain or correctness, GPL-3.0-compatible.
- `decision NNNN` and `TODO.md` in comments are not in this repo. Do not guess
  what they say.
- Running the title on a real GPU can reset the display driver. Ask first.

## Spending context

- Find with Grep, then Read only that range. Never read a source file over 800
  lines whole (`lower.rs` is 300 KB). A module's `//!` header gives its design.
- Skip `target/`, `Cargo.lock`, `LICENSE` and `bcpack-tables.bin`.
- Read `README.md` only for status, CLI or debug variables, and
  `docs/architecture.md` only for cross-crate flow or import resolution.
- Cap output: `gh ... --json <fields> -q <filter>`, `--stat` before a diff,
  `gh pr diff --name-only` before the full diff.
- Correctness beats saving tokens: before changing a function, read all of it,
  its callers and its tests. Grep every match arm of the instruction or API.
- Delegate only if the result is much smaller than the reading. Brief with the
  `task-brief` skill: a fresh agent does not see this conversation.

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
