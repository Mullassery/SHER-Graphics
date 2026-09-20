# Contributing to SHER Graphics

This is early, architecture-defining work. The surface is still moving, so
the most valuable contributions right now are **design feedback and
issues**, not large PRs against code that may be restructured shortly.
Open an issue if you find:

- A boundary violation — e.g. code that reimplements something
  `SHER-Kernel`'s `gpu_driver` already owns, instead of consuming it.
- A gap between [`ARCHITECTURE.md`](./ARCHITECTURE.md) and what's actually
  implemented (the README's "What exists today" section is the source of
  truth for current state; `ARCHITECTURE.md` mixes shipped and
  not-yet-built design).
- A case the capability/security model doesn't handle correctly.
- A test that passes but shouldn't, or a claim in the docs that doesn't
  match the code.

## Before you open a PR

This repo cannot be built standalone — it depends on
[`SHER-Kernel`](https://github.com/Mullassery/SHER-KERNEL) via relative
path (`../SHER-Kernel`), so both repos must be checked out as sibling
directories. See [`INSTALLATION.md`](./INSTALLATION.md) for setup.

For any PR touching code:

```bash
cargo fmt                                          # not `fmt --all` — see Makefile comment
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p graphics_runtime --example triangle && cargo run -p graphics_runtime --example triangle
```

All four must pass. CI (`.github/workflows/ci.yml`) runs the same checks
plus `vulkan_backend`'s tests against a real Vulkan ICD (Mesa lavapipe on
Linux).

## Scope boundaries (read this before adding code)

- **Never instantiate a driver another SHER subsystem already owns.**
  `SHER-Kernel`'s `gpu_driver::GPUDriver` is the presentation backend this
  repo builds on top of; it is not reimplemented here. If you're adding
  code that talks to display hardware, it should go through `gpu_driver`,
  not around it.
- Capability-gated operations (`GpuMemoryAlloc`, `GpuCommandSubmit`,
  `GpuAdmin`) reuse `SHER-Kernel`'s existing capability/tier model. Don't
  add a graphics-specific permission system.
- No `TODO`/`FIXME` placeholders left in committed code — either implement
  the thing or don't add the stub. If something is genuinely unfinished,
  document it plainly in the README's "Known Issues" section instead of a
  silent code comment.
- Don't describe untested code as working. If you add a feature, add a
  test that exercises it, or say explicitly in the PR description that
  it's untested and why.

## Commit style

Recent history (`git log --oneline`) favors short, factual, present-tense
subject lines describing what changed and, where it matters, why (e.g.
"Fix stale license badge... add Use Cases", "Bump workspace version to
0.2.0"). Match that style rather than Conventional-Commits prefixes.

## Reporting security issues

See [`SECURITY.md`](./SECURITY.md) — do not open a public issue for a
security concern.
