//! Todo pads: scratch pads for the next steps that cannot be started yet.
//!
//! Two kinds, both **plain text** under `$XDG_STATE_HOME/wingmux/`:
//!
//! - the global pad (`T`), `todo.md` — notes written while looking at one
//!   repository and needed while looking at another;
//! - one pad per repository (`t`), `todos/<escaped repository path>.md` —
//!   shared by the repository's worktrees, so it outlives any of them.
//!
//! Kept in the state dir rather than the repository so nothing shows up in
//! `git status`. Plain text so `$EDITOR` can reach it without the dashboard;
//! the next open re-reads it. No cursor or version is stored: a file with no
//! schema cannot have the wrong one.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Which pad a buffer belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pad {
    Global,
    /// Keyed by the repository's path: names are not unique, paths are.
    Repo(PathBuf),
}

/// File name of a repository's pad. `%` and `/` are escaped so the name
/// round-trips and stays readable in `ls`.
pub fn repo_file_name(repo: &Path) -> String {
    let escaped = repo
        .to_string_lossy()
        .replace('%', "%25")
        .replace('/', "%2F");
    format!("{escaped}.md")
}

/// Read a parked pad; the empty string when there is none.
///
/// Never errors: anything unreadable is logged and treated as empty, and
/// the next save rewrites it.
pub fn load(pad: &Pad) -> String {
    let path = crate::util::todo_path(pad);
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

/// Write a pad out, removing the file once nothing is left in it.
///
/// Temp file then rename, as [`crate::state::State::save`] does: two
/// dashboards share these files, so last-writer-wins, but never half-written.
pub fn save(pad: &Pad, text: &str) -> Result<()> {
    let path = crate::util::todo_path(pad);
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
    use super::Pad;
    use crate::util::testutil::with_fresh_xdg;
    use std::path::PathBuf;

    #[test]
    fn save_load_roundtrip() {
        with_fresh_xdg(|| {
            super::save(&Pad::Global, "- rebase wt2\n- bump the tmux floor").unwrap();
            assert_eq!(
                super::load(&Pad::Global),
                "- rebase wt2\n- bump the tmux floor"
            );
        });
    }

    #[test]
    fn a_missing_file_means_an_empty_todo() {
        with_fresh_xdg(|| {
            assert_eq!(super::load(&Pad::Global), "");
            assert_eq!(super::load(&Pad::Repo("/nowhere".into())), "");
        });
    }

    /// The empty pad is saved on every close, so removing an absent file is
    /// not an error.
    #[test]
    fn saving_nothing_removes_the_file() {
        with_fresh_xdg(|| {
            let pad = Pad::Repo("/src/api".into());
            super::save(&pad, "note").unwrap();
            assert!(crate::util::todo_path(&pad).exists());
            super::save(&pad, "  \n\n").unwrap();
            assert!(!crate::util::todo_path(&pad).exists());
            super::save(&pad, "").unwrap();
        });
    }

    #[test]
    fn each_repository_has_its_own_pad_apart_from_the_global_one() {
        with_fresh_xdg(|| {
            let a = Pad::Repo("/src/a".into());
            let b = Pad::Repo("/src/b".into());
            super::save(&Pad::Global, "global").unwrap();
            super::save(&a, "for a").unwrap();
            assert_eq!(super::load(&Pad::Global), "global");
            assert_eq!(super::load(&a), "for a");
            assert_eq!(super::load(&b), "");
        });
    }

    /// Escaping must keep distinct paths distinct: `/a%2Fb` is not `/a/b`.
    #[test]
    fn repository_file_names_do_not_collide() {
        let plain = super::repo_file_name(&PathBuf::from("/a/b"));
        let percent = super::repo_file_name(&PathBuf::from("/a%2Fb"));
        assert_eq!(plain, "%2Fa%2Fb.md");
        assert_ne!(plain, percent);
    }

    #[test]
    fn save_leaves_no_temporary_behind() {
        with_fresh_xdg(|| {
            let pad = Pad::Repo("/src/api".into());
            super::save(&Pad::Global, "note").unwrap();
            super::save(&pad, "note").unwrap();
            let dirs = [
                crate::util::state_dir(),
                crate::util::todo_path(&pad).parent().unwrap().to_path_buf(),
            ];
            let strays: Vec<String> = dirs
                .iter()
                .flat_map(|d| std::fs::read_dir(d).unwrap())
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains("tmp"))
                .collect();
            assert!(strays.is_empty(), "left behind: {strays:?}");
        });
    }
}
