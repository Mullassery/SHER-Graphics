# Competitive intelligence in roadmap and upgrade work

**Policy date:** 2026-10-19

Competitive findings are an input to product decisions, not a one-off report.
Every pending roadmap item, new feature, and routine dependency upgrade should
check the repository's [`AUDIT_FINDINGS.md`](AUDIT_FINDINGS.md) before work
starts, and record the decision in the PR or changelog.

## Required check before building

1. **Read `AUDIT_FINDINGS.md`.** Find the relevant competitor row and the
   ranked next actions. If the finding is stale, update it with a dated source.
2. **Name the competitor and the user problem.** A feature is not justified by
   "competitor has it". State the workflow it unlocks, who needs it, and how
   often. Mark unverified demand as a hypothesis.
3. **Prefer reuse over new code.** Check whether an existing Mullassery repo
   already provides the capability (see `BUNDLES.md` in the profile repo and
   the bundle role file in this repo, when present). Typical reuse:
   - crawling/fetching/robots/SSRF → **MudFish**
   - architecture maps / PR diffs → **CodeAtlas**
   - data-quality contracts / validation → **StatGuardian**
   - token counting / cost tables → **PyTokenCalc**
   - dependency graph / advisories / SBOM → **PyDeepMesh**
   - segmentation / CLV / churn → **ClusterAudienceKit**
   - activation / lineage / PII → **PyReverseETL**
   - fleet state sync → **SyncFleet** (only after conflict resolution)
4. **Choose one disposition:** build, integrate, defer, or reject. Write it
   down. A rejection with a reason is a successful outcome.
5. **For routine upgrades**, check whether the upgrade closes a known
   competitor gap or breaks a differentiated capability. Run the tests that
   cover the affected path; do not merge a green build that silently changes a
   user-visible contract.

## PR / changelog checklist

- [ ] Competitor and source URL/date recorded (or "no direct competitor").
- [ ] User impact and frequency stated; unverified demand labelled.
- [ ] Reuse check done against sibling repos/bundles.
- [ ] Failure mode and rollback considered (especially auth, PII, cost).
- [ ] Docs/roadmap status updated honestly.
- [ ] Tests or a manual verification command included.

## Review cadence

- Refresh the competitor row when a release changes the capability.
- Re-check the full `AUDIT_FINDINGS.md` at least quarterly and before a major
  release.
- If a finding is wrong or stale, correct it in the same PR that relies on it.

## Non-goals

- Copying features for parity.
- Adding a dependency just because a competitor uses it.
- Treating star counts or download counts as proof of user need.

## OSS gap analysis — 2026-10-24

**What matters most here:** make draw and dispatch real, then rasterize. Validating commands without executing them and shipping no pixel path makes the software-GPU claim aspirational; implement per-op execution and an offscreen readback test.

Scope: workspace v0.3.0 (`graphics_api`, `gpu_abstraction`, `graphics_runtime`,
`graphics_compat`, `vulkan_backend`). Prototype: 64 default tests (74 with
`--features vulkan`), no standalone build, no tagged releases. Comparisons are
aspirational where our code only models a capability.

### Comparable OSS projects

| Project | GitHub | License | Last activity | Why it is the comparison |
|---|---|---|---|---|
| SwiftShader | https://github.com/google/swiftshader | Apache-2.0 | Commits 2026-10-14; active | CPU implementation of Vulkan 1.3 shipped as an ICD; closest functional peer to a software GPU/Vulkan baseline |
| wgpu | https://github.com/gfx-rs/wgpu | Apache-2.0 | v30.0.1 released 2026-08-22; pushed 2026-10-24 | Safe pure-Rust graphics API over Vulkan/Metal/DX12/GL; what most Rust users pick instead of a bespoke API |
| ash | https://github.com/ash-rs/ash | Apache-2.0 / MIT (dual) | Release 0.38.0 on 2024-04-01; commits 2026-10-23 | Raw Vulkan FFI in Rust and the dependency our backend uses; the lower-level alternative |

Mesa/lavapipe is the conceptual software-driver peer, but its canonical repo is
GitLab, so it is not tabled as a GitHub comparable.

### Capability comparison

| Capability | Ours | SwiftShader | wgpu | ash | Gap |
|---|---|---|---|---|---|
| Full Vulkan API surface | No | Yes | Partial | Yes | Large: ours is a narrow native object model, not a Vulkan implementation |
| Executes draw/dispatch on a device | No | Yes | Yes | Partial | Absent: `DriverOp::Draw` is validated then ignored (`gpu_abstraction/src/lib.rs:270`) |
| CPU software execution of commands | Partial | Yes | No | No | Weaker: accounts VRAM and accepts commands but does not rasterize |
| Command-stream validation | Yes | Partial | Partial | No | Ahead in this slice: pipeline/resource/workload checks before submit |
| Device-lost and fault recovery | Yes | Partial | Partial | No | Differentiated: `VK_ERROR_DEVICE_LOST` maps to recoverable `GpuFault`; panic containment |
| Capability-gated security and audit | Yes | No | No | No | Differentiated, but only meaningful inside SHER Kernel |
| Multi-GPU | Partial | Partial | Yes | Yes | Weaker: software multi-device exists; Vulkan path selects one adapter |
| Memory-type-aware allocation | No | Yes | Yes | Yes | Absent: `allocate_vram` subtracts from a counter; no typed heaps or real buffers |
| Shader compilation / translation | No | Yes | Yes | Partial | Absent: `ShaderModule` is an opaque blob; no naga/GLSL/SPIR-V path |
| Window-system presentation | Partial | Partial | Yes | Partial | Weaker: kernel connector/framebuffer bridge and cursor primitives, no swapchain/WSI |
| Standalone build and packaging | No | Yes | Yes | Yes | Large adoption gap: relative `../SHER-KERNEL` paths, no crates.io, no releases |

### Gaps that matter (ranked)

1. **No draw or dispatch reaches a device (absent, L).** `SoftwareGpuDriver::submit`
   validates and accepts without executing, and the Vulkan adapter runs only a
   fixed offscreen clear. The `triangle` example is an object-model tour, not rendering.
2. **No software rasterization behind the "software GPU" label (absent, L).** The
   README calls the driver a scalar rasterizer, but there are no framebuffer writes
   or pixel path; SwiftShader/lavapipe are the real hardware-free baselines.
3. **No shader compilation or translation (absent, L).** `ShaderModule` is an opaque
   blob and `ROADMAP_HONEST.md` says the naga-style compiler does not exist; every
   non-trivial pipeline is blocked.
4. **Allocation is accounting, not memory management (absent, M).** VRAM is a
   decremented counter with a tag; there are no typed heaps, real buffers, or
   host-visible mappings behind the Vulkan path.
5. **No WSI or real presentation (weaker, L).** Presentation uses kernel
   connector/framebuffer/page-flip primitives; there is no `VkSurfaceKHR`/swapchain
   or DRM/KMS output, so the stack is offscreen-only and kernel-coupled.
6. **Not adoptable outside the monorepo (weaker, S/M).** Sibling path dependencies
   and no tags or published crates mean an external user cannot reproduce a version;
   small work relative to graphics features, but it blocks evaluation.

### Where we are differentiated

- Capability-gated submission (`GpuMemoryAlloc`, `GpuCommandSubmit`, `GpuAdmin`)
  with a grant/denial audit trail has no peer in SwiftShader, wgpu, or ash; it is a
  kernel-integration feature, not a general graphics feature.
- Driver-panic containment via `catch_unwind`, converted into the same `GpuFault`
  channel as a hardware fault, with hot restart and per-context isolation, is
  stronger than the default error surfaces those projects expose.
- A deterministic, Vulkan-free test baseline is useful for kernel/OS work, but it
  tests the object model and fault paths, not rendering correctness.

### Disposition

- **Build:** per-op draw/dispatch translation, then memory-type-aware allocation;
  these make the Vulkan claim truthful. Add a real offscreen triangle readback test.
- **Integrate/reuse:** keep `ash`; adopt naga for shader IR; use SwiftShader or
  lavapipe as an external ICD/test oracle; revisit the Mesa winsys seam only when a
  consumer exists.
- **Defer/reject:** production renderer, full Gallium/NIR integration, native
  hardware driver, SIMD rasterizer, and crates.io publication until standalone.

### Sources

- https://github.com/google/swiftshader (accessed 2026-10-24)
- https://github.com/gfx-rs/wgpu and /releases/tag/v30.0.1 (accessed 2026-10-24)
- https://github.com/ash-rs/ash and /releases/tag/0.38.0 (accessed 2026-10-24)
- Repo files inspected: README.md, AUDIT_FINDINGS.md, ROADMAP_HONEST.md,
  ARCHITECTURE.md, Cargo.toml, all five crate manifests, `gpu_abstraction/src/lib.rs`,
  `graphics_runtime/src/lib.rs`, `graphics_api/src/lib.rs`, `vulkan_backend/src/lib.rs`
