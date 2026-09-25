// Seals or opens a call hello with the web client's own callmode.js (unmodified, from the clone),
// for crates/rhein-voice/tests/hello_interop.rs.
//
//   bun hello.mjs seal <key hex> <conversation> <sender> <posture> <counter> <epoch>
//   bun hello.mjs open <key hex> <conversation> <sender> <epoch> <sealed json>
import { sealHello, openHello } from '../../.dev/telium/web/src/lib/callmode.js';

const [op, keyHex, conversationId, sender, ...rest] = process.argv.slice(2);
const key = Uint8Array.from(keyHex.match(/../g).map((b) => parseInt(b, 16)));
if (op === 'seal') {
  const [posture, counter, epoch] = rest;
  const sealed = await sealHello(key, { conversationId, sender, posture, counter: Number(counter), epoch: Number(epoch) });
  console.log(JSON.stringify(sealed));
} else {
  const [epoch, sealed] = rest;
  const opened = await openHello(key, { conversationId, sender, epoch: Number(epoch) }, JSON.parse(sealed));
  console.log(JSON.stringify(opened));
}
