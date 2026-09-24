// Seeds a local Telium dev server (scripts/dev-server.sh a) with demo data for Rhein.
//
//   cd scripts/demo && npm install
//   node seed.mjs ../../.dev/config-demo
//   RHEIN_CONFIG_DIR=$PWD/../../.dev/config-demo cargo run -p rhein
//
// Registers two accounts with @serenity-kit/opaque (the library the Telium web client uses), so
// they are exactly what a web sign-up produces. Creates a guild with a little history and writes a
// Rhein session file for the first account into the given directory.

import * as opaque from '@serenity-kit/opaque';
import { mkdirSync, writeFileSync } from 'node:fs';
import { deflateSync } from 'node:zlib';

await opaque.ready;

const base = process.env.TELIUM_API ?? 'http://localhost:8448';
const api = `${base}/_telium/v1`;
const password = 'rhein demo password';

async function call(method, path, body, token) {
  const res = await fetch(api + path, {
    method,
    headers: {
      'content-type': 'application/json',
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path}: ${text}`);
  return text ? JSON.parse(text) : null;
}

async function register(localpart) {
  const { clientRegistrationState, registrationRequest } = opaque.client.startRegistration({ password });
  const start = await call('POST', '/register/opaque/start', { localpart, registration_request: registrationRequest });
  const { registrationRecord } = opaque.client.finishRegistration({
    clientRegistrationState,
    registrationResponse: start.registration_response,
    password,
  });
  return call('POST', '/register/opaque/finish', {
    localpart,
    registration_record: registrationRecord,
    device_name: 'Rhein demo seed',
  });
}

async function guildToken(home, audience) {
  const { assertion } = await call('POST', '/assertions', { audience }, home.access_token);
  return (await call('POST', '/sessions', { assertion })).session_token;
}

const { server_name: serverName } = await (await fetch(`${base}/.well-known/telium/server`)).json();
const tag = Date.now().toString(36).slice(-4);
const anna = await register(`anna${tag}`);
const ben = await register(`ben${tag}`);
await call('PATCH', '/profile', { display_name: 'Anna' }, anna.access_token).catch(() => {});
await call('PATCH', '/profile', { display_name: 'Ben' }, ben.access_token).catch(() => {});

const annaGuild = await guildToken(anna, serverName);
const benGuild = await guildToken(ben, serverName);
const guild = await call('POST', '/guilds', { name: 'Riverside' }, annaGuild);
const general = (await call('GET', `/guilds/${guild.id}/channels`, null, annaGuild)).channels.find(
  (c) => c.type === 'text',
);
await call('POST', `/guilds/${guild.id}/channels`, { name: 'announcements', type: 'text' }, annaGuild);
await call('POST', `/guilds/${guild.id}/channels`, { name: 'Lounge', type: 'voice' }, annaGuild);
const invite = await call('POST', `/guilds/${guild.id}/invites`, { channel_id: general.id }, annaGuild);
await call('POST', `/invites/${encodeURIComponent(invite.code)}/join`, {}, benGuild);

const annaTag = `<${anna.user_id}>`;
const history = [
  [annaGuild, 'Hi everyone! Welcome to Riverside.'],
  [benGuild, 'Hey Anna 👋'],
  [benGuild, 'Is the new client working yet?'],
  [annaGuild, "Yes – it's native, written in Rust with GPUI, and talks straight to the Telium server."],
  [annaGuild, 'Sign-in uses OPAQUE, so the password never leaves the device.'],
];
let last;
for (const [token, content] of history) {
  last = await call('POST', `/channels/${general.id}/messages`, { content }, token);
}
await call('PUT', `/messages/${last.id}/reactions/${encodeURIComponent('🎉')}/@me`, null, benGuild);

// Everything the message renderer understands, in a few realistic messages.
const announcements = (await call('GET', `/guilds/${guild.id}/channels`, null, annaGuild)).channels.find(
  (c) => c.name === 'announcements',
);
const rich = [
  [benGuild, `Formatting works too: **bold**, *italic*, ~~struck~~, \`inline code\` and a ||spoiler||. Ping ${annaTag}!`],
  [annaGuild, 'Here is a snippet:\n```rust\nfn main() {\n    println!("hello from Rhein");\n}\n```'],
  [benGuild, '> Quotes look like this.\n> They can span lines.\nAnd then normal text again.'],
  [annaGuild, `Release notes live in <#${announcements.id}>.`],
  [benGuild, 'Link previews come from the guild server: https://www.rust-lang.org'],
  [annaGuild, 'GIFs animate inline: https://upload.wikimedia.org/wikipedia/commons/2/2c/Rotating_earth_%28large%29.gif'],
  [benGuild, '🎉🚀'],
];
for (const [token, content] of rich) {
  last = await call('POST', `/channels/${general.id}/messages`, { content }, token);
}
// A reply to Anna's first message, and an image attachment.
const first = (await call('GET', `/channels/${general.id}/messages?limit=100`, null, annaGuild)).messages[0];
await call('POST', `/channels/${general.id}/messages`, { content: 'Replying to the very first message.', reply_to: first.id }, benGuild);
const png = riverPng(64, 32);
const upload = await fetch(`${base}/_telium/v1/media`, {
  method: 'POST',
  headers: {
    authorization: `Bearer ${anna.access_token}`,
    'content-type': 'image/png',
    'x-telium-filename': 'river.png',
    'x-telium-purpose': 'attachment',
    'x-telium-channel-id': general.id,
    'x-telium-guild-id': guild.id,
    'x-telium-width': '64',
    'x-telium-height': '32',
  },
  body: png,
}).then((r) => r.json());
await call('POST', `/channels/${general.id}/messages`, { content: 'A tiny picture of the river:', attachments: [upload] }, annaGuild);
await call('POST', `/channels/${general.id}/messages`, { content: `Bring friends: ${base}/invite/${invite.code}` }, benGuild);

// A custom emoji, pins and a thread.
const wave = await call(
  'POST',
  `/guilds/${guild.id}/emojis`,
  { name: 'riverwave', data: riverPng(64, 64).toString('base64url'), mime: 'image/png' },
  annaGuild,
);
await call('POST', `/channels/${general.id}/messages`, { content: `Our own emoji: <riverwave:${wave.id}>` }, annaGuild);
const rules = await call('POST', `/channels/${general.id}/messages`, { content: '**House rules:** be kind, stay on topic, no spam.' }, annaGuild);
await call('PUT', `/channels/${general.id}/pins/${rules.id}`, null, annaGuild);
await call('PUT', `/channels/${general.id}/pins/${first.id}`, null, annaGuild);
const question = await call('POST', `/channels/${general.id}/messages`, { content: 'Which feature should come next: voice or DMs?' }, benGuild);
const thread = await call('POST', `/channels/${general.id}/threads`, { title: 'Voice or DMs?', message_id: question.id }, benGuild);
for (const [token, content] of [
  [annaGuild, 'DMs first – end-to-end encryption matters.'],
  [benGuild, 'Voice would be fun though 🎙️'],
  [annaGuild, 'Both are on the roadmap: M3 is DMs, M4 is voice.'],
]) {
  await call('POST', `/channels/${thread.id}/messages`, { content }, token);
}

const dir = process.argv[2] ?? '.dev/config-demo';
mkdirSync(dir, { recursive: true });
writeFileSync(
  `${dir}/session.json`,
  JSON.stringify({
    home: base.replace(/^https?:\/\//, ''),
    user_id: anna.user_id,
    device_id: anna.device_id,
    access_token: anna.access_token,
    expires_at: anna.expires_at,
    device_seed: '',
    servers: [],
  }),
  { mode: 0o600 },
);
// Ben's credentials, for poking the demo from outside (e.g. posting as Ben with curl).
writeFileSync(`${dir}/ben.json`, JSON.stringify({ token: benGuild, channel: general.id, invite: invite.code }));

console.log(`Seeded ${anna.user_id} and ${ben.user_id} in guild "${guild.name}" (invite ${serverName}/${invite.code}).`);
console.log(`Both passwords: "${password}"`);

/// A small valid PNG (a blue-green gradient), so the demo needs no image files.
function riverPng(width, height) {
  const raw = Buffer.alloc((width * 3 + 1) * height);
  for (let y = 0; y < height; y++) {
    const row = y * (width * 3 + 1);
    raw[row] = 0;
    for (let x = 0; x < width; x++) {
      const i = row + 1 + x * 3;
      raw[i] = 30 + (x * 40) / width;
      raw[i + 1] = 90 + (y * 120) / height;
      raw[i + 2] = 160 + (x * 80) / width;
    }
  }
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([len, body, crc]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8; // bit depth
  header[9] = 2; // RGB
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', header),
    chunk('IDAT', deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const b of bytes) {
    crc ^= b;
    for (let k = 0; k < 8; k++) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
  }
  return (crc ^ 0xffffffff) >>> 0;
}
