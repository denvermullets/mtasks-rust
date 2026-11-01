mod api;
mod app;
mod config;
mod csv_io;
mod dashboard_eval;
mod dashboard_session;
mod db;
mod display;
mod files;
mod fonts;
mod inbox;
mod issue_match;
mod keys;
mod lane_icon;
mod mentions;
mod palette_results;
mod saved_view;
mod schedule_summary;
mod scroll;
mod sync;
mod theme;
mod views;

use iced::{window, Size};

fn main() -> iced::Result {
    let cfg = config::load().0;
    theme::set_by_key(&cfg.theme);
    let font_key = cfg.font;
    let mut app = iced::application(app::App::title, app::App::update, app::App::view);
    for bytes in fonts::FONT_BYTES {
        app = app.font(bytes);
    }
    app.default_font(fonts::font_for_key(&font_key))
        .subscription(app::App::subscription)
        .theme(app::App::theme)
        .window(window::Settings {
            size: Size::new(1100.0, 700.0),
            min_size: Some(Size::new(800.0, 500.0)),
            ..Default::default()
        })
        .run_with(app::App::new)
}
