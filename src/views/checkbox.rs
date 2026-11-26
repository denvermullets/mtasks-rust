//! The app's `checkbox` widget: Iced's, styled with `theme::checkbox_style`
//! so the check mark keeps 4.5:1 on the accent box, hovered or not.
//!
//! Import it as `use crate::views::checkbox;` instead of `iced::widget::checkbox`.

use iced::widget::Checkbox;

/// Iced's `checkbox()` with the app's checkbox style.
pub fn checkbox<'a, Message>(is_checked: bool) -> Checkbox<'a, Message> {
    Checkbox::new(is_checked).style(crate::theme::checkbox_style)
}
