# Architecture — as-built

This page is the **as-built** counterpart to [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md),
which is a much larger design document covering both what's implemented
and what's still just planned (it says so explicitly in its own "Status"
line). This page only diagrams what exists in the repository today,
verified by building/testing it, not what's designed for later.

For the full design rationale — why native-first instead of
Vulkan-first, the Mesa integration strategy, the LKI driver-hosting
migration path, the security/capability model — see `ARCHITECTURE.md`.
For a plain list of what's tested vs. not built at all, see
[`../../ROADMAP_HONEST.md`](../../ROADMAP_HONEST.md).

## Crate dependency graph (as-built)

```mermaid
graph TD
    subgraph SHER-Kernel["SHER-Kernel (sibling repo, path dependency)"]
        sher_common["sher_common"]
        sher_objectmodel["sher_objectmodel"]
        sher_security["sher_security"]
        hal["hal"]
        gpu_driver["gpu_driver::GPUDriver"]
    end

    subgraph SHER-Graphics["SHER-Graphics (this repo)"]
        graphics_api["graphics_api\n(native object model)"]
        gpu_abstraction["gpu_abstraction\n(GpuDriver trait, SoftwareGpuDriver)"]
        graphics_runtime["graphics_runtime\n(GraphicsRuntime, owns presentation)"]
        graphics_compat["graphics_compat\n(DrmCompatShim — reference shim,\nnot wired to anything real)"]
        vulkan_backend["vulkan_backend\n(real ash/Vulkan FFI —\nstandalone, not integrated)"]
    end

    graphics_api --> sher_common
    graphics_api --> sher_objectmodel

    gpu_abstraction --> graphics_api
    gpu_abstraction --> sher_common
    gpu_abstraction --> hal

    graphics_runtime --> graphics_api
    graphics_runtime --> gpu_abstraction
    graphics_runtime --> sher_common
    graphics_runtime --> sher_objectmodel
    graphics_runtime --> sher_security
    graphics_runtime --> hal
    graphics_runtime -->|"instantiates GPUDriver directly\n(presentation backend, not\nreimplemented — see boundary\nnote below)"| gpu_driver

    graphics_compat --> graphics_api
    graphics_compat --> gpu_abstraction
    graphics_compat --> graphics_runtime
    graphics_compat --> sher_common
    graphics_compat --> sher_objectmodel

    vulkan_backend -.->|"no dependency on any\nother crate in this repo"| vulkan_backend

    style vulkan_backend stroke-dasharray: 5 5
    style graphics_compat stroke-dasharray: 5 5
```

Dashed borders mark crates that are real but **not wired into the rest of
the stack**: `vulkan_backend` has zero Cargo dependency on any other crate
in this repo (by design — see its module docs), and `graphics_compat`'s
`DrmCompatShim` wraps `graphics_runtime` but nothing outside its own test
suite calls it yet.

## Runtime layering and the SHER-Kernel boundary

```mermaid
graph LR
    App["Application\n(e.g. examples/triangle.rs)"] --> API["graphics_api\nGraphicsApi trait"]
    API --> Runtime["graphics_runtime\nGraphicsRuntime&lt;D: GpuDriver&gt;"]
    Runtime --> Abstraction["gpu_abstraction\nGpuDriver trait"]
    Abstraction --> SoftDriver["SoftwareGpuDriver\n(in-memory reference impl,\nlives in gpu_abstraction)"]
    Runtime --> KernelDriver["SHER-Kernel gpu_driver::GPUDriver\n(presentation: connectors,\nframebuffers, page flip)"]
    Abstraction -.extends.-> HAL["SHER-Kernel hal::HardwareDriver"]

    classDef kernel fill:#00000010,stroke:#888,stroke-dasharray: 3 3;
    class KernelDriver,HAL kernel;
```

**Boundary rule, verified for this pass:** `graphics_runtime` is the only
place in this repo that instantiates `gpu_driver::GPUDriver`
(`crates/graphics_runtime/src/lib.rs:143`,`:149`). That type is *defined*
once, in `SHER-Kernel/crates/gpu_driver/src/lib.rs:65` — SHER-Graphics
consumes it for presentation (connectors/framebuffers/page-flip), it does
not redefine or reimplement kernel-level GPU driver responsibilities.
Downstream, `SHER-Display` deliberately does **not** instantiate
`GPUDriver` itself — it consumes only value types from this repo — so the
kernel's own driver type has exactly one owner-that-instantiates-it across
the whole family. This is the boundary discipline the SHER family is
built around; if that ever changes (e.g. a second crate starts calling
`GPUDriver::new`), treat it as an architecture regression, not a
refactor.

## What each layer actually does today

| Layer | Real, tested | Not real |
|---|---|---|
| `graphics_api` | Object model, `GraphicsApi` trait | — |
| `gpu_abstraction` | `GpuDriver` trait, `SoftwareGpuDriver` (multi-GPU, VRAM accounting, fault injection) | No real hardware driver implements `GpuDriver` |
| `graphics_runtime` | Full submit/validate/present pipeline against `SoftwareGpuDriver`; real presentation bridge to `gpu_driver::GPUDriver`; capability enforcement; driver-panic containment | Presentation bridge has never been exercised against real display hardware — `gpu_driver` itself is a DRM/KMS-shaped simulation in SHER-Kernel |
| `graphics_compat` | Object-id-level request/response shapes matching real DRM ioctl semantics, tested against `SoftwareGpuDriver` | `ModeGetResources` always returns empty; `PrimeHandleToFd` returns an `ObjectId`, not a real fd; nothing calls this crate outside its own tests |
| `vulkan_backend` | Real device enumeration + offscreen clear-color render against a real Vulkan ICD | Not reachable from any other crate in this repo |
