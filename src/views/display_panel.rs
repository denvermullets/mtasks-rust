//! Display-options dropdown and filter panel, opened from the header. Both are
//! rendered as popovers anchored under the header's right edge (see `popover`).

use iced::widget::{button, checkbox, column, container, mouse_area, row, scrollable, Column};
use crate::views::text;
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Label, Lane, Member, Project};
use crate::display::{
    merged_status_options, CardProperty, CompletedFilter, DisplayOptions, FilterField, Filters, GroupBy, OrderBy,
    SubGroup, ViewMode, PRIORITY_TITLES,
};
use crate::theme as t;

const PANEL_WIDTH: f32 = 340.0;
/// Header strip height, so the popover sits just beneath it.
const HEADER_OFFSET: f32 = 44.0;

/// Anchor `panel` under the header, right-aligned, over a transparent backdrop
/// that closes it on outside click. iced 0.13 has no popover widget, so this is
/// a `stack!` layer like the modals.
pub fn popover<'a>(panel: Element<'a, Message>) -> Element<'a, Message> {
    // Swallow clicks on the panel itself so they don't reach the backdrop.
    let panel = mouse_area(panel).on_press(Message::Noop);
    let anchored = container(panel)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::End)
        .align_y(Alignment::Start)
        .padding(iced::Padding {
            top: HEADER_OFFSET,
            right: 16.0,
            bottom: 0.0,
            left: 0.0,
        });
    mouse_area(anchored).on_press(Message::CloseModal).into()
}

fn panel_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(t::bg_modal())),
        border: Border {
            color: t::border(),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

fn heading<'a>(label: &'a str) -> Element<'a, Message> {
    text(label).size(11).color(t::text_muted()).into()
}

/// A pill-style single-select: highlights `current`, emits `msg(value)`.
/// Chips flow onto as many lines as the panel width needs, and a label never
/// breaks inside its chip (JRST-455).
fn chips<'a, T: Copy + PartialEq + 'a>(
    options: &'a [(T, &'static str)],
    current: T,
    msg: impl Fn(T) -> Message,
    disabled: impl Fn(T) -> bool,
) -> Element<'a, Message> {
    let mut r = row![].spacing(4);
    for (value, label) in options {
        let active = *value == current;
        let off = disabled(*value);
        let mut b = button(text(*label).size(12).wrapping(text::Wrapping::None)).padding([3, 9]);
        if !off {
            b = b.on_press(msg(*value));
        }
        r = r.push(b.style(move |_theme: &Theme, _status| button::Style {
            background: Some(iced::Background::Color(if active {
                t::bg_card_focused()
            } else {
                t::bg_card()
            })),
            text_color: if off {
                t::text_muted()
            } else if active {
                t::accent()
            } else {
                t::text_secondary()
            },
            border: Border {
                color: if active { t::accent() } else { t::border() },
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        }));
    }
    r.wrap().into()
}

/// The "Display" dropdown: columns / rows / order / completed / properties.
/// `save_blocked` is the reason "Save view…" is unavailable, if it is.
pub fn display_view<'a>(
    opts: &'a DisplayOptions,
    save_blocked: Option<&'static str>,
) -> Element<'a, Message> {
    let mut props: Column<'a, Message> = column![].spacing(4);
    for (prop, label) in CardProperty::ALL {
        props = props.push(
            checkbox(opts.shows(prop)).label(label)
                .on_toggle(move |_| Message::ToggleCardProperty(prop))
                .size(14)
                .text_size(12),
        );
    }

    let board = opts.view_mode == ViewMode::Board;
    let body = column![]
        .spacing(12)
        .push(text("Display").size(14).color(t::text_primary()))
        .push(heading("VIEW"))
        .push(chips(
            &ViewMode::ALL,
            opts.view_mode,
            Message::SetViewMode,
            |_| false,
        ))
        // Rails' names: "Columns" on the board, "Grouping" in the list; the
        // "Rows" sub-grouping only exists on the board.
        .push(heading(if board { "COLUMNS" } else { "GROUPING" }))
        .push(chips(
            &GroupBy::ALL,
            opts.group_by,
            Message::SetGroupBy,
            |_| false,
        ))
        .push(board.then(|| heading("ROWS")))
        .push(board.then(|| {
            chips(
                &SubGroup::ALL,
                opts.sub_group,
                Message::SetSubGroup,
                |s| opts.sub_group_conflicts(s),
            )
        }))
        .push(heading("ORDER BY"))
        .push(chips(
            &OrderBy::ALL,
            opts.order_by,
            Message::SetOrderBy,
            |_| false,
        ))
        .push(heading("COMPLETED ISSUES"))
        .push(chips(
            &CompletedFilter::ALL,
            opts.completed_filter(),
            Message::SetCompletedFilter,
            |_| false,
        ))
        .push(heading("OPTIONS"))
        .push(
            checkbox(opts.show_sub_issues).label("Show sub-issues")
                .on_toggle(|_| Message::ToggleShowSubIssues)
                .size(14)
                .text_size(12),
        )
        .push(heading("BOARD OPTIONS"))
        .push(
            checkbox(opts.show_empty_groups).label("Show empty columns")
                .on_toggle(|_| Message::ToggleShowEmptyGroups)
                .size(14)
                .text_size(12),
        )
        .push(
            checkbox(opts.show_empty_rows).label("Show empty rows")
                .on_toggle(|_| Message::ToggleShowEmptyRows)
                .size(14)
                .text_size(12),
        )
        .push(board.then(|| {
            checkbox(opts.show_dependencies).label("Dependencies")
                .on_toggle(|_| Message::ToggleShowDependencies)
                .size(14)
                .text_size(12)
        }))
        .push(heading("CARD PROPERTIES"))
        .push(props)
        .push(heading("SAVED VIEW"))
        .push(
            button(text("Save view\u{2026}").size(12))
                .on_press_maybe(save_blocked.is_none().then_some(Message::OpenSaveView))
                .padding([5, 12])
                .style(button::secondary),
        );
    let body = match save_blocked {
        Some(reason) => body.push(text(reason).size(11).color(t::text_muted())),
        None => body,
    };

    container(body)
        .padding(16)
        .width(Length::Fixed(PANEL_WIDTH))
        .style(panel_style)
        .into()
}

/// Reference data the filter panel lists options from.
pub struct FilterData<'a> {
    pub lanes: &'a [Lane],
    pub members: &'a [Member],
    pub labels: &'a [Label],
    pub projects: &'a [Project],
    /// My Issues: assignee is fixed to the current user (section hidden) and
    /// same-named lanes from different teams collapse into one status option.
    pub cross_team: bool,
    /// Project detail is already scoped to one project, so the project
    /// section is hidden and not counted.
    pub scoped_to_project: bool,
    /// False while a Saved view is open: Rails can't store "unassigned".
    pub offer_unassigned: bool,
}

fn option_group<'a>(
    title: &'a str,
    field: FilterField,
    filters: &'a Filters,
    options: Vec<(String, String)>,
) -> Element<'a, Message> {
    let mut col: Column<'a, Message> = column![].spacing(4).push(heading(title));
    if options.is_empty() {
        col = col.push(text("None available").size(12).color(t::text_muted()));
    }
    for (value, label) in options {
        let selected = filters.is_selected(field, &value);
        col = col.push(
            checkbox(selected).label(label)
                .on_toggle(move |_| Message::ToggleFilter(field, value.clone()))
                .size(14)
                .text_size(12),
        );
    }
    col.into()
}

/// The filter panel: free-text search (shared with the header box) plus
/// assignee / priority / label / status / project multi-selects.
pub fn filter_view<'a>(
    filters: &'a Filters,
    search: &'a str,
    data: FilterData<'a>,
) -> Element<'a, Message> {
    let hidden = if data.scoped_to_project {
        filters.projects.len()
    } else {
        0
    };
    let count = filters.count() - hidden + usize::from(!crate::search_match::needle(search).is_empty());

    let mut assignees: Vec<(String, String)> = data
        .members
        .iter()
        .map(|m| (m.id.clone(), m.name.clone()))
        .collect();
    assignees.sort_by_key(|(_, n)| n.to_lowercase());
    if data.offer_unassigned {
        assignees.push((String::new(), "Unassigned".to_string()));
    }

    let priorities = PRIORITY_TITLES
        .iter()
        .enumerate()
        .map(|(i, n)| (i.to_string(), n.to_string()))
        .collect();
    let labels = data
        .labels
        .iter()
        .map(|l| (l.id.clone(), l.name.clone()))
        .collect();
    let statuses = if data.cross_team {
        merged_status_options(data.lanes)
    } else {
        data.lanes
            .iter()
            .map(|l| (l.id.clone(), l.name.clone()))
            .collect()
    };
    let mut projects: Vec<(String, String)> = data
        .projects
        .iter()
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect();
    projects.sort_by_key(|(_, n)| n.to_lowercase());
    projects.push((String::new(), "No project".to_string()));

    let mut clear = button(text("Clear all").size(12)).padding([3, 9]);
    if count > 0 {
        clear = clear.on_press(Message::ClearFilters);
    }

    let top = row![]
        .align_y(Alignment::Center)
        .push(text("Filters").size(14).color(t::text_primary()))
        .push(iced::widget::space::horizontal())
        .push(clear.style(button::secondary));

    let mut groups = column![].spacing(14);
    if !data.cross_team {
        groups = groups.push(option_group(
            "ASSIGNEE",
            FilterField::Assignee,
            filters,
            assignees,
        ));
    }
    let groups = groups
        .push(option_group(
            "PRIORITY",
            FilterField::Priority,
            filters,
            priorities,
        ))
        .push(option_group("LABEL", FilterField::Label, filters, labels))
        .push(option_group(
            "STATUS",
            FilterField::Status,
            filters,
            statuses,
        ))
        ;
    let groups = if data.scoped_to_project {
        groups
    } else {
        groups.push(option_group(
            "PROJECT",
            FilterField::Project,
            filters,
            projects,
        ))
    };

    let body = column![]
        .spacing(12)
        .push(top)
        .push(
            crate::undo_input::text_input("display-search", "Search…", search, Message::HeaderSearchChanged)
                .padding([5, 10])
                .size(13),
        )
        .push(scrollable(container(groups).padding([0, 12]).width(Length::Fill)).height(420));

    container(body)
        .padding(16)
        .width(Length::Fixed(PANEL_WIDTH))
        .style(panel_style)
        .into()
}
