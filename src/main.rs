//! LazyWingman — a tmux-native control centre for supervising coding agents.
//!
//! Runs as a pane inside an existing tmux session. Agent shells live in
//! deterministic tmux windows (`lw:<project>/<feature>/<n>`) that LazyWingman
//! spawns, polls, and tears down.

mod app;
mod error;
mod logging;
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
    let res = app::run();
    match &res {
        Ok(()) => log::info!("lazywingman exited cleanly"),
        Err(e) => log::error!("lazywingman exited with error: {e}"),
    }
    res
}
