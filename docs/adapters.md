# Adapters seen so far

What has run the title or the GPU tests. Add a row when a new adapter runs and
update "Last run" after each smoke run. No game data.

| Adapter | Wave | Mesh tier | Binding tier | View heap | Last run |
|---------|------|-----------|--------------|-----------|--------------------|
| NVIDIA RTX 2060 SUPER (Turing, 8 GB) | 32 | native (tier 1) | 3 | large | smoke, 2026-11-25 |
| NVIDIA GTX 1070 (Pascal) | 32 | none (emulated as compute, being built) | unknown | 1,000,000 slots | smoke 12, 2026-11-25 |

## Not yet verified (assume nothing)
- wave64 hardware (AMD), or a driver reporting a 32-to-64 range
- Intel (reported wave ranges can be wrong; see `docs/hardware-range.md`)
- binding tier 1 or 2, small view heaps
- UMA or low-VRAM devices
- the software adapter (`VICTORY_WARP=1`) runs shader tests only
