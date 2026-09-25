// Seals or opens a sync store value with the web client's own sealed-sync.js (unmodified, from
// the clone), for crates/rhein-mls/tests/sealed_sync_interop.rs.
//
//   bun sealed-sync.mjs seal <ring hex> <key name> <doc json>
//   bun sealed-sync.mjs open <ring hex> <key name> <envelope json>
import { open, seal, syncKeys } from '../../.dev/telium/web/src/lib/sealed-sync.js';

const [op, ringHex, name, json] = process.argv.slice(2);
const raw = Uint8Array.from(ringHex.match(/../g).map((b) => parseInt(b, 16)));
const keys = await syncKeys({ raw, retired: [] });
if (op === 'seal') {
  console.log(JSON.stringify(await seal(keys, name, JSON.parse(json))));
} else {
  const { doc } = await open(keys, name, JSON.parse(json));
  console.log(JSON.stringify(doc));
}
