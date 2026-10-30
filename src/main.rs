mod api;
mod app;
mod config;
mod csv_io;
mod db;
mod display;
mod fonts;
mod keys;
mod mentions;
mod scroll;
mod sync;
mod theme;
mod views;

use iced::{window, Size};

fn main() -> iced::Result {
    let font_key = config::load().0.font;
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
