mod api;
mod app;
mod config;
mod csv_io;
mod db;
mod display;
mod keys;
mod mentions;
mod scroll;
mod sync;
mod theme;
mod views;

use iced::{window, Size};

fn main() -> iced::Result {
    // PROTOTYPE (JRST-4): JAIT_THEME picks the starting theme, JAIT_FONT the font.
    if let Ok(key) = std::env::var("JAIT_THEME") {
        theme::set_by_key(&key);
    }
    iced::application(app::App::title, app::App::update, app::App::view)
        .font(include_bytes!("../assets/prototype-fonts/Inter.ttf").as_slice())
        .font(include_bytes!("../assets/prototype-fonts/JetBrainsMono.ttf").as_slice())
        .font(include_bytes!("../assets/prototype-fonts/IBMPlexMono.ttf").as_slice())
        .font(include_bytes!("../assets/prototype-fonts/SpaceGrotesk.ttf").as_slice())
        .default_font(theme::startup_font().1)
        .subscription(app::App::subscription)
        .theme(app::App::theme)
        .window(window::Settings {
            size: Size::new(1100.0, 700.0),
            min_size: Some(Size::new(800.0, 500.0)),
            ..Default::default()
        })
        .run_with(app::App::new)
}
