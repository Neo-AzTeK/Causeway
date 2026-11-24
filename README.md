# Causeway

Runs Grand Theft Auto VI (Xbox Series X|S) natively on Windows, from files you
supply. The title's x86-64 code runs unchanged in our process; Causeway
supplies the platform under it and recompiles its shaders for Direct3D 12.

A fork of VIctory: the crates, the binary (`victory`) and the variables keep
that name. No game files are included.

## Status (2026-11-24)

**Not playable.**

| Part | State |
|------|-------|
| Boot, setup screens, menus, intro video | work at 30 fps |
| Saves, pad and keyboard input, menu sound | work |
| World | loads, then about 0.5 fps on an RTX 2060 SUPER; credits draw, the 3D scene does not |
| Network | offline by design |
| Ray tracing, HDR, scaling, audio reverb | missing |

Some compute programs need 64-lane waves, which NVIDIA GPUs lack.

> **Warning.** Entering the world can hang the GPU, and Windows then resets
> the display driver. A [diagnostics build](docs/debugging.md) with
> `VICTORY_D3D12_WARP=1` renders on the software adapter instead.

## Requirements

- Windows 10/11 x64 and a CPU with AVX2
- a Direct3D 12 GPU with shader model 6.6, and mesh shaders for the world
- Rust (the version is pinned in `rust-toolchain.toml`)
- several GiB of free Windows commit; on error 1455 in `crashes.log`, close
  applications or enlarge the paging file

## Build and run

```powershell
copy victory.example.toml victory.toml   # then set your paths, outside the repo
cargo run -p victory -- config           # check them
cargo build --release -p victory
target\release\victory.exe run <game dir>\<the title's exe> --execute
```

Saves, settings and `crashes.log` are in `%LOCALAPPDATA%\victory\data` by
default. `victory --help` lists the other commands.

The host uses the high-performance GPU (the first hardware adapter DXGI ranks
so). `VICTORY_D3D12_ADAPTER=<index|name>` picks another one, by 0-based DXGI
index or a case-insensitive part of its name.

`victory translate --cache [--pipelines]` translates every shader the title
can ask for ahead of time (minutes, about 1.3 GB; again after a change of the
translator), and with `--pipelines` also makes the 27,000 pipelines of its
shader pack into a host pipeline library (some 20 minutes, belongs to the GPU
and driver it was made on). Without it they are made at first use and draws
wait.

Upscaling (Series S mode): to run AMD FidelityFX's PC upscaler instead of the
console build the title ships, point `VICTORY_FIDELITYFX_DIR` (or
`[paths].fidelityfx_dir`) at the
[FidelityFX SDK](https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK)'s
`Kits/FidelityFX/signedbin` folder (`amd_fidelityfx_upscaler_dx12.dll`).

## Controls

XInput pads. With none connected the keyboard is a pad while the window has
focus:

| Keys | Pad |
|------|-----|
| W A S D / I J K L | left / right stick |
| arrows | D-pad |
| Enter, Backspace, X, Y | A, B, X, Y |
| Q / E, Z / C | shoulders, triggers |
| F / H | stick clicks |
| Esc, Tab | Menu, View |

## Development

```powershell
cargo xtask install-hooks                # asset-policy check on every commit
cargo test --workspace                   # needs no game data
```

- [docs/architecture.md](docs/architecture.md): how it works
- [docs/debugging.md](docs/debugging.md): debug variables, unattended runs
- [docs/asset-policy.md](docs/asset-policy.md): nothing from the game enters
  the repository
- [CLAUDE.md](CLAUDE.md): rules for coding agents

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
