//! Persistent state: repositories and worktrees.
//!
//! Stored as JSON in `$XDG_STATE_HOME/lazywingman/state.json`. The file holds
//! only what no other system records: the repositories in scope and the
//! metadata of their worktrees. Everything tmux owns — shells, their commands,
//! agent sessions — lives on tmux options and is rebuilt by
//! [`State::reconcile`], so the file cannot disagree with the server.
//!
//! A file of any other version is discarded and rediscovered.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::git::Git;
use crate::tmux::{SESSION_PREFIX, SessionInfo, Tmux};

pub const STATE_VERSION: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub version: u32,
    pub repositories: Vec<Repository>,
}

impl Default for State {
    fn default() -> State {
        State {
            version: STATE_VERSION,
            repositories: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub name: String,
    pub path: PathBuf,
    /// Base branch resolved at import time.
    pub base_branch: String,
    /// Resolved agent command/shell (from config or defaults).
    pub agent_command: String,
    pub agent_shell: String,
    /// The path is currently not a git repo. Kept rather than pruned; flips
    /// back when the path is a repo again.
    #[serde(default)]
    pub missing: bool,
    /// The repository's inline shell (a login shell in the main checkout),
    /// spawned lazily on the first `Enter` on the row. Rebuilt from tmux.
    #[serde(skip)]
    pub shell: Option<Shell>,
    pub worktrees: Vec<Worktree>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub name: String,
    /// Slugified name == git branch == worktree directory.
    pub branch: String,
    pub path: PathBuf,
    pub created_at: String,
    /// The base-branch commit this worktree was cut from. A merged branch and
    /// a fresh cut both have an empty `base..HEAD`; only the fork point tells
    /// them apart. `None` for worktrees LazyWingman did not create.
    #[serde(default)]
    pub base_commit: Option<String>,
    /// The agent command picked in the new-worktree popup; `None` means
    /// "whatever the repository runs".
    #[serde(default)]
    pub agent_command: Option<String>,
    /// Rebuilt from tmux.
    #[serde(skip)]
    pub shells: Vec<Shell>,
}

impl Worktree {
    /// Index for the next shell: one past the highest live one, so it cannot
    /// collide with a pane already stamped with it.
    pub fn next_shell_index(&self) -> usize {
        self.shells.iter().map(|s| s.index + 1).max().unwrap_or(0)
    }
}

/// A live shell, as read back from its pane. Never persisted.
#[derive(Debug, Clone)]
pub struct Shell {
    pub index: usize,
    /// The command the shell was started with (`@lw_command`). Launched, not
    /// running: `#{pane_current_command}` reads `node` for Claude Code and
    /// `zsh` once an agent exits. `None` for a pane spawned by an older build.
    pub command: Option<String>,
    /// Human-facing label (`lw:<worktree>/<index>`), not an identity.
    pub window_name: String,
    /// Where the pane was at the last reconcile.
    pub window_id: String,
    pub pane_id: String,
}

impl Shell {
    /// How the tree names this shell: the program it was started with, or
    /// `#<index>` when none was recorded. First token, basenamed — a row
    /// shares its line with the status.
    pub fn label(&self) -> String {
        self.command
            .as_deref()
            .and_then(|c| c.split_whitespace().next())
            .and_then(|c| c.rsplit('/').next())
            .filter(|c| !c.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("#{}", self.index))
    }
}

impl State {
    /// Load from disk; missing file → empty state.
    pub fn load() -> Result<State> {
        let path = crate::util::state_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                // Version first: an older schema may not deserialise at all.
                // A well-formed file of another version is discarded and
                // rediscovered; malformed JSON is an error so the user is told
                // instead of silently losing the tree.
                let raw: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| Error::State(format!("{}: {e}", path.display())))?;
                let version = raw.get("version").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                if version != STATE_VERSION {
                    log::warn!(
                        "state v{version} is not v{STATE_VERSION} — discarding {} and starting empty",
                        path.display()
                    );
                    return Ok(State::default());
                }
                let state: State = serde_json::from_value(raw)
                    .map_err(|e| Error::State(format!("{}: {e}", path.display())))?;
                Ok(state)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// Persist to disk, atomically.
    ///
    /// Two dashboards share this file: last-writer-wins is survivable, a
    /// half-written file is not (`load` fails on it for every instance). So
    /// write a pid-named temporary beside the target and rename over it.
    pub fn save(&self) -> Result<()> {
        let path = crate::util::state_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| Error::State(e.to_string()))?;
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, text)?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::Io(e));
        }
        log::info!("state saved: {} repositories", self.repositories.len());
        Ok(())
    }

    /// Rebuild every shell from the panes, and reconstruct worktrees that
    /// have a live shell but no entry.
    ///
    /// The unit is the pane: it carries its own [`crate::dock::ShellId`], so a
    /// shell is found wherever it sits (including docked in a dashboard) and a
    /// window renamed by hand costs nothing. A shell whose pane is gone simply
    /// is not rebuilt.
    pub fn reconcile(&mut self, tmux: &Tmux) -> Result<()> {
        let panes = tmux.list_panes()?;
        let sessions = tmux.list_sessions()?;
        for repo in &mut self.repositories {
            repo.shell = None;
            for wt in &mut repo.worktrees {
                wt.shells.clear();
            }
        }
        for pane in panes {
            let Some(id) = pane.shell.clone() else {
                continue;
            };
            // The identity names the owning session, which is not necessarily
            // the one the pane sits in now.
            let session = format!("{SESSION_PREFIX}{}", id.repo);
            let owner = self.repo_for_session(
                &session,
                sessions
                    .iter()
                    .find(|s| s.name == session)
                    .and_then(|s| s.repo_path.as_deref()),
            );
            let shell = Shell {
                index: id.index,
                command: pane.command.clone(),
                window_name: Tmux::window_name(&id.worktree, id.index),
                window_id: pane.window_id.clone(),
                pane_id: pane.pane_id.clone(),
            };
            if id.is_repo_shell() {
                // No worktree to reconstruct: inventing one from the main
                // checkout would list the repository as its own worktree.
                if let Some(ri) = owner {
                    self.repositories[ri].shell = Some(shell);
                }
                continue;
            }
            let known = owner.and_then(|ri| {
                self.repositories[ri]
                    .worktrees
                    .iter()
                    .position(|f| sanitize_name(&f.branch) == id.worktree)
                    .map(|wi| (ri, wi))
            });
            let Some((ri, wi)) = known.or_else(|| {
                let path = pane_cwd(tmux, &pane.pane_id)?;
                self.reconstruct_worktree(&path, &id.worktree)
            }) else {
                continue;
            };
            let shells = &mut self.repositories[ri].worktrees[wi].shells;
            // Two panes claiming one shell: the last listed wins.
            shells.retain(|s| s.index != id.index);
            shells.push(shell);
            shells.sort_by_key(|s| s.index);
        }
        Ok(())
    }

    /// The live agent session of repository `ri`, if any.
    pub fn session_of(&self, sessions: &[SessionInfo], ri: usize) -> Option<String> {
        sessions
            .iter()
            .filter(|s| s.name.starts_with(SESSION_PREFIX))
            .find(|s| self.repo_for_session(&s.name, s.repo_path.as_deref()) == Some(ri))
            .map(|s| s.name.clone())
    }

    /// Which repository owns agent session `session`: by the path stamped on
    /// the session, else — a session from an older build, or one already gone —
    /// by the slug, hash-suffixed first because that form is unambiguous.
    fn repo_for_session(&self, session: &str, repo_path: Option<&Path>) -> Option<usize> {
        if let Some(path) = repo_path {
            return self
                .repositories
                .iter()
                .position(|r| r.path.as_path() == path);
        }
        let slug = session.strip_prefix(SESSION_PREFIX)?;
        self.repositories
            .iter()
            .position(|r| {
                format!("{}-{}", sanitize_name(&r.name), Tmux::hash_suffix(&r.path)) == slug
            })
            .or_else(|| {
                self.repositories
                    .iter()
                    .position(|r| sanitize_name(&r.name) == slug)
            })
    }

    /// Find or rebuild the worktree at `path` (a live pane's cwd) under the
    /// repository that owns it. `fname` is the pane's worktree component.
    fn reconstruct_worktree(&mut self, path: &Path, fname: &str) -> Option<(usize, usize)> {
        let Some(main_repo) = crate::git::main_repo_of_worktree(path) else {
            log::warn!("cannot find main repo for worktree {}", path.display());
            return None;
        };
        let Some(ri) = self.repositories.iter().position(|p| p.path == main_repo) else {
            log::info!("worktree {fname} belongs to unknown repository, skipping reconstruction");
            return None;
        };
        let worktrees = &mut self.repositories[ri].worktrees;
        if let Some(wi) = worktrees.iter().position(|w| w.path == path) {
            return Some((ri, wi));
        }
        let Ok(branch) = Git.current_branch(path) else {
            log::warn!("cannot resolve branch for {}", path.display());
            return None;
        };
        log::info!("reconstructing worktree {fname} (branch {branch})");
        worktrees.push(Worktree {
            name: fname.to_string(),
            branch,
            path: path.to_path_buf(),
            created_at: String::new(),
            base_commit: None,
            agent_command: None,
            shells: Vec::new(),
        });
        Some((ri, worktrees.len() - 1))
    }

    /// Prune worktrees that no longer match reality.
    ///
    /// Repositories are never dropped: one whose path is not a repo is marked
    /// `missing` and keeps its worktrees, which cannot be verified while gone.
    ///
    /// `verify_worktrees` is index-parallel to `repositories`: only
    /// repositories whose worktrees were loaded this run are checked against
    /// `git worktree list` — verifying 200 repos at boot is the cost lazy
    /// loading exists to avoid.
    pub fn prune(&mut self, verify_worktrees: &[bool]) {
        let git = Git;
        let mut removed = 0;
        for (idx, repo) in self.repositories.iter_mut().enumerate() {
            if !git.is_repo(&repo.path) {
                if !repo.missing {
                    repo.missing = true;
                    log::info!(
                        "repository '{}' marked missing ({} is not a git repo)",
                        repo.name,
                        repo.path.display()
                    );
                }
                continue;
            }
            if repo.missing {
                repo.missing = false;
                log::info!(
                    "repository '{}' re-activated ({} is a git repo again)",
                    repo.name,
                    repo.path.display()
                );
            }
            if !verify_worktrees.get(idx).copied().unwrap_or(true) {
                continue;
            }
            let Ok(worktrees) = git.worktree_list(&repo.path) else {
                log::warn!(
                    "worktree list failed for {} — leaving worktrees untouched",
                    repo.path.display()
                );
                continue;
            };
            let wt_paths: Vec<PathBuf> = worktrees.into_iter().map(|w| w.path).collect();
            repo.worktrees.retain(|f| {
                let keep = wt_paths.contains(&f.path);
                if !keep {
                    log::info!(
                        "pruning stale worktree '{}' (worktree {} gone)",
                        f.name,
                        f.path.display()
                    );
                    removed += 1;
                }
                keep
            });
        }
        if removed > 0 {
            log::info!("pruned {removed} worktrees");
        }
    }
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
    use crate::dock::ShellId;

    fn repository(name: &str, path: &str) -> Repository {
        Repository {
            name: name.into(),
            path: PathBuf::from(path),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            missing: false,
            shell: None,
            worktrees: vec![],
        }
    }

    fn worktree(name: &str, path: &str) -> Worktree {
        Worktree {
            name: name.into(),
            branch: name.into(),
            path: PathBuf::from(path),
            created_at: String::new(),
            base_commit: None,
            agent_command: None,
            shells: vec![],
        }
    }

    fn shell(index: usize) -> Shell {
        Shell {
            index,
            command: None,
            window_name: format!("lw:wt/{index}"),
            window_id: format!("@{index}"),
            pane_id: format!("%{index}"),
        }
    }

    #[test]
    fn next_shell_index_counts_past_the_highest_live_shell() {
        let mut wt = worktree("wt", "/tmp/wt");
        assert_eq!(wt.next_shell_index(), 0);
        wt.shells = vec![shell(0), shell(2)];
        assert_eq!(wt.next_shell_index(), 3);
    }

    #[test]
    fn window_name_is_the_human_facing_label() {
        assert_eq!(crate::tmux::Tmux::window_name("wt-one", 2), "lw:wt-one/2");
    }

    /// Nothing tmux owns reaches the file.
    #[test]
    fn state_json_holds_no_tmux_state() {
        let mut s = State::default();
        let mut repo = repository("repo", "/tmp/repo");
        repo.shell = Some(shell(0));
        let mut wt = worktree("wt", "/tmp/wt/wt");
        wt.shells = vec![shell(5)];
        repo.worktrees.push(wt);
        s.repositories.push(repo);
        let json = serde_json::to_string_pretty(&s).unwrap();
        for leak in ["\"shell\"", "shells", "session", "theme", "@5", "%5", "lw:"] {
            assert!(!json.contains(leak), "{leak} must not be written: {json}");
        }
        let back: State = serde_json::from_str(&json).unwrap();
        assert!(back.repositories[0].shell.is_none());
        assert!(back.repositories[0].worktrees[0].shells.is_empty());
    }

    /// A file written by an older build still loads; its tmux fields are ignored.
    #[test]
    fn a_file_with_retired_fields_still_loads() {
        let old = r#"{"version":4,"theme":"dracula","repositories":[{"name":"p","path":"/tmp/p","base_branch":"main","agent_command":"claude","agent_shell":"bash","session_name":"wingman-session-p","shell":{"index":0,"window_name":"lw:.repo/0"},"worktrees":[{"name":"wt","branch":"wt","path":"/tmp/wt","created_at":"","shells":[{"index":0,"window_name":"lw:wt/0"}]}]}]}"#;
        let s: State = serde_json::from_str(old).unwrap();
        assert!(s.repositories[0].shell.is_none());
        assert!(s.repositories[0].worktrees[0].shells.is_empty());
    }

    #[test]
    fn a_v3_file_is_discarded_rather_than_loaded() {
        crate::util::testutil::with_fresh_xdg(|| {
            std::fs::create_dir_all(crate::util::state_dir()).unwrap();
            let v3 = r#"{"version":3,"repositories":[{"name":"p","path":"/tmp/p","base_branch":"main","agent_command":"claude","agent_shell":"bash","worktrees":[]}],"theme":null}"#;
            std::fs::write(crate::util::state_path(), v3).unwrap();
            let s = State::load().unwrap();
            assert!(s.repositories.is_empty(), "v3 is not loaded");
            assert_eq!(s.version, STATE_VERSION);
        });
    }

    #[test]
    fn missing_optional_fields_default() {
        let v1 = r#"{"repositories":[{"name":"p","path":"/tmp/p","base_branch":"main","agent_command":"claude","agent_shell":"bash","worktrees":[]}]}"#;
        let s: State = serde_json::from_str(v1).unwrap();
        assert_eq!(s.version, 0);
        assert!(!s.repositories[0].missing);
        assert!(s.repositories[0].worktrees.is_empty());
    }

    #[test]
    fn sanitize_matches_session_names() {
        assert_eq!(sanitize_name("My Repository"), "My-Repository");
        assert_eq!(sanitize_name("wt/one"), "wt/one");
    }

    fn git_repo(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]);
        run_git(&repo, &["config", "user.name", "lw-test"]);
        run_git(&repo, &["config", "user.email", "lw-test@example.com"]);
        std::fs::write(repo.join("readme.md"), "# t\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-m", "init"]);
        repo
    }

    #[test]
    fn prune_keeps_missing_repositories_and_reactivates_them() {
        let root = tempfile::TempDir::new().unwrap();
        let repo = git_repo(root.path());
        let mut s = State::default();
        s.repositories
            .push(repository("repo", repo.to_str().unwrap()));
        s.prune(&[true]);
        assert!(!s.repositories[0].missing, "active repo stays active");

        std::fs::remove_dir_all(&repo).unwrap();
        s.prune(&[true]);
        assert_eq!(
            s.repositories.len(),
            1,
            "missing repository is never pruned"
        );
        assert!(s.repositories[0].missing);

        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]);
        s.prune(&[true]);
        assert_eq!(s.repositories.len(), 1);
        assert!(!s.repositories[0].missing, "re-activated");
    }

    #[test]
    fn prune_drops_gone_worktrees_but_keeps_those_of_missing_repositories() {
        let root = tempfile::TempDir::new().unwrap();
        let repo = git_repo(root.path());
        let mut s = State::default();
        let mut r = repository("repo", repo.to_str().unwrap());
        r.worktrees
            .push(worktree("wt", repo.join("../wt-wt").to_str().unwrap()));
        s.repositories.push(r);
        s.prune(&[true]);
        assert!(
            s.repositories[0].worktrees.is_empty(),
            "gone worktree pruned"
        );

        s.repositories[0].worktrees.push(worktree(
            "feat2",
            repo.join("../wt-feat2").to_str().unwrap(),
        ));
        std::fs::remove_dir_all(&repo).unwrap();
        s.prune(&[true]);
        assert!(s.repositories[0].missing);
        assert_eq!(s.repositories[0].worktrees.len(), 1, "kept while missing");
    }

    #[test]
    fn a_repository_shell_reconciles_onto_its_repository() {
        crate::tmux::testutil::with_tmux_server(|_sock, tmux| {
            let id = ShellId::for_session(
                "wingman-session-p",
                crate::dock::REPO_WORKTREE,
                crate::dock::REPO_INDEX,
            );
            assert!(id.is_repo_shell());
            let w = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:.repo/0",
                    Path::new("/tmp"),
                    "bash",
                    &id,
                    crate::tmux::testutil::HOLDER_W,
                )
                .expect("the repository's shell window");

            let mut s = State::default();
            s.repositories.push(repository("p", "/tmp/p"));
            s.reconcile(&tmux).expect("reconcile");

            let shell = s.repositories[0]
                .shell
                .as_ref()
                .expect("the live pane is adopted onto the repository");
            assert_eq!(shell.pane_id, w.pane_id);
            assert!(
                s.repositories[0].worktrees.is_empty(),
                "no worktree invented"
            );
        });
    }

    /// Shells are rebuilt, not merged: a killed pane leaves no row behind, and
    /// the launched command comes back off the pane.
    #[test]
    fn reconcile_rebuilds_shells_from_the_panes() {
        crate::tmux::testutil::with_tmux_server(|_sock, tmux| {
            let a = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:wt/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "wt", 0),
                    crate::tmux::testutil::HOLDER_W,
                )
                .expect("session");
            let b = tmux
                .new_window(
                    "wingman-session-p",
                    "lw:wt/1",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "wt", 1),
                    crate::tmux::testutil::HOLDER_W,
                )
                .expect("window");
            tmux.stamp_command(&a.pane_id, "codex exec | tee log")
                .expect("stamp");

            let mut s = State::default();
            let mut r = repository("p", "/tmp/p");
            r.worktrees.push(worktree("wt", "/tmp/p-wt"));
            s.repositories.push(r);
            s.reconcile(&tmux).expect("reconcile");
            let shells = &s.repositories[0].worktrees[0].shells;
            assert_eq!(shells.len(), 2, "{shells:?}");
            assert_eq!(shells[0].command.as_deref(), Some("codex exec | tee log"));
            assert_eq!(shells[1].command, None, "no command stamped");

            tmux.kill_window(&b.window_id).expect("kill");
            s.reconcile(&tmux).expect("reconcile again");
            let shells = &s.repositories[0].worktrees[0].shells;
            assert_eq!(shells.len(), 1, "the killed shell is gone: {shells:?}");
            assert_eq!(shells[0].pane_id, a.pane_id);
        });
    }

    /// Two repositories with one name: the path stamped on each session is
    /// what tells them apart, whichever got the unsuffixed name.
    #[test]
    fn sessions_map_to_repositories_by_their_stamped_path() {
        crate::tmux::testutil::with_tmux_server(|_sock, tmux| {
            for (session, path) in [
                ("wingman-session-app", "/work/b/app"),
                ("wingman-session-app-0000beef", "/work/a/app"),
            ] {
                tmux.new_session(
                    session,
                    "lw:.repo/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session(
                        session,
                        crate::dock::REPO_WORKTREE,
                        crate::dock::REPO_INDEX,
                    ),
                    crate::tmux::testutil::HOLDER_W,
                )
                .expect("session");
                tmux.stamp_session_repo(session, Path::new(path))
                    .expect("stamp");
            }
            let mut s = State::default();
            s.repositories.push(repository("app", "/work/a/app"));
            s.repositories.push(repository("app", "/work/b/app"));
            let sessions = tmux.list_sessions().expect("sessions");
            assert_eq!(
                s.session_of(&sessions, 0).as_deref(),
                Some("wingman-session-app-0000beef")
            );
            assert_eq!(
                s.session_of(&sessions, 1).as_deref(),
                Some("wingman-session-app")
            );
            s.reconcile(&tmux).expect("reconcile");
            assert!(s.repositories.iter().all(|r| r.shell.is_some()));
        });
    }

    fn run_git(dir: &Path, args: &[&str]) {
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
            s.repositories.push(repository("p", "/tmp/p"));
            s.save().unwrap();
            let loaded = State::load().unwrap();
            assert_eq!(loaded.repositories.len(), 1);
            assert_eq!(loaded.repositories[0].name, "p");
            assert_eq!(loaded.version, STATE_VERSION);
        });
    }

    /// Identity lives on the pane, so renaming every agent window costs the
    /// tree nothing.
    #[test]
    fn rediscovery_survives_windows_renamed_by_hand() {
        crate::tmux::testutil::with_tmux_server(|_sock, tmux| {
            let a = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:wt/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "wt", 0),
                    crate::tmux::testutil::HOLDER_W,
                )
                .expect("session");
            let b = tmux
                .new_window(
                    "wingman-session-p",
                    "lw:wt/1",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "wt", 1),
                    crate::tmux::testutil::HOLDER_W,
                )
                .expect("window");
            for w in [&a, &b] {
                tmux.run_raw(&["rename-window", "-t", &w.window_id, "not-ours"])
                    .expect("rename");
            }

            let mut s = State::default();
            let mut r = repository("p", "/tmp/p");
            r.worktrees.push(worktree("wt", "/tmp/p-wt"));
            s.repositories.push(r);
            s.reconcile(&tmux).expect("reconcile");

            let shells = &s.repositories[0].worktrees[0].shells;
            assert_eq!(shells.len(), 2, "both shells rediscovered: {shells:?}");
            assert_eq!(shells[0].pane_id, a.pane_id);
            assert_eq!(shells[1].pane_id, b.pane_id);
        });
    }

    /// Furniture is never a shell, whatever identity is written onto it: a
    /// holder stamped with an agent's identity once made two panes claim one
    /// shell, and the dashboard docked the sleeping placeholder.
    #[test]
    fn a_holder_is_never_mistaken_for_the_shell_it_stands_in_for() {
        crate::tmux::testutil::with_tmux_server(|_sock, tmux| {
            let id = ShellId::for_session("wingman-session-p", "feat", 0);
            let w = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:feat/0",
                    Path::new("/tmp"),
                    "bash",
                    &id,
                    crate::tmux::testutil::HOLDER_W,
                )
                .expect("agent window with its holder");
            let holder = tmux
                .list_furniture()
                .expect("furniture")
                .into_iter()
                .find(|p| p.holder)
                .expect("the window has a holder");

            let mut s = State::default();
            let mut r = repository("p", "/tmp/p");
            r.worktrees.push(worktree("feat", "/tmp/p-feat"));
            s.repositories.push(r);

            tmux.stamp_shell(&holder.pane_id, &id).expect("stamp it");
            s.reconcile(&tmux).expect("reconcile");

            let shells = &s.repositories[0].worktrees[0].shells;
            assert_eq!(shells.len(), 1, "still exactly one shell: {shells:?}");
            assert_eq!(
                shells[0].pane_id, w.pane_id,
                "the agent, not the placeholder"
            );

            s.reconcile(&tmux).expect("reconcile again");
            assert_eq!(s.repositories[0].worktrees[0].shells.len(), 1);
        });
    }

    #[test]
    fn save_is_atomic_so_a_torn_file_is_impossible() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut s = State::default();
            s.repositories.push(repository("p", "/tmp/p"));
            s.save().expect("save");

            let path = crate::util::state_path();
            assert!(path.exists());
            State::load().expect("what was written parses");

            let strays: Vec<String> = std::fs::read_dir(crate::util::state_dir())
                .expect("state dir")
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("state.") && n != "state.json")
                .collect();
            assert!(strays.is_empty(), "temporaries left behind: {strays:?}");

            s.repositories[0].name = "renamed".into();
            s.save().expect("save again");
            let back = State::load().expect("still parses");
            assert_eq!(back.repositories[0].name, "renamed");
        });
    }

    #[test]
    fn missing_state_file_is_empty() {
        crate::util::testutil::with_fresh_xdg(|| {
            let s = State::load().unwrap();
            assert!(s.repositories.is_empty());
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
