# Contributing to Causeway

Short version of how work lands here. Agents follow `CLAUDE.md` (same rules,
more detail on cost).

1. **Rule #1.** Nothing from the game or derived from it goes in the repo: no
   files, dumps, disassembly, screenshots or snapshots. `docs/asset-policy.md`.
2. **Pick work** from "Next" in `docs/worklog.md`. One task per branch, small
   diff, no drive-by refactors or formatting.
3. **Hardware range.** A fix must hold on other GPUs, vendors and drivers.
   Query `Caps` (`docs/hardware-range.md`), keep a clean refusal where a feature
   is missing, test both paths (`caps::reference` gives fake caps, no GPU
   needed), and say which adapters you did and did not verify
   (`docs/adapters.md`).
4. **Checks** before merging (what CI runs, plus policy):

       cargo clippy -q --workspace --all-targets --locked -- -D warnings
       cargo clippy -q --workspace --all-targets --all-features --locked -- -D warnings
       cargo test -q --workspace --locked
       cargo xtask policy

5. **Tests.** A bug fix has a test that fails without it. A performance claim
   has before and after numbers. Tests never need game files.
6. **Review.** For a GPU-facing diff, answer the questions in
   `docs/gpu-review-checklist.md`.
7. **Merge** the branch into `main` once checks pass; no stacks. Update
   `docs/worklog.md` in the same change.
8. **Deliberate simplifications** are marked `// shortcut:` with their limit.
   New dependencies need a measured gain and a GPL-3.0-compatible licence.
