# SHER Graphics

Native graphics architecture for [SHER Kernel](https://github.com/Mullassery/SHER-KERNEL), built on the same philosophy [Aurora](https://github.com/Mullassery/aurora) applies to the desktop layer:

> Compatibility at the boundary, freedom underneath.

OpenGL and Vulkan applications run through Mesa unmodified, treated as compatibility-facing interfaces. Underneath them, SHER Graphics is free to define its own native runtime, GPU abstraction, memory model, and synchronization primitives — designed for SHER Kernel specifically, not constrained by decades of Vulkan/OpenGL/Direct3D API-versioning baggage.

See [`ARCHITECTURE.md`](./ARCHITECTURE.md) for the full design: philosophy, why this isn't "just another Vulkan," the native API/runtime/abstraction layering, Mesa integration strategy, driver migration path, and the phased roadmap.

## Status

Design phase. Crate skeletons exist, compile, and pass 31 tests; no hardware backend yet. `gpu_abstraction::SoftwareGpuDriver` is a hardware-independent reference driver — the whole stack runs and tests without a GPU, the same role `llvmpipe`/`lavapipe` play for Mesa (see `ARCHITECTURE.md` section 21).

## Workspace

```
crates/
├── graphics_api/       # Native SHER Graphics API — object model + trait, app-facing
├── gpu_abstraction/     # GpuDriver trait (extends SHER-Kernel's hal::HardwareDriver)
├── graphics_runtime/     # SHER Graphics Runtime — implements graphics_api, owns
│                          # resource tracking, memory, command translation,
│                          # timeline sync, and the presentation bridge
└── graphics_compat/       # Mesa winsys/WSI + DRM-ioctl-compatibility seam (thin)
```

## Prerequisites

- Rust 1.75+
- [`SHER-Kernel`](https://github.com/Mullassery/SHER-KERNEL) checked out as a **sibling directory** (`../SHER-Kernel` relative to this repo) — `sher_common`, `sher_objectmodel`, `hal`, and `gpu_driver` are consumed via relative path dependencies, not published crates yet

Full setup and troubleshooting: [`INSTALLATION.md`](./INSTALLATION.md).

## Building

```bash
./scripts/install.sh   # checks toolchain + sibling checkout, builds, tests
# or, if both repos are already cloned as siblings:
cargo build
cargo test
```

See the [`Makefile`](./Makefile) (`make help`) for the rest of the dev workflow — `fmt`, `clippy`, `doc`, `clean`.

## License

Free to use with explicit attribution — see [`LICENSE`](./LICENSE).
