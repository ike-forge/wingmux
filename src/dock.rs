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

use crate::error::{Error, Result};
use crate::tmux::{SESSION_PREFIX, Tmux};

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
/// exist. Matches `ui::TREE_MIN`'s width, so a docked dashboard never cuts
/// itself down to the "terminal too small" tier.
pub const DASH_MIN_WIDTH: u16 = 26;
/// Widest: the tree never earns more, and every extra column is one the
/// agent does not get. Matches `ui::TREE_MAX`.
pub const DASH_MAX_WIDTH: u16 = 54;

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

// -------------------------------------------------------------- movement

/// Dock `pane` (an agent pane) into `slot` (this dashboard's slot pane).
///
/// One `swap-pane`: the agent lands beside the tree and the slot goes to
/// live in the agent's own window, where it becomes what undocking swaps
/// back. `dash_pane` — the docking dashboard's own pane — is written onto
/// the agent pane as [`DOCKED_KEY`], which is the lease: it is how every
/// other instance sees that this shell is taken, and how a later startup
/// tells a live dock from one whose dashboard died.
///
/// Exactly one pane is docked at a time. Docking a different shell is
/// undock-then-dock — two swaps — never two occupied slots.
// Nothing calls it yet: Task 12 binds the key that does, and the swap is
// exercised by this module's tests meanwhile.
#[allow(dead_code)]
pub fn dock(tmux: &Tmux, slot: &str, pane: &str, dash_pane: &str) -> Result<()> {
    tmux.swap_panes(slot, pane)?;
    // After the swap, not before: a mark written onto a pane that then
    // fails to move would advertise a lease over a shell that never left
    // home.
    tmux.set_pane_option(pane, DOCKED_KEY, dash_pane)?;
    log::info!("docked {pane} into slot {slot} (dashboard {dash_pane})");
    Ok(())
}

/// Send a docked `pane` back to `home_window`, its own agent window.
///
/// The pane it changes places with is the one furniture pane still sitting
/// at home: the slot, put there by [`dock`], or the holder when the slot
/// has been killed since (a dashboard that died with a pane docked).
/// Swapping with the holder leaves the window one pane short of furnished;
/// `ensure_holders` puts that right on the next reconcile.
pub fn undock(tmux: &Tmux, pane: &str, home_window: &str) -> Result<()> {
    let target = home_target(tmux, home_window, pane)?;
    tmux.swap_panes(pane, &target)?;
    // The lease is released only once the pane is actually home.
    if let Err(e) = tmux.clear_pane_option(pane, DOCKED_KEY) {
        log::warn!("cannot clear the docked mark on {pane}: {e}");
    }
    log::info!("undocked {pane} back into {home_window} (swapped with {target})");
    Ok(())
}

/// The pane in `home_window` a docked pane changes places with: the slot
/// first, the holder second. `docked` is excluded — a pane cannot swap
/// with itself, and a "home window" that already holds it is a no-op we
/// must not turn into a tmux error.
fn home_target(tmux: &Tmux, home_window: &str, docked: &str) -> Result<String> {
    let panes = tmux.list_furniture()?;
    let here = || {
        panes
            .iter()
            .filter(|p| p.window_id == home_window && p.pane_id != docked)
    };
    here()
        .find(|p| p.slot)
        .or_else(|| here().find(|p| p.holder))
        .map(|p| p.pane_id.clone())
        .ok_or_else(|| {
            Error::Tmux(format!(
                "{home_window} has no furniture to swap {docked} for"
            ))
        })
}

/// The window an agent pane belongs back in: a window of its own agent
/// session that holds furniture and no shell of ours — exactly the shape
/// [`dock`] leaves behind when it takes the agent pane away.
///
/// `taken` excludes windows already spoken for earlier in the same pass,
/// so two orphans of one repository are never sent to the same window.
pub fn home_window(
    furniture: &[crate::tmux::FurniturePane],
    id: &ShellId,
    taken: &[String],
) -> Option<String> {
    let home_session = format!("{SESSION_PREFIX}{}", id.repo);
    furniture
        .iter()
        .filter(|p| p.session_name == home_session && !taken.contains(&p.window_id))
        .map(|p| p.window_id.clone())
        .find(|w| {
            let here = || furniture.iter().filter(|p| &p.window_id == w);
            here().any(|p| p.slot || p.holder) && !here().any(|p| p.shell)
        })
}

/// Put every stray agent pane back in its own session.
///
/// This is the crash path. A dashboard that dies mid-dock leaves its agent
/// pane alive in a session that does not own it — locatable, because
/// identity travels with the pane, but in the wrong place: the tree would
/// render it under a window that is not its home. Adoption runs before the
/// first reconcile, so the tree never draws that.
///
/// A pane whose [`DOCKED_KEY`] names a dashboard pane that is still alive
/// is left alone: that is a live lease held by another instance, not
/// wreckage. Our own pane id does not count as alive for this purpose — a
/// dashboard that has just started holds nothing, whatever a pane inherited
/// from the run before says.
///
/// Returns how many panes were brought home.
///
/// Deviates from the plan's `adopt_orphans(tmux, state)` in taking no
/// state: adoption runs *before* the first reconcile, when state knows no
/// panes yet, and a pane's own [`ShellId`] already names the session that
/// owns it.
pub fn adopt_orphans(tmux: &Tmux) -> Result<usize> {
    let furniture = tmux.list_furniture()?;
    let own = tmux.inside.as_ref().map(|i| i.pane_id.as_str());
    let mut adopted = 0;
    // Home windows already spoken for by an earlier adoption in this pass:
    // two orphans of one repository must not be sent to the same window.
    let mut taken: Vec<String> = Vec::new();
    for pane in tmux.list_panes()? {
        let Some(id) = &pane.shell else { continue };
        let home_session = format!("{SESSION_PREFIX}{}", id.repo);
        if pane.session_name == home_session {
            continue;
        }
        if let Some(holder) = &pane.docked_by
            && Some(holder.as_str()) != own
            && furniture.iter().any(|p| &p.pane_id == holder)
        {
            log::info!(
                "{} is docked in {holder}, which is alive — leaving it there",
                pane.pane_id
            );
            continue;
        }
        let Some(home) = home_window(&furniture, id, &taken) else {
            log::warn!(
                "{} belongs to {home_session}, which has no window waiting for it",
                pane.pane_id
            );
            continue;
        };
        match undock(tmux, &pane.pane_id, &home) {
            Ok(()) => {
                taken.push(home);
                adopted += 1;
            }
            Err(e) => log::warn!("cannot adopt {} into {home}: {e}", pane.pane_id),
        }
    }
    if adopted > 0 {
        log::info!("adopted {adopted} stray agent pane(s)");
    }
    Ok(adopted)
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

    // ---- M18 Task 13 — recovery ----

    use crate::tmux::testutil::{HOLDER_W, with_tmux_server};
    use std::path::Path;

    /// Where a pane sits right now, as (session, window).
    fn at(tmux: &Tmux, pane: &str) -> (String, String) {
        tmux.list_furniture()
            .expect("furniture")
            .into_iter()
            .find(|p| p.pane_id == pane)
            .map(|p| (p.session_name, p.window_id))
            .expect("pane still exists")
    }

    /// The crash path. A dashboard that died mid-dock left its agent pane
    /// alive in a session that does not own it; the next dashboard must put
    /// it back before anything renders a tree, or the shell shows up under
    /// a window that is not its home.
    #[test]
    fn a_pane_orphaned_by_a_dead_dashboard_is_swapped_home() {
        with_tmux_server(|_sock, tmux| {
            let agent = tmux
                .new_session(
                    "wingman-session-o",
                    "lw:feat/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-o", "feat", 0),
                    HOLDER_W,
                )
                .expect("agent session");
            // A dashboard of its own, in the 'main' session, which docks the
            // agent pane and then dies.
            let dash = tmux
                .run_raw(&[
                    "split-window",
                    "-d",
                    "-t",
                    "%0",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "bash",
                ])
                .expect("dashboard pane")
                .trim()
                .to_string();
            let slot = tmux.split_slot(&dash, 40).expect("slot");
            dock(&tmux, &slot, &agent.pane_id, &dash).expect("dock");
            tmux.kill_pane(&dash).expect("the dashboard dies");

            // A fresh dashboard, holding nothing of its own.
            let mut fresh = tmux.clone();
            fresh.inside = None;
            assert_eq!(adopt_orphans(&fresh).expect("adopt"), 1);

            assert_eq!(
                at(&tmux, &agent.pane_id).1,
                agent.window_id,
                "the agent pane is home"
            );
            assert_eq!(at(&tmux, &slot).0, "main", "and the slot came back with it");
            let home = tmux
                .list_panes()
                .expect("panes")
                .into_iter()
                .find(|p| p.pane_id == agent.pane_id)
                .expect("still ours");
            assert_eq!(home.docked_by, None, "the dead lease is released");
            assert_eq!(
                adopt_orphans(&fresh).expect("again"),
                0,
                "nothing left to do"
            );
        });
    }

    /// A pane docked in a dashboard that is still alive is a lease, not
    /// wreckage: adoption must leave it exactly where it is, or two running
    /// instances would take the same shell from each other on every start.
    #[test]
    fn adoption_leaves_a_pane_docked_in_a_live_dashboard_alone() {
        with_tmux_server(|_sock, tmux| {
            let agent = tmux
                .new_session(
                    "wingman-session-l",
                    "lw:feat/0",
                    Path::new("/tmp"),
                    "bash",
                    &ShellId::for_session("wingman-session-l", "feat", 0),
                    HOLDER_W,
                )
                .expect("agent session");
            let slot = tmux.split_slot("%0", 40).expect("slot");
            // %0 — the docking dashboard — is alive and stays alive.
            dock(&tmux, &slot, &agent.pane_id, "%0").expect("dock");

            let mut other = tmux.clone();
            other.inside = None;
            assert_eq!(adopt_orphans(&other).expect("adopt"), 0);
            assert_eq!(
                at(&tmux, &agent.pane_id).0,
                "main",
                "the leased pane stays where its dashboard put it"
            );
        });
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
