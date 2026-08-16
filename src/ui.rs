//! TUI rendering: tree, detail, preview, status bar, overlays.
//!
//! All colours come from the resolved [`Theme`]; the renderer never hardcodes
//! a colour.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{App, BarAction, InputKind, Mode, Sel};
use crate::status::Status;
use crate::theme::ResolvedColors;

/// Minimum sizes for the layout tiers. Resize is normal, not an edge case:
/// the UI degrades gracefully — preview hides first, then detail, then the
/// bar — and a too-small frame shows a message instead of drawing garbage.
const FULL_MIN: (u16, u16) = (64, 12);
const NO_PREVIEW_MIN: (u16, u16) = (44, 9);
const TREE_MIN: (u16, u16) = (26, 7);

/// Which panels fit in a frame of `(width, height)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    /// Tree + detail + preview + statusbar.
    Full,
    /// Tree + detail + statusbar (preview hidden).
    NoPreview,
    /// Tree + statusbar only.
    TreeOnly,
    /// Too small for the tree — a message instead of the UI.
    Message,
}

pub(crate) fn tier_impl(w: u16, h: u16) -> Tier {
    if w >= FULL_MIN.0 && h >= FULL_MIN.1 {
        Tier::Full
    } else if w >= NO_PREVIEW_MIN.0 && h >= NO_PREVIEW_MIN.1 {
        Tier::NoPreview
    } else if w >= TREE_MIN.0 && h >= TREE_MIN.1 {
        Tier::TreeOnly
    } else {
        Tier::Message
    }
}

/// Width of the tree panel for a viewport, or `None` when the frame is too
/// small for the tree at all. Same math for rendering and mouse hit-testing.
pub(crate) fn tree_width(viewport: (u16, u16)) -> Option<u16> {
    let (w, h) = viewport;
    match tier_impl(w, h) {
        Tier::Full => Some(w * 32 / 100),
        Tier::NoPreview => Some(w * 42 / 100),
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
        OverlayKind::Input | OverlayKind::Confirm => (34, 10),
    }
}

#[derive(Clone, Copy)]
enum OverlayKind {
    Help,
    Input,
    Confirm,
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
        Tier::Full => draw_full(f, area, app),
        Tier::NoPreview => draw_no_preview(f, area, app),
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
        Mode::Input { .. } => {
            let kind = OverlayKind::Input;
            if fits(area, overlay_min(kind)) {
                let rect = draw_input(f, app);
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
pub(crate) fn arrow(ascii: bool) -> &'static str {
    if ascii { "->" } else { "→" }
}

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

/// The full layout: tree | detail / preview, statusbar below.
fn draw_full(f: &mut Frame, area: Rect, app: &mut App) {
    let [main, statusbar] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [tree, right] =
        Layout::horizontal([Constraint::Percentage(32), Constraint::Percentage(68)]).areas(main);
    let [detail, preview] =
        Layout::vertical([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(right);

    draw_tree(f, tree, app);
    draw_detail(f, detail, app);
    draw_preview(f, preview, app);
    draw_statusbar(f, statusbar, app);
}

/// Tree + detail, no preview.
fn draw_no_preview(f: &mut Frame, area: Rect, app: &mut App) {
    let [main, statusbar] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    let [tree, detail] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(main);

    draw_tree(f, tree, app);
    draw_detail(f, detail, app);
    draw_statusbar(f, statusbar, app);
}

/// Tree across the whole main area.
fn draw_tree_only(f: &mut Frame, area: Rect, app: &mut App) {
    let [main, statusbar] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    draw_tree(f, main, app);
    draw_statusbar(f, statusbar, app);
}

fn draw_too_small(f: &mut Frame, area: Rect, app: &mut App) {
    app.popup_area = None;
    app.bar_hits.clear();
    let theme = &app.theme.colors;
    f.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(Span::styled(
                " terminal too small ",
                on(theme.title_fg, theme.title_bg).add_modifier(Modifier::BOLD),
            )),
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(theme.popup_border))
        .title(Span::styled(
            " frame too small for this dialog ",
            on(theme.title_fg, theme.title_bg).add_modifier(Modifier::BOLD),
        ));
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

fn panel_block(theme: &ResolvedColors, title: &str, focused: bool) -> Block<'static> {
    let border = if focused {
        theme.border_focused
    } else {
        theme.border
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(fg(border))
        .title(Span::styled(
            format!(" {title} "),
            on(theme.title_fg, theme.title_bg).add_modifier(Modifier::BOLD),
        ))
}

fn draw_tree(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme.colors;
    // ASCII fallback (terminals that mangle box-drawing): plain markers.
    let (exp, coll, hl) = if app.ascii {
        ("v", ">", ">")
    } else {
        ("▾", "▸", "›")
    };
    let mut items: Vec<ListItem> = Vec::new();
    let mut sel_row: Option<usize> = None;
    let mut row: usize = 0;

    for (pi, proj) in app.state.projects.iter().enumerate() {
        let expanded = app.proj_expanded.get(pi).copied().unwrap_or(false);
        let marker = if expanded { exp } else { coll };
        let count = proj.features.len();
        let mut spans = vec![Span::styled(
            format!("{marker} {} ", proj.name),
            fg(theme.tree_project).add_modifier(Modifier::BOLD),
        )];
        if proj.missing {
            spans.push(Span::styled("(missing) ", fg(theme.dim)));
        }
        spans.push(Span::styled(
            format!("({count} feature{})", if count == 1 { "" } else { "s" }),
            fg(theme.dim),
        ));
        items.push(ListItem::new(Line::from(spans)));
        if app.sel == Sel::Project(pi) {
            sel_row = Some(row);
        }
        row += 1;
        if !expanded {
            continue;
        }
        for (fi, feat) in proj.features.iter().enumerate() {
            let fexp = app
                .feat_expanded
                .get(pi)
                .and_then(|v| v.get(fi))
                .copied()
                .unwrap_or(false);
            let marker = if fexp { exp } else { coll };
            let n = feat.shells.len();
            items.push(ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{marker} {}", feat.name), fg(theme.tree_feature)),
                Span::styled(
                    format!(" [{n} shell{}]", if n == 1 { "" } else { "s" }),
                    fg(theme.dim),
                ),
            ])));
            if app.sel == Sel::Feature(pi, fi) {
                sel_row = Some(row);
            }
            row += 1;
            if !fexp {
                continue;
            }
            for (si, sh) in feat.shells.iter().enumerate() {
                let st = sh
                    .pane_id
                    .as_ref()
                    .and_then(|p| app.statuses.get(p))
                    .copied()
                    .unwrap_or(Status::Idle);
                items.push(ListItem::new(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(format!("#{}", sh.index), fg(theme.tree_shell)),
                    Span::raw(" "),
                    Span::styled(st.label(), fg(status_color(theme, st))),
                    Span::styled(
                        format!("  {}", sh.window_id.as_deref().unwrap_or("no window")),
                        fg(theme.dim),
                    ),
                ])));
                if app.sel == Sel::Shell(pi, fi, si) {
                    sel_row = Some(row);
                }
                row += 1;
            }
        }
    }

    let block = panel_block(
        theme,
        &format!("projects/features ({})", app.state.projects.len()),
        true,
    );
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

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme.colors;
    let lines: Vec<Line> = match app.sel {
        Sel::Project(pi) => {
            if let Some(p) = app.state.projects.get(pi) {
                vec![
                    Line::from(Span::styled(
                        &p.name,
                        fg(theme.tree_project).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("path: ", fg(theme.accent)),
                        Span::raw(p.path.display().to_string()),
                    ]),
                    Line::from(vec![
                        Span::styled("status: ", fg(theme.accent)),
                        Span::styled(
                            if p.missing {
                                "missing — path is not a git repo"
                            } else {
                                "active"
                            },
                            fg(if p.missing {
                                theme.status_failed
                            } else {
                                theme.status_working
                            }),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("base branch: ", fg(theme.accent)),
                        Span::raw(&p.base_branch),
                    ]),
                    Line::from(vec![
                        Span::styled("agent: ", fg(theme.accent)),
                        Span::raw(&p.agent_command),
                        Span::styled(format!(" (shell {})", p.agent_shell), fg(theme.dim)),
                    ]),
                    Line::from(vec![
                        Span::styled("worktrees: ", fg(theme.accent)),
                        Span::raw(app.cfg.worktree_root_for(&p.path).display().to_string()),
                    ]),
                    Line::from(""),
                    Line::from(Span::styled("features:", fg(theme.accent))),
                ]
                .into_iter()
                .chain(p.features.iter().map(|ft| {
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(&ft.name, fg(theme.tree_feature)),
                        Span::styled(
                            format!(" {} {}", arrow(app.ascii), ft.branch),
                            fg(theme.dim),
                        ),
                    ])
                }))
                .collect()
            } else {
                vec![Line::from("no project selected")]
            }
        }
        Sel::Feature(pi, fi) => {
            if let Some(ft) = app.state.projects.get(pi).and_then(|p| p.features.get(fi)) {
                let dirty = app.feature_is_dirty(pi, fi);
                vec![
                    Line::from(Span::styled(
                        &ft.name,
                        fg(theme.tree_feature).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("branch: ", fg(theme.accent)),
                        Span::raw(&ft.branch),
                    ]),
                    Line::from(vec![
                        Span::styled("worktree: ", fg(theme.accent)),
                        Span::raw(ft.worktree_path.display().to_string()),
                    ]),
                    Line::from(vec![
                        Span::styled("created: ", fg(theme.accent)),
                        Span::raw(if ft.created_at.is_empty() {
                            "unknown".into()
                        } else {
                            ft.created_at.clone()
                        }),
                    ]),
                    Line::from(vec![
                        Span::styled("status: ", fg(theme.accent)),
                        Span::styled(
                            if dirty {
                                "dirty (uncommitted changes)"
                            } else {
                                "clean"
                            },
                            fg(if dirty {
                                theme.status_failed
                            } else {
                                theme.status_working
                            }),
                        ),
                    ]),
                    Line::from(""),
                    Line::from(Span::styled("shells:", fg(theme.accent))),
                ]
                .into_iter()
                .chain(ft.shells.iter().map(|sh| {
                    let st = sh
                        .pane_id
                        .as_ref()
                        .and_then(|p| app.statuses.get(p))
                        .copied()
                        .unwrap_or(Status::Idle);
                    let cmd = sh
                        .pane_id
                        .as_ref()
                        .and_then(|p| app.panes.get(p))
                        .map(|p| p.current_command.clone())
                        .unwrap_or_default();
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("#{}", sh.index), fg(theme.tree_shell)),
                        Span::raw("  "),
                        Span::styled(st.label(), fg(status_color(theme, st))),
                        Span::styled(format!("  {cmd}"), fg(theme.dim)),
                    ])
                }))
                .collect()
            } else {
                vec![Line::from("no feature selected")]
            }
        }
        Sel::Shell(pi, fi, si) => {
            let sh = app
                .state
                .projects
                .get(pi)
                .and_then(|p| p.features.get(fi))
                .and_then(|ft| ft.shells.get(si));
            if let Some(sh) = sh {
                let st = sh
                    .pane_id
                    .as_ref()
                    .and_then(|p| app.statuses.get(p))
                    .copied()
                    .unwrap_or(Status::Idle);
                let pinfo = sh.pane_id.as_ref().and_then(|p| app.panes.get(p));
                vec![
                    Line::from(Span::styled(
                        format!("shell #{}", sh.index),
                        fg(theme.tree_shell).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("window: ", fg(theme.accent)),
                        Span::raw(&sh.window_name),
                        Span::styled(
                            format!(" ({})", sh.window_id.as_deref().unwrap_or("-")),
                            fg(theme.dim),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("pane: ", fg(theme.accent)),
                        Span::raw(sh.pane_id.as_deref().unwrap_or("-")),
                    ]),
                    Line::from(vec![
                        Span::styled("status: ", fg(theme.accent)),
                        Span::styled(st.label(), fg(status_color(theme, st))),
                    ]),
                    Line::from(vec![
                        Span::styled("process: ", fg(theme.accent)),
                        Span::raw(
                            pinfo
                                .map(|p| p.current_command.clone())
                                .unwrap_or_else(|| "-".into()),
                        ),
                        Span::styled(
                            format!(
                                "  {}x{}",
                                pinfo.map(|p| p.width).unwrap_or(0),
                                pinfo.map(|p| p.height).unwrap_or(0)
                            ),
                            fg(theme.dim),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("keys: ", fg(theme.accent)),
                        Span::raw(format!(
                            "Enter attach{}s quick-send{}d delete feature",
                            sep(app.ascii),
                            sep(app.ascii)
                        )),
                    ]),
                ]
            } else {
                vec![Line::from("no shell selected")]
            }
        }
    };
    let block = panel_block(theme, "detail", false);
    let p = Paragraph::new(Text::from(lines)).block(block);
    f.render_widget(p, area);
    if app.ascii {
        asciify(f, area);
    }
}

fn draw_preview(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme.colors;
    let block = panel_block(theme, "preview (capture-pane)", false);
    let text = match app.sel {
        Sel::Shell(_, _, _) => {
            if let Some(pid) = app.selected_shell_pane() {
                if let Some(cap) = app.captures.get(&pid) {
                    Text::from(cap.clone())
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
        _ => Text::from(
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
        Sel::Project(_) => vec![
            ("I scan".into(), Scan),
            ("i import".into(), Import),
            ("n feature".into(), NewFeature),
            ("x remove".into(), RemoveProject),
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
            ("d delete".into(), DeleteFeature),
            ("? help".into(), Help),
        ],
    }
}

fn draw_statusbar(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme.colors;
    let session = match &app.tmux {
        Some(t) => match &t.inside {
            Some(i) => format!("dashboard: {}", i.session_name),
            None => "standalone".into(),
        },
        None => "no tmux".into(),
    };
    let left = format!(
        " lazywingman{} {session}: {} project(s) ",
        sep(app.ascii),
        app.state.projects.len()
    );
    // Right side: contextual shortcut entries, each with a hit range so a
    // mouse click can run it. Truncated (never wrapped) on narrow frames —
    // the tail just drops off the screen.
    let entries = bar_entries(app);
    let sp = sep(app.ascii);
    let mut right = String::new();
    let mut hits: Vec<(u16, u16, BarAction)> = Vec::new();
    let mut cx: u16 = 0;
    for (i, (label, action)) in entries.iter().enumerate() {
        if i > 0 {
            right.push_str(sp);
            cx += sp.chars().count() as u16;
        }
        let start = cx;
        right.push_str(label);
        cx += label.chars().count() as u16;
        hits.push((start, cx, *action));
    }
    let right_x = area.width.saturating_sub(cx);
    app.bar_hits = hits
        .into_iter()
        .map(|(a, b, act)| (right_x + a, right_x + b, act))
        .collect();
    let msg = app.message.clone().unwrap_or_else(|| {
        format!(
            "j/k move{} Enter attach{} i import{} I scan{} n feature{} s send{} d delete",
            sep(app.ascii),
            sep(app.ascii),
            sep(app.ascii),
            sep(app.ascii),
            sep(app.ascii),
            sep(app.ascii)
        )
    });
    let line = Line::from(vec![
        Span::styled(left, on(theme.title_fg, theme.title_bg)),
        Span::styled(format!(" {msg} "), fg(theme.foreground)),
        Span::styled(right, fg(theme.dim)),
    ]);
    f.render_widget(Paragraph::new(line), area);
    if app.ascii {
        asciify(f, area);
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
    let area = centered_rect(f.area(), 62, 78);
    f.render_widget(Clear, area);
    let rows = [
        ("i", "import project (absolute path)"),
        ("I", "scan for new projects (discovery rescan)"),
        ("n", "new feature (worktree + agent shell)"),
        ("a", "add shell to feature"),
        ("s", "quick-send a line to the selected shell"),
        ("d", "delete feature (kills windows, removes worktree)"),
        ("x", "remove project from LazyWingman"),
        ("↵ / l", "attach to shell · expand project/feature"),
        ("h", "collapse · jump to parent"),
        ("j / k", "move selection"),
        ("g / G", "first / last item"),
        ("PgUp / PgDn, ctrl-u / ctrl-d", "half page up / down"),
        ("Home / End", "first / last item"),
        ("r", "refresh (reconcile state with reality)"),
        ("?", "this help"),
        ("q / Esc", "quit"),
    ];
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        " keybindings ",
        on(theme.title_fg, theme.title_bg).add_modifier(Modifier::BOLD),
    ))];
    lines.push(Line::from(""));
    for (key, desc) in rows {
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {key:<30} "),
                fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::raw(desc),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " mouse: click selects · double-click opens · wheel scrolls · click a bar entry runs it",
        fg(theme.dim),
    )));
    lines.push(Line::from(""));
    // The way back differs by where the dashboard runs: inside tmux the
    // back-key jumps to this window; standalone there is no back-key — the
    // user detaches from `tmux attach` with C-b d and lands back here.
    if app.tmux.as_ref().and_then(|t| t.inside.as_ref()).is_some() {
        lines.push(Line::from(Span::styled(
            "attach: press Enter in the dashboard, then press the tmux back-key",
            fg(theme.dim),
        )));
        lines.push(Line::from(Span::styled(
            format!(
                "back-key is '{}' (config tmux.back_key) — jump back to this window",
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(theme.popup_border))
        .title(Span::styled(
            " help ",
            on(theme.title_fg, theme.title_bg).add_modifier(Modifier::BOLD),
        ));
    f.render_widget(Paragraph::new(Text::from(lines)).block(block), area);
    if app.ascii {
        asciify(f, area);
    }
    area
}

fn draw_input(f: &mut Frame, app: &App) -> Rect {
    let theme = &app.theme.colors;
    let (title, label) = match &app.mode {
        Mode::Input { kind, .. } => match kind {
            InputKind::ImportProject => ("import project", "repo path (absolute): "),
            InputKind::NewFeature => ("new feature", "feature name: "),
            InputKind::QuickSend => ("quick-send", "send to agent: "),
        },
        _ => unreachable!(),
    };
    let area = centered_rect(f.area(), 60, 18);
    f.render_widget(Clear, area);
    let Mode::Input { text, .. } = &app.mode else {
        return area;
    };
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(theme.popup_border))
        .title(Span::styled(
            format!(" {title} "),
            on(theme.title_fg, theme.title_bg).add_modifier(Modifier::BOLD),
        ));
    let mut lines = vec![Line::from("")];
    lines.push(Line::from(vec![
        Span::styled(label, fg(theme.accent)),
        Span::raw(text.clone()),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Enter to submit{}Esc to cancel", sep(app.ascii)),
        fg(theme.dim),
    )));
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

fn draw_confirm(f: &mut Frame, app: &App) -> Rect {
    let theme = &app.theme.colors;
    let Mode::Confirm { confirm } = &app.mode else {
        return Rect::default();
    };
    let area = centered_rect(f.area(), 62, 26);
    f.render_widget(Clear, area);
    let sp = sep(app.ascii);
    let options = match &confirm.force_label {
        Some(fl) => format!(" [y] yes{sp}[f] {fl}{sp}[n] cancel "),
        None => format!(" [y] yes{sp}[n] cancel "),
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(fg(theme.popup_border))
        .title(Span::styled(
            format!(" {} ", confirm.title),
            on(theme.title_fg, theme.title_bg).add_modifier(Modifier::BOLD),
        ));
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
        app.startup().unwrap();

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        for mode in [
            Mode::Normal,
            Mode::Help,
            Mode::Input {
                kind: InputKind::ImportProject,
                text: "/tmp/some/path".into(),
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
        app.startup().unwrap();
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
        let cfg = Config::default();
        let theme = theme::load("ansi", false).unwrap();
        let mut app = App::new(cfg, theme, None);
        app.startup().unwrap();

        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        for (kind, label, text) in [
            (
                InputKind::ImportProject,
                "repo path (absolute): ",
                "/tmp/repo",
            ),
            (InputKind::NewFeature, "feature name: ", "foo"),
            (InputKind::QuickSend, "send to agent: ", "busy 2"),
        ] {
            app.mode = Mode::Input {
                kind,
                text: text.into(),
            };
            terminal
                .draw(|f| draw(f, &mut app))
                .expect("draw must not fail");

            // popup area for this backend size, same math as draw_input
            let area = centered_rect(terminal.size().unwrap().into(), 60, 18);
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

    // ---- M8.4a — small terminals, resize, ASCII fallback ----

    fn seeded_app() -> App {
        let cfg = Config::default();
        let theme = theme::load("ansi", false).unwrap();
        let mut app = App::new(cfg, theme, None);
        app.startup().unwrap();
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

    /// Layout tiers degrade gracefully instead of drawing garbage: full
    /// layout at 120x40 (detail + preview present), tree-only below the
    /// detail threshold, a plain message below the tree threshold.
    #[test]
    fn small_terminals_degrade_gracefully() {
        let mut app = seeded_app();
        let mut at = |w: u16, h: u16| {
            let backend = ratatui::backend::TestBackend::new(w, h);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            buffer_text(&terminal)
        };

        let full = at(120, 40);
        assert!(full.contains("detail"), "full layout has the detail panel");
        assert!(
            full.contains("preview (capture-pane)"),
            "full layout has the preview panel"
        );

        let no_preview = at(50, 10);
        assert!(no_preview.contains("detail"), "tree+detail tier");
        assert!(
            !no_preview.contains("preview"),
            "preview hides first on a small frame"
        );

        let tree_only = at(30, 8);
        assert!(
            tree_only.contains("projects/features"),
            "tree-only tier still shows the tree"
        );
        assert!(!tree_only.contains("detail"), "detail hides second");

        let message = at(20, 5);
        assert!(
            message.contains("terminal too small"),
            "below the tree threshold a message shows instead: {message:?}"
        );
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
                kind: InputKind::ImportProject,
                text: "/tmp/repo-a".into(),
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
        assert_eq!(tree_width((30, 8)), Some(30)); // tree-only: 100%
        assert_eq!(tree_width((20, 5)), None); // message tier
    }
}
