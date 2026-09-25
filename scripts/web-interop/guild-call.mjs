// Drives the web client's own guild call keys (web/src/lib/guild-call-keys.js, unmodified, from
// the clone) for crates/rhein-mls/tests/call_group_interop.rs. Only its store (`mls`) and its
// API are stand-ins: the MLS work is ts-mls through the web client's code.
//
// Protocol: one JSON object per line on stdin, one JSON answer per line on stdout.
//   { op: "new", name, user, device, channel }             -> { ok }
//   { op: "offer", name }                                  -> { ok, key_package }
//   { op: "begin", name, group_id, member }                -> { ok }
//   { op: "hear", name, event }                            -> { ok }
//   { op: "change", name, view }                           -> { ok, body }   (the commit it posted)
//   { op: "key", name }                                    -> { ok, key, epoch }
import { createRequire } from 'node:module';
import { createInterface } from 'node:readline';
import { join, resolve } from 'node:path';

const WEB = resolve(process.env.TELIUM_WEB ?? join(import.meta.dir, '../../.dev/telium/web'));
const need = createRequire(join(WEB, 'package.json'));
await import(need.resolve('fake-indexeddb/auto'));
const memory = new Map();
globalThis.localStorage = {
  getItem: (k) => (memory.has(k) ? memory.get(k) : null),
  setItem: (k, v) => memory.set(k, String(v)),
  removeItem: (k) => memory.delete(k),
};
globalThis.navigator ??= { platform: 'rhein-interop' };

const { GuildCallKeys } = await import(join(WEB, 'src/lib/guild-call-keys.js'));
const { capabilities } = await import(join(WEB, 'src/lib/mls.js'));
const tsmls = await import(need.resolve('ts-mls'));
const cs = await tsmls.getCiphersuiteImpl(tsmls.getCiphersuiteFromName('MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519'));
const te = new TextEncoder();
const b64 = (bytes) => Buffer.from(bytes).toString('base64');

const devices = new Map();

async function create({ name, user, device, channel }) {
  const credential = { credentialType: 'basic', identity: te.encode(`${user}|${device}`) };
  const first = await tsmls.generateKeyPackage(credential, capabilities(), tsmls.defaultLifetime, [], cs);
  const pair = { signKey: first.privatePackage.signaturePrivateKey, publicKey: first.publicPackage.leafNode.signaturePublicKey };
  const d = { view: null, posted: null, key: null };
  const mls = { cs, deviceId: device, credential: () => credential, signatureKeyPair: async () => pair };
  const api = {
    voiceE2ee: async () => d.view,
    voiceE2eeCommit: async (_channel, body) => { d.posted = body; return { epoch: body.epoch + 1 }; },
  };
  d.keys = new GuildCallKeys({
    mls, api, channelId: channel, me: user,
    onKey: (raw, epoch) => { d.key = { key: b64(raw), epoch }; },
    onTrouble: (e) => { d.trouble = String(e?.message || e); },
  });
  devices.set(name, d);
  return {};
}

async function handle(req) {
  if (req.op === 'new') return create(req);
  const d = devices.get(req.name);
  if (!d) throw new Error(`no device ${req.name}`);
  switch (req.op) {
    case 'offer': return { key_package: await d.keys.offer() };
    case 'begin': await d.keys.begin({ group_id: req.group_id, member: req.member }); return {};
    case 'hear': await d.keys.hear(req.event); return {};
    case 'change': d.view = req.view; d.posted = null; await d.keys.run(() => d.keys.change()); return { body: d.posted };
    case 'key': if (d.trouble) throw new Error(d.trouble); return d.key ?? {};
    default: throw new Error(`unknown op ${req.op}`);
  }
}

const rl = createInterface({ input: process.stdin });
for await (const line of rl) {
  let answer;
  try {
    answer = { ok: true, ...(await handle(JSON.parse(line))) };
  } catch (e) {
    answer = { ok: false, error: String(e?.stack || e) };
  }
  process.stdout.write(`${JSON.stringify(answer)}\n`);
}
