mod http;
mod views;

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, actions, px, size,
};
use rhein_store::{Client, Io};

actions!(rhein, [Quit]);

fn main() {
    // A release says only what went wrong; RUST_LOG still asks for more.
    let level = if cfg!(debug_assertions) { "info" } else { "warn" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level)).init();

    let io = match Io::new() {
        Ok(io) => io,
        Err(err) => {
            eprintln!("rhein: cannot start runtime: {err:#}");
            std::process::exit(1);
        }
    };

    gpui_platform::application()
        .with_assets(rhein_ui::Assets)
        .with_http_client(Arc::new(http::Client::new(io.handle())))
        .run(move |cx: &mut App| {
        rhein_ui::init(cx);
        cx.bind_keys(views::chat::bindings());
        cx.bind_keys(views::shell::bindings());
        cx.set_global(io.clone());
        cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None), KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());

        let client = cx.new(|cx| Client::new(io.clone(), cx));
        // GPUI's own switch covers every animation; keep it in step with the preference.
        let reduce = client.read(cx).prefs.reduce_motion;
        cx.set_reduce_motion(reduce);
        // The theme follows the preference too; switching repaints everything, so only on change.
        let mut light = client.read(cx).prefs.light_theme;
        if light {
            rhein_ui::set_theme(light, cx);
        }
        cx.observe(&client, move |client, cx| {
            let prefs = &client.read(cx).prefs;
            let reduce = prefs.reduce_motion;
            let now_light = prefs.light_theme;
            cx.set_reduce_motion(reduce);
            if now_light != light {
                light = now_light;
                rhein_ui::set_theme(light, cx);
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some("Rhein".into()), ..Default::default() }),
                app_id: Some("chat.rhein.Rhein".into()),
                window_min_size: Some(size(px(720.), px(480.))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| views::Root::new(client, window, cx)),
        )
        .expect("open window");
        cx.activate(true);
    });
}
