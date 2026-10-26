# Vendored gpui_wgpu (Zed 23973bd)

A copy of Zed's `crates/gpui_wgpu` at `zed-industries/zed@23973bd`, the revision Rhein takes GPUI
from. Rhein's root `Cargo.toml` patches the Zed git source with it. The workspace dependencies of
Zed's manifest are spelled out as Zed's workspace has them; benches and dev-dependencies are left
out.

Changes (marked `Rhein:` in `src/cosmic_text_system.rs`):
- A font with colour glyphs (COLR palettes or bitmap strikes) may be the font of a run although
  it has no 'm'. Upstream skips every such face, so `Noto Color Emoji`, which `rhein_ui::emoji`
  names for emoji runs, failed to load ("requested font family contains no font matching"), and
  cosmic-text fell back to whatever covers the code points first: DejaVu Sans, drawing most
  emoji as black outlines.
- Fonts with colour glyphs are drawn as emoji, not only one named `NotoColorEmoji`.

Both follow the sonorahq/gpui fork's commit `5715f3d`, which Rhein used before.

To move to a newer Zed: copy `crates/gpui_wgpu/src` from that revision, apply the two changes
again, and update the revisions here and in `Cargo.toml`.
