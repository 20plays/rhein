# Vendored gpui (Zed 23973bd)

A copy of Zed's `crates/gpui` at `zed-industries/zed@23973bd`, the revision Rhein takes GPUI from.
Rhein's root `Cargo.toml` patches the Zed git source with it, so `gpui_platform`, `gpui_wgpu` and
`gpui_linux` use it too.

`vendor.py` made the copy and wrote `Cargo.toml`: Zed's `workspace = true` dependencies are spelled
out as Zed's workspace has them, Zed's own crates become git dependencies on the same revision, and
dev-dependencies, examples and benches are left out (GPUI's own tests are not run here).

Changes are marked `Rhein:` in the source.

- **Kept-size cached views** (`ViewElement::keep_size`, `view.rs`): a cached view whose height
  follows its content, such as a row of a `list`. While it is not dirty it is laid out at the size
  it had in the last frame, without being rendered; a dirty one is rendered during layout to be
  measured, and cached again (`request_layout_kept`, and the first branch of `prepaint_view`).
  Unlike other cached views it is also reused while an ancestor draws afresh: GPUI sets
  `Window::refreshing` for the whole subtree of a cached view that redraws, which would redraw
  every row of a history whenever the history redraws. A new `Window::refresh_requested`, set
  only by `Window::refresh`, is what makes such a view redraw regardless.
  `Window::peek_element_state` reads the kept size before the view takes its state. What the
  view's child views read while they lay out and prepaint is part of what it read, so a view
  inside it (a video playing in a row) still redraws it after it was reused.

  Tests: `crates/rhein-ui/tests/kept_rows.rs`.
- **Kept-size views drawn moved** (`view.rs` `movable`, `window.rs` `reuse_prepaint_at`,
  `reuse_paint_at`, `scene.rs` `replay_moved`): a clean kept-size view whose position changed (a
  list scrolled) is drawn from what it drew, moved, instead of rendered and laid out again. Hitboxes
  and every primitive move with it (a sprite's matrix translation by `d - R·d`). It has to be the
  same size and text style, wholly inside its clip before and after, with nothing that keeps
  positions outside hitboxes and primitives (deferred draws, tooltips, input handlers). Its mouse
  listeners still hold the old positions, so they are not called (`Frame::moved_listeners`); the
  view is never drawn moved while the pointer is over it or a button is held, and when the pointer
  comes over one, the dispatch notifies it so it draws afresh where it is (`Frame::moved_views`,
  carried over like hitboxes when an ancestor is reused).

To move to a newer Zed: run `vendor.py` on a checkout of that revision, apply the changes again,
and update the revision here and in Rhein's `Cargo.toml`.
