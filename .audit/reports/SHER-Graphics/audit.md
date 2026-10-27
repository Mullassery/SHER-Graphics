# Audit Report: SHER-Graphics

**Audit Date:** 2026-10-26  
**Repository Path:** `/Users/georgimullassery/Desktop/Projects/SHER-Graphics`  
**Audit Type:** Read-only systems design audit  
**Version:** 0.3.0 (workspace)  
**Branch:** `main`  
**Source Size:** ~4,298 Rust LOC; 5 crates; 74 tests

## Executive Summary

SHER-Graphics is a five-crate graphics stack combining a software GPU reference driver, a capability-secured runtime, and a real Vulkan/ash backend wired through a dedicated render thread. The Vulkan path is not a stub: it creates a device, records a clear command, submits, waits on a fence, reads pixels back, and cleans up. Panic containment, device-loss mapping, capability gates, and explicit software fallback are all real and tested.

The main risks are liveness and the limits of the containment story. A real fence wait uses `u64::MAX`, so a wedged driver can hang a caller indefinitely. The adapter’s command queue is unbounded, so concurrent producers can grow memory if the render thread stalls. `catch_unwind` contains Rust panics only; it cannot catch aborts, segfaults, or native driver crashes. Device recovery intentionally does not tear down timelines. None of these is hidden, but they matter before the backend is treated as production-hardened.

## Purpose and Positioning

- **What it is:** a native graphics object model with a software reference driver, a capability-gated runtime, and a real Vulkan backend.
- **Who it is for:** the SHER OS stack and developers studying a GPU abstraction designed around a kernel capability model.
- **What it is not:** a complete Vulkan renderer, a game engine, a shader compiler, a production-hardened GPU service, or a replacement for Mesa.
- **Differentiator:** software-reference testability plus a genuine fence-synchronized Vulkan submission path and capability-gated recovery, without making the runtime async.

## Architecture

```text
Application / compositor
        |
        v
graphics_runtime
  GraphicsRuntime: devices, queues, resources, shaders, pipelines, timelines
  capability checks, GPU fault channel, panic containment, recover_device
        |
        v
gpu_abstraction
  SoftwareGpuDriver (default reference)
  VulkanDriver (feature "vulkan"): synchronous facade over a render thread
  FallbackDriver (explicit software fallback)
        |
        +--> vulkan_backend
               VulkanContext::probe
               real device/queue/image/buffer/command/fence
               offscreen clear + pixel readback
        |
        +--> SHER-Kernel hal/gpu_driver/common/objectmodel/security
```

### Vulkan submission path

```text
GraphicsRuntime::submit
  -> capability check + timeline monotonicity
  -> GpuDriver::submit
  -> VulkanDriver enqueues Command on render-thread channel
  -> render thread creates/uses Vulkan context
  -> render_clear_to_offscreen
       create image/buffer/memory/command pool/command buffer/fence
       vkCmdClearColorImage
       queue submit
       vkWaitForFences(..., u64::MAX)
       map/read pixels
       destroy all objects
  -> reply channel
```

## Repository Layout

```text
crates/graphics_api/       device/resource/pipeline/command/timeline types + capability checks
crates/gpu_abstraction/    software driver, VulkanDriver, fallback driver
crates/graphics_runtime/   runtime orchestration, fault channel, recovery, panic containment
crates/graphics_compat/    compatibility seam
crates/vulkan_backend/     real ash/Vulkan context and offscreen clear
.github/workflows/ci.yml   test/fmt/clippy/audit; Mesa lavapipe on Linux
```

## Technology Stack and Dependencies

- **Rust:** 2021 edition; Tokio (render-thread command runtime), `ash` for Vulkan, serde/tracing/thiserror.
- **Siblings:** SHER-Kernel `hal`, `gpu_driver`, `common`, `objectmodel`, `security`.
- **Testing:** software driver tests, mock backend tests, real Vulkan tests when a loader/ICD exists; CI installs Mesa lavapipe.
- **Build:** default workspace excludes real Vulkan execution; `--features vulkan` enables the adapter.
- **Release profile:** `panic = "unwind"` is pinned so `catch_unwind` is not silently disabled.
- **CI:** x86_64 + arm64 Linux, fmt, clippy, test, audit.

## Public API

- `graphics_api`: device, queue, resource, shader, pipeline, timeline, command and capability types.
- `graphics_runtime::GraphicsRuntime`: create/destroy device, create queue/timeline, submit, wait, recover, fault polling, audit trail.
- `graphics_runtime::submit_to_driver`: panic-contained driver submission.
- `gpu_abstraction::{SoftwareGpuDriver, VulkanDriver, FallbackDriver}`.
- `vulkan_backend::{VulkanContext, AdapterInfo, VulkanError}`.
- `graphics_compat`: compatibility seam.

## Design Patterns

- **Reference implementation first:** software GPU behavior is the correctness baseline.
- **Explicit fallback:** Vulkan absence produces `Vulkan unavailable` or a visible `FallbackDriver`, never silent pretend-success.
- **Synchronous facade over async hardware:** public calls enqueue work and block on a reply; all Vulkan calls stay on one render thread.
- **Capability gates:** device creation, submission, and admin recovery require kernel capabilities; decisions are audited.
- **Fault normalization:** panics, device loss, and software faults all surface through `GpuFault`.
- **Fence-synchronized proof:** the real backend waits before reading pixels rather than asserting success.
- **No global panic-abort override:** release profile explicitly pins unwinding.

## Data and Storage

- In-memory device/resource/pipeline/timeline tables.
- No database or persistent state.
- Timeline table maps `ObjectId -> u64`; it does not record device ownership.
- Vulkan resources are per-call in the offscreen probe and destroyed in `Drop`/cleanup paths.
- Fault/audit records are in-memory.
- No credentials or user data.

## Concurrency and Performance

- `VulkanDriver` owns one render thread and a Tokio unbounded command channel; callers block on a reply channel.
- The unbounded queue has no backpressure: many concurrent submitters can grow memory if the render thread is stalled or slow.
- `render_clear_to_offscreen` creates and destroys a device plus all resources per call; this is acceptable for a probe but expensive as a general submit path.
- `vkWaitForFences(..., u64::MAX)` has no timeout; a hung GPU/driver can block forever.
- Each public submit currently executes a 1×1 offscreen clear, not per-op draw/dispatch translation.
- Device recovery is serialized through the runtime; timelines remain in memory.
- No benchmark or stress test for concurrent submissions, long-running GPU work, or device-loss storms.

## Security and Privacy

### Strengths

- Capability-gated device creation, submission, and admin recovery.
- `GpuAdmin` is required for recovery, deliberately not included in ordinary full capabilities.
- Capability decisions feed an audit trail.
- Panic containment converts driver panics into a recoverable fault.
- `panic = "unwind"` is pinned in the release profile.
- Explicit fallback avoids silent unsafe success.
- Real Vulkan code is isolated in one backend crate; the rest of the stack is software/reference logic.
- No network, credentials, or user data.

### Gaps

- **Containment is not a sandbox:** `catch_unwind` cannot catch aborts, stack overflows, segfaults, or native ICD/driver crashes. The README says this, but callers must not treat it as process isolation.
- **Unsafe FFI surface:** `vulkan_backend` contains the ash/unsafe boundary. Safety comments are extensive, but there is no fuzzing or sanitizer run over the FFI path.
- **Unbounded command queue:** a stalled render thread plus concurrent submitters can exhaust memory.
- **No fence timeout:** a wedged driver can hang a thread indefinitely.
- **Timeline state survives recovery:** stale timeline IDs/values may remain valid after a device reset because ownership is not tracked.
- **No adversarial test for malformed driver/FFI inputs.**
- **Audit records are in-memory only; no tamper-evident log.**
- **No real-hardware validation beyond an offscreen clear**; CI uses lavapipe, which is a software ICD.

## Reliability and Observability

- Fault channel surfaces panic, device loss, and software faults uniformly.
- `recover_device` resets the driver and drops device resources/shaders/pipelines.
- Timelines are explicitly not torn down and are documented as follow-up work.
- `VulkanDriver::drop` sends `Shutdown` and joins the render thread.
- Audit trail records capability-authorized actions.
- No metrics/tracing spans for GPU latency, queue depth, or fault rates.
- No health endpoint or circuit breaker.
- Fence wait has no timeout or cancellation.
- No recovery test for a hung GPU, only device-loss/panic simulations.

## Testing, CI, and Quality

- 74 static tests, matching the README’s default + Vulkan feature count.
- Software-driver tests cover object lifecycle, command validation, fault isolation, timeline monotonicity, and capability gates.
- Mock-backed Vulkan adapter tests cover command queueing and device loss without hardware.
- Real Vulkan tests perform clear + pixel readback; CI installs Mesa lavapipe so they run rather than skip.
- Panic-containment tests verify recovery through the same fault path.
- CI covers x86_64 and arm64 Linux, fmt, clippy, test, and audit.
- No fuzzing, sanitizer/Valgrind run, concurrency stress, or hardware matrix.
- No test for fence timeout/queue backpressure/timeline cleanup on recovery.

## Documentation, Packaging, and Release

- Workspace version is 0.3.0.
- README accurately distinguishes software reference, real Vulkan submission, and unimplemented per-op draw/dispatch.
- README documents panic containment limits, timeline follow-up, and dependency on a loader/ICD.
- Cargo path casing is documented for Linux.
- CI’s lavapipe install is explicitly justified.
- No published package or release provenance.
- No tagged-release audit.

## Strengths

- Real Vulkan submission with a fence and pixel readback, not a mock.
- Clear separation of software reference, runtime, and hardware backend.
- Capability-gated submission/recovery with an audit trail.
- Panic containment and device-loss recovery share one fault path.
- Explicit fallback and unavailable states.
- Strong documentation of what is and is not implemented.
- CI runs real Vulkan via lavapipe on two architectures.
- No silent success when Vulkan is absent.

## Risks and Gaps

| Risk | Severity | Evidence | Impact |
|---|---|---|---|
| Fence wait has no timeout | Medium | `vulkan_backend`: `wait_for_fences(..., u64::MAX)` | A wedged GPU/driver can hang a caller forever. |
| Unbounded render-thread command queue | Medium | `gpu_abstraction/src/vulkan.rs`: `mpsc::unbounded_channel` | Memory growth under stalled GPU/concurrent submitters. |
| Panic containment is not process isolation | Medium | `catch_unwind`; README/limitations | Aborts/segfaults/native crashes still take down the process. |
| Timelines survive device recovery | Medium | `recover_device`; documented follow-up | Stale timeline IDs/values can remain valid after reset. |
| No hardware validation beyond offscreen clear | Medium (known) | CI uses lavapipe; README | Real draws/dispatches and presentation are unproven. |
| No per-op draw/dispatch translation | Medium (known) | `RealVulkanBackend::execute` performs a clear | The adapter proves wiring, not general rendering. |
| Unsafe FFI has no fuzz/sanitizer coverage | Low/Medium | `vulkan_backend` ash surface | Memory-safety bugs may evade unit tests. |
| No queue-depth/latency metrics | Low | Audit review | Hard to diagnose stalls or leaks. |
| Audit trail is in-memory | Low | Runtime audit records | Not durable or tamper-evident. |

## Prioritized Recommendations

### P0 — liveness and bounded resources

1. Replace the infinite fence wait with a finite timeout plus cancellation/device-lost handling; surface a recoverable fault on timeout.
2. Bound the render-thread command channel and define backpressure (block, reject, or coalesce) for concurrent submitters.
3. Add a hang/device-loss test that exercises timeout, queue saturation, and recovery without requiring a real wedged GPU.

### P1 — recovery and verification

4. Track timeline ownership and tear down or invalidate timelines on device recovery.
5. Run the Vulkan FFI path under sanitizers/Valgrind and add fuzz/property tests for command/resource validation.
6. Add a real-hardware matrix or document exactly which GPUs/drivers have passed the offscreen clear.
7. Add metrics/tracing for queue depth, submit latency, fence wait, faults, and recovery count.

### P2 — product truth

8. Keep the “proves submission, not general rendering” limitation prominent until per-op draw/dispatch lands.
9. Add a recovery test for multiple devices/queues and stale timeline IDs.
10. Document the fallback decision path and make `FallbackDriver` use visible in logs/metrics.
11. Verify any future release artifacts/provenance.

## Audit Limitations

- No real GPU hardware session was run; the audit relies on CI’s lavapipe configuration and existing tests.
- No sanitizer, Valgrind, fuzz, or stress test was executed.
- Fence-timeout and queue-growth risks were identified by static inspection, not reproduced.
- Test counts are static definitions, not a verified pass count.
- Dependency advisories were not independently re-scanned.
