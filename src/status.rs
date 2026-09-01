//! Agent status classification.
//!
//! Heuristics only — the pane preview in the dashboard lets the user confirm
//! what the classifier guessed. Order of checks:
//!
//! `dead` (the pane is gone from tmux, or tmux says its process died) →
//! the agent's own hook-reported state, when the pane carries a live
//! `@lw_agent` overlay and an agent is still the foreground command (see
//! [`OVERLAY_STALE_AFTER_SECS`] for the one case this is *not* trusted:
//! stale AND the tail looks idle) → `ended` (the foreground command is a
//! login shell) → `failed` (any line matches a failure pattern) →
//! `waiting` if the *last* line matches a prompt pattern (the freshest
//! signal) → `working` (any line matches a busy pattern) → `waiting` (any
//! line) → `idle`.
//!
//! The shell heuristic sits above the regex ladder, not below it: what
//! tmux reports as the pane's foreground command is a fact about what is
//! running *now*, while the captured tail is scrollback that outlives the
//! process which wrote it. With the ladder first, `ended` was effectively
//! unreachable — a finished agent leaves its prompt glyphs, spinner
//! frames and the word "error" all over the last screenful, so the pane
//! kept reading `waiting` (or `working`, or `failed`) for as long as that
//! text stayed on screen. The cost is deliberate: a failure the agent
//! printed on its way out reads as `ended` rather than `failed`, and the
//! preview panel is where that text is still visible.

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

/// How long a hook-reported overlay is trusted without corroboration from
/// the pane's own tail.
///
/// Claude's `Stop` hook does not fire on every path that ends a turn: an
/// interrupted turn (Esc), a turn that ends in an API error, and `blocked`
/// after a *denied* permission (`PostToolUse` only fires after a tool
/// completes) all leave no later hook to fire, stranding the overlay at
/// whatever it last said — usually `working` — while the pane visibly sits
/// back at its prompt. Left untreated the shell reads `working` forever.
///
/// A blanket age-out is wrong: a legitimate agent turn routinely runs far
/// longer than any sane threshold, so age alone must never override a
/// fresh, still-running turn. The overlay is therefore only ignored when
/// BOTH hold — it is older than this threshold AND the pane's own tail
/// corroborates that the agent looks idle (its *last* line matches a
/// `waiting` pattern). Either condition alone is the wrong rule.
const OVERLAY_STALE_AFTER_SECS: u64 = 60;

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
    // The last line is the freshest state: an agent parked at its prompt is
    // waiting even if older lines still show busy markers. Computed once,
    // up front — both the overlay age-out below and the regex ladder need
    // it.
    let last_line = tail.lines().last().unwrap_or("");
    // An instrumented agent reports its own state; believe it over the
    // regexes. The login-shell guard drops a value stranded by an agent
    // that died without firing SessionEnd — the option lives in the tmux
    // server, so it would otherwise outlive the process that wrote it.
    if !is_shell_command(&pane.current_command)
        && let Some(o) = crate::hooks::Overlay::parse(&pane.overlay_raw)
    {
        let stale = crate::hooks::now_secs().saturating_sub(o.at) > OVERLAY_STALE_AFTER_SECS;
        let tail_looks_idle = any_match(&patterns.waiting, last_line);
        // See OVERLAY_STALE_AFTER_SECS: only fall through to the ladder
        // when BOTH hold — otherwise a legitimate mid-turn agent would get
        // misclassified the moment it ran past the threshold.
        if !(stale && tail_looks_idle) {
            return o.status;
        }
    }
    // Nothing but a login shell is running: the agent has exited (or was
    // never started), so every marker left in the tail is scrollback from
    // a process that is gone. Checked ahead of the regex ladder for that
    // reason — below it, `ended` was unreachable in practice, because a
    // finished session's scrollback almost always still holds one of the
    // agent's own prompt, spinner or error markers, and the pane went on
    // reading `waiting`/`working`/`failed` long after the agent quit.
    if is_shell_command(&pane.current_command) {
        return Status::Ended;
    }
    if any_match(&patterns.failed, tail) {
        return Status::Failed;
    }
    if any_match(&patterns.waiting, last_line) {
        return Status::Waiting;
    }
    if any_match(&patterns.working, tail) {
        return Status::Working;
    }
    if any_match(&patterns.waiting, tail) {
        return Status::Waiting;
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
            ..Default::default()
        }
    }

    fn pane_with(current_command: &str, dead: bool, overlay: &str) -> PaneInfo {
        PaneInfo {
            pane_id: "%1".to_string(),
            current_command: current_command.to_string(),
            dead,
            overlay_raw: overlay.to_string(),
            ..Default::default()
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

    /// What tmux reports as the foreground command outranks the tail: a
    /// login shell in charge means the agent has exited, and the markers
    /// still on screen are its leftovers. This is the whole reason `ended`
    /// is reachable at all — see the module doc.
    #[test]
    fn a_shell_foreground_is_ended_whatever_the_scrollback_says() {
        let p = c(&["Thinking"], &["\u{276f}"], &["Error"]);
        for tail in [
            "Thinking...",
            "What would you like to do? \u{276f}",
            "\u{2716} Error: boom",
            "nothing matches",
        ] {
            assert_eq!(
                classify(&p, tail, Some(&pane("bash", false))),
                Status::Ended,
                "{tail:?}"
            );
        }
    }

    /// The reported symptom: an agent quits, the shell prompt comes back,
    /// and the session it left on screen keeps the row yellow. Nothing
    /// matches the user's own prompt line, so the whole-tail fallbacks
    /// used to answer from the dead agent's scrollback instead.
    #[test]
    fn a_finished_agent_session_reads_ended_not_waiting() {
        let p = Compiled::compile(&crate::config::builtin_patterns("claude").unwrap());
        let tail = "\u{25cf} Wrote src/main.rs\n\u{276f} \n ~/projects/lw \u{e0b0} main \u{e0b0}";
        assert_eq!(classify(&p, tail, Some(&pane("zsh", false))), Status::Ended);
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
                Some(&pane("claude", false))
            ),
            Status::Waiting
        );
        // mid-task: the busy line is on the bottom → working
        assert_eq!(
            classify(
                &p,
                "What would you like to do? ❯ busy 2\nWorking on it (2)...",
                Some(&pane("claude", false))
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

    /// The regression this age-out exists to fix: an interrupted turn (Esc)
    /// never fires another hook, stranding the overlay at `working` while
    /// the pane visibly sits back at its prompt. Once the overlay is old
    /// AND the tail's last line looks like a prompt, it must be ignored in
    /// favour of the ladder.
    #[test]
    fn a_stale_overlay_with_a_prompt_shaped_tail_falls_back_to_the_ladder() {
        let p = c(&["Thinking"], &["❯"], &["Error:"]);
        let stale_at = crate::hooks::now_secs().saturating_sub(OVERLAY_STALE_AFTER_SECS + 1);
        let overlay = format!("working:{stale_at}");
        assert_eq!(
            classify(
                &p,
                "Thinking...\n❯",
                Some(&pane_with("claude", false, &overlay))
            ),
            Status::Waiting,
            "a stale overlay whose tail looks idle must fall through to the ladder"
        );
    }

    /// Age alone must never be enough: without a prompt-shaped tail, a
    /// stale overlay is still trusted — the agent may simply be mid-turn on
    /// something that happens to run long.
    #[test]
    fn a_stale_overlay_without_a_prompt_shaped_tail_is_still_believed() {
        let p = c(&["Thinking"], &["❯"], &["Error:"]);
        let stale_at = crate::hooks::now_secs().saturating_sub(OVERLAY_STALE_AFTER_SECS + 1);
        let overlay = format!("blocked:{stale_at}");
        assert_eq!(
            classify(
                &p,
                "Error: something that looks like a failure",
                Some(&pane_with("claude", false, &overlay))
            ),
            Status::Blocked,
            "a stale overlay must still win when the tail does not look idle"
        );
    }

    /// The mid-turn protection this whole rule exists to preserve: a FRESH
    /// overlay is trusted even when the tail happens to look prompt-shaped
    /// (e.g. the agent printed something that momentarily matches a
    /// waiting pattern while still working).
    #[test]
    fn a_fresh_overlay_with_a_prompt_shaped_tail_is_still_believed() {
        let p = c(&["Thinking"], &["❯"], &["Error:"]);
        let fresh_at = crate::hooks::now_secs();
        let overlay = format!("working:{fresh_at}");
        assert_eq!(
            classify(
                &p,
                "Thinking...\n❯",
                Some(&pane_with("claude", false, &overlay))
            ),
            Status::Working,
            "a fresh overlay must win even when the tail looks prompt-shaped"
        );
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
