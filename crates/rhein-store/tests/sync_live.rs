//! The sealed sync store against a running dev server (`RHEIN_LIVE`): GIF favourites written
//! sealed, read back on a second device of the account (which derives the same key from the
//! recovery keyring), and nothing readable on the server.

use std::time::Duration;

use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_mls::vault;
use rhein_net::{Api, auth, discover};
use rhein_store::dm::{Secrets, bring_up};
use rhein_store::sync_store::{GIF_FAVORITES, Loaded, SyncStore};
use serde_json::json;

#[tokio::test]
async fn gif_favourites_are_sealed_and_follow_the_account() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("rhein-sync-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("RHEIN_CONFIG_DIR", &dir) };
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let home = Api::new(http.clone(), &doc.api, None);
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    let localpart = format!("sy{}", std::process::id());
    let mut signed = None;
    for _ in 0..20 {
        match auth::register(&home, &localpart, "correct horse battery", &DeviceKey::generate(), "rhein sync test", &ksf).await {
            Ok(s) => {
                signed = Some(s);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    let signed = signed.expect("registered");
    let user = signed.session.user_id.clone();
    bring_up(&home, &user, &signed.session.device_id, Some(vault::recovery_wrapper(&signed.export_key))).await.unwrap();
    let ring = Secrets::load(&user).keyring.expect("the recovery keyring");

    let favorites = json!(["https://media.example/cat.gif", "https://media.example/dog.gif"]);
    let mut store = SyncStore::new(&user, &ring);
    assert!(matches!(store.load(&home, GIF_FAVORITES).await.unwrap(), Loaded::Missing));
    store.save(&home, GIF_FAVORITES, &favorites).await.unwrap();

    // The server holds an envelope, not the links.
    let raw = home.sync_all().await.unwrap();
    let stored = raw[GIF_FAVORITES]["value"].to_string();
    assert!(stored.contains("\"sealed\":\"v1\"") && !stored.contains("cat.gif"), "{stored}");

    // A second device of the account (fresh marks) opens it with the same keyring.
    let _ = std::fs::remove_file(dir.join("dm").join(format!("{}.sync.json", user.replace(['@', ':'], "_"))));
    let home2 = Api::new(http.clone(), &doc.api, None);
    auth::login(&home2, &localpart, "correct horse battery", &DeviceKey::generate(), "rhein sync test 2", &ksf).await.unwrap();
    let mut other = SyncStore::new(&user, &ring);
    match other.load(&home2, GIF_FAVORITES).await.unwrap() {
        Loaded::Value(v) => assert_eq!(v, favorites),
        _ => panic!("the favourites should open on the second device"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
