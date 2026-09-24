# Rhein

A native desktop client for [Telium](../telium), written in Rust with GPUI.

Rhein talks to Telium servers directly and is meant to work next to the official web client on the same account: same OPAQUE login, same guild sessions, same gateway.

## Status

| Milestone | State |
| --- | --- |
| M0 – workspace, local test servers | done |
| M1 – login (OPAQUE), guild chat | done: history, sending, editing, deleting, reactions, typing, members, file upload |
| UI phases 1–6 | done: friends (requests, accept, block), conversation list, voice stage with live voice states; settings (account, devices, servers, appearance), status, guild/channel/member menus, profile cards, Ctrl+K switcher, keyboard navigation; emoji picker, autocomplete, message context menu, pins, threads, search; design system, layout with unread/mentions, rich text (web-client markdown), images, link previews through the guild server, replies, multi-line composer, paste and drag & drop uploads |
| M2 – multiple servers (federation) | done: extra servers, invites across servers, one gateway per server |
| Profiles and rich presence | done: banners, accents and card themes (shown and edited); activities of others with artwork through camo; sharing your own from a Discord-compatible RPC server (IPC and WebSocket), game detection and MPRIS |
| M3 – DMs (MLS) | done: openmls with Telium's profile (vendored patch for ts-mls's GroupInfo encoding), recovery vault, cross-signing and identity continuity, the conversation engine with self-join and the history mirror, DMs and group chats in the chat view; interop tested against the web client's own code. Open: read receipts. See [`mplan.md`](mplan.md) |
| M4 – voice/video | planned in [`mplan.md`](mplan.md) |

The plan and its risks are in `~/.claude/plans/lies-die-das-hier-vectorized-snail.md`.

## Layout

| Crate | Contents |
| --- | --- |
| `rhein-proto` | Wire types (REST, gateway, discovery) |
| `rhein-auth` | OPAQUE client (identical to `@serenity-kit/opaque` 1.1.0) and the device key |
| `rhein-net` | Discovery, REST client, sign-in, assertion → guild session, gateway with reconnect |
| `rhein-store` | GPUI state (`Client` entity) on a tokio runtime (`Io`) |
| `rhein-mls` | Telium's MLS profile on openmls, recovery vault, cross-signing and identity continuity, DM payloads |
| `rhein-rpc` | Local activity sources: Discord RPC server (`discord-ipc-N`, WebSocket 6463–6472), game detection from Discord's detectable list, MPRIS; no GPUI |
| `rhein-ui` | Design system: black-and-white theme tokens, motion tokens, bundled fonts (Geist, Geist Mono), Lucide icons, components |
| `rhein` | The app: views built from `rhein-ui` components |

## Running it

```sh
scripts/dev-server.sh a      # Telium "localhost" on :8448 (built from .dev/telium, a clone)
scripts/dev-server.sh b      # Telium "b.localhost" on :8449, federates with a
cargo run -p rhein
```

The login form defaults to `localhost:8448` (`RHEIN_SERVER` overrides it). Saved accounts are stored in `~/.config/rhein/accounts.json` (mode 0600), or in `RHEIN_CONFIG_DIR` if that is set; a `session.json` from earlier versions is taken over as the first account.

Invites are entered as `server/CODE`, for example `localhost:8449/~abcd1234`.

### Demo data

```sh
cd scripts/demo && npm install
node seed.mjs ../../.dev/config-demo
RHEIN_CONFIG_DIR=$PWD/../../.dev/config-demo cargo run -p rhein
```

The script registers two accounts, Anna and Ben, with password `rhein demo password`. It creates a guild "Riverside" with a few messages, gives both profile looks (banner, accent, theme) and signs Rhein in as Anna.

```sh
node looks.mjs @anna…:localhost @ben…:localhost      # profile looks for accounts seeded earlier
node presence.mjs @ben…:localhost music             # keep Ben online with an activity (or: game)
cargo run -p rhein-rpc --example watch              # print what Rhein would share from this machine
```

Debug builds can start on a given screen, for screenshots: `RHEIN_DEV_OPEN=home|settings|settings=Activity|join|switcher|signout|account|switch=@user:server|profile=@user:server`.

## Tests

```sh
cargo test --workspace

# against running dev servers:
RHEIN_LIVE=localhost:8448 RHEIN_LIVE_B=localhost:8449 \
  cargo test -p rhein-net --test live -- --test-threads=1 --nocapture
```

`crates/rhein-store/tests/web_interop_live.rs` runs the Telium web client's own DM code (from `.dev/telium/web`, after `bun install` there) against Rhein; `crates/rhein-mls/tests/interop.rs` needs `cd scripts/mls-interop && npm install`.

The live tests run one at a time: each registers a fresh account, and a Telium server on SQLite answers concurrent registrations with "database is locked".

`login_account_made_by_web_client` also takes `RHEIN_WEB_ACCOUNT=localpart:password:exportKey`, from an account registered with serenity-kit. It checks that Rhein can log into web-client accounts and derives the same export key.

`~/projects/telium` itself is never modified. Servers run from the clone in `.dev/telium`.
