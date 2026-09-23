//! Sign-in against a home server and the assertion → guild-session exchange.

use anyhow::{Context as _, Result, bail};
use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_proto::*;

use crate::api::Api;

pub struct SignedIn {
    pub session: HomeSession,
    /// OPAQUE export key; kept in memory only, it unwraps the recovery vault later.
    pub export_key: Vec<u8>,
}

/// Refuses servers that do not speak OPAQUE rather than falling back to a password that travels.
pub async fn require_opaque(home: &Api) -> Result<Capabilities> {
    let caps = home.capabilities().await.context("capabilities")?;
    if !caps.auth.opaque {
        bail!("{} does not offer OPAQUE sign-in; Rhein will not send a password", caps.server_name);
    }
    Ok(caps)
}

pub async fn login(
    home: &Api,
    user: &str,
    password: &str,
    device: &DeviceKey,
    device_name: &str,
    ksf: &SerenityKsf,
) -> Result<SignedIn> {
    require_opaque(home).await?;
    let start = rhein_auth::login_start(password)?;
    let answer: LoginStartResponse = home
        .post("/login/opaque/start", &LoginStart { user, start_login_request: start.request.clone() })
        .await?;
    if answer.legacy_upgrade_required {
        bail!("this account still uses a legacy password; sign in once with the web client to upgrade it");
    }
    let finished = start.finish(password, &answer.login_response, ksf)?;
    let session: HomeSession = home
        .post(
            "/login/opaque/finish",
            &LoginFinish {
                login_id: &answer.login_id,
                finish_login_request: finished.message,
                device_name,
                device_pubkey: device.public_b64u(),
                mfa_code: None,
            },
        )
        .await?;
    home.set_token(session.access_token.clone()).await;
    Ok(SignedIn { session, export_key: finished.export_key })
}

pub async fn register(
    home: &Api,
    localpart: &str,
    password: &str,
    device: &DeviceKey,
    device_name: &str,
    ksf: &SerenityKsf,
) -> Result<SignedIn> {
    let caps = require_opaque(home).await?;
    if !caps.registration_open {
        bail!("registration is closed on {}", caps.server_name);
    }
    let start = rhein_auth::registration_start(password)?;
    let answer: RegisterStartResponse = home
        .post("/register/opaque/start", &RegisterStart { localpart, registration_request: start.request.clone() })
        .await?;
    let finished = start.finish(password, &answer.registration_response, ksf)?;
    let session: HomeSession = home
        .post(
            "/register/opaque/finish",
            &RegisterFinish {
                localpart,
                registration_record: finished.message,
                device_name,
                device_pubkey: device.public_b64u(),
            },
        )
        .await?;
    home.set_token(session.access_token.clone()).await;
    Ok(SignedIn { session, export_key: finished.export_key })
}

/// Trades a short-lived, audience-bound assertion from the home server for a session on the guild
/// server named `audience`. After this the home server is off the guild's hot path.
pub async fn guild_session(home: &Api, guild: &Api, audience: &str) -> Result<GuildSession> {
    let assertion = home.assertion(audience).await.context("identity assertion")?;
    let session = guild.create_session(&assertion.assertion).await.context("guild session")?;
    guild.set_token(session.session_token.clone()).await;
    Ok(session)
}
