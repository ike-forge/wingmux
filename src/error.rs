//! Central error type for LazyWingman.
//!
//! Everything that can fail converges on [`Error`], so the TUI layer can
//! surface a single error surface (usually via a popup or the log file).

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config error: {0}")]
    Config(String),

    #[error("theme error: {0}")]
    Theme(String),

    #[error("git error: {0}")]
    Git(String),

    #[error("tmux error: {0}")]
    Tmux(String),

    #[error("state error: {0}")]
    State(String),

    #[error("regex error: {0}")]
    Regex(#[from] regex::Error),
}
