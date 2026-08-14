//! Agent status classification.
//!
//! Heuristics only — the pane preview in the dashboard lets the user confirm
//! what the classifier guessed. Order of checks:
//!
//! `dead` (tmux says the pane's process died) → `failed` (any line matches a
//! failure pattern) → `waiting` if the *last* line matches a prompt pattern
//! (the freshest signal) → `working` (any line matches a busy pattern) →
//! `waiting` (any line) → `ended` (foreground command is a login shell *and*
//! the tail shows no agent activity) → `idle`.
//!
//! The tail is checked before the shell heuristic because agents are often
//! launched *from* a shell (e.g. `bash` running a script): the tail carries
//! the agent's real state, the foreground command only says "a shell is in
//! charge".

use regex::Regex;

use crate::config::StatusPatterns;
use crate::tmux::is_shell_command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Working,
    Waiting,
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

/// Classify a pane's state from its capture tail + tmux metadata.
pub fn classify(patterns: &Compiled, tail: &str, current_command: &str, dead: bool) -> Status {
    if dead {
        return Status::Dead;
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
    if is_shell_command(current_command) {
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
            classify(&p, "Thinking ❯ Error", "claude", true),
            Status::Dead
        );
    }

    #[test]
    fn shell_foreground_with_agent_tail_is_not_ended() {
        // bash running an agent script: the tail carries the agent's state
        let p = c(&["Thinking"], &["❯"], &["Error"]);
        assert_eq!(classify(&p, "Thinking...", "bash", false), Status::Working);
        assert_eq!(
            classify(&p, "What would you like to do? ❯", "bash", false),
            Status::Waiting
        );
        assert_eq!(classify(&p, "✖ Error: boom", "zsh", false), Status::Failed);
        // ... but a bare shell with no agent signal is Ended
        assert_eq!(
            classify(&p, "nothing matches", "bash", false),
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
                "bash",
                false
            ),
            Status::Waiting
        );
        // mid-task: the busy line is on the bottom → working
        assert_eq!(
            classify(
                &p,
                "What would you like to do? ❯ busy 2\nWorking on it (2)...",
                "bash",
                false
            ),
            Status::Working
        );
    }

    #[test]
    fn failed_beats_working_beats_waiting() {
        let p = c(&["Thinking"], &["❯"], &["Error:"]);
        assert_eq!(classify(&p, "Error: boom", "claude", false), Status::Failed);
        assert_eq!(
            classify(&p, "Thinking...\n❯", "claude", false),
            Status::Waiting
        );
        assert_eq!(classify(&p, "❯", "claude", false), Status::Waiting);
    }

    #[test]
    fn idle_is_the_default() {
        let p = c(&["Thinking"], &["❯"], &["Error"]);
        assert_eq!(
            classify(&p, "nothing matches", "claude", false),
            Status::Idle
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
