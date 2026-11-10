use iced::widget::{button, column, container, row, scrollable};
use crate::views::text;
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::csv_io::{ImportPreview, RowProblem};
use crate::theme as t;

/// State for the CSV import preview modal.
pub struct CsvImportState {
    /// File name shown in the header.
    pub file_name: String,
    pub preview: ImportPreview,
}

/// Cap on inline-listed problems so a huge bad file doesn't build a giant list.
const MAX_LISTED: usize = 50;

fn problem_list<'a>(
    title: String,
    problems: &'a [RowProblem],
    color: iced::Color,
) -> Element<'a, Message> {
    let mut col = column![].spacing(3).push(text(title).size(12).color(color));
    for p in problems.iter().take(MAX_LISTED) {
        let prefix = if p.line > 0 {
            format!("Line {}: ", p.line)
        } else {
            String::new()
        };
        col = col.push(
            text(format!("{prefix}{}", p.message))
                .size(12)
                .color(t::text_secondary()),
        );
    }
    if problems.len() > MAX_LISTED {
        col = col.push(
            text(format!("… and {} more", problems.len() - MAX_LISTED))
                .size(12)
                .color(t::text_muted()),
        );
    }
    col.into()
}

pub fn view(state: &CsvImportState) -> Element<'_, Message> {
    let p = &state.preview;

    let mut body = column![].spacing(12).push(
        text(format!(
            "{} issue(s) ready to import, {} row(s) with errors will be skipped.",
            p.rows.len(),
            p.errors.len()
        ))
        .size(13)
        .color(t::text_primary()),
    );

    if !p.new_projects.is_empty() {
        body = body.push(
            text(format!(
                "Will create {} project(s): {}",
                p.new_projects.len(),
                p.new_projects.join(", ")
            ))
            .size(12)
            .color(t::text_secondary()),
        );
    }

    // Short preview of the first few valid rows.
    if !p.rows.is_empty() {
        let mut list = column![].spacing(2);
        for r in p.rows.iter().take(8) {
            list = list.push(
                text(format!("• {}", r.title))
                    .size(12)
                    .color(t::text_secondary()),
            );
        }
        if p.rows.len() > 8 {
            list = list.push(
                text(format!("… and {} more", p.rows.len() - 8))
                    .size(12)
                    .color(t::text_muted()),
            );
        }
        body = body.push(list);
    }
    if !p.duplicates.is_empty() {
        body = body.push(problem_list(
            format!(
                "Already in this team ({}) — these rows are skipped",
                p.duplicates.len()
            ),
            &p.duplicates,
            t::text_muted(),
        ));
    }
    if !p.errors.is_empty() {
        body = body.push(problem_list(
            format!("Errors ({}) — these rows are skipped", p.errors.len()),
            &p.errors,
            t::danger(),
        ));
    }
    if !p.warnings.is_empty() {
        body = body.push(problem_list(
            format!("Warnings ({})", p.warnings.len()),
            &p.warnings,
            t::warning(),
        ));
    }

    let can_import = !p.rows.is_empty();
    let content = column![]
        .spacing(14)
        .width(Length::Fixed(560.0))
        .push(text("Import CSV").size(18).color(t::text_primary()))
        .push(text(&state.file_name).size(12).color(t::text_muted()))
        .push(scrollable(body).height(Length::Shrink).spacing(8))
        .push(
            row![]
                .spacing(8)
                .push(iced::widget::horizontal_space().width(Length::Fill))
                .push(
                    button(text("Cancel").size(14))
                        .on_press(Message::CloseModal)
                        .padding([8, 24])
                        .style(button::secondary),
                )
                .push(
                    button(text("Import").size(14).color(t::bg_primary()))
                        .on_press_maybe(can_import.then_some(Message::ConfirmCsvImport))
                        .padding([8, 24])
                        .style(move |_theme: &Theme, _status| button::Style {
                            background: Some(iced::Background::Color(if can_import {
                                t::accent()
                            } else {
                                t::text_muted()
                            })),
                            text_color: t::bg_primary(),
                            border: Border {
                                radius: 4.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                ),
        );

    let modal = container(content)
        .padding(28)
        .max_height(560)
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
