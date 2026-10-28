//! Seeds a small, lively guild for screenshots: registers six accounts with names, pictures and
//! accents on a server, creates the guild "Riverside" with a few channels and a conversation in
//! #general, and saves the first account (Anna) as a sign-in would, in `RHEIN_CONFIG_DIR`.
//!
//!   RHEIN_CONFIG_DIR=.dev/config-shots cargo run -p rhein-store --example demo_seed -- staging.telium.chat
//!
//! The accounts get a random password, printed at the end; nothing else needs it.

use std::time::Duration;

use image::{Rgb, RgbImage};
use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_net::{Api, auth, discover};
use rhein_proto::{Attachment, SendMessage};
use rhein_store::persist;

struct Person {
    localpart: &'static str,
    name: &'static str,
    bio: &'static str,
    /// The two ends of the picture's gradient; the first is also the accent.
    colors: [u32; 2],
}

const PEOPLE: [Person; 6] = [
    Person { localpart: "anna", name: "Anna", bio: "Keeps the river tidy.", colors: [0xe0558c, 0xf3a66b] },
    Person { localpart: "ben", name: "Ben", bio: "Asks the big questions.", colors: [0x3e8ff0, 0x33c2c2] },
    Person { localpart: "cara", name: "Cara", bio: "Shaders, synths and sourdough.", colors: [0x8b5cf6, 0xec4899] },
    Person { localpart: "dario", name: "Dario", bio: "Ships on Fridays.", colors: [0x16a34a, 0xa3e635] },
    Person { localpart: "elif", name: "Elif", bio: "Packaging and paperwork.", colors: [0xf59e0b, 0xef4444] },
    Person { localpart: "finn", name: "Finn", bio: "Mostly here for the memes.", colors: [0x0ea5e9, 0x6366f1] },
];

struct Member {
    home: Api,
    guild: Api,
    signed: auth::SignedIn,
    key: DeviceKey,
}

/// Registers, waiting out the server's registration rate limit (429) instead of failing.
async fn register(home: &Api, localpart: &str, password: &str, key: &DeviceKey) -> auth::SignedIn {
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    for _ in 0..90 {
        match auth::register(home, localpart, password, key, "Rhein demo seed", &ksf).await {
            Ok(signed) => return signed,
            Err(err) if err.downcast_ref::<rhein_net::ApiError>().and_then(|e| e.status()) == Some(429) => {
                eprintln!("register {localpart}: {err:#}");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            Err(err) => panic!("register {localpart}: {err:#}"),
        }
    }
    panic!("register {localpart}: still rate limited")
}

fn rgb(c: u32) -> [f32; 3] {
    [(c >> 16 & 0xff) as f32, (c >> 8 & 0xff) as f32, (c & 0xff) as f32]
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> Rgb<u8> {
    let t = t.clamp(0.0, 1.0);
    Rgb([0, 1, 2].map(|i| (a[i] + (b[i] - a[i]) * t) as u8))
}

fn png(img: &RgbImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).expect("encode png");
    out.into_inner()
}

/// A diagonal gradient with a soft light in one corner: a picture without a face.
fn avatar([from, to]: [u32; 2]) -> Vec<u8> {
    let (a, b) = (rgb(from), rgb(to));
    let n = 256;
    png(&RgbImage::from_fn(n, n, |x, y| {
        let (fx, fy) = (x as f32 / n as f32, y as f32 / n as f32);
        let Rgb(base) = mix(a, b, (fx + fy) / 2.0);
        let glow = (1.0 - ((fx - 0.3).powi(2) + (fy - 0.25).powi(2)).sqrt() * 2.2).max(0.0) * 0.35;
        mix(base.map(f32::from), [255.0; 3], glow)
    }))
}

/// An evening over a river: sky, sun, three ridges and the water mirroring the sky.
fn landscape(width: u32, height: u32) -> Vec<u8> {
    let horizon = height as f32 * 0.62;
    let (top, low) = (rgb(0x2b3a67), rgb(0xf4a261));
    let ridges = [(0.40, 0.08, rgb(0x6d597a), 3.1), (0.50, 0.06, rgb(0x4a3f63), 5.3), (0.58, 0.04, rgb(0x2e2a45), 8.7)];
    png(&RgbImage::from_fn(width, height, |x, y| {
        let (fx, fy) = (x as f32 / width as f32, y as f32);
        if fy > horizon {
            // Water: the sky upside down, darker, with a band of sunlight.
            let depth = (fy - horizon) / (height as f32 - horizon);
            let Rgb(sky) = mix(low, top, depth);
            let shimmer = ((fy * 0.9).sin() * 0.5 + 0.5) * (1.0 - ((fx - 0.68).abs() * 8.0).min(1.0)) * 0.5;
            return mix(sky.map(|c| f32::from(c) * 0.7), rgb(0xffd6a0), shimmer);
        }
        for &(base, amp, color, freq) in ridges.iter().rev() {
            let ridge = height as f32 * (base + amp * ((fx * freq).sin() + 0.5 * (fx * freq * 2.3 + 1.0).sin()));
            if fy > ridge {
                return mix(color, low, 0.12);
            }
        }
        let sun = ((fx - 0.68).powi(2) * 4.0 + ((fy / height as f32) - 0.42).powi(2) * 9.0).sqrt();
        let Rgb(sky) = mix(top, low, fy / horizon);
        mix(sky.map(f32::from), rgb(0xfff1c1), (1.0 - sun * 4.0).max(0.0) + (0.25 - sun).max(0.0))
    }))
}

#[tokio::main]
async fn main() {
    let server = std::env::args().nth(1).expect("the server, e.g. staging.telium.chat");
    assert!(std::env::var("RHEIN_CONFIG_DIR").is_ok(), "set RHEIN_CONFIG_DIR to where the app should find the account");
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let tag = format!("{:x}", std::process::id() & 0xfff);
    // Random bytes from a throwaway key make a password nobody else knows.
    let password = rhein_auth::b64u(&DeviceKey::generate().seed());

    let mut members = Vec::new();
    for person in &PEOPLE {
        let home = Api::new(http.clone(), &doc.api, None);
        let key = DeviceKey::generate();
        let signed = register(&home, &format!("{}{tag}", person.localpart), &password, &key).await;
        let media = home.upload_profile_media("avatar.png", "image/png", avatar(person.colors)).await.expect("avatar");
        let avatar_ref = format!("{}?cap={}", media.hash, media.capability.unwrap_or_default());
        home.patch_profile(&serde_json::json!({
            "display_name": person.name,
            "avatar_ref": avatar_ref,
            "accent_color": person.colors[0],
            "bio": person.bio,
        }))
        .await
        .expect("profile");
        // The guild session comes after the profile, so the guild learns the name and picture.
        let guild = Api::new(http.clone(), &doc.api, None);
        auth::guild_session(&home, &guild, &doc.server_name).await.expect("guild session");
        eprintln!("registered {} as {}", signed.session.user_id, person.name);
        members.push(Member { home, guild, signed, key });
    }

    let owner = &members[0];
    let guild = owner.guild.create_guild("Riverside").await.expect("create guild");
    let gid = guild.id.clone();
    let icon = owner.guild.upload_guild_media(&gid, &doc.server_name, "icon.png", "image/png", avatar([0x0f766e, 0x38bdf8])).await.expect("icon");
    owner.guild.patch_guild(&gid, &serde_json::json!({ "icon_ref": format!("{}?cap={}", icon.hash, icon.capability.unwrap_or_default()) })).await.expect("guild icon");

    let general = owner.guild.channels(&gid).await.expect("channels").into_iter().find(|c| c.is_textual()).expect("a text channel");
    let community = owner.guild.create_channel(&gid, "Community", "category", None).await.expect("category");
    for name in ["announcements", "showcase", "help", "off-topic"] {
        owner.guild.create_channel(&gid, name, "text", Some(&community.id)).await.expect("channel");
    }
    let voice = owner.guild.create_channel(&gid, "Voice", "category", None).await.expect("category");
    for name in ["Lounge", "Pair programming"] {
        owner.guild.create_channel(&gid, name, "voice", Some(&voice.id)).await.expect("channel");
    }
    let invite = owner.guild.create_invite(&gid, Some(&general.id)).await.expect("invite");
    let code = invite["code"].as_str().expect("invite code").to_owned();
    for m in &members[1..] {
        m.guild.join_invite(&code).await.expect("join");
    }

    let (anna, ben, cara, dario, elif, finn) = (0, 1, 2, 3, 4, 5);
    let mention = |i: usize| format!("<{}>", members[i].signed.session.user_id);
    let say = async |who: usize, content: String, reply_to: Option<&str>, attachments: Vec<Attachment>| {
        let body = SendMessage { content, attachments, reply_to: reply_to.map(str::to_owned), nonce: None };
        let sent = members[who].guild.send_message(&general.id, &body).await.expect("send");
        tokio::time::sleep(Duration::from_millis(400)).await;
        sent.id
    };
    let react = async |who: usize, message: &str, emoji: &str| {
        members[who].guild.react(message, emoji, true).await.expect("react");
    };

    let welcome = say(anna, "Morning everyone ☀️ The new build is up, grab it from the releases page.".into(), None, vec![]).await;
    react(ben, &welcome, "🎉").await;
    react(dario, &welcome, "🎉").await;
    react(finn, &welcome, "🙌").await;
    say(ben, "Installed it on my laptop last night. Startup is basically instant now.".into(), None, vec![]).await;
    let scroll = say(cara, "Same here. Scrolling back through #showcase doesn't stutter anymore, even with all the GIFs.".into(), None, vec![]).await;
    react(anna, &scroll, "❤️").await;
    say(dario, format!("{} did you end up fixing the thing where videos went black after switching channels?", mention(anna)), None, vec![]).await;
    let fixed = say(anna, "Yep, that's in. It was a cached view forgetting what its children had read.".into(), None, vec![]).await;
    react(dario, &fixed, "🔥").await;
    say(elif, "I'm putting together the Flatpak manifest this week. If anyone runs GNOME on Wayland, I'd love a tester.".into(), None, vec![]).await;
    let tester = say(finn, "Me! Fedora 41, GNOME, two monitors with different scaling. Good luck 😄".into(), None, vec![]).await;
    react(elif, &tester, "🫡").await;
    say(cara, "For anyone building from source, this installs it just for you, no sudo:\n```sh\n./install --prefix ~/.local\n```".into(), None, vec![]).await;
    say(ben, "Also, did someone sneak in a light theme?".into(), None, vec![]).await;
    let light = say(cara, "Guilty. Settings → Appearance.".into(), None, vec![]).await;
    react(ben, &light, "👀").await;
    let photo = members[dario].home.upload_attachment("riverside.png", "image/png", landscape(960, 540), &general.id, &gid).await.expect("upload");
    let evening = say(dario, "Unrelated, but the view from the bridge on my way home today:".into(), None, vec![photo]).await;
    for (who, emoji) in [(anna, "😍"), (cara, "😍"), (elif, "😍"), (finn, "🌅")] {
        react(who, &evening, emoji).await;
    }
    say(finn, "Okay that's a wallpaper now.".into(), Some(&evening), vec![]).await;
    say(anna, format!("Voice hangout in the Lounge tonight at 8 for anyone who wants to test calls before the release. {} you in?", mention(ben)), None, vec![]).await;
    say(ben, "Wouldn't miss it 🎧".into(), None, vec![]).await;

    persist::store(&persist::Saved {
        home: server.clone(),
        user_id: members[anna].signed.session.user_id.clone(),
        device_id: members[anna].signed.session.device_id.clone(),
        access_token: members[anna].signed.session.access_token.clone(),
        expires_at: members[anna].signed.session.expires_at,
        device_seed: rhein_auth::b64u(&members[anna].key.seed()),
        display_name: Some("Anna".into()),
        ..Default::default()
    })
    .expect("save the session");
    persist::set_active(&members[anna].signed.session.user_id).expect("make it the active account");
    eprintln!("seeded guild {gid} (invite {}/{code}) in {}", doc.server_name, std::env::var("RHEIN_CONFIG_DIR").unwrap());
    eprintln!("password of all six accounts: {password}");
}
