//! The docked Terminal panel: a strip, and a canvas that draws one pane.
//!
//! API: `terminal_panel::view(PanelView { .. })` builds the panel;
//! `Pane` holds what the canvas draws and its per-row caches. Drawing is ported
//! from the JRST-328 spike. Every row has its own `Cache`, so a frame only
//! re-lays-out the rows whose `Arc<RowData>` changed. ASCII runs use Basic
//! shaping (no ligatures, no fallback); other runs use Advanced shaping.

use std::sync::Arc;

use iced::advanced::text::{self as adv_text, Paragraph as _};
use iced::alignment::{Horizontal, Vertical};
use iced::keyboard;
use iced::mouse::{self, Cursor};
use iced::widget::canvas::{self, Cache, Canvas, Geometry, Text};
use iced::widget::{button, column, container, row, scrollable};
use crate::views::text;
use iced::{font, Color, Element, Font, Length, Pixels, Point, Rectangle, Renderer, Size, Theme};

use crate::app::Message;
use crate::terminal::panel::{Dock, Layout, PaneNotice, HANDLE_WIDTH, PANE_GAP, STRIP_HEIGHT};
use crate::terminal::{
    links::Link, pointer, CursorShape, Frame, KeyMods, MouseAction, MouseButton, MouseInput, Rgb, RowData, Run,
    TerminalId, TerminalMessage, WheelInput,
};
use crate::theme as t;

pub const FONT_SIZE: f32 = 13.0;

/// The Terminal font: a bundled monospace family and a size, both from config.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TermFont {
    pub family: &'static str,
    pub size: f32,
}

impl Default for TermFont {
    fn default() -> Self {
        TermFont {
            family: crate::fonts::terminal_family(crate::fonts::DEFAULT_TERMINAL_FONT_KEY),
            size: FONT_SIZE,
        }
    }
}

impl TermFont {
    /// The font a config key names, at `size`. An unknown key gives the default family.
    pub fn from_key(key: &str, size: f32) -> Self {
        TermFont { family: crate::fonts::terminal_family(key), size: crate::fonts::clamp_terminal_size(size) }
    }

    /// Height of one cell: 1.25 em, rounded up.
    fn line_height(&self) -> f32 {
        (self.size * 1.25).ceil()
    }
}

fn col(c: Rgb) -> Color {
    Color::from_rgb8(c[0], c[1], c[2])
}

/// Indexes of rows whose data differs between two frames. A different row
/// count changes every row.
pub fn changed_rows(prev: &[Arc<RowData>], next: &[Arc<RowData>]) -> Vec<usize> {
    if prev.len() != next.len() {
        return (0..next.len()).collect();
    }
    (0..next.len())
        .filter(|&y| !Arc::ptr_eq(&prev[y], &next[y]))
        .collect()
}

/// What the canvas remembers between events: modifiers, whether a button is held
/// over the pane, and the last cell it reported a move for.
#[derive(Default)]
pub struct PaneState {
    mods: keyboard::Modifiers,
    pressed: bool,
    last_cell: (i32, i32),
}

fn map_button(b: mouse::Button) -> Option<MouseButton> {
    match b {
        mouse::Button::Left => Some(MouseButton::Left),
        mouse::Button::Middle => Some(MouseButton::Middle),
        mouse::Button::Right => Some(MouseButton::Right),
        _ => None,
    }
}

fn key_mods(m: keyboard::Modifiers) -> KeyMods {
    KeyMods { shift: m.shift(), alt: m.alt(), ctrl: m.control(), logo: m.logo() }
}

/// What the canvas draws: the latest frame plus a cache per row.
pub struct Pane {
    frame: Arc<Frame>,
    caches: Vec<Cache>,
    last: Vec<Arc<RowData>>,
    bg: Cache,
    cursor: Cache,
    /// The link under a Cmd+hover, underlined over the rows.
    hover: Option<Link>,
    hover_cache: Cache,
    cell: (f32, f32),
    font: TermFont,
    focused: bool,
    /// The panel slot this pane draws, sent with its focus click.
    slot: usize,
}

impl Pane {
    pub fn new() -> Self {
        Self::for_slot(0)
    }

    pub fn for_slot(slot: usize) -> Self {
        Pane {
            slot,
            frame: Arc::new(Frame::default()),
            caches: Vec::new(),
            last: Vec::new(),
            bg: Cache::new(),
            cursor: Cache::new(),
            hover: None,
            hover_cache: Cache::new(),
            cell: (FONT_SIZE * 0.6, (FONT_SIZE * 1.25).ceil()),
            font: TermFont::default(),
            focused: false,
        }
    }

    /// Take a new frame, clearing only the caches of rows that changed.
    pub fn show(&mut self, frame: Arc<Frame>) {
        let changed = changed_rows(&self.last, &frame.rows);
        if self.caches.len() != frame.rows.len() {
            self.caches.resize_with(frame.rows.len(), Cache::new);
        }
        for y in changed {
            self.caches[y].clear();
        }
        if frame.bg != self.frame.bg {
            self.bg.clear();
        }
        self.cursor.clear();
        // The text under an underline may have moved; the next pointer event finds it again.
        if self.hover.is_some() && !changed_rows(&self.last, &frame.rows).is_empty() {
            self.hover = None;
            self.hover_cache.clear();
        }
        self.last = frame.rows.clone();
        self.frame = frame;
    }

    /// Forget everything (another Terminal, a theme change, a new cell size).
    pub fn reset(&mut self) {
        for c in &self.caches {
            c.clear();
        }
        self.bg.clear();
        self.cursor.clear();
        self.hover = None;
        self.hover_cache.clear();
        self.last.clear();
    }

    /// Underline this link (Cmd+hover), or clear the underline.
    pub fn set_hover(&mut self, link: Option<Link>) {
        if link != self.hover {
            self.hover = link;
            self.hover_cache.clear();
        }
    }

    /// The link a Cmd+hover would find at a position inside the pane.
    fn link_under(&self, p: Point) -> Option<Link> {
        let (c, r) = pointer::cell_at(p.x, p.y, self.cell.0, self.cell.1);
        if c < 0 || r < 0 {
            return None;
        }
        self.frame.link_at(r as usize, c as u16)
    }

    pub fn set_cell(&mut self, cell: (f32, f32)) {
        if cell != self.cell {
            self.cell = cell;
            self.reset();
        }
    }

    /// A new font or size: the glyphs change, so every row redraws. The cell
    /// size follows through `set_cell`.
    pub fn set_font(&mut self, font: TermFont) {
        if font != self.font {
            self.font = font;
            self.reset();
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        if focused != self.focused {
            self.focused = focused;
            self.cursor.clear();
        }
    }

    #[cfg(test)]
    pub fn frame(&self) -> &Arc<Frame> {
        &self.frame
    }
}

impl Default for Pane {
    fn default() -> Self {
        Self::new()
    }
}

impl Pane {
    /// Route a pointer or modifier event: whether it's captured, and the
    /// message it produces. `update` turns this into a canvas `Action`.
    fn route(&self, state: &mut PaneState, event: canvas::Event, bounds: Rectangle, cursor: Cursor) -> (bool, Option<Message>) {
        const CAPTURED: bool = true;
        const IGNORED: bool = false;
        if let canvas::Event::Keyboard(keyboard::Event::ModifiersChanged(m)) = event {
            state.mods = m;
            // Cmd going down or up over a link underlines it or not.
            let link = cursor.position_in(bounds).filter(|_| m.logo()).and_then(|p| self.link_under(p));
            if link != self.hover {
                return (IGNORED, Some(Message::Terminal(TerminalMessage::Hover(self.slot, link))));
            }
            return (IGNORED, None);
        }
        let canvas::Event::Mouse(ev) = event else {
            return (IGNORED, None);
        };
        let slot = self.slot;
        let send = |input: MouseInput| Some(Message::Terminal(TerminalMessage::Mouse(slot, input)));
        let at = |p: Point| {
            let (c, r) = pointer::cell_at(p.x - bounds.x, p.y - bounds.y, self.cell.0, self.cell.1);
            (c, r)
        };
        let mods = key_mods(state.mods);
        match ev {
            mouse::Event::ButtonPressed(b) => {
                let Some(p) = cursor.position_in(bounds) else {
                    return (IGNORED, None);
                };
                let Some(button) = map_button(b) else {
                    return (IGNORED, None);
                };
                // Cmd+click opens a link and does nothing else; a plain click never does.
                if b == mouse::Button::Left && state.mods.logo() {
                    if let Some(link) = self.link_under(p) {
                        return (
                            CAPTURED,
                            Some(Message::Terminal(TerminalMessage::OpenLink {
                                url: link.url,
                                explicit: link.explicit,
                            })),
                        );
                    }
                }
                state.pressed = true;
                let (col, row) = at(bounds.position() + iced::Vector::new(p.x, p.y));
                (
                    CAPTURED,
                    send(MouseInput { action: MouseAction::Press(button), col, row, mods }),
                )
            }
            mouse::Event::ButtonReleased(b) => {
                let Some(button) = map_button(b) else {
                    return (IGNORED, None);
                };
                if !std::mem::take(&mut state.pressed) {
                    return (IGNORED, None);
                }
                let (col, row) = cursor.position().map(at).unwrap_or(state.last_cell);
                (
                    CAPTURED,
                    send(MouseInput { action: MouseAction::Release(button), col, row, mods }),
                )
            }
            mouse::Event::CursorMoved { position } if state.mods.logo() && !state.pressed => {
                let inside = Point::new(position.x - bounds.x, position.y - bounds.y);
                let link = bounds.contains(position).then(|| self.link_under(inside)).flatten();
                if link != self.hover {
                    return (IGNORED, Some(Message::Terminal(TerminalMessage::Hover(slot, link))));
                }
                (IGNORED, None)
            }
            mouse::Event::CursorMoved { position } => {
                // Moves matter to a program tracking the mouse, and to a drag in
                // progress (a selection); send one per cell.
                if !(state.pressed || (self.frame.tracking && cursor.is_over(bounds))) {
                    return (IGNORED, None);
                }
                let cell = at(position);
                if cell == state.last_cell {
                    return (IGNORED, None);
                }
                state.last_cell = cell;
                (
                    IGNORED,
                    send(MouseInput { action: MouseAction::Move, col: cell.0, row: cell.1, mods }),
                )
            }
            mouse::Event::WheelScrolled { delta } => {
                let Some(p) = cursor.position_in(bounds) else {
                    return (IGNORED, None);
                };
                let (y, pixels) = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => (y, false),
                    mouse::ScrollDelta::Pixels { y, .. } => (y, true),
                };
                let (col, row) = pointer::cell_at(p.x, p.y, self.cell.0, self.cell.1);
                let rows = pointer::wheel_rows(y, pixels, self.cell.1);
                (
                    CAPTURED,
                    Some(Message::Terminal(TerminalMessage::Wheel(
                        slot,
                        WheelInput { rows, col, row, mods },
                    ))),
                )
            }
            _ => (IGNORED, None),
        }
    }
}

impl canvas::Program<Message> for Pane {
    type State = PaneState;

    fn update(
        &self,
        state: &mut PaneState,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<canvas::Action<Message>> {
        match self.route(state, event.clone(), bounds, cursor) {
            (false, None) => None,
            (true, None) => Some(canvas::Action::capture()),
            (false, Some(message)) => Some(canvas::Action::publish(message)),
            (true, Some(message)) => Some(canvas::Action::publish(message).and_capture()),
        }
    }

    fn draw(
        &self,
        _state: &PaneState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let (cw, ch) = self.cell;
        let f = &self.frame;
        let size = bounds.size();
        let mut out = Vec::with_capacity(f.rows.len() + 2);
        // While the window is translucent the panel body's base layer paints
        // this (see `view`); drawing it here too would stack two layers.
        if !t::translucent() {
            out.push(
                self.bg
                    .draw(renderer, size, |fr| fr.fill_rectangle(Point::ORIGIN, size, col(f.bg))),
            );
        }
        let base = Font::with_name(self.font.family);
        let font_size = self.font.size;
        for (y, data) in f.rows.iter().enumerate() {
            let Some(cache) = self.caches.get(y) else {
                break;
            };
            let py = y as f32 * ch;
            out.push(cache.draw(renderer, size, |fr| {
                for g in &data.bgs {
                    fr.fill_rectangle(
                        Point::new(g.col as f32 * cw, py),
                        Size::new(g.len as f32 * cw, ch),
                        col(g.color),
                    );
                }
                if let Some((from, to)) = data.selected {
                    fr.fill_rectangle(
                        Point::new(from as f32 * cw, py),
                        Size::new((to - from + 1) as f32 * cw, ch),
                        col(f.selection_bg),
                    );
                }
                for run in &data.runs {
                    draw_run(fr, run, base, font_size, cw, ch, py, None, f.bg);
                }
            }));
        }
        out.push(self.hover_cache.draw(renderer, size, |fr| {
            let Some(link) = &self.hover else {
                return;
            };
            for &(row, from, len) in &link.spans {
                fr.fill_rectangle(
                    Point::new(from as f32 * cw, row as f32 * ch + ch - 2.0),
                    Size::new(len as f32 * cw, 1.0),
                    col(f.fg),
                );
            }
        }));
        out.push(self.cursor.draw(renderer, size, |fr| {
            let Some(c) = f.cursor else {
                return;
            };
            let (px, py) = (c.x as f32 * cw, c.y as f32 * ch);
            let color = col(c.color);
            match (self.focused, c.shape) {
                (false, _) => fr.stroke(
                    &canvas::Path::rectangle(
                        Point::new(px + 0.5, py + 0.5),
                        Size::new(cw - 1.0, ch - 1.0),
                    ),
                    canvas::Stroke::default().with_color(color).with_width(1.0),
                ),
                (_, CursorShape::Bar) => {
                    fr.fill_rectangle(Point::new(px, py), Size::new(2.0, ch), color)
                }
                (_, CursorShape::Underline) => {
                    fr.fill_rectangle(Point::new(px, py + ch - 2.0), Size::new(cw, 2.0), color)
                }
                (_, CursorShape::Block) => {
                    fr.fill_rectangle(Point::new(px, py), Size::new(cw, ch), color);
                    // Redraw the covered character in the background colour.
                    let covered = f.rows.get(c.y as usize).and_then(|data| {
                        data.runs
                            .iter()
                            .find(|r| c.x >= r.col && c.x < r.col + r.cells)
                    });
                    if let Some(run) = covered {
                        let chr = if run.ascii {
                            run.text.chars().nth((c.x - run.col) as usize)
                        } else {
                            run.text.chars().next()
                        };
                        if let Some(chr) = chr {
                            let one = Run {
                                col: c.x,
                                text: chr.to_string(),
                                cells: 1,
                                ..run.clone()
                            };
                            draw_run(fr, &one, base, font_size, cw, ch, py, Some(f.bg), f.bg);
                        }
                    }
                }
            }
        }));
        out
    }

    fn mouse_interaction(&self, _s: &PaneState, bounds: Rectangle, cursor: Cursor) -> mouse::Interaction {
        if self.hover.is_some() && cursor.is_over(bounds) {
            mouse::Interaction::Pointer
        } else if cursor.is_over(bounds) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::default()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_run(
    fr: &mut canvas::Frame,
    run: &Run,
    base: Font,
    font_size: f32,
    cw: f32,
    ch: f32,
    py: f32,
    over: Option<Rgb>,
    bg: Rgb,
) {
    let x = run.col as f32 * cw;
    let mut c = col(over.unwrap_or(run.fg));
    if run.faint {
        let b = col(bg);
        c = Color::from_rgb((c.r + b.r) / 2.0, (c.g + b.g) / 2.0, (c.b + b.b) / 2.0);
    }
    let font = Font {
        weight: if run.bold { font::Weight::Bold } else { font::Weight::Normal },
        style: if run.italic { font::Style::Italic } else { font::Style::Normal },
        ..base
    };
    if !run.text.trim_end().is_empty() {
        fr.fill_text(Text {
            content: run.text.clone(),
            position: Point::new(x, py),
            color: c,
            size: Pixels(font_size),
            line_height: iced::widget::text::LineHeight::Absolute(Pixels(ch)),
            font,
            align_x: Horizontal::Left.into(),
            align_y: Vertical::Top,
            shaping: if run.ascii {
                iced::widget::text::Shaping::Basic
            } else {
                iced::widget::text::Shaping::Advanced
            },
            max_width: f32::INFINITY,
        });
    }
    let w = run.cells as f32 * cw;
    if run.underline {
        fr.fill_rectangle(Point::new(x, py + ch - 2.0), Size::new(w, 1.0), c);
    }
    if run.strike {
        fr.fill_rectangle(Point::new(x, py + ch / 2.0), Size::new(w, 1.0), c);
    }
}

/// Pixel size of one cell for the Terminal font: width of an "M", and a line
/// height of 1.25 em. Needs the font loaded, so call it after startup.
pub fn measure_cell(font: TermFont) -> (f32, f32) {
    type P = <Renderer as adv_text::Renderer>::Paragraph;
    let ch = font.line_height();
    let n = 100.0;
    let p = P::with_text(adv_text::Text {
        content: &"M".repeat(n as usize),
        bounds: Size::INFINITE,
        size: Pixels(font.size),
        line_height: iced::widget::text::LineHeight::Absolute(Pixels(ch)),
        font: Font::with_name(font.family),
        align_x: Horizontal::Left.into(),
        align_y: Vertical::Top,
        shaping: iced::widget::text::Shaping::Basic,
        wrapping: iced::widget::text::Wrapping::None,
    });
    (p.min_bounds().width / n, ch)
}

/// Everything the panel view needs, as plain borrowed data.
pub struct PanelView<'a> {
    pub layout: Layout,
    /// One entry per pane of the layout, in slot order.
    pub slots: Vec<PaneView<'a>>,
    /// The team's Terminals, in strip order.
    pub items: Vec<StripItem>,
    /// A line for the strip, such as an exited notice or a spawn error.
    pub note: Option<String>,
    /// The strip's starting folder and whose it is ("Project folder: api"),
    /// if one applies.
    pub dir_label: Option<String>,
    /// Whether the strip has its own folder to clear: false on a project's
    /// strip that uses the team's.
    pub can_clear_dir: bool,
    pub dock: Dock,
    /// Panel height docked at the bottom.
    pub height: f32,
    /// Panel width docked right.
    pub width: f32,
}

/// One pane of the panel.
pub struct PaneView<'a> {
    pub slot: usize,
    /// The canvas to draw, or `None` for an empty pane.
    pub pane: Option<&'a Pane>,
    /// The pane that takes keys.
    pub focused: bool,
    /// Under 20x4 cells: say so instead of drawing.
    pub too_small: bool,
    /// For an empty pane: the team's Terminals no pane shows.
    pub offer: Vec<(TerminalId, String)>,
    /// The Terminal whose shell exited, to say so over its last screen.
    pub notice: Option<(TerminalId, PaneNotice)>,
}

/// One Terminal in the strip.
pub struct StripItem {
    pub id: TerminalId,
    pub name: String,
    /// The Terminal the pane is showing.
    pub active: bool,
    pub exited: bool,
    /// What its program is waiting on the person for, shown as a dot.
    pub need: Option<crate::terminal::Need>,
    /// The text being typed while this Terminal is renamed.
    pub editing: Option<String>,
}

/// An ask-first dialog for closing a busy Terminal or quitting with one.
pub fn confirm_modal<'a>(
    title: &'a str,
    body: String,
    confirm_label: &'a str,
    confirm: Message,
) -> Element<'a, Message> {
    confirm_modal_with(title, body, confirm_label, confirm, "Cancel", Message::CloseModal)
}

/// [`confirm_modal`] with its own label and message for the dismiss button.
pub fn confirm_modal_with<'a>(
    title: &'a str,
    body: String,
    confirm_label: &'a str,
    confirm: Message,
    cancel_label: &'a str,
    cancel: Message,
) -> Element<'a, Message> {
    let content = column![]
        .spacing(16)
        .push(text(title).size(18).color(t::text_primary()))
        .push(text(body).size(14).color(t::text_secondary()))
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text(confirm_label).size(14).color(Color::WHITE))
                        .on_press(confirm)
                        .padding([8, 24])
                        .style(|_theme: &Theme, _status| button::Style {
                            background: Some(iced::Background::Color(t::danger())),
                            text_color: Color::WHITE,
                            border: iced::Border { radius: 4.0.into(), ..Default::default() },
                            ..Default::default()
                        }),
                )
                .push(
                    button(text(cancel_label).size(14))
                        .on_press(cancel)
                        .padding([8, 24])
                        .style(button::secondary),
                ),
        );
    let card = container(content).padding(28).style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_modal())),
        border: iced::Border { color: t::border(), width: 1.0, radius: 8.0.into() },
        ..Default::default()
    });
    crate::views::util::modal_backdrop(card)
        .into()
}

const RENAME_KEY: &str = "terminal-rename";

pub fn rename_input_id() -> iced::widget::Id {
    crate::undo_input::id(RENAME_KEY)
}

fn strip_button<'a>(item: StripItem) -> Element<'a, Message> {
    if let Some(draft) = item.editing {
        return crate::undo_input::text_input(RENAME_KEY, "Name", &draft, |s| {
            Message::Terminal(TerminalMessage::RenameInput(s))
        })
            .size(12)
            .width(140)
            .padding([1, 6])
            .on_submit(Message::Terminal(TerminalMessage::RenameSubmit))
            .into();
    }
    let label = if item.exited { format!("{} (exited)", item.name) } else { item.name };
    let (active, id) = (item.active, item.id);
    let content = row![text(label).size(12)]
        .push(item.need.map(attention_dot))
        .spacing(6)
        .align_y(iced::Alignment::Center);
    button(content)
        .padding([2, 8])
        // Choosing the Terminal already shown renames it.
        .on_press(Message::Terminal(if active {
            TerminalMessage::RenameStart(id)
        } else {
            TerminalMessage::Select(id)
        }))
        .style(move |_theme: &Theme, _status| button::Style {
            background: active.then(|| iced::Background::Color(t::bg_card_focused())),
            text_color: if active { t::text_primary() } else { t::text_secondary() },
            border: iced::Border { radius: 4.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

/// Diameter of the attention dot.
const DOT: f32 = 7.0;

/// The dot a Terminal, a Tab or a team row shows while a Terminal's program
/// waits on the person: the warning color when it is blocked or failed, the
/// accent color when it finished or asked for a look.
pub fn attention_dot<'a>(need: crate::terminal::Need) -> Element<'a, Message> {
    let color = match need {
        crate::terminal::Need::Error => t::danger(),
        n if n.urgent() => t::warning(),
        _ => t::accent(),
    };
    container(iced::widget::Space::new().width(DOT).height(DOT))
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(color)),
            border: iced::Border { radius: (DOT / 2.0).into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

/// "Shell exited" over the bottom of the last screen, with Restart and Close.
fn exited_bar<'a>(id: TerminalId, kind: PaneNotice) -> Element<'a, Message> {
    let (message, restart) = match kind {
        PaneNotice::Exited => ("Shell exited", true),
        PaneNotice::Dead => ("This Terminal stopped working. Close it and open a new one", false),
    };
    let mut actions = row![text(message).size(12).color(t::text_primary())]
        .spacing(10)
        .align_y(iced::Alignment::Center);
    if restart {
        actions = actions.push(
            button(text("Restart").size(12))
                .padding([2, 10])
                .on_press(Message::Terminal(TerminalMessage::Restart(id)))
                .style(button::secondary),
        );
    }
    actions = actions.push(
        button(text("Close").size(12))
            .padding([2, 10])
            .on_press(Message::Terminal(TerminalMessage::CloseConfirmed(id)))
            .style(button::secondary),
    );
    let bar = container(actions)
    .padding([6, 12])
    .width(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_menu())),
        border: iced::Border { color: t::border(), width: 1.0, radius: 0.0.into() },
        ..Default::default()
    });
    container(bar).align_bottom(Length::Fill).width(Length::Fill).height(Length::Fill).into()
}

/// One pane: its canvas (with the exited bar over it), or the picker for an
/// empty pane. A focused pane in a multi-pane layout has an accent outline.
fn pane_body<'a>(pv: PaneView<'a>) -> Element<'a, Message> {
    let slot = pv.slot;
    let inner: Element<'a, Message> = match pv.pane {
        Some(_) if pv.too_small => container(text("Too small").size(12).color(t::text_muted()))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into(),
        Some(pane) => {
            let canvas = Canvas::new(pane).width(Length::Fill).height(Length::Fill);
            match pv.notice {
                Some((id, kind)) => iced::widget::stack![canvas, exited_bar(id, kind)].into(),
                None => canvas.into(),
            }
        }
        None => {
            let mut picks = column![].spacing(6).align_x(iced::Alignment::Center);
            for (id, name) in pv.offer {
                picks = picks.push(
                    button(text(name).size(13))
                        .padding([6, 14])
                        .on_press(Message::Terminal(TerminalMessage::Pick(slot, id)))
                        .style(button::secondary),
                );
            }
            picks = picks.push(
                button(text("New Terminal").size(13))
                    .padding([6, 14])
                    .on_press(Message::Terminal(TerminalMessage::NewIn(slot))),
            );
            container(picks).center_x(Length::Fill).center_y(Length::Fill).into()
        }
    };
    let focused = pv.focused;
    container(inner)
        .width(Length::FillPortion(1))
        .height(Length::Fill)
        .style(move |_theme: &Theme| container::Style {
            border: iced::Border {
                color: if focused { t::accent() } else { iced::Color::TRANSPARENT },
                width: if focused { 1.0 } else { 0.0 },
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
}

pub fn view<'a>(p: PanelView<'a>) -> Element<'a, Message> {
    // The Terminals (and the note) scroll sideways when they don't fit; the
    // controls after them stay put. A press on empty strip still starts the
    // height drag below, and the buttons capture their own presses.
    let mut names = row![].spacing(6).align_y(iced::Alignment::Center);
    for item in p.items {
        let id = item.id;
        let editing = item.editing.is_some();
        names = names.push(strip_button(item));
        if !editing {
            names = names.push(
                button(text("\u{00d7}").size(13))
                    .padding([0, 4])
                    .on_press(Message::Terminal(TerminalMessage::Close(id)))
                    .style(button::text),
            );
        }
    }
    if let Some(note) = p.note {
        names = names.push(text(note).size(12).color(t::text_muted()));
    }
    let names = scrollable::Scrollable::with_direction(
        names,
        scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(3).scroller_width(3).margin(0),
        ),
    )
    .width(Length::Fill);
    let mut strip = row![names].spacing(6).align_y(iced::Alignment::Center);
    strip = strip.push(
        button(text("+").size(13))
            .padding([0, 8])
            .on_press(Message::Terminal(TerminalMessage::New))
            .style(button::text),
    );
    for layout in [Layout::Single, Layout::Columns, Layout::Grid] {
        let label = layout.label(p.dock);
        let on = layout == p.layout;
        strip = strip.push(
            button(text(label).size(11))
                .padding([2, 6])
                .on_press(Message::Terminal(TerminalMessage::SetLayout(layout)))
                .style(move |_theme: &Theme, _status| button::Style {
                    background: on.then(|| iced::Background::Color(t::bg_card_focused())),
                    text_color: if on { t::text_primary() } else { t::text_secondary() },
                    border: iced::Border { radius: 4.0.into(), ..Default::default() },
                    ..Default::default()
                }),
        );
    }
    // Dock the panel at the other side (also in the palette). Device-wide.
    strip = strip.push(
        button(
            text(match p.dock {
                Dock::Bottom => "Dock right",
                Dock::Right => "Dock bottom",
            })
            .size(11),
        )
        .padding([2, 8])
        .on_press(Message::ToggleTerminalDock)
        .style(button::text),
    );
    strip = strip
        .push(
            button(text(p.dir_label.unwrap_or_else(|| "Folder".to_string())).size(11))
            .padding([2, 8])
            .on_press(Message::Terminal(TerminalMessage::PickDir))
            .style(button::text),
        );
    if p.can_clear_dir {
        // New shells start in the team's folder, or home, again.
        strip = strip.push(
            button(text("\u{00d7}").size(13))
                .padding([0, 4])
                .on_press(Message::Terminal(TerminalMessage::ClearDir))
                .style(button::text),
        );
    }
    if p.dock == Dock::Bottom {
        strip = strip.push(text("Cmd+J").size(11).color(t::text_muted()));
    }
    let strip = container(strip)
        .padding([0, 10])
        .height(STRIP_HEIGHT)
        .center_y(STRIP_HEIGHT)
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::base(t::bg_menu()))),
            border: iced::Border { color: t::border(), width: 1.0, radius: 0.0.into() },
            ..Default::default()
        });

    // Docked at the bottom, dragging the strip resizes the panel (JRST-397).
    // Docked right, the left edge does.
    let strip: Element<'a, Message> = match p.dock {
        Dock::Bottom => iced::widget::mouse_area(strip)
            .on_press(Message::Terminal(TerminalMessage::DragStart))
            .interaction(mouse::Interaction::ResizingVertically)
            .into(),
        Dock::Right => strip.into(),
    };

    let (cols, _) = p.layout.grid(p.dock);
    let mut slots = p.slots.into_iter();
    let mut grid = column![].spacing(PANE_GAP).width(Length::Fill).height(Length::Fill);
    loop {
        let mut line = row![].spacing(PANE_GAP).width(Length::Fill).height(Length::FillPortion(1));
        let mut any = false;
        for pv in slots.by_ref().take(cols) {
            line = line.push(pane_body(pv));
            any = true;
        }
        if !any {
            break;
        }
        grid = grid.push(line);
    }
    // While translucent, one base layer behind every pane (the panes skip
    // their own backgrounds); opaque, the panes paint as before.
    let grid = container(grid).width(Length::Fill).height(Length::Fill).style(|_theme: &Theme| container::Style {
        background: t::translucent().then(|| iced::Background::Color(t::base(t::bg_primary()))),
        ..Default::default()
    });
    match p.dock {
        Dock::Bottom => column![strip, grid].width(Length::Fill).height(Length::Fixed(p.height)).into(),
        Dock::Right => {
            // Pane sizes come from the body as laid out: its height depends on
            // the bars above it, which the Terminal state doesn't know.
            let body = crate::views::measure::measure(grid, |size| {
                Message::Terminal(TerminalMessage::BodySize(size))
            });
            let handle = iced::widget::mouse_area(
                container(iced::widget::Space::new().width(Length::Fixed(HANDLE_WIDTH)).height(Length::Fill))
                    .width(Length::Fixed(HANDLE_WIDTH))
                    .height(Length::Fill)
                    .style(|_theme: &Theme| container::Style {
                        background: Some(iced::Background::Color(t::border())),
                        ..Default::default()
                    }),
            )
            .on_press(Message::Terminal(TerminalMessage::DragStart))
            .interaction(mouse::Interaction::ResizingHorizontally);
            row![handle, column![strip, body].width(Length::Fill).height(Length::Fill)]
                .width(Length::Fixed(p.width))
                .height(Length::Fill)
                .into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_of(text: &str) -> Arc<RowData> {
        Arc::new(RowData {
            runs: vec![Run {
                col: 0,
                text: text.into(),
                cells: text.len() as u16,
                fg: [1, 2, 3],
                bold: false,
                italic: false,
                faint: false,
                underline: false,
                strike: false,
                ascii: true,
            }],
            bgs: vec![],
            ..Default::default()
        })
    }

    #[test]
    fn only_rows_with_new_data_count_as_changed() {
        let (a, b, c) = (row_of("a"), row_of("b"), row_of("c"));
        let prev = vec![a.clone(), b.clone(), c.clone()];
        let next = vec![a.clone(), row_of("b2"), c.clone()];
        assert_eq!(changed_rows(&prev, &next), vec![1]);
        assert_eq!(changed_rows(&prev, &prev.clone()), Vec::<usize>::new());
    }

    #[test]
    fn a_new_row_count_changes_every_row() {
        let a = row_of("a");
        assert_eq!(changed_rows(std::slice::from_ref(&a), &[a.clone(), a.clone()]), vec![0, 1]);
        assert_eq!(changed_rows(&[], &[a]), vec![0]);
    }

    #[test]
    fn showing_a_frame_keeps_one_cache_per_row() {
        let mut pane = Pane::new();
        let frame = Frame { cols: 80, rows: vec![row_of("x"); 24], ..Default::default() };
        pane.show(Arc::new(frame));
        assert_eq!(pane.caches.len(), 24);
        assert_eq!(pane.frame().rows.len(), 24);
    }
}
