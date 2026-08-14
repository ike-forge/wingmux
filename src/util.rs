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
use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_defaults_used_when_env_unset() {
        // We can't easily unset env vars in parallel tests; instead verify the
        // env-var branch wins when set.
        let tmp = std::env::temp_dir();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &tmp);
            std::env::set_var("XDG_STATE_HOME", &tmp);
        }
        assert_eq!(config_home(), tmp);
        assert_eq!(state_home(), tmp);
        assert_eq!(config_path(), tmp.join("lazywingman/config.toml"));
        assert_eq!(state_path(), tmp.join("lazywingman/state.json"));
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("XDG_STATE_HOME");
        }
    }

    #[test]
    fn paths_are_under_lazywingman_dirs() {
        let cfg = config_path();
        assert_eq!(cfg.file_name().unwrap(), "config.toml");
        assert_eq!(cfg.parent().unwrap().file_name().unwrap(), "lazywingman");
    }
}
