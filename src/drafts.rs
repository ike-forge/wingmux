//! Parked quick-send drafts, persisted across runs.
//!
//! Stored as JSON in `$XDG_STATE_HOME/lazywingman/drafts.json` — beside
//! `state.json`, not inside it. A half-written prompt is state (it cannot
//! be regenerated, so it is not a cache), but it is not part of the tree:
//! keeping it in its own file means a corrupt or stale drafts file can be
//! dropped without taking the repositories with it, which is exactly what
//! [`Drafts::load`] does.
//!
//! In memory drafts are keyed by tmux pane id; on disk they cannot be. Pane
//! ids do not survive a restart — the panes are recreated with new ids and
//! `State::reconcile` rediscovers shells by window name — so the on-disk key
//! is the stable triple the window name encodes: repository, worktree
//! branch, shell index. Resolving that triple back to a pane id is
//! `App::load_drafts`, and an entry whose shell is gone is simply not
//! resolved: a draft lives until its shell does, and no longer.

use serde::{Deserialize, Serialize};

use crate::error::Result;

pub const DRAFTS_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Drafts {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<Draft>,
}

impl Default for Drafts {
    fn default() -> Drafts {
        Drafts {
            version: DRAFTS_VERSION,
            entries: Vec::new(),
        }
    }
}

/// One parked draft, keyed by the triple that survives a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub repository: String,
    pub branch: String,
    pub shell: usize,
    pub text: String,
    /// Cursor, so resuming lands where the typing stopped. Byte offset into
    /// the row, like `TextBuf`; both are clamped on load, so a hand-edited
    /// or truncated file cannot place the caret mid-character.
    #[serde(default)]
    pub cursor_row: usize,
    #[serde(default)]
    pub cursor_col: usize,
}

impl Drafts {
    /// Load from disk. Anything wrong with the file — missing, malformed,
    /// a version this build does not know — yields no drafts and a log
    /// line. Unlike `State::load` this never errors: losing a parked
    /// prompt is a small loss, and refusing to start over it is a big one.
    pub fn load() -> Drafts {
        let path = crate::util::drafts_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Drafts::default(),
            Err(e) => {
                log::warn!(
                    "cannot read {}: {e} — starting with no drafts",
                    path.display()
                );
                return Drafts::default();
            }
        };
        match serde_json::from_str::<Drafts>(&text) {
            Ok(d) if d.version == DRAFTS_VERSION => d,
            Ok(d) => {
                log::warn!(
                    "drafts v{} is not v{DRAFTS_VERSION} — ignoring {}",
                    d.version,
                    path.display()
                );
                Drafts::default()
            }
            Err(e) => {
                log::warn!(
                    "cannot parse {}: {e} — starting with no drafts",
                    path.display()
                );
                Drafts::default()
            }
        }
    }

    /// Write to disk, removing the file when there is nothing to keep — an
    /// empty `drafts.json` left behind would outlive the last draft.
    pub fn save(&self) -> Result<()> {
        let path = crate::util::drafts_path();
        if self.entries.is_empty() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| crate::error::Error::State(e.to_string()))?;
        std::fs::write(&path, text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testutil::with_fresh_xdg;

    fn draft(text: &str) -> Draft {
        Draft {
            repository: "p".into(),
            branch: "wt".into(),
            shell: 0,
            text: text.into(),
            cursor_row: 0,
            cursor_col: 1,
        }
    }

    #[test]
    fn save_load_roundtrip() {
        with_fresh_xdg(|| {
            let d = Drafts {
                version: DRAFTS_VERSION,
                entries: vec![draft("hi")],
            };
            d.save().unwrap();
            let back = Drafts::load();
            assert_eq!(back.entries.len(), 1);
            assert_eq!(back.entries[0].text, "hi");
            assert_eq!(back.entries[0].branch, "wt");
        });
    }

    /// Saving nothing removes the file: an empty `drafts.json` left behind
    /// would outlive the last draft it held.
    #[test]
    fn saving_no_drafts_removes_the_file() {
        with_fresh_xdg(|| {
            Drafts {
                version: DRAFTS_VERSION,
                entries: vec![draft("hi")],
            }
            .save()
            .unwrap();
            assert!(crate::util::drafts_path().exists());
            Drafts::default().save().unwrap();
            assert!(!crate::util::drafts_path().exists());
            // and removing an absent file is not an error
            Drafts::default().save().unwrap();
        });
    }

    /// A missing file is the normal first run, and a broken one must not
    /// stop the dashboard: both mean "no drafts", never an error.
    #[test]
    fn a_missing_or_broken_file_means_no_drafts() {
        with_fresh_xdg(|| {
            assert!(Drafts::load().entries.is_empty(), "missing file");
            let path = crate::util::drafts_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{ not json").unwrap();
            assert!(Drafts::load().entries.is_empty(), "malformed file");
            std::fs::write(&path, r#"{"version":99,"entries":[]}"#).unwrap();
            assert!(Drafts::load().entries.is_empty(), "unknown version");
        });
    }
}
