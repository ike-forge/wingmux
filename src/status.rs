//! Agent status classification.
//!
//! Heuristics only — the pane preview in the dashboard lets the user confirm
//! what the classifier guessed. Order of checks:
//!
//! `dead` (tmux says the pane's process died) → `ended` (foreground command is
//! a login shell, i.e. the agent exited) → `failed` (tail matches a failure
//! pattern) → `waiting` (tail matches a prompt pattern) → `working` (tail
//! matches a busy pattern) → `idle`.

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
    if is_shell_command(current_command) {
        return Status::Ended;
    }
    if any_match(&patterns.failed, tail) {
        return Status::Failed;
    }
    if any_match(&patterns.waiting, tail) {
        return Status::Waiting;
    }
    if any_match(&patterns.working, tail) {
        return Status::Working;
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
    fn ended_when_shell_is_foreground() {
        let p = c(&["Thinking"], &["❯"], &["Error"]);
        assert_eq!(classify(&p, "anything", "bash", false), Status::Ended);
        assert_eq!(classify(&p, "anything", "zsh", false), Status::Ended);
    }

    #[test]
    fn failed_beats_waiting_beats_working() {
        let p = c(&["Thinking"], &["❯"], &["Error:"]);
        assert_eq!(classify(&p, "Error: boom", "claude", false), Status::Failed);
        assert_eq!(
            classify(&p, "Thinking... ❯", "claude", false),
            Status::Waiting
        );
        assert_eq!(
            classify(&p, "Thinking...", "claude", false),
            Status::Working
        );
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
    fn agent_process_name_beats_tail() {
        // an agent binary in the foreground is never "ended" even if the
        // tail shows a shell prompt
        let p = c(&[], &["\\$"], &[]);
        assert_eq!(classify(&p, "$ ", "claude", false), Status::Waiting);
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
