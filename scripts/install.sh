#!/usr/bin/env bash
#
# SHER Graphics - dev environment bootstrap
#
# Checks the Rust toolchain, verifies (or clones) the sibling SHER-Kernel
# checkout that graphics_api/gpu_abstraction/graphics_runtime/graphics_compat
# depend on via relative path, then builds and tests the workspace.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PARENT_DIR="$(cd "$REPO_ROOT/.." && pwd)"
SHER_KERNEL_DIR="$PARENT_DIR/SHER-Kernel"
SHER_KERNEL_URL="https://github.com/Mullassery/SHER-KERNEL.git"
MIN_RUST_MINOR=75

info()  { echo "[install] $*"; }
warn()  { echo "[install] WARNING: $*" >&2; }
fail()  { echo "[install] ERROR: $*" >&2; exit 1; }

info "SHER Graphics dev environment setup"
info "Repo root: $REPO_ROOT"

# 1. Toolchain checks
command -v cargo >/dev/null 2>&1 || fail "cargo not found. Install Rust via https://rustup.rs and re-run."
command -v rustc >/dev/null 2>&1 || fail "rustc not found. Install Rust via https://rustup.rs and re-run."

RUST_VERSION="$(rustc --version | awk '{print $2}')"
RUST_MINOR="$(echo "$RUST_VERSION" | cut -d. -f2)"
if [ "${RUST_MINOR:-0}" -lt "$MIN_RUST_MINOR" ]; then
    warn "rustc $RUST_VERSION detected; SHER Graphics targets 1.${MIN_RUST_MINOR}+. Consider: rustup update"
else
    info "rustc $RUST_VERSION OK"
fi

# 2. Sibling SHER-Kernel checkout
if [ -f "$SHER_KERNEL_DIR/crates/common/Cargo.toml" ]; then
    info "Found sibling SHER-Kernel checkout at $SHER_KERNEL_DIR"
else
    warn "SHER-Kernel not found at $SHER_KERNEL_DIR (required as a path dependency)."
    read -r -p "[install] Clone it now from $SHER_KERNEL_URL ? [y/N] " REPLY
    case "$REPLY" in
        [yY]*)
            git clone "$SHER_KERNEL_URL" "$SHER_KERNEL_DIR"
            ;;
        *)
            fail "SHER-Kernel is required. Clone it manually to $SHER_KERNEL_DIR and re-run."
            ;;
    esac
fi

# 3. Build and test
cd "$REPO_ROOT"
info "cargo build --release"
cargo build --release

info "cargo test"
cargo test

info "Done. 'cargo doc --open' for API docs, or see INSTALLATION.md."
