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
    /// Alt+Enter / Ctrl+Enter = send, Esc = cancel. The accessibility
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
    /// Startup diagnostics overlay (shown when configuration problems were
    /// dropped at load — see the configuration guidelines). Scrollable list
    /// of diagnostics, dismissed with Esc/q/Enter.
    Diagnostics,
}

/// A multiline text buffer for the quick-send editor: lines + a cursor
/// (`row`, `col` in characters) + a viewport scroll (first visible line).
/// Editing happens at the cursor; `text()` joins the lines for sending.
#[derive(Debug, Clone, Default)]
pub struct TextBuf {
    lines: Vec<String>,
    row: usize,
    col: usize,
    scroll: usize,
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

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, i: usize) -> &str {
        self.lines.get(i).map(String::as_str).unwrap_or("")
    }

    pub fn cursor_row(&self) -> usize {
        self.row
    }

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

    pub fn insert_char(&mut self, c: char) {
        self.lines[self.row].insert(self.col, c);
        self.col += 1;
    }

    pub fn newline(&mut self) {
        let rest = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
        self.ensure_visible(1);
    }

    pub fn insert_tab(&mut self) {
        self.insert_char('\t');
    }

    /// Backspace: delete the char before the cursor; at line start, join
    /// with the previous line.
    pub fn backspace(&mut self) {
        if self.col > 0 {
            self.lines[self.row].remove(self.col - 1);
            self.col -= 1;
        } else if self.row > 0 {
            let prev_len = self.lines[self.row - 1].len();
            let rest = self.lines.remove(self.row);
            self.lines[self.row - 1].push_str(&rest);
            self.row -= 1;
            self.col = prev_len;
            self.ensure_visible(1);
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
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len();
            self.ensure_visible(1);
        }
    }

    pub fn move_right(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
            self.ensure_visible(1);
        }
    }

    pub fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.clamp_col();
            self.ensure_visible(1);
        }
    }

    pub fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.clamp_col();
            self.ensure_visible(1);
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
        self.ensure_visible(page.max(1));
        true
    }

    fn clamp_col(&mut self) {
        self.col = self.col.min(self.lines[self.row].len());
    }

    /// Keep the cursor row visible within a `visible`-line viewport:
    /// scroll down if the cursor moved above the viewport, up if it moved
    /// below it.
    fn ensure_visible(&mut self, visible: usize) {
        let view = visible.max(1);
        if self.row < self.scroll {
            self.scroll = self.row;
        } else if self.row >= self.scroll + view {
            self.scroll = self.row + 1 - view;
        }
    }
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

/// Content rows of the textarea popup (inside the border, between the
/// label row and the hint row): height − 4 (2 borders + label + hint).
pub(crate) fn textarea_visible_lines(viewport_h: u16) -> usize {
    textarea_height(viewport_h) as usize - 4
}

/// An action reachable from the statusbar shortcut entries (and by mouse
/// click). Same words as the help overlay — learning one teaches the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarAction {
    Help,
    Import,
    Scan,
    NewFeature,
    AddShell,
    QuickSend,
    DeleteFeature,
    RemoveProject,
    Attach,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    ImportProject,
    NewFeature,
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
    DeleteFeature { project: usize, feature: usize },
    RemoveProject { project: usize },
}

pub struct App {
    pub cfg: Config,
    pub theme: Theme,
    pub tmux: Option<Tmux>,
    pub state: State,
    pub sel: Sel,
    pub proj_expanded: Vec<bool>,
    pub feat_expanded: Vec<Vec<bool>>,
    pub mode: Mode,
    pub panes: HashMap<String, PaneInfo>,
    pub captures: HashMap<String, String>,
    pub statuses: HashMap<String, Status>,
    pattern_cache: HashMap<String, Compiled>,
    pub last_poll: Instant,
    pub message: Option<String>,
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
}

impl App {
    pub fn new(cfg: Config, theme: Theme, tmux: Option<Tmux>) -> App {
        App {
            cfg,
            theme,
            tmux,
            state: State::default(),
            sel: Sel::Project(0),
            proj_expanded: Vec::new(),
            feat_expanded: Vec::new(),
            mode: Mode::Normal,
            panes: HashMap::new(),
            captures: HashMap::new(),
            statuses: HashMap::new(),
            pattern_cache: HashMap::new(),
            last_poll: Instant::now(),
            message: None,
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

    /// The root directory scanned by the `I` key and the startup scope:
    /// `[discovery].scan_root`, or the directory LazyWingman was started
    /// from when unset. Relative roots resolve against the cwd.
    fn scan_root(&self) -> PathBuf {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        self.scan_root_from(&cwd)
    }

    /// [`Self::scan_root`] with an explicit base directory (the cwd is
    /// injected here so tests don't have to `chdir`).
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
        // Load worktrees for the projects that are already visible in
        // their expanded state: the flattened single project, and every
        // unfolded project when the count is ≤5. Collapsed (>5 projects)
        // stay untouched — that is the lazy part.
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
        for pi in to_load {
            self.ensure_worktrees(pi)?;
        }
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

    /// Run a discovery scan and report the outcome in the statusbar;
    /// also re-prunes so projects that came back from `missing` re-activate.
    fn run_scan(&mut self, root: PathBuf) {
        match self.discover_and_import(&root) {
            Ok(n) => {
                if let Some(tmux) = &self.tmux {
                    let _ = self.state.prune(Some(tmux), &self.worktrees_loaded);
                }
                self.build_tree_flags();
                self.clamp_sel();
                // Persist: imports must survive a restart.
                if let Err(e) = self.save_state() {
                    log::warn!("save after scan: {e}");
                }
                if n > 0 {
                    self.message = Some(format!("scan: {n} new project(s) added"));
                } else {
                    self.message =
                        Some(format!("scan of {} found no new projects", root.display()));
                }
            }
            Err(e) => self.fail(&e),
        }
    }

    pub(crate) fn build_tree_flags(&mut self) {
        // Scope-aware: only in-scope projects get rows, and >5 projects
        // stay collapsed by default (per the discovery rework — many repos
        // must not unfold everything on boot). ≤5 projects expand so the
        // user sees their features immediately.
        let vis = self.visible_projects();
        // Flatten: exactly one project in scope WITH features → its
        // features ARE the tree, no project row. A lone project with no
        // features (missing, or nothing linked yet) still shows its row.
        self.flatten = vis.len() == 1 && !self.state.projects[vis[0]].features.is_empty();
        let expand = vis.len() <= 5;
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
            .map(|p| vec![false; p.features.len()])
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
        if added > 0 {
            log::info!("lazy worktree load: {added} feature(s) for '{}'", proj.name);
        }
        Ok(())
    }

    // ---------------------------------------------------------- navigation

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
                if !fexp {
                    continue;
                }
                for si in 0..feat.shells.len() {
                    v.push(Sel::Shell(pi, fi, si));
                }
            }
        }
        v
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
                if let Some(e) = self.feat_expanded.get_mut(pi).and_then(|v| v.get_mut(fi)) {
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
                    .unwrap_or(false);
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

    fn selected_shell(&self) -> Option<(usize, usize, usize)> {
        match self.sel {
            Sel::Shell(pi, fi, si) => Some((pi, fi, si)),
            _ => None,
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

    pub fn import_project(&mut self, path: &str) -> Result<()> {
        let path = PathBuf::from(path.trim());
        if !path.is_absolute() {
            return Err(Error::Config(format!(
                "path must be absolute: {}",
                path.display()
            )));
        }
        if !Git.is_repo(&path) {
            return Err(Error::Git(format!(
                "{} is not a git repository",
                path.display()
            )));
        }
        if self.state.projects.iter().any(|p| p.path == path) {
            return Err(Error::State("project already imported".into()));
        }
        self.add_project(path)?;
        self.build_tree_flags();
        self.clamp_sel();
        self.save_state()?;
        self.message = Some(format!(
            "imported project '{}'",
            self.state.projects.last().unwrap().name
        ));
        Ok(())
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
        tmux.send_keys(&w.pane_id, &proj.agent_command, true)?;

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
        self.message = Some(format!("feature '{}' created", name));
        Ok(())
    }

    pub fn add_shell(&mut self, pi: usize, fi: usize) -> Result<()> {
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
        let idx = feat.shells.len();
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
        tmux.send_keys(&w.pane_id, &proj.agent_command, true)?;
        self.state.projects[pi].session_name = Some(session_name.clone());
        self.state.projects[pi].features[fi].shells.push(Shell {
            index: idx,
            window_name: wname,
            window_id: Some(w.window_id),
            pane_id: Some(w.pane_id),
        });
        self.save_state()?;
        self.message = Some(format!("shell #{} added", idx));
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
            self.message = Some("shell has no live window".into());
            return;
        };
        let Some(session) = self.state.projects[pi].session_name.clone() else {
            self.message = Some("project has no agent session".into());
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
            self.message = Some(format!("attach failed: {e}"));
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
        // Killing a session's last window destroys the session; if it
        // survives (other features still have shells), keep it.
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
        Git.worktree_remove(&feat.worktree_path, force)?;
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
        self.message = Some(format!("feature '{}' deleted", feat.name));
        Ok(())
    }

    pub fn remove_project(&mut self, pi: usize) -> Result<()> {
        let tmux = self.require_tmux()?;
        let proj = self
            .state
            .projects
            .get(pi)
            .cloned()
            .ok_or_else(|| Error::State(format!("project index {pi} out of range")))?;
        for feat in &proj.features {
            for sh in &feat.shells {
                if let Some(wid) = &sh.window_id {
                    let _ = tmux.kill_window(wid);
                }
            }
        }
        // Tear down the project's agent session entirely (idempotent).
        if let Some(sess) = &proj.session_name
            && tmux.session_exists(sess)
        {
            log::info!("killing agent session {sess}");
            let _ = tmux.kill_session(sess);
        }
        self.state.projects.remove(pi);
        self.proj_expanded.remove(pi);
        self.feat_expanded.remove(pi);
        self.clamp_sel();
        self.save_state()?;
        // worktrees stay on disk (git manages them; LazyWingman never deletes)
        self.message = Some(format!(
            "project '{}' removed from LazyWingman (worktrees kept on disk)",
            proj.name
        ));
        Ok(())
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
            self.message = Some("select a feature first".into());
            return;
        };
        let dirty = self.feature_is_dirty(pi, fi);
        let feat = &self.state.projects[pi].features[fi];
        self.mode = Mode::Confirm {
            confirm: Confirm {
                title: "delete feature".into(),
                body: format!(
                    "Delete feature '{}'?\nworktree: {}\n{}",
                    feat.name,
                    feat.worktree_path.display(),
                    if dirty {
                        format!(
                            "{} worktree has uncommitted changes — force delete will discard them",
                            crate::ui::warn_mark(self.ascii)
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

    pub fn confirm_remove_project(&mut self) {
        let Sel::Project(pi) = self.sel else {
            self.message = Some("select a project first".into());
            return;
        };
        let proj = &self.state.projects[pi];
        self.mode = Mode::Confirm {
            confirm: Confirm {
                title: "remove project".into(),
                body: format!(
                    "Remove '{}' from LazyWingman?\n{}\nAgent windows are killed; worktrees stay on disk.",
                    proj.name,
                    proj.path.display()
                ),
                force_label: None,
                action: ConfirmAction::RemoveProject { project: pi },
            },
        };
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
            let Some((current_cmd, dead)) = self
                .panes
                .get(&pid)
                .map(|p| (p.current_command.clone(), p.dead))
            else {
                continue;
            };
            let tail = match tmux.capture_pane(&pid, 120) {
                Ok(t) => t,
                Err(e) => {
                    log::debug!("capture-pane {pid}: {e}");
                    continue;
                }
            };
            // Classify on the *recent* tail only — stale lines from the
            // banner would otherwise keep a waiting agent "working".
            // capture-pane pads the pane's empty area with blank lines, so
            // trailing blanks are trimmed first.
            let mut tail_lines: Vec<&str> = tail.lines().collect();
            while tail_lines
                .last()
                .map(|l| l.trim().is_empty())
                .unwrap_or(false)
            {
                tail_lines.pop();
            }
            let start = tail_lines.len().saturating_sub(RECENT_TAIL_LINES);
            let recent = tail_lines[start..].join("\n");
            let pat = self.patterns_for(&path, &cmd);
            let st = status::classify(&pat, &recent, &current_cmd, dead);
            self.statuses.insert(pid.clone(), st);
            if Some(&pid) == sel_pane.as_ref() {
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
        // The textarea's page size = its visible content rows.
        let visible_lines = textarea_visible_lines(self.viewport.1);
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
                // Send: Alt+Enter or Ctrl+Enter. A plain Enter inserts a
                // newline; Esc cancels. The textarea owns all editing keys.
                let send_chord = key.code == KeyCode::Enter
                    && (key.modifiers.contains(KeyModifiers::ALT)
                        || key.modifiers.contains(KeyModifiers::CONTROL));
                if send_chord {
                    let text = buf.text();
                    self.mode = Mode::Normal;
                    self.submit_input(InputKind::QuickSend, text);
                } else {
                    match key.code {
                        KeyCode::Enter => buf.newline(),
                        KeyCode::Esc => self.mode = Mode::Normal,
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
                }
            }
            Mode::Confirm { confirm } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
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
            KeyCode::Char('i') => {
                self.mode = Mode::Input {
                    kind: InputKind::ImportProject,
                    text: String::new(),
                };
            }
            // `I` — the stronger variant of `i`: scan a whole root for new
            // projects (per the TUI guidelines, shifted = stronger).
            KeyCode::Char('I') => {
                let root = self.scan_root();
                self.run_scan(root);
            }
            KeyCode::Char('n') => match self.sel {
                Sel::Project(_) | Sel::Feature(_, _) => {
                    self.mode = Mode::Input {
                        kind: InputKind::NewFeature,
                        text: String::new(),
                    };
                }
                Sel::Shell(..) => {
                    self.message = Some("select a project to add a feature".into());
                }
            },
            KeyCode::Char('a') => {
                if let Some((pi, fi, _)) = self.selected_feature_ctx() {
                    match self.add_shell(pi, fi) {
                        Ok(()) => {}
                        Err(e) => self.fail(&e),
                    }
                } else {
                    self.message = Some("select a feature to add a shell".into());
                }
            }
            KeyCode::Char('s') => match self.sel {
                Sel::Shell(..) => {
                    self.mode = Mode::Textarea {
                        buf: TextBuf::new(),
                    };
                }
                _ => {
                    self.message = Some("select a shell to quick-send".into());
                }
            },
            KeyCode::Char('d') => self.confirm_delete_feature(),
            KeyCode::Char('x') => self.confirm_remove_project(),
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
                self.message = Some(format!("theme '{}': {e}", name));
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
            self.message = Some(format!("theme '{name}' (not saved: {e})"));
        } else {
            self.message = Some(format!("theme: {name}"));
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
            BarAction::Import => {
                self.mode = Mode::Input {
                    kind: InputKind::ImportProject,
                    text: String::new(),
                };
            }
            BarAction::Scan => {
                let root = self.scan_root();
                self.run_scan(root);
            }
            BarAction::NewFeature => match self.sel {
                Sel::Project(_) | Sel::Feature(_, _) => {
                    self.mode = Mode::Input {
                        kind: InputKind::NewFeature,
                        text: String::new(),
                    };
                }
                _ => {}
            },
            BarAction::AddShell => {
                if let Some((pi, fi, _)) = self.selected_feature_ctx()
                    && let Err(e) = self.add_shell(pi, fi)
                {
                    self.fail(&e);
                }
            }
            BarAction::QuickSend => {
                if let Sel::Shell(..) = self.sel {
                    self.mode = Mode::Textarea {
                        buf: TextBuf::new(),
                    };
                }
            }
            BarAction::DeleteFeature => self.confirm_delete_feature(),
            BarAction::RemoveProject => self.confirm_remove_project(),
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
        if let Mode::ThemePicker { index } = &mut self.mode {
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
        if let Mode::ThemePicker { index } = &mut self.mode {
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
        if let Mode::ThemePicker { index } = &mut self.mode {
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
                self.message = Some("refreshed".into());
            } else {
                self.message = Some("refresh had errors (see log)".into());
            }
        } else {
            self.message = Some("no tmux session".into());
        }
    }

    fn fail(&mut self, e: &Error) {
        self.message = Some(format!("error: {e}"));
        log::error!("action failed: {e}");
    }

    fn submit_input(&mut self, kind: InputKind, text: String) {
        let res = match kind {
            InputKind::ImportProject => self.import_project(&text),
            InputKind::NewFeature => match self.sel {
                Sel::Project(pi) => self.create_feature(pi, &text),
                Sel::Feature(pi, _) => self.create_feature(pi, &text),
                _ => Err(Error::State("select a project first".into())),
            },
            InputKind::QuickSend => match self.sel {
                Sel::Shell(pi, fi, si) => self.quick_send(pi, fi, si, &text),
                _ => Err(Error::State("select a shell first".into())),
            },
        };
        if let Err(e) = res {
            self.fail(&e);
        }
    }

    fn run_confirm(&mut self, action: ConfirmAction, force: bool) {
        let res = match action {
            ConfirmAction::DeleteFeature { project, feature } => {
                self.delete_feature(project, feature, force)
            }
            ConfirmAction::RemoveProject { project } => self.remove_project(project),
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
                app.message = Some(format!("attach handoff failed: {e}"));
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
        if app.last_poll.elapsed() >= Duration::from_millis(app.cfg.poll_interval_ms) {
            app.poll();
        }
        // wait for input, at most until the next poll is due
        let since = app.last_poll.elapsed();
        let until_poll = Duration::from_millis(app.cfg.poll_interval_ms).saturating_sub(since);
        let timeout = until_poll.min(Duration::from_millis(100));
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
            // Two projects → not flattened, both expanded (≤5), and their
            // worktrees ARE loaded at boot (they are the visible rows).
            assert!(!app.flatten);
            assert!(app.visible_projects().len() == 2);
            assert!(
                app.worktrees_loaded.iter().all(|f| *f),
                "≤5 expanded projects load worktrees at boot"
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
            // 6 repos → collapsed by default → worktrees NOT loaded at boot
            let mut repos = Vec::new();
            for i in 0..6 {
                repos.push(repo_with_commit(r, &format!("repo-{i}")));
            }

            let mut app = app_with(tmux_standalone("unused"));
            app.boot_discovery_from(r).unwrap();
            assert!(!app.flatten, "6 projects → not flattened");
            assert!(
                app.worktrees_loaded.iter().all(|f| !*f),
                ">5 collapsed projects keep worktrees lazy at boot"
            );
            // Expanding a project loads ITS worktrees, once.
            let pi = app.visible_projects()[0];
            app.proj_expanded[pi] = false; // ensure collapsed
            app.sel = Sel::Project(pi);
            app.expand_or_attach();
            assert!(app.worktrees_loaded[pi], "expand loads worktrees");
            let n_loaded = app.worktrees_loaded.iter().filter(|f| **f).count();
            assert_eq!(n_loaded, 1, "only the expanded project loads");
            // collapsing + expanding again does not re-run the listing
            app.proj_expanded[pi] = false;
            app.expand_or_attach();
            assert_eq!(
                app.worktrees_loaded.iter().filter(|f| **f).count(),
                1,
                "loaded exactly once"
            );
        });
    }

    /// >5 projects stay collapsed by default; ≤5 expand.
    #[test]
    fn many_projects_collapse_by_default_few_expand() {
        let mut app = app_with(tmux_standalone("unused"));
        app.scope_root = PathBuf::from("/");
        for i in 0..6 {
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
        // 6 projects → collapsed, not flattened
        assert!(!app.flatten);
        assert!(app.proj_expanded.iter().all(|e| !*e));

        let mut few = app_with(tmux_standalone("unused"));
        few.scope_root = PathBuf::from("/");
        for i in 0..3 {
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
        // 3 projects → expanded
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

    /// The `I` key scans the configured root and reports new projects.
    #[test]
    fn i_key_scans_and_imports_new_projects() {
        crate::util::testutil::with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let r = root.path();
            repo_with_commit(r, "repo-a");

            let mut app = app_with(tmux_standalone("unused"));
            app.cfg.discovery.scan_root = r.to_string_lossy().into_owned();
            app.on_key(KeyEvent::new(KeyCode::Char('I'), KeyModifiers::NONE));
            assert_eq!(app.state.projects.len(), 1);
            assert_eq!(app.state.projects[0].name, "repo-a");
            let msg = app.message.as_deref().unwrap();
            assert!(msg.contains("1 new project"), "message: {msg}");
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
        let err = app.add_shell(0, 0).unwrap_err();
        assert!(err.to_string().contains("missing"), "err: {err}");
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
        app.clamp_sel();
        app.sel = Sel::Shell(0, 0, 0);
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
        // a project's bar has "x remove" → confirm_remove_project
        let (x0, x1, _) = app
            .bar_hits
            .iter()
            .find(|(_, _, a)| *a == BarAction::RemoveProject)
            .copied()
            .expect("bar must offer 'x remove' for a project");
        app.on_mouse(click((x0 + x1) / 2, 39));
        assert!(
            matches!(app.mode, Mode::Confirm { .. }),
            "clicking 'x remove' must open the confirm"
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
}
