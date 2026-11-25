---
name: problem-session
description: Own one problem from docs/problems.md until it is 100% fixed, with a claim, running notes and a proof. Use when told to work on, resume, take over or confirm a problem.
argument-hint: "<id> | new <id> <symptom> | confirm <id>"
---
Request: $ARGUMENTS

Read `docs/problem-sessions.md` (the rules) and `CONTRIBUTING.md`. No id given:
run `cargo xtask problem list` and ask which one. Commands below are
`cargo xtask problem ...`.

## Start or resume

1. `list`, then `resume <id>`. No folder: `new <id>`. A new problem also needs a
   row in `docs/problems.md`; ask the owner for its rank. Check it is not part
   of a listed one.
2. Held by another session: stop and tell the owner, unless it is stale and you
   were asked to take over (`claim <id> --take-over`). An `external` problem: stop.
3. `claim <id>`. Work in your own worktree, on the branch it names if any, else
   a new one off `main`.
4. Read `problem.md`, `agreed.md`, `notes.md`; the end of `log.md`. Check
   `git branch --no-merged main` for earlier work on it.
5. Not agreed: fill `problem.md` from the evidence, then `block --question` and
   ask the owner to agree "Scope" and "Done when", quoting both in full. On
   their yes: `unblock`, `agree --owner "<their words>"`.

## Work, until fixed

Loop: pick the next step from "Next step" -> write the question it answers and
what each result would mean -> do it -> update `notes.md` (hypotheses, ruled out
with evidence, commits, runs, next step) -> `log <id> "<one line>"`.

- `claim <id> --files <paths>` before editing code; run `check <id>` after
  commits.
- Tests that may use the GPU, the full chain's too:
  `cargo xtask alone -- cargo test ...`; wait if it refuses. Before any other
  build or test, `Get-Process victory -ErrorAction SilentlyContinue` prints
  nothing.
- Runs: `cargo xtask smoke --problem <id> ...`, `--quiet` when timing counts. A
  run without `machine: quiet` proves no timing. `--accept-terms` needs the
  owner's OK for that launch.
- Commit what is verified before a long step; say in the notes what is not.
- No limit on runs and no early end. Only the owner shrinks the goal or lets
  the problem go (`release --owner "<their words>"`). Stuck or needing a
  decision: `block`, ask one question, end the turn.
- After compaction or a pause: `resume <id>` and reread the notes first.

## Finish

Meet every point of "Proof" in `docs/problem-sessions.md`, fill `proof.md`, run
the full chain and the `reviewer`, then `proven <id>`. Report: what was wrong,
before and after, what was not verified, the branch. Ask to merge. Keep the
claim. Never merge or push without the owner's explicit yes.

## `confirm <id>`

Do not claim. `resume <id>`, read `proof.md`, check the proven commit is in
`main`, run "Done when" on `main` (quiet if timed). Pass: `confirm <id>
--evidence "<run, commit, result>"`. Fail: `reopen <id> --evidence "..."`.
Report either way.
