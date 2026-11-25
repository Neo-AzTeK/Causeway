# GPU-facing diff: review checklist

Answer in the review, in writing. "None" is an answer.

1. **Which capability does this assume?** (wave size, binding tier, mesh tier,
   view-heap size, format support, memory budget, UMA, atomics). Name the
   `Caps` field. A literal `32`, `64`, a vendor id or an adapter name outside
   `caps.rs` is a finding.
2. **Where is the fallback tested?** Name the test that runs the path taken
   when the capability is absent (fake caps: `caps::reference`, grid in
   `caps/tests.rs`). No test, no merge.
3. **What does a device without it see?** A clean refusal
   (`the host GPU lacks <feature>: needs <x>, has <y>`), never a wrong value or
   a hang.
4. **Which adapters ran it?** List them in the commit message and `docs/adapters.md`; list the classes
   not verified (wave64, no mesh shaders, small view heap, UMA, low VRAM).
5. **Is the conservative portable path the default?** A faster path needs a
   capability proven by the query, not by the owner's GPU.
6. **Timing.** A speed claim has before and after numbers on a named adapter.
7. **Rule #1.** No game-derived data in the diff, tests, logs or notes.
