# openmls 0.9.0, patched for Telium

A copy of openmls 0.9.0 from crates.io. The one functional change is in `src/extensions/codec.rs`:

The GroupInfo `external_pub` extension (RFC 9420 §12.4.3.2) is written as the bare HPKE public
key, without the `opaque<V>` length prefix the RFC specifies, and is read in either form.

**Why:** the Telium web client uses ts-mls 1.6.4, which writes and reads the key bare. Its
GroupInfos are signed over that form, so openmls could neither parse them nor produce ones the web
client can use for an external commit (the new-device path of encrypted DMs). Everything Rhein
talks to is Telium, so Rhein follows the deployed format.

Also:

- tests, benches and examples of the upstream crate are left out;
- `src/lib.rs` allows warnings, as the crate would build from crates.io (where lints are capped).
