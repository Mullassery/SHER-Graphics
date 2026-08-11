.PHONY: help build test check fmt fmt-check clippy doc clean install

help:
	@echo "SHER Graphics Build System"
	@echo "==========================="
	@echo ""
	@echo "Targets:"
	@echo "  make install    - Bootstrap dev environment (checks toolchain + sibling SHER-Kernel checkout, builds, tests)"
	@echo "  make build      - cargo build (debug)"
	@echo "  make release    - cargo build --release"
	@echo "  make test       - Run all tests across the workspace"
	@echo "  make check      - cargo check (fast type/borrow check, no codegen)"
	@echo "  make fmt        - Format all crates"
	@echo "  make fmt-check  - Check formatting without modifying files (CI)"
	@echo "  make clippy     - Run clippy lints"
	@echo "  make doc        - Build and open API documentation"
	@echo "  make clean      - Remove build artifacts"

install:
	@./scripts/install.sh

build:
	cargo build

release:
	cargo build --release

test:
	cargo test

check:
	cargo check --workspace

# Deliberately not `--all`: rustfmt's --all also formats path dependencies,
# and SHER-Kernel is one (../SHER-Kernel via relative path). Plain `cargo
# fmt` stays scoped to this workspace's own members.
fmt:
	cargo fmt

fmt-check:
	cargo fmt -- --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

doc:
	cargo doc --workspace --no-deps --open

clean:
	cargo clean

.DEFAULT_GOAL := help
