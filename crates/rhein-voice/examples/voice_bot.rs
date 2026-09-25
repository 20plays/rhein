//! A participant for trying calls by hand: joins a guild's first voice channel through the SFU
//! and "speaks" a tone, one second on and one off, until stopped.
//!
//!   cargo run -p rhein-voice --example voice_bot -- localhost:8448 @ben…:localhost 'rhein demo password'
//!
//! Signs in as that account (a new device each time) and uses the guild on its home server.
//! Needs a server with an SFU region (`scripts/dev-sfu.sh`, `RHEIN_SFU=1 scripts/dev-server.sh
//! a`); a mesh would need a gateway for its signals.

use std::time::Duration;

use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_net::{Api, auth, discover};
use rhein_proto::VoiceServer;
use rhein_voice::audio::AudioEngine;
use rhein_voice::sfu::{self, SfuCall};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(server), Some(user), Some(password)) = (args.next(), args.next(), args.next()) else {
        anyhow::bail!("usage: voice_bot <server> <user id> <password>");
    };
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await?;
    let home = Api::new(http.clone(), &doc.api, None);
    auth::login(&home, &user, &password, &DeviceKey::generate(), "rhein voice bot", &SerenityKsf::memory_constrained()).await?;
    let api = Api::new(http.clone(), &doc.api, None);
    auth::guild_session(&home, &api, &doc.server_name).await?;
    let guild = api.guilds().await?.into_iter().next().ok_or_else(|| anyhow::anyhow!("in no guild"))?;
    let channel = api.channels(&guild.id).await?.into_iter().find(|c| c.is_voice()).ok_or_else(|| anyhow::anyhow!("no voice channel"))?;
    let joined = api.voice_join(&channel.id, false, false, false, None).await?;
    let VoiceServer::Sfu { endpoint, token, .. } = joined.voice_server else {
        api.voice_leave(&channel.id).await.ok();
        anyhow::bail!("the server answered with a mesh; the bot needs an SFU region");
    };
    let engine = AudioEngine::headless()?;
    let session = sfu::session(&http, &endpoint, &token).await?;
    let (call, mut events) = SfuCall::connect(session, engine.microphone(), None).await?;
    println!("in #{} of {}; Ctrl-C to leave", channel.name, guild.name);
    tokio::spawn(async move { while events.recv().await.is_some() {} });

    let mut ticker = tokio::time::interval(Duration::from_millis(10));
    let mut n: u64 = 0;
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let on = (n / 100).is_multiple_of(2);
                let frame: Vec<i16> = (0..480u64)
                    .map(|i| if on { ((((n * 480 + i) as f32) * 2. * std::f32::consts::PI * 330. / 48_000.).sin() * 6000.) as i16 } else { 0 })
                    .collect();
                engine.feed(&frame);
                n += 1;
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    call.close().await;
    api.voice_leave(&channel.id).await.ok();
    Ok(())
}
