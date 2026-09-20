# Changelog

All notable changes to this project are documented here. Format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this
project does not yet follow Semantic Versioning strictly (no crates.io
release exists — see README).

Entries below are reconstructed from `git log`, not hand-maintained during
development, so wording is a summary of the actual commit, not a
contemporaneous release note. Run `git log --oneline` for the exact
commit-level history.

## [Unreleased]

### Added
- `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, this
  `CHANGELOG.md`, and `ROADMAP_HONEST.md`.
- `.github/dependabot.yml` (Cargo + GitHub Actions ecosystems).
- `.github/ISSUE_TEMPLATE/` (bug report, feature request) and a PR
  template.
- `docs/architecture/README.md`: Mermaid diagrams of the as-built crate
  layering and the `SHER-Kernel` dependency, alongside (not replacing)
  the existing design-level `ARCHITECTURE.md`.
- `cargo-audit` CI job (advisory-database dependency scan).

### Fixed
- `Cargo.lock` was listed in `.gitignore` despite this workspace shipping
  a binary example (`triangle`); it is now tracked for reproducible
  builds.
- `INSTALLATION.md` claimed 61 total tests; actual current count is 63
  (59 in the software-driver crates + 4 in `vulkan_backend`).

## [0.3.0] - 2026-09-06

### Changed
- Relicensed from a proprietary/unspecified license to Apache License 2.0
  (`33a8cb3`).

### Fixed
- Stale license badge said "SHER Graphics License"; corrected to Apache
  2.0, and added a "Use Cases" section to the README (`69eef62`).
- `clippy::chunks_exact_to_as_chunks` lint fix from toolchain drift, not a
  behavioral change (`b3d53fa`).

### Added
- Driver-panic containment: `GraphicsRuntime::submit` now catches a
  panicking driver via `catch_unwind` and routes it through the existing
  `GpuFault`/`recover_device` path instead of unwinding into the caller
  (`fac4691`).
- Documentation: verified cross-repo Cargo-level compatibility across the
  5-repo SHER family (`e447f0e`); known-issue documentation from an
  external critique review (`de49454`).

## [0.2.0] - 2026-08-16

### Added
- Real Vulkan backend (`vulkan_backend`, via `ash`): physical-device
  enumeration and an offscreen clear-color render against a real
  Vulkan loader/ICD, verified against MoltenVK on macOS (`e923817`).
  Standalone — not wired into `gpu_abstraction::GpuDriver`.
- CI now installs Mesa lavapipe so `vulkan_backend`'s tests exercise a
  real (if CPU-rendered) Vulkan device instead of skipping (`980fa1a`).

### Fixed
- Corrected Vulkan/OpenGL/Mesa framing in docs — the project previously
  described itself as having Mesa/Vulkan dependency it didn't yet have
  (`980fa1a`).
- Corrected a stale test-count claim in the docs; added a "Known Issues"
  section (`2c99264`).

## [0.1.0] - 2026-08-11

### Added
- Initial scaffold: native graphics API (`graphics_api`), GPU abstraction
  layer (`gpu_abstraction`), and runtime (`graphics_runtime`) (`f3795a8`).
- Command-stream validation, GPU fault detection/recovery, an end-to-end
  example, and initial CI (`1004c21`).
- Multi-GPU support and `GpuCommandSubmit` capability enforcement
  (`8b66a74`).
- `GpuAdmin` capability enforcement on fault recovery and the
  `driver_mut()` escape hatch (`1011121`, `925ff90`).
- GPU capability audit trail wired into `graphics_runtime` (`71e2e38`).
- Cursor rendering primitives: `set_cursor_image`, `set_cursor_position`,
  `show_cursor`, `hide_cursor` (`7c150c5`).
- README reworked for GitHub presentation (`dff8ec7`).
