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
}
