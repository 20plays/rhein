# Vendored openh264-sys2 0.9.9 (OpenH264 1bca623)

A copy of the `openh264-sys2` crate, with only what Rhein needs to play H.264 videos from messages
(`rhein_voice::movie`). Rhein's root `Cargo.toml` patches crates.io with it.

Changes:
- Only OpenH264's decoder and its common code are kept and built; the encoder, the video
  processing and the console tools are left out.
- libwebrtc (vendor/libwebrtc) links OpenH264's encoder and common code from another revision.
  Both copies in one binary clash at link time, so `build.rs` renames this one's C++ namespaces
  (`WelsCommon` and `WelsDec` become `RheinWelsCommon` and `RheinWelsDec`: the linker merges
  inline code and vtables by name, local or not), links it into a single object (`ld -r`) and
  makes every symbol in it local except the decoder's entry points
  (`WelsCreateDecoder`, `WelsDestroyDecoder`, `WelsGetDecoderCapability`), which libwebrtc lacks.
  The encoder functions and `WelsGetCodecVersion` the bindings name resolve to libwebrtc's;
  Rhein never calls them. Building needs `ld`, `objcopy` and `ar` (binutils).
- No assembly is built: its symbols would clash the same way and cannot be renamed. The decoder
  runs on its C++ code; a chat video, at most 1280 pixels wide once shown, decodes well within
  real time.
- The libloading tests are gone with the `tests` folder; `source` stays as an empty feature for
  the `openh264` crate, which asks for it.
