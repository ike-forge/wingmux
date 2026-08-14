//! LazyWingman — a tmux-native control centre for supervising coding agents.
//!
//! Runs as a pane inside an existing tmux session. Agent shells live in
//! deterministic tmux windows (`lw:<project>/<feature>/<n>`) that LazyWingman
//! spawns, polls, and tears down.

mod app;
mod config;
mod error;
mod git;
mod logging;
mod state;
mod status;
mod theme;
mod tmux;
mod ui;
mod util;

use error::Result;

fn main() {
    if let Err(e) = real_main() {
        // Before the TUI takes over, stderr is still safe.
        eprintln!("lazywingman: {e}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    if let Err(e) = logging::init() {
        eprintln!("lazywingman: warning: cannot open log file: {e}");
    }
    log::info!("lazywingman starting (pid {})", std::process::id());
    let cfg = config::Config::load()?;
    let truecolor = theme::supports_truecolor();
    let theme = theme::load(&cfg.theme, truecolor)?;
    log::info!(
        "config loaded: theme={} poll={}ms projects={} truecolor={}",
        cfg.theme,
        cfg.poll_interval_ms,
        cfg.projects.len(),
        truecolor
    );
    let tmux = match tmux::Tmux::detect()? {
        Some(t) => {
            log::info!(
                "inside tmux: session='{}' window={} pane={}",
                t.session_name,
                t.window_id,
                t.pane_id
            );
            Some(t)
        }
        None => {
            if std::env::var_os("LAZYWINGMAN_NO_TMUX").is_some() {
                log::warn!("not inside tmux — running in no-tmux mode (tmux actions disabled)");
                None
            } else {
                return Err(error::Error::Tmux(
                    "LazyWingman must run inside tmux (TMUX env var not set). \
                     Start it from a tmux pane, or set LAZYWINGMAN_NO_TMUX=1 to \
                     run without tmux (tmux actions will fail)."
                        .into(),
                ));
            }
        }
    };
    app::run(cfg, theme, tmux)
}
