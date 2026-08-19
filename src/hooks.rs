//! Claude Code hook instrumentation (see
//! `docs/superpowers/specs/2026-08-19-semantic-agent-monitoring-design.md`).

use crate::status::Status;

/// tmux pane user option carrying the agent's self-reported state.
#[allow(dead_code)] // wired up when the poll loop reads the overlay (later task)
pub const OVERLAY_KEY: &str = "@lw_agent";

/// What a hook wrote onto a pane: a state plus when it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // constructed by the hook subcommand and read by the poll loop (later tasks)
pub struct Overlay {
    pub status: Status,
    /// Unix seconds. Recorded for age-out and time-in-state display; the
    /// staleness guard that matters today is the login-shell check in
    /// `status::classify`.
    pub at: u64,
}

impl Overlay {
    #[allow(dead_code)] // called by the hook subcommand (later task)
    pub fn encode(&self) -> String {
        format!("{}:{}", self.status.label(), self.at)
    }

    /// Parse a value read back from tmux. Only the three states a hook can
    /// emit are accepted: `dead`, `failed`, `ended` and `idle` are
    /// LazyWingman's own judgements and must never arrive over the wire.
    #[allow(dead_code)] // called by the poll loop (later task)
    pub fn parse(s: &str) -> Option<Overlay> {
        let (label, ts) = s.split_once(':')?;
        let status = match label {
            "working" => Status::Working,
            "waiting" => Status::Waiting,
            "blocked" => Status::Blocked,
            _ => return None,
        };
        Some(Overlay {
            status,
            at: ts.parse().ok()?,
        })
    }
}

/// Unix seconds now, or 0 if the clock is before the epoch (never in
/// practice; a 0 timestamp is harmless — it only ages the overlay out).
#[allow(dead_code)] // called by the hook subcommand (later task)
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What a hook event does to the pane's overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // used by the hook subcommand (later task)
pub enum Action {
    Set(Status),
    Clear,
    /// Leave whatever is on the pane untouched.
    Ignore,
}

/// Substrings that identify a `Notification` as "the agent is blocked on
/// the user", as opposed to the 60-second idle nudge. Matched
/// case-insensitively.
#[allow(dead_code)] // used by action_for (later task)
const BLOCKING_NOTIFICATIONS: &[&str] = &["needs your permission", "permission required"];

/// Map one hook event to its effect. `event` comes from argv;
/// `payload` is Claude's JSON on stdin (only `Notification` uses it).
#[allow(dead_code)] // called by the hook subcommand (later task)
pub fn action_for(event: &str, payload: &serde_json::Value) -> Action {
    match event {
        "SessionStart" | "Stop" => Action::Set(Status::Waiting),
        "UserPromptSubmit" | "PostToolUse" => Action::Set(Status::Working),
        "SessionEnd" => Action::Clear,
        "Notification" => {
            let msg = payload
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_lowercase();
            if BLOCKING_NOTIFICATIONS.iter().any(|n| msg.contains(n)) {
                Action::Set(Status::Blocked)
            } else {
                // Unrecognised wording must not overwrite a good state.
                Action::Ignore
            }
        }
        _ => Action::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Status;

    #[test]
    fn overlay_round_trips() {
        let o = Overlay {
            status: Status::Blocked,
            at: 1_755_600_000,
        };
        assert_eq!(o.encode(), "blocked:1755600000");
        let back = Overlay::parse("blocked:1755600000").expect("parses");
        assert_eq!(back.status, Status::Blocked);
        assert_eq!(back.at, 1_755_600_000);
    }

    #[test]
    fn every_overlay_status_round_trips() {
        for st in [Status::Working, Status::Waiting, Status::Blocked] {
            let s = Overlay { status: st, at: 7 }.encode();
            assert_eq!(Overlay::parse(&s).expect("parses").status, st);
        }
    }

    /// A value tmux hands back that we did not write (or a truncated one)
    /// must not panic and must not be believed.
    #[test]
    fn junk_overlays_are_rejected() {
        for s in [
            "",
            "blocked",
            "blocked:",
            ":12",
            "nonsense:12",
            "blocked:abc",
        ] {
            assert!(Overlay::parse(s).is_none(), "should reject {s:?}");
        }
    }

    /// Statuses the hooks never emit must never come back from the wire —
    /// `dead` in particular is tmux's judgement, never an agent's.
    #[test]
    fn non_overlay_statuses_are_rejected() {
        for s in ["dead:12", "failed:12", "ended:12", "idle:12"] {
            assert!(Overlay::parse(s).is_none(), "should reject {s:?}");
        }
    }

    fn payload(message: &str) -> serde_json::Value {
        serde_json::json!({ "message": message })
    }

    fn none() -> serde_json::Value {
        serde_json::Value::Null
    }

    #[test]
    fn lifecycle_events_map_to_states() {
        assert_eq!(
            action_for("SessionStart", &none()),
            Action::Set(Status::Waiting)
        );
        assert_eq!(
            action_for("UserPromptSubmit", &none()),
            Action::Set(Status::Working)
        );
        assert_eq!(
            action_for("PostToolUse", &none()),
            Action::Set(Status::Working)
        );
        assert_eq!(action_for("Stop", &none()), Action::Set(Status::Waiting));
    }

    #[test]
    fn session_end_clears_the_overlay() {
        assert_eq!(action_for("SessionEnd", &none()), Action::Clear);
    }

    /// Permission requests are the reason `blocked` exists.
    #[test]
    fn permission_notifications_block() {
        assert_eq!(
            action_for(
                "Notification",
                &payload("Claude needs your permission to use Bash")
            ),
            Action::Set(Status::Blocked)
        );
        assert_eq!(
            action_for("Notification", &payload("Permission required for Edit")),
            Action::Set(Status::Blocked)
        );
    }

    /// The 60s idle nudge is NOT a block: `Stop` already parked the shell at
    /// `waiting`, and flipping it to `blocked` would blur exactly the
    /// distinction this milestone adds.
    #[test]
    fn idle_notifications_leave_the_state_alone() {
        assert_eq!(
            action_for("Notification", &payload("Claude is waiting for your input")),
            Action::Ignore
        );
    }

    /// The one place we couple to Claude's wording, so it must fail safe:
    /// wording we do not recognise leaves the existing state standing.
    #[test]
    fn unrecognised_notifications_fail_safe() {
        assert_eq!(
            action_for("Notification", &payload("something new in v9")),
            Action::Ignore
        );
        assert_eq!(action_for("Notification", &none()), Action::Ignore);
    }

    /// Events we deliberately do not subscribe to — and anything a future
    /// Claude invents — must be inert rather than misclassified.
    #[test]
    fn unsubscribed_events_are_ignored() {
        for e in ["PreToolUse", "SubagentStop", "PreCompact", "Whatever"] {
            assert_eq!(action_for(e, &none()), Action::Ignore, "event {e}");
        }
    }
}
