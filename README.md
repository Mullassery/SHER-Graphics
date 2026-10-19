# SHER Graphics

Native graphics architecture for [SHER Kernel](https://github.com/Mullassery/SHER-KERNEL): a pure-Rust software GPU reference driver, a capability-secured graphics runtime, and a real Vulkan/`ash` FFI backend.

[![License: Apache-2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](./LICENSE)
[![CI](https://github.com/Mullassery/SHER-Graphics/actions/workflows/ci.yml/badge.svg)](https://github.com/Mullassery/SHER-Graphics/actions/workflows/ci.yml)

## What it is

SHER Graphics is a pure-Rust graphics stack designed around SHER Kernel's capability/security model rather than as a Vulkan wrapper with native bits bolted on. It is a five-crate workspace consisting of a hardware-independent software GPU simulation (the bulk of the code) and a genuine Vulkan/`ash` backend developed alongside it, not as a shim over the software driver.

Most of the workspace plays the role `llvmpipe`/`lavapipe` play for Mesa: a correctness baseline the rest of the stack is built and tested against. About 59 tests pass with zero GPU hardware, zero Vulkan, and zero Mesa dependency in the loop (`cargo test --workspace --exclude vulkan_backend`).

## Features / Highlights

- **Native graphics API** — device, resource, pipeline, command-stream, and timeline object model (`graphics_api`).
- **GPU abstraction layer** with multi-GPU support and an in-process software reference driver (`gpu_abstraction`).
- **Command-stream validation, GPU fault detection, and isolated per-context fault recovery** — one context's failure does not take down others.
- **Driver panic containment** — `graphics_runtime::submit` catches a panic inside driver code (`catch_unwind`), marks the device faulted through the same `GpuFault` channel a hardware fault uses, and lets the existing `recover_device` path hot-restart it; a fresh device can be created and used immediately, without crashing the process.
- **Capability-gated operations** — device creation, submission, and admin/recovery reuse SHER Kernel's existing capability and tier model, and every grant/denial flows into an audit trail.
- **Cursor rendering primitives** — `set_cursor_image`, `set_cursor_position`, `show_cursor`, `hide_cursor` own how the cursor is drawn without owning position tracking, focus, or input (those stay in SHER-INPUT/SHER-Display).
- **A real Vulkan backend** (`vulkan_backend`) — genuine `ash` FFI bindings against a real Vulkan loader/ICD: real physical-device enumeration via `vkEnumeratePhysicalDevices` and a real offscreen clear-color render (allocate `VkImage`, record/submit a command buffer, wait on a fence, read pixels back and assert their bytes).
- **End-to-end example** — `cargo run -p graphics_runtime --example triangle` walks device → shaders → pipeline → resource → validated command stream → submit → wait → present.

## Who it's for / Use cases

- **Studying a native GPU abstraction designed around a specific kernel's capability/security model**, instead of adapting Vulkan/OpenGL abstractions after the fact.
- **A hardware-independent software GPU reference** (`gpu_abstraction`'s `SoftwareGpuDriver`) for testing graphics-stack logic without real GPU hardware.
- **Building and testing a real Vulkan path** without requiring a GPU to build the workspace — `vulkan_backend` compiles and tests cleanly with no Vulkan loader installed at all.

## What it is not

- **Not yet a production renderer.** The real Vulkan backend exists but is not wired into `gpu_abstraction::GpuDriver`/`graphics_runtime::GraphicsRuntime`; nothing in the runtime/API layers currently routes through it. Bridging its asynchronous, device-lost-capable semantics into the software driver's synchronous trait shape is tracked follow-up work, not done.
- **Not a Mesa/OpenGL integration yet.** `graphics_compat` (the Mesa winsys/WSI compatibility seam) models the planned Phase A seam but is not backed by a real Mesa build.
- **Not a standalone build.** It depends on `SHER-KERNEL` via a relative path (`../SHER-KERNEL`, case-sensitive on Linux), so that repo must be a sibling directory; there is no crates.io publish target.
- **Not OS-level sandboxing.** Panic containment is in-process only — there is no subprocess/WASM/eBPF isolation in this workspace; the real sandbox mechanism lives in SHER-Kernel's `driver_runtime` and is not yet built.
- **Not vectorized.** The software rasterizer is intentionally scalar (a correctness reference, not a production renderer) — no SIMD yet, tracked as a future enhancement.

## Project status

- **Implemented and tested:** `graphics_api`, `gpu_abstraction`, `graphics_runtime` (including panic containment and fault recovery), `graphics_compat` (thin compatibility seam), and the standalone `vulkan_backend` (device enumeration + offscreen clear-color render).
- **Real but not yet integrated:** `vulkan_backend` is deliberately not wired into the runtime/API layers.
- **Not built:** the Mesa/OpenGL compatibility seam and a native SHER driver backend. See [`ARCHITECTURE.md`](./ARCHITECTURE.md) section 21 and [`ROADMAP_HONEST.md`](./ROADMAP_HONEST.md).
- **No open GitHub issues and no `TODO`/`FIXME` markers in `crates/`** as of the last verification pass.

## Installation

Prerequisites:

- Rust 1.75+.
- [`SHER-KERNEL`](https://github.com/Mullassery/SHER-KERNEL) checked out as a **sibling directory named exactly `SHER-KERNEL`** (`../SHER-KERNEL` relative to this repo).
- *Optional*, for `vulkan_backend`'s tests to exercise a real device instead of skipping: a Vulkan loader + ICD. macOS: `brew install molten-vk vulkan-loader vulkan-tools`. Linux: `mesa-vulkan-drivers`/`vulkan-tools` from your distro (what CI uses). Not required to build.

```bash
git clone https://github.com/Mullassery/SHER-KERNEL.git
git clone https://github.com/Mullassery/SHER-Graphics.git
cd SHER-Graphics
./scripts/install.sh   # checks toolchain + sibling checkout, builds, tests
```

Full setup and troubleshooting: [`INSTALLATION.md`](./INSTALLATION.md).

## Quick start

```bash
# If both repos are already cloned as siblings:
cargo build
cargo test

# Walk the whole software stack end to end:
cargo run -p graphics_runtime --example triangle
```

See the [`Makefile`](./Makefile) (`make help`) for the rest of the dev workflow: `fmt`, `clippy`, `doc`, `clean`.

## Usage

`graphics_runtime` is the runtime crate that instantiates `gpu_driver::GPUDriver` from SHER-Kernel; application-facing integrations target `graphics_api`. As-built architecture diagrams are in [`docs/architecture/README.md`](./docs/architecture/README.md). To run the workspace tests without the Vulkan backend:

```bash
cargo test --workspace --exclude vulkan_backend
```

## Documentation

- [`ARCHITECTURE.md`](./ARCHITECTURE.md) — full design: philosophy, native API/runtime/abstraction layering, Mesa integration strategy, and the phased migration path
- [`INSTALLATION.md`](./INSTALLATION.md) — setup and troubleshooting
- [`ROADMAP_HONEST.md`](./ROADMAP_HONEST.md) — honest status/tech-debt supplement to `ARCHITECTURE.md`
- [`docs/architecture/README.md`](./docs/architecture/README.md) — as-built Mermaid diagrams
- [`crates/vulkan_backend/src/lib.rs`](./crates/vulkan_backend/src/lib.rs) — what is real vs. simulated in the Vulkan backend
- [`CONTRIBUTING.md`](./CONTRIBUTING.md) and [`CODE_OF_CONDUCT.md`](./CODE_OF_CONDUCT.md)
- [`SECURITY.md`](./SECURITY.md) — how to report a vulnerability
- [`CHANGELOG.md`](./CHANGELOG.md)

## Queue status

Shipped, blocked/failed, and rejected work for this repository is documented in
[`QUEUE_STATUS.md`](QUEUE_STATUS.md).

## Contributing

This is early, architecture-defining work, so the most valuable contributions right now are design feedback and issues, not large PRs against a surface that is still moving. Open an issue if you find a boundary violation, a gap between `ARCHITECTURE.md` and the implementation, or a case the capability model does not handle correctly. See [`CONTRIBUTING.md`](./CONTRIBUTING.md) for the full checklist and scope boundaries, and [`CODE_OF_CONDUCT.md`](./CODE_OF_CONDUCT.md).

## Part of the SHER OS family

SHER-Graphics is part of the SHER OS family — an AI-native operating system built from scratch in Rust. The subsystems are designed to work together as a coherent OS stack.

| Tool | Role | What it does |
|---|---|---|
| [SHER-KERNEL](https://github.com/Mullassery/SHER-KERNEL) | Kernel core | Core kernel scaffolding: object model, scheduling, memory, driver-lifecycle concepts, and a Linux Kernel Interface (LKI) compatibility layer. |
| [SHER-Graphics](https://github.com/Mullassery/SHER-Graphics) | Graphics/GPU | **This project** — GPU abstraction and rendering — software simulation plus a real, working Vulkan/MoltenVK backend. |
| [SHER-Display](https://github.com/Mullassery/SHER-Display) | Display/compositor | Display server, compositor, and window management. |
| [SHER-INPUT](https://github.com/Mullassery/SHER-INPUT) | Input | Canonical, backend-independent input event stream — real evdev on Linux, simulated backend for hardware-free testing. |
| [SHER-Aurora](https://github.com/Mullassery/SHER-Aurora) | Design system | GNOME-style design system — tokens, typography, motion, and an automated WCAG contrast audit. |
| [SHER-Process-Explorer](https://github.com/Mullassery/SHER-Process-Explorer) | Observability | Evidence-based Linux process investigation tool — /proc telemetry, bpftrace integration, daemon + CLI + desktop UI. |

Each works standalone; they're complementary by design. [Browse all projects →](https://github.com/Mullassery?tab=repositories)

## License

This project is licensed under the [Apache License 2.0](./LICENSE).
