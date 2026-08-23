# SHER Graphics

[![CI](https://github.com/Mullassery/SHER-Graphics/actions/workflows/ci.yml/badge.svg)](https://github.com/Mullassery/SHER-Graphics/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-SHER%20Graphics%20License-blue)](./LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange)](./Cargo.toml)

Native graphics architecture for [SHER Kernel](https://github.com/Mullassery/SHER-KERNEL), built on the same philosophy [Aurora](https://github.com/Mullassery/aurora) applies to the desktop layer:

> Compatibility at the boundary, freedom underneath.

## Why this exists

Most OS graphics stacks are built as a Vulkan or OpenGL implementation first, with anything native bolted on afterward, if it exists at all. SHER Graphics inverts that: the goal is a native runtime, GPU abstraction, memory model, and synchronization primitives designed for SHER Kernel specifically, not constrained by decades of Vulkan/OpenGL/Direct3D API-versioning baggage — with OpenGL and Vulkan support as compatibility-facing interfaces at the boundary, layered on top once there's a real driver underneath to target. That Mesa integration (`graphics_compat`, section 5 of `ARCHITECTURE.md`) is **planned, not built**: see "What exists today" below for exactly what runs now.

See [`ARCHITECTURE.md`](./ARCHITECTURE.md) for the full design: philosophy, why this isn't "just another Vulkan," the native API/runtime/abstraction layering, the Mesa integration strategy, the driver migration path, and the phased roadmap.

## What exists today

Two things, honestly labeled, alongside each other:

**A pure-Rust, zero-`unsafe`, zero-FFI software GPU simulation** — the bulk of this workspace. It plays the same role `llvmpipe`/`lavapipe` play for Mesa: a hardware-independent correctness baseline the rest of the stack is built and tested against. 57 tests pass with no GPU hardware, no Vulkan, and no Mesa dependency in the loop (`cargo test --workspace --exclude vulkan_backend`).

- Native graphics API: device, resource, pipeline, command-stream, and timeline object model (`graphics_api`)
- GPU abstraction layer with multi-GPU support and an in-process software reference driver (`gpu_abstraction`)
- Command-stream validation, GPU fault detection, and isolated per-context fault recovery, so one context's failure doesn't take down others
- Every capability-gated operation (device creation, submission, admin/recovery) reuses SHER Kernel's existing capability and tier security model, not a graphics-specific one, and every grant/denial flows into an audit trail
- Cursor rendering primitives (`set_cursor_image`, `set_cursor_position`, `show_cursor`, `hide_cursor`) that own how the cursor is drawn without touching position tracking, focus, or input, which stay in SHER-Input/SHER-Display
- `cargo run -p graphics_runtime --example triangle` walks the entire stack end to end: device → shaders → pipeline → resource → validated command stream → submit → wait → present

**A real Vulkan backend** (`vulkan_backend`), new alongside the simulation — genuine `ash` FFI bindings against a real Vulkan loader/ICD, not a wrapper around the software driver:

- Real physical-device enumeration via `vkEnumeratePhysicalDevices`/`vkGetPhysicalDeviceProperties` — verified against MoltenVK on macOS, reporting an actual `Apple M5` GPU, no synthetic data
- A real offscreen clear-color render: allocates a real `VkImage`, records and submits a real command buffer (`vkCmdClearColorImage` → `vkCmdCopyImageToBuffer`), waits on a real fence, and reads the resulting pixels back from actual GPU/ICD memory — the crate's tests assert the bytes read back match the requested color, so this proves real execution, not just that the calls didn't error
- Deliberately **not** wired into `gpu_abstraction::GpuDriver`/`graphics_runtime::GraphicsRuntime` yet — bridging a real, asynchronous, device-lost-capable Vulkan device into a trait shaped around the software driver's synchronous semantics is real design work, tracked as follow-up, not claimed as done
- Builds and `cargo test`s cleanly with **no Vulkan loader installed at all** (this repo's CI installs Mesa's lavapipe so the real-hardware paths execute instead of skipping, but that's not required for the workspace to build): `ash`'s `loaded` feature `dlopen()`s the loader at runtime rather than linking against it, and every entry point reports unavailability via `Result` instead of panicking

Full detail on exactly what's real vs. simulated, and the reasoning behind not integrating them yet, is in [`crates/vulkan_backend/src/lib.rs`](./crates/vulkan_backend/src/lib.rs)'s module docs and `ARCHITECTURE.md`'s Vulkan-backend addendum (end of the document).

The Mesa/OpenGL compatibility seam and a native SHER driver backend (as opposed to a real-Vulkan/MoltenVK backend used directly) remain unbuilt; the roadmap and effort estimate in `ARCHITECTURE.md` section 21 lays out what's realistically implementable next versus what needs more research.

## Architecture at a glance

```
OpenGL apps ──┐
Vulkan apps ──┼──→ SHER Graphics Runtime → SHER GPU Abstraction → SHER Kernel → Hardware   (planned)
Native apps ──┘

Real today, separately: vulkan_backend (ash) → real Vulkan loader/ICD → real GPU
```

`ARCHITECTURE.md` covers the full layering: the native API/runtime/abstraction split, the Mesa winsys/WSI integration strategy, the security and capability model, and the phased migration path from LKI-hosted Linux GPU drivers to native SHER drivers.

## Cross-repo compatibility (verified, whole family)

This repo is part of a 5-repo family under the Mullassery org, expected to
be cloned as sibling directories: `SHER-Kernel`, `SHER-Graphics` (this
repo), `SHER-Display`, `SHER-Input`, and `Aurora` (GitHub: `SHER-Aurora`).
Actual Cargo-level coupling, confirmed by reading every `Cargo.toml` in the
family:

- **SHER-Kernel** — foundation. This repo depends on it (`sher_common`,
  `sher_objectmodel`, `sher_security`, `hal`, `gpu_driver`) via relative
  path (`../SHER-Kernel/crates/...`), so both repos must be sibling
  directories.
- **SHER-Input** — standalone, no dependency on this repo or vice versa.
- **SHER-Display** — depends on this repo (`graphics_api`,
  `gpu_abstraction`, `graphics_runtime`, `graphics_compat`) in addition to
  SHER-Kernel and SHER-Input.
- **Aurora** — zero Cargo-level coupling to this repo. Standalone GTK/Qt/Web
  toolkit; shared "SHER" naming is organizational only.

Both edition (2021) and the dependency contract are verified current: a
from-scratch `cargo build --workspace` in this repo against SHER-Kernel's
current state compiles clean (`graphics_api`, `vulkan_backend`,
`gpu_abstraction`, `graphics_runtime`, `graphics_compat` all build); a
from-scratch `cargo build --workspace` + `cargo test --workspace` in
SHER-Display against this repo's current state also compiles and passes
56/56. This repo is the sole owner of `ash` (Vulkan bindings, 0.38) in the
family — that dependency isn't leaked across the repo boundary via public
APIs, so there's no divergent-version risk downstream. `graphics_runtime`
is the crate that actually instantiates `gpu_driver::GPUDriver` from
SHER-Kernel; SHER-Display deliberately does not (see SHER-Display's
`outputs` crate), consuming only value types instead — boundary discipline
holds across the live chain.

## Workspace

```
crates/
├── graphics_api/       # Native SHER Graphics API — object model + trait, app-facing
├── gpu_abstraction/     # GpuDriver trait (extends SHER-Kernel's hal::HardwareDriver)
├── graphics_runtime/     # SHER Graphics Runtime — implements graphics_api, owns
│                          # resource tracking, memory, command translation,
│                          # timeline sync, and the presentation bridge
├── graphics_compat/       # Mesa winsys/WSI + DRM-ioctl-compatibility seam (thin; models
│                          # the Phase A seam, not yet backed by a real Mesa build)
└── vulkan_backend/        # Real ash/Vulkan FFI: device enumeration + offscreen
                           # clear-color render against a real loader/ICD. Standalone —
                           # not yet wired into gpu_abstraction::GpuDriver.
```

## Prerequisites

- Rust 1.75+
- [`SHER-Kernel`](https://github.com/Mullassery/SHER-KERNEL) checked out as a **sibling directory** (`../SHER-Kernel` relative to this repo) — `sher_common`, `sher_objectmodel`, `sher_security`, `hal`, and `gpu_driver` are consumed via relative path dependencies, not published crates yet
- **Optional**, for `vulkan_backend`'s tests to exercise a real device instead of skipping: a Vulkan loader + ICD. macOS: `brew install molten-vk vulkan-loader vulkan-tools`. Linux: `mesa-vulkan-drivers`/`vulkan-tools` from your distro (what CI uses). Not required to build — see `crates/vulkan_backend/src/lib.rs`.

Full setup and troubleshooting: [`INSTALLATION.md`](./INSTALLATION.md).

## Building

```bash
./scripts/install.sh   # checks toolchain + sibling checkout, builds, tests
# or, if both repos are already cloned as siblings:
cargo build
cargo test
```

See the [`Makefile`](./Makefile) (`make help`) for the rest of the dev workflow: `fmt`, `clippy`, `doc`, `clean`.

## Known Issues

- `vulkan_backend` is real but standalone: it is not yet wired into
  `gpu_abstraction::GpuDriver`/`graphics_runtime::GraphicsRuntime`, so
  nothing in the runtime/API layers currently routes through it. Bridging
  its asynchronous, device-lost-capable semantics into the software
  driver's synchronous trait shape is tracked as follow-up work, not done.
- `graphics_compat` (the Mesa winsys/WSI compatibility seam) models the
  planned Phase A seam but is not backed by a real Mesa build yet.
- This workspace depends on `SHER-Kernel` via relative path (`../SHER-Kernel`),
  not a published crate, so it cannot be built or published standalone —
  no crates.io registry drift check applies to a systems component with no
  independent publish target.
- No open GitHub issues and no `TODO`/`FIXME` markers in `crates/` as of
  this pass.

## Contributing

This is early, architecture-defining work, so the most valuable contributions right now are design feedback and issues, not large PRs against a surface that's still moving. Open an issue if you find a boundary violation, a gap between `ARCHITECTURE.md` and the implementation, or a case the capability model doesn't handle correctly.

## License

Free to use with explicit attribution — see [`LICENSE`](./LICENSE).
