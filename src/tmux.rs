//! tmux integration via its CLI.
//!
//! LazyWingman supervises agent shells that live in DEDICATED tmux sessions
//! (`wingman-session-<repository>`), never in the user's session — creating a
//! worktree or shell must not pollute the dashboard's session. The dashboard
//! itself is always a tmux pane (`main::should_wrap` execs into one when it
//! is not), so `inside` is `Some` for the whole life of the TUI; it stays an
//! `Option` because the `hook` subcommand runs on the same type from panes
//! that are not ours.
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
    /// The launched command (`@lw_command`), unescaped.
    pub command: Option<String>,
    /// Is the cursor still at the home position, with nothing drawn yet?
    /// A shell that has not finished reading its rc files has printed no
    /// prompt, and typing into it lands in the tty's line buffer instead
    /// of its line editor — see `App::drain_pending_launches`, the only
    /// reader.
    pub blank: bool,
}

/// One pane, seen only as furniture: where it sits and what kind of
/// LazyWingman fixture (if any) it is. The holder backfill and the slot
/// lookup both need exactly this and nothing else, so they share one call.
#[derive(Debug, Clone)]
pub struct FurniturePane {
    pub pane_id: String,
    pub window_id: String,
    pub session_name: String,
    /// Carries `@lw_holder`.
    pub holder: bool,
    /// Carries `@lw_slot`.
    pub slot: bool,
    /// Carries a shell identity (`@lw_repo`) — an agent pane, wherever it
    /// currently sits.
    pub shell: bool,
}

/// One tmux session (agent sessions only, filtered by the caller).
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub name: String,
    /// `@lw_repo_path`; `None` for a foreign session or one made by an older build.
    pub repo_path: Option<std::path::PathBuf>,
}

/// Result of spawning / listing a window.
#[derive(Debug, Clone)]
pub struct WindowRef {
    pub window_id: String,
    /// Active pane id (set by `new_window` / `new_session`).
    pub pane_id: String,
    /// The session the window is in. A window id is server-global, so this
    /// is only needed by the callers that must ATTACH to it — the self-wrap
    /// looking up an existing dashboard.
    pub session_name: String,
}

/// What a window still carrying `@lw_scope` actually holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeWindow {
    /// A dashboard is running in it. Attach; never start a second one.
    Live,
    /// No dashboard, but panes of ours are still there — a slot, a holder,
    /// or an agent pane that was docked in when the dashboard died. Split a
    /// fresh dashboard back beside panes that never died.
    Furniture,
    /// Neither. The mark outlived the dashboard that wrote it: usually the
    /// user's own shell pane, which survives the TUI quitting inside it.
    Stale,
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

    /// A `tmux` [`Command`] on our socket, for the callers that must run a
    /// tmux CLIENT rather than send it a command — the self-wrap, which
    /// `exec`s into `attach-session` and never returns.
    pub fn client_command(&self) -> Command {
        base_command(&self.socket)
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
        // The path is last: taken as the remainder, so a '|' in it is harmless.
        let fmt = format!("#{{session_name}}|#{{{}}}", dock::REPO_PATH_KEY);
        let out = match self.run(&["list-sessions", "-F", &fmt]) {
            Ok(o) => o,
            Err(e) if is_no_server_error(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(out
            .lines()
            .filter_map(|line| {
                let (name, path) = line.split_once('|').unwrap_or((line, ""));
                let name = name.trim().to_string();
                if name.is_empty() {
                    return None;
                }
                Some(SessionInfo {
                    name,
                    repo_path: (!path.is_empty()).then(|| std::path::PathBuf::from(path)),
                })
            })
            .collect())
    }

    /// Record which repository owns an agent session.
    pub fn stamp_session_repo(&self, session: &str, path: &Path) -> Result<()> {
        self.run(&[
            "set-option",
            "-t",
            session,
            dock::REPO_PATH_KEY,
            &path.to_string_lossy(),
        ])?;
        Ok(())
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
        holder_width: u16,
    ) -> Result<WindowRef> {
        let mut last = None;
        for attempt in 0..3 {
            match self.try_new_session(session, win_name, cwd, shell) {
                Ok(w) => {
                    self.stamp_shell(&w.pane_id, id)?;
                    // Before the holder: the session's windows must be off
                    // automatic sizing while we are still the only ones
                    // arranging them.
                    if let Err(e) = self.set_manual_window_size(session) {
                        log::warn!("cannot pin the size of session {session}: {e}");
                    }
                    self.furnish(&w.window_id, holder_width);
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

    /// Give a freshly spawned agent window its holder. Never fatal: a
    /// window without one still runs its agent, it merely cannot be docked
    /// from — and the next reconcile backfills it ([`dock::ensure_holders`]).
    fn furnish(&self, window_id: &str, holder_width: u16) {
        match self.split_holder(window_id, holder_width) {
            Ok(p) => log::info!("holder {p} for window {window_id}"),
            Err(e) => log::warn!("cannot split a holder into {window_id}: {e}"),
        }
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
        Ok(WindowRef {
            window_id,
            pane_id,
            session_name: session.to_string(),
        })
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
        holder_width: u16,
    ) -> Result<WindowRef> {
        let mut last = None;
        for attempt in 0..3 {
            match self.try_new_window(session, name, cwd, shell) {
                Ok(w) => {
                    self.stamp_shell(&w.pane_id, id)?;
                    self.furnish(&w.window_id, holder_width);
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
        Ok(WindowRef {
            window_id,
            pane_id,
            session_name: session.to_string(),
        })
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
    /// **Furniture is never a shell**, whatever options it carries. The
    /// rule used to be implicit — furniture carries no identity, so it
    /// could not parse as one — and that assumption was wrong: anything
    /// that stamps a pane by where it *sits* rather than by what it *is*
    /// can put a shell's identity on a holder, and then a dashboard docks
    /// the sleeping placeholder instead of the agent. So the exclusion is
    /// structural now: a `@lw_holder` or `@lw_slot` mark outranks any
    /// identity on the same pane.
    ///
    /// A server with no sessions yet yields an empty list, not an error.
    pub fn list_panes(&self) -> Result<Vec<PaneInfo>> {
        // Built from the option constants rather than spelled out, so the
        // read path cannot drift from what `stamp_shell` writes.
        let fmt = format!(
            "#{{pane_id}}|#{{window_id}}|#{{pane_current_command}}|#{{pane_dead}}|#{{session_name}}\
             |#{{{repo}}}|#{{{worktree}}}|#{{{index}}}|#{{pane_active}}|#{{{docked}}}\
             |#{{{holder}}}#{{{slot}}}|#{{cursor_x}}|#{{cursor_y}}|#{{{command}}}|#{{{overlay}}}",
            repo = dock::REPO_KEY,
            worktree = dock::WORKTREE_KEY,
            index = dock::INDEX_KEY,
            docked = dock::DOCKED_KEY,
            holder = dock::HOLDER_KEY,
            slot = dock::SLOT_KEY,
            command = dock::COMMAND_KEY,
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

    /// Record the command a shell was launched with; the tree names the row by it.
    pub fn stamp_command(&self, pane_id: &str, command: &str) -> Result<()> {
        self.set_pane_option(pane_id, dock::COMMAND_KEY, &escape_field(command))
    }

    // ----------------------------------------------------- furniture

    /// Split a **holder** into an agent window, `width` columns wide, on
    /// the left — the side the dashboard occupies in its own window.
    ///
    /// The holder is what makes docking a plain `swap-pane`: the agent
    /// window never drops to zero panes, so no window and no session is
    /// ever destroyed by a dock. `-d` keeps the agent pane active, so
    /// someone attaching the session directly still lands in their agent.
    pub fn split_holder(&self, window_id: &str, width: u16) -> Result<String> {
        let pane = self.split_furniture(window_id, width, true, dock::HOLDER_NOTE)?;
        self.set_pane_option(&pane, dock::HOLDER_KEY, "1")?;
        Ok(pane)
    }

    /// Split the dashboard's **slot** off `pane_id` (the dashboard's own
    /// pane), `width` columns wide, on the right.
    pub fn split_slot(&self, pane_id: &str, width: u16) -> Result<String> {
        let pane = self.split_furniture(pane_id, width, false, dock::SLOT_NOTE)?;
        self.set_pane_option(&pane, dock::SLOT_KEY, "1")?;
        Ok(pane)
    }

    fn split_furniture(
        &self,
        target: &str,
        width: u16,
        before: bool,
        note: &str,
    ) -> Result<String> {
        let width = width.max(1).to_string();
        let cmd = dock::park_command(note);
        let mut args: Vec<&str> = vec!["split-window", "-h", "-d", "-l", &width, "-t", target];
        if before {
            args.push("-b");
        }
        args.extend(["-P", "-F", "#{pane_id}", &cmd]);
        let out = self.run(&args)?;
        let pane = out.trim().to_string();
        if pane.is_empty() {
            return Err(Error::Tmux("split-window returned no pane id".into()));
        }
        Ok(pane)
    }

    /// Every pane on the server, as furniture: one call serving both the
    /// holder backfill and the slot lookup.
    pub fn list_furniture(&self) -> Result<Vec<FurniturePane>> {
        let fmt = format!(
            "#{{pane_id}}|#{{window_id}}|#{{session_name}}|#{{{holder}}}|#{{{slot}}}|#{{{repo}}}",
            holder = dock::HOLDER_KEY,
            slot = dock::SLOT_KEY,
            repo = dock::REPO_KEY,
        );
        let out = match self.run(&["list-panes", "-a", "-F", &fmt]) {
            Ok(o) => o,
            Err(e) if is_no_server_error(&e) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(out
            .lines()
            .filter_map(|line| {
                let mut it = line.splitn(6, '|');
                Some(FurniturePane {
                    pane_id: it.next()?.to_string(),
                    window_id: it.next()?.to_string(),
                    session_name: it.next()?.to_string(),
                    holder: !it.next()?.trim().is_empty(),
                    slot: !it.next()?.trim().is_empty(),
                    shell: !it.next().unwrap_or("").trim().is_empty(),
                })
            })
            .collect())
    }

    /// Exchange two panes. This is the whole of docking.
    ///
    /// `-d` keeps the active pane where it is: a swap must never move the
    /// user's cursor, least of all into a sleeping holder. Nothing here is
    /// created or destroyed — each window keeps its pane count, so no
    /// window and no session can die of a dock — and when the two panes
    /// are the same size tmux redraws from its own buffer, so the agent
    /// sees no `SIGWINCH` and does not repaint.
    ///
    /// Both panes must live on the same tmux server; `swap-pane` cannot
    /// cross servers, which is why the dashboard and its agent sessions
    /// share one.
    pub fn swap_panes(&self, a: &str, b: &str) -> Result<()> {
        self.run(&["swap-pane", "-d", "-s", a, "-t", b])?;
        Ok(())
    }

    /// Kill one pane. Killing a window's last pane kills the window, so
    /// this is only ever aimed at furniture that has a neighbour.
    pub fn kill_pane(&self, pane_id: &str) -> Result<()> {
        self.run(&["kill-pane", "-t", pane_id])?;
        Ok(())
    }

    // ------------------------------------------------------- geometry

    /// A window's size, in cells. Test-only: the layout is laid out
    /// against the dashboard's own pane ([`Self::pane_geometry`]), never
    /// against the window it happens to share with panes of the user's.
    #[cfg(test)]
    pub fn window_size(&self, window_id: &str) -> Result<(u16, u16)> {
        let out = self.run(&[
            "display-message",
            "-p",
            "-t",
            window_id,
            "#{window_width}|#{window_height}",
        ])?;
        let mut it = out.trim().splitn(2, '|');
        let x = it.next().unwrap_or("").parse::<u16>().ok();
        let y = it.next().unwrap_or("").parse::<u16>().ok();
        match (x, y) {
            (Some(x), Some(y)) => Ok((x, y)),
            _ => Err(Error::Tmux(format!("cannot read the size of {window_id}"))),
        }
    }

    /// A pane's size in cells, and the window it is in.
    ///
    /// The dashboard lays its furniture out against its own pane rather
    /// than against the window: a window may hold panes that are none of
    /// our business, and a split can only ever divide the pane it is aimed
    /// at. The window id comes back with the size because the only other
    /// pane the layout may count — the one beside us — counts only while it
    /// really is beside us.
    pub fn pane_geometry(&self, pane_id: &str) -> Result<(u16, u16, String)> {
        let out = self.run(&[
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{pane_width}|#{pane_height}|#{window_id}",
        ])?;
        let mut it = out.trim().splitn(3, '|');
        let x = it.next().unwrap_or("").parse::<u16>().ok();
        let y = it.next().unwrap_or("").parse::<u16>().ok();
        let w = it.next().unwrap_or("").trim().to_string();
        match (x, y) {
            (Some(x), Some(y)) if !w.is_empty() => Ok((x, y, w)),
            _ => Err(Error::Tmux(format!(
                "cannot read the geometry of {pane_id}"
            ))),
        }
    }

    /// Force a window to a size. Only meaningful once its session carries
    /// `window-size manual` ([`Self::set_manual_window_size`]); otherwise
    /// tmux recomputes the size from the attached clients and undoes this.
    pub fn resize_window(&self, window_id: &str, x: u16, y: u16) -> Result<()> {
        self.run(&[
            "resize-window",
            "-t",
            window_id,
            "-x",
            &x.to_string(),
            "-y",
            &y.to_string(),
        ])?;
        Ok(())
    }

    /// Set one pane's width, leaving its neighbour the rest.
    pub fn resize_pane_width(&self, pane_id: &str, width: u16) -> Result<()> {
        self.run(&[
            "resize-pane",
            "-t",
            pane_id,
            "-x",
            &width.max(1).to_string(),
        ])?;
        Ok(())
    }

    /// Is `pane_id` the active pane of its window — i.e. does it hold the
    /// keyboard?
    ///
    /// Asked of tmux on demand rather than cached in the poll: the answer
    /// changes the moment the user moves between panes, and a docking
    /// decision made against a poll-old answer is exactly the "swap a pane
    /// out from under someone mid-sentence" bug the guard exists to
    /// prevent.
    pub fn pane_active(&self, pane_id: &str) -> Result<bool> {
        let out = self.run(&["display-message", "-p", "-t", pane_id, "#{pane_active}"])?;
        Ok(out.trim() == "1")
    }

    /// The scope announced by the window `pane_id` sits in, if it announces
    /// one. This is how a dashboard can name the *other* dashboard holding
    /// a shell — `@lw_scope` is a window option, so asking a pane for it
    /// asks its window.
    pub fn pane_scope(&self, pane_id: &str) -> Option<String> {
        let fmt = format!("#{{{}}}", dock::SCOPE_KEY);
        let out = self
            .run(&["display-message", "-p", "-t", pane_id, &fmt])
            .ok()?;
        let out = out.trim();
        (!out.is_empty()).then(|| out.to_string())
    }

    /// Take a session's windows off automatic sizing, so their dimensions
    /// are ours to set and a client attaching elsewhere cannot change them
    /// behind our back.
    pub fn set_manual_window_size(&self, session: &str) -> Result<()> {
        self.run(&["set-option", "-t", session, "window-size", "manual"])?;
        Ok(())
    }

    // -------------------------------------------------- window options

    /// Set a window user option (`@key`). Used to stamp the dashboard's own
    /// window with the scope it is showing, so a second launch in the same
    /// folder finds the running dashboard instead of starting a rival one.
    pub fn set_window_option(&self, window_id: &str, key: &str, value: &str) -> Result<()> {
        self.run(&["set-option", "-w", "-t", window_id, key, value])?;
        Ok(())
    }

    /// The window announcing `scope`, if one exists on this server.
    ///
    /// One `list-windows -a`, the same discipline as the pane poll: the
    /// scope is a user-chosen path and may itself contain `|`, so it goes
    /// LAST and the parse keeps the remainder whole.
    pub fn find_scope_window(&self, scope: &Path) -> Result<Option<WindowRef>> {
        let want = scope.to_string_lossy();
        let fmt = format!(
            "#{{window_id}}|#{{session_name}}|#{{pane_id}}|#{{{scope}}}",
            scope = dock::SCOPE_KEY
        );
        let out = match self.run(&["list-windows", "-a", "-F", &fmt]) {
            Ok(o) => o,
            // No server, hence no dashboard: not an error, just no match.
            Err(e) if is_no_server_error(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(out.lines().find_map(|line| {
            let mut it = line.splitn(4, '|');
            let window_id = it.next()?.to_string();
            let session_name = it.next()?.to_string();
            // In `list-windows`, `#{pane_id}` is the window's ACTIVE pane.
            let pane_id = it.next()?.to_string();
            let announced = it.next()?;
            if announced.is_empty() || announced != want {
                return None;
            }
            Some(WindowRef {
                window_id,
                pane_id,
                session_name,
            })
        }))
    }

    /// What a window carrying `@lw_scope` actually holds *right now*.
    ///
    /// The mark is written at startup and nothing unwrites it, so its
    /// presence proves only that a dashboard was once here. The pane it was
    /// running in usually outlives it — quitting a TUI started from a shell
    /// pane hands that pane back to the shell — and a window is not
    /// evidence of a live dashboard. `#{pane_current_command}` is, because
    /// tmux recomputes it from the pane's foreground process every time it
    /// is asked.
    pub fn scope_window_state(&self, window_id: &str, exe: &str) -> Result<ScopeWindow> {
        let fmt = format!(
            "#{{pane_current_command}}|#{{{repo}}}|#{{{holder}}}|#{{{slot}}}",
            repo = dock::REPO_KEY,
            holder = dock::HOLDER_KEY,
            slot = dock::SLOT_KEY,
        );
        let out = self.run(&["list-panes", "-t", window_id, "-F", &fmt])?;
        let mut furniture = false;
        for line in out.lines() {
            let mut it = line.split('|');
            if it.next().unwrap_or("").trim() == exe {
                return Ok(ScopeWindow::Live);
            }
            if it.any(|f| !f.trim().is_empty()) {
                furniture = true;
            }
        }
        Ok(if furniture {
            ScopeWindow::Furniture
        } else {
            ScopeWindow::Stale
        })
    }

    /// Unset a window user option. Used to retract a stale `@lw_scope`, so
    /// the next launch does not have to rediscover the same lie.
    pub fn clear_window_option(&self, window_id: &str, key: &str) -> Result<()> {
        self.run(&["set-option", "-w", "-u", "-t", window_id, key])?;
        Ok(())
    }

    /// Split a dashboard into `window_id`, beside whatever is already there.
    pub fn spawn_dashboard_pane(
        &self,
        window_id: &str,
        cwd: &Path,
        exe: &Path,
        argv: &[String],
    ) -> Result<()> {
        let mut args: Vec<String> = vec![
            "split-window".into(),
            "-t".into(),
            window_id.into(),
            "-c".into(),
            cwd.to_string_lossy().into_owned(),
            "--".into(),
            exe.to_string_lossy().into_owned(),
        ];
        args.extend(argv.iter().cloned());
        self.run(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
        Ok(())
    }

    /// Open a new window running a dashboard in an existing session, and
    /// return its id.
    ///
    /// This is what `new-session -A` cannot do: given an existing session
    /// `-A` attaches to it and silently DROPS the command, so a wrapper
    /// session that outlived its dashboard would be attached to empty-handed.
    pub fn spawn_dashboard_window(
        &self,
        session: &str,
        cwd: &Path,
        exe: &Path,
        argv: &[String],
    ) -> Result<String> {
        let mut args: Vec<String> = vec![
            "new-window".into(),
            "-t".into(),
            session.into(),
            "-c".into(),
            cwd.to_string_lossy().into_owned(),
            "-P".into(),
            "-F".into(),
            "#{window_id}".into(),
            "--".into(),
            exe.to_string_lossy().into_owned(),
        ];
        args.extend(argv.iter().cloned());
        let out = self.run(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
        let id = out.trim().to_string();
        if id.is_empty() {
            return Err(Error::Tmux("new-window returned no window id".into()));
        }
        Ok(id)
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

    /// Put the cursor in `pane_id`.
    ///
    /// This is the second half of docking: the pane is beside the tree, and
    /// one key should leave the user typing in it. No `switch-client` and no
    /// `select-window` — a docked pane is in the dashboard's own window, so
    /// nothing but the focus has to move.
    pub fn select_pane(&self, pane_id: &str) -> Result<()> {
        self.run(&["select-pane", "-t", pane_id])?;
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
}

/// `list-panes -F` output is one `|`-separated line per pane, so a free-form
/// value in a middle field cannot carry either character.
fn escape_field(s: &str) -> String {
    s.replace('%', "%25")
        .replace('|', "%7C")
        .replace('\n', "%0A")
}

fn unescape_field(s: &str) -> String {
    s.replace("%7C", "|")
        .replace("%0A", "\n")
        .replace("%25", "%")
}

fn parse_pane_line(line: &str) -> Option<PaneInfo> {
    // Field order: pane | window | command | dead | session | repo |
    // worktree | index | active | docked | furniture | cursor_x | cursor_y
    // | command | overlay. Every new field goes BEFORE the overlay: the overlay
    // is taken verbatim as the remainder, so a '|' inside it cannot desync
    // parsing, and only a last field can afford that (the command is escaped
    // instead). `furniture` is the holder and slot marks concatenated —
    // neither has a value worth telling apart here, only whether either is set.
    let mut it = line.splitn(15, '|');
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
    let furniture = !it.next().unwrap_or("").trim().is_empty();
    // Home cursor: the pane is still bare. Both coordinates are read (and
    // each `next()` bound to its own name, so no short-circuit can leave a
    // field unconsumed and desync the overlay behind it), because a prompt
    // may well sit on the first column of a later line.
    let home_x = it.next().unwrap_or("0").trim() == "0";
    let home_y = it.next().unwrap_or("0").trim() == "0";
    let blank = home_x && home_y;
    let command = it.next().unwrap_or("");
    let command = (!command.is_empty()).then(|| unescape_field(command));
    Some(PaneInfo {
        command,
        pane_id,
        window_id,
        current_command,
        dead,
        session_name,
        // A holder or a slot is furniture even if something has written
        // an identity onto it: the placeholder is not the agent.
        shell: if furniture {
            None
        } else {
            ShellId::from_fields(repo, worktree, index)
        },
        active,
        docked_by: (!docked_by.is_empty()).then_some(docked_by),
        overlay_raw: it.next().unwrap_or("").to_string(),
        blank,
    })
}

/// tmux errors that merely mean "no server / no sessions yet" — normal on
/// the read paths (`list-panes -a`, `list-windows -a`) before any session
/// exists, and never a reason to fail a poll.
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
        let p = parse_pane_line("%3|@1|claude|0|wingman-session-p|p|feat|2|1||").unwrap();
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
        let dead = parse_pane_line("%9|@1|sh|1|wingman-session-p|p|feat|2|0||").unwrap();
        assert!(dead.dead);
        assert!(!dead.active);
    }

    /// An unstamped pane parses but carries no identity — that is what
    /// `list_panes` filters on, so "unset" must read as `None`, not as a
    /// shell named the empty string.
    #[test]
    fn a_pane_without_the_options_carries_no_identity() {
        let p = parse_pane_line("%0|@1|bash|0|work||||1||").expect("parses");
        assert_eq!(p.shell, None);
        assert_eq!(p.session_name, "work");
    }

    /// The docking dashboard's pane id, when there is one.
    #[test]
    fn docked_by_is_read_back_when_set() {
        let p = parse_pane_line("%3|@1|claude|0|main|p|feat|0|1|%7||").expect("parses");
        assert_eq!(p.docked_by.as_deref(), Some("%7"));
    }

    #[test]
    fn pane_line_parsing_keeps_the_overlay_verbatim() {
        let p =
            parse_pane_line("%3|@1|claude|0|wingman-session-x|x|f|0|1|||0|0||blocked:1755600000")
                .expect("parses");
        assert_eq!(p.pane_id, "%3");
        assert_eq!(p.current_command, "claude");
        assert!(!p.dead);
        assert_eq!(p.command, None);
        assert_eq!(p.overlay_raw, "blocked:1755600000");
    }

    /// The command sits in a middle field, so it is escaped rather than taken
    /// as a remainder.
    #[test]
    fn an_escaped_command_cannot_desync_the_overlay() {
        let command = "codex | tee 100%\nlog";
        let line = format!(
            "%3|@1|node|0|wingman-session-x|x|f|0|1|||0|0|{}|a|b",
            escape_field(command)
        );
        let p = parse_pane_line(&line).expect("parses");
        assert_eq!(p.command.as_deref(), Some(command));
        assert_eq!(p.overlay_raw, "a|b");
    }

    /// The overlay is last and taken as the remainder, so a `|` inside it
    /// can never desync the parser.
    #[test]
    fn overlay_may_contain_the_field_separator() {
        let p =
            parse_pane_line("%3|@1|claude|0|wingman-session-x|x|f|0|1|||0|0||a|b").expect("parses");
        assert_eq!(p.overlay_raw, "a|b");
        assert_eq!(
            p.shell,
            Some(ShellId::for_session("wingman-session-x", "f", 0))
        );
    }

    /// Furniture is never a shell, whatever identity has been written onto
    /// it. The exclusion used to rest on furniture carrying no identity at
    /// all, which any path that stamps a pane by where it *sits* rather
    /// than by what it *is* can falsify — and then a sleeping placeholder
    /// claims to be the agent, and a dashboard docks the placeholder.
    #[test]
    fn a_holder_or_slot_is_never_a_shell_however_it_was_stamped() {
        let holder =
            parse_pane_line("%4|@1|sh|0|wingman-session-p|p|feat|0|0||1|").expect("parses");
        assert_eq!(holder.shell, None, "a stamped holder is still furniture");
        assert_eq!(holder.pane_id, "%4", "and is still reported as a pane");
        let slot = parse_pane_line("%5|@0|sh|0|main|p|feat|0|0||1|").expect("parses");
        assert_eq!(slot.shell, None);
        let agent =
            parse_pane_line("%6|@1|claude|0|wingman-session-p|p|feat|0|1|||").expect("parses");
        assert!(agent.shell.is_some(), "an unmarked pane is unaffected");
    }

    #[test]
    fn missing_overlay_field_is_empty_not_an_error() {
        let p = parse_pane_line("%3|@1|claude|0|wingman-session-x|x|f|0|1||").expect("parses");
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
                    crate::tmux::testutil::HOLDER_W,
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

    /// Every agent window is born with a holder beside its agent pane.
    ///
    /// The holder is what makes docking a plain `swap-pane`: with two panes
    /// in the window, moving the agent out never drops the window — and so
    /// never drops the session, which would take rediscovery and undock's
    /// target with it. It sits on the LEFT, the side the dashboard occupies
    /// in its own window, so the agent pane and the slot are the two panes
    /// that face each other across a swap.
    #[test]
    fn every_agent_window_is_born_with_a_holder() {
        with_tmux_server(|_sock, tmux| {
            let id = ShellId::for_session("wingman-session-h", "feat", 0);
            let first = tmux
                .new_session(
                    "wingman-session-h",
                    "lw:feat/0",
                    Path::new("/tmp"),
                    "bash",
                    &id,
                    HOLDER_W,
                )
                .expect("session");
            let second = tmux
                .new_window(
                    "wingman-session-h",
                    "lw:feat/1",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-h", "feat", 1),
                    HOLDER_W,
                )
                .expect("window");

            for (label, w) in [("new_session", &first), ("new_window", &second)] {
                let panes: Vec<_> = tmux
                    .list_furniture()
                    .expect("furniture")
                    .into_iter()
                    .filter(|p| p.window_id == w.window_id)
                    .collect();
                assert_eq!(panes.len(), 2, "{label}: exactly two panes, got {panes:?}");
                let holders: Vec<_> = panes.iter().filter(|p| p.holder).collect();
                assert_eq!(holders.len(), 1, "{label}: exactly one holder");
                assert_ne!(
                    holders[0].pane_id, w.pane_id,
                    "{label}: the agent pane is not the holder"
                );
                // Left of the agent pane, and exactly as wide as the
                // dashboard's own pane will be.
                let geom = tmux
                    .run_raw(&[
                        "list-panes",
                        "-t",
                        &w.window_id,
                        "-F",
                        "#{pane_id} #{pane_left} #{pane_width}",
                    ])
                    .expect("geometry");
                let holder_line = geom
                    .lines()
                    .find(|l| l.starts_with(&holders[0].pane_id))
                    .expect("holder geometry");
                assert_eq!(
                    holder_line.split_whitespace().nth(1),
                    Some("0"),
                    "{label}: the holder is the leftmost pane: {geom}"
                );
                assert_eq!(
                    holder_line.split_whitespace().nth(2),
                    Some(HOLDER_W.to_string().as_str()),
                    "{label}: the holder is the dashboard pane's width: {geom}"
                );
            }

            // And the poll sees shells, not furniture: a holder runs a
            // sleeping `sh`, which is exactly what the classifier reads as
            // an ended agent — so it must never reach it.
            let shells = tmux.list_panes().expect("panes");
            assert_eq!(
                shells.len(),
                2,
                "one shell per window, no holders: {shells:?}"
            );
            assert!(
                shells.iter().all(|p| p.shell.is_some()),
                "every polled pane carries an identity: {shells:?}"
            );
            assert!(
                shells
                    .iter()
                    .all(|p| p.pane_id == first.pane_id || p.pane_id == second.pane_id),
                "only the agent panes are shells: {shells:?}"
            );
        });
    }

    /// A server started by a build that predates holders keeps its windows;
    /// the backfill gives them the holder they were never born with, once,
    /// and leaves the ones that already have one alone.
    #[test]
    fn holders_are_backfilled_into_windows_that_predate_them() {
        with_tmux_server(|_sock, tmux| {
            let w = tmux
                .new_session(
                    "wingman-session-b",
                    "lw:feat/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-b", "feat", 0),
                    HOLDER_W,
                )
                .expect("session");
            // Simulate the old world: the user (or an older build) leaves
            // the window with nothing but its agent pane.
            let holder = tmux
                .list_furniture()
                .expect("furniture")
                .into_iter()
                .find(|p| p.holder)
                .expect("a holder to remove");
            tmux.kill_pane(&holder.pane_id).expect("kill the holder");
            assert_eq!(
                tmux.list_furniture()
                    .unwrap()
                    .iter()
                    .filter(|p| p.window_id == w.window_id)
                    .count(),
                1
            );

            assert_eq!(
                dock::ensure_holders(&tmux, HOLDER_W),
                1,
                "one window repaired"
            );
            let panes: Vec<_> = tmux
                .list_furniture()
                .unwrap()
                .into_iter()
                .filter(|p| p.window_id == w.window_id)
                .collect();
            assert_eq!(panes.len(), 2, "the window is furnished again: {panes:?}");
            assert_eq!(panes.iter().filter(|p| p.holder).count(), 1);

            // Idempotent: a second pass adds nothing, and the user's own
            // session is never furnished at all.
            assert_eq!(
                dock::ensure_holders(&tmux, HOLDER_W),
                0,
                "nothing left to do"
            );
            assert!(
                tmux.list_furniture()
                    .unwrap()
                    .iter()
                    .all(|p| !(p.session_name == "main" && p.holder)),
                "the user's own session is not ours to furnish"
            );
        });
    }

    /// A scope mark is not evidence of a live dashboard. Nothing unwrites
    /// it: a TUI quitting in a shell pane hands that pane back to the shell,
    /// and the window, the pane and the option all survive. Attaching to
    /// such a window was the bug — it landed the user in a tmux with no
    /// dashboard in it — so the three cases are told apart by what the
    /// window HOLDS, not by what it once announced.
    #[test]
    fn a_scope_window_is_judged_by_what_is_running_in_it() {
        with_tmux_server(|_sock, tmux| {
            // The user's own shell pane, still carrying the mark of a
            // dashboard that has already quit.
            let stale = tmux
                .run(&[
                    "new-window",
                    "-P",
                    "-F",
                    "#{window_id}",
                    "-d",
                    "--",
                    "sh",
                    "-c",
                    "sleep 600",
                ])
                .unwrap()
                .trim()
                .to_string();
            tmux.set_window_option(&stale, dock::SCOPE_KEY, "/work/scope")
                .unwrap();
            assert_eq!(
                tmux.scope_window_state(&stale, "lazywingman").unwrap(),
                ScopeWindow::Stale,
                "a shell that once hosted a dashboard is not a dashboard"
            );

            // The same window once a pane of ours is in it: the dashboard is
            // gone but its furniture is not, so it is worth splitting into.
            let slot = tmux
                .run(&[
                    "split-window",
                    "-t",
                    &stale,
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-d",
                    "--",
                    "sh",
                    "-c",
                    "sleep 600",
                ])
                .unwrap()
                .trim()
                .to_string();
            tmux.set_pane_option(&slot, dock::SLOT_KEY, "1").unwrap();
            assert_eq!(
                tmux.scope_window_state(&stale, "lazywingman").unwrap(),
                ScopeWindow::Furniture,
                "a slot left behind is worth splitting a dashboard back into"
            );

            // And a window actually running the binary reads as live —
            // `sleep` stands in for it, since the test cannot run itself.
            let live = tmux
                .run(&[
                    "new-window",
                    "-P",
                    "-F",
                    "#{window_id}",
                    "-d",
                    "--",
                    "sleep",
                    "600",
                ])
                .unwrap()
                .trim()
                .to_string();
            assert_eq!(
                tmux.scope_window_state(&live, "sleep").unwrap(),
                ScopeWindow::Live,
                "the foreground command is the only live proof there is"
            );

            // Retracting the mark is what keeps the next launch from
            // rediscovering the same lie.
            tmux.clear_window_option(&stale, dock::SCOPE_KEY).unwrap();
            assert!(
                tmux.find_scope_window(Path::new("/work/scope"))
                    .unwrap()
                    .is_none(),
                "a retracted mark must stop matching"
            );
        });
    }

    /// `new-session -A` against an EXISTING session attaches and silently
    /// drops the command — a tmux with no dashboard in it, which is the
    /// second door to the same bug. An existing wrapper session therefore
    /// gets a window of its own.
    #[test]
    fn an_existing_wrapper_session_gets_a_window_not_a_bare_attach() {
        with_tmux_server(|_sock, tmux| {
            let win = tmux
                .spawn_dashboard_window(
                    "main",
                    Path::new("/tmp"),
                    Path::new("/bin/sleep"),
                    &["600".to_string()],
                )
                .unwrap();
            assert!(win.starts_with('@'));
            assert_eq!(
                tmux.scope_window_state(&win, "sleep").unwrap(),
                ScopeWindow::Live,
                "the window must be running what it was asked to run"
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
                    crate::tmux::testutil::HOLDER_W,
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
                    crate::tmux::testutil::HOLDER_W,
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
                crate::tmux::testutil::HOLDER_W,
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
                crate::tmux::testutil::HOLDER_W,
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
                    crate::tmux::testutil::HOLDER_W,
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
                    crate::tmux::testutil::HOLDER_W,
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
            tmux.select_window(&w1.window_id).unwrap();
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
                    crate::tmux::testutil::HOLDER_W,
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
                    crate::tmux::testutil::HOLDER_W,
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
                    crate::tmux::testutil::HOLDER_W,
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
                    crate::tmux::testutil::HOLDER_W,
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
                    crate::tmux::testutil::HOLDER_W,
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
    // ---- M18 Task 11 — the swap ----

    /// Count the windows of every session, so a dock can be shown to
    /// create and destroy none: that is the whole reason docking swaps
    /// against a holder instead of moving panes between windows.
    fn window_counts(tmux: &Tmux) -> Vec<(String, usize)> {
        let out = tmux
            .run_raw(&["list-windows", "-a", "-F", "#{session_name}"])
            .expect("list-windows");
        let mut counts: Vec<(String, usize)> = Vec::new();
        for name in out.lines() {
            match counts.iter_mut().find(|(n, _)| n == name) {
                Some(e) => e.1 += 1,
                None => counts.push((name.to_string(), 1)),
            }
        }
        counts.sort();
        counts
    }

    /// Docking is one cross-session `swap-pane`, and undocking is the same
    /// swap run backwards. Both panes move, identity travels with them, the
    /// lease is written on the way in and released on the way out — and no
    /// window is created or destroyed by either.
    #[test]
    fn a_dock_swaps_both_panes_and_an_undock_swaps_them_back() {
        with_tmux_server(|_sock, tmux| {
            let id = ShellId::for_session("wingman-session-d", "feat", 0);
            let agent = tmux
                .new_session(
                    "wingman-session-d",
                    "lw:feat/0",
                    Path::new("/tmp"),
                    "bash",
                    &id,
                    HOLDER_W,
                )
                .expect("agent session");
            let slot = tmux.split_slot("%0", 40).expect("slot");
            let before = window_counts(&tmux);

            dock::dock(&tmux, &slot, &agent.pane_id, "%0").expect("dock");

            let panes = tmux.list_furniture().expect("furniture");
            let at = |pane: &str| {
                panes
                    .iter()
                    .find(|p| p.pane_id == pane)
                    .map(|p| (p.session_name.clone(), p.window_id.clone()))
                    .expect("pane still exists")
            };
            assert_eq!(
                at(&agent.pane_id),
                ("main".to_string(), "@0".to_string()),
                "the agent pane is in the dashboard's window"
            );
            assert_eq!(
                at(&slot),
                ("wingman-session-d".to_string(), agent.window_id.clone()),
                "and the slot has gone to live where it came from"
            );
            assert_eq!(
                window_counts(&tmux),
                before,
                "a swap creates and destroys no window"
            );

            let docked = tmux
                .list_panes()
                .expect("panes")
                .into_iter()
                .find(|p| p.pane_id == agent.pane_id)
                .expect("a docked pane is still one of ours");
            assert_eq!(docked.shell, Some(id.clone()), "identity travels with it");
            assert_eq!(
                docked.docked_by.as_deref(),
                Some("%0"),
                "the lease names the dashboard holding it"
            );
            assert!(
                tmux.capture_pane(&agent.pane_id, 20).is_ok(),
                "a docked pane still answers capture-pane"
            );
            assert!(
                tmux.list_furniture()
                    .unwrap()
                    .iter()
                    .any(|p| p.pane_id == slot && p.slot),
                "the slot keeps its own mark too"
            );

            dock::undock(&tmux, &agent.pane_id, &agent.window_id).expect("undock");

            assert_eq!(
                at_now(&tmux, &agent.pane_id),
                ("wingman-session-d".to_string(), agent.window_id.clone()),
                "the agent pane is home"
            );
            assert_eq!(
                at_now(&tmux, &slot),
                ("main".to_string(), "@0".to_string()),
                "and the slot is back beside the tree"
            );
            assert_eq!(window_counts(&tmux), before, "still no window created");
            let home = tmux
                .list_panes()
                .expect("panes")
                .into_iter()
                .find(|p| p.pane_id == agent.pane_id)
                .expect("still ours");
            assert_eq!(home.shell, Some(id));
            assert_eq!(home.docked_by, None, "the lease is released");
        });
    }

    /// Where a pane sits right now, as (session, window).
    fn at_now(tmux: &Tmux, pane: &str) -> (String, String) {
        tmux.list_furniture()
            .expect("furniture")
            .into_iter()
            .find(|p| p.pane_id == pane)
            .map(|p| (p.session_name, p.window_id))
            .expect("pane still exists")
    }

    /// With the slot killed under it — a dashboard that died with a pane
    /// docked — the pane still has somewhere to go: the holder that never
    /// left home. This is what keeps the crash path recoverable.
    #[test]
    fn undock_falls_back_to_the_holder_when_the_slot_is_gone() {
        with_tmux_server(|_sock, tmux| {
            let agent = tmux
                .new_session(
                    "wingman-session-f",
                    "lw:feat/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-f", "feat", 0),
                    HOLDER_W,
                )
                .expect("agent session");
            let slot = tmux.split_slot("%0", 40).expect("slot");
            dock::dock(&tmux, &slot, &agent.pane_id, "%0").expect("dock");
            tmux.kill_pane(&slot).expect("the dashboard's window died");

            dock::undock(&tmux, &agent.pane_id, &agent.window_id).expect("undock");

            assert_eq!(
                at_now(&tmux, &agent.pane_id).1,
                agent.window_id,
                "the agent pane is home, having changed places with its holder"
            );
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

    /// Holder width the fixtures spawn agent windows with. The test server
    /// is 200x50, so this is what `dock::dash_width` would hand a real one
    /// — spelled once here rather than at every spawn site.
    pub const HOLDER_W: u16 = crate::dock::DASH_MAX_WIDTH;

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

    /// Raw tmux, for fixtures a `Tmux` method would have no business
    /// offering — standing up a *second* dashboard's window, say. Returns
    /// trimmed stdout and panics on failure: a broken fixture is a broken
    /// test, not a case to handle.
    pub fn tmux_raw(sock: &str, args: &[&str]) -> String {
        let out = Command::new("tmux")
            .arg("-L")
            .arg(sock)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("tmux {args:?}: {e}"));
        assert!(
            out.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A handle on a socket **no other test shares**: a tmux for the tests
    /// that never mean to talk to one.
    ///
    /// It used to be the literal socket `"unused"`, on the assumption that
    /// nothing would ever run a command on it. That assumption is not the
    /// test's to make — an action under test may legitimately reach tmux
    /// (pressing `Enter` on a repository row starts its inline shell), and
    /// a session left on a shared socket is then found by the next test
    /// that captures a pane there. A socket of one's own turns that from a
    /// cross-test failure into nothing at all.
    pub fn dead_socket() -> String {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        format!(
            "lwdead-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// [`tmux_inside`] on a socket of its own — see [`dead_socket`].
    pub fn tmux_nowhere() -> Tmux {
        tmux_inside(&dead_socket())
    }

    /// Kill whatever server a handle's socket ended up with — for a test
    /// that deliberately reaches tmux on a [`dead_socket`] and should not
    /// leave a server (and its shells) running past the run.
    pub fn kill_socket(tmux: &Tmux) {
        if let Some(sock) = &tmux.socket {
            let _ = Command::new("tmux")
                .arg("-L")
                .arg(sock)
                .arg("kill-server")
                .output();
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
}
