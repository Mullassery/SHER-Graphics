# SHER Graphics - Installation & Development Setup

**Status**: Design phase for the native runtime (`graphics_api`/`gpu_abstraction`/
`graphics_runtime`/`graphics_compat`) — those crate skeletons compile and
pass tests against a software reference driver, no real GPU driver wired in
yet. Separately, `vulkan_backend` is a real, working Vulkan (`ash`) binding
against an actual loader/ICD (device enumeration + an offscreen
clear-color render) — see `README.md` and that crate's module docs for
exactly what's real there. This guide covers setting up the workspace for
development, not deploying a running graphics stack (there isn't one to
deploy yet — see [`ARCHITECTURE.md`](./ARCHITECTURE.md) section 21 for
what's implementable first).

---

## System Requirements

- **Rust**: 1.75+ (edition 2021)
- **OS**: Linux, macOS, or any platform `cargo` runs on — none of the
  current crates touch hardware yet, so there's no Linux-only requirement
  at this stage
- **Disk**: ~1 GB for both repos plus build artifacts

---

## Pre-Installation Checklist

```bash
rustc --version   # 1.75 or newer
cargo --version
git --version
```

---

## Installation Steps

SHER Graphics depends on `sher_common`, `sher_objectmodel`, `hal`, and
`gpu_driver` from SHER Kernel via **relative path dependencies**
(`../SHER-Kernel/crates/...`), not published crates. Both repositories must
be checked out as siblings:

```
~/
├── SHER-Kernel/
└── SHER-Graphics/
```

### 1. Clone both repositories

```bash
git clone https://github.com/Mullassery/SHER-KERNEL.git
git clone https://github.com/Mullassery/SHER-Graphics.git
```

They must sit side by side — if you already have `SHER-Kernel` checked out
elsewhere, symlink or re-clone it as a sibling of `SHER-Graphics` rather
than editing the path dependencies.

### 2. Build

```bash
cd SHER-Graphics
cargo build
```

### 3. Run tests

```bash
cargo test
```

Four of the five crates (`graphics_api`, `gpu_abstraction`, `graphics_runtime`,
`graphics_compat`) run entirely in software — `gpu_abstraction::SoftwareGpuDriver`
is a hardware-independent reference driver, so no GPU or root access is
required to build or test.

The fifth, `vulkan_backend`, is a real Vulkan (`ash`) binding, but only
`dlopen()`s the actual Vulkan loader at **runtime**, not build/link time —
see "Optional: real Vulkan" below — so `cargo build`/`cargo test` also
succeed with no Vulkan installed at all; its GPU-dependent tests report
"skipping: no Vulkan loader/ICD available" and pass rather than fail.

### Or, automated

```bash
./scripts/install.sh
```

Checks the Rust toolchain, verifies (or offers to clone) the sibling
`SHER-Kernel` checkout, then builds and tests the workspace. See
[`scripts/install.sh`](./scripts/install.sh).

---

## Verification

```bash
# Per-crate
cargo test -p graphics_api
cargo test -p gpu_abstraction
cargo test -p graphics_runtime
cargo test -p graphics_compat
cargo test -p vulkan_backend

# With output
cargo test -- --nocapture

# API documentation
cargo doc --open
```

Expected: 61 tests passing, zero warnings from this workspace's own crates
(pre-existing warnings from `sher_objectmodel` in SHER-Kernel are unrelated
and safe to ignore). If no Vulkan loader is installed, `vulkan_backend`'s
GPU-dependent tests still count as passing — they detect that up front and
skip, per its module docs.

---

## Optional: real Vulkan (for `vulkan_backend`)

Not required to build or pass tests — `vulkan_backend`'s tests skip
gracefully without a Vulkan loader/ICD present (see above). Installing one
lets those tests exercise a real device instead:

**macOS** (verified against an Apple M5 via MoltenVK during this repo's own
Vulkan-integration pass):

```bash
brew install molten-vk vulkan-loader vulkan-tools vulkan-headers
export VK_ICD_FILENAMES=/opt/homebrew/etc/vulkan/icd.d/MoltenVK_icd.json
export DYLD_LIBRARY_PATH=/opt/homebrew/lib:$DYLD_LIBRARY_PATH
vulkaninfo --summary   # confirm a real device is listed
cargo test -p vulkan_backend -- --nocapture
```

Homebrew's `/opt/homebrew/lib` isn't on macOS's default `dyld` search path,
so both environment variables above are required every session (or export
them from your shell profile) — without them the loader simply won't be
found and `vulkan_backend`'s tests will (correctly) skip rather than fail.

**Linux**: `mesa-vulkan-drivers` (lavapipe, a real CPU-rendered Vulkan ICD)
from your distro's package manager — this is what this repo's CI installs:

```bash
sudo apt-get install -y mesa-vulkan-drivers vulkan-tools libvulkan1
cargo test -p vulkan_backend -- --nocapture
```

---

## Troubleshooting

### `failed to load source for dependency 'sher_common'` / path errors

`SHER-Kernel` isn't checked out as a sibling directory, or is checked out
under a different name. Confirm:

```bash
ls ../SHER-Kernel/crates/common/Cargo.toml
```

If that fails, clone `SHER-Kernel` next to `SHER-Graphics` (step 1 above).

### Build failures after pulling new changes

```bash
cargo clean
cargo update
cargo build
```

If the failure is inside a `sher_*`/`hal`/`gpu_driver` type, check whether
`SHER-Kernel` has moved ahead of what this repo's `Cargo.lock` expects —
`git -C ../SHER-Kernel log --oneline -5` to see what changed.

### `cargo test` hangs or is slow

It shouldn't be — every current crate is in-memory/software-only with no
I/O or hardware access. A hang points at a bug in a newly added test, not
environment setup.

---

## Uninstall / Clean

```bash
cargo clean          # remove build artifacts for this repo only
rm -rf target/        # equivalent, if cargo clean is unavailable
```

Removing the repo directories themselves is sufficient beyond that — nothing
here installs to system paths, services, or `~/.cargo/bin` yet.

---

## Support

- **This repo**: https://github.com/Mullassery/SHER-Graphics
- **SHER Kernel**: https://github.com/Mullassery/SHER-KERNEL
- **Architecture**: [`ARCHITECTURE.md`](./ARCHITECTURE.md)
