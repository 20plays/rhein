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
    login_with_code(home, user, password, device, device_name, ksf, None).await
}

/// Signs in, with the code from an authenticator app for an account with two-factor sign-in (the
/// server answers `mfa_required` without one).
pub async fn login_with_code(
    home: &Api,
    user: &str,
    password: &str,
    device: &DeviceKey,
    device_name: &str,
    ksf: &SerenityKsf,
    mfa_code: Option<&str>,
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
                mfa_code,
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

/// Proves the password again inside a signed-in session (no new device session), and hands back
/// the OPAQUE export key: how a restored session unlocks the encrypted-DM vault.
pub async fn reauthenticate(home: &Api, password: &str, ksf: &SerenityKsf) -> Result<Vec<u8>> {
    Ok(reauth(home, password, ksf).await?.0)
}

/// Proves the password again for a sensitive change (two-factor, the recovery address, deleting
/// the account): the `{reauth_token}` those routes take.
pub async fn reauth_proof(home: &Api, password: &str, ksf: &SerenityKsf) -> Result<serde_json::Value> {
    let token = reauth(home, password, ksf).await?.1.context("the server issued no reauthentication token")?;
    Ok(serde_json::json!({ "reauth_token": token }))
}

async fn reauth(home: &Api, password: &str, ksf: &SerenityKsf) -> Result<(Vec<u8>, Option<String>)> {
    let start = rhein_auth::login_start(password)?;
    let answer: serde_json::Value = home
        .post("/auth/opaque/reauth/start", &serde_json::json!({ "start_login_request": start.request.clone() }))
        .await
        .context("reauthentication")?;
    let login_response = answer["login_response"].as_str().context("no login response")?;
    let finished = start.finish(password, login_response, ksf).context("That password was not accepted.")?;
    let proof: serde_json::Value = home
        .post(
            "/auth/opaque/reauth/finish",
            &serde_json::json!({ "login_id": answer["login_id"], "finish_login_request": finished.message }),
        )
        .await
        .context("That password was not accepted.")?;
    Ok((finished.export_key, proof["reauth_token"].as_str().map(str::to_owned)))
}

/// Trades a short-lived, audience-bound assertion from the home server for a session on the guild
/// server named `audience`. After this the home server is off the guild's hot path.
pub async fn guild_session(home: &Api, guild: &Api, audience: &str) -> Result<GuildSession> {
    let assertion = home.assertion(audience).await.context("identity assertion")?;
    let session = guild.create_session(&assertion.assertion).await.context("guild session")?;
    guild.set_token(session.session_token.clone()).await;
    guild.set_expires_at(session.expires_at);
    // From here on it renews itself when it runs out, as the web client's sessions do.
    guild.renew_from(home, audience);
    Ok(session)
}
