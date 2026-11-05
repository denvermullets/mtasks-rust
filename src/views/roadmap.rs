use iced::widget::{button, column, container, mouse_area, row, scrollable, text};
use iced::{Alignment, Border, Color, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Project, ProjectStatus, RoadmapCommitment, Team};
use crate::theme as t;
use crate::views::header;
use crate::views::projects::format_target_date;

/// The three swimlanes, in display order, with their header copy + dot color.
fn lane_defs() -> [(RoadmapCommitment, &'static str, &'static str, Color); 3] {
    [
        (
            RoadmapCommitment::Now,
            "Now",
            "Shipping in the next ~6 weeks",
            t::accent(),
        ),
        (
            RoadmapCommitment::Next,
            "Next",
            "Starting once Now slots open",
            t::highlight(),
        ),
        (
            RoadmapCommitment::Later,
            "Later",
            "On the radar, not committed",
            t::text_muted(),
        ),
    ]
}

/// Cards per grid row within a lane body.
const PER_ROW: usize = 3;

/// Full-pane Roadmap view: three vertical swimlanes (Now / Next / Later) of
/// project cards grouped by `roadmap_commitment`, with header status counts and
/// per-lane "Add project" buttons. `projects` is the selected team's full list;
/// off-roadmap projects (commitment `None`) simply don't appear in any lane, and
/// completed projects are hidden entirely (Rails scopes to `not_completed`).
/// `needle` (from the header search) narrows the lanes; the header counts
/// still reflect the whole roadmap.
/// Key of the roadmap in `App::scroll_state` (distinct from the lane indices
/// and the other views' keys).
pub const SCROLL_KEY: usize = usize::MAX - 4;

/// Scrollable id of the roadmap.
pub fn scroll_id() -> scrollable::Id {
    scrollable::Id::new("roadmap")
}

pub fn view<'a>(
    projects: &'a [Project],
    team: Option<&'a Team>,
    needle: &str,
) -> Element<'a, Message> {
    let identifier = team.map(|t| t.identifier.as_str()).unwrap_or("");

    // Projects on the roadmap drive the header counts (mirrors the Rails
    // controller, which flattens the three lanes' values).
    let on_roadmap: Vec<&Project> = roadmap_scope(projects)
        .filter(|p| p.roadmap_commitment.is_some())
        .collect();
    let behind = on_roadmap.iter().filter(|p| p.behind_schedule).count();
    let in_progress = on_roadmap
        .iter()
        .filter(|p| !p.behind_schedule && p.status == Some(ProjectStatus::Started))
        .count();
    let planned = on_roadmap
        .iter()
        .filter(|p| {
            !p.behind_schedule
                && !matches!(
                    p.status,
                    Some(
                        ProjectStatus::Started
                            | ProjectStatus::Completed
                            | ProjectStatus::Cancelled
                    )
                )
        })
        .count();

    let mut chips = row![].spacing(6).align_y(Alignment::Center);
    if behind > 0 {
        chips = chips.push(count_chip(behind, "behind", t::danger()));
    }
    if in_progress > 0 {
        chips = chips.push(count_chip(in_progress, "in progress", t::warning()));
    }
    chips = chips.push(count_chip(planned, "planned", t::text_muted()));

    let header = row![]
        .spacing(12)
        .align_y(Alignment::Center)
        .push(
            column![]
                .spacing(2)
                .push(text("Now / Next / Later").size(16).color(t::text_primary()))
                .push(
                    text("No specific dates — just intent.")
                        .size(12)
                        .color(t::text_muted()),
                ),
        )
        .push(iced::widget::horizontal_space())
        .push(chips)
        .push(add_project_button(RoadmapCommitment::Now));

    // One lane block per commitment.
    let mut lanes = column![].spacing(12).width(Length::Fill);
    for (commitment, label, subtitle, dot) in lane_defs() {
        let mut in_lane: Vec<&Project> = roadmap_scope(projects)
            .filter(|p| p.roadmap_commitment == Some(commitment))
            .filter(|p| header::project_matches(p, needle))
            .collect();
        // due_date ASC, NULLS LAST (mirrors Project.in_commitment).
        in_lane.sort_by(|a, b| cmp_due_nulls_last(&a.due_date, &b.due_date));
        lanes = lanes.push(lane_block(commitment, label, subtitle, dot, &in_lane, identifier));
    }

    let body = column![]
        .spacing(0)
        .width(Length::Fill)
        .push(container(header).padding([12, 16]))
        .push(divider())
        .push(
            scrollable(container(lanes).padding(16))
                .id(scroll_id())
                .on_scroll(|vp| {
                    Message::Scrolled(SCROLL_KEY, vp.absolute_offset().y, vp.bounds().height)
                })
                .height(Length::Fill),
        );

    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_primary())),
            ..Default::default()
        })
        .into()
}

/// A single swimlane: fixed header column on the left, wrapped card grid (plus a
/// trailing "Add project" cell) on the right, boxed in a foreground panel.
fn lane_block<'a>(
    commitment: RoadmapCommitment,
    label: &'a str,
    subtitle: &'a str,
    dot: Color,
    projects: &[&'a Project],
    identifier: &'a str,
) -> Element<'a, Message> {
    let header_col = column![]
        .spacing(6)
        .width(Length::Fixed(160.0))
        .push(
            row![]
                .spacing(6)
                .align_y(Alignment::Center)
                .push(text("\u{25CF}").size(9).color(dot))
                .push(text(label).size(14).color(dot))
                .push(count_pill(projects.len())),
        )
        .push(text(subtitle).size(11).color(t::text_muted()));

    let mut cells: Vec<Element<'a, Message>> = projects
        .iter()
        .map(|p| card(p, identifier))
        .collect();
    cells.push(add_to_lane_cell(commitment));

    let block = row![]
        .spacing(16)
        .push(header_col)
        .push(container(grid(cells)).width(Length::Fill));

    container(block)
        .padding(16)
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_secondary())),
            border: Border {
                color: t::border(),
                width: 1.0,
                radius: 12.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// A read-only project card. The whole card is clickable → ProjectDetail.
fn card<'a>(project: &'a Project, identifier: &'a str) -> Element<'a, Message> {
    let (status_label, status_color) = card_status(project);

    let top = row![]
        .spacing(6)
        .align_y(Alignment::Center)
        .push(text("\u{25CF}").size(8).color(status_color))
        .push(text(status_label).size(10).color(status_color));

    let due: Element<'a, Message> = match project.due_date.as_deref() {
        Some(d) if !d.is_empty() => text(format!("Due {}", format_target_date(d)))
            .size(11)
            .color(if project.behind_schedule {
                t::danger()
            } else {
                t::text_muted()
            })
            .into(),
        _ => iced::widget::horizontal_space().into(),
    };

    let ident_pill = container(text(identifier).size(10).color(t::text_secondary()))
        .padding([2, 6])
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_primary())),
            border: Border {
                color: t::border(),
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        });

    let bottom = row![]
        .align_y(Alignment::Center)
        .push(container(due).width(Length::Fill))
        .push(ident_pill);

    let mut inner = column![]
        .spacing(8)
        .push(top)
        .push(
            text(&project.name)
                .size(13)
                .color(t::text_primary())
                .wrapping(text::Wrapping::WordOrGlyph),
        )
        .push(bottom);

    if project.total_issues_count > 0 {
        inner = inner.push(progress_bar(project));
    }

    let card = container(inner)
        .padding(14)
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_primary())),
            border: Border {
                color: t::border(),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });

    mouse_area(card)
        .on_press(Message::OpenProjectDetail(project.id.clone()))
        .into()
}

/// Status pill label + color, mirroring the Rails `roadmaps/_card` logic.
fn card_status(project: &Project) -> (&'static str, Color) {
    if project.behind_schedule {
        return ("Behind", t::danger());
    }
    match project.status {
        Some(ProjectStatus::Completed) => ("Completed", t::priority_low()),
        Some(ProjectStatus::Cancelled) => ("Cancelled", t::text_muted()),
        Some(ProjectStatus::Paused) => ("Paused", t::warning()),
        Some(ProjectStatus::Started) => {
            if project.progress_percentage > 0 {
                ("In progress", t::priority_low())
            } else {
                ("Started", t::warning())
            }
        }
        Some(ProjectStatus::Backlog) | None => ("Planned", t::text_muted()),
    }
}

/// Thin 2px progress bar at the card's foot, colored per Rails.
fn progress_bar<'a>(project: &Project) -> Element<'a, Message> {
    let pct = project.progress_percentage.clamp(0, 100);
    let bar_color = if project.behind_schedule {
        t::danger()
    } else if project.status == Some(ProjectStatus::Started) && pct < 50 {
        t::warning()
    } else {
        t::priority_low()
    };
    let filled = pct.max(2) as u16;
    let empty = (100 - pct).clamp(0, 100) as u16;

    row![]
        .height(Length::Fixed(2.0))
        .push(
            container(text(""))
                .width(Length::FillPortion(filled))
                .height(Length::Fixed(2.0))
                .style(move |_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(bar_color)),
                    ..Default::default()
                }),
        )
        .push(
            container(text(""))
                .width(Length::FillPortion(empty.max(1)))
                .height(Length::Fixed(2.0)),
        )
        .into()
}

/// The trailing dashed "Add project" cell rendered at the end of each lane grid.
fn add_to_lane_cell<'a>(commitment: RoadmapCommitment) -> Element<'a, Message> {
    button(
        container(
            text("+ Add project")
                .size(12)
                .color(t::text_secondary()),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill),
    )
    .on_press(Message::OpenRoadmapAdd(commitment))
    .width(Length::Fill)
    .height(Length::Fixed(96.0))
    .style(|_theme: &Theme, _status| button::Style {
        background: None,
        text_color: t::text_secondary(),
        border: Border {
            color: t::border(),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    })
    .into()
}

/// The header's "+ Add project" button (opens the picker preset to `commitment`).
fn add_project_button<'a>(commitment: RoadmapCommitment) -> Element<'a, Message> {
    button(text("+ Add project").size(13).color(t::accent()))
        .on_press(Message::OpenRoadmapAdd(commitment))
        .padding([6, 12])
        .style(|_theme: &Theme, _status| button::Style {
            background: None,
            text_color: t::accent(),
            border: Border {
                color: t::accent(),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// A "N label" status chip with a leading colored dot.
fn count_chip<'a>(n: usize, label: &'a str, dot: Color) -> Element<'a, Message> {
    container(
        row![]
            .spacing(5)
            .align_y(Alignment::Center)
            .push(text("\u{25CF}").size(7).color(dot))
            .push(
                text(format!("{n} {label}"))
                    .size(11)
                    .color(t::text_secondary()),
            ),
    )
    .padding([2, 6])
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_secondary())),
        border: Border {
            color: t::border(),
            width: 1.0,
            radius: 4.0.into(),
        },
        ..Default::default()
    })
    .into()
}

/// The small count badge next to a lane label.
fn count_pill<'a>(n: usize) -> Element<'a, Message> {
    container(text(n.to_string()).size(11).color(t::text_secondary()))
        .padding([1, 6])
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_primary())),
            border: Border {
                color: t::border(),
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// Wrap `cells` into rows of `PER_ROW`, padding short rows with equal-width
/// spacers so card widths stay consistent across rows.
fn grid<'a>(cells: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut col = column![].spacing(8).width(Length::Fill);
    let mut current = row![].spacing(8).width(Length::Fill);
    let mut count = 0;
    for cell in cells {
        current = current.push(container(cell).width(Length::FillPortion(1)));
        count += 1;
        if count == PER_ROW {
            col = col.push(current);
            current = row![].spacing(8).width(Length::Fill);
            count = 0;
        }
    }
    if count > 0 {
        for _ in count..PER_ROW {
            current = current.push(
                container(text(""))
                    .width(Length::FillPortion(1)),
            );
        }
        col = col.push(current);
    }
    col.into()
}

fn divider<'a>() -> Element<'a, Message> {
    container(iced::widget::horizontal_rule(1))
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::border())),
            ..Default::default()
        })
        .into()
}

/// Projects eligible for the roadmap at all: everything but completed ones
/// (mirrors `current_team.projects.not_completed` in the Rails controller).
fn roadmap_scope(projects: &[Project]) -> impl Iterator<Item = &Project> {
    projects
        .iter()
        .filter(|p| p.status != Some(ProjectStatus::Completed))
}

/// Compare two optional due dates for ascending sort with NULLs last.
fn cmp_due_nulls_last(a: &Option<String>, b: &Option<String>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (
        a.as_deref().filter(|s| !s.is_empty()),
        b.as_deref().filter(|s| !s.is_empty()),
    ) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

// ── "Add project to roadmap" modal ──

/// The picker modal: a lane-chip selector at the top, then a list of off-roadmap
/// projects. Picking a project assigns it to `selected_lane` and closes.
pub fn add_modal_view<'a>(
    selected_lane: RoadmapCommitment,
    projects: &'a [Project],
    team: Option<&'a Team>,
) -> Element<'a, Message> {
    let identifier = team.map(|t| t.identifier.as_str()).unwrap_or("");

    let mut off_roadmap: Vec<&Project> = roadmap_scope(projects)
        .filter(|p| p.roadmap_commitment.is_none())
        .collect();
    off_roadmap.sort_by_key(|p| p.name.to_lowercase());

    // Lane chip selector.
    let mut chip_row = row![].spacing(6).align_y(Alignment::Center);
    for (commitment, label, _subtitle, dot) in lane_defs() {
        let active = commitment == selected_lane;
        chip_row = chip_row.push(
            button(
                row![]
                    .spacing(5)
                    .align_y(Alignment::Center)
                    .push(text("\u{25CF}").size(7).color(dot))
                    .push(text(label).size(12).color(if active {
                        t::text_primary()
                    } else {
                        t::text_secondary()
                    })),
            )
            .on_press(Message::RoadmapAddLaneChanged(commitment))
            .padding([4, 10])
            .style(move |_theme: &Theme, _status| button::Style {
                background: active.then_some(iced::Background::Color(t::bg_card_focused())),
                text_color: t::text_primary(),
                border: Border {
                    color: if active { t::accent() } else { t::border() },
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }),
        );
    }

    // Off-roadmap project list.
    let list: Element<'a, Message> = if off_roadmap.is_empty() {
        container(
            text("No projects off the roadmap.")
                .size(12)
                .color(t::text_muted()),
        )
        .center_x(Length::Fill)
        .padding(24)
        .into()
    } else {
        let mut items = column![].spacing(0).width(Length::Fill);
        for p in off_roadmap {
            items = items.push(
                button(
                    row![]
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(text(identifier).size(10).color(t::text_muted()))
                        .push(text(&p.name).size(13).color(t::text_primary())),
                )
                .on_press(Message::RoadmapAssignProject(p.id.clone()))
                .padding([8, 12])
                .width(Length::Fill)
                .style(|_theme: &Theme, status: button::Status| button::Style {
                    background: matches!(status, button::Status::Hovered)
                        .then_some(iced::Background::Color(t::bg_card_focused())),
                    text_color: t::text_primary(),
                    border: Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            );
        }
        scrollable(items).height(Length::Fixed(280.0)).into()
    };

    let content = column![]
        .spacing(14)
        .width(Length::Fixed(420.0))
        .push(
            text("Add project to roadmap")
                .size(16)
                .color(t::text_primary()),
        )
        .push(
            column![]
                .spacing(6)
                .push(text("Add to lane").size(11).color(t::text_muted()))
                .push(chip_row),
        )
        .push(divider())
        .push(list)
        .push(
            row![]
                .push(iced::widget::horizontal_space())
                .push(
                    button(text("Close").size(14))
                        .on_press(Message::CloseModal)
                        .padding([8, 20])
                        .style(button::secondary),
                ),
        );

    let modal = container(content)
        .padding(24)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_modal())),
            border: Border {
                color: t::border(),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });

    container(modal)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.7,
            ))),
            ..Default::default()
        })
        .into()
}
