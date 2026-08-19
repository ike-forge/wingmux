//! TUI rendering: tree, preview, status bar, toast, overlays.
//!
//! All colours come from the resolved [`Theme`]; the renderer never hardcodes
//! a colour.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{App, BarAction, InputKind, Mode, Sel};
use crate::status::Status;
use crate::theme::ResolvedColors;

/// Minimum sizes for the layout tiers. Resize is normal, not an edge case:
/// the UI degrades gracefully — the preview hides first, then the bar —
/// and a too-small frame shows a message instead of drawing garbage.
const WIDE_MIN: (u16, u16) = (64, 12);
const SPLIT_MIN: (u16, u16) = (44, 9);
const TREE_MIN: (u16, u16) = (26, 7);

/// Cap on the tree panel's width, in columns (see [`tree_width`]).
const TREE_MAX: u16 = 48;

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

/// Minimum popup size per overlay kind; below it the overlay is refused
/// with a message ("frame too small for this dialog") instead of drawing
/// garbage — never a crash.
fn overlay_min(kind: OverlayKind) -> (u16, u16) {
    match kind {
        OverlayKind::Help => (44, 14),
        OverlayKind::Diagnostics => (44, 10),
        OverlayKind::ThemePicker => (34, 10),
        OverlayKind::Messages => (34, 10),
        OverlayKind::Input | OverlayKind::Confirm => (34, 10),
        // The textarea is the main interaction — it needs real estate.
        OverlayKind::Textarea => (50, 12),
    }
}

#[derive(Clone, Copy)]
enum OverlayKind {
    Help,
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

fn status_color(theme: &ResolvedColors, st: Status) -> Color {
    match st {
        Status::Working => theme.status_working,
        Status::Waiting => theme.status_waiting,
        Status::Idle => theme.status_idle,
        Status::Failed => theme.status_failed,
        Status::Ended => theme.status_ended,
        Status::Dead => theme.status_dead,
    }
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

/// A feature is dead once every shell under it is — a feature with no
/// shells at all is not dead, it was simply never started.
fn feature_dead(app: &App, feat: &crate::state::Feature) -> bool {
    !feat.shells.is_empty()
        && feat
            .shells
            .iter()
            .all(|sh| shell_status(app, sh) == Status::Dead)
}

/// A project is dead once every feature under it is.
fn project_dead(app: &App, proj: &crate::state::Project) -> bool {
    !proj.features.is_empty() && proj.features.iter().all(|f| feature_dead(app, f))
}

/// The colour a tree row draws in: a dead entry is muted *whole* — guide,
/// icon, name and count all take the dead colour — so the eye skips it
/// instead of re-reading a row that carries no live agent.
fn row_color(theme: &ResolvedColors, dead: bool, live: Color) -> Color {
    if dead { theme.status_dead } else { live }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.viewport = (area.width, area.height);
    // theme background
    f.render_widget(
        Block::default().style(Style::default().bg(app.theme.colors.background)),
        area,
    );
    match tier_impl(area.width, area.height) {
        Tier::Message => {
            draw_too_small(f, area, app);
            return;
        }
        Tier::Wide | Tier::Split => {
            let tree_w = tree_width((area.width, area.height)).unwrap_or(area.width);
            draw_split(f, area, app, tree_w);
        }
        Tier::TreeOnly => draw_tree_only(f, area, app),
    }
    // Overlays render last (on top). popup_area is (re)computed every
    // frame so mouse "click outside dismisses" tracks what is on screen;
    // in Normal there is nothing to dismiss.
    app.popup_area = None;
    match &app.mode {
        Mode::Help => {
            let kind = OverlayKind::Help;
            if fits(area, overlay_min(kind)) {
                let rect = draw_help(f, app);
                app.popup_area = Some(rect);
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::Diagnostics => {
            let kind = OverlayKind::Diagnostics;
            if fits(area, overlay_min(kind)) {
                let rect = draw_diagnostics(f, app);
                app.popup_area = Some(rect);
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::ThemePicker { .. } => {
            let kind = OverlayKind::ThemePicker;
            if fits(area, overlay_min(kind)) {
                let rect = draw_theme_picker(f, app);
                app.popup_area = Some(rect);
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::Messages { .. } => {
            let kind = OverlayKind::Messages;
            if fits(area, overlay_min(kind)) {
                let rect = draw_messages(f, app);
                app.popup_area = Some(rect);
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::Input { .. } => {
            let kind = OverlayKind::Input;
            if fits(area, overlay_min(kind)) {
                let rect = draw_input(f, app);
                app.popup_area = Some(rect);
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::Textarea { .. } => {
            let kind = OverlayKind::Textarea;
            if fits(area, overlay_min(kind)) {
                let rect = draw_textarea(f, app);
                app.popup_area = Some(rect);
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::Confirm { .. } => {
            let kind = OverlayKind::Confirm;
            if fits(area, overlay_min(kind)) {
                let rect = draw_confirm(f, app);
                app.popup_area = Some(rect);
            } else {
                draw_overlay_too_small(f, area, app, kind);
            }
        }
        Mode::Normal => {}
    }
}

fn fits(area: Rect, min: (u16, u16)) -> bool {
    area.width >= min.0 && area.height >= min.1
}

/// App-generated glyphs with ASCII fallbacks (the user's pane content is
/// never rewritten — only our own labels).
pub(crate) fn warn_mark(ascii: bool) -> &'static str {
    if ascii { "!" } else { "⚠" }
}

pub(crate) fn sep(ascii: bool) -> &'static str {
    if ascii { " - " } else { " · " }
}

/// Replace box-drawing glyphs in a rendered area with plain ASCII
/// (`+ - |`). Terminals that mangle box-drawing stay readable. Cell
/// styles are untouched — the fallback changes shapes, not colours.
fn asciify(f: &mut Frame, area: Rect) {
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            let repl = match cell.symbol() {
                "─" => "-",
                "│" => "|",
                "┌" | "┐" | "└" | "┘" => "+",
                "╭" | "╮" | "╰" | "╯" => "+",
                _ => continue,
            };
            cell.set_symbol(repl);
        }
    }
}

/// Tree | preview, statusbar below. `tree_w` comes from [`tree_width`] —
/// the same math drives mouse hit-testing.
fn draw_split(f: &mut Frame, area: Rect, app: &mut App, tree_w: u16) {
    let [main, statusbar] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [tree, preview] =
        Layout::horizontal([Constraint::Length(tree_w), Constraint::Fill(1)]).areas(main);

    draw_tree(f, tree, app);
    draw_preview(f, preview, app);
    draw_toast(f, main, app);
    draw_statusbar(f, statusbar, app);
}

/// Tree across the whole main area.
fn draw_tree_only(f: &mut Frame, area: Rect, app: &mut App) {
    let [main, statusbar] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    draw_tree(f, main, app);
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
            Line::from(Span::styled("q quits", fg(theme.dim))),
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
    let block = popup_block(theme, " frame too small for this dialog ");
    let text = Text::from(vec![
        Line::from(""),
        Line::from(Span::raw(format!(
            "enlarge the window to at least {min_w}x{min_h}"
        ))),
        Line::from(Span::styled(
            "click anywhere or press Esc to dismiss",
            fg(theme.dim),
        )),
    ]);
    f.render_widget(Paragraph::new(text).block(block), rect);
    if app.ascii {
        asciify(f, rect);
    }
}

/// Panel/popup title: foreground only, no background fill — the "simple
/// title display without background" from the M9 UI pass.
fn title_style(theme: &ResolvedColors) -> Style {
    fg(theme.title_fg).add_modifier(Modifier::BOLD)
}

/// Overlay block: rounded corners, popup border colour, bg-less title.
fn popup_block(theme: &ResolvedColors, title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(fg(theme.popup_border))
        .title(Span::styled(title.into(), title_style(theme)))
}

fn panel_block(theme: &ResolvedColors, title: &str, focused: bool) -> Block<'static> {
    let border = if focused {
        theme.border_focused
    } else {
        theme.border
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(fg(border))
        .title(Span::styled(format!(" {title} "), title_style(theme)))
}

fn draw_tree(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme.colors;
    // ASCII fallback (terminals that mangle box-drawing): plain markers.
    let (exp, coll, hl) = if app.ascii {
        ("v", ">", ">")
    } else {
        ("▾", "▸", "›")
    };
    // Tree guides: `├─ `/`└─ ` for children, `│` continuing parents.
    let (child, last_child) = if app.ascii {
        ("  |- ", "  `- ")
    } else {
        ("  ├─ ", "  └─ ")
    };
    let (grandchild, last_grandchild) = if app.ascii {
        ("  |  |- ", "  |  `- ")
    } else {
        ("  │  ├─ ", "  │  └─ ")
    };
    let (no_grandchild, no_last_grandchild) = if app.ascii {
        ("     |- ", "     `- ")
    } else {
        ("     ├─ ", "     └─ ")
    };
    // Per-type icons (aesthetic pass; ASCII keeps plain markers). Each
    // icon carries the one fact its row cannot show otherwise: the
    // project icon is filled when it holds features and hollow when it is
    // empty; the feature icon is hollow for a clean worktree and filled
    // when it has uncommitted changes. Counts stay out of the rows — the
    // tree is expanded by default, so the children speak for themselves.
    let (proj_icon, empty_proj_icon, feat_icon, dirty_icon, shell_icon) = if app.ascii {
        ("*", "o", "-", "*", "")
    } else {
        ("▣", "▢", "◇", "◆", "● ")
    };
    let mut items: Vec<ListItem> = Vec::new();
    let mut sel_row: Option<usize> = None;
    let mut row: usize = 0;

    // Scope-filtered: only projects inside the current path get rows.
    let vis = app.visible_projects();
    for pi in vis {
        let proj = &app.state.projects[pi];
        if !app.flatten {
            let expanded = app.proj_expanded.get(pi).copied().unwrap_or(false);
            let marker = if expanded { exp } else { coll };
            let icon = if proj.features.is_empty() {
                empty_proj_icon
            } else {
                proj_icon
            };
            let pdead = project_dead(app, proj);
            let mut spans = vec![Span::styled(
                format!("{marker} {icon} {}", proj.name),
                fg(row_color(theme, pdead, theme.tree_project)).add_modifier(Modifier::BOLD),
            )];
            if proj.missing {
                spans.push(Span::styled(" (missing)", fg(theme.dim)));
            }
            items.push(ListItem::new(Line::from(spans)));
            if app.sel == Sel::Project(pi) {
                sel_row = Some(row);
            }
            row += 1;
            if !expanded {
                continue;
            }
        }
        let n_feat = proj.features.len();
        for (fi, feat) in proj.features.iter().enumerate() {
            let fexp = app
                .feat_expanded
                .get(pi)
                .and_then(|v| v.get(fi))
                .copied()
                .unwrap_or(false);
            // One worktree, one agent: a feature with a single shell is
            // folded for good — no marker, no child row, and its own row
            // carries that shell's status.
            let solo = app.solo_shell(pi, fi);
            let fdead = feature_dead(app, feat);
            let marker = match solo {
                // a blank of the marker's width keeps the names aligned
                Some(_) => " ".repeat(exp.chars().count()),
                None if fexp => exp.to_string(),
                None => coll.to_string(),
            };
            let n = feat.shells.len();
            let dirty = app.feature_dirty_cached(pi, fi);
            let ficon = if dirty { dirty_icon } else { feat_icon };
            // In flatten mode the features are the top level (no guide);
            // otherwise they hang off the project with a tree branch.
            let (prefix, icon_span) = if app.flatten {
                (format!("{marker} "), format!("{ficon} "))
            } else {
                let guide = if fi + 1 == n_feat { last_child } else { child };
                (format!("{guide}{marker} "), format!("{ficon} "))
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(prefix, fg(row_color(theme, fdead, theme.tree_feature))),
                Span::styled(
                    icon_span,
                    fg(row_color(
                        theme,
                        fdead,
                        if dirty {
                            theme.status_waiting
                        } else {
                            theme.tree_feature
                        },
                    )),
                ),
                Span::styled(
                    feat.name.clone(),
                    fg(row_color(theme, fdead, theme.tree_feature)),
                ),
                match solo {
                    Some(si) => {
                        let st = shell_status(app, &feat.shells[si]);
                        Span::styled(format!("  {}", st.label()), fg(status_color(theme, st)))
                    }
                    // A multi-shell feature says nothing extra: its shells
                    // are one row below, each with its own status.
                    None => Span::raw(""),
                },
            ])));
            if app.sel == Sel::Feature(pi, fi) {
                sel_row = Some(row);
            }
            row += 1;
            // A solo feature emits no child row whatever its flag says —
            // `App::visible_nodes` skips it the same way, and the two must
            // never drift (the cursor and mouse hit-testing map rows to
            // nodes by index).
            if !fexp || solo.is_some() {
                continue;
            }
            for (si, sh) in feat.shells.iter().enumerate() {
                let st = shell_status(app, sh);
                let guide = if app.flatten {
                    if si + 1 == n {
                        last_grandchild
                    } else {
                        grandchild
                    }
                } else if fi + 1 == n_feat {
                    if si + 1 == n {
                        no_last_grandchild
                    } else {
                        no_grandchild
                    }
                } else if si + 1 == n {
                    last_grandchild
                } else {
                    grandchild
                };
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{guide}{shell_icon}#{}", sh.index),
                        fg(row_color(theme, st == Status::Dead, theme.tree_shell)),
                    ),
                    Span::raw(" "),
                    Span::styled(st.label(), fg(status_color(theme, st))),
                ])));
                if app.sel == Sel::Shell(pi, fi, si) {
                    sel_row = Some(row);
                }
                row += 1;
            }
        }
    }

    // Flatten mode lists a single repo's features, so the title names
    // what the rows actually are. The count follows the tree: only
    // in-scope projects have rows, so out-of-scope ones must not be
    // counted.
    let vis = app.visible_projects();
    let title = if app.flatten {
        let n = vis
            .first()
            .map(|&pi| app.state.projects[pi].features.len())
            .unwrap_or(0);
        format!("Features ({n})")
    } else {
        format!("Projects ({})", vis.len())
    };
    let block = panel_block(theme, &title, true);
    let list = List::new(items)
        .block(block)
        .highlight_style(on(theme.selection_fg, theme.selection_bg))
        .highlight_symbol(format!("{hl} "));
    let mut state = ListState::default();
    state.select(sel_row);
    f.render_stateful_widget(list, area, &mut state);
    if app.ascii {
        asciify(f, area);
    }
}

/// The slice of a pane capture the preview shows in a panel of
/// `inner_height` rows: the *newest* lines, filling the panel down to its
/// bottom border.
///
/// `capture-pane -S -N` returns a window that starts N lines back in the
/// scrollback and ends with the blank padding of the pane's empty area, so
/// rendering it from the top shows neither the start nor the end of the
/// agent's output — the trailing blanks are dropped and the tail wins.
fn preview_tail(capture: &str, inner_height: usize) -> String {
    let mut lines: Vec<&str> = capture.lines().collect();
    while lines
        .last()
        .map(|l| crate::ansi::is_blank(l))
        .unwrap_or(false)
    {
        lines.pop();
    }
    let rows = inner_height.max(1);
    let start = lines.len().saturating_sub(rows);
    lines[start..].join("\n")
}

fn draw_preview(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme.colors;
    let block = panel_block(theme, "Preview", false);
    let text = match app.selected_shell() {
        Some(_) => {
            if let Some(pid) = app.selected_shell_pane() {
                if let Some(cap) = app.captures.get(&pid) {
                    // borders take two rows off the panel; the capture keeps
                    // the pane's escapes, rendered per [preview].colors
                    let tail = preview_tail(cap, area.height.saturating_sub(2) as usize);
                    crate::ansi::styled_text(&tail, app.cfg.preview.colors)
                } else {
                    Text::from(if app.ascii {
                        "capturing... (select the shell in the tree)"
                    } else {
                        "capturing… (select the shell in the tree)"
                    })
                }
            } else {
                Text::from("no live pane for this shell")
            }
        }
        None => Text::from(
            "select a shell to preview its pane\n\n\
             the tail of `capture-pane` is shown here, updated on every poll.\n\
             status is classified from this text + the pane's foreground process.",
        ),
    };
    let p = Paragraph::new(text)
        .block(block)
        .style(fg(theme.foreground));
    f.render_widget(p, area);
    if app.ascii {
        asciify(f, area);
    }
}

/// The contextual shortcut entries for the statusbar — same words as the
/// help overlay. The bar is contextual: keys that would do nothing in the
/// current state do not appear. `? help` is always there (it is the
/// palette key); hjkl/arrows/Esc/q are assumed knowledge and never take a
/// slot (per the discoverability rules).
pub(crate) fn bar_entries(app: &App) -> Vec<(String, BarAction)> {
    use BarAction::*;
    match app.sel {
        Sel::Project(_) => vec![("n feature".into(), NewFeature), ("? help".into(), Help)],
        // A folded feature row is its shell: it offers the shell actions
        // on top of the feature ones (`a` gives it a second shell, which
        // unfolds the subtree).
        Sel::Feature(pi, fi) if app.solo_shell(pi, fi).is_some() => vec![
            ("↵ attach".into(), Attach),
            ("s send".into(), QuickSend),
            ("p predefined".into(), PredefinedSend),
            ("a shell".into(), AddShell),
            ("d delete".into(), DeleteFeature),
            ("? help".into(), Help),
        ],
        Sel::Feature(..) => vec![
            ("n feature".into(), NewFeature),
            ("a shell".into(), AddShell),
            ("d delete".into(), DeleteFeature),
            ("? help".into(), Help),
        ],
        Sel::Shell(..) => vec![
            ("↵ attach".into(), Attach),
            ("s send".into(), QuickSend),
            ("p predefined".into(), PredefinedSend),
            ("d close".into(), DeleteShell),
            ("? help".into(), Help),
        ],
    }
}

/// The statusbar: the contextual shortcuts, and nothing else. No app
/// name, no session, no project count, no sticky message — anything that
/// is not actionable is noise on a row the user reads constantly.
/// Notices live in [`draw_toast`] and fade by themselves.
fn draw_statusbar(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme.colors;
    // Each entry gets a hit range so a mouse click can run it. Truncated
    // (never wrapped) on narrow frames — the tail just drops off.
    let entries = bar_entries(app);
    let sp = sep(app.ascii);
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    let mut hits: Vec<(u16, u16, BarAction)> = Vec::new();
    let mut cx: u16 = 1;
    for (i, (label, action)) in entries.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(sp, fg(theme.dim)));
            cx += sp.chars().count() as u16;
        }
        let start = cx;
        cx += label.chars().count() as u16;
        spans.push(Span::styled(label.clone(), fg(theme.foreground)));
        hits.push((start, cx, *action));
    }
    app.bar_hits = hits;
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    if app.ascii {
        asciify(f, area);
    }
}

/// Cut a notice to `max` columns, marking the cut with an ellipsis.
fn truncate(text: &str, max: usize, ascii: bool) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mark = if ascii { "..." } else { "…" };
    let keep = max.saturating_sub(mark.chars().count());
    text.chars().take(keep).collect::<String>() + mark
}

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
    let color = if toast.error {
        theme.status_failed
    } else {
        theme.accent
    };
    // Fit inside the frame: the text, plus borders and one space either
    // side, capped at two thirds of the width. It floats one cell inside
    // the panel below it, so no panel border is left half-covered.
    let max_text = (area.width.saturating_sub(6)).min(area.width * 2 / 3) as usize;
    if max_text < 8 || area.height < 5 {
        return;
    }
    let text = truncate(&toast.text, max_text, app.ascii);
    let w = text.chars().count() as u16 + 4;
    let rect = Rect {
        x: area.right().saturating_sub(w + 2),
        y: area.bottom().saturating_sub(4),
        width: w,
        height: 3,
    };
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if app.ascii {
            BorderType::Plain
        } else {
            BorderType::Rounded
        })
        .border_style(fg(color))
        .style(on(theme.title_fg, theme.title_bg));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {text} "), fg(color)))).block(block),
        rect,
    );
    if app.ascii {
        asciify(f, rect);
    }
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

fn draw_help(f: &mut Frame, app: &App) -> Rect {
    let theme = &app.theme.colors;
    // Grouped by context (discoverability rule): the overlay is the app's
    // palette, so every action appears grouped — navigate / projects /
    // features / shells / global — with the same words as the statusbar.
    // The popup sizes itself to the content (bounded by the frame); long
    // content scrolls (j/k, arrows, PgUp/PgDn, ctrl-u/d, Home/End) and the
    // title advertises it.
    let lines = help_lines(app);
    let h = (lines.len() as u16 + 2).min(f.area().height);
    let w = 74u16.min(f.area().width);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(w)) / 2,
        f.area().y + (f.area().height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    let inner_h = h.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_h);
    let scroll = app.help_scroll.min(max_scroll);
    let window: Vec<Line> = lines.iter().skip(scroll).take(inner_h).cloned().collect();
    let mut title_text = " Help ".to_string();
    if max_scroll > 0 {
        // The separator follows the ASCII fallback like every other label.
        if app.ascii {
            title_text.push_str("- j/k scrolls");
        } else {
            title_text.push_str("· j/k scrolls");
        }
    }
    let block = popup_block(theme, title_text);
    f.render_widget(Paragraph::new(Text::from(window)).block(block), area);
    if app.ascii {
        asciify(f, area);
    }
    area
}

/// One help row: the key spellings (joined with the app's separator) and
/// the description parts (same words as the statusbar).
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
            (&["l", "Enter"], &["expand", "attach"]),
            (&["g/G", "Home/End"], &["first / last item"]),
            (&["PgUp/PgDn", "ctrl-u/d"], &["half page"]),
            (&["ctrl-f/b"], &["full page"]),
        ],
    ),
    (
        "projects",
        &[(&["n"], &["new feature", "worktree + agent shell"])],
    ),
    (
        "features",
        &[
            (&["n"], &["new feature"]),
            (&["a"], &["add shell", "prompts for the command"]),
            (&["Enter"], &["attach", "one-shell feature"]),
            (
                &["s", "p"],
                &["quick-send / predefined", "one-shell feature"],
            ),
            (
                &["d"],
                &["delete feature", "kills windows", "removes worktree"],
            ),
        ],
    ),
    (
        "shells",
        &[
            (&["Enter"], &["attach to shell"]),
            (&["s"], &["quick-send", "ctrl-s sends"]),
            (&["p"], &["predefined message", "Enter sends", "1-9 pick"]),
            (
                &["d"],
                &["close shell", "kills the window", "feature stays"],
            ),
        ],
    ),
    (
        "global",
        &[
            (&["r"], &["refresh", "reconcile state"]),
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
fn help_lines(app: &App) -> Vec<Line<'_>> {
    let theme = &app.theme.colors;
    let s = sep(app.ascii);
    let mut lines: Vec<Line> = Vec::new();
    for (group_title, rows) in HELP_GROUPS {
        lines.push(Line::from(Span::styled(
            format!(" {} ", group_title),
            fg(theme.dim).add_modifier(Modifier::BOLD),
        )));
        for (keys, desc) in *rows {
            let key_str = keys.join(s);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {key_str:<24} "),
                    fg(theme.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(desc.join(s), fg(theme.foreground)),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        format!(
            "mouse: click selects{s}double-click opens{s}wheel scrolls{s}bar entries clickable"
        ),
        fg(theme.dim),
    )));
    lines.push(Line::from(""));
    // The way back differs by where the dashboard runs: inside tmux the
    // back-key jumps to this window; standalone there is no back-key — the
    // user detaches from `tmux attach` with C-b d and lands back here.
    if app.tmux.as_ref().and_then(|t| t.inside.as_ref()).is_some() {
        lines.push(Line::from(Span::styled(
            "attach: press Enter in the dashboard, then the tmux prefix + back-key",
            fg(theme.dim),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "back chord is <prefix>+'{}' (config tmux.back_key) — jump back to this window",
                app.cfg.tmux.back_key
            ),
            fg(theme.dim),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "attach: press Enter — the terminal is handed over to tmux attach",
            fg(theme.dim),
        )));
        lines.push(Line::from(Span::styled(
            "detach with C-b d to return to the dashboard",
            fg(theme.dim),
        )));
    }
    lines
}

/// Total help line count — the app clamps its scroll offset against this
/// (same construction as the renderer, so they cannot drift).
pub(crate) fn help_line_count(app: &App) -> usize {
    help_lines(app).len()
}

/// The startup diagnostics overlay: one line per dropped config unit, with
/// the same motions and sizing as the help palette (scrolls, dismisses).
/// Shown once at startup when `Config::diagnostics` is non-empty.
fn draw_diagnostics(f: &mut Frame, app: &App) -> Rect {
    let theme = &app.theme.colors;
    let lines = diag_lines(app);
    let h = (lines.len() as u16 + 2).min(f.area().height);
    let w = 74u16.min(f.area().width);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(w)) / 2,
        f.area().y + (f.area().height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    let inner_h = h.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_h);
    let scroll = app.diag_scroll.min(max_scroll);
    let window: Vec<Line> = lines.iter().skip(scroll).take(inner_h).cloned().collect();
    let mut title_text = format!(" Startup Diagnostics ({}) ", app.cfg.diagnostics.len());
    if max_scroll > 0 {
        if app.ascii {
            title_text.push_str("- j/k scrolls");
        } else {
            title_text.push_str("· j/k scrolls");
        }
    }
    let block = popup_block(theme, title_text);
    f.render_widget(
        Paragraph::new(Text::from(window))
            .block(block)
            .style(on(theme.popup_fg, theme.popup_bg)),
        area,
    );
    if app.ascii {
        asciify(f, area);
    }
    area
}

/// The diagnostics overlay lines — single source for rendering and the
/// app's scroll clamp (same pattern as the help palette).
fn diag_lines(app: &App) -> Vec<Line<'_>> {
    let theme = &app.theme.colors;
    let mark = warn_mark(app.ascii);
    let mut lines: Vec<Line> = app
        .cfg
        .diagnostics
        .iter()
        .map(|d| {
            Line::from(vec![
                Span::styled(
                    format!(" {mark} "),
                    fg(theme.status_failed).add_modifier(Modifier::BOLD),
                ),
                Span::styled(d.clone(), fg(theme.foreground)),
            ])
        })
        .collect();
    lines.push(Line::from(Span::styled("Esc/q dismisses", fg(theme.dim))));
    lines
}

/// Total diagnostics content height (rendered lines + footer) — the app
/// clamps its scroll against this; same construction as the renderer.
pub(crate) fn diag_line_count(app: &App) -> usize {
    diag_lines(app).len()
}

/// The live theme picker (`ctrl-t`): one row per theme, built-ins first.
/// Moving the cursor previews live (the whole screen repaints with the new
/// palette via `App::preview_picker_theme`); Enter keeps, Esc restores.
fn draw_theme_picker(f: &mut Frame, app: &App) -> Rect {
    let theme = &app.theme.colors;
    let names: Vec<&str> = app.theme_names.iter().map(String::as_str).collect();
    let index = match &app.mode {
        Mode::ThemePicker { index } => *index,
        _ => 0,
    };
    // Size to content (bounded by the frame), like the help overlay.
    let w = names
        .iter()
        .map(|n| n.chars().count() as u16)
        .max()
        .unwrap_or(20)
        .saturating_add(8)
        .max(34)
        .min(f.area().width);
    let h = (names.len() as u16 + 3).min(f.area().height);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(w)) / 2,
        f.area().y + (f.area().height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let rows = inner.height.saturating_sub(1) as usize; // last line = footer
    let mut lines: Vec<Line> = Vec::new();
    for (i, name) in names.iter().take(rows).enumerate() {
        let selected = i == index;
        let marker = if app.ascii { ">" } else { "▸" };
        let label = if selected {
            format!(" {marker} {name} ")
        } else {
            format!("   {name} ")
        };
        lines.push(Line::from(if selected {
            vec![Span::styled(
                label,
                on(theme.selection_fg, theme.selection_bg).add_modifier(Modifier::BOLD),
            )]
        } else {
            vec![Span::styled(label, fg(theme.foreground))]
        }));
    }
    let footer = format!(
        " Enter keep{}Esc cancel ",
        if app.ascii { "-" } else { " · " }
    );
    lines.push(Line::from(Span::styled(footer, fg(theme.dim))));
    let block = popup_block(theme, " Themes ");
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    if app.ascii {
        asciify(f, area);
    }
    area
}

/// The predefined-message modal (`p`): the configured presets as a
/// numbered list. Same shape as the theme picker — content-sized, cursor
/// on the selection, keys in the footer.
fn draw_messages(f: &mut Frame, app: &App) -> Rect {
    let theme = &app.theme.colors;
    let index = match &app.mode {
        Mode::Messages { index } => *index,
        _ => 0,
    };
    let labels: Vec<&str> = app.cfg.messages.iter().map(|m| m.label.as_str()).collect();
    let w = labels
        .iter()
        .map(|l| l.chars().count() as u16)
        .max()
        .unwrap_or(20)
        .saturating_add(10)
        .max(34)
        .min(f.area().width);
    let h = (labels.len() as u16 + 3).min(f.area().height);
    let area = Rect::new(
        f.area().x + (f.area().width.saturating_sub(w)) / 2,
        f.area().y + (f.area().height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let rows = inner.height.saturating_sub(1) as usize; // last line = footer
    let mut lines: Vec<Line> = Vec::new();
    for (i, label) in labels.iter().take(rows).enumerate() {
        let selected = i == index;
        let marker = if app.ascii { ">" } else { "▸" };
        // The number is the key that sends the entry (up to 9 of them).
        let num = if i < 9 {
            format!("{} ", i + 1)
        } else {
            "  ".into()
        };
        let label = if selected {
            format!(" {marker} {num}{label} ")
        } else {
            format!("   {num}{label} ")
        };
        lines.push(Line::from(if selected {
            vec![Span::styled(
                label,
                on(theme.selection_fg, theme.selection_bg).add_modifier(Modifier::BOLD),
            )]
        } else {
            vec![Span::styled(label, fg(theme.foreground))]
        }));
    }
    let footer = format!(" Enter send{}Esc cancel ", sep(app.ascii));
    lines.push(Line::from(Span::styled(footer, fg(theme.dim))));
    let block = popup_block(theme, " Messages ");
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    if app.ascii {
        asciify(f, area);
    }
    area
}

/// Popup geometry for the input prompt: 60% of the frame wide and exactly
/// as tall as its content (`lines` plus the two border rows). A fixed
/// height percentage used to clip the footer off short frames.
fn input_rect(area: Rect, lines: u16) -> Rect {
    centered_rect_wh(area, area.width * 60 / 100, lines + 2)
}

fn draw_input(f: &mut Frame, app: &App) -> Rect {
    let theme = &app.theme.colors;
    let (title, label) = match &app.mode {
        Mode::Input { kind, .. } => match kind {
            InputKind::NewFeature => ("New Feature", "feature name: "),
            InputKind::AddShell => ("Add Shell", "command: "),
            InputKind::QuickSend => ("Quick Send", "send to agent: "),
        },
        _ => unreachable!(),
    };
    let Mode::Input { kind, text } = &app.mode else {
        return input_rect(f.area(), 4);
    };
    let block = popup_block(theme, format!(" {title} "));
    let mut lines = vec![Line::from("")];
    lines.push(Line::from(vec![
        Span::styled(label, fg(theme.accent)),
        Span::raw(text.clone()),
    ]));
    // add-shell only: an empty submit runs the project's default command,
    // so name it right under the field instead of making it folklore.
    if *kind == InputKind::AddShell
        && let Some(def) = app.add_shell_default()
    {
        lines.push(Line::from(Span::styled(
            format!("default: {def}"),
            fg(theme.dim),
        )));
    }
    // new-feature only: the worktree is cut from the project's base
    // branch, NOT from whatever the repo has checked out. Name the branch
    // so nobody has to guess which one it is.
    if *kind == InputKind::NewFeature
        && let Some(hint) = app.new_feature_base()
    {
        lines.push(Line::from(Span::styled(
            format!("branches from: {} (base branch)", hint.branch),
            fg(theme.dim),
        )));
        if let Some(configured) = &hint.missing {
            lines.push(Line::from(Span::styled(
                format!("base branch '{configured}' not found — using current branch"),
                fg(theme.status_waiting),
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Enter to submit{}Esc to cancel", sep(app.ascii)),
        fg(theme.dim),
    )));
    let area = input_rect(f.area(), lines.len() as u16);
    f.render_widget(Clear, area);
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let popup_style = on(theme.popup_fg, theme.popup_bg);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .style(popup_style),
        area,
    );
    if app.ascii {
        asciify(f, area);
    }
    // cursor after the typed text, on the input row (the second content
    // line below the border: one spacer line, then label + text). Only
    // when the popup fits the frame — a cursor off the buffer would
    // corrupt the terminal.
    let cursor_x = inner.x + label.chars().count() as u16 + text.chars().count() as u16;
    let cursor_y = inner.y + 1;
    if cursor_x < area.x + area.width && cursor_y < area.y + area.height {
        f.set_cursor_position(Position::new(cursor_x, cursor_y));
    }
    area
}

fn draw_textarea(f: &mut Frame, app: &App) -> Rect {
    let theme = &app.theme.colors;
    // Span the popup to the textarea geometry (shared with the key
    // handler's page-size math via app::textarea_visible_lines).
    let w = crate::app::textarea_width(app.viewport.0).min(f.area().width);
    let h = crate::app::textarea_height(app.viewport.1).min(f.area().height);
    let area = centered_rect_wh(f.area(), w, h);
    f.render_widget(Clear, area);
    let Mode::Textarea { buf } = &app.mode else {
        return area;
    };
    let content_lines = crate::app::textarea_visible_lines(app.viewport.1).max(1);
    let block = popup_block(theme, " Quick Send ");
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
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
    // Spacer, then the hint row.
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("enter newline", fg(theme.accent)),
        Span::styled(sep(app.ascii), fg(theme.dim)),
        Span::styled("ctrl-s send", fg(theme.accent)),
        Span::styled(sep(app.ascii), fg(theme.dim)),
        Span::styled("esc cancel", fg(theme.accent)),
    ]));

    let popup_style = on(theme.popup_fg, theme.popup_bg);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .style(popup_style),
        area,
    );
    if app.ascii {
        asciify(f, area);
    }
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
    area
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

fn draw_confirm(f: &mut Frame, app: &App) -> Rect {
    let theme = &app.theme.colors;
    let Mode::Confirm { confirm } = &app.mode else {
        return Rect::default();
    };
    let area = centered_rect(f.area(), 62, 26);
    f.render_widget(Clear, area);
    let sp = sep(app.ascii);
    let options = match &confirm.force_label {
        Some(fl) => format!(" [y/d] yes{sp}[f] {fl}{sp}[n] cancel "),
        None => format!(" [y/d] yes{sp}[n] cancel "),
    };
    let mut lines: Vec<Line> = confirm
        .body
        .lines()
        .map(|l| Line::from(Span::raw(l.to_string())))
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        options,
        fg(theme.accent).add_modifier(Modifier::BOLD),
    )));
    let block = popup_block(theme, format!(" {} ", confirm.title));
    let popup_style = on(theme.popup_fg, theme.popup_bg);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .style(popup_style),
        area,
    );
    if app.ascii {
        asciify(f, area);
    }
    area
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
                kind: InputKind::NewFeature,
                text: "some-feature".into(),
            },
            Mode::Confirm {
                confirm: Confirm {
                    title: "delete feature".into(),
                    body: "delete 'x'?\nworktree is dirty — force will discard".into(),
                    force_label: Some("force".into()),
                    action: ConfirmAction::DeleteFeature {
                        project: 0,
                        feature: 0,
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

    /// A missing project renders with a dimmed "(missing)" marker in the
    /// tree — visibility without pretending the repo is there.
    #[test]
    fn missing_project_renders_dimmed_marker() {
        let cfg = Config::default();
        let theme = theme::load("ansi", false).unwrap();
        let mut app = App::new(cfg, theme, None);
        // No `startup()` — see `seeded_app`: it would load and rewrite the
        // real `state.json`.
        app.state.projects.push(crate::state::Project {
            name: "gone".into(),
            path: std::path::PathBuf::from("/gone"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: true,
            features: vec![],
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

    /// The input popup's block cursor must sit on the input text row, right
    /// after the typed text. Regression: it used to be drawn one row low
    /// (area.y + 3 instead of area.y + 2), making the caret appear below
    /// the text in every input popup (new feature, quick-send, import).
    #[test]
    fn input_popup_cursor_sits_on_input_row() {
        // seeded: the add-shell hint needs a project to name, and it adds
        // a content line, which moves the (content-sized) popup.
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Project(0);

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        for (kind, label, text, lines) in [
            (InputKind::NewFeature, "feature name: ", "foo", 4),
            // + the `default: <cmd>` hint line
            (InputKind::AddShell, "command: ", "ls -la", 5),
            (InputKind::QuickSend, "send to agent: ", "busy 2", 4),
        ] {
            app.mode = Mode::Input {
                kind,
                text: text.into(),
            };
            terminal
                .draw(|f| draw(f, &mut app))
                .expect("draw must not fail");

            // popup area for this backend size, same math as draw_input
            let area = input_rect(terminal.size().unwrap().into(), lines);
            let expected_x =
                area.x + 1 + label.chars().count() as u16 + text.chars().count() as u16;
            let expected_y = area.y + 2; // input row = second content line below the border
            assert_eq!(
                terminal.get_cursor_position().unwrap(),
                Position::new(expected_x, expected_y),
                "cursor must sit on the input row after the typed text ({label:?})",
            );
        }
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
            crate::app::textarea_visible_lines(40),
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
        assert!(text.contains("ctrl-s send"), "hint names the send key");
    }

    /// The popup wraps a line too long for its width instead of letting
    /// it run past the border, drops the old "send to agent (N rows)"
    /// label, and keeps a blank row between the text and the hint.
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
            crate::app::textarea_visible_lines(40),
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
        for y in y0..y0 + h {
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

        // The wrapped text really is spread over several rows, and the
        // row above the hint is blank.
        let hint_y = (y0 + h - 2) as usize;
        assert!(rows[hint_y].contains("ctrl-s send"), "hint on the last row");
        let spacer: String = rows[hint_y - 1]
            .chars()
            .skip(x0 as usize + 1)
            .take(w as usize - 2)
            .collect();
        assert!(
            spacer.trim().is_empty(),
            "a spacer separates the text from the hint, got {spacer:?}"
        );
        let filled = rows[(y0 + 1) as usize..hint_y - 1]
            .iter()
            .filter(|r| r.contains("wrap"))
            .count();
        assert!(filled > 1, "the long line wrapped over {filled} rows");
    }

    /// The New Feature popup spells out where the branch comes from: the
    /// project's base branch, not the branch the repo has checked out. The
    /// hint is the one place a user can see it before hitting Enter.
    #[test]
    fn new_feature_popup_states_the_base_branch() {
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
            app.state.projects.push(crate::state::Project {
                name: "repo".into(),
                path: repo.clone(),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: None,
                missing: false,
                features: vec![],
            });
            app.build_tree_flags();
            app.clamp_sel();
            app.sel = crate::app::Sel::Project(0);
            app.open_new_feature();

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
                "the checked-out branch is not what a feature branches from: {text:?}"
            );

            // configured base missing from the repo: creation falls back to the
            // checkout, and the popup warns instead of naming a phantom base.
            app.state.projects[0].base_branch = "master".into();
            app.open_new_feature();
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
        // the fixture would inherit the developer's projects (dozens of
        // them, past the unfold cap these render tests rely on) and race
        // with every other test doing the same.
        let mut app = App::new(cfg, theme, None);
        app.state.projects.push(crate::state::Project {
            name: "repo-a".into(),
            path: std::path::PathBuf::from("/tmp/repo-a"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features: vec![],
        });
        // A second project keeps the fixture out of flatten mode (a lone
        // project's features would become the top level, hiding the rows
        // these render tests assert on).
        app.state.projects.push(crate::state::Project {
            name: "repo-b".into(),
            path: std::path::PathBuf::from("/tmp/repo-b"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features: vec![],
        });
        app.build_tree_flags();
        app.clamp_sel();
        app
    }

    fn buffer_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| buf[(x, y)].symbol()))
            .collect()
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
            wide.contains("Projects"),
            "the wide tier still shows the tree"
        );

        let split = at(50, 10);
        assert!(
            split.contains("Preview"),
            "the narrow split tier keeps the preview"
        );

        let tree_only = at(30, 8);
        assert!(
            tree_only.contains("Projects"),
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
    fn bare_app(features: Vec<crate::state::Feature>) -> App {
        let mut app = App::new(Config::default(), theme::load("ansi", false).unwrap(), None);
        app.state.projects.push(crate::state::Project {
            name: "repo-a".into(),
            path: std::path::PathBuf::from("/tmp/repo-a"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features,
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
            base.contains("Projects (1)"),
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
                    kind: InputKind::NewFeature,
                    text: String::new(),
                },
                "New Feature",
            ),
            (
                Mode::Input {
                    kind: InputKind::AddShell,
                    text: String::new(),
                },
                "Add Shell",
            ),
        ] {
            app.mode = mode;
            let text = render_wide(&mut app);
            assert!(text.contains(want), "overlay title {want:?}: {text:?}");
        }
    }

    /// A single git repo flattens to its features, so the tree names what
    /// it actually lists: `Features (n)`, not `Projects`.
    #[test]
    fn tree_title_says_features_when_flattened() {
        let mut app = bare_app(vec![crate::state::Feature {
            name: "feat".into(),
            branch: "feat".into(),
            worktree_path: std::path::PathBuf::from("/tmp/repo-a/feat"),
            created_at: String::new(),
            shells: vec![],
        }]);
        assert!(app.flatten, "one project with features flattens");
        let text = render_wide(&mut app);
        assert!(
            text.contains("Features (1)"),
            "flattened tree title: {text:?}"
        );
        assert!(!text.contains("Projects"), "no project wording: {text:?}");
    }

    /// An overlay on a frame too small for it is refused with a message,
    /// not drawn as garbage.
    #[test]
    fn overlay_refused_on_tiny_frame() {
        let mut app = seeded_app();
        app.mode = Mode::Help;
        let backend = ratatui::backend::TestBackend::new(30, 8);
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
                kind: InputKind::NewFeature,
                text: "some-feature".into(),
            },
            Mode::Confirm {
                confirm: Confirm {
                    title: "delete feature".into(),
                    body: "delete 'x'?".into(),
                    force_label: None,
                    action: ConfirmAction::DeleteFeature {
                        project: 0,
                        feature: 0,
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

    /// ASCII fallback: no box-drawing, no unicode markers — plain `+ - |`
    /// and `v`/`>` — so terminals that mangle box-drawing stay readable.
    #[test]
    fn ascii_fallback_renders_without_box_glyphs() {
        let mut app = seeded_app();
        app.ascii = true;
        app.state.projects.push(crate::state::Project {
            name: "repo-b".into(),
            path: std::path::PathBuf::from("/tmp/repo-b"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features: vec![],
        });
        app.build_tree_flags();
        app.clamp_sel();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        for glyph in ["─", "│", "┌", "▾", "▸", "›", "·", "→"] {
            assert!(
                !text.contains(glyph),
                "ascii render must not contain {glyph:?}: {text:?}"
            );
        }
        for glyph in ["+", "-", "|", "v", ">"] {
            assert!(text.contains(glyph), "ascii render must use {glyph:?}");
        }
    }

    /// In unicode mode the box-drawing tree markers are present — the
    /// fallback must not silently replace them.
    #[test]
    fn unicode_mode_keeps_box_drawing() {
        let mut app = seeded_app();
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("─"), "unicode render keeps box-drawing");
        assert!(text.contains("▾"), "unicode render keeps the tree markers");
    }

    /// tree_width matches the renderer's tier math (used by both drawing
    /// and mouse hit-testing — they must never drift).
    #[test]
    fn tree_width_follows_tier_math() {
        assert_eq!(tree_width((120, 40)), Some(38)); // full: 32%
        assert_eq!(tree_width((50, 10)), Some(21)); // no-preview: 42%
        assert_eq!(tree_width((400, 60)), Some(48)); // wide screen: capped
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
        let text = help_text(&mut app, 120, 40);
        for header in ["navigate", "projects", "features", "shells", "global"] {
            assert!(text.contains(header), "group header {header:?} missing");
        }
        // every action is listed (palette = the index of everything)
        for needle in [
            "move selection",
            "collapse",
            "expand",
            "attach",
            "new feature",
            "add shell",
            "delete feature",
            "close shell",
            "quick-send",
            "refresh",
            "this help",
        ] {
            assert!(text.contains(needle), "action {needle:?} missing from help");
        }
        assert!(
            app.popup_area.is_some(),
            "help overlay must register its dismissable rect"
        );
    }

    /// `I scan` / `i import` are gone from the project row: discovery is a
    /// boot-time concern, so the row offers only what still exists.
    #[test]
    fn the_project_bar_offers_no_scan_or_import() {
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Project(0);
        let labels: Vec<String> = crate::ui::bar_entries(&app)
            .into_iter()
            .map(|(l, _)| l)
            .collect();
        assert!(
            !labels
                .iter()
                .any(|l| l.contains("scan") || l.contains("import")),
            "bar: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("feature")),
            "bar keeps `n feature`: {labels:?}"
        );
    }

    /// The statusbar entries and the help overlay share their words — the
    /// discoverability rule that makes learning one teach the other.
    #[test]
    fn bar_entries_and_help_share_wording() {
        // For every bar label, the action's key word appears in the help
        // overlay (bar: "n feature" vs help: "new feature", etc.).
        let mut app = seeded_app();
        let text = help_text(&mut app, 120, 40);
        let app2 = seeded_app();
        for (label, _) in crate::ui::bar_entries(&app2) {
            let word = label
                .split(' ')
                .nth(1)
                .or_else(|| label.split(' ').next())
                .unwrap_or(label.as_str());
            if word == "help" {
                assert!(text.contains("this help"), "bar '? help' ↔ help row");
            } else if word == "send" {
                assert!(text.contains("quick-send"), "bar 's send' ↔ help row");
            } else if word == "predefined" {
                assert!(
                    text.contains("predefined message"),
                    "bar 'p predefined' ↔ help row"
                );
            } else if word == "feature" {
                assert!(text.contains("new feature"), "bar 'n feature' ↔ help");
            } else if word == "shell" {
                assert!(text.contains("add shell"), "bar 'a shell' ↔ help");
            } else if word == "delete" {
                assert!(text.contains("delete feature"), "bar 'd delete' ↔ help");
            } else if word == "attach" {
                assert!(text.contains("attach"), "bar '↵ attach' ↔ help");
            } else if word == "close" {
                assert!(text.contains("close shell"), "bar 'd close' ↔ help");
            } else {
                panic!("unmapped bar label: {label:?}");
            }
        }
    }

    /// The statusbar carries the shortcuts and nothing else: no app name,
    /// no session, no project count, no sticky notice. Anything the user
    /// cannot act on is noise on the row they read constantly.
    #[test]
    fn statusbar_is_shortcuts_only() {
        let mut app = seeded_app();
        app.notify("feature 'foo' created");
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let last: String = (0..buf.area.width)
            .map(|x| buf[(x, buf.area.height - 1)].symbol())
            .collect();

        assert!(!last.contains("lazywingman"), "no app name: {last:?}");
        assert!(!last.contains("project(s)"), "no project count: {last:?}");
        assert!(!last.contains("standalone"), "no session chip: {last:?}");
        assert!(
            !last.contains("created"),
            "notices are not on the bar: {last:?}"
        );
        for (label, _) in bar_entries(&app) {
            assert!(last.contains(&label), "bar shows {label:?}: {last:?}");
        }
    }

    /// A notice is a toast above the bar, and it is gone once it expires —
    /// a message that never leaves is exactly the clutter the bar shed.
    #[test]
    fn toast_shows_then_expires() {
        let mut app = seeded_app();
        app.notify("feature 'foo' created");
        let render = |app: &mut App| {
            let backend = ratatui::backend::TestBackend::new(120, 40);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            buffer_text(&terminal)
        };
        assert!(
            render(&mut app).contains("feature 'foo' created"),
            "a fresh notice is drawn as a toast"
        );
        app.expire_toast_for_test();
        assert!(
            !render(&mut app).contains("feature 'foo' created"),
            "the toast is gone once its TTL has passed"
        );
    }

    /// The feature icon carries the worktree state: hollow when clean,
    /// filled when there are uncommitted changes.
    #[test]
    fn dirty_feature_gets_a_filled_icon() {
        let mut app = seeded_app();
        app.state.projects[0].features.push(crate::state::Feature {
            name: "clean-one".into(),
            branch: "clean-one".into(),
            worktree_path: std::path::PathBuf::from("/tmp/wt-clean"),
            created_at: String::new(),
            shells: vec![],
        });
        app.state.projects[0].features.push(crate::state::Feature {
            name: "dirty-one".into(),
            branch: "dirty-one".into(),
            worktree_path: std::path::PathBuf::from("/tmp/wt-dirty"),
            created_at: String::new(),
            shells: vec![],
        });
        app.dirty
            .insert(std::path::PathBuf::from("/tmp/wt-dirty"), true);
        app.build_tree_flags();
        app.proj_expanded[0] = true;

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let clean = rows.iter().find(|r| r.contains("clean-one")).unwrap();
        let dirty = rows.iter().find(|r| r.contains("dirty-one")).unwrap();
        assert!(clean.contains("◇ clean-one"), "clean feature: {clean:?}");
        assert!(dirty.contains("◆ dirty-one"), "dirty feature: {dirty:?}");
    }

    /// The tree folds the nominal case: a feature with one shell shows
    /// that shell's status on its own row and gets no child row, while a
    /// feature with several just gets its expanded subtree. No row carries
    /// a shell count (the shells are right below) and none carries the
    /// tmux window id — that is plumbing, not information.
    #[test]
    fn solo_feature_row_carries_the_status_and_no_child_row() {
        let mut app = seeded_app();
        let shell = |i: usize| crate::state::Shell {
            index: i,
            window_name: format!("lw:f/{i}"),
            window_id: Some(format!("@{}", i + 7)),
            pane_id: Some(format!("%{i}")),
        };
        app.state.projects[0].features.push(crate::state::Feature {
            name: "many-shells".into(),
            branch: "many-shells".into(),
            worktree_path: std::path::PathBuf::from("/tmp/wt-many"),
            created_at: String::new(),
            shells: vec![shell(0), shell(1)],
        });
        app.state.projects[0].features.push(crate::state::Feature {
            name: "one-shell".into(),
            branch: "one-shell".into(),
            worktree_path: std::path::PathBuf::from("/tmp/wt-one"),
            created_at: String::new(),
            shells: vec![shell(2)],
        });
        app.statuses.insert("%0".into(), Status::Working);
        app.statuses.insert("%1".into(), Status::Working);
        app.statuses.insert("%2".into(), Status::Waiting);
        app.build_tree_flags();
        app.proj_expanded[0] = true;
        let n = app.state.projects[0].features.len();
        app.feat_expanded[0][n - 2] = true; // the many-shell feature is open

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rows = buffer_rows(&terminal);

        let solo = rows.iter().find(|r| r.contains("one-shell")).unwrap();
        assert!(
            solo.contains(Status::Waiting.label()),
            "a folded feature shows its shell's status: {solo:?}"
        );
        assert!(
            !solo.contains("shell]"),
            "a folded feature drops the shell count: {solo:?}"
        );
        let many = rows.iter().find(|r| r.contains("many-shells")).unwrap();
        assert!(
            !many.contains("shells]"),
            "a multi-shell feature carries no count either: {many:?}"
        );
        // the folded feature contributes no `#2` row; the open one does
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

    /// Project rows carry no feature count — the tree is expanded by
    /// default, so the features themselves are the count. What the row
    /// cannot show otherwise is whether a folded project holds anything
    /// at all, and the icon says it: filled when it has features, hollow
    /// when it is empty.
    #[test]
    fn project_rows_swap_the_count_for_a_full_or_empty_icon() {
        let mut app = seeded_app();
        app.state.projects[0].features.push(crate::state::Feature {
            name: "only-feature".into(),
            branch: "only-feature".into(),
            worktree_path: std::path::PathBuf::from("/tmp/wt-one"),
            created_at: String::new(),
            shells: vec![crate::state::Shell {
                index: 0,
                window_name: "lw:f/0".into(),
                window_id: Some("@7".into()),
                pane_id: Some("%0".into()),
            }],
        });
        app.build_tree_flags();
        app.proj_expanded[0] = true;

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rows = buffer_rows(&terminal);

        let full = rows.iter().find(|r| r.contains("repo-a")).unwrap();
        let empty = rows.iter().find(|r| r.contains("repo-b")).unwrap();
        for row in [full, empty] {
            assert!(!row.contains("feature)"), "no count on a row: {row:?}");
            assert!(!row.contains("features)"), "no count on a row: {row:?}");
        }
        assert!(
            full.contains("▣ repo-a"),
            "a project with features: {full:?}"
        );
        assert!(empty.contains("▢ repo-b"), "an empty project: {empty:?}");
    }

    /// A dead entry is muted *whole*: once every shell under it has
    /// exited, the row's guide, icon, name and status all take the dead
    /// colour instead of only the status word, so the eye skips a feature
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
        app.state.projects[0].features.push(crate::state::Feature {
            name: "gone-one".into(),
            branch: "gone-one".into(),
            worktree_path: std::path::PathBuf::from("/tmp/wt-gone"),
            created_at: String::new(),
            shells: vec![shell(0)],
        });
        app.state.projects[0].features.push(crate::state::Feature {
            name: "half-gone".into(),
            branch: "half-gone".into(),
            worktree_path: std::path::PathBuf::from("/tmp/wt-half"),
            created_at: String::new(),
            shells: vec![shell(1), shell(2)],
        });
        app.statuses.insert("%0".into(), Status::Dead);
        app.statuses.insert("%1".into(), Status::Dead);
        app.statuses.insert("%2".into(), Status::Working);
        app.build_tree_flags();
        app.proj_expanded[0] = true;
        let n = app.state.projects[0].features.len();
        app.feat_expanded[0][n - 1] = true; // the mixed feature is open
        app.sel = Sel::Project(1); // keep the highlight off the rows asserted on

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
            "a solo dead feature is muted whole: {text:?} {colors:?}"
        );
        // The shell row of a dead shell is muted the same way...
        let (text, colors) = row(" #1");
        assert!(
            colors.iter().all(|c| *c == dead),
            "a dead shell row is muted whole: {text:?} {colors:?}"
        );
        // ...while its live sibling, and the feature holding it, keep
        // their normal colours.
        let (text, colors) = row(" #2");
        assert!(
            colors.iter().any(|c| *c != dead),
            "a live shell row is not muted: {text:?} {colors:?}"
        );
        let (text, colors) = row("half-gone");
        assert!(
            colors.iter().any(|c| *c != dead),
            "a feature with one live shell is not muted: {text:?} {colors:?}"
        );
    }

    /// The help overlay scrolls: on a short frame the title advertises
    /// 'j/k scrolls' and the content is clipped to the popup; on a tall
    /// frame everything fits and no scroll hint appears.
    #[test]
    fn help_overlay_scrolls_on_short_frames() {
        let mut app = seeded_app();
        let short = help_text(&mut app, 80, 18);
        assert!(
            short.contains("j/k scrolls"),
            "short frame must advertise scrolling: {short:?}"
        );
        let mut app = seeded_app();
        let tall = help_text(&mut app, 120, 40);
        assert!(
            !tall.contains("j/k scrolls"),
            "tall frame fits everything: {tall:?}"
        );
    }

    /// ASCII mode: the help overlay's separators are plain `-` and no
    /// unicode glyphs survive (the sep()/arrow() code path, not a second
    /// renderer).
    #[test]
    fn help_overlay_ascii_fallback_is_clean() {
        let mut app = seeded_app();
        app.ascii = true;
        let text = help_text(&mut app, 120, 40);
        for glyph in ["·", "→", "─", "│"] {
            assert!(
                !text.contains(glyph),
                "ascii help must not contain {glyph:?}"
            );
        }
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

    /// The picker is ascii-safe: plain `>` cursor, `-` separators, no
    /// unicode glyphs survive the fallback.
    #[test]
    fn theme_picker_ascii_fallback_is_clean() {
        let mut app = seeded_app();
        app.ascii = true;
        app.theme_names = crate::theme::available_names().unwrap();
        app.mode = Mode::ThemePicker { index: 0 };
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        for glyph in ["▸", "·", "─", "│"] {
            assert!(
                !text.contains(glyph),
                "ascii picker must not contain {glyph:?}"
            );
        }
    }

    /// The picker on a tiny frame is refused with the standard message
    /// (never garbage, never a panic).
    #[test]
    fn theme_picker_refused_on_tiny_frame() {
        let mut app = seeded_app();
        app.theme_names = crate::theme::available_names().unwrap();
        app.mode = Mode::ThemePicker { index: 0 };
        let backend = ratatui::backend::TestBackend::new(30, 8);
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
        let visible = app.popup_area.unwrap().height.saturating_sub(2) as usize;
        app.diag_scroll = visible;
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            !text.contains("config problem 0"),
            "scrolled-out diagnostics are gone"
        );
        assert!(text.contains("j/k scrolls"), "scroll hint advertised");
    }

    /// The diagnostics overlay is ascii-safe: `!` mark, no box glyphs.
    #[test]
    fn diagnostics_ascii_fallback_is_clean() {
        let mut app = app_with_diags(2);
        app.ascii = true;
        app.mode = Mode::Diagnostics;
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains('!'), "ascii warn mark");
        for glyph in ["⚠", "·", "─", "│"] {
            assert!(
                !text.contains(glyph),
                "ascii diagnostics must not contain {glyph:?}"
            );
        }
    }

    /// The diagnostics overlay on a tiny frame is refused with the standard
    /// message (never garbage, never a panic).
    #[test]
    fn diagnostics_refused_on_tiny_frame() {
        let mut app = app_with_diags(2);
        app.mode = Mode::Diagnostics;
        let backend = ratatui::backend::TestBackend::new(30, 8);
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
        app.state.projects.push(crate::state::Project {
            name: "repo-a".into(),
            path: std::path::PathBuf::from("/tmp/repo-a"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features: vec![],
        });
        app.state.projects[0].features.push(crate::state::Feature {
            name: "feat".into(),
            branch: "feat".into(),
            worktree_path: std::path::PathBuf::from("/tmp/repo-a-feat"),
            created_at: "2026-01-01T00:00:00Z".into(),
            shells: vec![crate::state::Shell {
                index: 1,
                window_name: "feat-1".into(),
                window_id: Some("@1".into()),
                pane_id: Some("%1".into()),
            }],
        });
        app.build_tree_flags();
        // One shell → the feature row is folded onto it; that row is what
        // the preview follows.
        app.sel = crate::app::Sel::Feature(0, 0);
        app.clamp_sel();
        assert_eq!(
            app.selected_shell(),
            Some((0, 0, 0)),
            "fixture must leave the folded feature's shell selected"
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

    /// `preview_tail` edge cases: a capture shorter than the panel keeps
    /// every line, and a zero-row panel still shows a row.
    #[test]
    fn preview_tail_handles_short_captures_and_tiny_panels() {
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
        assert!(
            text.contains("2 merge + drop worktree"),
            "second entry is numbered, got: {text}"
        );
        assert!(text.contains("Enter send"), "modal states its keys");
        assert!(
            app.popup_area.is_some(),
            "modal must register its dismissable rect"
        );
    }

    /// A frame too small for the modal is refused with a message, never
    /// drawn as garbage.
    #[test]
    fn tiny_frame_refuses_the_messages_modal() {
        let mut app = seeded_app();
        app.mode = Mode::Messages { index: 0 };
        let backend = ratatui::backend::TestBackend::new(30, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("too small"), "tiny frame refuses the modal");
    }

    /// Ascii mode keeps the modal free of unicode glyphs.
    #[test]
    fn messages_modal_ascii_fallback_is_clean() {
        let mut app = seeded_app();
        app.ascii = true;
        app.mode = Mode::Messages { index: 0 };
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.is_ascii(),
            "ascii mode must not emit unicode: {text:?}"
        );
    }

    /// The statusbar advertises `p` on a shell (same words as the help).
    #[test]
    fn statusbar_offers_predefined_on_a_shell() {
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Shell(0, 0, 0);
        let labels: Vec<String> = bar_entries(&app).into_iter().map(|(l, _)| l).collect();
        assert!(
            labels.iter().any(|l| l == "p predefined"),
            "shell shortcuts: {labels:?}"
        );
        assert!(
            bar_entries(&app)
                .iter()
                .any(|(_, a)| *a == BarAction::PredefinedSend),
            "the entry is clickable"
        );
    }

    /// A shell row offers `d close` (the shell), a feature row `d delete`
    /// (the feature) — the bar never offers to delete a feature from a
    /// shell row. No selection offers a project removal any more.
    #[test]
    fn statusbar_delete_entry_is_contextual() {
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Shell(0, 0, 0);
        let entries = bar_entries(&app);
        assert!(
            entries
                .iter()
                .any(|(l, a)| l == "d close" && *a == BarAction::DeleteShell),
            "a shell closes itself: {entries:?}"
        );
        assert!(
            !entries.iter().any(|(_, a)| *a == BarAction::DeleteFeature),
            "a shell row must not offer the feature delete: {entries:?}"
        );

        app.sel = crate::app::Sel::Feature(0, 0);
        let entries = bar_entries(&app);
        assert!(
            entries
                .iter()
                .any(|(l, a)| l == "d delete" && *a == BarAction::DeleteFeature),
            "a feature deletes itself: {entries:?}"
        );

        app.sel = crate::app::Sel::Project(0);
        let labels: Vec<String> = bar_entries(&app).into_iter().map(|(l, _)| l).collect();
        assert!(
            !labels.iter().any(|l| l.contains("remove")),
            "the project removal entry is gone: {labels:?}"
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

    /// The add-shell prompt names the command it will run and shows the
    /// project's default underneath — submitting empty runs that default,
    /// so the user has to be able to see it.
    #[test]
    fn add_shell_popup_shows_the_default_command_hint() {
        let mut app = seeded_app();
        app.state.projects[0].agent_command = "codex exec --yolo".into();
        app.sel = crate::app::Sel::Project(0);
        app.mode = Mode::Input {
            kind: InputKind::AddShell,
            text: String::new(),
        };
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Add Shell"), "popup title: {text}");
        assert!(text.contains("command: "), "command field label: {text}");
        assert!(
            text.contains("default: codex exec --yolo"),
            "the project's default command must be shown as a hint: {text}"
        );
    }

    /// The prompt is sized to its content, so the `Esc to cancel` footer
    /// survives a short frame — the add-shell hint line used to push it out
    /// of a popup whose height was a fixed percentage of the terminal.
    #[test]
    fn input_popup_keeps_its_footer_on_short_frames() {
        let mut app = seeded_app();
        app.sel = crate::app::Sel::Project(0);
        for kind in [InputKind::AddShell, InputKind::NewFeature] {
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
                    text.contains("Esc to cancel"),
                    "{kind:?} popup must keep its footer at height {height}: {text}"
                );
            }
        }
    }
}
