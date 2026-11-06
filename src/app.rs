//! Application state machine, event loop, and user actions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::git::{Git, WorktreeState};
use crate::state::{Repository, Shell, State, Worktree};
use crate::status::{self, Compiled, Status};
use crate::theme::Theme;
use crate::tmux::{PaneInfo, Tmux};

/// Lines of the capture used for status classification.
const RECENT_TAIL_LINES: usize = 10;

/// How long a toast stays on screen before it fades by itself. Errors get
/// longer — they are the ones worth reading.
const TOAST_TTL: Duration = Duration::from_secs(4);
const TOAST_TTL_ERR: Duration = Duration::from_secs(6);

/// How often the per-worktree states are recomputed. Each one shells
/// out to git two or three times, so it runs on its own slow cadence rather
/// than on every tmux poll. The fastest thing it reports is an agent
/// dirtying the tree, and a few seconds of lag on "it has started" is not
/// something the user can act on any sooner.
const WT_STATE_INTERVAL: Duration = Duration::from_secs(5);

/// How long a queued launch waits for its shell to draw a prompt before
/// it is typed anyway. A cold zsh with a heavy rc takes a couple of
/// seconds; past this the shell either has no prompt to draw or is wedged,
/// and typing blind is still better than never starting the agent.
const LAUNCH_GRACE: Duration = Duration::from_secs(8);

/// Window name of the pad's `$EDITOR`. No `lw:` prefix: that marks the
/// windows agents live in, and this one holds no shell.
const EDITOR_WINDOW: &str = "todo";

/// Hard ceiling on one text buffer. Not a limit anyone types into —
/// a backstop so a runaway paste cannot grow the buffer without bound.
const TEXTBUF_MAX_BYTES: usize = 1_000_000;

/// Repositories unfold by default up to this many in scope. Past it the tree
/// is longer than any terminal and folding is the readable default.
const UNFOLD_CAP: usize = 30;

/// The first command of a freshly spawned shell, waiting for the shell to
/// be ready for it.
///
/// `new-window` returns as soon as tmux has forked the shell — long before
/// the shell has read its rc files and started its line editor, which on a
/// cold zsh is a good two seconds later. Anything typed into that gap is
/// echoed raw by the tty line discipline, reflowed by the holder split
/// that follows it, and then drawn a SECOND time when the line editor
/// finally picks the buffered line up: the messy first screen (command
/// shown twice, or once, or swallowed by an rc that clears) this exists to
/// prevent. Held here instead, and typed by [`App::drain_pending_launches`]
/// once the pane has drawn something.
#[derive(Debug, Clone)]
struct PendingLaunch {
    pane_id: String,
    command: String,
    /// When to stop waiting for a prompt and type regardless.
    deadline: Instant,
}

/// A transient notice. Rendered as a small box above the statusbar and
/// gone on its own after [`TOAST_TTL`] — nothing on the bar is permanent.
#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    at: Instant,
}

impl Toast {
    fn expired(&self) -> bool {
        let ttl = if self.error { TOAST_TTL_ERR } else { TOAST_TTL };
        self.at.elapsed() >= ttl
    }
}

/// A node in the repositories/worktrees/shells tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sel {
    Repository(usize),
    Worktree(usize, usize),
    Shell(usize, usize, usize),
}

/// A shell docked in another Wingmux instance.
///
/// Held only for the length of one poll: `@lw_docked` on the agent pane is
/// the single source of truth, because it is the only thing two instances
/// that know nothing of each other can both see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    /// The pane the other dashboard is drawing in — what `@lw_docked` holds,
    /// and what proves the lease is live rather than crash wreckage.
    pub dash_pane: String,
    /// The scope that dashboard announces (`@lw_scope` on its window), when
    /// it announces one. What lets us say *whose* it is rather than just
    /// that it is not ours.
    pub scope: Option<String>,
}

impl Lease {
    /// The shortest thing that still names the holder: the last component
    /// of its scope path, which is the folder the user thinks of it as.
    pub fn label(&self) -> String {
        self.scope
            .as_deref()
            .map(|s| {
                std::path::Path::new(s)
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_else(|| s.to_string())
            })
            .unwrap_or_else(|| "another dashboard".to_string())
    }
}

/// What the user is doing right now.
#[derive(Debug)]
pub enum Mode {
    Normal,
    Input {
        text: String,
        /// The second field, when the prompt has one (new worktree: which
        /// agent its first shell runs). `None` for single-field prompts —
        /// `Tab` then has nothing to move to and stays inert.
        agent: Option<AgentField>,
    },
    Confirm {
        confirm: Confirm,
    },
    Help,
    /// Live theme picker (`ctrl-t`): previews as the cursor moves, keeps
    /// on Enter, restores the theme it opened with on Esc.
    ThemePicker {
        index: usize,
    },
    /// Add-shell picker (`a`): what the new shell should run — the
    /// repository's agent command, any other configured agent preset, or
    /// a plain login shell. j/k move, digits `1`..`9` pick an entry
    /// directly, Enter takes the selected one, Esc closes. A picker, not
    /// a prompt: "start a normal shell" is one keystroke, and anything
    /// else you want to run you type in that shell.
    AddShell {
        index: usize,
    },
    /// Predefined-message modal (`p`): the configured messages as a list.
    /// j/k move, digits `1`..`9` pick an entry directly, Enter sends the
    /// selected one, Esc closes. Sending pastes like a human would type.
    Messages {
        index: usize,
    },
    /// Startup diagnostics overlay (shown when configuration problems were
    /// dropped at load — see the configuration guidelines). Scrollable list
    /// of diagnostics, dismissed with Esc/q/Enter.
    Diagnostics,
    /// A todo pad (`t` repository, `T` global), edited **in place** in the tree's own panel
    /// rather than in an overlay: a scratch pad you keep open while
    /// reading the tree beside it is not a dialog, and a dialog would
    /// cover the preview it is written about.
    ///
    /// The panel is the editor, so it owns every printable key — there is
    /// no navigation layer left underneath, and `Esc` is the way back to
    /// the tree (saving as it goes). It keeps only ctrl-c (quit) and
    /// ctrl-t (theme) global.
    Todo {
        buf: TextBuf,
        pad: crate::todo::Pad,
    },
}

/// A multiline text buffer for the todo pads: lines + a cursor
/// (`row`, and `col` as a BYTE offset into that line — the popup slices
/// `line[..col]` to place the caret) + a viewport scroll (first visible
/// line). Every motion lands on a char boundary: prompts are full of
/// em-dashes and typographic quotes, and a `col` inside a multi-byte
/// character panics the next edit that indexes with it.
/// Editing happens at the cursor; `text()` joins the lines for sending.
#[derive(Debug, Clone)]
pub struct TextBuf {
    lines: Vec<String>,
    row: usize,
    col: usize,
    scroll: usize,
}

impl Default for TextBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl TextBuf {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            scroll: 0,
        }
    }

    /// The full buffer joined with newlines — what gets sent to the agent.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Logical line count. The renderer walks `visual_rows` instead —
    /// this is the buffer-level view, used by the editing tests.
    #[cfg(test)]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, i: usize) -> &str {
        self.lines.get(i).map(String::as_str).unwrap_or("")
    }

    /// Logical cursor position. The renderer uses `visual_cursor`, which
    /// accounts for soft-wrapping; these are the buffer-level view, read by
    /// the editing tests and by pad reloads.
    pub fn cursor_row(&self) -> usize {
        self.row
    }

    pub fn cursor_col(&self) -> usize {
        self.col
    }

    /// Rebuild a buffer parked in a previous run: text plus the cursor it
    /// was left at. Both coordinates are clamped — a pad edited outside the
    /// dashboard must not place `col` past the end of a
    /// line or inside a multi-byte character, which the next edit would
    /// panic on.
    pub fn from_parked(text: &str, row: usize, col: usize) -> TextBuf {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        let row = row.min(lines.len().saturating_sub(1));
        let line = &lines[row];
        let mut col = col.min(line.len());
        while col > 0 && !line.is_char_boundary(col) {
            col -= 1;
        }
        TextBuf {
            lines,
            row,
            col,
            scroll: 0,
        }
    }

    /// First visible line (clamped so the cursor is always on screen).
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Page height for scroll motions (visible lines, minus the hint row).
    pub fn page(&self, visible: usize) -> usize {
        visible.saturating_sub(1).max(1)
    }

    /// Bytes the buffer would send, newlines included — what
    /// [`TEXTBUF_MAX_BYTES`] bounds.
    fn byte_len(&self) -> usize {
        self.lines.iter().map(String::len).sum::<usize>() + self.lines.len().saturating_sub(1)
    }

    /// Is there room for `n` more bytes? Growth past the cap is dropped
    /// silently: an error toast per keystroke would be worse than a buffer
    /// that stops accepting input a megabyte in.
    fn has_room(&self, n: usize) -> bool {
        self.byte_len() + n <= TEXTBUF_MAX_BYTES
    }

    pub fn insert_char(&mut self, c: char) {
        if !self.has_room(c.len_utf8()) {
            return;
        }
        self.lines[self.row].insert(self.col, c);
        self.col += c.len_utf8();
    }

    pub fn newline(&mut self) {
        if !self.has_room(1) {
            return;
        }
        let rest = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    pub fn insert_tab(&mut self) {
        self.insert_char('\t');
    }

    /// Backspace: delete the char before the cursor; at line start, join
    /// with the previous line.
    pub fn backspace(&mut self) {
        if self.col > 0 {
            let prev = prev_boundary(&self.lines[self.row], self.col);
            self.lines[self.row].remove(prev);
            self.col = prev;
        } else if self.row > 0 {
            let prev_len = self.lines[self.row - 1].len();
            let rest = self.lines.remove(self.row);
            self.lines[self.row - 1].push_str(&rest);
            self.row -= 1;
            self.col = prev_len;
        }
    }

    /// Delete forward: the char under the cursor; at line end, join with
    /// the next line.
    pub fn delete(&mut self) {
        let line_len = self.lines[self.row].len();
        if self.col < line_len {
            self.lines[self.row].remove(self.col);
        } else if self.row + 1 < self.lines.len() {
            let rest = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&rest);
        }
    }

    pub fn move_left(&mut self) {
        if self.col > 0 {
            self.col = prev_boundary(&self.lines[self.row], self.col);
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len();
        }
    }

    pub fn move_right(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.col = next_boundary(&self.lines[self.row], self.col);
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    pub fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.clamp_col();
        }
    }

    pub fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.clamp_col();
        }
    }

    pub fn move_home(&mut self) {
        self.col = 0;
    }

    pub fn move_end(&mut self) {
        self.col = self.lines[self.row].len();
    }

    /// Page motion: move the cursor up/down by `page` lines (clamped to
    /// the buffer start/end). Returns false when already at the edge.
    pub fn move_page(&mut self, direction: isize, page: usize) -> bool {
        let target = if direction > 0 {
            (self.row + page).min(self.lines.len() - 1)
        } else {
            self.row.saturating_sub(page)
        };
        if target == self.row {
            return false;
        }
        self.row = target;
        self.clamp_col();
        true
    }

    /// Clamp the cursor into the current line, then back onto a char
    /// boundary — moving between lines of different widths can otherwise
    /// drop it inside a multi-byte character.
    fn clamp_col(&mut self) {
        let line = &self.lines[self.row];
        let mut col = self.col.min(line.len());
        while col > 0 && !line.is_char_boundary(col) {
            col -= 1;
        }
        self.col = col;
    }

    /// Keep the cursor row visible within a `visible`-row viewport, in
    /// VISUAL rows (soft-wrapped at `width` columns): scroll down if the
    /// cursor moved above the viewport, up if it moved below it. The key
    /// handler calls this once per key — only it knows the panel's real
    /// content height.
    pub fn ensure_visible(&mut self, visible: usize, width: usize) {
        let view = visible.max(1);
        let total = self.visual_len(width);
        self.scroll = self.scroll.min(total.saturating_sub(view));
        let (row, _) = self.visual_cursor(width);
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + view {
            self.scroll = row + 1 - view;
        }
    }

    /// Every visual row of the buffer at `width` columns, as
    /// `(logical row, byte start, byte end)` — what the renderer walks
    /// from `scroll()` down.
    pub fn visual_rows(&self, width: usize) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        for (r, line) in self.lines.iter().enumerate() {
            for (s, e) in wrap_line(line, width) {
                out.push((r, s, e));
            }
        }
        out
    }

    /// Number of visual rows at `width` columns.
    pub fn visual_len(&self, width: usize) -> usize {
        self.lines.iter().map(|l| wrap_line(l, width).len()).sum()
    }

    /// The cursor as `(visual row, rendered column)` at `width` columns.
    pub fn visual_cursor(&self, width: usize) -> (usize, usize) {
        let before: usize = self.lines[..self.row]
            .iter()
            .map(|l| wrap_line(l, width).len())
            .sum();
        let line = self.line(self.row);
        let segs = wrap_line(line, width);
        for (i, &(s, e)) in segs.iter().enumerate() {
            if self.col < e || i + 1 == segs.len() {
                return (before + i, display_width(&line[s..self.col.max(s)]));
            }
        }
        (before, 0)
    }
}

/// Rendered width of `s`: tabs take four columns, every other character
/// one (the pad renders tabs as four spaces).
fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

/// Soft-wrap one logical line into byte ranges `[start, end)` that each
/// render within `width` columns. Breaks after a space where the line
/// offers one, hard-breaks words longer than the width. Always yields at
/// least one range, so an empty line still occupies one visual row.
fn wrap_line(line: &str, width: usize) -> Vec<(usize, usize)> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut start = 0;
    let mut cols = 0;
    // Byte index just past the last space seen in the current row — the
    // preferred break point.
    let mut last_break: Option<usize> = None;
    for (i, c) in line.char_indices() {
        let w = if c == '\t' { 4 } else { 1 };
        if cols + w > width && i > start {
            let cut = last_break.filter(|&b| b > start && b <= i).unwrap_or(i);
            out.push((start, cut));
            start = cut;
            cols = display_width(&line[cut..i]);
            last_break = None;
        }
        cols += w;
        if c == ' ' || c == '\t' {
            last_break = Some(i + c.len_utf8());
        }
    }
    out.push((start, line.len()));
    out
}

/// Byte offset of the char boundary before `col` (the start of the char
/// the cursor sits after), or 0.
fn prev_boundary(line: &str, col: usize) -> usize {
    line[..col]
        .chars()
        .next_back()
        .map(|c| col - c.len_utf8())
        .unwrap_or(0)
}

/// Byte offset of the char boundary after `col`, or `col` at end of line.
fn next_boundary(line: &str, col: usize) -> usize {
    line[col..]
        .chars()
        .next()
        .map(|c| col + c.len_utf8())
        .unwrap_or(col)
}

/// Content rows of the todo panel: its share of the tree's column, less the
/// panel's two borders. The pad is a panel split off the tree's column, so
/// its geometry is [`crate::ui::todo_panel_h`]'s.
///
/// Read while the pad is *open*, which is the only time the caret needs a
/// window, so this is the open half of the column rather than the collapsed
/// height a parked pad draws at.
pub(crate) fn todo_visible_lines(app: &App) -> usize {
    let h = crate::ui::todo_panel_h(app, crate::ui::main_h(app));
    (h.saturating_sub(2) as usize).max(1)
}

/// Soft-wrap width of the todo panel's text: the panel width minus its two
/// borders, minus one more so a cursor at the end of a full row still has a
/// cell of its own.
/// A frame too small for a tree has no panel to wrap into, so it falls back
/// to the narrowest strip the dashboard ever draws in.
pub(crate) fn todo_wrap_width(app: &App) -> usize {
    let w = crate::ui::tree_width_for(app).unwrap_or(crate::dock::DASH_MIN_WIDTH);
    (w.saturating_sub(3) as usize).max(1)
}

/// An action reachable from the statusbar shortcut entries (and by mouse
/// click). Same words as the help overlay — learning one teaches the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarAction {
    Help,
    /// Open the selected repository's todo pad (`t`).
    Todo,
    /// Leave the pad, keeping what is in it (`Esc`).
    TodoBack,
    /// Hand the open pad to `$EDITOR` (`ctrl-e`).
    TodoEdit,
    NewWorktree,
    AddShell,
    PredefinedSend,
    DeleteWorktree,
    DeleteShell,
    /// Start a visual selection of shells (`v`).
    VisualStart,
    /// Leave visual mode (`Esc`).
    VisualCancel,
    /// Delete everything selected (`d` in visual mode).
    DeleteMarked,
    /// Close the selected repository's inline shell.
    CloseRepoShell,
    /// Start (if needed) and dock the selected repository's inline shell.
    RepoShell,
    /// Bring the selected shell's real pane beside the tree.
    Dock,
    /// Back to the live tail from a preview parked in the scrollback.
    PreviewLive,
}

/// The new-worktree popup's agent field: a `[[agents]]` preset name (or a
/// command typed verbatim), prefilled with what the repository already
/// runs so the common case is still `n`, a name, `Enter`. `focused` is
/// which of the two fields the keys go to — `Tab` flips it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentField {
    pub text: String,
    pub focused: bool,
}

/// What a new shell runs. The add-shell picker (`a`) offers one of these
/// per row; there is no free-text command prompt, because a plain shell
/// *is* the free-text path — you type the command in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellSpawn {
    /// An agent command, sent to the window's agent shell once it is up.
    Agent(String),
    /// Nothing is sent: the window runs the user's own login shell
    /// (`$SHELL`) and is left at its prompt.
    Plain,
}

/// One row of the add-shell picker: the text it shows and what it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellChoice {
    pub label: String,
    pub spawn: ShellSpawn,
}

#[derive(Debug, Clone)]
pub struct Confirm {
    pub title: String,
    pub body: String,
    /// The plain answer, `None` when it cannot succeed — a dirty worktree
    /// only removes with `--force`, so offering `y`/`d` there would be a
    /// key whose whole job is to fail. An unanswerable question is worse
    /// than a missing option.
    pub yes_label: Option<String>,
    /// A caveat the answer depends on ("uncommitted changes will be
    /// discarded"). Kept out of `body` because it is drawn in the failure
    /// colour: the eye must land on it before the fingers reach a key.
    pub warning: Option<String>,
    /// When `Some`, the `f` key is offered (e.g. `--force` on a dirty
    /// worktree) and this labels it.
    pub force_label: Option<String>,
    pub action: ConfirmAction,
}

/// A visual selection resolved to what `d` does to it.
#[derive(Debug, Clone, Default)]
pub struct Marks {
    pub worktrees: Vec<(usize, usize)>,
    pub shells: Vec<(usize, usize, usize)>,
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteWorktree {
        repository: usize,
        worktree: usize,
    },
    DeleteShell {
        repository: usize,
        worktree: usize,
        shell: usize,
    },
    /// Close a repository's inline shell. The repository, its worktrees
    /// and their agents are untouched — this kills one window. Named for
    /// what the dialog says rather than for the two variants above it: `d`
    /// on a shell has always read "Close Shell".
    CloseRepoShell {
        repository: usize,
    },
    /// Delete everything a visual selection covers.
    DeleteMarked(Marks),
}

pub struct App {
    pub cfg: Config,
    pub theme: Theme,
    pub tmux: Option<Tmux>,
    pub state: State,
    pub sel: Sel,
    /// Repositories whose `git worktree list` is still owed, drained one per
    /// event-loop iteration by [`App::drain_pending_worktrees`].
    pub pending_worktrees: std::collections::VecDeque<usize>,
    pub repo_expanded: Vec<bool>,
    pub wt_expanded: Vec<Vec<bool>>,
    pub mode: Mode,
    pub panes: HashMap<String, PaneInfo>,
    /// Spawn commands owed to shells that are not ready to be typed into
    /// yet, drained by every poll. See [`PendingLaunch`].
    pending_launches: Vec<PendingLaunch>,
    pub captures: HashMap<String, String>,
    /// How far the preview is parked above the live tail, in lines. `0` is
    /// the live tail — the only state the preview had before it scrolled.
    pub preview_scroll: usize,
    /// The deep capture the preview is scrolling through, taken once when
    /// it leaves the live tail so the polls cannot move the text under the
    /// reader. `None` while live.
    pub preview_buf: Option<String>,
    /// The pane the parked view was taken from. A parked preview belongs to
    /// one shell: rather than resetting the scroll at every one of the six
    /// places `sel` is assigned — and missing the seventh — the offset is
    /// read through [`App::preview_offset`], which reports live whenever
    /// the selection has moved on.
    pub preview_pane: Option<String>,
    pub statuses: HashMap<String, Status>,
    /// How spawn commands are instrumented, when it is enabled
    /// (`[defaults] agent_hooks`). `None` disables injection entirely.
    pub instrumentation: Option<crate::hooks::Instrumentation>,
    pattern_cache: HashMap<String, Compiled>,
    pub last_poll: Instant,
    /// When the dashboard started. Only the working spinner reads it: its
    /// frame is a function of elapsed time, so the animation runs off the
    /// redraws the event loop already does (~10 Hz) and needs no timer of
    /// its own.
    pub started: Instant,
    /// The transient notice shown as a toast above the statusbar. Set by
    /// [`App::notify`] / [`App::notify_err`]; read through [`App::toast`],
    /// which drops it once its TTL has passed — the statusbar is
    /// shortcuts-only, so notices are never allowed to become permanent
    /// furniture. Everything also goes to the log.
    message: Option<Toast>,
    /// Interactive boot: scope discovery runs automatically (no prompt).
    /// Off in tests (and irrelevant for a TUI that already booted);
    /// `run()` turns it on.
    pub interactive_boot: bool,
    pub quit: bool,
    /// The current scope: only repositories whose path is inside this root are
    /// listed (startup cwd, or `[discovery].scan_root` when set). Repositories
    /// in state from other scopes stay saved but are not shown — scopes
    /// never mix.
    pub scope_root: PathBuf,
    /// Has `git worktree list` been run for each repository (index-parallel
    /// to `state.repositories`)? Worktrees are the expensive part of
    /// discovery, so they load lazily — when a repository is expanded.
    pub worktrees_loaded: Vec<bool>,
    /// Last frame size — drives page-sized motions and mouse hit-testing.
    pub viewport: (u16, u16),
    /// Size of the dashboard's **strip** — its own pane plus the slot
    /// beside it ([`App::strip_size`]). The furniture is laid out against
    /// the strip, so that a holder and the dashboard pane — and therefore
    /// an agent pane and the slot — come out the same size. Refreshed by
    /// [`App::refresh_strip`]; `None` until tmux has been asked once.
    pub strip: Option<(u16, u16)>,
    /// The dashboard's slot pane, once one has been split off
    /// ([`App::ensure_slot`]). Re-derived from `@lw_slot` rather than
    /// trusted: the user may close it by hand.
    pub slot: Option<String>,
    /// The agent pane currently docked in the slot, if any. Never trusted
    /// either: the pane may be killed, its agent may exit, or another
    /// instance may steal it, and each of those is an ordinary event that
    /// puts the panel back on the preview — checked every poll by
    /// [`App::reconcile_docked`].
    pub docked: Option<String>,
    /// Shells held by *another* dashboard, by agent pane id. Re-read from
    /// `@lw_docked` on every poll and never cached across one — anything
    /// shared between instances lives in tmux, which is the only thing
    /// both instances can see.
    pub leases: HashMap<String, Lease>,
    /// What dock-on-look last acted on: `Some(Some(pane))` for a dock it
    /// attempted, `Some(None)` for a "nothing here to dock" it acted on,
    /// and `None` for "nothing tried since the last poll". It keeps a
    /// yielded lease, a failed dock and a focus-guarded frame from being
    /// re-attempted on every frame; every poll clears it, so anything that
    /// has changed since is picked up at the poll cadence.
    pub auto_dock_seen: Option<Option<String>>,
    /// The window the docked pane came from — where [`crate::dock::undock`]
    /// sends it back to. Learned at dock time, because once a pane is
    /// docked its own window id is the dashboard's and no longer says
    /// where home was. Set and cleared with [`App::docked`].
    pub docked_home: Option<String>,
    /// Rect of the overlay currently on screen, if any. Mouse clicks
    /// outside it dismiss the overlay (accessibility rule).
    pub popup_area: Option<Rect>,
    /// Scroll offset of the grouped help overlay (palette). Navigation
    /// keys scroll the help when it is open instead of the tree.
    pub help_scroll: usize,
    /// Scroll offset of the startup diagnostics overlay. Motions scroll the
    /// diagnostics when it is open instead of the tree (same code path as
    /// help/picker — mode-dependent target).
    pub diag_scroll: usize,
    /// Themes offered by the picker (built-ins first, then user themes),
    /// computed when the picker opens.
    pub theme_names: Vec<String>,
    /// The theme name the picker opened with — restored on Esc.
    pub picker_original: String,
    /// Hit ranges of the statusbar shortcut entries, set by the renderer:
    /// (x_start, x_end, y, action). The bar wraps onto a second row on a
    /// narrow frame, so each entry carries the row it landed on.
    pub bar_hits: Vec<(u16, u16, u16, BarAction)>,
    /// Last left-click (time, x, y) — double-click detection, since this
    /// crossterm version does not report a click count on mouse events.
    last_click: Option<(Instant, u16, u16)>,
    /// Worktree path → has uncommitted changes. The tree draws this as the
    /// worktree icon, so it must be a lookup, not a `git status` per frame;
    /// [`App::poll_worktree_state`] refills it every [`WT_STATE_INTERVAL`].
    pub(crate) wt_state: HashMap<PathBuf, WorktreeState>,
    /// Worktree removals running off the loop; their rows stay until
    /// [`App::drain_removals`] applies the result.
    pub(crate) remover: crate::remover::Remover,
    /// Resolved base branch per repository path, refreshed with `wt_state`,
    /// so `<base_branch>` expands without a git call per frame.
    base_branches: HashMap<PathBuf, String>,
    last_wt_state_poll: Option<Instant>,
    /// What the New Worktree popup says the branch will be cut from,
    /// re-resolved every time the popup opens (a `git` call per keystroke
    /// is not on). `None` when the repository could not be read.
    new_worktree_base: Option<BaseHint>,
    /// The global pad at rest — where [`Mode::Todo`] hands its buffer back
    /// when the panel closes, so reopening lands on the line the typing
    /// stopped at. Only the text reaches disk; see [`crate::todo`].
    pub todo: TextBuf,
    /// Per-repository pads at rest, keyed by repository path. Loaded for
    /// every known repository so the parked panel can follow the selection
    /// without touching the disk per frame.
    pub repo_todos: HashMap<PathBuf, TextBuf>,
    /// Which kind of pad the parked panel shows: the one opened last. A
    /// repository pad follows the selection.
    pub todo_global_parked: bool,
    /// The pad changed since it was last written. Also flushed by
    /// [`App::poll`], so a dashboard killed with its terminal loses at most
    /// one poll interval of typing.
    todo_dirty: Option<crate::todo::Pad>,
    /// Visual-mode anchor (`v`): the shells on the rows between it and
    /// `sel` are the selection.
    pub visual: Option<Sel>,
    /// The pad handed to `$EDITOR` (`ctrl-e`): the window running it, and
    /// which pad it holds. The poll that finds the window gone reads the
    /// file back in and clears this.
    editor: Option<(String, crate::todo::Pad)>,
}

/// The base branch a new worktree would branch from, as shown in the New
/// Worktree popup. `missing`: the configured branch is not in the repo, so
/// creation will refuse.
#[derive(Debug, Clone)]
pub struct BaseHint {
    pub branch: String,
    pub missing: bool,
}

/// The slice of a pane capture status classification runs on: the last
/// [`RECENT_TAIL_LINES`] non-blank lines, as plain text.
///
/// Stale lines from an agent's banner would otherwise keep a waiting agent
/// "working", and `capture-pane` pads the pane's empty area with blank
/// lines, so trailing blanks go first. The capture carries the pane's escape
/// sequences (the preview renders them) while the status patterns match
/// plain text — a coloured "Thinking…" must classify like an uncoloured one,
/// so the escapes are stripped here.
fn recent_tail(capture: &str) -> String {
    let plain = crate::ansi::strip(capture);
    let mut lines: Vec<&str> = plain.lines().collect();
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let start = lines.len().saturating_sub(RECENT_TAIL_LINES);
    lines[start..].join("\n")
}

impl App {
    pub fn new(cfg: Config, theme: Theme, tmux: Option<Tmux>) -> App {
        App {
            cfg,
            theme,
            tmux,
            state: State::default(),
            sel: Sel::Repository(0),
            pending_worktrees: std::collections::VecDeque::new(),
            repo_expanded: Vec::new(),
            wt_expanded: Vec::new(),
            mode: Mode::Normal,
            panes: HashMap::new(),
            pending_launches: Vec::new(),
            captures: HashMap::new(),
            preview_scroll: 0,
            preview_buf: None,
            preview_pane: None,
            statuses: HashMap::new(),
            instrumentation: None,
            pattern_cache: HashMap::new(),
            last_poll: Instant::now(),
            started: Instant::now(),
            message: None,
            wt_state: HashMap::new(),
            base_branches: HashMap::new(),
            last_wt_state_poll: None,
            interactive_boot: false,
            quit: false,
            viewport: (0, 0),
            strip: None,
            slot: None,
            leases: HashMap::new(),
            auto_dock_seen: None,
            docked: None,
            docked_home: None,
            popup_area: None,
            help_scroll: 0,
            diag_scroll: 0,
            scope_root: PathBuf::from("."),
            worktrees_loaded: Vec::new(),
            theme_names: vec![crate::theme::DEFAULT_THEME.into()],
            picker_original: crate::theme::DEFAULT_THEME.into(),
            bar_hits: Vec::new(),
            last_click: None,
            new_worktree_base: None,
            todo: TextBuf::new(),
            repo_todos: HashMap::new(),
            todo_global_parked: false,
            todo_dirty: None,
            visual: None,
            editor: None,
            remover: Default::default(),
        }
    }

    // ---------------------------------------------------------- lifecycle

    /// Load state, reconcile against reality, bind the back keys.
    pub fn startup(&mut self) -> Result<()> {
        self.state = State::load()?;
        self.merge_config_repositories()?;
        // Interactive boot: auto-import the cwd's repo (with its worktrees
        // as worktrees) or, when the cwd is not a repo, arm the startup
        // scan prompt.
        if self.interactive_boot {
            self.boot_discovery()?;
        }
        if let Some(tmux) = &self.tmux {
            // Before the first reconcile: a dashboard that died with a pane
            // docked left that pane in a session that does not own it, and
            // the tree must never render a shell in the wrong place.
            if let Err(e) = crate::dock::adopt_orphans(tmux) {
                log::warn!("cannot adopt stray agent panes: {e}");
            }
            self.state.reconcile(tmux)?;
            self.state.prune(&self.worktrees_loaded);
            self.announce_scope(tmux);
            // A server started by an older build has agent windows with no
            // holder in them; so has one whose holder the user closed.
            crate::dock::ensure_holders(tmux, crate::dock::dash_width(self.strip_size(tmux).0));
        }
        self.build_tree_flags();
        self.clamp_sel();
        self.load_todo();
        self.save_state()?;
        Ok(())
    }

    /// The absolute scope this dashboard is showing — what its window
    /// announces as `@lw_scope`, and what a second launch in the same folder
    /// looks itself up by. Absolute because a relative path means nothing to
    /// a process started in another directory.
    pub fn scope_path(&self) -> PathBuf {
        if self.scope_root.is_absolute() {
            return self.scope_root.clone();
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        crate::util::normalize_path(&cwd.join(&self.scope_root))
    }

    /// Stamp the scope onto the dashboard's own tmux window, so the
    /// self-wrap can find this dashboard rather than starting a rival one
    /// beside it. Never fatal: an unstamped window costs a duplicate
    /// dashboard, not a broken one.
    fn announce_scope(&self, tmux: &Tmux) {
        let Some(inside) = &tmux.inside else {
            return;
        };
        let scope = self.scope_path();
        match tmux.set_window_option(
            &inside.window_id,
            crate::dock::SCOPE_KEY,
            &scope.to_string_lossy(),
        ) {
            Ok(()) => log::info!(
                "window {} announces scope {}",
                inside.window_id,
                scope.display()
            ),
            Err(e) => log::warn!("cannot announce the scope on our window: {e}"),
        }
    }

    // ---------------------------------------------------- furniture

    /// The **strip** the dashboard lays its furniture out against: its own
    /// pane, plus the pane beside it that is also ours — the slot, or the
    /// agent standing in the slot's place.
    ///
    /// The pane and not the window, because the window is not ours: a user
    /// who splits a pane of their own off to the right leaves the dashboard
    /// a strip narrower than the window, and furniture cut to the window's
    /// width no longer fits in it. A split feels it first — `split-window`
    /// divides the pane it is aimed at, so a slot asked for at the window's
    /// proportions squeezes the dashboard down to a couple of columns.
    ///
    /// With the dashboard alone in its window the strip *is* the window,
    /// which is the ordinary case and the one the proportions are tuned
    /// for. Falls back to the viewport — the dashboard's pane as it was
    /// last drawn — which is never worse than nothing.
    fn strip_size(&self, tmux: &Tmux) -> (u16, u16) {
        if let Some(inside) = &tmux.inside
            && let Ok((w, h, window)) = tmux.pane_geometry(&inside.pane_id)
            && w > 0
        {
            // Only a neighbour that really is beside us counts: `docked`
            // may name a pane another dashboard has since taken away.
            let beside = self
                .docked
                .as_deref()
                .or(self.slot.as_deref())
                .and_then(|p| tmux.pane_geometry(p).ok())
                .filter(|(_, _, wid)| *wid == window)
                .map_or(0, |(sw, _, _)| sw + 1);
            return (w + beside, h);
        }
        self.strip.unwrap_or(self.viewport)
    }

    /// Width every holder — and the dashboard's own pane, once a slot
    /// exists — is cut to.
    fn holder_width(&self, tmux: &Tmux) -> u16 {
        crate::dock::dash_width(self.strip_size(tmux).0)
    }

    /// Re-read the dashboard's strip and, when it has changed, put every
    /// agent window back in step with it.
    ///
    /// Agent sessions carry `window-size manual`, so nothing else moves
    /// them: their windows keep whatever size we last gave them. Keeping
    /// them equal to the strip is what makes a dock a same-size swap, and a
    /// same-size swap is what spares the agent a repaint. It is also the
    /// self-repair for a strip the user has just taken space away from —
    /// the next poll cuts the furniture down to what is left.
    fn refresh_strip(&mut self, tmux: &Tmux) {
        // `choose-tree -Z` and friends zoom our pane; its size is then the
        // window's, and re-cutting would unzoom the picker a moment after
        // it opened.
        if let Some(inside) = &tmux.inside
            && tmux.window_zoomed(&inside.window_id)
        {
            return;
        }
        let size = self.strip_size(tmux);
        if size == (0, 0) || self.strip == Some(size) {
            return;
        }
        log::info!("dashboard strip is now {}x{}", size.0, size.1);
        self.strip = Some(size);
        self.resize_agent_windows(tmux, size);
        // The dashboard's own pane owes the slot the rest of the row.
        if self.slot.is_some()
            && let Some(inside) = &tmux.inside
            && let Err(e) = tmux.resize_pane_width(&inside.pane_id, crate::dock::dash_width(size.0))
        {
            log::warn!("cannot re-cut the dashboard pane: {e}");
        }
    }

    /// Resize every agent window to `size`, and re-cut its holder to the
    /// dashboard pane's width.
    ///
    /// Accepted degradation: a user who attaches an agent session directly
    /// gets its windows resized to their own terminal, and the next poll
    /// puts them back — so that dock costs one repaint. Correctness is
    /// unaffected; only the flicker is.
    fn resize_agent_windows(&self, tmux: &Tmux, size: (u16, u16)) {
        let (x, y) = size;
        let holder_w = crate::dock::dash_width(x);
        let Ok(panes) = tmux.list_furniture() else {
            return;
        };
        let mut done: Vec<&str> = Vec::new();
        for p in &panes {
            if !crate::tmux::is_wingmux_session(&p.session_name)
                || done.contains(&p.window_id.as_str())
            {
                continue;
            }
            done.push(&p.window_id);
            if let Err(e) = tmux.resize_window(&p.window_id, x, y) {
                log::warn!("resize-window {}: {e}", p.window_id);
            }
        }
        for p in panes.iter().filter(|p| p.holder) {
            if let Err(e) = tmux.resize_pane_width(&p.pane_id, holder_w) {
                log::warn!("resize holder {}: {e}", p.pane_id);
            }
        }
    }

    /// Put the layout back at the proportions it is meant to have: the
    /// dashboard's own strip at [`dock::dash_width`][crate::dock::dash_width],
    /// every agent window at the dashboard window's size, every holder cut
    /// to match, and any missing holder backfilled.
    ///
    /// It exists because the sizes are only re-cut when the *window* size
    /// changes ([`App::refresh_strip`]): a strip the user dragged, or
    /// a stray `resize-pane`, stays dragged until something asks for it
    /// back. Nothing here moves a shell — the furniture is repaired, what
    /// is docked stays docked.
    pub(crate) fn reset_layout(&mut self) {
        let Ok(tmux) = self.require_tmux() else {
            self.notify_err("no tmux session");
            return;
        };
        let size = self.strip_size(&tmux);
        if size == (0, 0) {
            self.notify_err("cannot read the dashboard pane size");
            return;
        }
        self.strip = Some(size);
        let width = crate::dock::dash_width(size.0);
        // Our own strip first, and only when something of ours sits beside
        // it: the neighbour takes the rest of the row, so one resize
        // settles both. With no slot and nothing docked the dashboard is a
        // guest in the user's own window, and its width is theirs to set.
        if (self.slot.is_some() || self.docked.is_some())
            && let Some(inside) = &tmux.inside
            && let Err(e) = tmux.resize_pane_width(&inside.pane_id, width)
        {
            log::warn!("cannot re-cut the dashboard pane: {e}");
        }
        self.resize_agent_windows(&tmux, size);
        crate::dock::ensure_holders(&tmux, width);
        log::info!("layout reset to {width} columns of {}", size.0);
        self.notify("Layout reset");
    }

    /// The dashboard's slot pane, created if it is not there.
    ///
    /// Lazy on purpose: the dashboard often runs in the user's own window,
    /// and splitting it uninvited at startup would be rude. Splitting it
    /// because the user asked to dock is not. The slot is always located by
    /// [`crate::dock::SLOT_KEY`], never by pane index — the user is free to
    /// rearrange the window around us.
    pub fn ensure_slot(&mut self) -> Result<String> {
        let tmux = self.require_tmux()?;
        let inside = tmux
            .inside
            .clone()
            .ok_or_else(|| Error::Tmux("the dashboard is not in a tmux pane".into()))?;
        if let Some(pane) = self.find_slot(&tmux) {
            self.slot = Some(pane.clone());
            return Ok(pane);
        }
        let (x, y) = self.strip_size(&tmux);
        self.strip = Some((x, y));
        let pane = tmux.split_slot(&inside.pane_id, crate::dock::agent_width(x))?;
        log::info!("slot {pane} split into window {}", inside.window_id);
        // The agent windows must match the window the slot now lives in.
        self.resize_agent_windows(&tmux, (x, y));
        self.slot = Some(pane.clone());
        Ok(pane)
    }

    /// The slot pane in the dashboard's own window, if one is still there.
    fn find_slot(&self, tmux: &Tmux) -> Option<String> {
        let inside = tmux.inside.as_ref()?;
        tmux.list_furniture()
            .ok()?
            .into_iter()
            .find(|p| p.slot && p.window_id == inside.window_id)
            .map(|p| p.pane_id)
    }

    /// The lease on `pane`, when the dashboard holding it is not us.
    ///
    /// A shell one instance has docked is off limits to another's
    /// *looking*, though not to its `Enter`.
    pub fn leased_by(&self, pane: &str) -> Option<&Lease> {
        self.leases.get(pane)
    }

    /// Re-read the leases from the panes this poll just listed.
    ///
    /// A `@lw_docked` mark is only evidence of a lease while the dashboard
    /// pane it names is still alive: a mark left by an instance that
    /// crashed names a pane that is gone, and treating that as a lease
    /// would fence the shell off forever. The furniture list is only
    /// fetched when some pane claims to be docked elsewhere, so the
    /// ordinary single-instance poll costs nothing extra.
    fn refresh_leases(&mut self, tmux: &Tmux, panes: &[crate::tmux::PaneInfo]) {
        let own = tmux.inside.as_ref().map(|i| i.pane_id.as_str());
        let claimed: Vec<(String, String)> = panes
            .iter()
            .filter_map(|p| {
                let by = p.docked_by.clone()?;
                (Some(by.as_str()) != own).then(|| (p.pane_id.clone(), by))
            })
            .collect();
        if claimed.is_empty() {
            self.leases.clear();
            return;
        }
        let alive = match tmux.list_furniture() {
            Ok(f) => f,
            Err(e) => {
                log::warn!("cannot check leases: {e}");
                return;
            }
        };
        self.leases = claimed
            .into_iter()
            .filter(|(_, by)| alive.iter().any(|p| &p.pane_id == by))
            .map(|(pane, by)| {
                // Reuse what the last poll learned: the scope of a window
                // does not change under a live dashboard, and this saves a
                // `display-message` per leased shell per poll.
                let scope = self
                    .leases
                    .get(&pane)
                    .filter(|l| l.dash_pane == by)
                    .and_then(|l| l.scope.clone())
                    .or_else(|| tmux.pane_scope(&by));
                (
                    pane,
                    Lease {
                        dash_pane: by,
                        scope,
                    },
                )
            })
            .collect();
    }

    /// Let go of a docked pane that is no longer ours.
    ///
    /// Three ordinary things end a dock without us asking: the pane is
    /// killed, its agent exits, or another instance steals the shell. None
    /// of them is an error — the panel simply goes back to the preview,
    /// which is the browse mode and works for every shell — so this is a
    /// log line and not a toast. The slot stays: it is furniture, and the
    /// next dock reuses it.
    fn reconcile_docked(&mut self, tmux: &Tmux, panes: &[crate::tmux::PaneInfo]) {
        let Some(docked) = self.docked.clone() else {
            return;
        };
        let own = tmux.inside.as_ref().map(|i| i.pane_id.as_str());
        let ours = own.is_some()
            && panes
                .iter()
                .find(|p| p.pane_id == docked)
                .is_some_and(|p| p.docked_by.as_deref() == own);
        if ours {
            return;
        }
        log::info!("docked pane {docked} is no longer ours — back to the preview");
        // Losing a shell to another dashboard is the one of the three that
        // needs saying: the pane is still alive and still in the tree, so
        // a panel that silently reverted to the preview would read as an
        // agent that stalled. A pane that was killed or whose agent exited
        // says so in its own row, and stays a log line.
        let taken_by = panes
            .iter()
            .find(|p| p.pane_id == docked)
            .and_then(|p| p.docked_by.as_deref())
            .and_then(|by| tmux.pane_scope(by));
        if let Some(scope) = taken_by {
            let who = Lease {
                dash_pane: String::new(),
                scope: Some(scope),
            }
            .label();
            let name = self
                .shell_name_for_pane(&docked)
                .unwrap_or_else(|| docked.clone());
            self.notify(format!("{name} taken by {who}"));
        }
        self.docked = None;
        self.docked_home = None;
        // The slot exists exactly while something is docked in it: with
        // the dock gone the dashboard takes its whole window back, and the
        // preview — the browse mode, and the only view that works for a
        // shell we do not hold — returns with it.
        self.drop_slot();
    }

    /// Take the slot back down. Called on quit: the slot is this
    /// dashboard's furniture, and leaving a sleeping pane behind in the
    /// user's own window is litter.
    pub fn drop_slot(&mut self) {
        // The docked agent pane is in the window we are about to tidy, and
        // the slot is sitting in *its* window — send them back to each
        // other before anything is killed, or quitting would take the
        // agent down with the dashboard.
        self.undock_current();
        let Some(pane) = self.slot.take() else {
            return;
        };
        let Ok(tmux) = self.require_tmux() else {
            return;
        };
        // tmux gives a killed pane's columns to either neighbour; claim
        // them first or a foreign pane beside the strip takes them.
        if let Some(inside) = &tmux.inside
            && let Ok((w, _, window)) = tmux.pane_geometry(&inside.pane_id)
            && let Ok((sw, _, slot_window)) = tmux.pane_geometry(&pane)
            && window == slot_window
            && let Err(e) = tmux.resize_pane_width(&inside.pane_id, w + sw + 1)
        {
            log::warn!("cannot widen the dashboard pane: {e}");
        }
        match tmux.kill_pane(&pane) {
            Ok(()) => log::info!("slot {pane} removed"),
            Err(e) => log::warn!("cannot remove slot {pane}: {e}"),
        }
    }

    fn save_state(&mut self) -> Result<()> {
        self.state.save()?;
        Ok(())
    }

    /// Import repositories declared in config.toml that aren't in state yet.
    fn merge_config_repositories(&mut self) -> Result<()> {
        let mut imported = 0;
        let paths: Vec<PathBuf> = self
            .cfg
            .usable_repositories()
            .into_iter()
            .map(|p| PathBuf::from(&p.path))
            .collect();
        for path in paths {
            if self.state.repositories.iter().any(|x| x.path == path) {
                continue;
            }
            if !Git.is_repo(&path) {
                log::warn!(
                    "config repository {} is not a git repo, skipping",
                    path.display()
                );
                continue;
            }
            self.add_repository(path);
            imported += 1;
        }
        if imported > 0 {
            log::info!("imported {imported} repositories from config");
        }
        Ok(())
    }

    /// Push a repository for a repo path. Its base branch and agent are read
    /// from config on use ([`Config::agent_at`], [`Config::base_branch_at`]).
    fn add_repository(&mut self, path: PathBuf) {
        self.state.repositories.push(Repository {
            name: Git.repo_name(&path),
            path,
            missing: false,
            shell: None,
            worktrees: Vec::new(),
        });
    }

    // ---------------------------------------------------------- discovery

    /// The startup scope: `[discovery].scan_root`, or the directory
    /// Wingmux was started from when unset. Relative roots resolve
    /// against `cwd`, which is injected so tests don't have to `chdir`.
    fn scan_root_from(&self, cwd: &std::path::Path) -> PathBuf {
        scope_for(&self.cfg, cwd)
    }

    /// Startup scope discovery (interactive boot only). No prompt, no
    /// cache: the scope is what is INSIDE the startup directory.
    /// - cwd (or an ancestor) is a git repo → that repo is the scope; it is
    ///   imported as the single repository, and its linked worktrees are
    ///   what the tree lists.
    /// - otherwise → the scope is scanned LIVE for git repos and each one
    ///   is imported. Worktrees stay lazy until a repository is expanded.
    ///
    /// Either way `scope_root` is set, and the tree only lists repositories
    /// inside it — different scopes never mix.
    fn boot_discovery(&mut self) -> Result<()> {
        let cwd = std::env::current_dir().unwrap_or_default();
        self.boot_discovery_from(&cwd)
    }

    fn boot_discovery_from(&mut self, cwd: &std::path::Path) -> Result<()> {
        let scope = crate::util::normalize_path(&self.scan_root_from(cwd));
        self.scope_root = scope.clone();
        if let Some(main_repo) = Git.main_repo(cwd) {
            // Single-repo scope: the repo we are standing in.
            log::info!(
                "cwd is inside a git repo (main: {}), single-repository scope",
                main_repo.display()
            );
            self.import_cwd_repo(&main_repo)?;
        } else {
            // Folder scope: live-scan for repos. No cache — finding .git
            // is a few stats per dir, cheap even at hundreds of folders.
            let found = self.discover_and_import(&scope)?;
            log::info!(
                "scope {}: {found} repositories from live scan",
                scope.display()
            );
        }
        self.build_tree_flags();
        // Worktrees for every unfolded repository Only the first one
        // is loaded here: it is what the first frame draws, and blocking
        // boot on one `git worktree list` per repo would cost seconds in a
        // scope with dozens of them. The rest are queued and drained by the
        // event loop, so the tree paints immediately and fills in.
        let vis = self.visible_repositories();
        let to_load: Vec<usize> = vis
            .iter()
            .copied()
            .filter(|&ri| self.repo_expanded.get(ri).copied().unwrap_or(false))
            .collect();
        let mut queued = to_load.into_iter();
        if let Some(ri) = queued.next() {
            self.ensure_worktrees(ri)?;
        }
        self.pending_worktrees.extend(queued);
        Ok(())
    }

    /// Import (or complete) the repository for `main_repo`. Worktrees are NOT
    /// listed here — [`Self::ensure_worktrees`] does that lazily when the
    /// repository is expanded. Never
    /// fails startup on git errors — discovery is best-effort.
    fn import_cwd_repo(&mut self, main_repo: &std::path::Path) -> Result<()> {
        let path = main_repo.to_path_buf();
        let mut ri = self.state.repositories.iter().position(|x| x.path == path);
        if ri.is_none() {
            self.add_repository(path.clone());
            ri = Some(self.state.repositories.len() - 1);
        }
        let ri = ri.unwrap();
        self.ensure_worktrees(ri)?;
        Ok(())
    }

    /// Scan `root` for git repos and add every one not listed yet. Filesystem
    /// only — no git process per repository. Returns the number added.
    fn discover_and_import(&mut self, root: &std::path::Path) -> Result<usize> {
        let root = crate::util::normalize_path(root);
        let paths = crate::discovery::scan(
            &root,
            self.cfg.discovery.max_depth,
            &self.cfg.discovery.skip_dirs,
        );
        let mut added = 0;
        for p in paths {
            if self.state.repositories.iter().any(|x| x.path == p) {
                continue;
            }
            self.add_repository(p);
            added += 1;
        }
        if added > 0 {
            log::info!(
                "discovery: {added} new repositories under {}",
                root.display()
            );
        }
        Ok(added)
    }

    /// Load the next queued repository's worktrees. Returns false when the
    /// queue is empty. One repository per call: the event loop redraws
    /// between calls, so a slow scope fills the tree in visibly instead of
    /// freezing before the first frame.
    pub fn drain_pending_worktrees(&mut self) -> bool {
        let Some(ri) = self.pending_worktrees.pop_front() else {
            return false;
        };
        if let Err(e) = self.ensure_worktrees(ri) {
            log::warn!("background worktree load for repository {ri}: {e}");
        }
        true
    }

    pub(crate) fn build_tree_flags(&mut self) {
        // Scope-aware: only in-scope repositories get rows, and everything
        // unfolds by default — the dashboard exists to show the shells, and
        // a folded tree hides them. The cap is a readability backstop for
        // scopes with a wild number of repos, not a cost guard: the
        // `git worktree list` each unfolded repository owes is queued and
        // drained in the background (see `boot_discovery_from`).
        let vis = self.visible_repositories();
        let expand = vis.len() <= UNFOLD_CAP;
        self.repo_expanded = self
            .state
            .repositories
            .iter()
            .enumerate()
            .map(|(i, _)| vis.contains(&i) && expand)
            .collect();
        self.wt_expanded = self
            .state
            .repositories
            .iter()
            .map(|p| vec![true; p.worktrees.len()])
            .collect();
        // Keep the loaded flag parallel to repositories.
        self.worktrees_loaded
            .resize(self.state.repositories.len(), false);
    }

    /// Indices of the repositories currently in scope (path inside
    /// `scope_root`). Repositories from other scopes stay in state but never
    /// get rows — scopes don't mix. When the scope was never set (tests,
    /// or an app that skipped boot discovery) `scope_root` is the "."
    /// sentinel and everything is visible.
    pub(crate) fn visible_repositories(&self) -> Vec<usize> {
        let scoped = self.scope_root.as_os_str() != ".";
        self.state
            .repositories
            .iter()
            .enumerate()
            .filter(|(_, p)| !scoped || p.path.starts_with(&self.scope_root))
            .map(|(i, _)| i)
            .collect()
    }

    /// Lazily load a repository's worktrees (the expensive part of
    /// discovery): `git worktree list` runs ONCE per repository, when its
    /// details are actually needed (expanded). Worktrees are added deduplicated by worktree path.
    fn ensure_worktrees(&mut self, ri: usize) -> Result<()> {
        if self.worktrees_loaded.get(ri).copied().unwrap_or(false) {
            return Ok(());
        }
        let repo = self
            .state
            .repositories
            .get(ri)
            .cloned()
            .ok_or_else(|| Error::State(format!("repository index {ri} out of range")))?;
        if repo.missing {
            return Ok(());
        }
        let worktrees = match Git.worktree_list(&repo.path) {
            Ok(w) => w,
            Err(e) => {
                log::warn!("worktree list failed for {}: {e}", repo.path.display());
                return Ok(());
            }
        };
        let existing: std::collections::HashSet<PathBuf> = self.state.repositories[ri]
            .worktrees
            .iter()
            .map(|f| f.path.clone())
            .collect();
        let mut added = 0;
        for info in worktrees {
            // The first entry is the main worktree — that IS the repository.
            if info.path == repo.path || existing.contains(&info.path) {
                continue;
            }
            let branch = info.branch_short().unwrap_or("").to_string();
            let name = if branch.is_empty() {
                info.path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "worktree".into())
            } else {
                branch.clone()
            };
            let wt = self.state.restore_worktree(info.path, branch, name);
            self.state.repositories[ri].worktrees.push(wt);
            added += 1;
        }
        if let Some(flag) = self.worktrees_loaded.get_mut(ri) {
            *flag = true;
        }
        // `wt_expanded` is index-parallel to `worktrees`; a short vector
        // reads as folded, so the rows this just discovered would arrive
        // collapsed. They unfold like every other row.
        if let Some(flags) = self.wt_expanded.get_mut(ri) {
            flags.resize(self.state.repositories[ri].worktrees.len(), true);
        }
        if added > 0 {
            log::info!("lazy worktree load: {added} worktrees for '{}'", repo.name);
        }
        Ok(())
    }

    // ---------------------------------------------------------- navigation

    /// The shell a folded worktree row stands for. The nominal case is one
    /// worktree, one agent: a worktree with exactly one shell gets no child
    /// row at all — its own row *is* that shell (status, preview, attach,
    /// predefined messages). Worktrees with none or several shells keep their
    /// subtree, so `a` on a folded worktree unfolds it by giving it a
    /// second shell.
    pub(crate) fn solo_shell(&self, ri: usize, wi: usize) -> Option<usize> {
        let wt = self.state.repositories.get(ri)?.worktrees.get(wi)?;
        (wt.shells.len() == 1).then_some(0)
    }

    /// All visible tree nodes in display order (scope-filtered).
    fn visible_nodes(&self) -> Vec<Sel> {
        let mut v = Vec::new();
        for ri in self.visible_repositories() {
            let repo = &self.state.repositories[ri];
            v.push(Sel::Repository(ri));
            if !self.repo_expanded.get(ri).copied().unwrap_or(false) {
                continue;
            }
            for (wi, wt) in repo.worktrees.iter().enumerate() {
                v.push(Sel::Worktree(ri, wi));
                let fexp = self
                    .wt_expanded
                    .get(ri)
                    .and_then(|e| e.get(wi))
                    .copied()
                    .unwrap_or(false);
                // A solo worktree is folded for good: its own row is the
                // shell, so it never emits a child — whatever its flag says.
                if !fexp || self.solo_shell(ri, wi).is_some() {
                    continue;
                }
                for si in 0..wt.shells.len() {
                    v.push(Sel::Shell(ri, wi, si));
                }
            }
        }
        v
    }

    /// Row-count contract for the UI tests: see
    /// `ui::tests::rendered_tree_rows_match_visible_nodes`.
    #[cfg(test)]
    pub(crate) fn visible_nodes_len_for_test(&self) -> usize {
        self.visible_nodes().len()
    }

    /// Keep `sel` valid after tree mutations.
    pub(crate) fn clamp_sel(&mut self) {
        let nodes = self.visible_nodes();
        if nodes.is_empty() {
            self.sel = Sel::Repository(0);
            return;
        }
        if !nodes.contains(&self.sel) {
            self.sel = nodes[0];
        }
    }

    fn move_sel(&mut self, delta: i32) {
        let nodes = self.visible_nodes();
        if nodes.is_empty() {
            return;
        }
        let idx = nodes.iter().position(|n| *n == self.sel).unwrap_or(0);
        let ni = (idx as i32 + delta).clamp(0, nodes.len() as i32 - 1) as usize;
        self.sel = nodes[ni];
    }

    fn expand_or_attach(&mut self) {
        match self.sel {
            Sel::Repository(ri) => {
                // Expanding a repository is when its worktrees are needed:
                // load them lazily (once per repository per run).
                if !self.repo_expanded.get(ri).copied().unwrap_or(false)
                    && self.ensure_worktrees(ri).is_ok()
                    && self.worktrees_loaded.get(ri).copied().unwrap_or(false)
                {
                    // worktrees may have appeared; refresh the per-worktree
                    // expand flags
                    let n = self.state.repositories[ri].worktrees.len();
                    if let Some(v) = self.wt_expanded.get_mut(ri) {
                        v.resize(n, false);
                    }
                }
                if let Some(e) = self.repo_expanded.get_mut(ri) {
                    *e = !*e;
                }
            }
            Sel::Worktree(ri, wi) => {
                // Folded worktree: there is nothing to expand into, so the
                // row acts as its shell and opens the agent.
                if let Some(si) = self.solo_shell(ri, wi) {
                    self.dock_shell(ri, wi, si);
                } else if self
                    .state
                    .repositories
                    .get(ri)
                    .and_then(|p| p.worktrees.get(wi))
                    .is_some_and(|f| f.shells.is_empty())
                {
                    // Nothing to expand into and nothing to dock: toggling
                    // an expand flag here would look like a dead key.
                    self.notify("No agent shell here — press `a` to start one");
                } else if let Some(e) = self.wt_expanded.get_mut(ri).and_then(|v| v.get_mut(wi)) {
                    *e = !*e;
                }
            }
            Sel::Shell(ri, wi, si) => self.dock_shell(ri, wi, si),
        }
    }

    fn collapse_or_up(&mut self) {
        match self.sel {
            Sel::Repository(ri) => {
                if let Some(e) = self.repo_expanded.get_mut(ri)
                    && *e
                {
                    *e = false;
                }
            }
            Sel::Worktree(ri, wi) => {
                let fexp = self
                    .wt_expanded
                    .get(ri)
                    .and_then(|v| v.get(wi))
                    .copied()
                    .unwrap_or(false)
                    && self.solo_shell(ri, wi).is_none();
                if fexp {
                    if let Some(e) = self.wt_expanded.get_mut(ri).and_then(|v| v.get_mut(wi)) {
                        *e = false;
                    }
                } else {
                    self.sel = Sel::Repository(ri);
                }
            }
            Sel::Shell(ri, wi, _) => self.sel = Sel::Worktree(ri, wi),
        }
    }

    /// What the rows between the visual anchor and the cursor stand for,
    /// each as its own `d` would act: a worktree row deletes the worktree,
    /// a shell row closes the shell. Shells of a worktree that is going
    /// anyway are dropped. Empty when no selection is active or the anchor
    /// row is gone.
    pub(crate) fn visual_marks(&self) -> Marks {
        let mut marks = Marks::default();
        let Some(anchor) = self.visual else {
            return marks;
        };
        let nodes = self.visible_nodes();
        let (Some(a), Some(c)) = (
            nodes.iter().position(|n| *n == anchor),
            nodes.iter().position(|n| *n == self.sel),
        ) else {
            return marks;
        };
        for node in &nodes[a.min(c)..=a.max(c)] {
            match *node {
                Sel::Worktree(ri, wi) => marks.worktrees.push((ri, wi)),
                Sel::Shell(ri, wi, si) => marks.shells.push((ri, wi, si)),
                Sel::Repository(_) => {}
            }
        }
        let wts = marks.worktrees.clone();
        marks.shells.retain(|&(ri, wi, _)| !wts.contains(&(ri, wi)));
        marks
    }

    fn start_visual(&mut self) {
        if matches!(self.sel, Sel::Repository(_)) {
            self.notify("Select a worktree or shell to start a selection");
        } else {
            self.visual = Some(self.sel);
        }
    }

    fn on_key_visual(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('v') | KeyCode::Esc => self.visual = None,
            KeyCode::Char('d') => self.confirm_delete_marked(),
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    pub fn confirm_delete_marked(&mut self) {
        let mut marks = self.visual_marks();
        self.visual = None;
        marks
            .worktrees
            .retain(|&(ri, wi)| !self.worktree_removing(ri, wi));
        if marks.worktrees.is_empty() && marks.shells.is_empty() {
            return;
        }
        let mut lines: Vec<String> = Vec::new();
        let (mut dirty, mut unmerged, mut locked) = (false, false, false);
        for &(ri, wi) in &marks.worktrees {
            dirty |= self.worktree_is_dirty(ri, wi);
            locked |= self.worktree_lock(ri, wi).is_some();
            unmerged |= self.worktree_has_unmerged_commits(ri, wi);
            let wt = &self.state.repositories[ri].worktrees[wi];
            lines.push(format!("  worktree {}", wt.name));
        }
        for &(ri, wi, si) in &marks.shells {
            let wt = &self.state.repositories[ri].worktrees[wi];
            lines.push(format!(
                "  shell {} ({})",
                wt.shells[si].window_name, wt.name
            ));
        }
        let drop_branch = self.cfg.worktrees.delete_branch;
        let warning = match (dirty, unmerged) {
            (true, _) => Some("Uncommitted changes — force delete discards them".to_string()),
            _ if locked => Some("Locked worktrees — force delete unlocks them".to_string()),
            (false, true) if drop_branch => {
                Some("Unmerged commits — deleting the branches discards them".to_string())
            }
            (false, true) => Some("Unmerged commits — the branches keep them".to_string()),
            (false, false) => None,
        };
        self.mode = Mode::Confirm {
            confirm: Confirm {
                title: "Delete Selection".into(),
                body: format!("Delete {} items?\n{}", lines.len(), lines.join("\n")),
                yes_label: (!dirty && !locked).then(|| "yes".into()),
                warning,
                force_label: if dirty {
                    Some("force delete (discards changes)".into())
                } else {
                    locked.then(|| "force delete (locked)".into())
                },
                action: ConfirmAction::DeleteMarked(marks),
            },
        };
    }

    /// Shells first (they leave worktree indices alone), then worktrees,
    /// both last-index-first so a removal never shifts a pending one. A
    /// failure stops the batch and is reported.
    fn delete_marked(&mut self, mut marks: Marks, force: bool) -> Result<()> {
        marks.shells.sort_unstable_by(|a, b| b.cmp(a));
        marks.worktrees.sort_unstable_by(|a, b| b.cmp(a));
        let n = marks.shells.len() + marks.worktrees.len();
        for (ri, wi, si) in marks.shells {
            self.delete_shell(ri, wi, si)?;
        }
        let queued = !marks.worktrees.is_empty();
        for (ri, wi) in marks.worktrees {
            self.delete_worktree(ri, wi, force)?;
        }
        self.notify(if queued {
            format!("deleting {n} items")
        } else {
            format!("{n} items deleted")
        });
        Ok(())
    }

    /// The shell the selection acts on: an explicit shell row, or the
    /// single shell a folded worktree row stands for.
    pub(crate) fn selected_shell(&self) -> Option<(usize, usize, usize)> {
        match self.sel {
            Sel::Shell(ri, wi, si) => Some((ri, wi, si)),
            Sel::Worktree(ri, wi) => self.solo_shell(ri, wi).map(|si| (ri, wi, si)),
            _ => None,
        }
    }

    // ------------------------------------------------------------- todo

    /// Path of the repository the selected row belongs to — a worktree or
    /// shell row counts as its repository's.
    fn selected_repo_path(&self) -> Option<PathBuf> {
        let ri = match self.sel {
            Sel::Repository(ri) | Sel::Worktree(ri, _) | Sel::Shell(ri, ..) => ri,
        };
        self.state.repositories.get(ri).map(|r| r.path.clone())
    }

    /// The pad the parked panel shows, and its buffer. `None` when the
    /// selection has no repository or that repository's pad is not loaded.
    pub(crate) fn parked_todo(&self) -> Option<(crate::todo::Pad, &TextBuf)> {
        if self.todo_global_parked {
            return Some((crate::todo::Pad::Global, &self.todo));
        }
        let path = self.selected_repo_path()?;
        let buf = self.repo_todos.get(&path)?;
        Some((crate::todo::Pad::Repo(path), buf))
    }

    fn todo_buf_mut(&mut self, pad: &crate::todo::Pad) -> &mut TextBuf {
        match pad {
            crate::todo::Pad::Global => &mut self.todo,
            crate::todo::Pad::Repo(path) => self.repo_todos.entry(path.clone()).or_default(),
        }
    }

    /// `t`: the pad of the selected row's repository.
    fn open_repo_todo(&mut self) {
        match self.selected_repo_path() {
            Some(path) => self.open_todo(crate::todo::Pad::Repo(path)),
            None => self.notify("Select a repository for its todo"),
        }
    }

    /// Open a pad in the tree's panel.
    ///
    /// The text is re-read from disk: another dashboard, or `$EDITOR`, may
    /// have moved it on since. The cursor stays in memory, so an unchanged
    /// file reopens where the typing stopped.
    fn open_todo(&mut self, pad: crate::todo::Pad) {
        // A failed save of the previous pad must not be forgotten by the
        // dirty flag moving on to this one.
        self.save_todo();
        let text = crate::todo::load(&pad);
        let parked = self.todo_buf_mut(&pad);
        if text != parked.text() {
            let (row, col) = (parked.cursor_row(), parked.cursor_col());
            *parked = TextBuf::from_parked(&text, row, col);
        }
        let buf = parked.clone();
        self.todo_global_parked = pad == crate::todo::Pad::Global;
        // The mode goes up first: the caret's window is the open panel's.
        self.mode = Mode::Todo { buf, pad };
        let (visible, wrap) = (todo_visible_lines(self), todo_wrap_width(self));
        if let Mode::Todo { buf, .. } = &mut self.mode {
            buf.ensure_visible(visible, wrap);
        }
    }

    /// Leave the pad, keeping what is in it. Every exit goes through here —
    /// `Esc`, a theme switch, a click outside the panel.
    fn close_todo(&mut self) {
        if let Mode::Todo { buf, pad } = &self.mode {
            let (buf, pad) = (buf.clone(), pad.clone());
            self.mode = Mode::Normal;
            self.park_todo(pad, buf);
        }
    }

    /// Park an edited pad and write it out.
    fn park_todo(&mut self, pad: crate::todo::Pad, buf: TextBuf) {
        *self.todo_buf_mut(&pad) = buf;
        self.todo_dirty = Some(pad);
        self.save_todo();
    }

    /// `ctrl-e`: hand the open pad's file to `$EDITOR`, in a tmux window of
    /// its own, and close the panel.
    ///
    /// A window, not a popup: popups are tmux 3.2 and the floor here is
    /// 3.0. A window, not a split of the dashboard's own pane: the pad is
    /// drawn *inside* that pane, so an editor where the pad is would mean
    /// squeezing the tree into half a column beside a docked agent.
    ///
    /// The pad is written before the spawn — the editor must open what is
    /// on screen — and the file is the one source of truth for the round
    /// trip: the buffer is read back whole in [`Self::reap_editor`].
    fn edit_todo_externally(&mut self) {
        let Mode::Todo { buf, pad } = &self.mode else {
            return;
        };
        let (buf, pad) = (buf.clone(), pad.clone());
        let Some(editor) = crate::util::editor() else {
            self.notify("Set $EDITOR (or $VISUAL) to edit the pad there");
            return;
        };
        // Still in the pad while this is decided: a write that fails must
        // leave the note on screen, with the typing still in it.
        self.park_todo(pad.clone(), buf);
        if self.todo_dirty.is_some() {
            self.notify_err("cannot write the pad — $EDITOR not opened");
            return;
        }
        let tmux = match self.require_tmux() {
            Ok(t) => t,
            Err(e) => {
                self.notify_err(format!("cannot open $EDITOR: {e}"));
                return;
            }
        };
        let Some(inside) = tmux.inside.clone() else {
            self.notify("$EDITOR needs a tmux window; this dashboard has none");
            return;
        };
        let path = crate::util::todo_path(&pad);
        // The repository for its own pad; for the global one the directory
        // the pads live in — a cwd the editor's own file commands can use.
        let cwd = match &pad {
            crate::todo::Pad::Repo(p) => p.clone(),
            crate::todo::Pad::Global => path.parent().unwrap_or(&path).to_path_buf(),
        };
        let command = format!("{editor} {}", crate::util::shell_quote(&path));
        match tmux.command_window(&inside.session_name, EDITOR_WINDOW, &cwd, &command) {
            Ok(window) => {
                self.mode = Mode::Normal;
                self.editor = Some((window.window_id, pad));
            }
            Err(e) => self.notify_err(format!("cannot open $EDITOR: {e}")),
        }
    }

    /// The `$EDITOR` window is gone → read that pad back in.
    ///
    /// Whole file, cursor reset: the editor may have rewritten every line,
    /// and a caret kept from before it ran would point into text that is no
    /// longer there.
    fn reap_editor(&mut self, tmux: &Tmux) {
        let Some((window_id, pad)) = self.editor.clone() else {
            return;
        };
        if tmux.window_exists(&window_id) {
            return;
        }
        self.editor = None;
        log::info!(
            "$EDITOR closed; re-reading {}",
            crate::util::todo_path(&pad).display()
        );
        *self.todo_buf_mut(&pad) = Self::read_pad(&pad).unwrap_or_default();
        // Reopened while the editor was running: the panel is showing the
        // text from before it ran.
        if matches!(&self.mode, Mode::Todo { pad: open, .. } if *open == pad) {
            self.open_todo(pad);
        }
    }

    /// Write the dirty pad out, if any.
    ///
    /// Failures are logged, never toasted — the text is still in memory —
    /// and the flag stays up so the next poll retries.
    fn save_todo(&mut self) {
        let Some(pad) = self.todo_dirty.clone() else {
            return;
        };
        // The open panel is the live buffer; the parked one only catches up
        // when it closes.
        let text = match &self.mode {
            Mode::Todo { buf, pad: open } if *open == pad => buf.text(),
            _ => self.todo_buf_mut(&pad).text(),
        };
        match crate::todo::save(&pad, &text) {
            Ok(()) => self.todo_dirty = None,
            Err(e) => log::warn!("cannot save todo: {e}"),
        }
    }

    /// Wheel over the pad: walk the caret by whole lines and keep it on
    /// screen. The pad has no scroll of its own — the caret is what the
    /// view follows — so the wheel moves
    /// the caret rather than sliding text out from under it.
    fn todo_scroll_by(&mut self, lines: isize) {
        let visible = todo_visible_lines(self);
        let wrap = todo_wrap_width(self);
        if let Mode::Todo { buf, .. } = &mut self.mode {
            for _ in 0..lines.unsigned_abs() {
                if lines < 0 {
                    buf.move_up();
                } else {
                    buf.move_down();
                }
            }
            buf.ensure_visible(visible, wrap);
        }
    }

    /// Restore the pads parked by an earlier run (or written by hand).
    fn load_todo(&mut self) {
        if let Some(buf) = Self::read_pad(&crate::todo::Pad::Global) {
            self.todo = buf;
        }
        self.load_repo_todos();
    }

    /// Load the pad of every repository not loaded yet — including ones
    /// added since startup, so their parked pad shows without a `t` first.
    fn load_repo_todos(&mut self) {
        let missing: Vec<PathBuf> = self
            .state
            .repositories
            .iter()
            .filter(|r| !self.repo_todos.contains_key(&r.path))
            .map(|r| r.path.clone())
            .collect();
        for path in missing {
            let pad = crate::todo::Pad::Repo(path.clone());
            let buf = Self::read_pad(&pad).unwrap_or_default();
            self.repo_todos.insert(path, buf);
        }
    }

    fn read_pad(pad: &crate::todo::Pad) -> Option<TextBuf> {
        let text = crate::todo::load(pad);
        if text.len() > TEXTBUF_MAX_BYTES {
            log::warn!(
                "{} exceeds the buffer cap — not loaded",
                crate::util::todo_path(pad).display()
            );
            return None;
        }
        Some(TextBuf::from_parked(&text, 0, 0))
    }

    /// Pane id of the selected shell, if any.
    ///
    /// Indexed with `get`, not `[]`: `Sel` carries indices that a tree
    /// mutation can outdate for the frame between the change and the next
    /// `clamp_sel`, and a renderer asking what is selected must get `None`
    /// there rather than panic.
    /// A repository row answers with its own inline shell, which is what
    /// makes docking, the preview and the lease follow that row exactly as
    /// they follow a worktree's. What it deliberately does NOT reach is
    /// [`App::selected_shell`] — the `(repository, worktree, shell)` triple
    /// the send paths take — so `s` and `p` stay unbound on a repository
    /// row: the presets are agent prompts, and a command you want a shell
    /// to run is a command you type in it.
    pub fn selected_shell_pane(&self) -> Option<String> {
        if let Sel::Repository(ri) = self.sel {
            return self.repo_shell_pane(ri);
        }
        let (ri, wi, si) = self.selected_shell()?;
        let sh = self
            .state
            .repositories
            .get(ri)?
            .worktrees
            .get(wi)?
            .shells
            .get(si)?;
        Some(sh.pane_id.clone())
    }

    // ---------------------------------------------------------- actions

    fn require_tmux(&self) -> Result<Tmux> {
        self.tmux
            .clone()
            .ok_or_else(|| Error::Tmux("no tmux server available".into()))
    }

    /// The command actually typed into a fresh pane: the user's
    /// `agent_command`, plus the hook flags when it is a recognised
    /// `claude` or `codex` and instrumentation is on. The stored command is
    /// never rewritten.
    pub fn agent_command_for_spawn(&self, command: &str) -> String {
        match &self.instrumentation {
            Some(i) => i.apply(command),
            None => command.to_string(),
        }
    }

    /// Make sure repository `ri`'s agent session exists, creating it if
    /// needed. Returns the session name and, when a NEW session was created,
    /// its first window (already running `shell` in `cwd`).
    ///
    /// A new session is `wingmux-session-<slug>`, suffixed with a stable hash
    /// of the path when that name is taken. Callers stamp each pane with the
    /// repository path, so the name never has to be remembered.
    fn ensure_agent_session(
        &mut self,
        tmux: &Tmux,
        ri: usize,
        worktree: &str,
        index: usize,
        cwd: &std::path::Path,
        shell: &str,
    ) -> Result<(String, crate::dock::ShellId, Option<crate::tmux::WindowRef>)> {
        let repo = &self.state.repositories[ri];
        let sessions = tmux.list_sessions()?;
        if let Some(name) = self
            .state
            .session_of(&tmux.list_panes()?, ri)
            .filter(|n| sessions.iter().any(|s| &s.name == n))
        {
            let id = crate::dock::ShellId::for_session(&name, worktree, index);
            return Ok((name, id, None));
        }
        let base = Tmux::session_name(&repo.name);
        let name = if sessions.iter().any(|s| s.name == base) {
            format!("{base}-{}", Tmux::hash_suffix(&repo.path))
        } else {
            base
        };
        let id = crate::dock::ShellId::for_session(&name, worktree, index);
        let w = tmux.new_session(
            &name,
            &Tmux::window_name(worktree, index),
            cwd,
            shell,
            &id,
            self.holder_width(tmux),
        )?;
        log::info!(
            "created agent session {name} (first window {})",
            w.window_id
        );
        Ok((name, id, Some(w)))
    }

    /// Create a worktree and its first agent shell. `agent` is what the
    /// popup's agent field held: a `[[agents]]` preset name, a command
    /// typed verbatim, or empty for "whatever the repository runs".
    pub fn create_worktree(&mut self, ri: usize, name: &str, agent: &str) -> Result<()> {
        let repo = self
            .state
            .repositories
            .get(ri)
            .cloned()
            .ok_or_else(|| Error::State(format!("repository index {ri} out of range")))?;
        if repo.missing {
            return Err(Error::State(format!(
                "repository '{}' is missing ({} is not a git repo)",
                repo.name,
                repo.path.display()
            )));
        }
        let tmux = self.require_tmux()?;
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::State("worktree name is empty".into()));
        }
        let slug = crate::git::slugify(name);
        if repo.worktrees.iter().any(|f| f.branch == slug) {
            return Err(Error::State(format!(
                "worktree '{}' already exists in '{}'",
                name, repo.name
            )));
        }
        if Git.branch_exists(&repo.path, &slug) {
            return Err(Error::Git(format!(
                "branch '{slug}' already exists in {}",
                repo.path.display()
            )));
        }
        let base = Git.resolve_base_branch(&repo.path, &self.cfg.base_branch_at(&repo.path))?;
        let repo_agent = self.cfg.agent_at(&repo.path);
        let root = self
            .cfg
            .worktree_root_for(&repo.path)
            .join(&repo.name)
            .join(&slug);
        if root.exists() {
            return Err(Error::Git(format!(
                "worktree path {} already exists",
                root.display()
            )));
        }
        // The fork point, captured before anything can move: `base` will
        // grow while this worktree lives, and "has it committed anything of
        // its own?" is asked against where it started, not against where the
        // base got to.
        let base_commit = Git.rev_parse(&repo.path, &base).ok();
        Git.worktree_add(&repo.path, &root, &slug, &base)?;

        // spawn the agent shell in the repository's dedicated session (created
        // on demand; never touches the dashboard's own session)
        let wname = Tmux::window_name(&slug, 0);
        let (session_name, id, window) =
            self.ensure_agent_session(&tmux, ri, &slug, 0, &root, &repo_agent.shell)?;
        let w = match window {
            Some(w) => w,
            None => tmux.new_window(
                &session_name,
                &wname,
                &root,
                &repo_agent.shell,
                &id,
                self.holder_width(&tmux),
            )?,
        };
        // The worktree's own agent, resolved once here: what its first
        // shell runs, what its later shells default to, and what the
        // poll picks status patterns by.
        let agent_command = match self.cfg.command_for_agent(agent) {
            c if c.is_empty() => None,
            c => Some(c),
        };
        let command = agent_command.clone().unwrap_or(repo_agent.command);
        let spawn_cmd = self.agent_command_for_spawn(&command);
        self.queue_launch(&w.pane_id, spawn_cmd);
        tmux.stamp_command(&w.pane_id, &command);
        tmux.stamp_repo_path(&w.pane_id, &repo.path);

        let wt = Worktree {
            name: name.to_string(),
            branch: slug,
            path: root,
            created_at: now_str(),
            base_commit,
            agent_command,
            shells: vec![Shell {
                command: Some(command.clone()),
                index: 0,
                window_name: wname,
                window_id: w.window_id,
                pane_id: w.pane_id,
            }],
        };
        self.state.record(&wt);
        self.state.repositories[ri].worktrees.push(wt);
        if let Some(v) = self.wt_expanded.get_mut(ri) {
            v.push(true); // new rows unfold, like the ones discovery finds
        }
        // Reveal what was just created: the repository may be collapsed (past
        // the unfold cap, or folded by hand — and an empty one draws the same
        // glyph either way, so the user cannot even tell). A collapsed
        // repository emits no worktree rows, so the new worktree would be
        // invisible and `clamp_sel` would drop the selection to the top row.
        if let Some(e) = self.repo_expanded.get_mut(ri) {
            *e = true;
        }
        self.sel = Sel::Worktree(ri, self.state.repositories[ri].worktrees.len() - 1);
        self.clamp_sel();
        self.save_state()?;
        self.poll_worktree_state(true);
        self.notify(format!("worktree '{}' created", name));
        Ok(())
    }

    /// Spawn one more shell under a worktree, running `command`. An empty
    /// (or whitespace-only) `command` means "whatever this repository
    /// normally runs" — the repository's default agent command.
    pub fn add_shell(&mut self, ri: usize, wi: usize, spawn: &ShellSpawn) -> Result<()> {
        let tmux = self.require_tmux()?;
        let repo = self
            .state
            .repositories
            .get(ri)
            .cloned()
            .ok_or_else(|| Error::State(format!("repository index {ri} out of range")))?;
        if repo.missing {
            return Err(Error::State(format!(
                "repository '{}' is missing ({} is not a git repo)",
                repo.name,
                repo.path.display()
            )));
        }
        let wt = repo
            .worktrees
            .get(wi)
            .cloned()
            .ok_or_else(|| Error::State(format!("worktree index {wi} out of range")))?;
        let idx = wt.next_shell_index();
        let wname = Tmux::window_name(&wt.branch, idx);
        // A plain shell runs the user's own login shell; an agent runs
        // under the repository's configured agent shell, whatever the
        // user's interactive shell happens to be.
        let shell = match spawn {
            ShellSpawn::Plain => crate::util::login_shell(),
            ShellSpawn::Agent(_) => self.cfg.agent_at(&repo.path).shell,
        };
        let (session_name, id, window) =
            self.ensure_agent_session(&tmux, ri, &wt.branch, idx, &wt.path, &shell)?;
        let w = match window {
            Some(w) => w,
            None => tmux.new_window(
                &session_name,
                &wname,
                &wt.path,
                &shell,
                &id,
                self.holder_width(&tmux),
            )?,
        };
        let what = match spawn {
            // An empty command falls back to the repository's default, so
            // a repository whose agent_command is set is honoured even
            // when the choice carried nothing.
            ShellSpawn::Agent(c) => {
                let command = match c.trim() {
                    "" => wt
                        .agent_command
                        .clone()
                        .unwrap_or_else(|| self.cfg.agent_at(&repo.path).command),
                    c => c.to_string(),
                };
                // The command the user chose (or the repository default) is
                // what gets instrumented — M17 injects --settings only when
                // it is a recognised `claude`.
                let spawn_cmd = self.agent_command_for_spawn(&command);
                self.queue_launch(&w.pane_id, spawn_cmd);
                command
            }
            // Nothing is sent: the window already *is* the shell.
            ShellSpawn::Plain => crate::util::shell_name(&shell),
        };
        tmux.stamp_command(&w.pane_id, &what);
        tmux.stamp_repo_path(&w.pane_id, &repo.path);
        self.state.repositories[ri].worktrees[wi]
            .shells
            .push(Shell {
                command: Some(what.clone()),
                index: idx,
                window_name: wname,
                window_id: w.window_id,
                pane_id: w.pane_id,
            });
        self.save_state()?;
        self.notify(format!("shell #{idx} added ({what})"));
        Ok(())
    }

    /// Bring a shell's real pane beside the tree and put the cursor in it.
    ///
    /// This is what `Enter` does. One `swap-pane` puts the agent's own
    /// tmux pane in the slot, and one `select-pane` leaves the user typing
    /// in it — direct keys, copy-mode, search, selection and resize are
    /// tmux's own from there, confined to that pane. Moving back out is
    /// the user's own pane binding (`prefix + <-` by default); Wingmux
    /// binds no navigation key.
    ///
    /// Exactly one pane is docked at a time, so docking a different shell
    /// sends the current one home first — two swaps, never two occupied
    /// slots.
    pub fn dock_shell(&mut self, ri: usize, wi: usize, si: usize) {
        let Some(sh) = self
            .state
            .repositories
            .get(ri)
            .and_then(|p| p.worktrees.get(wi))
            .and_then(|f| f.shells.get(si))
        else {
            return;
        };
        let (pane, name) = (sh.pane_id.clone(), sh.window_name.clone());
        self.enter_pane(&pane, &name);
    }

    /// The half of [`App::dock_shell`] that is about a pane rather than a
    /// tree position — shared with the repository row, whose inline shell
    /// hangs off the repository and has no `(worktree, shell)` to be found
    /// by.
    fn enter_pane(&mut self, pane: &str, name: &str) {
        // Taken from another dashboard, unconfirmed: the case is rare and
        // the answer is always yes — someone pressed `Enter` on the row.
        // A dialog here would cost every ordinary dock a habit of dismissing
        // one. Both sides are told instead: this toast, and the loser's in
        // `reconcile_docked`.
        let taken_from = self.leased_by(pane).map(|l| l.label());
        if let Err(e) = self.dock_pane(pane, true) {
            self.notify_err(format!("dock failed: {e}"));
            log::error!("dock {name} ({pane}): {e}");
            return;
        }
        if let Some(who) = taken_from {
            self.notify(format!("{name} taken from {who}"));
        }
    }

    /// The pane of the repository's own inline shell, if it has one.
    pub fn repo_shell_pane(&self, ri: usize) -> Option<String> {
        let sh = self.state.repositories.get(ri)?.shell.as_ref()?;
        Some(sh.pane_id.clone())
    }

    /// Start the repository's inline shell if it has none yet: a plain
    /// login shell in the repository's own checkout — the base branch, the
    /// working copy the worktrees were cut from.
    ///
    /// Nothing is sent to it, for the reason a plain worktree shell gets
    /// nothing sent to it either: the window already *is* the shell. And
    /// nothing starts it but an explicit `a` on the row — spawning one
    /// per repository the cursor passed over would cost two or three
    /// seconds each for shells nobody asked for.
    pub fn ensure_repo_shell(&mut self, ri: usize) -> Result<()> {
        if self
            .state
            .repositories
            .get(ri)
            .is_some_and(|r| r.shell.is_some())
        {
            return Ok(());
        }
        let tmux = self.require_tmux()?;
        let repo = self
            .state
            .repositories
            .get(ri)
            .cloned()
            .ok_or_else(|| Error::State(format!("repository index {ri} out of range")))?;
        // `missing` is a poll's verdict and can be a few seconds old; the
        // directory is asked about here, because a shell opened in a path
        // that is not there lands wherever tmux decides to put it, which is
        // the one answer nobody wants from "run a command in this repo".
        if repo.missing || !repo.path.is_dir() {
            return Err(Error::State(format!(
                "repository '{}' is missing ({} is not a git repo)",
                repo.name,
                repo.path.display()
            )));
        }
        // The user's own shell, not `[defaults].agent_shell`: the point of
        // the row is running the commands you would type by hand, and the
        // shell you type them in is `$SHELL`.
        let shell = crate::util::login_shell();
        let wname = Tmux::window_name(crate::dock::REPO_WORKTREE, crate::dock::REPO_INDEX);
        let (session_name, id, window) = self.ensure_agent_session(
            &tmux,
            ri,
            crate::dock::REPO_WORKTREE,
            crate::dock::REPO_INDEX,
            &repo.path,
            &shell,
        )?;
        let w = match window {
            Some(w) => w,
            None => tmux.new_window(
                &session_name,
                &wname,
                &repo.path,
                &shell,
                &id,
                self.holder_width(&tmux),
            )?,
        };
        let command = crate::util::shell_name(&shell);
        tmux.stamp_command(&w.pane_id, &command);
        tmux.stamp_repo_path(&w.pane_id, &repo.path);
        self.state.repositories[ri].shell = Some(Shell {
            command: Some(command),
            index: crate::dock::REPO_INDEX,
            window_name: wname,
            window_id: w.window_id,
            pane_id: w.pane_id,
        });
        self.save_state()?;
        self.notify(format!(
            "shell started in {} ({} on {})",
            repo.name,
            crate::util::shell_name(&shell),
            Git.current_branch(&repo.path).unwrap_or_default()
        ));
        Ok(())
    }

    /// `a` on a repository row: start the inline shell if there is
    /// none, then dock it and put the cursor in it.
    fn enter_repo_shell(&mut self, ri: usize) {
        if let Err(e) = self.ensure_repo_shell(ri) {
            self.fail(&e);
            return;
        }
        let Some(pane) = self.repo_shell_pane(ri) else {
            return;
        };
        let name = self.state.repositories[ri].name.clone();
        self.enter_pane(&pane, &name);
    }

    /// The window name of the shell living in `pane`, for a message about
    /// a pane the tree still knows even though tmux has moved it.
    fn shell_name_for_pane(&self, pane: &str) -> Option<String> {
        self.state
            .repositories
            .iter()
            .flat_map(|r| {
                r.shell
                    .iter()
                    .chain(r.worktrees.iter().flat_map(|w| w.shells.iter()))
            })
            .find(|s| s.pane_id == pane)
            .map(|s| s.window_name.clone())
    }

    /// Follow the selection: what the panel shows is what the cursor is on.
    ///
    /// **No timer.** The plan called for a ~150 ms settle so that held `j`
    /// autorepeat could not fire one swap per frame; a settle, though,
    /// leaves the pane of the row you have already left sitting on screen
    /// for the length of it. So the burst is *coalesced* rather than
    /// delayed: `input_pending` is the event queue's own answer to "are
    /// more keys already waiting", and autorepeat keeps it true for as long
    /// as the key is held. The dock fires on the first frame after the keys
    /// stop — no added latency at all, and never mid-burst.
    ///
    /// What the panel owes the selection is one of two things, and never a
    /// third shell's pane left over from a row the cursor has left:
    ///
    /// - the selected shell's **real pane**, docked; or
    /// - the **preview**, when there is no pane to dock — no shell under
    ///   the cursor, or a shell another dashboard holds. A leased shell is
    ///   shown as a preview (whose title names the holder), never as
    ///   someone else's live pane, and yielding is also what keeps two idle
    ///   dashboards from trading a pane forever with nobody pressing a key.
    ///
    /// Two things it will not do:
    ///
    /// - **Steal focus.** A dock that also ran `select-pane` would take the
    ///   cursor out of the tree, and the next `j` would go to the agent.
    ///   `Enter` focuses; looking does not.
    /// - **Move anything while the docked pane holds the keyboard.**
    ///   Keyboard selection cannot move while the TUI is unfocused, but a
    ///   poll-driven reconcile can, and swapping a pane out from under
    ///   someone mid-sentence is the worst bug this design can produce.
    pub fn auto_dock(&mut self, input_pending: bool) {
        if input_pending || !matches!(self.mode, Mode::Normal) {
            return;
        }
        let want = self
            .selected_shell_pane()
            .filter(|p| self.leased_by(p).is_none());
        if want == self.docked {
            return;
        }
        // Recorded before the attempt, not after: a decline must be
        // remembered too, or a leased shell — or a pane that is simply
        // gone — is re-asked on every frame.
        if self.auto_dock_seen.as_ref() == Some(&want) {
            return;
        }
        self.auto_dock_seen = Some(want.clone());
        let Ok(tmux) = self.require_tmux() else {
            return;
        };
        let Some(own) = tmux.inside.as_ref().map(|i| i.pane_id.clone()) else {
            return;
        };
        if !tmux.pane_active(&own).unwrap_or(false) {
            log::debug!("not following the selection: the docked pane holds the keyboard");
            return;
        }
        match want {
            Some(pane) => {
                if let Err(e) = self.dock_pane(&pane, false) {
                    // Nobody asked for this one, so it does not earn a toast.
                    log::warn!("cannot follow the selection into {pane}: {e}");
                }
            }
            // Nothing here to put on screen: give the space back to the
            // preview, which can show a leased shell — and can say when
            // there is no shell at all.
            None => self.drop_slot(),
        }
    }

    /// The tmux half of [`App::dock_shell`], so the error path has one
    /// place to land. `focus` puts the cursor in the pane afterwards —
    /// what `Enter` means, and what following the selection must not do.
    fn dock_pane(&mut self, pane: &str, focus: bool) -> Result<()> {
        let tmux = self.require_tmux()?;
        // Already ours: `Enter` on the docked shell is "put me back in it",
        // not a second swap.
        if self.docked.as_deref() == Some(pane) {
            if focus {
                tmux.select_pane(pane)?;
            }
            return Ok(());
        }
        // A shell another dashboard holds is sent home BEFORE it is taken,
        // rather than swapped straight out of their window into ours. A
        // direct swap would work, but it would leave their slot sitting in
        // our window and ours in the agent's — furniture crossed over, and
        // one instance able to kill a pane in another's window on quit.
        // Undock-then-dock keeps the invariant that a slot is only ever in
        // its own dashboard's window or in the home window of the pane that
        // dashboard has docked.
        if self.leases.contains_key(pane) {
            self.send_home(&tmux, pane)?;
            self.leases.remove(pane);
        }
        // Where it lives now, asked of tmux rather than of state: this is
        // the window it must be sent back to, and it is read before the
        // swap because afterwards it says the dashboard's.
        let home = tmux
            .list_panes()?
            .into_iter()
            .find(|p| p.pane_id == pane)
            .map(|p| p.window_id)
            .ok_or_else(|| Error::Tmux(format!("{pane} is not on the server")))?;
        self.undock_current();
        let slot = self.ensure_slot()?;
        let dash = tmux
            .inside
            .as_ref()
            .map(|i| i.pane_id.clone())
            .ok_or_else(|| Error::Tmux("the dashboard is not in a tmux pane".into()))?;
        crate::dock::dock(&tmux, &slot, pane, &dash)?;
        self.docked = Some(pane.to_string());
        self.docked_home = Some(home);
        // `self.slot` is left alone: a swap moves panes, so the slot keeps
        // its id while it sits in the agent's window, and undocking brings
        // that same pane back.
        if focus {
            tmux.select_pane(pane)?;
        }
        Ok(())
    }

    /// Put an agent pane back in its own window, wherever it currently
    /// sits and whoever put it there.
    ///
    /// Its home is derived from the pane's own identity, not from any
    /// bookkeeping: the instance stealing a shell was never told where the
    /// instance holding it found the pane.
    fn send_home(&self, tmux: &Tmux, pane: &str) -> Result<()> {
        let id = tmux
            .list_panes()?
            .into_iter()
            .find(|p| p.pane_id == pane)
            .and_then(|p| p.shell)
            .ok_or_else(|| Error::Tmux(format!("{pane} is not one of our shells")))?;
        let furniture = tmux.list_furniture()?;
        let home = crate::dock::home_window(&furniture, &id, &[]).ok_or_else(|| {
            Error::Tmux(format!("{pane} has no window of its own waiting for it"))
        })?;
        crate::dock::undock(tmux, pane, &home)
    }

    /// Send the docked pane home, if there is one. Never fatal: a pane
    /// that cannot be sent back is left where it is and picked up by
    /// [`crate::dock::adopt_orphans`] at the next startup.
    pub fn undock_current(&mut self) {
        let (Some(pane), Some(home)) = (self.docked.take(), self.docked_home.take()) else {
            return;
        };
        let Ok(tmux) = self.require_tmux() else {
            return;
        };
        if let Err(e) = crate::dock::undock(&tmux, &pane, &home) {
            log::warn!("cannot undock {pane} into {home}: {e}");
        }
    }

    pub fn send_text(&mut self, ri: usize, wi: usize, si: usize, text: &str) -> Result<()> {
        let tmux = self.require_tmux()?;
        let sh = self
            .state
            .repositories
            .get(ri)
            .and_then(|p| p.worktrees.get(wi))
            .and_then(|f| f.shells.get(si))
            .cloned()
            .ok_or_else(|| Error::State("shell out of range".into()))?;
        if text.trim().is_empty() {
            return Ok(());
        }
        tmux.paste_keys(&sh.pane_id, text, true)?;
        log::info!("send to {}: {text}", sh.window_name);
        Ok(())
    }

    /// Send the docked pane home before `window` is killed.
    ///
    /// While a shell is docked, its own window holds the **slot**, not the
    /// agent pane — the swap put the agent beside the tree. Killing that
    /// window would take the furniture and leave the agent pane living in
    /// the dashboard's own window, still drawing over the panel, with no
    /// row left in the tree to say what it is. Undocking first puts the
    /// agent pane back where the kill can reach it.
    fn release_dock_before_kill(&mut self, window: &str, pane: Option<&str>) {
        let ours = self.docked_home.as_deref() == Some(window)
            || (pane.is_some() && self.docked.as_deref() == pane);
        if ours {
            self.drop_slot();
        }
    }

    /// Kill the worktree's windows now and queue the removal itself; the
    /// row stays, marked, until [`Self::drain_removals`] drops it.
    pub fn delete_worktree(&mut self, ri: usize, wi: usize, force: bool) -> Result<()> {
        let tmux = self.require_tmux()?;
        let repo = self
            .state
            .repositories
            .get(ri)
            .cloned()
            .ok_or_else(|| Error::State(format!("repository index {ri} out of range")))?;
        let wt = repo
            .worktrees
            .get(wi)
            .cloned()
            .ok_or_else(|| Error::State(format!("worktree index {wi} out of range")))?;
        if self.remover.is_removing(&wt.path) {
            return Ok(());
        }
        let session = self.session_name(&tmux, ri);
        for sh in &wt.shells {
            self.kill_shell_window(&tmux, sh);
        }
        Self::kill_session_if_empty(&tmux, session);
        // The poll would drop them too, but until then Enter would reach
        // for a dead pane.
        self.state.repositories[ri].worktrees[wi].shells.clear();
        self.remover.submit(crate::remover::Job {
            repo: repo.path.clone(),
            path: wt.path.clone(),
            name: wt.name.clone(),
            branch: wt.branch.clone(),
            force,
            delete_branch: self.cfg.worktrees.delete_branch,
        });
        self.notify(format!("deleting worktree '{}'", wt.name));
        Ok(())
    }

    /// Is the worktree's removal still running?
    pub(crate) fn worktree_removing(&self, ri: usize, wi: usize) -> bool {
        self.state
            .repositories
            .get(ri)
            .and_then(|r| r.worktrees.get(wi))
            .is_some_and(|wt| self.remover.is_removing(&wt.path))
    }

    /// Apply finished removals. Deferred while a popup or a visual selection
    /// is open: those hold worktree indices that dropping a row would shift.
    pub fn drain_removals(&mut self) {
        if !matches!(self.mode, Mode::Normal) || self.visual.is_some() {
            return;
        }
        for done in self.remover.poll() {
            self.finish_removal(done);
        }
    }

    /// Block until every queued removal is done (quit path).
    pub fn wait_removals(&mut self) {
        for done in self.remover.wait() {
            self.finish_removal(done);
        }
    }

    fn finish_removal(&mut self, done: crate::remover::Done) {
        if let Err(e) = done.result {
            self.fail(&e);
            return;
        }
        // Looked up by path: other rows may have come and gone since submit.
        // `r` may already have pruned it.
        let at = self
            .state
            .repositories
            .iter()
            .enumerate()
            .find_map(|(ri, r)| {
                r.worktrees
                    .iter()
                    .position(|w| w.path == done.path)
                    .map(|wi| (ri, wi))
            });
        if let Some((ri, wi)) = at {
            self.state.repositories[ri].worktrees.remove(wi);
            if let Some(v) = self.wt_expanded.get_mut(ri)
                && wi < v.len()
            {
                v.remove(wi);
            }
            // Keep the cursor on the row it was on.
            match &mut self.sel {
                Sel::Worktree(r, w) | Sel::Shell(r, w, _) if *r == ri && *w > wi => *w -= 1,
                _ => {}
            }
            self.clamp_sel();
        }
        self.state.forget(&done.path);
        self.wt_state.remove(&done.path);
        if let Err(e) = self.save_state() {
            log::warn!("save: {e}");
        }
        self.notify(format!("worktree '{}' deleted", done.name));
    }

    /// Close one shell: kill its tmux window and drop it from the
    /// worktree. The worktree itself — worktree, branch, sibling shells —
    /// is untouched; deleting the worktree is `d` on the worktree row.
    pub fn delete_shell(&mut self, ri: usize, wi: usize, si: usize) -> Result<()> {
        let tmux = self.require_tmux()?;
        let repo = self
            .state
            .repositories
            .get(ri)
            .cloned()
            .ok_or_else(|| Error::State(format!("repository index {ri} out of range")))?;
        let wt = repo
            .worktrees
            .get(wi)
            .cloned()
            .ok_or_else(|| Error::State(format!("worktree index {wi} out of range")))?;
        let sh = wt
            .shells
            .get(si)
            .cloned()
            .ok_or_else(|| Error::State(format!("shell index {si} out of range")))?;
        let session = self.session_name(&tmux, ri);
        self.kill_shell_window(&tmux, &sh);
        self.state.repositories[ri].worktrees[wi].shells.remove(si);
        Self::kill_session_if_empty(&tmux, session);
        self.clamp_sel();
        self.save_state()?;
        self.poll_worktree_state(true);
        self.notify(format!("shell '{}' closed", sh.window_name));
        Ok(())
    }

    /// Killing a session's last window destroys the session; if it
    /// survives with no shells left, kill it too.
    ///
    /// "No shells left" is asked of the identities, not of the session's
    /// windows: a shell belongs to its session wherever its pane currently
    /// sits, and a window that merely looks like ours is not one.
    fn kill_session_if_empty(tmux: &Tmux, session: Option<String>) {
        if let Some(sess) = session
            && tmux.session_exists(&sess)
            && let Some(slug) = sess.strip_prefix(crate::tmux::SESSION_PREFIX)
            && tmux
                .list_panes()
                .map(|ps| {
                    !ps.iter()
                        .any(|p| p.shell.as_ref().is_some_and(|id| id.repo == slug))
                })
                .unwrap_or(false)
        {
            log::info!("agent session {sess} has no shells left, killing it");
            let _ = tmux.kill_session(&sess);
        }
    }

    /// The agent session of repository `ri`, read off its live panes. Asked
    /// before a kill: once the last pane is gone nothing names it.
    fn session_name(&self, tmux: &Tmux, ri: usize) -> Option<String> {
        tmux.list_panes()
            .ok()
            .and_then(|ps| self.state.session_of(&ps, ri))
    }

    fn kill_shell_window(&mut self, tmux: &Tmux, sh: &Shell) {
        self.release_dock_before_kill(&sh.window_id, Some(&sh.pane_id));
        // A poll taken while docked recorded the dashboard's window; ask
        // where the pane sits now that it is home.
        let window = tmux
            .pane_geometry(&sh.pane_id)
            .map(|(_, _, w)| w)
            .unwrap_or_else(|_| sh.window_id.clone());
        if tmux.inside.as_ref().is_some_and(|i| i.window_id == window) {
            log::warn!(
                "not killing {window} ({}): it is the dashboard's own window",
                sh.window_name
            );
            let _ = tmux.kill_pane(&sh.pane_id);
            return;
        }
        match tmux.kill_window(&window) {
            Ok(()) => log::info!("killed window {window} ({})", sh.window_name),
            Err(e) => log::warn!("kill-window {window}: {e}"),
        }
    }

    // ---------------------------------------------------------- notices

    /// Show a transient notice. It fades on its own; the log keeps the
    /// permanent record.
    pub fn notify(&mut self, text: impl Into<String>) {
        let text = text.into();
        log::info!("{text}");
        self.message = Some(Toast {
            text,
            error: false,
            at: Instant::now(),
        });
    }

    /// Same, for something that went wrong: longer TTL, error colours.
    pub fn notify_err(&mut self, text: impl Into<String>) {
        let text = text.into();
        log::warn!("{text}");
        self.message = Some(Toast {
            text,
            error: true,
            at: Instant::now(),
        });
    }

    /// Age the current toast past its TTL (tests only — the renderer has
    /// no clock to fake).
    #[cfg(test)]
    pub fn expire_toast_for_test(&mut self) {
        if let Some(t) = self.message.as_mut() {
            t.at = Instant::now() - TOAST_TTL_ERR - Duration::from_secs(1);
        }
    }

    /// The notice to draw right now, or `None` once it has expired.
    pub fn toast(&self) -> Option<&Toast> {
        self.message.as_ref().filter(|t| !t.expired())
    }

    // ---------------------------------------------------- worktree state

    /// Cached worktree state for the tree — filled by
    /// [`App::poll_worktree_state`], never a git call on the render path.
    pub fn worktree_state_cached(&self, ri: usize, wi: usize) -> WorktreeState {
        self.state
            .repositories
            .get(ri)
            .and_then(|p| p.worktrees.get(wi))
            .and_then(|f| self.wt_state.get(&f.path))
            .copied()
            .unwrap_or_default()
    }

    /// Refresh the commit-state cache, at most every [`WT_STATE_INTERVAL`].
    /// Called from the poll tick; `force` skips the interval after a change
    /// that invalidates it (worktree created/deleted).
    ///
    /// The base branch is resolved ONCE per repository — every worktree of a
    /// repository is measured against the same base, and resolving it per
    /// worktree would multiply the git calls for an answer that cannot
    /// differ. A missing repository is skipped: its path is gone, so every
    /// call would fail and the entries would churn between states.
    pub fn poll_worktree_state(&mut self, force: bool) {
        if !force
            && self
                .last_wt_state_poll
                .is_some_and(|t| t.elapsed() < WT_STATE_INTERVAL)
        {
            return;
        }
        self.last_wt_state_poll = Some(Instant::now());
        let cfg = &self.cfg;
        let bases: HashMap<PathBuf, String> = self
            .state
            .repositories
            .iter()
            .filter(|p| !p.missing)
            .filter_map(|p| {
                let base = Git
                    .resolve_base_branch(&p.path, &cfg.base_branch_at(&p.path))
                    .ok()?;
                Some((p.path.clone(), base))
            })
            .collect();
        let work: Vec<(PathBuf, String, String)> = self
            .state
            .repositories
            .iter()
            .filter_map(|p| {
                let base = bases.get(&p.path)?.clone();
                let remover = &self.remover;
                Some(
                    p.worktrees
                        .iter()
                        .filter(|f| !remover.is_removing(&f.path))
                        .map(move |f| {
                            // No recorded fork point (reconciled worktree, or an
                            // older state file): measure against the base branch and
                            // accept that a merged worktree reads as new.
                            let fork = f.base_commit.clone().unwrap_or_else(|| base.clone());
                            (f.path.clone(), fork, base.clone())
                        }),
                )
            })
            .flatten()
            .collect();
        self.wt_state = work
            .into_iter()
            .map(|(path, fork, base)| {
                let st = Git.worktree_state(&path, &fork, &base).unwrap_or_default();
                (path, st)
            })
            .collect();
        self.base_branches = bases;
    }

    /// Is the worktree's worktree dirty? (drives the --force confirm option)
    pub fn worktree_is_dirty(&self, ri: usize, wi: usize) -> bool {
        self.state
            .repositories
            .get(ri)
            .and_then(|p| p.worktrees.get(wi))
            .and_then(|f| Git.worktree_dirty(&f.path).ok())
            .unwrap_or(false)
    }

    /// The lock reason when the worktree is locked (drives the `--force`
    /// confirm option: git refuses to remove a locked worktree).
    pub fn worktree_lock(&self, ri: usize, wi: usize) -> Option<String> {
        let wt = self.state.repositories.get(ri)?.worktrees.get(wi)?;
        Git.worktree_lock(&wt.path)
    }

    /// Does the worktree hold commits the base branch does not have?
    /// (drives the delete confirm's wording, not the `--force` option:
    /// `git worktree remove` is happy to remove a worktree whose branch is
    /// ahead.) Asked fresh rather than read off `wt_state`, which is a
    /// cache polled on an interval and would answer for the tree as it was
    /// up to [`WT_STATE_INTERVAL`] ago.
    pub fn worktree_has_unmerged_commits(&self, ri: usize, wi: usize) -> bool {
        let Some(repo) = self.state.repositories.get(ri) else {
            return false;
        };
        let Some(wt) = repo.worktrees.get(wi) else {
            return false;
        };
        let Ok(base) = Git.resolve_base_branch(&repo.path, &self.cfg.base_branch_at(&repo.path))
        else {
            return false;
        };
        let fork = wt.base_commit.clone().unwrap_or_else(|| base.clone());
        Git.worktree_committed_ahead(&wt.path, &fork, &base)
            .unwrap_or(false)
    }

    pub fn confirm_delete_worktree(&mut self) {
        let Some((ri, wi, _)) = self.selected_worktree_ctx() else {
            self.notify("Select a worktree first");
            return;
        };
        if self.worktree_removing(ri, wi) {
            self.notify("Worktree is already being deleted");
            return;
        }
        let lock = self.worktree_lock(ri, wi);
        let locked = lock.is_some();
        let dirty = self.worktree_is_dirty(ri, wi);
        let unmerged = self.worktree_has_unmerged_commits(ri, wi);
        let wt = &self.state.repositories[ri].worktrees[wi];
        self.mode = Mode::Confirm {
            confirm: Confirm {
                title: "Delete Worktree".into(),
                // Three beats, blank line between each: the question, the
                // path it is about, the state of the tree. The state line
                // says only clean or not — *what* is unclean is the
                // warning line's job, and saying it twice made the dialog
                // read as two different problems.
                //
                // "clean" is claimed only when the worktree carries no work
                // at all: an unmerged branch with a clean tree is still
                // work, and calling that clean is the one lie the dialog
                // must not tell.
                body: format!(
                    "Delete worktree '{}'?\n\n{}\n\n{}",
                    wt.name,
                    wt.path.display(),
                    if dirty || unmerged || locked {
                        "Worktree is NOT clean"
                    } else {
                        "Worktree is clean"
                    }
                ),
                // Dirty: `git worktree remove` refuses, so `f` is the only
                // answer that deletes anything. Unmerged commits do not
                // block the removal — the branch outlives the worktree.
                yes_label: (!dirty && !locked).then(|| "yes".into()),
                // Which caveat is the true one depends on
                // `worktrees.delete_branch`: with it on (the default) the
                // branch is deleted with `-D`, so unmerged commits are
                // lost, not parked.
                warning: match (dirty, unmerged) {
                    _ if locked => {
                        let reason = lock.as_deref().unwrap_or_default();
                        let mut w = String::from("Worktree is locked");
                        if !reason.is_empty() {
                            w.push_str(&format!(" ({reason})"));
                        }
                        w.push_str(" — force delete unlocks and removes it");
                        if dirty {
                            w.push_str(", discarding uncommitted changes");
                        }
                        Some(w)
                    }
                    (true, true) if self.cfg.worktrees.delete_branch => Some(
                        "Uncommitted changes and unmerged commits — force delete discards both"
                            .into(),
                    ),
                    (true, true) => Some(
                        "Uncommitted changes — force delete discards them; the branch keeps its \
                         commits"
                            .into(),
                    ),
                    (true, false) => {
                        Some("Uncommitted changes — force delete discards them".into())
                    }
                    (false, true) if self.cfg.worktrees.delete_branch => {
                        Some("Unmerged commits — deleting the branch discards them".into())
                    }
                    (false, true) => {
                        Some("Unmerged commits — the worktree goes, its branch keeps them".into())
                    }
                    (false, false) => None,
                },
                force_label: if dirty {
                    Some("force delete (discards changes)".into())
                } else if locked {
                    Some("force delete (locked)".into())
                } else {
                    None
                },
                action: ConfirmAction::DeleteWorktree {
                    repository: ri,
                    worktree: wi,
                },
            },
        };
    }

    pub fn confirm_delete_shell(&mut self) {
        let Sel::Shell(ri, wi, si) = self.sel else {
            self.notify("Select a shell first");
            return;
        };
        let wt = &self.state.repositories[ri].worktrees[wi];
        let sh = &wt.shells[si];
        self.mode = Mode::Confirm {
            confirm: Confirm {
                title: "Close Shell".into(),
                body: format!(
                    "Close shell '{}' of worktree '{}'?\nThe tmux window is killed; the worktree and its other shells stay.",
                    sh.window_name, wt.name
                ),
                yes_label: Some("yes".into()),
                warning: None,
                force_label: None,
                action: ConfirmAction::DeleteShell {
                    repository: ri,
                    worktree: wi,
                    shell: si,
                },
            },
        };
    }

    pub fn confirm_delete_repo_shell(&mut self, ri: usize) {
        let Some(repo) = self.state.repositories.get(ri) else {
            return;
        };
        if repo.shell.is_none() {
            self.notify("This repository has no shell");
            return;
        }
        self.mode = Mode::Confirm {
            confirm: Confirm {
                title: "Close Shell".into(),
                body: format!(
                    "Close the inline shell of repository '{}'?\nThe tmux window is killed; the repository, its worktrees and their agents stay.",
                    repo.name
                ),
                yes_label: Some("yes".into()),
                warning: None,
                force_label: None,
                action: ConfirmAction::CloseRepoShell { repository: ri },
            },
        };
    }

    /// Close a repository's inline shell: kill its tmux window and drop it
    /// from the repository. `a` on the row starts a fresh one.
    pub fn delete_repo_shell(&mut self, ri: usize) -> Result<()> {
        let tmux = self.require_tmux()?;
        let repo = self
            .state
            .repositories
            .get(ri)
            .cloned()
            .ok_or_else(|| Error::State(format!("repository index {ri} out of range")))?;
        let Some(sh) = repo.shell.clone() else {
            return Err(Error::State(format!(
                "repository '{}' has no shell",
                repo.name
            )));
        };
        let session = self.session_name(&tmux, ri);
        self.kill_shell_window(&tmux, &sh);
        self.state.repositories[ri].shell = None;
        Self::kill_session_if_empty(&tmux, session);
        self.save_state()?;
        self.notify(format!("shell of '{}' closed", repo.name));
        Ok(())
    }

    /// Open the add-shell picker (`a`) on the selected worktree. The
    /// cursor starts on the first entry — the repository's own agent
    /// command — so the common case is still `a` then Enter. A repository
    /// has one plain shell and no picker.
    fn open_add_shell(&mut self) {
        if let Sel::Repository(ri) = self.sel {
            self.enter_repo_shell(ri);
        } else if let Some((ri, wi, _)) = self.selected_worktree_ctx() {
            if self.worktree_removing(ri, wi) {
                self.notify("Worktree is being deleted");
                return;
            }
            self.mode = Mode::AddShell { index: 0 };
        } else {
            self.notify("Select a worktree to add a shell");
        }
    }

    /// The rows of the add-shell picker, in order: the selected
    /// repository's agent command first (the default, and what Enter
    /// takes), then every other configured agent preset, then a plain
    /// login shell. The plain entry is why the picker exists: starting a
    /// normal `zsh`/`bash` in a worktree used to mean typing over an
    /// agent-command prompt.
    pub fn add_shell_choices(&self) -> Vec<ShellChoice> {
        let (ri, wi) = match self.sel {
            Sel::Repository(ri) => (ri, None),
            Sel::Worktree(ri, wi) | Sel::Shell(ri, wi, _) => (ri, Some(wi)),
        };
        let repo = self.state.repositories.get(ri);
        // The worktree's own agent leads when it chose one at creation —
        // "one more of what is already running here" is the reason the
        // first row exists, and that is the worktree's agent, not the
        // repository's.
        let default = wi
            .and_then(|wi| repo?.worktrees.get(wi)?.agent_command.clone())
            .or_else(|| repo.map(|p| self.cfg.agent_at(&p.path).command))
            .unwrap_or_default()
            .trim()
            .to_string();
        let mut out: Vec<ShellChoice> = Vec::new();
        let push = |cmd: &str, out: &mut Vec<ShellChoice>| {
            if cmd.is_empty() || out.iter().any(|c| c.label == cmd) {
                return;
            }
            out.push(ShellChoice {
                label: cmd.to_string(),
                spawn: ShellSpawn::Agent(cmd.to_string()),
            });
        };
        push(&default, &mut out);
        for agent in &self.cfg.agents {
            push(agent.command.trim(), &mut out);
        }
        let shell = crate::util::login_shell();
        out.push(ShellChoice {
            label: format!("{} (plain shell)", crate::util::shell_name(&shell)),
            spawn: ShellSpawn::Plain,
        });
        out
    }

    /// The cursor of the open add-shell picker (0 when it is not open).
    fn add_shell_index(&self) -> usize {
        match self.mode {
            Mode::AddShell { index } => index,
            _ => 0,
        }
    }

    /// Move the picker cursor to `n`, clamped to the offered entries.
    fn add_shell_to(&mut self, n: usize) {
        let last = self.add_shell_choices().len().saturating_sub(1);
        if let Mode::AddShell { index } = &mut self.mode {
            *index = n.min(last);
        }
    }

    /// Close the picker and create the shell entry `i` describes.
    fn take_shell_choice(&mut self, i: usize) {
        let Some(choice) = self.add_shell_choices().into_iter().nth(i) else {
            return;
        };
        self.mode = Mode::Normal;
        let res = match self.selected_worktree_ctx() {
            Some((ri, wi, _)) => self.add_shell(ri, wi, &choice.spawn),
            None => Err(Error::State("select a worktree first".into())),
        };
        if let Err(e) = res {
            self.fail(&e);
        }
    }

    /// Open the New Worktree popup for the selected repository, resolving the
    /// branch the worktree would be cut from up front so the popup can name
    /// it.
    pub(crate) fn open_new_worktree(&mut self) {
        let ri = match self.sel {
            Sel::Repository(ri) | Sel::Worktree(ri, _) => ri,
            Sel::Shell(..) => {
                self.notify("Select a repository to add a worktree");
                return;
            }
        };
        self.new_worktree_base = self.base_hint(ri);
        let agent = self
            .state
            .repositories
            .get(ri)
            .map(|p| {
                self.cfg
                    .agent_name_for_command(&self.cfg.agent_at(&p.path).command)
            })
            .unwrap_or_else(|| self.cfg.defaults.agent.clone());
        self.mode = Mode::Input {
            text: String::new(),
            agent: Some(AgentField {
                text: agent,
                focused: false,
            }),
        };
    }

    /// Resolve what `create_worktree` would branch from for repository `ri` —
    /// same call, so the popup cannot promise a base the creation then
    /// ignores. `None` for a missing repo (creation refuses it anyway).
    fn base_hint(&self, ri: usize) -> Option<BaseHint> {
        let repo = self.state.repositories.get(ri)?;
        if repo.missing {
            return None;
        }
        let configured = self.cfg.base_branch_at(&repo.path).trim().to_string();
        match Git.resolve_base_branch(&repo.path, &configured) {
            Ok(branch) => Some(BaseHint {
                branch,
                missing: false,
            }),
            Err(_) if !configured.is_empty() => Some(BaseHint {
                branch: configured,
                missing: true,
            }),
            Err(_) => None,
        }
    }

    /// Base branch of the repository at `path` without a git call: the
    /// configured one, else the current branch as of the last state poll.
    pub fn base_branch_of(&self, path: &std::path::Path) -> Option<String> {
        let configured = self.cfg.base_branch_at(path).trim().to_string();
        if configured.is_empty() {
            self.base_branches.get(path).cloned()
        } else {
            Some(configured)
        }
    }

    /// What the New Worktree popup should say the branch is cut from.
    pub fn new_worktree_base(&self) -> Option<&BaseHint> {
        self.new_worktree_base.as_ref()
    }

    fn selected_worktree_ctx(&self) -> Option<(usize, usize, Option<usize>)> {
        match self.sel {
            Sel::Worktree(ri, wi) => Some((ri, wi, None)),
            Sel::Shell(ri, wi, si) => Some((ri, wi, Some(si))),
            _ => None,
        }
    }

    // ---------------------------------------------------------- polling

    /// Compiled status patterns for a repository (cached by path).
    fn patterns_for(&mut self, repo_path: &str, agent_command: &str) -> Compiled {
        let key = format!("{repo_path}\u{1f}{agent_command}");
        if let Some(c) = self.pattern_cache.get(&key) {
            return c.clone();
        }
        let patterns = self
            .cfg
            .patterns_for(agent_command)
            .unwrap_or_else(crate::config::generic_patterns);
        let compiled = Compiled::compile(&patterns);
        self.pattern_cache.insert(key, compiled.clone());
        compiled
    }

    /// Poll tmux: pane metadata + capture tail → statuses, preview.
    /// Owe `command` to a shell that has just been spawned: it is typed by
    /// the first poll that finds the pane ready. See [`PendingLaunch`] for
    /// why it is not sent right here.
    fn queue_launch(&mut self, pane_id: &str, command: String) {
        self.pending_launches.push(PendingLaunch {
            pane_id: pane_id.to_string(),
            command,
            deadline: Instant::now() + LAUNCH_GRACE,
        });
    }

    /// Type the queued spawn commands into the shells that are ready for
    /// them. Readiness is "the pane has drawn something" — a prompt on
    /// screen means the shell's line editor is up and will take the keys
    /// as a line, echo it once, and run it.
    ///
    /// Nothing here costs a tmux call of its own: the poll's single
    /// `list-panes` already carries the cursor position.
    fn drain_pending_launches(&mut self, tmux: &Tmux) {
        if self.pending_launches.is_empty() {
            return;
        }
        let now = Instant::now();
        let pending = std::mem::take(&mut self.pending_launches);
        for p in pending {
            let pane = self.panes.get(&p.pane_id);
            let ready = pane.is_some_and(|i| !i.blank);
            let expired = now >= p.deadline;
            if !ready && !expired {
                self.pending_launches.push(p);
                continue;
            }
            // A pane that never appeared has died before it could run
            // anything; typing into it is at best pointless.
            if pane.is_none() {
                log::warn!("pane {} vanished before '{}' ran", p.pane_id, p.command);
                continue;
            }
            if !ready {
                log::warn!(
                    "pane {} drew no prompt in {LAUNCH_GRACE:?}; typing '{}' anyway",
                    p.pane_id,
                    p.command
                );
            }
            if let Err(e) = tmux.send_keys(&p.pane_id, &p.command, true) {
                log::warn!("cannot start '{}' in {}: {e}", p.command, p.pane_id);
                self.notify_err(format!("cannot start agent: {e}"));
            }
        }
    }

    pub fn poll(&mut self) {
        // Stamped FIRST, and unconditionally: every early return below is
        // a reason to wait for the next tick, not to poll again straight
        // away. Without this the event loop's "is a poll due" test stayed
        // true for ever, its input timeout collapsed to zero, and the
        // dashboard span-polled — spawning a `capture-pane` per shell per
        // frame, and retrying a failing dock ~90 times a second.
        self.last_poll = Instant::now();
        // Independent of tmux (and of its own cadence) — the tree draws
        // the dirty flag whether or not a session exists.
        self.poll_worktree_state(false);
        // Same reasoning, and before the early return below for the same
        // reason: a pad typed on a server that has gone away must still
        // reach the disk.
        self.save_todo();
        self.load_repo_todos();
        let Ok(tmux) = self.require_tmux() else {
            return;
        };
        // Before anything reads a pane size: a dashboard strip that has
        // been resized takes every agent window with it.
        self.refresh_strip(&tmux);
        // One extra tmux call, and only while a pad is out at the editor:
        // that window is unstamped, so the pane map below cannot see it.
        self.reap_editor(&tmux);
        let panes = match tmux.list_panes() {
            Ok(p) => p,
            Err(e) => {
                log::warn!("poll list-panes: {e}");
                return;
            }
        };
        self.panes = panes
            .iter()
            .map(|p| (p.pane_id.clone(), p.clone()))
            .collect();
        self.state.sync_shells(&panes);
        self.clamp_sel();
        // Straight after the pane map is refreshed: a shell spawned since
        // the last poll may have finished starting up and be owed its
        // first command.
        self.drain_pending_launches(&tmux);
        self.refresh_leases(&tmux, &panes);
        self.reconcile_docked(&tmux, &panes);
        // A decline is only good for the poll that made it: a lease may
        // have been released, or the pane may have come back.
        self.auto_dock_seen = None;
        let sel_pane = self.selected_shell_pane();
        // Collect shell metadata first to avoid borrowing self twice.
        let mut shells: Vec<(String, String, String)> = Vec::new();
        for repo in &self.state.repositories {
            let path = repo.path.to_string_lossy().into_owned();
            let cmd = self.cfg.agent_at(&repo.path).command;
            // The inline shell is polled like any other: the preview needs
            // its capture, and `Status::Dead` is how a row learns that the
            // shell behind it exited.
            if let Some(sh) = &repo.shell {
                shells.push((path.clone(), cmd.clone(), sh.pane_id.clone()));
            }
            for wt in &repo.worktrees {
                let wt_cmd = wt.agent_command.clone().unwrap_or_else(|| cmd.clone());
                for sh in &wt.shells {
                    shells.push((path.clone(), wt_cmd.clone(), sh.pane_id.clone()));
                }
            }
        }
        for (path, cmd, pid) in shells {
            // A pane tmux no longer lists is a dead shell (ctrl-d, or the
            // window killed outside the dashboard): there is nothing to
            // capture, and the classifier reads `None` as dead. Skipping
            // it instead would leave the last live status on screen
            // forever.
            let mut pane = self.panes.get(&pid).cloned();
            if !self.cfg.defaults.agent_hooks
                && let Some(p) = pane.as_mut()
            {
                // The kill switch: `agent_hooks = false` must classify
                // purely from `capture-pane`, exactly as before this
                // worktree existed. Turning the flag off only stops writing
                // new overlays — a pane instrumented earlier (or by another
                // Wingmux instance sharing the tmux server) still
                // carries one, and it outlives both a restart and this
                // flag flip. `classify` has no way to know the flag is
                // off, so silence the overlay here, the one seam every
                // poll goes through, rather than scattering the check.
                p.overlay_raw.clear();
            }
            let tail = match &pane {
                None => String::new(),
                Some(_) => match tmux.capture_pane(&pid, 120) {
                    Ok(t) => t,
                    Err(e) => {
                        log::debug!("capture-pane {pid}: {e}");
                        continue;
                    }
                },
            };
            let recent = recent_tail(&tail);
            let pat = self.patterns_for(&path, &cmd);
            let st = status::classify(&pat, &recent, pane.as_ref());
            self.statuses.insert(pid.clone(), st);
            // A gone pane captured nothing — keep the last capture around
            // so the preview still shows how the shell ended.
            if pane.is_some() && Some(&pid) == sel_pane.as_ref() {
                self.captures.insert(pid.clone(), tail);
            }
        }
    }

    // ---------------------------------------------------------- keys

    pub fn on_key(&mut self, key: KeyEvent) {
        // Accessibility layer, dispatched above everything (the ctrl layer
        // works over text input and overlays too): PageUp/PageDown and
        // ctrl-u/ctrl-d half-page, ctrl-f/ctrl-b full-page, Home/End
        // first/last, ctrl-c quit — always, from any state. Handled keys
        // are consumed: they must not fall through to a mode handler
        // (ctrl-d is navigation here, never "delete worktree"). While the
        // help overlay is open these motions scroll the help (the palette
        // takes over the viewport); while the theme picker is open they
        // move the picker cursor; elsewhere they move the tree.
        //
        // ctrl-t opens the live theme picker (a chord, so it dispatches
        // over every state — a theme switch is never behind a prompt).
        // The todo pad owns its own motions: while it is open the
        // page/Home/End keys move the caret through the text, so the
        // accessibility layer keeps only ctrl-c (quit) and ctrl-t (theme).
        let in_todo = matches!(self.mode, Mode::Todo { .. });
        let handled = match key.code {
            KeyCode::PageUp if !in_todo => {
                self.move_view(-1);
                true
            }
            KeyCode::PageDown if !in_todo => {
                self.move_view(1);
                true
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) && !in_todo => {
                self.move_view(-1);
                true
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) && !in_todo => {
                self.move_view(1);
                true
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) && !in_todo => {
                self.move_view(2);
                true
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) && !in_todo => {
                self.move_view(-2);
                true
            }
            KeyCode::Home if !in_todo => {
                self.move_view_first();
                true
            }
            KeyCode::End if !in_todo => {
                self.move_view_last();
                true
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
                true
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Park an open pad first: the picker covers the panel, and
                // a theme switch must not eat the text.
                self.close_todo();
                self.open_theme_picker();
                true
            }
            _ => false,
        };
        if handled {
            return;
        }
        // `g`/`G` (first/last) are normal-mode motions only: printable
        // keys belong to text input everywhere else.
        if matches!(self.mode, Mode::Normal) {
            match key.code {
                KeyCode::Char('g') => self.move_sel_first(),
                KeyCode::Char('G') => self.move_sel_last(),
                _ => {}
            }
        }
        // The pad's page size and soft-wrap width are the tree panel's —
        // resolved here, before `mode` is borrowed.
        let todo_lines = todo_visible_lines(self);
        let todo_wrap = todo_wrap_width(self);
        // The add-shell picker's row count, resolved before the borrow of
        // `mode` below (its arm cannot call back into `self`).
        let shell_choices = match self.mode {
            Mode::AddShell { .. } => self.add_shell_choices().len(),
            _ => 0,
        };
        match &mut self.mode {
            Mode::Input { text, agent } => match key.code {
                KeyCode::Esc => self.mode = Mode::Normal,
                KeyCode::Enter => {
                    let text = std::mem::take(text);
                    let agent = agent.as_ref().map(|a| a.text.clone()).unwrap_or_default();
                    self.mode = Mode::Normal;
                    self.submit_new_worktree(&text, &agent);
                }
                // Tab moves between the two fields; with only one there is
                // nowhere to go and the key does nothing.
                KeyCode::Tab | KeyCode::BackTab => {
                    if let Some(a) = agent {
                        a.focused = !a.focused;
                    }
                }
                // The agent field is a short list in practice, so the
                // arrows walk the configured presets rather than asking
                // for the name to be typed out.
                KeyCode::Up | KeyCode::Down if agent.as_ref().is_some_and(|a| a.focused) => {
                    let step: isize = if key.code == KeyCode::Up { -1 } else { 1 };
                    let names: Vec<String> =
                        self.cfg.agents.iter().map(|a| a.name.clone()).collect();
                    if let Mode::Input { agent: Some(a), .. } = &mut self.mode
                        && !names.is_empty()
                    {
                        let at = names.iter().position(|n| *n == a.text);
                        let next = match at {
                            Some(i) => {
                                (i as isize + step).rem_euclid(names.len() as isize) as usize
                            }
                            None => 0,
                        };
                        a.text = names[next].clone();
                    }
                }
                KeyCode::Backspace => {
                    match agent {
                        Some(a) if a.focused => a.text.pop(),
                        _ => text.pop(),
                    };
                }
                KeyCode::Char(c) => match agent {
                    Some(a) if a.focused => a.text.push(c),
                    _ => text.push(c),
                },
                _ => {}
            },
            Mode::Todo { buf, pad } => {
                // The panel *is* the editor, so every printable key is
                // text and there is no navigation layer underneath: `Esc`
                // is the way back to the tree, and it saves. ctrl-s saves
                // without leaving, for a pad kept open across a session.
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                if key.code == KeyCode::Esc {
                    self.close_todo();
                } else if key.code == KeyCode::Char('s') && ctrl {
                    let (parked, pad) = (buf.clone(), pad.clone());
                    self.park_todo(pad, parked);
                    self.notify("Todo saved");
                } else if key.code == KeyCode::Char('e') && ctrl {
                    self.edit_todo_externally();
                } else {
                    let edited = match key.code {
                        KeyCode::Enter => {
                            buf.newline();
                            true
                        }
                        KeyCode::Tab => {
                            buf.insert_tab();
                            true
                        }
                        KeyCode::Backspace => {
                            buf.backspace();
                            true
                        }
                        KeyCode::Delete => {
                            buf.delete();
                            true
                        }
                        KeyCode::Left => {
                            buf.move_left();
                            false
                        }
                        KeyCode::Right => {
                            buf.move_right();
                            false
                        }
                        KeyCode::Up => {
                            buf.move_up();
                            false
                        }
                        KeyCode::Down => {
                            buf.move_down();
                            false
                        }
                        KeyCode::Home => {
                            buf.move_home();
                            false
                        }
                        KeyCode::End => {
                            buf.move_end();
                            false
                        }
                        KeyCode::PageUp => {
                            buf.move_page(-1, buf.page(todo_lines));
                            false
                        }
                        KeyCode::PageDown => {
                            buf.move_page(1, buf.page(todo_lines));
                            false
                        }
                        KeyCode::Char(c) if ctrl => {
                            // The pad owns ctrl-u/d/f/b too (the
                            // accessibility layer yielded them).
                            match c {
                                'u' => buf.move_page(-1, buf.page(todo_lines)),
                                'd' => buf.move_page(1, buf.page(todo_lines)),
                                'f' => buf.move_page(1, todo_lines.saturating_sub(1)),
                                'b' => buf.move_page(-1, todo_lines.saturating_sub(1)),
                                _ => false,
                            };
                            false
                        }
                        KeyCode::Char(c) => {
                            buf.insert_char(c);
                            true
                        }
                        _ => false,
                    };
                    // Scrolling is decided here, where the panel's real
                    // content height and wrap width are known.
                    buf.ensure_visible(todo_lines, todo_wrap);
                    if edited {
                        // The text itself is not copied out on every
                        // keystroke — `save_todo` reads the open buffer.
                        // This only says there is something to write.
                        self.todo_dirty = Some(pad.clone());
                    }
                }
            }
            Mode::Confirm { confirm } => match key.code {
                // `d` opened this popup, so `d` again closes the loop —
                // the vim `dd` reflex confirms without a hand move. `Enter`
                // is deliberately NOT bound: it is the app's "open / drill
                // in" key everywhere else, and a destructive answer must
                // not sit under the key the fingers press by reflex.
                KeyCode::Char('y') | KeyCode::Char('d') if confirm.yes_label.is_some() => {
                    let action = confirm.action.clone();
                    self.mode = Mode::Normal;
                    self.run_confirm(action, false);
                }
                KeyCode::Char('f') if confirm.force_label.is_some() => {
                    let action = confirm.action.clone();
                    self.mode = Mode::Normal;
                    self.run_confirm(action, true);
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
                    self.mode = Mode::Normal;
                }
                _ => {}
            },
            Mode::Help => match key.code {
                KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                    self.mode = Mode::Normal;
                }
                // The palette scrolls with the same motions as the tree
                // (j/k + arrows step by line; g/G jump to first/last).
                KeyCode::Char('j') | KeyCode::Down => {
                    self.help_scroll_to(self.help_scroll + 1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.help_scroll_to(self.help_scroll.saturating_sub(1));
                }
                KeyCode::Char('g') => self.help_scroll_to(0),
                KeyCode::Char('G') => self.help_scroll_to(usize::MAX),
                _ => {}
            },
            Mode::Diagnostics => match key.code {
                // Dismiss: the diagnostic list is informational; the
                // session proceeds normally. Clicking outside also dismisses.
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Char('?') => {
                    self.close_diagnostics();
                }
                // Same motions as the help palette (j/k + arrows, g/G).
                KeyCode::Char('j') | KeyCode::Down => {
                    self.diag_scroll_to(self.diag_scroll + 1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.diag_scroll_to(self.diag_scroll.saturating_sub(1));
                }
                KeyCode::Char('g') => self.diag_scroll_to(0),
                KeyCode::Char('G') => self.diag_scroll_to(usize::MAX),
                _ => {}
            },
            Mode::ThemePicker { index } => match key.code {
                // j/k + arrows step by one theme (previewing live);
                // g/G jump to first/last. PgUp/PgDn etc. are handled by the
                // accessibility layer above.
                KeyCode::Char('j') | KeyCode::Down => {
                    *index = (*index + 1).min(self.theme_names.len().saturating_sub(1));
                    self.preview_picker_theme();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    *index = index.saturating_sub(1);
                    self.preview_picker_theme();
                }
                KeyCode::Char('g') => self.picker_to(0),
                KeyCode::Char('G') => self.picker_to(usize::MAX),
                // Enter keeps the previewed theme (persisted to state —
                // the config file is never rewritten), Esc/`q` restores
                // the theme the picker opened with.
                KeyCode::Enter => self.keep_picked_theme(),
                KeyCode::Esc | KeyCode::Char('q') => self.cancel_theme_picker(),
                _ => {}
            },
            Mode::AddShell { index } => {
                let last = shell_choices.saturating_sub(1);
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => *index = (*index + 1).min(last),
                    KeyCode::Char('k') | KeyCode::Up => *index = index.saturating_sub(1),
                    KeyCode::Char('g') => *index = 0,
                    KeyCode::Char('G') => *index = last,
                    KeyCode::Enter | KeyCode::Char('l') => {
                        let i = *index;
                        self.take_shell_choice(i);
                    }
                    // The list numbers itself, so the numbers are keys:
                    // `2` starts the second entry without moving there.
                    KeyCode::Char(c @ '1'..='9') => {
                        let i = c as usize - '1' as usize;
                        if i <= last {
                            self.take_shell_choice(i);
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
                    _ => {}
                }
            }
            Mode::Messages { index } => {
                let last = self.cfg.messages.len().saturating_sub(1);
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => *index = (*index + 1).min(last),
                    KeyCode::Char('k') | KeyCode::Up => *index = index.saturating_sub(1),
                    KeyCode::Enter | KeyCode::Char('l') => {
                        let i = *index;
                        self.mode = Mode::Normal;
                        self.send_predefined(i);
                    }
                    // The list numbers itself, so the numbers are keys:
                    // `2` sends the second entry without moving there.
                    KeyCode::Char(c @ '1'..='9') => {
                        let i = c as usize - '1' as usize;
                        if i < self.cfg.messages.len() {
                            self.mode = Mode::Normal;
                            self.send_predefined(i);
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
                    _ => {}
                }
            }
            Mode::Normal => self.on_key_normal(key),
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) {
        if self.visual.is_some() {
            return self.on_key_visual(key);
        }
        match key.code {
            KeyCode::Char('v') => self.start_visual(),
            // Innermost first: Esc leaves the parked preview before it
            // leaves the app. `q` stays the unconditional quit.
            KeyCode::Esc if self.preview_offset() > 0 => self.preview_live(),
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('h') | KeyCode::Left => self.collapse_or_up(),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => self.expand_or_attach(),
            KeyCode::Char('n') => self.open_new_worktree(),
            KeyCode::Char('a') => self.open_add_shell(),
            KeyCode::Char('p') => self.open_messages(),
            KeyCode::Char('t') => self.open_repo_todo(),
            KeyCode::Char('T') => self.open_todo(crate::todo::Pad::Global),
            KeyCode::Char('d') => match self.sel {
                Sel::Shell(..) => self.confirm_delete_shell(),
                // `d` stays "close the thing this row stands for": on a
                // repository row that is its inline shell, and only while
                // it has one. With none it falls through to the worktree
                // path, which says what to select instead.
                Sel::Repository(ri) if self.repo_shell_pane(ri).is_some() => {
                    self.confirm_delete_repo_shell(ri)
                }
                _ => self.confirm_delete_worktree(),
            },
            KeyCode::Char('r') => {
                self.refresh();
            }
            KeyCode::Char('R') => self.reset_layout(),
            KeyCode::Char('?') => self.open_help(),
            _ => {}
        }
    }

    /// Open the grouped help overlay (the palette) at its top. Also used
    /// by the statusbar help entry.
    pub(crate) fn open_help(&mut self) {
        self.help_scroll = 0;
        self.mode = Mode::Help;
    }

    /// Open the predefined-message modal on the selected shell. With
    /// nothing to show — no shell selected, or no configured messages —
    /// say so instead of opening an empty box.
    fn open_messages(&mut self) {
        if self.selected_shell().is_none() {
            self.notify("Select a shell to send a predefined message");
            return;
        }
        if self.cfg.messages.is_empty() {
            self.notify("No predefined messages configured");
            return;
        }
        self.mode = Mode::Messages { index: 0 };
    }

    /// Resolve a message template against the selected shell's context.
    /// Every call site — the modal that lists the presets and the send
    /// that pastes one — goes through here, so what the list shows is
    /// exactly what the agent receives. Reads state and config only (the
    /// configured base branch, not git), which is what makes it safe on the
    /// render path. Unknown tokens are left as written, and so is
    /// everything when no shell is selected: there is no context to
    /// resolve against, and blanks would be worse than the template.
    pub(crate) fn expand_message(&self, text: &str) -> String {
        let Some((ri, wi, _)) = self.selected_shell() else {
            return text.to_string();
        };
        let Some(repo) = self.state.repositories.get(ri) else {
            return text.to_string();
        };
        let Some(wt) = repo.worktrees.get(wi) else {
            return text.to_string();
        };
        let text = match self.base_branch_of(&repo.path) {
            Some(b) => text.replace("<base_branch>", &b),
            None => text.to_string(),
        };
        text.replace("<branch>", &wt.branch)
            .replace("<worktree_path>", &wt.path.display().to_string())
            .replace("<worktree>", &wt.name)
            .replace("<repository>", &repo.name)
    }

    /// Send the configured message at `index` to the selected shell, pasted
    /// so the agent cannot tell it from typed input.
    fn send_predefined(&mut self, index: usize) {
        let Some(text) = self
            .cfg
            .messages
            .get(index)
            .map(|m| self.expand_message(&m.text))
        else {
            return;
        };
        let Some((ri, wi, si)) = self.selected_shell() else {
            self.notify("Select a shell to send a predefined message");
            return;
        };
        if let Err(e) = self.send_text(ri, wi, si, &text) {
            self.fail(&e);
        }
    }

    /// The cursor of the open predefined-message modal (0 when it is not
    /// open — callers only ask while it is).
    fn messages_index(&self) -> usize {
        match self.mode {
            Mode::Messages { index } => index,
            _ => 0,
        }
    }

    /// Move the modal cursor to `n`, clamped to the configured list.
    fn messages_to(&mut self, n: usize) {
        let last = self.cfg.messages.len().saturating_sub(1);
        if let Mode::Messages { index } = &mut self.mode {
            *index = n.min(last);
        }
    }

    /// Close the startup diagnostics overlay.
    fn close_diagnostics(&mut self) {
        self.mode = Mode::Normal;
    }

    /// Open the startup diagnostics overlay when there is anything to show
    /// (once per start — dismissed with Esc/q/Enter).
    fn open_startup_diagnostics(&mut self) {
        if self.cfg.diagnostics.is_empty() {
            return;
        }
        self.diag_scroll = 0;
        self.mode = Mode::Diagnostics;
    }

    // ------------------------------------------------------ theme picker

    /// Open the live theme picker on the current theme. Built-ins are
    /// listed before user themes (theming rule: the first rows are the
    /// same in every repository).
    fn open_theme_picker(&mut self) {
        let names = match crate::theme::available_names() {
            Ok(n) if !n.is_empty() => n,
            _ => crate::theme::builtin_names(),
        };
        self.picker_original = self.theme.name.clone();
        let index = names
            .iter()
            .position(|n| *n == self.theme.name)
            .unwrap_or(0);
        self.theme_names = names;
        self.mode = Mode::ThemePicker { index };
    }

    /// Preview the theme under the picker cursor (live preview on move —
    /// the screen repaints with the new palette immediately).
    fn preview_picker_theme(&mut self) {
        let Mode::ThemePicker { index } = &self.mode else {
            return;
        };
        let Some(name) = self.theme_names.get(*index) else {
            return;
        };
        let truecolor = crate::theme::supports_truecolor();
        match crate::theme::load(name, truecolor) {
            Ok(t) => self.theme = t,
            Err(e) => {
                log::warn!("theme preview '{}' failed: {e}", name);
                self.notify_err(format!("theme '{}': {e}", name));
            }
        }
    }

    /// The picker cursor index, when the mode is a theme picker (0 otherwise).
    fn picker_index(&self) -> usize {
        match &self.mode {
            Mode::ThemePicker { index } => *index,
            _ => 0,
        }
    }

    /// Move the picker cursor to `n` (clamped) and preview.
    fn picker_to(&mut self, n: usize) {
        let Mode::ThemePicker { index } = &mut self.mode else {
            return;
        };
        *index = n.min(self.theme_names.len().saturating_sub(1));
        self.preview_picker_theme();
    }

    /// Keep the previewed theme and close, writing it to the user config.
    fn keep_picked_theme(&mut self) {
        let Mode::ThemePicker { index } = &self.mode else {
            return;
        };
        let name = self
            .theme_names
            .get(*index)
            .cloned()
            .unwrap_or_else(|| self.picker_original.clone());
        self.theme.name = name.clone();
        self.cfg.theme = name.clone();
        // A layer above the user file still wins at the next start.
        let shadowed_by = self
            .cfg
            .sources
            .iter()
            .find(|(k, _)| k == "theme")
            .map(|(_, layer)| *layer)
            .filter(|l| ![crate::config::LAYER_DEFAULTS, crate::config::LAYER_USER].contains(l));
        match crate::config::save_theme(&name) {
            Err(e) => {
                log::warn!("save theme '{name}': {e}");
                self.notify_err(format!("theme '{name}' (not saved: {e})"));
            }
            Ok(()) => match shadowed_by {
                Some(layer) => self.notify(format!(
                    "theme: {name} (saved, but the {layer} config overrides it)"
                )),
                None => self.notify(format!("theme: {name}")),
            },
        }
        self.mode = Mode::Normal;
    }

    /// Close without keeping: restore the theme the picker opened with.
    fn cancel_theme_picker(&mut self) {
        self.restore_original_theme();
        self.mode = Mode::Normal;
    }

    /// Reload the theme the picker opened with (called on Esc and on a
    /// click outside the picker).
    fn restore_original_theme(&mut self) {
        let name = std::mem::take(&mut self.picker_original);
        if name.is_empty() {
            return;
        }
        let truecolor = crate::theme::supports_truecolor();
        match crate::theme::load(&name, truecolor) {
            Ok(t) => self.theme = t,
            Err(e) => log::warn!("restore theme '{}' failed: {e}", name),
        }
    }

    // -------------------------------------------------- preview scrolling

    /// The preview panel's inner height: the frame minus the statusbar and
    /// the panel's two borders — the arithmetic the tree's page motions use.
    fn preview_rows(&self) -> usize {
        (self.viewport.1.saturating_sub(3)).max(1) as usize
    }

    /// Is the preview panel on screen at all? Below the split tier it is
    /// the first thing the layout drops, and with nothing to scroll the
    /// wheel goes back to the tree rather than becoming dead.
    fn preview_on_screen(&self) -> bool {
        matches!(
            crate::ui::tier(self),
            crate::ui::Tier::Wide | crate::ui::Tier::Split
        )
    }

    /// Is the parked view still the selected shell's?
    fn preview_parked_here(&self) -> bool {
        self.preview_pane.is_some() && self.preview_pane == self.selected_shell_pane()
    }

    /// How far above the live tail the preview actually draws. A parked
    /// view belongs to the pane it was taken from, so moving the tree
    /// selection reads as live without every caller having to say so — and
    /// a panel the layout has dropped cannot be parked at all, or `Esc`
    /// would refuse to quit with nothing on screen to explain why.
    pub fn preview_offset(&self) -> usize {
        if self.preview_on_screen() && self.preview_parked_here() {
            self.preview_scroll
        } else {
            0
        }
    }

    /// The text the preview draws: the deep buffer while it is parked, the
    /// poll's capture while it is live.
    pub fn preview_source(&self) -> Option<&str> {
        let pid = self.selected_shell_pane()?;
        if self.preview_parked_here()
            && let Some(buf) = &self.preview_buf
        {
            return Some(buf);
        }
        self.captures.get(&pid).map(String::as_str)
    }

    /// Move the preview `delta` lines back through its scrollback
    /// (positive = older, toward the top). Leaving the live tail fetches a
    /// deep capture once — the poll keeps only enough lines to classify a
    /// status, which is barely more than a screenful.
    pub fn preview_scroll_by(&mut self, delta: i32) {
        // A parked view left behind on another shell is already reported as
        // live; drop it before moving so the two agree.
        if !self.preview_parked_here() {
            self.preview_live();
        }
        if delta > 0 && self.preview_buf.is_none() {
            self.preview_pane = self.selected_shell_pane();
            self.load_preview_buf();
        }
        let max = self
            .preview_source()
            .map(|s| crate::ui::preview_scroll_max(s, self.preview_rows()))
            .unwrap_or(0);
        let next = (self.preview_scroll as i64 + delta as i64).clamp(0, max as i64) as usize;
        self.preview_scroll = next;
        if next == 0 {
            self.preview_buf = None;
        }
    }

    /// Back to the live tail, dropping the deep buffer.
    pub fn preview_live(&mut self) {
        self.preview_scroll = 0;
        self.preview_buf = None;
        self.preview_pane = None;
    }

    fn load_preview_buf(&mut self) {
        let Some(pid) = self.selected_shell_pane() else {
            return;
        };
        let Some(tmux) = &self.tmux else {
            return;
        };
        match tmux.capture_pane_full(&pid) {
            Ok(t) => self.preview_buf = Some(t),
            // No deep buffer is not a failure: the poll's shallow capture is
            // still scrollable, just shorter.
            Err(e) => log::debug!("capture-pane (full) {pid}: {e}"),
        }
    }

    // ---------------------------------------------------------- mouse

    pub fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let dbl = self.is_double_click(m.column, m.row);
                self.mouse_click(m.column, m.row, dbl);
            }
            // The wheel belongs to the preview: it walks the selected
            // pane's scrollback and never moves the tree selection, so
            // reading back through an agent's output cannot lose your
            // place in the tree. An open overlay owns the viewport for as
            // long as it is up, and below the split tier — where the
            // preview is not drawn — the wheel falls back to the tree
            // rather than becoming dead.
            MouseEventKind::ScrollDown => {
                if matches!(self.mode, Mode::Help) {
                    self.help_scroll_to(self.help_scroll + 3);
                } else if matches!(self.mode, Mode::Diagnostics) {
                    self.diag_scroll_to(self.diag_scroll + 3);
                } else if matches!(self.mode, Mode::Messages { .. }) {
                    self.messages_to(self.messages_index().saturating_add(3));
                } else if matches!(self.mode, Mode::AddShell { .. }) {
                    self.add_shell_to(self.add_shell_index().saturating_add(3));
                } else if matches!(self.mode, Mode::ThemePicker { .. }) {
                    self.picker_to(self.picker_index().saturating_add(3));
                } else if matches!(self.mode, Mode::Todo { .. }) {
                    self.todo_scroll_by(3);
                } else if self.preview_on_screen() {
                    self.preview_scroll_by(-3);
                } else {
                    self.move_sel(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if matches!(self.mode, Mode::Help) {
                    self.help_scroll_to(self.help_scroll.saturating_sub(3));
                } else if matches!(self.mode, Mode::Diagnostics) {
                    self.diag_scroll_to(self.diag_scroll.saturating_sub(3));
                } else if matches!(self.mode, Mode::Messages { .. }) {
                    self.messages_to(self.messages_index().saturating_sub(3));
                } else if matches!(self.mode, Mode::AddShell { .. }) {
                    self.add_shell_to(self.add_shell_index().saturating_sub(3));
                } else if matches!(self.mode, Mode::ThemePicker { .. }) {
                    self.picker_to(self.picker_index().saturating_sub(3));
                } else if matches!(self.mode, Mode::Todo { .. }) {
                    self.todo_scroll_by(-3);
                } else if self.preview_on_screen() {
                    self.preview_scroll_by(3);
                } else {
                    self.move_sel(-3);
                }
            }
            _ => {}
        }
    }

    /// Two left-clicks at the same spot within 400 ms count as a
    /// double-click (crossterm here reports no click count).
    fn is_double_click(&mut self, x: u16, y: u16) -> bool {
        const DOUBLE_CLICK_MS: u64 = 400;
        let now = Instant::now();
        let dbl = self
            .last_click
            .map(|(t, px, py)| {
                py == y
                    && px.abs_diff(x) <= 1
                    && now.duration_since(t).as_millis() as u64 <= DOUBLE_CLICK_MS
            })
            .unwrap_or(false);
        self.last_click = Some((now, x, y));
        dbl
    }

    fn mouse_click(&mut self, x: u16, y: u16, dbl: bool) {
        // The todo pad is a panel, not an overlay, so it is answered before
        // the overlay rules below (which would read its absent `popup_area`
        // as "clicked outside" and drop the mode without saving). The
        // statusbar keeps working; a click on the pad leaves the caret
        // alone — it is placed by the keyboard, like every other cursor
        // here — and a click anywhere else is the way back to the tree.
        if matches!(self.mode, Mode::Todo { .. }) {
            // The statusbar wraps, so its rows are found the same way the
            // normal-mode click below finds them: by the row each entry
            // landed on, never by "the last line of the frame".
            let bar_top = self.bar_hits.iter().map(|(_, _, by, _)| *by).min();
            if let Some(top) = bar_top
                && y >= top
            {
                if let Some((_, _, _, action)) = self
                    .bar_hits
                    .iter()
                    .find(|(x0, x1, by, _)| *by == y && *x0 <= x && x < *x1)
                {
                    self.run_bar_action(*action);
                }
                return;
            }
            // Inside the pad's own rows the click is a no-op (the caret
            // is placed by the keyboard, like every other cursor here);
            // anywhere else — the tree above it included — is the way back.
            let in_column = crate::ui::tree_width_for(self).is_some_and(|w| x < w);
            let in_pad =
                crate::ui::todo_panel_rows(self).is_some_and(|(top, bot)| y >= top && y < bot);
            if !(in_column && in_pad) {
                self.close_todo();
            }
            return;
        }
        // Overlays: a click outside dismisses them (accessibility rule);
        // a click inside keeps keyboard as the way in. The theme picker is
        // the exception: clicking a theme row selects + previews it, and a
        // double-click keeps it (Enter).
        if matches!(self.mode, Mode::AddShell { .. }) {
            let inside = self
                .popup_area
                .map(|r| x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height)
                .unwrap_or(false);
            if !inside {
                self.mode = Mode::Normal; // click outside = Esc
                return;
            }
            // Rows start one line below the popup border; a click on one
            // selects it, a double-click starts it (Enter).
            if let Some(r) = self.popup_area {
                let row = y.saturating_sub(r.y + 1) as usize;
                if row < self.add_shell_choices().len() {
                    self.add_shell_to(row);
                    if dbl {
                        self.take_shell_choice(row);
                    }
                }
            }
            return;
        }
        if let Mode::Messages { index } = &mut self.mode {
            let inside = self
                .popup_area
                .map(|r| x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height)
                .unwrap_or(false);
            if !inside {
                self.mode = Mode::Normal; // click outside = Esc
                return;
            }
            // Rows start one line below the popup border; a click on one
            // selects it, a double-click sends it (Enter).
            if let Some(r) = self.popup_area {
                let row = y.saturating_sub(r.y + 1) as usize;
                if row < self.cfg.messages.len() {
                    *index = row;
                    if dbl {
                        self.mode = Mode::Normal;
                        self.send_predefined(row);
                    }
                }
            }
            return;
        }
        if let Mode::ThemePicker { index } = &mut self.mode {
            let inside = self
                .popup_area
                .map(|r| x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height)
                .unwrap_or(false);
            if !inside {
                // click outside = Esc (restores the original theme)
                self.mode = Mode::Normal;
                self.restore_original_theme();
                return;
            }
            let Some(rect) = self.popup_area else {
                return;
            };
            let row = y.saturating_sub(rect.y + 1) as usize;
            if row < self.theme_names.len() {
                *index = row;
                self.preview_picker_theme();
                if dbl {
                    self.keep_picked_theme();
                }
            }
            return;
        }
        if !matches!(self.mode, Mode::Normal) {
            let inside = self
                .popup_area
                .map(|r| x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height)
                .unwrap_or(false);
            if !inside {
                if matches!(self.mode, Mode::Diagnostics) {
                    self.close_diagnostics();
                } else {
                    self.mode = Mode::Normal;
                }
            }
            return;
        }
        // Statusbar rows: click a shortcut entry to run its action.
        let bar_top = self.bar_hits.iter().map(|(_, _, by, _)| *by).min();
        if let Some(top) = bar_top
            && y >= top
        {
            if let Some((_, _, _, action)) = self
                .bar_hits
                .iter()
                .find(|(x0, x1, by, _)| *by == y && *x0 <= x && x < *x1)
            {
                self.run_bar_action(*action);
            }
            return;
        }
        // Tree rows: click selects, double-click opens (Enter).
        let Some(tree_w) = crate::ui::tree_width_for(self) else {
            return; // terminal too small for the tree
        };
        if x >= tree_w {
            return; // detail/preview panels are not selectable
        }
        // A parked pad shares the column: clicking it is how you get the
        // keyboard into it, the same way clicking a tree row selects it.
        if let Some((top, bot)) = crate::ui::todo_panel_rows(self)
            && y >= top
            && y < bot
            && let Some((pad, _)) = self.parked_todo()
        {
            self.open_todo(pad);
            return;
        }
        let row = y.saturating_sub(1) as usize; // below the top border
        let nodes = self.visible_nodes();
        if let Some(sel) = nodes.get(row) {
            self.sel = *sel;
            if dbl {
                self.expand_or_attach();
            }
        }
    }

    fn run_bar_action(&mut self, action: BarAction) {
        match action {
            BarAction::Help => self.open_help(),
            BarAction::PreviewLive => self.preview_live(),
            BarAction::NewWorktree => self.open_new_worktree(),
            BarAction::AddShell => self.open_add_shell(),
            BarAction::PredefinedSend => self.open_messages(),
            BarAction::DeleteWorktree => self.confirm_delete_worktree(),
            BarAction::DeleteShell => self.confirm_delete_shell(),
            BarAction::VisualStart => self.start_visual(),
            BarAction::VisualCancel => self.visual = None,
            BarAction::DeleteMarked => self.confirm_delete_marked(),
            BarAction::CloseRepoShell => {
                if let Sel::Repository(ri) = self.sel {
                    self.confirm_delete_repo_shell(ri);
                }
            }
            BarAction::RepoShell => {
                if let Sel::Repository(ri) = self.sel {
                    self.enter_repo_shell(ri);
                }
            }
            BarAction::Dock => {
                if let Some((ri, wi, si)) = self.selected_shell() {
                    self.dock_shell(ri, wi, si);
                }
            }
            BarAction::Todo => self.open_repo_todo(),
            BarAction::TodoBack => self.close_todo(),
            BarAction::TodoEdit => self.edit_todo_externally(),
        }
    }

    fn move_sel_page(&mut self, half_pages: i32) {
        // Page size = the tree's visible rows (statusbar, the todo panel
        // and the tree's own borders subtracted). ctrl-u/d are half pages,
        // ctrl-f/b full pages.
        let rows = crate::ui::tree_rows(self) as i32;
        let step = (rows / 2).max(1) * half_pages;
        self.move_sel(step);
    }

    /// The accessibility "move the viewport" motions. While the help
    /// overlay (palette) is open they scroll the help instead of the tree:
    /// the overlay owns the viewport for as long as it is up. While the
    /// theme picker is open they move the picker cursor. Same code path
    /// as the tree motions, so the two spellings cannot drift.
    fn move_view(&mut self, half_pages: i32) {
        if matches!(self.mode, Mode::Messages { .. }) {
            let step = 5 * half_pages;
            self.messages_to(self.messages_index().saturating_add_signed(step as isize));
        } else if matches!(self.mode, Mode::AddShell { .. }) {
            let step = 5 * half_pages;
            self.add_shell_to(self.add_shell_index().saturating_add_signed(step as isize));
        } else if let Mode::ThemePicker { index } = &mut self.mode {
            let step = 5 * half_pages;
            *index = (index.saturating_add_signed(step as isize))
                .min(self.theme_names.len().saturating_sub(1));
            self.preview_picker_theme();
        } else if matches!(self.mode, Mode::Help) {
            let total = crate::ui::help_line_count(self);
            let visible = crate::ui::overlay_body_height(self.popup_area);
            let max_scroll = total.saturating_sub(visible);
            let step = (visible / 2).max(1) * half_pages.max(0) as usize;
            if half_pages < 0 {
                self.help_scroll = self.help_scroll.saturating_sub(step.max(1));
            } else {
                self.help_scroll = (self.help_scroll + step).min(max_scroll);
            }
        } else if matches!(self.mode, Mode::Diagnostics) {
            let total = crate::ui::diag_line_count(self);
            let visible = crate::ui::overlay_body_height(self.popup_area);
            let max_scroll = total.saturating_sub(visible);
            let step = (visible / 2).max(1) * half_pages.max(0) as usize;
            if half_pages < 0 {
                self.diag_scroll = self.diag_scroll.saturating_sub(step.max(1));
            } else {
                self.diag_scroll = (self.diag_scroll + step).min(max_scroll);
            }
        } else {
            self.move_sel_page(half_pages);
        }
    }

    fn move_view_first(&mut self) {
        if matches!(self.mode, Mode::Messages { .. }) {
            self.messages_to(0);
        } else if matches!(self.mode, Mode::AddShell { .. }) {
            self.add_shell_to(0);
        } else if let Mode::ThemePicker { index } = &mut self.mode {
            *index = 0;
            self.preview_picker_theme();
        } else if matches!(self.mode, Mode::Help) {
            self.help_scroll = 0;
        } else if matches!(self.mode, Mode::Diagnostics) {
            self.diag_scroll = 0;
        } else {
            self.move_sel_first();
        }
    }

    fn move_view_last(&mut self) {
        if matches!(self.mode, Mode::Messages { .. }) {
            self.messages_to(usize::MAX);
        } else if matches!(self.mode, Mode::AddShell { .. }) {
            self.add_shell_to(usize::MAX);
        } else if let Mode::ThemePicker { index } = &mut self.mode {
            *index = self.theme_names.len().saturating_sub(1);
            self.preview_picker_theme();
        } else if matches!(self.mode, Mode::Help) {
            let total = crate::ui::help_line_count(self);
            let visible = crate::ui::overlay_body_height(self.popup_area);
            self.help_scroll = total.saturating_sub(visible);
        } else if matches!(self.mode, Mode::Diagnostics) {
            let total = crate::ui::diag_line_count(self);
            let visible = crate::ui::overlay_body_height(self.popup_area);
            self.diag_scroll = total.saturating_sub(visible);
        } else {
            self.move_sel_last();
        }
    }

    /// Step the help overlay by whole lines (`j`/`k` and arrows) or to the
    /// given index (`g`/`G`), clamped to the scrollable range.
    fn help_scroll_to(&mut self, n: usize) {
        let total = crate::ui::help_line_count(self);
        let visible = crate::ui::overlay_body_height(self.popup_area);
        self.help_scroll = n.min(total.saturating_sub(visible));
    }

    /// Same motion target as [`Self::help_scroll_to`], for the startup
    /// diagnostics overlay (its content height comes from
    /// `ui::diag_line_count`, never drifts from the renderer).
    fn diag_scroll_to(&mut self, n: usize) {
        let total = crate::ui::diag_line_count(self);
        let visible = crate::ui::overlay_body_height(self.popup_area);
        self.diag_scroll = n.min(total.saturating_sub(visible));
    }

    fn move_sel_first(&mut self) {
        self.move_sel(i32::MIN / 2);
    }

    fn move_sel_last(&mut self) {
        self.move_sel(i32::MAX / 2);
    }

    fn refresh(&mut self) {
        if let Some(tmux) = &self.tmux {
            let mut ok = true;
            if let Err(e) = self.state.reconcile(tmux) {
                log::warn!("reconcile: {e}");
                ok = false;
            }
            self.state.prune(&self.worktrees_loaded);
            if ok {
                let width = crate::dock::dash_width(self.strip_size(tmux).0);
                crate::dock::ensure_holders(tmux, width);
                self.build_tree_flags();
                self.clamp_sel();
                self.poll();
                if let Err(e) = self.save_state() {
                    log::warn!("save: {e}");
                }
                self.notify("Refreshed");
            } else {
                self.notify_err("refresh had errors (see log)");
            }
        } else {
            self.notify_err("no tmux session");
        }
    }

    fn fail(&mut self, e: &Error) {
        self.notify_err(format!("error: {e}"));
        log::error!("action failed: {e}");
    }

    fn submit_new_worktree(&mut self, text: &str, agent: &str) {
        let res = match self.sel {
            Sel::Repository(ri) | Sel::Worktree(ri, _) => self.create_worktree(ri, text, agent),
            _ => Err(Error::State("select a repository first".into())),
        };
        if let Err(e) = res {
            self.fail(&e);
        }
    }

    fn run_confirm(&mut self, action: ConfirmAction, force: bool) {
        let res = match action {
            ConfirmAction::DeleteWorktree {
                repository,
                worktree,
            } => self.delete_worktree(repository, worktree, force),
            ConfirmAction::DeleteShell {
                repository,
                worktree,
                shell,
            } => self.delete_shell(repository, worktree, shell),
            ConfirmAction::CloseRepoShell { repository } => self.delete_repo_shell(repository),
            ConfirmAction::DeleteMarked(marks) => self.delete_marked(marks, force),
        };
        if let Err(e) = res {
            self.fail(&e);
        }
    }
}

fn now_str() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // civil date (Howard Hinnant) — reused from logging
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let yy = if m <= 2 { y + 1 } else { y };
    format!("{yy:04}-{m:02}-{d:02}")
}

/// The main loop: draw, poll on interval, handle keys.
/// The scope a launch in `cwd` will show: `[discovery].scan_root`, or `cwd`
/// itself when that is unset. Relative roots resolve against `cwd`.
///
/// Absolute and normalised, and a free function rather than a method,
/// because the self-wrap needs it BEFORE an `App` exists: it is the key a
/// second launch looks a running dashboard up by (`@lw_scope`), so the two
/// must be computed exactly the same way.
pub fn scope_for(cfg: &Config, cwd: &std::path::Path) -> PathBuf {
    let root = cfg.discovery.scan_root.trim();
    let p = if root.is_empty() {
        cwd.to_path_buf()
    } else {
        let p = PathBuf::from(root);
        if p.is_absolute() { p } else { cwd.join(p) }
    };
    crate::util::normalize_path(&p)
}

pub fn run(cfg: Config, theme: Theme, tmux: Tmux) -> Result<()> {
    let mut app = App::new(cfg, theme, Some(tmux));
    app.interactive_boot = true;
    if app.cfg.defaults.agent_hooks {
        match crate::hooks::Instrumentation::prepare() {
            Ok(i) => {
                log::info!("agent hooks enabled: {}", i.settings().display());
                app.instrumentation = Some(i);
            }
            // Never fatal: without the file we simply fall back to the
            // regex heuristics, which is exactly pre-M17 behaviour.
            Err(e) => log::warn!("agent hooks disabled ({e})"),
        }
    } else {
        log::info!("agent hooks disabled by config");
    }
    if let Err(e) = app.startup() {
        // before the terminal is taken over, stderr is safe
        eprintln!("wingmux: startup failed: {e}");
        return Err(e);
    }
    app.poll();
    // Startup diagnostics overlay: configuration problems dropped at load
    // are shown ONCE, at startup (the guidelines demand a visible report,
    // not a log line nobody reads). If a startup confirm (scan prompt) was
    // armed by boot_discovery, it resumes after the overlay is dismissed.
    app.open_startup_diagnostics();
    let mut terminal = term_init();
    let res = event_loop(&mut terminal, &mut app);
    term_restore();
    res
}

/// Take over the terminal: ratatui's `init` enables raw mode and the
/// alternate screen but NOT mouse reporting, so nothing would ever send us
/// a `MouseEvent`. Turn it on here — and off again in [`term_restore`] and
/// on the panic path, or the shell we hand the terminal back to keeps
/// receiving mouse escape sequences.
fn term_init() -> ratatui::DefaultTerminal {
    let terminal = ratatui::init();
    // After `ratatui::init`, so this hook wraps ratatui's: mouse reporting
    // is turned off before ratatui leaves the alternate screen. Once only —
    // the attach handoff re-inits the terminal and must not stack hooks.
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            disable_mouse();
            prev(info);
        }));
    });
    if let Err(e) = execute!(std::io::stdout(), EnableMouseCapture) {
        // Not fatal: keyboard-only operation still works.
        log::error!("cannot enable mouse reporting: {e}");
    }
    terminal
}

/// Hand the terminal back: mouse reporting off first, then ratatui's own
/// restore (raw mode off, leave the alternate screen).
fn term_restore() {
    disable_mouse();
    ratatui::restore();
}

fn disable_mouse() {
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        // Dock-on-look, before the frame that shows its result. The
        // argument is the event queue's own answer to "are more keys
        // already waiting": held autorepeat keeps it true, so a burst of
        // `j` produces one dock at the end instead of one per row, with
        // no timer and so no window in which the panel shows the row the
        // cursor has already left.
        app.auto_dock(event::poll(Duration::ZERO)?);
        terminal.draw(|f| crate::ui::draw(f, app))?;
        if app.quit {
            // ctrl-c quits from any state, the open pad included: flush it
            // before the loop ends or the last note typed dies with the
            // process.
            app.save_todo();
            // Killing a half-done `git worktree remove` would leave a
            // partial checkout behind.
            let n = app.remover.pending();
            if n > 0 {
                app.notify(format!("finishing {n} worktree deletions"));
                terminal.draw(|f| crate::ui::draw(f, app))?;
                app.wait_removals();
            }
            // Furniture of ours, in what may well be the user's own window.
            app.drop_slot();
            break;
        }
        // Background worktree discovery: one repository per iteration, so a
        // scope with dozens of repos fills the tree in over the next few
        // frames instead of freezing before the first one.
        let loading = app.drain_pending_worktrees();
        app.drain_removals();
        if app.last_poll.elapsed() >= Duration::from_millis(app.cfg.poll_interval_ms) {
            app.poll();
        }
        // wait for input, at most until the next poll is due — but never
        // while there is still a repository to load: that work owes a redraw.
        let since = app.last_poll.elapsed();
        let until_poll = Duration::from_millis(app.cfg.poll_interval_ms).saturating_sub(since);
        let timeout = if loading {
            Duration::ZERO
        } else {
            until_poll.min(Duration::from_millis(100))
        };
        if event::poll(timeout)? {
            dispatch_event(app, event::read()?);
        }
    }
    Ok(())
}

/// Route one terminal event. Split out of the loop so tests drive exactly
/// the path the loop takes: reading first and matching after is what keeps
/// a mouse event from being consumed by the key branch and lost.
fn dispatch_event(app: &mut App, ev: Event) {
    match ev {
        Event::Key(key) => app.on_key(key),
        Event::Mouse(m) => app.on_mouse(m),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Repository, Shell, Worktree};
    use crate::theme;
    use crate::tmux::testutil::{tmux_inside, tmux_nowhere, with_tmux_server};
    use std::path::Path;

    fn app_with(tmux: Tmux) -> App {
        let mut app = App::new(
            Config::default(),
            theme::load("ansi", false).unwrap(),
            Some(tmux),
        );
        // Tests seed repositories under /tmp without going through
        // boot_discovery — make the whole filesystem the scope.
        app.scope_root = PathBuf::from("/");
        // Fixture repos are `git init -b main`.
        app.cfg.worktrees.base_branch = "main".into();
        app
    }

    /// Both agents are instrumented, each the way its CLI takes hooks —
    /// and a command that is neither is handed to the pane untouched.
    #[test]
    fn spawn_command_is_instrumented_when_hooks_are_on() {
        let mut app = App::new(Config::default(), theme::load("ansi", false).unwrap(), None);
        app.instrumentation = Some(crate::hooks::Instrumentation::for_test(
            "/s/wingmux-activity-hooks.json",
            "/bin/wingmux",
            None,
        ));
        assert_eq!(
            app.agent_command_for_spawn("claude"),
            "claude --settings '/s/wingmux-activity-hooks.json'"
        );
        let codex = app.agent_command_for_spawn("codex exec");
        assert!(
            codex.starts_with("codex -c 'hooks.SessionStart=") && codex.ends_with(" exec"),
            "the overrides go after the binary, before its own arguments: {codex}"
        );
        assert_eq!(app.agent_command_for_spawn("bash"), "bash");
    }

    #[test]
    fn spawn_command_is_untouched_when_hooks_are_off() {
        let mut app = App::new(Config::default(), theme::load("ansi", false).unwrap(), None);
        app.instrumentation = None;
        assert_eq!(app.agent_command_for_spawn("claude"), "claude");
        assert_eq!(app.agent_command_for_spawn("codex"), "codex");
    }

    /// Seed state with one repository whose agent session and first shell
    /// window really exist on the test tmux server.
    fn seed_repository_with_shell(tmux: &Tmux, app: &mut App) -> (String, String) {
        let w = tmux
            .new_session(
                "wingmux-session-p",
                "lw:wt/0",
                Path::new("/tmp"),
                "bash",
                &crate::dock::ShellId::for_session("wingmux-session-p", "wt", 0),
                crate::tmux::testutil::HOLDER_W,
            )
            .unwrap();
        // A detached session's windows default to 80x24, which leaves the
        // agent pane a 25-column sliver beside a full-width holder. The
        // real app never runs that way — `resize_agent_windows` puts every
        // agent window at the dashboard window's size — so the fixture
        // does the same, and no test comes to depend on where 80 columns
        // happen to wrap.
        tmux.resize_window(&w.window_id, 200, 50).unwrap();
        app.state.repositories.push(Repository {
            name: "p".into(),
            path: PathBuf::from("/tmp/p"),
            missing: false,
            shell: None,
            worktrees: vec![Worktree {
                name: "wt".into(),
                branch: "wt".into(),
                path: PathBuf::from("/tmp/p-wt"),
                created_at: "2026-08-16".into(),
                base_commit: None,
                agent_command: None,
                shells: vec![Shell {
                    command: None,
                    index: 0,
                    window_name: "lw:wt/0".into(),
                    window_id: w.window_id.clone(),
                    pane_id: w.pane_id.clone(),
                }],
            }],
        });
        (w.window_id, w.pane_id)
    }

    /// `agent_hooks = false` must be a real kill switch: even a pane that
    /// still carries a live `@lw_agent` overlay (written by an earlier
    /// spawn, or by a second Wingmux instance) must classify purely
    /// from `capture-pane`, exactly as README/STATE promise. The overlay
    /// says `blocked`, which a foreground `cat` (not a shell, so the
    /// login-shell guard alone would not save this test) would otherwise
    /// win outright.
    #[test]
    fn agent_hooks_off_ignores_a_live_overlay_during_poll() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            app.cfg.defaults.agent_hooks = false;
            let (_wid, pid) = seed_repository_with_shell(&tmux, &mut app);
            tmux.send_keys(&pid, "cat", true).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            tmux.set_pane_option(&pid, crate::hooks::OVERLAY_KEY, "blocked:9999999999")
                .unwrap();

            app.poll();

            assert_eq!(
                app.statuses.get(&pid),
                Some(&Status::Idle),
                "agent_hooks = false must classify from the ladder, ignoring the overlay"
            );
        });
    }

    /// M18 Task 5 — the dashboard's window announces the scope it shows, so
    /// a second launch in the same folder finds this dashboard instead of
    /// racing a rival one beside it.
    #[test]
    fn startup_announces_the_scope_on_the_dashboard_window() {
        crate::util::testutil::with_fresh_xdg(|| {
            with_tmux_server(|_sock, tmux| {
                let mut app = app_with(tmux);
                app.scope_root = PathBuf::from("/work/scope-a");
                app.startup().unwrap();
                let t = app.tmux.as_ref().unwrap();
                let read = t
                    .run_raw(&[
                        "display-message",
                        "-p",
                        "-t",
                        "@0",
                        &format!("#{{{}}}", crate::dock::SCOPE_KEY),
                    ])
                    .unwrap();
                assert_eq!(read.trim(), "/work/scope-a");

                let found = t
                    .find_scope_window(Path::new("/work/scope-a"))
                    .unwrap()
                    .expect("the announced scope is findable");
                assert_eq!(found.window_id, "@0");
                assert_eq!(found.session_name, "main");
                assert_eq!(found.pane_id, "%0");
                assert!(
                    t.find_scope_window(Path::new("/work/scope-b"))
                        .unwrap()
                        .is_none(),
                    "another scope must not match this window"
                );
            });
        });
    }

    // ---- M8.3 — repository discovery ----

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    /// Init a repo with one commit (worktree add needs a commit-ish).
    fn repo_with_commit(root: &Path, name: &str) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(&p).unwrap();
        git(&p, &["init", "-b", "main"]);
        git(&p, &["config", "user.name", "lw-test"]);
        git(&p, &["config", "user.email", "lw-test@example.com"]);
        std::fs::write(p.join("readme.md"), "# t\n").unwrap();
        git(&p, &["add", "."]);
        git(&p, &["commit", "-m", "init"]);
        p
    }

    /// `discover_and_import` finds repos under a root, imports them as
    /// repositories, and is idempotent. No cache — the scan is live.
    #[test]
    fn discover_and_import_adds_new_repositories_and_is_idempotent() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            repo_with_commit(r, "repo-a");
            repo_with_commit(r, "repo-b");
            std::fs::create_dir_all(r.join("node_modules/dep/.git")).unwrap();

            let mut app = app_with(tmux_nowhere());
            let n = app.discover_and_import(r).unwrap();
            assert_eq!(n, 2);
            assert_eq!(app.state.repositories.len(), 2);
            let names: Vec<&str> = app
                .state
                .repositories
                .iter()
                .map(|p| p.name.as_str())
                .collect();
            assert!(names.contains(&"repo-a") && names.contains(&"repo-b"));

            // idempotent: re-scan adds nothing
            let n2 = app.discover_and_import(r).unwrap();
            assert_eq!(n2, 0);
            assert_eq!(app.state.repositories.len(), 2);
        });
    }

    /// Boot from a non-repo directory live-scans it (no prompt, no cache)
    /// and imports what is inside — the scope is exactly what is there.
    #[test]
    fn boot_discovery_live_scans_folder_scope() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            repo_with_commit(r, "repo-a");
            repo_with_commit(r, "repo-b");
            std::fs::create_dir_all(r.join("node_modules/dep/.git")).unwrap();

            let mut app = app_with(tmux_nowhere());
            app.boot_discovery_from(r).unwrap();
            // No prompt — the scan just ran and imported the scope.
            assert!(matches!(app.mode, Mode::Normal));
            assert_eq!(app.state.repositories.len(), 2);
            assert_eq!(app.scope_root, crate::util::normalize_path(r));
            // Two repositories, both unfolded; the first
            // repository's worktrees load at boot and the second's is queued.
            assert!(app.visible_repositories().len() == 2);
            assert_eq!(app.pending_worktrees.len(), 1, "the second is queued");
            while app.drain_pending_worktrees() {}
            assert!(
                app.worktrees_loaded.iter().all(|f| *f),
                "draining the queue loads every unfolded repository"
            );
        });
    }

    /// Expanding a repository lazily loads its worktrees (once per run).
    #[test]
    fn expand_loads_worktrees_lazily_and_only_once() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            // 6 repos → only the first loads at boot, the rest are queued
            let mut repos = Vec::new();
            for i in 0..6 {
                repos.push(repo_with_commit(r, &format!("repo-{i}")));
            }

            let tmux = tmux_nowhere();
            let mut app = app_with(tmux.clone());
            app.boot_discovery_from(r).unwrap();
            assert_eq!(
                app.worktrees_loaded.iter().filter(|f| **f).count(),
                1,
                "boot loads the first repository only; the rest wait in the queue"
            );
            // Expanding a repository the drain has not reached yet loads ITS
            // worktrees there and then, once.
            let ri = *app.pending_worktrees.back().unwrap();
            app.repo_expanded[ri] = false; // ensure collapsed
            app.sel = Sel::Repository(ri);
            app.expand_or_attach();
            assert!(app.worktrees_loaded[ri], "expand loads worktrees");
            assert_eq!(
                app.worktrees_loaded.iter().filter(|f| **f).count(),
                2,
                "only the boot repository and the expanded one"
            );
            // collapsing + expanding again does not re-run the listing
            app.repo_expanded[ri] = false;
            app.expand_or_attach();
            assert_eq!(
                app.worktrees_loaded.iter().filter(|f| **f).count(),
                2,
                "loaded exactly once"
            );
            // …and the background drain skips what expanding already did
            while app.drain_pending_worktrees() {}
            assert!(app.worktrees_loaded.iter().all(|f| *f));
            crate::tmux::testutil::kill_socket(&tmux);
        });
    }

    /// Everything unfolds by default up to the cap: >30 repositories stay
    /// collapsed (the tree would be unreadable and every row costs a
    /// `git worktree list`), ≤30 expand.
    #[test]
    fn many_repositories_collapse_by_default_few_expand() {
        let mut app = app_with(tmux_nowhere());
        app.scope_root = PathBuf::from("/");
        for i in 0..31 {
            app.state.repositories.push(Repository {
                name: format!("p{i}"),
                path: PathBuf::from(format!("/tmp/p{i}")),
                missing: false,
                shell: None,
                worktrees: vec![],
            });
        }
        app.build_tree_flags();
        // 31 repositories → collapsed
        assert!(app.repo_expanded.iter().all(|e| !*e));

        let mut few = app_with(tmux_nowhere());
        few.scope_root = PathBuf::from("/");
        for i in 0..30 {
            few.state.repositories.push(Repository {
                name: format!("p{i}"),
                path: PathBuf::from(format!("/tmp/p{i}")),
                missing: false,
                shell: None,
                worktrees: vec![],
            });
        }
        few.build_tree_flags();
        // 30 repositories → expanded
        assert!(few.repo_expanded.iter().all(|e| *e));
    }

    /// Worktrees unfold by default too — a folded worktree hides the very
    /// shells the dashboard exists to supervise, and expanding one costs
    /// nothing (its shells are already in state).
    #[test]
    fn worktrees_are_expanded_by_default() {
        let mut app = app_with(tmux_nowhere());
        app.scope_root = PathBuf::from("/");
        for i in 0..2 {
            app.state.repositories.push(Repository {
                name: format!("p{i}"),
                path: PathBuf::from(format!("/tmp/p{i}")),
                missing: false,
                shell: None,
                worktrees: vec![Worktree {
                    name: "f".into(),
                    branch: "f".into(),
                    path: PathBuf::from(format!("/tmp/p{i}/f")),
                    created_at: String::new(),
                    base_commit: None,
                    agent_command: None,
                    shells: vec![],
                }],
            });
        }
        app.build_tree_flags();
        assert!(
            app.wt_expanded.iter().flatten().all(|e| *e),
            "every worktree starts unfolded"
        );
    }

    /// Boot does not block on one `git worktree list` per repository: only
    /// the first visible repository loads synchronously (it is what the first
    /// frame draws), the rest are queued and drained by the event loop.
    #[test]
    fn boot_queues_worktree_loading_instead_of_blocking() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            for i in 0..6 {
                repo_with_commit(r, &format!("repo-{i}"));
            }

            let mut app = app_with(tmux_nowhere());
            app.boot_discovery_from(r).unwrap();
            assert!(
                app.repo_expanded.iter().all(|e| *e),
                "6 repositories are under the cap → all unfolded"
            );
            assert_eq!(
                app.worktrees_loaded.iter().filter(|f| **f).count(),
                1,
                "boot loads only the first repository's worktrees"
            );
            assert_eq!(app.pending_worktrees.len(), 5, "the rest are queued");

            // The event loop drains one repository per iteration.
            let mut steps = 0;
            while app.drain_pending_worktrees() {
                steps += 1;
                assert!(steps <= 10, "draining must terminate");
            }
            assert_eq!(steps, 5, "one repository per drain step");
            assert!(
                app.worktrees_loaded.iter().all(|f| *f),
                "every repository is loaded once the queue is drained"
            );
        });
    }

    /// Worktrees discovered by a lazy worktree load are unfolded like every
    /// other row: `ensure_worktrees` pushes onto `worktrees`, so the
    /// parallel `wt_expanded` must grow with it (it used to stay short,
    /// and a missing entry reads as folded).
    #[test]
    fn lazily_loaded_worktrees_are_expanded() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            let repo = repo_with_commit(r, "repo-a");
            git(&repo, &["worktree", "add", "-b", "wt", "../repo-a-wt"]);
            repo_with_commit(r, "repo-b");

            let mut app = app_with(tmux_nowhere());
            app.boot_discovery_from(r).unwrap();
            while app.drain_pending_worktrees() {}

            let ri = app
                .visible_repositories()
                .into_iter()
                .find(|&i| app.state.repositories[i].name == "repo-a")
                .expect("repo-a imported");
            assert_eq!(
                app.state.repositories[ri].worktrees.len(),
                1,
                "the linked worktree became a worktree"
            );
            assert_eq!(
                app.wt_expanded[ri].len(),
                1,
                "wt_expanded stays parallel to worktrees"
            );
            assert!(app.wt_expanded[ri][0], "the new worktree is unfolded");
        });
    }

    /// Out-of-scope repositories (other scopes' leftovers in state) stay saved
    /// but never get rows.
    #[test]
    fn scope_filters_repositories_from_other_scopes() {
        let mut app = app_with(tmux_nowhere());
        app.scope_root = PathBuf::from("/work/scope-a");
        app.state.repositories.push(Repository {
            name: "in-a".into(),
            path: PathBuf::from("/work/scope-a/repo-x"),
            missing: false,
            shell: None,
            worktrees: vec![],
        });
        app.state.repositories.push(Repository {
            name: "in-b".into(),
            path: PathBuf::from("/work/scope-b/repo-y"),
            missing: false,
            shell: None,
            worktrees: vec![],
        });
        let vis = app.visible_repositories();
        assert_eq!(vis, vec![0], "only scope-a's repository is visible");
        assert_eq!(app.state.repositories.len(), 2, "state keeps both");
        // Missing repositories inside the scope still show.
        app.state.repositories[0].missing = true;
        assert_eq!(app.visible_repositories(), vec![0]);
    }

    /// Boot from inside a git repo auto-imports the MAIN repo (even from a
    /// linked worktree) and adds its worktrees as worktrees with 0 shells.
    #[test]
    fn boot_discovery_auto_imports_cwd_repo_and_its_worktrees() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            let repo = repo_with_commit(r, "repo");
            let wt = r.join("wt-worktree-x");
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "worktree-x",
                    wt.to_str().unwrap(),
                    "main",
                ],
            );

            let mut app = app_with(tmux_nowhere());
            app.boot_discovery_from(&repo).unwrap();
            assert_eq!(app.state.repositories.len(), 1);
            let imported = &app.state.repositories[0];
            assert_eq!(imported.path, repo);
            assert_eq!(
                imported.worktrees.len(),
                1,
                "the linked worktree is picked up"
            );
            assert_eq!(imported.worktrees[0].name, "worktree-x");
            assert_eq!(imported.worktrees[0].branch, "worktree-x");
            assert!(imported.worktrees[0].shells.is_empty(), "0 shells");

            // booting again is idempotent (no duplicate worktrees)
            app.boot_discovery_from(&repo).unwrap();
            assert_eq!(app.state.repositories[0].worktrees.len(), 1);

            // booting from INSIDE the worktree resolves to the main repo
            let mut app2 = app_with(tmux_nowhere());
            app2.boot_discovery_from(&wt).unwrap();
            assert_eq!(app2.state.repositories.len(), 1);
            assert_eq!(app2.state.repositories[0].path, repo);
            assert_eq!(app2.state.repositories[0].worktrees.len(), 1);
        });
    }

    /// `i` (import a repo by path) and `I` (rescan the discovery root) are
    /// gone: repositories come from boot discovery and `[[repositories]]` in the
    /// config. Both keys must be inert — no popup, no repository.
    #[test]
    fn the_import_and_scan_keys_are_gone() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            repo_with_commit(r, "repo-a");

            let mut app = app_with(tmux_nowhere());
            // A root that HAS a repo to find, so a surviving scan would show.
            app.cfg.discovery.scan_root = r.to_string_lossy().into_owned();
            for k in ['i', 'I'] {
                app.on_key(KeyEvent::new(KeyCode::Char(k), KeyModifiers::NONE));
                assert!(matches!(app.mode, Mode::Normal), "`{k}` opened a mode");
                assert!(
                    app.state.repositories.is_empty(),
                    "`{k}` imported a repository"
                );
            }
        });
    }

    /// Creating a worktree or adding a shell on a missing repository is
    /// rejected with a clear error instead of failing deep in git/tmux.
    #[test]
    fn actions_reject_missing_repositories() {
        let mut app = app_with(tmux_nowhere());
        app.state.repositories.push(Repository {
            name: "gone".into(),
            path: PathBuf::from("/definitely/not/here"),
            missing: true,
            shell: None,
            worktrees: vec![Worktree {
                name: "wt".into(),
                branch: "wt".into(),
                path: PathBuf::from("/definitely/not/here/wt"),
                created_at: "".into(),
                base_commit: None,
                agent_command: None,
                shells: vec![],
            }],
        });
        let err = app.create_worktree(0, "new-wt", "").unwrap_err();
        assert!(err.to_string().contains("missing"), "err: {err}");
        let err = app
            .add_shell(0, 0, &ShellSpawn::Agent(String::new()))
            .unwrap_err();
        assert!(err.to_string().contains("missing"), "err: {err}");
    }

    /// Creating a worktree reveals it: the repository unfolds (it may have
    /// been collapsed, or empty — an empty repository draws the same glyph
    /// whatever its flag says, so the user cannot tell), and the new row —
    /// which the selection now points at — is one of the tree's visible
    /// nodes. Without the unfold `clamp_sel` would drop the selection back
    /// to the first row and the worktree just created would not be drawn.
    #[test]
    fn creating_a_worktree_opens_a_collapsed_repository() {
        with_tmux_server(|sock, _tmux| {
            crate::util::testutil::with_fresh_xdg(|| {
                let root = tempfile::TempDir::new().unwrap();
                let path = repo_with_commit(root.path(), "p");
                let mut app = app_with(tmux_inside(sock));
                app.state.repositories.push(Repository {
                    name: "p".into(),
                    path,
                    missing: false,
                    shell: None,
                    worktrees: vec![],
                });
                app.cfg.repositories.push(crate::config::RepositoryConfig {
                    path: app.state.repositories[0]
                        .path
                        .to_string_lossy()
                        .into_owned(),
                    agent_command: Some("true".into()),
                    ..Default::default()
                });
                app.build_tree_flags();
                app.repo_expanded[0] = false; // collapsed, as >30 repos or `h` leaves it

                app.create_worktree(0, "feature-x", "").unwrap();

                assert!(
                    app.repo_expanded[0],
                    "creating a worktree must open the repository holding it"
                );
                assert_eq!(
                    app.sel,
                    Sel::Worktree(0, 0),
                    "the new worktree must keep the selection, not lose it to clamp_sel"
                );
                assert!(
                    app.visible_nodes().contains(&Sel::Worktree(0, 0)),
                    "the new worktree must be a drawn row"
                );
                // …and it unfolds like every other row: a second shell shows
                // up instead of hiding under a folded worktree.
                app.add_shell(0, 0, &ShellSpawn::Agent("true".into()))
                    .unwrap();
                assert!(
                    app.visible_nodes().contains(&Sel::Shell(0, 0, 1)),
                    "a created worktree unfolds like a discovered one"
                );
            });
        });
    }

    /// The agent picked in the popup sticks to the worktree: its first
    /// shell runs it, the add-shell picker offers it first, and the poll
    /// classifies that pane with that agent's patterns — a codex worktree
    /// under a claude repository stays a codex worktree.
    #[test]
    fn a_worktree_keeps_the_agent_it_was_created_with() {
        with_tmux_server(|sock, _tmux| {
            crate::util::testutil::with_fresh_xdg(|| {
                let root = tempfile::TempDir::new().unwrap();
                let path = repo_with_commit(root.path(), "p");
                let mut app = app_with(tmux_inside(sock));
                app.state.repositories.push(Repository {
                    name: "p".into(),
                    path,
                    missing: false,
                    shell: None,
                    worktrees: vec![],
                });
                app.build_tree_flags();

                app.create_worktree(0, "feature-x", "codex").unwrap();

                assert_eq!(
                    app.state.repositories[0].worktrees[0]
                        .agent_command
                        .as_deref(),
                    Some("codex"),
                    "the preset name is stored resolved to its command"
                );
                app.sel = Sel::Worktree(0, 0);
                assert_eq!(
                    app.add_shell_choices()[0].spawn,
                    ShellSpawn::Agent("codex".into()),
                    "one more of what is already running here is codex"
                );
                assert_eq!(
                    app.cfg.agent_at(&app.state.repositories[0].path).command,
                    "claude",
                    "the repository is untouched by one worktree's choice"
                );
                assert_eq!(
                    app.state.repositories[0].worktrees[0].shells[0]
                        .command
                        .as_deref(),
                    Some("codex"),
                    "the shell row is named by what it runs, not by the repository's agent"
                );
            });
        });
    }

    /// An empty agent field means "whatever the repository runs" — the
    /// worktree records no agent of its own and keeps following the repo.
    #[test]
    fn an_empty_agent_field_leaves_the_worktree_on_the_repository_agent() {
        with_tmux_server(|sock, _tmux| {
            crate::util::testutil::with_fresh_xdg(|| {
                let root = tempfile::TempDir::new().unwrap();
                let path = repo_with_commit(root.path(), "p");
                let mut app = app_with(tmux_inside(sock));
                app.state.repositories.push(Repository {
                    name: "p".into(),
                    path,
                    missing: false,
                    shell: None,
                    worktrees: vec![],
                });
                app.cfg.repositories.push(crate::config::RepositoryConfig {
                    path: app.state.repositories[0]
                        .path
                        .to_string_lossy()
                        .into_owned(),
                    agent_command: Some("true".into()),
                    ..Default::default()
                });
                app.build_tree_flags();

                app.create_worktree(0, "feature-x", "  ").unwrap();

                assert!(
                    app.state.repositories[0].worktrees[0]
                        .agent_command
                        .is_none()
                );
                app.sel = Sel::Worktree(0, 0);
                assert_eq!(
                    app.add_shell_choices()[0].spawn,
                    ShellSpawn::Agent("true".into())
                );
            });
        });
    }

    /// With no base branch configured the popup names the branch the repo
    /// has checked out; a configured one wins over the checkout.
    #[test]
    fn new_worktree_popup_names_the_base_branch() {
        crate::util::testutil::with_test_lock(|| {
            let root = tempfile::TempDir::new().unwrap();
            let path = repo_with_commit(root.path(), "repo");
            git(&path, &["checkout", "-b", "some-other-branch"]);

            let mut app = app_with(tmux_nowhere());
            app.cfg.worktrees.base_branch = String::new();
            app.state.repositories.push(Repository {
                name: "repo".into(),
                path: path.clone(),
                missing: false,
                shell: None,
                worktrees: vec![],
            });
            app.build_tree_flags();
            app.sel = Sel::Repository(0);
            app.open_new_worktree();

            assert!(matches!(app.mode, Mode::Input { .. }));
            let hint = app.new_worktree_base().expect("base hint");
            assert_eq!(hint.branch, "some-other-branch");
            assert!(!hint.missing);

            app.cfg.worktrees.base_branch = "main".into();
            app.open_new_worktree();
            let hint = app.new_worktree_base().expect("base hint");
            assert_eq!(hint.branch, "main");
            assert!(!hint.missing);
        });
    }

    /// A worktree whose working tree is clean but whose branch is ahead is
    /// still carrying work: the confirm must not call it clean. The plain
    /// `y` stays on offer — `git worktree remove` removes it happily and
    /// the branch keeps the commits — so the caveat is the whole warning.
    #[test]
    fn the_delete_confirm_names_unmerged_commits_on_a_clean_tree() {
        crate::util::testutil::with_test_lock(|| {
            let root = tempfile::TempDir::new().unwrap();
            let path = repo_with_commit(root.path(), "repo");
            let fork = Git.rev_parse(&path, "main").unwrap();
            let wt = root.path().join("wt-ahead");
            Git.worktree_add(&path, &wt, "ahead", "main").unwrap();
            std::fs::write(wt.join("f.txt"), "work").unwrap();
            git(&wt, &["add", "."]);
            git(&wt, &["commit", "-m", "work"]);

            let mut app = app_with(tmux_nowhere());
            app.state.repositories.push(Repository {
                name: "repo".into(),
                path,
                missing: false,
                shell: None,
                worktrees: vec![Worktree {
                    name: "ahead".into(),
                    branch: "ahead".into(),
                    path: wt,
                    created_at: String::new(),
                    base_commit: Some(fork),
                    agent_command: None,
                    shells: vec![],
                }],
            });
            app.build_tree_flags();
            app.sel = Sel::Worktree(0, 0);
            app.confirm_delete_worktree();

            let Mode::Confirm { confirm } = &app.mode else {
                panic!("expected a confirm, got {:?}", app.mode);
            };
            let lines: Vec<&str> = confirm.body.lines().collect();
            assert_eq!(
                lines.len(),
                5,
                "question, path and state, one blank line apart: {lines:?}"
            );
            assert!(lines[0].starts_with("Delete worktree 'ahead'?"));
            assert!(lines[1].is_empty() && lines[3].is_empty(), "{lines:?}");
            assert!(
                lines[2].ends_with("ahead"),
                "the path stands alone: {lines:?}"
            );
            assert_eq!(
                lines[4], "Worktree is NOT clean",
                "committed work must not read as clean"
            );
            assert!(
                !confirm.body.contains("unmerged commits"),
                "the reason belongs to the warning line alone: {}",
                confirm.body
            );
            assert!(
                confirm
                    .warning
                    .as_deref()
                    .is_some_and(|w| w.contains("Unmerged commits")),
                "the caveat names the reason and is drawn as a warning"
            );
            assert!(
                confirm.yes_label.is_some(),
                "the plain answer still succeeds — only a dirty tree needs --force"
            );
            assert!(confirm.force_label.is_none(), "nothing to force-discard");
        });
    }

    /// The tree's commit-state cache is filled from real git state, keyed
    /// by worktree path, and a repository whose path is gone is skipped
    /// rather than filling the cache with failures.
    #[test]
    fn worktree_state_cache_reports_new_ahead_and_merged() {
        crate::util::testutil::with_test_lock(|| {
            let root = tempfile::TempDir::new().unwrap();
            let path = repo_with_commit(root.path(), "repo");
            let fork = Git.rev_parse(&path, "main").unwrap();

            let mut wts = Vec::new();
            for name in ["fresh", "ahead", "merged"] {
                let wt = root.path().join(format!("wt-{name}"));
                Git.worktree_add(&path, &wt, name, "main").unwrap();
                if name != "fresh" {
                    std::fs::write(wt.join("f.txt"), name).unwrap();
                    git(&wt, &["add", "."]);
                    git(&wt, &["commit", "-m", name]);
                }
                wts.push(Worktree {
                    name: name.into(),
                    branch: name.into(),
                    path: wt,
                    created_at: String::new(),
                    base_commit: Some(fork.clone()),
                    agent_command: None,
                    shells: vec![],
                });
            }
            git(&path, &["merge", "--no-ff", "-m", "merge", "merged"]);

            let mut app = app_with(tmux_nowhere());
            app.state.repositories.push(Repository {
                name: "repo".into(),
                path: path.clone(),
                missing: false,
                shell: None,
                worktrees: wts,
            });
            // a missing repository must not be probed at all
            app.state.repositories.push(Repository {
                name: "gone".into(),
                path: PathBuf::from("/definitely/not/here"),
                missing: true,
                shell: None,
                worktrees: vec![Worktree {
                    name: "wt".into(),
                    branch: "wt".into(),
                    path: PathBuf::from("/definitely/not/here/wt"),
                    created_at: String::new(),
                    base_commit: None,
                    agent_command: None,
                    shells: vec![],
                }],
            });
            app.build_tree_flags();
            app.poll_worktree_state(true);

            assert_eq!(app.worktree_state_cached(0, 0), WorktreeState::New);
            assert_eq!(app.worktree_state_cached(0, 1), WorktreeState::Ahead);
            assert_eq!(app.worktree_state_cached(0, 2), WorktreeState::Merged);
            assert_eq!(app.wt_state.len(), 3, "the missing repo is skipped");
            // an unprobed worktree reads as New rather than panicking
            assert_eq!(app.worktree_state_cached(1, 0), WorktreeState::New);
        });
    }

    /// A configured base branch that is not in the repo is named as missing,
    /// and creation refuses instead of cutting from the checkout.
    #[test]
    fn new_worktree_popup_flags_a_missing_base_branch() {
        crate::util::testutil::with_test_lock(|| {
            let root = tempfile::TempDir::new().unwrap();
            let path = repo_with_commit(root.path(), "repo");

            let mut app = app_with(tmux_nowhere());
            app.cfg.repositories.push(crate::config::RepositoryConfig {
                path: path.to_string_lossy().into_owned(),
                base_branch: Some("master".into()),
                ..Default::default()
            });
            app.state.repositories.push(Repository {
                name: "repo".into(),
                path: path.clone(),
                missing: false,
                shell: None,
                worktrees: vec![],
            });
            app.build_tree_flags();
            app.sel = Sel::Repository(0);
            app.open_new_worktree();

            let hint = app.new_worktree_base().expect("base hint");
            assert_eq!(hint.branch, "master");
            assert!(hint.missing);
            let err = app.create_worktree(0, "x", "claude").unwrap_err();
            assert!(err.to_string().contains("master"), "{err}");
        });
    }

    /// A missing repo has no base to name: the popup still opens (creation
    /// is what rejects it) but shows no branch line.
    #[test]
    fn new_worktree_popup_has_no_base_hint_for_a_missing_repository() {
        crate::util::testutil::with_test_lock(|| {
            let mut app = app_with(tmux_nowhere());
            app.state.repositories.push(Repository {
                name: "gone".into(),
                path: PathBuf::from("/definitely/not/here"),
                missing: true,
                shell: None,
                worktrees: vec![],
            });
            app.build_tree_flags();
            app.sel = Sel::Repository(0);
            app.open_new_worktree();

            assert!(app.new_worktree_base().is_none());
        });
    }

    // ---- M8.4a — accessibility keys ----

    /// Seed a headless app with `n` repositories (no tmux needed: navigation
    /// is pure state).
    fn app_with_repositories(n: usize) -> App {
        let mut app = app_with(tmux_nowhere());
        for i in 0..n {
            app.state.repositories.push(Repository {
                name: format!("p{i}"),
                path: PathBuf::from(format!("/tmp/p{i}")),
                missing: false,
                shell: None,
                worktrees: vec![],
            });
        }
        app.build_tree_flags();
        app.clamp_sel();
        app
    }

    /// A repository with one worktree holding one shell, for action tests.
    fn app_with_shell() -> App {
        let mut app = app_with(tmux_nowhere());
        app.state.repositories.push(Repository {
            name: "p".into(),
            path: PathBuf::from("/tmp/p"),
            missing: false,
            shell: None,
            worktrees: vec![Worktree {
                name: "wt".into(),
                branch: "wt".into(),
                path: PathBuf::from("/tmp/p/.wingmux-worktrees/p/wt"),
                created_at: String::new(),
                base_commit: None,
                agent_command: None,
                shells: vec![Shell {
                    command: None,
                    index: 0,
                    window_name: "lw:wt/0".into(),
                    window_id: "@1".into(),
                    pane_id: "%1".into(),
                }],
            }],
        });
        app.build_tree_flags();
        // One shell → the worktree row is folded onto it, which is the only
        // way this shell is reachable in the tree.
        app.sel = Sel::Worktree(0, 0);
        app.clamp_sel();
        app
    }

    fn key(code: KeyCode, ctrl: bool) -> KeyEvent {
        KeyEvent::new(
            code,
            if ctrl {
                KeyModifiers::CONTROL
            } else {
                KeyModifiers::NONE
            },
        )
    }

    // ------------------------------------------------------------- todo

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c), false));
        }
    }

    /// `t` puts the pad in the tree's own panel — no overlay — and `Esc`
    /// hands the panel back to the tree, keeping what was typed.
    #[test]
    fn t_opens_the_pad_in_place_and_esc_goes_back() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_shell();
            app.viewport = (120, 40);
            app.on_key(key(KeyCode::Char('t'), false));
            assert!(matches!(app.mode, Mode::Todo { .. }), "`t` opens the pad");
            assert!(
                app.popup_area.is_none(),
                "the pad is a panel, not an overlay"
            );
            typed(&mut app, "rebase wt2");
            app.on_key(key(KeyCode::Esc, false));
            assert!(
                matches!(app.mode, Mode::Normal),
                "esc goes back to the tree"
            );
            assert_eq!(
                app.repo_todos[&PathBuf::from("/tmp/p")].text(),
                "rebase wt2"
            );
        });
    }

    fn p_pad() -> crate::todo::Pad {
        crate::todo::Pad::Repo(PathBuf::from("/tmp/p"))
    }

    /// Run `f` with `$EDITOR` set (or unset), putting back whatever the
    /// environment had.
    fn with_editor<R>(value: Option<&str>, f: impl FnOnce() -> R) -> R {
        crate::util::testutil::with_test_lock(|| {
            let before = [
                ("EDITOR", std::env::var("EDITOR")),
                ("VISUAL", std::env::var("VISUAL")),
            ];
            unsafe {
                std::env::remove_var("VISUAL");
                match value {
                    Some(v) => std::env::set_var("EDITOR", v),
                    None => std::env::remove_var("EDITOR"),
                }
            }
            let r = f();
            for (k, v) in before {
                unsafe {
                    match v {
                        Ok(v) => std::env::set_var(k, v),
                        Err(_) => std::env::remove_var(k),
                    }
                }
            }
            r
        })
    }

    /// `ctrl-e` writes the pad out before handing it over: `$EDITOR` opens
    /// what was on screen, never the file as it was one keystroke ago.
    ///
    /// There is no tmux server here, so the spawn itself stops at a toast —
    /// the save in front of it is what this pins, and it happens either way.
    #[test]
    fn ctrl_e_saves_the_pad_before_the_editor_gets_it() {
        crate::util::testutil::with_fresh_xdg(|| {
            with_editor(Some("true"), || {
                let mut app = app_with_shell();
                app.viewport = (120, 40);
                app.on_key(key(KeyCode::Char('t'), false));
                typed(&mut app, "bump the tmux floor");
                app.on_key(key(KeyCode::Char('e'), true));
                assert_eq!(crate::todo::load(&p_pad()), "bump the tmux floor");
            });
        });
    }

    /// With no `$EDITOR` the key says so and the pad stays open with the
    /// text in it — no dead key, and nothing guessed.
    #[test]
    fn ctrl_e_without_an_editor_says_so_and_keeps_the_pad() {
        crate::util::testutil::with_fresh_xdg(|| {
            with_editor(None, || {
                let mut app = app_with_shell();
                app.viewport = (120, 40);
                app.on_key(key(KeyCode::Char('t'), false));
                typed(&mut app, "note");
                app.on_key(key(KeyCode::Char('e'), true));
                let Mode::Todo { buf, .. } = &app.mode else {
                    panic!("the pad stays open");
                };
                assert_eq!(buf.text(), "note");
                let toast = app.message.as_ref().expect("the key says why");
                assert!(toast.text.contains("$EDITOR"), "{:?}", toast.text);
                assert!(app.editor.is_none(), "nothing was spawned");
            });
        });
    }

    /// The editor's window leaving the pane map is what ends the round
    /// trip: the pad is re-read whole, whatever the editor did to it.
    #[test]
    fn a_closed_editor_window_re_reads_the_pad() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_shell();
            app.viewport = (120, 40);
            app.on_key(key(KeyCode::Char('t'), false));
            typed(&mut app, "before");
            app.on_key(key(KeyCode::Esc, false));
            app.editor = Some(("@42".into(), p_pad()));
            crate::todo::save(&p_pad(), "rewritten in $EDITOR").unwrap();

            // No window on this (dead) server, so the editor has closed.
            app.reap_editor(&tmux_nowhere());
            assert_eq!(
                app.repo_todos[&PathBuf::from("/tmp/p")].text(),
                "rewritten in $EDITOR"
            );
            assert!(app.editor.is_none(), "the round trip is over");
        });
    }

    /// `T` opens the global pad, `t` the repository's — separate buffers,
    /// separate files.
    #[test]
    fn shift_t_opens_the_global_pad_apart_from_the_repository_one() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_shell();
            app.viewport = (120, 40);
            app.on_key(key(KeyCode::Char('T'), false));
            typed(&mut app, "global note");
            app.on_key(key(KeyCode::Esc, false));
            app.on_key(key(KeyCode::Char('t'), false));
            typed(&mut app, "repo note");
            app.on_key(key(KeyCode::Esc, false));
            assert_eq!(crate::todo::load(&crate::todo::Pad::Global), "global note");
            assert_eq!(crate::todo::load(&p_pad()), "repo note");
        });
    }

    /// A parked repository pad follows the selection: another repository
    /// shows its own pad, or none. A parked global pad stays put.
    #[test]
    fn the_parked_repository_pad_follows_the_selection() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_shell();
            app.viewport = (120, 41);
            let mut other = app.state.repositories[0].clone();
            other.name = "q".into();
            other.path = PathBuf::from("/tmp/q");
            other.worktrees.clear();
            app.state.repositories.push(other);
            app.build_tree_flags();
            app.load_repo_todos();

            app.on_key(key(KeyCode::Char('t'), false));
            typed(&mut app, "for p");
            app.on_key(key(KeyCode::Esc, false));
            assert!(crate::ui::todo_panel_rows(&app).is_some());

            app.sel = Sel::Repository(1);
            assert_eq!(crate::ui::todo_panel_rows(&app), None, "q has no notes");
            app.on_key(key(KeyCode::Char('T'), false));
            typed(&mut app, "global");
            app.on_key(key(KeyCode::Esc, false));
            app.sel = Sel::Worktree(0, 0);
            let (pad, buf) = app.parked_todo().unwrap();
            assert_eq!(pad, crate::todo::Pad::Global);
            assert_eq!(buf.text(), "global");
        });
    }

    /// Every printable key is text while the pad is open: the normal-mode
    /// bindings underneath (`q` quit, `d` delete, `j`/`k` move, `?` help)
    /// must not fire, or the pad could not hold a sentence.
    #[test]
    fn the_pad_owns_every_printable_key() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_shell();
            app.viewport = (120, 40);
            let before = app.sel;
            app.on_key(key(KeyCode::Char('t'), false));
            typed(&mut app, "djkq? gG");
            let Mode::Todo { buf, .. } = &app.mode else {
                panic!("still the pad");
            };
            assert_eq!(buf.text(), "djkq? gG");
            assert!(!app.quit, "`q` typed a letter, it did not quit");
            assert_eq!(app.sel, before, "`j`/`k` typed letters, they did not move");
        });
    }

    /// The global pad outlives the process: one plain file for the whole
    /// machine, so a dashboard started anywhere else finds the same notes.
    #[test]
    fn the_global_pad_survives_a_restart_and_is_not_per_scope() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_shell();
            app.viewport = (120, 40);
            app.on_key(key(KeyCode::Char('T'), false));
            typed(&mut app, "wait for the schema");
            app.on_key(key(KeyCode::Enter, false));
            typed(&mut app, "then rebase");
            app.on_key(key(KeyCode::Esc, false));
            assert_eq!(
                std::fs::read_to_string(crate::util::todo_path(&crate::todo::Pad::Global)).unwrap(),
                "wait for the schema\nthen rebase",
                "written as plain text, editable without the dashboard"
            );

            // A second dashboard, in another scope entirely.
            let mut next = app_with_shell();
            next.viewport = (120, 40);
            next.scope_root = PathBuf::from("/somewhere/else");
            next.load_todo();
            next.on_key(key(KeyCode::Char('T'), false));
            let Mode::Todo { buf, .. } = &next.mode else {
                panic!("the pad opens");
            };
            assert_eq!(buf.text(), "wait for the schema\nthen rebase");
        });
    }

    /// ctrl-c quits from any state, the open pad included — and the last
    /// thing typed must still reach the disk. `save_todo` is what the
    /// event loop calls on the way out.
    #[test]
    fn quitting_out_of_the_open_pad_still_writes_it() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_shell();
            app.viewport = (120, 40);
            app.on_key(key(KeyCode::Char('t'), false));
            typed(&mut app, "unsaved note");
            app.on_key(key(KeyCode::Char('c'), true));
            assert!(app.quit, "ctrl-c quits from the pad too");
            app.save_todo();
            assert_eq!(crate::todo::load(&p_pad()), "unsaved note");
        });
    }

    /// A theme switch is a chord that works over every state, and it
    /// replaces the panel — so it parks the pad first.
    #[test]
    fn a_theme_switch_parks_the_open_pad() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_shell();
            app.viewport = (120, 40);
            app.on_key(key(KeyCode::Char('t'), false));
            typed(&mut app, "keep me");
            app.on_key(key(KeyCode::Char('t'), true));
            assert!(matches!(app.mode, Mode::ThemePicker { .. }));
            assert_eq!(crate::todo::load(&p_pad()), "keep me");
        });
    }

    /// A click outside the pad is the way back to the tree, and it keeps
    /// the text. The pad has no `popup_area`, so without its own branch the
    /// overlay rule would have dropped the mode — and the note with it.
    #[test]
    fn a_click_outside_the_pad_keeps_what_is_in_it() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_shell();
            app.viewport = (120, 40);
            app.on_key(key(KeyCode::Char('t'), false));
            typed(&mut app, "still here");
            let tree_w = crate::ui::tree_width_for(&app).unwrap();
            app.mouse_click(tree_w + 5, 3, false);
            assert!(matches!(app.mode, Mode::Normal), "back to the tree");
            assert_eq!(crate::todo::load(&p_pad()), "still here");
        });
    }

    /// `Esc` hands the keyboard back to the tree but does not take the
    /// note off the screen: the panel stays, collapsed to the note's own
    /// height. An empty pad is the one that goes away — there is nothing
    /// left to stand as a reminder.
    #[test]
    fn esc_collapses_the_pad_rather_than_closing_it() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_shell();
            app.viewport = (120, 41);
            app.on_key(key(KeyCode::Char('t'), false));
            typed(&mut app, "rebase wt2");
            app.on_key(key(KeyCode::Esc, false));
            assert!(matches!(app.mode, Mode::Normal), "the tree has the keys");
            assert_eq!(
                crate::ui::todo_panel_rows(&app),
                Some((37, 40)),
                "one line of text, so a three-row panel at the column's foot"
            );

            // Emptied and left: nothing to remind anyone of, no panel.
            app.on_key(key(KeyCode::Char('t'), false));
            for _ in 0.."rebase wt2".len() {
                app.on_key(key(KeyCode::Backspace, false));
            }
            app.on_key(key(KeyCode::Esc, false));
            assert_eq!(crate::ui::todo_panel_rows(&app), None);
        });
    }

    /// Clicking the parked panel is how the keyboard gets into it — the
    /// same gesture that selects a tree row. A click on the tree while the
    /// pad is open is the way back out, and it keeps the text.
    #[test]
    fn clicks_move_the_keyboard_between_the_tree_and_the_parked_pad() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_shell();
            app.viewport = (120, 41);
            app.on_key(key(KeyCode::Char('t'), false));
            typed(&mut app, "later");
            app.on_key(key(KeyCode::Esc, false));
            let (top, _) = crate::ui::todo_panel_rows(&app).unwrap();

            app.mouse_click(3, top + 1, false);
            assert!(
                matches!(app.mode, Mode::Todo { .. }),
                "the pad has the keys"
            );

            // The tree is above the open pad now, so clicking it leaves.
            app.mouse_click(3, 2, false);
            assert!(matches!(app.mode, Mode::Normal), "back to the tree");
            assert_eq!(crate::todo::load(&p_pad()), "later");
        });
    }

    /// A pad edited outside the dashboard must not place the cursor
    /// past the line, or inside a multi-byte character — the next edit
    /// indexes with `col` and would panic on a split char.
    #[test]
    fn a_restored_cursor_is_clamped_to_a_char_boundary() {
        let buf = TextBuf::from_parked("héllo\nworld", 0, 2);
        assert_eq!(buf.cursor_row(), 0);
        assert_eq!(buf.cursor_col(), 1, "col 2 splits 'é' — back to its start");
        let buf = TextBuf::from_parked("hi", 9, 99);
        assert_eq!(buf.cursor_row(), 0, "row past the end clamps");
        assert_eq!(buf.cursor_col(), 2, "col past the end clamps");
        let buf = TextBuf::from_parked("a\nbb", 1, 2);
        assert_eq!((buf.cursor_row(), buf.cursor_col()), (1, 2));
        assert_eq!(buf.text(), "a\nbb", "the newlines round-trip");
    }

    /// Typing non-ASCII must not blow up the TUI. `col` is a BYTE offset
    /// (the popup slices `line[..col]` to place the cursor), so advancing
    /// it by one per char leaves it mid-character and the next edit hits a
    /// non-char-boundary panic.
    #[test]
    fn the_text_buffer_handles_non_ascii() {
        let mut buf = TextBuf::new();
        for c in "please — do".chars() {
            buf.insert_char(c);
        }
        assert_eq!(buf.text(), "please — do");
        buf.backspace();
        assert_eq!(buf.text(), "please — d");
        buf.move_home();
        buf.move_end();
        assert_eq!(buf.cursor_col(), "please — d".len());
    }

    /// Buffers are unbounded in practice but not in principle: the buffer
    /// refuses to grow past the cap rather than eating memory.
    #[test]
    fn the_text_buffer_stops_at_the_cap() {
        let mut buf = TextBuf::new();
        for _ in 0..TEXTBUF_MAX_BYTES {
            buf.insert_char('x');
        }
        assert_eq!(buf.text().len(), TEXTBUF_MAX_BYTES);
        buf.insert_char('y');
        buf.newline();
        assert_eq!(buf.text().len(), TEXTBUF_MAX_BYTES, "the cap holds");
        assert!(!buf.text().contains('y'), "the refused char is not stored");
        // deleting makes room again
        buf.backspace();
        buf.insert_char('z');
        assert!(buf.text().ends_with('z'), "the cap is not a one-way door");
    }

    /// The nominal case is one worktree, one agent: a worktree with a
    /// single shell is folded — it gets no child row (whatever its expand
    /// flag says) and the selection resolves to that shell. A second shell
    /// unfolds the subtree again.
    #[test]
    fn solo_worktree_folds_and_a_second_shell_unfolds_it() {
        let mut app = app_with_shell();
        assert_eq!(app.solo_shell(0, 0), Some(0));
        // even flagged expanded, a folded worktree emits no shell row
        app.wt_expanded[0][0] = true;
        let nodes = app.visible_nodes();
        assert!(
            !nodes.iter().any(|n| matches!(n, Sel::Shell(..))),
            "a one-shell worktree must not emit a child row: {nodes:?}"
        );
        // ...and the worktree row acts as that shell
        assert_eq!(app.sel, Sel::Worktree(0, 0));
        assert_eq!(app.selected_shell(), Some((0, 0, 0)));
        assert_eq!(app.selected_shell_pane().as_deref(), Some("%1"));

        // a second shell brings the subtree back
        app.state.repositories[0].worktrees[0].shells.push(Shell {
            command: None,
            index: 1,
            window_name: "lw:wt/1".into(),
            window_id: "@2".into(),
            pane_id: "%2".into(),
        });
        assert_eq!(app.solo_shell(0, 0), None);
        let nodes = app.visible_nodes();
        assert!(
            nodes.contains(&Sel::Shell(0, 0, 0)) && nodes.contains(&Sel::Shell(0, 0, 1)),
            "two shells must show both rows: {nodes:?}"
        );
        // the worktree row is no longer a shell — it expands again
        assert_eq!(app.selected_shell(), None);
    }

    /// `v` anchors on a shell, `j` extends over the rows below, `d` confirms
    /// closing exactly those shells, and `Esc` drops the selection.
    #[test]
    fn v_then_j_then_d_confirms_closing_the_marked_shells() {
        let mut app = app_with_shell();
        for i in 1..3 {
            app.state.repositories[0].worktrees[0].shells.push(Shell {
                command: None,
                index: i,
                window_name: format!("lw:wt/{i}"),
                window_id: format!("@{}", i + 1),
                pane_id: format!("%{}", i + 1),
            });
        }
        app.build_tree_flags();
        app.sel = Sel::Shell(0, 0, 0);

        app.on_key(key(KeyCode::Char('v'), false));
        app.on_key(key(KeyCode::Char('j'), false));
        assert_eq!(app.visual_marks().shells, vec![(0, 0, 0), (0, 0, 1)]);

        app.on_key(key(KeyCode::Char('d'), false));
        match &app.mode {
            Mode::Confirm { confirm } => assert!(
                matches!(&confirm.action, ConfirmAction::DeleteMarked(m) if m.shells.len() == 2),
                "{:?}",
                confirm.action
            ),
            m => panic!("expected a confirm, got {m:?}"),
        }
        assert!(app.visual.is_none());

        app.mode = Mode::Normal;
        app.on_key(key(KeyCode::Char('v'), false));
        app.on_key(key(KeyCode::Esc, false));
        assert!(app.visual.is_none() && !app.quit);
    }

    /// On worktree rows `d` deletes worktrees, as it does on one row; a
    /// shell of a worktree that goes anyway is not listed twice.
    #[test]
    fn v_over_worktree_rows_deletes_the_worktrees() {
        let mut app = app_with_shell();
        app.sel = Sel::Worktree(0, 0);
        app.on_key(key(KeyCode::Char('v'), false));
        let marks = app.visual_marks();
        assert_eq!(marks.worktrees, vec![(0, 0)]);
        assert!(marks.shells.is_empty());
        app.on_key(key(KeyCode::Char('d'), false));
        match &app.mode {
            Mode::Confirm { confirm } => assert!(
                matches!(&confirm.action, ConfirmAction::DeleteMarked(m) if m.worktrees == [(0, 0)]),
                "{:?}",
                confirm.action
            ),
            m => panic!("expected a confirm, got {m:?}"),
        }
    }

    /// Repository rows cannot anchor a selection.
    #[test]
    fn v_on_a_repository_row_starts_nothing() {
        let mut app = app_with_shell();
        app.sel = Sel::Repository(0);
        app.on_key(key(KeyCode::Char('v'), false));
        assert!(app.visual.is_none());
    }

    /// `d` is contextual: on a shell row it closes that shell, on a
    /// worktree row it deletes the worktree. Closing a shell must never
    /// offer to take the worktree (and its sibling shells) with it.
    #[test]
    fn d_on_a_shell_confirms_closing_only_that_shell() {
        let mut app = app_with_shell();
        app.state.repositories[0].worktrees[0].shells.push(Shell {
            command: None,
            index: 1,
            window_name: "lw:wt/1".into(),
            window_id: "@2".into(),
            pane_id: "%2".into(),
        });
        app.build_tree_flags();
        app.sel = Sel::Shell(0, 0, 1);

        app.on_key(key(KeyCode::Char('d'), false));
        match &app.mode {
            Mode::Confirm { confirm } => {
                assert!(
                    matches!(
                        confirm.action,
                        ConfirmAction::DeleteShell {
                            repository: 0,
                            worktree: 0,
                            shell: 1
                        }
                    ),
                    "d on a shell must confirm closing that shell: {:?}",
                    confirm.action
                );
                assert!(
                    confirm.body.contains("lw:wt/1"),
                    "the confirm names the shell: {:?}",
                    confirm.body
                );
            }
            m => panic!("expected a confirm, got {m:?}"),
        }

        // the worktree row keeps the worktree-level delete
        app.mode = Mode::Normal;
        app.sel = Sel::Worktree(0, 0);
        app.on_key(key(KeyCode::Char('d'), false));
        match &app.mode {
            Mode::Confirm { confirm } => assert!(
                matches!(confirm.action, ConfirmAction::DeleteWorktree { .. }),
                "d on a worktree still deletes the worktree: {:?}",
                confirm.action
            ),
            m => panic!("expected a confirm, got {m:?}"),
        }
    }

    /// A dirty worktree only removes with `--force`, so the plain answer is
    /// gone: `y`, `d` and `Enter` must all leave the popup standing rather
    /// than fire a delete that git will refuse.
    #[test]
    fn a_dirty_worktree_offers_no_plain_yes_and_enter_never_confirms() {
        let mut app = app_with_shell();
        app.mode = Mode::Confirm {
            confirm: Confirm {
                title: "Delete Worktree".into(),
                body: "delete 'wt'?".into(),
                yes_label: None,
                warning: Some("uncommitted changes".into()),
                force_label: Some("force".into()),
                action: ConfirmAction::DeleteWorktree {
                    repository: 0,
                    worktree: 0,
                },
            },
        };

        for k in [KeyCode::Char('y'), KeyCode::Char('d'), KeyCode::Enter] {
            app.on_key(key(k, false));
            assert!(
                matches!(app.mode, Mode::Confirm { .. }),
                "{k:?} must not answer a force-only confirm"
            );
        }
    }

    /// `Enter` is the app's "open / drill into" key; it must never double as
    /// a delete, not even on a confirm that does offer the plain answer.
    #[test]
    fn enter_does_not_confirm_a_delete() {
        let mut app = app_with_shell();
        app.sel = Sel::Worktree(0, 0);
        app.on_key(key(KeyCode::Char('d'), false));
        assert!(matches!(app.mode, Mode::Confirm { .. }));
        app.on_key(key(KeyCode::Enter, false));
        assert!(
            matches!(app.mode, Mode::Confirm { .. }),
            "Enter is unbound in a confirm"
        );
    }

    // ---- M18 Task 8-10 — the furniture ----

    /// A holder is a sleeping `sh` beside every agent pane, and a sleeping
    /// shell is exactly what the classifier is built to notice. It must
    /// therefore never reach the tree or the classifier: the poll selects
    /// on identity, and furniture carries none.
    #[test]
    fn the_tree_and_the_poll_see_shells_but_never_holders() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_wid, pane) = seed_repository_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.poll();

            let holder = tmux
                .list_furniture()
                .unwrap()
                .into_iter()
                .find(|p| p.holder)
                .expect("the spawn furnished the window");
            assert!(
                !app.panes.contains_key(&holder.pane_id),
                "the poll must not carry the holder"
            );
            assert!(
                !app.statuses.contains_key(&holder.pane_id),
                "and must not classify it"
            );
            assert!(
                app.statuses.contains_key(&pane),
                "the shell itself is polled"
            );
            assert_eq!(
                app.state.repositories[0].worktrees[0].shells.len(),
                1,
                "one row, not two"
            );
        });
    }

    /// The slot is furniture in what may well be the user's own window, so
    /// it is split when docking asks for it and taken down on the way out —
    /// never at startup, and never left behind.
    #[test]
    fn the_slot_is_split_on_demand_and_removed_on_quit() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            crate::util::testutil::with_fresh_xdg(|| app.startup().expect("startup"));
            assert_eq!(app.slot, None, "startup splits nothing");
            let dash_window = tmux.inside.as_ref().unwrap().window_id.clone();
            let panes_in_dash = |t: &Tmux| {
                t.list_furniture()
                    .unwrap()
                    .into_iter()
                    .filter(|p| p.window_id == dash_window)
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                panes_in_dash(&tmux).len(),
                1,
                "the dashboard window is untouched"
            );

            let slot = app.ensure_slot().expect("slot");
            assert_eq!(app.slot.as_deref(), Some(slot.as_str()));
            let panes = panes_in_dash(&tmux);
            assert_eq!(panes.len(), 2, "the slot joined the dashboard window");
            assert_eq!(panes.iter().filter(|p| p.slot).count(), 1);

            // Idempotent: asking again returns the same pane, not a third one.
            assert_eq!(app.ensure_slot().expect("again"), slot);
            assert_eq!(panes_in_dash(&tmux).len(), 2);

            app.drop_slot();
            assert_eq!(app.slot, None);
            assert_eq!(
                panes_in_dash(&tmux).len(),
                1,
                "quitting leaves no furniture behind"
            );
        });
    }

    /// The slot is located by its `@lw_slot` mark, never by index — the
    /// user may close it, or rearrange the window around it. A slot closed
    /// by hand is simply split again.
    #[test]
    fn ensure_slot_recreates_a_slot_the_user_closed() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let first = app.ensure_slot().expect("slot");
            tmux.kill_pane(&first).expect("the user closes it by hand");

            let second = app.ensure_slot().expect("slot again");
            assert_ne!(second, first, "a new pane, since the old one is gone");
            assert_eq!(
                tmux.list_furniture()
                    .unwrap()
                    .iter()
                    .filter(|p| p.slot)
                    .count(),
                1,
                "and exactly one slot, not two"
            );

            // Stale bookkeeping is no obstacle either: `ensure_slot` asks
            // tmux, never its own memory.
            app.slot = Some("%999".into());
            assert_eq!(app.ensure_slot().expect("re-derived"), second);
        });
    }

    // ---- M18 Task 13 — recovery ----

    /// A docked pane can end its dock without us: killed, its agent exits,
    /// or another instance steals the shell. Each is ordinary — the panel
    /// goes back to the preview, which works for every shell — so the poll
    /// lets go quietly and raises no error toast.
    #[test]
    fn a_docked_pane_that_disappears_falls_back_to_the_preview() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_wid, pane) = seed_repository_with_shell(&tmux, &mut app);
            let slot = app.ensure_slot().expect("slot");
            let dash = tmux.inside.as_ref().unwrap().pane_id.clone();
            crate::dock::dock(&tmux, &slot, &pane, &dash).expect("dock");
            app.docked = Some(pane.clone());

            app.poll();
            assert_eq!(
                app.docked.as_deref(),
                Some(pane.as_str()),
                "a live dock survives the poll"
            );

            // Stolen: the lease now names another instance's pane.
            tmux.set_pane_option(&pane, crate::dock::DOCKED_KEY, "%999")
                .expect("steal");
            app.poll();
            assert_eq!(app.docked, None, "a stolen shell is let go of");
            assert!(app.message.is_none(), "and losing it is not an error");

            // Killed: the pane is not on the server at all any more.
            app.docked = Some(pane.clone());
            tmux.kill_pane(&pane).expect("kill the agent pane");
            app.poll();
            assert_eq!(app.docked, None);
            assert!(app.message.is_none());
        });
    }

    /// Killing a docked shell kills its **home** window — and while it is
    /// docked, that window holds the slot, not the agent pane. Without a
    /// release the agent pane would outlive the row that named it, still
    /// drawn beside the tree with nothing left to select.
    #[test]
    fn deleting_the_docked_shell_takes_its_pane_off_the_dashboard() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (wid, pane) = seed_repository_with_shell(&tmux, &mut app);
            app.dock_pane(&pane, false).expect("dock");
            assert_eq!(app.docked.as_deref(), Some(pane.as_str()));

            // Some other window's shell: not ours to let go of.
            app.release_dock_before_kill("@999", Some("%999"));
            assert_eq!(app.docked.as_deref(), Some(pane.as_str()));

            app.release_dock_before_kill(&wid, Some(&pane));
            assert_eq!(app.docked, None, "the dock is released");
            assert_eq!(app.slot, None, "and the slot with it");
            let home = tmux
                .list_panes()
                .expect("panes")
                .into_iter()
                .find(|p| p.pane_id == pane)
                .expect("the agent pane is still alive");
            assert_eq!(
                home.window_id, wid,
                "the agent pane is home, where the kill can reach it"
            );
        });
    }

    /// A poll taken while docked records the dashboard's window as the
    /// shell's; the kill must not trust it.
    #[test]
    fn deleting_a_docked_shell_after_a_poll_spares_the_dashboard() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_wid, pane) = seed_repository_with_shell(&tmux, &mut app);
            app.dock_pane(&pane, false).expect("dock");
            app.poll();
            let dash = tmux.inside.clone().expect("inside");
            assert_eq!(
                app.state.repositories[0].worktrees[0].shells[0].window_id, dash.window_id,
                "the poll recorded the docked pane in the dashboard's window"
            );

            app.delete_shell(0, 0, 0).expect("delete");

            let out = tmux
                .run_raw(&["list-panes", "-a", "-F", "#{pane_id}"])
                .expect("panes");
            let panes: Vec<&str> = out.lines().collect();
            assert!(
                panes.contains(&dash.pane_id.as_str()),
                "the dashboard survives"
            );
            assert!(!panes.contains(&pane.as_str()), "the shell is gone");
        });
    }

    /// While no slot exists the dashboard owns its whole pane, and the
    /// layout is the one it has always drawn: tree plus preview.
    #[test]
    fn the_preview_keeps_the_full_layout_while_no_slot_exists() {
        let mut app = app_with_shell();
        assert_eq!(app.slot, None);
        app.viewport = (120, 40);
        assert_eq!(
            crate::ui::tree_width_for(&app),
            Some(38),
            "the tree keeps its share of an undocked dashboard"
        );
        assert!(
            matches!(crate::ui::tier(&app), crate::ui::Tier::Wide),
            "and the preview keeps the rest"
        );
    }

    /// The moment a pane is docked, the preview panel has been replaced by
    /// a real tmux pane on the other side of a divider. The dashboard's own
    /// strip must then be tree and nothing else: a second, narrower,
    /// one-poll-behind copy of the agent beside the agent is the exact
    /// duplication docking exists to end.
    #[test]
    fn a_docked_pane_takes_the_preview_panel_off_the_dashboard() {
        let mut app = app_with_shell();
        // The strip a docked dashboard keeps for itself, not the window.
        app.viewport = (crate::dock::dash_width(120), 40);
        app.slot = Some("%7".into());
        app.docked = Some("%8".into());
        assert!(
            matches!(crate::ui::tier(&app), crate::ui::Tier::TreeOnly),
            "no preview panel while a pane is docked beside us"
        );
        assert_eq!(
            crate::ui::tree_width_for(&app),
            Some(app.viewport.0),
            "the tree gets the whole strip"
        );

        // Even a strip wide enough for two panels keeps only the tree:
        // width is not the reason the preview is gone.
        app.viewport = (120, 40);
        assert!(matches!(crate::ui::tier(&app), crate::ui::Tier::TreeOnly));
        // An empty slot is not a reason: it is on its way out, and the
        // preview is what takes its place.
        app.docked = None;
        assert!(matches!(crate::ui::tier(&app), crate::ui::Tier::Wide));
    }

    // ---- M18 Task 14 — dock on look ----

    /// A second shell in the same worktree, so the selection has somewhere
    /// to move to. Returns its pane.
    fn seed_second_shell(tmux: &Tmux, app: &mut App) -> String {
        let w = tmux
            .new_window(
                "wingmux-session-p",
                "lw:wt/1",
                Path::new("/tmp"),
                "bash",
                &crate::dock::ShellId::for_session("wingmux-session-p", "wt", 1),
                crate::tmux::testutil::HOLDER_W,
            )
            .unwrap();
        app.state.repositories[0].worktrees[0].shells.push(Shell {
            command: None,
            index: 1,
            window_name: "lw:wt/1".into(),
            window_id: w.window_id.clone(),
            pane_id: w.pane_id.clone(),
        });
        app.build_tree_flags();
        w.pane_id
    }

    /// Put the cursor on a row, the way a keypress would.
    fn look_at(app: &mut App, sel: Sel) {
        app.build_tree_flags();
        app.sel = sel;
        app.clamp_sel();
        app.auto_dock_seen = None;
    }

    /// Dock-on-look **coalesces** a burst rather than delaying it. The plan
    /// asked for a ~150 ms settle so held `j` autorepeat could not fire one
    /// swap per row; a settle, though, leaves the pane of the row you have
    /// already left on screen for its whole length. So the queue answers
    /// instead: while more input is waiting nothing moves, and the dock
    /// fires on the first frame after the keys stop — no timer, and so no
    /// added latency.
    #[test]
    fn following_the_selection_waits_for_the_burst_not_for_a_timer() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_w, pane) = seed_repository_with_shell(&tmux, &mut app);
            look_at(&mut app, Sel::Worktree(0, 0));

            app.auto_dock(true);
            assert_eq!(
                app.docked, None,
                "nothing moves while keys are still coming"
            );
            assert_eq!(app.slot, None, "and no furniture is built for it either");

            app.auto_dock(false);
            assert_eq!(
                app.docked.as_deref(),
                Some(pane.as_str()),
                "the dock lands the moment the burst ends"
            );
        });
    }

    /// Looking is not typing: the dock follows the cursor, the cursor does
    /// not follow the dock. A `select-pane` here would put the keyboard in
    /// the agent and the next `j` would go to the agent with it.
    #[test]
    fn following_the_selection_leaves_the_keyboard_in_the_tree() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_w, pane) = seed_repository_with_shell(&tmux, &mut app);
            look_at(&mut app, Sel::Worktree(0, 0));

            app.auto_dock(false);
            assert_eq!(app.docked.as_deref(), Some(pane.as_str()));
            let dash = tmux.inside.as_ref().unwrap().pane_id.clone();
            assert!(
                tmux.pane_active(&dash).unwrap(),
                "the tree still holds the keyboard"
            );

            // `Enter`, by contrast, is exactly a request to be put in it.
            app.dock_shell(0, 0, 0);
            assert!(tmux.pane_active(&pane).unwrap(), "Enter focuses the agent");
        });
    }

    /// The worst bug this design can produce is swapping a pane out from
    /// under someone mid-sentence. Keyboard selection cannot move while
    /// the TUI is unfocused, but a poll-driven reconcile can — so the
    /// guard is on the dock, not on the keys.
    #[test]
    fn nothing_is_docked_while_the_docked_pane_holds_the_keyboard() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_w, first) = seed_repository_with_shell(&tmux, &mut app);
            let second = seed_second_shell(&tmux, &mut app);

            look_at(&mut app, Sel::Shell(0, 0, 0));
            app.dock_shell(0, 0, 0);
            assert_eq!(app.docked.as_deref(), Some(first.as_str()));
            assert!(
                tmux.pane_active(&first).unwrap(),
                "the user is typing in it"
            );

            // The tree moves under them — a poll rebuilt it, say.
            look_at(&mut app, Sel::Shell(0, 0, 1));
            app.auto_dock(false);
            assert_eq!(
                app.docked.as_deref(),
                Some(first.as_str()),
                "the pane being typed in is not swapped away"
            );

            // Back to the tree, and looking resumes.
            let dash = tmux.inside.as_ref().unwrap().pane_id.clone();
            tmux.select_pane(&dash).expect("back to the tree");
            app.auto_dock_seen = None;
            app.auto_dock(false);
            assert_eq!(app.docked.as_deref(), Some(second.as_str()));
        });
    }

    /// The poll must stamp its own clock. It did not, so the event loop's
    /// "is a poll due" test stayed true for ever once the first interval had
    /// passed: the input timeout collapsed to zero and the dashboard
    /// span-polled — a `capture-pane` per shell per frame, and a failing
    /// dock retried about ninety times a second in the log.
    #[test]
    fn a_poll_stamps_its_own_clock_so_the_loop_can_wait() {
        let mut app = App::new(Config::default(), theme::load("ansi", false).unwrap(), None);
        app.last_poll = Instant::now() - Duration::from_secs(5);
        assert!(app.last_poll.elapsed() >= Duration::from_millis(app.cfg.poll_interval_ms));
        // No tmux: `poll` returns early, which is precisely the path that
        // must still count as "polled" — every early return is a reason to
        // wait for the next tick, not to try again on the next frame.
        app.poll();
        assert!(
            app.last_poll.elapsed() < Duration::from_millis(app.cfg.poll_interval_ms),
            "the next poll is not due yet"
        );
    }

    /// There is no undocked mode to be in. The dashboard docks the selected
    /// shell from the first frame, without waiting to be armed by a
    /// keypress: an "undocked" startup state offers nothing the docked one
    /// does not, and there is no key that returns to it — so it was only
    /// ever a state you left once and could not explain.
    #[test]
    fn the_selected_shell_is_docked_from_the_first_frame() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_w, pane) = seed_repository_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.sel = Sel::Worktree(0, 0);
            app.clamp_sel();

            app.auto_dock(false);
            assert_eq!(
                app.docked.as_deref(),
                Some(pane.as_str()),
                "nothing had to be pressed first"
            );
        });
    }

    /// What the panel shows is what the cursor is on, or the preview —
    /// never a third shell's pane left over from a row the cursor has
    /// left. A leftover reads as the selected agent, which is the one
    /// thing a live pane must never be mistaken for.
    #[test]
    fn a_row_with_nothing_to_dock_gives_the_space_back_to_the_preview() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_w, pane) = seed_repository_with_shell(&tmux, &mut app);
            app.viewport = (120, 40);
            look_at(&mut app, Sel::Worktree(0, 0));
            app.auto_dock(false);
            assert_eq!(app.docked.as_deref(), Some(pane.as_str()));
            let slot = app.slot.clone().expect("a slot while something is docked");

            // A second repository, to have a sibling row.
            app.state.repositories.push(Repository {
                name: "q".into(),
                path: PathBuf::from("/tmp/q"),
                missing: false,
                shell: None,
                worktrees: vec![],
            });
            look_at(&mut app, Sel::Repository(0));
            assert!(matches!(app.sel, Sel::Repository(0)), "on a repository row");
            app.auto_dock(false);
            assert_eq!(app.docked, None, "the previous shell does not linger");
            assert_eq!(app.slot, None, "and the slot goes with it");
            assert!(
                tmux.list_furniture()
                    .unwrap()
                    .iter()
                    .all(|p| p.pane_id != slot),
                "really gone, not just forgotten"
            );
            assert!(
                matches!(crate::ui::tier(&app), crate::ui::Tier::Wide),
                "the preview has the space back"
            );
            assert_eq!(
                tmux.list_panes()
                    .unwrap()
                    .iter()
                    .find(|p| p.pane_id == pane)
                    .map(|p| p.session_name.clone()),
                Some("wingmux-session-p".into()),
                "and the agent pane went home rather than being killed with it"
            );
        });
    }

    /// The same rule with a lease as the reason: a shell another dashboard
    /// holds is shown as a preview — whose title names the holder — never
    /// as the pane of whatever row the cursor was on before.
    #[test]
    fn looking_at_a_leased_shell_falls_back_to_the_preview() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_w, first) = seed_repository_with_shell(&tmux, &mut app);
            let second = seed_second_shell(&tmux, &mut app);
            app.viewport = (120, 40);

            look_at(&mut app, Sel::Shell(0, 0, 0));
            app.auto_dock(false);
            assert_eq!(app.docked.as_deref(), Some(first.as_str()));

            // Another dashboard takes the second shell.
            let (rival, rival_slot) = rival_dashboard(sock, &tmux, "/home/ike/other");
            crate::dock::dock(&tmux, &rival_slot, &second, &rival).expect("they dock it");
            app.poll();
            assert!(app.leased_by(&second).is_some());

            look_at(&mut app, Sel::Shell(0, 0, 1));
            app.auto_dock(false);
            assert_eq!(app.docked, None, "we do not show them the first shell");
            assert!(
                matches!(crate::ui::tier(&app), crate::ui::Tier::Wide),
                "the preview says what is going on instead"
            );
            assert_eq!(
                tmux.list_panes()
                    .unwrap()
                    .iter()
                    .find(|p| p.pane_id == second)
                    .and_then(|p| p.docked_by.clone()),
                Some(rival),
                "and their dock is untouched"
            );
        });
    }

    // ---- M18 Task 15 — the lease ----

    /// The whole defence against two idle dashboards trading a pane
    /// forever: looking yields. Without it, two instances with the same
    /// shell selected take it from each other on every poll with nobody
    /// pressing a key.
    #[test]
    fn following_the_selection_yields_to_another_dashboard() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_w, pane) = seed_repository_with_shell(&tmux, &mut app);
            look_at(&mut app, Sel::Worktree(0, 0));

            // Another instance holds it. Its dashboard pane is real and
            // alive — a mark naming a dead pane is wreckage, not a lease.
            let rival = tmux
                .split_slot(&tmux.inside.as_ref().unwrap().pane_id, 20)
                .expect("a pane to stand in for the other dashboard");
            tmux.set_pane_option(&pane, crate::dock::DOCKED_KEY, &rival)
                .expect("their lease");

            app.poll();
            assert_eq!(
                app.leased_by(&pane).map(|l| l.dash_pane.clone()),
                Some(rival.clone()),
                "the poll reads the lease off the pane, not off any state of ours"
            );

            app.auto_dock(false);
            assert_eq!(app.docked, None, "looking does not take a held shell");

            // Released: the next poll clears the lease and looking follows.
            tmux.clear_pane_option(&pane, crate::dock::DOCKED_KEY)
                .expect("they let go");
            app.poll();
            assert!(app.leased_by(&pane).is_none());
            app.auto_dock(false);
            assert_eq!(app.docked.as_deref(), Some(pane.as_str()));
        });
    }

    /// A `@lw_docked` mark left behind by an instance that crashed names a
    /// pane that no longer exists. Reading that as a lease would fence the
    /// shell off from every dashboard for good.
    #[test]
    fn a_mark_naming_a_dead_dashboard_is_wreckage_not_a_lease() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_w, pane) = seed_repository_with_shell(&tmux, &mut app);
            look_at(&mut app, Sel::Worktree(0, 0));
            tmux.set_pane_option(&pane, crate::dock::DOCKED_KEY, "%999")
                .expect("a mark from a dashboard that died");

            app.poll();
            assert!(app.leased_by(&pane).is_none());
            app.auto_dock(false);
            assert_eq!(app.docked.as_deref(), Some(pane.as_str()));
        });
    }

    // ---- M18 Task 16 — eager steal ----

    /// Stand up a second dashboard: its own window, announcing its own
    /// scope, with a slot of its own. Returns (its pane, its slot).
    fn rival_dashboard(sock: &str, tmux: &Tmux, scope: &str) -> (String, String) {
        use crate::tmux::testutil::tmux_raw;
        let pane = tmux_raw(
            sock,
            &["new-window", "-P", "-F", "#{pane_id}", "-t", "main", "-d"],
        );
        let window = tmux_raw(
            sock,
            &["display-message", "-p", "-t", &pane, "#{window_id}"],
        );
        tmux.set_window_option(&window, crate::dock::SCOPE_KEY, scope)
            .expect("the rival announces its scope");
        let slot = tmux.split_slot(&pane, 40).expect("their slot");
        (pane, slot)
    }

    /// Which window a pane is in right now.
    fn window_of(tmux: &Tmux, pane: &str) -> String {
        tmux.list_furniture()
            .expect("furniture")
            .into_iter()
            .find(|p| p.pane_id == pane)
            .map(|p| p.window_id)
            .expect("the pane still exists")
    }

    /// `Enter` on a shell another dashboard holds takes it, with no dialog
    /// in the way: the case is rare and the answer is always yes — someone
    /// pressed the key on the row. Both sides are told instead.
    #[test]
    fn enter_takes_a_leased_shell_with_no_confirm_and_says_so() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (home, pane) = seed_repository_with_shell(&tmux, &mut app);
            look_at(&mut app, Sel::Worktree(0, 0));

            let (rival, rival_slot) = rival_dashboard(sock, &tmux, "/home/ike/other-scope");
            crate::dock::dock(&tmux, &rival_slot, &pane, &rival).expect("they dock it first");
            let rival_window = window_of(&tmux, &rival);
            app.poll();
            assert!(app.leased_by(&pane).is_some(), "they hold it");

            app.dock_shell(0, 0, 0);
            assert!(
                matches!(app.mode, Mode::Normal),
                "no dialog stands between the key and the pane"
            );
            assert_eq!(app.docked.as_deref(), Some(pane.as_str()), "we hold it now");
            assert_eq!(
                window_of(&tmux, &pane),
                tmux.inside.as_ref().unwrap().window_id,
                "and it is really in our window"
            );
            let toast = app
                .message
                .as_ref()
                .expect("the taker is told")
                .text
                .clone();
            assert!(
                toast.contains("other-scope"),
                "the toast names who lost it: {toast}"
            );

            // Their furniture is their own again: a steal that swapped
            // straight out of their window would have left our slot in the
            // agent's window and theirs in ours.
            assert_eq!(
                window_of(&tmux, &rival_slot),
                rival_window,
                "their slot went back to them"
            );
            assert_eq!(
                window_of(&tmux, app.slot.as_deref().expect("our slot")),
                home,
                "and ours is where the agent came from"
            );
        });
    }

    /// The other half of the same event. The loser's next poll finds the
    /// shell gone, falls back to the preview — the pane is still alive and
    /// still in the tree, so a panel that silently reverted would read as a
    /// stalled agent — and says which scope took it.
    #[test]
    fn the_instance_that_lost_a_shell_falls_back_to_the_preview_and_is_told_who_took_it() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_home, pane) = seed_repository_with_shell(&tmux, &mut app);
            look_at(&mut app, Sel::Worktree(0, 0));
            app.viewport = (120, 40);

            app.dock_shell(0, 0, 0);
            assert_eq!(app.docked.as_deref(), Some(pane.as_str()));
            let ours = app.slot.clone().expect("our slot");
            assert!(
                matches!(crate::ui::tier(&app), crate::ui::Tier::TreeOnly),
                "a docked dashboard is tree-only"
            );

            // They take it: home first, then into their own slot — the same
            // two steps our own steal takes.
            let (rival, rival_slot) = rival_dashboard(sock, &tmux, "/home/ike/thief");
            let id = crate::dock::ShellId::for_session("wingmux-session-p", "wt", 0);
            let home = crate::dock::home_window(&tmux.list_furniture().unwrap(), &id, &[])
                .expect("its own window is waiting");
            crate::dock::undock(&tmux, &pane, &home).expect("they send it home");
            crate::dock::dock(&tmux, &rival_slot, &pane, &rival).expect("they take it");

            app.poll();
            assert_eq!(app.docked, None, "we let go");
            let toast = app
                .message
                .as_ref()
                .expect("the loser is told")
                .text
                .clone();
            assert!(
                toast.contains("thief"),
                "the toast names the taker: {toast}"
            );
            assert_eq!(app.slot, None, "our slot came down with the dock");
            assert!(
                tmux.list_furniture()
                    .unwrap()
                    .iter()
                    .all(|p| p.pane_id != ours),
                "and the pane really went"
            );
            assert!(
                matches!(crate::ui::tier(&app), crate::ui::Tier::Wide),
                "the preview is back — it is the only view that works for a \
                 shell we do not hold"
            );
        });
    }

    /// The dashboard never cuts itself down to the "terminal too small"
    /// tier: the narrowest strip docking will leave it is still a strip the
    /// tree renders in.
    #[test]
    fn the_narrowest_docked_dashboard_still_draws_its_tree() {
        let mut app = app_with_shell();
        app.slot = Some("%7".into());
        app.docked = Some("%8".into());
        for window in [40u16, 60, 120, 400] {
            app.viewport = (crate::dock::dash_width(window), 40);
            assert!(
                matches!(crate::ui::tier(&app), crate::ui::Tier::TreeOnly),
                "a {window}-column window leaves a {}-column strip that cannot draw the tree",
                app.viewport.0
            );
        }
    }

    /// A same-size swap is what spares the agent a repaint, and equal sizes
    /// are what `window-size manual` plus one `resize-window` per agent
    /// window buy. With the agent window matching the dashboard's, the
    /// agent pane and the slot are the same size and the swap moves them
    /// without touching either one's dimensions.
    #[test]
    fn a_size_matched_swap_costs_neither_pane_a_resize() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_wid, agent) = seed_repository_with_shell(&tmux, &mut app);
            let slot = app.ensure_slot().expect("slot");

            let size = |pane: &str| {
                tmux.run_raw(&[
                    "display-message",
                    "-p",
                    "-t",
                    pane,
                    "#{pane_width}x#{pane_height}",
                ])
                .unwrap()
                .trim()
                .to_string()
            };
            let (before_slot, before_agent) = (size(&slot), size(&agent));
            assert_eq!(
                before_slot, before_agent,
                "the agent pane and the slot must start out identical"
            );

            tmux.run_raw(&["swap-pane", "-d", "-s", &slot, "-t", &agent])
                .expect("dock");
            assert_eq!(size(&slot), before_slot, "the slot pane kept its size");
            assert_eq!(size(&agent), before_agent, "and so did the agent");

            // Neither window gained or lost a pane — the whole reason a
            // swap beat join-pane/break-pane.
            let furniture = tmux.list_furniture().unwrap();
            for window in [
                tmux.inside.as_ref().unwrap().window_id.clone(),
                furniture
                    .iter()
                    .find(|p| p.holder)
                    .expect("holder")
                    .window_id
                    .clone(),
            ] {
                assert_eq!(
                    furniture.iter().filter(|p| p.window_id == window).count(),
                    2,
                    "window {window} still holds two panes"
                );
            }
        });
    }

    // ---- M18 Task 12 — wiring the key ----

    /// Give the seeded worktree a second, real shell, so its row stops
    /// being folded onto one and the tree shows shell rows of its own.
    fn add_second_shell(tmux: &Tmux, app: &mut App) -> String {
        let w = tmux
            .new_window(
                "wingmux-session-p",
                "lw:wt/1",
                Path::new("/tmp"),
                "bash",
                &crate::dock::ShellId::for_session("wingmux-session-p", "wt", 1),
                crate::tmux::testutil::HOLDER_W,
            )
            .expect("second shell");
        app.state.repositories[0].worktrees[0].shells.push(Shell {
            command: None,
            index: 1,
            window_name: "lw:wt/1".into(),
            window_id: w.window_id.clone(),
            pane_id: w.pane_id.clone(),
        });
        app.build_tree_flags();
        w.pane_id
    }

    /// Where a pane sits now, and who claims it.
    fn docked_state(tmux: &Tmux, pane: &str) -> (String, Option<String>, bool) {
        let p = tmux
            .list_panes()
            .expect("panes")
            .into_iter()
            .find(|p| p.pane_id == pane)
            .expect("pane is still one of ours");
        (p.window_id, p.docked_by, p.active)
    }

    /// Seed a repository whose path is a real directory — the inline shell
    /// refuses a path that is not there, and `/tmp/p` is not.
    fn seed_bare_repository(app: &mut App, dir: &std::path::Path) {
        app.state.repositories.push(Repository {
            name: "p".into(),
            path: dir.to_path_buf(),
            missing: false,
            shell: None,
            worktrees: vec![],
        });
        app.build_tree_flags();
        app.sel = Sel::Repository(0);
        app.clamp_sel();
    }

    /// `a` on a repository row starts a shell in the repository's own
    /// checkout and docks it, and a second `a` re-enters that same shell.
    /// `Enter` only toggles the row.
    #[test]
    fn a_on_a_repository_row_starts_and_docks_its_inline_shell() {
        crate::util::testutil::with_fresh_xdg(|| {
            with_tmux_server(|sock, _tmux| {
                let dir = tempfile::TempDir::new().unwrap();
                let tmux = tmux_inside(sock);
                let mut app = app_with(tmux.clone());
                seed_bare_repository(&mut app, dir.path());

                app.on_key(key(KeyCode::Enter, false));
                assert!(
                    app.repo_shell_pane(0).is_none(),
                    "Enter expands, it starts nothing"
                );

                app.on_key(key(KeyCode::Char('a'), false));

                let pane = app.repo_shell_pane(0).expect("`a` starts the shell");
                assert_eq!(
                    app.selected_shell_pane().as_deref(),
                    Some(pane.as_str()),
                    "the repository row owns that pane, so the panel follows it"
                );
                assert_eq!(
                    app.docked.as_deref(),
                    Some(pane.as_str()),
                    "and the pane is docked beside the tree"
                );
                let id = tmux
                    .list_panes()
                    .expect("panes")
                    .into_iter()
                    .find(|p| p.pane_id == pane)
                    .and_then(|p| p.shell)
                    .expect("the pane carries an identity");
                assert!(
                    id.is_repo_shell(),
                    "stamped as the repository's shell, not a worktree's"
                );

                app.on_key(key(KeyCode::Char('a'), false));
                assert_eq!(
                    app.repo_shell_pane(0).as_deref(),
                    Some(pane.as_str()),
                    "a second `a` re-enters the shell it started"
                );
            });
        });
    }

    /// The shell is lazy: moving the cursor onto a repository row must not
    /// pay for a shell nobody asked for.
    #[test]
    fn looking_at_a_repository_row_starts_nothing() {
        crate::util::testutil::with_fresh_xdg(|| {
            with_tmux_server(|sock, _tmux| {
                let dir = tempfile::TempDir::new().unwrap();
                let tmux = tmux_inside(sock);
                let mut app = app_with(tmux.clone());
                seed_bare_repository(&mut app, dir.path());

                app.poll();
                app.auto_dock(false);

                assert!(
                    app.state.repositories[0].shell.is_none(),
                    "looking is free; only Enter starts a shell"
                );
                assert!(app.docked.is_none(), "and there is nothing to dock");
            });
        });
    }

    /// `d` on a repository row closes its inline shell and nothing else —
    /// and `Enter` afterwards starts a fresh one.
    #[test]
    fn d_on_a_repository_row_closes_its_inline_shell() {
        crate::util::testutil::with_fresh_xdg(|| {
            with_tmux_server(|sock, _tmux| {
                let dir = tempfile::TempDir::new().unwrap();
                let tmux = tmux_inside(sock);
                let mut app = app_with(tmux.clone());
                seed_bare_repository(&mut app, dir.path());
                app.ensure_repo_shell(0).expect("shell");
                let pane = app.repo_shell_pane(0).expect("pane");

                app.on_key(key(KeyCode::Char('d'), false));
                assert!(
                    matches!(app.mode, Mode::Confirm { .. }),
                    "closing a shell asks first"
                );
                app.on_key(key(KeyCode::Char('y'), false));

                assert!(app.state.repositories[0].shell.is_none());
                assert!(
                    !tmux
                        .list_panes()
                        .expect("panes")
                        .iter()
                        .any(|p| p.pane_id == pane),
                    "the tmux window went with it"
                );
            });
        });
    }

    /// A repository whose path is gone gets a message, not a shell opened
    /// wherever tmux felt like putting it.
    #[test]
    fn a_missing_repository_gets_no_inline_shell() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with(tmux_nowhere());
            seed_bare_repository(&mut app, std::path::Path::new("/tmp/lw-does-not-exist"));

            assert!(app.ensure_repo_shell(0).is_err());
            assert!(app.state.repositories[0].shell.is_none());
        });
    }

    /// `Enter` on a shell row is the whole gesture: the shell's own tmux
    /// pane lands beside the tree and the cursor is already in it.
    #[test]
    fn enter_on_a_shell_row_docks_and_focuses() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (home, first) = seed_repository_with_shell(&tmux, &mut app);
            add_second_shell(&tmux, &mut app);
            // Two shells, so the worktree row is not folded onto one and
            // the shell rows are what the tree actually offers.
            app.wt_expanded[0][0] = true;
            app.sel = Sel::Shell(0, 0, 0);
            assert!(app.visible_nodes().contains(&Sel::Shell(0, 0, 0)));

            app.on_key(key(KeyCode::Enter, false));

            let dash = tmux.inside.clone().expect("inside");
            assert_eq!(app.docked.as_deref(), Some(first.as_str()));
            assert_eq!(
                app.docked_home.as_deref(),
                Some(home.as_str()),
                "and remembers where to send it back"
            );
            let (window, by, active) = docked_state(&tmux, &first);
            assert_eq!(window, dash.window_id, "the pane is beside the tree");
            assert_eq!(
                by.as_deref(),
                Some(dash.pane_id.as_str()),
                "the lease says whose"
            );
            assert!(active, "and the cursor is in it");
            assert!(app.message.is_none(), "docking is not news");
        });
    }

    /// The folded worktree row *is* its shell, and `Enter` must agree with
    /// what the tree draws: no shell row exists to press instead.
    #[test]
    fn enter_on_a_solo_worktree_row_docks_its_shell() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_home, pane) = seed_repository_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.sel = Sel::Worktree(0, 0);
            assert_eq!(app.solo_shell(0, 0), Some(0));
            assert!(
                !app.visible_nodes()
                    .iter()
                    .any(|n| matches!(n, Sel::Shell(..))),
                "the solo rule folds the shell away, so the worktree row must act for it"
            );

            app.on_key(key(KeyCode::Enter, false));

            assert_eq!(app.docked.as_deref(), Some(pane.as_str()));
            let dash = tmux.inside.clone().expect("inside");
            assert_eq!(docked_state(&tmux, &pane).0, dash.window_id);
        });
    }

    /// One docked pane at a time: docking a second shell sends the first
    /// one home, so no window is ever left short of its agent.
    #[test]
    fn docking_a_second_shell_sends_the_first_one_home() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (first_home, first) = seed_repository_with_shell(&tmux, &mut app);
            let second = add_second_shell(&tmux, &mut app);
            app.wt_expanded[0][0] = true;

            app.sel = Sel::Shell(0, 0, 0);
            app.on_key(key(KeyCode::Enter, false));
            app.sel = Sel::Shell(0, 0, 1);
            app.on_key(key(KeyCode::Enter, false));

            let dash = tmux.inside.clone().expect("inside");
            assert_eq!(app.docked.as_deref(), Some(second.as_str()));
            assert_eq!(docked_state(&tmux, &second).0, dash.window_id);
            let (window, by, _) = docked_state(&tmux, &first);
            assert_eq!(window, first_home, "the first shell went home");
            assert_eq!(by, None, "and released its lease");
        });
    }

    /// `Enter` on the shell that is already docked is "put me back in it",
    /// not a second swap that would bounce the pane out and in again.
    #[test]
    fn enter_on_the_docked_shell_only_moves_the_focus() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_home, pane) = seed_repository_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.sel = Sel::Worktree(0, 0);
            app.on_key(key(KeyCode::Enter, false));
            let dash = tmux.inside.clone().expect("inside");
            tmux.select_pane(&dash.pane_id).expect("focus the tree");

            app.on_key(key(KeyCode::Enter, false));

            assert_eq!(app.docked.as_deref(), Some(pane.as_str()));
            let (window, by, active) = docked_state(&tmux, &pane);
            assert_eq!(window, dash.window_id, "it never left");
            assert_eq!(by.as_deref(), Some(dash.pane_id.as_str()));
            assert!(active, "the focus came back to it");
        });
    }

    /// Quitting with a pane docked must send it home first: the slot is
    /// killed on the way out, and an agent left in a window we are tidying
    /// would go with it.
    #[test]
    fn quitting_sends_a_docked_pane_home_before_the_slot_goes() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (home, pane) = seed_repository_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.sel = Sel::Worktree(0, 0);
            app.on_key(key(KeyCode::Enter, false));
            assert_eq!(app.docked.as_deref(), Some(pane.as_str()));

            app.drop_slot();

            assert_eq!(app.docked, None);
            assert_eq!(app.slot, None);
            let (window, by, _) = docked_state(&tmux, &pane);
            assert_eq!(window, home, "the agent is back in its own window");
            assert_eq!(by, None);
            let dash = tmux.inside.clone().expect("inside");
            assert_eq!(
                tmux.list_furniture()
                    .unwrap()
                    .iter()
                    .filter(|p| p.window_id == dash.window_id)
                    .count(),
                1,
                "and the dashboard window is its own again"
            );
        });
    }

    /// A worktree with no shell has nothing to expand into and nothing to
    /// dock. `Enter` there must say so — silently toggling an invisible
    /// expand flag is a dead key.
    #[test]
    fn enter_on_a_worktree_with_no_shell_explains_itself() {
        let mut app = app_with_shell();
        app.state.repositories[0].worktrees[0].shells.clear();
        app.build_tree_flags();
        app.sel = Sel::Worktree(0, 0);

        app.on_key(key(KeyCode::Enter, false));

        assert_eq!(app.docked, None, "nothing was docked");
        let toast = app.toast().expect("it explains itself");
        assert!(
            toast.text.contains("No agent shell") && toast.text.contains('a'),
            "and names the key that fixes it: {}",
            toast.text
        );
    }

    /// A resized dashboard takes every agent window with it, so the next
    /// dock is still a same-size swap.
    #[test]
    fn resizing_the_dashboard_window_resizes_the_agent_windows() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (agent_window, _pane) = seed_repository_with_shell(&tmux, &mut app);
            app.poll();
            assert_eq!(app.strip, Some((200, 50)));
            assert_eq!(tmux.window_size(&agent_window).unwrap(), (200, 50));

            // The user drags their terminal narrower; tmux resizes the
            // dashboard's window, and the next poll passes it on.
            tmux.run_raw(&["set-option", "-t", "main", "window-size", "manual"])
                .unwrap();
            tmux.resize_window(&tmux.inside.as_ref().unwrap().window_id, 140, 40)
                .unwrap();
            app.poll();

            assert_eq!(app.strip, Some((140, 40)));
            assert_eq!(
                tmux.window_size(&agent_window).unwrap(),
                (140, 40),
                "the agent window followed the dashboard's"
            );
            let holder = tmux
                .list_furniture()
                .unwrap()
                .into_iter()
                .find(|p| p.holder)
                .expect("holder");
            let width = tmux
                .run_raw(&[
                    "display-message",
                    "-p",
                    "-t",
                    &holder.pane_id,
                    "#{pane_width}",
                ])
                .unwrap()
                .trim()
                .parse::<u16>()
                .unwrap();
            assert_eq!(
                width,
                crate::dock::dash_width(140),
                "and its holder was re-cut to the dashboard pane's width"
            );
        });
    }

    /// The dashboard is a guest in the user's own window, and the user may
    /// keep a pane of their own beside it. The slot is then split out of a
    /// strip narrower than the window — so it must be asked for at the
    /// strip's proportions, not the window's, or `split-window` gives the
    /// slot everything and leaves the tree a couple of columns.
    #[test]
    fn a_foreign_pane_beside_the_dashboard_does_not_squeeze_the_tree() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            seed_repository_with_shell(&tmux, &mut app);
            let dash = tmux.inside.as_ref().unwrap().pane_id.clone();
            // The user opens a pane of their own on the right.
            let theirs = tmux
                .run_raw(&[
                    "split-window",
                    "-h",
                    "-d",
                    "-l",
                    "120",
                    "-t",
                    &dash,
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "bash",
                ])
                .unwrap()
                .trim()
                .to_string();
            let width = |pane: &str| tmux.pane_geometry(pane).unwrap().0;
            let strip = width(&dash);
            assert_eq!(strip, 79, "the dashboard keeps what the split left it");

            // Unfocusing and refocusing a shell drops the slot and splits a
            // new one — the cycle that used to squeeze the tree away.
            for _ in 0..3 {
                app.ensure_slot().expect("slot");
                let slot = app.slot.clone().expect("slot id");
                assert_eq!(
                    width(&dash),
                    crate::dock::dash_width(strip),
                    "the tree keeps its share of the strip"
                );
                assert_eq!(
                    width(&slot),
                    crate::dock::agent_width(strip),
                    "and the slot takes the rest of it"
                );
                assert_eq!(width(&theirs), 120, "the user's own pane is left alone");
                app.drop_slot();
            }
        });
    }

    /// `R` is the repair for a layout dragged out of shape: an agent
    /// window someone resized by hand, and a holder cut to the wrong
    /// width, are both put back without the dashboard window changing size
    /// (which is the only thing that otherwise re-cuts them).
    #[test]
    fn reset_layout_puts_the_agent_windows_and_holders_back() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (agent_window, _pane) = seed_repository_with_shell(&tmux, &mut app);
            app.poll();

            // Someone attaches the agent session and drags it about.
            tmux.resize_window(&agent_window, 100, 30).unwrap();
            let holder = tmux
                .list_furniture()
                .unwrap()
                .into_iter()
                .find(|p| p.holder)
                .expect("holder");
            tmux.resize_pane_width(&holder.pane_id, 20).unwrap();

            app.on_key(key(KeyCode::Char('R'), false));

            assert_eq!(
                tmux.window_size(&agent_window).unwrap(),
                (200, 50),
                "the agent window is back at the dashboard window's size"
            );
            let width = tmux
                .run_raw(&[
                    "display-message",
                    "-p",
                    "-t",
                    &holder.pane_id,
                    "#{pane_width}",
                ])
                .unwrap()
                .trim()
                .parse::<u16>()
                .unwrap();
            assert_eq!(
                width,
                crate::dock::dash_width(200),
                "and the holder is back at the dashboard pane's width"
            );
            assert!(
                app.toast().is_some_and(|t| t.text.contains("Layout reset")),
                "and it says so"
            );
        });
    }

    /// The confirm popup takes `d` as a yes: `dd` deletes the way vim
    /// does, without moving the hand off the key that opened the popup.
    #[test]
    fn d_confirms_the_delete_popup() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            seed_repository_with_shell(&tmux, &mut app);
            let w2 = tmux
                .new_window(
                    "wingmux-session-p",
                    "lw:wt/1",
                    Path::new("/tmp"),
                    "bash",
                    &crate::dock::ShellId::for_session("wingmux-session-p", "wt", 1),
                    crate::tmux::testutil::HOLDER_W,
                )
                .unwrap();
            app.state.repositories[0].worktrees[0].shells.push(Shell {
                command: None,
                index: 1,
                window_name: "lw:wt/1".into(),
                window_id: w2.window_id.clone(),
                pane_id: w2.pane_id.clone(),
            });
            app.build_tree_flags();
            app.sel = Sel::Shell(0, 0, 1);

            app.on_key(key(KeyCode::Char('d'), false));
            assert!(
                matches!(app.mode, Mode::Confirm { .. }),
                "the first d opens the confirm, got {:?}",
                app.mode
            );
            // the delete saves state — keep it off the real state.json
            crate::util::testutil::with_fresh_xdg(|| app.on_key(key(KeyCode::Char('d'), false)));

            assert!(
                matches!(app.mode, Mode::Normal),
                "the second d answers the confirm, got {:?}",
                app.mode
            );
            assert_eq!(
                app.state.repositories[0].worktrees[0].shells.len(),
                1,
                "dd closed the selected shell"
            );
        });
    }

    /// Closing a shell kills that shell's window and nothing else: the
    /// worktree, its worktree entry and the sibling shell survive.
    #[test]
    fn delete_shell_kills_one_window_and_keeps_the_worktree() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            seed_repository_with_shell(&tmux, &mut app);
            let w2 = tmux
                .new_window(
                    "wingmux-session-p",
                    "lw:wt/1",
                    Path::new("/tmp"),
                    "bash",
                    &crate::dock::ShellId::for_session("wingmux-session-p", "wt", 1),
                    crate::tmux::testutil::HOLDER_W,
                )
                .unwrap();
            app.state.repositories[0].worktrees[0].shells.push(Shell {
                command: None,
                index: 1,
                window_name: "lw:wt/1".into(),
                window_id: w2.window_id.clone(),
                pane_id: w2.pane_id.clone(),
            });
            app.build_tree_flags();
            app.sel = Sel::Shell(0, 0, 1);

            // save_state() writes to XDG_STATE_HOME — keep it off the
            // developer's real state.json (the shared helper is the rule).
            crate::util::testutil::with_fresh_xdg(|| app.delete_shell(0, 0, 1).unwrap());

            assert_eq!(
                app.state.repositories[0].worktrees.len(),
                1,
                "the worktree must survive closing one of its shells"
            );
            let wt = &app.state.repositories[0].worktrees[0];
            assert_eq!(wt.shells.len(), 1, "only the closed shell is dropped");
            assert_eq!(wt.shells[0].index, 0);
            let live: Vec<usize> = tmux
                .list_panes()
                .unwrap()
                .into_iter()
                .filter_map(|p| p.shell)
                .filter(|id| id.worktree == "wt")
                .map(|id| id.index)
                .collect();
            assert_eq!(
                live,
                vec![0],
                "the sibling shell stays and the closed one is gone"
            );
        });
    }

    /// Deleting a worktree returns at once with the row still there and
    /// marked; the result is applied by the loop, never under an open popup,
    /// and the cursor stays on the row it was on.
    #[test]
    fn worktree_removal_runs_off_the_loop() {
        with_tmux_server(|sock, _tmux| {
            let root = tempfile::TempDir::new().unwrap();
            let path = repo_with_commit(root.path(), "repo");
            let mut wts = Vec::new();
            for name in ["a", "b"] {
                let wt = root.path().join(format!("wt-{name}"));
                Git.worktree_add(&path, &wt, name, "main").unwrap();
                wts.push(Worktree {
                    name: name.into(),
                    branch: name.into(),
                    path: wt,
                    created_at: String::new(),
                    base_commit: None,
                    agent_command: None,
                    shells: vec![],
                });
            }
            let mut app = app_with(tmux_inside(sock));
            app.state.repositories.push(Repository {
                name: "repo".into(),
                path: path.clone(),
                missing: false,
                shell: None,
                worktrees: wts,
            });
            app.build_tree_flags();
            app.sel = Sel::Worktree(0, 1);
            let gone = root.path().join("wt-a");

            crate::util::testutil::with_fresh_xdg(|| {
                app.delete_worktree(0, 0, false).unwrap();
                assert_eq!(app.state.repositories[0].worktrees.len(), 2);
                assert!(app.worktree_removing(0, 0));

                app.open_help();
                let deadline = Instant::now() + Duration::from_secs(10);
                while gone.exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                app.drain_removals();
                assert_eq!(
                    app.state.repositories[0].worktrees.len(),
                    2,
                    "an open popup holds indices: nothing is applied under it"
                );

                app.mode = Mode::Normal;
                app.wait_removals();
            });

            let names: Vec<_> = app.state.repositories[0]
                .worktrees
                .iter()
                .map(|w| w.name.as_str())
                .collect();
            assert_eq!(names, ["b"]);
            assert_eq!(app.sel, Sel::Worktree(0, 0), "the cursor follows 'b'");
            assert!(!gone.exists());
            assert!(
                !Git.branch_exists(&path, "a"),
                "the branch goes with the worktree"
            );
        });
    }

    /// `l`/Enter on a folded worktree attaches instead of expanding, and
    /// `h` walks up to the repository rather than toggling a flag nothing
    /// renders.
    #[test]
    fn enter_on_a_folded_worktree_attaches_its_shell() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (wid, _pid) = seed_repository_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.sel = Sel::Worktree(0, 0);

            app.expand_or_attach();
            // `switch-client` needs a client, which a headless test has not
            // got; the window selection is the half that lands either way,
            // and it is the half that says WHICH shell was attached.
            let active = tmux
                .run_raw(&[
                    "display-message",
                    "-p",
                    "-t",
                    "wingmux-session-p",
                    "#{window_id}",
                ])
                .unwrap();
            assert_eq!(
                active.trim(),
                wid,
                "Enter on a folded worktree must attach its shell"
            );
            assert!(
                !app.visible_nodes().contains(&Sel::Shell(0, 0, 0)),
                "a folded worktree must never grow a child row"
            );

            app.wt_expanded[0][0] = true; // stale flag must not eat the key
            app.collapse_or_up();
            assert_eq!(
                app.sel,
                Sel::Repository(0),
                "h walks up from a folded worktree"
            );
        });
    }

    /// PageDown / PageUp and ctrl-d / ctrl-u move half a page; ctrl-f / b
    /// a full page; Home / End and g / G jump to first / last; arrows are
    /// the same code path as j/k.
    #[test]
    fn accessibility_motions_work_in_normal_mode() {
        let mut app = app_with_repositories(20);
        app.viewport = (120, 40);
        app.on_key(key(KeyCode::Char('j'), false));
        assert_eq!(app.sel, Sel::Repository(1));

        // arrows = the same code path as j/k
        app.on_key(key(KeyCode::Up, false));
        assert_eq!(app.sel, Sel::Repository(0));

        // half page down: (40-3)/2 = 18 rows
        app.on_key(key(KeyCode::PageDown, false));
        assert_eq!(app.sel, Sel::Repository(18));
        app.on_key(key(KeyCode::PageUp, false));
        assert_eq!(app.sel, Sel::Repository(0));

        // ctrl-d / ctrl-u = PageDown/PageUp spellings
        app.on_key(key(KeyCode::Char('d'), true));
        assert_eq!(app.sel, Sel::Repository(18));
        app.on_key(key(KeyCode::Char('u'), true));
        assert_eq!(app.sel, Sel::Repository(0));

        // ctrl-f / ctrl-b = full page
        app.on_key(key(KeyCode::Char('f'), true));
        assert_eq!(
            app.sel,
            Sel::Repository(19),
            "full page down clamps to last"
        );
        app.on_key(key(KeyCode::Char('b'), true));
        assert_eq!(app.sel, Sel::Repository(0));

        // Home / End = g / G
        app.on_key(key(KeyCode::End, false));
        assert_eq!(app.sel, Sel::Repository(19));
        app.on_key(key(KeyCode::Home, false));
        assert_eq!(app.sel, Sel::Repository(0));
        app.on_key(key(KeyCode::Char('G'), false));
        assert_eq!(app.sel, Sel::Repository(19));
        app.on_key(key(KeyCode::Char('g'), false));
        assert_eq!(app.sel, Sel::Repository(0));
    }

    /// `n` opens the popup with the repository's own agent already in the
    /// agent field: the common case stays "name, Enter", and the field
    /// says what that Enter will start.
    #[test]
    fn the_new_worktree_popup_prefills_the_repository_agent() {
        let mut app = app_with_repositories(1);
        app.open_new_worktree();
        let Mode::Input {
            agent: Some(a),
            text,
            ..
        } = &app.mode
        else {
            panic!("the new-worktree popup carries an agent field");
        };
        assert_eq!(a.text, "claude", "prefilled with what the repository runs");
        assert!(!a.focused, "the name field has the keys first");
        assert!(text.is_empty());
    }

    /// The two-field flow: type the name, Tab, override the agent. What is
    /// typed after Tab must land in the agent field, not in the name.
    #[test]
    fn tab_moves_the_typing_to_the_agent_field() {
        let mut app = app_with_repositories(1);
        app.open_new_worktree();
        for c in "wt".chars() {
            app.on_key(key(KeyCode::Char(c), false));
        }
        app.on_key(key(KeyCode::Tab, false));
        // the prefill is replaced, not appended to
        for _ in 0..6 {
            app.on_key(key(KeyCode::Backspace, false));
        }
        for c in "codex".chars() {
            app.on_key(key(KeyCode::Char(c), false));
        }
        let Mode::Input {
            text,
            agent: Some(a),
            ..
        } = &app.mode
        else {
            panic!("popup stays open");
        };
        assert_eq!(text, "wt", "the name keeps what was typed before Tab");
        assert!(a.focused);
        assert_eq!(a.text, "codex");
        // ...and Tab goes back
        app.on_key(key(KeyCode::Tab, false));
        let Mode::Input { agent: Some(a), .. } = &app.mode else {
            panic!("popup stays open");
        };
        assert!(!a.focused, "Tab moves between the two fields, both ways");
    }

    /// The agent field is a short list, so the arrows walk the configured
    /// presets instead of asking for the name to be typed out.
    #[test]
    fn the_arrows_cycle_the_configured_agents_in_the_agent_field() {
        let mut app = app_with_repositories(1);
        app.open_new_worktree();
        app.on_key(key(KeyCode::Tab, false));
        app.on_key(key(KeyCode::Down, false));
        let Mode::Input { agent: Some(a), .. } = &app.mode else {
            panic!("popup stays open");
        };
        assert_eq!(a.text, "codex", "down walks claude → codex");
        app.on_key(key(KeyCode::Down, false));
        let Mode::Input { agent: Some(a), .. } = &app.mode else {
            panic!("popup stays open");
        };
        assert_eq!(a.text, "claude", "and wraps around");
    }

    /// Cycling belongs to the agent field alone: on the name field the
    /// arrows stay as inert as they were before there was a second field.
    #[test]
    fn the_arrows_leave_the_agent_alone_from_the_name_field() {
        let mut app = app_with_repositories(1);
        app.open_new_worktree();
        app.on_key(key(KeyCode::Down, false));
        let Mode::Input { agent: Some(a), .. } = &app.mode else {
            panic!("popup stays open");
        };
        assert_eq!(a.text, "claude", "the unfocused field is not cycled");
    }

    /// The ctrl/accessibility layer dispatches ABOVE text input, so
    /// ctrl-d / PageDown still move the tree while a popup is open — and
    /// ctrl-c quits from any state (it used to type a 'c' into the input).
    #[test]
    fn ctrl_layer_works_over_text_input() {
        let mut app = app_with_repositories(20);
        app.viewport = (120, 40);
        app.mode = Mode::Input {
            text: "abc".into(),
            agent: None,
        };
        app.on_key(key(KeyCode::Char('d'), true));
        assert_eq!(
            app.sel,
            Sel::Repository(18),
            "ctrl-d moves behind the popup"
        );
        let Mode::Input { text, .. } = &app.mode else {
            panic!("input popup stays open");
        };
        assert_eq!(text, "abc", "no stray 'd' typed");

        app.on_key(key(KeyCode::Char('c'), true));
        assert!(app.quit, "ctrl-c quits from an input popup");
    }

    /// `g`/`G` are motions in Normal mode but printable keys in a prompt.
    #[test]
    fn g_and_g_respect_text_input() {
        let mut app = app_with_repositories(5);
        app.on_key(key(KeyCode::Char('G'), false));
        assert_eq!(app.sel, Sel::Repository(4));
        app.mode = Mode::Input {
            text: String::new(),
            agent: None,
        };
        app.on_key(key(KeyCode::Char('g'), false));
        let Mode::Input { text, .. } = &app.mode else {
            panic!()
        };
        assert_eq!(text, "g");
    }

    // ---- M9.2 — multiline text buffer ----

    #[test]
    fn textbuf_types_and_edits_at_cursor() {
        let mut buf = TextBuf::new();
        // insert "hello_world" (11 chars, no space)
        for c in "hello_world".chars() {
            buf.insert_char(c);
        }
        assert_eq!(buf.text(), "hello_world");
        assert_eq!(buf.cursor_row(), 0);
        assert_eq!(buf.cursor_col(), 11);

        // move to the middle (col 5 = after "hello"), insert a tab
        for _ in 0..6 {
            buf.move_left();
        }
        assert_eq!(buf.cursor_col(), 5);
        buf.insert_tab();
        assert_eq!(buf.text(), "hello\t_world");
        assert_eq!(buf.cursor_col(), 6);

        // split the line at the tab: "hello\t" / "_world"
        buf.newline();
        assert_eq!(buf.text(), "hello\t\n_world");
        assert_eq!(buf.cursor_row(), 1);
        assert_eq!(buf.cursor_col(), 0);

        // type at the start of the new line
        buf.insert_char('!');
        assert_eq!(buf.text(), "hello\t\n!_world");
        assert_eq!(buf.cursor_row(), 1);
        assert_eq!(buf.cursor_col(), 1);

        // up: column 1 fits in "hello\t" (6 chars) → stays 1
        buf.move_up();
        assert_eq!(buf.cursor_row(), 0);
        assert_eq!(buf.cursor_col(), 1);
        // down: clamp to line 1's length
        buf.move_down();
        assert_eq!(buf.cursor_row(), 1);
        assert_eq!(buf.cursor_col(), 1);
        // left at line start wraps to the previous line's end
        buf.move_left();
        assert_eq!(buf.cursor_row(), 1, "col 1 → col 0 on the same line");
        assert_eq!(buf.cursor_col(), 0);
        buf.move_left();
        assert_eq!(buf.cursor_row(), 0, "line start wraps up");
        assert_eq!(buf.cursor_col(), 6);
        // right at line end wraps to the next line's start
        buf.move_right();
        assert_eq!(buf.cursor_row(), 1);
        assert_eq!(buf.cursor_col(), 0);

        // backspace at line start joins the lines
        buf.backspace();
        assert_eq!(buf.text(), "hello\t!_world");
        assert_eq!(buf.cursor_row(), 0);
        assert_eq!(buf.cursor_col(), 6);

        // delete forward removes under the cursor ("!")
        buf.delete();
        assert_eq!(buf.text(), "hello\t_world");
        // move to the end, delete = no-op
        buf.move_end();
        buf.delete();
        assert_eq!(buf.text(), "hello\t_world");
        // home + delete removes the first char
        buf.move_home();
        buf.delete();
        assert_eq!(buf.text(), "ello\t_world");

        // home/end insert at the extremes
        buf.move_end();
        buf.insert_char('X');
        assert_eq!(buf.text(), "ello\t_worldX");
        buf.move_home();
        buf.insert_char('S');
        assert_eq!(buf.text(), "Sello\t_worldX");
    }

    #[test]
    fn textbuf_scroll_and_clamp() {
        let mut buf = TextBuf::new();
        for _ in 0..10 {
            buf.newline();
            for c in "line".chars() {
                buf.insert_char(c);
            }
        }
        // 11 lines: row 0..=10
        assert_eq!(buf.line_count(), 11);
        // cursor trails to the last line after the newlines
        assert_eq!(buf.cursor_row(), 10);

        // scrolled to the bottom by construction (ensure_visible)
        // moving up pages keeps the cursor on screen
        let moved = buf.move_page(-1, 5);
        assert!(moved);
        assert!(buf.cursor_row() <= buf.scroll() + 5);
    }

    /// Long lines soft-wrap instead of overflowing the popup border:
    /// the segments cover the line exactly, none is wider than the wrap
    /// width, and the break lands after a space where one exists.
    #[test]
    fn textbuf_wraps_long_lines_at_spaces() {
        let line = "the quick brown fox jumps over the lazy dog";
        let segs = wrap_line(line, 10);
        let joined: String = segs.iter().map(|&(s, e)| &line[s..e]).collect();
        assert_eq!(joined, line, "the segments cover the line exactly");
        for &(s, e) in &segs {
            assert!(
                display_width(&line[s..e]) <= 10,
                "segment {:?} is wider than 10",
                &line[s..e]
            );
        }
        assert_eq!(&line[segs[0].0..segs[0].1], "the quick ");

        // A word longer than the width hard-breaks rather than overflow.
        let long = "aaaaaaaaaaaaaaa";
        let segs = wrap_line(long, 6);
        assert_eq!(segs.len(), 3);
        assert!(segs.iter().all(|&(s, e)| e - s <= 6));

        // An empty line still occupies exactly one visual row.
        assert_eq!(wrap_line("", 10), vec![(0, 0)]);
    }

    /// The cursor follows the wrap: past the break it renders on the next
    /// visual row, at the column it occupies inside that row.
    #[test]
    fn textbuf_visual_cursor_follows_the_wrap() {
        let mut buf = TextBuf::new();
        for c in "the quick brown fox".chars() {
            buf.insert_char(c);
        }
        // "the quick " / "brown fox" at width 10; the cursor sits at the
        // end of the buffer, i.e. row 1, column 9.
        assert_eq!(buf.visual_len(10), 2);
        assert_eq!(buf.visual_cursor(10), (1, 9));

        buf.move_home();
        assert_eq!(buf.visual_cursor(10), (0, 0));
        // Column 10 is the start of the second visual row, not the end
        // of the first.
        for _ in 0..10 {
            buf.move_right();
        }
        assert_eq!(buf.visual_cursor(10), (1, 0));
    }

    /// Scrolling counts visual rows: one wrapped line can fill the whole
    /// window on its own.
    #[test]
    fn textbuf_scrolls_by_visual_rows() {
        let mut buf = TextBuf::new();
        for _ in 0..12 {
            for c in "wrap me ".chars() {
                buf.insert_char(c);
            }
        }
        // One logical line, many visual rows.
        assert_eq!(buf.line_count(), 1);
        let visible = 4;
        let total = buf.visual_len(10);
        assert!(
            total > visible,
            "{total} visual rows must exceed the window"
        );
        buf.ensure_visible(visible, 10);
        assert_eq!(
            buf.scroll(),
            total - visible,
            "the window ends on the cursor's visual row"
        );
        buf.move_home();
        buf.ensure_visible(visible, 10);
        assert_eq!(buf.scroll(), 0, "Home scrolls back to the first row");
    }

    // ---- M8.4a — mouse ----

    fn click(x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Click selects the row under the cursor; double-click opens it.
    #[test]
    fn mouse_click_selects_row_and_double_click_opens() {
        let mut app = app_with_repositories(5);
        app.viewport = (120, 40);
        // row 3 in the tree (below the top border), inside the tree column
        app.on_mouse(click(5, 4));
        assert_eq!(app.sel, Sel::Repository(3));
        // double-click on the same row toggles expansion (repositories start
        // expanded, so the toggle collapses it)
        app.on_mouse(click(5, 4));
        assert!(
            !app.repo_expanded[3],
            "double-click must toggle the repository expansion"
        );
    }

    /// Regression: the event loop must route a mouse event to `on_mouse`.
    /// It used to `read()` inside a `let Event::Key(..)` chain, which
    /// consumed mouse events and then dropped them on the floor, so every
    /// click was silently lost.
    #[test]
    fn event_dispatch_routes_mouse_events() {
        let mut app = app_with_repositories(5);
        app.viewport = (120, 40);
        dispatch_event(&mut app, Event::Mouse(click(5, 4)));
        assert_eq!(
            app.sel,
            Sel::Repository(3),
            "a mouse event must reach on_mouse through the event loop"
        );
    }

    fn wheel(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// An app standing on a shell whose pane captured `n` numbered lines.
    /// A second repository keeps the tree out of the single-repository flat
    /// layout, so there is another row to move the selection to.
    fn app_with_preview(n: usize) -> App {
        let mut app = app_with_shell();
        app.state.repositories.push(Repository {
            name: "p2".into(),
            path: PathBuf::from("/tmp/p2"),
            missing: false,
            shell: None,
            worktrees: vec![],
        });
        app.build_tree_flags();
        app.sel = Sel::Worktree(0, 0);
        app.clamp_sel();
        app.viewport = (120, 40);
        app.captures
            .insert("%1".into(), (1..=n).map(|i| format!("l{i}\n")).collect());
        app
    }

    /// The wheel belongs to the preview, not the tree: it parks the preview
    /// in its scrollback and leaves the selection exactly where it was.
    /// Scrolling back down to the live tail resumes tailing.
    #[test]
    fn mouse_wheel_scrolls_the_preview_never_the_tree() {
        let mut app = app_with_preview(200);
        let before = app.sel;

        app.on_mouse(wheel(MouseEventKind::ScrollUp));
        assert_eq!(
            app.preview_scroll, 3,
            "the wheel parks the preview 3 lines back"
        );
        assert_eq!(app.sel, before, "the tree never moves under the wheel");

        app.on_mouse(wheel(MouseEventKind::ScrollDown));
        assert_eq!(
            app.preview_scroll, 0,
            "scrolling down resumes the live tail"
        );
        assert!(
            app.preview_buf.is_none(),
            "the deep buffer is dropped once the preview is live again"
        );
        assert_eq!(app.sel, before);
    }

    /// The wheel stops at both ends of the buffer rather than running off:
    /// at the live tail scrolling down is a no-op, and scrolling up parks at
    /// the oldest line the capture holds.
    #[test]
    fn preview_scroll_clamps_at_both_ends() {
        let mut app = app_with_preview(200);
        app.on_mouse(wheel(MouseEventKind::ScrollDown));
        assert_eq!(
            app.preview_scroll, 0,
            "already live; nothing below the tail"
        );

        for _ in 0..100 {
            app.on_mouse(wheel(MouseEventKind::ScrollUp));
        }
        // the preview panel is the frame minus the statusbar and its borders
        let inner = app.viewport.1 as usize - 3;
        assert_eq!(
            app.preview_scroll,
            200 - inner,
            "parked on the oldest line the capture holds"
        );
    }

    /// Esc is innermost-first: while the preview is parked in its
    /// scrollback it drops back to the live tail rather than quitting the
    /// dashboard. At the live tail Esc still quits.
    #[test]
    fn esc_resumes_the_live_tail_before_it_quits() {
        let mut app = app_with_preview(200);
        app.on_mouse(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.preview_offset(), 3);

        app.on_key(key(KeyCode::Esc, false));
        assert!(!app.quit, "Esc leaves the parked preview, not the app");
        assert_eq!(app.preview_offset(), 0, "back to the live tail");

        app.on_key(key(KeyCode::Esc, false));
        assert!(app.quit, "Esc at the live tail still quits");
    }

    /// `q` is the unconditional quit — a parked preview never holds it up.
    #[test]
    fn q_quits_even_while_the_preview_is_parked() {
        let mut app = app_with_preview(200);
        app.on_mouse(wheel(MouseEventKind::ScrollUp));
        app.on_key(key(KeyCode::Char('q'), false));
        assert!(app.quit);
    }

    /// The preview follows the tree: selecting another row reads as live
    /// again, so a parked view never outlives the shell it was taken from.
    #[test]
    fn moving_the_selection_snaps_the_preview_back_to_live() {
        let mut app = app_with_preview(200);
        app.on_mouse(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.preview_offset(), 3);

        app.on_key(key(KeyCode::Char('k'), false));
        assert_ne!(app.sel, Sel::Worktree(0, 0), "the motion moved the tree");
        assert_eq!(
            app.preview_offset(),
            0,
            "another row is another pane: the preview is live again"
        );
    }

    /// A preview that is not on screen cannot be parked: below the split
    /// tier the panel is gone, and an `Esc` that quietly refused to quit
    /// would have nothing on screen to explain itself.
    #[test]
    fn a_hidden_preview_is_never_parked() {
        let mut app = app_with_preview(200);
        app.on_mouse(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.preview_offset(), 3);

        app.viewport = (30, 20); // tree-only tier: no preview panel
        assert_eq!(app.preview_offset(), 0, "no panel, nothing parked");
        app.on_key(key(KeyCode::Esc, false));
        assert!(
            app.quit,
            "Esc quits when there is no parked preview to leave"
        );
    }

    /// Clicks outside the tree column do nothing.
    #[test]
    fn clicks_on_the_preview_panel_are_dead() {
        let mut app = app_with_repositories(10);
        app.viewport = (120, 40);
        let before = app.sel;
        app.on_mouse(click(60, 5)); // preview panel area
        assert_eq!(app.sel, before, "the preview panel is not selectable");
    }

    /// Clicking a statusbar shortcut entry runs its action (a context bar
    /// is clickable — discoverability rule).
    #[test]
    fn mouse_click_on_bar_entry_runs_action() {
        let mut app = app_with_repositories(3);
        app.viewport = (120, 40);
        // render once so the renderer computes the bar hit ranges
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert!(!app.bar_hits.is_empty());
        assert_eq!(app.sel, Sel::Repository(0));
        // a repository's bar has "n worktree" → the new-worktree input
        let (x0, x1, y, _) = app
            .bar_hits
            .iter()
            .find(|(_, _, _, a)| *a == BarAction::NewWorktree)
            .copied()
            .expect("bar must offer 'n worktree' for a repository");
        app.on_mouse(click((x0 + x1) / 2, y));
        assert!(
            matches!(app.mode, Mode::Input { .. }),
            "clicking 'n worktree' must open the input"
        );
    }

    /// Clicking outside an open overlay dismisses it; clicking inside does
    /// not.
    #[test]
    fn mouse_click_outside_overlay_dismisses_it() {
        let mut app = app_with_repositories(3);
        app.viewport = (120, 40);
        app.mode = Mode::Help;
        app.popup_area = Some(Rect::new(20, 5, 70, 20));
        app.on_mouse(click(60, 10)); // inside
        assert!(matches!(app.mode, Mode::Help), "inside click keeps it open");
        app.on_mouse(click(2, 2)); // outside → dismiss
        assert!(matches!(app.mode, Mode::Normal));
    }

    /// A missing popup_area (too-small fallback) means any click dismisses.
    #[test]
    fn overlay_dismisses_when_no_popup_area_known() {
        let mut app = app_with_repositories(3);
        app.viewport = (20, 5);
        app.mode = Mode::Help;
        app.popup_area = None;
        app.on_mouse(click(10, 2));
        assert!(matches!(app.mode, Mode::Normal));
    }

    // ---- M8.4b — grouped help overlay (palette) scrolling ----

    /// Build a help overlay with a known popup rect so scroll math is
    /// deterministic (the popup's inner height comes from popup_area).
    fn app_with_help(n: usize) -> App {
        let mut app = app_with_repositories(n);
        app.open_help();
        app.popup_area = Some(Rect::new(20, 5, 70, 30)); // 28 visible rows
        app
    }

    /// j/k and arrows scroll the help overlay one line at a time and never
    /// move the tree selection; the overlay owns the viewport while open.
    #[test]
    fn help_scrolls_with_jk_and_arrows_but_keeps_selection() {
        let mut app = app_with_help(20);
        let before = app.sel;
        app.on_key(key(KeyCode::Char('j'), false));
        assert_eq!(app.help_scroll, 1);
        app.on_key(key(KeyCode::Down, false)); // arrows = same code path
        assert_eq!(app.help_scroll, 2);
        app.on_key(key(KeyCode::Char('k'), false));
        assert_eq!(app.help_scroll, 1);
        app.on_key(key(KeyCode::Up, false));
        assert_eq!(app.help_scroll, 0);
        assert_eq!(app.sel, before, "help scrolling must not move the tree");
    }

    /// g/G and Home/End jump the help to first/last; PageDown/ctrl-d scroll
    /// half a page; all clamp at the ends (never underflow, never past the
    /// last line).
    #[test]
    fn help_palette_motions_jump_scroll_and_clamp() {
        let mut app = app_with_help(3);
        // scroll a long way down — the palette reflows for a narrow strip,
        // so it is many more lines than it is rows
        for _ in 0..500 {
            app.on_key(key(KeyCode::Char('j'), false));
        }
        let max_scroll = app.help_scroll;
        assert!(max_scroll > 0, "help content must overflow 28 rows");
        // PageDown = ctrl-d = half page; from the bottom it clamps
        app.on_key(key(KeyCode::PageDown, false));
        assert_eq!(app.help_scroll, max_scroll, "clamped at the bottom");
        assert!(matches!(app.mode, Mode::Help), "still in help");

        // Home / End and g / G jump to first / last
        app.on_key(key(KeyCode::Home, false));
        assert_eq!(app.help_scroll, 0);
        app.on_key(key(KeyCode::End, false));
        assert_eq!(app.help_scroll, max_scroll);
        app.on_key(key(KeyCode::Char('g'), false));
        assert_eq!(app.help_scroll, 0);
        app.on_key(key(KeyCode::Char('G'), false));
        assert_eq!(app.help_scroll, max_scroll);

        // dark-climb from the top must not underflow
        app.on_key(key(KeyCode::Home, false));
        app.on_key(key(KeyCode::Char('k'), false));
        assert_eq!(app.help_scroll, 0);
    }

    /// In Normal mode the same key code paths still move the tree (the
    /// routing is mode-dependent, not key-dependent).
    #[test]
    fn page_motions_still_move_tree_outside_help() {
        let mut app = app_with_repositories(20);
        app.viewport = (120, 40);
        app.on_key(key(KeyCode::PageDown, false));
        assert_eq!(app.sel, Sel::Repository(18));
    }

    /// The mouse wheel scrolls the help overlay while it is open, and the
    /// tree when it is not.
    #[test]
    fn mouse_wheel_scrolls_help_or_tree_by_mode() {
        let mut app = app_with_help(20);
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 40,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.help_scroll, 3, "wheel scrolls the help");
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 40,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.help_scroll, 0);
    }

    // ---- M8.4c — theme picker ----

    /// A helper that opens the picker on a headless app standing on the
    /// given theme (defaults to the config default when unset).
    fn app_with_picker(theme_name: &str) -> App {
        let mut app = app_with_repositories(3);
        if theme_name != app.theme.name {
            app.theme = theme::load(theme_name, false).unwrap();
        }
        app.open_theme_picker();
        app
    }

    /// ctrl-t opens the picker on the CURRENT theme with built-in names
    /// (Catppuccin first, per the theming rule that the first rows are the
    /// same in every repository); the cursor sits on the active theme.
    #[test]
    fn ctrl_t_opens_picker_on_current_theme() {
        let mut app = app_with_picker("catppuccin-mocha");
        app.on_key(key(KeyCode::Char('t'), true));
        let Mode::ThemePicker { index } = app.mode else {
            panic!("ctrl-t must open the picker");
        };
        assert_eq!(app.theme_names[index], "catppuccin-mocha");
        assert_eq!(
            app.theme_names[0], "catppuccin-mocha",
            "built-ins first, in the picker's stable order"
        );
        assert!(
            app.theme_names.contains(&"ansi".to_string()),
            "legacy built-ins still offered"
        );
        assert_eq!(
            app.picker_original, "catppuccin-mocha",
            "the theme the picker opened with is remembered for Esc"
        );
    }

    /// j/k move the cursor and PREVIEW live (the app's theme follows the
    /// cursor); g/G and End/Home jump to first/last; everything clamps.
    #[test]
    fn picker_cursor_previews_and_clamps() {
        let mut app = app_with_picker("catppuccin-mocha");
        let start = match &app.mode {
            Mode::ThemePicker { index } => *index,
            _ => panic!(),
        };
        app.on_key(key(KeyCode::Char('j'), false));
        let Mode::ThemePicker { index } = app.mode else {
            panic!("picker must still be open");
        };
        assert_eq!(index, start + 1);
        assert_eq!(
            app.theme.name,
            app.theme_names[start + 1],
            "cursor move previews the theme live"
        );
        // up again returns to the original
        app.on_key(key(KeyCode::Up, false)); // arrows = same code path
        assert_eq!(app.theme.name, "catppuccin-mocha");

        // End = last theme, previewing it; Home = back to first
        app.on_key(key(KeyCode::End, false));
        let n = app.theme_names.len();
        assert_eq!(app.theme.name, app.theme_names[n - 1], "End previews last");
        app.on_key(key(KeyCode::Home, false));
        assert_eq!(app.theme.name, app.theme_names[0]);

        // clamping: j from the last row stays there
        app.on_key(key(KeyCode::End, false));
        app.on_key(key(KeyCode::Char('j'), false));
        let Mode::ThemePicker { index } = app.mode else {
            panic!("picker must still be open");
        };
        assert_eq!(index, n - 1, "no overflow past the last theme");
    }

    /// Enter keeps the previewed theme and writes it to the user config,
    /// which the next start reads.
    #[test]
    fn picker_enter_keeps_and_persists_theme() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_picker("catppuccin-mocha");
            app.on_key(key(KeyCode::Char('j'), false)); // preview a different one
            let picked = app.theme.name.clone();
            app.on_key(key(KeyCode::Enter, false));
            assert!(matches!(app.mode, Mode::Normal), "picker closes on Enter");
            assert_eq!(app.theme.name, picked);
            assert_eq!(app.cfg.theme, picked);
            let cwd = tempfile::TempDir::new().unwrap();
            let loaded = crate::config::Config::load_from(cwd.path(), None).unwrap();
            assert_eq!(loaded.theme, picked);
        });
    }

    /// Esc restores the theme the picker opened with, even after previews,
    /// and does not persist anything.
    #[test]
    fn picker_esc_restores_original() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_picker("catppuccin-mocha");
            let configured = app.cfg.theme.clone();
            app.on_key(key(KeyCode::Char('j'), false));
            assert_ne!(app.theme.name, "catppuccin-mocha");
            app.on_key(key(KeyCode::Esc, false));
            assert_eq!(app.theme.name, "catppuccin-mocha", "Esc restores the theme");
            assert!(matches!(app.mode, Mode::Normal));
            assert_eq!(app.cfg.theme, configured, "nothing kept on cancel");
            assert!(
                !crate::util::config_path().exists(),
                "nothing written on cancel"
            );
            assert!(app.picker_original.is_empty(), "restore consumed the name");
        });
    }

    /// PageDown/ctrl-d move the picker by a page (5), g/G jump to
    /// first/last — the accessibility layer routes to the picker, not the
    /// tree, while it is open.
    #[test]
    fn picker_page_and_g_motions_route_to_picker() {
        let mut app = app_with_picker("catppuccin-mocha");
        app.on_key(key(KeyCode::PageDown, false));
        let Mode::ThemePicker { index } = app.mode else {
            panic!("picker must still be open");
        };
        assert_eq!(index, 5, "page down = 5 rows in the picker");
        app.on_key(key(KeyCode::Char('G'), false));
        let Mode::ThemePicker { index } = app.mode else {
            panic!("picker must still be open");
        };
        assert_eq!(index, app.theme_names.len() - 1);
        app.on_key(key(KeyCode::Char('g'), false));
        let Mode::ThemePicker { index } = app.mode else {
            panic!("picker must still be open");
        };
        assert_eq!(index, 0);
        // the tree selection never moved while the picker was open
        assert_eq!(app.sel, Sel::Repository(0));
    }

    /// Mouse: clicking a theme row selects + previews it, double-click
    /// keeps it, click outside restores the original (Esc semantics).
    #[test]
    fn picker_mouse_selects_and_double_click_keeps() {
        // Keeping writes the user config.
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_picker("catppuccin-mocha");
            let rect = Rect::new(20, 5, 40, app.theme_names.len() as u16 + 3);
            app.popup_area = Some(rect);
            // single click row 1 → select + preview (not kept)
            app.on_mouse(click(rect.x + 5, rect.y + 1 + 1));
            let Mode::ThemePicker { index } = app.mode else {
                panic!("picker must still be open after a single click");
            };
            assert_eq!(index, 1);
            assert_eq!(app.theme.name, app.theme_names[1]);
            // double-click the same row keeps it
            app.on_mouse(click(rect.x + 5, rect.y + 1 + 1));
            assert!(matches!(app.mode, Mode::Normal));
            assert_eq!(app.theme.name, app.theme_names[1]);
            assert_eq!(app.cfg.theme, app.theme_names[1]);
        });
    }

    /// Click outside the picker = Esc: restores the original theme.
    #[test]
    fn picker_click_outside_restores_original() {
        let mut app = app_with_picker("catppuccin-mocha");
        app.popup_area = Some(Rect::new(20, 5, 40, 12));
        app.on_key(key(KeyCode::Char('j'), false));
        assert_ne!(app.theme.name, "catppuccin-mocha");
        app.on_mouse(click(2, 2));
        assert_eq!(app.theme.name, "catppuccin-mocha", "outside click restores");
        assert!(matches!(app.mode, Mode::Normal));
    }

    /// The wheel moves the picker cursor when the picker is open.
    #[test]
    fn picker_wheel_moves_cursor() {
        let mut app = app_with_picker("catppuccin-mocha");
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 40,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        let Mode::ThemePicker { index } = app.mode else {
            panic!("picker must still be open");
        };
        assert_eq!(index, 3, "wheel moves the picker by 3");
    }

    // ------------------------------------------- M8.4d: diagnostics overlay

    fn app_with_diagnostics(n: usize) -> App {
        let mut cfg = Config::default();
        for i in 0..n {
            cfg.diagnostics.push(format!("diagnostic number {i}"));
        }
        let mut app = App::new(cfg, theme::load("ansi", false).unwrap(), None);
        // an overlay is on screen once drawn: give it a tall popup
        app.popup_area = Some(Rect::new(10, 4, 60, 12));
        app
    }

    /// The startup diagnostics overlay opens when the config carried
    /// diagnostics, and dismisses back to Normal (there is no parked
    /// confirm anymore — discovery is automatic).
    #[test]
    fn startup_diagnostics_overlay_opens_and_dismisses() {
        let mut app = app_with_diagnostics(3);
        app.open_startup_diagnostics();
        assert!(matches!(app.mode, Mode::Diagnostics));
        // dismiss → Normal (boot continues; discovery already ran)
        app.on_key(key(KeyCode::Esc, false));
        assert!(matches!(app.mode, Mode::Normal), "dismiss lands in Normal");
    }

    /// No diagnostics → the overlay never opens (not even an empty one).
    #[test]
    fn startup_diagnostics_skipped_when_empty() {
        let mut app = app_with_diagnostics(0);
        app.open_startup_diagnostics();
        assert!(matches!(app.mode, Mode::Normal));
    }

    /// q / Esc / Enter dismiss; j/k/arrows/g/G scroll with the same clamp
    /// as the help palette; the tree selection never moves.
    #[test]
    fn diagnostics_keys_scroll_and_dismiss() {
        let mut app = app_with_diagnostics(20); // 20 diags + footer in a 10-row popup
        app.mode = Mode::Diagnostics;
        app.sel = Sel::Repository(0);
        // j steps down, clamped by the popup height
        app.on_key(key(KeyCode::Char('j'), false));
        assert_eq!(app.diag_scroll, 1);
        app.on_key(key(KeyCode::Char('j'), false));
        assert_eq!(app.diag_scroll, 2);
        app.on_key(key(KeyCode::Char('k'), false));
        assert_eq!(app.diag_scroll, 1);
        // g/G jump to first/last
        app.on_key(key(KeyCode::Char('g'), false));
        assert_eq!(app.diag_scroll, 0);
        app.on_key(key(KeyCode::Char('G'), false));
        let total = crate::ui::diag_line_count(&app);
        let visible = crate::ui::overlay_body_height(app.popup_area);
        assert_eq!(app.diag_scroll, total.saturating_sub(visible));
        // the tree selection never moved while the overlay was open
        assert_eq!(app.sel, Sel::Repository(0));
        // dismiss keys
        for k in [KeyCode::Char('q'), KeyCode::Enter, KeyCode::Char('?')] {
            app.mode = Mode::Diagnostics;
            app.on_key(key(k, false));
            assert!(matches!(app.mode, Mode::Normal), "{k:?} dismisses");
        }
    }

    /// PageUp/PageDown/ctrl-u/d/Home/End route to the diagnostics overlay
    /// while it is open (same accessibility path as help/picker).
    #[test]
    fn diagnostics_a11y_motions_scroll_overlay_not_tree() {
        let mut app = app_with_diagnostics(30);
        app.mode = Mode::Diagnostics;
        app.sel = Sel::Repository(0);
        let before = app.sel;
        app.on_key(key(KeyCode::PageDown, false));
        assert!(app.diag_scroll > 0, "PageDown scrolls the diagnostics");
        app.on_key(key(KeyCode::Home, false));
        assert_eq!(app.diag_scroll, 0);
        app.on_key(key(KeyCode::End, false));
        let total = crate::ui::diag_line_count(&app);
        let visible = crate::ui::overlay_body_height(app.popup_area);
        assert_eq!(app.diag_scroll, total.saturating_sub(visible));
        assert_eq!(app.sel, before, "tree selection untouched");
    }

    /// The wheel scrolls the diagnostics by 3 lines.
    #[test]
    fn diagnostics_wheel_scrolls() {
        let mut app = app_with_diagnostics(20);
        app.mode = Mode::Diagnostics;
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 40,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.diag_scroll, 3);
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 40,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.diag_scroll, 0);
    }

    /// Clicking outside the diagnostics overlay dismisses it.
    /// Classification runs on plain text: a capture whose status line is
    /// coloured (every agent colours its spinner) must classify exactly like
    /// the uncoloured one, and the trailing pad lines must not push the real
    /// output out of the recent window.
    #[test]
    fn recent_tail_strips_colours_and_padding() {
        let coloured = "banner\n\x1b[33m⠋ Thinking…\x1b[0m\n\x1b[49m\n\n";
        assert_eq!(recent_tail(coloured), "banner\n⠋ Thinking…");
        assert_eq!(
            recent_tail("banner\n⠋ Thinking…\n\n"),
            recent_tail(coloured)
        );

        let pat = crate::status::Compiled::compile(&crate::config::generic_patterns());
        let pane = PaneInfo {
            pane_id: "%1".into(),
            current_command: "node".into(),
            dead: false,
            overlay_raw: String::new(),
            ..Default::default()
        };
        assert_eq!(
            crate::status::classify(&pat, &recent_tail(coloured), Some(&pane)),
            crate::status::classify(&pat, "⠋ Thinking…", Some(&pane)),
        );

        // only the last RECENT_TAIL_LINES lines survive
        let long: String = (1..=40)
            .map(|i| format!("\x1b[32mline-{i}\x1b[0m\n"))
            .collect();
        let tail = recent_tail(&long);
        assert_eq!(tail.lines().count(), RECENT_TAIL_LINES);
        assert!(tail.ends_with("line-40"));
        assert!(!tail.contains('\x1b'));
    }

    #[test]
    fn diagnostics_click_outside_dismisses() {
        let mut app = app_with_diagnostics(2);
        app.mode = Mode::Diagnostics;
        app.popup_area = Some(Rect::new(20, 5, 40, 10));
        app.on_mouse(click(2, 2)); // outside the popup
        assert!(matches!(app.mode, Mode::Normal), "outside click dismisses");
        // ...and clicking INSIDE keeps the overlay open
        app.mode = Mode::Diagnostics;
        app.on_mouse(click(22, 6));
        assert!(matches!(app.mode, Mode::Diagnostics));
    }

    // ---- M11 — predefined messages (`p`) ----

    /// `p` on a shell opens the predefined-message modal; anywhere else it
    /// says what to select instead (same shape as `s`).
    #[test]
    fn p_opens_predefined_messages_only_on_a_shell() {
        let mut app = app_with_shell();
        app.sel = Sel::Repository(0);
        app.on_key(key(KeyCode::Char('p'), false));
        assert!(matches!(app.mode, Mode::Normal), "no modal on a repository");
        assert!(
            app.message
                .as_ref()
                .is_some_and(|m| m.text.contains("Select a shell")),
            "a hint is shown instead"
        );

        app.sel = Sel::Shell(0, 0, 0);
        app.on_key(key(KeyCode::Char('p'), false));
        assert!(
            matches!(app.mode, Mode::Messages { index: 0 }),
            "p opens the modal on the first entry"
        );
    }

    /// A folded one-shell worktree row IS its shell (M11), so `p` opens
    /// the modal there exactly like `s` sends from it.
    #[test]
    fn p_opens_from_a_folded_worktree_row() {
        let mut app = app_with_shell();
        app.sel = Sel::Worktree(0, 0);
        assert!(
            app.solo_shell(0, 0).is_some(),
            "the fixture's worktree is folded"
        );
        app.on_key(key(KeyCode::Char('p'), false));
        assert!(
            matches!(app.mode, Mode::Messages { index: 0 }),
            "the folded row opens the modal"
        );
    }

    /// The modal is a list: j/k move the cursor, Esc closes it without
    /// sending anything.
    #[test]
    fn predefined_modal_moves_and_esc_cancels() {
        let mut app = app_with_shell();
        let last = app.cfg.messages.len() - 1;
        app.on_key(key(KeyCode::Char('p'), false));
        app.on_key(key(KeyCode::Char('j'), false));
        assert!(matches!(app.mode, Mode::Messages { index: 1 }));
        // clamped at the end of the list, however long it is
        for _ in 0..app.cfg.messages.len() {
            app.on_key(key(KeyCode::Char('j'), false));
        }
        assert!(matches!(app.mode, Mode::Messages { index } if index == last));
        app.on_key(key(KeyCode::Char('k'), false));
        assert!(matches!(app.mode, Mode::Messages { index } if index == last - 1));
        app.on_key(key(KeyCode::Esc, false));
        assert!(matches!(app.mode, Mode::Normal), "Esc closes the modal");
    }

    /// Without configured messages there is nothing to show — say so
    /// rather than opening an empty box.
    #[test]
    fn p_without_configured_messages_notifies() {
        let mut app = app_with_shell();
        app.cfg.messages.clear();
        app.on_key(key(KeyCode::Char('p'), false));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(
            app.message
                .as_ref()
                .is_some_and(|m| m.text.contains("No predefined messages")),
        );
    }

    /// Enter sends the selected entry to the shell's pane — the same paste
    /// path as `s`, so the agent sees exactly what a human would have typed.
    #[test]
    fn predefined_modal_enter_sends_the_selected_text() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_wid, pid) = seed_repository_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.sel = Sel::Shell(0, 0, 0);
            app.cfg.messages = vec![
                crate::config::MessageConfig {
                    text: "lw-first-message".into(),
                },
                crate::config::MessageConfig {
                    text: "lw-second-message".into(),
                },
            ];

            app.on_key(key(KeyCode::Char('p'), false));
            app.on_key(key(KeyCode::Char('j'), false));
            app.on_key(key(KeyCode::Enter, false));
            assert!(matches!(app.mode, Mode::Normal), "sending closes the modal");

            std::thread::sleep(std::time::Duration::from_millis(300));
            let capture = tmux.capture_pane(&pid, 50).unwrap();
            assert!(
                capture.contains("lw-second-message"),
                "the selected entry was sent, capture: {capture}"
            );
            assert!(
                !capture.contains("lw-first-message"),
                "only the selected entry is sent, capture: {capture}"
            );
        });
    }

    /// Digits are the list's own shortcuts: `2` sends the second entry
    /// without moving the cursor there first.
    #[test]
    fn predefined_modal_digit_sends_that_entry() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_wid, pid) = seed_repository_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.sel = Sel::Shell(0, 0, 0);
            app.cfg.messages = vec![
                crate::config::MessageConfig {
                    text: "lw-digit-one".into(),
                },
                crate::config::MessageConfig {
                    text: "lw-digit-two".into(),
                },
            ];

            app.on_key(key(KeyCode::Char('p'), false));
            app.on_key(key(KeyCode::Char('2'), false));
            assert!(matches!(app.mode, Mode::Normal));

            std::thread::sleep(std::time::Duration::from_millis(300));
            let capture = tmux.capture_pane(&pid, 50).unwrap();
            assert!(
                capture.contains("lw-digit-two"),
                "digit 2 sent the second entry, capture: {capture}"
            );

            // a digit past the end of the list does nothing
            app.on_key(key(KeyCode::Char('p'), false));
            app.on_key(key(KeyCode::Char('9'), false));
            assert!(
                matches!(app.mode, Mode::Messages { .. }),
                "an out-of-range digit leaves the modal open"
            );
        });
    }

    /// A message is a template: the tokens naming the selected shell's
    /// context are resolved from state before the text is shown or sent,
    /// so a preset can say "merge to LOCAL <base_branch>" once and be
    /// right in every project.
    #[test]
    fn expand_message_resolves_the_selected_shells_context() {
        let app = app_with_shell();
        assert_eq!(
            app.expand_message("merge to <base_branch>, drop <branch>"),
            "merge to main, drop wt"
        );
        assert_eq!(
            app.expand_message("<repository>/<worktree> at <worktree_path>"),
            "p/wt at /tmp/p/.wingmux-worktrees/p/wt"
        );
    }

    /// With no base branch configured, `<base_branch>` is the checkout's
    /// branch as the last state poll resolved it — the same branch the
    /// New Worktree popup names.
    #[test]
    fn expand_message_uses_the_resolved_base_without_config() {
        let mut app = app_with_shell();
        app.cfg.worktrees.base_branch = String::new();
        assert_eq!(
            app.expand_message("merge to LOCAL <base_branch>"),
            "merge to LOCAL <base_branch>",
            "nothing resolved yet: the token stays"
        );
        app.base_branches
            .insert(PathBuf::from("/tmp/p"), "trunk".into());
        assert_eq!(
            app.expand_message("merge to LOCAL <base_branch>"),
            "merge to LOCAL trunk"
        );
    }

    /// Only the documented tokens are substituted: anything else is text
    /// the user meant to send, angle brackets and all.
    #[test]
    fn expand_message_leaves_unknown_tokens_alone() {
        let app = app_with_shell();
        assert_eq!(
            app.expand_message("rename <foo> to <base_branch>"),
            "rename <foo> to main"
        );
    }

    /// With nothing selected there is no context to resolve against, so
    /// the template is left as written rather than filled with blanks.
    #[test]
    fn expand_message_without_a_selected_shell_keeps_the_tokens() {
        let mut app = app_with_shell();
        app.sel = Sel::Repository(0);
        assert_eq!(
            app.expand_message("merge to LOCAL <base_branch>"),
            "merge to LOCAL <base_branch>"
        );
    }

    /// What the modal listed is what the pane gets: the send path resolves
    /// the same tokens, so the preset cannot promise "merge to master" and
    /// then paste an unresolved template.
    #[test]
    fn predefined_send_expands_tokens_before_pasting() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_wid, pid) = seed_repository_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.sel = Sel::Shell(0, 0, 0);
            app.cfg.messages = vec![crate::config::MessageConfig {
                text: "lw-merge to <base_branch> now".into(),
            }];

            app.on_key(key(KeyCode::Char('p'), false));
            app.on_key(key(KeyCode::Enter, false));

            std::thread::sleep(std::time::Duration::from_millis(300));
            let capture = tmux.capture_pane(&pid, 50).unwrap();
            assert!(
                capture.contains("lw-merge to main now"),
                "the token is resolved before the paste, capture: {capture}"
            );
            assert!(
                !capture.contains("<base_branch>"),
                "no raw token reaches the agent, capture: {capture}"
            );
        });
    }

    /// Mouse: a click inside the modal selects that row, a double-click
    /// sends it, and a click outside dismisses (Esc semantics).
    #[test]
    fn messages_modal_mouse_selects_and_dismisses() {
        let mut app = app_with_shell();
        app.on_key(key(KeyCode::Char('p'), false));
        let rect = Rect::new(20, 5, 40, app.cfg.messages.len() as u16 + 3);
        app.popup_area = Some(rect);

        // click the second row (border + row index) → selects, stays open
        app.on_mouse(click(rect.x + 5, rect.y + 1 + 1));
        let Mode::Messages { index } = app.mode else {
            panic!("modal must still be open after a single click");
        };
        assert_eq!(index, 1);

        // click outside → dismissed, nothing sent
        app.on_mouse(click(2, 2));
        assert!(matches!(app.mode, Mode::Normal), "outside click dismisses");
    }

    /// The wheel and the page/Home/End motions move the modal cursor
    /// instead of the tree — the overlay owns the viewport while it is up.
    #[test]
    fn messages_modal_owns_the_motion_keys() {
        let mut app = app_with_shell();
        let before = app.sel;
        app.on_key(key(KeyCode::Char('p'), false));

        app.on_key(key(KeyCode::End, false));
        let Mode::Messages { index } = app.mode else {
            panic!("modal must still be open");
        };
        assert_eq!(index, app.cfg.messages.len() - 1, "End = last entry");

        app.on_key(key(KeyCode::Home, false));
        let Mode::Messages { index } = app.mode else {
            panic!("modal must still be open");
        };
        assert_eq!(index, 0, "Home = first entry");

        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 40,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        let Mode::Messages { index } = app.mode else {
            panic!("modal must still be open");
        };
        assert_eq!(index, app.cfg.messages.len() - 1, "wheel moves the cursor");

        assert_eq!(app.sel, before, "the tree never moved");
    }

    // ---- M16 — add-shell picker ----

    /// `a` no longer spawns a shell on the spot, and no longer asks for a
    /// command: it opens a picker of what the shell should run, cursor on
    /// the repository's own agent command. Esc backs out without creating
    /// anything.
    #[test]
    fn a_opens_the_add_shell_picker_instead_of_spawning() {
        let mut app = app_with_shell();
        app.on_key(key(KeyCode::Char('a'), false));
        assert!(
            matches!(app.mode, Mode::AddShell { index: 0 }),
            "`a` must open the picker at its first entry, got {:?}",
            app.mode
        );
        assert_eq!(
            app.state.repositories[0].worktrees[0].shells.len(),
            1,
            "nothing is spawned before an entry is taken"
        );
        app.on_key(key(KeyCode::Esc, false));
        assert!(matches!(app.mode, Mode::Normal), "Esc closes the picker");
        assert_eq!(
            app.state.repositories[0].worktrees[0].shells.len(),
            1,
            "Esc creates no shell"
        );
    }

    /// The picker's first entry is the repository's agent command (Enter
    /// keeps the old one-keystroke path) and its last is a plain login
    /// shell — the entry that exists so starting a normal shell needs no
    /// typing at all.
    #[test]
    fn picker_offers_the_repository_agent_first_and_a_plain_shell_last() {
        let mut app = app_with_shell();
        app.cfg.repositories.push(crate::config::RepositoryConfig {
            path: app.state.repositories[0]
                .path
                .to_string_lossy()
                .into_owned(),
            agent_command: Some("my-agent".into()),
            ..Default::default()
        });
        let choices = app.add_shell_choices();
        assert_eq!(
            choices.first().map(|c| &c.spawn),
            Some(&ShellSpawn::Agent("my-agent".into())),
            "the repository default leads: {choices:?}"
        );
        assert_eq!(
            choices.last().map(|c| &c.spawn),
            Some(&ShellSpawn::Plain),
            "a plain shell is always offered: {choices:?}"
        );
        assert!(
            choices.last().unwrap().label.contains("plain shell"),
            "the plain entry says so: {choices:?}"
        );
    }

    /// Configured agent presets join the list, and a preset that repeats
    /// the repository default does not show up twice.
    #[test]
    fn picker_lists_configured_agents_without_duplicating_the_default() {
        let mut app = app_with_shell();
        app.cfg.repositories.push(crate::config::RepositoryConfig {
            path: app.state.repositories[0]
                .path
                .to_string_lossy()
                .into_owned(),
            agent_command: Some("claude".into()),
            ..Default::default()
        });
        app.cfg.agents = vec![
            crate::config::AgentConfig {
                name: "claude".into(),
                command: "claude".into(),
                ..Default::default()
            },
            crate::config::AgentConfig {
                name: "codex".into(),
                command: "codex".into(),
                ..Default::default()
            },
        ];
        let labels: Vec<String> = app
            .add_shell_choices()
            .into_iter()
            .map(|c| c.label)
            .collect();
        assert_eq!(
            labels.iter().filter(|l| *l == "claude").count(),
            1,
            "the default is not repeated: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l == "codex"),
            "presets join: {labels:?}"
        );
    }

    /// Drive polls until every queued spawn command has been typed, the way
    /// the event loop does — a spawn is owed to a shell that has not drawn
    /// its prompt yet, so nothing lands until a poll finds it ready.
    fn settle_launches(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !app.pending_launches.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            app.poll();
        }
        assert!(
            app.pending_launches.is_empty(),
            "the spawn command never went in"
        );
    }

    /// The spawn command waits for a prompt. Typed into a shell that is
    /// still sourcing its rc files it would be echoed raw by the tty, then
    /// reflowed by the holder split, then drawn again by the line editor —
    /// the garbled first screen this queue exists to prevent.
    #[test]
    fn a_spawn_command_waits_for_the_shell_to_draw_its_prompt() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_wid, pid) = seed_repository_with_shell(&tmux, &mut app);
            app.queue_launch(&pid, "echo lw-late-marker".into());

            app.panes.insert(
                pid.clone(),
                PaneInfo {
                    pane_id: pid.clone(),
                    blank: true,
                    ..Default::default()
                },
            );
            app.drain_pending_launches(&tmux);
            assert_eq!(
                app.pending_launches.len(),
                1,
                "a pane that has drawn nothing is not typed into"
            );
            std::thread::sleep(Duration::from_millis(300));
            let capture = tmux.capture_pane(&pid, 50).unwrap();
            assert!(
                !capture.contains("lw-late-marker"),
                "nothing reached the pane, capture: {capture}"
            );

            app.panes.get_mut(&pid).unwrap().blank = false;
            app.drain_pending_launches(&tmux);
            assert!(app.pending_launches.is_empty(), "the queue is drained");
            std::thread::sleep(Duration::from_millis(300));
            let capture = tmux.capture_pane(&pid, 50).unwrap();
            assert!(
                capture.contains("lw-late-marker"),
                "a drawn prompt takes the command, capture: {capture}"
            );
        });
    }

    /// Enter on the first entry runs the repository's agent command — the
    /// pre-picker default behaviour, one keystroke longer.
    #[test]
    fn picker_enter_runs_the_repository_default() {
        crate::util::testutil::with_fresh_xdg(|| {
            with_tmux_server(|sock, _tmux| {
                let tmux = tmux_inside(sock);
                let mut app = app_with(tmux.clone());
                seed_repository_with_shell(&tmux, &mut app);
                app.cfg.repositories.push(crate::config::RepositoryConfig {
                    path: app.state.repositories[0]
                        .path
                        .to_string_lossy()
                        .into_owned(),
                    agent_command: Some("echo lw-default-marker".into()),
                    ..Default::default()
                });
                app.state.repositories[0].worktrees[0].path = PathBuf::from("/tmp");
                app.build_tree_flags();
                app.sel = Sel::Worktree(0, 0);

                app.on_key(key(KeyCode::Char('a'), false));
                app.on_key(key(KeyCode::Enter, false));
                assert!(matches!(app.mode, Mode::Normal), "Enter closes the picker");

                let shells = &app.state.repositories[0].worktrees[0].shells;
                assert_eq!(shells.len(), 2, "the shell was created");
                let pid = shells[1].pane_id.clone();
                settle_launches(&mut app);
                std::thread::sleep(std::time::Duration::from_millis(300));
                let capture = tmux.capture_pane(&pid, 50).unwrap();
                assert!(
                    capture.contains("lw-default-marker"),
                    "the first entry runs the repository default, capture: {capture}"
                );
            });
        });
    }

    /// The plain-shell entry starts a shell and sends it nothing: the
    /// repository's agent command must not sneak in behind it.
    #[test]
    fn picker_plain_shell_runs_no_agent_command() {
        crate::util::testutil::with_fresh_xdg(|| {
            with_tmux_server(|sock, _tmux| {
                let tmux = tmux_inside(sock);
                let mut app = app_with(tmux.clone());
                seed_repository_with_shell(&tmux, &mut app);
                app.cfg.repositories.push(crate::config::RepositoryConfig {
                    path: app.state.repositories[0]
                        .path
                        .to_string_lossy()
                        .into_owned(),
                    agent_command: Some("echo lw-default-marker".into()),
                    ..Default::default()
                });
                app.state.repositories[0].worktrees[0].path = PathBuf::from("/tmp");
                app.build_tree_flags();
                app.sel = Sel::Worktree(0, 0);

                let last = app.add_shell_choices().len();
                app.on_key(key(KeyCode::Char('a'), false));
                // The rows are numbered from 1, so the last row's digit is
                // its count — the plain shell always sits at the end.
                app.on_key(key(
                    KeyCode::Char(char::from_digit(last as u32, 10).unwrap()),
                    false,
                ));
                assert!(
                    matches!(app.mode, Mode::Normal),
                    "the digit takes the entry"
                );

                let shells = &app.state.repositories[0].worktrees[0].shells;
                assert_eq!(shells.len(), 2, "the shell was created");
                let pid = shells[1].pane_id.clone();
                std::thread::sleep(std::time::Duration::from_millis(300));
                let capture = tmux.capture_pane(&pid, 50).unwrap();
                assert!(
                    !capture.contains("lw-default-marker"),
                    "a plain shell runs no agent, capture: {capture}"
                );
            });
        });
    }

    /// j/k move the picker cursor, clamped at both ends like every other
    /// modal list.
    #[test]
    fn picker_cursor_moves_and_clamps() {
        let mut app = app_with_shell();
        let last = app.add_shell_choices().len() - 1;
        app.on_key(key(KeyCode::Char('a'), false));
        app.on_key(key(KeyCode::Char('k'), false));
        assert!(
            matches!(app.mode, Mode::AddShell { index: 0 }),
            "k at the top stays put"
        );
        app.on_key(key(KeyCode::Char('G'), false));
        assert!(matches!(app.mode, Mode::AddShell { index } if index == last));
        app.on_key(key(KeyCode::Char('j'), false));
        assert!(
            matches!(app.mode, Mode::AddShell { index } if index == last),
            "j at the bottom stays put"
        );
        app.on_key(key(KeyCode::Char('g'), false));
        assert!(matches!(app.mode, Mode::AddShell { index: 0 }));
    }
}
