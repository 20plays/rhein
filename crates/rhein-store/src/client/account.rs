//! The account's security on its home server, as the web client's Account and Security panes
//! offer it: the recovery address, two-factor sign-in with its recovery codes, signing out
//! everywhere, and deactivating or deleting the account. Each sensitive change first proves the
//! password again (an OPAQUE reauthentication, whose token the route takes).

use std::future::Future;

use serde_json::Value;

use super::*;

type Done = Box<dyn FnOnce(&mut Client, Result<Value, String>, &mut Context<Client>)>;

impl Client {
    /// Runs one home-server call off the main thread and hands its answer to `done`.
    fn home_call<F, Fut>(&mut self, call: F, done: impl FnOnce(&mut Self, Result<Value, String>, &mut Context<Self>) + 'static, cx: &mut Context<Self>)
    where
        F: FnOnce(Api) -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<Value>> + Send + 'static,
    {
        let Some(api) = self.home.as_ref().map(|h| h.api.clone()) else { return };
        let io = self.io.clone();
        let done: Done = Box::new(done);
        cx.spawn(async move |this, cx| {
            let result = join(io.spawn(async move { call(api).await })).await;
            this.update(cx, |this, cx| done(this, result.map_err(|e| friendly(&e)), cx)).ok();
        })
        .detach();
    }

    /// `{mfa, recovery}`: the two-factor state and the recovery address.
    pub fn load_security(&mut self, done: impl FnOnce(Result<Value, String>, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        self.home_call(
            |api| async move {
                let mfa = api.mfa_status().await?;
                let recovery = api.recovery_email().await.unwrap_or(Value::Null);
                Ok(serde_json::json!({ "mfa": mfa, "recovery": recovery }))
            },
            move |_, r, cx| done(r, cx),
            cx,
        );
    }

    /// Starts two-factor setup: `{secret, otpauth_uri}` for the authenticator app.
    pub fn mfa_setup(&mut self, password: String, done: impl FnOnce(Result<Value, String>, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        self.home_call(
            move |api| async move {
                let proof = auth::reauth_proof(&api, &password, &SerenityKsf::memory_constrained()).await?;
                Ok(api.mfa_setup(&proof).await?)
            },
            move |_, r, cx| done(r, cx),
            cx,
        );
    }

    /// Turns two-factor sign-in on with the app's first code: `{recovery_codes}`.
    pub fn mfa_enable(&mut self, code: String, done: impl FnOnce(Result<Value, String>, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        self.home_call(move |api| async move { Ok(api.mfa_enable(code.trim()).await?) }, move |_, r, cx| done(r, cx), cx);
    }

    pub fn mfa_disable(&mut self, password: String, code: String, done: impl FnOnce(Result<Value, String>, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        self.home_call(
            move |api| async move {
                let proof = auth::reauth_proof(&api, &password, &SerenityKsf::memory_constrained()).await?;
                Ok(api.mfa_disable(&proof, code.trim()).await?)
            },
            move |_, r, cx| done(r, cx),
            cx,
        );
    }

    /// Sets the recovery address (`None` removes it); a new one waits for its confirmation mail.
    pub fn set_recovery_email(
        &mut self,
        password: String,
        email: Option<String>,
        done: impl FnOnce(Result<Value, String>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.home_call(
            move |api| async move {
                let proof = auth::reauth_proof(&api, &password, &SerenityKsf::memory_constrained()).await?;
                Ok(api.set_recovery_email(&proof, email.as_deref().map(str::trim)).await?)
            },
            move |_, r, cx| done(r, cx),
            cx,
        );
    }

    /// Saves what the home server holds for the account (`GET /account/export`) into
    /// Downloads; with `password`, the signed account bundle instead (`POST /account/bundle`), the
    /// record to keep when a server shuts down. Answers `{path}`.
    pub fn save_account_data(&mut self, password: Option<String>, done: impl FnOnce(Result<Value, String>, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        let local: String = self.user_id().unwrap_or("account").trim_start_matches('@').chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        self.home_call(
            move |api| async move {
                let (bytes, name) = match password {
                    Some(password) => {
                        let proof = auth::reauth_proof(&api, &password, &SerenityKsf::memory_constrained()).await?;
                        let bundle = api.post::<_, Value>("/account/bundle", &proof).await?;
                        (serde_json::to_vec_pretty(&bundle)?, format!("telium-account-bundle-{local}.json"))
                    }
                    None => {
                        // Kept as the server sends it (JSON, or an archive).
                        let (bytes, kind) = api.get_raw("/account/export").await?;
                        let ext = match kind.as_deref().unwrap_or_default() {
                            k if k.contains("zip") => "zip",
                            k if k.contains("json") => "json",
                            _ => "bin",
                        };
                        (bytes, format!("telium-account-export-{local}.{ext}"))
                    }
                };
                let dir = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir);
                let path = dir.join(name);
                crate::persist::write_private(&path, &bytes)?;
                Ok(serde_json::json!({ "path": path.display().to_string() }))
            },
            move |_, r, cx| done(r, cx),
            cx,
        );
    }

    /// Ends every session of the account on every device, then this one here.
    pub fn sign_out_everywhere(&mut self, cx: &mut Context<Self>) {
        self.home_call(
            |api| async move { Ok(api.logout_all().await?) },
            |this, result, cx| match result {
                Ok(_) => this.sign_out(cx),
                Err(e) => {
                    this.error = Some(format!("Couldn't sign out everywhere: {e}").into());
                    cx.notify();
                }
            },
            cx,
        );
    }

    /// Deactivates (can be undone by signing in again) or deletes the account, then signs out.
    pub fn close_account(&mut self, password: String, delete: bool, done: impl FnOnce(Result<Value, String>, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        self.home_call(
            move |api| async move {
                let proof = auth::reauth_proof(&api, &password, &SerenityKsf::memory_constrained()).await?;
                Ok(if delete { api.delete_account(&proof).await? } else { api.deactivate_account(&proof).await? })
            },
            move |this, result, cx| {
                let ok = result.is_ok();
                done(result, cx);
                if ok {
                    this.sign_out(cx);
                }
            },
            cx,
        );
    }
}
