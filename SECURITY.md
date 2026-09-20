# Security Policy

## What this project is, security-wise

SHER Graphics is pre-production, in-memory simulation code: the bulk of the
workspace (`graphics_api`, `gpu_abstraction`, `graphics_runtime`,
`graphics_compat`) is a pure-Rust, zero-`unsafe`, zero-FFI software GPU
model with no network exposure, no file I/O, and no privilege boundary of
its own — it runs entirely inside a host process's memory. It is **not**
hardened, audited, or fit for production use as of this writing. Treat any
capability/security-model behavior here as a design exercise mirroring
`SHER-Kernel`'s model, not a proven security boundary.

The one crate with real external surface is `vulkan_backend`, which uses
`unsafe` FFI (via `ash`) to call a real Vulkan loader/ICD. Its `unsafe`
usage is scoped to FFI call sites required by the Vulkan C API; see
[`crates/vulkan_backend/src/lib.rs`](./crates/vulkan_backend/src/lib.rs)
for what it does and does not do (it is not wired into the rest of the
workspace — see the README's "Known Issues").

## Supported versions

There are no tagged releases yet; only `main` is supported. There is no
long-term-support branch and no backport policy.

## Reporting a vulnerability

Do **not** open a public GitHub issue for a security concern.

Instead, email **mullassery@gmail.com** with:

- A description of the issue and its potential impact.
- Steps to reproduce, or a minimal example.
- Which crate(s) are affected.

This is a small, single-maintainer project — there is no dedicated
security team and no guaranteed response SLA. Reports will be
acknowledged and triaged as soon as practical.

## Known, disclosed gaps (not vulnerabilities to report — already tracked)

- In-process driver-panic containment exists (`GraphicsRuntime::submit`
  catches a panicking driver via `catch_unwind` and routes it through the
  fault-recovery path), but there is **no process-, WASM-, or eBPF-level
  sandboxing** anywhere in this workspace. A malicious or buggy driver
  implementation still runs in-process with the caller's full privileges.
  See the README's "Known Issues" section for detail.
- `vulkan_backend` is not yet wired into `gpu_abstraction::GpuDriver`, so
  none of its real-GPU code paths are reachable through the runtime today.
