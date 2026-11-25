//! A date field with a calendar popover, standing in for the web's native
//! `<input type="date">`: the text stays typeable (`YYYY-MM-DD`), and the
//! calendar icon opens a month grid under the field to pick from.
//!
//! One picker is open app-wide (`App::date_picker`); each field names itself
//! with a `DateField` so only its own popover shows.

use chrono::{Datelike, Duration, NaiveDate};
use iced::widget::{button, column, container, row, Space};
use iced::{Alignment, Border, Color, Element, Length, Theme};

use crate::app::Message;
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::dropdown::dropdown;
use crate::views::{form_page, text, util};

/// Every date field that can open the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateField {
    IssueDue,
    ProjectStart,
    ProjectDue,
    ProjectDetailStart,
    ProjectDetailDue,
    RecurringStartsOn,
}

/// The open picker: which field, and the month on show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatePickerState {
    pub field: DateField,
    /// First day of the month shown.
    pub month: NaiveDate,
}

impl DatePickerState {
    /// Open on the field's current date's month, or today's.
    pub fn open(field: DateField, value: &str, today: NaiveDate) -> Self {
        let at = parse(value).unwrap_or(today);
        Self { field, month: first_of_month(at) }
    }

    /// Step the shown month by `delta` months.
    pub fn shift(&mut self, delta: i32) {
        let months = self.month.year() * 12 + self.month.month0() as i32 + delta;
        self.month = NaiveDate::from_ymd_opt(months.div_euclid(12), months.rem_euclid(12) as u32 + 1, 1)
            .unwrap_or(self.month);
    }
}

/// How the field is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Issue form property chip: icon first, compact.
    Chip,
    /// Sidebar/form control: full width, icon at the right like a native
    /// date input.
    Field,
}

/// A date field.
pub struct DateInput<'a> {
    pub field: DateField,
    pub value: &'a str,
    pub placeholder: &'a str,
    /// Typing in the field; also what a pick sends, with `YYYY-MM-DD`.
    pub on_change: fn(String) -> Message,
    /// Enter in the field, and sent after a pick (e.g. commit the project).
    pub on_commit: Option<Message>,
    pub look: Look,
}

const CELL: f32 = 32.0;
const MENU_WIDTH: f32 = CELL * 7.0 + space::S3 * 2.0;

const ICON_CALENDAR: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M8 7V3m8 4V3m-9 8h10M5 21h14a2 2 0 002-2V7a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z"/></svg>"##;
const ICON_PREV: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M15 19l-7-7 7-7"/></svg>"##;
const ICON_NEXT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M9 5l7 7-7 7"/></svg>"##;

pub fn parse(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok()
}

fn first_of_month(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap_or(d)
}

/// The 42 days (6 weeks, Sunday first) the grid shows for `month`.
pub fn month_grid(month: NaiveDate) -> Vec<NaiveDate> {
    let first = first_of_month(month);
    let start = first - Duration::days(first.weekday().num_days_from_sunday() as i64);
    (0..42).map(|i| start + Duration::days(i)).collect()
}

/// What a pick (or Today / Clear) sends: the value, then the commit.
fn pick(input: &DateInput<'_>, value: String) -> Message {
    let mut msgs = vec![(input.on_change)(value)];
    msgs.extend(input.on_commit.clone());
    Message::DatePicked(msgs)
}

pub fn view<'a>(input: DateInput<'a>, open: Option<&DatePickerState>, today: NaiveDate) -> Element<'a, Message> {
    let state = open.filter(|s| s.field == input.field).copied();
    let is_open = state.is_some();

    let icon_button = button(form_page::icon(
        ICON_CALENDAR,
        space::S4,
        if is_open { t::text_strong() } else { t::text_body() },
    ))
    .on_press(Message::DatePickerToggled(input.field, input.value.to_string()))
    .padding(0)
    .style(|_theme: &Theme, _status| button::Style::default());

    let mut field = crate::undo_input::text_input(format!("date-{:?}", input.field), input.placeholder, input.value, input.on_change)
        .padding(0)
        .size(type_scale::SM)
        .style(form_page::borderless_chip_input);
    if let Some(commit) = input.on_commit.clone() {
        field = field.on_submit(commit);
    }

    let anchor: Element<'a, Message> = match input.look {
        Look::Chip => form_page::chip_frame(
            row![]
                .spacing(space::S2)
                .align_y(Alignment::Center)
                .push(icon_button)
                .push(field.width(Length::Fixed(96.0))),
        ),
        Look::Field => container(
            row![]
                .spacing(space::S2)
                .align_y(Alignment::Center)
                .push(field.width(Length::Fill))
                .push(icon_button),
        )
        .padding(util::SIDEBAR_CONTROL_PADDING)
        .width(Length::Fill)
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_secondary())),
            border: Border {
                color: if is_open { t::accent() } else { t::border() },
                width: 1.0,
                radius: radius::MD.into(),
            },
            ..Default::default()
        })
        .into(),
    };

    let menu = state.map(|s| calendar(&input, s, today));
    dropdown(anchor, menu, MENU_WIDTH, Message::DatePickerClosed).align_left().into()
}

fn calendar<'a>(input: &DateInput<'a>, state: DatePickerState, today: NaiveDate) -> Element<'a, Message> {
    let selected = parse(input.value);
    let nav = |src: &'static str, delta: i32| {
        button(form_page::icon(src, space::S4, t::text_secondary()))
            .on_press(Message::DatePickerMonth(delta))
            .padding(space::S1)
            .style(|_theme: &Theme, status| button::Style {
                background: matches!(status, button::Status::Hovered)
                    .then(|| iced::Background::Color(t::bg_card_focused())),
                border: Border { radius: radius::MD.into(), ..Default::default() },
                ..Default::default()
            })
    };
    let header = row![]
        .align_y(Alignment::Center)
        .push(
            text(state.month.format("%B %Y").to_string())
                .size(type_scale::SM)
                .color(t::text_strong()),
        )
        .push(Space::new().width(Length::Fill))
        .push(nav(ICON_PREV, -1))
        .push(nav(ICON_NEXT, 1));

    let weekdays = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"].iter().fold(row![], |r, d| {
        r.push(
            container(text(*d).size(type_scale::XS).color(t::text_muted()))
                .width(Length::Fixed(CELL))
                .center_x(Length::Fixed(CELL)),
        )
    });

    let days = month_grid(state.month);
    let weeks = days.chunks(7).fold(column![], |col, week| {
        col.push(week.iter().fold(row![], |r, &day| {
            let in_month = day.month() == state.month.month();
            let is_selected = selected == Some(day);
            let is_today = day == today;
            let cell = button(
                container(text(day.day().to_string()).size(type_scale::SM))
                    .center_x(Length::Fill)
                    .center_y(Length::Fill),
            )
            .on_press(pick(input, day.format("%Y-%m-%d").to_string()))
            .width(Length::Fixed(CELL))
            .height(Length::Fixed(CELL))
            .padding(0)
            .style(move |_theme: &Theme, status| {
                let hovered = matches!(status, button::Status::Hovered);
                button::Style {
                    background: if is_selected {
                        Some(iced::Background::Color(t::accent()))
                    } else if hovered {
                        Some(iced::Background::Color(t::bg_card_focused()))
                    } else {
                        None
                    },
                    text_color: if is_selected {
                        t::on_accent(status)
                    } else if in_month {
                        t::text_body()
                    } else {
                        t::text_muted()
                    },
                    border: Border {
                        color: if is_today && !is_selected { t::accent() } else { Color::TRANSPARENT },
                        width: 1.0,
                        radius: radius::MD.into(),
                    },
                    ..Default::default()
                }
            });
            r.push(cell)
        }))
    });

    let link = |label: &'static str, msg: Message| {
        button(text(label).size(type_scale::XS))
            .on_press(msg)
            .padding([space::S1, space::S2])
            .style(|_theme: &Theme, status| button::Style {
                background: None,
                text_color: if matches!(status, button::Status::Hovered) {
                    t::text_strong()
                } else {
                    t::accent()
                },
                ..Default::default()
            })
    };
    let footer = row![]
        .push(link("Clear", pick(input, String::new())))
        .push(Space::new().width(Length::Fill))
        .push(link("Today", pick(input, today.format("%Y-%m-%d").to_string())));

    container(
        column![]
            .spacing(space::S2)
            .push(header)
            .push(weekdays)
            .push(weeks)
            .push(footer),
    )
    .padding(space::S3)
    .width(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_secondary())),
        border: Border { color: t::border(), width: 1.0, radius: radius::MD.into() },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.4),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 12.0,
        },
        ..Default::default()
    })
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn grid_starts_on_the_sunday_before_the_first() {
        // 1 Nov 2026 is a Sunday; 1 Dec 2026 a Tuesday.
        let nov = month_grid(d(2026, 11, 10));
        assert_eq!((nov[0], nov.len()), (d(2026, 11, 1), 42));
        let dec = month_grid(d(2026, 12, 1));
        assert_eq!((dec[0], dec[41]), (d(2026, 11, 29), d(2027, 1, 9)));
    }

    #[test]
    fn opens_on_the_value_month_or_today() {
        let today = d(2026, 11, 10);
        assert_eq!(DatePickerState::open(DateField::IssueDue, "2027-03-15", today).month, d(2027, 3, 1));
        assert_eq!(DatePickerState::open(DateField::IssueDue, "soon", today).month, d(2026, 11, 1));
    }

    #[test]
    fn shifting_wraps_the_year() {
        let mut s = DatePickerState::open(DateField::IssueDue, "2026-12-05", d(2026, 1, 1));
        s.shift(1);
        assert_eq!(s.month, d(2027, 1, 1));
        s.shift(-2);
        assert_eq!(s.month, d(2026, 11, 1));
    }
}
