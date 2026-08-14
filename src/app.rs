//! Application state machine, event loop, and user actions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use crate::config::{Config, ProjectConfig};
use crate::error::{Error, Result};
use crate::git::Git;
use crate::state::{Feature, Project, Shell, State};
use crate::status::{self, Compiled, Status};
use crate::theme::Theme;
use crate::tmux::{PaneInfo, Tmux};

/// A node in the projects/features/shells tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sel {
    Project(usize),
    Feature(usize, usize),
    Shell(usize, usize, usize),
}

/// What the user is doing right now.
pub enum Mode {
    Normal,
    Input { kind: InputKind, text: String },
    Confirm { confirm: Confirm },
    Help,
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
    pub quit: bool,
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
            quit: false,
        }
    }

    // ---------------------------------------------------------- lifecycle

    /// Load state, reconcile against reality, bind the back key.
    pub fn startup(&mut self) -> Result<()> {
        self.state = State::load()?;
        self.merge_config_projects()?;
        if let Some(tmux) = &self.tmux {
            self.state.reconcile(tmux)?;
            self.state.prune(Some(tmux))?;
            tmux.bind_back_key(&self.cfg.tmux.back_key)?;
            log::info!(
                "back key {} → dashboard window {}",
                self.cfg.tmux.back_key,
                tmux.window_id
            );
        }
        self.build_tree_flags();
        self.clamp_sel();
        self.save_state()?;
        Ok(())
    }

    fn save_state(&mut self) -> Result<()> {
        self.state.save()?;
        Ok(())
    }

    /// Import projects declared in config.toml that aren't in state yet.
    fn merge_config_projects(&mut self) -> Result<()> {
        let mut imported = 0;
        for p in self.cfg.usable_projects() {
            let path = PathBuf::from(&p.path);
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
            let name = Git.repo_name(&path);
            let base = Git.resolve_base_branch(&path, &self.cfg.base_branch_for(p))?;
            let agent = self.cfg.agent_for(p);
            self.state.projects.push(Project {
                name,
                path,
                base_branch: base,
                agent_command: agent.command,
                agent_shell: agent.shell,
                features: Vec::new(),
            });
            imported += 1;
        }
        if imported > 0 {
            log::info!("imported {imported} project(s) from config");
        }
        Ok(())
    }

    fn build_tree_flags(&mut self) {
        self.proj_expanded = vec![true; self.state.projects.len()];
        self.feat_expanded = self
            .state
            .projects
            .iter()
            .map(|p| vec![false; p.features.len()])
            .collect();
    }

    // ---------------------------------------------------------- navigation

    /// All visible tree nodes in display order.
    fn visible_nodes(&self) -> Vec<Sel> {
        let mut v = Vec::new();
        for (pi, proj) in self.state.projects.iter().enumerate() {
            v.push(Sel::Project(pi));
            if !self.proj_expanded.get(pi).copied().unwrap_or(false) {
                continue;
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
    fn clamp_sel(&mut self) {
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
                if let Some(e) = self.proj_expanded.get_mut(pi) {
                    if *e {
                        *e = false;
                    }
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
                } else {
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
            .ok_or_else(|| Error::Tmux("not running inside tmux (LAZYWINGMAN_NO_TMUX mode)".into()))
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
        let name = Git.repo_name(&path);
        // consult config for per-project agent settings by path
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
            features: Vec::new(),
        });
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
        let tmux = self.require_tmux()?;
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::State("feature name is empty".into()));
        }
        let slug = crate::git::slugify(name);
        let proj = self
            .state
            .projects
            .get(pi)
            .cloned()
            .ok_or_else(|| Error::State(format!("project index {pi} out of range")))?;
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

        // spawn the agent shell in the new worktree
        let wname = Tmux::window_name(&proj.name, &slug, 0);
        let w = tmux.new_window(&wname, &root, &proj.agent_shell)?;
        tmux.send_keys(&w.pane_id, &proj.agent_command, true)?;

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
        self.feat_expanded.get_mut(pi).map(|v| v.push(false));
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
        let feat = proj
            .features
            .get(fi)
            .cloned()
            .ok_or_else(|| Error::State(format!("feature index {fi} out of range")))?;
        let idx = feat.shells.len();
        let wname = Tmux::window_name(&proj.name, &feat.branch, idx);
        let w = tmux.new_window(&wname, &feat.worktree_path, &proj.agent_shell)?;
        tmux.send_keys(&w.pane_id, &proj.agent_command, true)?;
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
        match self.require_tmux().and_then(|t| t.select_window(wid)) {
            Ok(()) => log::info!("attached to {}", sh.window_name),
            Err(e) => {
                self.message = Some(format!("attach failed: {e}"));
                log::error!("attach failed: {e}");
            }
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
        tmux.send_keys(pid, text, true)?;
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
        Git.worktree_remove(&feat.worktree_path, force)?;
        if self.cfg.worktrees.delete_branch {
            match Git.branch_delete(&proj.path, &feat.branch) {
                Ok(()) => log::info!("deleted branch '{}'", feat.branch),
                Err(e) => log::warn!("branch delete: {e}"),
            }
        }
        self.state.projects[pi].features.remove(fi);
        self.feat_expanded.get_mut(pi).map(|v| v.remove(fi));
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
                        "⚠ worktree has uncommitted changes — force delete will discard them"
                    } else {
                        "worktree is clean"
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
            let pat = self.patterns_for(&path, &cmd);
            let st = status::classify(&pat, &tail, &current_cmd, dead);
            self.statuses.insert(pid.clone(), st);
            if Some(&pid) == sel_pane.as_ref() {
                self.captures.insert(pid.clone(), tail);
            }
        }
    }

    // ---------------------------------------------------------- keys

    pub fn on_key(&mut self, key: KeyEvent) {
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
                _ => {}
            },
            Mode::Normal => self.on_key_normal(key),
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
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
                    self.mode = Mode::Input {
                        kind: InputKind::QuickSend,
                        text: String::new(),
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
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
    }

    fn refresh(&mut self) {
        if let Some(tmux) = &self.tmux {
            let mut ok = true;
            if let Err(e) = self.state.reconcile(tmux) {
                log::warn!("reconcile: {e}");
                ok = false;
            }
            if let Err(e) = self.state.prune(Some(tmux)) {
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
pub fn run(cfg: Config, theme: Theme, tmux: Option<Tmux>) -> Result<()> {
    let mut app = App::new(cfg, theme, tmux);
    if let Err(e) = app.startup() {
        // before the terminal is taken over, stderr is safe
        eprintln!("lazywingman: startup failed: {e}");
        return Err(e);
    }
    app.poll();
    let mut terminal = ratatui::init();
    let res = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    res
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
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
            if let Event::Key(key) = event::read()? {
                app.on_key(key);
            }
        }
    }
    Ok(())
}
