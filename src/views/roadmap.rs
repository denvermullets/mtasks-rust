use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::widget::{button, column, container, mouse_area, row, scrollable, stack, svg, Column};
use crate::views::text;
use iced::mouse::Interaction;
use iced::{Alignment, Border, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use crate::app::Message;
use crate::scroll::{track, ScrollKey};
use crate::db::models::{Project, ProjectStatus, RoadmapCommitment, Team};
use crate::theme::{self as t, radius, space};
use crate::views::dropdown::dropdown;
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

/// Web `w-72` commitment picker.
const MENU_W: f32 = 288.0;

/// Pointer travel (px) before a pressed card counts as a drag, not a click.
const DRAG_THRESHOLD: f32 = 4.0;

const ICON_KEBAB: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="#000" viewBox="0 0 20 20"><path d="M10 6a2 2 0 110-4 2 2 0 010 4zM10 12a2 2 0 110-4 2 2 0 010 4zM10 18a2 2 0 110-4 2 2 0 010 4z"/></svg>"##;

/// Transient Roadmap interaction state owned by the app: which card's ⋮
/// commitment menu is open, and the card being dragged between lanes.
#[derive(Debug, Clone, Default)]
pub struct RoadmapUi {
    /// Local project UUID whose commitment menu is open.
    pub menu: Option<String>,
    pub drag: Option<RoadmapDrag>,
}

/// A card pressed on the Roadmap. It stays a click until the pointer travels
/// past `DRAG_THRESHOLD`; after that, releasing over another lane moves it there.
#[derive(Debug, Clone, PartialEq)]
pub struct RoadmapDrag {
    pub project_id: String,
    /// The lane the card started in.
    pub from: RoadmapCommitment,
    /// First pointer position seen after the press (the press itself carries none).
    start: Option<Point>,
    pub moved: bool,
    /// The lane under the pointer, if any.
    pub over: Option<RoadmapCommitment>,
}

/// What releasing a pressed card means.
#[derive(Debug, Clone, PartialEq)]
pub enum DragOutcome {
    /// The pointer never left the threshold: open the project.
    Click(String),
    /// Dropped on a different lane: move the project there.
    Move(String, RoadmapCommitment),
    /// Dropped on its own lane or outside every lane.
    Cancel,
}

impl RoadmapDrag {
    pub fn new(project_id: String, from: RoadmapCommitment) -> Self {
        Self { project_id, from, start: None, moved: false, over: None }
    }

    pub fn move_to(&mut self, p: Point) {
        let start = *self.start.get_or_insert(p);
        if !self.moved && p.distance(start) >= DRAG_THRESHOLD {
            self.moved = true;
        }
    }

    pub fn enter(&mut self, lane: RoadmapCommitment) {
        self.over = Some(lane);
    }

    /// Leaving `lane` only clears the hover if it is still the current one:
    /// moving up a lane delivers the upper lane's enter before the lower's exit.
    pub fn leave(&mut self, lane: RoadmapCommitment) {
        if self.over == Some(lane) {
            self.over = None;
        }
    }

    /// The lane a release would move the card to, if any.
    pub fn target(&self) -> Option<RoadmapCommitment> {
        self.over.filter(|lane| self.moved && *lane != self.from)
    }

    pub fn outcome(&self) -> DragOutcome {
        if !self.moved {
            return DragOutcome::Click(self.project_id.clone());
        }
        match self.target() {
            Some(lane) => DragOutcome::Move(self.project_id.clone(), lane),
            None => DragOutcome::Cancel,
        }
    }
}

/// Full-pane Roadmap view: three vertical swimlanes (Now / Next / Later) of
/// project cards grouped by `roadmap_commitment`, with header status counts and
/// per-lane "Add project" buttons. `projects` is the selected team's full list;
/// off-roadmap projects (commitment `None`) simply don't appear in any lane, and
/// completed projects are hidden entirely (Rails scopes to `not_completed`).
/// `needle` (from the header search) narrows the lanes; the header counts
/// still reflect the whole roadmap.
pub fn view<'a>(
    projects: &'a [Project],
    team: Option<&'a Team>,
    needle: &str,
    ui: &'a RoadmapUi,
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
        // The card in flight, when this lane is where it would drop.
        let incoming = ui
            .drag
            .as_ref()
            .filter(|d| d.target() == Some(commitment))
            .and_then(|d| projects.iter().find(|p| p.id == d.project_id));
        lanes = lanes.push(lane_block(
            commitment, label, subtitle, dot, &in_lane, incoming, identifier, ui,
        ));
    }

    let body = column![]
        .spacing(0)
        .width(Length::Fill)
        .push(container(header).padding([12, 16]))
        .push(divider())
        .push(
            track(scrollable(container(lanes).padding(16)), ScrollKey::Roadmap)
                .height(Length::Fill),
        );

    let pane = container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: t::repaint(t::bg_primary()),
            ..Default::default()
        });

    // Always wrapped (so starting a drag doesn't reshape the widget tree and
    // reset the scroll); only grabs the cursor while a card is in flight.
    let pane = mouse_area(pane);
    if ui.drag.as_ref().is_some_and(|d| d.moved) {
        pane.interaction(Interaction::Grabbing).into()
    } else {
        pane.into()
    }
}

/// A single swimlane: fixed header column on the left, wrapped card grid (plus a
/// trailing "Add project" cell) on the right, boxed in a foreground panel.
fn lane_block<'a>(
    commitment: RoadmapCommitment,
    label: &'a str,
    subtitle: &'a str,
    dot: Color,
    projects: &[&'a Project],
    incoming: Option<&'a Project>,
    identifier: &'a str,
    ui: &'a RoadmapUi,
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
        .map(|p| card(p, identifier, ui))
        .collect();
    // Where the dragged card will land: lanes sort by due date, so its slot is
    // known before the drop.
    if let Some(project) = incoming {
        let at = projects.partition_point(|p| {
            cmp_due_nulls_last(&p.due_date, &project.due_date) != std::cmp::Ordering::Greater
        });
        cells.insert(at, drop_placeholder(project));
    }
    cells.push(add_to_lane_cell(commitment));

    let block = row![]
        .spacing(16)
        .push(header_col)
        .push(container(grid(cells)).width(Length::Fill));

    let drop_target = ui.drag.as_ref().and_then(RoadmapDrag::target) == Some(commitment);
    let panel = container(block)
        .padding(16)
        .width(Length::Fill)
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_secondary())),
            border: Border {
                color: if drop_target { t::accent() } else { t::border() },
                width: 1.0,
                radius: 12.0.into(),
            },
            ..Default::default()
        });

    // Hover tracking only while a card is pressed; the wrapper is always there
    // so its hover state is current the moment a drag starts.
    let area = mouse_area(panel);
    if ui.drag.is_some() {
        area.on_enter(Message::RoadmapDragEnter(commitment))
            .on_exit(Message::RoadmapDragLeave(commitment))
            .into()
    } else {
        area.into()
    }
}

/// A project card. Clicking opens ProjectDetail; pressing and dragging moves
/// it to another lane; the ⋮ menu changes (or removes) its commitment.
fn card<'a>(project: &'a Project, identifier: &'a str, ui: &'a RoadmapUi) -> Element<'a, Message> {
    let (status_label, status_color) = card_status(project);
    let dragging = ui
        .drag
        .as_ref()
        .is_some_and(|d| d.moved && d.project_id == project.id);

    let top = row![]
        .spacing(6)
        .align_y(Alignment::Center)
        .push(text("\u{25CF}").size(8).color(status_color))
        .push(text(status_label).size(10).color(status_color))
        .push(iced::widget::horizontal_space())
        .push(commitment_menu(project, identifier, ui.menu.as_deref() == Some(project.id.as_str())));

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
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(if dragging {
                t::bg_card_focused()
            } else {
                t::bg_primary()
            })),
            border: Border {
                color: if dragging { Color::TRANSPARENT } else { t::border() },
                width: 1.0,
                radius: CARD_RADIUS.into(),
            },
            ..Default::default()
        });

    // The card being dragged keeps its slot but gets a dashed outline.
    let card: Element<'a, Message> = if dragging {
        stack![card, dashed_outline(t::accent())].into()
    } else {
        card.into()
    };
    let area = mouse_area(card).interaction(Interaction::Pointer);
    match project.roadmap_commitment {
        Some(from) => area
            .on_press(Message::RoadmapDragPress(project.id.clone(), from))
            .into(),
        None => area
            .on_press(Message::OpenProjectDetail(project.id.clone()))
            .into(),
    }
}

/// Corner radius of a card and of the drag outlines drawn over one.
const CARD_RADIUS: f32 = 8.0;

/// Height of the drop placeholder (web card `min-h-[98px]`).
const PLACEHOLDER_H: f32 = 98.0;

/// A dashed rounded outline filling its bounds.
struct DashedOutline(Color);

impl canvas::Program<Message> for DashedOutline {
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
            CARD_RADIUS.into(),
        );
        let mut stroke = Stroke::default().with_color(self.0).with_width(1.0);
        stroke.line_dash = canvas::LineDash { segments: &[5.0, 4.0], offset: 0 };
        frame.stroke(&outline, stroke);
        vec![frame.into_geometry()]
    }
}

fn dashed_outline<'a>(color: Color) -> Element<'a, Message> {
    iced::widget::canvas(DashedOutline(color))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The dashed slot shown in the lane a dragged card will drop into.
fn drop_placeholder<'a>(project: &'a Project) -> Element<'a, Message> {
    let label = container(
        text(&project.name)
            .size(13)
            .color(t::text_muted())
            .wrapping(text::Wrapping::WordOrGlyph),
    )
    .padding(14)
    .center_y(Length::Fill)
    .width(Length::Fill);
    stack![label, dashed_outline(t::accent())]
        .width(Length::Fill)
        .height(Length::Fixed(PLACEHOLDER_H))
        .into()
}

/// The card's ⋮ trigger plus its commitment menu (web `_commitment_picker`):
/// Now / Next / Later radios, then "Remove from roadmap".
fn commitment_menu<'a>(project: &'a Project, identifier: &'a str, open: bool) -> Element<'a, Message> {
    let anchor = button(
        svg(svg::Handle::from_memory(ICON_KEBAB.as_bytes()))
            .width(Length::Fixed(14.0))
            .height(Length::Fixed(14.0))
            .style(|_theme: &Theme, _status| svg::Style { color: Some(t::text_secondary()) }),
    )
    .padding(space::S0_5)
    .style(move |_theme: &Theme, status| button::Style {
        background: (open || matches!(status, button::Status::Hovered))
            .then(|| iced::Background::Color(t::bg_card_focused())),
        border: Border { radius: radius::SM.into(), ..Default::default() },
        ..Default::default()
    })
    .on_press(Message::ToggleRoadmapMenu(project.id.clone()));

    let menu = open.then(|| {
        let current = project.roadmap_commitment;
        let name: String = if project.name.chars().count() > 30 {
            project.name.chars().take(27).chain("...".chars()).collect()
        } else {
            project.name.clone()
        };
        let mut items: Vec<Element<'a, Message>> = vec![
            container(
                text(format!("{identifier} \u{2022} {name}"))
                    .size(12)
                    .color(t::text_muted()),
            )
            .padding([space::S2, space::S3])
            .into(),
            divider(),
        ];
        let options = [
            (RoadmapCommitment::Now, "Now", "shipping soon", t::accent()),
            (RoadmapCommitment::Next, "Next", "up after Now", t::highlight()),
            (RoadmapCommitment::Later, "Later", "on the radar", t::text_muted()),
        ];
        for (commitment, label, subtitle, dot) in options {
            items.push(menu_option(
                &project.id,
                Some(commitment),
                current == Some(commitment),
                dot,
                label.to_string(),
                Some(subtitle),
            ));
        }
        items.push(divider());
        items.push(menu_option(
            &project.id,
            None,
            current.is_none(),
            t::text_muted(),
            "Remove from roadmap".to_string(),
            None,
        ));

        container(Column::with_children(items).width(Length::Fill).padding([space::S1, 0.0]))
            .width(Length::Fill)
            .style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::bg_secondary())),
                border: Border { color: t::border(), width: 1.0, radius: radius::MD.into() },
                shadow: iced::Shadow {
                    color: Color::from_rgba(0.0, 0.0, 0.0, 0.4),
                    offset: iced::Vector::new(0.0, space::S1),
                    blur_radius: space::S3,
                },
                ..Default::default()
            })
            .into()
    });

    dropdown(anchor, menu, MENU_W, Message::CloseRoadmapMenu).into()
}

/// One radio row in the commitment menu.
fn menu_option<'a>(
    project_id: &str,
    commitment: Option<RoadmapCommitment>,
    checked: bool,
    dot: Color,
    label: String,
    subtitle: Option<&'static str>,
) -> Element<'a, Message> {
    let radio = text(if checked { "\u{25C9}" } else { "\u{25CB}" })
        .size(14)
        .color(if checked { t::accent() } else { t::text_muted() });
    let mut line = row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(radio)
        .push(text("\u{25CF}").size(7).color(dot))
        .push(text(label).size(14).color(t::text_secondary()));
    if let Some(sub) = subtitle {
        line = line
            .push(iced::widget::horizontal_space())
            .push(text(sub).size(12).color(t::text_muted()));
    }
    button(line)
        .on_press(Message::SetRoadmapCommitment(project_id.to_string(), commitment))
        .padding([space::S2, space::S3])
        .width(Length::Fill)
        .style(|_theme: &Theme, status: button::Status| button::Style {
            background: matches!(status, button::Status::Hovered)
                .then_some(iced::Background::Color(t::bg_card_focused())),
            text_color: t::text_primary(),
            ..Default::default()
        })
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

    crate::views::util::modal_backdrop(modal)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pressed() -> RoadmapDrag {
        RoadmapDrag::new("p1".into(), RoadmapCommitment::Now)
    }

    #[test]
    fn a_press_without_travel_is_a_click() {
        let mut d = pressed();
        d.move_to(Point::new(10.0, 10.0));
        d.move_to(Point::new(12.0, 11.0));
        d.enter(RoadmapCommitment::Next);
        assert_eq!(d.outcome(), DragOutcome::Click("p1".into()));
    }

    #[test]
    fn dropping_on_another_lane_moves_the_project() {
        let mut d = pressed();
        d.move_to(Point::new(10.0, 10.0));
        d.move_to(Point::new(10.0, 60.0));
        d.enter(RoadmapCommitment::Later);
        assert_eq!(d.target(), Some(RoadmapCommitment::Later));
        assert_eq!(d.outcome(), DragOutcome::Move("p1".into(), RoadmapCommitment::Later));
    }

    #[test]
    fn dropping_on_its_own_lane_or_outside_cancels() {
        let mut d = pressed();
        d.move_to(Point::new(0.0, 0.0));
        d.move_to(Point::new(0.0, 50.0));
        d.enter(RoadmapCommitment::Now);
        assert_eq!(d.outcome(), DragOutcome::Cancel);
        d.leave(RoadmapCommitment::Now);
        assert_eq!(d.outcome(), DragOutcome::Cancel);
    }

    #[test]
    fn a_stale_exit_does_not_clear_the_new_lane() {
        // Moving up: the upper lane's enter arrives before the lower lane's exit.
        let mut d = pressed();
        d.move_to(Point::new(0.0, 0.0));
        d.move_to(Point::new(0.0, 50.0));
        d.enter(RoadmapCommitment::Later);
        d.enter(RoadmapCommitment::Next);
        d.leave(RoadmapCommitment::Later);
        assert_eq!(d.outcome(), DragOutcome::Move("p1".into(), RoadmapCommitment::Next));
    }
}
