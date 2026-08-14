//! XDG base directory resolution.
//!
//! LazyWingman stores:
//!   - config:      `$XDG_CONFIG_HOME/lazywingman/config.toml`  (default `~/.config`)
//!   - themes:      `$XDG_CONFIG_HOME/lazywingman/themes/*.toml`
//!   - state:       `$XDG_STATE_HOME/lazywingman/state.json`    (default `~/.local/state`)
//!   - logs:        `$XDG_STATE_HOME/lazywingman/lazywingman.log`
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

/// `$XDG_DATA_HOME` or `~/.local/share`.
pub fn data_home() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
}

/// LazyWingman config directory: `$XDG_CONFIG_HOME/lazywingman`.
pub fn config_dir() -> PathBuf {
    config_home().join("lazywingman")
}

/// Config file path: `$XDG_CONFIG_HOME/lazywingman/config.toml`.
pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Themes directory: `$XDG_CONFIG_HOME/lazywingman/themes`.
pub fn themes_dir() -> PathBuf {
    config_dir().join("themes")
}

/// State directory: `$XDG_STATE_HOME/lazywingman`.
pub fn state_dir() -> PathBuf {
    state_home().join("lazywingman")
}

/// State file path: `$XDG_STATE_HOME/lazywingman/state.json`.
pub fn state_path() -> PathBuf {
    state_dir().join("state.json")
}

/// Log file path: `$XDG_STATE_HOME/lazywingman/lazywingman.log`.
pub fn log_path() -> PathBuf {
    state_dir().join("lazywingman.log")
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
    //! Shared helper so env-mutating tests don't race each other. All tests
    //! that touch XDG env vars must go through [`with_fresh_xdg`].
    use std::sync::Mutex;

    static XDG_LOCK: Mutex<()> = Mutex::new(());

    /// Run `f` with XDG dirs pointed at fresh temp directories.
    pub fn with_fresh_xdg<R>(f: impl FnOnce() -> R) -> R {
        let _guard = XDG_LOCK.lock().unwrap();
        let base = std::env::temp_dir().join(format!("lwtest-{}", std::process::id()));
        let cfg = base.join("config");
        let state = base.join("state");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &cfg);
            std::env::set_var("XDG_STATE_HOME", &state);
        }
        let r = f();
        let _ = std::fs::remove_dir_all(&base);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_defaults_used_when_env_unset() {
        testutil::with_fresh_xdg(|| {
            let tmp = std::env::temp_dir().join(format!("lwtest-{}", std::process::id()));
            assert_eq!(config_home(), tmp.join("config"));
            assert_eq!(state_home(), tmp.join("state"));
            assert_eq!(config_path(), tmp.join("config/lazywingman/config.toml"));
            assert_eq!(state_path(), tmp.join("state/lazywingman/state.json"));
        });
    }

    #[test]
    fn paths_are_under_lazywingman_dirs() {
        let cfg = config_path();
        assert_eq!(cfg.file_name().unwrap(), "config.toml");
        assert_eq!(cfg.parent().unwrap().file_name().unwrap(), "lazywingman");
    }

    #[test]
    fn normalize_lexically_resolves_dotdots() {
        assert_eq!(
            normalize_path(Path::new("/work/projects/../wt")),
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
