//! TUI rendering: tree, detail, preview, status bar, overlays.
//!
//! All colours come from the resolved [`Theme`]; the renderer never hardcodes
//! a colour.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{App, InputKind, Mode, Sel};
use crate::status::Status;
use crate::theme::ResolvedColors;

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
    // theme background
    f.render_widget(
        Block::default().style(Style::default().bg(app.theme.colors.background)),
        area,
    );
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

    match &app.mode {
        Mode::Help => draw_help(f, app),
        Mode::Input { .. } => draw_input(f, app),
        Mode::Confirm { .. } => draw_confirm(f, app),
        Mode::Normal => {}
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
    let mut items: Vec<ListItem> = Vec::new();
    let mut sel_row: Option<usize> = None;
    let mut row: usize = 0;

    for (pi, proj) in app.state.projects.iter().enumerate() {
        let expanded = app.proj_expanded.get(pi).copied().unwrap_or(false);
        let marker = if expanded { "▾" } else { "▸" };
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
            let marker = if fexp { "▾" } else { "▸" };
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
        .highlight_symbol("› ");
    let mut state = ListState::default();
    state.select(sel_row);
    f.render_stateful_widget(list, area, &mut state);
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
                        Span::styled(format!(" → {}", ft.branch), fg(theme.dim)),
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
                        Span::raw("Enter attach · s quick-send · d delete feature"),
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
                    Text::from("capturing… (select the shell in the tree)")
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
}

fn draw_statusbar(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme.colors;
    let session = match &app.tmux {
        Some(t) => match &t.inside {
            Some(i) => format!("dashboard: {}", i.session_name),
            None => "standalone".into(),
        },
        None => "no tmux".into(),
    };
    let left = format!(" lazywingman · {session} ");
    let right = format!(" poll {}ms · ? help · q quit ", app.cfg.poll_interval_ms);
    let msg = app.message.clone().unwrap_or_else(|| {
        "j/k move · ↵ attach · i import · I scan · n feature · s send · d delete".into()
    });
    let line = Line::from(vec![
        Span::styled(left, on(theme.title_fg, theme.title_bg)),
        Span::styled(format!(" {msg} "), fg(theme.foreground)),
        Span::styled(right, fg(theme.dim)),
    ]);
    f.render_widget(Paragraph::new(line), area);
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

fn draw_help(f: &mut Frame, app: &App) {
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
                format!(" {key:<9} "),
                fg(theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::raw(desc),
        ]));
    }
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
}

fn draw_input(f: &mut Frame, app: &App) {
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
        return;
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
        "Enter to submit · Esc to cancel",
        fg(theme.dim),
    )));
    let popup_style = on(theme.popup_fg, theme.popup_bg);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .style(popup_style),
        area,
    );
    // cursor after the typed text, on the input row (the second content
    // line below the border: one spacer line, then label + text)
    let cursor_x = inner.x + label.chars().count() as u16 + text.chars().count() as u16;
    let cursor_y = inner.y + 1;
    f.set_cursor_position(Position::new(cursor_x, cursor_y));
}

fn draw_confirm(f: &mut Frame, app: &App) {
    let theme = &app.theme.colors;
    let Mode::Confirm { confirm } = &app.mode else {
        return;
    };
    let area = centered_rect(f.area(), 62, 26);
    f.render_widget(Clear, area);
    let options = match &confirm.force_label {
        Some(fl) => format!(" [y] yes · [f] {fl} · [n] cancel "),
        None => " [y] yes · [n] cancel ".to_string(),
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
}
