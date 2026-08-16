//! LazyWingman — a control centre for supervising coding agents.
//!
//! Agent shells live in dedicated tmux sessions
//! (`wingman-session-<project>`) that LazyWingman spawns, polls, and tears
//! down — never in the user's own session. The dashboard itself runs either
//! inside tmux (as a pane of the user's session) or standalone in a plain
//! terminal; both are supported.

mod app;
mod config;
mod discovery;
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
    log::info!("theme '{}' resolved", theme.name);
    let tmux = tmux::Tmux::detect()?;
    match &tmux.inside {
        Some(i) => log::info!(
            "dashboard inside tmux: session='{}' window={} pane={}",
            i.session_name,
            i.window_id,
            i.pane_id
        ),
        None => log::info!("dashboard standalone (no tmux around it)"),
    }
    app::run(cfg, theme, tmux)
}
