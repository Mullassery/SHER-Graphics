## What this changes

<!-- What, and why. Reference an issue if one exists. -->

## Checklist

- [ ] `cargo fmt -- --check` passes (not `fmt --all` — it would reach into the `SHER-Kernel` sibling checkout, see the Makefile comment)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo run -p graphics_runtime --example triangle` still runs end-to-end, if you touched `graphics_api`/`gpu_abstraction`/`graphics_runtime`
- [ ] New behavior has a test; if it's genuinely untested, said so explicitly below
- [ ] No `TODO`/`FIXME` left in committed code
- [ ] Doesn't instantiate a driver that `SHER-Kernel` (or another SHER subsystem) already owns

## Testing performed

<!-- Real commands you ran and their real output, not "should work." -->

## Anything left deliberately unfinished

<!-- State plainly if something is a partial fix, or "N/A" -->
