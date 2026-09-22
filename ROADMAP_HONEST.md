# ROADMAP_HONEST.md

Status supplement to `ARCHITECTURE.md`. `ARCHITECTURE.md` is a design
document — most of it (sections 1–22) describes architecture that
*precedes* implementation, by its own admission in its "Status" line.
This file states, plainly and without hedging, what is actually built,
what actually works, what's tested, and what's simply not built — as of
**2026-09-20**, verified by running the commands below myself in this
pass, not by re-reading old commit messages.

**Quick-fix follow-up pass, 2026-09-22**: re-ran the full validation suite
(fmt/build/test/clippy/triangle example — all still green, 63/63 tests),
ran `cargo audit` for real (network was available this time — see the
technical-debt section below), re-verified the `.unwrap()`/`.expect()`
count and the `ModeGetResources` zero-callers claim with fresh greps, and
disclosed the `ModeGetResources` stub in-line via doc comments. No new
behavior was implemented; see `CHANGELOG.md`'s `[Unreleased]` section for
the exact diff.

## Bucket 1 — Built and verified working (by me, this pass)

Commands run, real output, no cherry-picking:

- `cargo fmt -- --check` — clean, no diff.
- `cargo clippy --workspace --all-targets -- -D warnings` — zero warnings.
- `cargo build --workspace --all-targets` — succeeds.
- `cargo test --workspace` — **63/63 pass**: 15 (`gpu_abstraction`) + 6
  (`graphics_api`) + 4 (`graphics_compat`) + 34 (`graphics_runtime`) + 4
  (`vulkan_backend`). The 4 `vulkan_backend` tests ran against a real
  Vulkan device on this machine (not the "skip, no loader" path) and
  passed, including the clear-color-round-trips-through-real-GPU-memory
  test.
- `cargo run -p graphics_runtime --example triangle` — runs end-to-end,
  produces real object IDs at every stage (device → shaders → pipeline →
  resource → command stream → submit → wait → present), exits 0.

Functionally, this means the following are real and tested, not aspirational:
- Native object model (`graphics_api`): devices, resources, command
  streams, timelines, pipelines.
- `SoftwareGpuDriver` reference driver with multi-GPU support.
- Command-stream validation (rejects unbound pipelines, unknown
  resources, mismatched workload classes — see the
  `submit_rejects_*`/`pipeline_creation_requires_known_shader` tests in
  `crates/graphics_runtime/src/lib.rs`).
- GPU fault detection/recovery, isolated per device.
- Driver-panic containment: a panicking driver inside `submit()` is
  caught via `catch_unwind`, converted to a `GpuFault`, and the device is
  hot-restartable afterward (`crates/graphics_runtime/src/lib.rs`, tests
  `submit_catches_a_driver_panic_instead_of_crashing` and
  `a_caught_driver_panic_marks_the_device_faulted_and_is_hot_restartable`).
- Capability-gated operations (`GpuMemoryAlloc`, `GpuCommandSubmit`,
  `GpuAdmin`) reusing `SHER-Kernel`'s capability/tier model, with an audit
  trail for grants/denials.
- Cursor primitives (`set_cursor_image`, `set_cursor_position`,
  `show_cursor`, `hide_cursor`) — tested, and correctly scoped to *not*
  touch position tracking, focus, or input (those stay in
  SHER-Input/SHER-Display).
- `vulkan_backend`: real `ash` FFI, real physical-device enumeration, a
  real offscreen clear-color render verified by reading back actual GPU
  memory — genuinely works, verified against MoltenVK on this machine.
- No architecture-boundary violation found: `graphics_runtime` consumes
  `gpu_driver::GPUDriver`, defined once in `SHER-Kernel`
  (`SHER-Kernel/crates/gpu_driver/src/lib.rs:65`), not reimplemented here
  (`crates/graphics_runtime/src/lib.rs:143`,`:149`). This is the correct
  side of the boundary rule: SHER-Graphics is allowed to instantiate the
  kernel's own driver type; it must not (and does not) redefine one.

## Bucket 2 — Built, but not integrated / not what it sounds like

- **`vulkan_backend` is real but disconnected.** It does not implement
  `gpu_abstraction::GpuDriver` and is not reachable through
  `graphics_runtime::GraphicsRuntime`. Nothing in the tested end-to-end
  path (the triangle example) touches real Vulkan. This is disclosed
  already in the README's "Known Issues"; confirmed still true.
- **`graphics_compat`'s `DrmCompatShim` is a reference shim, not a real
  DRM-ioctl implementation.** It's honestly documented as such in its
  module docs (`crates/graphics_compat/src/lib.rs:1-23`), and, as of this
  pass, in an explicit doc comment on `DrmRequest::ModeGetResources`
  itself: `DrmRequest::ModeGetResources` **always returns an empty `Vec`**
  regardless of what devices/connectors exist
  (`crates/graphics_compat/src/lib.rs:43-57`, handler at `:98`) — it
  never actually queries the underlying `GraphicsRuntime`/`GPUDriver` for
  real connector data. **Re-verified this pass**: grepped this repo plus
  `SHER-Display` and `SHER-Kernel` for `ModeGetResources`/`DrmCompatShim`
  — zero callers anywhere outside `graphics_compat`'s own tests.
  `SHER-Display/Cargo.toml` declares a `graphics_compat` path dependency,
  but `SHER-Display/docs/architecture/README.md` itself marks that edge
  "declared; not yet called — Phase 3." If anyone ever tries to point a
  real Mesa winsys at this shim expecting `ModeGetResources` to enumerate
  anything, it will silently report zero resources — this is now disclosed
  in-line via doc comments rather than only in this file. Still not
  implemented (real enumeration needs actual hardware I/O and remains real
  feature work), and still low risk today since nothing consumes it.
- **`graphics_compat::DrmRequest::PrimeHandleToFd` does not return a real
  file descriptor** — it returns the resource's `ObjectId` standing in for
  one (`crates/graphics_compat/src/lib.rs:79`, and the module docs say so
  explicitly). Fine as a reference/test shim; would need real work before
  any real DMA-buf export is possible.

## Bucket 3 — Not built at all (no hedging)

- **Phase A (LKI-hosted Linux GPU drivers)**: does not exist. Zero code.
  `ARCHITECTURE.md` §20-21 itself calls this the highest-risk, highest-
  effort item and does not recommend attempting it casually — that
  assessment still holds.
- **Mesa/Gallium/NIR integration**: does not exist. No Mesa dependency
  anywhere in this workspace. `graphics_compat` models the *shape* of the
  seam Mesa would eventually talk to; it does not talk to Mesa.
- **Native shader compilation path** (`naga`-style Rust-native compiler
  integration, §13 of `ARCHITECTURE.md`): does not exist.
- **Process/WASM/eBPF-level driver sandboxing**: does not exist anywhere
  in this workspace. Only in-process panic containment exists (Bucket 1).
  The actual sandbox mechanism this would depend on
  (`driver_runtime::DriverContainer`/`SyscallPolicy`) lives in
  `SHER-Kernel` and is itself not yet built there either.
- **Any real hardware GPU I/O**: this entire workspace, `vulkan_backend`
  aside, runs in-memory/software-only. No DRM/KMS ioctls, no real VRAM
  allocation, no real command-buffer submission to real hardware outside
  `vulkan_backend`'s narrow, disconnected offscreen-render path.
- **Published crates**: none of these crates are published to crates.io.
  This repo cannot build standalone; it requires `SHER-Kernel` checked out
  as a sibling directory via relative path dependencies.
- **Tagged releases**: no GitHub releases/tags exist; `CHANGELOG.md`'s
  version headers reflect `Cargo.toml`'s `workspace.package.version`
  bumps only, not published releases.

## Technical debt (concrete, with file:line)

Ordered roughly by how much a dedicated follow-up session would need to
do about it.

1. **`cargo-audit` CI job status: RESOLVED THIS PASS.** The job added in
   the previous pass hadn't run against `main` yet, so its result was
   unverified. This pass had network access and ran `cargo audit` for
   real, locally: fetched the RustSec advisory database (1,258 advisories
   loaded), scanned all 65 dependencies in `Cargo.lock` — **zero
   vulnerabilities found**, exit code 0. This doesn't confirm the *CI job
   itself* is wired correctly (that still needs a real push to `main` to
   verify the GitHub Actions environment specifically), but it does
   confirm the dependency tree is currently clean.
2. **Dependency freshness: RESOLVED THIS PASS (previously blocked on no
   network access).** Ran `cargo audit` with real network access this
   pass — see item 1. Only dependency in this workspace outside the
   `sher_*`/`hal`/`gpu_driver` path family is `ash 0.38` (plus its
   transitive `libloading`/`cfg-if`) in `crates/vulkan_backend/Cargo.toml`;
   none flagged.
3. **`crates/graphics_runtime/src/lib.rs` is 1,361 lines** — the largest
   file in the workspace by a wide margin (`graphics_api` is 354,
   `gpu_abstraction` 493, `vulkan_backend` 825, `graphics_compat` 156). It
   holds device management, command submission/validation, cursor
   primitives, fault handling, and capability/audit logic all in one
   `impl` block set. Not broken — clippy is clean and tests are thorough —
   but it's the one module where "one more feature" risks becoming
   unmanageable. **Follow-up-worthy**: split into submodules (e.g.
   `device.rs`, `submit.rs`, `cursor.rs`, `fault.rs`) before adding
   anything else non-trivial to this crate.
4. **`graphics_compat::DrmRequest::ModeGetResources` always returns an
   empty list** (`crates/graphics_compat/src/lib.rs:43-57`, handler
   `:98`) — see Bucket 2. **Disclosure fixed this pass**: added an
   explicit doc comment on the enum variant and a short comment on the
   handler arm stating this is an unimplemented stub by design, not an
   oversight, plus a pointer to this file. The actual behavior
   (always-empty) is unchanged and still not fixed — real DRM resource
   enumeration is real feature work requiring actual hardware I/O, out of
   scope for a disclosure-only pass, and nothing consumes this crate yet
   (re-verified via fresh grep across this repo, `SHER-Display`, and
   `SHER-Kernel`).
5. **124 `.unwrap()`/`.expect()` call sites across `crates/**/*.rs`.**
   **Re-verified this pass with a fresh grep**: count is still exactly
   124, and the "exactly one in production code" claim still holds.
   Spot-checked: the overwhelming majority are inside `#[cfg(test)]`
   modules and `crates/graphics_runtime/examples/triangle.rs` (example
   code, where `.expect()` with a clear message is idiomatic); one line
   (`crates/graphics_runtime/src/lib.rs:428`) is a doc comment that only
   mentions `.unwrap()` in prose, not an actual call. The one production,
   non-test `.expect()` is still:
   `crates/gpu_abstraction/src/lib.rs:169` — `.expect("SoftwareGpuDriver
   constructed with zero devices")`, guarding an internal invariant that
   the driver is always built with at least one device. Not a bug today
   (the invariant is enforced at construction), but worth a `debug_assert!`
   + graceful `Result` path if `SoftwareGpuDriver`'s construction path
   ever becomes more flexible. Low priority.
6. **No `docs/architecture/` diagrams existed before this pass** — added
   `docs/architecture/README.md` with Mermaid diagrams of the as-built
   crate graph and the `SHER-Kernel` dependency, to complement (not
   replace) `ARCHITECTURE.md`'s much larger design document. Not a defect,
   just a discoverability gap that's now closed.
7. **`Cargo.lock` was `.gitignore`d** despite this workspace producing a
   real binary (the `triangle` example) — fixed in this pass (now tracked;
   see `CHANGELOG.md`). This is a recurring pattern across sibling repos
   in this org; worth checking any *new* SHER-family repo for the same
   mistake at creation time rather than discovering it later.
8. **Version drift between `Cargo.toml` and any external "0.x" claims**:
   none found — `workspace.package.version = "0.3.0"` and the README's
   Rust badge, LICENSE, and CI badge are all internally consistent as of
   this pass.

## Explicitly not fixed in this pass (deliberate, documentation-first)

- `graphics_compat::ModeGetResources`'s always-empty *behavior* (item 4
  above) — the disclosure (doc comments) was fixed this pass; the
  underlying always-empty implementation is real but small feature work
  (needs actual connector/CRTC/encoder state to query), deliberately
  deferred to a dedicated session per this pass's quick-fix-only scope.
- `graphics_runtime`'s file-size/module-split debt (item 3) — a
  non-trivial refactor, deferred.
- `PrimeHandleToFd` returning an `ObjectId` instead of a real fd — by
  design, already documented in the module and enum docs; skipped per
  this pass's explicit scope.
- Wiring `vulkan_backend` into `gpu_abstraction::GpuDriver` — already
  correctly identified in the existing README/`ARCHITECTURE.md` as
  "real, separate design work," and this pass agrees with that framing;
  not attempted here.
- Verifying the new `cargo-audit` CI job specifically goes green *in
  GitHub Actions* — this pass ran `cargo audit` for real, locally (see
  item 1: 65 dependencies scanned, zero vulnerabilities), which is a
  stronger result than the previous pass had, but confirming the GitHub
  Actions job itself executes correctly still requires a real push to
  `main`, which this pass does not do (no push performed; see final
  report).
