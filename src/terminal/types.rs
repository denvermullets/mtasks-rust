//! Plain data that crosses the Terminal API. No type from `libghostty-vt` or
//! `portable-pty` appears here; `engine.rs` converts at the boundary.
use std::sync::Arc;

pub type Rgb = [u8; 3];

/// Local id of one Terminal, unique for the life of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TerminalId(pub u64);

/// A run of cells on one row sharing a style.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub col: u16,
    pub text: String,
    pub cells: u16,
    pub fg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub underline: bool,
    pub strike: bool,
    pub ascii: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BgRun {
    pub col: u16,
    pub len: u16,
    pub color: Rgb,
}

#[derive(Debug, Default, PartialEq)]
pub struct RowData {
    pub runs: Vec<Run>,
    pub bgs: Vec<BgRun>,
    /// Selected cells on this row, as an inclusive column range.
    pub selected: Option<(u16, u16)>,
    /// The text soft-wraps onto the next row (one logical line over several rows).
    pub wrapped: bool,
    /// Links the program marked itself (OSC 8).
    pub links: Vec<LinkSpan>,
}

/// Cells of one row that an OSC 8 link covers.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkSpan {
    pub col: u16,
    pub len: u16,
    pub uri: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorData {
    pub x: u16,
    pub y: u16,
    pub shape: CursorShape,
    pub color: Rgb,
}

/// One drawable snapshot of a Terminal's screen. Unchanged rows are shared
/// (`Arc`) with the previous frame, so a row cache can compare by pointer.
#[derive(Debug, Default, Clone)]
pub struct Frame {
    pub cols: u16,
    pub rows: Vec<Arc<RowData>>,
    pub bg: Rgb,
    pub fg: Rgb,
    /// Colour behind selected cells (the theme's selection colour).
    pub selection_bg: Rgb,
    pub cursor: Option<CursorData>,
    pub title: String,
    pub scrollback: usize,
    /// The shell has exited. The last screen stays readable.
    pub exited: bool,
    /// The owner thread panicked; this Terminal will not update again.
    pub dead: bool,
    /// A program is capturing the mouse.
    pub tracking: bool,
    pub alt: bool,
}

impl Frame {
    /// The screen as text, one string per row, with blanks as spaces and no
    /// trailing spaces. Used by tests and by anything that needs the content.
    pub fn lines(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                let mut line = String::new();
                let mut width = 0usize;
                for run in &row.runs {
                    while width < run.col as usize {
                        line.push(' ');
                        width += 1;
                    }
                    line.push_str(&run.text);
                    width += run.cells as usize;
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    pub fn contains_line(&self, text: &str) -> bool {
        self.lines().iter().any(|l| l.trim() == text)
    }
}

#[derive(Debug, Clone)]
pub struct TermTheme {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    /// Selection highlight, drawn behind selected cells.
    pub selection: Rgb,
    /// A light theme: answers `CSI ?996n`.
    pub light: bool,
    pub ansi: [Rgb; 16],
}

/// A physical key, as the app's key handler sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermKey {
    /// A letter, digit or punctuation key, named by the character it types unshifted.
    Char(char),
    Space,
    Enter,
    Escape,
    Backspace,
    Tab,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    /// F1 to F12.
    F(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub logo: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyInput {
    pub key: TermKey,
    pub mods: KeyMods,
    /// The text the key produced, if any (already shifted).
    pub text: Option<String>,
    /// Treat Option as Alt.
    pub option_as_alt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press(MouseButton),
    Release(MouseButton),
    Move,
}

/// A pointer event over a pane, in cells. `col` and `row` are signed because a
/// drag can leave the pane; reporting clamps them to the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseInput {
    pub action: MouseAction,
    pub col: i32,
    pub row: i32,
    pub mods: KeyMods,
}

/// Wheel or trackpad movement over a pane. `rows` is positive toward older
/// output and may be fractional; the engine adds up fractions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelInput {
    pub rows: f32,
    pub col: i32,
    pub row: i32,
    pub mods: KeyMods,
}

/// What `close` did, once it finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseReport {
    /// The shell ignored SIGHUP and had to be killed after the grace period.
    pub escalated_to_sigkill: bool,
    /// How many of the Terminal's three threads were joined (3 when all exited).
    pub threads_exited: usize,
    /// The shell was reaped. False only when it was still in the kernel 5 seconds
    /// after SIGKILL; `close` gives up rather than block.
    pub reaped: bool,
}

pub struct SpawnConfig {
    /// Team the Terminal belongs to (local team id).
    pub team: String,
    pub cols: u16,
    pub rows: u16,
    pub theme: TermTheme,
    /// Falls back to the home folder when absent or missing.
    pub cwd: Option<std::path::PathBuf>,
    /// Program and arguments. `None` runs the user's login shell.
    pub program: Option<(String, Vec<String>)>,
}

/// Whose strip a Terminal is in (ADR 0003): a team's own (`project: None`), or
/// one of its projects'. Ids are local ids. A project's Terminals still belong
/// to its team: the team's sidebar count includes them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scope {
    pub team: String,
    pub project: Option<String>,
}

impl Scope {
    pub fn team(team: impl Into<String>) -> Self {
        Scope { team: team.into(), project: None }
    }

    pub fn project(team: impl Into<String>, project: impl Into<String>) -> Self {
        Scope { team: team.into(), project: Some(project.into()) }
    }
}

/// What a blocked program needs from the user (OSC 7501 `kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// The program didn't say.
    Unspecified,
    /// Approval to do something ("Allow this command?").
    Permission,
    /// An answer to type.
    Question,
    /// A login, password or token.
    Auth,
}

/// What a program says it is doing (OSC 7501 `state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramState {
    Idle,
    Working,
    Done,
    Blocked(BlockKind),
    Error,
    /// Forget what the program said before.
    Clear,
}

/// Something a program in a Terminal asked of the person, from the escape
/// sequences that ask for attention. Text is what the program sent, lossily
/// made UTF-8; empty when it sent none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// A program status report (OSC 7501), as Claude Code sends: working,
    /// blocked on a permission prompt, done, and so on.
    Status { state: ProgramState, app: String, title: String, message: String },
    /// A desktop notification request (OSC 9 or OSC 777).
    Notification { title: String, body: String },
    /// BEL.
    Bell,
}
