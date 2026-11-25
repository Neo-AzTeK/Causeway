# Problem list

Ranked by effect on the picture, then stability, then speed. One row is one
overall problem, worked by at most one session
(`docs/problem-sessions.md`). `cargo xtask problem list` adds who holds what on
this machine. This table changes rarely: when the owner re-ranks, or a diff adds,
splits or closes a problem. Live state, evidence with run folders and shader
hashes stay in the machine-local registry, never here (rule #1).

`owner` is `here`, or who else owns it: no session here claims it.
**No rank below 1 is proven.** Nothing yet shows which loss keeps the 3D scene
from showing; the order is a guess from dropped-work counts, which do not
measure what the picture loses. Measured figures stay on this machine, not here (rule #1).

| rank | id | owner | effect | how sure |
|------|----|-------|--------|----------|
| 1 | `scene-blocker` | here | which loss keeps the 3D scene from showing is unknown; the answer ranks the rest | unknown |
| 2 | `wave64-refusals` | here | compute needing waves of 64 is refused: most refused pipelines in world runs | counts measured; picture effect a guess |
| 3 | `descriptor-slots` | other AI | draws and dispatches dropped for lack of descriptor slots; a large share of drops | counts measured (some mislabelled as pipeline losses); picture effect a guess |
| 4 | `mesh-pipeline-invalid` | here | a mesh pipeline the host rejects loses many indirect draws in some runs, almost none in others | cause unknown |
| 5 | `gpu-long-dispatch` | here | long dispatches remove the GPU; held off by a skip switch, and by leaving a fix for indirect dispatches reverted | skip proven to avoid one removal; the second link strongly suggested by one run |
| 6 | `untranslated-shaders` | here | some programs in world runs do not translate (several reasons); one reason may need data only the descriptor heap has | few draws lost; picture effect a guess |
| 7 | `title-crash-breakpoint` | here | the title stopped itself on a failed internal check once, during a long world run | seen once, not reproduced since |
| 8 | `compile-backlog` | here | on a cold shader cache, work is dropped for a long stretch while pipelines are made | measured on cold cache only; warm effect small |
| 9 | `frame-preparing-cost` | here | CPU preparation is a large share of each world frame | measured, some runs not on a quiet machine |
| 10 | `texture-refresh-menus` | here | on the GTX 1070 slow texture refresh stalls the menus, so unattended runs miss the world | measured on that adapter, older build |
| 11 | `gpu-tests-hang` | here | `victory-gpu` tests can hang or read zeros on a real GPU, which weakens every proof | may be mostly fixed (guess) |

## Finding what keeps the 3D scene from showing

The method for `scene-blocker`. It is a question, so its "Done when" is an answer
the owner accepts, not a visible scene.

1. Name one reference frame or moment that should show 3D geometry, and how a
   run reaches it. Say whether it is the intro or the playable world: geometry
   in one does not prove the other.
2. Capture it with all its render targets (`VICTORY_CAPTURE_FRAMES`,
   `VICTORY_DUMP_TARGETS=1`; dumps stay outside the repository).
3. Walk back from the presented image to the earliest target that is flat or
   wrong although the title writes it. Whether the log can say which draws and
   dispatches write each target is not verified; adding that is part of the work.
4. List what was dropped or substituted among those writers and their inputs.
   Rule out the others: clears, formats, copies, culling inputs, constants.
5. A candidate counts only with causal evidence: restoring it changes the
   target, or removing equivalent working work reproduces the loss. A large
   dropped-draw count is not evidence.
6. Re-rank this table by what the chain shows, and mark what is still a guess.
