// Profile looks for the demo accounts: a banner, an accent and a card theme, so profile cards
// have something to show. seed.mjs applies them to fresh accounts; run this file on its own to
// dress up accounts seeded before looks existed:
//
//   node looks.mjs @annaXXXX:localhost @benXXXX:localhost
//
// It signs in with the demo password to get home-server tokens.

import * as opaque from '@serenity-kit/opaque';
import { generateKeyPairSync } from 'node:crypto';
import { deflateSync } from 'node:zlib';

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

export async function signIn(user) {
  await opaque.ready;
  const { clientLoginState, startLoginRequest } = opaque.client.startLogin({ password });
  const start = await call('POST', '/login/opaque/start', { user, start_login_request: startLoginRequest });
  const finished = opaque.client.finishLogin({ clientLoginState, loginResponse: start.login_response, password });
  const { publicKey } = generateKeyPairSync('ed25519');
  const raw = publicKey.export({ format: 'der', type: 'spki' }).subarray(-32);
  return call('POST', '/login/opaque/finish', {
    login_id: start.login_id,
    finish_login_request: finished.finishLoginRequest,
    device_name: 'Rhein demo looks',
    device_pubkey: raw.toString('base64url'),
  });
}

async function uploadBanner(token, png) {
  const media = await fetch(`${api}/media`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${token}`,
      'content-type': 'image/png',
      'x-telium-filename': 'banner.png',
      'x-telium-purpose': 'profile',
    },
    body: png,
  }).then((r) => r.json());
  return `${media.hash}?cap=${media.capability}`;
}

/// Anna gets a rose accent; Ben a banner with an azure-to-teal theme.
export async function applyLooks(annaToken, benToken) {
  await call('PATCH', '/profile', { accent_color: 0xe0558c, bio: 'Keeps the river tidy.' }, annaToken);
  const banner = await uploadBanner(benToken, bannerPng(600, 210));
  await call(
    'PATCH',
    '/profile',
    { banner_ref: banner, accent_color: 0x3e8ff0, theme_colors: [0x3e8ff0, 0x33c2c2], bio: 'Asks the big questions.' },
    benToken,
  );
}

/// A diagonal dusk gradient, generated so the demo needs no image files.
export function bannerPng(width, height) {
  const raw = Buffer.alloc((width * 3 + 1) * height);
  for (let y = 0; y < height; y++) {
    const row = y * (width * 3 + 1);
    for (let x = 0; x < width; x++) {
      const t = (x / width + y / height) / 2;
      const i = row + 1 + x * 3;
      raw[i] = 40 + 180 * t;
      raw[i + 1] = 60 + 60 * (1 - t);
      raw[i + 2] = 160 + 80 * (1 - t);
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
  header[8] = 8;
  header[9] = 2;
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

if (import.meta.url === `file://${process.argv[1]}`) {
  const [annaId, benId] = process.argv.slice(2);
  if (!annaId || !benId) {
    console.error('usage: node looks.mjs @anna:server @ben:server');
    process.exit(1);
  }
  const anna = await signIn(annaId);
  const ben = await signIn(benId);
  await applyLooks(anna.access_token, ben.access_token);
  console.log(`Applied profile looks to ${annaId} and ${benId}.`);
}
