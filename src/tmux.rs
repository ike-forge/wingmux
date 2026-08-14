//! tmux integration via its CLI.
//!
//! LazyWingman runs *inside* a tmux pane of an existing session. Agent shells
//! live in sibling windows named `lw:<project>/<feature>/<n>` in the same
//! session, so attaching is a plain `select-window` and everything is
//! rediscoverable after a restart.
//!
//! All commands go through the `tmux` binary; nothing is shell-interpreted.
//! A `-L <socket>` override is honoured via `$LAZYWINGMAN_TMUX_SOCKET` (used
//! by the integration tests and handy for multi-server setups).

use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// Our own tmux coordinates: the session, window and pane LazyWingman lives in.
#[derive(Debug, Clone)]
pub struct Tmux {
    pub session_name: String,
    pub session_id: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
}

/// One pane, as reported by `list-panes -F`.
#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub pane_id: String,
    pub window_id: String,
    pub window_name: String,
    pub current_command: String,
    pub dead: bool,
    pub width: u32,
    pub height: u32,
}

/// Result of spawning / listing a window.
#[derive(Debug, Clone)]
pub struct WindowRef {
    pub window_id: String,
    pub window_name: String,
    /// Active pane id (set by `new_window` and `list_windows`).
    pub pane_id: String,
}

/// Login shells whose presence as the foreground command means the agent
/// process inside has exited.
const SHELL_COMMANDS: &[&str] = &[
    "bash", "sh", "zsh", "fish", "dash", "ksh", "mksh", "ash", "tcsh", "csh", "nu",
];

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
    /// Returns `Ok(None)` when not in tmux (caller decides how to degrade).
    pub fn detect() -> Result<Option<Tmux>> {
        if std::env::var_os("TMUX").is_none() {
            return Ok(None);
        }
        let socket = socket_override();
        let pane_id = std::env::var("TMUX_PANE").unwrap_or_default();
        if pane_id.is_empty() {
            return Err(Error::Tmux(
                "inside tmux but TMUX_PANE is unset".to_string(),
            ));
        }
        let fmt = "#{session_name}|#{session_id}|#{window_id}|#{window_name}";
        let out = run_tmux(&socket, &["display-message", "-p", "-t", &pane_id, fmt])?;
        let mut parts = out.trim().splitn(4, '|');
        let (session_name, session_id, window_id, window_name) = (
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("").to_string(),
        );
        if session_name.is_empty() {
            return Ok(None);
        }
        Ok(Some(Tmux {
            session_name,
            session_id,
            window_id,
            window_name,
            pane_id,
        }))
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        run_tmux(&socket_override(), args)
    }

    /// Run a raw tmux command and return stdout (used by state.rs for
    /// pane-cwd lookups).
    pub fn run_raw(&self, args: &[&str]) -> Result<String> {
        self.run(args)
    }

    /// Sanitise a window-name component: no colons, spaces or control chars.
    fn sanitize(s: &str) -> String {
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

    /// Deterministic window name for a feature shell.
    pub fn window_name(project: &str, feature: &str, index: usize) -> String {
        let mut name = format!(
            "lw:{}/{}/{}",
            Self::sanitize(project),
            Self::sanitize(feature),
            index
        );
        // tmux window names are capped; keep ours well under.
        name.truncate(200);
        name
    }

    /// All panes of our session (one tmux call per poll). Uses `-a` because
    /// `list-panes -t <session>` only reports the session's *active* window.
    pub fn list_panes(&self) -> Result<Vec<PaneInfo>> {
        let fmt = "#{pane_id}|#{window_id}|#{window_name}|#{pane_current_command}|#{pane_dead}|#{pane_width}|#{pane_height}|#{session_id}";
        let out = self.run(&["list-panes", "-a", "-F", fmt])?;
        let want = format!("|{}", self.session_id);
        Ok(out
            .lines()
            .filter(|line| line.ends_with(&want))
            .filter_map(|line| parse_pane_line(line))
            .collect())
    }

    /// All windows of our session.
    pub fn list_windows(&self) -> Result<Vec<WindowRef>> {
        let fmt = "#{window_id}|#{window_name}|#{pane_id}";
        let out = self.run(&["list-windows", "-t", &self.session_name, "-F", fmt])?;
        Ok(out
            .lines()
            .filter_map(|line| {
                let mut it = line.splitn(3, '|');
                let window_id = it.next()?.to_string();
                let window_name = it.next().unwrap_or("").to_string();
                let pane_id = it.next().unwrap_or("").to_string();
                Some(WindowRef {
                    window_id,
                    window_name,
                    pane_id,
                })
            })
            .collect())
    }

    /// Spawn a window running an interactive `shell` in `cwd` (not selected).
    ///
    /// Retries briefly: a freshly started tmux server can still be finishing
    /// its first window, and `new-window` then races window index 0.
    pub fn new_window(&self, name: &str, cwd: &Path, shell: &str) -> Result<WindowRef> {
        let mut last = None;
        for attempt in 0..3 {
            match self.try_new_window(name, cwd, shell) {
                Ok(w) => return Ok(w),
                Err(e) => {
                    log::warn!("new-window attempt {} failed: {e}", attempt + 1);
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(120 * (attempt + 1)));
                }
            }
        }
        Err(last.unwrap())
    }

    fn try_new_window(&self, name: &str, cwd: &Path, shell: &str) -> Result<WindowRef> {
        let out = self.run(&[
            "new-window",
            "-d",
            "-t",
            &self.session_name,
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
            window_name: String::new(),
            pane_id,
        })
    }

    /// Type text into a pane. `enter` appends a trailing Enter.
    pub fn send_keys(&self, pane_id: &str, text: &str, enter: bool) -> Result<()> {
        self.run(&["send-keys", "-t", pane_id, "-l", text])?;
        if enter {
            self.run(&["send-keys", "-t", pane_id, "Enter"])?;
        }
        Ok(())
    }

    /// Last `lines` lines of a pane's visible content (escape codes stripped).
    pub fn capture_pane(&self, pane_id: &str, lines: u32) -> Result<String> {
        let out = self.run(&[
            "capture-pane",
            "-p",
            "-t",
            pane_id,
            "-S",
            &format!("-{lines}"),
        ])?;
        Ok(out)
    }

    /// Switch the attached client to a window of our session.
    pub fn select_window(&self, window_id: &str) -> Result<()> {
        self.run(&[
            "select-window",
            "-t",
            &format!(
                "{}:@{}",
                self.session_name,
                window_id.trim_start_matches('@')
            ),
        ])?;
        Ok(())
    }

    /// Kill a window by id (used on feature deletion).
    pub fn kill_window(&self, window_id: &str) -> Result<()> {
        self.run(&["kill-window", "-t", window_id])?;
        Ok(())
    }

    /// Register a global key that jumps straight back to the dashboard
    /// window from any agent window. Rebound on every startup.
    pub fn bind_back_key(&self, key: &str) -> Result<()> {
        self.run(&[
            "bind-key",
            "-n",
            key,
            "select-window",
            "-t",
            &format!(
                "{}:@{}",
                self.session_name,
                self.window_id.trim_start_matches('@')
            ),
        ])?;
        Ok(())
    }
}

fn parse_pane_line(line: &str) -> Option<PaneInfo> {
    let mut it = line.splitn(8, '|');
    Some(PaneInfo {
        pane_id: it.next()?.to_string(),
        window_id: it.next()?.to_string(),
        window_name: it.next()?.to_string(),
        current_command: it.next()?.to_string(),
        dead: it.next()? == "1",
        width: it.next()?.parse().unwrap_or(0),
        height: it.next()?.parse().unwrap_or(0),
    })
}

/// Is `cmd` a login shell (i.e. no agent running in the pane)?
pub fn is_shell_command(cmd: &str) -> bool {
    let c = cmd.trim();
    SHELL_COMMANDS.contains(&c) || c.ends_with("-login")
}

/// Is `name` one of our agent window names?
pub fn is_lazywingman_window(name: &str) -> bool {
    name.starts_with("lw:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_names_are_deterministic_and_safe() {
        let a = Tmux::window_name("my project", "Feat One!", 0);
        assert_eq!(a, "lw:my-project/Feat-One/0");
        let b = Tmux::window_name("my project", "Feat One!", 0);
        assert_eq!(a, b);
        assert!(a.starts_with("lw:"));
        assert!(!a.contains(' '));
        assert!(a.len() <= 200);
    }

    #[test]
    fn pane_line_parsing() {
        let line = "%3|@5|lw:p/f/0|claude|0|120|40|$0";
        let p = parse_pane_line(line).unwrap();
        assert_eq!(p.pane_id, "%3");
        assert_eq!(p.window_id, "@5");
        assert_eq!(p.window_name, "lw:p/f/0");
        assert_eq!(p.current_command, "claude");
        assert!(!p.dead);
        assert_eq!(p.width, 120);
        let dead = parse_pane_line("%9|@1|sh|sh|1|80|24|$0").unwrap();
        assert!(dead.dead);
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

    #[test]
    fn window_name_prefix_detection() {
        assert!(is_lazywingman_window("lw:p/f/0"));
        assert!(!is_lazywingman_window("0:bash"));
    }

    // ---- integration tests against a real (throwaway) tmux server ----
    //
    // NOTE: the mutex MUST live at module level — a `static` inside the
    // generic `with_tmux_server` would be duplicated per closure type, so
    // tests would not serialize and would stomp each other's env vars.

    static TMUX_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static SOCK_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    fn with_tmux_server(f: impl FnOnce(&str, Tmux)) {
        // Skip gracefully when tmux isn't installed.
        if Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("skipping: tmux not available");
            return;
        }
        // Recover from a previous test's panic (poisoned lock).
        let _guard = TMUX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Unique socket per invocation: zero cross-test server coupling.
        let n = SOCK_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let sock = format!("lwtest-{}-{n}", std::process::id());
        let status = Command::new("tmux")
            .args([
                "-L",
                &sock,
                "new-session",
                "-d",
                "-s",
                "t",
                "-x",
                "200",
                "-y",
                "50",
            ])
            .status()
            .unwrap();
        assert!(status.success(), "new-session failed on socket {sock}");
        // Let the server finish initialising its first window — `new-window`
        // right after `new-session -d` can race window index 0.
        std::thread::sleep(std::time::Duration::from_millis(150));
        // NOTE: TMUX/TMUX_PANE must stay UNSET here — a fake value makes the
        // tmux client behave as a nested client and breaks window creation.
        unsafe {
            std::env::set_var("LAZYWINGMAN_TMUX_SOCKET", &sock);
            std::env::remove_var("TMUX");
            std::env::remove_var("TMUX_PANE");
        }
        let tmux = Tmux {
            session_name: "t".into(),
            session_id: "$0".into(),
            window_id: "@0".into(),
            window_name: "sh".into(),
            pane_id: "%0".into(),
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

    #[test]
    fn detect_finds_our_coordinates() {
        with_tmux_server(|_sock, _t| {
            // Simulate being inside tmux for the duration of detect() only.
            unsafe {
                std::env::set_var("TMUX", "1,0,");
                std::env::set_var("TMUX_PANE", "%0");
            }
            let detected = Tmux::detect().unwrap().unwrap();
            unsafe {
                std::env::remove_var("TMUX");
                std::env::remove_var("TMUX_PANE");
            }
            assert_eq!(detected.session_name, "t");
            assert_eq!(detected.session_id, "$0");
            assert_eq!(detected.window_id, "@0");
            assert_eq!(detected.pane_id, "%0");
        });
    }

    #[test]
    fn spawn_send_capture_kill_roundtrip() {
        with_tmux_server(|_sock, tmux| {
            let w = tmux
                .new_window("lw:p/f/0", Path::new("/tmp"), "bash")
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

            // window listing sees it
            let windows = tmux.list_windows().unwrap();
            assert!(windows.iter().any(|x| x.window_id == w.window_id));

            // kill it
            tmux.kill_window(&w.window_id).unwrap();
            let panes = tmux.list_panes().unwrap();
            assert!(!panes.iter().any(|p| p.pane_id == w.pane_id));
        });
    }

    #[test]
    fn dead_pane_is_reported() {
        with_tmux_server(|_sock, tmux| {
            // run a command that exits immediately: window dies
            let w = tmux
                .new_window("lw:p/f/1", Path::new("/tmp"), "bash")
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
    fn select_window_switches_active_window() {
        with_tmux_server(|_sock, tmux| {
            let w = tmux
                .new_window("lw:p/f/2", Path::new("/tmp"), "bash")
                .unwrap();
            tmux.select_window(&w.window_id).unwrap();
            let active = tmux
                .run(&["display-message", "-p", "-t", "t", "#{window_id}"])
                .unwrap();
            assert_eq!(active.trim(), w.window_id);
        });
    }
}
