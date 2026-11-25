# victory-shader: console shader containers to PC DXIL (SM 6.6)

Pipeline, in order:

| Step | File in `src/` |
|------|------|
| container, parts | `dxbc.rs`, `dxil.rs` |
| native GFX10 program | `native.rs`; decoding in `gfx10/` (`opcodes`, `fields`, `sp3` text) |
| SSA, one inst per machine inst | `ir.rs` |
| EXEC removed, per-lane booleans | `structure.rs` |
| descriptors to root parameters, waterfalls | `resource.rs` |
| input/output raising | `io.rs` |
| whole-wave sections | `emit/wave.rs` |
| instructions to DXIL | `emit/lower.rs` |
| module, bitcode, container | `emit/shader.rs`, `emit/bitcode.rs`, `emit/container.rs` |

Entry point: `emit::translate`.

Where an instruction lives in `emit/lower.rs` (Grep `fn <name>`): scalar
`sop`; scalar memory `smem`, `handle`, `heap_index`; vector `valu_op`,
`valu16_op`, `packed16`, `integer`, `compare`, `dpp`, `interp`; LDS `ds`;
`flat`; `image`; `buffer`; `export`. Operands: `operand`, `src`, `fsrc`,
`sdwa_src`. Writes: `write`, `write_f`, `write_i`.

- What cannot be expressed is `Missing` with a reason, never a wrong value.
- A new instruction needs a `valu_tests.rs` case; per-lane descriptors go in
  `waterfall_tests.rs`. GPU tests there take the `ONE_AT_A_TIME` lock.
- `VICTORY_WARP=1` runs shader tests on the software adapter. It is not
  `VICTORY_D3D12_WARP`, which is for the running title (diagnostics builds).
- `lower.rs` and its tests: one writer at a time.
- Check wave32 and wave64, and a lane with its EXEC bit clear.
- `victory translate` takes the census over the user's own files: programs
  gained and lost. Without them, say the census was not run.
