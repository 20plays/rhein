<p align="center">
  <img src="packaging/chat.rhein.Rhein.png" width="128" alt="Rhein logo">
</p>

<h1 align="center">Rhein</h1>

<p align="center">A native desktop client for <a href="https://docs.telium.chat/">Telium</a>, written in Rust with GPUI.</p>

Rhein talks to Telium servers directly and works next to the official web client on the same account. It uses the same OPAQUE login, the same guild sessions and gateway, and the same end-to-end encrypted conversations. Like Telium's protocol 1, it is alpha software.

## Features

- **Chat.** Guild channels with history, editing, deleting, replies, reactions, pins, threads and search. Messages render the web client's markdown, images and link previews. Uploads work by paste or drag and drop, and H.264 videos (MP4, MOV) play inline.
- **Federation.** One account can join several servers. Invites work across servers, and Rhein keeps one gateway per server.
- **Encrypted DMs.** Direct messages and group chats use MLS on openmls with Telium's profile, and they interoperate with the web client. Rhein also supports the recovery vault, cross-signing, identity continuity, message requests and the encrypted history mirror, so a new device sees earlier messages.
- **Voice and video.** Guild voice and DM calls run over mesh or an SFU, using the LiveKit Rust SDK on libwebrtc. Calls are end-to-end encrypted with keys derived from MLS. Rhein supports speaking indicators, mute and deafen, push-to-talk, per-user volume, echo cancellation and region probing. Screen sharing goes through the desktop portal.
- **Profiles and presence.** Banners, accents and card themes. Rhein can share activities from a Discord-compatible RPC server (IPC and WebSocket), from game detection and from MPRIS.
- **Guilds and bots.** AutoMod rules, forums, channel settings, timeouts and nicknames. Bot buttons, selects, cards, modals and ephemeral messages work.
- **Keyboard first.** A Ctrl+K switcher, keyboard navigation, an emoji picker and autocomplete.

Not done yet: read receipts, camera capture, the recovery key, a gapless switch from mesh to SFU, and translations.

## Installing

Rhein runs on Linux.

```sh
./install                  # check system packages, build, install into /usr (sudo when needed)
./install --prefix ~/.local
./install --build-only     # just target/production/rhein
./install --uninstall
```

`./install` first checks for the system packages Rhein needs and offers to install any that are missing with pacman, apt, dnf or zypper. Pass `--yes` to skip the prompt, or `--no-deps` to skip the check. The packages include:

- the compilers and headers for the build
- ALSA, PipeWire and the Vulkan loader
- `xdg-desktop-portal` with the backend for your desktop (wlroots, Hyprland, GNOME or KDE), plus `-gtk` on wlroots and Hyprland for the file chooser

It then builds the `production` profile, a release build with thin LTO and stripped symbols. It installs the `rhein` binary, the desktop entry `chat.rhein.Rhein.desktop` and its icon. Fonts, icons and sounds are compiled into the binary.

Accounts are saved in `~/.config/rhein/accounts.json` (mode 0600). Set `RHEIN_CONFIG_DIR` to use another directory. Invites are entered as `server/CODE`, for example `example.org/~abcd1234`.

## Development

The development scripts run local Telium servers built from a clone of the Telium server in `.dev/telium`.

```sh
./rheinrun                   # both dev servers, the dev SFU, then the client; stops them on exit
./rheinrun --demo            # the same, signed in as the demo account
./rheinrun --help            # --no-sfu, --null-audio, --release
```

You can also start each part yourself:

```sh
scripts/dev-sfu.sh           # optional: a Telium SFU in front of .dev/bin/livekit-server
scripts/dev-server.sh a      # Telium "localhost" on :8448
scripts/dev-server.sh b      # Telium "b.localhost" on :8449, federated with a
cargo run -p rhein
```

In debug builds the login form defaults to `localhost:8448`, and `RHEIN_SERVER` overrides that. Debug builds can also start on a given screen, which helps with screenshots: `RHEIN_DEV_OPEN=home|settings|settings=Activity|join|switcher|signout|account|switch=@user:server|profile=@user:server`. Release builds have none of these development hooks.

### Demo data

```sh
cd scripts/demo && npm install
node seed.mjs ../../.dev/config-demo
RHEIN_CONFIG_DIR=$PWD/../../.dev/config-demo cargo run -p rhein
```

The seed script registers two accounts, Anna and Ben, with the password `rhein demo password`. It creates a guild called "Riverside" with a few messages, gives both accounts profile looks, and signs Rhein in as Anna.

```sh
node looks.mjs @anna…:localhost @ben…:localhost   # profile looks for accounts seeded earlier
node presence.mjs @ben…:localhost music          # keep Ben online with an activity (or: game)
cargo run -p rhein-rpc --example watch           # print what Rhein would share from this machine
```

### Tests

```sh
cargo test --workspace

# against running dev servers, one test at a time:
RHEIN_LIVE=localhost:8448 RHEIN_LIVE_B=localhost:8449 \
  cargo test -p rhein-net --test live -- --test-threads=1 --nocapture
```

The live tests run one at a time because each one registers a fresh account, and a Telium server on SQLite rejects concurrent registrations.

The interop tests run the web client's own code against Rhein:
- `crates/rhein-mls/tests/interop.rs` needs `npm install` in `scripts/mls-interop`.
- `crates/rhein-store/tests/web_interop_live.rs` needs `bun install` in `.dev/telium/web`.

`login_account_made_by_web_client` also reads `RHEIN_WEB_ACCOUNT=localpart:password:exportKey`, taken from an account registered with serenity-kit.

## Workspace

| Crate | Contents |
| --- | --- |
| `rhein-proto` | Wire types: REST, gateway, discovery |
| `rhein-auth` | OPAQUE client (compatible with `@serenity-kit/opaque` 1.1.0), device key, signed JSON |
| `rhein-net` | Discovery, REST client, sign-in, guild sessions, gateway with reconnect |
| `rhein-mls` | Telium's MLS profile on openmls, recovery vault, cross-signing, identity continuity, DM payloads |
| `rhein-voice` | Mesh and SFU calls on libwebrtc, audio, screen capture, call encryption |
| `rhein-rpc` | Local activity sources: Discord RPC server, game detection, MPRIS |
| `rhein-store` | Application state as GPUI entities, backed by a tokio runtime |
| `rhein-ui` | Design system: theme and motion tokens, bundled fonts (Geist, Geist Mono), Lucide icons, components |
| `rhein` | The app |

`vendor/` holds the upstream code Rhein patches: GPUI and its wgpu renderer from Zed, openmls, libwebrtc and openh264-sys2. Each comes with a `RHEIN.md` that describes the change.

## License

Apache-2.0
