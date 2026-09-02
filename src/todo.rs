//! The todo buffer: one scratch pad for the next steps that cannot be
//! started yet.
//!
//! Stored as **plain text** in `$XDG_STATE_HOME/lazywingman/todo.md` — one
//! file for the whole machine, deliberately not one per scope. A note like
//! "rebase the API worktree once the schema lands" is written while looking
//! at one repository and needed while looking at another; a per-directory
//! pad would hide it exactly then. It is the same reasoning behind
//! [`crate::drafts`] living beside `state.json` rather than inside it, taken
//! one step further: this file is not keyed to anything in the tree at all.
//!
//! Plain text, not JSON, because the point of a global scratch pad is that
//! it is also reachable without the dashboard: `$EDITOR ~/.local/state/…`
//! and the dashboard picks the change up the next time the panel opens.
//! Nothing is stored beside the text (no cursor, no version) — a file with
//! no schema cannot have the wrong one, and the cursor is a within-run
//! convenience that [`crate::app::App`] keeps in memory.

use crate::error::{Error, Result};

/// Read the parked todo text; the empty string when there is none.
///
/// Like [`crate::drafts::Drafts::load`] this never errors: losing the pad is
/// a small loss, and refusing to start over it is a big one. Anything
/// unreadable is logged and treated as empty — the next save rewrites it.
pub fn load() -> String {
    let path = crate::util::todo_path();
    match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            log::warn!(
                "cannot read {}: {e} — starting with an empty todo",
                path.display()
            );
            String::new()
        }
    }
}

/// Write the todo text out, removing the file once nothing is left in it —
/// an empty `todo.md` would outlive the last note it held.
///
/// Written the way [`crate::state::State::save`] writes state: a temporary
/// file beside the target, then a rename over it. Two dashboards share this
/// file, so last-writer-wins is the rule, but a *half-written* pad is not
/// something the user can get back.
pub fn save(text: &str) -> Result<()> {
    let path = crate::util::todo_path();
    if text.trim().is_empty() {
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
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Io(e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::util::testutil::with_fresh_xdg;

    #[test]
    fn save_load_roundtrip() {
        with_fresh_xdg(|| {
            super::save("- rebase wt2\n- bump the tmux floor").unwrap();
            assert_eq!(super::load(), "- rebase wt2\n- bump the tmux floor");
        });
    }

    /// A missing file is the normal first run: it means an empty pad, never
    /// an error.
    #[test]
    fn a_missing_file_means_an_empty_todo() {
        with_fresh_xdg(|| {
            assert_eq!(super::load(), "");
        });
    }

    /// Saving nothing removes the file — and removing an absent one is not
    /// an error, so the empty pad can be saved on every close.
    #[test]
    fn saving_nothing_removes_the_file() {
        with_fresh_xdg(|| {
            super::save("note").unwrap();
            assert!(crate::util::todo_path().exists());
            super::save("  \n\n").unwrap();
            assert!(!crate::util::todo_path().exists());
            super::save("").unwrap();
        });
    }

    /// The save leaves no temporary behind: the pad is written beside the
    /// target and renamed over it, the way state is.
    #[test]
    fn save_leaves_no_temporary_behind() {
        with_fresh_xdg(|| {
            super::save("note").unwrap();
            let strays: Vec<String> = std::fs::read_dir(crate::util::state_dir())
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains("tmp"))
                .collect();
            assert!(strays.is_empty(), "left behind: {strays:?}");
        });
    }
}
