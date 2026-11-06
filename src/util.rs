//! XDG base directory resolution.
//!
//! Wingmux stores:
//!   - config:      `$XDG_CONFIG_HOME/wingmux/config.toml`  (default `~/.config`)
//!   - themes:      `$XDG_CONFIG_HOME/wingmux/themes/*.toml`
//!   - state:       `$XDG_STATE_HOME/wingmux/state.json`    (default `~/.local/state`)
//!   - logs:        `$XDG_STATE_HOME/wingmux/wingmux.log`
//!   - cache:       `$XDG_CACHE_HOME/wingmux/discovered.json` (default `~/.cache`)
//!
//! Falls back to the XDG defaults when the environment variables are unset,
//! per the XDG Base Directory specification.

use std::env;
use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `$XDG_CONFIG_HOME` or `~/.config`.
pub fn config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

/// `$XDG_STATE_HOME` or `~/.local/state`.
pub fn state_home() -> PathBuf {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
}

/// Wingmux config directory: `$XDG_CONFIG_HOME/wingmux`.
pub fn config_dir() -> PathBuf {
    config_home().join("wingmux")
}

/// Config file path: `$XDG_CONFIG_HOME/wingmux/config.toml`.
pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Themes directory: `$XDG_CONFIG_HOME/wingmux/themes`.
pub fn themes_dir() -> PathBuf {
    config_dir().join("themes")
}

/// State directory: `$XDG_STATE_HOME/wingmux`.
pub fn state_dir() -> PathBuf {
    state_home().join("wingmux")
}

/// State file path: `$XDG_STATE_HOME/wingmux/state.json`.
pub fn state_path() -> PathBuf {
    state_dir().join("state.json")
}

/// A todo pad: `$XDG_STATE_HOME/wingmux/todo.md` for the global one,
/// `…/todos/<escaped repository path>.md` per repository — see [`crate::todo`].
pub fn todo_path(pad: &crate::todo::Pad) -> PathBuf {
    match pad {
        crate::todo::Pad::Global => state_dir().join("todo.md"),
        crate::todo::Pad::Repo(repo) => state_dir()
            .join("todos")
            .join(crate::todo::repo_file_name(repo)),
    }
}

/// Log file path: `$XDG_STATE_HOME/wingmux/wingmux.log`.
pub fn log_path() -> PathBuf {
    state_dir().join("wingmux.log")
}

/// The user's own login shell — `$SHELL`, falling back to `bash` when the
/// variable is unset or empty (a bare `login` container, `env -i`). This is
/// what a *plain* shell runs: the point of that choice is to land in the
/// shell you actually use, not in the `[defaults].agent_shell` an agent is
/// spawned under.
pub fn login_shell() -> String {
    match env::var("SHELL") {
        Ok(s) if !s.trim().is_empty() => s,
        _ => "bash".to_string(),
    }
}

/// The last component of a shell path (`/usr/bin/zsh` → `zsh`) — what the
/// picker labels the plain-shell entry with.
pub fn shell_name(shell: &str) -> String {
    Path::new(shell)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| shell.to_string())
}

/// The editor to hand a file to: `$VISUAL`, then `$EDITOR`.
///
/// `None` when neither is set. The pad's `ctrl-e` then says so rather than
/// falling back to `vi`: an editor the user has not named is one they may
/// not know how to leave, and it would open over their dashboard.
pub fn editor() -> Option<String> {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|v| env::var(v).ok())
        .find(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_string())
}

/// Single-quote a path for the `sh -c` tmux wraps a one-string command in.
pub fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

/// Lexically resolve `.` and `..` in a path without touching the filesystem.
/// `/a/b/../c` → `/a/c`; relative `../wt` stays relative.
pub fn normalize_path(path: &Path) -> PathBuf {
    use std::ffi::OsStr;
    use std::path::Component;
    let absolute = path.is_absolute();
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.is_empty() {
                    if !absolute {
                        out.push(OsStr::new("..").to_os_string());
                    }
                } else if out.last().map(|c| c == OsStr::new("..")).unwrap_or(false) {
                    out.push(OsStr::new("..").to_os_string());
                } else {
                    out.pop();
                }
            }
            Component::Normal(c) => out.push(c.to_os_string()),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    let mut p = PathBuf::new();
    if absolute {
        p.push(std::path::MAIN_SEPARATOR.to_string());
    }
    for c in out {
        p.push(c);
    }
    p
}

#[cfg(test)]
pub mod testutil {
    //! Shared helpers so env-mutating tests don't race each other. All tests
    //! that touch XDG env vars must go through [`with_fresh_xdg`]; every
    //! other fixture that mutates process-global state serialises on the
    //! same lock via [`with_test_lock`].
    use std::cell::Cell;
    use std::sync::Mutex;

    /// The ONE lock every process-global test fixture takes.
    ///
    /// It used to be two — `XDG_LOCK` here and `TMUX_TEST_LOCK` in
    /// `tmux::testutil` — and the fixtures nest in BOTH orders: some tests
    /// wrap a tmux server around `with_fresh_xdg`, others the reverse. Two
    /// threads could therefore take one lock each and wait for the other,
    /// hanging the whole suite (no timeout) with an orphaned `wmtest-*`
    /// tmux server behind it. A single lock cannot invert; it only has to
    /// survive the nesting, which is what `DEPTH` is for.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        /// Whether this thread already holds [`TEST_LOCK`]. A nested
        /// fixture must NOT lock again — the thread would block on itself.
        static DEPTH: Cell<usize> = const { Cell::new(0) };
    }

    /// Reset on the way out, panic or not: an unwind that left `DEPTH` at 1
    /// would make every later fixture on that thread skip the lock.
    struct DepthReset;

    impl Drop for DepthReset {
        fn drop(&mut self) {
            DEPTH.with(|d| d.set(0));
        }
    }

    /// Run `f` holding the shared test lock, reentrantly.
    ///
    /// Nesting two DIFFERENT fixtures is the point (tmux around XDG and the
    /// reverse). Nesting the same one twice still works, but the inner call
    /// re-runs its own setup — `with_fresh_xdg` inside `with_fresh_xdg`
    /// would wipe the outer test's temp dirs.
    pub fn with_test_lock<R>(f: impl FnOnce() -> R) -> R {
        if DEPTH.with(Cell::get) > 0 {
            return f();
        }
        // Recover from a previous test's panic (poisoned lock).
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        DEPTH.with(|d| d.set(1));
        // Drops before `_guard` (reverse declaration order), so the depth is
        // back to 0 before another thread can take the lock.
        let _reset = DepthReset;
        f()
    }

    /// Run `f` with XDG dirs pointed at fresh temp directories.
    pub fn with_fresh_xdg<R>(f: impl FnOnce() -> R) -> R {
        with_test_lock(|| with_fresh_xdg_locked(f))
    }

    fn with_fresh_xdg_locked<R>(f: impl FnOnce() -> R) -> R {
        let base = std::env::temp_dir().join(format!("wmtest-{}", std::process::id()));
        let cfg = base.join("config");
        let state = base.join("state");
        let cache = base.join("cache");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &cfg);
            std::env::set_var("XDG_STATE_HOME", &state);
            std::env::set_var("XDG_CACHE_HOME", &cache);
        }
        let r = f();
        let _ = std::fs::remove_dir_all(&base);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `$VISUAL` wins over `$EDITOR`, and a variable set to nothing is not
    /// set — a blank one would spawn a window running the empty command.
    #[test]
    fn the_editor_is_visual_then_editor_then_nothing() {
        testutil::with_test_lock(|| {
            let restore = [
                ("VISUAL", env::var("VISUAL")),
                ("EDITOR", env::var("EDITOR")),
            ];
            unsafe {
                env::set_var("EDITOR", "vim");
                env::set_var("VISUAL", "  ");
            }
            assert_eq!(editor().as_deref(), Some("vim"), "a blank VISUAL is unset");
            unsafe { env::set_var("VISUAL", "hx") };
            assert_eq!(editor().as_deref(), Some("hx"));
            unsafe {
                env::remove_var("VISUAL");
                env::remove_var("EDITOR");
            }
            assert_eq!(editor(), None, "nothing is guessed");
            for (k, v) in restore {
                unsafe {
                    match v {
                        Ok(v) => env::set_var(k, v),
                        Err(_) => env::remove_var(k),
                    }
                }
            }
        });
    }

    /// The pad's path reaches `sh -c` whole, quote or space in it.
    #[test]
    fn a_quoted_path_survives_the_shell() {
        assert_eq!(shell_quote(Path::new("/a b/c.md")), "'/a b/c.md'");
        assert_eq!(shell_quote(Path::new("/a'b")), r"'/a'\''b'");
    }

    /// The fixtures nest in both directions — `with_fresh_xdg` inside a
    /// tmux server (`app::tests::d_confirms_the_delete_popup`) and a tmux
    /// server inside `with_fresh_xdg` (`app::tests::boot_discovery_*`).
    /// They now share ONE lock, so it has to be reentrant on a thread:
    /// a plain mutex would block on itself here and hang the suite.
    #[test]
    fn the_shared_test_lock_is_reentrant() {
        testutil::with_test_lock(|| {
            testutil::with_fresh_xdg(|| {
                testutil::with_test_lock(|| {
                    assert_eq!(config_home().file_name().unwrap(), "config");
                });
            });
        });
    }

    #[test]
    fn xdg_defaults_used_when_env_unset() {
        testutil::with_fresh_xdg(|| {
            let tmp = std::env::temp_dir().join(format!("wmtest-{}", std::process::id()));
            assert_eq!(config_home(), tmp.join("config"));
            assert_eq!(state_home(), tmp.join("state"));
            assert_eq!(config_path(), tmp.join("config/wingmux/config.toml"));
            assert_eq!(state_path(), tmp.join("state/wingmux/state.json"));
        });
    }

    #[test]
    fn paths_are_under_wingmux_dirs() {
        let cfg = config_path();
        assert_eq!(cfg.file_name().unwrap(), "config.toml");
        assert_eq!(cfg.parent().unwrap().file_name().unwrap(), "wingmux");
    }

    #[test]
    fn normalize_lexically_resolves_dotdots() {
        assert_eq!(
            normalize_path(Path::new("/work/repositories/../wt")),
            PathBuf::from("/work/wt")
        );
        assert_eq!(
            normalize_path(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(normalize_path(Path::new("../wt")), PathBuf::from("../wt"));
        assert_eq!(normalize_path(Path::new("/a/../../b")), PathBuf::from("/b"));
        assert_eq!(normalize_path(Path::new(".")), PathBuf::from(""));
    }
}
