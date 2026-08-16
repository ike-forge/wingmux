//! XDG base directory resolution.
//!
//! LazyWingman stores:
//!   - config:      `$XDG_CONFIG_HOME/lazywingman/config.toml`  (default `~/.config`)
//!   - themes:      `$XDG_CONFIG_HOME/lazywingman/themes/*.toml`
//!   - state:       `$XDG_STATE_HOME/lazywingman/state.json`    (default `~/.local/state`)
//!   - logs:        `$XDG_STATE_HOME/lazywingman/lazywingman.log`
//!   - cache:       `$XDG_CACHE_HOME/lazywingman/discovered.json` (default `~/.cache`)
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

/// `$XDG_CACHE_HOME` or `~/.cache`.
pub fn cache_home() -> PathBuf {
    env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
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

/// Cache directory: `$XDG_CACHE_HOME/lazywingman` (regenerable data).
pub fn cache_dir() -> PathBuf {
    cache_home().join("lazywingman")
}

/// Project-discovery cache file: `$XDG_CACHE_HOME/lazywingman/discovered.json`.
pub fn discovery_cache_path() -> PathBuf {
    cache_dir().join("discovered.json")
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

/// ASCII-only rendering: terminals that mangle box-drawing characters get a
/// plain fallback (`+ - |` borders, `v`/`>` tree markers). Triggered by a
/// non-empty `LW_ASCII` env var, or by a C/POSIX locale (exact match — a
/// UTF-8 locale like `C.UTF-8` renders box-drawing fine). First non-empty
/// locale var wins, mirroring libc precedence (LC_ALL > LC_CTYPE > LANG).
pub fn ascii_requested() -> bool {
    if let Ok(v) = std::env::var("LW_ASCII")
        && !v.is_empty()
    {
        return true;
    }
    for var in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.to_ascii_lowercase();
            if v.is_empty() {
                continue;
            }
            return v == "c" || v == "posix";
        }
    }
    false
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

    #[test]
    fn xdg_defaults_used_when_env_unset() {
        testutil::with_fresh_xdg(|| {
            let tmp = std::env::temp_dir().join(format!("lwtest-{}", std::process::id()));
            assert_eq!(config_home(), tmp.join("config"));
            assert_eq!(state_home(), tmp.join("state"));
            assert_eq!(cache_home(), tmp.join("cache"));
            assert_eq!(config_path(), tmp.join("config/lazywingman/config.toml"));
            assert_eq!(state_path(), tmp.join("state/lazywingman/state.json"));
            assert_eq!(
                discovery_cache_path(),
                tmp.join("cache/lazywingman/discovered.json")
            );
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

    /// ASCII fallback trigger: `LW_ASCII=1` forces it, a C/POSIX locale
    /// forces it, `C.UTF-8` does not. Env vars are process-global, so this
    /// runs under the same lock as the XDG tests.
    #[test]
    fn ascii_requested_follows_env() {
        testutil::with_fresh_xdg(|| {
            unsafe {
                std::env::remove_var("LW_ASCII");
                std::env::remove_var("LC_ALL");
                std::env::remove_var("LC_CTYPE");
                std::env::remove_var("LANG");
            }
            assert!(!ascii_requested(), "no signals → unicode mode");

            unsafe { std::env::set_var("LW_ASCII", "1") };
            assert!(ascii_requested(), "LW_ASCII forces ascii");

            unsafe {
                std::env::remove_var("LW_ASCII");
                std::env::set_var("LC_ALL", "C");
            }
            assert!(ascii_requested(), "C locale forces ascii");

            unsafe { std::env::set_var("LC_ALL", "POSIX") };
            assert!(ascii_requested(), "POSIX locale forces ascii");

            unsafe {
                std::env::remove_var("LC_ALL");
                std::env::set_var("LC_CTYPE", "C.UTF-8");
            }
            assert!(
                !ascii_requested(),
                "C.UTF-8 renders box-drawing fine — no ascii"
            );

            unsafe {
                std::env::remove_var("LC_CTYPE");
                std::env::set_var("LANG", "en_US.UTF-8");
            }
            assert!(!ascii_requested(), "an ordinary UTF-8 locale is unicode");

            unsafe { std::env::set_var("LC_ALL", "c") };
            assert!(
                ascii_requested(),
                "LC_ALL wins over LANG and is case-insensitive"
            );

            unsafe {
                std::env::remove_var("LC_ALL");
                std::env::remove_var("LC_CTYPE");
                std::env::remove_var("LANG");
            }
        });
    }
}
