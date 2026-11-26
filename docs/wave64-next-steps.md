# wave64-refusals: next steps (2026-11-26)

Branch `claude/wave64-loop`. Problem claim and full notes: `cargo xtask problem
resume wave64-refusals`. Delete this file when the problem is proven.

## Done on this branch

- Several console waves per group: blocks run in one order, and reducible loops
  run until no wave of the group goes round (`emit/lower/linear.rs`).
- What a loop computed is held for the waves that left it, by a pass over the
  DXIL instructions (`emit/bitcode/hold.rs`).
- Tests: loops, break, nested loops, no shared-memory write by an off wave,
  refusal when the exchanges do not fit in 32 KiB of shared memory.
- Exception to the overlap with `untranslated-shaders` (owner allowed it): the
  one line `pub(crate) mod hold;` in `emit/bitcode.rs`. Whoever merges second
  merges `main`.

## State

Not proven. Shader tests (314, 3 ignored) passed at the last code commit on an
RTX 2060 SUPER; shader clippy passed there. The census was last taken at the
loop commit: no program lost, and every multi-wave program is still refused.
Not done: reviewer, full check chain, census with `--pipelines`, game run, other
adapters.

## Next

1. Guard what ignores EXEC in blocks a wave may be off in: scalar loads, flat and
   root-address loads, buffers, images and heap-index descriptor uses (guarded
   loads give 0). Widen `linear::supports` only as each one is covered, each
   with a GPU test that fails without the guard. This is what lets the refused
   programs through.
2. With it, check the descriptor phis in `lower.rs`: they do not go through
   `Lower::join`, which linear emission needs. A descriptor handle made in a
   loop and used after it is refused by `hold.rs`.
3. Test LDS between waves across `s_barrier` in a loop, and a loop headed by the
   entry block. Check the refused programs for spin-waits on another wave.
4. Programs that use all 32 KiB of shared memory stay refused. Once the others
   work, ask the owner (`problem block --question`).
5. Check pipeline creation time and dispatch time: every block runs, with two
   barriers per lane-varying branch and per loop pass.
6. Remaining items in the problem notes, then the proof (full chain, reviewer,
   census, quiet smoke run; `--accept-terms` needs the owner's OK).

Test trap: a program takes this path only if a **live** lane read sits in a loop
or in a skipped block. Otherwise another form translates it, and the test passes
without testing anything.

`cargo fmt` also reformats old code in `emit/*.rs`: format only your own hunks.
