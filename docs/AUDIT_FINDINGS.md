# Audit Findings: SHER-Graphics

**Audit Date:** 2026-10-26  
**Version:** 0.3.0  
**Branch:** `main`  
**Full report:** [`.audit/reports/SHER-Graphics/audit.md`](../.audit/reports/SHER-Graphics/audit.md)

## Scope

Read-only review of the 5-crate graphics stack: object model, software driver, capability-secured runtime, Vulkan adapter, real ash backend, tests, CI, and release documentation. No real GPU hardware session was run.

## Findings

| ID | Severity | Finding | Evidence | Recommendation |
|---|---|---|---|---|
| SG-01 | Medium | Fence wait has no timeout. | `vulkan_backend/src/lib.rs`: `wait_for_fences(..., u64::MAX)`. | Use a finite timeout and map timeout to a recoverable fault. |
| SG-02 | Medium | Render-thread command channel is unbounded. | `gpu_abstraction/src/vulkan.rs`: `mpsc::unbounded_channel`. | Bound the queue and define backpressure. |
| SG-03 | Medium | `catch_unwind` containment is not process isolation. | Panic containment; README limitation. | Document clearly; add out-of-process isolation if native crashes matter. |
| SG-04 | Medium | Timelines survive device recovery. | `graphics_runtime::recover_device`; documented follow-up. | Track timeline ownership and invalidate/tear down on reset. |
| SG-05 | Medium (known) | No hardware validation beyond offscreen clear. | CI uses Mesa lavapipe; README. | Add a hardware matrix or state validated GPUs/drivers. |
| SG-06 | Medium (known) | Per-op draw/dispatch is not implemented. | `RealVulkanBackend::execute` performs a clear. | Keep the limitation prominent until translation lands. |
| SG-07 | Low/Medium | No fuzz/sanitizer coverage for the ash/FFI path. | CI review. | Add sanitizer/Valgrind and fuzz/property tests. |
| SG-08 | Low | No queue-depth/latency/fault metrics. | Audit review. | Add tracing/metrics for submit, fence wait, faults, recovery. |
| SG-09 | Low | Audit records are in-memory only. | Runtime audit trail. | Make audit durability/tamper-evidence explicit. |

## Positive Controls

- Real Vulkan submission with fence synchronization and pixel readback.
- Capability gates for device creation, submission, and recovery.
- `GpuAdmin` required for recovery, separate from ordinary capabilities.
- Panic, device-loss, and software faults share one fault channel.
- Release profile pins `panic = "unwind"`.
- Explicit software fallback; no silent fake success.
- Strong CI: x86_64 + arm64, fmt, clippy, test, audit, lavapipe.
- Clear documentation of implemented vs unimplemented rendering.

## Audit Limitations

- No real GPU hardware run; relies on existing tests/CI configuration.
- No sanitizer/Valgrind/fuzz/stress execution.
- Timeout and queue-growth risks were found statically.
- Test counts are definitions, not a verified pass count.
- Dependency advisories were not independently re-scanned.
