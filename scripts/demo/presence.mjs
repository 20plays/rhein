// Keeps a demo account online with a rich-presence activity, so Rhein's member list and profile
// cards have something to show. Runs until stopped.
//
//   node presence.mjs @benXXXX:localhost [music|game]

import { signIn } from './looks.mjs';

const base = process.env.TELIUM_API ?? 'http://localhost:8448';
const api = `${base}/_telium/v1`;
const [user, kind = 'music'] = process.argv.slice(2);
if (!user) {
  console.error('usage: node presence.mjs @user:server [music|game]');
  process.exit(1);
}

const now = Date.now();
const activities = {
  music: {
    name: 'Spotify',
    type: 2,
    details: 'Rhine Valley Morning',
    state: 'The Riverside Band',
    timestamps: { start: now - 42_000, end: now + 180_000 },
    assets: { large_image: 'https://picsum.photos/seed/rhein/300', large_text: 'Songs of the River' },
  },
  game: {
    name: 'Factorio',
    type: 0,
    details: 'Building a rail network',
    state: 'Multiplayer',
    timestamps: { start: now - 3_600_000 },
    party: { size: [3, 8] },
    buttons: [{ label: 'Visit website', url: 'https://factorio.com' }],
  },
};
const activity = activities[kind];

const home = await signIn(user);
const post = (path, body, token) =>
  fetch(api + path, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: JSON.stringify(body),
  }).then((r) => r.json());
const { server_name: audience, gateway } = await (await fetch(`${base}/.well-known/telium/server`)).json();
const { assertion } = await post('/assertions', { audience }, home.access_token);
const { session_token: token } = await post('/sessions', { assertion });

const ws = new WebSocket(gateway);
ws.onmessage = (event) => {
  const frame = JSON.parse(event.data);
  if (frame.op === 'HELLO') {
    ws.send(JSON.stringify({
      op: 'IDENTIFY',
      d: {
        token,
        intents: 0,
        protocol: { version: 1 },
        presence: {
          status: 'online',
          activity: `telium-rich-v1:${JSON.stringify(activity)}`,
          rich_activities: [activity],
        },
      },
    }));
    setInterval(() => ws.send(JSON.stringify({ op: 'HEARTBEAT' })), frame.d?.heartbeat_interval_ms ?? 30_000);
  }
  if (frame.op === 'READY') console.log(`${user} is online: ${activity.name}`);
};
ws.onclose = (event) => {
  console.log(`gateway closed (${event.code})`);
  process.exit(1);
};
