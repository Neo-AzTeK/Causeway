# victory-hle: console API implementations

Import order: our modules, then the host if `HostPolicy` allows the DLL, then a
DLL the title ships, then a stub (see `docs/architecture.md`).

Add an API: write an `extern "system"` function named exactly like the import
in its area's module under `src/modules/`, and list it with `hle_export!` in
that module's `module()`. A new module also goes in `modules/mod.rs` (`all`).

| Topic | Module in `src/modules/` |
|-------|--------|
| runtime objects by interface id | `xgameruntime`, then `xuser`, `xstore`, `xpackage`, `xgamesave`, `xgameui`, `xsystem`, `xgame`, `xnetworking` |
| async blocks and task queues | `xasync` |
| files and virtual drives | `files`, `storage` |
| streaming, zlib, BCPack | `dstorage` |
| memory | `xmem` |
| audio, video, speech | `audio`, `apu`, `dsp`, `media`, `speech`; codecs in `victory-formats` |
| input | `gameinput` |
| network (offline by design) | `net` |
| graphics entry points | `d3d12x`, `xg`, `display` |
| process, lifecycle | `kernel`, `lifecycle` |

- Pointers and sizes come from the title: check null, overflow and short
  buffers, and give each `unsafe` block its `SAFETY:` comment.
- Saves must survive a kill at any step.
- Tests must not need game files: paths come from environment variables and
  the test skips when unset.
