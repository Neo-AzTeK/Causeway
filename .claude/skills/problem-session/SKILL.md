---
name: problem-session
description: Work on problems from docs/problems.md until they are 100% fixed, with running notes and a proof; any number at once, no locks. Use when told to work on, resume or confirm a problem.
argument-hint: "<id> | new <id> <symptom> | confirm <id>"
---
Request: $ARGUMENTS

Read `docs/problem-sessions.md` (the rules) and `CONTRIBUTING.md`. No id given:
run `cargo xtask problem list` and ask which one. Commands below are
`cargo xtask problem ...`. Problems are not locked: a session may work on
several, and several sessions on one.

## Start or resume

1. `list`, then `resume <id>`. No folder: `new <id>`. A new problem also needs a
   row in `docs/problems.md`; ask the owner for its rank. Check it is not part
   of a listed one. An `external` problem: stop.
2. Read `problem.md`, `agreed.md`, `notes.md`; the end of `log.md`. If another
   session logged recently, build on its branch (`git branch --no-merged main`),
   or tell the owner. Work in your own worktree, else a new branch off `main`.
3. Not agreed: fill `problem.md` from the evidence, then `block --question` and
   ask the owner to agree "Scope" and "Done when", quoting both in full. On
   their yes: `unblock`, `agree --owner "<their words>"`.

## Work, until fixed

Loop: pick the next step from "Next step" -> write the question it answers and
what each result would mean -> do it -> update `notes.md` (hypotheses, ruled out
with evidence, commits, runs, next step) -> `log <id> "<one line>"`.

- Run `check <id>` after commits.
- Tests that may use the GPU, the full chain's too:
  `cargo xtask alone -- cargo test ...`; wait if it refuses. Before any other
  build, test or offline scan, the title check under "The GPU" in
  `docs/problem-sessions.md` prints nothing (an offline scan is not a title).
- Runs: `cargo xtask smoke --problem <id> ...`, `--quiet` when timing counts. A
  run without `machine: quiet` proves no timing. `--accept-terms` needs the
  owner's OK for that launch.
- Commit what is verified before a long step; say in the notes what is not.
- No limit on runs and no early end. Only the owner shrinks the goal or lets
  the problem go (log their words). Stuck or needing a decision: `block`, ask
  one question, end the turn.
- After compaction or a pause: `resume <id>` and reread the notes first.

## Finish

Meet every point of "Proof" in `docs/problem-sessions.md`, fill `proof.md`, run
the full chain and the `reviewer`, then `proven <id>`. Report: what was wrong,
before and after, what was not verified, the branch. Ask to merge. Never merge
or push without the owner's explicit yes.

## `confirm <id>`

`resume <id>`, read `proof.md`, check the proven commit is in `main`, run "Done
when" on `main` (quiet if timed). Pass: `confirm <id> --evidence "<run, commit,
result>"`. Fail: `reopen <id> --evidence "..."`. Report either way.
