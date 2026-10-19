# Audit findings — 2026-10-19

This file records the repository-specific findings from the portfolio audit
conducted on 2026-10-19. It is documentation only; no code or release artifacts
were changed in this pass unless explicitly noted under "Implementation status".

Sources: the repository's own README/source/workflows, GitHub metadata and
Actions runs, PyPI metadata, and official competitor documentation. Claims are
marked as verified, inferred, or open questions. "Absent" means not found in the
inspected repository/docs, not proof that it cannot exist.

## What it is

Graphics stack for SHER Kernel: software GPU reference driver, capability-secured runtime, and a real Vulkan backend wired behind a non-default feature via `gpu_abstraction::VulkanDriver`.

## Verified strengths

- Software GPU, command validation, fault detection/recovery, panic containment, capability gating, cursor primitives, real Vulkan device enumeration/offscreen render; tests.
- Panic containment and fault recovery are real engineering.

## Verified gaps and user-facing risks

- Vulkan backend is wired behind the non-default `vulkan` feature (submission currently proves the real clear/fence path; per-op draw/dispatch translation is still missing); no Mesa/OpenGL; path dependency on SHER-KERNEL; no crates.io.
- Not a production renderer; scalar software rasterizer.

## Direct competitors

- Vulkan/ash, wgpu, Mesa.

## Ranked next actions

- P3: freeze; do not build a production renderer or Mesa integration.

## Do not build

- Mesa/OpenGL compatibility and a native driver backend; too costly.

## Evidence gaps / validation needed

- No real workloads; no performance comparison.

## Recommended disposition

Freeze as part of the SHER research portfolio.

## Implementation status

No code changes in this pass.
