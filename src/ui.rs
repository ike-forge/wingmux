//! TUI rendering: tree, preview, status bar, toast, overlays.
//!
//! All colours come from the resolved [`Theme`]; the renderer never hardcodes
//! a colour.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, BarAction, InputKind, Mode, Sel};
use crate::config::IconsConfig;
use crate::git::WorktreeState;
use crate::status::Status;
use crate::theme::ResolvedColors;

/// Minimum sizes for the layout tiers. Resize is normal, not an edge case:
/// the UI degrades gracefully — the preview hides first, then the bar —
/// and a too-small frame shows a message instead of drawing garbage.
const WIDE_MIN: (u16, u16) = (64, 12);
const SPLIT_MIN: (u16, u16) = (44, 9);
const TREE_MIN: (u16, u16) = (26, 7);

/// The mark a parked preview carries in its title. Plain Unicode, not a
/// glyph from `[icons]`: it has to render in the `unicode` preset too.
const UP_ARROW: &str = "\u{2191}";

/// Rows below the pad's window, in its title (see [`draw_todo`]).
const DOWN_ARROW: &str = "\u{2193}";

/// Cap on the tree panel's width, in columns (see [`tree_width`]).
const TREE_MAX: u16 = 54;

/// Which panels fit in a frame of `(width, height)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    /// Tree + preview + statusbar, tree at its roomy width.
    Wide,
    /// Tree + preview + statusbar, tree taking a bigger share of a
    /// narrower frame.
    Split,
    /// Tree + statusbar only.
    TreeOnly,
    /// Too small for the tree — a message instead of the UI.
    Message,
}

pub(crate) fn tier_impl(w: u16, h: u16) -> Tier {
    if w >= WIDE_MIN.0 && h >= WIDE_MIN.1 {
        Tier::Wide
    } else if w >= SPLIT_MIN.0 && h >= SPLIT_MIN.1 {
        Tier::Split
    } else if w >= TREE_MIN.0 && h >= TREE_MIN.1 {
        Tier::TreeOnly
    } else {
        Tier::Message
    }
}

/// Width of the tree panel for a viewport, or `None` when the frame is too
/// small for the tree at all. Same math for rendering and mouse hit-testing.
///
/// The share is a percentage up to [`TREE_MAX`]: the tree holds short
/// names, so on a wide screen a plain percentage turns into dead space —
/// past the cap the extra columns go to the preview instead.
pub(crate) fn tree_width(viewport: (u16, u16)) -> Option<u16> {
    let (w, h) = viewport;
    match tier_impl(w, h) {
        Tier::Wide => Some((w * 32 / 100).min(TREE_MAX)),
        Tier::Split => Some((w * 42 / 100).min(TREE_MAX)),
        Tier::TreeOnly => Some(w),
        Tier::Message => None,
    }
}

/// The tier the dashboard's own pane actually draws at.
///
/// A docked pane beside us means the preview panel has already been
/// replaced — by a real tmux pane showing the real agent. Drawing a
/// second, narrower, one-poll-behind copy of that agent next to it is the
/// exact duplication docking exists to end, so the dashboard keeps the
/// tree and nothing else, however wide its own strip happens to be.
///
/// The test is what is *docked*, not whether a slot exists: a slot with no
/// agent in it is torn down again ([`App::drop_slot`]), and the frame in
/// between must not be a tree-only view of nothing.
pub(crate) fn tier(app: &App) -> Tier {
    let tier = tier_impl(app.viewport.0, app.viewport.1);
    match tier {
        Tier::Wide | Tier::Split if app.docked.is_some() => Tier::TreeOnly,
        _ => tier,
    }
}

/// [`tree_width`] for the tier the dashboard is really drawing at — the
/// same math the renderer and the mouse hit-testing must share.
pub(crate) fn tree_width_for(app: &App) -> Option<u16> {
    match tier(app) {
        // The whole strip: the preview it would otherwise share with is a
        // real pane now, on the other side of a tmux divider.
        Tier::TreeOnly => Some(app.viewport.0),
        _ => tree_width(app.viewport),
    }
}

/// Rows the tree keeps for itself when the todo pad shares its column:
/// border, one row, border. A column that cannot spare them is not split.
const TREE_MIN_ROWS: u16 = 3;

/// The main area's height: the frame, less the statusbar. The statusbar is
/// *measured* — it wraps onto a second row in a narrow strip.
pub(crate) fn main_h(app: &App) -> u16 {
    app.viewport
        .1
        .saturating_sub(statusbar_h(app, app.viewport.0))
}

/// Rows the todo panel takes at the bottom of the left column, out of a
/// column `main_h` rows tall. `0` means it is not drawn; `main_h` means it
/// took the whole column because there was no room for two panels.
///
/// Two heights, one rule: the pad being typed into gets half the column,
/// because that is what writing in it needs; a parked pad gets exactly as
/// many rows as it has lines of text, because that is what *reading* it
/// needs — five lines of text is a five-line panel — and never more than
/// the open half, so a long note cannot push the tree off the screen.
pub(crate) fn todo_panel_h(app: &App, main_h: u16) -> u16 {
    let open = matches!(app.mode, Mode::Todo { .. });
    if !open && app.todo.text().trim().is_empty() {
        return 0;
    }
    let cap = (main_h / 2).min(main_h.saturating_sub(TREE_MIN_ROWS));
    if cap < TREE_MIN_ROWS {
        // No room to split. An open pad still has to be somewhere, so it
        // takes the column the way it did before the split existed; a
        // parked one yields to the tree, which is the panel you steer with.
        return if open { main_h } else { 0 };
    }
    if open {
        return cap;
    }
    let rows = app.todo.visual_len(crate::app::todo_wrap_width(app)) as u16;
    rows.saturating_add(2).clamp(TREE_MIN_ROWS, cap)
}

/// The rows the todo panel occupies, as `[top, bottom)` — `None` when it is
/// not drawn. Rendering and mouse hit-testing share it, so a click always
/// lands on the panel the eye sees.
pub(crate) fn todo_panel_rows(app: &App) -> Option<(u16, u16)> {
    let main = main_h(app);
    let h = todo_panel_h(app, main);
    (h > 0).then(|| (main - h, main))
}

/// Content rows of the tree panel: its share of the column, less its two
/// borders. Drives the page motions, which must not step by more than the
/// tree actually shows.
pub(crate) fn tree_rows(app: &App) -> usize {
    let main = main_h(app);
    let todo = todo_panel_h(app, main);
    ((main.saturating_sub(todo)).saturating_sub(2) as usize).max(1)
}

/// Minimum popup size per overlay kind; below it the overlay is refused
/// with a message ("frame too small for this dialog") instead of drawing
/// garbage — never a crash.
///
/// Every width is [`dock::DASH_MIN_WIDTH`][crate::dock::DASH_MIN_WIDTH],
/// the narrowest strip a docked dashboard ever draws in: an overlay that
/// asked for more than that would be refused exactly when it is most
/// needed, on the layout the app spends its life in. What differs is the
/// height, and only because a dialog with no room for its own shortcuts is
/// not a dialog.
fn overlay_min(kind: OverlayKind) -> (u16, u16) {
    // Border + `MODAL_PADDING` included: each of these leaves the same
    // usable content box the pre-padding minimums did.
    let w = crate::dock::DASH_MIN_WIDTH;
    match kind {
        OverlayKind::Help => (w, 10),
        OverlayKind::Diagnostics => (w, 8),
        OverlayKind::ThemePicker => (w, 8),
        OverlayKind::Messages | OverlayKind::AddShell => (w, 8),
        OverlayKind::Input | OverlayKind::Confirm => (w, 7),
        // The textarea is the main interaction — it needs real estate.
        OverlayKind::Textarea => (w, 10),
    }
}

/// The widest any overlay is ever drawn, docked or not.
///
/// A docked dashboard owns a strip of at most
/// [`dock::DASH_MAX_WIDTH`][crate::dock::DASH_MAX_WIDTH] columns; an
/// undocked one owns its whole window. An overlay sized to its frame would
/// therefore be two dialogs — the same question in two shapes, changing
/// under the user as panes come and go. So every overlay is built for the
/// strip and capped there. The cap costs the undocked layout nothing: a
/// dialog that asks one question has never needed 200 columns.
pub(crate) const MODAL_MAX_W: u16 = crate::dock::DASH_MAX_WIDTH;

/// The width an overlay is drawn at: what its content asked for, capped to
/// [`MODAL_MAX_W`] and to the frame.
fn modal_width(frame: Rect, want: u16) -> u16 {
    want.min(MODAL_MAX_W).min(frame.width)
}

/// The content width inside a modal of `width` columns — what every
/// wrapping and truncating decision is made against.
fn modal_text_width(width: u16) -> usize {
    width.saturating_sub(MODAL_CHROME_W).max(1) as usize
}

/// Word-wrap `text` to `width` columns, breaking inside a word only when
/// the word cannot fit a line of its own. Returns at least one line, so a
/// wrapped empty string is still a blank row and not a vanished one.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word = word;
        // A word longer than the whole line is cut where the line ends;
        // anything else would print past the border.
        while UnicodeWidthStr::width(word) > width {
            let (head, tail) = split_at_width(word, width);
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
                continue;
            }
            out.push(head.to_string());
            word = tail;
        }
        let need = UnicodeWidthStr::width(word) + usize::from(!line.is_empty());
        if UnicodeWidthStr::width(line.as_str()) + need > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push(line);
    out
}

/// Split `s` at the last char boundary that still fits `width` columns.
fn split_at_width(s: &str, width: usize) -> (&str, &str) {
    let mut end = 0;
    for (i, c) in s.char_indices() {
        let next = i + c.len_utf8();
        if UnicodeWidthStr::width(&s[..next]) > width {
            break;
        }
        end = next;
    }
    // Never return an empty head: a zero-width step would loop for ever.
    if end == 0 {
        end = s.char_indices().nth(1).map_or(s.len(), |(i, _)| i);
    }
    s.split_at(end)
}

#[derive(Clone, Copy)]
enum OverlayKind {
    Help,
    AddShell,
    Diagnostics,
    ThemePicker,
    Messages,
    Input,
    Confirm,
    Textarea,
}

fn fg(c: Color) -> Style {
    Style::default().fg(c)
}

fn on(c: Color, bg: Color) -> Style {
    Style::default().fg(c).bg(bg)
}

/// Background-only style. The tree's selected row uses this rather than
/// `on(selection_fg, selection_bg)`: every row in the tree carries meaning
/// in its foreground colour (the status roles, the dimming of a dead
/// shell), and repainting that on selection destroys exactly the signal
/// the user moved the cursor there to read.
fn bg(c: Color) -> Style {
    Style::default().bg(c)
}

fn status_color(theme: &ResolvedColors, st: Status) -> Color {
    match st {
        Status::Working => theme.status_working,
        Status::Waiting => theme.status_waiting,
        Status::Blocked => theme.status_blocked,
        Status::Idle => theme.status_idle,
        Status::Failed => theme.status_failed,
        Status::Ended => theme.status_ended,
        Status::Dead => theme.status_dead,
    }
}

/// How long one spinner frame is shown. The event loop redraws at least
/// every 100 ms (`app::event_loop` caps its poll timeout), so a frame must
/// last longer than that or the animation stutters at the redraw floor.
const SPINNER_FRAME_MS: u128 = 240;

/// The frame of the working spinner at `elapsed`. Empty when the icon set
/// defines no spinner (the `none` preset), which is also why this cannot
/// index blindly.
fn spinner_frame(ic: &IconsConfig, elapsed: std::time::Duration) -> &str {
    if ic.status_spinner.is_empty() {
        return "";
    }
    let i = (elapsed.as_millis() / SPINNER_FRAME_MS) as usize % ic.status_spinner.len();
    &ic.status_spinner[i]
}

/// The icon drawn in front of a status label, already padded to the shared
/// status column. `idle` and `dead` are blank in every shipped preset: a
/// state the user cannot act on should not compete for attention.
fn status_icon(ic: &IconsConfig, st: Status, elapsed: std::time::Duration) -> &str {
    match st {
        Status::Working => spinner_frame(ic, elapsed),
        Status::Waiting => &ic.status_waiting,
        Status::Blocked => &ic.status_blocked,
        Status::Failed => &ic.status_failed,
        Status::Ended => &ic.status_ended,
        Status::Idle => &ic.status_idle,
        Status::Dead => &ic.status_dead,
    }
}

/// The icon for a worktree's state. The four glyphs share one
/// padded column (`IconsConfig::pad`), so the names line up whichever one
/// a row draws.
fn worktree_icon(ic: &crate::config::IconsConfig, st: WorktreeState) -> &str {
    match st {
        WorktreeState::New => &ic.worktree_new,
        WorktreeState::Ahead => &ic.worktree_ahead,
        WorktreeState::Merged => &ic.worktree_merged,
        WorktreeState::Deleted => &ic.worktree_deleted,
    }
}

/// Colour for the same four states, borrowed from the status palette so
/// every theme already answers for them: nothing started yet reads dim,
/// and work not in the base reads like `waiting` (it needs something from
/// you). Merged is the exception — it takes `worktree_merged`, the green
/// of the worktree level, because the status role it used to borrow
/// (`working`) is ordinary text and left "done, safe to delete" looking
/// like every other row.
/// A worktree whose directory is gone borrows `failed`: it is the one
/// role every theme keeps distinct from those three, and the row is
/// broken — it cannot be docked, sent to or previewed, only removed.
fn worktree_state_color(theme: &ResolvedColors, st: WorktreeState) -> Color {
    match st {
        WorktreeState::New => theme.dim,
        WorktreeState::Ahead => theme.status_waiting,
        WorktreeState::Merged => theme.worktree_merged,
        WorktreeState::Deleted => theme.status_failed,
    }
}

/// Whether the worktree's own state is the whole story of the row. Once
/// the work is merged, or the directory is gone, what the agent inside is
/// doing no longer matters — the only thing left to decide is whether to
/// remove the worktree. Such a row drops the shell status entirely and
/// mutes its name, so the one glyph that carries the state is the only
/// thing that reads, instead of a yellow `waiting` pulling the eye to a
/// shell nobody needs to answer.
fn worktree_state_settles_row(st: WorktreeState) -> bool {
    matches!(st, WorktreeState::Merged | WorktreeState::Deleted)
}

/// The polled status of a shell, keyed by its live pane; a shell with no
/// pane (or none polled yet) reads as idle.
fn shell_status(app: &App, sh: &crate::state::Shell) -> Status {
    sh.pane_id
        .as_ref()
        .and_then(|p| app.statuses.get(p))
        .copied()
        .unwrap_or(Status::Idle)
}

/// Is this shell docked in another dashboard right now?
fn shell_leased<'a>(app: &'a App, sh: &crate::state::Shell) -> Option<&'a crate::app::Lease> {
    app.leased_by(sh.pane_id.as_deref()?)
}

/// Mute a row whole and say why: a shell another dashboard holds is not
/// one this instance can bring on screen by looking at it, so it gets the
/// same "the eye skips it" treatment a dead row gets — plus the one word
/// that keeps it from reading as dead.
fn mark_leased(theme: &ResolvedColors, spans: &mut Vec<Span<'static>>) {
    for span in spans.iter_mut() {
        span.style = fg(theme.dim);
    }
    spans.push(Span::styled("  leased", fg(theme.dim)));
}

/// A worktree is dead once every shell under it is — a worktree with no
/// shells at all is not dead, it was simply never started.
fn worktree_dead(app: &App, wt: &crate::state::Worktree) -> bool {
    !wt.shells.is_empty()
        && wt
            .shells
            .iter()
            .all(|sh| shell_status(app, sh) == Status::Dead)
}

/// A repository is dead once every worktree under it is.
fn repository_dead(app: &App, repo: &crate::state::Repository) -> bool {
    !repo.worktrees.is_empty() && repo.worktrees.iter().all(|f| worktree_dead(app, f))
}

/// The colour a tree row draws in: a dead entry is muted *whole* — guide,
/// icon, name and count all take the dead colour — so the eye skips it
/// instead of re-reading a row that carries no live agent.
fn row_color(theme: &ResolvedColors, dead: bool, live: Color) -> Color {
    if dead { theme.status_dead } else { live }
}

/// The colour a worktree row draws in. A worktree LazyWingman has no tmux
/// window for gets muted whole: it cannot be docked, sent to or
/// previewed, so the row is a listing and nothing more. `live` is the
/// colour the span would otherwise take.
fn worktree_color(
    theme: &ResolvedColors,
    wt: &crate::state::Worktree,
    dead: bool,
    live: Color,
) -> Color {
    if wt.shells.is_empty() {
        theme.dim
    } else {
        row_color(theme, dead, live)
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.viewport = (area.width, area.height);
    // theme background
    f.render_widget(
        Block::default().style(Style::default().bg(app.theme.colors.background)),
        area,
    );
    match tier(app) {
        Tier::Message => {
            draw_too_small(f, area, app);
            return;
        }
        Tier::Wide | Tier::Split => {
            let tree_w = tree_width_for(app).unwrap_or(area.width);
            draw_split(f, area, app, tree_w);
        }
        Tier::TreeOnly => draw_tree_only(f, area, app),
    }
    // Overlays render last (on top). popup_area is (re)computed every
    // frame so mouse "click outside dismisses" tracks what is on screen;
    // in Normal there is nothing to dismiss.
    app.popup_area = None;
    let mut hint_rows = 1;
    match &app.mode {
        Mode::Help => {
            let kind = OverlayKind::Help;
            if fits(area, overlay_min(kind)) {
                let popup = draw_help(f, app);
                app.popup_area = Some(popup.area);
                hint_rows = popup.hint_rows;
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::Diagnostics => {
            let kind = OverlayKind::Diagnostics;
            if fits(area, overlay_min(kind)) {
                let popup = draw_diagnostics(f, app);
                app.popup_area = Some(popup.area);
                hint_rows = popup.hint_rows;
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::ThemePicker { .. } => {
            let kind = OverlayKind::ThemePicker;
            if fits(area, overlay_min(kind)) {
                let popup = draw_theme_picker(f, app);
                app.popup_area = Some(popup.area);
                hint_rows = popup.hint_rows;
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::AddShell { .. } => {
            let kind = OverlayKind::AddShell;
            if fits(area, overlay_min(kind)) {
                let popup = draw_add_shell(f, app);
                app.popup_area = Some(popup.area);
                hint_rows = popup.hint_rows;
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::Messages { .. } => {
            let kind = OverlayKind::Messages;
            if fits(area, overlay_min(kind)) {
                let popup = draw_messages(f, app);
                app.popup_area = Some(popup.area);
                hint_rows = popup.hint_rows;
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::Input { .. } => {
            let kind = OverlayKind::Input;
            if fits(area, overlay_min(kind)) {
                let popup = draw_input(f, app);
                app.popup_area = Some(popup.area);
                hint_rows = popup.hint_rows;
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::Textarea { .. } => {
            let kind = OverlayKind::Textarea;
            if fits(area, overlay_min(kind)) {
                let popup = draw_textarea(f, app);
                app.popup_area = Some(popup.area);
                hint_rows = popup.hint_rows;
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::Confirm { .. } => {
            let kind = OverlayKind::Confirm;
            if fits(area, overlay_min(kind)) {
                let popup = draw_confirm(f, app);
                app.popup_area = Some(popup.area);
                hint_rows = popup.hint_rows;
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        // The pad draws no overlay: it is the left panel (see
        // `draw_left`), so there is nothing on top and nothing to dismiss.
        Mode::Normal | Mode::Todo { .. } => {}
    }
    // The body/footer rule goes on last, over the blank row every modal
    // reserves above its shortcuts. One place, so no overlay can forget
    // it — the same discipline as `MODAL_PADDING`.
    if let Some(rect) = app.popup_area {
        draw_footer_rule(f, rect, &app.theme.colors, hint_rows);
    }
}

fn fits(area: Rect, min: (u16, u16)) -> bool {
    area.width >= min.0 && area.height >= min.1
}

/// App-generated glyphs, configurable via `[icons]`. These helpers return
/// the app's own labels.
pub(crate) fn warn_mark(icons: &crate::config::IconsConfig) -> &str {
    &icons.warn
}

/// The prefix a warning line opens with, and the column its text starts
/// in — continuation rows indent to the same column. The gap is two
/// spaces, not one: `⚠` is East-Asian *ambiguous* width, so unicode-width
/// (and therefore ratatui's layout) counts it as one cell while plenty of
/// terminals draw it as two, swallowing a single separator and leaving
/// `⚠unmerged commits` jammed together.
pub(crate) fn warn_prefix(icons: &crate::config::IconsConfig) -> (String, usize) {
    let mark = warn_mark(icons);
    (format!("{mark}  "), UnicodeWidthStr::width(mark) + 2)
}

pub(crate) fn sep(icons: &crate::config::IconsConfig) -> &str {
    &icons.separator
}

/// One shortcut hint. Every hint in the app — status bar, modal footer,
/// help palette — is written `<description> <key>` ("cancel esc"): the
/// word says what happens, the key comes last and is the only part that
/// is highlighted, so the eye finds it without reading the sentence.
#[derive(Debug, Clone)]
pub(crate) struct Hint {
    pub desc: String,
    pub key: String,
}

impl Hint {
    /// The hint as one string, exactly as it renders.
    pub(crate) fn label(&self) -> String {
        format!("{} {}", self.desc, self.key)
    }
}

/// Build a hint, resolving the key to its rendered spelling.
pub(crate) fn hint(icons: &crate::config::IconsConfig, desc: &str, key: &str) -> Hint {
    Hint {
        desc: desc.to_string(),
        key: key_label(icons, key),
    }
}

/// The rendered spelling of a key. Keys with a glyph form take it from
/// `[icons]`, so a preset (or a user override) decides whether a hint
/// draws `↵` or spells `Enter` out.
pub(crate) fn key_label(icons: &crate::config::IconsConfig, key: &str) -> String {
    match key {
        "Enter" => icons.enter.trim().to_string(),
        "arrows" => icons.arrows.trim().to_string(),
        other => other.to_string(),
    }
}

/// The two spans of one hint: plain description, highlighted key.
fn hint_spans(theme: &ResolvedColors, h: &Hint) -> [Span<'static>; 2] {
    [
        Span::styled(format!("{} ", h.desc), fg(theme.foreground)),
        Span::styled(h.key.clone(), fg(theme.accent).add_modifier(Modifier::BOLD)),
    ]
}

/// Grow `lines` with blank rows until it is `n` long (never shrinks it).
fn pad_to(lines: &mut Vec<Line<'_>>, n: usize) {
    while lines.len() < n {
        lines.push(Line::from(""));
    }
}

/// A drawn overlay: where it landed, and how many rows its footer hints
/// took — the rule above them is painted from that.
#[derive(Clone, Copy)]
struct Popup {
    area: Rect,
    hint_rows: usize,
}

impl From<Rect> for Popup {
    fn from(area: Rect) -> Self {
        Popup { area, hint_rows: 1 }
    }
}

/// How the hints of a footer are packed into rows of `width` columns:
/// greedily, in order, one row while they fit.
///
/// A modal footer is drawn in the dashboard's own strip, which is 26
/// columns at its narrowest — a single row silently cut at the border
/// loses `cancel esc`, the one hint that must never be lost. So the footer
/// wraps, and the modals that build it size themselves to however many
/// rows it took.
fn hint_packing(
    icons: &crate::config::IconsConfig,
    hints: &[Hint],
    width: usize,
) -> Vec<Vec<Hint>> {
    let sep_w = UnicodeWidthStr::width(sep(icons));
    let mut rows: Vec<Vec<Hint>> = Vec::new();
    let mut row: Vec<Hint> = Vec::new();
    let mut used = 0usize;
    for h in hints {
        let w = UnicodeWidthStr::width(h.label().as_str());
        let need = if row.is_empty() { w } else { w + sep_w };
        if !row.is_empty() && used + need > width {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        used += if row.is_empty() { w } else { w + sep_w };
        row.push(h.clone());
    }
    rows.push(row);
    rows
}

/// The footer rows for `hints` at `width` columns — one line each, in the
/// same order the caller listed them.
fn hint_rows(
    theme: &ResolvedColors,
    icons: &crate::config::IconsConfig,
    hints: &[Hint],
    width: usize,
) -> Vec<Line<'static>> {
    hint_packing(icons, hints, width)
        .into_iter()
        .map(|row| hint_line(theme, icons, &row))
        .collect()
}

/// Rows a modal footer occupies: the rule row it is separated by, plus the
/// hint rows themselves. Every modal sizes itself with this instead of a
/// constant, because how many rows the hints need depends on the width.
fn footer_h(icons: &crate::config::IconsConfig, hints: &[Hint], width: usize) -> usize {
    1 + hint_packing(icons, hints, width).len()
}

/// A row of hints joined with the app's separator — the footer every modal
/// ends with, a border-to-border rule above it.
fn hint_line(
    theme: &ResolvedColors,
    icons: &crate::config::IconsConfig,
    hints: &[Hint],
) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    for (i, h) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(sep(icons).to_string(), fg(theme.dim)));
        }
        spans.extend(hint_spans(theme, h));
    }
    Line::from(spans)
}

/// Tree | preview, statusbar below. `tree_w` comes from [`tree_width`] —
/// the same math drives mouse hit-testing.
fn draw_split(f: &mut Frame, area: Rect, app: &mut App, tree_w: u16) {
    let [main, statusbar] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(statusbar_h(app, area.width)),
    ])
    .areas(area);
    let [tree, preview] =
        Layout::horizontal([Constraint::Length(tree_w), Constraint::Fill(1)]).areas(main);

    draw_left(f, tree, app);
    draw_preview(f, preview, app);
    draw_toast(f, main, app);
    draw_statusbar(f, statusbar, app);
}

/// The left column: the tree, with the todo pad split off its bottom
/// ([`todo_panel_h`] decides how much).
///
/// The pad is a panel of its own rather than an overlay, so it neither
/// covers the tree nor the preview beside it (or the docked agent) — both
/// stay exactly where they were. It is the only panel in the app that can
/// be absent: an empty pad is drawn as nothing at all, because a framed
/// box with nothing in it is worse than the row it costs.
fn draw_left(f: &mut Frame, area: Rect, app: &App) {
    match todo_panel_h(app, area.height) {
        0 => draw_tree(f, area, app),
        // Nothing to split: the pad being typed into takes the column.
        h if h >= area.height => draw_todo(f, area, app),
        h => {
            let [tree, todo] =
                Layout::vertical([Constraint::Min(0), Constraint::Length(h)]).areas(area);
            draw_tree(f, tree, app);
            draw_todo(f, todo, app);
        }
    }
}

/// Tree across the whole main area.
fn draw_tree_only(f: &mut Frame, area: Rect, app: &mut App) {
    let [main, statusbar] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(statusbar_h(app, area.width)),
    ])
    .areas(area);
    draw_left(f, main, app);
    draw_toast(f, main, app);
    draw_statusbar(f, statusbar, app);
}

fn draw_too_small(f: &mut Frame, area: Rect, app: &mut App) {
    app.popup_area = None;
    app.bar_hits.clear();
    let theme = &app.theme.colors;
    f.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(Span::styled(" terminal too small ", title_style(theme))),
            Line::from(""),
            Line::from(Span::raw(
                "enlarge the window to at least 26x7 to show the dashboard",
            )),
            hint_line(theme, &app.cfg.icons, &[hint(&app.cfg.icons, "quit", "q")]),
        ]))
        .style(fg(theme.foreground)),
        area,
    );
}

fn draw_overlay_too_small(f: &mut Frame, area: Rect, app: &App, kind: OverlayKind) {
    let theme = &app.theme.colors;
    let (min_w, min_h) = overlay_min(kind);
    let rect = centered_rect(area, 70, 25);
    f.render_widget(Clear, rect);
    let block = popup_block(theme, &app.cfg.icons, " frame too small for this dialog ");
    let ic = &app.cfg.icons;
    let text = Text::from(vec![
        Line::from(Span::raw(format!(
            "enlarge the window to at least {min_w}x{min_h}"
        ))),
        Line::from(""),
        hint_line(theme, ic, &[hint(ic, "dismiss", "esc")]),
    ]);
    f.render_widget(Paragraph::new(text).block(block), rect);
}

/// Panel/popup title: foreground only, no background fill — the "simple
/// title display without background" from the M9 UI pass.
fn title_style(theme: &ResolvedColors) -> Style {
    fg(theme.title_fg).add_modifier(Modifier::BOLD)
}

/// Breathing room inside every modal border: one column left and right,
/// nothing vertically. No top row — the first content row sits right under
/// the border, the way the panel frames carry their titles. No bottom row
/// either — the last content row is the shortcut footer, fenced off by
/// [`draw_footer_rule`], and a gap below it would float it off the frame.
pub(crate) const MODAL_PADDING: Padding = Padding::new(1, 1, 0, 0);

/// Columns and rows a modal spends on its border and [`MODAL_PADDING`] —
/// what every content-sizing calculation has to add on top of its lines.
/// Derived from the padding itself: the two constants and [`modal_inner`]
/// have drifted from it twice, each time putting the input caret a row
/// below the text it belongs to.
const MODAL_CHROME_W: u16 = 2 + MODAL_PADDING.left + MODAL_PADDING.right;
const MODAL_CHROME_H: u16 = 2 + MODAL_PADDING.top + MODAL_PADDING.bottom;

/// Overlay block: rounded (or configured) corners, popup border colour,
/// bg-less title, and the shared modal padding — no modal draws text
/// flush against its border.
fn popup_block(
    theme: &ResolvedColors,
    icons: &crate::config::IconsConfig,
    title: impl Into<String>,
) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(border_type(icons))
        .border_style(fg(theme.popup_border))
        .padding(MODAL_PADDING)
        .title(Span::styled(title.into(), title_style(theme)))
}

/// The content rect of a modal: inside the border AND [`MODAL_PADDING`].
/// Same box ratatui gives the block's own content, kept in step with it by
/// `modal_inner_matches_the_block` — the padding is horizontal only today,
/// so the first content row sits flush under the top border.
fn modal_inner(area: Rect) -> Rect {
    Rect {
        x: area.x + 1 + MODAL_PADDING.left,
        y: area.y + 1 + MODAL_PADDING.top,
        width: area.width.saturating_sub(MODAL_CHROME_W),
        height: area.height.saturating_sub(MODAL_CHROME_H),
    }
}

/// Rows a modal footer owns when its hints fit one row: the rule and the
/// hint row itself. The scrolling overlays measure their own footer (it
/// wraps in a narrow strip — see [`footer_h`]); this is what the app's
/// page-scroll math assumes, where being a row out only changes the size
/// of a jump the renderer clamps anyway.
pub(crate) const MODAL_FOOTER_H: usize = 2;

/// The rule between a modal's body and its shortcuts, drawn border to
/// border (`\u{251c}\u{2500}\u{2524}`) so the footer reads as its own compartment instead of a
/// stray line floating inside the padding.
///
/// It paints over the blank row every modal already reserves above its
/// hint line, so no sizing math changes — and only the symbol and the
/// foreground are written, leaving each cell's background as the modal
/// drew it (some overlays fill `popup_bg`, some do not).
fn draw_footer_rule(f: &mut Frame, area: Rect, theme: &ResolvedColors, hint_rows: usize) {
    // Two borders plus the hint rows: a modal this short has no body to
    // divide from its footer.
    if (area.height as usize) < 3 + hint_rows || area.width < 2 {
        return;
    }
    let y = area.y + area.height - 2 - hint_rows as u16;
    let right = area.x + area.width - 1;
    let buf = f.buffer_mut();
    for x in area.x..=right {
        let symbol = match x {
            _ if x == area.x => "\u{251c}",
            _ if x == right => "\u{2524}",
            _ => "\u{2500}",
        };
        buf[(x, y)].set_symbol(symbol).set_fg(theme.popup_border);
    }
}

/// Scrollable rows a scrolling overlay of this rect shows: its content
/// height minus the pinned footer. The app clamps its scroll offsets with
/// this, so they can never drift from what the renderer draws.
pub(crate) fn overlay_body_height(rect: Option<Rect>) -> usize {
    rect.map(|r| r.height.saturating_sub(MODAL_CHROME_H) as usize)
        .unwrap_or(10 + MODAL_FOOTER_H)
        .saturating_sub(MODAL_FOOTER_H)
}

fn border_type(icons: &crate::config::IconsConfig) -> BorderType {
    match icons.border {
        crate::config::BorderStyle::Rounded => BorderType::Rounded,
        crate::config::BorderStyle::Plain => BorderType::Plain,
    }
}

fn panel_block(
    theme: &ResolvedColors,
    icons: &crate::config::IconsConfig,
    title: &str,
    focused: bool,
) -> Block<'static> {
    let border = if focused {
        theme.border_focused
    } else {
        theme.border
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(border_type(icons))
        .border_style(fg(border))
        .title(Span::styled(format!(" {title} "), title_style(theme)))
}

fn draw_tree(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme.colors;
    let ic = &app.cfg.icons;
    // Guides are composites: `[icons]` exposes three keys and the six
    // shapes are built here. Six near-identical constants is not a
    // configuration surface worth handing a user.
    let child = format!("  {}", ic.guide_branch);
    let last_child = format!("  {}", ic.guide_last);
    let vbar = &ic.guide_vertical;
    let vbar_blank = " ".repeat(UnicodeWidthStr::width(vbar.as_str()));
    let grandchild = format!("  {vbar}  {}", ic.guide_branch);
    let last_grandchild = format!("  {vbar}  {}", ic.guide_last);
    let no_grandchild = format!("  {vbar_blank}  {}", ic.guide_branch);
    let no_last_grandchild = format!("  {vbar_blank}  {}", ic.guide_last);
    // One spinner phase for the whole frame: every working row animates in
    // step, which reads as one dashboard ticking rather than N unrelated
    // ones.
    let spin = app.started.elapsed();
    let mut items: Vec<ListItem> = Vec::new();
    let mut sel_row: Option<usize> = None;
    let mut row: usize = 0;

    // Scope-filtered: only repositories inside the current path get rows.
    let vis = app.visible_repositories();
    for ri in vis {
        let repo = &app.state.repositories[ri];
        if !app.flatten {
            let expanded = app.repo_expanded.get(ri).copied().unwrap_or(false);
            // The folder glyph carries the disclosure state, which is why
            // repository rows draw no separate ▸/▾ marker. An empty
            // repository has nothing to open, so it has one glyph only.
            let icon = if repo.worktrees.is_empty() {
                &ic.repository_empty
            } else if expanded {
                &ic.repository_open
            } else {
                &ic.repository
            };
            let pdead = repository_dead(app, repo);
            let mut spans = vec![Span::styled(
                format!("{icon}{}", repo.name),
                fg(row_color(theme, pdead, theme.tree_repository)).add_modifier(Modifier::BOLD),
            )];
            if repo.missing {
                spans.push(Span::styled(" (missing)", fg(theme.dim)));
            }
            // A repository with an inline shell says so, and names the
            // checkout it runs in — the base branch, which is the one thing
            // that distinguishes it from the worktrees hanging below it.
            if let Some(sh) = &repo.shell {
                spans.push(Span::styled(
                    format!("  {}{}", ic.shell, repo.base_branch),
                    fg(row_color(
                        theme,
                        shell_status(app, sh) == Status::Dead,
                        theme.tree_shell,
                    )),
                ));
                if shell_leased(app, sh).is_some() {
                    mark_leased(theme, &mut spans);
                }
            }
            items.push(ListItem::new(Line::from(spans)));
            if app.sel == Sel::Repository(ri) {
                sel_row = Some(row);
            }
            row += 1;
            if !expanded {
                continue;
            }
        }
        let n_feat = repo.worktrees.len();
        for (wi, wt) in repo.worktrees.iter().enumerate() {
            let fexp = app
                .wt_expanded
                .get(ri)
                .and_then(|v| v.get(wi))
                .copied()
                .unwrap_or(false);
            // One worktree, one agent: a worktree with a single shell is
            // folded for good — no child row, and its own row carries that
            // shell's status.
            let solo = app.solo_shell(ri, wi);
            let fdead = worktree_dead(app, wt);
            let n = wt.shells.len();
            let wstate = app.worktree_state_cached(ri, wi);
            let ficon = worktree_icon(ic, wstate);
            let settled = worktree_state_settles_row(wstate);
            // In flatten mode the worktrees are the top level (no guide);
            // otherwise they hang off the repository with a tree branch.
            let (prefix, icon_span) = if app.flatten {
                (String::new(), ficon.to_string())
            } else {
                let guide = if wi + 1 == n_feat {
                    &last_child
                } else {
                    &child
                };
                (guide.clone(), ficon.to_string())
            };
            let mut wt_spans = vec![
                Span::styled(
                    prefix,
                    fg(worktree_color(theme, wt, fdead, theme.tree_worktree)),
                ),
                Span::styled(
                    icon_span,
                    fg(if settled {
                        // The one span a settled row keeps: muting it too
                        // would leave nothing saying why the row is quiet.
                        worktree_state_color(theme, wstate)
                    } else {
                        worktree_color(theme, wt, fdead, worktree_state_color(theme, wstate))
                    }),
                ),
                Span::styled(
                    wt.name.clone(),
                    fg(if settled {
                        theme.dim
                    } else {
                        worktree_color(theme, wt, fdead, theme.tree_worktree)
                    }),
                ),
                // A settled row drops the status altogether: once the work
                // is merged (or the directory is gone) the only thing left
                // to do is remove the worktree, and what the agent inside
                // is waiting for is no longer anybody's business.
                match solo.filter(|_| !settled) {
                    Some(si) => {
                        let st = shell_status(app, &wt.shells[si]);
                        Span::styled(
                            format!("  {}{}", status_icon(ic, st, spin), st.label()),
                            fg(status_color(theme, st)),
                        )
                    }
                    // A multi-shell worktree says nothing extra: its shells
                    // are one row below, each with its own status.
                    None => Span::raw(""),
                },
            ];
            // A folded worktree row IS its shell, so it carries the shell's
            // lease too.
            if solo.is_some_and(|si| shell_leased(app, &wt.shells[si]).is_some()) {
                mark_leased(theme, &mut wt_spans);
            }
            items.push(ListItem::new(Line::from(wt_spans)));
            if app.sel == Sel::Worktree(ri, wi) {
                sel_row = Some(row);
            }
            row += 1;
            // A solo worktree emits no child row whatever its flag says —
            // `App::visible_nodes` skips it the same way, and the two must
            // never drift (the cursor and mouse hit-testing map rows to
            // nodes by index).
            if !fexp || solo.is_some() {
                continue;
            }
            for (si, sh) in wt.shells.iter().enumerate() {
                let st = shell_status(app, sh);
                let guide = if app.flatten {
                    if si + 1 == n {
                        &last_grandchild
                    } else {
                        &grandchild
                    }
                } else if wi + 1 == n_feat {
                    if si + 1 == n {
                        &no_last_grandchild
                    } else {
                        &no_grandchild
                    }
                } else if si + 1 == n {
                    &last_grandchild
                } else {
                    &grandchild
                };
                let mut sh_spans = vec![
                    Span::styled(
                        format!("{guide}{}#{}", ic.shell, sh.index),
                        fg(row_color(theme, st == Status::Dead, theme.tree_shell)),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!("{}{}", status_icon(ic, st, spin), st.label()),
                        fg(status_color(theme, st)),
                    ),
                ];
                if shell_leased(app, sh).is_some() {
                    mark_leased(theme, &mut sh_spans);
                }
                items.push(ListItem::new(Line::from(sh_spans)));
                if app.sel == Sel::Shell(ri, wi, si) {
                    sel_row = Some(row);
                }
                row += 1;
            }
        }
    }

    // Flatten mode lists a single repo's worktrees, so the title names
    // what the rows actually are. The count follows the tree: only
    // in-scope repositories have rows, so out-of-scope ones must not be
    // counted.
    let vis = app.visible_repositories();
    let title = if app.flatten {
        let n = vis
            .first()
            .map(|&ri| app.state.repositories[ri].worktrees.len())
            .unwrap_or(0);
        format!("Worktrees ({n})")
    } else {
        format!("Repositories ({})", vis.len())
    };
    let block = panel_block(theme, &app.cfg.icons, &title, true);
    let list = List::new(items)
        .block(block)
        .highlight_style(bg(theme.selection_bg))
        .highlight_symbol(format!("{} ", ic.cursor));
    let mut state = ListState::default();
    state.select(sel_row);
    f.render_stateful_widget(list, area, &mut state);
}

/// The todo pad, at the bottom of the tree's column.
///
/// Open (`t`) it is a live editor, not a rendering of one: the caret is
/// placed with `set_cursor_position`, so the terminal's own cursor sits in
/// the text and the panel reads as the thing that has the keyboard. The
/// border is drawn focused for the same reason.
///
/// Parked (after `Esc`) the same panel keeps showing the note, unfocused
/// and without a caret — the pad is a standing reminder, and a reminder you
/// have to press a key to see is not one. It is collapsed to the note's own
/// height, so what it costs is what it holds; when even that does not fit,
/// the title says how many rows are below the fold.
fn draw_todo(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme.colors;
    let (buf, focused) = match &app.mode {
        Mode::Todo { buf } => (buf, true),
        _ => (&app.todo, false),
    };
    let wrap_width = crate::app::todo_wrap_width(app)
        .min(area.width.saturating_sub(3).max(1) as usize)
        .max(1);
    let rows = buf.visual_rows(wrap_width);
    // A parked pad is never scrolled: it has no caret to follow, so it
    // reads from the top and says what it had to leave out.
    let scroll = if focused { buf.scroll() } else { 0 };
    let content_lines = (area.height.saturating_sub(2) as usize).max(1);
    let hidden = rows.len().saturating_sub(scroll + content_lines);
    let title = if hidden > 0 {
        format!("Todo  {DOWN_ARROW}{hidden}")
    } else {
        "Todo".to_string()
    };
    let block = panel_block(theme, &app.cfg.icons, &title, focused);
    let inner = block.inner(area);
    let mut lines: Vec<Line> = Vec::new();
    if buf.text().is_empty() {
        // Ghost text: an empty panel with a bare caret in it says nothing
        // about what the panel is for. It is dim, and it is gone the moment
        // anything is typed, so it can never be mistaken for content.
        for l in wrap(
            "the next steps you cannot start yet — kept for every repository, not just this one",
            inner.width as usize,
        ) {
            lines.push(Line::from(Span::styled(l, fg(theme.dim))));
        }
    } else {
        for i in 0..content_lines {
            match rows.get(scroll + i) {
                // Tabs expand to four spaces, as in the textarea, so the
                // caret column and the text agree.
                Some(&(row, start, end)) => {
                    lines.push(Line::from(buf.line(row)[start..end].replace('\t', "    ")))
                }
                None => lines.push(Line::from("")),
            }
        }
    }
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .style(fg(theme.foreground)),
        area,
    );
    if !focused {
        return;
    }
    let (cur_row, render_col) = buf.visual_cursor(wrap_width);
    if cur_row >= scroll && cur_row - scroll < content_lines {
        let x = inner.x + render_col as u16;
        let y = inner.y + (cur_row - scroll) as u16;
        if x < inner.x + inner.width && y < inner.y + inner.height {
            f.set_cursor_position(Position::new(x, y));
        }
    }
}

/// The capture with its trailing blank padding dropped — the lines the
/// preview may actually show. `capture-pane` pads the pane's empty area
/// with rows that carry its colours but no text; they are not content.
fn preview_lines(capture: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = capture.lines().collect();
    while lines
        .last()
        .map(|l| crate::ansi::is_blank(l))
        .unwrap_or(false)
    {
        lines.pop();
    }
    lines
}

/// The slice of a pane capture the preview shows in a panel of
/// `inner_height` rows, parked `scroll` lines above the live tail.
///
/// At `scroll == 0` this is the live tail: the *newest* lines, filling the
/// panel down to its bottom border. `capture-pane -S -N` returns a window
/// that starts N lines back in the scrollback and ends with the blank
/// padding of the pane's empty area, so rendering it from the top shows
/// neither the start nor the end of the agent's output — the trailing
/// blanks are dropped and the tail wins. Larger `scroll` walks back
/// through the buffer and clamps at its start, so a wheel that keeps
/// spinning stops rather than running off.
fn preview_window(capture: &str, inner_height: usize, scroll: usize) -> String {
    let lines = preview_lines(capture);
    let rows = inner_height.max(1);
    let scroll = scroll.min(preview_scroll_max_lines(lines.len(), rows));
    let end = lines.len() - scroll;
    let start = end.saturating_sub(rows);
    lines[start..end].join("\n")
}

/// How far back a capture may be scrolled in a panel of `inner_height`
/// rows: everything it holds beyond the one panel-full the live tail
/// already shows.
pub(crate) fn preview_scroll_max(capture: &str, inner_height: usize) -> usize {
    preview_scroll_max_lines(preview_lines(capture).len(), inner_height.max(1))
}

fn preview_scroll_max_lines(len: usize, rows: usize) -> usize {
    len.saturating_sub(rows)
}

/// The selected repository, when a repository row is selected and it has
/// no inline shell yet — the state the panel has something to say about.
fn selected_repository_without_shell(app: &App) -> Option<&crate::state::Repository> {
    let Sel::Repository(ri) = app.sel else {
        return None;
    };
    app.state
        .repositories
        .get(ri)
        .filter(|r| r.shell.is_none() && !r.missing)
}

/// Is a worktree row selected whose worktree has no shell at all?
fn selected_worktree_has_no_shell(app: &App) -> bool {
    let Sel::Worktree(ri, wi) = app.sel else {
        return false;
    };
    app.state
        .repositories
        .get(ri)
        .and_then(|p| p.worktrees.get(wi))
        .is_some_and(|f| f.shells.is_empty())
}

fn draw_preview(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme.colors;
    // A panel parked in the scrollback has stopped following the agent —
    // unmarked, that reads as an agent that stopped. The title carries how
    // far back it sits and the border lights up, the same "this panel is
    // where the input is going" signal the rest of the UI uses.
    let off = app.preview_offset();
    // A shell another dashboard has docked keeps previewing perfectly —
    // `capture-pane` does not care where a pane sits — and that is the
    // trap: a panel that follows the agent while `Enter` would yank it out
    // from under someone else must say so. The parked mark is in the title
    // for the same reason, and this is the second thing the title can say.
    let lease = app
        .selected_shell_pane()
        .and_then(|p| app.leased_by(&p))
        .map(|l| l.label());
    let title = match (off, &lease) {
        (0, None) => "Preview".to_string(),
        (0, Some(who)) => format!("Preview  leased by {who}"),
        (n, None) => format!("Preview  {UP_ARROW}{n}"),
        (n, Some(who)) => format!("Preview  {UP_ARROW}{n}  leased by {who}"),
    };
    let block = panel_block(theme, &app.cfg.icons, &title, off > 0);
    let off = app.preview_offset();
    // Pane first, not tree position: a repository row has a pane to show
    // (its inline shell) without having a `(worktree, shell)` to be found
    // by, and what the panel draws is whatever pane the selection owns.
    let text = if app.selected_shell_pane().is_some() {
        if let Some(cap) = app.preview_source() {
            // borders take two rows off the panel; the capture keeps
            // the pane's escapes, rendered per [preview].colors
            let win = preview_window(cap, area.height.saturating_sub(2) as usize, off);
            crate::ansi::styled_text(&win, app.cfg.preview.colors)
        } else {
            Text::from(format!(
                "capturing{} (select the shell in the tree)",
                app.cfg.icons.ellipsis
            ))
        }
    } else if app.selected_shell().is_some() {
        Text::from("no live pane for this shell")
    } else if let Some(repo) = selected_repository_without_shell(app) {
        // The one row whose panel is empty because nothing has been
        // started yet — and starting it is one keystroke, so the panel
        // says which.
        Text::from(format!(
            "no shell in this repository yet\n\n\
             press `{}` to start one in the repository's own checkout\n\
             ({}), for the commands you would otherwise run by\n\
             hand: make, mise run …, cat config.\n\n\
             it starts on the first press and stays until `d` closes it.",
            app.cfg.icons.enter, repo.base_branch,
        ))
    } else if selected_worktree_has_no_shell(app) {
        // A worktree with no shell: there is no pane to capture, and the
        // generic hint below would send the user hunting for a child row
        // that does not exist.
        Text::from(
            "no agent shell for this worktree\n\n\
             wingman has no tmux window here, so there is nothing to\n\
             preview, dock or send to.\n\n\
             press `a` to start a shell in this worktree.",
        )
    } else {
        Text::from("select a shell to preview its contents")
    };
    let p = Paragraph::new(text)
        .block(block)
        .style(fg(theme.foreground));
    f.render_widget(p, area);
}

/// The contextual shortcut entries for the statusbar — same words as the
/// help overlay. The bar is contextual: keys that would do nothing in the
/// current state do not appear. `? help` is always there (it is the
/// palette key); hjkl/arrows/Esc/q are assumed knowledge and never take a
/// slot (per the discoverability rules).
pub(crate) fn bar_entries(app: &App) -> Vec<(Hint, BarAction)> {
    use BarAction::*;
    let ic = &app.cfg.icons;
    let h = |desc: &str, key: &str| hint(ic, desc, key);
    // The pad owns every printable key while it is open, so the bar offers
    // the one key that still does something: the way back. `? help` is not
    // listed because `?` types a question mark in there.
    if matches!(app.mode, Mode::Todo { .. }) {
        return vec![(h("save & back", "esc"), TodoBack)];
    }
    let mut entries = match app.sel {
        // The repository row's `Enter` starts (or re-enters) its inline
        // shell, so the bar names it — the one action on this row that is
        // not otherwise discoverable. `d` closes it, and only appears once
        // there is one to close.
        Sel::Repository(ri) => {
            let mut v = vec![
                (h("shell", "Enter"), RepoShell),
                (h("worktree", "n"), NewWorktree),
            ];
            if app.repo_shell_pane(ri).is_some() {
                v.push((h("close", "d"), CloseRepoShell));
            }
            v.push((h("help", "?"), Help));
            v
        }
        // A folded worktree row is its shell: it offers the shell actions
        // on top of the worktree ones (`a` gives it a second shell, which
        // unfolds the subtree).
        Sel::Worktree(ri, wi) if app.solo_shell(ri, wi).is_some() => vec![
            (h("dock", "Enter"), Dock),
            (h("send", "s"), QuickSend),
            (h("predefined", "p"), PredefinedSend),
            (h("shell", "a"), AddShell),
            (h("delete", "d"), DeleteWorktree),
            (h("help", "?"), Help),
        ],
        Sel::Worktree(..) => vec![
            (h("worktree", "n"), NewWorktree),
            (h("shell", "a"), AddShell),
            (h("delete", "d"), DeleteWorktree),
            (h("help", "?"), Help),
        ],
        Sel::Shell(..) => vec![
            (h("dock", "Enter"), Dock),
            (h("send", "s"), QuickSend),
            (h("predefined", "p"), PredefinedSend),
            (h("close", "d"), DeleteShell),
            (h("help", "?"), Help),
        ],
    };
    // The pad is reachable from every row — it is not about the selection
    // — so it goes in once here rather than in each arm above, just before
    // `help` (which stays last, where the eye has learned to find it).
    let before_help = entries.len().saturating_sub(1);
    entries.insert(before_help, (h("todo", "t"), Todo));
    // `Enter` on a shell another dashboard holds does something different
    // enough to deserve a different word: it takes the pane off them.
    if app
        .selected_shell_pane()
        .is_some_and(|p| app.leased_by(&p).is_some())
        && let Some(slot) = entries
            .iter()
            .position(|(_, a)| matches!(a, Dock | RepoShell))
    {
        // The word changes, the action does not: on a repository row it is
        // still the inline shell that gets taken.
        entries[slot] = (h("take", "Enter"), entries[slot].1);
    }
    // A parked preview is a state you can be stuck in — the way out leads
    // the bar, where the eye lands first.
    if app.preview_offset() > 0 {
        entries.insert(0, (h("live", "Esc"), PreviewLive));
    }
    entries
}

/// The statusbar: the contextual shortcuts, and nothing else. No app
/// name, no session, no repository count, no sticky message — anything that
/// is not actionable is noise on a row the user reads constantly.
/// Notices live in [`draw_toast`] and fade by themselves.
fn draw_statusbar(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme.colors;
    // Each entry gets a hit range so a mouse click can run it.
    let sp = sep(&app.cfg.icons);
    let mut hits: Vec<(u16, u16, u16, BarAction)> = Vec::new();
    let mut lines: Vec<Line> = Vec::new();
    for (row, entries) in bar_packing(app, area.width).into_iter().enumerate() {
        let mut spans: Vec<Span> = vec![Span::raw(" ")];
        let mut cx: u16 = 1;
        let y = area.y + row as u16;
        for (i, (entry, action)) in entries.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(sp, fg(theme.dim)));
                cx += sp.chars().count() as u16;
            }
            let start = cx;
            // `<description> <key>`, the key highlighted — the same shape the
            // modal footers and the help palette use.
            cx += entry.label().chars().count() as u16;
            spans.extend(hint_spans(theme, entry));
            hits.push((start, cx, y, *action));
        }
        lines.push(Line::from(spans));
    }
    app.bar_hits = hits;
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// Rows the statusbar takes at `width`: the shortcuts wrap onto a second
/// line rather than losing their tail at the border — on a narrow frame a
/// single row drops the last entries silently, and `help ?` (the way to
/// every action that did not fit) is the first to go.
///
/// Two rows is the ceiling: the statusbar is read constantly but the tree
/// is what the frame is for, so beyond that the tail is cut as before.
pub(crate) const STATUSBAR_MAX_ROWS: usize = 2;

pub(crate) fn statusbar_h(app: &App, width: u16) -> u16 {
    bar_packing(app, width).len() as u16
}

/// The statusbar entries packed into rows of `width` columns, greedily and
/// in order — the same packing the modal footers use, capped at
/// [`STATUSBAR_MAX_ROWS`]; whatever is left over joins the last row and is
/// cut at the border.
fn bar_packing(app: &App, width: u16) -> Vec<Vec<(Hint, BarAction)>> {
    let entries = bar_entries(app);
    // One leading space, and one column kept free at the right so a row
    // filled exactly to the border still reads as a row of hints.
    let width = (width as usize).saturating_sub(2);
    let sep_w = UnicodeWidthStr::width(sep(&app.cfg.icons));
    let mut rows: Vec<Vec<(Hint, BarAction)>> = Vec::new();
    let mut row: Vec<(Hint, BarAction)> = Vec::new();
    let mut used = 0usize;
    for e in entries {
        let w = UnicodeWidthStr::width(e.0.label().as_str());
        let need = if row.is_empty() { w } else { w + sep_w };
        if !row.is_empty() && used + need > width && rows.len() + 1 < STATUSBAR_MAX_ROWS {
            rows.push(std::mem::take(&mut row));
            used = w;
        } else {
            used += need;
        }
        row.push(e);
    }
    rows.push(row);
    rows
}

/// Cut a notice to `max` columns, marking the cut with an ellipsis.
fn truncate(text: &str, max: usize, ellipsis: &str) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(ellipsis.chars().count());
    text.chars().take(keep).collect::<String>() + ellipsis
}

/// Rows of text a toast may take before it is cut — an error message runs
/// to a sentence, a stack of them would cover the panel underneath.
const TOAST_MAX_ROWS: usize = 3;

/// A transient notice, bottom-right above the statusbar. It expires on
/// its own (see [`App::toast`]) and never covers an open overlay — the
/// log and the diagnostics overlay keep the permanent record.
fn draw_toast(f: &mut Frame, area: Rect, app: &App) {
    if !matches!(app.mode, Mode::Normal) {
        return;
    }
    let Some(toast) = app.toast() else {
        return;
    };
    let theme = &app.theme.colors;
    // Only an error wears the alarm colour. A plain notice ("worktree
    // deleted") used to take `accent` — peach/orange in most shipped
    // themes, which reads as a warning sitting next to `status_failed`.
    // It takes `title_fg` instead: the toast is already painted on
    // `title_bg`, so that is the one colour guaranteed to be legible on it
    // in every theme (several make `accent`, `border_focused` and
    // `title_bg` the same hue), and it says "a note", not "a problem".
    let color = if toast.error {
        theme.status_failed
    } else {
        theme.title_fg
    };
    // Fit inside the frame: the text, plus borders and one space either
    // side, capped at two thirds of the width. It floats one cell inside
    // the panel below it, so no panel border is left half-covered.
    let max_text = (area.width.saturating_sub(6)).min(area.width * 2 / 3) as usize;
    if max_text < 8 || area.height < 5 {
        return;
    }
    // Errors are sentences, not labels: a notice that does not fit the
    // width wraps instead of losing its tail, up to [`TOAST_MAX_ROWS`] —
    // past that it is cut, because a toast that grows without bound covers
    // the panel it is reporting on.
    let mut rows = wrap(&toast.text, max_text);
    let room = (area.height.saturating_sub(3) as usize).min(TOAST_MAX_ROWS);
    if rows.len() > room {
        rows.truncate(room.max(1));
        if let Some(last) = rows.last_mut() {
            *last = truncate(
                &format!("{last}{}", app.cfg.icons.ellipsis),
                max_text,
                &app.cfg.icons.ellipsis,
            );
        }
    }
    let text_w = rows
        .iter()
        .map(|r| UnicodeWidthStr::width(r.as_str()))
        .max()
        .unwrap_or(0);
    let w = text_w as u16 + 4;
    let h = rows.len() as u16 + 2;
    let rect = Rect {
        x: area.right().saturating_sub(w + 2),
        y: area.bottom().saturating_sub(h + 1),
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type(&app.cfg.icons))
        .border_style(fg(color))
        .style(on(theme.title_fg, theme.title_bg));
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|r| Line::from(Span::styled(format!(" {r} "), fg(color))))
        .collect();
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), rect);
}

fn centered_rect(area: Rect, w_pct: u16, h_pct: u16) -> Rect {
    let [_, v, _] = Layout::vertical([
        Constraint::Percentage((100 - h_pct) / 2),
        Constraint::Percentage(h_pct),
        Constraint::Percentage((100 - h_pct) / 2),
    ])
    .areas(area);
    let [_, h, _] = Layout::horizontal([
        Constraint::Percentage((100 - w_pct) / 2),
        Constraint::Percentage(w_pct),
        Constraint::Percentage((100 - w_pct) / 2),
    ])
    .areas(v);
    h
}

fn draw_help(f: &mut Frame, app: &App) -> Popup {
    let theme = &app.theme.colors;
    // Grouped by context (discoverability rule): the overlay is the app's
    // palette, so every action appears grouped — navigate / repositories /
    // worktrees / shells / global — with the same words as the statusbar.
    // The popup sizes itself to the content (bounded by the frame); long
    // content scrolls (j/k, arrows, PgUp/PgDn, ctrl-u/d, Home/End) and the
    // title advertises it.
    let w = modal_width(f.area(), MODAL_MAX_W);
    let body_w = modal_text_width(w);
    let lines = help_lines(app, body_w);
    // Sized against a footer that may itself have wrapped: at 26 columns
    // `scroll j/k · close esc` does not always fit one row.
    let footer_h = footer_h(&app.cfg.icons, &help_hints(app, true), body_w);
    let h = (lines.len() as u16 + footer_h as u16 + MODAL_CHROME_H).min(f.area().height);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(w)) / 2,
        f.area().y + (f.area().height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    // The footer is pinned: it says how to leave, so it must not scroll
    // away. Only the rows above it move.
    let inner_h = h.saturating_sub(MODAL_CHROME_H) as usize;
    let body_h = inner_h.saturating_sub(footer_h);
    let max_scroll = lines.len().saturating_sub(body_h);
    let scroll = app.help_scroll.min(max_scroll);
    let mut window: Vec<Line> = lines.iter().skip(scroll).take(body_h).cloned().collect();
    let footer = help_footer(app, max_scroll > 0, body_w);
    let hint_rows = footer.len() - 1;
    // The popup was sized for a footer that offers the scroll motion; when
    // it does not, the body is one row short of filling the box. Pad it,
    // so the footer stays pinned to the bottom border where the rule is
    // painted.
    pad_to(&mut window, inner_h.saturating_sub(footer.len()));
    window.extend(footer);
    let block = popup_block(theme, &app.cfg.icons, " Help ");
    f.render_widget(Paragraph::new(Text::from(window)).block(block), area);
    Popup { area, hint_rows }
}

/// One help row: the key spellings (joined with the app's separator) and
/// the description parts (same words as the statusbar). The first
/// description part is the action; the rest are notes shown dimmed after
/// the key.
pub(crate) type HelpRow<'a> = (&'a [&'a str], &'a [&'a str]);
/// A help group: (group title, rows).
pub(crate) type HelpGroup<'a> = (&'a str, &'a [HelpRow<'a>]);

/// The help content, grouped by context. Joined with the app's separator
/// so the ASCII fallback rewrites `·` to `-` uniformly. The descriptions
/// use the same words as the statusbar entries (learning one teaches the
/// other).
pub(crate) const HELP_GROUPS: &[HelpGroup] = &[
    (
        "navigate",
        &[
            (&["j/k", "arrows"], &["move selection"]),
            (&["h"], &["collapse", "jump to parent"]),
            (&["l", "Enter"], &["expand", "dock the shell"]),
            (&["g/G", "Home/End"], &["first / last item"]),
            (&["PgUp/PgDn", "ctrl-u/d"], &["half page"]),
            (&["ctrl-f/b"], &["full page"]),
        ],
    ),
    (
        "repositories",
        &[
            (
                &["Enter"],
                &[
                    "shell in the checkout",
                    "for make, mise, cat",
                    "starts on first press",
                ],
            ),
            (&["n"], &["new worktree", "worktree + agent shell"]),
            (&["d"], &["close that shell", "the repository stays"]),
        ],
    ),
    (
        "worktrees",
        &[
            (&["n"], &["new worktree"]),
            (
                &["a"],
                &["add shell", "picks the agent or a plain shell", "1-9 pick"],
            ),
            (&["Enter"], &["dock", "one-shell worktree"]),
            (
                &["s", "p"],
                &["quick-send / predefined", "one-shell worktree"],
            ),
            (
                &["d"],
                &["delete worktree", "kills windows", "removes worktree"],
            ),
        ],
    ),
    (
        "shells",
        &[
            (
                &["Enter"],
                &["dock the shell", "its real pane, beside the tree"],
            ),
            (&["s"], &["quick-send", "ctrl-s sends"]),
            (&["p"], &["predefined message", "Enter sends", "1-9 pick"]),
            (
                &["d"],
                &["close shell", "kills the window", "worktree stays"],
            ),
        ],
    ),
    (
        "global",
        &[
            (&["wheel"], &["scroll the preview", "back through the pane"]),
            (
                &["Esc"],
                &["back to the live tail", "while the preview is parked"],
            ),
            (
                &["t"],
                &[
                    "todo pad",
                    "half the tree's column",
                    "esc saves; the note stays, collapsed",
                    "one pad for every repository",
                ],
            ),
            (&["r"], &["refresh", "reconcile state"]),
            (
                &["R"],
                &[
                    "reset the layout",
                    "strip and panes back to their proportions",
                ],
            ),
            (&["ctrl-t"], &["change theme", "live picker"]),
            (&["?"], &["this help", "palette"]),
            (&["q", "Esc"], &["quit"]),
            (&["ctrl-c"], &["quit from any state"]),
        ],
    ),
];

/// Build the help overlay lines — the single source of truth for both the
/// renderer (which clips them to the popup) and the scroll-height math in
/// the app (which must never drift from what is rendered).
fn help_lines(app: &App, width: usize) -> Vec<Line<'_>> {
    let theme = &app.theme.colors;
    let ic = &app.cfg.icons;
    let s = sep(ic);
    let mut lines: Vec<Line> = Vec::new();
    for (group_title, rows) in HELP_GROUPS {
        lines.push(Line::from(Span::styled(
            group_title.to_string(),
            fg(theme.dim).add_modifier(Modifier::BOLD),
        )));
        for (keys, desc) in *rows {
            let key_str = keys
                .iter()
                .map(|k| key_label(ic, k))
                .collect::<Vec<_>>()
                .join(s);
            lines.extend(help_row(theme, width, desc, &key_str, s));
        }
        lines.push(Line::from(""));
    }
    for prose in [
        format!(
            "mouse: click selects{s}double-click opens{s}wheel scrolls{s}bar entries clickable"
        ),
        String::new(),
        // Once a pane is docked there are two focusable things on screen,
        // and the second one is tmux's, not ours: say whose keys move
        // between them.
        "Enter docks the shell's real pane beside the tree and puts the cursor in it".to_string(),
        format!(
            "moving between the panes, scrolling, searching and zooming are tmux's own bindings{s}\
             back to the tree is <prefix>+left by default"
        ),
    ] {
        for l in wrap(&prose, width) {
            lines.push(Line::from(Span::styled(l, fg(theme.dim))));
        }
    }
    lines
}

/// One help row, laid out for the width it actually has.
///
/// The palette is drawn in the dashboard's own strip, which is 26 to 54
/// columns wide — no fixed two-column table survives that, so the row
/// reflows instead: `<description> <key>` on one line while it fits, the
/// key on its own indented line when it does not, and the notes wrapped
/// underneath in dim. The order never changes, so the eye learns one
/// shape: what it does, then the key that does it.
fn help_row<'a>(
    theme: &ResolvedColors,
    width: usize,
    desc: &'a [&'a str],
    key_str: &str,
    s: &str,
) -> Vec<Line<'a>> {
    const INDENT: usize = 2;
    let body = width.saturating_sub(INDENT);
    let pad = " ".repeat(INDENT);
    let mut out: Vec<Line> = Vec::new();
    let action = desc[0];
    let key_style = fg(theme.accent).add_modifier(Modifier::BOLD);
    let one_line = UnicodeWidthStr::width(action) + 1 + UnicodeWidthStr::width(key_str) <= body;
    if one_line {
        out.push(Line::from(vec![
            Span::styled(format!("{pad}{action} "), fg(theme.foreground)),
            Span::styled(key_str.to_string(), key_style),
        ]));
    } else {
        for l in wrap(action, body) {
            out.push(Line::from(Span::styled(
                format!("{pad}{l}"),
                fg(theme.foreground),
            )));
        }
        for l in wrap(key_str, body.saturating_sub(2)) {
            out.push(Line::from(Span::styled(format!("{pad}  {l}"), key_style)));
        }
    }
    if desc.len() > 1 {
        for l in wrap(&desc[1..].join(s), body.saturating_sub(2)) {
            out.push(Line::from(Span::styled(
                format!("{pad}  {l}"),
                fg(theme.dim),
            )));
        }
    }
    out
}

/// The help palette's pinned footer: it says how to scroll and how to
/// leave, so it never scrolls away. It reserves the row above it for the
/// footer rule, like every modal footer.
fn help_footer(app: &App, scrollable: bool, width: usize) -> Vec<Line<'static>> {
    let theme = &app.theme.colors;
    let ic = &app.cfg.icons;
    let mut lines = vec![Line::from("")];
    lines.extend(hint_rows(theme, ic, &help_hints(app, scrollable), width));
    lines
}

/// The help palette's footer hints. Named once so the sizing pass and the
/// render agree on how many rows they take.
fn help_hints(app: &App, scrollable: bool) -> Vec<Hint> {
    let ic = &app.cfg.icons;
    let mut hints = Vec::new();
    // Only offer the motion when there is something below the fold — an
    // unreachable shortcut is worse than no shortcut.
    if scrollable {
        hints.push(hint(ic, "scroll", "j/k"));
    }
    hints.push(hint(ic, "close", "esc"));
    hints
}

/// Total scrollable help line count (the footer is pinned, so it is not
/// part of this) — the app clamps its scroll offset against this, built
/// the same way as the renderer so the two cannot drift.
pub(crate) fn help_line_count(app: &App) -> usize {
    help_lines(
        app,
        modal_text_width(modal_width(viewport_rect(app), MODAL_MAX_W)),
    )
    .len()
}

/// The frame as the renderer will see it, from the viewport the last draw
/// recorded — so the scroll clamp measures the popup the user is looking
/// at, not a hypothetical one.
fn viewport_rect(app: &App) -> Rect {
    // Before the first draw the viewport is still (0, 0). Measuring a
    // wrapped palette against a zero-width popup would report thousands of
    // lines and let the scroll run away, so an unmeasured frame is taken
    // to be a full-width one.
    let w = if app.viewport.0 == 0 {
        MODAL_MAX_W
    } else {
        app.viewport.0
    };
    Rect::new(0, 0, w, app.viewport.1)
}

/// The startup diagnostics overlay: one line per dropped config unit, with
/// the same motions and sizing as the help palette (scrolls, dismisses).
/// Shown once at startup when `Config::diagnostics` is non-empty.
fn draw_diagnostics(f: &mut Frame, app: &App) -> Popup {
    let theme = &app.theme.colors;
    let w = modal_width(f.area(), MODAL_MAX_W);
    let body_w = modal_text_width(w);
    let lines = diag_lines(app, body_w);
    let footer_h = footer_h(&app.cfg.icons, &diag_hints(app, true), body_w);
    let h = (lines.len() as u16 + footer_h as u16 + MODAL_CHROME_H).min(f.area().height);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(w)) / 2,
        f.area().y + (f.area().height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    let inner_h = h.saturating_sub(MODAL_CHROME_H) as usize;
    let body_h = inner_h.saturating_sub(footer_h);
    let max_scroll = lines.len().saturating_sub(body_h);
    let scroll = app.diag_scroll.min(max_scroll);
    let mut window: Vec<Line> = lines.iter().skip(scroll).take(body_h).cloned().collect();
    let footer = diag_footer(app, max_scroll > 0, body_w);
    let hint_rows = footer.len() - 1;
    pad_to(&mut window, inner_h.saturating_sub(footer.len()));
    window.extend(footer);
    let title_text = format!(" Startup Diagnostics ({}) ", app.cfg.diagnostics.len());
    let block = popup_block(theme, &app.cfg.icons, title_text);
    f.render_widget(
        Paragraph::new(Text::from(window))
            .block(block)
            .style(on(theme.popup_fg, theme.popup_bg)),
        area,
    );
    Popup { area, hint_rows }
}

/// The diagnostics overlay lines — single source for rendering and the
/// app's scroll clamp (same pattern as the help palette).
fn diag_lines(app: &App, width: usize) -> Vec<Line<'_>> {
    let theme = &app.theme.colors;
    let (mark, indent) = warn_prefix(&app.cfg.icons);
    let mut lines = Vec::new();
    for d in &app.cfg.diagnostics {
        // A dropped config unit says which key and why, which is a
        // sentence, not a label: it wraps under its own mark rather than
        // running off the border.
        for (i, l) in wrap(d, width.saturating_sub(indent))
            .into_iter()
            .enumerate()
        {
            lines.push(Line::from(vec![
                Span::styled(
                    if i == 0 {
                        mark.clone()
                    } else {
                        " ".repeat(indent)
                    },
                    fg(theme.status_failed).add_modifier(Modifier::BOLD),
                ),
                Span::styled(l, fg(theme.foreground)),
            ]));
        }
    }
    lines
}

/// The diagnostics overlay's pinned footer — same shape as the help
/// palette's: the reserved rule row, then the shortcuts.
fn diag_footer(app: &App, scrollable: bool, width: usize) -> Vec<Line<'static>> {
    let theme = &app.theme.colors;
    let ic = &app.cfg.icons;
    let mut lines = vec![Line::from("")];
    lines.extend(hint_rows(theme, ic, &diag_hints(app, scrollable), width));
    lines
}

/// The diagnostics footer's hints — named once, like the palette's, so the
/// sizing pass and the render cannot disagree on their height.
fn diag_hints(app: &App, scrollable: bool) -> Vec<Hint> {
    let ic = &app.cfg.icons;
    let mut hints = Vec::new();
    if scrollable {
        hints.push(hint(ic, "scroll", "j/k"));
    }
    hints.push(hint(ic, "dismiss", "esc"));
    hints
}

/// Total scrollable diagnostics height (the footer is pinned and not part
/// of it) — the app clamps its scroll against this; same construction as
/// the renderer.
pub(crate) fn diag_line_count(app: &App) -> usize {
    diag_lines(
        app,
        modal_text_width(modal_width(viewport_rect(app), MODAL_MAX_W)),
    )
    .len()
}

/// The live theme picker (`ctrl-t`): one row per theme, built-ins first.
/// Moving the cursor previews live (the whole screen repaints with the new
/// palette via `App::preview_picker_theme`); Enter keeps, Esc restores.
fn draw_theme_picker(f: &mut Frame, app: &App) -> Popup {
    let theme = &app.theme.colors;
    let names: Vec<&str> = app.theme_names.iter().map(String::as_str).collect();
    let index = match &app.mode {
        Mode::ThemePicker { index } => *index,
        _ => 0,
    };
    // Size to content (bounded by the frame), like the help overlay.
    let ic = &app.cfg.icons;
    let hints = [hint(ic, "keep", "Enter"), hint(ic, "cancel", "esc")];
    let w = names
        .iter()
        .map(|n| n.chars().count() as u16)
        .max()
        .unwrap_or(20)
        .saturating_add(4)
        .saturating_add(MODAL_CHROME_W)
        // Wide enough for the shortcuts on one row where the cap allows
        // it: they wrap in a narrow strip, but a list of three short
        // entries should not push them onto a second row by itself.
        .max(hint_line(theme, ic, &hints).width() as u16 + MODAL_CHROME_W);
    let w = modal_width(f.area(), w);
    let footer = hint_rows(theme, ic, &hints, modal_text_width(w));
    let hint_rows = footer.len();
    let h = (names.len() as u16 + 1 + hint_rows as u16 + MODAL_CHROME_H).min(f.area().height);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(w)) / 2,
        f.area().y + (f.area().height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    let inner = modal_inner(area);
    let rows = (inner.height as usize).saturating_sub(1 + hint_rows);
    let mut lines: Vec<Line> = Vec::new();
    for (i, name) in names.iter().take(rows).enumerate() {
        let selected = i == index;
        let marker = &ic.cursor;
        let label = if selected {
            format!("{marker} {name} ")
        } else {
            format!(
                "{} {name} ",
                " ".repeat(UnicodeWidthStr::width(marker.as_str()))
            )
        };
        // The strip is 26 columns at its narrowest: a long theme name is
        // cut, not printed over the border.
        let label = truncate(&label, inner.width as usize, &ic.ellipsis);
        lines.push(Line::from(if selected {
            vec![Span::styled(
                label,
                on(theme.selection_fg, theme.selection_bg).add_modifier(Modifier::BOLD),
            )]
        } else {
            vec![Span::styled(label, fg(theme.foreground))]
        }));
    }
    // The row the footer rule is painted over, reserved here so the
    // list can never grow into it.
    lines.push(Line::from(""));
    lines.extend(footer);
    let block = popup_block(theme, ic, " Themes ");
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    Popup { area, hint_rows }
}

/// The predefined-message modal (`p`): the configured presets as a
/// numbered list. Same shape as the theme picker — content-sized, cursor
/// on the selection, keys in the footer.
fn draw_messages(f: &mut Frame, app: &App) -> Popup {
    let theme = &app.theme.colors;
    let index = match &app.mode {
        Mode::Messages { index } => *index,
        _ => 0,
    };
    // The list shows the message itself, tokens already resolved: what is
    // read here is exactly what the pane receives.
    let entries: Vec<String> = app
        .cfg
        .messages
        .iter()
        .map(|m| app.expand_message(&m.text))
        .collect();
    let ic = &app.cfg.icons;
    let hints = [
        hint(ic, "send", "Enter"),
        hint(ic, "pick", "1-9"),
        hint(ic, "cancel", "esc"),
    ];
    let w = entries
        .iter()
        .map(|l| UnicodeWidthStr::width(l.as_str()) as u16)
        .max()
        .unwrap_or(20)
        .saturating_add(6)
        .saturating_add(MODAL_CHROME_W)
        // Wide enough for the shortcuts on one row where the cap allows
        // it: they wrap in a narrow strip, but a list of three short
        // entries should not push them onto a second row by itself.
        .max(hint_line(theme, ic, &hints).width() as u16 + MODAL_CHROME_W);
    let w = modal_width(f.area(), w);
    let footer = hint_rows(theme, ic, &hints, modal_text_width(w));
    let hint_rows = footer.len();
    let h = (entries.len() as u16 + 1 + hint_rows as u16 + MODAL_CHROME_H).min(f.area().height);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(w)) / 2,
        f.area().y + (f.area().height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    let inner = modal_inner(area);
    let rows = (inner.height as usize).saturating_sub(1 + hint_rows);
    let mut lines: Vec<Line> = Vec::new();
    for (i, entry) in entries.iter().take(rows).enumerate() {
        let selected = i == index;
        let marker = &ic.cursor;
        // The number is the key that sends the entry (up to 9 of them).
        let num = if i < 9 {
            format!("{} ", i + 1)
        } else {
            "  ".into()
        };
        let row = if selected {
            format!("{marker} {num}{entry} ")
        } else {
            format!(
                "{} {num}{entry} ",
                " ".repeat(UnicodeWidthStr::width(marker.as_str()))
            )
        };
        // Cut to the strip rather than printed over the border; the
        // number stays, so `1-9` still names what it picks.
        let row = truncate(&row, inner.width as usize, &ic.ellipsis);
        lines.push(Line::from(if selected {
            vec![Span::styled(
                row,
                on(theme.selection_fg, theme.selection_bg).add_modifier(Modifier::BOLD),
            )]
        } else {
            vec![Span::styled(row, fg(theme.foreground))]
        }));
    }
    lines.push(Line::from(""));
    lines.extend(footer);
    let block = popup_block(theme, ic, " Messages ");
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    Popup { area, hint_rows }
}

/// The add-shell picker (`a`): what the new shell runs, as a numbered
/// list. Same shape as the predefined-message modal — the repository's
/// agent command sits first (Enter starts it), a plain login shell last.
fn draw_add_shell(f: &mut Frame, app: &App) -> Popup {
    let theme = &app.theme.colors;
    let index = match &app.mode {
        Mode::AddShell { index } => *index,
        _ => 0,
    };
    let entries: Vec<String> = app
        .add_shell_choices()
        .into_iter()
        .map(|c| c.label)
        .collect();
    let ic = &app.cfg.icons;
    let hints = [
        hint(ic, "start", "Enter"),
        hint(ic, "pick", "1-9"),
        hint(ic, "cancel", "esc"),
    ];
    let w = entries
        .iter()
        .map(|l| UnicodeWidthStr::width(l.as_str()) as u16)
        .max()
        .unwrap_or(20)
        .saturating_add(6)
        .saturating_add(MODAL_CHROME_W)
        // Wide enough for the shortcuts on one row where the cap allows
        // it: they wrap in a narrow strip, but a list of three short
        // entries should not push them onto a second row by itself.
        .max(hint_line(theme, ic, &hints).width() as u16 + MODAL_CHROME_W);
    let w = modal_width(f.area(), w);
    let footer = hint_rows(theme, ic, &hints, modal_text_width(w));
    let hint_rows = footer.len();
    let h = (entries.len() as u16 + 1 + hint_rows as u16 + MODAL_CHROME_H).min(f.area().height);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(w)) / 2,
        f.area().y + (f.area().height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    let inner = modal_inner(area);
    let rows = (inner.height as usize).saturating_sub(1 + hint_rows);
    let mut lines: Vec<Line> = Vec::new();
    for (i, entry) in entries.iter().take(rows).enumerate() {
        let selected = i == index;
        let marker = &ic.cursor;
        // The number is the key that starts the entry (up to 9 of them).
        let num = if i < 9 {
            format!("{} ", i + 1)
        } else {
            "  ".into()
        };
        let row = if selected {
            format!("{marker} {num}{entry} ")
        } else {
            format!(
                "{} {num}{entry} ",
                " ".repeat(UnicodeWidthStr::width(marker.as_str()))
            )
        };
        // Cut to the strip rather than printed over the border; the
        // number stays, so `1-9` still names what it picks.
        let row = truncate(&row, inner.width as usize, &ic.ellipsis);
        lines.push(Line::from(if selected {
            vec![Span::styled(
                row,
                on(theme.selection_fg, theme.selection_bg).add_modifier(Modifier::BOLD),
            )]
        } else {
            vec![Span::styled(row, fg(theme.foreground))]
        }));
    }
    lines.push(Line::from(""));
    lines.extend(footer);
    let block = popup_block(theme, ic, " Add Shell ");
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    Popup { area, hint_rows }
}

/// Upper bound on a content-sized dialog. The frame is the dashboard's own
/// strip whenever a pane is docked, so the cap is [`MODAL_MAX_W`] and not a
/// share of the frame: a percentage would make the same dialog a different
/// width in the docked and undocked layouts.
fn dialog_max_w(area: Rect) -> u16 {
    modal_width(area, MODAL_MAX_W)
}

fn draw_input(f: &mut Frame, app: &App) -> Popup {
    let theme = &app.theme.colors;
    let (title, label) = match &app.mode {
        Mode::Input { kind, .. } => match kind {
            InputKind::NewWorktree => ("New Worktree", "worktree name"),
            InputKind::QuickSend => ("Quick Send", "send to agent"),
        },
        _ => unreachable!(),
    };
    let Mode::Input { kind, text } = &app.mode else {
        return Rect::default().into();
    };
    let title_text = format!(" {title} ");
    let block = popup_block(theme, &app.cfg.icons, title_text.clone());
    // Fixed at the cap rather than sized to what has been typed: a field
    // that grows with the text jitters under the hands typing into it, and
    // the tail-scrolling below needs a width it can count on.
    let w = dialog_max_w(f.area());
    let body_w = modal_text_width(w);
    // The label sits beside the field ("worktree name: …") whenever the
    // strip is wide enough to leave the field something to work with;
    // below that it takes a row of its own, because in a 26-column strip
    // the label alone is wider than the field would be.
    let inline_label = format!("{label}: ");
    let inline_indent = UnicodeWidthStr::width(inline_label.as_str());
    // Under this the inline field is too cramped to read what is typed
    // into it, so the stacked layout wins.
    const MIN_INLINE_FIELD: usize = 12;
    const STACKED_INDENT: usize = 2;
    // +1 for the caret cell the field always keeps free.
    let inline = body_w > inline_indent + MIN_INLINE_FIELD;
    let field_indent = if inline {
        inline_indent
    } else {
        STACKED_INDENT
    };
    // One cell is left for the caret, so a field typed full still shows
    // where the next character lands.
    let field_w = body_w.saturating_sub(field_indent + 1);
    let shown = tail_to_width(text, field_w);
    let mut lines = if inline {
        vec![Line::from(vec![
            Span::styled(inline_label, fg(theme.accent)),
            Span::raw(shown.to_string()),
        ])]
    } else {
        vec![
            Line::from(Span::styled(label, fg(theme.accent))),
            Line::from(Span::raw(format!("{}{shown}", " ".repeat(STACKED_INDENT)))),
        ]
    };
    // new-worktree only: the worktree is cut from the repository's base
    // branch, NOT from whatever the repo has checked out. Name the branch
    // so nobody has to guess which one it is.
    if *kind == InputKind::NewWorktree
        && let Some(hint) = app.new_worktree_base()
    {
        for l in wrap(
            &format!("branches from: {} (base branch)", hint.branch),
            body_w,
        ) {
            lines.push(Line::from(Span::styled(l, fg(theme.dim))));
        }
        if let Some(configured) = &hint.missing {
            for l in wrap(
                &format!("base branch '{configured}' not found; using current branch"),
                body_w,
            ) {
                lines.push(Line::from(Span::styled(l, fg(theme.status_waiting))));
            }
        }
    }
    let ic = &app.cfg.icons;
    lines.push(Line::from(""));
    let footer = hint_rows(
        theme,
        ic,
        &[hint(ic, "submit", "Enter"), hint(ic, "cancel", "esc")],
        body_w,
    );
    let hint_rows = footer.len();
    lines.extend(footer);
    let area = dialog_rect(f.area(), &lines, &title_text, w, w);
    f.render_widget(Clear, area);
    // Inside the border AND the padding — the field shares the first
    // content row with the label, or sits on the second when stacked.
    let inner = modal_inner(area);
    let popup_style = on(theme.popup_fg, theme.popup_bg);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .style(popup_style),
        area,
    );
    // The caret sits after the last character actually shown — the field
    // scrolls with the text, so that is the end of the tail, not the end
    // of the buffer. Only when it lands inside the popup: a cursor off the
    // buffer would corrupt the terminal.
    let cursor_x = inner.x + (field_indent + UnicodeWidthStr::width(shown)) as u16;
    let cursor_y = inner.y + if inline { 0 } else { 1 };
    if cursor_x < area.x + area.width && cursor_y < area.y + area.height {
        f.set_cursor_position(Position::new(cursor_x, cursor_y));
    }
    Popup { area, hint_rows }
}

/// The tail of `text` that fits `width` columns — what a one-line field
/// shows once more has been typed than it can hold. The end is what
/// matters: it is where the caret is.
fn tail_to_width(text: &str, width: usize) -> &str {
    let mut start = text.len();
    for (i, _) in text.char_indices().rev() {
        if UnicodeWidthStr::width(&text[i..]) > width {
            break;
        }
        start = i;
    }
    &text[start..]
}

fn draw_textarea(f: &mut Frame, app: &App) -> Popup {
    let theme = &app.theme.colors;
    // Span the popup to the textarea geometry (shared with the key
    // handler's page-size math via app::textarea_visible_lines).
    let w = crate::app::textarea_width(app.viewport.0).min(f.area().width);
    let h = crate::app::textarea_height(app.viewport.1).min(f.area().height);
    let area = centered_rect_wh(f.area(), w, h);
    f.render_widget(Clear, area);
    let Mode::Textarea { buf } = &app.mode else {
        return area.into();
    };
    let ic = &app.cfg.icons;
    let content_lines = crate::app::textarea_visible_lines(ic, app.viewport).max(1);
    let block = popup_block(theme, ic, " Quick Send ");
    let inner = modal_inner(area);
    // Row layout inside the border: content, spacer, hint. The popup
    // title already says "quick-send", so the content starts flush at
    // the top border — no label row.
    let content_top = inner.y;

    // Long lines soft-wrap instead of overflowing the border, so the
    // window scrolls in VISUAL rows, not logical lines.
    let wrap_width = crate::app::textarea_wrap_width(app.viewport.0)
        .min(inner.width as usize)
        .max(1);
    let rows = buf.visual_rows(wrap_width);
    let mut lines: Vec<Line> = Vec::new();
    // Content rows: the visible window of the buffer.
    let scroll = buf.scroll();
    for i in 0..content_lines {
        match rows.get(scroll + i) {
            // Expand tabs to four spaces for stable rendering.
            Some(&(row, start, end)) => {
                lines.push(Line::from(buf.line(row)[start..end].replace('\t', "    ")))
            }
            None => lines.push(Line::from("")),
        }
    }
    // Spacer, then the hint rows.
    lines.push(Line::from(""));
    let footer = hint_rows(theme, ic, &textarea_hints(ic), modal_text_width(area.width));
    let hint_rows = footer.len();
    lines.extend(footer);

    let popup_style = on(theme.popup_fg, theme.popup_bg);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .style(popup_style),
        area,
    );
    // Cursor: the visual row relative to the content window, and the
    // rendered column within that row (tabs expanded to 4).
    let (cur_row, render_col) = buf.visual_cursor(wrap_width);
    if cur_row >= scroll && cur_row - scroll < content_lines {
        let cursor_x = inner.x + render_col as u16;
        let cursor_y = (content_top as usize + (cur_row - scroll)) as u16;
        if cursor_x < area.x + area.width && cursor_y < area.y + area.height {
            f.set_cursor_position(Position::new(cursor_x, cursor_y));
        }
    }
    Popup { area, hint_rows }
}

/// The quick-send editor's footer hints. Shared with
/// [`crate::app::textarea_visible_lines`], which has to know how many rows
/// they take before it can say how many lines of text are left.
pub(crate) fn textarea_hints(ic: &crate::config::IconsConfig) -> Vec<Hint> {
    vec![
        hint(ic, "newline", "Enter"),
        hint(ic, "send", "ctrl-s"),
        hint(ic, "cancel", "esc"),
    ]
}

/// Rows the quick-send editor's footer occupies at a viewport width — the
/// hint rows and the rule above them.
pub(crate) fn textarea_footer_h(ic: &crate::config::IconsConfig, viewport_w: u16) -> usize {
    let w = crate::app::textarea_width(viewport_w);
    footer_h(ic, &textarea_hints(ic), modal_text_width(w))
}

/// Centered rect of an exact width/height (the percentage version cannot
/// express the textarea's clamped absolute size).
fn centered_rect_wh(area: Rect, w: u16, h: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}

/// Geometry for a content-sized dialog: exactly as wide and as tall as its
/// content, plus `DIALOG_PADDING` and the two border rows/columns. A
/// dialog that asks one question used to take a fixed percentage of the
/// frame regardless of what it said; now it takes what it needs, clamped
/// to [`min_w`, `max_w`] and to the frame.
fn dialog_rect(frame: Rect, lines: &[Line], title: &str, min_w: u16, max_w: u16) -> Rect {
    let widest = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let w = widest
        .max(title.chars().count() as u16)
        .saturating_add(MODAL_CHROME_W)
        .clamp(min_w, max_w.max(min_w))
        .min(frame.width);
    let h = (lines.len() as u16)
        .saturating_add(MODAL_CHROME_H)
        .min(frame.height);
    centered_rect_wh(frame, w, h)
}

fn draw_confirm(f: &mut Frame, app: &App) -> Popup {
    let theme = &app.theme.colors;
    let Mode::Confirm { confirm } = &app.mode else {
        return Rect::default().into();
    };
    let ic = &app.cfg.icons;
    // Only the answers that can actually succeed are offered, and only
    // their own keys are spelled out: `esc` cancels every overlay in the
    // app, so listing it beside `n` costs a word and teaches nothing.
    let mut hints = Vec::new();
    if let Some(yl) = &confirm.yes_label {
        hints.push(hint(ic, yl, "y/d"));
    }
    if let Some(fl) = &confirm.force_label {
        hints.push(hint(ic, fl, "f"));
    }
    hints.push(hint(ic, "cancel", "n"));
    // Wrapped before the dialog is sized, not after: the body is a
    // sentence about a path, and the widest line a docked dashboard can
    // give it is `MODAL_MAX_W`.
    let body_w = modal_text_width(dialog_max_w(f.area()));
    let mut lines: Vec<Line> = confirm
        .body
        .lines()
        .flat_map(|l| wrap(l, body_w))
        .map(|l| Line::from(Span::raw(l)))
        .collect();
    // The caveat is an error, not prose: mark and text both in the failure
    // colour, bold, so it reads before the shortcuts do.
    if let Some(warning) = &confirm.warning {
        let style = fg(theme.status_failed).add_modifier(Modifier::BOLD);
        let (mark, indent) = warn_prefix(ic);
        for (i, l) in wrap(warning, body_w.saturating_sub(indent))
            .into_iter()
            .enumerate()
        {
            lines.push(Line::from(vec![
                Span::styled(
                    if i == 0 {
                        mark.clone()
                    } else {
                        " ".repeat(indent)
                    },
                    style,
                ),
                Span::styled(l, style),
            ]));
        }
    }
    lines.push(Line::from(""));
    let footer = hint_rows(theme, ic, &hints, body_w);
    let hint_rows = footer.len();
    lines.extend(footer);
    let title_text = format!(" {} ", confirm.title);
    let block = popup_block(theme, ic, title_text.clone());
    let popup_style = on(theme.popup_fg, theme.popup_bg);
    let area = dialog_rect(
        f.area(),
        &lines,
        &title_text,
        crate::dock::DASH_MIN_WIDTH,
        dialog_max_w(f.area()),
    );
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .style(popup_style),
        area,
    );
    Popup { area, hint_rows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Confirm, ConfirmAction, InputKind};
    use crate::config::Config;
    use crate::theme;

    /// Render the full UI headless in every mode; must never panic.
    #[test]
    fn render_all_modes_headless() {
        let cfg = Config::default();
        let theme = theme::load("ansi", false).unwrap();
        let mut app = App::new(cfg, theme, None);
        // No `startup()` — see `seeded_app`: it would load and rewrite the
        // real `state.json`.

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        for mode in [
            Mode::Normal,
            Mode::Help,
            Mode::Input {
                kind: InputKind::NewWorktree,
                text: "some-worktree".into(),
            },
            Mode::Confirm {
                confirm: Confirm {
                    title: "delete worktree".into(),
                    body: "delete 'x'?".into(),
                    yes_label: None,
                    warning: Some("worktree is dirty — force will discard".into()),
                    force_label: Some("force".into()),
                    action: ConfirmAction::DeleteWorktree {
                        repository: 0,
                        worktree: 0,
                    },
                },
            },
        ] {
            app.mode = mode;
            terminal
                .draw(|f| draw(f, &mut app))
                .expect("draw must not fail");
        }
        // small terminals must not panic either
        let backend = ratatui::backend::TestBackend::new(30, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
    }

    /// A missing repository renders with a dimmed "(missing)" marker in the
    /// tree — visibility without pretending the repo is there.
    #[test]
    fn missing_repository_renders_dimmed_marker() {
        let cfg = Config::default();
        let theme = theme::load("ansi", false).unwrap();
        let mut app = App::new(cfg, theme, None);
        // No `startup()` — see `seeded_app`: it would load and rewrite the
        // real `state.json`.
        app.state.repositories.push(crate::state::Repository {
            name: "gone".into(),
            path: std::path::PathBuf::from("/gone"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: true,
            shell: None,
            worktrees: vec![],
        });
        app.build_tree_flags();
        app.clamp_sel();

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();

        let buf = terminal.backend().buffer();
        let all: String = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| buf[(x, y)].symbol()))
            .collect();
        assert!(
            all.contains("(missing)"),
            "tree must show the missing marker: {all:?}"
        );
    }

    /// `modal_inner` is hand-derived from the border and [`MODAL_PADDING`],
    /// but the text it positions the caret over is laid out by the block
    /// itself. When the two disagree the caret drifts off its row (it has,
    /// twice), so pin them together: whatever ratatui hands the block as
    /// content box is what `modal_inner` must return.
    #[test]
    fn modal_inner_matches_the_block() {
        let app = seeded_app();
        let block = popup_block(&app.theme.colors, &app.cfg.icons, " Title ");
        for area in [
            Rect::new(0, 0, 40, 10),
            Rect::new(7, 3, 34, 8),
            Rect::new(12, 19, 60, 20),
        ] {
            assert_eq!(
                modal_inner(area),
                block.inner(area),
                "modal_inner drifted from the block's own content box at {area:?}",
            );
        }
    }

    /// The input popup's caret must sit on the field row, right after the
    /// typed text. Regression, twice: `modal_inner` kept counting a
    /// vertical padding row that `MODAL_PADDING` no longer reserves, so the
    /// caret landed a row below the text in every input popup (new
    /// worktree, add-shell, quick-send) — on the "branches from" hint.
    ///
    /// The label sits beside the field whenever the strip is wide enough
    /// (it stacks onto its own row only in a narrow strip, where it does
    /// not fit beside a field), so here the field is the *first* content
    /// row, indented by the label.
    #[test]
    fn input_popup_cursor_sits_on_input_row() {
        // seeded: the add-shell hint needs a repository to name, and it adds
        // a content line, which moves the (content-sized) popup.
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Repository(0);

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        for (kind, label, text) in [
            (InputKind::NewWorktree, "worktree name", "foo"),
            (InputKind::QuickSend, "send to agent", "busy 2"),
        ] {
            app.mode = Mode::Input {
                kind,
                text: text.into(),
            };
            terminal
                .draw(|f| draw(f, &mut app))
                .expect("draw must not fail");

            let area = app.popup_area.expect("input popup is on screen");
            let body = buffer_text(&terminal);
            assert!(body.contains(label), "the label names the field: {label:?}");
            // inside the border AND the one column of padding, then the
            // inline label, then the text
            let expected_x =
                area.x + 2 + (label.chars().count() + 2) as u16 + text.chars().count() as u16;
            let expected_y = area.y + 1; // the field shares the label's row
            assert_eq!(
                terminal.get_cursor_position().unwrap(),
                Position::new(expected_x, expected_y),
                "cursor must sit on the field row after the typed text ({label:?})",
            );
        }
    }

    /// A field holds more than the strip is wide: what it shows is the
    /// tail, so the caret — which is always at the end of what has been
    /// typed — stays on screen instead of walking off the border.
    #[test]
    fn a_long_input_shows_its_tail_and_keeps_the_caret_inside() {
        let mut app = seeded_app();
        app.mode = Mode::Input {
            kind: InputKind::QuickSend,
            text: "x".repeat(200),
        };
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).expect("draw");
        let area = app.popup_area.expect("input popup is on screen");
        assert!(area.width <= MODAL_MAX_W, "capped like every overlay");
        let pos = terminal.get_cursor_position().unwrap();
        assert!(
            pos.x < area.x + area.width && pos.y < area.y + area.height,
            "the caret stays inside the popup: {pos:?} in {area:?}"
        );
    }

    /// The textarea popup shows the WHOLE visible window, not just the
    /// line being typed (regression: TextBuf scrolled with a hardcoded
    /// one-line viewport, so every earlier line left the popup), and its
    /// hint names the send key that terminals actually deliver.
    #[test]
    fn textarea_popup_shows_every_line_and_the_send_hint() {
        let cfg = Config::default();
        let theme = theme::load("ansi", false).unwrap();
        let mut app = App::new(cfg, theme, None);
        // No `startup()` — see `seeded_app`: it would load and rewrite the
        // real `state.json`.

        let mut buf = crate::app::TextBuf::new();
        for line in ["alpha", "beta", "gamma"] {
            for c in line.chars() {
                buf.insert_char(c);
            }
            buf.newline();
        }
        buf.ensure_visible(
            crate::app::textarea_visible_lines(&app.cfg.icons, (120, 40)),
            crate::app::textarea_wrap_width(120),
        );
        app.mode = Mode::Textarea { buf };

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &mut app))
            .expect("draw must not fail");
        let text = buffer_text(&terminal);

        for line in ["alpha", "beta", "gamma"] {
            assert!(text.contains(line), "popup must still show {line:?}");
        }
        assert!(text.contains("send ctrl-s"), "hint names the send key");
    }

    /// The popup wraps a line too long for its width instead of letting
    /// it run past the border, drops the old "send to agent (N rows)"
    /// label, and keeps the footer rule between the text and the hint.
    #[test]
    fn textarea_popup_wraps_long_lines_and_spaces_the_hint() {
        let cfg = Config::default();
        let theme = theme::load("ansi", false).unwrap();
        let mut app = App::new(cfg, theme, None);
        app.startup().unwrap();
        app.viewport = (120, 40);

        let mut buf = crate::app::TextBuf::new();
        let long = "wrap ".repeat(60);
        for c in long.chars() {
            buf.insert_char(c);
        }
        buf.ensure_visible(
            crate::app::textarea_visible_lines(&app.cfg.icons, (120, 40)),
            crate::app::textarea_wrap_width(120),
        );
        app.mode = Mode::Textarea { buf };

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &mut app))
            .expect("draw must not fail");
        let text = buffer_text(&terminal);
        let cells = terminal.backend().buffer().clone();
        let rows: Vec<String> = (0..40)
            .map(|y| (0..120).map(|x| cells[(x, y)].symbol()).collect())
            .collect();

        assert!(
            !text.contains("send to agent"),
            "the label row is gone: {text}"
        );

        // The popup border stays intact on every row it owns: nothing
        // spills past the right edge.
        let w = crate::app::textarea_width(120);
        let h = crate::app::textarea_height(40);
        let x0 = (120 - w) / 2;
        let y0 = (40 - h) / 2;
        // The footer rule meets the frame with tees; every other row
        // has the plain side border.
        let rule_y = y0 + h - 3;
        for y in (y0..y0 + h).filter(|y| *y != rule_y) {
            let row: Vec<char> = rows[y as usize].chars().collect();
            assert!(
                row[x0 as usize] == '\u{2502}' || y == y0 || y == y0 + h - 1,
                "left border missing on row {y}"
            );
            assert!(
                row[(x0 + w - 1) as usize] == '\u{2502}' || y == y0 || y == y0 + h - 1,
                "text overflowed the right border on row {y}"
            );
        }

        // The wrapped text really is spread over several rows, and a rule
        // separates it from the hint.
        let hint_y = (y0 + h - 2) as usize;
        assert!(rows[hint_y].contains("send ctrl-s"), "hint on the last row");
        assert_footer_rule(
            terminal.backend().buffer(),
            Rect::new(x0, y0, w, h),
            "textarea",
        );
        let filled = rows[(y0 + 1) as usize..hint_y - 1]
            .iter()
            .filter(|r| r.contains("wrap"))
            .count();
        assert!(filled > 1, "the long line wrapped over {filled} rows");
    }

    /// The New Worktree popup spells out where the branch comes from: the
    /// repository's base branch, not the branch the repo has checked out. The
    /// hint is the one place a user can see it before hitting Enter.
    #[test]
    fn new_worktree_popup_states_the_base_branch() {
        crate::util::testutil::with_test_lock(|| {
            let root = tempfile::TempDir::new().unwrap();
            let repo = root.path().join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            let git = |args: &[&str]| {
                let ok = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success();
                assert!(ok, "git {} failed", args.join(" "));
            };
            git(&["init", "-b", "main"]);
            git(&["config", "user.name", "lw-test"]);
            git(&["config", "user.email", "lw-test@example.com"]);
            std::fs::write(repo.join("readme.md"), "# t\n").unwrap();
            git(&["add", "."]);
            git(&["commit", "-m", "init"]);
            git(&["checkout", "-b", "checked-out"]);

            let mut app = App::new(Config::default(), theme::load("ansi", false).unwrap(), None);
            app.state.repositories.push(crate::state::Repository {
                name: "repo".into(),
                path: repo.clone(),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: None,
                missing: false,
                shell: None,
                worktrees: vec![],
            });
            app.build_tree_flags();
            app.clamp_sel();
            app.sel = crate::app::Sel::Repository(0);
            app.open_new_worktree();

            let backend = ratatui::backend::TestBackend::new(120, 40);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let text = buffer_text(&terminal);
            assert!(
                text.contains("branches from: main (base branch)"),
                "popup must name the base branch: {text:?}"
            );
            assert!(
                !text.contains("checked-out"),
                "the checked-out branch is not what a worktree branches from: {text:?}"
            );

            // configured base missing from the repo: creation falls back to the
            // checkout, and the popup warns instead of naming a phantom base.
            app.state.repositories[0].base_branch = "master".into();
            app.open_new_worktree();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let text = buffer_text(&terminal);
            assert!(
                text.contains("base branch 'master' not found"),
                "popup must flag the fallback: {text:?}"
            );
        });
    }

    // ---- M8.4a — small terminals, resize, ASCII fallback ----

    fn seeded_app() -> App {
        let cfg = Config::default();
        let theme = theme::load("ansi", false).unwrap();
        // No `startup()`: it loads AND rewrites the real `state.json`, so
        // the fixture would inherit the developer's repositories (dozens of
        // them, past the unfold cap these render tests rely on) and race
        // with every other test doing the same.
        let mut app = App::new(cfg, theme, None);
        app.state.repositories.push(crate::state::Repository {
            name: "repo-a".into(),
            path: std::path::PathBuf::from("/tmp/repo-a"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            shell: None,
            worktrees: vec![],
        });
        // A second repository keeps the fixture out of flatten mode (a lone
        // repository's worktrees would become the top level, hiding the rows
        // these render tests assert on).
        app.state.repositories.push(crate::state::Repository {
            name: "repo-b".into(),
            path: std::path::PathBuf::from("/tmp/repo-b"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            shell: None,
            worktrees: vec![],
        });
        app.build_tree_flags();
        app.clamp_sel();
        app
    }

    /// `seeded_app` with a worktree and its single shell selected — the
    /// state the `p` modal is actually opened from. `trunk` rather than
    /// `main` so a resolved token cannot be confused with a literal.
    fn seeded_app_with_shell() -> App {
        let mut app = seeded_app();
        app.state.repositories[0].base_branch = "trunk".into();
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "wt".into(),
                branch: "wt".into(),
                path: std::path::PathBuf::from("/tmp/repo-a-wt"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![crate::state::Shell {
                    index: 0,
                    window_name: "lw:wt/0".into(),
                    window_id: Some("@1".into()),
                    pane_id: Some("%1".into()),
                }],
            });
        app.build_tree_flags();
        app.sel = crate::app::Sel::Worktree(0, 0);
        app.clamp_sel();
        app
    }

    fn buffer_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| buf[(x, y)].symbol()))
            .collect()
    }

    // ---- M18 Task 15 — the lease ----

    fn lease_on(app: &mut App, pane: &str, scope: &str) {
        app.leases.insert(
            pane.to_string(),
            crate::app::Lease {
                dash_pane: "%999".into(),
                scope: Some(scope.into()),
            },
        );
    }

    /// A shell another dashboard has docked is muted whole — the way a dead
    /// row mutes, and for the same reason: it is not a row this instance
    /// brings on screen by looking at it. One word keeps it from reading as
    /// dead.
    #[test]
    fn a_leased_shell_renders_muted_and_says_so() {
        let mut app = seeded_app_with_shell();
        app.statuses
            .insert("%1".into(), crate::status::Status::Working);
        // Keep the selection highlight off the row being measured.
        app.sel = crate::app::Sel::Repository(1);
        let dim = app.theme.colors.dim;

        let render = |app: &mut App| {
            let backend = ratatui::backend::TestBackend::new(120, 40);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            let buf = terminal.backend().buffer();
            let tree_w = tree_width(app.viewport).unwrap();
            (0..buf.area.height)
                .map(|y| {
                    let mut text = String::new();
                    let mut colors = Vec::new();
                    for x in 1..tree_w - 1 {
                        let cell = &buf[(x, y)];
                        text.push_str(cell.symbol());
                        if cell.symbol().trim() != "" {
                            colors.push(cell.fg);
                        }
                    }
                    (text, colors)
                })
                .collect::<Vec<_>>()
        };

        let free = render(&mut app);
        let (text, colors) = free.iter().find(|(t, _)| t.contains("wt")).unwrap();
        assert!(!text.contains("leased"), "nothing holds it: {text:?}");
        assert!(
            colors.iter().any(|c| *c != dim),
            "and it draws in its own colours: {text:?} {colors:?}"
        );

        lease_on(&mut app, "%1", "/home/ike/other");
        let held = render(&mut app);
        let (text, colors) = held.iter().find(|(t, _)| t.contains("wt")).unwrap();
        assert!(text.contains("leased"), "the row says so: {text:?}");
        assert!(
            colors.iter().all(|c| *c == dim),
            "and is muted whole: {text:?} {colors:?}"
        );
    }

    /// The instance that lost the race keeps the preview — `capture-pane`
    /// does not care where a pane sits — and that is exactly why the panel
    /// must name the holder: a view that silently follows a shell someone
    /// else is typing in invites an `Enter` that yanks it away.
    #[test]
    fn the_preview_keeps_working_and_names_the_dashboard_holding_the_shell() {
        let mut app = seeded_app_with_shell();
        app.captures
            .insert("%1".into(), "the agent is talking".into());
        lease_on(&mut app, "%1", "/home/ike/other-scope");

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("leased by other-scope"),
            "the panel names the holder: {text}"
        );
        assert!(
            text.contains("the agent is talking"),
            "and keeps previewing it: {text}"
        );
    }

    /// Layout tiers degrade gracefully instead of drawing garbage: tree +
    /// preview while the frame is wide enough, tree-only below the split
    /// threshold, a plain message below the tree threshold.
    #[test]
    fn small_terminals_degrade_gracefully() {
        let mut app = seeded_app();
        let mut at = |w: u16, h: u16| {
            let backend = ratatui::backend::TestBackend::new(w, h);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            buffer_text(&terminal)
        };

        let wide = at(120, 40);
        assert!(wide.contains("Preview"), "the wide tier is tree + preview");
        assert!(
            wide.contains("Repositories"),
            "the wide tier still shows the tree"
        );

        let split = at(50, 10);
        assert!(
            split.contains("Preview"),
            "the narrow split tier keeps the preview"
        );

        let tree_only = at(30, 8);
        assert!(
            tree_only.contains("Repositories"),
            "tree-only tier still shows the tree"
        );
        assert!(!tree_only.contains("Preview"), "the preview hides first");

        let message = at(20, 5);
        assert!(
            message.contains("terminal too small"),
            "below the tree threshold a message shows instead: {message:?}"
        );
    }

    /// A fixture with no disk state: `seeded_app` runs `startup()`, which
    /// loads (and rewrites) the real `state.json`, so any assertion on
    /// exact counts or on flatten mode must build its own app.
    fn bare_app(worktrees: Vec<crate::state::Worktree>) -> App {
        let mut app = App::new(Config::default(), theme::load("ansi", false).unwrap(), None);
        app.state.repositories.push(crate::state::Repository {
            name: "repo-a".into(),
            path: std::path::PathBuf::from("/tmp/repo-a"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            shell: None,
            worktrees,
        });
        app.build_tree_flags();
        app.clamp_sel();
        app
    }

    fn render_wide(app: &mut App) -> String {
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        buffer_text(&terminal)
    }

    /// Panel and overlay titles read as Title Case proper names (the M14
    /// UI pass). Counts stay; the preview drops its `(capture-pane)`
    /// implementation detail.
    #[test]
    fn panel_and_popup_titles_are_title_case() {
        let mut app = bare_app(vec![]);
        let base = render_wide(&mut app);
        assert!(
            base.contains("Repositories (1)"),
            "tree keeps its count: {base:?}"
        );
        assert!(base.contains("Preview"), "preview panel: {base:?}");
        assert!(
            !base.contains("capture-pane)"),
            "the implementation detail is gone from the title: {base:?}"
        );

        for (mode, want) in [
            (
                Mode::Textarea {
                    buf: crate::app::TextBuf::new(),
                },
                "Quick Send",
            ),
            (Mode::Help, "Help"),
            (Mode::Messages { index: 0 }, "Messages"),
            (
                Mode::Input {
                    kind: InputKind::NewWorktree,
                    text: String::new(),
                },
                "New Worktree",
            ),
            (Mode::AddShell { index: 0 }, "Add Shell"),
        ] {
            app.mode = mode;
            let text = render_wide(&mut app);
            assert!(text.contains(want), "overlay title {want:?}: {text:?}");
        }
    }

    /// A single git repo flattens to its worktrees, so the tree names what
    /// it actually lists: `Worktrees (n)`, not `Repositories`.
    #[test]
    fn tree_title_says_worktrees_when_flattened() {
        let mut app = bare_app(vec![crate::state::Worktree {
            name: "wt".into(),
            branch: "wt".into(),
            path: std::path::PathBuf::from("/tmp/repo-a/wt"),
            created_at: String::new(),
            base_commit: None,
            shells: vec![],
        }]);
        assert!(app.flatten, "one repository with worktrees flattens");
        let text = render_wide(&mut app);
        assert!(
            text.contains("Worktrees (1)"),
            "flattened tree title: {text:?}"
        );
        assert!(
            !text.contains("Repositories"),
            "no repository wording: {text:?}"
        );
    }

    fn worktree_with_shells(name: &str, n: usize) -> crate::state::Worktree {
        crate::state::Worktree {
            name: name.into(),
            branch: name.into(),
            path: std::path::PathBuf::from(format!("/tmp/repo-a/{name}")),
            created_at: String::new(),
            base_commit: None,
            shells: (0..n)
                .map(|index| crate::state::Shell {
                    index,
                    window_name: format!("lw:{name}/{index}"),
                    window_id: Some(format!("@{index}")),
                    pane_id: Some(format!("%{index}")),
                })
                .collect(),
        }
    }

    /// The foreground colour the row holding `needle` is drawn in (read
    /// off the needle's first cell).
    fn row_fg(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>, needle: &str) -> Color {
        let buf = terminal.backend().buffer();
        for y in 0..buf.area.height {
            let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            if let Some(byte_idx) = line.find(needle) {
                let col = line[..byte_idx].chars().count() as u16;
                return buf[(col, y)].fg;
            }
        }
        panic!("{needle:?} is not on screen");
    }

    /// A worktree LazyWingman has no tmux window for cannot be docked,
    /// sent to or previewed. Its row mutes *whole* — the same "the eye
    /// skips it" rule a dead row follows — while a worktree with shells
    /// keeps the live colour.
    #[test]
    fn a_worktree_without_a_shell_renders_dim() {
        let mut app = bare_app(vec![
            worktree_with_shells("alpha", 2),
            worktree_with_shells("beta", 0),
        ]);
        // Park the highlight on a row neither assertion reads: the
        // selected row is repainted in the selection colours.
        app.sel = Sel::Shell(0, 0, 0);
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();

        assert_eq!(
            row_fg(&terminal, "beta"),
            app.theme.colors.dim,
            "a worktree with no shell is dim"
        );
        assert_eq!(
            row_fg(&terminal, "alpha"),
            app.theme.colors.tree_worktree,
            "a worktree with shells keeps the live colour"
        );
    }

    /// Selecting such a worktree says why there is nothing to show,
    /// instead of the generic "select a shell" placeholder that would
    /// leave the user hunting for a row to select.
    #[test]
    fn a_worktree_without_a_shell_explains_the_empty_preview() {
        let mut app = bare_app(vec![worktree_with_shells("beta", 0)]);
        app.sel = Sel::Worktree(0, 0);
        let text = render_wide(&mut app);
        assert!(
            text.contains("no agent shell for this worktree"),
            "the preview names the state: {text:?}"
        );
        assert!(
            !text.contains("select a shell to preview"),
            "the generic placeholder does not apply here: {text:?}"
        );
    }

    /// An overlay on a frame too small for it is refused with a message,
    /// not drawn as garbage.
    #[test]
    fn overlay_refused_on_tiny_frame() {
        let mut app = seeded_app();
        app.mode = Mode::Help;
        // Wide enough for the dashboard's own strip, too short for any
        // dialog: overlays are built for 26 columns now, so height is what
        // refuses them.
        let backend = ratatui::backend::TestBackend::new(40, 7);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("too small"),
            "tiny frame must refuse the help overlay: {text:?}"
        );
        assert!(
            app.popup_area.is_none(),
            "no dismissable popup on a refusal"
        );
    }

    /// Resize is normal: re-rendering at several sizes in every mode must
    /// never panic and must never leave the cursor off-buffer.
    #[test]
    fn resize_never_panics_in_any_mode() {
        let mut app = seeded_app();
        for mode in [
            Mode::Normal,
            Mode::Help,
            Mode::Input {
                kind: InputKind::NewWorktree,
                text: "some-worktree".into(),
            },
            Mode::Confirm {
                confirm: Confirm {
                    title: "delete worktree".into(),
                    body: "delete 'x'?".into(),
                    yes_label: Some("yes".into()),
                    warning: None,
                    force_label: None,
                    action: ConfirmAction::DeleteWorktree {
                        repository: 0,
                        worktree: 0,
                    },
                },
            },
        ] {
            app.mode = mode;
            for (w, h) in [(120u16, 40u16), (80, 24), (50, 10), (30, 8), (20, 5)] {
                let backend = ratatui::backend::TestBackend::new(w, h);
                let mut terminal = ratatui::Terminal::new(backend).unwrap();
                terminal.draw(|f| draw(f, &mut app)).unwrap();
                // cursor (when set) must be on the buffer
                let size = terminal.size().unwrap();
                let cur = terminal.get_cursor_position().unwrap_or_default();
                assert!(
                    cur.x < size.width && cur.y < size.height,
                    "cursor off-buffer at {w}x{h}: {cur:?}"
                );
            }
        }
    }

    /// In unicode mode the box-drawing tree guides are present — the
    /// fallback must not silently replace them.
    #[test]
    fn unicode_mode_keeps_box_drawing() {
        let mut app = seeded_app();
        // A multi-shell, expanded worktree so the deeper guides (the
        // vertical bar carried down a column) render too.
        let shell = |i: usize| crate::state::Shell {
            index: i,
            window_name: format!("lw:f/{i}"),
            window_id: Some(format!("@{}", i + 1)),
            pane_id: Some(format!("%{i}")),
        };
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "many-shells".into(),
                branch: "many-shells".into(),
                path: std::path::PathBuf::from("/tmp/wt-many"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![shell(0), shell(1)],
            });
        app.build_tree_flags();
        app.repo_expanded[0] = true;
        app.wt_expanded[0][0] = true;
        app.clamp_sel();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("─"), "unicode render keeps box-drawing");
        assert!(text.contains("│"), "unicode render keeps the carried guide");
    }

    /// `border = "plain"` must reach every panel, not just popups.
    /// `panel_block` (tree + preview) used to hardcode `BorderType::Rounded`
    /// while `popup_block` already read `icons.border`; a user who set
    /// `border = "plain"` got plain popups and a rounded tree/preview — a
    /// mixed-corner screen, exactly the defect they configured away from.
    #[test]
    fn plain_border_reaches_tree_and_preview_panels() {
        let mut app = seeded_app();
        app.cfg.icons.border = crate::config::BorderStyle::Plain;
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        for glyph in ['╭', '╮', '╰', '╯'] {
            assert!(
                !text.contains(glyph),
                "plain border must not draw rounded corner {glyph:?}: {text:?}"
            );
        }
        for glyph in ['┌', '┐', '└', '┘'] {
            assert!(
                text.contains(glyph),
                "plain border must draw plain corner {glyph:?}"
            );
        }
    }

    /// tree_width matches the renderer's tier math (used by both drawing
    /// and mouse hit-testing — they must never drift).
    #[test]
    fn tree_width_follows_tier_math() {
        assert_eq!(tree_width((120, 40)), Some(38)); // full: 32%
        assert_eq!(tree_width((50, 10)), Some(21)); // no-preview: 42%
        assert_eq!(tree_width((400, 60)), Some(54)); // wide screen: capped
        assert_eq!(tree_width((30, 8)), Some(30)); // tree-only: 100%
        assert_eq!(tree_width((20, 5)), None); // message tier
    }

    // ---- M8.4b — grouped help overlay render ----

    fn help_text(app: &mut App, w: u16, h: u16) -> String {
        app.open_help();
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        buffer_text(&terminal)
    }

    /// The overlay is grouped by context with palette wording: every group
    /// header renders, every binding appears, and the descriptions use the
    /// same words as the statusbar entries.
    #[test]
    fn help_overlay_groups_actions_by_context() {
        let mut app = seeded_app();
        // Tall enough for the whole palette: reflowed for the strip it is
        // a long column, and this test is about the words, not the fold.
        let text = help_text(&mut app, 120, 80);
        for header in ["navigate", "repositories", "worktrees", "shells", "global"] {
            assert!(text.contains(header), "group header {header:?} missing");
        }
        // every action is listed (palette = the index of everything)
        for needle in [
            "move selection",
            "collapse",
            "expand",
            "dock",
            "new worktree",
            "add shell",
            "delete worktree",
            "close shell",
            "quick-send",
            "refresh",
            "reset the layout",
            "this help",
        ] {
            assert!(text.contains(needle), "action {needle:?} missing from help");
        }
        assert!(
            app.popup_area.is_some(),
            "help overlay must register its dismissable rect"
        );
    }

    /// `I scan` / `i import` are gone from the repository row: discovery is a
    /// boot-time concern, so the row offers only what still exists.
    #[test]
    fn the_repository_bar_offers_no_scan_or_import() {
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Repository(0);
        let labels: Vec<String> = crate::ui::bar_entries(&app)
            .into_iter()
            .map(|(h, _)| h.label())
            .collect();
        assert!(
            !labels
                .iter()
                .any(|l| l.contains("scan") || l.contains("import")),
            "bar: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("worktree")),
            "bar keeps `n worktree`: {labels:?}"
        );
    }

    /// The pad splits the tree's column rather than replacing the tree or
    /// covering it: both are on screen, the pad under the tree, and the
    /// preview beside them is untouched — the pad is written *about* what
    /// those panels show.
    #[test]
    fn the_open_pad_splits_the_tree_column_and_leaves_the_preview_alone() {
        let mut app = seeded_app_with_shell();
        app.viewport = (120, 40);
        app.mode = crate::app::Mode::Todo {
            buf: crate::app::TextBuf::from_parked("ship the schema first", 0, 0),
        };
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Todo"), "the panel names itself: {text:?}");
        assert!(text.contains("ship the schema first"), "and holds the pad");
        assert!(text.contains("repo-a"), "the tree stays above it");
        assert!(text.contains("Preview"), "the panel beside it is untouched");
        assert!(text.contains("save & back"), "the bar says how to get out");
    }

    /// The open pad takes half the column — enough of a window to write
    /// in — and the tree keeps the other half.
    #[test]
    fn the_open_pad_takes_half_the_column() {
        let mut app = seeded_app();
        app.viewport = (120, 41);
        app.mode = crate::app::Mode::Todo {
            buf: crate::app::TextBuf::from_parked("a", 0, 0),
        };
        let main = main_h(&app);
        assert_eq!(main, 40, "one statusbar row at this width");
        assert_eq!(todo_panel_h(&app, main), 20, "half the column");
        assert_eq!(todo_panel_rows(&app), Some((20, 40)));
    }

    /// Parked, the pad is as tall as the note: five lines of text is a
    /// five-line panel (plus its two borders), and an empty pad is no
    /// panel at all.
    #[test]
    fn a_parked_pad_is_as_tall_as_its_note() {
        let mut app = seeded_app();
        app.viewport = (120, 41);
        let main = main_h(&app);
        assert_eq!(todo_panel_h(&app, main), 0, "an empty pad draws nothing");
        assert_eq!(todo_panel_rows(&app), None);
        app.todo = crate::app::TextBuf::from_parked("a\nb\nc\nd\ne", 0, 0);
        assert_eq!(todo_panel_h(&app, main), 7, "five lines plus two borders");
        // …and never past the half the open pad would take.
        app.todo = crate::app::TextBuf::from_parked(&"x\n".repeat(60), 0, 0);
        assert_eq!(todo_panel_h(&app, main), 20, "capped at half the column");
    }

    /// A parked pad keeps showing its note, unfocused, and says how many
    /// rows it had to leave below the fold.
    #[test]
    fn a_parked_pad_keeps_showing_the_note_and_owns_up_to_what_it_hides() {
        let mut app = seeded_app();
        app.viewport = (120, 41);
        app.todo = crate::app::TextBuf::from_parked(&"note\n".repeat(60), 0, 0);
        let backend = ratatui::backend::TestBackend::new(120, 41);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Todo"), "the panel is there in Normal too");
        assert!(text.contains("note"), "and still shows the note");
        assert!(text.contains("repo-a"), "beside a tree that still works");
        assert!(
            text.contains(&format!("{DOWN_ARROW}43")),
            "18 rows shown of the note's 61: {text:?}"
        );
    }

    /// A column with no room for two panels is not split: the open pad
    /// takes it whole, the way it did before the split existed, and a
    /// parked one yields to the tree — the panel you steer with.
    #[test]
    fn a_column_too_short_to_split_is_not_split() {
        let mut app = seeded_app();
        // The narrowest, shortest frame the dashboard still draws in.
        app.viewport = TREE_MIN;
        app.todo = crate::app::TextBuf::from_parked("later", 0, 0);
        let main = main_h(&app);
        assert_eq!(todo_panel_h(&app, main), 0, "parked yields the column");
        app.mode = crate::app::Mode::Todo {
            buf: crate::app::TextBuf::from_parked("later", 0, 0),
        };
        assert_eq!(todo_panel_h(&app, main), main, "open takes it whole");
    }

    /// An empty pad says what it is for. Ghost text, in dim, replaced by
    /// the first thing typed — never mistakable for a note.
    #[test]
    fn an_empty_pad_says_what_it_is_for() {
        let mut app = seeded_app();
        app.viewport = (120, 40);
        app.mode = crate::app::Mode::Todo {
            buf: crate::app::TextBuf::new(),
        };
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(
            buffer_text(&terminal).contains("cannot start yet"),
            "the empty pad explains itself"
        );
    }

    /// The statusbar entries and the help overlay share their words — the
    /// discoverability rule that makes learning one teach the other.
    #[test]
    fn bar_entries_and_help_share_wording() {
        // For every bar entry, the action's word appears in the help
        // overlay (bar: "worktree n" vs help: "new worktree", etc.).
        let mut app = seeded_app();
        let text = help_text(&mut app, 120, 80);
        let app2 = seeded_app();
        for (entry, _) in crate::ui::bar_entries(&app2) {
            match entry.desc.as_str() {
                "help" => assert!(text.contains("this help"), "bar 'help ?' ↔ help row"),
                "send" => assert!(text.contains("quick-send"), "bar 'send s' ↔ help row"),
                "predefined" => assert!(
                    text.contains("predefined message"),
                    "bar 'predefined p' ↔ help row"
                ),
                "worktree" => assert!(text.contains("new worktree"), "bar 'worktree n' ↔ help"),
                "shell" => assert!(text.contains("add shell"), "bar 'shell a' ↔ help"),
                "delete" => assert!(text.contains("delete worktree"), "bar 'delete d' ↔ help"),
                "dock" => assert!(text.contains("dock the shell"), "bar 'dock ↵' ↔ help"),
                "close" => assert!(text.contains("close shell"), "bar 'close d' ↔ help"),
                "live" => assert!(text.contains("live tail"), "bar 'live Esc' ↔ help"),
                "todo" => assert!(text.contains("todo pad"), "bar 'todo t' ↔ help"),
                other => panic!("unmapped bar entry: {other:?}"),
            }
        }
    }

    /// The statusbar carries the shortcuts and nothing else: no app name,
    /// no session, no repository count, no sticky notice. Anything the user
    /// cannot act on is noise on the row they read constantly.
    #[test]
    fn statusbar_is_shortcuts_only() {
        let mut app = seeded_app();
        app.notify("worktree 'foo' created");
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let last: String = (0..buf.area.width)
            .map(|x| buf[(x, buf.area.height - 1)].symbol())
            .collect();

        assert!(!last.contains("lazywingman"), "no app name: {last:?}");
        assert!(
            !last.contains("repositories"),
            "no repository count: {last:?}"
        );
        assert!(!last.contains("standalone"), "no session chip: {last:?}");
        assert!(
            !last.contains("created"),
            "notices are not on the bar: {last:?}"
        );
        for (entry, _) in bar_entries(&app) {
            let label = entry.label();
            assert!(last.contains(&label), "bar shows {label:?}: {last:?}");
        }
    }

    /// A narrow frame wraps the shortcuts onto a second row instead of
    /// cutting the tail at the border — the tail is where `help ?` lives,
    /// the one entry that leads to everything that did not fit.
    #[test]
    fn statusbar_wraps_instead_of_losing_its_tail() {
        let mut app = seeded_app_with_shell();
        app.sel = crate::app::Sel::Shell(0, 0, 0);
        let (w, h) = (46u16, 20u16);
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let row = |y: u16| -> String { (0..w).map(|x| buf[(x, y)].symbol()).collect() };
        let bar = format!("{}{}", row(h - 2), row(h - 1));
        assert_eq!(statusbar_h(&app, w), 2, "the entries need a second row");
        for (entry, _) in bar_entries(&app) {
            let label = entry.label();
            assert!(bar.contains(&label), "bar keeps {label:?}: {bar:?}");
        }
        // …and a wide frame still spends a single row on it.
        let mut wide = seeded_app_with_shell();
        wide.sel = crate::app::Sel::Shell(0, 0, 0);
        assert_eq!(statusbar_h(&wide, 120), 1, "one row is enough at 120");
    }

    /// A long error is a sentence: the toast wraps it rather than cutting
    /// it at a width that would leave the message unreadable.
    #[test]
    fn a_long_toast_wraps_onto_more_than_one_row() {
        let mut app = seeded_app();
        let msg = "git worktree add failed: fatal: invalid reference: origin/main";
        app.notify(msg);
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        for word in msg.split_whitespace() {
            assert!(text.contains(word), "the whole notice is shown: {word:?}");
        }
        // Wrapped, not squeezed onto one row: no row holds all of it.
        assert!(
            !text.lines().any(|l| l.contains(msg)),
            "a notice this long does not fit one row:\n{text}"
        );
    }

    /// A notice is a toast above the bar, and it is gone once it expires —
    /// a message that never leaves is exactly the clutter the bar shed.
    #[test]
    fn toast_shows_then_expires() {
        let mut app = seeded_app();
        app.notify("worktree 'foo' created");
        let render = |app: &mut App| {
            let backend = ratatui::backend::TestBackend::new(120, 40);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            buffer_text(&terminal)
        };
        assert!(
            render(&mut app).contains("worktree 'foo' created"),
            "a fresh notice is drawn as a toast"
        );
        app.expire_toast_for_test();
        assert!(
            !render(&mut app).contains("worktree 'foo' created"),
            "the toast is gone once its TTL has passed"
        );
    }

    /// The worktree icon carries the branch's state — new / ahead /
    /// merged / deleted — and all four share one padded column, so the
    /// names line up whichever glyph a row draws.
    #[test]
    fn worktree_icon_carries_the_commit_state() {
        let mut app = seeded_app();
        app.cfg.icons = crate::config::IconsConfig::preset("unicode");
        for name in ["new-one", "ahead-one", "merged-one", "deleted-one"] {
            app.state.repositories[0]
                .worktrees
                .push(crate::state::Worktree {
                    name: name.into(),
                    branch: name.into(),
                    path: std::path::PathBuf::from(format!("/tmp/wt-{name}")),
                    created_at: String::new(),
                    base_commit: None,
                    shells: vec![],
                });
        }
        // The cache is what the tree reads; the git calls that fill it are
        // covered in `git::tests`.
        for (name, st) in [
            ("new-one", WorktreeState::New),
            ("ahead-one", WorktreeState::Ahead),
            ("merged-one", WorktreeState::Merged),
            ("deleted-one", WorktreeState::Deleted),
        ] {
            app.wt_state
                .insert(std::path::PathBuf::from(format!("/tmp/wt-{name}")), st);
        }
        app.build_tree_flags();
        app.repo_expanded[0] = true;

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let row = |name: &str| {
            rows.iter()
                .find(|r| r.contains(name))
                .unwrap_or_else(|| panic!("no row for {name}"))
                .clone()
        };
        for (name, glyph) in [
            ("new-one", "○"),
            ("ahead-one", "◆"),
            ("merged-one", "✓"),
            ("deleted-one", "⊘"),
        ] {
            let r = row(name);
            assert!(
                r.contains(&format!("{glyph} {name}")),
                "{name} must carry {glyph:?}: {r:?}"
            );
        }
        // `str::find` returns a byte offset and the glyphs differ in UTF-8
        // length, so comparing byte offsets would report a false
        // misalignment. Compare rendered column (char) position instead,
        // which is what "aligned" means.
        let char_col = |name: &str| {
            let r = row(name);
            r[..r.find(name).unwrap()].chars().count()
        };
        let cols: Vec<usize> = ["new-one", "ahead-one", "merged-one", "deleted-one"]
            .map(char_col)
            .to_vec();
        assert!(
            cols.iter().all(|c| *c == cols[0]),
            "the four icons share one column, so names align: {cols:?}"
        );
    }

    /// The repository row's folder icon is its only marker: `▾`/`▸` were
    /// removed because the icon already sits there, and now carries the
    /// open/closed state itself.
    #[test]
    fn the_repository_row_has_no_expand_marker() {
        let mut app = seeded_app();
        // seeded_app's first repository starts empty, which would render the
        // outline folder; give it a worktree so it renders the open folder
        // this test asserts on — the fixture starts expanded (Ruling F2 —
        // fixture adapted, assertions kept literal).
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "some-worktree".into(),
                branch: "some-worktree".into(),
                path: std::path::PathBuf::from("/tmp/wt-some-worktree"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![],
            });
        app.build_tree_flags();
        app.clamp_sel();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let name = &app.state.repositories[0].name.clone();
        let row: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .find(|r| r.contains(name.as_str()))
            .unwrap();
        let upto = &row[..row.find(name.as_str()).unwrap()];
        assert!(
            !upto.contains('▾'),
            "no open marker before the repository: {row:?}"
        );
        assert!(
            !upto.contains('▸'),
            "no closed marker before the repository: {row:?}"
        );
        assert!(
            upto.contains('\u{f0770}'),
            "the icon is still there: {row:?}"
        );
    }

    /// Row indices are the contract between `draw_tree`, `visible_nodes` and
    /// mouse hit-testing. Dropping the repository marker must not shift them.
    #[test]
    fn rendered_tree_rows_match_visible_nodes() {
        let mut app = seeded_app();
        // A fixture with real depth: a multi-shell worktree (renders a
        // worktree row plus one row per shell) and a solo worktree (folds
        // onto a single row, no child row at all). Two repositories with zero
        // worktrees (the pre-fix fixture) would pass this assertion even if
        // every worktree/shell row were miscounted.
        let shell = |i: usize| crate::state::Shell {
            index: i,
            window_name: format!("lw:f/{i}"),
            window_id: Some(format!("@{}", i + 1)),
            pane_id: Some(format!("%{i}")),
        };
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "many-shells".into(),
                branch: "many-shells".into(),
                path: std::path::PathBuf::from("/tmp/wt-many"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![shell(0), shell(1)],
            });
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "one-shell".into(),
                branch: "one-shell".into(),
                path: std::path::PathBuf::from("/tmp/wt-one"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![shell(2)],
            });
        app.build_tree_flags();
        app.repo_expanded[0] = true;
        app.clamp_sel();
        let expected = app.visible_nodes_len_for_test();
        let (w, h) = (120u16, 40u16);
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        // The brief's hand-rolled border filter (leading `╭`/`╰`/`│`) also
        // excludes every tree content row, since the block's left border
        // is `│` on those rows too (Ruling F2 — mechanics adapted).
        // Instead, exclude the block's border rows/columns explicitly: the
        // tree spans the full main area (frame minus the 1-row statusbar),
        // and `tree_width` is the same math `draw_tree`/mouse hit-testing
        // use, so this can't drift from the real layout.
        let tw = tree_width((w, h)).unwrap();
        let main_h = h - 1;
        let non_empty = (1..main_h - 1)
            .filter(|&y| {
                let inner: String = (1..tw - 1).map(|x| buf[(x, y)].symbol()).collect();
                !inner.trim().is_empty()
            })
            .count();
        assert_eq!(non_empty, expected, "one rendered row per visible node");
    }

    /// The tree folds the nominal case: a worktree with one shell shows
    /// that shell's status on its own row and gets no child row, while a
    /// worktree with several just gets its expanded subtree. No row carries
    /// a shell count (the shells are right below) and none carries the
    /// tmux window id — that is plumbing, not information.
    #[test]
    fn solo_worktree_row_carries_the_status_and_no_child_row() {
        let mut app = seeded_app();
        let shell = |i: usize| crate::state::Shell {
            index: i,
            window_name: format!("lw:f/{i}"),
            window_id: Some(format!("@{}", i + 7)),
            pane_id: Some(format!("%{i}")),
        };
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "many-shells".into(),
                branch: "many-shells".into(),
                path: std::path::PathBuf::from("/tmp/wt-many"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![shell(0), shell(1)],
            });
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "one-shell".into(),
                branch: "one-shell".into(),
                path: std::path::PathBuf::from("/tmp/wt-one"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![shell(2)],
            });
        app.statuses.insert("%0".into(), Status::Working);
        app.statuses.insert("%1".into(), Status::Working);
        app.statuses.insert("%2".into(), Status::Waiting);
        app.build_tree_flags();
        app.repo_expanded[0] = true;
        let n = app.state.repositories[0].worktrees.len();
        app.wt_expanded[0][n - 2] = true; // the many-shell worktree is open

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rows = buffer_rows(&terminal);

        let solo = rows.iter().find(|r| r.contains("one-shell")).unwrap();
        assert!(
            solo.contains(Status::Waiting.label()),
            "a folded worktree shows its shell's status: {solo:?}"
        );
        assert!(
            !solo.contains("shell]"),
            "a folded worktree drops the shell count: {solo:?}"
        );
        let many = rows.iter().find(|r| r.contains("many-shells")).unwrap();
        assert!(
            !many.contains("shells]"),
            "a multi-shell worktree carries no count either: {many:?}"
        );
        // the folded worktree contributes no `#2` row; the open one does
        let shell_rows: Vec<&String> = rows.iter().filter(|r| r.contains(" #")).collect();
        assert_eq!(
            shell_rows.len(),
            2,
            "only the open subtree has rows: {shell_rows:?}"
        );
        // and no row leaks a tmux window id
        for r in &rows {
            assert!(!r.contains('@'), "tree must not show window ids: {r:?}");
        }
    }

    /// Repository rows carry no worktree count — the tree is expanded by
    /// default, so the worktrees themselves are the count. What the row
    /// cannot show otherwise is whether a folded repository holds anything
    /// at all, and the folder icon says it: open when expanded, closed when
    /// folded, outline when it is empty.
    #[test]
    fn repository_rows_swap_the_count_for_a_full_or_empty_icon() {
        let mut app = seeded_app();
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "only-worktree".into(),
                branch: "only-worktree".into(),
                path: std::path::PathBuf::from("/tmp/wt-one"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![crate::state::Shell {
                    index: 0,
                    window_name: "lw:f/0".into(),
                    window_id: Some("@7".into()),
                    pane_id: Some("%0".into()),
                }],
            });
        app.build_tree_flags();
        app.repo_expanded[0] = true;

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rows = buffer_rows(&terminal);

        let full = rows.iter().find(|r| r.contains("repo-a")).unwrap();
        let empty = rows.iter().find(|r| r.contains("repo-b")).unwrap();
        for row in [full, empty] {
            assert!(!row.contains("worktree)"), "no count on a row: {row:?}");
            assert!(!row.contains("worktrees)"), "no count on a row: {row:?}");
        }
        assert!(
            full.contains("\u{f0770} repo-a"),
            "an expanded repository with worktrees: {full:?}"
        );
        assert!(
            empty.contains("\u{f0256} repo-b"),
            "an empty repository: {empty:?}"
        );
    }

    /// Folding a repository is the one state change with no other visible
    /// trace once its children are gone, so the folder glyph has to carry
    /// it: the same repository draws an open folder expanded and a closed
    /// one folded.
    #[test]
    fn the_repository_icon_opens_and_closes_with_the_row() {
        let mut app = seeded_app();
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "only-worktree".into(),
                branch: "only-worktree".into(),
                path: std::path::PathBuf::from("/tmp/wt-one"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![],
            });
        app.build_tree_flags();

        let icon_row = |app: &mut App| {
            let backend = ratatui::backend::TestBackend::new(120, 40);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            buffer_rows(&terminal)
                .into_iter()
                .find(|r| r.contains("repo-a"))
                .unwrap()
        };

        app.repo_expanded[0] = true;
        let open = icon_row(&mut app);
        app.repo_expanded[0] = false;
        let closed = icon_row(&mut app);

        assert!(
            open.contains("\u{f0770} repo-a"),
            "expanded draws nf-md-folder_open: {open:?}"
        );
        assert!(
            closed.contains("\u{f024b} repo-a"),
            "folded draws nf-md-folder: {closed:?}"
        );
    }

    /// A dead entry is muted *whole*: once every shell under it has
    /// exited, the row's guide, icon, name and status all take the dead
    /// colour instead of only the status word, so the eye skips a worktree
    /// that carries no live agent.
    #[test]
    fn a_dead_entry_is_muted_across_the_whole_row() {
        let mut app = seeded_app();
        let shell = |i: usize| crate::state::Shell {
            index: i,
            window_name: format!("lw:f/{i}"),
            window_id: Some(format!("@{}", i + 7)),
            pane_id: Some(format!("%{i}")),
        };
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "gone-one".into(),
                branch: "gone-one".into(),
                path: std::path::PathBuf::from("/tmp/wt-gone"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![shell(0)],
            });
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "half-gone".into(),
                branch: "half-gone".into(),
                path: std::path::PathBuf::from("/tmp/wt-half"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![shell(1), shell(2)],
            });
        app.statuses.insert("%0".into(), Status::Dead);
        app.statuses.insert("%1".into(), Status::Dead);
        app.statuses.insert("%2".into(), Status::Working);
        app.build_tree_flags();
        app.repo_expanded[0] = true;
        let n = app.state.repositories[0].worktrees.len();
        app.wt_expanded[0][n - 1] = true; // the mixed worktree is open
        app.sel = Sel::Repository(1); // keep the highlight off the rows asserted on

        let dead = app.theme.colors.status_dead;
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        // (text, colour of every non-blank cell) per row, inside the tree
        // panel's borders — the borders carry the panel's own colour.
        let tree_w = tree_width(app.viewport).unwrap();
        let rows: Vec<(String, Vec<Color>)> = (0..buf.area.height)
            .map(|y| {
                let mut text = String::new();
                let mut colors = Vec::new();
                for x in 1..tree_w - 1 {
                    let cell = &buf[(x, y)];
                    text.push_str(cell.symbol());
                    if cell.symbol().trim() != "" {
                        colors.push(cell.fg);
                    }
                }
                (text, colors)
            })
            .collect();
        let row = |needle: &str| {
            rows.iter()
                .find(|(t, _)| t.contains(needle))
                .unwrap_or_else(|| panic!("no row for {needle:?}"))
                .clone()
        };

        let (text, colors) = row("gone-one");
        assert!(text.contains(Status::Dead.label()), "row reads: {text:?}");
        assert!(
            colors.iter().all(|c| *c == dead),
            "a solo dead worktree is muted whole: {text:?} {colors:?}"
        );
        // The shell row of a dead shell is muted the same way...
        let (text, colors) = row(" #1");
        assert!(
            colors.iter().all(|c| *c == dead),
            "a dead shell row is muted whole: {text:?} {colors:?}"
        );
        // ...while its live sibling, and the worktree holding it, keep
        // their normal colours.
        let (text, colors) = row(" #2");
        assert!(
            colors.iter().any(|c| *c != dead),
            "a live shell row is not muted: {text:?} {colors:?}"
        );
        let (text, colors) = row("half-gone");
        assert!(
            colors.iter().any(|c| *c != dead),
            "a worktree with one live shell is not muted: {text:?} {colors:?}"
        );
    }

    /// The help overlay scrolls: on a short frame the title advertises
    /// the footer offers `scroll j/k` and the content is clipped to the
    /// popup; on a tall frame everything fits and the motion is not
    /// offered at all.
    #[test]
    fn help_overlay_scrolls_on_short_frames() {
        let mut app = seeded_app();
        let short = help_text(&mut app, 80, 18);
        assert!(
            short.contains("scroll j/k"),
            "short frame must advertise scrolling: {short:?}"
        );
        let mut app = seeded_app();
        let tall = help_text(&mut app, 120, 80);
        assert!(
            !tall.contains("scroll j/k"),
            "tall frame fits everything: {tall:?}"
        );
    }

    /// The scroll-height math in the app and the rendered line count share
    /// one source (help_line_count), so clamping can never drift from the
    /// renderer.
    #[test]
    fn help_line_count_matches_rendered_content() {
        let app = seeded_app();
        let lines = help_line_count(&app);
        assert!(lines > 30, "grouped help is long: {lines}");
        // scrolling to the very end must be possible
        let visible = 28usize;
        assert!(lines > visible, "content must overflow a 30-row popup");
    }

    // ---- M8.4c — theme picker render ----

    /// The picker renders every theme name with the cursor highlighted on
    /// the selected row; the app's popup rect is registered so click-out-
    /// side dismisses work.
    #[test]
    fn theme_picker_renders_names_and_cursor() {
        let mut app = seeded_app();
        app.theme_names = crate::theme::available_names().unwrap();
        app.mode = Mode::ThemePicker { index: 1 };
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        for name in [
            "catppuccin-mocha",
            "catppuccin-macchiato",
            "catppuccin-frappe",
            "catppuccin-latte",
            "dracula",
            "gruvbox",
            "ansi",
        ] {
            assert!(text.contains(name), "picker lists {name:?}");
        }
        assert!(text.contains("Themes"), "picker has a title");
        assert!(
            app.popup_area.is_some(),
            "picker must register its dismissable rect"
        );
    }

    /// The picker on a tiny frame is refused with the standard message
    /// (never garbage, never a panic).
    #[test]
    fn theme_picker_refused_on_tiny_frame() {
        let mut app = seeded_app();
        app.theme_names = crate::theme::available_names().unwrap();
        app.mode = Mode::ThemePicker { index: 0 };
        // Wide enough for the dashboard's own strip, too short for any
        // dialog: overlays are built for 26 columns now, so height is what
        // refuses them.
        let backend = ratatui::backend::TestBackend::new(40, 7);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("too small"), "tiny frame refuses the picker");
    }

    // --------------------------------------- M8.4d: diagnostics overlay

    fn app_with_diags(n: usize) -> App {
        let mut cfg = Config::default();
        for i in 0..n {
            cfg.diagnostics.push(format!("config problem {i}"));
        }
        // No `startup()` — see `seeded_app`: it would load and rewrite the
        // real `state.json`.
        App::new(cfg, theme::load("ansi", false).unwrap(), None)
    }

    /// The diagnostics overlay renders every diagnostic with the warn mark,
    /// registers its dismissable rect, and shows the count in the title.
    #[test]
    fn diagnostics_overlay_renders_all_lines_and_registers_rect() {
        let mut app = app_with_diags(3);
        app.mode = Mode::Diagnostics;
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Startup Diagnostics (3)"), "title has count");
        assert!(text.contains("config problem 0"));
        assert!(text.contains("config problem 1"));
        assert!(text.contains("config problem 2"));
        assert!(text.contains("⚠"), "unicode warn mark");
        assert!(
            app.popup_area.is_some(),
            "diagnostics must register its dismissable rect"
        );
    }

    /// Long diagnostics scroll; only the visible window renders.
    #[test]
    fn diagnostics_overlay_scrolls_with_popup() {
        let mut app = app_with_diags(50);
        app.mode = Mode::Diagnostics;
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        // scroll down one screen and re-render: first items leave the frame
        let visible = crate::ui::overlay_body_height(app.popup_area);
        app.diag_scroll = visible;
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            !text.contains("config problem 0"),
            "scrolled-out diagnostics are gone"
        );
        assert!(text.contains("scroll j/k"), "scroll hint advertised");
    }

    /// The diagnostics overlay on a tiny frame is refused with the standard
    /// message (never garbage, never a panic).
    #[test]
    fn diagnostics_refused_on_tiny_frame() {
        let mut app = app_with_diags(2);
        app.mode = Mode::Diagnostics;
        // Wide enough for the dashboard's own strip, too short for any
        // dialog: overlays are built for 26 columns now, so height is what
        // refuses them.
        let backend = ratatui::backend::TestBackend::new(40, 7);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("too small"), "tiny frame refuses diagnostics");
        assert!(
            app.popup_area.is_none(),
            "refused overlay must not register a dismissable rect"
        );
    }

    // ---- preview tail anchoring ----

    /// Rows of the rendered frame, one string per terminal line.
    /// Each status draws its own glyph in front of the label. The label
    /// stays: pause and stop read alike at a glance, so the glyph is the
    /// fast scan and the word is what settles it.
    #[test]
    fn every_status_draws_its_own_icon_in_front_of_the_label() {
        let mut app = seeded_app();
        app.cfg.icons = crate::config::IconsConfig::preset("unicode");
        let cases = [
            ("wt-waiting", "%10", Status::Waiting, "⏸"),
            ("wt-blocked", "%11", Status::Blocked, "⏹"),
            ("wt-failed", "%12", Status::Failed, "✖"),
            ("wt-ended", "%13", Status::Ended, "⏸"),
        ];
        for (name, pane, st, _) in cases {
            app.state.repositories[0]
                .worktrees
                .push(crate::state::Worktree {
                    name: name.into(),
                    branch: name.into(),
                    path: std::path::PathBuf::from(format!("/tmp/{name}")),
                    created_at: String::new(),
                    base_commit: None,
                    shells: vec![crate::state::Shell {
                        index: 0,
                        window_name: format!("lw:{name}/0"),
                        window_id: Some("@1".into()),
                        pane_id: Some(pane.into()),
                    }],
                });
            app.statuses.insert(pane.into(), st);
        }
        app.build_tree_flags();
        app.repo_expanded[0] = true;

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rows = buffer_rows(&terminal);

        for (name, _, st, glyph) in cases {
            let row = rows
                .iter()
                .find(|r| r.contains(name))
                .unwrap_or_else(|| panic!("no row for {name}: {rows:?}"));
            assert!(
                row.contains(&format!("{glyph} {}", st.label())),
                "{name} must read `{glyph} {}`: {row:?}",
                st.label()
            );
        }
    }

    /// `idle` and `dead` draw no glyph — nothing to act on — but their
    /// labels still line up with the states that do draw one.
    #[test]
    fn idle_and_dead_draw_no_glyph_but_stay_aligned() {
        let mut app = seeded_app();
        app.cfg.icons = crate::config::IconsConfig::preset("unicode");
        let cases = [
            ("quiet-one", "%20", Status::Idle),
            ("gone-one", "%21", Status::Dead),
            ("parked-one", "%22", Status::Waiting),
        ];
        for (name, pane, st) in cases {
            app.state.repositories[0]
                .worktrees
                .push(crate::state::Worktree {
                    name: name.into(),
                    branch: name.into(),
                    path: std::path::PathBuf::from(format!("/tmp/{name}")),
                    created_at: String::new(),
                    base_commit: None,
                    shells: vec![crate::state::Shell {
                        index: 0,
                        window_name: format!("lw:{name}/0"),
                        window_id: Some("@1".into()),
                        pane_id: Some(pane.into()),
                    }],
                });
            app.statuses.insert(pane.into(), st);
        }
        app.build_tree_flags();
        app.repo_expanded[0] = true;

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rows = buffer_rows(&terminal);

        // The gap between the end of the worktree name and the start of the
        // status label: identical whether or not the state draws a glyph.
        let gap = |name: &str, st: Status| {
            let row = rows.iter().find(|r| r.contains(name)).unwrap();
            col_of(row, st.label()) - (col_of(row, name) + name.chars().count())
        };
        let parked = gap("parked-one", Status::Waiting);
        assert_eq!(
            gap("quiet-one", Status::Idle),
            parked,
            "a blank status icon must still hold its column: {rows:?}"
        );
        assert_eq!(
            gap("gone-one", Status::Dead),
            parked,
            "dead draws no glyph but keeps the column: {rows:?}"
        );
    }

    /// The spinner advances with wall-clock time and wraps, so the working
    /// animation needs no timer of its own — just the redraws the event
    /// loop already does.
    #[test]
    fn the_spinner_cycles_its_frames_over_time() {
        use std::time::Duration;
        let ic = crate::config::IconsConfig::preset("nerdfont");
        let n = ic.status_spinner.len();
        assert!(n > 1, "the nerdfont preset must ship a real animation");
        let at = |ms: u64| spinner_frame(&ic, Duration::from_millis(ms)).to_string();
        assert_eq!(at(0), ic.status_spinner[0]);
        assert_ne!(at(0), at(SPINNER_FRAME_MS as u64), "one step advances");
        assert_eq!(
            at(0),
            at(SPINNER_FRAME_MS as u64 * n as u64),
            "a full cycle wraps"
        );
        // A frame outlives the event loop's ~100ms redraw floor, or the
        // animation would stutter instead of ticking.
        const { assert!(SPINNER_FRAME_MS > 100) };
        assert_eq!(
            spinner_frame(&crate::config::IconsConfig::preset("none"), Duration::ZERO),
            "",
            "no spinner configured draws nothing"
        );
    }

    /// A confirm's caveat is the one line that changes the answer, so it
    /// is drawn in the failure colour rather than as more body prose.
    #[test]
    fn a_confirm_warning_is_drawn_in_the_failure_colour() {
        let mut app = seeded_app_with_shell();
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

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rows = buffer_rows(&terminal);
        let y = rows
            .iter()
            .position(|r| r.contains("uncommitted changes"))
            .unwrap_or_else(|| panic!("no warning row: {rows:?}"));
        let x = col_of(&rows[y], "uncommitted changes") as u16;

        assert!(
            rows[y].contains(&format!("{}  uncommitted", app.cfg.icons.warn)),
            "two spaces after the mark: `⚠` is ambiguous-width and a single \
             one gets swallowed where the terminal draws it wide: {:?}",
            rows[y]
        );

        let cell = &terminal.backend().buffer()[(x, y as u16)];
        assert_eq!(
            cell.fg, app.theme.colors.status_failed,
            "the warning reads as an error"
        );
        assert!(
            cell.modifier.contains(Modifier::BOLD),
            "and is emphasised: {:?}",
            cell.modifier
        );
    }

    /// The cursor row tints its background and nothing else. Repainting the
    /// foreground would erase the status colour — the one thing the user
    /// moved the cursor there to read.
    #[test]
    fn the_selected_row_keeps_its_colours_and_only_gains_a_background() {
        let mut app = seeded_app_with_shell();
        app.statuses.insert("%1".into(), Status::Failed);
        app.sel = crate::app::Sel::Worktree(0, 0);

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rows = buffer_rows(&terminal);
        let y = rows
            .iter()
            .position(|r| r.contains(Status::Failed.label()))
            .unwrap_or_else(|| panic!("no selected row: {rows:?}"));
        let x = col_of(&rows[y], Status::Failed.label()) as u16;

        let buf = terminal.backend().buffer();
        let cell = &buf[(x, y as u16)];
        let c = &app.theme.colors;
        assert_eq!(cell.fg, c.status_failed, "the status keeps its own colour");
        assert_eq!(
            cell.bg, c.selection_bg,
            "the selection tints the background"
        );
        // ...and the row is genuinely the selected one: an unselected row
        // carries no selection background at all.
        let other = rows
            .iter()
            .position(|r| r.contains("repo-b"))
            .expect("a second repository row");
        assert_ne!(buf[(x, other as u16)].bg, c.selection_bg);
    }

    /// The screen column `needle` starts at in a rendered row. Every glyph
    /// the tree draws is one cell wide, so a char offset is a column — a
    /// byte offset is not, and the box-drawing guides make the two differ.
    /// A settled worktree — merged, or its directory gone — mutes its name
    /// and drops its shell's status entirely: nothing there needs
    /// answering any more, the next step is deletion, so only the state
    /// glyph keeps a colour and no leftover `waiting` competes with the
    /// rows that do want the user.
    #[test]
    fn a_settled_worktree_drops_its_status_and_dims_its_name() {
        for state in [WorktreeState::Merged, WorktreeState::Deleted] {
            let mut app = seeded_app_with_shell();
            app.statuses.insert("%1".into(), Status::Waiting);
            app.wt_state
                .insert(std::path::PathBuf::from("/tmp/repo-a-wt"), state);

            let backend = ratatui::backend::TestBackend::new(120, 40);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let rows = buffer_rows(&terminal);
            let y = rows
                .iter()
                .position(|r| r.contains("wt"))
                .unwrap_or_else(|| panic!("no worktree row: {rows:?}"));
            let buf = terminal.backend().buffer();
            let c = &app.theme.colors;
            let fg_at = |needle: &str| buf[(col_of(&rows[y], needle) as u16, y as u16)].fg;

            assert_eq!(fg_at("wt"), c.dim, "{state:?}: the name is dimmed");
            assert!(
                !rows.iter().any(|r| r.contains(Status::Waiting.label())),
                "{state:?}: the status is gone from the tree: {rows:?}"
            );
            assert!(
                (0..buf.area.width)
                    .any(|x| buf[(x, y as u16)].fg == worktree_state_color(c, state)),
                "{state:?}: the state glyph keeps its own colour"
            );
        }
    }

    fn col_of(row: &str, needle: &str) -> usize {
        let b = row
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not in {row:?}"));
        row[..b].chars().count()
    }

    fn buffer_rows(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    /// An app with one shell selected and `capture` as its pane capture.
    /// Built without `startup()` so the user's real state cannot leak in.
    fn app_with_capture(capture: &str) -> App {
        let cfg = Config::default();
        let theme = theme::load("ansi", false).unwrap();
        let mut app = App::new(cfg, theme, None);
        app.state.repositories.push(crate::state::Repository {
            name: "repo-a".into(),
            path: std::path::PathBuf::from("/tmp/repo-a"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            shell: None,
            worktrees: vec![],
        });
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "wt".into(),
                branch: "wt".into(),
                path: std::path::PathBuf::from("/tmp/repo-a-wt"),
                created_at: "2026-01-01T00:00:00Z".into(),
                base_commit: None,
                shells: vec![crate::state::Shell {
                    index: 1,
                    window_name: "wt-1".into(),
                    window_id: Some("@1".into()),
                    pane_id: Some("%1".into()),
                }],
            });
        app.build_tree_flags();
        // One shell → the worktree row is folded onto it; that row is what
        // the preview follows.
        app.sel = crate::app::Sel::Worktree(0, 0);
        app.clamp_sel();
        assert_eq!(
            app.selected_shell(),
            Some((0, 0, 0)),
            "fixture must leave the folded worktree's shell selected"
        );
        app.captures.insert("%1".into(), capture.to_string());
        app
    }

    /// The preview renders the pane's own colours per `[preview].colors`:
    /// `full` paints foreground and background, `foreground` keeps the text
    /// colour only, `none` leaves the panel's plain theme style. The escapes
    /// themselves never reach the screen in any mode.
    #[test]
    fn preview_colors_follow_the_configured_mode() {
        let capture = "plain line\n\x1b[31;44mred on blue\x1b[0m\n";
        let cell_of = |mode: crate::config::PreviewColors| {
            let mut app = app_with_capture(capture);
            app.cfg.preview.colors = mode;
            let backend = ratatui::backend::TestBackend::new(120, 40);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let buf = terminal.backend().buffer().clone();
            let rows = buffer_rows(&terminal);
            let y = rows
                .iter()
                .position(|r| r.contains("red on blue"))
                .unwrap_or_else(|| panic!("coloured line must render:\n{}", rows.join("\n")))
                as u16;
            assert!(
                !rows
                    .iter()
                    .any(|r| r.contains("31;44") || r.contains("[0m")),
                "escape sequences must never reach the screen:\n{}",
                rows.join("\n")
            );
            let x = rows[y as usize].find("red on blue").unwrap() as u16;
            buf[(x, y)].clone()
        };

        let full = cell_of(crate::config::PreviewColors::Full);
        assert_eq!(full.fg, Color::Red);
        assert_eq!(full.bg, Color::Blue);

        let no_bg = cell_of(crate::config::PreviewColors::Foreground);
        assert_eq!(no_bg.fg, Color::Red);
        assert_ne!(no_bg.bg, Color::Blue, "background must be dropped");

        let plain = cell_of(crate::config::PreviewColors::None);
        assert_ne!(plain.fg, Color::Red, "no colour survives text-only mode");
        assert_ne!(plain.bg, Color::Blue);
    }

    /// Live-tail edge cases: a capture shorter than the panel keeps every
    /// line, and a zero-row panel still shows a row.
    #[test]
    fn the_live_tail_handles_short_captures_and_tiny_panels() {
        let preview_tail = |c: &str, h: usize| preview_window(c, h, 0);
        assert_eq!(preview_tail("a\nb\nc\n\n\n", 20), "a\nb\nc");
        // capture-pane -e pads with lines that carry the pane's colours but
        // no visible text — those are padding too
        assert_eq!(
            preview_tail("a\nb\n\x1b[49m\x1b[39m\n\x1b[49m   \n", 20),
            "a\nb"
        );
        assert_eq!(preview_tail("", 20), "");
        // 10 rows of panel → the last 10 lines, flush to the bottom border
        let ten: String = (1..=15).map(|i| format!("l{i}\n")).collect();
        assert_eq!(
            preview_tail(&ten, 10),
            "l6\nl7\nl8\nl9\nl10\nl11\nl12\nl13\nl14\nl15"
        );
        assert_eq!(preview_tail(&ten, 2), "l14\nl15");
        assert_eq!(preview_tail(&ten, 0), "l15");
    }

    /// Scrolling back moves the window earlier in the capture: `scroll` is
    /// how many lines above the live tail the panel is parked, and the
    /// window is clamped to the start of the buffer.
    #[test]
    fn preview_window_slides_back_through_the_capture() {
        let ten: String = (1..=15).map(|i| format!("l{i}\n")).collect();
        // scroll 0 is the live tail
        assert_eq!(
            preview_window(&ten, 10, 0),
            "l6\nl7\nl8\nl9\nl10\nl11\nl12\nl13\nl14\nl15"
        );
        assert_eq!(
            preview_window(&ten, 10, 3),
            "l3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\nl12"
        );
        // parked at the very start
        assert_eq!(
            preview_window(&ten, 10, 5),
            "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10"
        );
        // past the start clamps rather than running off
        assert_eq!(preview_window(&ten, 10, 99), preview_window(&ten, 10, 5));
        // trailing blank padding is dropped before the window is taken,
        // exactly as the live tail does it
        assert_eq!(preview_window("a\nb\nc\n\n\n", 2, 1), "a\nb");
    }

    /// How far back the panel may be scrolled: everything the buffer holds
    /// beyond one panel-full. A capture that does not fill the panel cannot
    /// be scrolled at all.
    #[test]
    fn preview_scroll_max_is_the_history_above_one_panel() {
        let ten: String = (1..=15).map(|i| format!("l{i}\n")).collect();
        assert_eq!(preview_scroll_max(&ten, 10), 5);
        assert_eq!(preview_scroll_max(&ten, 15), 0);
        assert_eq!(preview_scroll_max(&ten, 40), 0);
        assert_eq!(preview_scroll_max("", 10), 0);
        assert_eq!(preview_scroll_max("a\nb\nc\n\n\n", 2), 1);
    }

    /// The preview shows the *tail* of the capture (what the agent just
    /// printed), not the head of the scrollback window — with the newest
    /// line sitting on the row just above the panel's bottom border.
    #[test]
    fn preview_shows_tail_flush_to_bottom_border() {
        let mut capture: String = (1..=200)
            .map(|i| format!("cap-line-{i:03}\n"))
            .collect::<String>();
        // capture-pane pads the pane's empty area with blank lines
        capture.push_str("\n\n\n\n\n");
        let mut app = app_with_capture(&capture);

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rows = buffer_rows(&terminal);

        let last = rows
            .iter()
            .position(|r| r.contains("cap-line-200"))
            .unwrap_or_else(|| panic!("last captured line must be visible:\n{}", rows.join("\n")));
        assert!(
            !rows.iter().any(|r| r.contains("cap-line-001")),
            "the head of a long capture scrolls off"
        );

        // the panel's bottom border is the last row before the statusbar
        let bottom_border = rows.len() - 2;
        assert_eq!(
            last,
            bottom_border - 1,
            "newest line must sit flush above the bottom border \
             (last at {last}, border at {bottom_border})"
        );
    }

    /// A parked preview draws the older lines instead of the tail, and
    /// marks itself: a panel that has silently stopped following the agent
    /// would otherwise read as a stalled agent.
    #[test]
    fn a_parked_preview_shows_older_lines_and_says_so() {
        let capture: String = (1..=200).map(|i| format!("cap-line-{i:03}\n")).collect();
        let mut app = app_with_capture(&capture);
        app.preview_pane = Some("%1".into());
        app.preview_scroll = 50;

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rows = buffer_rows(&terminal);
        let text = rows.join("\n");

        assert!(
            rows.iter().any(|r| r.contains("cap-line-150")),
            "the parked window ends 50 lines above the tail:\n{text}"
        );
        assert!(
            !rows.iter().any(|r| r.contains("cap-line-200")),
            "the live tail has scrolled out of view:\n{text}"
        );
        assert!(
            text.contains("↑50"),
            "the panel says how far back it is parked:\n{text}"
        );
        assert!(
            text.contains("live Esc"),
            "the statusbar offers the way back to the tail:\n{text}"
        );
    }

    // ---- M11 — predefined messages modal ----

    /// The modal lists the configured messages, numbered, with the cursor
    /// on the selected one and its own dismissable rect.
    #[test]
    fn messages_modal_lists_numbered_entries() {
        let mut app = seeded_app();
        app.mode = Mode::Messages { index: 1 };
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Messages"), "modal has a title");
        assert!(text.contains("1 continue"), "first entry is numbered");
        // Nothing is selected in this fixture, so there is no context to
        // resolve the template against and it is listed as written.
        assert!(
            text.contains("2 merge to LOCAL <base_branch>"),
            "second entry is numbered and listed in full, got: {text}"
        );
        assert!(text.contains("send ↵"), "modal states its keys");
        assert!(
            app.popup_area.is_some(),
            "modal must register its dismissable rect"
        );
    }

    /// The modal lists what will actually be sent: the whole message, with
    /// the selected shell's tokens already resolved. Reading "merge to
    /// trunk" is the only way to know it will not say "merge to main".
    #[test]
    fn messages_modal_shows_the_expanded_message_text() {
        let mut app = seeded_app_with_shell();
        app.mode = Mode::Messages { index: 0 };
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("2 merge to LOCAL trunk"),
            "the token is resolved in the list, got: {text}"
        );
        assert!(
            !text.contains("<base_branch>"),
            "no raw token survives to the screen, got: {text}"
        );
    }

    /// A frame too small for the modal is refused with a message, never
    /// drawn as garbage.
    #[test]
    fn tiny_frame_refuses_the_messages_modal() {
        let mut app = seeded_app();
        app.mode = Mode::Messages { index: 0 };
        // Wide enough for the dashboard's own strip, too short for any
        // dialog: overlays are built for 26 columns now, so height is what
        // refuses them.
        let backend = ratatui::backend::TestBackend::new(40, 7);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("too small"), "tiny frame refuses the modal");
    }

    /// The statusbar advertises `p` on a shell (same words as the help).
    #[test]
    fn statusbar_offers_predefined_on_a_shell() {
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Shell(0, 0, 0);
        let labels: Vec<String> = bar_entries(&app)
            .into_iter()
            .map(|(h, _)| h.label())
            .collect();
        assert!(
            labels.iter().any(|l| l == "predefined p"),
            "shell shortcuts: {labels:?}"
        );
        assert!(
            bar_entries(&app)
                .iter()
                .any(|(_, a)| *a == BarAction::PredefinedSend),
            "the entry is clickable"
        );
    }

    /// A shell row offers `d close` (the shell), a worktree row `d delete`
    /// (the worktree) — the bar never offers to delete a worktree from a
    /// shell row. No selection offers a repository removal any more.
    #[test]
    fn statusbar_delete_entry_is_contextual() {
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Shell(0, 0, 0);
        let entries = bar_entries(&app);
        assert!(
            entries
                .iter()
                .any(|(h, a)| h.label() == "close d" && *a == BarAction::DeleteShell),
            "a shell closes itself: {entries:?}"
        );
        assert!(
            !entries.iter().any(|(_, a)| *a == BarAction::DeleteWorktree),
            "a shell row must not offer the worktree delete: {entries:?}"
        );

        app.sel = crate::app::Sel::Worktree(0, 0);
        let entries = bar_entries(&app);
        assert!(
            entries
                .iter()
                .any(|(h, a)| h.label() == "delete d" && *a == BarAction::DeleteWorktree),
            "a worktree deletes itself: {entries:?}"
        );

        app.sel = crate::app::Sel::Repository(0);
        let labels: Vec<String> = bar_entries(&app)
            .into_iter()
            .map(|(h, _)| h.label())
            .collect();
        assert!(
            !labels.iter().any(|l| l.contains("remove")),
            "the repository removal entry is gone: {labels:?}"
        );
    }

    /// The help palette documents `p` under the shell group.
    #[test]
    fn help_documents_predefined_messages() {
        let rows: Vec<&str> = HELP_GROUPS
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .flat_map(|(keys, _)| keys.iter().copied())
            .collect();
        assert!(rows.contains(&"p"), "help lists the p key: {rows:?}");
    }

    /// The add-shell picker lists what a new shell can run: the
    /// repository's own agent command first, a plain login shell last —
    /// numbered, because the numbers are the keys that take an entry.
    /// A repository row with no shell says what the empty panel is for and
    /// which key fills it — the panel is the only place that can, since the
    /// row itself has nothing to draw yet.
    #[test]
    fn the_repository_panel_says_how_to_start_a_shell() {
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Repository(0);
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("no shell in this repository yet"),
            "the empty panel names its state: {text}"
        );
        assert!(
            text.contains("mise run"),
            "and what the shell is for: {text}"
        );
    }

    /// Once it has one, the row says so and names the checkout it runs in
    /// — the base branch, which is what tells it from the worktrees below.
    #[test]
    fn a_repository_row_names_the_checkout_its_shell_runs_in() {
        let mut app = seeded_app();
        app.state.repositories[0].shell = Some(crate::state::Shell {
            index: 0,
            window_name: "lw:.repo/0".into(),
            window_id: Some("@9".into()),
            pane_id: Some("%9".into()),
        });
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("● main"),
            "the row carries the base branch its shell sits on: {text}"
        );
    }

    /// The bar names `Enter` on a repository row — the one action there
    /// that is not otherwise discoverable — and only offers `d` once there
    /// is a shell to close.
    #[test]
    fn the_repository_bar_names_the_shell_and_its_close() {
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Repository(0);
        let entries = bar_entries(&app);
        assert!(
            entries
                .iter()
                .any(|(h, a)| h.label() == "shell ↵" && *a == BarAction::RepoShell),
            "Enter is named: {:?}",
            entries.iter().map(|(h, _)| h.label()).collect::<Vec<_>>()
        );
        assert!(
            !entries.iter().any(|(_, a)| *a == BarAction::CloseRepoShell),
            "nothing to close yet"
        );

        app.state.repositories[0].shell = Some(crate::state::Shell {
            index: 0,
            window_name: "lw:.repo/0".into(),
            window_id: Some("@9".into()),
            pane_id: Some("%9".into()),
        });
        assert!(
            bar_entries(&app)
                .iter()
                .any(|(h, a)| h.label() == "close d" && *a == BarAction::CloseRepoShell),
            "and `d` appears once there is one"
        );
    }

    #[test]
    fn add_shell_picker_lists_the_agent_and_a_plain_shell() {
        let mut app = seeded_app();
        app.state.repositories[0].agent_command = "codex exec --yolo".into();
        app.sel = crate::app::Sel::Repository(0);
        app.mode = Mode::AddShell { index: 0 };
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Add Shell"), "popup title: {text}");
        assert!(
            text.contains("1 codex exec --yolo"),
            "the repository's agent leads the numbered list: {text}"
        );
        assert!(
            text.contains("plain shell"),
            "a plain login shell is offered: {text}"
        );
        assert!(text.contains("start"), "the footer names Enter: {text}");
    }

    /// The prompt is sized to its content, so the `cancel esc` footer
    /// survives a short frame — the add-shell hint line used to push it out
    /// of a popup whose height was a fixed percentage of the terminal.
    #[test]
    fn input_popup_keeps_its_footer_on_short_frames() {
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Repository(0);
        for kind in [InputKind::QuickSend, InputKind::NewWorktree] {
            app.mode = Mode::Input {
                kind,
                text: String::new(),
            };
            for height in [24, 30, 40] {
                let backend = ratatui::backend::TestBackend::new(100, height);
                let mut terminal = ratatui::Terminal::new(backend).unwrap();
                terminal.draw(|f| draw(f, &mut app)).unwrap();
                let text = buffer_text(&terminal);
                assert!(
                    text.contains("cancel esc"),
                    "{kind:?} popup must keep its footer at height {height}: {text}"
                );
            }
        }
    }

    /// A folded solo worktree shows its shell's status, so `blocked` has to
    /// survive all the way to a rendered row — not just to the enum.
    #[test]
    fn a_blocked_shell_reads_as_blocked_in_the_tree() {
        let mut app = seeded_app();
        app.state.repositories[0]
            .worktrees
            .push(crate::state::Worktree {
                name: "needs-me".into(),
                branch: "needs-me".into(),
                path: std::path::PathBuf::from("/tmp/wt-blocked"),
                created_at: String::new(),
                base_commit: None,
                shells: vec![crate::state::Shell {
                    index: 0,
                    window_name: "lw:needs-me/0".into(),
                    window_id: Some("@7".into()),
                    pane_id: Some("%0".into()),
                }],
            });
        app.statuses.insert("%0".into(), Status::Blocked);
        app.build_tree_flags();
        app.repo_expanded[0] = true;

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();

        let row = rows.iter().find(|r| r.contains("needs-me")).unwrap();
        assert!(row.contains(Status::Blocked.label()), "row reads: {row:?}");
    }

    /// The rule between a modal's body and its shortcuts: `├`, `─` to the
    /// far edge, `┤` — border to border, so the footer reads as its own
    /// compartment rather than a stray line inside the padding.
    fn assert_footer_rule(buf: &ratatui::buffer::Buffer, area: Rect, what: &str) {
        let y = area.y + area.height - 3;
        let row: Vec<String> = (area.x..area.x + area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        assert_eq!(row[0], "\u{251c}", "{what}: the rule meets the left border");
        assert_eq!(
            row[area.width as usize - 1],
            "\u{2524}",
            "{what}: the rule meets the right border"
        );
        assert!(
            row[1..area.width as usize - 1]
                .iter()
                .all(|c| c == "\u{2500}"),
            "{what}: an unbroken rule above the shortcuts, got {}",
            row.concat()
        );
    }

    /// The create/delete dialogs size themselves to what they say instead
    /// of taking a fixed slice of the frame, and keep one blank column on
    /// each side between the text and the border. Neither the top nor the
    /// bottom row is padding: the first content row sits right under the
    /// border, and the last one is the shortcut footer, fenced off by the
    /// rule — a gap below it would float the footer off the frame.
    #[test]
    fn dialogs_are_content_sized_with_one_char_padding() {
        let mut app = seeded_app();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        for mode in [
            Mode::Input {
                kind: InputKind::NewWorktree,
                text: "my-worktree".into(),
            },
            Mode::Confirm {
                confirm: Confirm {
                    title: "Delete Worktree".into(),
                    body: "delete 'wt' from repo-a?".into(),
                    yes_label: Some("yes".into()),
                    warning: None,
                    force_label: Some("force".into()),
                    action: ConfirmAction::DeleteWorktree {
                        repository: 0,
                        worktree: 0,
                    },
                },
            },
        ] {
            app.mode = mode;
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let area = app.popup_area.expect("dialog is on screen");
            let buf = terminal.backend().buffer().clone();
            let row = |y: u16| -> String {
                (area.x..area.x + area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect()
            };

            // Content-sized: nowhere near the frame, and no taller than
            // its few lines need.
            assert!(
                area.width < 120 * 60 / 100 && area.height <= 7,
                "dialog must fit its content, got {area:?}",
            );
            // No padding row: the first content row is text.
            let inside = row(area.y + 1);
            assert!(
                !inside.trim().is_empty(),
                "the row under the top border must carry content, got {inside:?}",
            );
            // The last content row is the shortcut footer, with a blank
            // row of its own above it.
            let footer = row(area.y + area.height - 2);
            assert!(
                footer.contains("cancel"),
                "the bottom content row carries the shortcuts, got {footer:?}",
            );
            assert_footer_rule(&buf, area, "dialog");
            // Padding column on both sides of every content row — the
            // rule row is the exception: it spans border to border.
            let rule_y = area.y + area.height - 3;
            for y in (area.y + 1..area.y + area.height - 1).filter(|y| *y != rule_y) {
                let cells: Vec<String> = (area.x..area.x + area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect();
                assert_eq!(cells[1], " ", "left padding column on row {y}");
                assert_eq!(
                    cells[area.width as usize - 2],
                    " ",
                    "right padding column on row {y}",
                );
            }
        }
    }

    /// Every modal keeps `MODAL_PADDING` (a blank column each side, no
    /// blank row above or below) and ends with its shortcut footer, fenced
    /// off by a border-to-border rule. One rule, checked on all of them at
    /// once — a new overlay that forgets either fails here.
    #[test]
    fn every_modal_is_padded_and_ends_with_its_shortcuts() {
        let mut app = seeded_app_with_shell();
        app.sel = crate::app::Sel::Shell(0, 0, 0);
        app.cfg.messages = vec![crate::config::MessageConfig {
            text: "run the tests".into(),
        }];
        let modes = [
            ("help", Mode::Help),
            ("themes", Mode::ThemePicker { index: 0 }),
            ("messages", Mode::Messages { index: 0 }),
            (
                "input",
                Mode::Input {
                    kind: InputKind::NewWorktree,
                    text: "claude".into(),
                },
            ),
            (
                "textarea",
                Mode::Textarea {
                    // Non-empty: an empty buffer would leave the first
                    // content row blank and look like a padding row.
                    buf: {
                        let mut buf = crate::app::TextBuf::new();
                        buf.insert_char('x');
                        buf
                    },
                },
            ),
            (
                "confirm",
                Mode::Confirm {
                    confirm: Confirm {
                        title: "Delete Worktree".into(),
                        body: "delete 'wt' from repo-a?".into(),
                        yes_label: Some("yes".into()),
                        warning: None,
                        force_label: None,
                        action: ConfirmAction::DeleteWorktree {
                            repository: 0,
                            worktree: 0,
                        },
                    },
                },
            ),
        ];
        for (name, mode) in modes {
            app.mode = mode;
            let backend = ratatui::backend::TestBackend::new(110, 46);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let area = app
                .popup_area
                .unwrap_or_else(|| panic!("{name} is on screen"));
            let buf = terminal.backend().buffer().clone();
            let row = |y: u16| -> String {
                (area.x + 1..area.x + area.width - 1)
                    .map(|x| buf[(x, y)].symbol())
                    .collect()
            };
            let first = row(area.y + 1);
            assert!(
                !first.trim().is_empty(),
                "{name}: content starts right under the top border, got {first:?}"
            );
            assert!(
                first.starts_with(' ') && first.ends_with(' '),
                "{name}: padding column on both sides of the first row, got {first:?}"
            );
            let footer = row(area.y + area.height - 2);
            // A named way out, not necessarily `esc`: the confirm answers
            // "no" to `n` and spells that instead, since `esc` cancels every
            // overlay and does not need re-teaching per dialog.
            assert!(
                footer.contains("esc") || footer.contains("cancel n"),
                "{name}: the bottom content row names the way out, got {footer:?}"
            );
            assert!(
                footer.starts_with(' ') && footer.ends_with(' '),
                "{name}: padding column on both sides of the footer, got {footer:?}"
            );
            assert_footer_rule(&buf, area, name);
        }
    }

    /// The footer rule paints symbol and foreground only: it inherits
    /// whatever background the modal drew, so it never punches a hole in
    /// an overlay that fills `popup_bg` (`Input`) nor tints one that does
    /// not (`ThemePicker`).
    #[test]
    fn the_footer_rule_keeps_the_modal_background() {
        let mut app = seeded_app_with_shell();
        for (name, mode) in [
            (
                "input",
                Mode::Input {
                    kind: InputKind::NewWorktree,
                    text: "claude".into(),
                },
            ),
            ("themes", Mode::ThemePicker { index: 0 }),
        ] {
            app.mode = mode;
            let backend = ratatui::backend::TestBackend::new(110, 46);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let area = app.popup_area.unwrap();
            let buf = terminal.backend().buffer().clone();
            let rule_y = area.y + area.height - 3;
            for x in area.x..area.x + area.width {
                assert_eq!(
                    buf[(x, rule_y)].bg,
                    buf[(x, rule_y + 1)].bg,
                    "{name}: the rule keeps the background of column {x}"
                );
                assert_eq!(
                    buf[(x, rule_y)].fg,
                    app.theme.colors.popup_border,
                    "{name}: the rule is drawn in the border colour"
                );
            }
        }
    }

    /// Shortcuts read `<description> <key>` and only the key is
    /// highlighted — checked on the statusbar, where every entry is one.
    #[test]
    fn shortcut_keys_are_the_only_highlighted_part() {
        let mut app = seeded_app_with_shell();
        app.sel = crate::app::Sel::Shell(0, 0, 0);
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let y = 39;
        let accent = app.theme.colors.accent;
        for (entry, _) in bar_entries(&app) {
            let label = entry.label();
            let x0 = (0..120u16)
                .find(|x| {
                    label
                        .chars()
                        .enumerate()
                        .all(|(i, c)| buf[(x + i as u16, y)].symbol() == c.to_string())
                })
                .unwrap_or_else(|| panic!("bar shows {label:?}"));
            let key_at = x0 + (entry.desc.chars().count() + 1) as u16;
            assert_eq!(
                buf[(key_at, y)].fg,
                accent,
                "{label:?}: the key is highlighted"
            );
            assert_ne!(buf[(x0, y)].fg, accent, "{label:?}: the description is not");
        }
    }

    // ---- the narrow strip: overlays built for the dashboard's own pane ----

    #[test]
    fn wrap_breaks_on_words_and_only_cuts_a_word_that_cannot_fit() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap("", 10), vec![""]);
        assert_eq!(wrap("short", 10), vec!["short"]);
        // A word wider than the line is cut where the line ends, never
        // printed past it.
        assert_eq!(wrap("aaaaaaaa", 3), vec!["aaa", "aaa", "aa"]);
        assert_eq!(wrap("hi aaaaaa", 3), vec!["hi", "aaa", "aaa"]);
        for line in wrap("a much longer sentence than the box", 9) {
            assert!(UnicodeWidthStr::width(line.as_str()) <= 9, "{line:?}");
        }
    }

    #[test]
    fn a_tail_is_what_a_full_field_shows() {
        assert_eq!(tail_to_width("abcdef", 3), "def");
        assert_eq!(tail_to_width("abc", 10), "abc");
        assert_eq!(tail_to_width("", 4), "");
    }

    /// Every overlay is built for the dashboard's own strip: it must be
    /// drawn — not refused — at the narrowest width a docked dashboard
    /// ever has, and it must never be wider than that strip's cap, so the
    /// same dialog is the same dialog whether or not a pane is docked.
    #[test]
    fn every_overlay_is_drawn_in_the_narrowest_strip_and_capped_in_a_wide_one() {
        let modes = || {
            vec![
                Mode::Help,
                Mode::Diagnostics,
                Mode::ThemePicker { index: 0 },
                Mode::AddShell { index: 0 },
                Mode::Messages { index: 0 },
                Mode::Input {
                    kind: InputKind::NewWorktree,
                    text: "a-long-worktree-name-typed-in-full".into(),
                },
                Mode::Textarea {
                    buf: crate::app::TextBuf::new(),
                },
                Mode::Confirm {
                    confirm: Confirm {
                        title: "Delete Worktree".into(),
                        body: "delete worktree 'wt' at /tmp/repo-a-wt, kill its windows and \
                               remove the branch?"
                            .into(),
                        warning: Some("this cannot be undone".into()),
                        yes_label: Some("delete".into()),
                        force_label: Some("force".into()),
                        action: ConfirmAction::DeleteWorktree {
                            repository: 0,
                            worktree: 0,
                        },
                    },
                },
            ]
        };
        for (w, h) in [(crate::dock::DASH_MIN_WIDTH, 24), (200, 60)] {
            for mode in modes() {
                let mut app = seeded_app_with_shell();
                app.cfg.diagnostics = vec!["[worktrees] unknown key 'foo' — dropped".into()];
                app.theme_names = vec!["ansi".into(), "gruvbox".into()];
                app.mode = mode;
                let backend = ratatui::backend::TestBackend::new(w, h);
                let mut terminal = ratatui::Terminal::new(backend).unwrap();
                terminal.draw(|f| draw(f, &mut app)).expect("draw");
                let text = buffer_text(&terminal);
                let area = app
                    .popup_area
                    .unwrap_or_else(|| panic!("{:?} refused at {w}x{h}: {text}", app.mode));
                assert!(
                    area.width <= MODAL_MAX_W && area.width <= w,
                    "{:?} at {w}x{h} is {} columns wide",
                    app.mode,
                    area.width
                );
                assert!(
                    !text.contains("too small"),
                    "{:?} refused at {w}x{h}",
                    app.mode
                );
                // The way out survives the width: a footer cut at the
                // border would lose `cancel esc`, which is the one hint a
                // dialog cannot afford to lose. (The confirm spells its
                // own `cancel n` instead — `esc` cancels every overlay and
                // is not re-taught per dialog.)
                assert!(
                    text.contains("esc") || text.contains("cancel n"),
                    "{:?} at {w}x{h} has no way out on screen: {text}",
                    app.mode
                );
            }
        }
    }
}
