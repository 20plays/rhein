//! DM keys and trust against a running dev server (`RHEIN_LIVE=localhost:8448`), in one test
//! because the secrets location comes from a process-wide environment variable.

use std::time::Duration;

use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_mls::trust::{self, IdentityKey};
use rhein_mls::vault::{self, Keyring};
use rhein_net::{Api, auth, discover};
use rhein_store::dm::{Secrets, bring_up};

async fn register(home: &Api, localpart: &str) -> auth::SignedIn {
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    for _ in 0..20 {
        match auth::register(home, localpart, "correct horse battery", &DeviceKey::generate(), "rhein dm test", &ksf).await {
            Ok(signed) => return signed,
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    panic!("could not register {localpart}");
}

#[tokio::test]
async fn vault_cross_signing_and_continuity() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("rhein-dm-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("RHEIN_CONFIG_DIR", &dir) };

    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let home = Api::new(http.clone(), &doc.api, None);
    let signed = register(&home, &format!("dm{}", std::process::id())).await;
    let (user, device) = (signed.session.user_id.clone(), signed.session.device_id.clone());
    let wrapper = vault::recovery_wrapper(&signed.export_key);

    // A fresh account: Rhein creates the opaque-custody vault, and there is no identity yet.
    let first = bring_up(&home, &user, &device, Some(wrapper)).await.expect("first bring-up");
    assert_eq!(first.vault_mode, "opaque_custody");
    assert!(first.locked.is_none());
    assert!(first.identity_key.is_none() && !first.cross_signed);
    let ring = Secrets::load(&user).keyring.expect("the keyring is kept on this device");
    let stored = home.vault().await.unwrap();
    assert_eq!(vault::open_vault(&stored, Some(&wrapper), None).unwrap_as_ring(), ring);

    // The web client then sets up an identity (as its Trust.start does on first use): a
    // cross-signing key whose secret is sealed under R.
    let identity = IdentityKey::from_secret(&vault::rand_bytes::<32>()).unwrap();
    let body = serde_json::json!({
        "identity_key": identity.public_b64(),
        "sealed_secret": trust::seal_cross_signing_secret(&identity, &ring),
    });
    let _: serde_json::Value = home.put("/crosssigning", &body).await.expect("publish identity");

    // A later start without the password (a restored session): R from this device opens the
    // secret, the identity is anchored and this device is vouched for.
    let again = bring_up(&home, &user, &device, None).await.expect("second bring-up");
    assert!(again.cross_signed, "{again:?}");
    assert!(again.identity_problem.is_none(), "{again:?}");
    let tid = again.immutable_id.clone().expect("anchored");
    assert_eq!(tid, trust::immutable_id(&identity.public_b64()).unwrap());

    // What a web peer checks: the published device list vouches for our MLS key.
    let list = home.device_list(&user).await.unwrap();
    let ours = list["devices"].as_array().unwrap().iter().find(|d| d["device_id"] == device.as_str()).expect("our device");
    assert_eq!(ours["mls_key"], again.mls_key.as_str());
    assert!(trust::device_is_vouched(&user, list["identity_key"].as_str().unwrap(), ours));
    let published = home.identity_document(&user).await.unwrap();
    let continuity = trust::verify_identity_document(&published, Some(&user)).unwrap().unwrap();
    assert_eq!(continuity.current_key, identity.public_b64());

    // A third start changes nothing and stays verified.
    let third = bring_up(&home, &user, &device, None).await.expect("third bring-up");
    assert_eq!(third, again);
    let _ = std::fs::remove_dir_all(&dir);
}

trait RingOf {
    fn unwrap_as_ring(self) -> Keyring;
}

impl RingOf for anyhow::Result<vault::Opened> {
    fn unwrap_as_ring(self) -> Keyring {
        match self.unwrap() {
            vault::Opened::Ring { keyring, .. } => keyring,
            other => panic!("expected the keyring, got {other:?}"),
        }
    }
}
