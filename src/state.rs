//! Persistent state: projects, features (worktrees), shells (tmux windows).
//!
//! Stored as JSON in `$XDG_STATE_HOME/lazywingman/state.json`. On startup the
//! state is reconciled against reality (`git worktree list`,
//! `tmux list-sessions` + per-session `list-windows`) so stale entries
//! self-heal and running shells are rediscovered after a restart.
//!
//! Schema v2: agent shells live in dedicated tmux sessions
//! (`wingman-session-<project>`, one per project) instead of windows in the
//! dashboard's session. v1 files load through serde defaults; v1 shells are
//! pruned on the first v2 reconcile (their windows are not in any agent
//! session), while projects and worktrees survive.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::git::Git;
use crate::tmux::{Tmux, is_lazywingman_session, is_lazywingman_window};

pub const STATE_VERSION: u32 = 2;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub version: u32,
    pub projects: Vec<Project>,
    /// Theme picked in the TUI (ctrl-t). Wins over the config's `theme`
    /// line at startup — config is never rewritten behind the user's back;
    /// in-app changes persist to state per the configuration guidelines.
    #[serde(default)]
    pub theme: Option<String>,
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
    /// The dedicated tmux session holding this project's shells
    /// (`wingman-session-<slug>`), if any has been created.
    #[serde(default)]
    pub session_name: Option<String>,
    /// The project's path is currently not a git repo (moved, unmounted,
    /// deleted). Kept in state (never pruned) and shown "(missing)" in the
    /// tree; flips back to `false` automatically when the path is a repo
    /// again.
    #[serde(default)]
    pub missing: bool,
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
            Ok(text) => {
                let mut state: State = serde_json::from_str(&text)
                    .map_err(|e| Error::State(format!("{}: {e}", path.display())))?;
                if state.version != STATE_VERSION {
                    log::info!(
                        "state v{} → v{} (schema migration)",
                        state.version,
                        STATE_VERSION
                    );
                    state.version = STATE_VERSION;
                }
                Ok(state)
            }
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

    /// Reconcile tmux sessions against state: fill in window/pane ids and
    /// reconstruct features that exist on disk but not in state. Windows are
    /// matched inside the project's dedicated agent session, never in the
    /// dashboard's own session.
    pub fn reconcile(&mut self, tmux: &Tmux) -> Result<()> {
        let sessions = tmux.list_sessions()?;
        for s in &sessions {
            if !is_lazywingman_session(&s.name) {
                continue;
            }
            let slug = s
                .name
                .trim_start_matches(crate::tmux::SESSION_PREFIX)
                .to_string();
            let windows = tmux.list_windows(&s.name)?;
            // Which project owns this session? Exact session-name match
            // first; else the slug matches the project's sanitised name;
            // else a hash-suffixed session; else reconstruct from pane cwd.
            let proj_idx = self
                .projects
                .iter()
                .position(|p| p.session_name.as_deref() == Some(&s.name))
                .or_else(|| {
                    self.projects
                        .iter()
                        .position(|p| sanitize_name(&p.name) == slug)
                })
                .or_else(|| {
                    self.projects.iter().position(|p| {
                        n_contains_slug(p, &slug)
                            && p.session_name
                                .as_deref()
                                .map(|n| {
                                    n.ends_with(&format!(
                                        "-{}",
                                        crate::tmux::Tmux::hash_suffix(&p.path)
                                    ))
                                })
                                .unwrap_or(false)
                    })
                });
            for w in &windows {
                if !is_lazywingman_window(&w.window_name) {
                    continue;
                }
                let Some((fname, idx)) = parse_window_name(&w.window_name) else {
                    continue;
                };
                let Some(pi) = proj_idx else {
                    // Session for an unknown project — try reconstructing
                    // from the pane's cwd (the worktree path) + git metadata.
                    if let Some(path) = pane_cwd(tmux, &w.pane_id) {
                        self.reconstruct_feature(&path, &slug, &fname, idx);
                    }
                    continue;
                };
                let proj = &mut self.projects[pi];
                // Remember the session this project lives in.
                if proj.session_name.as_deref() != Some(&s.name) {
                    proj.session_name = Some(s.name.clone());
                    log::info!("project '{}' → agent session {}", proj.name, s.name);
                }
                // Features are matched by their branch slug.
                let feat_idx = proj
                    .features
                    .iter()
                    .position(|f| sanitize_name(&f.branch) == fname);
                let Some(fi) = feat_idx else {
                    // Feature state lost but window alive: reconstruct.
                    if let Some(path) = pane_cwd(tmux, &w.pane_id) {
                        self.reconstruct_feature(&path, &slug, &fname, idx);
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
                window_name: format!("lw:{fname}/{idx}"),
                window_id: None,
                pane_id: None,
            }],
        });
    }

    /// Prune state entries that no longer match reality. When `tmux` is
    /// given, shells whose window has vanished are dropped too (windows are
    /// looked up across all agent sessions).
    ///
    /// Projects are NEVER dropped here: a project whose path is not a repo
    /// is kept and marked `missing` (shown "(missing)" in the tree); it
    /// re-activates automatically when the path is a repo again. Only
    /// features (worktree gone) and shells (window gone) are pruned.
    pub fn prune(&mut self, tmux: Option<&Tmux>) -> Result<()> {
        let git = Git;
        let live_windows: Option<std::collections::HashSet<String>> = tmux.and_then(|t| {
            let mut set = std::collections::HashSet::new();
            let sessions = t.list_sessions().ok()?;
            for s in sessions
                .into_iter()
                .filter(|s| is_lazywingman_session(&s.name))
            {
                if let Ok(ws) = t.list_windows(&s.name) {
                    set.extend(ws.into_iter().map(|w| w.window_id));
                }
            }
            Some(set)
        });
        let mut removed_features = 0;
        let mut removed_shells = 0;
        for proj in &mut self.projects {
            let is_repo = git.is_repo(&proj.path);
            if !is_repo {
                // Keep the project; mark it missing (re-activates when the
                // path is a repo again).
                if !proj.missing {
                    proj.missing = true;
                    log::info!(
                        "project '{}' marked missing ({} is not a git repo)",
                        proj.name,
                        proj.path.display()
                    );
                }
                // Features/shells cannot be verified while missing — keep
                // them as they are (their windows may still be alive).
            } else {
                if proj.missing {
                    proj.missing = false;
                    log::info!(
                        "project '{}' re-activated ({} is a git repo again)",
                        proj.name,
                        proj.path.display()
                    );
                }
                if let Ok(worktrees) = git.worktree_list(&proj.path) {
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
                } else {
                    log::warn!(
                        "worktree list failed for {} — leaving features untouched",
                        proj.path.display()
                    );
                }
            }
            // If the agent session vanished (crashed server, killed session)
            // but the project still exists, drop the stale name so a new
            // session is created on demand.
            if let Some(sess) = &proj.session_name {
                let alive = tmux.map(|t| t.session_exists(sess)).unwrap_or(false);
                if !alive {
                    log::info!(
                        "agent session {} gone for project '{}', clearing",
                        sess,
                        proj.name
                    );
                    proj.session_name = None;
                }
            }
        }
        if removed_features + removed_shells > 0 {
            log::info!("pruned {removed_features} features, {removed_shells} shells");
        }
        Ok(())
    }
}

/// Does the project's name match the session slug (for hash-suffixed
/// session names)?
fn n_contains_slug(p: &Project, slug: &str) -> bool {
    let n = sanitize_name(&p.name);
    n == slug || n.starts_with(slug) || slug.starts_with(&n)
}

/// Sanitise a name the same way session/window names are built.
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

/// Parse `lw:<feature>/<index>` (project is implied by the session).
pub fn parse_window_name(name: &str) -> Option<(String, usize)> {
    let rest = name.strip_prefix(crate::tmux::WINDOW_PREFIX)?;
    let (f, idx) = rest.rsplit_once('/')?;
    Some((f.to_string(), idx.parse().ok()?))
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
        let name = crate::tmux::Tmux::window_name("feat-one", 2);
        assert_eq!(name, "lw:feat-one/2");
        let (f, i) = parse_window_name(&name).unwrap();
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
            session_name: Some("wingman-session-proj".into()),
            missing: false,
            features: vec![Feature {
                name: "feat".into(),
                branch: "feat".into(),
                worktree_path: PathBuf::from("/tmp/wt/feat"),
                created_at: "now".into(),
                shells: vec![Shell {
                    index: 0,
                    window_name: "lw:feat/0".into(),
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
        assert_eq!(
            back.projects[0].session_name.as_deref(),
            Some("wingman-session-proj")
        );
    }

    #[test]
    fn v1_state_loads_with_defaults() {
        // A v1 state file has no version and no session_name.
        let v1 = r#"{"projects":[{"name":"p","path":"/tmp/p","base_branch":"main","agent_command":"claude","agent_shell":"bash","features":[]}]}"#;
        let s: State = serde_json::from_str(v1).unwrap();
        assert_eq!(s.version, 0);
        assert!(s.projects[0].session_name.is_none());
        assert!(!s.projects[0].missing);
        assert!(s.projects[0].features.is_empty());
    }

    #[test]
    fn sanitize_matches_session_names() {
        assert_eq!(sanitize_name("My Project"), "My-Project");
        assert_eq!(sanitize_name("feat/one"), "feat/one");
    }

    /// A project whose path vanished is KEPT and marked missing; it is
    /// never pruned, and it re-activates when the path is a repo again.
    #[test]
    fn prune_keeps_missing_projects_and_reactivates_them() {
        let root = tempfile::TempDir::new().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]);
        run_git(&repo, &["config", "user.name", "lw-test"]);
        run_git(&repo, &["config", "user.email", "lw-test@example.com"]);
        std::fs::write(repo.join("readme.md"), "# t\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-m", "init"]);

        let mut s = State::default();
        s.projects.push(Project {
            name: "repo".into(),
            path: repo.clone(),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features: vec![],
        });
        s.prune(None).unwrap();
        assert!(!s.projects[0].missing, "active repo stays active");

        // path vanishes → kept, marked missing
        std::fs::remove_dir_all(&repo).unwrap();
        s.prune(None).unwrap();
        assert_eq!(s.projects.len(), 1, "missing project is never pruned");
        assert!(s.projects[0].missing);

        // path is a repo again → re-activated
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]);
        s.prune(None).unwrap();
        assert_eq!(s.projects.len(), 1);
        assert!(!s.projects[0].missing, "re-activated");
    }

    /// Missing projects keep their features (they cannot be verified while
    /// the path is gone; they are not dropped).
    #[test]
    fn prune_keeps_features_of_missing_projects() {
        let root = tempfile::TempDir::new().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]);
        run_git(&repo, &["config", "user.name", "lw-test"]);
        run_git(&repo, &["config", "user.email", "lw-test@example.com"]);
        std::fs::write(repo.join("readme.md"), "# t\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-m", "init"]);

        let mut s = State::default();
        s.projects.push(Project {
            name: "repo".into(),
            path: repo.clone(),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            features: vec![Feature {
                name: "feat".into(),
                branch: "feat".into(),
                worktree_path: repo.join("../wt-feat"),
                created_at: "2026-08-16".into(),
                shells: vec![Shell {
                    index: 0,
                    window_name: "lw:feat/0".into(),
                    window_id: Some("@9".into()),
                    pane_id: Some("%9".into()),
                }],
            }],
        });
        // verify the feature is pruned while the repo is present (worktree
        // is gone)
        s.prune(None).unwrap();
        assert!(s.projects[0].features.is_empty());

        // now make the project missing: a feature added while missing is
        // kept intact
        s.projects[0].features.push(Feature {
            name: "feat2".into(),
            branch: "feat2".into(),
            worktree_path: repo.join("../wt-feat2"),
            created_at: "".into(),
            shells: vec![Shell {
                index: 0,
                window_name: "lw:feat2/0".into(),
                window_id: Some("@10".into()),
                pane_id: Some("%10".into()),
            }],
        });
        std::fs::remove_dir_all(&repo).unwrap();
        s.prune(None).unwrap();
        assert!(s.projects[0].missing);
        assert_eq!(
            s.projects[0].features.len(),
            1,
            "features survive while missing"
        );
        assert_eq!(s.projects[0].features[0].shells.len(), 1);
    }

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {} failed", args.join(" "));
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
                session_name: None,
                missing: false,
                features: vec![],
            });
            s.save().unwrap();
            let loaded = State::load().unwrap();
            assert_eq!(loaded.projects.len(), 1);
            assert_eq!(loaded.projects[0].name, "p");
            assert_eq!(loaded.version, STATE_VERSION);
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
}
