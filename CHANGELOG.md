# Changelog

Every release of Rhein, newest first. The release notes on GitHub come from here.

## 0.1.0 – 2026-10-28

The first public release of Rhein, a native Telium client for Linux, and the start of its open beta. It works next to Telium's web client on the same account.

- **Chat**: guild channels with history, editing, deleting, replies, reactions, pins, threads and search. Markdown, images and link previews render as in the web client. Uploads by paste or drag and drop, and H.264 videos (MP4, MOV) play inline.
- **Federation**: one account on several servers, invites across servers, and one gateway per server.
- **Encrypted DMs**: direct messages and group chats use MLS on openmls with Telium's profile and interoperate with the web client. Recovery vault, cross-signing, identity continuity, message requests, and the encrypted history mirror, so a new device sees earlier messages.
- **Voice and video**: guild voice and DM calls over mesh or an SFU, encrypted end to end with keys derived from MLS. Speaking indicators, mute and deafen, push-to-talk, per-user volume, echo cancellation, region probing, and screen sharing through the desktop portal.
- **Profiles and presence**: banners, accents and card themes. Activities from a Discord-compatible RPC server, game detection and MPRIS.
- **Guilds and bots**: AutoMod rules, forums, channel settings, timeouts and nicknames. Bot buttons, selects, cards, modals and ephemeral messages.
- **Keyboard**: a Ctrl+K switcher, keyboard navigation, an emoji picker and autocomplete.
- **Install**: a prebuilt build for x86_64 Linux that runs on glibc 2.35 or newer, on Wayland and X11.

Not done yet: read receipts, camera capture, the recovery key, a gapless switch from mesh to SFU, and translations.
