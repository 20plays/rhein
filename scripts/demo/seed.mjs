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
