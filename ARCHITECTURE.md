# SHER Graphics Architecture

**Status**: Design for the native runtime/abstraction/Mesa-compatibility layers described below — precedes implementation there. Not true project-wide: see §23 for a real, working Vulkan (`ash`) backend (device enumeration + offscreen render, verified against MoltenVK) that exists today, standalone from the software simulation the rest of this document covers.
**Scope**: Native graphics subsystem for SHER Kernel, with OpenGL and Vulkan as compatibility-facing interfaces
**Relationship to existing work**: Extends Phase 11 (`hal`, `gpu_driver`, `wayland_server`) and the LKI compatibility model (`lki`, `driver_runtime`, `compatibility`)
**Precedent**: Applies the same philosophy already proven by Aurora at the desktop layer — see `CLAUDE.md` principle 2, *"Compatibility Without Dependency"*, and `INTEGRATION_ANALYSIS.md` / `COMPLETE_ECOSYSTEM_ANALYSIS.md`, which already document Aurora's rendering path running on this same GPU stack (`Aurora colors use GPU (DRM/KMS)`).

---

## 0. Philosophy

SHER Kernel's second architectural pillar already states the principle this document exists to apply:

> **Compatibility Without Dependency**: Linux hardware ecosystem is a compatibility target, not an architectural constraint.

Aurora is the proof that this works at the UI layer: GTK4/libadwaita applications run unmodified, while Aurora owns the actual design language underneath. Nothing about GTK4's widget model constrains how Aurora computes color, motion, or typography.

Graphics gets the identical treatment, one layer down:

```
Existing graphics ecosystem (OpenGL, Vulkan apps, Mesa)
                    ↓
              SHER Graphics
                    ↓
   Modern native graphics architecture (SHER Graphics Runtime)
```

OpenGL and Vulkan are **application-facing interfaces implemented on top of SHER Graphics**, not the architecture SHER Graphics is built to satisfy. The distinction drawn in the brief is the load-bearing one for this whole document:

> Build a native graphics architecture for SHER Kernel that happens to support OpenGL and Vulkan applications — not "build another Vulkan."

---

## 1. High-Level Architecture

```
Applications
     │
     ├── OpenGL applications
     │        ↓
     │      Mesa (Gallium state tracker)
     │        ↓
     │   SHER OpenGL compatibility (Gallium winsys → SHER)
     │
     ├── Vulkan applications
     │        ↓
     │      Mesa (Vulkan common runtime + vendor driver)
     │        ↓
     │   SHER Vulkan compatibility (WSI + kernel-driver shim → SHER)
     │
     └── Native SHER applications
              ↓
       Native SHER Graphics API  (new crate: sher_graphics_api)
              ↓
       SHER Graphics Runtime     (new crate: sher_graphics_runtime)
              │
              ▼
       SHER GPU Abstraction      (new crate: sher_gpu_abstraction,
              │                    extends existing `hal`)
              ▼
         GPU driver               (existing `gpu_driver`,
              │                    + LKI-hosted Linux drivers initially)
              ▼
        SHER Kernel               (existing `core`, `memory`,
              │                    `interrupt`, `security`, `scheduler`)
              ▼
           Hardware
```

All three application paths converge below the compatibility boundary. Nothing above `SHER Graphics Runtime` is allowed to leak into it — Mesa's object model, Vulkan's queue-family concept, and OpenGL's implicit global state all get translated *before* they reach the runtime, the same way `lki::linux_api` translates `kmalloc` into `sher_memory::MemoryAllocator` calls instead of letting Linux's allocator semantics dictate SHER's own.

### Mapping onto existing Phase 11 layers

`PHASE_11_ARCHITECTURE.md` already sketched a 6-layer stack (HAL → GPU/Audio/Input drivers → Unified Device Manager → Wayland Compositor). This document fills in what that stack left as a stub — the `gpu_driver` crate today only has connector/framebuffer/page-flip primitives (DRM/KMS-shaped), with a one-line placeholder for "Vulkan/OpenGL Bridge." Here's how the new pieces slot in:

| Existing crate | Today | Becomes |
|---|---|---|
| `hal` | Generic `HardwareDriver` trait, MMIO, register I/O | Stays the device-discovery floor; `sher_gpu_abstraction` implements `HardwareDriver` for GPUs specifically |
| `gpu_driver` | DRM/KMS-shaped: connectors, modes, framebuffers, page flip | Becomes the **presentation backend** — the thing both the native API's swapchain and Mesa's WSI target |
| `wayland_server` | Compositor: surfaces, buffers, outputs | Becomes a **consumer** of the native SHER Graphics API for compositing, same relationship GNOME Shell has to Mesa today |
| `lki`, `driver_runtime` | Linux driver translation + sandboxed execution | Becomes the **initial GPU driver hosting mechanism** (Phase A below) — unmodified `amdgpu`/`i915`/`nouveau` kernel drivers run inside a container, exposing a DRM-ioctl-compatible node |
| `compatibility` (`linux.rs`, `posix.rs`) | POSIX/Linux syscall compatibility | Gains siblings: `opengl.rs`-equivalent and `vulkan.rs`-equivalent compatibility modules — but see §7–9, these live mostly in Mesa itself, not hand-written translation code |
| `scheduler` | Heterogeneous compute scheduling | Gains GPU workloads as a scheduled class, same as it already schedules NPU/AI work |

---

## 2. Why Not Just Reimplement Vulkan

This is the question the brief asks first, and it has to be answered honestly rather than rhetorically, because most of Vulkan's object model *is* good engineering — the risk is copying it uncritically instead of copying it deliberately.

Vulkan's primitives split into two categories:

**Genuinely necessary for any modern GPU architecture** (SHER keeps these, in spirit):
- Explicit, app-visible synchronization (GPUs are async; hiding that costs performance)
- Explicit memory management with typed heaps (device-local vs. host-visible is physical reality, not API taste)
- Command recording as data, submitted in batches (matches how GPU command processors actually consume work)
- Multiple independent queues for overlapping graphics/compute/transfer
- Low, predictable per-call driver overhead

**Artifacts of Vulkan's specific constraints** (SHER does *not* inherit these):
- **The binary-semaphore / timeline-semaphore split.** This exists because Vulkan 1.0 shipped without timeline semaphores and couldn't break ABI later. SHER has no legacy version to preserve — it needs exactly one synchronization primitive (§13), and it's a timeline.
- **Queue families as an opaque, driver-enumerated, capability-bitmask concept.** This is a compromise for a C ABI that has to describe arbitrary vendor hardware without recompiling. SHER's runtime already knows what hardware it's running on via `hal`; the native API can expose workload *classes* (Graphics, Compute, Transfer) directly instead of asking every app to re-derive them from a queue family table at startup.
- **Renderpass/subpass objects** (even post-`VK_KHR_dynamic_rendering`, the legacy shape still haunts the spec). This was a tile-based-GPU optimization hint bolted onto a desktop-GPU-shaped API. SHER's runtime can make tiling decisions inside the driver layer, where it belongs, instead of asking application authors to describe it.
- **Pipeline state objects as giant, monolithic, hashable structs.** This was Vulkan's answer to "we don't trust driver-side shader recompilation to be fast enough." SHER controls its own runtime and can afford a real pipeline cache with async, background (re)compilation instead of pushing that cost onto every app author (§15).
- **The extension mechanism itself.** Vulkan's `pNext` chains and 400+ extensions exist because the spec can't be revised, only appended to, across a decade of hardware evolution and multiple vendors with veto power. SHER's native API has one implementation and one kernel behind it — versioning is a `sher_kernel` release concern, not an infinite structural-extension problem.

The design rule that falls out of this: **keep the primitives that reflect physical GPU reality (async execution, typed memory, explicit sync, multi-queue); drop the primitives that exist only because Vulkan can never break its own ABI.**

---

## 3. Native SHER Graphics API

Lives in a new crate, `crates/graphics_api` (`sher_graphics_api`), following the same idioms as the rest of the kernel: every GPU-visible thing is a `KernelObject` variant (`GPU`, and new object subtypes below), addressed by `ObjectId`, gated by `CapabilitySet`, and fallible operations return `Result<T>`.

```rust
// Sketch — crates/graphics_api/src/lib.rs

pub struct GraphicsDevice {
    pub id: ObjectId,              // KernelObject { obj_type: ObjectType::Gpu, .. }
    pub queues: Vec<QueueHandle>,
}

pub enum WorkloadClass { Graphics, Compute, Transfer }

pub struct QueueHandle {
    pub id: ObjectId,
    pub class: WorkloadClass,
}

// A single unified synchronization primitive — see §13.
pub struct Timeline {
    pub id: ObjectId,
    pub current_value: u64,
}

pub enum ResourceKind {
    Buffer { size: usize, usage: BufferUsage },
    Image   { width: u32, height: u32, depth: u32, format: PixelFormat, usage: ImageUsage },
}

pub struct Resource {
    pub id: ObjectId,
    pub kind: ResourceKind,
    pub memory_class: MemoryClass,     // §12
}

pub struct CommandStream {
    pub id: ObjectId,
    pub target_class: WorkloadClass,
    // Recorded as portable, driver-independent ops — not a raw driver-specific
    // command buffer. Translation to ISA-level commands happens in the driver.
    pub ops: Vec<GraphicsOp>,
}

pub trait GraphicsApi {
    fn create_device(&mut self, gpu: &ObjectId, caps: &CapabilitySet) -> Result<GraphicsDevice>;
    fn create_resource(&mut self, device: &ObjectId, kind: ResourceKind) -> Result<Resource>;
    fn create_command_stream(&mut self, device: &ObjectId, class: WorkloadClass) -> Result<CommandStream>;
    fn submit(&mut self, queue: &QueueHandle, stream: CommandStream, signal: &Timeline, value: u64) -> Result<()>;
    fn wait(&self, timeline: &Timeline, value: u64) -> Result<()>;
    fn create_pipeline(&mut self, device: &ObjectId, shader: ShaderModule, class: WorkloadClass) -> Result<Pipeline>;
    fn present(&mut self, swapchain: &ObjectId, resource: &ObjectId, signal: &Timeline, value: u64) -> Result<()>;
}
```

What's deliberately **not** in this list, and why:

- No `VkInstance`/`VkPhysicalDevice`/`VkDevice` three-tier bootstrap — `hal` already enumerates GPUs; `GraphicsDevice::id` *is* the `ObjectId` `hal` assigned it.
- No descriptor sets/descriptor pools as a distinct object family — resource binding is expressed directly on the `CommandStream` as ops, and the runtime decides the on-hardware binding-table representation (this is a compiler-backend decision, not something every app should have to manage).
- No separate "device memory" vs. "resource" allocation dance (`vkAllocateMemory` + `vkBindBufferMemory`) — `create_resource` returns something already backed by memory from the correct `MemoryClass` (§12); apps that need manual placement control get an explicit `bind_memory` escape hatch, not the default path.

---

## 4. SHER Graphics Runtime

New crate, `crates/graphics_runtime` (`sher_graphics_runtime`). This is the layer the brief is most insistent be designed *for* SHER Kernel specifically, not as a portable abstraction:

```
Application
     ↓
SHER Graphics API        (crates/graphics_api)
     ↓
SHER Graphics Runtime     (crates/graphics_runtime)
     ↓
SHER GPU Abstraction      (crates/gpu_abstraction, built on `hal`)
     ↓
GPU driver                (`gpu_driver`, or LKI-hosted Linux driver)
     ↓
Hardware
```

Runtime responsibilities:

- **Command scheduling** — hands GPU workloads to `scheduler` (existing heterogeneous compute scheduler) as a first-class workload class alongside CPU/NPU, not a bolt-on. GPU work becomes visible to the Adaptive Resource Orchestrator (`aro`) the same way AI inference work already is.
- **Resource tracking** — owns the liveness/lifetime graph for every `Resource`/`CommandStream`/`Timeline`, reusing `KernelObject.dependencies` rather than inventing a parallel tracking structure.
- **Memory management** — talks to `sher_memory` for host-side allocations and to `gpu_abstraction` for device-local VRAM; unifies both under `MemoryClass` (§12).
- **Pipeline compilation** — owns the shader/pipeline cache (§15); this is runtime state, not driver state, so it survives driver reloads and is shareable across processes subject to capability checks.
- **Presentation** — bridges `create_swapchain`/`present` calls to `gpu_driver`'s connector/framebuffer/page-flip primitives.
- **GPU context management** — one runtime-level context per `GraphicsDevice`, capability-scoped per process via `security::CapabilityGrant`.
- **Error handling** — GPU hangs/faults are reported up through the same `Error` channel every other subsystem uses; a faulting context is torn down in isolation (§18), never a whole-kernel panic.
- **Workload prioritization / GPU resource isolation** — enforced via the existing `PermissionTier` model: a `GpuCommandSubmit` capability at `Tier 3 (High, 2h)` for a foreground compositor client looks different from a `Tier 1 (Low, 1h)` batch-compute client, and `ARO` can preempt accordingly.

The runtime is intentionally **not** reusable outside SHER — it assumes SHER's object model, SHER's capability system, and SHER's scheduler. That's the point: portability lives at the API surface (§3) and at the compatibility boundary (§7–9), not here.

---

## 5. Mesa Integration

Mesa is not one monolith — it's a set of largely independent layers, and the honest answer to "what can we reuse" requires naming them individually.

```
              OpenGL app                     Vulkan app
                   │                              │
              Gallium state                  Vulkan common
              tracker (mesa/st)              runtime (vulkan/runtime)
                   │                              │
        ┌──────────┴──────────┐          ┌────────┴────────┐
        │   NIR / GLSL front  │          │  SPIR-V → NIR    │
        │   end (shared)      │          │  (shared)        │
        └──────────┬──────────┘          └────────┬────────┘
                   │                              │
         pipe driver (radeonsi,             vendor Vulkan driver
         iris, nouveau, ...)                (radv, anv, nvk, lavapipe)
                   │                              │
        ┌──────────┴──────────────────────────────┴────────┐
        │         winsys / loader / WSI  ← SHER-SPECIFIC     │
        └──────────────────────────────┬─────────────────────┘
                                        ↓
                                 SHER Graphics
```

**Reused unmodified:**
- NIR (Mesa's common shader IR) and the GLSL / SPIR-V → NIR front ends
- Shader compiler backends: LLVM-based (AMD's `ac_llvm`) and ACO for RDNA, Intel's backend, etc.
- Gallium pipe drivers (`radeonsi`, `iris`, `nouveau`) and Vulkan drivers (`radv`, `anv`, `nvk`) — these talk to the kernel through `libdrm` ioctls, and don't care what's underneath `libdrm` as long as the ioctl surface behaves like DRM/KMS
- `llvmpipe` / `lavapipe` (software rasterizers) — zero kernel dependency, useful immediately for headless/CI and as a correctness reference

**SHER-specific work required:**
- **The winsys/loader layer** — Mesa already supports multiple platforms (X11, Wayland, DRM, Android, Windows) via a pluggable winsys. SHER adds one more: a winsys that talks to `gpu_driver`/`gpu_abstraction` instead of `libdrm`.
- **The Vulkan WSI platform backend** — same idea, one more platform in `vk_wsi`, targeting SHER's presentation objects (§16) instead of `xcb`/`wayland`/`KHR_display`.
- **A DRM-ioctl-compatible device node** (Phase A only, §9) — the pragmatic unlock: rather than rewriting every winsys immediately, expose something that answers GEM-alloc/PRIME/execbuffer/sync ioctls the way `/dev/dri/cardN` does, backed by `sher_gpu_abstraction`. This is the *exact same move* `lki` already makes for `kmalloc`/`request_irq` — translate the boundary ABI, not the internals.

This mirrors the Aurora precedent precisely: Aurora didn't reimplement GTK4's widget rendering, it reused GTK4/libadwaita wholesale and changed the layer underneath. Mesa's pipe-driver and Vulkan-driver layers are the GTK4 of graphics — mature, correct, hardware-specific code that would be wasteful to duplicate. The winsys/WSI/kernel-ABI seam is the equivalent of Aurora's GTK4 theming hooks: the one place SHER-specific code has to exist.

---

## 6. OpenGL Compatibility Path

```
OpenGL application
        ↓
libGL / Mesa Gallium state tracker (unmodified)
        ↓
SHER winsys  (new: crates/graphics_api's Mesa-facing glue, or a
              separate `mesa-sher-winsys` component depending on
              how Mesa's build wants it packaged)
        ↓
SHER Graphics Runtime
        ↓
SHER Kernel
```

No OpenGL-specific translation code lives inside SHER itself beyond the winsys glue — `libGL` and the Gallium state tracker already are the "OpenGL implementation." SHER's job is only to be a valid Gallium winsys target.

---

## 7. Vulkan Compatibility Path

```
Vulkan application
        ↓
Vulkan loader → vendor ICD (radv / anv / nvk / lavapipe, unmodified)
        ↓
SHER WSI platform backend  (new, small)
        ↓
SHER Graphics Runtime
        ↓
SHER Kernel
```

Same shape as OpenGL. The Vulkan *API* itself never gets reimplemented by SHER — the vendor ICDs already are the Vulkan implementation. SHER only has to be a valid WSI target and a valid backing store for the ICD's memory/command-submission ioctls (Phase A) or native calls (Phase B, §9).

---

## 8. GPU Abstraction Layer & Driver Architecture

New crate, `crates/gpu_abstraction`, extends `hal::HardwareDriver`:

```rust
pub trait GpuDriver: HardwareDriver {
    fn allocate_vram(&mut self, device: &ObjectId, size: usize, class: MemoryClass) -> Result<GpuAllocation>;
    fn submit(&mut self, queue: &ObjectId, ops: &[GraphicsOp], signal: (&ObjectId, u64)) -> Result<()>;
    fn map_dma(&mut self, allocation: &GpuAllocation) -> Result<DmaMapping>;   // ties into IOMMU, §10
    fn fault_status(&self, device: &ObjectId) -> Result<Option<GpuFault>>;
}
```

### Driver strategy: two phases, matching the brief's requested migration path

**Phase A — LKI-hosted Linux drivers (ship first):**

```
Existing Linux GPU drivers (amdgpu, i915/xe, nouveau — unmodified)
             ↓
       LKI compatibility  (driver_runtime container + lki translation)
             ↓
        SHER Graphics
```

The real, mature, multi-vendor-maintained Linux kernel GPU drivers run inside `driver_runtime`'s existing sandbox (`DriverContainer`, `ResourceLimits`, `SyscallPolicy`), the same isolation model already used for other Linux drivers. `lki` translates the subset of Linux kernel internals these drivers actually call (DMA-buf, IOMMU/IOVA management, interrupt registration, MMIO) into SHER primitives. The driver exposes a DRM-ioctl-compatible device node; Mesa's unmodified winsys/WSI talks to it exactly as it would talk to `/dev/dri/cardN` on Linux.

This is the fastest path to real hardware support across AMD, Intel, and Nouveau-supported NVIDIA/ARM GPUs, and it costs nothing extra in Mesa-side engineering — it's the same trick already validated by the LKI design for storage and networking drivers.

**Phase B — Native SHER GPU drivers (gradual):**

```
Native SHER GPU drivers
             ↓
        SHER Graphics
             ↓
        SHER Kernel
```

Per-vendor drivers implementing `GpuDriver` directly against SHER's memory/interrupt/scheduler primitives, no DRM-ioctl shim in the path. Mesa's winsys/WSI is retargeted to call `gpu_abstraction` directly instead of ioctls. This is where the real payoff of "freedom underneath" shows up — no ioctl marshaling overhead, GPU scheduling fully visible to `ARO`, capability-scoped GPU memory instead of DRM's coarser permission model.

Vendor-by-vendor risk (informs §22 roadmap ordering):

| Vendor | Kernel driver complexity | Mesa userspace | Phase A difficulty | Phase B difficulty |
|---|---|---|---|---|
| AMD (amdgpu) | High, but open and well-documented | radeonsi + radv, both open | Medium | High |
| Intel (i915/xe) | High | iris + anv, both open | Medium | High |
| ARM (panfrost/etc.) | Lower complexity, open | Gallium + Vulkan drivers exist | Lower | Medium |
| NVIDIA (nouveau/NVK) | High; proprietary firmware dependency (GSP) even for the open driver | nouveau + NVK, improving but younger | High | Very high |
| NVIDIA (proprietary) | Closed source, binary-only | Closed | Very high / likely infeasible under LKI sandboxing | Not applicable |

---

## 9. SHER Kernel ↔ Graphics Interface

Extends what `hal::DeviceInfo`/`MemoryMapping` already model, with GPU-specific additions the kernel must expose:

| Kernel responsibility | Owned by | GPU-specific extension needed |
|---|---|---|
| GPU memory | `memory` | VRAM as a distinct `MemoryClass`, GPU virtual address (GPUVA) space per context |
| DMA | `memory` + `gpu_abstraction` | DMA-buf-equivalent handles for zero-copy sharing with `wayland_server` |
| Interrupts | `interrupt` | GPU completion/fence interrupts, page-fault interrupts routed to runtime fault handler |
| Device discovery | `device_manager`, `unified_device_manager` | GPU enumeration already covered by `hal`; multi-GPU topology added (§17) |
| Scheduling | `scheduler` | GPU workload class alongside existing heterogeneous compute classes |
| Process isolation | `security` | New `Capability` variants: `GpuMemoryAlloc`, `GpuCommandSubmit`, `GpuAdmin` |
| Synchronization primitives | `graphics_runtime` | Timeline objects backed by kernel futex-equivalent wake mechanism |
| Virtual memory / IOMMU | `memory` | IOMMU/IOVA mapping for DMA safety, reused from whatever the storage/networking DMA path already established |
| Power management | new, small | GPU power states surfaced through existing kernel power-management hooks (not yet built elsewhere either — track as a shared dependency, not graphics-specific debt) |
| Display devices | `gpu_driver` | Already modeled: connectors, modes |
| GPU fault handling | `graphics_runtime` | Faulting context isolated and torn down, never a kernel panic (§18) |
| Multi-GPU coordination | `unified_device_manager` | Device enumeration + explicit app-facing selection (§17) |

Boundary discipline: **the kernel exposes primitives (memory, interrupts, scheduling, isolation); the runtime composes them into graphics-specific behavior; drivers translate them into hardware-specific bit patterns.** None of these three should know the internals of either neighbor — this is the same "no circular dependencies, each layer depends only on layers below" rule `PHASE_11_ARCHITECTURE.md` already commits to.

---

## 10. GPU Memory Architecture

```rust
pub enum MemoryClass {
    DeviceLocal,        // VRAM, not CPU-visible without a copy
    HostVisible,         // CPU-writable, GPU-readable (uniform/staging)
    HostVisibleCached,    // CPU-readable back (readback buffers)
    DeviceLocalHostVisible, // Resizable BAR / SAM — CPU-visible VRAM window, when hardware supports it
}
```

- Allocator design follows the same bump/pool/free-list layering already used in `memory` for host RAM, applied to GPU address space instead — no new allocator philosophy invented for graphics specifically.
- Residency/eviction: `graphics_runtime` tracks working-set size per context; when VRAM pressure is signaled, it evicts least-recently-submitted resources to host memory, informed by `ARO`'s existing predictive-allocation heuristics rather than a graphics-specific LRU bolted on separately.
- Zero-copy sharing: a `Resource` handed to `wayland_server` for compositing shares the same DMA-buf-equivalent handle rather than being copied — this is what makes the compositor path (and by extension, Aurora) cheap.

---

## 11. Synchronization Model

One primitive: the **Timeline** (§3), a monotonically increasing 64-bit counter per GPU context, directly analogous to Vulkan's timeline semaphores but without the binary-semaphore sibling Vulkan carries for legacy reasons (§2).

- `submit(..., signal: &Timeline, value: u64)` — GPU work signals the timeline to `value` on completion.
- `wait(timeline, value)` — CPU-side blocking or async wait for the timeline to reach `value`.
- Cross-queue and cross-process dependencies both reduce to "wait for timeline X to reach value N" — no separate fence/semaphore/event taxonomy.
- Backed at the kernel level by the same wake-on-value primitive that would back a futex-style construct — one mechanism, reused rather than reinvented per subsystem.

---

## 12. Command Submission Model

- Commands are recorded into a `CommandStream` as portable `GraphicsOp`s (draw, dispatch, copy, barrier, bind) — not a driver-specific opaque blob. Translation into hardware ring-buffer commands happens once, inside the driver (`gpu_driver` / vendor driver), not duplicated per application.
- Submission targets a `WorkloadClass`, not an opaque queue-family index — the runtime picks the concrete hardware queue, informed by `scheduler`.
- Because `graphics_runtime` owns scheduling, GPU work is preemptible and reprioritizable the same way CPU work is — a background compute job can be deprioritized under `ARO` the instant a foreground compositor submission arrives, instead of running to hardware-scheduler completion uninterrupted the way many current consumer GPU schedulers behave.

---

## 13. Shader Compilation Architecture

Two ingestion paths feeding one internal representation:

- **Compatibility paths** (OpenGL/Vulkan): shaders arrive as GLSL or SPIR-V, and get compiled by Mesa's own front end into NIR — reused wholesale, no reason to duplicate a decade of shader-compiler correctness work (§5).
- **Native SHER path**: since SHER Kernel and its userspace are Rust-first, the native shading path targets a Rust-native shader compiler (e.g. `naga`-style SPIR-V/WGSL-class tooling) rather than requiring native apps to hand-write SPIR-V. This keeps native app authors inside the Rust toolchain end to end, consistent with the rest of the kernel's "no unsafe without review, explicit types" discipline.
- Both paths converge on the same driver-facing IR boundary (NIR, or a NIR-compatible SHER IR) so vendor driver backends (`radeonsi`, `iris`, native SHER drivers alike) only ever have to compile one shape of input.
- Pipeline caching lives in `graphics_runtime` (§4), not in individual drivers — cache entries are keyed by shader hash + pipeline state, persist across process restarts subject to capability checks, and are compiled asynchronously in the background rather than stalling the submitting app on first use (the problem Vulkan's monolithic PSOs were partly a defensive response to, §2).

---

## 14. Display / Presentation Architecture

```
graphics_api::present(swapchain, resource, signal, value)
        ↓
graphics_runtime  — waits on `signal`, then hands the resource to:
        ↓
gpu_driver          — existing connector/mode/framebuffer/page-flip primitives
        ↓
Display hardware
```

`wayland_server` sits *beside* this, not below it: the compositor is a client of the native SHER Graphics API like any other renderer, composites client surfaces into a single framebuffer using the same `Resource`/`CommandStream` primitives, and then presents that composited framebuffer through the same path. This is exactly how Aurora/GTK4 apps already reach the screen today per `COMPLETE_ECOSYSTEM_ANALYSIS.md` — this document formalizes and extends that existing dependency rather than replacing it.

---

## 15. Multi-GPU Architecture

- `unified_device_manager` enumerates all GPUs; `graphics_api::create_device` is explicit about which one it targets — no implicit SLI/Crossfire-style automatic fan-out, matching the brief's instruction to make GPU scheduling and resource management first-class rather than hidden.
- Cross-device transfers go through the kernel-mediated DMA path (§10), never a direct GPU-to-GPU backchannel the kernel can't see or account for — this keeps `security`'s capability model and `ARO`'s scheduling both authoritative over cross-device traffic.
- `ARO` is the natural place for placement heuristics (which GPU should host which workload) since it already does predictive resource allocation for heterogeneous compute.

---

## 16. Security / Isolation Model

Directly reuses SHER's existing capability + tier model rather than inventing a graphics-specific permission system:

| New capability | Typical tier | Rationale |
|---|---|---|
| `GpuMemoryAlloc` | Tier 2 (Medium, 24h) for system compositor; Tier 1 (Low, 1h) default for apps | Bounds how long an app can hold GPU memory without re-authentication |
| `GpuCommandSubmit` | Tier 3 (High, 2h) for compositor; Tier 1 for apps | Distinguishes trusted display-path submitters from arbitrary app submitters |
| `GpuAdmin` | Tier 4 (Critical, 30m) | Driver reload, power-state changes, fault-recovery triggers |

- Per-context GPU virtual address isolation (§10's GPUVA) prevents one process's GPU submissions from addressing another's memory, mirroring CPU-side process isolation.
- Driver sandboxing reuses `driver_runtime`'s existing `DriverContainer` + `SandboxPolicy` model unmodified for Phase A (LKI-hosted drivers); Phase B native drivers get the same sandbox treatment other native SHER drivers get.
- Audit logging reuses `lki::audit`'s `AuditLog`/`AuditEntry` pattern — GPU capability grants, submissions from unusual contexts, and fault events all flow into the same audit trail the rest of the kernel already writes to, rather than a separate graphics-only log.

---

## 17. Error / Fault Handling

- A GPU hang or page fault is reported through the standard `Result`/`Error` channel, tagged to the offending context.
- `graphics_runtime` tears down *only* the faulting context — in-flight work on other contexts/queues continues, matching the kernel-wide "one driver's failure doesn't crash others" principle already stated for Phase 11's Unified Device Manager.
- Recovery follows the same "fail secure" pillar already in `CLAUDE.md`: on ambiguous fault state, the context is denied further submission rather than allowed to retry blindly.
- Timeout-based hang detection (a submission that doesn't signal its timeline within a bounded window) triggers context-level recovery without requiring a full GPU reset unless the fault is device-wide.

---

## 18. Performance Model

Targets stated the same way existing `CLAUDE.md` performance objectives are (concrete, falsifiable numbers, not aspirational language):

- Command submission overhead: comparable to or lower than Vulkan's per-`vkQueueSubmit` cost, since SHER's runtime doesn't need to validate against a public C ABI surface on every call.
- Driver isolation overhead ceiling: **< 5%**, matching the existing driver-isolation target — Phase A's LKI-hosted drivers are the likely worst case here and should be benchmarked explicitly against bare-metal Linux DRM before Phase A ships.
- Pipeline cache warm path: pipeline lookup + bind must not stall a frame; cold compilation happens off the submission thread.
- GPU scheduling: reuse the existing **80%+ utilization for eligible workloads** target already set for heterogeneous compute, applied to GPU compute workloads specifically.

---

## 19. Crate Structure (proposed additions)

```
crates/
├── hal/                    # existing — device discovery, MMIO, IRQ (unchanged)
├── gpu_driver/              # existing — DRM/KMS-shaped display primitives (extended, not replaced)
├── wayland_server/           # existing — compositor (becomes a graphics_api client)
├── lki/, driver_runtime/      # existing — Phase A driver hosting mechanism (reused, not extended)
├── gpu_abstraction/          # NEW — GpuDriver trait, extends hal::HardwareDriver
├── graphics_runtime/          # NEW — SHER Graphics Runtime (§4)
├── graphics_api/               # NEW — Native SHER Graphics API (§3)
└── graphics_compat/             # NEW, thin — Mesa winsys/WSI glue code (§5–7);
                                   # most of the compatibility surface is Mesa itself,
                                   # not code living in this repo
```

Dependency direction (no cycles, matching the existing Phase 11 rule):

```
graphics_api → graphics_runtime → gpu_abstraction → hal → core
                     ↑
              graphics_compat (Mesa winsys/WSI targets graphics_runtime
                                the same way graphics_api does)
                     ↑
              wayland_server (client of graphics_api)
```

---

## 20. Migration Strategy From Linux

Directly parallel to the LKI roadmap already committed to in `CLAUDE.md` for storage/networking:

1. **Phase A (ship first): LKI-hosted Linux GPU drivers.** Real `amdgpu`/`i915`/`nouveau` drivers, sandboxed, exposing a DRM-ioctl-compatible surface. Mesa runs completely unmodified above a thin SHER winsys/WSI layer that itself talks to that ioctl-compatible surface — meaning even the winsys glue is minimal at this stage, since it's mostly "pass through to the ioctl shim."
2. **Phase B: retarget Mesa's winsys/WSI to `gpu_abstraction` directly**, removing the ioctl-shim overhead while Mesa's actual rendering code (state trackers, ICDs, shader compilers) stays exactly as-is.
3. **Phase C: native SHER GPU drivers** replace LKI-hosted Linux drivers vendor by vendor, prioritized by the risk table in §9 (AMD/Intel/ARM first; NVIDIA last given firmware dependency and driver immaturity).
4. **Phase D: native SHER applications** built directly against `graphics_api`, bypassing Mesa entirely, become viable once the native API and runtime are proven under the compatibility paths — matching the brief's instruction that native SHER should have the cleanest, most direct path to hardware.

Native apps are never blocked on this migration completing — `graphics_api`/`graphics_runtime` can be built and used by native apps talking to `gpu_driver` (Phase 11's existing skeleton) well before Phase A's Linux-driver hosting is production-ready, the same way Aurora didn't wait for the full kernel before it had something to render.

---

## 21. Roadmap and Effort Estimate

Following the same table shape as `PHASE_11_ARCHITECTURE.md`:

| Component | Depends on | Est. LOC | Est. tests | Risk |
|---|---|---|---|---|
| `graphics_api` (native API surface) | `hal` | 1,500–2,000 | 25–35 | Low — mostly type/trait design |
| `graphics_runtime` core (scheduling, resource tracking, memory) | `graphics_api`, `scheduler`, `memory`, `security` | 3,000–4,000 | 40–50 | Medium |
| `graphics_runtime` presentation bridge | `gpu_driver`, `wayland_server` | 500–800 | 10–15 | Low — `gpu_driver` skeleton already exists |
| `gpu_abstraction` (`GpuDriver` trait + VRAM/DMA/fault plumbing) | `hal`, `memory`, `interrupt` | 1,200–1,800 | 20–25 | Medium |
| Phase A: LKI-hosted GPU driver container + DRM-ioctl shim | `lki`, `driver_runtime` | 2,500–3,500 | 30–40 | **High** — real Linux GPU drivers are large, tightly coupled to Linux internals (fences, DMA-buf, GEM, firmware loading); this is the single biggest scope item |
| Mesa winsys/WSI glue | `graphics_runtime` (Phase B) or ioctl shim (Phase A) | 800–1,500 | 15–20 | Medium — depends on Mesa's own extension points staying stable |
| Native shader compilation path (Rust-native compiler integration) | `graphics_runtime` | 1,500–2,500 | 20–30 | **High** — substantial research; shader compiler correctness across vendor ISAs is a multi-year problem even for Mesa |
| Multi-GPU coordination | `unified_device_manager`, `graphics_runtime` | 600–1,000 | 10–15 | Medium |
| Security/capability integration | `security` | 400–600 | 15–20 | Low — mostly wiring existing primitives |

### What's realistically implementable first

1. `graphics_api` type/trait surface (no hardware dependency)
2. `graphics_runtime` presentation bridge on top of the *already-existing* `gpu_driver` skeleton
3. `llvmpipe`/`lavapipe` software-rendering path as the first end-to-end validation of the whole stack, entirely independent of any real GPU driver

### What needs substantial research before committing to an approach

- Native shader compilation (Rust-native compiler maturity for production GPU ISA targets)
- GPU scheduling fairness/preemption semantics under `ARO` (no existing SHER subsystem has had to arbitrate hardware-scheduled, non-preemptible-by-default work before)
- Exact DRM-ioctl surface required for Phase A — this needs to be scoped per kernel driver (amdgpu's ioctl set differs meaningfully from i915's), not assumed uniform

### Highest technical risk

**Phase A's LKI-hosted Linux GPU drivers.** This is a categorically harder problem than LKI's existing storage/networking driver hosting: GPU kernel drivers are enormous (amdgpu alone is hundreds of thousands of lines), depend on Linux-specific subsystems with no clean boundary (DMA-buf, `drm_sched`, firmware/PSP loading, ACPI power management hooks), and any incompatibility surfaces as a hang or corruption rather than a clean error. This should be prototyped against exactly one driver (recommend `amdgpu`, given its open documentation and active upstream) before the strategy is assumed to generalize to Intel and Nouveau.

---

## 22. Summary

The convergence point required by the brief holds throughout:

```
OpenGL apps ──┐
Vulkan apps ──┼──→ SHER Graphics Runtime → SHER GPU Abstraction → SHER Kernel → Hardware
Native apps ──┘
```

OpenGL and Vulkan are compatibility interfaces satisfied almost entirely by reusing Mesa's existing, mature, vendor-maintained code — the same way Aurora satisfies GTK4 compatibility by reusing GTK4/libadwaita wholesale. The only SHER-specific compatibility code is the thin winsys/WSI/ioctl-shim seam. Everything below that seam — the runtime, the abstraction layer, the memory model, the synchronization primitive, the scheduling integration — is designed against SHER Kernel's actual primitives (`ObjectId`, `CapabilitySet`, `scheduler`, `ARO`), not against what Vulkan happened to need in 2016.

---

## 23. Addendum: a real Vulkan backend (`vulkan_backend`)

Everything above this section was written, and largely still holds, as design that *precedes* implementation. One gap needed correcting: this document and this repo's public description previously described the project as "Vulkan/OpenGL via Mesa" while containing zero Vulkan/OpenGL/Mesa dependency anywhere — every crate was a pure-Rust software simulation (§21's `llvmpipe`/`lavapipe`-equivalent path), never connected to a real Vulkan loader, ICD, or GPU. That was a real gap between claim and implementation, not just an imprecise sentence, and it's fixed by this section plus `crates/vulkan_backend`.

### The question this section answers

Section 21 lists Phase A (LKI-hosted Linux GPU drivers) as the highest-risk, highest-effort item and correctly does not recommend attempting it casually. But there's a narrower, much cheaper question Phase A doesn't ask: on a **userspace** machine — no kernel driver work, no ring-0, nothing SHER Kernel-specific required — can this project make *real* Vulkan calls at all, today? Vulkan is a userspace API reached through a loader + ICD; unlike kernel driver hosting, nothing about it requires special privilege or SHER Kernel integration to attempt.

The answer, verified rather than assumed: **yes.**

### What was actually done

On a macOS development machine with no Vulkan/MoltenVK preinstalled (`vulkaninfo`: not found; no MoltenVK libs present):

```bash
brew install molten-vk vulkan-loader vulkan-tools vulkan-headers
export VK_ICD_FILENAMES=/opt/homebrew/etc/vulkan/icd.d/MoltenVK_icd.json
export DYLD_LIBRARY_PATH=/opt/homebrew/lib:$DYLD_LIBRARY_PATH
vulkaninfo --summary
```

This produced a real device listing — `Apple M5`, `driverID = DRIVER_ID_MOLTENVK`, `deviceType = INTEGRATED_GPU`, `apiVersion = 1.4.357` — proving a genuine Vulkan-to-Metal translation path is viable on this machine with no GPU passthrough, no VM, and no custom driver work: MoltenVK is exactly the "Vulkan-to-Metal translation layer" this kind of investigation is supposed to check for, and it works.

`crates/vulkan_backend` was then built as a real FFI binding using [`ash`](https://docs.rs/ash) (the standard idiomatic Rust Vulkan binding, matching this document's stated preference elsewhere for reusing mature, vendor-maintained code rather than hand-rolling FFI). It implements, and tests against the real MoltenVK ICD above, verify:

1. **Real device enumeration** — `vkEnumeratePhysicalDevices` + `vkGetPhysicalDeviceProperties`, returning the ICD's actual reported name/vendor/device-type/driver-version data.
2. **A real offscreen clear-color render** — a real device-local `VkImage`, a real recorded-and-submitted command buffer (`vkCmdClearColorImage` → `vkCmdCopyImageToBuffer` with correct layout-transition barriers between them), a real fence wait, and a readback from a real host-visible `VkDeviceMemory` mapping. The crate's test asserts the read-back bytes equal the requested clear color (±2 for UNORM rounding) — meaning the test fails if the GPU/ICD didn't actually do the work, not merely if a Vulkan call returned an error code.

### What was deliberately *not* done, and why

`vulkan_backend` does not implement `gpu_abstraction::GpuDriver` or plug into `graphics_runtime::GraphicsRuntime`. That integration is real, separate design work this pass didn't try to rush:

- `GpuDriver::submit` and the rest of the trait are shaped around `SoftwareGpuDriver`'s synchronous, always-deterministic semantics (§21's reference-driver role). A real Vulkan device is asynchronous, can report `VK_ERROR_DEVICE_LOST`, and has real memory-type/heap constraints a bump allocator (`SoftwareGpuDriver::allocate_vram`'s current approach) doesn't model.
- `gpu_abstraction::GpuFault`/`reset_device` assume software-driver-shaped fault injection (§18); mapping that onto real device-lost recovery, real fence-based timeline semantics (§11's `Timeline`, currently satisfied synchronously per `graphics_runtime`'s `wait`), and real memory-type-aware allocation is enough independent work to deserve its own pass rather than being bolted on as a side effect of proving Vulkan calls work at all.

So today there are honestly two, separate things in this workspace, not one hybrid: the software simulation (§21, all of `graphics_api`/`gpu_abstraction`/`graphics_runtime`/`graphics_compat`) that the rest of this document describes, and `vulkan_backend`, a standalone, real, independently-verified Vulkan binding. Wiring the second into the first is the natural next roadmap item once the real device's error/async semantics have been thought through as carefully as §16–18 thought through the software driver's.

### Why this doesn't change §20's Phase A risk assessment

MoltenVK/real-Vulkan-in-userspace and "Phase A: LKI-hosted Linux GPU kernel drivers" are different problems at different layers. Proving userspace Vulkan calls reach a real GPU says nothing about the difficulty of hosting `amdgpu`/`i915` kernel driver code inside SHER Kernel's LKI sandbox — that risk assessment in §20–21 is unchanged. What this section changes is narrower and more honest: the project no longer claims a Mesa/Vulkan/OpenGL dependency it doesn't have, and it now also doesn't claim to have *no* real Vulkan integration when a genuine, verified one — narrower in scope than full driver hosting, but real — exists in `vulkan_backend`.
