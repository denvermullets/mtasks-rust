//! "New recurring issue" / "Edit recurring issue" modal (JRST-172, JRST-177).
//!
//! The same state serves both: `editing: Some(local id)` is edit mode, which
//! saves through [`validate_edit`] (content, routing and the live schedule
//! fields). Starts-on defaults to today in edit mode; it only reschedules when
//! the schedule fields or starts-on differ from the stored row / the default.
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
use iced::widget::{button, column, container, pick_list, row, text_editor, text_input};
use crate::views::text;
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Label, Lane, Member, Priority, Project};
use crate::db::recurring_issues::{
    NewRecurringIssue, RecurringEdit, RecurringIssue, ScheduleEdit, FIRST_LANE,
};
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

/// ActiveSupport's `TimeZone.us_zones`, in the order and spelling the web form
/// offers (`time_zone_select ... us_zones`). Keep in step with
/// `db::recurring_issues::time_zone_abbreviation`.
pub const US_TIME_ZONES: [&str; 29] = [
    "America/Adak",
    "Hawaii",
    "Alaska",
    "America/Anchorage",
    "America/Metlakatla",
    "America/Nome",
    "America/Sitka",
    "America/Yakutat",
    "Pacific Time (US & Canada)",
    "America/Boise",
    "Arizona",
    "Mountain Time (US & Canada)",
    "America/Indiana/Knox",
    "America/Indiana/Tell_City",
    "America/Menominee",
    "America/North_Dakota/Beulah",
    "America/North_Dakota/Center",
    "America/North_Dakota/New_Salem",
    "Central Time (US & Canada)",
    "America/Detroit",
    "America/Indiana/Marengo",
    "America/Indiana/Petersburg",
    "America/Indiana/Vevay",
    "America/Indiana/Vincennes",
    "America/Indiana/Winamac",
    "America/Kentucky/Louisville",
    "America/Kentucky/Monticello",
    "Eastern Time (US & Canada)",
    "Indiana (East)",
];

/// Time zone choice for the pick list; `None` leaves the zone unset so the
/// server uses the creator's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeZoneChoice(pub Option<String>);

impl std::fmt::Display for TimeZoneChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_deref().unwrap_or("My time zone preference"))
    }
}

/// The zone pick list: "Default" (unless `with_default` is false), then the
/// stored zone when it is outside [`US_TIME_ZONES`] (so it stays visible and
/// selectable when editing), then the US zones.
fn time_zone_choices(stored: Option<&str>, with_default: bool) -> Vec<TimeZoneChoice> {
    let extra = stored.filter(|z| !US_TIME_ZONES.contains(z));
    let default = with_default.then_some(TimeZoneChoice(None));
    default
        .into_iter()
        .chain(extra.map(|z| TimeZoneChoice(Some(z.to_string()))))
        .chain(US_TIME_ZONES.iter().map(|z| TimeZoneChoice(Some(z.to_string()))))
        .collect()
}

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
    /// Set once the user edits Starts on; an edit that never touches it
    /// doesn't count as moving the schedule, but any touch does (even back
    /// to today).
    pub starts_on_touched: bool,
    /// Pick lists (first entry is the empty choice) and the current picks.
    pub assignee_options: Vec<Choice>,
    pub project_options: Vec<Choice>,
    pub lane_options: Vec<Choice>,
    pub assignee: Choice,
    pub project: Choice,
    pub lane: Choice,
    /// `None` = server default (the creator's zone).
    pub time_zone: Option<String>,
    /// Local UUIDs of the labels toggled on in the chip grid.
    pub label_ids: Vec<String>,
    /// Inline errors from the last failed Save; cleared by a successful one.
    pub errors: FormErrors,
    /// `Some(local id)` when editing an existing recurring issue.
    pub editing: Option<String>,
    /// The zone stored on the row being edited (kept even when not in
    /// [`US_TIME_ZONES`]; the server cannot unset it).
    pub stored_time_zone: Option<String>,
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
            starts_on_touched: false,
            time_zone: None,
            label_ids: Vec::new(),
            errors: FormErrors::default(),
            editing: None,
            stored_time_zone: None,
        }
    }

    /// Edit form prefilled from a stored row and its local label ids. Call
    /// `with_routing_options` next; the picks resolve against those lists (a
    /// stored id that isn't offered is added so saving doesn't clear it).
    pub fn for_edit(row: &RecurringIssue, label_ids: Vec<String>, today: NaiveDate) -> Self {
        let mut f = Self::new(today);
        f.editing = Some(row.id.clone());
        f.title = row.title.clone();
        f.description = text_editor::Content::with_text(row.description.as_deref().unwrap_or(""));
        f.priority = row.priority;
        f.frequency = match row.frequency.as_str() {
            "weekly" => Frequency::Weekly,
            "monthly" => Frequency::Monthly,
            _ => Frequency::Daily,
        };
        f.interval = row.interval.to_string();
        if let Some(w) = row.weekday {
            f.weekday = w;
        }
        if let Some(d) = row.day_of_month {
            f.day_of_month = d.to_string();
        }
        f.time_zone = row.time_zone.clone();
        f.stored_time_zone = row.time_zone.clone();
        f.label_ids = label_ids;
        f.assignee = Choice { id: row.assignee_id.clone(), label: String::new() };
        f.project = Choice { id: row.project_id.clone(), label: String::new() };
        f.lane = Choice { id: row.lane_id.clone(), label: String::new() };
        f
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
        // Resolve prefilled picks (edit mode) to the offered choices.
        fn resolve(pick: &mut Choice, options: &mut Vec<Choice>) {
            let Some(id) = pick.id.clone() else { return };
            match options.iter().find(|c| c.id.as_deref() == Some(id.as_str())) {
                Some(c) => *pick = c.clone(),
                None => {
                    *pick = Choice::of(&id, "(unavailable)");
                    options.push(pick.clone());
                }
            }
        }
        resolve(&mut self.assignee, &mut self.assignee_options);
        resolve(&mut self.project, &mut self.project_options);
        resolve(&mut self.lane, &mut self.lane_options);
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
    pub time_zone: Option<String>,
}

impl FormErrors {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The schedule rules shared by create and edit: interval 1-365, weekday 0-6
/// for weekly, day of month 1-31 for monthly, valid starts-on date. Failures
/// land in `errors`; the returned values are only meaningful when it stays empty.
fn validate_schedule(
    form: &RecurringFormState,
    errors: &mut FormErrors,
) -> (i64, Option<i64>, Option<i64>, String) {
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
    (interval, weekday, day_of_month, starts_on.to_string())
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

    let (interval, weekday, day_of_month, starts_on) = validate_schedule(form, &mut errors);

    if let Some(z) = &form.time_zone {
        if !US_TIME_ZONES.contains(&z.as_str()) {
            errors.time_zone = Some("Time zone must be one of the listed zones".into());
        }
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
        starts_on,
        assignee_id: form.assignee.id.clone(),
        project_id: form.project.id.clone(),
        lane_id: form.lane.id.clone(),
        time_zone: form.time_zone.clone(),
        label_ids: form.label_ids.clone(),
    })
}

/// Check an edit: title, the live schedule fields and the zone. Starts-on
/// defaults to today and only counts as moving the schedule when changed. A time zone outside [`US_TIME_ZONES`] is accepted when
/// it is the zone already stored on the row.
pub fn validate_edit(form: &RecurringFormState) -> Result<RecurringEdit, FormErrors> {
    let mut errors = FormErrors::default();
    let title = form.title.trim();
    if title.is_empty() {
        errors.title = Some("Title can't be blank".into());
    }
    let (interval, weekday, day_of_month, starts_on) = validate_schedule(form, &mut errors);
    if let Some(z) = &form.time_zone {
        if !US_TIME_ZONES.contains(&z.as_str()) && form.stored_time_zone.as_ref() != Some(z) {
            errors.time_zone = Some("Time zone must be one of the listed zones".into());
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let description = form.description.text();
    Ok(RecurringEdit {
        title: title.to_string(),
        description: if description.trim().is_empty() {
            None
        } else {
            Some(description.trim_end().to_string())
        },
        priority: form.priority,
        assignee_id: form.assignee.id.clone(),
        project_id: form.project.id.clone(),
        lane_id: form.lane.id.clone(),
        time_zone: form.time_zone.clone(),
        label_ids: form.label_ids.clone(),
        schedule: ScheduleEdit {
            frequency: form.frequency.as_str().to_string(),
            interval,
            weekday,
            day_of_month,
            starts_on_moved: form.starts_on_touched,
            starts_on,
        },
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
        .push_maybe(error_line(&form.errors.starts_on));
    let editing = form.editing.is_some();
    schedule = schedule
        .push(
            column![]
                .spacing(4)
                .push(text("Time zone").size(12).color(t::text_muted()))
                .push(pick_list(
                    time_zone_choices(
                        form.stored_time_zone.as_deref(),
                        form.stored_time_zone.is_none(),
                    ),
                    Some(TimeZoneChoice(form.time_zone.clone())),
                    |c: TimeZoneChoice| Message::RecurringFormTimeZoneChanged(c.0),
                )),
        )
        .push_maybe(error_line(&form.errors.time_zone))
        .push_maybe(form.summary().map(|s| text(s).size(13).color(t::text_secondary())));

    let form_content = column![]
        .spacing(14)
        .width(Length::Fixed(460.0))
        .push(
            text(if editing { "Edit recurring issue" } else { "New recurring issue" })
                .size(18)
                .color(t::text_primary()),
        )
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
                            text_color: t::on_accent(status),
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
    fn time_zone_defaults_to_unset_and_must_be_a_listed_zone() {
        assert_eq!(validate(&valid()).unwrap().time_zone, None);
        let mut f = valid();
        f.time_zone = Some("Mountain Time (US & Canada)".into());
        assert_eq!(
            validate(&f).unwrap().time_zone.as_deref(),
            Some("Mountain Time (US & Canada)")
        );
        f.time_zone = Some("Tokyo".into());
        assert!(validate(&f).unwrap_err().time_zone.is_some());
    }

    #[test]
    fn the_zone_list_is_unique_and_covers_the_abbreviation_map() {
        let mut seen = std::collections::HashSet::new();
        assert!(US_TIME_ZONES.iter().all(|z| seen.insert(*z)));
        for z in [
            "Eastern Time (US & Canada)",
            "Central Time (US & Canada)",
            "Mountain Time (US & Canada)",
            "Arizona",
            "Pacific Time (US & Canada)",
            "Alaska",
            "Hawaii",
            "Indiana (East)",
        ] {
            assert!(US_TIME_ZONES.contains(&z), "{z}");
            assert!(crate::db::recurring_issues::time_zone_abbreviation(z).is_some(), "{z}");
        }
        for z in US_TIME_ZONES {
            assert!(crate::db::recurring_issues::time_zone_abbreviation(z).is_some(), "{z}");
        }
        let abbr = crate::db::recurring_issues::time_zone_abbreviation;
        assert_eq!(abbr("America/Detroit"), Some("ET"));
        assert_eq!(abbr("America/Boise"), Some("MT"));
        assert_eq!(abbr("America/Juneau"), Some("AKT"));
    }

    #[test]
    fn toggled_labels_reach_the_db_write() {
        let mut f = valid();
        f.toggle_label("a");
        f.toggle_label("b");
        f.toggle_label("a");
        assert_eq!(validate(&f).unwrap().label_ids, vec!["b".to_string()]);
    }

    fn stored_row() -> RecurringIssue {
        RecurringIssue {
            id: "r1".into(),
            remote_id: Some(5),
            team_id: "t1".into(),
            title: "Water".into(),
            description: Some("Ferns".into()),
            priority: Priority::High,
            frequency: "weekly".into(),
            interval: 2,
            weekday: Some(5),
            day_of_month: None,
            time_zone: Some("Tokyo".into()),
            active: true,
            next_run_on: Some("2026-11-06".into()),
            last_run_at: None,
            assignee_id: Some("m1".into()),
            assignee_name: None,
            project_id: Some("missing".into()),
            lane_id: Some("l1".into()),
            creator_remote_id: None,
            creator_name: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn the_edit_form_is_prefilled_and_keeps_an_out_of_list_zone() {
        let members = [Member {
            id: "m1".into(),
            remote_id: None,
            team_id: "t1".into(),
            name: "Ann".into(),
            email: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }];
        let f = RecurringFormState::for_edit(&stored_row(), vec!["lab1".into()], today())
            .with_routing_options(&members, &[], &[]);
        assert_eq!(f.editing.as_deref(), Some("r1"));
        assert_eq!((f.title.as_str(), f.description.text().trim_end()), ("Water", "Ferns"));
        assert_eq!(f.priority, Priority::High);
        assert_eq!((f.frequency, f.interval.as_str(), f.weekday), (Frequency::Weekly, "2", 5));
        // Starts-on defaults to today, not the row's next run.
        assert_eq!(f.starts_on, "2026-11-04");
        assert_eq!(f.assignee.label, "Ann");
        // A stored id that isn't offered stays selected rather than clearing.
        assert_eq!(f.project.id.as_deref(), Some("missing"));
        assert!(f.project_options.contains(&f.project));
        assert_eq!(f.label_ids, ["lab1"]);

        // The out-of-list zone is shown (and offered) and accepted on save.
        let choices = time_zone_choices(f.stored_time_zone.as_deref(), false);
        assert_eq!(choices[0], TimeZoneChoice(Some("Tokyo".into())));
        assert!(!choices.contains(&TimeZoneChoice(None)));
        let edit = validate_edit(&f).unwrap();
        assert_eq!(edit.time_zone.as_deref(), Some("Tokyo"));
        assert_eq!(edit.title, "Water");

        let mut other = f;
        other.time_zone = Some("Paris".into());
        assert!(validate_edit(&other).unwrap_err().time_zone.is_some());
        other.time_zone = Some("Alaska".into());
        other.title = " ".into();
        assert!(validate_edit(&other).unwrap_err().title.is_some());
    }

    #[test]
    fn edit_validation_checks_the_schedule_and_flags_a_moved_starts_on() {
        let mut f = RecurringFormState::for_edit(&stored_row(), vec![], today());
        let e = validate_edit(&f).unwrap();
        assert!(!e.schedule.starts_on_moved);
        assert_eq!(e.schedule.starts_on, "2026-11-04");
        assert_eq!(
            (e.schedule.frequency.as_str(), e.schedule.interval, e.schedule.weekday),
            ("weekly", 2, Some(5))
        );
        f.starts_on = "2026-12-01".into();
        f.starts_on_touched = true;
        assert!(validate_edit(&f).unwrap().schedule.starts_on_moved);
        // Rescheduling back to the default (today) is still a schedule change.
        f.starts_on = today().format("%Y-%m-%d").to_string();
        assert!(validate_edit(&f).unwrap().schedule.starts_on_moved);
        f.interval = "0".into();
        f.starts_on = "nope".into();
        let errs = validate_edit(&f).unwrap_err();
        assert!(errs.interval.is_some() && errs.starts_on.is_some());
    }

    #[test]
    fn a_hidden_field_is_not_validated() {
        let mut f = valid();
        f.day_of_month = "99".into();
        f.weekday = 9;
        assert!(validate(&f).is_ok());
    }
}
