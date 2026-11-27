# Problem list and todo

What to do next, and the ranked problems behind it. **Update it whenever a task
ends:** tick or drop what is done, add what was found, and move what comes next
to the top. Measured figures, run folders and shader hashes stay on the
developer's machine, never here (rule #1).

Goal: **one correct frame** (`docs/agent-workflow.md`).

## Todo, next first

Each item names its problem in the ranked table below.

- [ ] Find the pass that should put lit people and ground into the scene's
      colour before tonemap (`scene-blocker`). Traced on the newest build:
      most surfaces have no base colour in the main pass, the earliest wrong
      step, as before (empty texture slots, the descriptor side). Surfaces
      that do have colour stay near black in the scene's colour before
      tonemap, while one lighting pass draws them lit: on near geometry that
      pass lights, the scene's colour is hundreds of times darker, almost
      everywhere. Tonemap and upscale keep the picture they get, so this
      second loss is in lighting or composition. Measured in one frame of
      one shot; not pinned to one pass, no causal test yet
- [ ] Owner decides how the original project's later updates come in: one
      commit at a time, or squashed; cleaned either way
- [ ] Game run that includes the newest shader fix (lane mask joined with data)
      and shows the picture: ran on 2026-11-27 with dumps; surfaces still
      black (item above); its own effect not separated
- [ ] Run the `victory-gpu` tests with the new shader changes (`gpu-tests-hang`
      may get in the way)
- [ ] Mesh programs whose outputs pass the host's size limit: see whether
      mesh-as-compute, which has no such limit, can be chosen per program
      (`untranslated-shaders`). Lowering the declared maxima was tried and
      does not help
- [ ] Remaining shader refusals with a lead (`untranslated-shaders`): lane mask
      read as data (the AMD fork's `d1f883e`; its switch must be in the shader
      cache key, or go), `v_permlanex16_b32` (`fxceless/victory-fork`)
- [ ] Waves of 64: the unmerged branch with several console waves per group
      (`wave64-refusals`; needs the owner's OK to take it over)
- [ ] Refusals with no fix found yet (`untranslated-shaders`): structured
      buffer of unknown stride; a descriptor read as data (first settle whether
      it is descriptor work, which another owner has)
- [ ] Look for fixes in forks not yet checked: `avatarmanz/VIctory-7900XTX`,
      `DexNom/VIctory`
- [ ] Reach one fixed test scene that every run can repeat (`scene-blocker`)
- [ ] When shader changes stop: make pipelines ahead of time again, with the
      same build that plays (`compile-backlog`)
- [ ] Frame replay tool, if the owner chooses it
- [ ] Speed, after the picture: look at waiting for the GPU after every batch
      sent, and at the copies between the two views of memory; the gain is not
      measured (`frame-preparing-cost`)

## Done lately

Not seen to change the picture yet. Where each ran: `docs/adapters.md`.

- [x] Loop limits by shader stage (`gpu-long-dispatch`)
- [x] Shader flag for UAVs outside pixel and compute (validator refusals)
- [x] Compute shaders with implicit derivatives pass the validator
- [x] Lane mask joined with data at a uniform join
- [x] Render-target dumps no longer stop the game: a submission kept the
      lock of the GPU detiling's lists while the dumps at its end submitted
      again (it stopped the title at the first captured frame since
      `a69a361`); one game run with dumps ran its full time

## Ranked problems

Ranked by effect on the picture, then stability, then speed. One row is one
overall problem, worked by at most one session
(`docs/problem-sessions.md`). `cargo xtask problem list` adds who holds what on
this machine. Ids and ranks change only when the owner re-ranks, or a diff adds,
splits or closes a problem; `cargo xtask problem` reads this table, so keep its
first two columns and number the rows from 1.

**No rank below 1 is proven.** Nothing yet shows which loss keeps the 3D scene
from showing; the order is a guess from dropped-work counts, which do not
measure what the picture loses.

| rank | id | effect | how sure |
|------|----|--------|----------|
| 1 | `scene-blocker` | which loss keeps the 3D scene from showing is unknown; the answer ranks the rest | unknown |
| 2 | `wave64-refusals` | compute needing waves of 64 is refused: most refused pipelines in world runs | counts measured; picture effect a guess |
| 3 | `descriptor-slots` | draws and dispatches dropped for lack of descriptor slots; a large share of drops | counts measured (some mislabelled as pipeline losses); picture effect a guess |
| 4 | `mesh-pipeline-invalid` | a mesh pipeline the host rejects loses many indirect draws in some runs, almost none in others | cause unknown |
| 5 | `gpu-long-dispatch` | long dispatches remove the GPU; held off by loop limits, a skip switch, and by leaving a fix for indirect dispatches reverted | skip proven to avoid one removal; the second link strongly suggested by one run; loop limits' effect not recorded |
| 6 | `untranslated-shaders` | some programs do not translate or are refused by the validator (several reasons); one reason may need data only the descriptor heap has | refusals counted; picture effect a guess |
| 7 | `title-crash-breakpoint` | the title stopped itself on a failed internal check once, during a long world run | seen once, not reproduced since |
| 8 | `compile-backlog` | on a cold shader cache, work is dropped for a long stretch while pipelines are made | measured on cold cache only; warm effect small |
| 9 | `frame-preparing-cost` | CPU preparation is a large share of each world frame | measured, some runs not on a quiet machine |
| 10 | `texture-refresh-menus` | on the GTX 1070 slow texture refresh stalls the menus, so unattended runs miss the world | measured on that adapter, older build |
| 11 | `gpu-tests-hang` | `victory-gpu` tests can hang or read zeros on a real GPU, which weakens every proof | may be mostly fixed (guess) |
| 12 | `device-methods-missing` | the title calls device methods that are not implemented; what it loses is unknown (rank not confirmed by the owner) | seen in run reports, effect unknown |

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
