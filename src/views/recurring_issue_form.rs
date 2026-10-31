//! "New recurring issue" modal (JRST-172).
//!
//! Extension points for the follow-up form tickets:
//! - State: add a field to [`RecurringFormState`] (and its default in `new`).
//! - Message: add a `RecurringForm*Changed` variant in `app.rs`, handled next
//!   to the others (they just assign the field on `App::recurring_form`).
//! - Validation: add a field to [`FormErrors`] and a check in [`validate`]; an
//!   extra value that reaches the db goes on [`NewRecurringIssue`].
//! - View: add a widget (with its inline error) to `view`.
//!
//! `validate` is pure and mirrors the server's rules, so it is unit tested
//! without the UI.

use chrono::{Datelike, NaiveDate};
use iced::widget::{button, column, container, pick_list, row, text, text_editor, text_input};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Label, Lane, Member, Priority, Project};
use crate::db::recurring_issues::{NewRecurringIssue, FIRST_LANE};
use crate::theme as t;
use crate::views::labels as labels_view;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
}

impl Frequency {
    /// The value stored locally and sent to Rails.
    pub fn as_str(self) -> &'static str {
        match self {
            Frequency::Daily => "daily",
            Frequency::Weekly => "weekly",
            Frequency::Monthly => "monthly",
        }
    }
}

impl std::fmt::Display for Frequency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Frequency::Daily => "Daily",
            Frequency::Weekly => "Weekly",
            Frequency::Monthly => "Monthly",
        })
    }
}

const FREQUENCIES: [Frequency; 3] = [Frequency::Daily, Frequency::Weekly, Frequency::Monthly];

const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// Weekday choice (0 = Sunday) for the pick list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeekdayOption(pub i64);

impl std::fmt::Display for WeekdayOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(DAY_NAMES.get(self.0 as usize).copied().unwrap_or("?"))
    }
}

/// One entry of an assignee / project / lane pick list. `id: None` is the
/// empty choice (its label is "Unassigned", "No project" or [`FIRST_LANE`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub id: Option<String>,
    pub label: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

impl Choice {
    fn empty(label: &str) -> Self {
        Self { id: None, label: label.to_string() }
    }

    fn of(id: &str, label: &str) -> Self {
        Self { id: Some(id.to_string()), label: label.to_string() }
    }
}

const PRIORITIES: [Priority; 5] = [
    Priority::Urgent,
    Priority::High,
    Priority::Medium,
    Priority::Low,
    Priority::None,
];

/// Open create form. Number and date fields are raw text so half-typed input
/// is representable; [`validate`] parses them.
pub struct RecurringFormState {
    pub title: String,
    pub description: text_editor::Content,
    pub priority: Priority,
    pub frequency: Frequency,
    pub interval: String,
    /// 0 = Sunday. Only used (and shown) for weekly.
    pub weekday: i64,
    /// Raw text. Only used (and shown) for monthly.
    pub day_of_month: String,
    /// Raw `YYYY-MM-DD` text.
    pub starts_on: String,
    /// Pick lists (first entry is the empty choice) and the current picks.
    pub assignee_options: Vec<Choice>,
    pub project_options: Vec<Choice>,
    pub lane_options: Vec<Choice>,
    pub assignee: Choice,
    pub project: Choice,
    pub lane: Choice,
    /// Local UUIDs of the labels toggled on in the chip grid.
    pub label_ids: Vec<String>,
    /// Inline errors from the last failed Save; cleared by a successful one.
    pub errors: FormErrors,
}

impl RecurringFormState {
    /// Blank form: daily every 1, weekday/day/starts-on defaulting to `today`.
    pub fn new(today: NaiveDate) -> Self {
        let (assignee, project, lane) = (
            Choice::empty("Unassigned"),
            Choice::empty("No project"),
            Choice::empty(FIRST_LANE),
        );
        Self {
            assignee_options: vec![assignee.clone()],
            project_options: vec![project.clone()],
            lane_options: vec![lane.clone()],
            assignee,
            project,
            lane,
            title: String::new(),
            description: text_editor::Content::new(),
            priority: Priority::None,
            frequency: Frequency::Daily,
            interval: "1".to_string(),
            weekday: i64::from(today.weekday().num_days_from_sunday()),
            day_of_month: today.day().to_string(),
            starts_on: today.format("%Y-%m-%d").to_string(),
            label_ids: Vec::new(),
            errors: FormErrors::default(),
        }
    }

    /// Fill the pick lists from the selected team's members, projects and lanes.
    pub fn with_routing_options(
        mut self,
        members: &[Member],
        projects: &[Project],
        lanes: &[Lane],
    ) -> Self {
        self.assignee_options
            .extend(members.iter().map(|m| Choice::of(&m.id, &m.name)));
        self.project_options
            .extend(projects.iter().map(|p| Choice::of(&p.id, &p.name)));
        self.lane_options
            .extend(lanes.iter().map(|l| Choice::of(&l.id, &l.name)));
        self
    }

    /// Toggle a label's presence in the selected set.
    pub fn toggle_label(&mut self, label_id: &str) {
        if let Some(pos) = self.label_ids.iter().position(|id| id == label_id) {
            self.label_ids.remove(pos);
        } else {
            self.label_ids.push(label_id.to_string());
        }
    }

    /// Live schedule wording; `None` while the interval isn't a number or the
    /// weekday/day of month for the chosen frequency doesn't parse.
    pub fn summary(&self) -> Option<String> {
        let interval = self.interval.trim().parse::<i64>().ok()?;
        let (weekday, dom) = match self.frequency {
            Frequency::Daily => (None, None),
            Frequency::Weekly => (Some(self.weekday), None),
            Frequency::Monthly => (None, Some(self.day_of_month.trim().parse::<i64>().ok()?)),
        };
        Some(crate::schedule_summary::schedule_summary(
            self.frequency.as_str(),
            interval,
            weekday,
            dom,
        ))
    }
}

/// One optional inline message per field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormErrors {
    pub title: Option<String>,
    pub interval: Option<String>,
    pub weekday: Option<String>,
    pub day_of_month: Option<String>,
    pub starts_on: Option<String>,
}

impl FormErrors {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Check the form against the server's rules (title present, interval 1-365,
/// weekday 0-6 for weekly, day of month 1-31 for monthly, valid starts-on
/// date). Ok carries the db write; Err every failing field at once.
pub fn validate(form: &RecurringFormState) -> Result<NewRecurringIssue, FormErrors> {
    let mut errors = FormErrors::default();

    let title = form.title.trim();
    if title.is_empty() {
        errors.title = Some("Title can't be blank".into());
    }

    let interval = match form.interval.trim().parse::<i64>() {
        Ok(n) if (1..=365).contains(&n) => n,
        _ => {
            errors.interval = Some("Interval must be a whole number from 1 to 365".into());
            1
        }
    };

    let mut weekday = None;
    let mut day_of_month = None;
    match form.frequency {
        Frequency::Daily => {}
        Frequency::Weekly => {
            if (0..=6).contains(&form.weekday) {
                weekday = Some(form.weekday);
            } else {
                errors.weekday = Some("Weekday must be 0 to 6".into());
            }
        }
        Frequency::Monthly => match form.day_of_month.trim().parse::<i64>() {
            Ok(n) if (1..=31).contains(&n) => day_of_month = Some(n),
            _ => errors.day_of_month = Some("Day of month must be from 1 to 31".into()),
        },
    }

    let starts_on = form.starts_on.trim();
    if NaiveDate::parse_from_str(starts_on, "%Y-%m-%d").is_err() {
        errors.starts_on = Some("Starts on must be a valid date (YYYY-MM-DD)".into());
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    let description = form.description.text();
    Ok(NewRecurringIssue {
        title: title.to_string(),
        description: if description.trim().is_empty() {
            None
        } else {
            Some(description.trim_end().to_string())
        },
        priority: form.priority,
        frequency: form.frequency.as_str().to_string(),
        interval,
        weekday,
        day_of_month,
        starts_on: starts_on.to_string(),
        assignee_id: form.assignee.id.clone(),
        project_id: form.project.id.clone(),
        lane_id: form.lane.id.clone(),
        label_ids: form.label_ids.clone(),
    })
}

fn error_line<'a>(e: &'a Option<String>) -> Option<Element<'a, Message>> {
    e.as_deref()
        .map(|m| text(m).size(12).color(t::danger()).into())
}

fn choice_pick<'a>(
    label: &'a str,
    options: &'a [Choice],
    selected: &'a Choice,
    on_pick: fn(Choice) -> Message,
) -> Element<'a, Message> {
    column![]
        .spacing(4)
        .push(text(label).size(12).color(t::text_muted()))
        .push(pick_list(options, Some(selected.clone()), on_pick).width(Length::Fill))
        .into()
}

fn labels_section<'a>(form: &'a RecurringFormState, labels: &'a [Label]) -> Element<'a, Message> {
    let content: Element<'a, Message> = if labels.is_empty() {
        text("No labels yet.").size(12).color(t::text_muted()).into()
    } else {
        labels_view::chip_grid(
            labels,
            |l| form.label_ids.contains(&l.id),
            |l| Message::RecurringFormLabelToggled(l.id.clone()),
            4,
        )
    };
    column![]
        .spacing(6)
        .push(text("Labels").size(12).color(t::text_muted()))
        .push(content)
        .into()
}

pub fn view<'a>(form: &'a RecurringFormState, labels: &'a [Label]) -> Element<'a, Message> {
    let weekday_options: Vec<WeekdayOption> = (0..7).map(WeekdayOption).collect();

    let mut schedule = column![].spacing(10).push(
        row![]
            .spacing(12)
            .push(
                column![]
                    .spacing(4)
                    .width(Length::Fill)
                    .push(text("Repeats").size(12).color(t::text_muted()))
                    .push(pick_list(
                        FREQUENCIES.as_slice(),
                        Some(form.frequency),
                        Message::RecurringFormFrequencyChanged,
                    )),
            )
            .push(
                column![]
                    .spacing(4)
                    .width(Length::Fill)
                    .push(text("Every (interval)").size(12).color(t::text_muted()))
                    .push(
                        text_input("1", &form.interval)
                            .on_input(Message::RecurringFormIntervalChanged)
                            .padding(8)
                            .size(14),
                    ),
            ),
    );
    schedule = schedule.push_maybe(error_line(&form.errors.interval));
    if form.frequency == Frequency::Weekly {
        schedule = schedule
            .push(
                column![]
                    .spacing(4)
                    .push(text("On").size(12).color(t::text_muted()))
                    .push(pick_list(
                        weekday_options,
                        Some(WeekdayOption(form.weekday)),
                        |w: WeekdayOption| Message::RecurringFormWeekdayChanged(w.0),
                    )),
            )
            .push_maybe(error_line(&form.errors.weekday));
    }
    if form.frequency == Frequency::Monthly {
        schedule = schedule
            .push(
                column![]
                    .spacing(4)
                    .push(text("Day of month").size(12).color(t::text_muted()))
                    .push(
                        text_input("1-31", &form.day_of_month)
                            .on_input(Message::RecurringFormDayOfMonthChanged)
                            .padding(8)
                            .size(14),
                    ),
            )
            .push_maybe(error_line(&form.errors.day_of_month));
    }
    schedule = schedule
        .push(
            column![]
                .spacing(4)
                .push(text("Starts on").size(12).color(t::text_muted()))
                .push(
                    text_input("YYYY-MM-DD", &form.starts_on)
                        .on_input(Message::RecurringFormStartsOnChanged)
                        .padding(8)
                        .size(14),
                ),
        )
        .push_maybe(error_line(&form.errors.starts_on))
        .push_maybe(form.summary().map(|s| text(s).size(13).color(t::text_secondary())));

    let form_content = column![]
        .spacing(14)
        .width(Length::Fixed(460.0))
        .push(text("New recurring issue").size(18).color(t::text_primary()))
        .push(
            text_input("Title", &form.title)
                .on_input(Message::RecurringFormTitleChanged)
                .padding(10)
                .size(14),
        )
        .push_maybe(error_line(&form.errors.title))
        .push(
            text_editor(&form.description)
                .placeholder("Description")
                .on_action(Message::RecurringFormDescriptionChanged)
                .padding(10)
                .size(14)
                .height(Length::Fixed(120.0)),
        )
        .push(
            column![]
                .spacing(4)
                .push(text("Priority").size(12).color(t::text_muted()))
                .push(pick_list(
                    PRIORITIES.as_slice(),
                    Some(form.priority),
                    Message::RecurringFormPriorityChanged,
                )),
        )
        .push(schedule)
        .push(choice_pick(
            "Assignee",
            &form.assignee_options,
            &form.assignee,
            Message::RecurringFormAssigneeChanged,
        ))
        .push(choice_pick(
            "Project",
            &form.project_options,
            &form.project,
            Message::RecurringFormProjectChanged,
        ))
        .push(choice_pick(
            "Lane",
            &form.lane_options,
            &form.lane,
            Message::RecurringFormLaneChanged,
        ))
        .push(labels_section(form, labels))
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text("Save").size(14).color(t::bg_primary()))
                        .on_press(Message::SaveRecurringIssue)
                        .padding([8, 24])
                        .style(move |_theme: &Theme, status| button::Style {
                            background: Some(iced::Background::Color(t::accent_fill(status))),
                            text_color: t::bg_primary(),
                            border: Border {
                                radius: 4.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                )
                .push(
                    button(text("Cancel").size(14))
                        .on_press(Message::CloseModal)
                        .padding([8, 24])
                        .style(button::secondary),
                ),
        );

    let modal = container(form_content)
        .padding(28)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        // A Wednesday, the 4th.
        NaiveDate::from_ymd_opt(2026, 11, 4).unwrap()
    }

    fn valid() -> RecurringFormState {
        let mut f = RecurringFormState::new(today());
        f.title = "Water plants".into();
        f
    }

    #[test]
    fn new_form_defaults_to_today() {
        let f = RecurringFormState::new(today());
        assert_eq!(f.weekday, 3);
        assert_eq!(f.day_of_month, "4");
        assert_eq!(f.starts_on, "2026-11-04");
        assert_eq!(f.interval, "1");
        assert_eq!(f.frequency, Frequency::Daily);
    }

    #[test]
    fn routing_choices_default_empty_and_flow_into_the_write() {
        let mut f = valid();
        let n = validate(&f).unwrap();
        assert_eq!((n.assignee_id, n.project_id, n.lane_id), (None, None, None));
        assert_eq!(f.lane.label, FIRST_LANE);

        f.assignee = Choice::of("m1", "Ann");
        f.project = Choice::of("p1", "P");
        f.lane = Choice::of("l1", "Todo");
        let n = validate(&f).unwrap();
        assert_eq!(n.assignee_id.as_deref(), Some("m1"));
        assert_eq!(n.project_id.as_deref(), Some("p1"));
        assert_eq!(n.lane_id.as_deref(), Some("l1"));
    }

    #[test]
    fn summary_follows_the_fields() {
        let mut f = valid();
        assert_eq!(f.summary().as_deref(), Some("Every day"));
        f.frequency = Frequency::Weekly;
        f.interval = "2".into();
        assert_eq!(f.summary().as_deref(), Some("Every 2 weeks on Wednesday"));
        f.frequency = Frequency::Monthly;
        f.day_of_month = "1".into();
        assert_eq!(f.summary().as_deref(), Some("Every 2 months on the 1st"));
        f.interval = "x".into();
        assert_eq!(f.summary(), None);
    }

    #[test]
    fn a_valid_form_yields_only_the_fields_of_its_frequency() {
        let mut f = valid();
        f.frequency = Frequency::Weekly;
        let n = validate(&f).unwrap();
        assert_eq!((n.weekday, n.day_of_month), (Some(3), None));
        f.frequency = Frequency::Monthly;
        let n = validate(&f).unwrap();
        assert_eq!((n.weekday, n.day_of_month), (None, Some(4)));
        f.frequency = Frequency::Daily;
        let n = validate(&f).unwrap();
        assert_eq!((n.weekday, n.day_of_month), (None, None));
        assert_eq!(n.starts_on, "2026-11-04");
    }

    #[test]
    fn each_rule_blocks_saving() {
        let mut f = valid();
        f.title = "  ".into();
        assert!(validate(&f).unwrap_err().title.is_some());

        for bad in ["0", "366", "abc", ""] {
            let mut f = valid();
            f.interval = bad.into();
            assert!(validate(&f).unwrap_err().interval.is_some(), "{bad}");
        }
        for ok in ["1", "365"] {
            let mut f = valid();
            f.interval = ok.into();
            assert!(validate(&f).is_ok(), "{ok}");
        }

        let mut f = valid();
        f.frequency = Frequency::Weekly;
        f.weekday = 7;
        assert!(validate(&f).unwrap_err().weekday.is_some());

        for bad in ["0", "32", "x"] {
            let mut f = valid();
            f.frequency = Frequency::Monthly;
            f.day_of_month = bad.into();
            assert!(validate(&f).unwrap_err().day_of_month.is_some(), "{bad}");
        }

        for bad in ["", "2026-13-01", "2026-02-30", "tomorrow"] {
            let mut f = valid();
            f.starts_on = bad.into();
            assert!(validate(&f).unwrap_err().starts_on.is_some(), "{bad}");
        }
    }

    #[test]
    fn toggled_labels_reach_the_db_write() {
        let mut f = valid();
        f.toggle_label("a");
        f.toggle_label("b");
        f.toggle_label("a");
        assert_eq!(validate(&f).unwrap().label_ids, vec!["b".to_string()]);
    }

    #[test]
    fn a_hidden_field_is_not_validated() {
        let mut f = valid();
        f.day_of_month = "99".into();
        f.weekday = 9;
        assert!(validate(&f).is_ok());
    }
}
