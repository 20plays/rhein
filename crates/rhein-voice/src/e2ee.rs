//! End-to-end encrypted media for DM calls, as the web client's `MlsKeyProvider` does it: one
//! key shared by every participant, the MLS exporter secret of the conversation's current
//! epoch, in key slot `epoch % 16`, never ratcheted.
//!
//! livekit-client derives the AES-GCM key from that secret with HKDF-SHA256 (salt
//! `LKFrameEncryptionKey`, 128 zero bytes of info, 128 bits). The native frame cryptor does the
//! same when asked for HKDF; its default is PBKDF2, which would not interoperate.

use livekit::e2ee::key_provider::{KeyDerivationAlgorithm, KeyProvider, KeyProviderOptions};
use livekit::RoomOptions;
use livekit::e2ee::{E2eeOptions, EncryptionType};

/// The exporter label of the call media key; the context is the conversation id, 32 bytes.
pub const MEDIA_EXPORTER_LABEL: &str = "telium call media v1";
/// The exporter label of the posture hello key.
pub const SIGNAL_EXPORTER_LABEL: &str = "telium call signal v1";
const KEY_SLOTS: u64 = 16;

/// The key slot of an MLS epoch.
pub fn key_slot(epoch: u64) -> i32 {
    (epoch % KEY_SLOTS) as i32
}

fn options() -> KeyProviderOptions {
    KeyProviderOptions {
        ratchet_window_size: 0,
        ratchet_salt: b"LKFrameEncryptionKey".to_vec(),
        failure_tolerance: -1,
        key_ring_size: KEY_SLOTS as i32,
        key_derivation_algorithm: KeyDerivationAlgorithm::HKDF,
    }
}

/// A key provider holding `secret` as the key of `epoch`; later epochs go in with
/// [`set_epoch_key`].
pub fn key_provider(secret: &[u8], epoch: u64) -> KeyProvider {
    // The constructor fills slot 0; the epoch's own slot is what gets used.
    let provider = KeyProvider::with_shared_key(options(), secret.to_vec());
    set_epoch_key(&provider, secret, epoch);
    provider
}

/// Installs the key of a new epoch and makes it the one used for sending.
pub fn set_epoch_key(provider: &KeyProvider, secret: &[u8], epoch: u64) {
    provider.set_shared_key(secret.to_vec(), key_slot(epoch));
}

/// Makes `options` an encrypted call. Only media is encrypted, like the web client's `e2ee`
/// room option: the `encryption` option that replaces it would also encrypt data messages, which
/// the web client could not read.
#[allow(deprecated)]
pub fn encrypt(options: &mut RoomOptions, provider: KeyProvider) {
    options.e2ee = Some(E2eeOptions { encryption_type: EncryptionType::Gcm, key_provider: provider });
}
