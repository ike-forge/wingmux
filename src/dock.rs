//! The docking layer: what identifies a pane, and how a pane moves.
//!
//! Shell identity lives on tmux **pane user options**, not on window names.
//! A pane carries its own repository / worktree / index, so it keeps its
//! meaning wherever it is — including while it is docked in the dashboard's
//! own window, which is exactly where a window-name scheme loses track of it.
//!
//! Options are read back through `list-panes -F '#{@lw_repo}…'`; tmux
//! reports an **unset** option as the empty string, so parsing must treat
//! "unset" and "junk" the same way and simply decline.

/// Repository slug, on an agent pane.
pub const REPO_KEY: &str = "@lw_repo";
/// Worktree name, on an agent pane.
pub const WORKTREE_KEY: &str = "@lw_worktree";
/// Shell index (decimal), on an agent pane.
pub const INDEX_KEY: &str = "@lw_index";
/// `1` on a holder pane — the placeholder that keeps an agent window alive
/// while its agent pane is docked elsewhere.
#[allow(dead_code)] // the holder pane arrives in Phase 3
pub const HOLDER_KEY: &str = "@lw_holder";
/// `1` on the dashboard's slot pane — the pane an agent is swapped into.
#[allow(dead_code)] // the slot arrives in Phase 3
pub const SLOT_KEY: &str = "@lw_slot";
/// While docked: the docking dashboard's own pane id, on the agent pane.
pub const DOCKED_KEY: &str = "@lw_docked";
/// Absolute scope path, on the dashboard *window*.
#[allow(dead_code)] // the dashboard window announces its scope in Phase 2
pub const SCOPE_KEY: &str = "@lw_scope";

/// The identity of one agent shell, as carried by its pane.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShellId {
    pub repo: String,
    pub worktree: String,
    pub index: usize,
}

impl ShellId {
    /// Build a [`ShellId`] from three raw option values as tmux reports
    /// them. Returns `None` when any field is missing (tmux prints an unset
    /// option as `""`) or when the index is not a decimal number — a pane
    /// that is not one of our shells must decline, never panic.
    pub fn from_fields(repo: &str, worktree: &str, index: &str) -> Option<ShellId> {
        let repo = repo.trim();
        let worktree = worktree.trim();
        let index = index.trim();
        if repo.is_empty() || worktree.is_empty() {
            return None;
        }
        let index: usize = index.parse().ok()?;
        Some(ShellId {
            repo: repo.to_string(),
            worktree: worktree.to_string(),
            index,
        })
    }

    /// The identity of shell `index` of `worktree`, living in agent session
    /// `session`.
    ///
    /// The repository field is the **session slug** — the session name minus
    /// its `wingman-session-` prefix — because that is what already
    /// distinguishes two repositories whose names collide (the second one's
    /// session carries a hash suffix). Both components are sanitised the
    /// same way the window name is, so a stamp and a name backfilled from a
    /// window name agree by construction.
    pub fn for_session(session: &str, worktree: &str, index: usize) -> ShellId {
        ShellId {
            repo: session
                .strip_prefix(crate::tmux::SESSION_PREFIX)
                .unwrap_or(session)
                .to_string(),
            worktree: crate::tmux::Tmux::sanitize(worktree),
            index,
        }
    }

    /// The index as tmux should store it.
    pub fn index_value(&self) -> String {
        self.index.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shell_id_round_trips_through_its_option_values() {
        let id = ShellId {
            repo: "lazywingman".into(),
            worktree: "featureX".into(),
            index: 3,
        };
        let back =
            ShellId::from_fields(&id.repo, &id.worktree, &id.index_value()).expect("round-trips");
        assert_eq!(back, id);
    }

    /// tmux prints an unset user option as the empty string, so "unset" and
    /// "junk" arrive by the same door and must both be declined.
    #[test]
    fn unset_or_junk_fields_decline_instead_of_panicking() {
        assert_eq!(ShellId::from_fields("", "featureX", "0"), None);
        assert_eq!(ShellId::from_fields("repo", "", "0"), None);
        assert_eq!(ShellId::from_fields("repo", "featureX", ""), None);
        assert_eq!(ShellId::from_fields("repo", "featureX", "three"), None);
        assert_eq!(ShellId::from_fields("repo", "featureX", "-1"), None);
        assert_eq!(ShellId::from_fields("", "", ""), None);
    }

    #[test]
    fn surrounding_whitespace_is_not_an_identity() {
        assert_eq!(
            ShellId::from_fields(" repo ", " featureX ", " 7 "),
            Some(ShellId {
                repo: "repo".into(),
                worktree: "featureX".into(),
                index: 7,
            })
        );
    }

    /// The repository field is the session slug, so a hash-suffixed session
    /// names its own repository rather than colliding with the plain one.
    #[test]
    fn for_session_takes_the_session_slug_as_the_repository() {
        let a = ShellId::for_session("wingman-session-app", "Feat One!", 0);
        assert_eq!(a.repo, "app");
        assert_eq!(a.worktree, "Feat-One");
        let b = ShellId::for_session("wingman-session-app-1a2b3c4d", "feat", 0);
        assert_eq!(b.repo, "app-1a2b3c4d");
        assert_ne!(a.repo, b.repo);
    }

    /// Every option key is distinct: a typo that aliased two of them would
    /// make one silently overwrite the other on the same pane.
    #[test]
    fn the_option_keys_are_distinct_and_namespaced() {
        let keys = [
            REPO_KEY,
            WORKTREE_KEY,
            INDEX_KEY,
            HOLDER_KEY,
            SLOT_KEY,
            DOCKED_KEY,
            SCOPE_KEY,
        ];
        for k in keys {
            assert!(k.starts_with("@lw_"), "{k} is not namespaced");
        }
        let mut sorted = keys.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "duplicate option key");
    }
}
