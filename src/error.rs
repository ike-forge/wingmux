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

    #[error("{context}: {source}")]
    Wrapped {
        context: String,
        #[source]
        source: Box<Error>,
    },
}

impl Error {
    /// Add context to an existing error (boxed to keep the enum small).
    pub fn context(self, context: impl Into<String>) -> Error {
        Error::Wrapped {
            context: context.into(),
            source: Box::new(self),
        }
    }
}

/// Convenience trait to attach context to any [`Result`].
pub trait ResultContext<T> {
    fn ctx(self, context: impl Into<String>) -> Result<T>;
}

impl<T, E> ResultContext<T> for std::result::Result<T, E>
where
    Error: From<E>,
{
    fn ctx(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|e| Error::from(e).context(context))
    }
}

/// Helper for building stringly-typed errors (git/tmux output messages).
pub fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Git(message.into()))
}

/// Used by tests to assert on error kinds.
pub fn tmux_err<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Tmux(message.into()))
}
