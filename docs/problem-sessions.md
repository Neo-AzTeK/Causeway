# Problem sessions

One session takes one overall problem and keeps it until it is 100% fixed.
Several run side by side. The ranked list is `docs/problems.md`. Start one by
telling a new session:

    /problem-session <id>

## Life of a problem

| State | Meaning | Leaves it by |
|-------|---------|--------------|
| open | nobody holds it | `claim` |
| active | one session works on it | `block`, `proven`, `release` |
| blocked | it waits for the owner | `unblock` with their answer |
| proven | fixed on its branch, with proof; waits for the owner's merge | `confirm`; `reopen` (back to active, same session); `release` |
| fixed | the proven commit is in `main` and "Done when" passed there | `reopen` |

All commands are `cargo xtask problem <command>` (`--help` lists them).

## Writing a problem down

`problem.md` has five sections, filled before any fix: **Symptom**, **Evidence**
(run folders, counted log lines, guesses marked), **Effect** on the picture or
stability (or "unknown"), **Scope** (every sub-cause and case in, and what is
out) and **Done when**.

"Done when" is measurable: which build, run or command, flags, scene, length and
adapter, what number or picture must come out, and which test. The session
proposes it, the owner agrees it together with **Scope**, and
`agree --owner "<their words>"` records both. "Done when" stays the last
section, and `problem.md` has no other `## ` heading: everything from "Done
when" to the end is part of the agreement. Until then the
session only does what writing the definition needs.

None of these is "fixed": diagnostics, logging, a workaround, a skip or limit
switch, a refusal made cleaner, or some of the cases. A problem with several
sub-causes stays open until every one is closed; new ones found on the way are
added to the list in `notes.md`. If one turns out to belong to another problem
or another owner, or the test cannot be met, the session blocks and asks. Only
the owner changes "Done when" or the scope, or lets a problem go unfixed:
`release --owner "<their words>"`.

## One session, until fixed

- No cap on game runs, reads or hours, and no early end because the context is
  long: compaction keeps the session going. It ends its work on a problem only
  as proven, blocked (waiting for the owner), or released on the owner's word.
- Every run still needs a question it answers. Write the question in `notes.md`
  before launching. Three results in a row that change no hypothesis mean the
  method is wrong: write why, and change it (more instrumentation, an offline
  reproduction, the testbed, a bisect). Still no way forward: block.
- Blocked: `block --question "..."`, ask the owner, end the turn. One question,
  with what was tried. Do not carry on with a smaller goal.
- A different error met on the way is not this problem's: note it, tell the
  owner at the next report, do not chase it. If it prevents the work, block.
- The session debugs itself. No agents by default (`docs/agent-workflow.md`);
  the `reviewer` at the end is required.

## Notes

The registry is `%USERPROFILE%\.causeway\problems\<id>\` (override:
`CAUSEWAY_PROBLEMS_DIR`), shared by every worktree, outside the repository and
not backed up. Notes name runs, hashes and addresses of the title, so nothing in
it is ever copied into the repository, a commit message or a PR.

| File | Holds | Written |
|------|-------|---------|
| `problem.md` | the definition | at the start |
| `agreed.md` | "Scope" and "Done when" as the owner agreed them | by `agree` |
| `notes.md` | now, next step, sub-causes, hypotheses, ruled out with evidence, commits, runs, questions | after each step and run |
| `log.md` | dated history, append only | by the commands and `log` |
| `proof.md` | the proof | before `proven` |
| `state.txt` | claim and state | by the commands only |

`notes.md` is the state now, at most 150 lines: move old detail out with `log`
or into other files of the folder (long findings, dumps of counts). "Next step"
is exact enough to run without this conversation: the command, the file and
symbol, what result means what. Update the notes **before** a long run or build,
not only after. The test: a fresh session given only this folder, the branch and
the repository can go on without redoing a ruled-out step.

**Not handoff files** (dropped by the owner). A handoff was written at the end,
to move the work into a new session. These notes are a lab book written as the
work goes: compaction is still how a session continues, and the notes are what
it reads back to check its summary, and what survives if the session dies.

After a compaction, a restart or a long wait: `problem resume <id>` first, then
read the files it names, before acting on memory. Where they disagree with the
summary, check the evidence.

## Working in parallel

**Claims** (checked by code). `claim <id> --files a,b` takes a problem. One
holder per problem, one problem per session. The holder is the Claude session id,
which compaction keeps. `log`, `check`, `agree`, `block`, `unblock`, `resume`
and `smoke --problem` (as the run starts and ends) keep the claim alive.

**Dead sessions.** A claim is stale after 8 hours without one of those, or at
once if its worktree is gone. A stale *active* claim is taken with
`claim <id> --take-over`. Blocked, proven or not yet stale takes
`--take-over --owner "<their words>"`, because waiting is not dying. The
command names the earlier branch: build on it, and look for uncommitted files in
the old worktree. A session that finds its problem held by another stops.
Before taking over, check that the old session is not just in a long run.

**Files.** `--files` names the repository files or folders the problem expects to
change; add more with another `claim` as the diff grows. A claim that meets
another's is refused. Then narrow it, or wait, or agree how the two stay apart
(which functions, who merges first) and record that with `--accept-overlap`,
which covers those two claims only; it is better to ask the owner than to guess
for a large shared file. `check`
fails on a changed file that is not claimed (`docs/` and `Cargo.lock` are
free) and on any descriptor file: that work belongs to another AI, as do the
rows of `docs/problems.md` whose owner is not `here`. Descriptor edits in
files other than `crates/victory-gpu/src/descriptor.rs` and the `descriptor/`
folder are not detected: block on those.
Whoever merges second first merges `main` into its branch and redoes the check
chain and the proof run.

**Each session has its own worktree and branch.** Do not share a checkout.

**The GPU.** One title or GPU test at a time on the machine. `cargo xtask smoke`
and `cargo xtask alone -- <command>` take the same machine-wide lock, and both
refuse to start while a `victory.exe`, the testbed or any cargo test binary
runs. Run every test that may use the GPU through `alone` (`victory-gpu`,
`victory-shader`, `victory-hle`, the whole workspace, testbed `--hardware`):

```powershell
cargo xtask alone -- cargo test -q -p victory-gpu --locked
```

When it refuses, wait for the other run and try again later. A plain
`cargo test` or build still starts during a run: before one, or before an
offline scan, check that no title is running. This must print nothing (it needs
no cargo):

```powershell
Get-CimInstance Win32_Process -Filter "Name='victory.exe'" | Where-Object CommandLine -notmatch '^("[^"]*"|[^"\s]\S*)\s+(translate|census)(\s|$)'
```

It lists every `victory.exe` except the offline scans (`translate`, `census`),
so a title, or one whose command line cannot be read, still makes you wait. An
offline scan alone does not hold up a build; it does take CPU, and a build in
its own worktree cannot replace the `victory.exe` it runs. The lock is
unchanged: `smoke` and `alone` still refuse beside any `victory.exe`, so GPU
tests wait for a scan too, and a scan that uses the GPU (`translate
--pipelines`) runs through `alone`.

A branch made before `alone` existed has no such command: merge `main` into it
first.

A damaged `state.txt` stops every claim until it is mended, on purpose: it may
be a claim on anything.

**Quiet machine.** Pass `--quiet` to every run whose timings matter. It refuses
to start while a compiler, linker, test binary or title runs. Every launched run
is sampled every 5 s, and `run.txt` and the digest's header say
`machine: quiet`, `NOT QUIET: ...` or `unknown`. Timings from a run that is not
`quiet` are not evidence, for or against; neither is a log without the line.
The sampling does not see short builds, `cargo` waiting alone, or other load
(browser, game, indexer). Before a long timing run, tell the other sessions or
the owner.

Pass `--problem <id>` to smoke: it refuses a problem this session does not hold
and writes the run folder into the log. Put each run in a new folder outside the
repository, named for the problem, and keep those the proof cites.

## Proof

`proven` refuses until `proof.md` is filled, "Scope" and "Done when" are as
agreed, every sub-cause is a ticked `- [x]` line (details indented under it),
changed files are claimed, the tree is clean and the branch is not `main` and
changes something `main` lacks. So prove before the merge. It cannot judge what
the text says. The session writes:

- **Before and after**, same flags and adapter, from quiet runs, on the commit
  before and the commit with the fix. Same scene: say how it is known (same
  moment, comparable draw counts, frames side by side). If not, say so and what
  it leaves unproven. Timings need at least two runs each side, with the spread.
  A stability fix needs a stated number of runs of a stated length, chosen from
  how often the failure showed before.
- **A failing test** without the fix, for a bug. Say that you saw it fail.
- **The full check chain** of `CONTRIBUTING.md`, after the last change.
- **The `reviewer` agent** on the final diff; every finding fixed or answered.
- **Portability**: `Caps` queried, the path without the capability tested,
  adapters verified and not verified (`docs/gpu-review-checklist.md`,
  `docs/adapters.md`).
- The "Done when" result line by line, with all workarounds for this problem
  off: no skip, limit or survive switch that hides it. (`smoke` sets
  `VICTORY_SURVIVE_REMOVAL=1` itself: read the removal line, not the exit.)

Then the session reports to the owner and asks for the merge. It never merges or
pushes without their explicit yes.

**After the merge**, someone other than the fixer verifies: a new session told
`/problem-session confirm <id>`, or the owner. It runs "Done when" on `main` and
records `confirm --evidence "..."`. `confirm` checks that the proven commit is
in `main` and the agreement unchanged, and refuses the fixing session without
`--owner`. If the test fails on `main` or the owner rejects the proof,
`reopen --evidence "..."`: before `confirm` the problem goes back to its
session. `reopen` gives it a fresh 8 hours: if it is dead, take it over after
them, or at once with the owner's words.

**Regression.** Anyone who sees a fixed problem's symptom again `reopen`s it with
the run that shows it, and tells the owner. The agreed "Done when" stands, the
old proof is kept aside, and a new session claims a fixed one. Whoever reads a run's
digest compares it with the fixed problems of `problem list`.

## What code enforces and what only these rules do

| Enforced by `xtask` | Written rule only |
|---|---|
| one holder per problem, one problem per session | the session stays until fixed, and asks when blocked |
| rows owned elsewhere (this checkout's list or `main`'s) and `descriptor/`, `descriptor.rs` refused | descriptor work in other files |
| take-over only when stale and active, or with `--owner`; `release` needs `--owner` | that `--owner` quotes something the owner really said |
| overlapping claims refused unless explained, per pair; unclaimed changes fail `check` | that sessions run `check`, and keep an agreed overlap apart |
| "Scope" and "Done when" compared with the agreed copy at `proven` and `confirm` | that the test is a good one; editing `agreed.md` or `state.txt` by hand |
| `proven` needs filled proof, ticked sub-causes, a clean branch that changes something | that the proof is true, the runs quiet, the review done |
| `confirm` needs the commit in `main` and another session id | that the id is not set by hand, and "Done when" was really run on `main` |
| one of smoke and `alone` at a time; neither starts beside a title, testbed or cargo test; `--quiet` | that GPU tests go through `alone`; builds, plain tests or `victory run` started during a run |
| load recorded in `run.txt` and the digest | that contended timings are thrown away |
| notes sections present, at most 150 lines | that notes are updated after each step |
