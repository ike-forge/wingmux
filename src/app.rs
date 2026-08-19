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

use crate::config::{Config, ProjectConfig};
use crate::error::{Error, Result};
use crate::git::Git;
use crate::state::{Feature, Project, Shell, State};
use crate::status::{self, Compiled, Status};
use crate::theme::Theme;
use crate::tmux::{PaneInfo, Tmux};

/// Lines of the capture used for status classification.
const RECENT_TAIL_LINES: usize = 10;

/// How long a toast stays on screen before it fades by itself. Errors get
/// longer — they are the ones worth reading.
const TOAST_TTL: Duration = Duration::from_secs(4);
const TOAST_TTL_ERR: Duration = Duration::from_secs(6);

/// How often the per-feature dirty flags are recomputed. Each one shells
/// out to `git status`, so it runs on its own slow cadence rather than on
/// every tmux poll.
const DIRTY_INTERVAL: Duration = Duration::from_secs(3);

/// Hard ceiling on one quick-send draft. Not a limit anyone types into —
/// a backstop so a runaway paste cannot grow the buffer without bound.
const TEXTBUF_MAX_BYTES: usize = 1_000_000;

/// Projects unfold by default up to this many in scope. Past it the tree
/// is longer than any terminal and folding is the readable default.
const UNFOLD_CAP: usize = 30;

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

/// A node in the projects/features/shells tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sel {
    Project(usize),
    Feature(usize, usize),
    Shell(usize, usize, usize),
}

/// What the user is doing right now.
#[derive(Debug)]
pub enum Mode {
    Normal,
    Input {
        kind: InputKind,
        text: String,
    },
    /// Multiline prompt editor (quick-send). Owns all editing keys:
    /// Enter = newline, Tab = tab, arrows/Home/End move the cursor,
    /// ctrl-s (or Alt+Enter) = send, Esc = cancel. The accessibility
    /// layer only keeps ctrl-c (quit) and ctrl-t (theme) while it is open.
    Textarea {
        buf: TextBuf,
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
    /// Predefined-message modal (`p`): the configured quick-send presets
    /// as a list. j/k move, digits `1`..`9` pick an entry directly, Enter
    /// sends the selected one, Esc closes. Sending uses the same paste
    /// path as the textarea — the agent sees what a human would type.
    Messages {
        index: usize,
    },
    /// Startup diagnostics overlay (shown when configuration problems were
    /// dropped at load — see the configuration guidelines). Scrollable list
    /// of diagnostics, dismissed with Esc/q/Enter.
    Diagnostics,
}

/// A multiline text buffer for the quick-send editor: lines + a cursor
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
    /// accounts for soft-wrapping; these stay for the editing tests.
    #[cfg(test)]
    pub fn cursor_row(&self) -> usize {
        self.row
    }

    #[cfg(test)]
    pub fn cursor_col(&self) -> usize {
        self.col
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
    /// handler calls this once per key — only it knows the popup's real
    /// content height, and scrolling inside the editing methods (with a
    /// hardcoded one-line window) pushed every earlier line out of the
    /// textarea.
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
/// one (the textarea renders tabs as four spaces).
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

/// Height of the quick-send textarea popup for a viewport height. Grows
/// with the terminal up to a cap — the textarea is the main interaction,
/// it should be roomy.
pub(crate) fn textarea_height(viewport_h: u16) -> u16 {
    (viewport_h / 2).clamp(10, 18)
}

/// Width of the quick-send textarea popup for a viewport width.
pub(crate) fn textarea_width(viewport_w: u16) -> u16 {
    (viewport_w * 70 / 100).clamp(40, 96)
}

/// Content rows of the textarea popup (inside the border, above the
/// spacer and the hint row): height − 4 (2 borders + spacer + hint).
pub(crate) fn textarea_visible_lines(viewport_h: u16) -> usize {
    textarea_height(viewport_h) as usize - 4
}

/// Soft-wrap width of the textarea content: the popup width minus the
/// two borders, minus one column so a cursor sitting at the end of a
/// full row still has a cell of its own inside the border.
pub(crate) fn textarea_wrap_width(viewport_w: u16) -> usize {
    (textarea_width(viewport_w).saturating_sub(3) as usize).max(1)
}

/// An action reachable from the statusbar shortcut entries (and by mouse
/// click). Same words as the help overlay — learning one teaches the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarAction {
    Help,
    NewFeature,
    AddShell,
    QuickSend,
    PredefinedSend,
    DeleteFeature,
    DeleteShell,
    Attach,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    NewFeature,
    /// The command the new shell should run. Empty means "the project's
    /// default agent command" — the prompt shows it as a hint.
    AddShell,
    QuickSend,
}

#[derive(Debug, Clone)]
pub struct Confirm {
    pub title: String,
    pub body: String,
    /// When `Some`, the `f` key is offered (e.g. `--force` on a dirty
    /// worktree) and this labels it.
    pub force_label: Option<String>,
    pub action: ConfirmAction,
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteFeature {
        project: usize,
        feature: usize,
    },
    DeleteShell {
        project: usize,
        feature: usize,
        shell: usize,
    },
}

pub struct App {
    pub cfg: Config,
    pub theme: Theme,
    pub tmux: Option<Tmux>,
    pub state: State,
    pub sel: Sel,
    /// Projects whose `git worktree list` is still owed, drained one per
    /// event-loop iteration by [`App::drain_pending_worktrees`].
    pub pending_worktrees: std::collections::VecDeque<usize>,
    pub proj_expanded: Vec<bool>,
    pub feat_expanded: Vec<Vec<bool>>,
    pub mode: Mode,
    pub panes: HashMap<String, PaneInfo>,
    pub captures: HashMap<String, String>,
    /// Unsent quick-send text, per shell (keyed by pane id, like
    /// `captures` and `statuses`). Closing the editor parks the buffer
    /// here so the next `s` on that shell resumes where it left off;
    /// delivering the message drops it.
    pub drafts: HashMap<String, TextBuf>,
    pub statuses: HashMap<String, Status>,
    /// Path to the generated Claude hook settings file, when
    /// instrumentation is enabled (`[defaults] agent_hooks`). `None`
    /// disables injection entirely.
    pub hook_settings: Option<std::path::PathBuf>,
    pattern_cache: HashMap<String, Compiled>,
    pub last_poll: Instant,
    /// The transient notice shown as a toast above the statusbar. Set by
    /// [`App::notify`] / [`App::notify_err`]; read through [`App::toast`],
    /// which drops it once its TTL has passed — the statusbar is
    /// shortcuts-only, so notices are never allowed to become permanent
    /// furniture. Everything also goes to the log.
    message: Option<Toast>,
    /// Set by `attach` when the dashboard runs standalone: the event loop
    /// hands the terminal over to `tmux attach -t <session>` and resumes.
    pub pending_attach: Option<String>,
    /// Interactive boot: scope discovery runs automatically (no prompt).
    /// Off in tests (and irrelevant for a TUI that already booted);
    /// `run()` turns it on.
    pub interactive_boot: bool,
    pub quit: bool,
    /// The current scope: only projects whose path is inside this root are
    /// listed (startup cwd, or `[discovery].scan_root` when set). Projects
    /// in state from other scopes stay saved but are not shown — scopes
    /// never mix.
    pub scope_root: PathBuf,
    /// Flatten mode: exactly one project is in scope, so the tree shows
    /// its features at the top level without a project row.
    pub flatten: bool,
    /// Has `git worktree list` been run for each project (index-parallel
    /// to `state.projects`)? Worktrees are the expensive part of
    /// discovery, so they load lazily — when a project is expanded, or for
    /// the single flattened project at boot.
    pub worktrees_loaded: Vec<bool>,
    /// Last frame size — drives page-sized motions and mouse hit-testing.
    pub viewport: (u16, u16),
    /// ASCII-only rendering (terminals that mangle box-drawing): `+ - |`
    /// borders, `v`/`>` tree markers. Set by `run()` from the environment.
    pub ascii: bool,
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
    /// (x_start, x_end, action) on the statusbar row.
    pub bar_hits: Vec<(u16, u16, BarAction)>,
    /// Last left-click (time, x, y) — double-click detection, since this
    /// crossterm version does not report a click count on mouse events.
    last_click: Option<(Instant, u16, u16)>,
    /// Worktree path → has uncommitted changes. The tree draws this as the
    /// feature icon, so it must be a lookup, not a `git status` per frame;
    /// [`App::poll_dirty`] refills it every [`DIRTY_INTERVAL`].
    pub(crate) dirty: HashMap<PathBuf, bool>,
    last_dirty_poll: Option<Instant>,
    /// What the New Feature popup says the branch will be cut from,
    /// re-resolved every time the popup opens (a `git` call per keystroke
    /// is not on). `None` when the project's repo could not be read.
    new_feature_base: Option<BaseHint>,
}

/// The base branch a new feature would branch from, as shown in the New
/// Feature popup. `missing` carries the configured base branch when it is
/// absent from the repo and `branch` is therefore the current checkout —
/// the one case where the popup must not claim a base branch.
#[derive(Debug, Clone)]
pub struct BaseHint {
    pub branch: String,
    pub missing: Option<String>,
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
            sel: Sel::Project(0),
            pending_worktrees: std::collections::VecDeque::new(),
            proj_expanded: Vec::new(),
            feat_expanded: Vec::new(),
            mode: Mode::Normal,
            panes: HashMap::new(),
            captures: HashMap::new(),
            drafts: HashMap::new(),
            statuses: HashMap::new(),
            hook_settings: None,
            pattern_cache: HashMap::new(),
            last_poll: Instant::now(),
            message: None,
            dirty: HashMap::new(),
            last_dirty_poll: None,
            pending_attach: None,
            interactive_boot: false,
            quit: false,
            viewport: (0, 0),
            ascii: false,
            popup_area: None,
            help_scroll: 0,
            diag_scroll: 0,
            scope_root: PathBuf::from("."),
            flatten: false,
            worktrees_loaded: Vec::new(),
            theme_names: vec![crate::theme::DEFAULT_THEME.into()],
            picker_original: crate::theme::DEFAULT_THEME.into(),
            bar_hits: Vec::new(),
            last_click: None,
            new_feature_base: None,
        }
    }

    // ---------------------------------------------------------- lifecycle

    /// Load state, reconcile against reality, bind the back keys.
    pub fn startup(&mut self) -> Result<()> {
        self.state = State::load()?;
        self.merge_config_projects()?;
        // Interactive boot: auto-import the cwd's repo (with its worktrees
        // as features) or, when the cwd is not a repo, arm the startup
        // scan prompt.
        if self.interactive_boot {
            self.boot_discovery()?;
        }
        if let Some(tmux) = &self.tmux {
            // Standalone boot usually happens before any tmux server exists;
            // start one so session/window operations work from the first poll.
            if tmux.inside.is_none() {
                tmux.ensure_server()?;
            }
            self.state.reconcile(tmux)?;
            self.state.prune(Some(tmux), &self.worktrees_loaded)?;
            self.bind_back_keys(tmux)?;
        }
        self.build_tree_flags();
        self.clamp_sel();
        self.save_state()?;
        Ok(())
    }

    /// Bind the back-key in every live agent session so the user can jump
    /// back to the dashboard from any agent window. Only meaningful when
    /// the dashboard itself runs inside tmux.
    fn bind_back_keys(&self, tmux: &Tmux) -> Result<()> {
        let Some(inside) = &tmux.inside else {
            return Ok(());
        };
        // Pre-M9 versions bound the key in the root table (`-n`), which
        // hijacked the chord for every session and TUI on the server.
        // Remove that legacy binding first (only ours — a user's own root
        // binding of the same key is untouched), then bind behind the
        // prefix, where it cannot collide with any application.
        tmux.unbind_legacy_root_back_key(&inside.session_name, &inside.window_id)?;
        let mut bound = 0;
        for proj in &self.state.projects {
            if let Some(sess) = &proj.session_name
                && tmux.session_exists(sess)
            {
                tmux.bind_back_key(
                    &self.cfg.tmux.back_key,
                    &inside.session_name,
                    &inside.window_id,
                )?;
                bound += 1;
            }
        }
        if bound > 0 {
            log::info!(
                "back chord <prefix>+{} bound in {bound} agent session(s) → dashboard {}",
                self.cfg.tmux.back_key,
                inside.window_id
            );
        }
        Ok(())
    }

    fn save_state(&mut self) -> Result<()> {
        self.state.save()?;
        Ok(())
    }

    /// Import projects declared in config.toml that aren't in state yet.
    fn merge_config_projects(&mut self) -> Result<()> {
        let mut imported = 0;
        let paths: Vec<PathBuf> = self
            .cfg
            .usable_projects()
            .into_iter()
            .map(|p| PathBuf::from(&p.path))
            .collect();
        for path in paths {
            if self.state.projects.iter().any(|x| x.path == path) {
                continue;
            }
            if !Git.is_repo(&path) {
                log::warn!(
                    "config project {} is not a git repo, skipping",
                    path.display()
                );
                continue;
            }
            self.add_project(path)?;
            imported += 1;
        }
        if imported > 0 {
            log::info!("imported {imported} project(s) from config");
        }
        Ok(())
    }

    /// Push a new project for a verified repo path, resolving base branch
    /// and agent settings from the config (per-project overrides by path).
    fn add_project(&mut self, path: PathBuf) -> Result<()> {
        let name = Git.repo_name(&path);
        let proj_cfg = ProjectConfig {
            path: path.to_string_lossy().into_owned(),
            base_branch: None,
            agent: None,
            agent_command: None,
        };
        let base = Git.resolve_base_branch(&path, &self.cfg.base_branch_for(&proj_cfg))?;
        let agent = self.cfg.agent_for(&proj_cfg);
        self.state.projects.push(Project {
            name,
            path,
            base_branch: base,
            agent_command: agent.command,
            agent_shell: agent.shell,
            session_name: None,
            missing: false,
            features: Vec::new(),
        });
        Ok(())
    }

    // ---------------------------------------------------------- discovery

    /// The startup scope: `[discovery].scan_root`, or the directory
    /// LazyWingman was started from when unset. Relative roots resolve
    /// against `cwd`, which is injected so tests don't have to `chdir`.
    fn scan_root_from(&self, cwd: &std::path::Path) -> PathBuf {
        let root = self.cfg.discovery.scan_root.trim();
        if root.is_empty() {
            cwd.to_path_buf()
        } else {
            let p = PathBuf::from(root);
            if p.is_absolute() { p } else { cwd.join(p) }
        }
    }

    /// Startup scope discovery (interactive boot only). No prompt, no
    /// cache: the scope is what is INSIDE the startup directory.
    /// - cwd (or an ancestor) is a git repo → that repo is the scope; it is
    ///   imported as the single project and its worktrees become features.
    /// - otherwise → the scope is scanned LIVE for git repos and each one
    ///   is imported. Worktrees stay lazy until a project is expanded.
    ///
    /// Either way `scope_root` is set, and the tree only lists projects
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
                "cwd is inside a git repo (main: {}), single-project scope",
                main_repo.display()
            );
            self.import_cwd_repo(&main_repo)?;
        } else {
            // Folder scope: live-scan for repos. No cache — finding .git
            // is a few stats per dir, cheap even at hundreds of folders.
            let found = self.discover_and_import(&scope)?;
            log::info!(
                "scope {}: {found} project(s) from live scan",
                scope.display()
            );
        }
        self.build_tree_flags();
        // Worktrees for every unfolded project (and for the flattened
        // single project, whose features ARE the tree). Only the first one
        // is loaded here: it is what the first frame draws, and blocking
        // boot on one `git worktree list` per repo would cost seconds in a
        // scope with dozens of them. The rest are queued and drained by the
        // event loop, so the tree paints immediately and fills in.
        let vis = self.visible_projects();
        let mut to_load: Vec<usize> = vis
            .iter()
            .copied()
            .filter(|&pi| self.proj_expanded.get(pi).copied().unwrap_or(false))
            .collect();
        if self.flatten
            && let Some(&pi) = vis.first()
            && !to_load.contains(&pi)
        {
            to_load.push(pi);
        }
        let mut queued = to_load.into_iter();
        if let Some(pi) = queued.next() {
            self.ensure_worktrees(pi)?;
        }
        self.pending_worktrees.extend(queued);
        // Re-decide flatten now that features are known (a lone project
        // with NO features still shows its row — a blank tree would be
        // worse than a redundant row).
        if let Some(&pi) = vis.first()
            && vis.len() == 1
            && !self.state.projects[pi].features.is_empty()
        {
            self.flatten = true;
            self.build_tree_flags();
        }
        // Imports must survive a restart — persist what discovery found.
        self.save_state()?;
        Ok(())
    }

    /// Import (or complete) the project for `main_repo`. Worktrees are NOT
    /// listed here — [`Self::ensure_worktrees`] does that lazily when the
    /// project is expanded (or right after boot in flatten mode). Never
    /// fails startup on git errors — discovery is best-effort.
    fn import_cwd_repo(&mut self, main_repo: &std::path::Path) -> Result<()> {
        let path = main_repo.to_path_buf();
        let mut pi = self.state.projects.iter().position(|x| x.path == path);
        if pi.is_none() {
            if !Git.is_repo(&path) {
                return Ok(()); // vanished between detect and import
            }
            self.add_project(path.clone())?;
            pi = Some(self.state.projects.len() - 1);
        }
        let pi = pi.unwrap();
        // In flatten mode the features ARE the tree — load them now.
        self.ensure_worktrees(pi)?;
        Ok(())
    }

    /// Scan `root` for git repos and import every new one (projects already
    /// in state are skipped). No cache — the scan is live. Returns the
    /// number of projects added.
    fn discover_and_import(&mut self, root: &std::path::Path) -> Result<usize> {
        let root = crate::util::normalize_path(root);
        let paths = crate::discovery::scan(
            &root,
            self.cfg.discovery.max_depth,
            &self.cfg.discovery.skip_dirs,
        );
        let mut added = 0;
        for p in &paths {
            if self.state.projects.iter().any(|x| x.path == *p) {
                continue;
            }
            if !Git.is_repo(p) {
                continue; // raced (removed mid-scan)
            }
            match self.add_project(p.clone()) {
                Ok(()) => added += 1,
                Err(e) => log::warn!("importing {} failed: {e}", p.display()),
            }
        }
        if added > 0 {
            log::info!("discovery: {added} new project(s) under {}", root.display());
        }
        Ok(added)
    }

    /// Load the next queued project's worktrees. Returns false when the
    /// queue is empty. One project per call: the event loop redraws
    /// between calls, so a slow scope fills the tree in visibly instead of
    /// freezing before the first frame.
    pub fn drain_pending_worktrees(&mut self) -> bool {
        let Some(pi) = self.pending_worktrees.pop_front() else {
            return false;
        };
        if let Err(e) = self.ensure_worktrees(pi) {
            log::warn!("background worktree load for project {pi}: {e}");
        }
        true
    }

    pub(crate) fn build_tree_flags(&mut self) {
        // Scope-aware: only in-scope projects get rows, and everything
        // unfolds by default — the dashboard exists to show the shells, and
        // a folded tree hides them. The cap is a readability backstop for
        // scopes with a wild number of repos, not a cost guard: the
        // `git worktree list` each unfolded project owes is queued and
        // drained in the background (see `boot_discovery_from`).
        let vis = self.visible_projects();
        // Flatten: exactly one project in scope WITH features → its
        // features ARE the tree, no project row. A lone project with no
        // features (missing, or nothing linked yet) still shows its row.
        self.flatten = vis.len() == 1 && !self.state.projects[vis[0]].features.is_empty();
        let expand = vis.len() <= UNFOLD_CAP;
        self.proj_expanded = self
            .state
            .projects
            .iter()
            .enumerate()
            .map(|(i, _)| vis.contains(&i) && expand)
            .collect();
        self.feat_expanded = self
            .state
            .projects
            .iter()
            .map(|p| vec![true; p.features.len()])
            .collect();
        // Keep the loaded flag parallel to projects.
        self.worktrees_loaded
            .resize(self.state.projects.len(), false);
    }

    /// Indices of the projects currently in scope (path inside
    /// `scope_root`). Projects from other scopes stay in state but never
    /// get rows — scopes don't mix. When the scope was never set (tests,
    /// or an app that skipped boot discovery) `scope_root` is the "."
    /// sentinel and everything is visible.
    pub(crate) fn visible_projects(&self) -> Vec<usize> {
        let scoped = self.scope_root.as_os_str() != ".";
        self.state
            .projects
            .iter()
            .enumerate()
            .filter(|(_, p)| !scoped || p.path.starts_with(&self.scope_root))
            .map(|(i, _)| i)
            .collect()
    }

    /// Lazily load a project's worktrees (the expensive part of
    /// discovery): `git worktree list` runs ONCE per project, when its
    /// details are actually needed (expanded, or the flattened single
    /// project at boot). Features are added deduplicated by worktree path.
    fn ensure_worktrees(&mut self, pi: usize) -> Result<()> {
        if self.worktrees_loaded.get(pi).copied().unwrap_or(false) {
            return Ok(());
        }
        let proj = self
            .state
            .projects
            .get(pi)
            .cloned()
            .ok_or_else(|| Error::State(format!("project index {pi} out of range")))?;
        if proj.missing {
            return Ok(());
        }
        let worktrees = match Git.worktree_list(&proj.path) {
            Ok(w) => w,
            Err(e) => {
                log::warn!("worktree list failed for {}: {e}", proj.path.display());
                return Ok(());
            }
        };
        let existing: std::collections::HashSet<PathBuf> = self.state.projects[pi]
            .features
            .iter()
            .map(|f| f.worktree_path.clone())
            .collect();
        let mut added = 0;
        for wt in worktrees {
            // The first entry is the main worktree — that IS the project.
            if wt.path == proj.path || existing.contains(&wt.path) {
                continue;
            }
            let branch = wt.branch_short().unwrap_or("").to_string();
            let name = if branch.is_empty() {
                wt.path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "worktree".into())
            } else {
                branch.clone()
            };
            self.state.projects[pi].features.push(Feature {
                name,
                branch,
                worktree_path: wt.path,
                created_at: String::new(),
                shells: Vec::new(),
            });
            added += 1;
        }
        if let Some(flag) = self.worktrees_loaded.get_mut(pi) {
            *flag = true;
        }
        // `feat_expanded` is index-parallel to `features`; a short vector
        // reads as folded, so the rows this just discovered would arrive
        // collapsed. They unfold like every other row.
        if let Some(flags) = self.feat_expanded.get_mut(pi) {
            flags.resize(self.state.projects[pi].features.len(), true);
        }
        if added > 0 {
            log::info!("lazy worktree load: {added} feature(s) for '{}'", proj.name);
        }
        Ok(())
    }

    // ---------------------------------------------------------- navigation

    /// The shell a folded feature row stands for. The nominal case is one
    /// worktree, one agent: a feature with exactly one shell gets no child
    /// row at all — its own row *is* that shell (status, preview, attach,
    /// quick-send). Features with none or several shells keep their
    /// subtree, so `a` on a folded feature unfolds it by giving it a
    /// second shell.
    pub(crate) fn solo_shell(&self, pi: usize, fi: usize) -> Option<usize> {
        let feat = self.state.projects.get(pi)?.features.get(fi)?;
        (feat.shells.len() == 1).then_some(0)
    }

    /// All visible tree nodes in display order (scope-filtered; in flatten
    /// mode the single project's features sit at the top level).
    fn visible_nodes(&self) -> Vec<Sel> {
        let mut v = Vec::new();
        for pi in self.visible_projects() {
            let proj = &self.state.projects[pi];
            if !self.flatten {
                v.push(Sel::Project(pi));
                if !self.proj_expanded.get(pi).copied().unwrap_or(false) {
                    continue;
                }
            }
            for (fi, feat) in proj.features.iter().enumerate() {
                v.push(Sel::Feature(pi, fi));
                let fexp = self
                    .feat_expanded
                    .get(pi)
                    .and_then(|e| e.get(fi))
                    .copied()
                    .unwrap_or(false);
                // A solo feature is folded for good: its own row is the
                // shell, so it never emits a child — whatever its flag says.
                if !fexp || self.solo_shell(pi, fi).is_some() {
                    continue;
                }
                for si in 0..feat.shells.len() {
                    v.push(Sel::Shell(pi, fi, si));
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
            self.sel = Sel::Project(0);
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
            Sel::Project(pi) => {
                // Expanding a project is when its worktrees are needed:
                // load them lazily (once per project per run).
                if !self.proj_expanded.get(pi).copied().unwrap_or(false)
                    && self.ensure_worktrees(pi).is_ok()
                    && self.worktrees_loaded.get(pi).copied().unwrap_or(false)
                {
                    // features may have appeared; refresh the per-feature
                    // expand flags
                    let n = self.state.projects[pi].features.len();
                    if let Some(v) = self.feat_expanded.get_mut(pi) {
                        v.resize(n, false);
                    }
                }
                if let Some(e) = self.proj_expanded.get_mut(pi) {
                    *e = !*e;
                }
            }
            Sel::Feature(pi, fi) => {
                // Folded feature: there is nothing to expand into, so the
                // row acts as its shell and opens the agent.
                if let Some(si) = self.solo_shell(pi, fi) {
                    self.attach(pi, fi, si);
                } else if let Some(e) = self.feat_expanded.get_mut(pi).and_then(|v| v.get_mut(fi)) {
                    *e = !*e;
                }
            }
            Sel::Shell(pi, fi, si) => self.attach(pi, fi, si),
        }
    }

    fn collapse_or_up(&mut self) {
        match self.sel {
            Sel::Project(pi) => {
                if let Some(e) = self.proj_expanded.get_mut(pi)
                    && *e
                {
                    *e = false;
                }
            }
            Sel::Feature(pi, fi) => {
                let fexp = self
                    .feat_expanded
                    .get(pi)
                    .and_then(|v| v.get(fi))
                    .copied()
                    .unwrap_or(false)
                    && self.solo_shell(pi, fi).is_none();
                if fexp {
                    if let Some(e) = self.feat_expanded.get_mut(pi).and_then(|v| v.get_mut(fi)) {
                        *e = false;
                    }
                } else if !self.flatten {
                    // In flatten mode there is no project row above the
                    // features — `h` at a collapsed feature is a no-op.
                    self.sel = Sel::Project(pi);
                }
            }
            Sel::Shell(pi, fi, _) => self.sel = Sel::Feature(pi, fi),
        }
    }

    /// The shell the selection acts on: an explicit shell row, or the
    /// single shell a folded feature row stands for.
    pub(crate) fn selected_shell(&self) -> Option<(usize, usize, usize)> {
        match self.sel {
            Sel::Shell(pi, fi, si) => Some((pi, fi, si)),
            Sel::Feature(pi, fi) => self.solo_shell(pi, fi).map(|si| (pi, fi, si)),
            _ => None,
        }
    }

    /// Open the quick-send editor on the selected shell, resuming that
    /// shell's parked draft. Shells with no live pane get no draft: they
    /// cannot receive a message, so there is nothing to resume.
    fn open_quick_send(&mut self) {
        let buf = self
            .selected_shell_pane()
            .and_then(|pid| self.drafts.get(&pid).cloned())
            .unwrap_or_default();
        self.mode = Mode::Textarea { buf };
    }

    /// Park an unsent buffer against its shell. An empty one is not a
    /// draft — it is cleared so a stale entry never shadows a fresh start.
    fn park_draft(&mut self, buf: TextBuf) {
        let Some(pid) = self.selected_shell_pane() else {
            return;
        };
        if buf.text().is_empty() {
            self.drafts.remove(&pid);
        } else {
            self.drafts.insert(pid, buf);
        }
    }

    /// Pane id of the selected shell, if any.
    pub fn selected_shell_pane(&self) -> Option<String> {
        self.selected_shell().and_then(|(pi, fi, si)| {
            self.state.projects[pi].features[fi].shells[si]
                .pane_id
                .clone()
        })
    }

    // ---------------------------------------------------------- actions

    fn require_tmux(&self) -> Result<Tmux> {
        self.tmux
            .clone()
            .ok_or_else(|| Error::Tmux("no tmux server available".into()))
    }

    /// The command actually typed into a fresh pane: the user's
    /// `agent_command`, plus `--settings` when it is a recognised `claude`
    /// and instrumentation is on. The stored command is never rewritten.
    pub fn agent_command_for_spawn(&self, command: &str) -> String {
        match &self.hook_settings {
            Some(p) => crate::hooks::instrument(command, p),
            None => command.to_string(),
        }
    }

    /// The dedicated agent-session name for a project. Deterministic:
    /// `wingman-session-<slug>`, suffixed with a stable hash of the path
    /// when another project already owns that name or a foreign session
    /// occupies it.
    fn agent_session_name(&self, tmux: &Tmux, proj: &crate::state::Project) -> String {
        let base = Tmux::session_name(&proj.name);
        let taken_by_other = self
            .state
            .projects
            .iter()
            .any(|p| p.path != proj.path && p.session_name.as_deref() == Some(&base));
        if taken_by_other || tmux.session_exists(&base) {
            format!("{base}-{}", Tmux::hash_suffix(&proj.path))
        } else {
            base
        }
    }

    /// Make sure the project's agent session exists, creating it if needed.
    /// Returns the session name and, when a NEW session was created, its
    /// first window (already running `shell` in `cwd`).
    fn ensure_agent_session(
        &mut self,
        tmux: &Tmux,
        proj: &crate::state::Project,
        first_window: &str,
        cwd: &std::path::Path,
        shell: &str,
    ) -> Result<(String, Option<crate::tmux::WindowRef>)> {
        let name = match &proj.session_name {
            Some(n) if tmux.session_exists(n) => n.clone(),
            _ => self.agent_session_name(tmux, proj),
        };
        if tmux.session_exists(&name) {
            return Ok((name, None));
        }
        let w = tmux.new_session(&name, first_window, cwd, shell)?;
        if let Some(inside) = &tmux.inside {
            let _ = tmux.bind_back_key(
                &self.cfg.tmux.back_key,
                &inside.session_name,
                &inside.window_id,
            );
        }
        log::info!(
            "created agent session {name} (first window {})",
            w.window_id
        );
        Ok((name, Some(w)))
    }

    pub fn create_feature(&mut self, pi: usize, name: &str) -> Result<()> {
        let proj = self
            .state
            .projects
            .get(pi)
            .cloned()
            .ok_or_else(|| Error::State(format!("project index {pi} out of range")))?;
        if proj.missing {
            return Err(Error::State(format!(
                "project '{}' is missing ({} is not a git repo)",
                proj.name,
                proj.path.display()
            )));
        }
        let tmux = self.require_tmux()?;
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::State("feature name is empty".into()));
        }
        let slug = crate::git::slugify(name);
        if proj.features.iter().any(|f| f.branch == slug) {
            return Err(Error::State(format!(
                "feature '{}' already exists in '{}'",
                name, proj.name
            )));
        }
        if Git.branch_exists(&proj.path, &slug) {
            return Err(Error::Git(format!(
                "branch '{slug}' already exists in {}",
                proj.path.display()
            )));
        }
        let base = Git.resolve_base_branch(&proj.path, &proj.base_branch)?;
        let root = self
            .cfg
            .worktree_root_for(&proj.path)
            .join(&proj.name)
            .join(&slug);
        if root.exists() {
            return Err(Error::Git(format!(
                "worktree path {} already exists",
                root.display()
            )));
        }
        Git.worktree_add(&proj.path, &root, &slug, &base)?;

        // spawn the agent shell in the project's dedicated session (created
        // on demand; never touches the dashboard's own session)
        let wname = Tmux::window_name(&slug, 0);
        let (session_name, window) =
            self.ensure_agent_session(&tmux, &proj, &wname, &root, &proj.agent_shell)?;
        let w = match window {
            Some(w) => w,
            None => tmux.new_window(&session_name, &wname, &root, &proj.agent_shell)?,
        };
        let spawn_cmd = self.agent_command_for_spawn(&proj.agent_command);
        tmux.send_keys(&w.pane_id, &spawn_cmd, true)?;

        self.state.projects[pi].session_name = Some(session_name.clone());
        self.state.projects[pi].features.push(Feature {
            name: name.to_string(),
            branch: slug,
            worktree_path: root,
            created_at: now_str(),
            shells: vec![Shell {
                index: 0,
                window_name: wname,
                window_id: Some(w.window_id),
                pane_id: Some(w.pane_id),
            }],
        });
        if let Some(v) = self.feat_expanded.get_mut(pi) {
            v.push(false);
        }
        self.sel = Sel::Feature(pi, self.state.projects[pi].features.len() - 1);
        self.clamp_sel();
        self.save_state()?;
        self.poll_dirty(true);
        self.notify(format!("feature '{}' created", name));
        Ok(())
    }

    /// Spawn one more shell under a feature, running `command`. An empty
    /// (or whitespace-only) `command` means "whatever this project
    /// normally runs" — the project's default agent command.
    pub fn add_shell(&mut self, pi: usize, fi: usize, command: &str) -> Result<()> {
        let tmux = self.require_tmux()?;
        let proj = self
            .state
            .projects
            .get(pi)
            .cloned()
            .ok_or_else(|| Error::State(format!("project index {pi} out of range")))?;
        if proj.missing {
            return Err(Error::State(format!(
                "project '{}' is missing ({} is not a git repo)",
                proj.name,
                proj.path.display()
            )));
        }
        let feat = proj
            .features
            .get(fi)
            .cloned()
            .ok_or_else(|| Error::State(format!("feature index {fi} out of range")))?;
        let idx = feat.next_shell_index();
        let wname = Tmux::window_name(&feat.branch, idx);
        let (session_name, window) = self.ensure_agent_session(
            &tmux,
            &proj,
            &wname,
            &feat.worktree_path,
            &proj.agent_shell,
        )?;
        let w = match window {
            Some(w) => w,
            None => tmux.new_window(
                &session_name,
                &wname,
                &feat.worktree_path,
                &proj.agent_shell,
            )?,
        };
        let command = match command.trim() {
            "" => proj.agent_command.as_str(),
            c => c,
        };
        // The command the user chose (or the project default) is what
        // gets instrumented — M17 injects --settings only when it is a
        // recognised `claude`.
        let spawn_cmd = self.agent_command_for_spawn(command);
        tmux.send_keys(&w.pane_id, &spawn_cmd, true)?;
        self.state.projects[pi].session_name = Some(session_name.clone());
        self.state.projects[pi].features[fi].shells.push(Shell {
            index: idx,
            window_name: wname,
            window_id: Some(w.window_id),
            pane_id: Some(w.pane_id),
        });
        self.save_state()?;
        self.notify(format!("shell #{} added ({})", idx, command));
        Ok(())
    }

    pub fn attach(&mut self, pi: usize, fi: usize, si: usize) {
        let Some(sh) = self
            .state
            .projects
            .get(pi)
            .and_then(|p| p.features.get(fi))
            .and_then(|f| f.shells.get(si))
        else {
            return;
        };
        let Some(wid) = &sh.window_id else {
            self.notify("shell has no live window");
            return;
        };
        let Some(session) = self.state.projects[pi].session_name.clone() else {
            self.notify("project has no agent session");
            return;
        };
        let res = match self.require_tmux() {
            Ok(t) if t.inside.is_some() => t.attach_switch(&session, wid).map(|_| {
                log::info!("attached (switch-client) to {}", sh.window_name);
            }),
            Ok(t) => {
                // standalone dashboard: select the window clientless, then
                // hand the terminal over; the event loop does the dance
                t.select_window_in_session(&session, wid).map(|_| {
                    self.pending_attach = Some(session);
                    log::info!("handing terminal over to {}", sh.window_name);
                })
            }
            Err(e) => Err(e),
        };
        if let Err(e) = res {
            self.notify_err(format!("attach failed: {e}"));
            log::error!("attach failed: {e}");
        }
    }

    pub fn quick_send(&mut self, pi: usize, fi: usize, si: usize, text: &str) -> Result<()> {
        let tmux = self.require_tmux()?;
        let sh = self
            .state
            .projects
            .get(pi)
            .and_then(|p| p.features.get(fi))
            .and_then(|f| f.shells.get(si))
            .cloned()
            .ok_or_else(|| Error::State("shell out of range".into()))?;
        let Some(pid) = &sh.pane_id else {
            return Err(Error::Tmux("shell has no live pane".into()));
        };
        if text.trim().is_empty() {
            return Ok(());
        }
        tmux.paste_keys(pid, text, true)?;
        log::info!("quick-send to {}: {text}", sh.window_name);
        Ok(())
    }

    pub fn delete_feature(&mut self, pi: usize, fi: usize, force: bool) -> Result<()> {
        let tmux = self.require_tmux()?;
        let proj = self
            .state
            .projects
            .get(pi)
            .cloned()
            .ok_or_else(|| Error::State(format!("project index {pi} out of range")))?;
        let feat = proj
            .features
            .get(fi)
            .cloned()
            .ok_or_else(|| Error::State(format!("feature index {fi} out of range")))?;
        for sh in &feat.shells {
            if let Some(wid) = &sh.window_id {
                match tmux.kill_window(wid) {
                    Ok(()) => log::info!("killed window {wid} ({})", sh.window_name),
                    Err(e) => log::warn!("kill-window {wid}: {e}"),
                }
            }
        }
        Self::kill_session_if_empty(&tmux, &proj);
        Git.worktree_remove(&proj.path, &feat.worktree_path, force)?;
        if self.cfg.worktrees.delete_branch {
            match Git.branch_delete(&proj.path, &feat.branch) {
                Ok(()) => log::info!("deleted branch '{}'", feat.branch),
                Err(e) => log::warn!("branch delete: {e}"),
            }
        }
        self.state.projects[pi].features.remove(fi);
        if let Some(v) = self.feat_expanded.get_mut(pi) {
            v.remove(fi);
        }
        self.clamp_sel();
        self.save_state()?;
        self.poll_dirty(true);
        self.notify(format!("feature '{}' deleted", feat.name));
        Ok(())
    }

    /// Close one shell: kill its tmux window and drop it from the
    /// feature. The feature itself — worktree, branch, sibling shells —
    /// is untouched; deleting the feature is `d` on the feature row.
    pub fn delete_shell(&mut self, pi: usize, fi: usize, si: usize) -> Result<()> {
        let tmux = self.require_tmux()?;
        let proj = self
            .state
            .projects
            .get(pi)
            .cloned()
            .ok_or_else(|| Error::State(format!("project index {pi} out of range")))?;
        let feat = proj
            .features
            .get(fi)
            .cloned()
            .ok_or_else(|| Error::State(format!("feature index {fi} out of range")))?;
        let sh = feat
            .shells
            .get(si)
            .cloned()
            .ok_or_else(|| Error::State(format!("shell index {si} out of range")))?;
        if let Some(wid) = &sh.window_id {
            match tmux.kill_window(wid) {
                Ok(()) => log::info!("killed window {wid} ({})", sh.window_name),
                Err(e) => log::warn!("kill-window {wid}: {e}"),
            }
        }
        Self::kill_session_if_empty(&tmux, &proj);
        self.state.projects[pi].features[fi].shells.remove(si);
        self.clamp_sel();
        self.save_state()?;
        self.poll_dirty(true);
        self.notify(format!("shell '{}' closed", sh.window_name));
        Ok(())
    }

    /// Killing a session's last window destroys the session; if it
    /// survives with no LazyWingman windows left, kill it too.
    fn kill_session_if_empty(tmux: &Tmux, proj: &crate::state::Project) {
        if let Some(sess) = &proj.session_name
            && tmux.session_exists(sess)
            && tmux
                .list_windows(sess)
                .map(|ws| {
                    ws.iter()
                        .all(|w| !crate::tmux::is_lazywingman_window(&w.window_name))
                })
                .unwrap_or(false)
        {
            log::info!("agent session {sess} has no shells left, killing it");
            let _ = tmux.kill_session(sess);
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

    // ------------------------------------------------------- dirty flags

    /// Cached dirty flag for the tree — filled by [`App::poll_dirty`],
    /// never a `git status` on the render path.
    pub fn feature_dirty_cached(&self, pi: usize, fi: usize) -> bool {
        self.state
            .projects
            .get(pi)
            .and_then(|p| p.features.get(fi))
            .and_then(|f| self.dirty.get(&f.worktree_path))
            .copied()
            .unwrap_or(false)
    }

    /// Refresh the dirty cache, at most every [`DIRTY_INTERVAL`]. Called
    /// from the poll tick; `force` skips the interval after a change that
    /// invalidates it (feature created/deleted).
    pub fn poll_dirty(&mut self, force: bool) {
        if !force
            && self
                .last_dirty_poll
                .is_some_and(|t| t.elapsed() < DIRTY_INTERVAL)
        {
            return;
        }
        self.last_dirty_poll = Some(Instant::now());
        let paths: Vec<PathBuf> = self
            .state
            .projects
            .iter()
            .flat_map(|p| p.features.iter().map(|f| f.worktree_path.clone()))
            .collect();
        self.dirty = paths
            .into_iter()
            .map(|path| {
                let d = Git.worktree_dirty(&path).unwrap_or(false);
                (path, d)
            })
            .collect();
    }

    /// Is the feature's worktree dirty? (drives the --force confirm option)
    pub fn feature_is_dirty(&self, pi: usize, fi: usize) -> bool {
        self.state
            .projects
            .get(pi)
            .and_then(|p| p.features.get(fi))
            .and_then(|f| Git.worktree_dirty(&f.worktree_path).ok())
            .unwrap_or(false)
    }

    pub fn confirm_delete_feature(&mut self) {
        let Some((pi, fi, _)) = self.selected_feature_ctx() else {
            self.notify("select a feature first");
            return;
        };
        let dirty = self.feature_is_dirty(pi, fi);
        let feat = &self.state.projects[pi].features[fi];
        self.mode = Mode::Confirm {
            confirm: Confirm {
                title: "Delete Feature".into(),
                body: format!(
                    "Delete feature '{}'?\nworktree: {}\n{}",
                    feat.name,
                    feat.worktree_path.display(),
                    if dirty {
                        format!(
                            "{} worktree has uncommitted changes — force delete will discard them",
                            crate::ui::warn_mark(&self.cfg.icons)
                        )
                    } else {
                        "worktree is clean".into()
                    }
                ),
                force_label: if dirty {
                    Some("force delete (discards changes)".into())
                } else {
                    None
                },
                action: ConfirmAction::DeleteFeature {
                    project: pi,
                    feature: fi,
                },
            },
        };
    }

    pub fn confirm_delete_shell(&mut self) {
        let Sel::Shell(pi, fi, si) = self.sel else {
            self.notify("select a shell first");
            return;
        };
        let feat = &self.state.projects[pi].features[fi];
        let sh = &feat.shells[si];
        self.mode = Mode::Confirm {
            confirm: Confirm {
                title: "Close Shell".into(),
                body: format!(
                    "Close shell '{}' of feature '{}'?\nThe tmux window is killed; the feature, its worktree and its other shells stay.",
                    sh.window_name, feat.name
                ),
                force_label: None,
                action: ConfirmAction::DeleteShell {
                    project: pi,
                    feature: fi,
                    shell: si,
                },
            },
        };
    }

    /// Open the add-shell prompt (`a`). The field starts empty; the popup
    /// shows the project's default command as a hint and an empty submit
    /// falls back to it.
    fn open_add_shell(&mut self) {
        if self.selected_feature_ctx().is_some() {
            self.mode = Mode::Input {
                kind: InputKind::AddShell,
                text: String::new(),
            };
        } else {
            self.notify("select a feature to add a shell");
        }
    }

    /// Open the New Feature popup for the selected project, resolving the
    /// branch the worktree would be cut from up front so the popup can name
    /// it. A feature always branches from the project's base branch, never
    /// from whatever the repo currently has checked out.
    pub(crate) fn open_new_feature(&mut self) {
        let pi = match self.sel {
            Sel::Project(pi) | Sel::Feature(pi, _) => pi,
            Sel::Shell(..) => {
                self.notify("select a project to add a feature");
                return;
            }
        };
        self.new_feature_base = self.base_hint(pi);
        self.mode = Mode::Input {
            kind: InputKind::NewFeature,
            text: String::new(),
        };
    }

    /// Resolve what `create_feature` would branch from for project `pi` —
    /// same call, so the popup cannot promise a base the creation then
    /// ignores. `None` for a missing repo (creation refuses it anyway).
    fn base_hint(&self, pi: usize) -> Option<BaseHint> {
        let proj = self.state.projects.get(pi)?;
        if proj.missing {
            return None;
        }
        let configured = proj.base_branch.trim().to_string();
        let branch = Git.resolve_base_branch(&proj.path, &configured).ok()?;
        // `resolve_base_branch` returns the configured branch when it
        // exists, so anything else means it fell back to the checkout.
        let missing = (!configured.is_empty() && branch != configured).then_some(configured);
        Some(BaseHint { branch, missing })
    }

    /// What the New Feature popup should say the branch is cut from.
    pub fn new_feature_base(&self) -> Option<&BaseHint> {
        self.new_feature_base.as_ref()
    }

    /// The command an empty add-shell submit would run: the selected
    /// project's default agent command. Drives the popup's hint line.
    pub fn add_shell_default(&self) -> Option<&str> {
        let pi = match self.sel {
            Sel::Project(pi) | Sel::Feature(pi, _) | Sel::Shell(pi, _, _) => pi,
        };
        self.state
            .projects
            .get(pi)
            .map(|p| p.agent_command.as_str())
    }

    fn selected_feature_ctx(&self) -> Option<(usize, usize, Option<usize>)> {
        match self.sel {
            Sel::Feature(pi, fi) => Some((pi, fi, None)),
            Sel::Shell(pi, fi, si) => Some((pi, fi, Some(si))),
            _ => None,
        }
    }

    // ---------------------------------------------------------- polling

    /// Compiled status patterns for a project (cached by path).
    fn patterns_for(&mut self, proj_path: &str, agent_command: &str) -> Compiled {
        let key = format!("{proj_path}\u{1f}{agent_command}");
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
    pub fn poll(&mut self) {
        // Independent of tmux (and of its own cadence) — the tree draws
        // the dirty flag whether or not a session exists.
        self.poll_dirty(false);
        let Ok(tmux) = self.require_tmux() else {
            return;
        };
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
        let sel_pane = self.selected_shell_pane();
        // Collect shell metadata first to avoid borrowing self twice.
        let mut shells: Vec<(String, String, String)> = Vec::new();
        for proj in &self.state.projects {
            let path = proj.path.to_string_lossy().into_owned();
            let cmd = proj.agent_command.clone();
            for feat in &proj.features {
                for sh in &feat.shells {
                    if let Some(pid) = &sh.pane_id {
                        shells.push((path.clone(), cmd.clone(), pid.clone()));
                    }
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
                // feature existed. Turning the flag off only stops writing
                // new overlays — a pane instrumented earlier (or by another
                // LazyWingman instance sharing the tmux server) still
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
        // (ctrl-d is navigation here, never "delete feature"). While the
        // help overlay is open these motions scroll the help (the palette
        // takes over the viewport); while the theme picker is open they
        // move the picker cursor; elsewhere they move the tree.
        //
        // The quick-send textarea owns its own motions: while it is open,
        // the page/Home/End keys edit the text, so the layer only keeps
        // ctrl-c (quit) and ctrl-t (theme) global.
        //
        // ctrl-t opens the live theme picker (a chord, so it dispatches
        // over every state — a theme switch is never behind a prompt).
        let in_textarea = matches!(self.mode, Mode::Textarea { .. });
        let handled = match key.code {
            KeyCode::PageUp if !in_textarea => {
                self.move_view(-1);
                true
            }
            KeyCode::PageDown if !in_textarea => {
                self.move_view(1);
                true
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) && !in_textarea => {
                self.move_view(-1);
                true
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) && !in_textarea => {
                self.move_view(1);
                true
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) && !in_textarea => {
                self.move_view(2);
                true
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) && !in_textarea => {
                self.move_view(-2);
                true
            }
            KeyCode::Home if !in_textarea => {
                self.move_view_first();
                true
            }
            KeyCode::End if !in_textarea => {
                self.move_view_last();
                true
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
                true
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
        // The textarea's page size = its visible content rows; its
        // soft-wrap width decides how many visual rows those hold.
        let visible_lines = textarea_visible_lines(self.viewport.1);
        let wrap_width = textarea_wrap_width(self.viewport.0);
        match &mut self.mode {
            Mode::Input { kind, text } => match key.code {
                KeyCode::Esc => self.mode = Mode::Normal,
                KeyCode::Enter => {
                    let text = std::mem::take(text);
                    let kind = *kind;
                    self.mode = Mode::Normal;
                    self.submit_input(kind, text);
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) => text.push(c),
                _ => {}
            },
            Mode::Textarea { buf } => {
                // Send: ctrl-s, or Alt+Enter where the terminal delivers
                // it. A plain Enter inserts a newline; Esc cancels. The
                // textarea owns all editing keys.
                //
                // ctrl-s is the reliable half of that pair: tmux with
                // `extended-keys off` (the default) cannot encode
                // Ctrl+Enter at all — it reaches us as a bare Enter — and
                // Alt+Enter is commonly swallowed by the outer terminal
                // emulator. A plain control byte always arrives.
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let send_chord = (key.code == KeyCode::Enter
                    && (key.modifiers.contains(KeyModifiers::ALT) || ctrl))
                    || (key.code == KeyCode::Char('s') && ctrl);
                if send_chord {
                    let parked = buf.clone();
                    let text = buf.text();
                    self.mode = Mode::Normal;
                    // Park first, drop only on delivery: `submit_input`
                    // toasts and swallows the error, so a send that never
                    // reached the pane must not take the text with it.
                    self.park_draft(parked);
                    if self.submit_input(InputKind::QuickSend, text)
                        && let Some(pid) = self.selected_shell_pane()
                    {
                        self.drafts.remove(&pid);
                    }
                } else if key.code == KeyCode::Esc {
                    let parked = buf.clone();
                    self.mode = Mode::Normal;
                    self.park_draft(parked);
                } else {
                    match key.code {
                        KeyCode::Enter => buf.newline(),
                        KeyCode::Tab => buf.insert_tab(),
                        KeyCode::Backspace => buf.backspace(),
                        KeyCode::Delete => buf.delete(),
                        KeyCode::Left => buf.move_left(),
                        KeyCode::Right => buf.move_right(),
                        KeyCode::Up => buf.move_up(),
                        KeyCode::Down => buf.move_down(),
                        KeyCode::Home => buf.move_home(),
                        KeyCode::End => buf.move_end(),
                        KeyCode::PageUp => {
                            buf.move_page(-1, buf.page(visible_lines));
                        }
                        KeyCode::PageDown => {
                            buf.move_page(1, buf.page(visible_lines));
                        }
                        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            // The textarea owns ctrl-u/d/f/b motions too
                            // (the accessibility layer yielded them):
                            // ctrl-u/d half a page, ctrl-f/b a full page.
                            match c {
                                'u' => {
                                    buf.move_page(-1, buf.page(visible_lines));
                                }
                                'd' => {
                                    buf.move_page(1, buf.page(visible_lines));
                                }
                                'f' => {
                                    buf.move_page(1, visible_lines.saturating_sub(1));
                                }
                                'b' => {
                                    buf.move_page(-1, visible_lines.saturating_sub(1));
                                }
                                _ => {}
                            }
                        }
                        KeyCode::Char(c) => buf.insert_char(c),
                        _ => {}
                    }
                    // Scrolling is decided here, where the popup's real
                    // content height and wrap width are known — never
                    // inside TextBuf.
                    buf.ensure_visible(visible_lines, wrap_width);
                }
            }
            Mode::Confirm { confirm } => match key.code {
                // `d` opened this popup, so `d` again closes the loop —
                // the vim `dd` reflex confirms without a hand move.
                KeyCode::Char('y') | KeyCode::Char('d') | KeyCode::Enter => {
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
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('h') | KeyCode::Left => self.collapse_or_up(),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => self.expand_or_attach(),
            KeyCode::Char('n') => self.open_new_feature(),
            KeyCode::Char('a') => self.open_add_shell(),
            KeyCode::Char('s') => {
                if self.selected_shell().is_some() {
                    self.open_quick_send();
                } else {
                    self.notify("select a shell to quick-send");
                }
            }
            KeyCode::Char('p') => self.open_messages(),
            KeyCode::Char('d') => match self.sel {
                Sel::Shell(..) => self.confirm_delete_shell(),
                _ => self.confirm_delete_feature(),
            },
            KeyCode::Char('r') => {
                self.refresh();
            }
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
            self.notify("select a shell to send a predefined message");
            return;
        }
        if self.cfg.messages.is_empty() {
            self.notify("no predefined messages configured");
            return;
        }
        self.mode = Mode::Messages { index: 0 };
    }

    /// Send the configured message at `index` to the selected shell — the
    /// same paste path as the quick-send textarea, so a preset and a typed
    /// prompt are indistinguishable to the agent.
    fn send_predefined(&mut self, index: usize) {
        let Some(text) = self.cfg.messages.get(index).map(|m| m.text.clone()) else {
            return;
        };
        let Some((pi, fi, si)) = self.selected_shell() else {
            self.notify("select a shell to send a predefined message");
            return;
        };
        if let Err(e) = self.quick_send(pi, fi, si, &text) {
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
    /// same in every project).
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

    /// Keep the previewed theme and close. Persist to STATE (per the
    /// configuration rules the app never rewrites the user's config file);
    /// startup honours `state.theme` over the config line.
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
        self.state.theme = Some(name.clone());
        if let Err(e) = self.save_state() {
            log::warn!("save theme '{}': {e}", name);
            self.notify_err(format!("theme '{name}' (not saved: {e})"));
        } else {
            self.notify(format!("theme: {name}"));
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

    // ---------------------------------------------------------- mouse

    pub fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let dbl = self.is_double_click(m.column, m.row);
                self.mouse_click(m.column, m.row, dbl);
            }
            // Wheel scrolls the panel under the cursor without changing
            // focus; the only scrollable panel is the tree (or the help
            // overlay / theme picker while one is open).
            MouseEventKind::ScrollDown => {
                if matches!(self.mode, Mode::Help) {
                    self.help_scroll_to(self.help_scroll + 3);
                } else if matches!(self.mode, Mode::Diagnostics) {
                    self.diag_scroll_to(self.diag_scroll + 3);
                } else if matches!(self.mode, Mode::Messages { .. }) {
                    self.messages_to(self.messages_index().saturating_add(3));
                } else if matches!(self.mode, Mode::ThemePicker { .. }) {
                    self.picker_to(self.picker_index().saturating_add(3));
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
                } else if matches!(self.mode, Mode::ThemePicker { .. }) {
                    self.picker_to(self.picker_index().saturating_sub(3));
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
        // Overlays: a click outside dismisses them (accessibility rule);
        // a click inside keeps keyboard as the way in. The theme picker is
        // the exception: clicking a theme row selects + previews it, and a
        // double-click keeps it (Enter).
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
        let h = self.viewport.1;
        // Statusbar row: click a shortcut entry to run its action.
        if h > 0 && y == h - 1 && !self.bar_hits.is_empty() {
            if let Some((_, _, action)) =
                self.bar_hits.iter().find(|(x0, x1, _)| *x0 <= x && x < *x1)
            {
                self.run_bar_action(*action);
            }
            return;
        }
        // Tree rows: click selects, double-click opens (Enter).
        let Some(tree_w) = crate::ui::tree_width(self.viewport) else {
            return; // terminal too small for the tree
        };
        if x >= tree_w {
            return; // detail/preview panels are not selectable
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
            BarAction::NewFeature => self.open_new_feature(),
            BarAction::AddShell => self.open_add_shell(),
            BarAction::QuickSend => {
                if self.selected_shell().is_some() {
                    self.open_quick_send();
                }
            }
            BarAction::PredefinedSend => self.open_messages(),
            BarAction::DeleteFeature => self.confirm_delete_feature(),
            BarAction::DeleteShell => self.confirm_delete_shell(),
            BarAction::Attach => {
                if let Some((pi, fi, si)) = self.selected_shell() {
                    self.attach(pi, fi, si);
                }
            }
        }
    }

    fn move_sel_page(&mut self, half_pages: i32) {
        // Page size = the tree's visible rows (statusbar + panel borders
        // subtracted). ctrl-u/d are half pages, ctrl-f/b full pages.
        let rows = (self.viewport.1.saturating_sub(3)).max(1) as i32;
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
        } else if let Mode::ThemePicker { index } = &mut self.mode {
            let step = 5 * half_pages;
            *index = (index.saturating_add_signed(step as isize))
                .min(self.theme_names.len().saturating_sub(1));
            self.preview_picker_theme();
        } else if matches!(self.mode, Mode::Help) {
            let total = crate::ui::help_line_count(self);
            let visible = self
                .popup_area
                .map(|r| r.height.saturating_sub(2) as usize)
                .unwrap_or(10);
            let max_scroll = total.saturating_sub(visible);
            let step = (visible / 2).max(1) * half_pages.max(0) as usize;
            if half_pages < 0 {
                self.help_scroll = self.help_scroll.saturating_sub(step.max(1));
            } else {
                self.help_scroll = (self.help_scroll + step).min(max_scroll);
            }
        } else if matches!(self.mode, Mode::Diagnostics) {
            let total = crate::ui::diag_line_count(self);
            let visible = self
                .popup_area
                .map(|r| r.height.saturating_sub(2) as usize)
                .unwrap_or(10);
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
        } else if let Mode::ThemePicker { index } = &mut self.mode {
            *index = self.theme_names.len().saturating_sub(1);
            self.preview_picker_theme();
        } else if matches!(self.mode, Mode::Help) {
            let total = crate::ui::help_line_count(self);
            let visible = self
                .popup_area
                .map(|r| r.height.saturating_sub(2) as usize)
                .unwrap_or(10);
            self.help_scroll = total.saturating_sub(visible);
        } else if matches!(self.mode, Mode::Diagnostics) {
            let total = crate::ui::diag_line_count(self);
            let visible = self
                .popup_area
                .map(|r| r.height.saturating_sub(2) as usize)
                .unwrap_or(10);
            self.diag_scroll = total.saturating_sub(visible);
        } else {
            self.move_sel_last();
        }
    }

    /// Step the help overlay by whole lines (`j`/`k` and arrows) or to the
    /// given index (`g`/`G`), clamped to the scrollable range.
    fn help_scroll_to(&mut self, n: usize) {
        let total = crate::ui::help_line_count(self);
        let visible = self
            .popup_area
            .map(|r| r.height.saturating_sub(2) as usize)
            .unwrap_or(10);
        self.help_scroll = n.min(total.saturating_sub(visible));
    }

    /// Same motion target as [`Self::help_scroll_to`], for the startup
    /// diagnostics overlay (its content height comes from
    /// `ui::diag_line_count`, never drifts from the renderer).
    fn diag_scroll_to(&mut self, n: usize) {
        let total = crate::ui::diag_line_count(self);
        let visible = self
            .popup_area
            .map(|r| r.height.saturating_sub(2) as usize)
            .unwrap_or(10);
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
            if let Err(e) = self.state.prune(Some(tmux), &self.worktrees_loaded) {
                log::warn!("prune: {e}");
                ok = false;
            }
            if ok {
                self.build_tree_flags();
                self.clamp_sel();
                self.poll();
                if let Err(e) = self.save_state() {
                    log::warn!("save: {e}");
                }
                self.notify("refreshed");
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

    /// Run a submitted input. Returns whether it succeeded — the quick-send
    /// path needs to know, to tell a delivered message from a draft it must
    /// keep. Failures are toasted here either way.
    fn submit_input(&mut self, kind: InputKind, text: String) -> bool {
        let res = match kind {
            InputKind::NewFeature => match self.sel {
                Sel::Project(pi) => self.create_feature(pi, &text),
                Sel::Feature(pi, _) => self.create_feature(pi, &text),
                _ => Err(Error::State("select a project first".into())),
            },
            InputKind::AddShell => match self.selected_feature_ctx() {
                Some((pi, fi, _)) => self.add_shell(pi, fi, &text),
                None => Err(Error::State("select a feature first".into())),
            },
            InputKind::QuickSend => match self.selected_shell() {
                Some((pi, fi, si)) => self.quick_send(pi, fi, si, &text),
                None => Err(Error::State("select a shell first".into())),
            },
        };
        if let Err(e) = res {
            self.fail(&e);
            return false;
        }
        true
    }

    fn run_confirm(&mut self, action: ConfirmAction, force: bool) {
        let res = match action {
            ConfirmAction::DeleteFeature { project, feature } => {
                self.delete_feature(project, feature, force)
            }
            ConfirmAction::DeleteShell {
                project,
                feature,
                shell,
            } => self.delete_shell(project, feature, shell),
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
pub fn run(cfg: Config, theme: Theme, tmux: Tmux) -> Result<()> {
    let mut app = App::new(cfg, theme, Some(tmux));
    app.interactive_boot = true;
    app.ascii = crate::util::ascii_requested();
    if app.ascii {
        log::info!("ascii-only rendering enabled (LW_ASCII or C locale)");
    }
    if app.cfg.defaults.agent_hooks {
        match crate::hooks::write_settings_file() {
            Ok(p) => {
                log::info!("agent hooks enabled: {}", p.display());
                app.hook_settings = Some(p);
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
        eprintln!("lazywingman: startup failed: {e}");
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
        // Standalone attach: hand the terminal over to `tmux attach` and
        // resume the TUI when the user detaches.
        if let Some(session) = app.pending_attach.take() {
            let res = (|| -> Result<()> {
                term_restore();
                let tmux = app.require_tmux()?;
                let r = tmux.attach_handoff(&session);
                term_init();
                r
            })();
            if let Err(e) = res {
                app.notify_err(format!("attach handoff failed: {e}"));
                log::error!("attach handoff failed: {e}");
            } else {
                log::info!("returned from tmux attach ({session})");
            }
            continue;
        }
        terminal.draw(|f| crate::ui::draw(f, app))?;
        if app.quit {
            break;
        }
        // Background worktree discovery: one project per iteration, so a
        // scope with dozens of repos fills the tree in over the next few
        // frames instead of freezing before the first one.
        let loading = app.drain_pending_worktrees();
        if app.last_poll.elapsed() >= Duration::from_millis(app.cfg.poll_interval_ms) {
            app.poll();
        }
        // wait for input, at most until the next poll is due — but never
        // while there is still a project to load: that work owes a redraw.
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
    use crate::state::{Feature, Project, Shell};
    use crate::theme;
    use crate::tmux::testutil::{tmux_inside, tmux_standalone, with_tmux_server};
    use std::path::Path;

    fn app_with(tmux: Tmux) -> App {
        let mut app = App::new(
            Config::default(),
            theme::load("ansi", false).unwrap(),
            Some(tmux),
        );
        // Tests seed projects under /tmp without going through
        // boot_discovery — make the whole filesystem the scope.
        app.scope_root = PathBuf::from("/");
        app
    }

    #[test]
    fn spawn_command_is_instrumented_when_hooks_are_on() {
        let mut app = App::new(Config::default(), theme::load("ansi", false).unwrap(), None);
        app.hook_settings = Some(std::path::PathBuf::from(
            "/s/lazywingman-activity-hooks.json",
        ));
        assert_eq!(
            app.agent_command_for_spawn("claude"),
            "claude --settings '/s/lazywingman-activity-hooks.json'"
        );
        assert_eq!(app.agent_command_for_spawn("codex exec"), "codex exec");
    }

    #[test]
    fn spawn_command_is_untouched_when_hooks_are_off() {
        let mut app = App::new(Config::default(), theme::load("ansi", false).unwrap(), None);
        app.hook_settings = None;
        assert_eq!(app.agent_command_for_spawn("claude"), "claude");
    }

    /// Seed state with one project whose agent session and first shell
    /// window really exist on the test tmux server.
    fn seed_project_with_shell(tmux: &Tmux, app: &mut App) -> (String, String) {
        let w = tmux
            .new_session("wingman-session-p", "lw:feat/0", Path::new("/tmp"), "bash")
            .unwrap();
        app.state.projects.push(Project {
            name: "p".into(),
            path: PathBuf::from("/tmp/p"),
            base_branch: "master".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: Some("wingman-session-p".into()),
            missing: false,
            features: vec![Feature {
                name: "feat".into(),
                branch: "feat".into(),
                worktree_path: PathBuf::from("/tmp/p-wt"),
                created_at: "2026-08-16".into(),
                shells: vec![Shell {
                    index: 0,
                    window_name: "lw:feat/0".into(),
                    window_id: Some(w.window_id.clone()),
                    pane_id: Some(w.pane_id.clone()),
                }],
            }],
        });
        (w.window_id, w.pane_id)
    }

    /// `agent_hooks = false` must be a real kill switch: even a pane that
    /// still carries a live `@lw_agent` overlay (written by an earlier
    /// spawn, or by a second LazyWingman instance) must classify purely
    /// from `capture-pane`, exactly as README/STATE promise. The overlay
    /// says `blocked`, which a foreground `cat` (not a shell, so the
    /// login-shell guard alone would not save this test) would otherwise
    /// win outright.
    #[test]
    fn agent_hooks_off_ignores_a_live_overlay_during_poll() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_standalone(sock);
            let mut app = app_with(tmux.clone());
            app.cfg.defaults.agent_hooks = false;
            let (_wid, pid) = seed_project_with_shell(&tmux, &mut app);
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

    /// M8.2 — the standalone handoff path, verified at the command level:
    /// attach must preselect the shell's window clientless (so `tmux attach`
    /// lands on it) and flag the event loop to hand the terminal over.
    #[test]
    fn standalone_attach_preselects_shell_window_and_requests_handoff() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_standalone(sock);
            let mut app = app_with(tmux.clone());
            let (wid, _pid) = seed_project_with_shell(&tmux, &mut app);

            app.attach(0, 0, 0);

            // the shell's window was preselected clientless...
            let active = tmux
                .run_raw(&[
                    "display-message",
                    "-p",
                    "-t",
                    "wingman-session-p",
                    "#{window_id}",
                ])
                .unwrap();
            assert_eq!(
                active.trim(),
                wid,
                "standalone attach must preselect the shell's window"
            );
            // ...and the event loop is told to run `tmux attach -t <session>`
            assert_eq!(
                app.pending_attach.as_deref(),
                Some("wingman-session-p"),
                "standalone attach must schedule the terminal handoff"
            );
        });
    }

    /// M8.2 — inside tmux the dashboard switches the client instead; it must
    /// never schedule a terminal handoff.
    #[test]
    fn inside_attach_never_requests_handoff() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_inside(sock);
            let mut app = app_with(tmux.clone());
            let (_wid, _pid) = seed_project_with_shell(&tmux, &mut app);

            app.attach(0, 0, 0);

            // switch-client needs a client, which headless tests don't have —
            // the point here is only that the standalone branch is NOT taken.
            assert!(
                app.pending_attach.is_none(),
                "inside-tmux attach must not hand the terminal over"
            );
        });
    }

    /// M8.2 — a standalone dashboard must boot even when no tmux server
    /// exists yet: startup auto-starts one instead of failing on
    /// "no server running".
    #[test]
    fn standalone_startup_boots_without_a_server() {
        // Isolate XDG: this test asserts an empty project list, and other
        // tests persist state to the real path when run in parallel.
        crate::util::testutil::with_fresh_xdg(|| {
            crate::tmux::testutil::with_no_server(|sock| {
                let tmux = tmux_standalone(sock);
                let mut app = app_with(tmux);
                app.startup().unwrap();
                assert!(app.state.projects.is_empty());
                // the server now exists and is supervised
                let t = app.tmux.as_ref().unwrap();
                assert!(!t.session_exists("wingman-session-p"));
                assert!(t.list_sessions().unwrap().is_empty());
            });
        });
    }

    // ---- M8.3 — project discovery ----

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
    /// projects, and is idempotent. No cache — the scan is live.
    #[test]
    fn discover_and_import_adds_new_projects_and_is_idempotent() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            repo_with_commit(r, "repo-a");
            repo_with_commit(r, "repo-b");
            std::fs::create_dir_all(r.join("node_modules/dep/.git")).unwrap();

            let mut app = app_with(tmux_standalone("unused"));
            let n = app.discover_and_import(r).unwrap();
            assert_eq!(n, 2);
            assert_eq!(app.state.projects.len(), 2);
            let names: Vec<&str> = app.state.projects.iter().map(|p| p.name.as_str()).collect();
            assert!(names.contains(&"repo-a") && names.contains(&"repo-b"));

            // idempotent: re-scan adds nothing
            let n2 = app.discover_and_import(r).unwrap();
            assert_eq!(n2, 0);
            assert_eq!(app.state.projects.len(), 2);
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

            let mut app = app_with(tmux_standalone("unused"));
            app.boot_discovery_from(r).unwrap();
            // No prompt — the scan just ran and imported the scope.
            assert!(matches!(app.mode, Mode::Normal));
            assert_eq!(app.state.projects.len(), 2);
            assert_eq!(app.scope_root, crate::util::normalize_path(r));
            // Two projects → not flattened, both unfolded; the first
            // project's worktrees load at boot and the second's is queued.
            assert!(!app.flatten);
            assert!(app.visible_projects().len() == 2);
            assert_eq!(app.pending_worktrees.len(), 1, "the second is queued");
            while app.drain_pending_worktrees() {}
            assert!(
                app.worktrees_loaded.iter().all(|f| *f),
                "draining the queue loads every unfolded project"
            );
            // the import is persisted — a restart must not lose it
            let loaded = State::load().unwrap();
            assert_eq!(loaded.projects.len(), 2, "scan imports must be saved");
        });
    }

    /// Expanding a project lazily loads its worktrees (once per run).
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

            let mut app = app_with(tmux_standalone("unused"));
            app.boot_discovery_from(r).unwrap();
            assert!(!app.flatten, "6 projects → not flattened");
            assert_eq!(
                app.worktrees_loaded.iter().filter(|f| **f).count(),
                1,
                "boot loads the first project only; the rest wait in the queue"
            );
            // Expanding a project the drain has not reached yet loads ITS
            // worktrees there and then, once.
            let pi = *app.pending_worktrees.back().unwrap();
            app.proj_expanded[pi] = false; // ensure collapsed
            app.sel = Sel::Project(pi);
            app.expand_or_attach();
            assert!(app.worktrees_loaded[pi], "expand loads worktrees");
            assert_eq!(
                app.worktrees_loaded.iter().filter(|f| **f).count(),
                2,
                "only the boot project and the expanded one"
            );
            // collapsing + expanding again does not re-run the listing
            app.proj_expanded[pi] = false;
            app.expand_or_attach();
            assert_eq!(
                app.worktrees_loaded.iter().filter(|f| **f).count(),
                2,
                "loaded exactly once"
            );
            // …and the background drain skips what expanding already did
            while app.drain_pending_worktrees() {}
            assert!(app.worktrees_loaded.iter().all(|f| *f));
        });
    }

    /// Everything unfolds by default up to the cap: >30 projects stay
    /// collapsed (the tree would be unreadable and every row costs a
    /// `git worktree list`), ≤30 expand.
    #[test]
    fn many_projects_collapse_by_default_few_expand() {
        let mut app = app_with(tmux_standalone("unused"));
        app.scope_root = PathBuf::from("/");
        for i in 0..31 {
            app.state.projects.push(Project {
                name: format!("p{i}"),
                path: PathBuf::from(format!("/tmp/p{i}")),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: None,
                missing: false,
                features: vec![],
            });
        }
        app.build_tree_flags();
        // 31 projects → collapsed, not flattened
        assert!(!app.flatten);
        assert!(app.proj_expanded.iter().all(|e| !*e));

        let mut few = app_with(tmux_standalone("unused"));
        few.scope_root = PathBuf::from("/");
        for i in 0..30 {
            few.state.projects.push(Project {
                name: format!("p{i}"),
                path: PathBuf::from(format!("/tmp/p{i}")),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: None,
                missing: false,
                features: vec![],
            });
        }
        few.build_tree_flags();
        // 30 projects → expanded
        assert!(few.proj_expanded.iter().all(|e| *e));

        // 1 project WITH features → flattened (features are the tree)
        let mut one = app_with(tmux_standalone("unused"));
        one.scope_root = PathBuf::from("/");
        one.state.projects.push(Project {
            name: "p".into(),
            path: PathBuf::from("/tmp/p"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features: vec![Feature {
                name: "f".into(),
                branch: "f".into(),
                worktree_path: PathBuf::from("/tmp/p/f"),
                created_at: "".into(),
                shells: vec![],
            }],
        });
        one.build_tree_flags();
        assert!(one.flatten, "1 project with features → flat");

        // 1 project WITHOUT features → NOT flattened; the row stays so
        // the project is visible instead of a blank tree.
        let mut empty = app_with(tmux_standalone("unused"));
        empty.scope_root = PathBuf::from("/");
        empty.state.projects.push(Project {
            name: "p".into(),
            path: PathBuf::from("/tmp/p"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features: vec![],
        });
        empty.build_tree_flags();
        assert!(!empty.flatten, "no features → keep the project row");
    }

    /// Features unfold by default too — a folded feature hides the very
    /// shells the dashboard exists to supervise, and expanding one costs
    /// nothing (its shells are already in state).
    #[test]
    fn features_are_expanded_by_default() {
        let mut app = app_with(tmux_standalone("unused"));
        app.scope_root = PathBuf::from("/");
        for i in 0..2 {
            app.state.projects.push(Project {
                name: format!("p{i}"),
                path: PathBuf::from(format!("/tmp/p{i}")),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: None,
                missing: false,
                features: vec![Feature {
                    name: "f".into(),
                    branch: "f".into(),
                    worktree_path: PathBuf::from(format!("/tmp/p{i}/f")),
                    created_at: String::new(),
                    shells: vec![],
                }],
            });
        }
        app.build_tree_flags();
        assert!(
            app.feat_expanded.iter().flatten().all(|e| *e),
            "every feature starts unfolded"
        );
    }

    /// Boot does not block on one `git worktree list` per project: only
    /// the first visible project loads synchronously (it is what the first
    /// frame draws), the rest are queued and drained by the event loop.
    #[test]
    fn boot_queues_worktree_loading_instead_of_blocking() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            for i in 0..6 {
                repo_with_commit(r, &format!("repo-{i}"));
            }

            let mut app = app_with(tmux_standalone("unused"));
            app.boot_discovery_from(r).unwrap();
            assert!(
                app.proj_expanded.iter().all(|e| *e),
                "6 projects are under the cap → all unfolded"
            );
            assert_eq!(
                app.worktrees_loaded.iter().filter(|f| **f).count(),
                1,
                "boot loads only the first project's worktrees"
            );
            assert_eq!(app.pending_worktrees.len(), 5, "the rest are queued");

            // The event loop drains one project per iteration.
            let mut steps = 0;
            while app.drain_pending_worktrees() {
                steps += 1;
                assert!(steps <= 10, "draining must terminate");
            }
            assert_eq!(steps, 5, "one project per drain step");
            assert!(
                app.worktrees_loaded.iter().all(|f| *f),
                "every project is loaded once the queue is drained"
            );
        });
    }

    /// Features discovered by a lazy worktree load are unfolded like every
    /// other row: `ensure_worktrees` pushes onto `features`, so the
    /// parallel `feat_expanded` must grow with it (it used to stay short,
    /// and a missing entry reads as folded).
    #[test]
    fn lazily_loaded_features_are_expanded() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            let repo = repo_with_commit(r, "repo-a");
            git(&repo, &["worktree", "add", "-b", "wt", "../repo-a-wt"]);
            repo_with_commit(r, "repo-b");

            let mut app = app_with(tmux_standalone("unused"));
            app.boot_discovery_from(r).unwrap();
            while app.drain_pending_worktrees() {}

            let pi = app
                .visible_projects()
                .into_iter()
                .find(|&i| app.state.projects[i].name == "repo-a")
                .expect("repo-a imported");
            assert_eq!(
                app.state.projects[pi].features.len(),
                1,
                "the linked worktree became a feature"
            );
            assert_eq!(
                app.feat_expanded[pi].len(),
                1,
                "feat_expanded stays parallel to features"
            );
            assert!(app.feat_expanded[pi][0], "the new feature is unfolded");
        });
    }

    /// Out-of-scope projects (other scopes' leftovers in state) stay saved
    /// but never get rows.
    #[test]
    fn scope_filters_projects_from_other_scopes() {
        let mut app = app_with(tmux_standalone("unused"));
        app.scope_root = PathBuf::from("/work/scope-a");
        app.state.projects.push(Project {
            name: "in-a".into(),
            path: PathBuf::from("/work/scope-a/repo-x"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features: vec![],
        });
        app.state.projects.push(Project {
            name: "in-b".into(),
            path: PathBuf::from("/work/scope-b/repo-y"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features: vec![],
        });
        let vis = app.visible_projects();
        assert_eq!(vis, vec![0], "only scope-a's project is visible");
        assert_eq!(app.state.projects.len(), 2, "state keeps both");
        // Missing projects inside the scope still show.
        app.state.projects[0].missing = true;
        assert_eq!(app.visible_projects(), vec![0]);
    }

    /// Boot from inside a git repo auto-imports the MAIN repo (even from a
    /// linked worktree) and adds its worktrees as features with 0 shells.
    #[test]
    fn boot_discovery_auto_imports_cwd_repo_and_its_worktrees() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            let repo = repo_with_commit(r, "proj");
            let wt = r.join("wt-feature-x");
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "feature-x",
                    wt.to_str().unwrap(),
                    "main",
                ],
            );

            let mut app = app_with(tmux_standalone("unused"));
            app.boot_discovery_from(&repo).unwrap();
            assert_eq!(app.state.projects.len(), 1);
            let proj = &app.state.projects[0];
            assert_eq!(proj.path, repo);
            assert_eq!(proj.features.len(), 1, "linked worktree becomes a feature");
            assert_eq!(proj.features[0].name, "feature-x");
            assert_eq!(proj.features[0].branch, "feature-x");
            assert!(proj.features[0].shells.is_empty(), "0 shells");

            // booting again is idempotent (no duplicate features)
            app.boot_discovery_from(&repo).unwrap();
            assert_eq!(app.state.projects[0].features.len(), 1);

            // booting from INSIDE the worktree resolves to the main repo
            let mut app2 = app_with(tmux_standalone("unused"));
            app2.boot_discovery_from(&wt).unwrap();
            assert_eq!(app2.state.projects.len(), 1);
            assert_eq!(app2.state.projects[0].path, repo);
            assert_eq!(app2.state.projects[0].features.len(), 1);
        });
    }

    /// `i` (import a repo by path) and `I` (rescan the discovery root) are
    /// gone: projects come from boot discovery and `[[projects]]` in the
    /// config. Both keys must be inert — no popup, no project.
    #[test]
    fn the_import_and_scan_keys_are_gone() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            repo_with_commit(r, "repo-a");

            let mut app = app_with(tmux_standalone("unused"));
            // A root that HAS a repo to find, so a surviving scan would show.
            app.cfg.discovery.scan_root = r.to_string_lossy().into_owned();
            for k in ['i', 'I'] {
                app.on_key(KeyEvent::new(KeyCode::Char(k), KeyModifiers::NONE));
                assert!(matches!(app.mode, Mode::Normal), "`{k}` opened a mode");
                assert!(app.state.projects.is_empty(), "`{k}` imported a project");
            }
        });
    }

    /// Creating a feature or adding a shell on a missing project is
    /// rejected with a clear error instead of failing deep in git/tmux.
    #[test]
    fn actions_reject_missing_projects() {
        let mut app = app_with(tmux_standalone("unused"));
        app.state.projects.push(Project {
            name: "gone".into(),
            path: PathBuf::from("/definitely/not/here"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: true,
            features: vec![Feature {
                name: "feat".into(),
                branch: "feat".into(),
                worktree_path: PathBuf::from("/definitely/not/here/wt"),
                created_at: "".into(),
                shells: vec![],
            }],
        });
        let err = app.create_feature(0, "new-feat").unwrap_err();
        assert!(err.to_string().contains("missing"), "err: {err}");
        let err = app.add_shell(0, 0, "").unwrap_err();
        assert!(err.to_string().contains("missing"), "err: {err}");
    }

    /// The New Feature popup names the branch the worktree is cut from:
    /// the project's base branch, even when the repo has something else
    /// checked out. Same resolution `create_feature` runs, so the popup
    /// cannot promise a base the creation then ignores.
    #[test]
    fn new_feature_popup_names_the_base_branch_not_the_checkout() {
        crate::util::testutil::with_test_lock(|| {
            let root = tempfile::TempDir::new().unwrap();
            let path = repo_with_commit(root.path(), "repo");
            // the repo sits on an unrelated branch — the popup must ignore it
            git(&path, &["checkout", "-b", "some-other-branch"]);

            let mut app = app_with(tmux_standalone("unused"));
            app.state.projects.push(Project {
                name: "repo".into(),
                path: path.clone(),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: None,
                missing: false,
                features: vec![],
            });
            app.build_tree_flags();
            app.sel = Sel::Project(0);
            app.open_new_feature();

            assert!(matches!(
                app.mode,
                Mode::Input {
                    kind: InputKind::NewFeature,
                    ..
                }
            ));
            let hint = app.new_feature_base().expect("base hint");
            assert_eq!(hint.branch, "main");
            assert!(hint.missing.is_none(), "main exists: {hint:?}");
        });
    }

    /// When the configured base branch is not in the repo, creation falls
    /// back to the current branch — so the popup says so instead of naming
    /// a base that would be a lie.
    #[test]
    fn new_feature_popup_flags_a_missing_base_branch() {
        crate::util::testutil::with_test_lock(|| {
            let root = tempfile::TempDir::new().unwrap();
            let path = repo_with_commit(root.path(), "repo");

            let mut app = app_with(tmux_standalone("unused"));
            app.state.projects.push(Project {
                name: "repo".into(),
                path: path.clone(),
                base_branch: "master".into(), // repo only has `main`
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: None,
                missing: false,
                features: vec![],
            });
            app.build_tree_flags();
            app.sel = Sel::Project(0);
            app.open_new_feature();

            let hint = app.new_feature_base().expect("base hint");
            assert_eq!(hint.branch, "main"); // the fallback: current branch
            assert_eq!(hint.missing.as_deref(), Some("master"));
        });
    }

    /// A missing repo has no base to name: the popup still opens (creation
    /// is what rejects it) but shows no branch line.
    #[test]
    fn new_feature_popup_has_no_base_hint_for_a_missing_project() {
        crate::util::testutil::with_test_lock(|| {
            let mut app = app_with(tmux_standalone("unused"));
            app.state.projects.push(Project {
                name: "gone".into(),
                path: PathBuf::from("/definitely/not/here"),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: None,
                missing: true,
                features: vec![],
            });
            app.build_tree_flags();
            app.sel = Sel::Project(0);
            app.open_new_feature();

            assert!(app.new_feature_base().is_none());
        });
    }

    // ---- M8.4a — accessibility keys ----

    /// Seed a headless app with `n` projects (no tmux needed: navigation
    /// is pure state).
    fn app_with_projects(n: usize) -> App {
        let mut app = app_with(tmux_standalone("unused"));
        for i in 0..n {
            app.state.projects.push(Project {
                name: format!("p{i}"),
                path: PathBuf::from(format!("/tmp/p{i}")),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: None,
                missing: false,
                features: vec![],
            });
        }
        app.build_tree_flags();
        app.clamp_sel();
        app
    }

    /// A project with one feature holding one shell, for action tests.
    fn app_with_shell() -> App {
        let mut app = app_with(tmux_standalone("unused"));
        app.state.projects.push(Project {
            name: "p".into(),
            path: PathBuf::from("/tmp/p"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features: vec![Feature {
                name: "feat".into(),
                branch: "feat".into(),
                worktree_path: PathBuf::from("/tmp/p/.lazywingman-worktrees/p/feat"),
                created_at: String::new(),
                shells: vec![Shell {
                    index: 0,
                    window_name: "lw:feat/0".into(),
                    window_id: Some("@1".into()),
                    pane_id: Some("%1".into()),
                }],
            }],
        });
        app.build_tree_flags();
        // One shell → the feature row is folded onto it, which is the only
        // way this shell is reachable in the tree.
        app.sel = Sel::Feature(0, 0);
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

    /// A half-written prompt is not lost by leaving the editor: each
    /// shell keeps its own draft, restored (cursor included) the next time
    /// quick-send opens on it.
    #[test]
    fn quick_send_drafts_are_kept_per_shell() {
        let mut app = app_with_shell();
        app.viewport = (120, 40);
        app.state.projects[0].features[0].shells.push(Shell {
            index: 1,
            window_name: "lw:feat/1".into(),
            window_id: Some("@2".into()),
            pane_id: Some("%2".into()),
        });
        app.build_tree_flags();

        let typed = |app: &mut App, text: &str| {
            app.on_key(key(KeyCode::Char('s'), false));
            for c in text.chars() {
                app.on_key(key(KeyCode::Char(c), false));
            }
            app.on_key(key(KeyCode::Esc, false));
        };

        app.sel = Sel::Shell(0, 0, 0);
        typed(&mut app, "hello");
        app.sel = Sel::Shell(0, 0, 1);
        app.on_key(key(KeyCode::Char('s'), false));
        let Mode::Textarea { buf } = &app.mode else {
            panic!("quick-send must open");
        };
        assert_eq!(buf.text(), "", "another shell starts from a blank draft");
        app.on_key(key(KeyCode::Esc, false));
        typed(&mut app, "world");

        app.sel = Sel::Shell(0, 0, 0);
        app.on_key(key(KeyCode::Char('s'), false));
        let Mode::Textarea { buf } = &app.mode else {
            panic!("quick-send must open");
        };
        assert_eq!(buf.text(), "hello", "the first shell's draft came back");
        assert_eq!(buf.cursor_col(), 5, "and the cursor sits where it was left");
    }

    /// A send that failed (dead pane, no tmux) must not swallow the text —
    /// the draft is what the user would have to retype.
    #[test]
    fn a_failed_send_keeps_the_draft() {
        // `tmux_standalone("unused")` has no server: `paste_keys` errors.
        let mut app = app_with_shell();
        app.viewport = (120, 40);
        app.on_key(key(KeyCode::Char('s'), false));
        for c in "retry me".chars() {
            app.on_key(key(KeyCode::Char(c), false));
        }
        app.on_key(key(KeyCode::Char('s'), true));
        assert!(matches!(app.mode, Mode::Normal), "the editor closes");

        app.on_key(key(KeyCode::Char('s'), false));
        let Mode::Textarea { buf } = &app.mode else {
            panic!("quick-send must open");
        };
        assert_eq!(buf.text(), "retry me", "a failed send keeps the text");
    }

    /// A delivered message is not a draft any more.
    #[test]
    fn a_delivered_draft_is_cleared() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_standalone(sock);
            let mut app = app_with(tmux.clone());
            seed_project_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.viewport = (120, 40);
            app.sel = Sel::Feature(0, 0);
            app.clamp_sel();

            app.on_key(key(KeyCode::Char('s'), false));
            for c in "ship it".chars() {
                app.on_key(key(KeyCode::Char(c), false));
            }
            app.on_key(key(KeyCode::Char('s'), true));
            let toast = app.toast().map(|t| t.text.clone()).unwrap_or_default();
            assert!(
                !toast.contains("failed"),
                "the send must succeed: {toast:?}"
            );

            app.on_key(key(KeyCode::Char('s'), false));
            let Mode::Textarea { buf } = &app.mode else {
                panic!("quick-send must open");
            };
            assert_eq!(buf.text(), "", "a sent message leaves no draft");
        });
    }

    /// Typing non-ASCII must not blow up the TUI. `col` is a BYTE offset
    /// (the popup slices `line[..col]` to place the cursor), so advancing
    /// it by one per char leaves it mid-character and the next edit hits a
    /// non-char-boundary panic.
    #[test]
    fn the_quick_send_buffer_handles_non_ascii() {
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

    /// Drafts are unbounded in practice but not in principle: the buffer
    /// refuses to grow past the cap rather than eating memory.
    #[test]
    fn the_quick_send_buffer_stops_at_the_cap() {
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

    /// The nominal case is one worktree, one agent: a feature with a
    /// single shell is folded — it gets no child row (whatever its expand
    /// flag says) and the selection resolves to that shell. A second shell
    /// unfolds the subtree again.
    #[test]
    fn solo_feature_folds_and_a_second_shell_unfolds_it() {
        let mut app = app_with_shell();
        assert_eq!(app.solo_shell(0, 0), Some(0));
        // even flagged expanded, a folded feature emits no shell row
        app.feat_expanded[0][0] = true;
        let nodes = app.visible_nodes();
        assert!(
            !nodes.iter().any(|n| matches!(n, Sel::Shell(..))),
            "a one-shell feature must not emit a child row: {nodes:?}"
        );
        // ...and the feature row acts as that shell
        assert_eq!(app.sel, Sel::Feature(0, 0));
        assert_eq!(app.selected_shell(), Some((0, 0, 0)));
        assert_eq!(app.selected_shell_pane().as_deref(), Some("%1"));

        // a second shell brings the subtree back
        app.state.projects[0].features[0].shells.push(Shell {
            index: 1,
            window_name: "lw:feat/1".into(),
            window_id: Some("@2".into()),
            pane_id: Some("%2".into()),
        });
        assert_eq!(app.solo_shell(0, 0), None);
        let nodes = app.visible_nodes();
        assert!(
            nodes.contains(&Sel::Shell(0, 0, 0)) && nodes.contains(&Sel::Shell(0, 0, 1)),
            "two shells must show both rows: {nodes:?}"
        );
        // the feature row is no longer a shell — it expands again
        assert_eq!(app.selected_shell(), None);
    }

    /// `d` is contextual: on a shell row it closes that shell, on a
    /// feature row it deletes the feature. Closing a shell must never
    /// offer to take the feature (and its sibling shells) with it.
    #[test]
    fn d_on_a_shell_confirms_closing_only_that_shell() {
        let mut app = app_with_shell();
        app.state.projects[0].features[0].shells.push(Shell {
            index: 1,
            window_name: "lw:feat/1".into(),
            window_id: Some("@2".into()),
            pane_id: Some("%2".into()),
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
                            project: 0,
                            feature: 0,
                            shell: 1
                        }
                    ),
                    "d on a shell must confirm closing that shell: {:?}",
                    confirm.action
                );
                assert!(
                    confirm.body.contains("lw:feat/1"),
                    "the confirm names the shell: {:?}",
                    confirm.body
                );
            }
            m => panic!("expected a confirm, got {m:?}"),
        }

        // the feature row keeps the feature-level delete
        app.mode = Mode::Normal;
        app.sel = Sel::Feature(0, 0);
        app.on_key(key(KeyCode::Char('d'), false));
        match &app.mode {
            Mode::Confirm { confirm } => assert!(
                matches!(confirm.action, ConfirmAction::DeleteFeature { .. }),
                "d on a feature still deletes the feature: {:?}",
                confirm.action
            ),
            m => panic!("expected a confirm, got {m:?}"),
        }
    }

    /// The confirm popup takes `d` as a yes: `dd` deletes the way vim
    /// does, without moving the hand off the key that opened the popup.
    #[test]
    fn d_confirms_the_delete_popup() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_standalone(sock);
            let mut app = app_with(tmux.clone());
            seed_project_with_shell(&tmux, &mut app);
            let w2 = tmux
                .new_window("wingman-session-p", "lw:feat/1", Path::new("/tmp"), "bash")
                .unwrap();
            app.state.projects[0].features[0].shells.push(Shell {
                index: 1,
                window_name: "lw:feat/1".into(),
                window_id: Some(w2.window_id.clone()),
                pane_id: Some(w2.pane_id.clone()),
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
                app.state.projects[0].features[0].shells.len(),
                1,
                "dd closed the selected shell"
            );
        });
    }

    /// Closing a shell kills that shell's window and nothing else: the
    /// feature, its worktree entry and the sibling shell survive.
    #[test]
    fn delete_shell_kills_one_window_and_keeps_the_feature() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_standalone(sock);
            let mut app = app_with(tmux.clone());
            seed_project_with_shell(&tmux, &mut app);
            let w2 = tmux
                .new_window("wingman-session-p", "lw:feat/1", Path::new("/tmp"), "bash")
                .unwrap();
            app.state.projects[0].features[0].shells.push(Shell {
                index: 1,
                window_name: "lw:feat/1".into(),
                window_id: Some(w2.window_id.clone()),
                pane_id: Some(w2.pane_id.clone()),
            });
            app.build_tree_flags();
            app.sel = Sel::Shell(0, 0, 1);

            // save_state() writes to XDG_STATE_HOME — keep it off the
            // developer's real state.json (the shared helper is the rule).
            crate::util::testutil::with_fresh_xdg(|| app.delete_shell(0, 0, 1).unwrap());

            assert_eq!(
                app.state.projects[0].features.len(),
                1,
                "the feature must survive closing one of its shells"
            );
            let feat = &app.state.projects[0].features[0];
            assert_eq!(feat.shells.len(), 1, "only the closed shell is dropped");
            assert_eq!(feat.shells[0].index, 0);
            let windows = tmux.list_windows("wingman-session-p").unwrap();
            assert!(
                windows.iter().any(|w| w.window_name == "lw:feat/0"),
                "the sibling shell's window stays: {windows:?}"
            );
            assert!(
                !windows.iter().any(|w| w.window_name == "lw:feat/1"),
                "the closed shell's window is killed: {windows:?}"
            );
        });
    }

    /// `l`/Enter on a folded feature attaches instead of expanding, and
    /// `h` walks up to the project rather than toggling a flag nothing
    /// renders.
    #[test]
    fn enter_on_a_folded_feature_attaches_its_shell() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_standalone(sock);
            let mut app = app_with(tmux.clone());
            seed_project_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.sel = Sel::Feature(0, 0);

            app.expand_or_attach();
            assert_eq!(
                app.pending_attach.as_deref(),
                Some("wingman-session-p"),
                "Enter on a folded feature must attach its shell"
            );
            assert!(
                !app.visible_nodes().contains(&Sel::Shell(0, 0, 0)),
                "a folded feature must never grow a child row"
            );

            app.pending_attach = None;
            app.feat_expanded[0][0] = true; // stale flag must not eat the key
            app.flatten = false; // `h` reaches a parent only when one has a row
            app.collapse_or_up();
            assert_eq!(app.sel, Sel::Project(0), "h walks up from a folded feature");
        });
    }

    /// `s` on a folded feature row opens the textarea and the send lands on
    /// its shell — the row IS the agent.
    #[test]
    fn quick_send_works_from_a_folded_feature_row() {
        let mut app = app_with_shell();
        app.viewport = (120, 40);
        assert_eq!(app.sel, Sel::Feature(0, 0));
        app.on_key(key(KeyCode::Char('s'), false));
        assert!(
            matches!(app.mode, Mode::Textarea { .. }),
            "s on a folded feature must open the textarea"
        );
        // no tmux here: the send fails at the tmux call, not at the
        // selection — "select a shell first" would mean the row did not
        // resolve.
        app.mode = Mode::Normal;
        app.submit_input(InputKind::QuickSend, "hi".into());
        let msg = app.toast().map(|t| t.text.clone()).unwrap_or_default();
        assert!(
            !msg.contains("select a shell"),
            "the folded row must resolve to its shell, got: {msg:?}"
        );
    }

    /// PageDown / PageUp and ctrl-d / ctrl-u move half a page; ctrl-f / b
    /// a full page; Home / End and g / G jump to first / last; arrows are
    /// the same code path as j/k.
    #[test]
    fn accessibility_motions_work_in_normal_mode() {
        let mut app = app_with_projects(20);
        app.viewport = (120, 40);
        app.on_key(key(KeyCode::Char('j'), false));
        assert_eq!(app.sel, Sel::Project(1));

        // arrows = the same code path as j/k
        app.on_key(key(KeyCode::Up, false));
        assert_eq!(app.sel, Sel::Project(0));

        // half page down: (40-3)/2 = 18 rows
        app.on_key(key(KeyCode::PageDown, false));
        assert_eq!(app.sel, Sel::Project(18));
        app.on_key(key(KeyCode::PageUp, false));
        assert_eq!(app.sel, Sel::Project(0));

        // ctrl-d / ctrl-u = PageDown/PageUp spellings
        app.on_key(key(KeyCode::Char('d'), true));
        assert_eq!(app.sel, Sel::Project(18));
        app.on_key(key(KeyCode::Char('u'), true));
        assert_eq!(app.sel, Sel::Project(0));

        // ctrl-f / ctrl-b = full page
        app.on_key(key(KeyCode::Char('f'), true));
        assert_eq!(app.sel, Sel::Project(19), "full page down clamps to last");
        app.on_key(key(KeyCode::Char('b'), true));
        assert_eq!(app.sel, Sel::Project(0));

        // Home / End = g / G
        app.on_key(key(KeyCode::End, false));
        assert_eq!(app.sel, Sel::Project(19));
        app.on_key(key(KeyCode::Home, false));
        assert_eq!(app.sel, Sel::Project(0));
        app.on_key(key(KeyCode::Char('G'), false));
        assert_eq!(app.sel, Sel::Project(19));
        app.on_key(key(KeyCode::Char('g'), false));
        assert_eq!(app.sel, Sel::Project(0));
    }

    /// The ctrl/accessibility layer dispatches ABOVE text input, so
    /// ctrl-d / PageDown still move the tree while a popup is open — and
    /// ctrl-c quits from any state (it used to type a 'c' into the input).
    #[test]
    fn ctrl_layer_works_over_text_input() {
        let mut app = app_with_projects(20);
        app.viewport = (120, 40);
        app.mode = Mode::Input {
            kind: InputKind::NewFeature,
            text: "abc".into(),
        };
        app.on_key(key(KeyCode::Char('d'), true));
        assert_eq!(app.sel, Sel::Project(18), "ctrl-d moves behind the popup");
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
        let mut app = app_with_projects(5);
        app.on_key(key(KeyCode::Char('G'), false));
        assert_eq!(app.sel, Sel::Project(4));
        app.mode = Mode::Textarea {
            buf: TextBuf::new(),
        };
        app.on_key(key(KeyCode::Char('g'), false));
        let Mode::Textarea { buf } = &app.mode else {
            panic!()
        };
        assert_eq!(buf.text(), "g");
    }

    // ---- M9.2 — multiline textarea (quick-send rework) ----

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

    /// The textarea: plain Enter inserts a newline, Alt+Enter sends,
    /// Esc cancels, Tab inserts a tab, arrows navigate.
    #[test]
    fn textarea_enter_newline_send_chord_cancel() {
        let mut app = app_with_shell();
        app.viewport = (120, 40);

        // `s` opens the textarea
        app.on_key(key(KeyCode::Char('s'), false));
        assert!(
            matches!(app.mode, Mode::Textarea { .. }),
            "s opens textarea"
        );

        // type two lines
        for c in "line one".chars() {
            app.on_key(key(KeyCode::Char(c), false));
        }
        app.on_key(key(KeyCode::Enter, false));
        for c in "line two".chars() {
            app.on_key(key(KeyCode::Char(c), false));
        }
        let Mode::Textarea { buf } = &app.mode else {
            panic!("still in textarea");
        };
        assert_eq!(buf.text(), "line one\nline two", "Enter inserts newline");

        // Alt+Enter sends: back to Normal, message + tree unchanged
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert!(matches!(app.mode, Mode::Normal), "Alt+Enter sends");
    }

    #[test]
    fn textarea_ctrl_enter_sends_and_esc_cancels() {
        let mut app = app_with_shell();
        app.viewport = (120, 40);

        app.on_key(key(KeyCode::Char('s'), false));
        app.on_key(key(KeyCode::Char('x'), false));
        // Ctrl+Enter sends too
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Normal), "Ctrl+Enter sends");

        // Esc cancels without sending
        app.on_key(key(KeyCode::Char('s'), false));
        app.on_key(key(KeyCode::Char('y'), false));
        app.on_key(key(KeyCode::Esc, false));
        assert!(matches!(app.mode, Mode::Normal), "Esc cancels");
    }

    /// Regression: the visible window is the popup's content height, not
    /// one line — typing Enter must not scroll every earlier line out of
    /// the textarea (M9.2 hardcoded `ensure_visible(1)`).
    #[test]
    fn textarea_enter_keeps_earlier_lines_visible() {
        let mut app = app_with_shell();
        app.viewport = (120, 40);
        let visible = textarea_visible_lines(40);
        assert!(visible >= 3, "the test needs a roomy window, got {visible}");

        app.on_key(key(KeyCode::Char('s'), false));
        for _ in 0..3 {
            app.on_key(key(KeyCode::Char('x'), false));
            app.on_key(key(KeyCode::Enter, false));
        }
        let Mode::Textarea { buf } = &app.mode else {
            panic!("still in textarea");
        };
        assert_eq!(buf.cursor_row(), 3);
        assert_eq!(
            buf.scroll(),
            0,
            "4 lines fit in {visible} rows — nothing scrolls away"
        );

        // one line past the window scrolls by exactly one
        for _ in 0..(visible - 3) {
            app.on_key(key(KeyCode::Enter, false));
        }
        let Mode::Textarea { buf } = &app.mode else {
            panic!("still in textarea");
        };
        assert_eq!(buf.cursor_row(), visible);
        assert_eq!(buf.scroll(), 1);
    }

    /// ctrl-s is the send key that survives a legacy terminal: tmux with
    /// `extended-keys off` cannot deliver Ctrl+Enter, and Alt+Enter is
    /// often eaten by the outer terminal emulator.
    #[test]
    fn textarea_ctrl_s_sends() {
        let mut app = app_with_shell();
        app.viewport = (120, 40);

        app.on_key(key(KeyCode::Char('s'), false));
        app.on_key(key(KeyCode::Char('x'), false));
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(app.mode, Mode::Normal), "ctrl-s sends");
    }

    #[test]
    fn textarea_tab_and_navigation_do_not_leak_to_tree() {
        let mut app = app_with_shell();
        app.viewport = (120, 40);

        app.on_key(key(KeyCode::Char('s'), false));
        let before = app.sel;
        app.on_key(key(KeyCode::Tab, false));
        app.on_key(key(KeyCode::Down, false));
        app.on_key(key(KeyCode::PageDown, false));
        app.on_key(key(KeyCode::Home, false));
        let Mode::Textarea { buf } = &app.mode else {
            panic!("still in textarea");
        };
        assert!(buf.text().starts_with('\t'), "Tab inserts a tab");
        assert_eq!(buf.cursor_row(), 0, "Home did not move the tree");
        assert_eq!(
            app.sel, before,
            "tree selection untouched while textarea open"
        );
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
        let mut app = app_with_projects(5);
        app.viewport = (120, 40);
        // row 3 in the tree (below the top border), inside the tree column
        app.on_mouse(click(5, 4));
        assert_eq!(app.sel, Sel::Project(3));
        // double-click on the same row toggles expansion (projects start
        // expanded, so the toggle collapses it)
        app.on_mouse(click(5, 4));
        assert!(
            !app.proj_expanded[3],
            "double-click must toggle the project expansion"
        );
    }

    /// Regression: the event loop must route a mouse event to `on_mouse`.
    /// It used to `read()` inside a `let Event::Key(..)` chain, which
    /// consumed mouse events and then dropped them on the floor, so every
    /// click was silently lost.
    #[test]
    fn event_dispatch_routes_mouse_events() {
        let mut app = app_with_projects(5);
        app.viewport = (120, 40);
        dispatch_event(&mut app, Event::Mouse(click(5, 4)));
        assert_eq!(
            app.sel,
            Sel::Project(3),
            "a mouse event must reach on_mouse through the event loop"
        );
    }

    /// The wheel scrolls the tree (its only scrollable surface) without a
    /// click; clicks outside the tree column do nothing.
    #[test]
    fn mouse_wheel_scrolls_and_right_panels_are_dead() {
        let mut app = app_with_projects(10);
        app.viewport = (120, 40);
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.sel, Sel::Project(3));
        let before = app.sel;
        app.on_mouse(click(60, 5)); // detail panel area
        assert_eq!(app.sel, before, "detail panel is not selectable");
    }

    /// Clicking a statusbar shortcut entry runs its action (a context bar
    /// is clickable — discoverability rule).
    #[test]
    fn mouse_click_on_bar_entry_runs_action() {
        let mut app = app_with_projects(3);
        app.viewport = (120, 40);
        // render once so the renderer computes the bar hit ranges
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert!(!app.bar_hits.is_empty());
        assert_eq!(app.sel, Sel::Project(0));
        // a project's bar has "n feature" → the new-feature input
        let (x0, x1, _) = app
            .bar_hits
            .iter()
            .find(|(_, _, a)| *a == BarAction::NewFeature)
            .copied()
            .expect("bar must offer 'n feature' for a project");
        app.on_mouse(click((x0 + x1) / 2, 39));
        assert!(
            matches!(
                app.mode,
                Mode::Input {
                    kind: InputKind::NewFeature,
                    ..
                }
            ),
            "clicking 'n feature' must open the input"
        );
    }

    /// Clicking outside an open overlay dismisses it; clicking inside does
    /// not.
    #[test]
    fn mouse_click_outside_overlay_dismisses_it() {
        let mut app = app_with_projects(3);
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
        let mut app = app_with_projects(3);
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
        let mut app = app_with_projects(n);
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
        // scroll a long way down
        for _ in 0..100 {
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
        let mut app = app_with_projects(20);
        app.viewport = (120, 40);
        app.on_key(key(KeyCode::PageDown, false));
        assert_eq!(app.sel, Sel::Project(18));
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
        let mut app = app_with_projects(3);
        if theme_name != app.theme.name {
            app.theme = theme::load(theme_name, false).unwrap();
        }
        app.open_theme_picker();
        app
    }

    /// ctrl-t opens the picker on the CURRENT theme with built-in names
    /// (Catppuccin first, per the theming rule that the first rows are the
    /// same in every project); the cursor sits on the active theme.
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

    /// Enter keeps the previewed theme: it becomes the app theme and the
    /// pick persists to STATE (never rewrites the user's config file);
    /// reopening the app honours it.
    #[test]
    fn picker_enter_keeps_and_persists_theme() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut app = app_with_picker("catppuccin-mocha");
            app.on_key(key(KeyCode::Char('j'), false)); // preview a different one
            let picked = app.theme.name.clone();
            app.on_key(key(KeyCode::Enter, false));
            assert!(matches!(app.mode, Mode::Normal), "picker closes on Enter");
            assert_eq!(app.theme.name, picked);
            assert_eq!(app.state.theme.as_deref(), Some(picked.as_str()));
            // persisted state file carries the pick (state dir is fresh XDG)
            let loaded = crate::state::State::load().unwrap();
            assert_eq!(loaded.theme.as_deref(), Some(picked.as_str()));
        });
    }

    /// Esc restores the theme the picker opened with, even after previews,
    /// and does not persist anything.
    #[test]
    fn picker_esc_restores_original() {
        let mut app = app_with_picker("catppuccin-mocha");
        app.on_key(key(KeyCode::Char('j'), false));
        assert_ne!(app.theme.name, "catppuccin-mocha");
        app.on_key(key(KeyCode::Esc, false));
        assert_eq!(app.theme.name, "catppuccin-mocha", "Esc restores the theme");
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.state.theme, None, "nothing persisted on cancel");
        assert!(app.picker_original.is_empty(), "restore consumed the name");
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
        assert_eq!(app.sel, Sel::Project(0));
    }

    /// Mouse: clicking a theme row selects + previews it, double-click
    /// keeps it, click outside restores the original (Esc semantics).
    #[test]
    fn picker_mouse_selects_and_double_click_keeps() {
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
        assert_eq!(
            app.state.theme.as_deref(),
            Some(app.theme_names[1].as_str())
        );
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
        app.sel = Sel::Project(0);
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
        let visible = app.popup_area.unwrap().height.saturating_sub(2) as usize;
        assert_eq!(app.diag_scroll, total.saturating_sub(visible));
        // the tree selection never moved while the overlay was open
        assert_eq!(app.sel, Sel::Project(0));
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
        app.sel = Sel::Project(0);
        let before = app.sel;
        app.on_key(key(KeyCode::PageDown, false));
        assert!(app.diag_scroll > 0, "PageDown scrolls the diagnostics");
        app.on_key(key(KeyCode::Home, false));
        assert_eq!(app.diag_scroll, 0);
        app.on_key(key(KeyCode::End, false));
        let total = crate::ui::diag_line_count(&app);
        let visible = app.popup_area.unwrap().height.saturating_sub(2) as usize;
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
        app.sel = Sel::Project(0);
        app.on_key(key(KeyCode::Char('p'), false));
        assert!(matches!(app.mode, Mode::Normal), "no modal on a project");
        assert!(
            app.message
                .as_ref()
                .is_some_and(|m| m.text.contains("select a shell")),
            "a hint is shown instead"
        );

        app.sel = Sel::Shell(0, 0, 0);
        app.on_key(key(KeyCode::Char('p'), false));
        assert!(
            matches!(app.mode, Mode::Messages { index: 0 }),
            "p opens the modal on the first entry"
        );
    }

    /// A folded one-shell feature row IS its shell (M11), so `p` opens
    /// the modal there exactly like `s` sends from it.
    #[test]
    fn p_opens_from_a_folded_feature_row() {
        let mut app = app_with_shell();
        app.sel = Sel::Feature(0, 0);
        assert!(
            app.solo_shell(0, 0).is_some(),
            "the fixture's feature is folded"
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
        app.on_key(key(KeyCode::Char('p'), false));
        app.on_key(key(KeyCode::Char('j'), false));
        assert!(matches!(app.mode, Mode::Messages { index: 1 }));
        // clamped at the end of the list
        app.on_key(key(KeyCode::Char('j'), false));
        assert!(matches!(app.mode, Mode::Messages { index: 1 }));
        app.on_key(key(KeyCode::Char('k'), false));
        assert!(matches!(app.mode, Mode::Messages { index: 0 }));
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
                .is_some_and(|m| m.text.contains("no predefined messages")),
        );
    }

    /// Enter sends the selected entry to the shell's pane — the same paste
    /// path as `s`, so the agent sees exactly what a human would have typed.
    #[test]
    fn predefined_modal_enter_sends_the_selected_text() {
        with_tmux_server(|sock, _tmux| {
            let tmux = tmux_standalone(sock);
            let mut app = app_with(tmux.clone());
            let (_wid, pid) = seed_project_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.sel = Sel::Shell(0, 0, 0);
            app.cfg.messages = vec![
                crate::config::MessageConfig {
                    label: "first".into(),
                    text: "lw-first-message".into(),
                },
                crate::config::MessageConfig {
                    label: "second".into(),
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
            let tmux = tmux_standalone(sock);
            let mut app = app_with(tmux.clone());
            let (_wid, pid) = seed_project_with_shell(&tmux, &mut app);
            app.build_tree_flags();
            app.sel = Sel::Shell(0, 0, 0);
            app.cfg.messages = vec![
                crate::config::MessageConfig {
                    label: "first".into(),
                    text: "lw-digit-one".into(),
                },
                crate::config::MessageConfig {
                    label: "second".into(),
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

    // ---- M16 — add-shell command prompt ----

    /// Type a whole string into the open input popup, key by key.
    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_key(key(KeyCode::Char(c), false));
        }
    }

    /// `a` no longer spawns a shell on the spot: it opens a prompt asking
    /// which command the new shell should run, starting empty. Esc backs
    /// out without creating anything.
    #[test]
    fn a_opens_the_add_shell_prompt_instead_of_spawning() {
        let mut app = app_with_shell();
        app.on_key(key(KeyCode::Char('a'), false));
        let Mode::Input { kind, text } = &app.mode else {
            panic!("`a` must open the command prompt, got {:?}", app.mode);
        };
        assert_eq!(*kind, InputKind::AddShell);
        assert!(text.is_empty(), "the command field starts empty: {text:?}");
        assert_eq!(
            app.state.projects[0].features[0].shells.len(),
            1,
            "nothing is spawned before the prompt is submitted"
        );

        app.on_key(key(KeyCode::Esc, false));
        assert!(matches!(app.mode, Mode::Normal), "Esc closes the prompt");
        assert_eq!(
            app.state.projects[0].features[0].shells.len(),
            1,
            "Esc creates no shell"
        );
    }

    /// The command typed into the prompt is what runs in the new shell —
    /// not the project's default agent command.
    #[test]
    fn add_shell_prompt_runs_the_typed_command() {
        crate::util::testutil::with_fresh_xdg(|| {
            with_tmux_server(|sock, _tmux| {
                let tmux = tmux_standalone(sock);
                let mut app = app_with(tmux.clone());
                seed_project_with_shell(&tmux, &mut app);
                app.state.projects[0].agent_command = "echo lw-default-marker".into();
                app.state.projects[0].features[0].worktree_path = PathBuf::from("/tmp");
                app.build_tree_flags();
                app.sel = Sel::Feature(0, 0);

                app.on_key(key(KeyCode::Char('a'), false));
                type_str(&mut app, "echo lw-typed-marker");
                app.on_key(key(KeyCode::Enter, false));
                assert!(matches!(app.mode, Mode::Normal), "Enter closes the prompt");

                let shells = &app.state.projects[0].features[0].shells;
                assert_eq!(shells.len(), 2, "the shell was created");
                let pid = shells[1].pane_id.clone().unwrap();
                std::thread::sleep(std::time::Duration::from_millis(300));
                let capture = tmux.capture_pane(&pid, 50).unwrap();
                assert!(
                    capture.contains("lw-typed-marker"),
                    "the typed command must run, capture: {capture}"
                );
                assert!(
                    !capture.contains("lw-default-marker"),
                    "the project default must not run too, capture: {capture}"
                );
            });
        });
    }

    /// Submitting the prompt empty means "whatever the project normally
    /// runs" — the default agent command.
    #[test]
    fn add_shell_prompt_empty_falls_back_to_the_project_default() {
        crate::util::testutil::with_fresh_xdg(|| {
            with_tmux_server(|sock, _tmux| {
                let tmux = tmux_standalone(sock);
                let mut app = app_with(tmux.clone());
                seed_project_with_shell(&tmux, &mut app);
                app.state.projects[0].agent_command = "echo lw-default-marker".into();
                app.state.projects[0].features[0].worktree_path = PathBuf::from("/tmp");
                app.build_tree_flags();
                app.sel = Sel::Feature(0, 0);

                app.on_key(key(KeyCode::Char('a'), false));
                app.on_key(key(KeyCode::Enter, false));

                let shells = &app.state.projects[0].features[0].shells;
                assert_eq!(shells.len(), 2, "the shell was created");
                let pid = shells[1].pane_id.clone().unwrap();
                std::thread::sleep(std::time::Duration::from_millis(300));
                let capture = tmux.capture_pane(&pid, 50).unwrap();
                assert!(
                    capture.contains("lw-default-marker"),
                    "an empty command falls back to the default, capture: {capture}"
                );
            });
        });
    }
}
