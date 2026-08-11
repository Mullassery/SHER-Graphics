# SHER Graphics - Installation & Development Setup

**Status**: Design phase — crate skeletons compile and pass tests; no hardware
backend yet. This guide covers setting up the workspace for development,
not deploying a running graphics stack (there isn't one to deploy yet — see
[`ARCHITECTURE.md`](./ARCHITECTURE.md) section 21 for what's implementable
first).

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

All four crates (`graphics_api`, `gpu_abstraction`, `graphics_runtime`,
`graphics_compat`) run entirely in software — `gpu_abstraction::SoftwareGpuDriver`
is a hardware-independent reference driver, so no GPU or root access is
required to build or test.

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

# With output
cargo test -- --nocapture

# API documentation
cargo doc --open
```

Expected: 31 tests passing, zero warnings from this workspace's own crates
(pre-existing warnings from `sher_objectmodel` in SHER-Kernel are unrelated
and safe to ignore).

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
