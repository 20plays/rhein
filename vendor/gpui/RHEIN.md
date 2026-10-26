# Vendored gpui (Zed 23973bd)

A copy of Zed's `crates/gpui` at `zed-industries/zed@23973bd`, the revision Rhein takes GPUI from.
Rhein's root `Cargo.toml` patches the Zed git source with it, so `gpui_platform`, `gpui_wgpu` and
`gpui_linux` use it too.

`vendor.py` made the copy and wrote `Cargo.toml`: Zed's `workspace = true` dependencies are spelled
out as Zed's workspace has them, Zed's own crates become git dependencies on the same revision, and
dev-dependencies, examples and benches are left out (GPUI's own tests are not run here).

Changes are marked `Rhein:` in the source.

None yet.

To move to a newer Zed: run `vendor.py` on a checkout of that revision, apply the changes again,
and update the revision here and in Rhein's `Cargo.toml`.
