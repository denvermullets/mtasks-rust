//! Card picker popover for one issue property (status, priority, project or
//! labels), opened by Rails' card shortcuts `s`, `p`, `j` and `l` on the
//! hovered card (or the open issue).
//!
//! Matches Rails' shared pickers (`*_picker_controller.js`): a 320px popover
//! anchored beside the card — to its left on the board, centred over a list
//! row — with an `IDENT • title` header and a focused search box (none for
//! priority). Nothing is highlighted at first; ↑/↓ wrap, Enter picks, Esc or
//! a click outside closes. Status, priority and project close on pick; labels
//! toggle in place and stay open, with a "Create" row when the search has no
//! exact match.

use iced::widget::{button, column, container, mouse_area, row, scrollable, Space};
use crate::views::text;
use iced::{Alignment, Border, Color, Element, Length, Padding, Point, Rectangle, Size, Theme};

use crate::app::Message;
use crate::db::models::{Label, Lane, Priority, Project};
use crate::theme::{self as t, radius, type_scale};

/// Rails' `w-80`.
pub const WIDTH: f32 = 320.0;
/// Rails' positioning estimate (`overlayHeight`).
const EST_HEIGHT: f32 = 400.0;
/// Rails' `max-h-96`.
const LIST_MAX_H: f32 = 384.0;
const GAP: f32 = 8.0;
/// Rows wider than this are list rows (Rails' `isWideElement`).
const WIDE_ROW: f32 = 800.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Status,
    Priority,
    Project,
    Labels,
}

impl Field {
    fn placeholder(self) -> Option<&'static str> {
        match self {
            Field::Status => Some("Search statuses..."),
            Field::Priority => None,
            Field::Project => Some("Search projects..."),
            Field::Labels => Some("Search or create labels..."),
        }
    }
}

/// The value one picker row writes to the issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Lane(String),
    Priority(i32),
    Project(Option<String>),
    /// Toggle this label on the issue.
    Label(String),
    /// Create a label with this name and add it to the issue.
    CreateLabel(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickerOption {
    pub label: String,
    pub color: Option<Color>,
    pub choice: Choice,
}

/// The card the picker belongs to, in window pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub rect: Rectangle,
}

/// One rendered line: a section heading or a pickable row.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Heading(&'static str),
    Item(PickerOption),
}

/// Open picker. The candidate rows are loaded for the issue's own team when
/// it opens, so cross-team views (My Issues) get the right lanes and labels.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerState {
    pub issue_id: String,
    pub team_id: String,
    pub field: Field,
    /// `IDENT • title` (title cut to 30 chars, as Rails does).
    pub header: String,
    pub query: String,
    /// Highlighted item (index into `items()`); `None` until ↑/↓.
    pub focus: Option<usize>,
    /// `None` opens it near the top, centred (issue page, keyboard focus).
    pub anchor: Option<Anchor>,
    /// Every candidate, in display order (labels: most used first).
    pub options: Vec<PickerOption>,
    /// Labels only: Rails' "Frequently used" (top 5 with at least one use).
    pub frequent: Vec<PickerOption>,
    /// The issue's current value(s): radio / checkbox state.
    pub current: Vec<Choice>,
    /// Opened on the issue page: drawn as the sidebar field's own dropdown
    /// (`sidebar_menu`), not as a floating popover.
    pub in_sidebar: bool,
}

/// Inputs for `PickerState::new`, all scoped to the issue's team.
pub struct Source<'a> {
    pub lanes: &'a [Lane],
    pub projects: &'a [Project],
    pub labels: &'a [Label],
    /// How many issues carry each label id.
    pub label_usage: &'a std::collections::HashMap<String, usize>,
}

const PRIORITIES: [Priority; 5] = [
    Priority::None,
    Priority::Urgent,
    Priority::High,
    Priority::Medium,
    Priority::Low,
];

fn hex(c: &Option<String>) -> Option<Color> {
    c.as_deref().and_then(t::hex_to_color)
}

/// "IDENT • title", the title cut to 30 characters like Rails' `truncate(30)`.
pub fn header(identifier: &str, title: &str) -> String {
    let title = if title.chars().count() > 30 {
        let cut: String = title.chars().take(27).collect();
        format!("{cut}...")
    } else {
        title.to_string()
    };
    if title.is_empty() {
        identifier.to_string()
    } else {
        format!("{identifier} • {title}")
    }
}

impl PickerState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        issue_id: String,
        team_id: String,
        field: Field,
        header: String,
        anchor: Option<Anchor>,
        current: Vec<Choice>,
        src: Source<'_>,
    ) -> Self {
        let mut frequent = Vec::new();
        let options = match field {
            Field::Status => src
                .lanes
                .iter()
                .map(|l| PickerOption {
                    label: l.name.clone(),
                    color: hex(&l.color),
                    choice: Choice::Lane(l.id.clone()),
                })
                .collect(),
            Field::Priority => PRIORITIES
                .iter()
                .map(|p| PickerOption {
                    label: p.label().to_string(),
                    color: Some(t::priority_dot_color(*p)),
                    choice: Choice::Priority(p.to_i32()),
                })
                .collect(),
            Field::Project => {
                // Rails only offers "No project" when the team has projects.
                let none = (!src.projects.is_empty()).then(|| PickerOption {
                    label: "No project".to_string(),
                    color: None,
                    choice: Choice::Project(None),
                });
                none.into_iter()
                    .chain(src.projects.iter().map(|p| PickerOption {
                        label: p.name.clone(),
                        color: None,
                        choice: Choice::Project(Some(p.id.clone())),
                    }))
                    .collect()
            }
            Field::Labels => {
                let uses = |l: &Label| src.label_usage.get(&l.id).copied().unwrap_or(0);
                let mut sorted: Vec<&Label> = src.labels.iter().collect();
                sorted.sort_by(|a, b| {
                    uses(b).cmp(&uses(a)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                let opt = |l: &Label| PickerOption {
                    label: l.name.clone(),
                    color: hex(&l.color),
                    choice: Choice::Label(l.id.clone()),
                };
                frequent = sorted.iter().filter(|l| uses(l) > 0).take(5).map(|l| opt(l)).collect();
                sorted.iter().map(|l| opt(l)).collect()
            }
        };
        Self {
            issue_id,
            team_id,
            field,
            header,
            query: String::new(),
            focus: None,
            anchor,
            options,
            frequent,
            current,
            in_sidebar: false,
        }
    }

    pub fn has_search(&self) -> bool {
        self.field.placeholder().is_some()
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.focus = None;
    }

    /// Lines to render: headings and items, filtered by the search.
    pub fn lines(&self) -> Vec<Line> {
        let q = self.query.trim().to_lowercase();
        let matches = |o: &&PickerOption| q.is_empty() || o.label.to_lowercase().contains(&q);
        let items = |opts: &[PickerOption]| -> Vec<Line> {
            opts.iter().filter(matches).cloned().map(Line::Item).collect()
        };
        if self.field != Field::Labels {
            return items(&self.options);
        }
        let mut lines = Vec::new();
        if q.is_empty() {
            if !self.frequent.is_empty() {
                lines.push(Line::Heading("Frequently used"));
                lines.extend(items(&self.frequent));
            }
        } else if !self.options.iter().any(|o| o.label.to_lowercase() == q) {
            lines.push(Line::Item(PickerOption {
                label: self.query.trim().to_string(),
                color: None,
                choice: Choice::CreateLabel(self.query.trim().to_string()),
            }));
        }
        lines.push(Line::Heading("Labels"));
        lines.extend(items(&self.options));
        lines
    }

    /// The pickable rows of `lines()`, in order; `focus` indexes this.
    pub fn items(&self) -> Vec<PickerOption> {
        self.lines()
            .into_iter()
            .filter_map(|l| match l {
                Line::Item(o) => Some(o),
                Line::Heading(_) => None,
            })
            .collect()
    }

    /// ↑/↓, wrapping at both ends like Rails' `focusNext`/`focusPrevious`.
    pub fn move_focus(&mut self, delta: i32) {
        let len = self.items().len() as i32;
        if len == 0 {
            return;
        }
        let from = self.focus.map_or(-1, |i| i as i32);
        let mut next = from + delta.signum();
        if next >= len {
            next = 0;
        }
        if next < 0 {
            next = len - 1;
        }
        self.focus = Some(next as usize);
    }

    pub fn is_current(&self, choice: &Choice) -> bool {
        self.current.contains(choice)
    }

    /// Flip a label's checkbox (after its write succeeded).
    pub fn toggle_label(&mut self, label_id: &str) {
        let c = Choice::Label(label_id.to_string());
        if let Some(i) = self.current.iter().position(|x| *x == c) {
            self.current.remove(i);
        } else {
            self.current.push(c);
        }
    }

    /// The issue's label ids after the checkboxes as they stand.
    pub fn label_ids(&self) -> Vec<String> {
        self.current
            .iter()
            .filter_map(|c| match c {
                Choice::Label(id) => Some(id.clone()),
                _ => None,
            })
            .collect()
    }
}

/// Top-left corner of the popover, following Rails' `positionOverlay`.
pub fn position(anchor: Option<Anchor>, window: Size) -> Point {
    let centred = ((window.width - WIDTH) / 2.0).max(GAP);
    let Some(Anchor { rect }) = anchor else {
        return Point::new(centred, 80.0);
    };
    let left = if rect.width > WIDE_ROW {
        centred
    } else {
        let mut left = rect.x - WIDTH - GAP;
        if left < GAP {
            left = rect.x + rect.width + GAP;
        }
        if left + WIDTH > window.width - GAP {
            left = centred;
        }
        left
    };
    let mut top = rect.y;
    if top + EST_HEIGHT > window.height {
        top = window.height - EST_HEIGHT - GAP;
    }
    if top < GAP {
        top = GAP;
    }
    Point::new(left, top)
}

const SEARCH_KEY: &str = "property-picker-search";

pub fn search_id() -> iced::widget::Id {
    crate::undo_input::id(SEARCH_KEY)
}

fn dot<'a>(color: Color) -> Element<'a, Message> {
    container(Space::new().width(8).height(8))
        .style(move |_: &Theme| container::Style {
            background: Some(color.into()),
            border: Border { radius: 4.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

/// Checkbox (labels) or radio (everything else) for a row.
fn marker<'a>(field: Field, on: bool) -> Element<'a, Message> {
    let round = if field == Field::Labels { 3.0 } else { 7.0 };
    let inner: Element<'a, Message> = if on && field == Field::Labels {
        text("✓").size(10).color(t::on_fill(t::accent())).into()
    } else if on {
        container(Space::new().width(6).height(6))
            .style(|_: &Theme| container::Style {
                background: Some(t::on_fill(t::accent()).into()),
                border: Border { radius: 3.0.into(), ..Default::default() },
                ..Default::default()
            })
            .into()
    } else {
        Space::new().width(0).height(0).into()
    };
    container(inner)
        .center_x(Length::Fixed(14.0))
        .center_y(Length::Fixed(14.0))
        .style(move |_: &Theme| container::Style {
            background: on.then(|| t::accent().into()),
            border: Border {
                color: if on { t::accent() } else { t::border() },
                width: 1.0,
                radius: round.into(),
            },
            ..Default::default()
        })
        .into()
}

/// The pickable rows (with section headings, or an empty-state line).
fn list(state: &PickerState) -> Element<'_, Message> {
    let mut list = column![];
    let mut item_idx = 0;
    for line in state.lines() {
        match line {
            Line::Heading(title) => {
                list = list.push(
                    container(text(title).size(type_scale::XS).color(t::text_muted()))
                        .padding([8, 12]),
                );
            }
            Line::Item(option) => {
                let i = item_idx;
                item_idx += 1;
                let focused = state.focus == Some(i);
                let content: Element<'_, Message> = match option.choice {
                    Choice::CreateLabel(ref name) => text(format!("+  Create \"{name}\""))
                        .size(type_scale::SM)
                        .color(t::accent())
                        .into(),
                    ref choice => {
                        let muted = matches!(choice, Choice::Project(None) | Choice::Priority(4));
                        let on = state.is_current(choice);
                        // Rails' priority rows carry a trailing check, not a radio.
                        let radio = state.field != Field::Priority;
                        row![]
                            .spacing(8)
                            .align_y(Alignment::Center)
                            .push(radio.then(|| marker(state.field, on)))
                            .push(option.color.map(dot))
                            .push(
                                text(option.label.clone())
                                    .size(type_scale::SM)
                                    .color(if muted { t::text_muted() } else { t::text_primary() })
                                    .width(Length::Fill),
                            )
                            .push(
                                (!radio && on)
                                    .then(|| text("✓").size(type_scale::SM).color(t::accent())),
                            )
                            .into()
                    }
                };
                list = list.push(
                    button(content)
                        .on_press(Message::PropertyPickerChoose(i))
                        .padding([8, 12])
                        .width(Length::Fill)
                        .style(move |theme: &Theme, status| {
                            let mut style = button::text(theme, status);
                            if focused || matches!(status, button::Status::Hovered) {
                                style.background = Some(t::bg_card_focused().into());
                            }
                            style
                        }),
                );
            }
        }
    }
    if item_idx == 0 {
        let empty = match state.field {
            Field::Labels => "No labels found. Start typing to create one.",
            Field::Project => "No projects found.",
            _ => "No matches.",
        };
        list = list.push(
            container(text(empty).size(type_scale::SM).color(t::text_muted()))
                .center_x(Length::Fill)
                .padding([32, 12]),
        );
    }
    list.into()
}

fn rule<'a>() -> container::Container<'a, Message> {
    container(Space::new().width(Length::Fill).height(1)).style(|_: &Theme| container::Style {
        background: Some(t::border().into()),
        ..Default::default()
    })
}

fn search_box<'a>(state: &'a PickerState, placeholder: &'static str) -> Element<'a, Message> {
    container(
        crate::undo_input::text_input(SEARCH_KEY, placeholder, &state.query, Message::PropertyPickerQuery)
            .on_submit(Message::PropertyPickerSubmit)
            .size(type_scale::SM)
            .padding([8, 12]),
    )
    .padding(8)
    .into()
}

fn new_project_row<'a>() -> Element<'a, Message> {
    button(text("+  New Project").size(type_scale::SM).color(t::accent()))
        .on_press(Message::PropertyPickerNewProject)
        .padding([8, 12])
        .width(Length::Fill)
        .style(button::text)
        .into()
}

/// The picker as the issue sidebar's own dropdown (Rails' `issue_sidebar`
/// menus): a plain list the width of the field, no header. Only labels keep
/// their search box, as Rails' sidebar label picker does.
pub fn sidebar_menu(state: &PickerState) -> Element<'_, Message> {
    let mut menu = column![].width(Length::Fill);
    if state.field == Field::Labels {
        if let Some(placeholder) = state.field.placeholder() {
            menu = menu.push(search_box(state, placeholder)).push(rule());
        }
    }
    menu = menu.push(container(scrollable(list(state))).max_height(LIST_MAX_H));
    if state.field == Field::Project {
        menu = menu.push(rule()).push(new_project_row());
    }
    container(menu)
        .style(|_: &Theme| container::Style {
            background: Some(t::bg_secondary().into()),
            border: Border { color: t::border(), width: 1.0, radius: radius::MD.into() },
            shadow: iced::Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.5),
                offset: iced::Vector::new(0.0, 8.0),
                blur_radius: 24.0,
            },
            ..Default::default()
        })
        .into()
}

/// The popover as a full-window layer: clicks outside it close it.
pub fn view(state: &PickerState, window: Size) -> Element<'_, Message> {
    let mut popover = column![]
        .width(Length::Fixed(WIDTH))
        .push(
            container(text(state.header.clone()).size(type_scale::XS).color(t::text_muted()))
                .padding([8, 12]),
        )
        .push(rule());
    if let Some(placeholder) = state.field.placeholder() {
        popover = popover.push(search_box(state, placeholder)).push(rule());
    }
    popover = popover.push(container(scrollable(list(state))).max_height(LIST_MAX_H));
    if state.field == Field::Project {
        popover = popover.push(rule()).push(new_project_row());
    }

    let card = container(popover).style(|_: &Theme| container::Style {
        background: Some(t::bg_card().into()),
        border: Border { color: t::border(), width: 2.0, radius: radius::MD.into() },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.5),
            offset: iced::Vector::new(0.0, 12.0),
            blur_radius: 32.0,
        },
        ..Default::default()
    });

    let at = position(state.anchor, window);
    // The inner mouse_area swallows clicks on the popover's own padding so
    // only clicks outside it reach the closing layer.
    mouse_area(
        container(mouse_area(card).on_press(Message::Noop))
            .padding(Padding { top: at.y, left: at.x, right: 0.0, bottom: 0.0 })
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .on_press(Message::CloseModal)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn label(id: &str, name: &str) -> Label {
        Label {
            id: id.into(),
            remote_id: None,
            team_id: "t".into(),
            name: name.into(),
            color: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn labels_picker(labels: &[Label], usage: &[(&str, usize)], current: &[&str]) -> PickerState {
        let usage: HashMap<String, usize> = usage.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        PickerState::new(
            "i".into(),
            "t".into(),
            Field::Labels,
            "PROJ-1 • x".into(),
            None,
            current.iter().map(|id| Choice::Label(id.to_string())).collect(),
            Source { lanes: &[], projects: &[], labels, label_usage: &usage },
        )
    }

    fn priority_picker() -> PickerState {
        PickerState::new(
            "i".into(),
            "t".into(),
            Field::Priority,
            String::new(),
            None,
            vec![Choice::Priority(2)],
            Source { lanes: &[], projects: &[], labels: &[], label_usage: &HashMap::new() },
        )
    }

    #[test]
    fn focus_starts_empty_and_wraps_both_ways() {
        let mut p = priority_picker();
        assert_eq!(p.focus, None);
        p.move_focus(-1);
        assert_eq!(p.focus, Some(4));
        p.move_focus(1);
        assert_eq!(p.focus, Some(0));
        p.focus = None;
        p.move_focus(1);
        assert_eq!(p.focus, Some(0));
    }

    #[test]
    fn labels_sort_by_use_with_top_five_frequent() {
        let ls: Vec<Label> = (0..7).map(|i| label(&format!("l{i}"), &format!("n{i}"))).collect();
        let usage = [("l6", 9), ("l5", 8), ("l4", 7), ("l3", 6), ("l2", 5), ("l1", 4)];
        let p = labels_picker(&ls, &usage, &[]);
        let ids = |v: &[PickerOption]| -> Vec<String> { v.iter().map(|o| o.label.clone()).collect() };
        assert_eq!(ids(&p.frequent), ["n6", "n5", "n4", "n3", "n2"]);
        assert_eq!(ids(&p.options)[0], "n6");
        assert_eq!(ids(&p.options)[6], "n0");
        // Unused labels are never "frequent".
        let p = labels_picker(&ls[..1], &[], &[]);
        assert!(p.frequent.is_empty());
        assert_eq!(p.lines()[0], Line::Heading("Labels"));
    }

    #[test]
    fn label_search_offers_create_unless_exact_match() {
        let ls = [label("a", "Bug"), label("b", "Bugfix")];
        let mut p = labels_picker(&ls, &[("a", 1)], &[]);
        p.set_query("bu".into());
        let items = p.items();
        assert_eq!(items[0].choice, Choice::CreateLabel("bu".into()));
        assert_eq!(items.len(), 3);
        // Frequently used hides while searching, so no duplicate rows.
        assert!(!p.lines().contains(&Line::Heading("Frequently used")));
        p.set_query("BUG".into());
        assert!(p.items().iter().all(|o| !matches!(o.choice, Choice::CreateLabel(_))));
    }

    #[test]
    fn toggling_labels_updates_checked_ids() {
        let ls = [label("a", "Bug")];
        let mut p = labels_picker(&ls, &[], &["a"]);
        assert!(p.is_current(&Choice::Label("a".into())));
        p.toggle_label("a");
        assert!(p.label_ids().is_empty());
        p.toggle_label("a");
        assert_eq!(p.label_ids(), ["a"]);
    }

    #[test]
    fn header_truncates_title_like_rails() {
        assert_eq!(header("PROJ-1", "short"), "PROJ-1 • short");
        let long = "a".repeat(40);
        assert_eq!(header("PROJ-1", &long), format!("PROJ-1 • {}...", "a".repeat(27)));
        assert_eq!(header("PROJ-1", ""), "PROJ-1");
    }

    #[test]
    fn positions_left_of_a_board_card_then_right_then_centre() {
        let win = Size::new(1200.0, 800.0);
        let card = |x: f32| Some(Anchor { rect: Rectangle::new(Point::new(x, 100.0), Size::new(280.0, 80.0)) });
        assert_eq!(position(card(600.0), win), Point::new(600.0 - WIDTH - GAP, 100.0));
        // No room on the left: to the card's right.
        assert_eq!(position(card(100.0), win), Point::new(100.0 + 280.0 + GAP, 100.0));
        // No room either side: centred.
        let narrow = Size::new(700.0, 800.0);
        assert_eq!(position(card(200.0), narrow).x, (700.0 - WIDTH) / 2.0);
    }

    #[test]
    fn list_rows_centre_horizontally_and_clamp_vertically() {
        let win = Size::new(1200.0, 800.0);
        let row = Some(Anchor { rect: Rectangle::new(Point::new(0.0, 700.0), Size::new(1000.0, 36.0)) });
        let at = position(row, win);
        assert_eq!(at.x, (1200.0 - WIDTH) / 2.0);
        assert_eq!(at.y, 800.0 - EST_HEIGHT - GAP);
    }
}
