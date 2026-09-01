//! tmux integration via its CLI.
//!
//! LazyWingman supervises agent shells that live in DEDICATED tmux sessions
//! (`wingman-session-<repository>`), never in the user's session — creating a
//! worktree or shell must not pollute the dashboard's session. The dashboard
//! itself may run inside tmux (its coordinates are detected) or standalone
//! in a plain terminal; every session/window/pane operation works in both
//! cases because it goes through the tmux server.
//!
//! All commands go through the `tmux` binary; nothing is shell-interpreted.
//! A `-L <socket>` override is honoured via `$LAZYWINGMAN_TMUX_SOCKET` (used
//! by the integration tests and handy for multi-server setups).

use std::path::Path;
use std::process::Command;

use crate::dock::{self, ShellId};
use crate::error::{Error, Result};

/// Dashboard coordinates when LazyWingman itself runs inside tmux.
#[derive(Debug, Clone)]
pub struct Inside {
    #[allow(dead_code)] // read once reconcile groups panes by session (Task 4)
    pub session_name: String,
    pub window_id: String,
    pub pane_id: String,
}

/// Our view of the tmux server. `inside` is `None` when the dashboard runs
/// in a plain terminal (standalone mode).
#[derive(Debug, Clone)]
pub struct Tmux {
    pub inside: Option<Inside>,
    socket: Option<String>,
}

/// One pane, as reported by `list-panes -F`.
#[derive(Debug, Clone, Default)]
pub struct PaneInfo {
    pub pane_id: String,
    /// The window the pane is in right now. Like [`Self::session_name`]
    /// this is a location, not an identity: it changes when the pane moves.
    pub window_id: String,
    pub current_command: String,
    pub dead: bool,
    /// The session the pane is in *right now*. A docked pane sits in the
    /// dashboard's session, so this is where it happens to live, never who
    /// it belongs to — [`Self::shell`] is the identity.
    #[allow(dead_code)] // reconcile groups panes by it (Task 4)
    pub session_name: String,
    /// The shell this pane is, read back from its `@lw_*` options.
    pub shell: Option<ShellId>,
    /// Is this the active pane of its window?
    #[allow(dead_code)] // the focus guard reads it (Phase 4)
    pub active: bool,
    /// While docked: the pane id of the dashboard holding it (`@lw_docked`).
    #[allow(dead_code)] // the lease reads it (Phase 5)
    pub docked_by: Option<String>,
    /// Raw `@lw_agent` value, empty when the pane carries none. Parsed by
    /// `hooks::Overlay::parse`; kept as a string here so tmux stays
    /// ignorant of what the value means.
    pub overlay_raw: String,
}

/// One tmux session (agent sessions only, filtered by the caller).
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub name: String,
}

/// Result of spawning / listing a window.
#[derive(Debug, Clone)]
pub struct WindowRef {
    pub window_id: String,
    /// Active pane id (set by `new_window` / `new_session`).
    pub pane_id: String,
}

/// Login shells whose presence as the foreground command means the agent
/// process inside has exited.
const SHELL_COMMANDS: &[&str] = &[
    "bash", "sh", "zsh", "fish", "dash", "ksh", "mksh", "ash", "tcsh", "csh", "nu",
];

/// Prefix of every session LazyWingman owns (agent shells).
pub const SESSION_PREFIX: &str = "wingman-session-";
/// Prefix of every window LazyWingman owns inside an agent session.
pub const WINDOW_PREFIX: &str = "lw:";

fn socket_override() -> Option<String> {
    std::env::var("LAZYWINGMAN_TMUX_SOCKET")
        .ok()
        .filter(|s| !s.is_empty())
}

fn base_command(socket: &Option<String>) -> Command {
    let mut c = Command::new("tmux");
    if let Some(s) = socket {
        c.arg("-L").arg(s);
    }
    c
}

/// Run tmux with the given args; returns stdout. Errors carry stderr.
fn run_tmux(socket: &Option<String>, args: &[&str]) -> Result<String> {
    let out = base_command(socket)
        .args(args)
        .output()
        .map_err(|e| Error::Tmux(format!("cannot run tmux: {e}")))?;
    if !out.status.success() {
        return Err(Error::Tmux(format!(
            "tmux {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Tmux {
    /// Detect whether we're running inside tmux and, if so, our coordinates.
    /// Always returns a usable [`Tmux`]; standalone operation is the default.
    pub fn detect() -> Result<Tmux> {
        let socket = socket_override();
        let Some(pane_id) = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty()) else {
            return Ok(Tmux {
                inside: None,
                socket,
            });
        };
        if std::env::var_os("TMUX").is_none() {
            return Ok(Tmux {
                inside: None,
                socket,
            });
        }
        let fmt = "#{session_name}|#{window_id}";
        let out = run_tmux(&socket, &["display-message", "-p", "-t", &pane_id, fmt])?;
        let mut parts = out.trim().splitn(2, '|');
        let (session_name, window_id) = (
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("").to_string(),
        );
        if session_name.is_empty() {
            return Ok(Tmux {
                inside: None,
                socket,
            });
        }
        Ok(Tmux {
            inside: Some(Inside {
                session_name,
                window_id,
                pane_id,
            }),
            socket,
        })
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        run_tmux(&self.socket, args)
    }

    /// Make sure a tmux server exists on our socket, starting one if needed.
    ///
    /// Standalone boot usually happens before any tmux server exists; tmux
    /// 3.5a shuts a server down as soon as it has no sessions and no
    /// clients, so a server we had to start ourselves is told to stay alive
    /// (`exit-empty off`) — in ONE client invocation, because the server
    /// can exit between two separate clients. A pre-existing server (e.g.
    /// the user's own) is never touched: `list-sessions` succeeds and we
    /// return.
    pub fn ensure_server(&self) -> Result<()> {
        match self.list_sessions() {
            Ok(_) => Ok(()),
            Err(e) if is_no_server_error(&e) => {
                self.run(&["start-server", ";", "set", "-g", "exit-empty", "off"])?;
                log::info!("started tmux server on our socket (exit-empty off)");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Run a raw tmux command and return stdout (used by state.rs for
    /// pane-cwd lookups).
    pub fn run_raw(&self, args: &[&str]) -> Result<String> {
        self.run(args)
    }

    // ---------------------------------------------------------- naming

    /// Sanitise a name component: no colons, spaces or control chars.
    pub fn sanitize(s: &str) -> String {
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

    /// Deterministic agent-session name for a repository slug:
    /// `wingman-session-<slug>`. On collisions the caller appends
    /// [`Self::hash_suffix`] to keep it unique.
    pub fn session_name(slug: &str) -> String {
        format!("{SESSION_PREFIX}{}", Self::sanitize(slug))
    }

    /// Stable 8-hex suffix for a path — disambiguates two repositories whose
    /// slugs collide. [`std::hash::DefaultHasher::new`] is deterministic
    /// (fixed keys), unlike `RandomState::new()`.
    pub fn hash_suffix(path: &Path) -> String {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        path.to_string_lossy().hash(&mut h);
        format!("{:08x}", h.finish() & 0xffff_ffff)
    }

    /// Deterministic window name for a worktree shell inside an agent session.
    pub fn window_name(worktree: &str, index: usize) -> String {
        let mut name = format!("{WINDOW_PREFIX}{}/{}", Self::sanitize(worktree), index);
        // tmux window names are capped; keep ours well under.
        name.truncate(200);
        name
    }

    // ---------------------------------------------------------- sessions

    /// All sessions on the server.
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let out = self.run(&["list-sessions", "-F", "#{session_name}"])?;
        Ok(out
            .lines()
            .filter_map(|line| {
                let name = line.trim().to_string();
                if name.is_empty() {
                    None
                } else {
                    Some(SessionInfo { name })
                }
            })
            .collect())
    }

    /// Does a session with this exact name exist?
    pub fn session_exists(&self, name: &str) -> bool {
        self.list_sessions()
            .map(|ss| ss.iter().any(|s| s.name == name))
            .unwrap_or(false)
    }

    /// Create a detached session whose FIRST window is named `win_name`,
    /// runs `shell` in `cwd`, and returns that window.
    ///
    /// Retries briefly: a freshly started tmux server can still be finishing
    /// its first window, and session/window creation then races index 0.
    pub fn new_session(
        &self,
        session: &str,
        win_name: &str,
        cwd: &Path,
        shell: &str,
        id: &ShellId,
    ) -> Result<WindowRef> {
        let mut last = None;
        for attempt in 0..3 {
            match self.try_new_session(session, win_name, cwd, shell) {
                Ok(w) => {
                    self.stamp_shell(&w.pane_id, id)?;
                    return Ok(w);
                }
                Err(e) => {
                    log::warn!("new-session attempt {} failed: {e}", attempt + 1);
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(120 * (attempt + 1)));
                }
            }
        }
        Err(last.unwrap())
    }

    fn try_new_session(
        &self,
        session: &str,
        win_name: &str,
        cwd: &Path,
        shell: &str,
    ) -> Result<WindowRef> {
        let out = self.run(&[
            "new-session",
            "-d",
            "-s",
            session,
            "-n",
            win_name,
            "-c",
            cwd.to_str().unwrap_or("."),
            "-P",
            "-F",
            "#{window_id}|#{pane_id}",
            shell,
        ])?;
        let mut it = out.trim().splitn(2, '|');
        let window_id = it.next().unwrap_or("").to_string();
        let pane_id = it.next().unwrap_or("").to_string();
        if window_id.is_empty() {
            return Err(Error::Tmux(format!(
                "new-session returned no window id: {out}"
            )));
        }
        Ok(WindowRef { window_id, pane_id })
    }

    /// Kill a session by name (errors when it no longer exists).
    pub fn kill_session(&self, session: &str) -> Result<()> {
        self.run(&["kill-session", "-t", session])?;
        Ok(())
    }

    // ---------------------------------------------------------- windows

    /// Spawn a window running an interactive `shell` in `cwd` inside an
    /// existing agent session (not selected). Retries on the fresh-server
    /// race, like [`Self::new_session`].
    pub fn new_window(
        &self,
        session: &str,
        name: &str,
        cwd: &Path,
        shell: &str,
        id: &ShellId,
    ) -> Result<WindowRef> {
        let mut last = None;
        for attempt in 0..3 {
            match self.try_new_window(session, name, cwd, shell) {
                Ok(w) => {
                    self.stamp_shell(&w.pane_id, id)?;
                    return Ok(w);
                }
                Err(e) => {
                    log::warn!("new-window attempt {} failed: {e}", attempt + 1);
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(120 * (attempt + 1)));
                }
            }
        }
        Err(last.unwrap())
    }

    fn try_new_window(
        &self,
        session: &str,
        name: &str,
        cwd: &Path,
        shell: &str,
    ) -> Result<WindowRef> {
        let out = self.run(&[
            "new-window",
            "-d",
            "-t",
            session,
            "-P",
            "-F",
            "#{window_id}|#{pane_id}",
            "-n",
            name,
            "-c",
            cwd.to_str().unwrap_or("."),
            shell,
        ])?;
        let mut it = out.trim().splitn(2, '|');
        let window_id = it.next().unwrap_or("").to_string();
        let pane_id = it.next().unwrap_or("").to_string();
        if window_id.is_empty() {
            return Err(Error::Tmux(format!(
                "new-window returned no window id: {out}"
            )));
        }
        Ok(WindowRef { window_id, pane_id })
    }

    /// Kill a window by id (used on worktree deletion; killing a session's
    /// last window destroys the session).
    pub fn kill_window(&self, window_id: &str) -> Result<()> {
        self.run(&["kill-window", "-t", window_id])?;
        Ok(())
    }

    // ---------------------------------------------------------- panes

    /// Every LazyWingman shell on the server (one tmux call per poll).
    ///
    /// A pane qualifies by **carrying an identity**, not by living in an
    /// agent session: a docked pane sits in the dashboard's own session, and
    /// a session-name filter would drop it from the poll, lose its status
    /// and prune it as dead. Conversely a pane that merely shares an agent
    /// session — the user's own split, a holder — carries no `@lw_repo` and
    /// is none of our business.
    ///
    /// A server with no sessions yet yields an empty list, not an error.
    pub fn list_panes(&self) -> Result<Vec<PaneInfo>> {
        // Built from the option constants rather than spelled out, so the
        // read path cannot drift from what `stamp_shell` writes.
        let fmt = format!(
            "#{{pane_id}}|#{{window_id}}|#{{pane_current_command}}|#{{pane_dead}}|#{{session_name}}\
             |#{{{repo}}}|#{{{worktree}}}|#{{{index}}}|#{{pane_active}}|#{{{docked}}}\
             |#{{{overlay}}}",
            repo = dock::REPO_KEY,
            worktree = dock::WORKTREE_KEY,
            index = dock::INDEX_KEY,
            docked = dock::DOCKED_KEY,
            overlay = crate::hooks::OVERLAY_KEY,
        );
        let out = match self.run(&["list-panes", "-a", "-F", &fmt]) {
            Ok(o) => o,
            // list-panes -a has no target when the server has zero sessions.
            Err(e) if is_no_server_error(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(out
            .lines()
            .filter_map(parse_pane_line)
            .filter(|p| p.shell.is_some())
            .collect())
    }

    /// Panes that carry no identity, with the window and session they sit
    /// in — the raw material for the one-off backfill that upgrades a
    /// server started by an older build (identity used to live in the
    /// window name). One call, like the poll's.
    pub fn list_unstamped_panes(&self) -> Result<Vec<(String, String, String)>> {
        let fmt = format!(
            "#{{pane_id}}|#{{session_name}}|#{{window_name}}|#{{{repo}}}",
            repo = dock::REPO_KEY
        );
        let out = match self.run(&["list-panes", "-a", "-F", &fmt]) {
            Ok(o) => o,
            Err(e) if is_no_server_error(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(out
            .lines()
            .filter_map(|line| {
                // The window name is a user-chosen string and may contain
                // '|', so the identity field is read from the END.
                let (head, repo) = line.rsplit_once('|')?;
                if !repo.trim().is_empty() {
                    return None;
                }
                let mut it = head.splitn(3, '|');
                let pane_id = it.next()?.to_string();
                let session = it.next()?.to_string();
                let window_name = it.next()?.to_string();
                Some((pane_id, session, window_name))
            })
            .collect())
    }

    // ---------------------------------------------------- pane options

    /// Set a pane user option (`@key`). Used by the hook subcommand to
    /// publish an agent's self-reported state onto its own pane; tmux
    /// disposes of it with the pane, so there is nothing to clean up.
    pub fn set_pane_option(&self, pane_id: &str, key: &str, value: &str) -> Result<()> {
        self.run(&["set-option", "-p", "-t", pane_id, key, value])?;
        Ok(())
    }

    /// Write a shell's identity onto its own pane. Called by both spawn
    /// paths before they return, so no caller can forget: an unstamped pane
    /// is invisible to [`Self::list_panes`] and would read as a lost shell.
    pub fn stamp_shell(&self, pane_id: &str, id: &ShellId) -> Result<()> {
        self.set_pane_option(pane_id, dock::REPO_KEY, &id.repo)?;
        self.set_pane_option(pane_id, dock::WORKTREE_KEY, &id.worktree)?;
        self.set_pane_option(pane_id, dock::INDEX_KEY, &id.index_value())?;
        Ok(())
    }

    /// Unset a pane user option, handing the shell back to the heuristics.
    pub fn clear_pane_option(&self, pane_id: &str, key: &str) -> Result<()> {
        self.run(&["set-option", "-p", "-u", "-t", pane_id, key])?;
        Ok(())
    }

    /// Type text into a pane. `enter` appends a trailing Enter.
    pub fn send_keys(&self, pane_id: &str, text: &str, enter: bool) -> Result<()> {
        self.run(&["send-keys", "-t", pane_id, "-l", text])?;
        if enter {
            self.run(&["send-keys", "-t", pane_id, "Enter"])?;
        }
        Ok(())
    }

    /// Paste text into a pane via tmux's paste-buffer with bracketed paste
    /// (`set-buffer` + `paste-buffer -p -d`). Unlike `send-keys -l`, this
    /// preserves embedded newlines: an application that enabled bracketed
    /// paste (Claude Code, most REPLs) receives the multi-line text as one
    /// paste instead of a newline being interpreted as Enter.
    pub fn paste_keys(&self, pane_id: &str, text: &str, enter: bool) -> Result<()> {
        const BUF: &str = "lw-paste";
        // Buffer names cannot contain the ':' character; text is fine.
        // Load the text, paste it (bracketed, delete after), then make
        // sure the buffer is gone even if -d was not enough.
        self.run(&["set-buffer", "-b", BUF, text])?;
        let paste_result = self.run(&["paste-buffer", "-b", BUF, "-p", "-d", "-t", pane_id]);
        let _ = self.run(&["delete-buffer", "-b", BUF]);
        paste_result?;
        if enter {
            self.run(&["send-keys", "-t", pane_id, "Enter"])?;
        }
        Ok(())
    }

    /// Last `lines` lines of a pane's visible content, **with** its escape
    /// sequences (`-e`): the preview renders the agent's own colours, so the
    /// capture has to carry them. Consumers that want plain text (status
    /// classification, the `none` preview mode) run it through
    /// [`crate::ansi::strip`] — tmux re-emits the active attributes at the
    /// start of every line, so the capture stays safe to slice line-wise.
    pub fn capture_pane(&self, pane_id: &str, lines: u32) -> Result<String> {
        let out = self.run(&[
            "capture-pane",
            "-p",
            "-e",
            "-t",
            pane_id,
            "-S",
            &format!("-{lines}"),
        ])?;
        Ok(out)
    }

    /// The pane's whole scrollback, escapes included — what the preview
    /// scrolls through. [`Tmux::capture_pane`] keeps only enough lines to
    /// classify a status and runs for every pane on every poll; real
    /// history is fetched once, on demand, for the selected pane alone.
    pub fn capture_pane_full(&self, pane_id: &str) -> Result<String> {
        self.run(&["capture-pane", "-p", "-e", "-t", pane_id, "-S", "-"])
    }

    // ---------------------------------------------------------- attach / back

    /// Attach the current client to an agent session and its shell window.
    /// Only valid when the dashboard runs inside tmux (`inside.is_some()`).
    pub fn attach_switch(&self, session: &str, window_id: &str) -> Result<()> {
        self.run(&["switch-client", "-t", session])?;
        self.select_window(window_id)?;
        Ok(())
    }

    /// Clientless: make `window_id` the active window of `session` (works on
    /// detached sessions too — verified). Used before a standalone handoff.
    pub fn select_window_in_session(&self, session: &str, window_id: &str) -> Result<()> {
        self.run(&[
            "select-window",
            "-t",
            &format!("{session}:@{}", window_id.trim_start_matches('@')),
        ])?;
        Ok(())
    }

    /// Select a window by server-global id (window IDs are unique per
    /// server, so this works from any session).
    pub fn select_window(&self, window_id: &str) -> Result<()> {
        self.run(&[
            "select-window",
            "-t",
            &format!("@{}", window_id.trim_start_matches('@')),
        ])?;
        Ok(())
    }

    /// Hand the current terminal over to `tmux attach -t <session>`.
    /// BLOCKS until the user detaches. The caller MUST restore the terminal
    /// (ratatui::restore) before calling and re-initialise after.
    pub fn attach_handoff(&self, session: &str) -> Result<()> {
        let status = base_command(&self.socket)
            .args(["attach", "-t", session])
            .status()
            .map_err(|e| Error::Tmux(format!("cannot run tmux attach: {e}")))?;
        if !status.success() {
            return Err(Error::Tmux(format!("tmux attach -t {session} failed")));
        }
        Ok(())
    }

    /// Bind the back-key in the PREFIX table: the user presses their tmux
    /// prefix (e.g. `C-b`) then `key` to jump back to the dashboard window
    /// from any agent window. Rebound on every startup and at agent-session
    /// creation. `key` is tmux syntax (e.g. `C-o`).
    ///
    /// Why the prefix table and not `-n` (root)? tmux key tables are
    /// server-wide, so a root-table binding hijacks that key in EVERY
    /// session of the server — including the user's own session — breaking
    /// other TUIs that use the same chord (vim's `C-o`, readline, ...).
    /// Behind the prefix, the key only fires when the user explicitly asks
    /// for a tmux chord, so no application ever loses it. The chord stays
    /// configurable via `tmux.back_key`.
    pub fn bind_back_key(&self, key: &str, dash_session: &str, dash_window_id: &str) -> Result<()> {
        self.run(&[
            "bind-key",
            "-T",
            "prefix",
            key,
            "switch-client",
            "-t",
            dash_session,
            "\\;",
            "select-window",
            "-t",
            &format!("@{}", dash_window_id.trim_start_matches('@')),
        ])?;
        Ok(())
    }

    /// Bind the back-key to `detach-client`, for the duration of a
    /// standalone attach handoff: detaching the handed-over client is
    /// exactly "back to the dashboard" when the dashboard is the process
    /// that ran `tmux attach`.
    pub fn bind_detach_back_key(&self, key: &str) -> Result<()> {
        self.run(&["bind-key", "-T", "prefix", key, "detach-client"])?;
        Ok(())
    }

    /// Drop the prefix-table binding for `key`.
    pub fn unbind_back_key(&self, key: &str) -> Result<()> {
        self.run(&["unbind-key", "-T", "prefix", key])?;
        Ok(())
    }

    /// What the prefix table binds `key` to right now.
    ///
    /// Key tables are server-wide, so this is the one shared slot every
    /// dashboard on the server competes for; reading it first is what keeps
    /// a standalone handoff from stomping on a binding it cannot restore.
    pub fn back_key_state(&self, key: &str) -> Result<BackKeyState> {
        let keys = self.run_raw(&["list-keys", "-T", "prefix"])?;
        Ok(keys
            .lines()
            .find_map(|l| prefix_binding(l, key))
            .unwrap_or(BackKeyState::Unbound))
    }

    /// Remove a pre-M9 root-table back-key binding if one exists: earlier
    /// versions bound `-n` (root table), hijacking the chord server-wide
    /// and breaking other TUIs. Only unbinds a binding whose command
    /// targets OUR dashboard window — a user's own root binding of the
    /// same key is never touched.
    pub fn unbind_legacy_root_back_key(
        &self,
        dash_session: &str,
        dash_window_id: &str,
    ) -> Result<()> {
        let win = dash_window_id.trim_start_matches('@');
        let keys = self.run_raw(&["list-keys", "-T", "root"])?;
        for line in keys.lines() {
            let Some(key) = legacy_root_back_key(line, dash_session, win) else {
                continue;
            };
            log::info!(
                "removing legacy root back-key binding for {key} (prefix-table binding replaces it)"
            );
            self.run(&["unbind-key", "-T", "root", &key])?;
        }
        Ok(())
    }
}

/// What `<prefix> + back-key` currently does on this tmux server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackKeyState {
    /// Nothing is bound to the key in the prefix table.
    Unbound,
    /// A dashboard back binding — ours — pointing at this session/window.
    Dashboard { session: String, window: String },
    /// A standalone handoff's `detach-client` — also ours.
    Detach,
    /// Somebody else's binding. Never touched, never restored.
    Foreign,
}

/// Read one `list-keys -T prefix` line as the state of `key`, or `None`
/// when the line binds a different key (or is not a binding at all).
///
/// Tokenised, never prefix-matched: `list-keys` is column-aligned and the
/// padding is version-dependent (see [`legacy_root_back_key`]).
fn prefix_binding(line: &str, key: &str) -> Option<BackKeyState> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "bind-key" {
        return None;
    }
    let table = loop {
        match parts.next() {
            Some("-T") => break parts.next()?,
            Some(_) => continue,
            None => return None,
        }
    };
    if table != "prefix" || parts.next()? != key {
        return None;
    }
    let command: Vec<&str> = parts.collect();
    if command == ["detach-client"] {
        return Some(BackKeyState::Detach);
    }
    // `switch-client -t <session> \; select-window -t @<window>` — the
    // shape `bind_back_key` writes, and the only one we claim as ours.
    let target = match command.as_slice() {
        [
            "switch-client",
            "-t",
            session,
            _,
            "select-window",
            "-t",
            window,
        ] => window
            .strip_prefix('@')
            .map(|w| (session.to_string(), w.to_string())),
        _ => None,
    };
    Some(match target {
        Some((session, window)) => BackKeyState::Dashboard { session, window },
        None => BackKeyState::Foreign,
    })
}

/// The key a `list-keys` line binds, if that line is *our* legacy root
/// back-key binding for this dashboard — otherwise `None`.
///
/// A legacy root binding reads:
///   `bind-key -T root C-o switch-client -t main \; select-window -t @0`
///
/// The line is tokenised rather than matched against a fixed-width prefix:
/// `list-keys` output is column-aligned and the padding is version-dependent
/// (3.6+ reserves a column for the optional `-r` flag, so `bind-key` is
/// followed by two spaces, not one). A prefix match against one spelling
/// stops matching entirely on the other, and the unbind then silently does
/// nothing.
///
/// This runs against the user's own tmux, so it is deliberately narrow: the
/// binding must target this exact dashboard session AND window, or it is
/// somebody else's key and must be left alone.
fn legacy_root_back_key(line: &str, dash_session: &str, win: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "bind-key" {
        return None;
    }
    // Skip any flags (`-r`) until the table selector, then take the table.
    let table = loop {
        match parts.next() {
            Some("-T") => break parts.next()?,
            Some(_) => continue,
            None => return None,
        }
    };
    if table != "root" {
        return None;
    }
    let key = parts.next()?;
    // The command follows; re-join it so the check is immune to the column
    // padding tmux inserts between the key and the command.
    let command = parts.collect::<Vec<_>>().join(" ");
    let targets_our_dashboard = command.contains(&format!("switch-client -t {dash_session} "))
        && command.contains(&format!("select-window -t @{win}"));
    targets_our_dashboard.then(|| key.to_string())
}

fn parse_pane_line(line: &str) -> Option<PaneInfo> {
    // Field order: pane | window | command | dead | session | repo |
    // worktree | index | active | docked | overlay. Every new field goes BEFORE the overlay:
    // the overlay is taken verbatim as the remainder, so a '|' inside it
    // cannot desync parsing, and only a last field can afford that.
    let mut it = line.splitn(11, '|');
    let pane_id = it.next()?.to_string();
    let window_id = it.next()?.to_string();
    let current_command = it.next()?.to_string();
    let dead = it.next()? == "1";
    let session_name = it.next()?.to_string();
    let repo = it.next().unwrap_or("");
    let worktree = it.next().unwrap_or("");
    let index = it.next().unwrap_or("");
    let active = it.next().unwrap_or("") == "1";
    let docked_by = it.next().unwrap_or("").trim().to_string();
    Some(PaneInfo {
        pane_id,
        window_id,
        current_command,
        dead,
        session_name,
        shell: ShellId::from_fields(repo, worktree, index),
        active,
        docked_by: (!docked_by.is_empty()).then_some(docked_by),
        overlay_raw: it.next().unwrap_or("").to_string(),
    })
}

/// tmux errors that merely mean "no server / no sessions yet" — normal
/// during a standalone boot before any agent session exists, and the
/// trigger for [`Tmux::ensure_server`].
fn is_no_server_error(e: &Error) -> bool {
    let msg = e.to_string();
    msg.contains("no server running")
        || msg.contains("error connecting")
        || msg.contains("no current target")
}

/// Is `cmd` a login shell (i.e. no agent running in the pane)?
pub fn is_shell_command(cmd: &str) -> bool {
    let c = cmd.trim();
    SHELL_COMMANDS.contains(&c) || c.ends_with("-login")
}

/// Is `name` one of our agent session names?
pub fn is_lazywingman_session(name: &str) -> bool {
    name.starts_with(SESSION_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `list-keys` is column-aligned and the padding is version-dependent:
    /// tmux 3.5 emits one space after `bind-key`, 3.6+ reserves a column for
    /// the optional `-r` flag and emits two. Parsing must not care — matching
    /// a fixed-width prefix silently stopped unbinding anything on 3.7.
    #[test]
    fn the_legacy_back_key_is_recognised_whatever_tmux_pads_with() {
        let cmd = "switch-client -t main \\; select-window -t @0";
        for (label, line) in [
            ("tmux 3.5 spacing", format!("bind-key -T root C-o {cmd}")),
            (
                "tmux 3.7 spacing",
                format!("bind-key  -T root C-o       {cmd}"),
            ),
            ("repeat flag", format!("bind-key -r -T root C-o {cmd}")),
        ] {
            assert_eq!(
                legacy_root_back_key(&line, "main", "0").as_deref(),
                Some("C-o"),
                "{label}: {line:?}"
            );
        }
    }

    /// Reading the shared back-chord slot: ours (either shape), the user's,
    /// or empty. The padding varies by tmux version, as everywhere else.
    #[test]
    fn the_back_chord_slot_is_read_for_what_it_holds() {
        let dash = r"switch-client -t main \; select-window -t @0";
        for (label, line, expected) in [
            (
                "our dashboard binding",
                format!("bind-key  -T prefix C-o     {dash}"),
                Some(BackKeyState::Dashboard {
                    session: "main".into(),
                    window: "0".into(),
                }),
            ),
            (
                "3.5 spacing, same binding",
                format!("bind-key -T prefix C-o {dash}"),
                Some(BackKeyState::Dashboard {
                    session: "main".into(),
                    window: "0".into(),
                }),
            ),
            (
                "a handoff's detach binding",
                "bind-key  -T prefix C-o     detach-client".to_string(),
                Some(BackKeyState::Detach),
            ),
            (
                "the user's own binding of our key",
                "bind-key  -T prefix C-o     choose-tree".to_string(),
                Some(BackKeyState::Foreign),
            ),
            (
                "another key entirely",
                format!("bind-key  -T prefix C-y     {dash}"),
                None,
            ),
            (
                "the root table, not prefix",
                format!("bind-key  -T root C-o     {dash}"),
                None,
            ),
            (
                "not a binding at all",
                "some other output".to_string(),
                None,
            ),
        ] {
            assert_eq!(prefix_binding(&line, "C-o"), expected, "{label}: {line:?}");
        }
    }

    /// The scan must leave every other root binding alone — it runs against
    /// the user's own tmux, so a false positive silently unbinds their key.
    #[test]
    fn other_root_bindings_are_left_alone() {
        for (label, line) in [
            (
                "another key, another command",
                "bind-key  -T root C-t choose-tree",
            ),
            (
                "same shape, different dashboard session",
                "bind-key  -T root C-o switch-client -t other \\; select-window -t @0",
            ),
            (
                "same shape, different window",
                "bind-key  -T root C-o switch-client -t main \\; select-window -t @7",
            ),
            (
                "prefix table, not root",
                "bind-key  -T prefix C-o switch-client -t main \\; select-window -t @0",
            ),
            ("not a binding at all", "some other output"),
        ] {
            assert_eq!(
                legacy_root_back_key(line, "main", "0"),
                None,
                "{label}: {line:?}"
            );
        }
    }

    #[test]
    fn session_and_window_names_are_deterministic_and_safe() {
        let s = Tmux::session_name("my repository");
        assert_eq!(s, "wingman-session-my-repository");
        let s2 = Tmux::session_name("my repository");
        assert_eq!(s, s2);
        // case is preserved: distinct names stay distinct (no silent merging)
        assert_ne!(s, Tmux::session_name("My-Repository"));
        assert!(s.starts_with(SESSION_PREFIX));
        assert!(!s.contains(' '));

        let w = Tmux::window_name("Feat One!", 0);
        assert_eq!(w, "lw:Feat-One/0");
        let w2 = Tmux::window_name("Feat One!", 0);
        assert_eq!(w, w2);
        assert!(w.starts_with(WINDOW_PREFIX));
        assert!(!w.contains(' '));
        assert!(w.len() <= 200);
    }

    #[test]
    fn session_prefix_detection() {
        assert!(is_lazywingman_session("wingman-session-foo"));
        assert!(!is_lazywingman_session("work"));
    }

    #[test]
    fn hash_suffix_is_stable_and_distinct() {
        let a = Tmux::hash_suffix(Path::new("/repo/one"));
        let b = Tmux::hash_suffix(Path::new("/repo/one"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
        let c = Tmux::hash_suffix(Path::new("/repo/two"));
        assert_ne!(a, c);
    }

    #[test]
    fn pane_line_parsing() {
        let p = parse_pane_line("%3|@1|claude|0|wingman-session-p|p|feat|2|1|").unwrap();
        assert_eq!(p.pane_id, "%3");
        assert_eq!(p.current_command, "claude");
        assert!(!p.dead);
        assert_eq!(p.window_id, "@1");
        assert_eq!(p.session_name, "wingman-session-p");
        assert_eq!(
            p.shell,
            Some(ShellId::for_session("wingman-session-p", "feat", 2))
        );
        assert!(p.active);
        assert_eq!(p.docked_by, None);
        let dead = parse_pane_line("%9|@1|sh|1|wingman-session-p|p|feat|2|0|").unwrap();
        assert!(dead.dead);
        assert!(!dead.active);
    }

    /// An unstamped pane parses but carries no identity — that is what
    /// `list_panes` filters on, so "unset" must read as `None`, not as a
    /// shell named the empty string.
    #[test]
    fn a_pane_without_the_options_carries_no_identity() {
        let p = parse_pane_line("%0|@1|bash|0|work||||1|").expect("parses");
        assert_eq!(p.shell, None);
        assert_eq!(p.session_name, "work");
    }

    /// The docking dashboard's pane id, when there is one.
    #[test]
    fn docked_by_is_read_back_when_set() {
        let p = parse_pane_line("%3|@1|claude|0|main|p|feat|0|1|%7|").expect("parses");
        assert_eq!(p.docked_by.as_deref(), Some("%7"));
    }

    #[test]
    fn pane_line_parsing_keeps_the_overlay_verbatim() {
        let p = parse_pane_line("%3|@1|claude|0|wingman-session-x|x|f|0|1||blocked:1755600000")
            .expect("parses");
        assert_eq!(p.pane_id, "%3");
        assert_eq!(p.current_command, "claude");
        assert!(!p.dead);
        assert_eq!(p.overlay_raw, "blocked:1755600000");
    }

    /// The overlay is last and taken as the remainder, so a `|` inside it
    /// can never desync the parser.
    #[test]
    fn overlay_may_contain_the_field_separator() {
        let p = parse_pane_line("%3|@1|claude|0|wingman-session-x|x|f|0|1||a|b").expect("parses");
        assert_eq!(p.overlay_raw, "a|b");
        assert_eq!(
            p.shell,
            Some(ShellId::for_session("wingman-session-x", "f", 0))
        );
    }

    #[test]
    fn missing_overlay_field_is_empty_not_an_error() {
        let p = parse_pane_line("%3|@1|claude|0|wingman-session-x|x|f|0|1|").expect("parses");
        assert_eq!(p.overlay_raw, "");
    }

    /// Identity, not location, is what makes a pane ours. A docked pane
    /// lives in the dashboard's session: a session-name filter would drop it
    /// from the poll, lose its status and prune it as dead. Equally, a pane
    /// that merely shares an agent session is not a shell.
    #[test]
    fn list_panes_selects_on_identity_not_on_the_session_name() {
        with_tmux_server(|_sock, tmux| {
            // A stamped pane, then moved to a session that is not ours —
            // this is what a docked pane looks like from the poll's side.
            let agent = tmux
                .new_session(
                    "wingman-session-id",
                    "lw:feat/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-id", "feat", 0),
                )
                .expect("agent session");
            // An unstamped pane inside that same agent session.
            let plain = tmux
                .run_raw(&[
                    "split-window",
                    "-d",
                    "-t",
                    &agent.pane_id,
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "bash",
                ])
                .expect("split")
                .trim()
                .to_string();

            let panes = tmux.list_panes().expect("panes");
            let ours: Vec<&str> = panes.iter().map(|p| p.pane_id.as_str()).collect();
            assert_eq!(
                ours,
                vec![agent.pane_id.as_str()],
                "only the stamped pane is a shell (unstamped: {plain})"
            );
            let got = &panes[0];
            assert_eq!(
                got.shell,
                Some(ShellId::for_session("wingman-session-id", "feat", 0))
            );

            // Now move it into 'main' — a session that is not an agent
            // session at all. It must still come back.
            tmux.run_raw(&["swap-pane", "-d", "-s", &agent.pane_id, "-t", "%0"])
                .expect("swap into the dashboard session");
            let panes = tmux.list_panes().expect("panes after the swap");
            let got = panes
                .iter()
                .find(|p| p.pane_id == agent.pane_id)
                .expect("a docked pane is still one of ours");
            assert_eq!(got.session_name, "main");
            assert_eq!(
                got.shell,
                Some(ShellId::for_session("wingman-session-id", "feat", 0)),
                "identity travels with the pane"
            );
        });
    }

    /// Both spawn paths stamp the pane before returning. `new_session`
    /// creates its first window on a different tmux command than
    /// `new_window`, so each is asserted separately — a shell spawned
    /// through the path nobody tested would be invisible to the poll.
    #[test]
    fn both_spawn_paths_stamp_the_pane_with_its_identity() {
        with_tmux_server(|_sock, tmux| {
            let read = |pane: &str, key: &str| {
                tmux.run_raw(&["display-message", "-p", "-t", pane, &format!("#{{{key}}}")])
                    .expect("display-message")
                    .trim()
                    .to_string()
            };
            let assert_stamped = |pane: &str, worktree: &str, index: &str| {
                assert_eq!(read(pane, dock::REPO_KEY), "stamp");
                assert_eq!(read(pane, dock::WORKTREE_KEY), worktree);
                assert_eq!(read(pane, dock::INDEX_KEY), index);
            };

            let first = tmux
                .new_session(
                    "wingman-session-stamp",
                    "lw:feat/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-stamp", "feat", 0),
                )
                .expect("session");
            assert_stamped(&first.pane_id, "feat", "0");

            let second = tmux
                .new_window(
                    "wingman-session-stamp",
                    "lw:feat/1",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-stamp", "feat", 1),
                )
                .expect("window");
            assert_stamped(&second.pane_id, "feat", "1");
        });
    }

    #[test]
    fn pane_option_roundtrip_and_clear() {
        with_tmux_server(|sock, tmux| {
            tmux.new_session(
                "wingman-session-opt",
                "lw:feat/0",
                std::path::Path::new("/tmp"),
                "bash",
                &ShellId::for_session("wingman-session-opt", "feat", 0),
            )
            .expect("session");
            let pane = tmux
                .list_panes()
                .expect("panes")
                .first()
                .expect("one pane")
                .clone();
            assert_eq!(pane.overlay_raw, "", "unset panes read as empty");

            tmux.set_pane_option(&pane.pane_id, crate::hooks::OVERLAY_KEY, "blocked:12")
                .expect("set");
            let after = tmux.list_panes().expect("panes");
            let got = after
                .iter()
                .find(|p| p.pane_id == pane.pane_id)
                .expect("same pane");
            assert_eq!(got.overlay_raw, "blocked:12");

            tmux.clear_pane_option(&pane.pane_id, crate::hooks::OVERLAY_KEY)
                .expect("clear");
            let cleared = tmux.list_panes().expect("panes");
            let got = cleared
                .iter()
                .find(|p| p.pane_id == pane.pane_id)
                .expect("same pane");
            assert_eq!(got.overlay_raw, "", "cleared reads as empty");
            let _ = sock;
        });
    }

    #[test]
    fn hook_writes_and_clears_the_overlay_on_a_real_pane() {
        with_tmux_server(|_sock, tmux| {
            tmux.new_session(
                "wingman-session-hook",
                "lw:feat/0",
                std::path::Path::new("/tmp"),
                "bash",
                &ShellId::for_session("wingman-session-hook", "feat", 0),
            )
            .expect("session");
            let pane = tmux
                .list_panes()
                .expect("panes")
                .first()
                .expect("pane")
                .clone();

            crate::hooks::run_hook("Stop", &pane.pane_id, &tmux, &serde_json::Value::Null)
                .expect("stop hook");
            let got = tmux.list_panes().expect("panes");
            let raw = &got
                .iter()
                .find(|p| p.pane_id == pane.pane_id)
                .expect("pane")
                .overlay_raw;
            assert!(raw.starts_with("waiting:"), "overlay reads {raw:?}");

            let msg = serde_json::json!({"message": "Claude needs your permission to use Bash"});
            crate::hooks::run_hook("Notification", &pane.pane_id, &tmux, &msg)
                .expect("notification hook");
            let got = tmux.list_panes().expect("panes");
            let raw = &got
                .iter()
                .find(|p| p.pane_id == pane.pane_id)
                .expect("pane")
                .overlay_raw;
            assert!(raw.starts_with("blocked:"), "overlay reads {raw:?}");

            crate::hooks::run_hook("SessionEnd", &pane.pane_id, &tmux, &serde_json::Value::Null)
                .expect("session end hook");
            let got = tmux.list_panes().expect("panes");
            assert_eq!(
                got.iter()
                    .find(|p| p.pane_id == pane.pane_id)
                    .expect("pane")
                    .overlay_raw,
                ""
            );
        });
    }

    #[test]
    fn shell_detection() {
        assert!(is_shell_command("bash"));
        assert!(is_shell_command("zsh"));
        assert!(is_shell_command("fish"));
        assert!(is_shell_command("sh"));
        assert!(!is_shell_command("claude"));
        assert!(!is_shell_command("codex exec"));
        assert!(!is_shell_command(""));
    }

    // ---- integration tests against a real (throwaway) tmux server ----
    // Server setup lives in the shared `testutil` module (see below) so the
    // app-level handoff tests can reuse it — all tmux tests in the binary
    // must serialize on ONE lock, because they mutate process-global env
    // vars (`LAZYWINGMAN_TMUX_SOCKET`, TMUX/TMUX_PANE).
    use super::testutil::*;

    #[test]
    fn detect_finds_our_coordinates() {
        with_tmux_server(|_sock, _t| {
            // Simulate being inside tmux for the duration of detect() only.
            unsafe {
                std::env::set_var("TMUX", "1,0,");
                std::env::set_var("TMUX_PANE", "%0");
            }
            let detected = Tmux::detect().unwrap();
            unsafe {
                std::env::remove_var("TMUX");
                std::env::remove_var("TMUX_PANE");
            }
            let inside = detected.inside.unwrap();
            assert_eq!(inside.session_name, "main");
            assert_eq!(inside.window_id, "@0");
            assert_eq!(inside.pane_id, "%0");
        });
    }

    #[test]
    fn detect_standalone_when_not_in_tmux() {
        unsafe {
            std::env::remove_var("TMUX");
            std::env::remove_var("TMUX_PANE");
        }
        let t = Tmux::detect().unwrap();
        assert!(t.inside.is_none());
    }

    #[test]
    fn ensure_server_starts_a_server_when_none_running() {
        testutil::with_no_server(|sock| {
            let tmux = testutil::tmux_standalone(sock);
            // no server yet: list-sessions fails, panes list is empty
            assert!(tmux.list_sessions().is_err());
            assert!(tmux.list_panes().unwrap().is_empty());

            tmux.ensure_server().unwrap();

            // server is now reachable and stays alive with zero sessions
            assert!(tmux.list_sessions().unwrap().is_empty());
            assert!(tmux.list_panes().unwrap().is_empty());
            // and real agent sessions can be created on it
            let w = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:f/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "f", 0),
                )
                .unwrap();
            assert!(w.window_id.starts_with('@'));
            assert!(tmux.session_exists("wingman-session-p"));
        });
    }

    #[test]
    fn session_lifecycle_roundtrip() {
        with_tmux_server(|_sock, tmux| {
            // dashboard session "main" exists; no agent sessions yet
            assert!(!tmux.session_exists("wingman-session-p"));
            let sessions = tmux.list_sessions().unwrap();
            assert!(sessions.iter().all(|s| s.name != "wingman-session-p"));

            // create the agent session with its first window
            let w0 = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:feat/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "feat", 0),
                )
                .unwrap();
            assert!(w0.window_id.starts_with('@'));
            assert!(w0.pane_id.starts_with('%'));
            assert!(tmux.session_exists("wingman-session-p"));

            // a second window joins the session
            let w1 = tmux
                .new_window(
                    "wingman-session-p",
                    "lw:feat/1",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "feat", 1),
                )
                .unwrap();
            // panes across agent sessions are visible; dashboard panes are NOT
            let panes = tmux.list_panes().unwrap();
            assert_eq!(panes.len(), 2, "one shell per window: {panes:?}");
            assert!(panes.iter().any(|p| p.window_id == w0.window_id));
            assert!(panes.iter().any(|p| p.window_id == w1.window_id));
            assert!(panes.iter().any(|p| p.pane_id == w0.pane_id));
            assert!(panes.iter().any(|p| p.pane_id == w1.pane_id));
            assert!(!panes.iter().any(|p| p.pane_id == "%0")); // main session pane

            // select-window works clientless and changes the active window
            tmux.select_window_in_session("wingman-session-p", &w1.window_id)
                .unwrap();
            let active = tmux
                .run_raw(&[
                    "display-message",
                    "-p",
                    "-t",
                    "wingman-session-p",
                    "#{window_id}",
                ])
                .unwrap();
            assert_eq!(active.trim(), w1.window_id);

            // killing the LAST window destroys the session
            tmux.kill_window(&w0.window_id).unwrap();
            assert!(tmux.session_exists("wingman-session-p"));
            tmux.kill_window(&w1.window_id).unwrap();
            assert!(!tmux.session_exists("wingman-session-p"));
        });
    }

    #[test]
    fn spawn_send_capture_kill_roundtrip() {
        with_tmux_server(|_sock, tmux| {
            let w = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:f/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "f", 0),
                )
                .unwrap();
            assert!(w.window_id.starts_with('@'));
            assert!(w.pane_id.starts_with('%'));

            // the pane exists and runs a shell
            let panes = tmux.list_panes().unwrap();
            assert!(panes.iter().any(|p| p.pane_id == w.pane_id));
            let p = panes.iter().find(|p| p.pane_id == w.pane_id).unwrap();
            assert!(is_shell_command(&p.current_command));
            assert!(!p.dead);

            // send a command and capture the output
            tmux.send_keys(&w.pane_id, "echo lw-test-123", true)
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            let capture = tmux.capture_pane(&w.pane_id, 50).unwrap();
            assert!(capture.contains("lw-test-123"), "capture: {capture}");

            // kill the last window → session gone
            tmux.kill_window(&w.window_id).unwrap();
            assert!(!tmux.session_exists("wingman-session-p"));
        });
    }

    /// The preview's scrollback source reaches past the poll's window:
    /// `capture_pane` returns only the lines it is asked for, while
    /// `capture_pane_full` returns everything the pane still holds.
    #[test]
    fn full_capture_reaches_past_the_polls_shallow_window() {
        with_tmux_server(|_sock, tmux| {
            let w = tmux
                .new_session(
                    "wingman-session-s",
                    "lw:s/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-s", "s", 0),
                )
                .unwrap();
            tmux.send_keys(
                &w.pane_id,
                "for i in $(seq 1 400); do echo lw-line-$i; done",
                true,
            )
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1500));

            // exact line match: `lw-line-1` is a prefix of `lw-line-100`
            let has = |cap: &str, needle: &str| {
                cap.lines()
                    .any(|l| crate::ansi::strip(l).trim_end() == needle)
            };
            let shallow = tmux.capture_pane(&w.pane_id, 50).unwrap();
            let full = tmux.capture_pane_full(&w.pane_id).unwrap();

            assert!(has(&shallow, "lw-line-400"), "both end at the newest line");
            assert!(has(&full, "lw-line-400"));
            assert!(
                !has(&shallow, "lw-line-1"),
                "the poll's window stops well short of the start"
            );
            assert!(
                has(&full, "lw-line-1"),
                "the full capture reaches the oldest line the pane holds"
            );
        });
    }

    /// End-to-end colour path: a pane that prints SGR sequences comes back
    /// from `capture-pane -e` with them intact, they survive into the
    /// preview's styled text, and stripping leaves the plain line the status
    /// patterns match against.
    #[test]
    fn capture_keeps_pane_colours_for_the_preview() {
        with_tmux_server(|_sock, tmux| {
            let w = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:f/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "f", 0),
                )
                .unwrap();
            tmux.send_keys(&w.pane_id, r#"printf '\033[31mlw-red\033[0m\n'"#, true)
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            let capture = tmux.capture_pane(&w.pane_id, 50).unwrap();

            assert!(
                capture.contains("\x1b[31m"),
                "capture must carry the pane's escapes: {capture:?}"
            );
            assert!(crate::ansi::strip(&capture).contains("lw-red"));

            let text = crate::ansi::styled_text(&capture, crate::config::PreviewColors::Full);
            // (the echoed command line also contains "lw-red", but as the
            // literal text the shell was sent — only the pane's *output*
            // carries a real escape and comes back styled)
            assert!(
                text.lines
                    .iter()
                    .flat_map(|l| l.spans.iter())
                    .any(|s| s.content.contains("lw-red")
                        && s.style.fg == Some(ratatui::style::Color::Red)),
                "coloured output must reach the preview: {capture:?}"
            );

            tmux.kill_window(&w.window_id).unwrap();
        });
    }

    #[test]
    fn paste_keys_inserts_literal_multiline_text() {
        with_tmux_server(|_sock, tmux| {
            let w = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:f/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "f", 0),
                )
                .unwrap();
            // Paste a multi-line prompt WITHOUT a trailing Enter: bracketed
            // paste (set-buffer + paste-buffer -p) delivers the text
            // literally — embedded newlines must NOT submit the line.
            tmux.paste_keys(&w.pane_id, "lw-line-1\nlw-line-2", false)
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            let capture = tmux.capture_pane(&w.pane_id, 50).unwrap();
            assert!(
                capture.contains("lw-line-1") && capture.contains("lw-line-2"),
                "both pasted lines visible: {capture}"
            );
            // The paste buffer is cleaned up afterwards.
            let buffers = tmux.run_raw(&["list-buffers"]).unwrap_or_default();
            assert!(
                !buffers.contains("lw-paste"),
                "paste buffer must be deleted: {buffers}"
            );
        });
    }

    #[test]
    fn dead_pane_is_reported() {
        with_tmux_server(|_sock, tmux| {
            // run a command that exits immediately: window dies
            let w = tmux
                .new_session(
                    "wingman-session-p",
                    "lw:f/1",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-p", "f", 1),
                )
                .unwrap();
            tmux.send_keys(&w.pane_id, "exit", true).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(400));
            let panes = tmux.list_panes().unwrap();
            let p = panes.iter().find(|p| p.pane_id == w.pane_id);
            match p {
                Some(p) => assert!(p.dead, "pane should be dead"),
                None => {
                    // window auto-closed when the shell exited; both outcomes
                    // are fine — dead or gone.
                }
            }
        });
    }

    #[test]
    fn back_key_binding_registers_in_prefix_table_not_root() {
        with_tmux_server(|_sock, tmux| {
            tmux.new_session(
                "wingman-session-p",
                "lw:f/0",
                Path::new("/tmp"),
                "bash",
                &ShellId::for_session("wingman-session-p", "f", 0),
            )
            .unwrap();
            tmux.bind_back_key("C-o", "main", "@0").unwrap();
            let prefix = tmux.run_raw(&["list-keys", "-T", "prefix"]).unwrap();
            assert!(
                prefix.contains("C-o") && prefix.contains("switch-client -t main"),
                "back key not in prefix table: {prefix}"
            );
            assert!(prefix.contains("select-window -t @0"));
            // The root table must be untouched — that is the whole point
            // (a root binding would hijack C-o for every TUI in the server).
            let root = tmux.run_raw(&["list-keys", "-T", "root"]).unwrap();
            assert!(
                !root
                    .lines()
                    .any(|l| l.contains("C-o") && l.contains("switch-client -t main")),
                "root table must not carry the back key: {root}"
            );
        });
    }

    /// Against a real server: what `bind_back_key` writes must read back as
    /// ours, `bind_detach_back_key` as a handoff's, and an unbind as empty.
    #[test]
    fn the_back_chord_round_trips_through_a_real_server() {
        with_tmux_server(|_sock, tmux| {
            // A key tmux itself does not bind reads as empty; `C-o` is
            // tmux's own `rotate-window` on a fresh server, which reads as
            // Foreign — the standalone handoff leaves that alone.
            assert_eq!(tmux.back_key_state("F12").unwrap(), BackKeyState::Unbound);
            assert_eq!(tmux.back_key_state("C-o").unwrap(), BackKeyState::Foreign);
            tmux.bind_back_key("C-o", "main", "@3").unwrap();
            assert_eq!(
                tmux.back_key_state("C-o").unwrap(),
                BackKeyState::Dashboard {
                    session: "main".into(),
                    window: "3".into()
                }
            );
            tmux.bind_detach_back_key("C-o").unwrap();
            assert_eq!(tmux.back_key_state("C-o").unwrap(), BackKeyState::Detach);
            tmux.unbind_back_key("C-o").unwrap();
            assert_eq!(tmux.back_key_state("C-o").unwrap(), BackKeyState::Unbound);
        });
    }

    #[test]
    fn legacy_root_back_key_is_unbound_but_user_binding_survives() {
        with_tmux_server(|sock, tmux| {
            // Recreate a pre-M9 root binding exactly as the old code did.
            tmux.run(&[
                "bind-key",
                "-n",
                "C-o",
                "switch-client",
                "-t",
                "main",
                "\\;",
                "select-window",
                "-t",
                "@0",
            ])
            .unwrap();
            tmux.new_session(
                "wingman-session-p",
                "lw:f/0",
                Path::new("/tmp"),
                "bash",
                &ShellId::for_session("wingman-session-p", "f", 0),
            )
            .unwrap();
            // A user's own root binding on a DIFFERENT key must survive:
            // bind C-t to something else in the root table.
            tmux.run(&["bind-key", "-n", "C-t", "choose-tree"]).unwrap();

            tmux.unbind_legacy_root_back_key("main", "@0").unwrap();

            let root = tmux.run_raw(&["list-keys", "-T", "root"]).unwrap();
            assert!(
                !root
                    .lines()
                    .any(|l| l.contains("C-o") && l.contains("switch-client -t main")),
                "legacy back key must be gone: {root}"
            );
            assert!(
                root.lines()
                    .any(|l| l.contains("C-t") && l.contains("choose-tree")),
                "user's own root binding must survive: {root}"
            );
            let _ = sock;
        });
    }
}

/// Shared fixture for tests that need a real (throwaway) tmux server.
///
/// Exposed to the app-level tests too, so the M8.2 handoff path can be
/// verified at the command level. Serialisation is NOT a lock of its own:
/// these fixtures and `util::testutil::with_fresh_xdg` nest in both orders,
/// so two locks could invert and hang the suite. They share one reentrant
/// lock — see `util::testutil::with_test_lock`.
#[cfg(test)]
pub(crate) mod testutil {
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::{Inside, Tmux};
    use crate::util::testutil::with_test_lock;

    static SOCK_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Run `f` against a throwaway tmux server on a unique socket.
    ///
    /// Sets `LAZYWINGMAN_TMUX_SOCKET` and clears TMUX/TMUX_PANE (a fake
    /// value makes the tmux client behave as a nested client and breaks
    /// session/window creation). The server is killed on the way out even if
    /// the test body panics.
    pub fn with_tmux_server(f: impl FnOnce(&str, Tmux)) {
        // Skip gracefully when tmux isn't installed.
        if Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("skipping: tmux not available");
            return;
        }
        with_test_lock(|| with_tmux_server_locked(f));
    }

    fn with_tmux_server_locked(f: impl FnOnce(&str, Tmux)) {
        // Unique socket per invocation: zero cross-test server coupling.
        let n = SOCK_COUNTER.fetch_add(1, Ordering::SeqCst);
        let sock = format!("lwtest-{}-{n}", std::process::id());
        let status = Command::new("tmux")
            .args([
                "-L",
                &sock,
                "new-session",
                "-d",
                "-s",
                "main",
                "-x",
                "200",
                "-y",
                "50",
            ])
            .status()
            .unwrap();
        assert!(status.success(), "new-session failed on socket {sock}");
        // Let the server finish initialising its first window — window
        // creation right after `new-session -d` can race window index 0.
        std::thread::sleep(std::time::Duration::from_millis(150));
        unsafe {
            std::env::set_var("LAZYWINGMAN_TMUX_SOCKET", &sock);
            std::env::remove_var("TMUX");
            std::env::remove_var("TMUX_PANE");
        }
        let tmux = Tmux {
            inside: Some(Inside {
                session_name: "main".into(),
                window_id: "@0".into(),
                pane_id: "%0".into(),
            }),
            socket: Some(sock.clone()),
        };
        // Guarantee the server is torn down even if the test body panics.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&sock, tmux)));
        let _ = Command::new("tmux")
            .args(["-L", &sock, "kill-server"])
            .status();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    /// A handle pretending the dashboard runs inside the `main` session.
    pub fn tmux_inside(sock: &str) -> Tmux {
        Tmux {
            inside: Some(Inside {
                session_name: "main".into(),
                window_id: "@0".into(),
                pane_id: "%0".into(),
            }),
            socket: Some(sock.to_string()),
        }
    }

    /// A handle for a dashboard running in a plain terminal (no tmux around
    /// it), still managing the same tmux server.
    pub fn tmux_standalone(sock: &str) -> Tmux {
        Tmux {
            inside: None,
            socket: Some(sock.to_string()),
        }
    }

    /// Run `f` with a unique socket and NO server running on it — the test
    /// body is expected to start one itself (e.g. via `ensure_server`).
    /// Serialises on the same lock as `with_tmux_server`.
    pub fn with_no_server(f: impl FnOnce(&str)) {
        with_test_lock(|| with_no_server_locked(f));
    }

    fn with_no_server_locked(f: impl FnOnce(&str)) {
        let n = SOCK_COUNTER.fetch_add(1, Ordering::SeqCst);
        let sock = format!("lwtest-{}-{n}", std::process::id());
        // Clear any leftover server from a previous run on this socket.
        let _ = Command::new("tmux")
            .args(["-L", &sock, "kill-server"])
            .status();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&sock)));
        let _ = Command::new("tmux")
            .args(["-L", &sock, "kill-server"])
            .status();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
}
