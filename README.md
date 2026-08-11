# SHER Graphics

[![CI](https://github.com/Mullassery/SHER-Graphics/actions/workflows/ci.yml/badge.svg)](https://github.com/Mullassery/SHER-Graphics/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-SHER%20Graphics%20License-blue)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange)](./Cargo.toml)

Native graphics architecture for [SHER Kernel](https://github.com/Mullassery/SHER-KERNEL), built on the same philosophy [Aurora](https://github.com/Mullassery/aurora) applies to the desktop layer:

> Compatibility at the boundary, freedom underneath.

## Why this exists

Most OS graphics stacks are built as a Vulkan or OpenGL implementation first, with anything native bolted on afterward, if it exists at all. SHER Graphics inverts that. OpenGL and Vulkan applications run through Mesa unmodified, treated as compatibility-facing interfaces at the boundary. Underneath them, SHER Graphics defines its own native runtime, GPU abstraction, memory model, and synchronization primitives, designed for SHER Kernel specifically and not constrained by decades of Vulkan/OpenGL/Direct3D API-versioning baggage.

See [`ARCHITECTURE.md`](./ARCHITECTURE.md) for the full design: philosophy, why this isn't "just another Vulkan," the native API/runtime/abstraction layering, Mesa integration strategy, driver migration path, and the phased roadmap.

## What exists today

Design phase, but unusually testable for one: 51 tests pass with zero GPU hardware in the loop, because `gpu_abstraction::SoftwareGpuDriver` is a hardware-independent reference driver, the same role `llvmpipe`/`lavapipe` play for Mesa (see `ARCHITECTURE.md` section 21).

- Native graphics API: device, resource, pipeline, command-stream, and timeline object model (`graphics_api`)
- GPU abstraction layer with multi-GPU support and an in-process software reference driver (`gpu_abstraction`)
- Command-stream validation, GPU fault detection, and isolated per-context fault recovery, so one context's failure doesn't take down others
- Every capability-gated operation (device creation, submission, admin/recovery) reuses SHER Kernel's existing capability and tier security model, not a graphics-specific one, and every grant/denial flows into an audit trail
- `cargo run -p graphics_runtime --example triangle` walks the entire stack end to end: device → shaders → pipeline → resource → validated command stream → submit → wait → present

No hardware backend yet; that's the next phase. The roadmap and effort estimate in `ARCHITECTURE.md` section 21 lays out what's realistically implementable first versus what needs research before committing to an approach.

## Architecture at a glance

```
OpenGL apps ──┐
Vulkan apps ──┼──→ SHER Graphics Runtime → SHER GPU Abstraction → SHER Kernel → Hardware
Native apps ──┘
```

`ARCHITECTURE.md` covers the full layering: the native API/runtime/abstraction split, the Mesa winsys/WSI integration strategy, the security and capability model, and the phased migration path from LKI-hosted Linux GPU drivers to native SHER drivers.

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
- [`SHER-Kernel`](https://github.com/Mullassery/SHER-KERNEL) checked out as a **sibling directory** (`../SHER-Kernel` relative to this repo) — `sher_common`, `sher_objectmodel`, `sher_security`, `hal`, and `gpu_driver` are consumed via relative path dependencies, not published crates yet

Full setup and troubleshooting: [`INSTALLATION.md`](./INSTALLATION.md).

## Building

```bash
./scripts/install.sh   # checks toolchain + sibling checkout, builds, tests
# or, if both repos are already cloned as siblings:
cargo build
cargo test
```

See the [`Makefile`](./Makefile) (`make help`) for the rest of the dev workflow: `fmt`, `clippy`, `doc`, `clean`.

## Contributing

This is early, architecture-defining work, so the most valuable contributions right now are design feedback and issues, not large PRs against a surface that's still moving. Open an issue if you find a boundary violation, a gap between `ARCHITECTURE.md` and the implementation, or a case the capability model doesn't handle correctly.

## License

Free to use with explicit attribution — see [`LICENSE`](./LICENSE).
