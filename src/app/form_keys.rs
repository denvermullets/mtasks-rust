//! The keyboard inside an open form: Tab / Shift+Tab through its stops, and
//! Enter / Space / arrows / Esc on its dropdown chips (see `form_focus`).

use iced::keyboard::{key::Named, Key, Modifiers};
use iced::Task;

use super::{App, Message, Modal};
use crate::form_focus::{self, Keys, MenuCursor, MenuId, Stop};
use crate::views::{form_page, issue_form, projects, recurring_issue_form};

impl App {
    /// The open form's Tab order, or `None` when no form is open.
    pub(super) fn form_tab_order(&self) -> Option<Vec<Stop>> {
        let fields = |keys: &[&str]| keys.iter().map(|k| Stop::field(*k)).collect();
        match &self.modal {
            Modal::IssueForm if self.issue_form.is_some() => Some(issue_form::tab_order()),
            Modal::ProjectForm if self.project_form.is_some() => Some(projects::tab_order()),
            Modal::RecurringForm => self.recurring_form.as_ref().map(recurring_issue_form::tab_order),
            Modal::Settings => Some(crate::views::settings::TAB_ORDER.iter().map(|k| Stop::field(*k)).collect()),
            Modal::NewDashboard | Modal::EditDashboard => {
                Some(fields(&[super::DASHBOARD_NAME_KEY, super::DASHBOARD_DESCRIPTION_KEY]))
            }
            Modal::NewGroup | Modal::EditGroup(_) => Some(crate::views::group_form::tab_order()),
            _ => None,
        }
    }

    /// The open form's keyboard state, for forms with dropdown chips.
    fn form_keys(&self) -> Option<Keys> {
        match self.modal {
            Modal::IssueForm => self.issue_form.as_ref().map(|f| f.keys),
            Modal::ProjectForm => self.project_form.as_ref().map(|f| f.keys),
            Modal::RecurringForm => self.recurring_form.as_ref().map(|f| f.keys),
            _ => None,
        }
    }

    fn form_keys_mut(&mut self) -> Option<&mut Keys> {
        match self.modal {
            Modal::IssueForm => self.issue_form.as_mut().map(|f| &mut f.keys),
            Modal::ProjectForm => self.project_form.as_mut().map(|f| &mut f.keys),
            Modal::RecurringForm => self.recurring_form.as_mut().map(|f| &mut f.keys),
            _ => None,
        }
    }

    /// The open form's open dropdown.
    fn open_form_menu(&self) -> Option<MenuId> {
        match self.modal {
            Modal::IssueForm => self.issue_form.as_ref()?.open_menu.map(MenuId::Issue),
            Modal::ProjectForm => self.project_form.as_ref()?.open_menu.map(MenuId::Project),
            Modal::RecurringForm => self.recurring_form.as_ref()?.open_menu.map(MenuId::Recurring),
            _ => None,
        }
    }

    fn close_form_menu(&mut self) {
        match self.modal {
            Modal::IssueForm => {
                if let Some(form) = self.issue_form.as_mut() {
                    form.open_menu = None;
                }
            }
            Modal::ProjectForm => {
                if let Some(form) = self.project_form.as_mut() {
                    form.open_menu = None;
                }
            }
            Modal::RecurringForm => {
                if let Some(form) = self.recurring_form.as_mut() {
                    form.open_menu = None;
                }
            }
            _ => {}
        }
    }

    /// `menu`'s rows as drawn: label, current, and the message picking it.
    fn form_menu_rows(&self, menu: MenuId) -> Vec<(String, bool, Message)> {
        match menu {
            MenuId::Issue(which) => {
                let Some(form) = self.issue_form.as_ref() else {
                    return Vec::new();
                };
                let (lanes, labels, choices) = self.issue_form_scope(form);
                issue_form::menu_options(form, which, lanes, labels, &choices)
            }
            MenuId::Project(which) => self
                .project_form
                .as_ref()
                .map(|form| projects::menu_options(form, which, &self.members))
                .unwrap_or_default(),
            MenuId::Recurring(which) => self
                .recurring_form
                .as_ref()
                .map(|form| recurring_issue_form::menu_options(form, which))
                .unwrap_or_default(),
        }
    }

    fn menu_visible_rows(menu: MenuId) -> usize {
        match menu {
            MenuId::Issue(which) => form_page::menu_visible_rows(which == issue_form::FormMenu::Labels),
            MenuId::Project(_) | MenuId::Recurring(_) => form_page::menu_visible_rows(false),
        }
    }

    /// `menu` just opened (keyboard or mouse): put the cursor on its
    /// current value and scroll that into view.
    pub(super) fn form_menu_opened(&mut self, menu: MenuId) -> Task<Message> {
        let row = self.form_menu_rows(menu).iter().position(|(_, current, _)| *current).unwrap_or(0);
        let cursor = MenuCursor::at(row, Self::menu_visible_rows(menu));
        if let Some(keys) = self.form_keys_mut() {
            keys.cursor = cursor;
        }
        form_page::scroll_menu_to(cursor)
    }

    fn open_form_menu_from_keys(&mut self, menu: MenuId) -> Task<Message> {
        match menu {
            MenuId::Issue(which) => {
                if let Some(form) = self.issue_form.as_mut() {
                    form.open_menu = Some(which);
                }
            }
            MenuId::Project(which) => {
                if let Some(form) = self.project_form.as_mut() {
                    form.open_menu = Some(which);
                }
            }
            MenuId::Recurring(which) => {
                if let Some(form) = self.recurring_form.as_mut() {
                    form.open_menu = Some(which);
                }
            }
        }
        self.form_menu_opened(menu)
    }

    /// A key pressed while a form is open. `Some` when the form used it.
    pub(super) fn form_key(&mut self, key: &Key, modifiers: Modifiers) -> Option<Task<Message>> {
        let order = self.form_tab_order()?;
        let named = match key {
            Key::Named(n) => Some(*n),
            _ => None,
        };
        let plain = modifiers.is_empty();
        let space = plain && (named == Some(Named::Space) || matches!(key, Key::Character(c) if c == " "));
        let tab = named == Some(Named::Tab) && !modifiers.command() && !modifiers.control() && !modifiers.alt();

        if let Some(menu) = self.open_form_menu() {
            let rows = self.form_menu_rows(menu).len();
            let visible = Self::menu_visible_rows(menu);
            let move_by = |app: &mut App, delta: i32| {
                let Some(keys) = app.form_keys_mut() else {
                    return Task::none();
                };
                keys.cursor = keys.cursor.moved(delta, rows, visible);
                form_page::scroll_menu_to(keys.cursor)
            };
            match named {
                Some(Named::ArrowDown) if plain => return Some(move_by(self, 1)),
                Some(Named::ArrowUp) if plain => return Some(move_by(self, -1)),
                Some(Named::Escape) => {
                    self.close_form_menu();
                    self.focus_form_menu(menu);
                    return Some(Task::none());
                }
                Some(Named::Enter) if plain => return Some(self.pick_form_menu_row(menu)),
                _ if space => return Some(self.pick_form_menu_row(menu)),
                // Tab leaves the menu closed, moving on from its chip.
                _ if tab => {
                    self.close_form_menu();
                    self.focus_form_menu(menu);
                }
                _ => {}
            }
        }

        if tab {
            let forward = !modifiers.shift();
            if let Some(menu) = self.form_keys().and_then(|k| k.focused) {
                let next = form_focus::step(&order, Some(&Stop::Menu(menu)), forward).cloned();
                return Some(self.focus_form_stop(next));
            }
            let keys = form_focus::field_keys(&order);
            return Some(
                crate::undo_input::focused(keys).map(move |from| Message::FormTabFrom { from, forward }),
            );
        }

        // On a chip: Enter, Space or ↓ opens its menu.
        if let Some(menu) = self.form_keys().and_then(|k| k.focused) {
            if (named == Some(Named::Enter) || named == Some(Named::ArrowDown)) && plain || space {
                return Some(self.open_form_menu_from_keys(menu));
            }
        }
        None
    }

    /// Tab's second half: iced said which field has focus.
    pub(super) fn form_tab_from(&mut self, from: Option<String>, forward: bool) -> Task<Message> {
        let Some(order) = self.form_tab_order() else {
            return Task::none();
        };
        let from = from.map(Stop::Field);
        let next = form_focus::step(&order, from.as_ref(), forward).cloned();
        self.focus_form_stop(next)
    }

    fn focus_form_menu(&mut self, menu: MenuId) {
        if let Some(keys) = self.form_keys_mut() {
            keys.focused = Some(menu);
        }
    }

    fn focus_form_stop(&mut self, stop: Option<Stop>) -> Task<Message> {
        self.close_form_menu();
        match stop {
            Some(Stop::Field(key)) => {
                if let Some(keys) = self.form_keys_mut() {
                    keys.focused = None;
                }
                // The text is selected, as a browser's Tab does.
                let id = crate::undo_input::id(key);
                iced::widget::operation::focus(id.clone())
                    .chain(iced::widget::operation::select_all(id))
            }
            Some(Stop::Menu(menu)) => {
                self.focus_form_menu(menu);
                form_focus::unfocus_all()
            }
            None => Task::none(),
        }
    }

    /// Enter on the cursor row: send its message. A single-choice menu
    /// closes (its handler does that); the keyboard stays on the chip.
    fn pick_form_menu_row(&mut self, menu: MenuId) -> Task<Message> {
        let row = self.form_keys().map(|k| k.cursor.row).unwrap_or(0);
        let Some((_, _, message)) = self.form_menu_rows(menu).into_iter().nth(row) else {
            return Task::none();
        };
        self.focus_form_menu(menu);
        self.update(message)
    }

    /// A mouse press: the pointer has taken over, so the chip ring goes.
    pub(super) fn form_pointer_pressed(&mut self) {
        if let Some(keys) = self.form_keys_mut() {
            keys.focused = None;
        }
    }

    /// Whether a form chip holds the keyboard (the subscription then
    /// listens for the mouse press that hands it back).
    pub(super) fn form_chip_focused(&self) -> bool {
        self.form_keys().is_some_and(|k| k.focused.is_some())
    }
}
