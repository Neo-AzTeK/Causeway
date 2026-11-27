# Architecture

```
 user's files (outside repo)             VIctory
 ───────────────────────────             ─────────────────────────────────────────
 game assets ──────────────────────────► victory-pipeline + victory-game
                                         (victory-formats, victory-shader)
                                                    │
                                                    ▼
                                         cache_dir: converted data
                                                    │
 game executable ─► victory-loader ────► title code (native x86-64)
                    prepare, map, bind      │ imports
                                            ▼
                                         victory-hle ── host Windows (shared APIs)
                                            │ D3D12.X
                                            ▼
                                         victory-gpu ─► host D3D12
                                            │ shaders at pipeline creation
                                            ▼
                                         victory-shader ◄─► cache_dir
```

Shaders translate at pipeline creation from title-supplied bytes regardless of source (executable, archive, memory). Ahead-of-time conversion (`victory translate --cache`) merely pre-warms the same cache under identical keys; `--pipelines` also pre-populates the host pipeline library from engine creation streams (`victory-game` builds, `victory-gpu` reads identically to title streams) so runs match them by name.

## Crates

| Crate | Role |
|-------|------|
| `victory-core` | config, asset-policy guard, logging |
| `victory-pe` | read-only PE32+ and import-library analysis |
| `victory-loader` | prepare, map, bind, and start title image |
| `victory-hle` | console API implementations, host pass-through policy |
| `victory-gpu` | D3D12.X objects, command lists, and GPU memory → host D3D12 |
| `victory-shader` | Xbox shader containers → host shaders |
| `victory-formats` | GPU texture layouts (sizes, tiling, detiling), BCPack, audio codecs |
| `victory-pipeline` | converter trait, content-addressed cache |
| `victory-game` | title-specific formats, build identities, hooks |
| `apps/victory` | CLI front end |
| `xtask` | asset-policy check, git hooks |
| `tests/fixtures/hle-probe` | stand-in "title" (in-house code) for loader tests |

All crates except `victory-game` are platform-level and testable against any Xbox Series title.

## Import resolution

Each title `dll!function` import resolves in priority order:

1. `victory-hle::modules` implementation (always wins).
2. Host function, if `HostPolicy` permits the DLL.
3. Export from a title-bundled DLL alongside the executable, mapped like the executable (only if neither 1 nor 2 handles the DLL).
4. Generated stub naming the import on call (`--unimplemented trap` breaks into debugger and aborts; `zero` logs once and returns `0`).

`LoadLibrary*` on implemented console-only DLLs (e.g., `xgameruntime.dll`) returns a pseudo-handle resolved by `GetProcAddress` against our overrides. `LoadLibrary*` on title DLLs maps (if unimported) and returns the mapped image. Name lookups enforce `HostPolicy` like import binding, and `GetProcAddress` on host DLLs returns our overrides where bound. `QueryApiImpl` provides runtime objects by provider and interface ID (`victory-hle::modules::xgameruntime` lists areas; online areas behave as an offline console). Game saves reside under `VICTORY_DATA_DIR`.

`victory-hle::modules::files` hooks all path-taking title imports (`kernel32` and C/C++ runtime), remapping console virtual-drive paths to backing host folders (including persistent local storage under `VICTORY_DATA_DIR`) while passing other paths through unchanged.

Because an executable accesses static TLS at slot 0 while DLL-mapped images receive another slot, `victory-loader::tls` redirects static TLS loads. `victory run --execute` installs a crash reporter logging faults as `module+rva` and unwinding the faulting thread's stack (`victory-loader::crash`).

`victory census` reports import classification statically without mapping title DLLs (`victory run` without `--execute` maps them).

## Shaders

`victory_shader::ir::build` lowers native programs to 1:1 instruction SSA. `victory_shader::structure` eliminates `EXEC`, converting lane masks to per-lane booleans, wave-wide mask tests to per-lane branches, and guarding vector instructions only where reachable with `EXEC` clear. `victory_shader::range::build` re-runs both passes when proving index bounds on register-relative moves (`v_movrel*`) in per-lane form, restricting moves to reachable registers. `victory_shader::resource` traces descriptors and scalar memory addresses to root parameters (table, heap, root descriptor, or constant). Waterfall-loop descriptors read via `v_readfirstlane_b32` trace back along structurized control flow to per-lane sources (`v_cndmask_b32` or `if`/`else` assignments) for single-pass non-uniform heap access. `victory translate` runs all discovered shaders (containers, SGD5 files) through these passes and lists unsupported constructs.
