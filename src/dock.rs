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
//!
//! # The furniture, and what it costs
//!
//! Every agent window carries a **holder** and the dashboard window carries
//! a **slot**: two sleeping panes of matching size, so that docking is one
//! `swap-pane` that changes no window's pane count and no pane's dimensions.
//! Agent sessions are set to `window-size manual` and their windows are held
//! at the dashboard window's size, which is what keeps the two sides equal.
//!
//! Two degradations are accepted rather than fixed:
//!
//! - **Someone attaching an agent session directly resizes its windows** to
//!   their own terminal. The next poll puts them back, so that one dock
//!   costs a single repaint. Correctness is unaffected; only the flicker is.
//! - **A holder is one sleeping process per shell.** It shows up in
//!   `list-panes`, in the user's own pane navigation and in anything that
//!   counts panes. It is marked once ([`HOLDER_KEY`]) and filtered once, in
//!   `Tmux::list_panes`, which selects on identity — furniture carries none.

/// Repository slug, on an agent pane.
pub const REPO_KEY: &str = "@lw_repo";
/// Worktree name, on an agent pane.
pub const WORKTREE_KEY: &str = "@lw_worktree";
/// Shell index (decimal), on an agent pane.
pub const INDEX_KEY: &str = "@lw_index";
/// `1` on a holder pane — the placeholder that keeps an agent window alive
/// while its agent pane is docked elsewhere.
pub const HOLDER_KEY: &str = "@lw_holder";
/// `1` on the dashboard's slot pane — the pane an agent is swapped into.
pub const SLOT_KEY: &str = "@lw_slot";
/// While docked: the docking dashboard's own pane id, on the agent pane.
pub const DOCKED_KEY: &str = "@lw_docked";
/// Absolute scope path, on the dashboard *window*.
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

// ------------------------------------------------------------- furniture

/// What a holder pane says to whoever attaches to an agent session directly
/// and finds a sleeping pane where their agent used to be.
pub const HOLDER_NOTE: &str = "[LazyWingman] holder — this window's agent pane is docked in a dashboard. \
It comes back here when it is undocked.";

/// What the dashboard's slot says while nothing is docked in it.
pub const SLOT_NOTE: &str = "[LazyWingman] slot — the selected agent's pane docks here.";

/// Narrowest useful dashboard pane: below this the tree stops being
/// readable, and a docked layout that cannot show the tree has no reason to
/// exist.
pub const DASH_MIN_WIDTH: u16 = 24;
/// Widest: the tree never earns more, and every extra column is one the
/// agent does not get. Matches `ui::TREE_MAX`.
pub const DASH_MAX_WIDTH: u16 = 48;

/// Width of the dashboard's own pane once a slot sits beside it — and so,
/// by construction, the width of every holder.
///
/// Holder and dashboard pane face each other across a swap: while they are
/// the same width, the agent pane and the slot are too, and the swap costs
/// no resize and no repaint. The proportion is the Wide tier's 32%, so the
/// docked layout looks like the split layout it replaces.
pub fn dash_width(window_width: u16) -> u16 {
    let want = (window_width as u32 * 32 / 100) as u16;
    want.clamp(DASH_MIN_WIDTH, DASH_MAX_WIDTH)
        // Never wider than the window can hold: one column for the divider,
        // one for whatever sits beside it.
        .min(window_width.saturating_sub(2))
}

/// Width left for the agent, opposite a [`dash_width`] pane (the divider
/// costs one column).
pub fn agent_width(window_width: u16) -> u16 {
    window_width.saturating_sub(dash_width(window_width) + 1)
}

/// Wrap `s` in single quotes for `/bin/sh`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The command a furniture pane runs: say what it is, then sleep.
///
/// The sleep is a loop rather than one enormous interval — `sleep` with a
/// ten-year argument is a GNU nicety, and this has to survive busybox. The
/// pane is furniture, so it is never reaped on a timer: it dies with its
/// window, or when [`SLOT_KEY`]'s owner quits.
pub fn park_command(note: &str) -> String {
    format!(
        "printf '%s\\n' {}; while :; do sleep 3600; done",
        sh_quote(note)
    )
}

/// Give every agent window a holder it is missing — the backfill for
/// windows spawned before holders existed, and the repair for a holder the
/// user closed by hand.
///
/// A window is judged by what is in it *now*: a window whose only pane is a
/// holder is a docked shell's home and is already furnished, and a window
/// with no pane of ours in it at all is none of our business. Returns how
/// many holders were added.
pub fn ensure_holders(tmux: &crate::tmux::Tmux, width: u16) -> usize {
    let panes = match tmux.list_furniture() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("holder backfill: {e}");
            return 0;
        }
    };
    // window id -> (has a holder, holds something of ours)
    let mut windows: Vec<(String, bool, bool)> = Vec::new();
    for p in &panes {
        if !crate::tmux::is_lazywingman_session(&p.session_name) {
            continue;
        }
        match windows.iter_mut().find(|(w, _, _)| *w == p.window_id) {
            Some(entry) => {
                entry.1 |= p.holder;
                entry.2 |= p.holder || p.shell;
            }
            None => windows.push((p.window_id.clone(), p.holder, p.holder || p.shell)),
        }
    }
    let mut added = 0;
    for (window_id, has_holder, ours) in windows {
        if has_holder || !ours {
            continue;
        }
        match tmux.split_holder(&window_id, width) {
            Ok(pane) => {
                log::info!("backfilled holder {pane} into {window_id}");
                added += 1;
            }
            Err(e) => log::warn!("cannot backfill a holder into {window_id}: {e}"),
        }
    }
    added
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
