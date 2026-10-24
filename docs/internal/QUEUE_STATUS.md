# Queue status — 2026-10-20

This file records what was shipped, what is blocked/failed, and what is rejected for this repository. It complements `AUDIT_FINDINGS.md`.

## Shipped
- Non-default `vulkan` feature: `vulkan_backend` is wired into `gpu_abstraction::VulkanDriver` and `graphics_runtime` via a dedicated render thread + Tokio command/result queue; the synchronous `GpuDriver` trait is unchanged. Device loss maps to a recoverable `GpuFault`/`reset_device`; no loader gives an explicit error or `FallbackDriver` software fallback. Mock-backed queue/device-lost tests, plus real Vulkan clear/fence validation via MoltenVK on Apple M5.

## Blocked / failed
- Mesa/OpenGL compatibility seam.
- Vulkan per-op draw/dispatch translation, memory-type-aware allocation, and real-hardware validation of actual draws.

## Rejected / deferred
- Production renderer.

## Policy

Any future feature or upgrade must check `AUDIT_FINDINGS.md` and this file before implementation. A blocked item is retried only when its external dependency is available; a rejected item needs new user evidence or a design change to reopen.
