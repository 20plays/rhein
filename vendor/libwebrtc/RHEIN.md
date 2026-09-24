# Vendored libwebrtc (LiveKit rust-sdks, rev c20fa7c)

A copy of the `libwebrtc` crate from https://github.com/livekit/rust-sdks at
`c20fa7c37e50cb97f1a588a6ad50534b53c2037e`. Rhein's root `Cargo.toml` patches the git
dependency with it.

Changes:
- `RtpTransceiver::set_direction`. webrtc-sys binds it, but the safe wrapper did not expose it.
  Telium's mesh calls depend on it: each peer connection has four m-lines, and a source is turned
  on or off by swapping the track and changing the direction, never by renegotiating new m-lines.
- Workspace-inherited manifest fields are written out, and `webrtc-sys` comes from the same git rev.
