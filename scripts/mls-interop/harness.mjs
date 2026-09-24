// Stands in for the Telium web client's MLS layer (web/src/lib/mls.js) in Rhein's interop tests.
// Every call below is the one mls.js makes, with the same arguments: suite, capabilities (defaults
// plus the policy extension 0xF7A0), default lifetime, ratchet tree in Welcome and GroupInfo,
// commits as PrivateMessage, external commits as PublicMessage.
//
// Protocol: one JSON object per line on stdin, one JSON answer per line on stdout.
//   { "op": "...", ... } -> { "ok": true, ... } | { "ok": false, "error": "..." }

import {
  createApplicationMessage, createCommit, createGroup,
  createGroupInfoWithExternalPubAndRatchetTree, decodeMlsMessage,
  defaultCapabilities, defaultLifetime, emptyPskIndex, encodeMlsMessage,
  generateKeyPackage, generateKeyPackageWithKey, getCiphersuiteFromName, getCiphersuiteImpl,
  joinGroup, joinGroupExternal, mlsExporter, processMessage,
} from 'ts-mls';
import { defaultClientConfig } from 'ts-mls/clientConfig.js';
import { createInterface } from 'node:readline';

const SUITE = 'MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519';
const POLICY_EXTENSION = 0xF7A0;
const te = new TextEncoder();
const td = new TextDecoder();
const cs = await getCiphersuiteImpl(getCiphersuiteFromName(SUITE));

const b64 = (bytes) => Buffer.from(bytes).toString('base64');
const unb64 = (text) => new Uint8Array(Buffer.from(text, 'base64'));
const decodeMessage = (text) => {
  const decoded = decodeMlsMessage(unb64(text), 0);
  if (!decoded) throw new Error('not a well-formed MLS message');
  return decoded[0];
};
const wire = {
  keyPackage: (keyPackage) => encodeMlsMessage({ version: 'mls10', wireformat: 'mls_key_package', keyPackage }),
  welcome: (welcome) => encodeMlsMessage({ version: 'mls10', wireformat: 'mls_welcome', welcome }),
  publicMessage: (publicMessage) => encodeMlsMessage({ version: 'mls10', wireformat: 'mls_public_message', publicMessage }),
  groupInfo: (groupInfo) => encodeMlsMessage({ version: 'mls10', wireformat: 'mls_group_info', groupInfo }),
};

function capabilities() {
  const base = defaultCapabilities();
  return { ...base, extensions: [...new Set([...(base.extensions || []), POLICY_EXTENSION])] };
}

function policyExtension(policy) {
  return { extensionType: POLICY_EXTENSION, extensionData: te.encode(JSON.stringify(policy)) };
}

function policyIn(extensions) {
  const ext = (extensions || []).find((e) => Number(e.extensionType) === POLICY_EXTENSION);
  return ext ? JSON.parse(td.decode(ext.extensionData)) : null;
}

function pack(tag, body) {
  const bytes = te.encode(body);
  const out = new Uint8Array(bytes.length + 1);
  out[0] = tag;
  out.set(bytes, 1);
  return out;
}

/// One simulated device: its signature key, stashed key packages and groups.
const clients = new Map();

async function client(id, identity) {
  if (clients.has(id)) return clients.get(id);
  const credential = { credentialType: 'basic', identity: te.encode(identity) };
  const first = await generateKeyPackage(credential, capabilities(), defaultLifetime, [], cs);
  const c = {
    credential,
    pair: { signKey: first.privatePackage.signaturePrivateKey, publicKey: first.publicPackage.leafNode.signaturePublicKey },
    packages: [first],
    groups: new Map(),
  };
  clients.set(id, c);
  return c;
}

const need = (id) => {
  const c = clients.get(id);
  if (!c) throw new Error(`no client ${id}`);
  return c;
};

const ops = {
  async new({ id, identity }) {
    const c = await client(id, identity);
    return { signature_key: b64(c.pair.publicKey) };
  },

  async key_package({ id }) {
    const c = need(id);
    const generated = await generateKeyPackageWithKey(c.credential, capabilities(), defaultLifetime, [], c.pair, cs);
    c.packages.push(generated);
    return { key_package: b64(wire.keyPackage(generated.publicPackage)) };
  },

  /// `establish`: create the group (with a policy if asked), add the given packages in one commit
  /// with the ratchet tree in the Welcome.
  async create_group({ id, group_id, policy, key_packages = [] }) {
    const c = need(id);
    const self = await generateKeyPackageWithKey(c.credential, capabilities(), defaultLifetime, [], c.pair, cs);
    let state = await createGroup(te.encode(group_id), self.publicPackage, self.privatePackage, policy ? [policyExtension(policy)] : [], cs);
    let welcome = null;
    if (key_packages.length) {
      const commit = await createCommit({ state, cipherSuite: cs }, {
        extraProposals: key_packages.map((kp) => ({ proposalType: 'add', add: { keyPackage: decodeMessage(kp).keyPackage } })),
        ratchetTreeExtension: true,
      });
      state = commit.newState;
      welcome = commit.welcome ? b64(wire.welcome(commit.welcome)) : null;
    }
    c.groups.set(group_id, state);
    return { welcome, epoch: Number(state.groupContext.epoch) };
  },

  /// Adds members to a running group: the commit for the others and the Welcome for the new.
  async add({ id, group_id, key_packages }) {
    const c = need(id);
    const commit = await createCommit({ state: c.groups.get(group_id), cipherSuite: cs }, {
      extraProposals: key_packages.map((kp) => ({ proposalType: 'add', add: { keyPackage: decodeMessage(kp).keyPackage } })),
      ratchetTreeExtension: true,
    });
    c.groups.set(group_id, commit.newState);
    return {
      commit: b64(encodeMlsMessage(commit.commit)),
      welcome: commit.welcome ? b64(wire.welcome(commit.welcome)) : null,
      epoch: Number(commit.newState.groupContext.epoch),
    };
  },

  /// An empty commit (what `refreshRecoveryEpochs` sends).
  async update({ id, group_id }) {
    const c = need(id);
    const commit = await createCommit({ state: c.groups.get(group_id), cipherSuite: cs }, { extraProposals: [], ratchetTreeExtension: true });
    c.groups.set(group_id, commit.newState);
    return { commit: b64(encodeMlsMessage(commit.commit)), epoch: Number(commit.newState.groupContext.epoch) };
  },

  /// `join`: tries every stashed package against the Welcome.
  async join({ id, welcome }) {
    const c = need(id);
    const message = decodeMessage(welcome);
    let last;
    for (const kp of c.packages) {
      try {
        const state = await joinGroup(message.welcome, kp.publicPackage, kp.privatePackage, emptyPskIndex, cs);
        const group_id = td.decode(state.groupContext.groupId);
        c.groups.set(group_id, state);
        return { group_id, epoch: Number(state.groupContext.epoch), policy: policyIn(state.groupContext.extensions) };
      } catch (e) { last = e; }
    }
    throw last || new Error('no matching key package');
  },

  async send({ id, group_id, text, tag = 0 }) {
    const c = need(id);
    const app = await createApplicationMessage(c.groups.get(group_id), pack(tag, text), cs);
    c.groups.set(group_id, app.newState);
    return { message: b64(encodeMlsMessage(app.privateMessage ? { version: 'mls10', wireformat: 'mls_private_message', privateMessage: app.privateMessage } : app.message)) };
  },

  async process({ id, group_id, message }) {
    const c = need(id);
    const result = await processMessage(decodeMessage(message), c.groups.get(group_id), emptyPskIndex, () => 'accept', cs);
    c.groups.set(group_id, result.newState);
    const epoch = Number(result.newState.groupContext.epoch);
    if (result.kind === 'applicationMessage') {
      const bytes = result.message;
      return { kind: 'application', tag: bytes[0], text: td.decode(bytes.slice(1)), epoch };
    }
    return { kind: 'handshake', epoch, members: members(result.newState) };
  },

  async group_info({ id, group_id }) {
    const c = need(id);
    const info = await createGroupInfoWithExternalPubAndRatchetTree(c.groups.get(group_id), [], cs);
    return { group_info: b64(wire.groupInfo(info)) };
  },

  /// The new-device path: commit ourselves in from a GroupInfo.
  async join_external({ id, group_info }) {
    const c = need(id);
    const self = await generateKeyPackageWithKey(c.credential, capabilities(), defaultLifetime, [], c.pair, cs);
    const joined = await joinGroupExternal(decodeMessage(group_info).groupInfo, self.publicPackage, self.privatePackage, false, cs, undefined, defaultClientConfig);
    const group_id = td.decode(joined.newState.groupContext.groupId);
    c.groups.set(group_id, joined.newState);
    return { group_id, commit: b64(wire.publicMessage(joined.publicMessage)), epoch: Number(joined.newState.groupContext.epoch) };
  },

  async exporter({ id, group_id, label, context, length }) {
    const c = need(id);
    const state = c.groups.get(group_id);
    const key = await mlsExporter(state.keySchedule.exporterSecret, label, te.encode(context), length, cs);
    return { secret: b64(key) };
  },

  async state({ id, group_id }) {
    const state = need(id).groups.get(group_id);
    return { epoch: Number(state.groupContext.epoch), members: members(state), policy: policyIn(state.groupContext.extensions) };
  },
};

function members(state) {
  return (state.ratchetTree || [])
    .filter((n) => n?.nodeType === 'leaf')
    .map((n) => td.decode(n.leaf.credential.identity))
    .sort();
}

const rl = createInterface({ input: process.stdin });
for await (const line of rl) {
  if (!line.trim()) continue;
  let answer;
  try {
    const request = JSON.parse(line);
    const op = ops[request.op];
    if (!op) throw new Error(`unknown op ${request.op}`);
    answer = { ok: true, ...(await op(request)) };
  } catch (e) {
    answer = { ok: false, error: String(e?.stack || e) };
  }
  process.stdout.write(`${JSON.stringify(answer)}\n`);
}
