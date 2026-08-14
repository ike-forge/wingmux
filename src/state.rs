//! Persistent state: projects, features (worktrees), shells (tmux windows).
//!
//! Stored as JSON in `$XDG_STATE_HOME/lazywingman/state.json`. On startup the
//! state is reconciled against reality (`git worktree list`,
//! `tmux list-windows`) so stale entries self-heal and running shells are
//! rediscovered after a restart.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::git::Git;
use crate::tmux::{Tmux, is_lazywingman_window};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub projects: Vec<Project>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub path: PathBuf,
    /// Base branch resolved at import time.
    pub base_branch: String,
    /// Resolved agent command/shell (from config or defaults).
    pub agent_command: String,
    pub agent_shell: String,
    pub features: Vec<Feature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    pub name: String,
    /// Slugified name == git branch == worktree directory.
    pub branch: String,
    pub worktree_path: PathBuf,
    pub created_at: String,
    pub shells: Vec<Shell>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shell {
    pub index: usize,
    pub window_name: String,
    pub window_id: Option<String>,
    pub pane_id: Option<String>,
}

impl State {
    /// Load from disk; missing file → empty state.
    pub fn load() -> Result<State> {
        let path = crate::util::state_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| Error::State(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// Persist to disk (pretty-printed).
    pub fn save(&self) -> Result<()> {
        let path = crate::util::state_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| Error::State(e.to_string()))?;
        std::fs::write(&path, text)?;
        log::info!("state saved: {} projects", self.projects.len());
        Ok(())
    }

    /// Reconcile tmux windows against state: fill in window/pane ids and
    /// reconstruct features that exist on disk but not in state.
    pub fn reconcile(&mut self, tmux: &Tmux) -> Result<()> {
        let windows = tmux.list_windows()?;
        for w in &windows {
            if !is_lazywingman_window(&w.window_name) {
                continue;
            }
            let Some((pname, fname, idx)) = parse_window_name(&w.window_name) else {
                continue;
            };
            // Match the project by sanitised name (window names are built
            // from the project's display name through the same sanitise).
            let proj_idx = self
                .projects
                .iter()
                .position(|p| sanitize_name(&p.name) == pname);
            let Some(proj_idx) = proj_idx else {
                // Project not in state — try reconstructing from the pane's
                // cwd (the worktree path) and its git metadata.
                if let Some(path) = pane_cwd(tmux, &w.pane_id) {
                    self.reconstruct_feature(&path, &pname, &fname, idx);
                }
                continue;
            };
            let proj = &mut self.projects[proj_idx];
            // Features are matched by their branch slug.
            let feat_idx = proj
                .features
                .iter()
                .position(|f| sanitize_name(&f.branch) == fname);
            let Some(fi) = feat_idx else {
                // Feature state lost but window alive: reconstruct from disk.
                if let Some(path) = pane_cwd(tmux, &w.pane_id) {
                    self.reconstruct_feature(&path, &pname, &fname, idx);
                }
                continue;
            };
            let feat = &mut proj.features[fi];
            if let Some(shell) = feat.shells.iter_mut().find(|s| s.index == idx) {
                shell.window_id = Some(w.window_id.clone());
                shell.pane_id = Some(w.pane_id.clone());
            } else {
                feat.shells.push(Shell {
                    index: idx,
                    window_name: w.window_name.clone(),
                    window_id: Some(w.window_id.clone()),
                    pane_id: Some(w.pane_id.clone()),
                });
                feat.shells.sort_by_key(|s| s.index);
            }
        }
        Ok(())
    }

    /// Rebuild a feature entry from a live window whose state was lost
    /// (e.g. state file deleted). The worktree path is the pane's cwd; the
    /// branch comes from git; the owning project from the worktree's main
    /// repo. The feature name is the sanitised window-name component.
    fn reconstruct_feature(
        &mut self,
        worktree_path: &std::path::Path,
        pname: &str,
        fname: &str,
        idx: usize,
    ) {
        let Some(main_repo) = crate::git::main_repo_of_worktree(worktree_path) else {
            log::warn!(
                "cannot find main repo for worktree {}",
                worktree_path.display()
            );
            return;
        };
        let Some(proj_idx) = self.projects.iter().position(|p| p.path == main_repo) else {
            log::info!(
                "window {pname}/{fname} belongs to unknown project, skipping reconstruction"
            );
            return;
        };
        let Some(branch) = Git.current_branch(worktree_path).ok() else {
            log::warn!("cannot resolve branch for {}", worktree_path.display());
            return;
        };
        log::info!("reconstructing feature {fname} (branch {branch})");
        self.projects[proj_idx].features.push(Feature {
            name: fname.to_string(),
            branch,
            worktree_path: worktree_path.to_path_buf(),
            created_at: String::new(),
            shells: vec![Shell {
                index: idx,
                window_name: format!("lw:{pname}/{fname}/{idx}"),
                window_id: None,
                pane_id: None,
            }],
        });
    }

    /// Prune state entries that no longer match reality. When `tmux` is
    /// given, shells whose window has vanished are dropped too.
    pub fn prune(&mut self, tmux: Option<&Tmux>) -> Result<()> {
        let git = Git;
        let live_windows: Option<std::collections::HashSet<String>> =
            match tmux.and_then(|t| t.list_windows().ok()) {
                Some(ws) => Some(ws.into_iter().map(|w| w.window_id).collect()),
                None => None,
            };
        let mut removed_projects = 0;
        let mut removed_features = 0;
        let mut removed_shells = 0;
        for proj in &mut self.projects {
            if !git.is_repo(&proj.path) {
                removed_projects += 1;
                continue;
            }
            let Ok(worktrees) = git.worktree_list(&proj.path) else {
                continue;
            };
            let wt_paths: Vec<PathBuf> = worktrees.into_iter().map(|w| w.path).collect();
            proj.features.retain(|f| {
                let keep = wt_paths.contains(&f.worktree_path);
                if !keep {
                    log::info!(
                        "pruning stale feature '{}' (worktree {} gone)",
                        f.name,
                        f.worktree_path.display()
                    );
                    removed_features += 1;
                }
                keep
            });
            for feat in &mut proj.features {
                if let Some(live) = &live_windows {
                    let before = feat.shells.len();
                    feat.shells.retain(|s| match &s.window_id {
                        Some(wid) => live.contains(wid),
                        None => true, // reconstructed, not yet matched
                    });
                    removed_shells += before - feat.shells.len();
                }
            }
        }
        self.projects.retain(|p| {
            let keep = git.is_repo(&p.path);
            if !keep {
                log::info!("pruning project '{}' (not a repo anymore)", p.name);
                removed_projects += 1;
            }
            keep
        });
        if removed_projects + removed_features + removed_shells > 0 {
            log::info!(
                "pruned {removed_projects} projects, {removed_features} features, {removed_shells} shells"
            );
        }
        Ok(())
    }
}

/// Sanitise a name the same way window names are built.
pub fn sanitize_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/') {
            out.push(c);
        } else if c == ' ' {
            out.push('-');
        }
    }
    out
}

/// Parse `lw:<project>/<feature>/<index>`.
pub fn parse_window_name(name: &str) -> Option<(String, String, usize)> {
    let rest = name.strip_prefix("lw:")?;
    let (p, rest) = rest.split_once('/')?;
    let (f, idx) = rest.rsplit_once('/')?;
    Some((p.to_string(), f.to_string(), idx.parse().ok()?))
}

fn pane_cwd(tmux: &Tmux, pane_id: &str) -> Option<PathBuf> {
    if pane_id.is_empty() {
        return None;
    }
    let out = tmux
        .run_raw(&[
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{pane_current_path}",
        ])
        .ok()?;
    let p = out.trim();
    if p.is_empty() {
        None
    } else {
        Some(PathBuf::from(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_name_roundtrip() {
        let name = Tmux::window_name("my project", "feat-one", 2);
        assert_eq!(name, "lw:my-project/feat-one/2");
        let (p, f, i) = parse_window_name(&name).unwrap();
        assert_eq!(p, "my-project");
        assert_eq!(f, "feat-one");
        assert_eq!(i, 2);
        assert!(parse_window_name("0:bash").is_none());
        assert!(parse_window_name("lw:a/b/x").is_none());
    }

    #[test]
    fn state_json_roundtrip() {
        let mut s = State::default();
        s.projects.push(Project {
            name: "proj".into(),
            path: PathBuf::from("/tmp/proj"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            features: vec![Feature {
                name: "feat".into(),
                branch: "feat".into(),
                worktree_path: PathBuf::from("/tmp/wt/feat"),
                created_at: "now".into(),
                shells: vec![Shell {
                    index: 0,
                    window_name: "lw:proj/feat/0".into(),
                    window_id: Some("@5".into()),
                    pane_id: Some("%9".into()),
                }],
            }],
        });
        let json = serde_json::to_string_pretty(&s).unwrap();
        let back: State = serde_json::from_str(&json).unwrap();
        assert_eq!(back.projects.len(), 1);
        assert_eq!(
            back.projects[0].features[0].shells[0].window_id.as_deref(),
            Some("@5")
        );
    }

    #[test]
    fn save_load_roundtrip() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut s = State::default();
            s.projects.push(Project {
                name: "p".into(),
                path: PathBuf::from("/tmp/p"),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                features: vec![],
            });
            s.save().unwrap();
            let loaded = State::load().unwrap();
            assert_eq!(loaded.projects.len(), 1);
            assert_eq!(loaded.projects[0].name, "p");
        });
    }

    #[test]
    fn missing_state_file_is_empty() {
        crate::util::testutil::with_fresh_xdg(|| {
            let s = State::load().unwrap();
            assert!(s.projects.is_empty());
        });
    }

    #[test]
    fn corrupt_state_is_an_error() {
        crate::util::testutil::with_fresh_xdg(|| {
            std::fs::create_dir_all(crate::util::state_dir()).unwrap();
            std::fs::write(crate::util::state_path(), "{not json").unwrap();
            assert!(State::load().is_err());
        });
    }

    #[test]
    fn sanitize_matches_window_names() {
        assert_eq!(sanitize_name("My Project"), "My-Project");
        assert_eq!(sanitize_name("feat/one"), "feat/one");
    }
}
