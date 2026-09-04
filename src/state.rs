//! Persistent state: repositories, worktrees, shells (tmux windows).
//!
//! Stored as JSON in `$XDG_STATE_HOME/lazywingman/state.json`. On startup the
//! state is reconciled against reality (`git worktree list`,
//! `tmux list-sessions` + per-session `list-windows`) so stale entries
//! self-heal and running shells are rediscovered after a restart.
//!
//! Schema v4: a shell's tmux coordinates are no longer persisted. A pane
//! carries its own identity (`dock::ShellId`, written onto `@lw_repo` /
//! `@lw_worktree` / `@lw_index`), and `reconcile` re-derives window and pane
//! ids from one `list-panes` — so a pane that has moved is still found, and
//! a stale id can never be believed. The file keeps repositories, worktrees
//! and shell indices; nothing that tmux owns.
//!
//! A file of any older version is discarded and rediscovered, the treatment
//! v3 gave v2.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::git::Git;
use crate::tmux::{SESSION_PREFIX, Tmux};

pub const STATE_VERSION: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub version: u32,
    pub repositories: Vec<Repository>,
    /// Theme picked in the TUI (ctrl-t). Wins over the config's `theme`
    /// line at startup — config is never rewritten behind the user's back;
    /// in-app changes persist to state per the configuration guidelines.
    #[serde(default)]
    pub theme: Option<String>,
}

impl Default for State {
    fn default() -> State {
        State {
            version: STATE_VERSION,
            repositories: Vec::new(),
            theme: None,
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
    /// The dedicated tmux session holding this repository's shells
    /// (`wingman-session-<slug>`), if any has been created.
    #[serde(default)]
    pub session_name: Option<String>,
    /// The repository's path is currently not a git repo (moved, unmounted,
    /// deleted). Kept in state (never pruned) and shown "(missing)" in the
    /// tree; flips back to `false` automatically when the path is a repo
    /// again.
    #[serde(default)]
    pub missing: bool,
    /// The repository's own inline shell: a plain login shell in the main
    /// checkout (`path`, on the base branch), for the one-off commands a
    /// repository row invites — `make`, `mise run …`, `cat config.toml`.
    ///
    /// Spawned **lazily**, on the first `Enter` on the repository row: a
    /// shell costs two or three seconds, and most repository rows are only
    /// ever expanded to look at the worktrees below them.
    #[serde(default)]
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
    /// The base-branch commit this worktree was cut from, recorded at
    /// creation. It is what makes "no commits of its own" answerable: once
    /// a branch is merged, `base..HEAD` is empty exactly as it is for a
    /// fresh cut, so the fork point is the only thing that tells the two
    /// apart. `None` for worktrees LazyWingman did not create (reconciled
    /// from `git worktree list`) or written by an older build — those fall
    /// back to measuring against the base branch itself, which reads a
    /// merged worktree as new.
    #[serde(default)]
    pub base_commit: Option<String>,
    pub shells: Vec<Shell>,
}

impl Worktree {
    /// Index for the next shell: one past the highest in use, never a
    /// freed slot. The index names the tmux window and is what
    /// [`State::reconcile`] matches a live window back to, so a closed
    /// shell's number must not be handed out again while the worktree
    /// lives.
    pub fn next_shell_index(&self) -> usize {
        self.shells.iter().map(|s| s.index + 1).max().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shell {
    pub index: usize,
    /// The command this shell was started with (`claude`, `zsh`, …) — what
    /// the tree names the row by. It is what was *launched*, not what is
    /// running now: `#{pane_current_command}` reads `node` for a Claude
    /// Code session and `zsh` again the moment an agent exits, so a live
    /// reading would rename rows behind the user's back.
    ///
    /// `None` for a shell adopted from tmux rather than spawned here (state
    /// file deleted, another instance's shell) and for one written by an
    /// older build: nothing recorded the command, and the pane cannot be
    /// asked. Those rows fall back to `#<index>`.
    #[serde(default)]
    pub command: Option<String>,
    /// Human-facing label (`lw:<worktree>/<index>`). A label, not an
    /// identity: the pane's options are the identity, and the window may be
    /// renamed by hand without costing the shell anything.
    pub window_name: String,
    /// Where the pane is right now — filled in by [`State::reconcile`] from
    /// `list-panes`, never read back off disk. A docked pane's window is the
    /// dashboard's, and a coordinate that survived a restart would name a
    /// window that no longer holds this shell (or holds someone else's).
    #[serde(skip)]
    pub window_id: Option<String>,
    #[serde(skip)]
    pub pane_id: Option<String>,
}

impl Shell {
    /// How the tree names this shell: the program it was started with
    /// (`claude`, `zsh`), falling back to `#<index>` when no command was
    /// recorded.
    ///
    /// The first token, basenamed — arguments and the path are dropped.
    /// A row has one line to share with the status, so a label that grew
    /// with `codex exec --yolo` would push what the agent is waiting for
    /// off the panel; the program is what tells two rows apart, the flags
    /// are not.
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
                // Read the version before the state itself: v3 renamed the
                // on-disk keys (`projects`/`features` → `repositories`/
                // `worktrees`) with no migration path, so an older file
                // cannot be deserialised at all. A well-formed file of the
                // wrong version is discarded rather than treated as corrupt
                // — reconcile rediscovers the repositories and reconstructs
                // their worktrees from tmux and git. Malformed JSON is still
                // an error: the user is told instead of silently losing the
                // tree.
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

    /// Persist to disk (pretty-printed), **atomically**.
    ///
    /// Two dashboards share this file and both write it, so last-writer-wins
    /// is the rule. That is survivable: everything live is re-derived from
    /// tmux on the next reconcile, so the loser loses nothing a poll does
    /// not put back. A **half-written** file is not survivable — `load`
    /// treats unparseable JSON as a hard error, so one badly-timed write
    /// would take the tree down for every instance and stay broken across
    /// restarts.
    ///
    /// So: write a temporary file beside the target, then `rename` over it.
    /// Same directory, because a rename is only atomic within a filesystem;
    /// the pid in the name, because two instances must not share the
    /// temporary either. A failed write leaves the previous state intact
    /// and the temporary behind, which is the right way round.
    pub fn save(&self) -> Result<()> {
        let path = crate::util::state_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| Error::State(e.to_string()))?;
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, text)?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            // Do not leave the temporary lying around when the swap is the
            // thing that failed.
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::Io(e));
        }
        log::info!("state saved: {} repositories", self.repositories.len());
        Ok(())
    }

    /// Reconcile tmux against state: fill in the window/pane ids of live
    /// shells and reconstruct worktrees that exist on disk but not in state.
    ///
    /// The unit of reconciliation is the **pane**, not the window: a pane
    /// carries its own [`ShellId`], so a shell is found wherever it lives
    /// (including docked in the dashboard's own session) and a window
    /// renamed by hand costs nothing.
    pub fn reconcile(&mut self, tmux: &Tmux) -> Result<()> {
        for pane in tmux.list_panes()? {
            let Some(id) = pane.shell.clone() else {
                continue;
            };
            // The identity names the session that owns the shell, which is
            // not necessarily the session the pane is sitting in now.
            let session = format!("{SESSION_PREFIX}{}", id.repo);
            let Some(ri) = self.repo_index_for_session(&session, &id.repo) else {
                // A shell of an unknown repository — try reconstructing from
                // the pane's cwd (the worktree path) + git metadata. A
                // repository shell has no worktree to reconstruct: its
                // repository is either known or out of scope, and inventing
                // a worktree from the main checkout would put the repo's own
                // path in the tree as a worktree row.
                if id.is_repo_shell() {
                    continue;
                }
                if let Some(path) = pane_cwd(tmux, &pane.pane_id) {
                    self.reconstruct_worktree(&path, &id.repo, &id.worktree, id.index);
                }
                continue;
            };
            let repo = &mut self.repositories[ri];
            // Remember the session this repository lives in.
            if repo.session_name.as_deref() != Some(&session) {
                repo.session_name = Some(session.clone());
                log::info!("repository '{}' → agent session {session}", repo.name);
            }
            // The repository's own inline shell hangs off the repository,
            // not off a worktree: no branch can sanitise to the sentinel,
            // so this arm can never swallow a worktree's shell.
            if id.is_repo_shell() {
                match &mut repo.shell {
                    Some(shell) => {
                        shell.window_id = Some(pane.window_id.clone());
                        shell.pane_id = Some(pane.pane_id.clone());
                    }
                    // Alive in tmux but not in state (state file deleted, a
                    // shell another instance started): adopt it. The
                    // repository is already known, so unlike a worktree
                    // there is nothing to reconstruct from the pane's cwd.
                    none => {
                        *none = Some(Shell {
                            command: None,
                            index: crate::dock::REPO_INDEX,
                            window_name: crate::tmux::Tmux::window_name(
                                crate::dock::REPO_WORKTREE,
                                crate::dock::REPO_INDEX,
                            ),
                            window_id: Some(pane.window_id.clone()),
                            pane_id: Some(pane.pane_id.clone()),
                        });
                        log::info!("adopted the inline shell of repository '{}'", repo.name);
                    }
                }
                continue;
            }
            // Worktrees are matched by their branch slug.
            let wt_idx = repo
                .worktrees
                .iter()
                .position(|f| sanitize_name(&f.branch) == id.worktree);
            let Some(wi) = wt_idx else {
                // Worktree state lost but the shell is alive: reconstruct.
                if let Some(path) = pane_cwd(tmux, &pane.pane_id) {
                    self.reconstruct_worktree(&path, &id.repo, &id.worktree, id.index);
                }
                continue;
            };
            let wt = &mut self.repositories[ri].worktrees[wi];
            if let Some(shell) = wt.shells.iter_mut().find(|s| s.index == id.index) {
                shell.window_id = Some(pane.window_id.clone());
                shell.pane_id = Some(pane.pane_id.clone());
            } else {
                wt.shells.push(Shell {
                    command: None,
                    index: id.index,
                    window_name: crate::tmux::Tmux::window_name(&id.worktree, id.index),
                    window_id: Some(pane.window_id.clone()),
                    pane_id: Some(pane.pane_id.clone()),
                });
                wt.shells.sort_by_key(|s| s.index);
            }
        }
        Ok(())
    }

    /// Which repository owns the agent session `session` (slug `slug`)?
    /// Exact session-name match first; else the slug matches the
    /// repository's sanitised name; else a hash-suffixed session.
    fn repo_index_for_session(&self, session: &str, slug: &str) -> Option<usize> {
        self.repositories
            .iter()
            .position(|p| p.session_name.as_deref() == Some(session))
            .or_else(|| {
                self.repositories
                    .iter()
                    .position(|p| sanitize_name(&p.name) == slug)
            })
            .or_else(|| {
                self.repositories.iter().position(|p| {
                    n_contains_slug(p, slug)
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
            })
    }

    /// Rebuild a worktree entry from a live window whose state was lost
    /// (e.g. state file deleted). The worktree path is the pane's cwd; the
    /// branch comes from git; the owning repository from the worktree's main
    /// repo. The worktree name is the sanitised window-name component.
    fn reconstruct_worktree(
        &mut self,
        path: &std::path::Path,
        pname: &str,
        fname: &str,
        idx: usize,
    ) {
        let Some(main_repo) = crate::git::main_repo_of_worktree(path) else {
            log::warn!("cannot find main repo for worktree {}", path.display());
            return;
        };
        let Some(repo_idx) = self.repositories.iter().position(|p| p.path == main_repo) else {
            log::info!(
                "window {pname}/{fname} belongs to unknown repository, skipping reconstruction"
            );
            return;
        };
        let Some(branch) = Git.current_branch(path).ok() else {
            log::warn!("cannot resolve branch for {}", path.display());
            return;
        };
        log::info!("reconstructing worktree {fname} (branch {branch})");
        self.repositories[repo_idx].worktrees.push(Worktree {
            name: fname.to_string(),
            branch,
            path: path.to_path_buf(),
            created_at: String::new(),
            base_commit: None,
            shells: vec![Shell {
                command: None,
                index: idx,
                window_name: crate::tmux::Tmux::window_name(fname, idx),
                window_id: None,
                pane_id: None,
            }],
        });
    }

    /// Prune state entries that no longer match reality. When `tmux` is
    /// given, shells whose **pane** has vanished are dropped too — the pane
    /// is the shell, and it may be sitting in a window that is not its own.
    ///
    /// Repositories are NEVER dropped here: a repository whose path is not a repo
    /// is kept and marked `missing` (shown "(missing)" in the tree); it
    /// re-activates automatically when the path is a repo again. Only
    /// worktrees (gone from `git worktree list`) and shells (window gone)
    /// are pruned.
    ///
    /// `verify_worktrees` is index-parallel to `repositories`: `true` means
    /// this repository's worktrees have been loaded this run (it is expanded
    /// or the flattened single repository), so its worktrees can be verified
    /// against `git worktree list`. Repositories with `false` keep their
    /// worktrees untouched — verifying 200 repos at boot would be the very
    /// cost the lazy loading exists to avoid.
    pub fn prune(&mut self, tmux: Option<&Tmux>, verify_worktrees: &[bool]) -> Result<()> {
        let git = Git;
        let live_panes: Option<std::collections::HashSet<String>> = tmux.and_then(|t| {
            Some(
                t.list_panes()
                    .ok()?
                    .into_iter()
                    .map(|p| p.pane_id)
                    .collect(),
            )
        });
        let mut removed_worktrees = 0;
        let mut removed_shells = 0;
        for (idx, repo) in self.repositories.iter_mut().enumerate() {
            let is_repo = git.is_repo(&repo.path);
            if !is_repo {
                // Keep the repository; mark it missing (re-activates when the
                // path is a repo again).
                if !repo.missing {
                    repo.missing = true;
                    log::info!(
                        "repository '{}' marked missing ({} is not a git repo)",
                        repo.name,
                        repo.path.display()
                    );
                }
                // Worktrees/shells cannot be verified while missing — keep
                // them as they are (their windows may still be alive).
            } else {
                if repo.missing {
                    repo.missing = false;
                    log::info!(
                        "repository '{}' re-activated ({} is a git repo again)",
                        repo.name,
                        repo.path.display()
                    );
                }
                // Worktree verification is the expensive part — only run it
                // for repositories whose worktrees were actually loaded this
                // run (see `App::ensure_worktrees`).
                let verify = verify_worktrees.get(idx).copied().unwrap_or(true);
                if verify {
                    if let Ok(worktrees) = git.worktree_list(&repo.path) {
                        let wt_paths: Vec<PathBuf> =
                            worktrees.into_iter().map(|w| w.path).collect();
                        repo.worktrees.retain(|f| {
                            let keep = wt_paths.contains(&f.path);
                            if !keep {
                                log::info!(
                                    "pruning stale worktree '{}' (worktree {} gone)",
                                    f.name,
                                    f.path.display()
                                );
                                removed_worktrees += 1;
                            }
                            keep
                        });
                        for wt in &mut repo.worktrees {
                            if let Some(live) = &live_panes {
                                let before = wt.shells.len();
                                wt.shells.retain(|s| match &s.pane_id {
                                    Some(pid) => live.contains(pid),
                                    // Not matched by this run's reconcile —
                                    // reconstructed, or reconcile failed. A
                                    // shell is never dropped on a guess.
                                    None => true,
                                });
                                removed_shells += before - wt.shells.len();
                            }
                        }
                    } else {
                        log::warn!(
                            "worktree list failed for {} — leaving worktrees untouched",
                            repo.path.display()
                        );
                    }
                }
                // else: worktrees stay as-is until the repository is expanded —
                // lazy loading owns verification then.
                //
                // The repository's own shell is pruned whatever `verify`
                // says: it hangs off the repository, not off the worktree
                // listing, so the expensive `git worktree list` this branch
                // is gated on has nothing to do with it.
                if let Some(live) = &live_panes
                    && repo
                        .shell
                        .as_ref()
                        .is_some_and(|s| s.pane_id.as_deref().is_some_and(|p| !live.contains(p)))
                {
                    log::info!("inline shell of repository '{}' is gone", repo.name);
                    repo.shell = None;
                    removed_shells += 1;
                }
            }
            // If the agent session vanished (crashed server, killed session)
            // but the repository still exists, drop the stale name so a new
            // session is created on demand.
            if let Some(sess) = &repo.session_name {
                let alive = tmux.map(|t| t.session_exists(sess)).unwrap_or(false);
                if !alive {
                    log::info!(
                        "agent session {} gone for repository '{}', clearing",
                        sess,
                        repo.name
                    );
                    repo.session_name = None;
                }
            }
        }
        if removed_worktrees + removed_shells > 0 {
            log::info!("pruned {removed_worktrees} worktrees, {removed_shells} shells");
        }
        Ok(())
    }
}

/// Does the repository's name match the session slug (for hash-suffixed
/// session names)?
fn n_contains_slug(p: &Repository, slug: &str) -> bool {
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

    /// A closed shell's index is never handed out again: `next_shell_index`
    /// counts past the highest in use, so the freed number cannot collide
    /// with a live window on reconcile.
    #[test]
    fn next_shell_index_never_reuses_a_freed_slot() {
        let mut wt = Worktree {
            name: "wt".into(),
            branch: "wt".into(),
            path: PathBuf::from("/tmp/wt"),
            created_at: String::new(),
            base_commit: None,
            shells: vec![],
        };
        assert_eq!(wt.next_shell_index(), 0);
        for idx in 0..3 {
            wt.shells.push(Shell {
                command: None,
                index: idx,
                window_name: format!("lw:wt/{idx}"),
                window_id: None,
                pane_id: None,
            });
        }
        assert_eq!(wt.next_shell_index(), 3);
        wt.shells.remove(1); // shell 1 closed
        assert_eq!(
            wt.next_shell_index(),
            3,
            "a freed index must not be reused while shell 2 holds a window"
        );
    }

    #[test]
    fn window_name_is_the_human_facing_label() {
        assert_eq!(crate::tmux::Tmux::window_name("wt-one", 2), "lw:wt-one/2");
    }

    #[test]
    fn state_json_roundtrip() {
        let mut s = State::default();
        s.repositories.push(Repository {
            name: "repo".into(),
            path: PathBuf::from("/tmp/repo"),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: Some("wingman-session-repo".into()),
            missing: false,
            shell: None,
            worktrees: vec![Worktree {
                name: "wt".into(),
                branch: "wt".into(),
                path: PathBuf::from("/tmp/wt/wt"),
                created_at: "now".into(),
                base_commit: None,
                shells: vec![Shell {
                    command: None,
                    index: 0,
                    window_name: "lw:wt/0".into(),
                    window_id: Some("@5".into()),
                    pane_id: Some("%9".into()),
                }],
            }],
        });
        let json = serde_json::to_string_pretty(&s).unwrap();
        let back: State = serde_json::from_str(&json).unwrap();
        assert_eq!(back.repositories.len(), 1);
        // Live tmux coordinates are NOT persisted: reconcile re-derives them
        // from the panes every run, and a coordinate that survived a restart
        // would name a window that no longer holds this shell.
        assert!(
            !json.contains("@5"),
            "window id must not be written: {json}"
        );
        assert!(!json.contains("%9"), "pane id must not be written: {json}");
        let shell = &back.repositories[0].worktrees[0].shells[0];
        assert_eq!(shell.index, 0);
        assert_eq!(shell.window_id, None);
        assert_eq!(shell.pane_id, None);
        assert_eq!(
            back.repositories[0].session_name.as_deref(),
            Some("wingman-session-repo")
        );
    }

    /// A file of the previous schema is discarded and rediscovered — the
    /// treatment v3 gave v2. Repositories come back from discovery, shells
    /// from the panes; nothing is silently half-migrated.
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
        // session_name and missing are optional on disk.
        let v1 = r#"{"repositories":[{"name":"p","path":"/tmp/p","base_branch":"main","agent_command":"claude","agent_shell":"bash","worktrees":[]}]}"#;
        let s: State = serde_json::from_str(v1).unwrap();
        assert_eq!(s.version, 0);
        assert!(s.repositories[0].session_name.is_none());
        assert!(!s.repositories[0].missing);
        assert!(s.repositories[0].worktrees.is_empty());
    }

    #[test]
    fn sanitize_matches_session_names() {
        assert_eq!(sanitize_name("My Repository"), "My-Repository");
        assert_eq!(sanitize_name("wt/one"), "wt/one");
    }

    /// A repository whose path vanished is KEPT and marked missing; it is
    /// never pruned, and it re-activates when the path is a repo again.
    #[test]
    fn prune_keeps_missing_repositories_and_reactivates_them() {
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
        s.repositories.push(Repository {
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
        s.prune(None, &vec![true; s.repositories.len()]).unwrap();
        assert!(!s.repositories[0].missing, "active repo stays active");

        // path vanishes → kept, marked missing
        std::fs::remove_dir_all(&repo).unwrap();
        s.prune(None, &vec![true; s.repositories.len()]).unwrap();
        assert_eq!(
            s.repositories.len(),
            1,
            "missing repository is never pruned"
        );
        assert!(s.repositories[0].missing);

        // path is a repo again → re-activated
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]);
        s.prune(None, &vec![true; s.repositories.len()]).unwrap();
        assert_eq!(s.repositories.len(), 1);
        assert!(!s.repositories[0].missing, "re-activated");
    }

    /// Missing repositories keep their worktrees (they cannot be verified while
    /// the path is gone; they are not dropped).
    #[test]
    fn prune_keeps_worktrees_of_missing_repositories() {
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
        s.repositories.push(Repository {
            name: "repo".into(),
            path: repo.clone(),
            base_branch: "main".into(),
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            session_name: None,
            missing: false,
            shell: None,
            worktrees: vec![Worktree {
                name: "wt".into(),
                branch: "wt".into(),
                path: repo.join("../wt-wt"),
                created_at: "2026-08-16".into(),
                base_commit: None,
                shells: vec![Shell {
                    command: None,
                    index: 0,
                    window_name: "lw:wt/0".into(),
                    window_id: Some("@9".into()),
                    pane_id: Some("%9".into()),
                }],
            }],
        });
        // verify the worktree is pruned while the repo is present (worktree
        // is gone)
        s.prune(None, &vec![true; s.repositories.len()]).unwrap();
        assert!(s.repositories[0].worktrees.is_empty());

        // now make the repository missing: a worktree added while missing is
        // kept intact
        s.repositories[0].worktrees.push(Worktree {
            name: "feat2".into(),
            branch: "feat2".into(),
            path: repo.join("../wt-feat2"),
            created_at: "".into(),
            base_commit: None,
            shells: vec![Shell {
                command: None,
                index: 0,
                window_name: "lw:feat2/0".into(),
                window_id: Some("@10".into()),
                pane_id: Some("%10".into()),
            }],
        });
        std::fs::remove_dir_all(&repo).unwrap();
        s.prune(None, &vec![true; s.repositories.len()]).unwrap();
        assert!(s.repositories[0].missing);
        assert_eq!(
            s.repositories[0].worktrees.len(),
            1,
            "worktrees survive while missing"
        );
        assert_eq!(s.repositories[0].worktrees[0].shells.len(), 1);
    }

    /// A pane stamped with the repository sentinel reconciles onto the
    /// repository itself, never onto a worktree — and is adopted when state
    /// has forgotten it, the way a worktree's shell is.
    #[test]
    fn a_repository_shell_reconciles_onto_its_repository() {
        crate::tmux::testutil::with_tmux_server(|_sock, tmux| {
            let id = crate::dock::ShellId::for_session(
                "wingman-session-p",
                crate::dock::REPO_WORKTREE,
                crate::dock::REPO_INDEX,
            );
            assert!(id.is_repo_shell());
            let w = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:.repo/0",
                    std::path::Path::new("/tmp"),
                    "bash",
                    &id,
                    crate::tmux::testutil::HOLDER_W,
                )
                .expect("the repository's shell window");

            let mut s = State::default();
            s.repositories.push(Repository {
                name: "p".into(),
                path: PathBuf::from("/tmp/p"),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: Some("wingman-session-p".into()),
                missing: false,
                shell: None,
                worktrees: vec![],
            });

            s.reconcile(&tmux).expect("reconcile");

            let shell = s.repositories[0]
                .shell
                .as_ref()
                .expect("the live pane is adopted onto the repository");
            assert_eq!(shell.pane_id.as_deref(), Some(w.pane_id.as_str()));
            assert!(
                s.repositories[0].worktrees.is_empty(),
                "and no worktree is invented for it"
            );
        });
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
            s.repositories.push(Repository {
                name: "p".into(),
                path: PathBuf::from("/tmp/p"),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: None,
                missing: false,
                shell: None,
                worktrees: vec![],
            });
            s.save().unwrap();
            let loaded = State::load().unwrap();
            assert_eq!(loaded.repositories.len(), 1);
            assert_eq!(loaded.repositories[0].name, "p");
            assert_eq!(loaded.version, STATE_VERSION);
        });
    }

    /// Identity lives on the pane, so renaming every agent window to
    /// garbage costs the tree nothing: reconcile still finds each shell and
    /// puts it back under its worktree.
    #[test]
    fn rediscovery_survives_windows_renamed_by_hand() {
        crate::tmux::testutil::with_tmux_server(|_sock, tmux| {
            let a = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:wt/0",
                    std::path::Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "wt", 0),
                    crate::tmux::testutil::HOLDER_W,
                )
                .expect("session");
            let b = tmux
                .new_window(
                    "wingman-session-p",
                    "lw:wt/1",
                    std::path::Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "wt", 1),
                    crate::tmux::testutil::HOLDER_W,
                )
                .expect("window");
            for w in [&a, &b] {
                tmux.run_raw(&["rename-window", "-t", &w.window_id, "not-ours"])
                    .expect("rename");
            }

            // State knows the shape of the tree but no tmux coordinates —
            // exactly what comes back off disk under v4.
            let mut s = State::default();
            s.repositories.push(Repository {
                name: "p".into(),
                path: PathBuf::from("/tmp/p"),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: Some("wingman-session-p".into()),
                missing: false,
                shell: None,
                worktrees: vec![Worktree {
                    name: "wt".into(),
                    branch: "wt".into(),
                    path: PathBuf::from("/tmp/p-wt"),
                    created_at: String::new(),
                    base_commit: None,
                    shells: vec![],
                }],
            });
            s.reconcile(&tmux).expect("reconcile");

            let shells = &s.repositories[0].worktrees[0].shells;
            assert_eq!(shells.len(), 2, "both shells rediscovered: {shells:?}");
            assert_eq!(shells[0].index, 0);
            assert_eq!(shells[0].pane_id.as_deref(), Some(a.pane_id.as_str()));
            assert_eq!(shells[1].index, 1);
            assert_eq!(shells[1].pane_id.as_deref(), Some(b.pane_id.as_str()));
        });
    }

    /// Furniture is never a shell, whatever identity is written onto it.
    ///
    /// The rule used to rest on an assumption — furniture carries no
    /// `@lw_repo`, so it cannot parse as one — and a stamping path that
    /// went by where a pane *sat* rather than what it *was* broke it: two
    /// panes claimed one shell, `reconcile` kept whichever `list-panes -a`
    /// listed last, and a dashboard that got the holder docked a sleeping
    /// placeholder and read the shell as `ended`. The exclusion is
    /// structural now, and this is the test that keeps it that way.
    #[test]
    fn a_holder_is_never_mistaken_for_the_shell_it_stands_in_for() {
        crate::tmux::testutil::with_tmux_server(|_sock, tmux| {
            let id = crate::dock::ShellId::for_session("wingman-session-p", "feat", 0);
            let w = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:feat/0",
                    std::path::Path::new("/tmp"),
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
            s.repositories.push(Repository {
                name: "p".into(),
                path: PathBuf::from("/tmp/p"),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: Some("wingman-session-p".into()),
                missing: false,
                shell: None,
                worktrees: vec![Worktree {
                    name: "feat".into(),
                    branch: "feat".into(),
                    path: PathBuf::from("/tmp/p-feat"),
                    created_at: String::new(),
                    base_commit: None,
                    shells: vec![],
                }],
            });

            // The holder claims to be the agent.
            tmux.stamp_shell(&holder.pane_id, &id).expect("stamp it");
            s.reconcile(&tmux).expect("reconcile");

            let shells = &s.repositories[0].worktrees[0].shells;
            assert_eq!(shells.len(), 1, "still exactly one shell: {shells:?}");
            assert_eq!(
                shells[0].pane_id.as_deref(),
                Some(w.pane_id.as_str()),
                "and it is the agent, not the placeholder standing in for it"
            );
            assert!(
                tmux.list_panes()
                    .expect("panes")
                    .iter()
                    .all(|p| p.pane_id != holder.pane_id),
                "the holder is not reported as a shell at all"
            );

            // A second reconcile does not change its mind.
            s.reconcile(&tmux).expect("reconcile again");
            assert_eq!(s.repositories[0].worktrees[0].shells.len(), 1);
        });
    }

    /// Two dashboards share this file and both write it. Last-writer-wins
    /// is survivable — reconcile re-derives everything live from tmux — but
    /// a half-written file is not: `load` treats unparseable JSON as a hard
    /// error, so one badly-timed write would take the tree down for every
    /// instance and stay broken across restarts. The write goes to a
    /// temporary beside the target and is renamed over it.
    #[test]
    fn save_is_atomic_so_a_torn_file_is_impossible() {
        crate::util::testutil::with_fresh_xdg(|| {
            let mut s = State::default();
            s.repositories.push(Repository {
                name: "p".into(),
                path: PathBuf::from("/tmp/p"),
                base_branch: "main".into(),
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                session_name: None,
                missing: false,
                shell: None,
                worktrees: vec![],
            });
            s.save().expect("save");

            let path = crate::util::state_path();
            assert!(path.exists());
            State::load().expect("what was written parses");

            // The temporary is not left behind, and nothing else is: the
            // rename IS the write, so the directory holds one state file.
            let strays: Vec<String> = std::fs::read_dir(crate::util::state_dir())
                .expect("state dir")
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("state.") && n != "state.json")
                .collect();
            assert!(strays.is_empty(), "temporaries left behind: {strays:?}");

            // Saving again over a live file replaces it whole.
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
