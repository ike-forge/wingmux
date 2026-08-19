//! Agent status classification.
//!
//! Heuristics only — the pane preview in the dashboard lets the user confirm
//! what the classifier guessed. Order of checks:
//!
//! `dead` (the pane is gone from tmux, or tmux says its process died) →
//! the agent's own hook-reported state, when the pane carries a live
//! `@lw_agent` overlay and an agent is still the foreground command →
//! `failed` (any line matches a failure pattern) → `waiting` if the *last*
//! line matches a prompt pattern (the freshest signal) → `working` (any
//! line matches a busy pattern) → `waiting` (any line) → `ended`
//! (foreground command is a login shell *and* the tail shows no agent
//! activity) → `idle`.
//!
//! The tail is checked before the shell heuristic because agents are often
//! launched *from* a shell (e.g. `bash` running a script): the tail carries
//! the agent's real state, the foreground command only says "a shell is in
//! charge".

use regex::Regex;

use crate::config::StatusPatterns;
use crate::tmux::{PaneInfo, is_shell_command};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Working,
    Waiting,
    Blocked,
    Idle,
    Failed,
    Ended,
    Dead,
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Status::Working => "working",
            Status::Waiting => "waiting",
            Status::Blocked => "blocked",
            Status::Idle => "idle",
            Status::Failed => "failed",
            Status::Ended => "ended",
            Status::Dead => "dead",
        }
    }
}

/// Pre-compiled status patterns for one agent.
#[derive(Debug, Default, Clone)]
pub struct Compiled {
    pub working: Vec<Regex>,
    pub waiting: Vec<Regex>,
    pub failed: Vec<Regex>,
}

impl Compiled {
    pub fn compile(patterns: &StatusPatterns) -> Compiled {
        Compiled {
            working: compile_all(&patterns.working),
            waiting: compile_all(&patterns.waiting),
            failed: compile_all(&patterns.failed),
        }
    }
}

fn compile_all(list: &[String]) -> Vec<Regex> {
    list.iter()
        .filter_map(|p| match Regex::new(p) {
            Ok(r) => Some(r),
            Err(e) => {
                log::warn!("bad status pattern {p:?}: {e}");
                None
            }
        })
        .collect()
}

/// Classify a shell's state from its capture tail + tmux metadata. `pane`
/// is `None` when tmux no longer lists the pane at all.
pub fn classify(patterns: &Compiled, tail: &str, pane: Option<&PaneInfo>) -> Status {
    // A pane tmux stopped listing means the shell exited (ctrl-d) or its
    // window was killed outside the dashboard. tmux destroys the pane on
    // exit unless `remain-on-exit` is set, so a *vanished* pane — not the
    // `pane_dead` flag — is the usual death signal.
    let Some(pane) = pane else {
        return Status::Dead;
    };
    if pane.dead {
        return Status::Dead;
    }
    // An instrumented agent reports its own state; believe it over the
    // regexes. The login-shell guard drops a value stranded by an agent
    // that died without firing SessionEnd — the option lives in the tmux
    // server, so it would otherwise outlive the process that wrote it.
    if !is_shell_command(&pane.current_command)
        && let Some(o) = crate::hooks::Overlay::parse(&pane.overlay_raw)
    {
        return o.status;
    }
    if any_match(&patterns.failed, tail) {
        return Status::Failed;
    }
    // The last line is the freshest state: an agent parked at its prompt is
    // waiting even if older lines still show busy markers.
    let last_line = tail.lines().last().unwrap_or("");
    if any_match(&patterns.waiting, last_line) {
        return Status::Waiting;
    }
    if any_match(&patterns.working, tail) {
        return Status::Working;
    }
    if any_match(&patterns.waiting, tail) {
        return Status::Waiting;
    }
    if is_shell_command(&pane.current_command) {
        return Status::Ended;
    }
    Status::Idle
}

fn any_match(regexes: &[Regex], text: &str) -> bool {
    regexes.iter().any(|r| r.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_has_its_own_label() {
        assert_eq!(Status::Blocked.label(), "blocked");
    }

    fn pane(current_command: &str, dead: bool) -> PaneInfo {
        PaneInfo {
            pane_id: "%1".to_string(),
            current_command: current_command.to_string(),
            dead,
            overlay_raw: String::new(),
        }
    }

    fn pane_with(current_command: &str, dead: bool, overlay: &str) -> PaneInfo {
        PaneInfo {
            pane_id: "%1".to_string(),
            current_command: current_command.to_string(),
            dead,
            overlay_raw: overlay.to_string(),
        }
    }

    fn c(working: &[&str], waiting: &[&str], failed: &[&str]) -> Compiled {
        Compiled::compile(&StatusPatterns {
            working: working.iter().map(|s| s.to_string()).collect(),
            waiting: waiting.iter().map(|s| s.to_string()).collect(),
            failed: failed.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[test]
    fn dead_wins_over_everything() {
        let p = c(&["Thinking"], &["❯"], &["Error"]);
        assert_eq!(
            classify(&p, "Thinking ❯ Error", Some(&pane("claude", true))),
            Status::Dead
        );
    }

    /// The pane tmux no longer lists: the shell exited (ctrl-d) or its
    /// window was killed outside the dashboard. Without this the shell
    /// would keep whatever status it had when it was last alive.
    #[test]
    fn a_pane_tmux_no_longer_lists_is_dead() {
        let p = c(&["Thinking"], &["❯"], &["Error"]);
        assert_eq!(classify(&p, "What next? ❯", None), Status::Dead);
    }

    #[test]
    fn shell_foreground_with_agent_tail_is_not_ended() {
        // bash running an agent script: the tail carries the agent's state
        let p = c(&["Thinking"], &["❯"], &["Error"]);
        assert_eq!(
            classify(&p, "Thinking...", Some(&pane("bash", false))),
            Status::Working
        );
        assert_eq!(
            classify(
                &p,
                "What would you like to do? ❯",
                Some(&pane("bash", false))
            ),
            Status::Waiting
        );
        assert_eq!(
            classify(&p, "✖ Error: boom", Some(&pane("zsh", false))),
            Status::Failed
        );
        // ... but a bare shell with no agent signal is Ended
        assert_eq!(
            classify(&p, "nothing matches", Some(&pane("bash", false))),
            Status::Ended
        );
    }

    #[test]
    fn last_line_prompt_beats_stale_busy_lines() {
        let p = c(
            &["Thinking", "Working"],
            &["What would you like to do"],
            &["Error"],
        );
        // banner says "Thinking" but the agent is parked at its prompt now
        assert_eq!(
            classify(
                &p,
                "Thinking about the task...\nDone.\nWhat would you like to do? ❯",
                Some(&pane("bash", false))
            ),
            Status::Waiting
        );
        // mid-task: the busy line is on the bottom → working
        assert_eq!(
            classify(
                &p,
                "What would you like to do? ❯ busy 2\nWorking on it (2)...",
                Some(&pane("bash", false))
            ),
            Status::Working
        );
    }

    #[test]
    fn failed_beats_working_beats_waiting() {
        let p = c(&["Thinking"], &["❯"], &["Error:"]);
        assert_eq!(
            classify(&p, "Error: boom", Some(&pane("claude", false))),
            Status::Failed
        );
        assert_eq!(
            classify(&p, "Thinking...\n❯", Some(&pane("claude", false))),
            Status::Waiting
        );
        assert_eq!(
            classify(&p, "❯", Some(&pane("claude", false))),
            Status::Waiting
        );
    }

    #[test]
    fn idle_is_the_default() {
        let p = c(&["Thinking"], &["❯"], &["Error"]);
        assert_eq!(
            classify(&p, "nothing matches", Some(&pane("claude", false))),
            Status::Idle
        );
    }

    /// The whole point: hooks beat the regexes when they disagree.
    #[test]
    fn a_live_overlay_beats_the_heuristics() {
        let p = c(&["Thinking"], &["❯"], &["Error:"]);
        // The tail screams "working"; the agent says it is blocked on us.
        assert_eq!(
            classify(
                &p,
                "Thinking...",
                Some(&pane_with("claude", false, "blocked:99"))
            ),
            Status::Blocked
        );
        // The tail looks like a failure; the agent says it is still working.
        assert_eq!(
            classify(
                &p,
                "Error: boom",
                Some(&pane_with("claude", false, "working:99"))
            ),
            Status::Working
        );
    }

    /// tmux's judgement is the one thing an agent cannot override.
    #[test]
    fn dead_beats_the_overlay() {
        let p = c(&["Thinking"], &["❯"], &["Error:"]);
        assert_eq!(
            classify(&p, "", Some(&pane_with("claude", true, "working:99"))),
            Status::Dead
        );
        assert_eq!(classify(&p, "", None), Status::Dead);
    }

    /// A SIGKILLed agent never fires SessionEnd, stranding its overlay on a
    /// pane that has fallen back to a login shell. Because the value lives
    /// in the tmux server it survives restarts, so it must be discarded on
    /// read rather than trusted.
    #[test]
    fn a_stale_overlay_on_a_login_shell_is_discarded() {
        let p = c(&["Thinking"], &["❯"], &["Error:"]);
        assert_eq!(
            classify(
                &p,
                "nothing matches",
                Some(&pane_with("bash", false, "working:99"))
            ),
            Status::Ended
        );
    }

    /// Garbage, or a state only LazyWingman may assign, falls through.
    #[test]
    fn unparsable_overlays_fall_through_to_the_ladder() {
        let p = c(&["Thinking"], &["❯"], &["Error:"]);
        for raw in ["", "garbage", "dead:99"] {
            assert_eq!(
                classify(&p, "Thinking...", Some(&pane_with("claude", false, raw))),
                Status::Working,
                "overlay {raw:?} should not be believed"
            );
        }
    }

    #[test]
    fn bad_regexes_are_skipped() {
        let p = Compiled::compile(&StatusPatterns {
            working: vec!["(".to_string()],
            waiting: vec![],
            failed: vec![],
        });
        assert_eq!(p.working.len(), 0);
    }
}
