mod api;
mod app;
mod body_cache;
mod body_hover;
mod card_placement;
mod config;
mod csv_io;
mod dashboard_eval;
mod dashboard_session;
mod db;
mod display;
mod document;
mod files;
mod fonts;
mod hover_card;
mod highlight;
mod inbox;
mod issue_match;
mod keys;
mod lane_icon;
mod mcp_prompt;
mod mentions;
mod palette_results;
mod quit;
mod saved_view;
mod schedule_summary;
mod scroll;
mod search_match;
mod sidebar_layout;
mod sync;
mod tab_state;
mod tabs;
mod terminal;
mod theme;
mod views;

use iced::{window, Size};

fn main() -> iced::Result {
    highlight::preload();
    let cfg = config::load().0;
    theme::set_by_key(&cfg.theme);
    let font_key = cfg.font;
    let mut app = iced::application(app::App::title, app::App::update, app::App::view);
    for bytes in fonts::FONT_BYTES {
        app = app.font(bytes);
    }
    fonts::set_active(fonts::font_for_key(&font_key));
    app.default_font(fonts::active())
        // The app owns quitting: see `quit.rs` and `App::quit`.
        .exit_on_close_request(false)
        .subscription(app::App::subscription)
        .theme(app::App::theme)
        .window(window::Settings {
            size: Size::new(1100.0, 700.0),
            min_size: Some(Size::new(800.0, 500.0)),
            #[cfg(target_os = "macos")]
            platform_specific: window::settings::PlatformSpecific {
                // The app paints its own title strip (`header::titlebar`) in
                // the theme's colors; only the traffic lights stay native.
                title_hidden: true,
                titlebar_transparent: true,
                fullsize_content_view: true,
            },
            ..Default::default()
        })
        .run_with(app::App::new)
}
