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

#[derive(Debug, Clone)]
pub struct KeyInput {
    pub key: TermKey,
    pub mods: KeyMods,
    /// The text the key produced, if any (already shifted).
    pub text: Option<String>,
    /// Treat Option as Alt.
    pub option_as_alt: bool,
}

/// What `close` did, once it finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseReport {
    /// The shell ignored SIGHUP and had to be killed after the grace period.
    pub escalated_to_sigkill: bool,
    /// How many of the Terminal's three threads were joined (3 when all exited).
    pub threads_exited: usize,
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
