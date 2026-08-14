//! Application entry point and top-level event loop (filled in at M5).

use crate::error::Result;

/// Run the LazyWingman TUI. Owns the terminal and returns only on exit.
pub fn run() -> Result<()> {
    log::info!("app::run entered (TUI not yet implemented)");
    Ok(())
}
