//! The tab bar across the top of the main area. Takes plain labels so other
//! Tab kinds only change what the caller passes in.

use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::widget::{button, container, mouse_area, row, scrollable, stack, tooltip};
use crate::views::text;
use iced::{Border, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use crate::app::{Message, TabMessage};
use crate::theme as t;

/// Narrowest a Tab shrinks to before the bar scrolls.
pub const MIN_TAB_WIDTH: f32 = 120.0;
/// Widest a Tab grows to when there's room.
pub const MAX_TAB_WIDTH: f32 = 200.0;
const TAB_SPACING: f32 = 2.0;
/// Width of the `+` button, padding included.
const PLUS_WIDTH: f32 = 36.0;
/// Horizontal padding either side of the Tab row.
const BAR_PADDING: f32 = 8.0;

/// Width the Tabs have between the sidebar and the `+` button, given the
/// window width. The sidebar is a fixed width, so this is exact enough to
/// keep the bar from ever widening the page.
pub fn tab_viewport(window_width: f32) -> f32 {
    (window_width - crate::terminal::panel::SIDEBAR_WIDTH - PLUS_WIDTH - 2.0 * BAR_PADDING).max(0.0)
}

/// Width of each Tab: shares the viewport evenly, clamped between the minimum
/// and maximum.
pub fn tab_width(count: usize, viewport: f32) -> f32 {
    if count == 0 {
        return MAX_TAB_WIDTH;
    }
    let gaps = TAB_SPACING * (count as f32 - 1.0);
    ((viewport - gaps) / count as f32).clamp(MIN_TAB_WIDTH, MAX_TAB_WIDTH)
}

/// Total width of the Tab row's content.
pub fn content_width(count: usize, tab_w: f32) -> f32 {
    if count == 0 {
        return 0.0;
    }
    count as f32 * tab_w + TAB_SPACING * (count as f32 - 1.0)
}

/// The horizontal scroll offset that keeps Tab `index` fully in view, moving
/// as little as possible from `current`.
pub fn reveal_offset(index: usize, count: usize, tab_w: f32, viewport: f32, current: f32) -> f32 {
    let left = index as f32 * (tab_w + TAB_SPACING);
    let right = left + tab_w;
    let wanted = if left < current {
        left
    } else if right > current + viewport {
        right - viewport
    } else {
        current
    };
    let max = (content_width(count, tab_w) - viewport).max(0.0);
    wanted.clamp(0.0, max)
}

/// Pixels the pointer must travel from the press before a Tab starts to drag,
/// so a plain click still focuses.
pub const DRAG_THRESHOLD: f32 = 8.0;

/// The Tab slot under window x `cursor_x`, given the bar's scroll offset.
/// Clamped to the first and last Tab.
pub fn drop_index(cursor_x: f32, offset: f32, count: usize, tab_w: f32) -> usize {
    if count == 0 {
        return 0;
    }
    let x = cursor_x - crate::terminal::panel::SIDEBAR_WIDTH - BAR_PADDING + offset;
    let slot = (x / (tab_w + TAB_SPACING)).floor();
    (slot.max(0.0) as usize).min(count - 1)
}

pub fn scroll_id() -> scrollable::Id {
    scrollable::Id::new("tab-bar")
}

/// Corner radius of a Tab and of the drag placeholder's outline.
const TAB_RADIUS: f32 = 6.0;

/// The dashed accent outline drawn over the slot of the Tab being dragged.
struct DragPlaceholder;

impl canvas::Program<Message> for DragPlaceholder {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        // Inset half the stroke so the outline isn't clipped at the edges.
        let outline = Path::rounded_rectangle(
            Point::new(0.5, 0.5),
            Size::new(bounds.width - 1.0, bounds.height - 1.0),
            TAB_RADIUS.into(),
        );
        let mut stroke = Stroke::default().with_color(t::accent()).with_width(1.0);
        stroke.line_dash = canvas::LineDash { segments: &[4.0, 3.0], offset: 0 };
        frame.stroke(&outline, stroke);
        vec![frame.into_geometry()]
    }
}

/// `dragging` is the position of the Tab being dragged, once the pointer has
/// passed the threshold: its slot draws as a dashed placeholder. `needs` is,
/// per Tab, what its Terminals wait on the person for (a dot before the label).
pub fn view<'a>(
    tabs: &[(String, bool)],
    needs: &[Option<crate::terminal::Need>],
    window_width: f32,
    dragging: Option<usize>,
) -> Element<'a, Message> {
    let tab_w = tab_width(tabs.len(), tab_viewport(window_width));
    let mut bar = row![].spacing(TAB_SPACING).padding([4.0, BAR_PADDING]);
    for (i, (label, active)) in tabs.iter().enumerate() {
        let active = *active;
        let placeholder = dragging == Some(i);
        // Not a button: a button captures the press, and the Tab drags from it.
        let label_button = mouse_area(
            container(crate::views::truncated::truncated(
                label.clone(),
                13.0,
                if placeholder {
                    t::text_muted()
                } else if active {
                    t::text_strong()
                } else {
                    t::text_secondary()
                },
            ))
            .width(Length::Fill)
            .clip(true)
            .padding(iced::Padding { top: 5.0, right: 4.0, bottom: 5.0, left: 12.0 }),
        )
        .on_press(Message::Tab(TabMessage::DragPress(i)))
        // The drag subscription only starts after the press is handled, so a
        // quick click can release before it listens; end the press here too.
        // A second DragEnd from the subscription is a no-op.
        .on_release(Message::Tab(TabMessage::DragEnd))
        .interaction(iced::mouse::Interaction::Pointer);
        // The label is cut to one line; hovering shows the full title.
        let label_button = tooltip(
            label_button,
            container(text(label.clone()).size(12).color(t::text_strong()))
                .padding([4, 8])
                .style(|_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(t::bg_menu())),
                    border: Border { color: t::border(), width: 1.0, radius: 4.0.into() },
                    ..Default::default()
                }),
            tooltip::Position::Bottom,
        )
        .gap(4);
        let close = button(text("\u{00D7}").size(14).color(t::text_muted()))
            .padding([3, 8])
            .on_press(Message::Tab(TabMessage::Close(i)))
            .style(|_theme: &Theme, status| button::Style {
                background: matches!(status, button::Status::Hovered)
                    .then(|| iced::Background::Color(t::bg_card_focused())),
                border: Border { radius: 4.0.into(), ..Default::default() },
                ..Default::default()
            });
        let dot = needs.get(i).copied().flatten().map(|n| -> Element<'a, Message> {
            container(crate::views::terminal_panel::attention_dot(n))
                .padding(iced::Padding { left: 8.0, ..Default::default() })
                .into()
        });
        let tab = container(
            row![]
                .push_maybe(dot)
                .push(label_button)
                .push(close)
                .align_y(iced::Alignment::Center),
        )
            .width(Length::Fixed(tab_w))
            .padding([0, 4])
            .style(move |_theme: &Theme| container::Style {
                background: (active && !placeholder)
                    .then(|| iced::Background::Color(t::bg_secondary())),
                // The placeholder drops the solid border so its dashed outline shows.
                border: Border {
                    color: t::border(),
                    width: if placeholder { 0.0 } else { 1.0 },
                    radius: TAB_RADIUS.into(),
                },
                ..Default::default()
            });
        if placeholder {
            let outline = Canvas::new(DragPlaceholder).width(Length::Fill).height(Length::Fill);
            bar = bar.push(stack![tab, outline]);
        } else {
            bar = bar.push(tab);
        }
    }
    let scroller = scrollable(bar)
        .id(scroll_id())
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(0).scroller_width(0),
        ))
        .on_scroll(|v| Message::Tab(TabMessage::BarScrolled(v.absolute_offset().x)))
        .width(Length::Fill);
    let plus = button(text("+").size(16).color(t::text_muted()))
        .padding([3, 12])
        .on_press(Message::OpenCommandPalette)
        .style(|_theme: &Theme, status| button::Style {
            background: matches!(status, button::Status::Hovered)
                .then(|| iced::Background::Color(t::bg_card_focused())),
            border: Border { radius: 4.0.into(), ..Default::default() },
            ..Default::default()
        });
    container(row![scroller, plus].align_y(iced::Alignment::Center))
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::base(t::bg_menu()))),
            border: Border { color: t::border(), width: 0.0, radius: 0.0.into() },
            ..Default::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_index_follows_the_pointer_and_the_scroll() {
        let left = crate::terminal::panel::SIDEBAR_WIDTH + BAR_PADDING;
        assert_eq!(drop_index(left + 10.0, 0.0, 5, 120.0), 0);
        assert_eq!(drop_index(left + 130.0, 0.0, 5, 120.0), 1);
        assert_eq!(drop_index(left + 10.0, 244.0, 5, 120.0), 2);
        assert_eq!(drop_index(0.0, 0.0, 5, 120.0), 0);
        assert_eq!(drop_index(5000.0, 0.0, 5, 120.0), 4);
    }

    #[test]
    fn few_tabs_cap_at_max() {
        assert_eq!(tab_width(2, 800.0), MAX_TAB_WIDTH);
    }

    #[test]
    fn tabs_share_the_viewport_then_floor() {
        let w = tab_width(5, 800.0);
        assert!(w > MIN_TAB_WIDTH && w < MAX_TAB_WIDTH);
        assert!((content_width(5, w) - 800.0).abs() < 0.01);
        assert_eq!(tab_width(20, 800.0), MIN_TAB_WIDTH);
        assert_eq!(tab_width(0, 800.0), MAX_TAB_WIDTH);
    }

    #[test]
    fn twenty_tabs_at_minimum_window_scroll_inside_the_viewport() {
        let viewport = tab_viewport(800.0);
        assert!(viewport > 0.0 && viewport < 800.0 - 220.0);
        let w = tab_width(20, viewport);
        assert_eq!(w, MIN_TAB_WIDTH);
        assert!(content_width(20, w) > viewport);
    }

    #[test]
    fn plus_button_has_room_beside_the_tabs_at_every_width() {
        for window in [800.0, 1100.0, 1920.0] {
            let used = crate::terminal::panel::SIDEBAR_WIDTH
                + 2.0 * BAR_PADDING
                + tab_viewport(window)
                + PLUS_WIDTH;
            assert!(used <= window + 0.01, "{used} > {window}");
        }
    }

    #[test]
    fn reveal_leaves_a_visible_tab_alone() {
        assert_eq!(reveal_offset(1, 20, 120.0, 500.0, 0.0), 0.0);
    }

    #[test]
    fn reveal_scrolls_right_to_a_tab_past_the_edge() {
        let off = reveal_offset(10, 20, 120.0, 500.0, 0.0);
        // Right edge of tab 10 lands on the right edge of the viewport.
        assert_eq!(off, 10.0 * 122.0 + 120.0 - 500.0);
    }

    #[test]
    fn reveal_scrolls_left_to_a_tab_before_the_edge() {
        assert_eq!(reveal_offset(2, 20, 120.0, 500.0, 800.0), 244.0);
    }

    #[test]
    fn reveal_stays_within_the_content() {
        let last = reveal_offset(19, 20, 120.0, 500.0, 0.0);
        assert_eq!(last, content_width(20, 120.0) - 500.0);
        assert_eq!(reveal_offset(0, 3, 120.0, 500.0, 90.0), 0.0);
    }
}
