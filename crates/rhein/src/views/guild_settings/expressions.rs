//! Emoji and stickers: the guild's pack, added from a file and removed.

use base64::Engine as _;
use gpui::{AnyElement, Context, IntoElement, ParentElement as _, PathPromptOptions, SharedString, Styled as _, div, img, prelude::*, px};
use rhein_proto::Expressions;
use rhein_ui::theme::{radius, text};
use rhein_ui::{Button, Icon, IconButton, Theme};

use super::lists::Load;
use super::{GuildSettingsView, heading, hint};

/// The server's ceiling for one emoji or sticker, as the web client checks it first.
const MAX_BYTES: usize = 512 * 1024;

pub(crate) struct State {
    pack: Load<Expressions>,
}

impl State {
    pub(crate) fn new() -> Self {
        Self { pack: Load::Loading }
    }
}

impl GuildSettingsView {
    pub(crate) fn load_expressions(&mut self, cx: &mut Context<Self>) {
        self.call(cx, |api, gid| async move { api.expression_pack(&gid).await }, |v, r, _| {
            v.expressions.pack = match r {
                Ok(p) => Load::Ready(p),
                Err(e) => Load::Failed(e),
            }
        });
    }

    /// Picks a picture and adds it as an emoji or a sticker, named after the file.
    fn add_expression(&mut self, sticker: bool, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(if sticker { "Choose a sticker".into() } else { "Choose an emoji".into() }),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let bytes = std::fs::read(&path);
            this.update(cx, |v, cx| match bytes {
                Ok(bytes) => v.upload_expression(sticker, &path, bytes, cx),
                Err(e) => v.report(false, format!("Couldn't read that file: {e}"), cx),
            })
            .ok();
        })
        .detach();
    }

    fn upload_expression(&mut self, sticker: bool, path: &std::path::Path, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.len() > MAX_BYTES {
            self.report(false, format!("That file is {} KB; at most {} KB fit.", bytes.len() / 1024, MAX_BYTES / 1024), cx);
            return;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        let mime = match ext.as_str() {
            "gif" => "image/gif",
            "png" => "image/png",
            "webp" => "image/webp",
            "jpg" | "jpeg" => "image/jpeg",
            _ => {
                self.report(false, "Emoji and stickers are PNG, GIF, WebP or JPEG pictures.", cx);
                return;
            }
        };
        // Emoji names are letters, digits and underscores, as `:name:` needs them.
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("emoji");
        let name: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect::<String>().trim_matches('_').chars().take(32).collect();
        let name = if name.len() < 2 { format!("emoji_{name}") } else { name };
        let animated = mime == "image/gif";
        let data = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes);
        let mut body = serde_json::json!({ "name": name, "data": data, "mime": mime, "animated": animated });
        if sticker {
            body["format"] = if animated { "gif" } else { "png" }.into();
        }
        self.call(
            cx,
            move |api, gid| async move { if sticker { api.create_sticker(&gid, &body).await } else { api.create_emoji(&gid, &body).await } },
            move |v, r, cx| {
                v.outcome(&r, if sticker { "Sticker added." } else { "Emoji added." }, cx);
                v.load_expressions(cx);
            },
        );
    }

    fn delete_expression(&mut self, id: String, cx: &mut Context<Self>) {
        self.call(
            cx,
            move |api, gid| async move { api.delete_emoji(&gid, &id).await },
            |v, r, cx| {
                v.outcome(&r, "Removed.", cx);
                v.load_expressions(cx);
            },
        );
    }

    pub(crate) fn expressions_page(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let actions = div()
            .flex()
            .gap_2()
            .child(Button::new("emoji-add", "Upload emoji").primary().icon(Icon::Smile).on_click(cx.listener(|v, _, _, cx| v.add_expression(false, cx))))
            .child(Button::new("sticker-add", "Upload sticker").icon(Icon::Image).on_click(cx.listener(|v, _, _, cx| v.add_expression(true, cx))));
        let page = div().flex().flex_col().gap_4().child(hint("Named after the file. PNG, GIF (animated), WebP or JPEG, up to 512 KB.", t)).child(actions);
        let pack = match &self.expressions.pack {
            Load::Loading => return page.child(hint("Loading…", t)).into_any_element(),
            Load::Failed(e) => return page.child(hint(format!("Couldn't load the emoji: {e}"), t)).into_any_element(),
            Load::Ready(p) => p.clone(),
        };
        let base = self.client.read(cx).guild_api_base(&self.guild_id);
        let emoji_urls: Vec<Option<String>> = {
            let client = self.client.read(cx);
            pack.emojis.iter().map(|e| client.emoji_url(Some(&self.guild_id), &e.id, None)).collect()
        };
        let tile = |id: &str, name: &str, url: Option<String>, size: f32, deletable: bool, cx: &mut Context<Self>| {
            let key = id.to_owned();
            div()
                .id(SharedString::from(format!("expr-{id}")))
                .group("expr")
                .relative()
                .w(px(size + 40.))
                .p_2()
                .rounded(radius::MD)
                .border_1()
                .border_color(t.border)
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .child(div().size(px(size)).flex().items_center().justify_center().children(url.map(|u| img(u).size_full().object_fit(gpui::ObjectFit::Contain))))
                .child(div().w_full().truncate().text_center().text_size(text::XS).text_color(t.text_muted).child(format!(":{name}:")))
                .when(deletable, |d| {
                    d.child(
                        div().absolute().top_1().right_1().invisible().group_hover("expr", |s| s.visible()).child(
                            IconButton::new(SharedString::from(format!("expr-del-{key}")), Icon::Trash)
                                .tooltip("Remove")
                                .danger(true)
                                .on_click(cx.listener(move |v, _, _, cx| v.delete_expression(key.clone(), cx))),
                        ),
                    )
                })
                .into_any_element()
        };
        let mut emojis = div().flex().flex_wrap().gap_2();
        for (e, url) in pack.emojis.iter().zip(emoji_urls) {
            emojis = emojis.child(tile(&e.id, &e.name, url, 40., true, cx));
        }
        let mut stickers = div().flex().flex_wrap().gap_2();
        for s in &pack.stickers {
            let url = base.as_ref().map(|b| format!("{b}/_telium/v1/sticker/{}", s.id));
            stickers = stickers.child(tile(&s.id, &s.name, url, 72., false, cx));
        }
        page.child(heading("emoji", t))
            .child(if pack.emojis.is_empty() { hint("No emoji yet.", t).into_any_element() } else { emojis.into_any_element() })
            .child(heading("stickers", t))
            .child(if pack.stickers.is_empty() { hint("No stickers yet.", t).into_any_element() } else { stickers.into_any_element() })
            .into_any_element()
    }
}
