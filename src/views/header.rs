use iced::widget::text::LineHeight;
use iced::widget::{button, container, row, text, text_input};
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::Project;
use crate::fonts;
use crate::theme as t;

/// Id of the header search box, so a shortcut can focus it.
pub fn search_input_id() -> text_input::Id {
    text_input::Id::new("header-search")
}

/// Whether `project` matches `needle` by name.
pub fn project_matches(project: &Project, needle: &str) -> bool {
    needle.is_empty() || project.name.to_lowercase().contains(needle)
}

/// "Update view" for a Modified Saved view: saves the working copy as the
/// view's definition. Disabled (no press) with a read-only token.
pub fn update_view_button<'a>(read_only: bool) -> Element<'a, Message> {
    button(text("Update view").size(13))
        .on_press_maybe((!read_only).then_some(Message::UpdateSavedView))
        .padding([5, 12])
        .style(button::secondary)
        .into()
}

/// "Save as new view" for a Modified Saved view: asks for a name and saves the
/// working copy as a new view. Disabled (no press) with a read-only token.
pub fn save_as_new_view_button<'a>(read_only: bool) -> Element<'a, Message> {
    button(text("Save as new view").size(13))
        .on_press_maybe((!read_only).then_some(Message::OpenSaveAsNewView))
        .padding([5, 12])
        .style(button::secondary)
        .into()
}

/// The header's Revert action for a Modified view.
pub fn revert_button<'a>() -> Element<'a, Message> {
    button(text("Revert").size(13))
        .on_press(Message::RevertSavedView)
        .padding([5, 12])
        .style(button::secondary)
        .into()
}

/// Rename for the open Saved view. Disabled (no press) with a read-only token.
pub fn rename_view_button<'a>(view_id: &str, read_only: bool) -> Element<'a, Message> {
    button(text("Rename").size(13))
        .on_press_maybe((!read_only).then(|| Message::OpenRenameSavedView(view_id.to_string())))
        .padding([5, 12])
        .style(button::secondary)
        .into()
}

/// Delete for the open Saved view (asks to confirm). Disabled (no press) with
/// a read-only token.
pub fn delete_view_button<'a>(view_id: &str, read_only: bool) -> Element<'a, Message> {
    button(text("Delete").size(13))
        .on_press_maybe((!read_only).then(|| Message::ConfirmDeleteSavedView(view_id.to_string())))
        .padding([5, 12])
        .style(button::secondary)
        .into()
}

/// Height of the macOS title bar, which the window draws its traffic lights in.
const TITLEBAR_HEIGHT: f32 = 28.0;

/// Themed strip behind the transparent macOS title bar (see `main.rs`). The
/// title bar sits above this and handles window dragging and double-click zoom,
/// so the strip has no interaction of its own.
pub fn titlebar<'a>(title: String) -> Element<'a, Message> {
    let bar = container(text(title).size(t::type_scale::XS).color(t::text_muted()))
        .center_x(Length::Fill)
        .center_y(Length::Fixed(TITLEBAR_HEIGHT))
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_menu())),
            ..Default::default()
        });
    let rule = container(iced::widget::Space::with_height(1.0))
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::border())),
            ..Default::default()
        });
    iced::widget::column![bar, rule].into()
}

/// The dashboard's top bar (web `h-14 bg-menu border-b`): breadcrumb on the
/// left, the dashboard's menu on the right.
pub fn dashboard_bar<'a>(
    crumbs: Vec<String>,
    menu: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut trail = row![].spacing(t::space::S4).align_y(Alignment::Center);
    let last = crumbs.len().saturating_sub(1);
    for (i, crumb) in crumbs.into_iter().enumerate() {
        if i > 0 {
            trail = trail.push(text("\u{203A}").size(t::type_scale::SM).color(t::text_muted()));
        }
        let color = if i == last { t::text_primary() } else { t::text_muted() };
        trail = trail.push(text(crumb).size(t::type_scale::SM).color(color));
    }
    let bar = container(
        row![]
            .align_y(Alignment::Center)
            .push(trail)
            .push(iced::widget::horizontal_space())
            .push_maybe(menu),
    )
    .padding([0.0, t::space::S4])
    .center_y(Length::Fixed(t::space::S12 + t::space::S2))
    .width(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_menu())),
        ..Default::default()
    });
    let rule = container(iced::widget::Space::with_height(1.0))
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::border())),
            ..Default::default()
        });
    iced::widget::column![bar, rule].into()
}

/// Top strip above the main pane: breadcrumb on the left; Search, Display and
/// "+ New" on the right. `search_enabled` is false on views with nothing to
/// filter; `new_action` is `None` where "+ New" has no target. `board_controls`
/// is `Some(active_filter_count)` on the board, where Display and Filter apply,
/// and `None` elsewhere (the buttons are disabled). `show_controls` false drops
/// Search, Filter, Display and "+ New" entirely (project and issue detail).
pub fn view<'a>(
    crumbs: Vec<String>,
    show_controls: bool,
    query: &'a str,
    search_enabled: bool,
    board_controls: Option<usize>,
    new_action: Option<Message>,
    back: Option<Message>,
    view_actions: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut trail = row![].spacing(8).align_y(Alignment::Center);
    if let Some(msg) = back {
        trail = trail.push(
            button(text("\u{2039} Back").size(13))
                .on_press(msg)
                .padding([3, 9])
                .style(button::secondary),
        );
    }
    let last = crumbs.len().saturating_sub(1);
    for (i, crumb) in crumbs.into_iter().enumerate() {
        if i > 0 {
            trail = trail.push(text("\u{203A}").size(14).color(t::text_muted())); // ›
        }
        let color = if i == last {
            t::text_primary()
        } else {
            t::text_secondary()
        };
        trail = trail.push(text(crumb).size(14).color(color));
    }

    let mut search = text_input(if search_enabled { "Search…" } else { "" }, query)
        .id(search_input_id())
        .padding([5, 10])
        .size(13)
        .width(Length::Fixed(220.0));
    if search_enabled {
        search = search.on_input(Message::HeaderSearchChanged);
    }

    let display = button(text("Display").size(13))
        .on_press_maybe(board_controls.map(|_| Message::OpenDisplayOptions))
        .padding([5, 12])
        .style(button::secondary);

    // Filter button with an active-filter count badge.
    let filter_count = board_controls.unwrap_or(0);
    let filter_label = if filter_count > 0 {
        format!("Filter \u{b7} {}", filter_count)
    } else {
        "Filter".to_string()
    };
    let filter = button(text(filter_label).size(13))
        .on_press_maybe(board_controls.map(|_| Message::OpenFilterPanel))
        .padding([5, 12])
        .style(move |theme: &Theme, status| {
            let mut style = button::secondary(theme, status);
            if filter_count > 0 {
                style.text_color = t::accent();
                style.border = Border {
                    color: t::accent(),
                    width: 1.0,
                    radius: 4.0.into(),
                };
            }
            style
        });

    // The web's New button: `gap-1.5 px-3 py-1.5 text-xs rounded bg-accent`. Both
    // texts take their color from the style, so they follow hover.
    let enabled = new_action.is_some();
    let new_label = row![]
        .spacing(t::space::S1_5)
        .align_y(Alignment::Center)
        .push(
            text("+")
                .size(t::type_scale::SM)
                .line_height(LineHeight::Relative(t::type_scale::LH_XS * t::type_scale::XS / t::type_scale::SM))
                .font(fonts::bold_active()),
        )
        .push(
            text("New")
                .size(t::type_scale::XS)
                .line_height(LineHeight::Relative(t::type_scale::LH_XS)),
        );
    let mut new_button = button(new_label)
        .padding([t::space::S1_5, t::space::S3])
        .style(move |_theme: &Theme, status| button::Style {
            background: Some(iced::Background::Color(if enabled {
                t::accent_fill(status)
            } else {
                t::bg_card()
            })),
            text_color: if enabled {
                t::on_accent(status)
            } else {
                t::text_muted()
            },
            border: Border {
                radius: t::radius::SM.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    if let Some(msg) = new_action {
        new_button = new_button.on_press(msg);
    }

    let mut content = row![]
        .spacing(8)
        .align_y(Alignment::Center)
        .push(trail)
        .push(iced::widget::horizontal_space())
        .push_maybe(view_actions);
    if show_controls {
        content = content
            .push(search)
            .push(filter)
            .push(display)
            .push(new_button);
    }

    container(content)
        .padding([8, 16])
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_secondary())),
            border: Border {
                color: t::border(),
                width: 0.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
}
