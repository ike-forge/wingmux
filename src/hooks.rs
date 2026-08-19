//! Claude Code hook instrumentation (see
//! `docs/superpowers/specs/2026-08-19-semantic-agent-monitoring-design.md`).

use crate::status::Status;
use std::path::{Path, PathBuf};

/// tmux pane user option carrying the agent's self-reported state.
pub const OVERLAY_KEY: &str = "@lw_agent";

/// What a hook wrote onto a pane: a state plus when it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overlay {
    pub status: Status,
    /// Unix seconds. Recorded for age-out and time-in-state display; the
    /// staleness guard that matters today is the login-shell check in
    /// `status::classify`.
    pub at: u64,
}

impl Overlay {
    pub fn encode(&self) -> String {
        format!("{}:{}", self.status.label(), self.at)
    }

    /// Parse a value read back from tmux. Only the three states a hook can
    /// emit are accepted: `dead`, `failed`, `ended` and `idle` are
    /// LazyWingman's own judgements and must never arrive over the wire.
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
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What a hook event does to the pane's overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Set(Status),
    Clear,
    /// Leave whatever is on the pane untouched.
    Ignore,
}

/// Substrings that identify a `Notification` as "the agent is blocked on
/// the user", as opposed to the 60-second idle nudge. Matched
/// case-insensitively.
const BLOCKING_NOTIFICATIONS: &[&str] = &["needs your permission", "permission required"];

/// Map one hook event to its effect. `event` comes from argv;
/// `payload` is Claude's JSON on stdin (only `Notification` uses it).
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

/// Apply one hook event to a pane. Errors are returned for the caller to
/// log; the process still exits 0 (see `main`).
pub fn run_hook(
    event: &str,
    pane_id: &str,
    tmux: &crate::tmux::Tmux,
    payload: &serde_json::Value,
) -> crate::error::Result<()> {
    match action_for(event, payload) {
        Action::Set(status) => {
            let value = Overlay {
                status,
                at: now_secs(),
            }
            .encode();
            tmux.set_pane_option(pane_id, OVERLAY_KEY, &value)
        }
        Action::Clear => tmux.clear_pane_option(pane_id, OVERLAY_KEY),
        Action::Ignore => Ok(()),
    }
}

/// Name of the generated settings file. User-facing: it appears in the
/// pane's visible command line and is the disclosure that instrumentation
/// happened, so it must stay self-explanatory.
#[allow(dead_code)] // read by write_settings_file and app startup (later task)
pub const SETTINGS_FILE_NAME: &str = "lazywingman-activity-hooks.json";

/// Events we subscribe to. `PreToolUse`, `SubagentStop` and `PreCompact`
/// are deliberately absent: they add nothing the three states express, and
/// tool hooks are the highest-frequency events.
const SUBSCRIBED: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "Notification",
    "Stop",
    "SessionEnd",
];

#[allow(dead_code)] // called by write_settings_file and app startup (later task)
pub fn settings_path() -> PathBuf {
    crate::util::state_dir().join(SETTINGS_FILE_NAME)
}

/// The `--settings` payload: one matcher-less group per subscribed event,
/// each running `<exe> [--socket <s>] hook <Event>`.
#[allow(dead_code)] // called by write_settings_file (later task wires startup call)
pub fn settings_json(exe: &str, socket: Option<&str>) -> String {
    let sock = match socket {
        Some(s) => format!(" --socket {s}"),
        None => String::new(),
    };
    let mut hooks = serde_json::Map::new();
    for event in SUBSCRIBED {
        hooks.insert(
            (*event).to_string(),
            serde_json::json!([{
                "hooks": [{
                    "type": "command",
                    "command": format!("{exe}{sock} hook {event}"),
                }]
            }]),
        );
    }
    serde_json::json!({ "hooks": hooks }).to_string()
}

/// Regenerate the settings file. Called once at startup so a moved binary
/// or a changed socket self-heals.
#[allow(dead_code)] // called from app::run at startup (later task)
pub fn write_settings_file() -> crate::error::Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|e| crate::error::Error::Config(format!("cannot resolve own path: {e}")))?;
    let socket = std::env::var("LAZYWINGMAN_TMUX_SOCKET")
        .ok()
        .filter(|s| !s.is_empty());
    let json = settings_json(&exe.to_string_lossy(), socket.as_deref());
    let path = settings_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| crate::error::Error::Config(format!("cannot create state dir: {e}")))?;
    }
    std::fs::write(&path, json).map_err(|e| {
        crate::error::Error::Config(format!("cannot write {}: {e}", path.display()))
    })?;
    Ok(path)
}

/// Whether `command` already carries a `--settings <path>` pair whose path
/// ends in our settings file name. Token-precise: unlike a raw substring
/// search, this does not false-positive on a command that merely *mentions*
/// the file name (e.g. inside a quoted prompt), which would otherwise cause
/// `instrument` to silently skip a command that has never been instrumented.
fn already_carries_our_settings_flag(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    while let Some(tok) = tokens.next() {
        if tok == "--settings"
            && let Some(path_tok) = tokens.next()
            && Path::new(path_tok).file_name().and_then(|f| f.to_str()) == Some(SETTINGS_FILE_NAME)
        {
            return true;
        }
    }
    false
}

/// Add `--settings <path>` to a command, if and only if it is recognisably
/// Claude. The flag goes immediately after the binary token, never at the
/// end: `claude` takes a positional prompt, and trailing flags after it
/// are needlessly fragile.
///
/// Recognition is deliberately narrow — the first token's file name must be
/// exactly `claude`. Wrapper scripts and aliases are not instrumented and
/// fall back to the regex heuristics.
///
/// The insertion is byte-surgical: everything after the binary token is
/// copied verbatim from the original string, never re-tokenised. `claude`
/// takes a free-text positional prompt, and re-joining tokens with a single
/// space (as `split_whitespace` + `join(" ")` would) silently collapses any
/// deliberate internal spacing (or quoting) in that prompt before Claude
/// ever sees it.
#[allow(dead_code)] // called from the spawn sites (later task)
pub fn instrument(command: &str, settings: &Path) -> String {
    let Some(bin_start) = command.find(|c: char| !c.is_whitespace()) else {
        return command.to_string();
    };
    let after_bin_start = &command[bin_start..];
    let bin_len = after_bin_start
        .find(char::is_whitespace)
        .unwrap_or(after_bin_start.len());
    let bin = &after_bin_start[..bin_len];
    if Path::new(bin).file_name().and_then(|f| f.to_str()) != Some("claude") {
        return command.to_string();
    }
    if already_carries_our_settings_flag(command) {
        return command.to_string();
    }
    let bin_end = bin_start + bin_len;
    let leading = &command[..bin_start];
    let remainder = &command[bin_end..];
    format!(
        "{leading}{bin} --settings {}{remainder}",
        settings.display()
    )
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

    #[test]
    fn settings_json_registers_exactly_the_subscribed_events() {
        let s = settings_json("/opt/bin/lazywingman", None);
        let v: serde_json::Value = serde_json::from_str(&s).expect("valid json");
        let hooks = v["hooks"].as_object().expect("hooks object");
        let mut keys: Vec<_> = hooks.keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "Notification",
                "PostToolUse",
                "SessionEnd",
                "SessionStart",
                "Stop",
                "UserPromptSubmit"
            ]
        );
        let cmd = hooks["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .expect("command string");
        assert_eq!(cmd, "/opt/bin/lazywingman hook Stop");
        assert_eq!(
            hooks["Stop"][0]["hooks"][0]["type"].as_str(),
            Some("command")
        );
    }

    #[test]
    fn settings_json_passes_the_socket_through() {
        let s = settings_json("/opt/bin/lazywingman", Some("lwtest"));
        let v: serde_json::Value = serde_json::from_str(&s).expect("valid json");
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str(),
            Some("/opt/bin/lazywingman --socket lwtest hook Stop")
        );
    }

    #[test]
    fn instrument_inserts_the_flag_after_the_binary_not_at_the_end() {
        let p = std::path::Path::new("/s/lazywingman-activity-hooks.json");
        assert_eq!(
            instrument("claude", p),
            "claude --settings /s/lazywingman-activity-hooks.json"
        );
        // A positional prompt must stay last.
        assert_eq!(
            instrument("claude --resume", p),
            "claude --settings /s/lazywingman-activity-hooks.json --resume"
        );
        assert_eq!(
            instrument("/usr/local/bin/claude -c", p),
            "/usr/local/bin/claude --settings /s/lazywingman-activity-hooks.json -c"
        );
    }

    /// Anything we do not positively recognise as Claude is left alone —
    /// injecting flags into a command we do not understand is worse than
    /// falling back to the heuristics.
    #[test]
    fn non_claude_commands_are_left_untouched() {
        let p = std::path::Path::new("/s/lazywingman-activity-hooks.json");
        for cmd in [
            "codex",
            "codex exec --yolo",
            "my-wrapper.sh",
            "vibe",
            "claude-wrapper",
            "notclaude",
            "",
        ] {
            assert_eq!(instrument(cmd, p), cmd, "must not instrument {cmd:?}");
        }
    }

    #[test]
    fn already_instrumented_commands_are_not_instrumented_twice() {
        let p = std::path::Path::new("/s/lazywingman-activity-hooks.json");
        let once = instrument("claude", p);
        assert_eq!(instrument(&once, p), once);
    }

    /// A command that literally already carries our `--settings <path>`
    /// pair must be left alone, however that path was spelled — this is
    /// the idempotence guard the substring check was protecting, now
    /// exercised directly rather than only via a self-produced string.
    #[test]
    fn genuinely_already_instrumented_command_is_unchanged() {
        let p = std::path::Path::new("/s/lazywingman-activity-hooks.json");
        let cmd = "claude --settings /elsewhere/lazywingman-activity-hooks.json --resume";
        assert_eq!(instrument(cmd, p), cmd);
    }

    /// A prompt that merely *mentions* the settings file name (e.g. the
    /// user asked Claude to explain the file) must NOT be mistaken for an
    /// already-instrumented command — a raw substring test over the whole
    /// command string would wrongly skip instrumentation here, silently
    /// defeating the milestone's core promise that hooks always fire.
    #[test]
    fn command_merely_mentioning_the_filename_still_gets_instrumented() {
        let p = std::path::Path::new("/s/lazywingman-activity-hooks.json");
        let cmd = r#"claude "explain what lazywingman-activity-hooks.json is for""#;
        assert_eq!(
            instrument(cmd, p),
            r#"claude --settings /s/lazywingman-activity-hooks.json "explain what lazywingman-activity-hooks.json is for""#
        );
    }

    /// `claude` takes a free-text positional prompt. Internal spacing
    /// (deliberate formatting, code, ASCII layout) inside that prompt must
    /// survive byte for byte — re-tokenising with `split_whitespace` and
    /// rejoining with a single space would silently collapse it.
    #[test]
    fn internal_spacing_in_the_prompt_is_preserved_byte_for_byte() {
        let p = std::path::Path::new("/s/lazywingman-activity-hooks.json");
        let cmd = "claude \"line one    line two\"";
        assert_eq!(
            instrument(cmd, p),
            "claude --settings /s/lazywingman-activity-hooks.json \"line one    line two\""
        );
    }

    /// Recognition edge cases the review flagged as untested: a
    /// whitespace-only command has no first token at all (left untouched);
    /// `./claude` and a trailing-slash path both resolve, via
    /// `Path::file_name`, to a final component of exactly `claude`, so both
    /// are recognised and instrumented.
    #[test]
    fn recognition_edge_cases() {
        let p = std::path::Path::new("/s/lazywingman-activity-hooks.json");
        assert_eq!(instrument("   ", p), "   ");
        assert_eq!(
            instrument("./claude", p),
            "./claude --settings /s/lazywingman-activity-hooks.json"
        );
        assert_eq!(
            instrument("/usr/local/bin/claude/", p),
            "/usr/local/bin/claude/ --settings /s/lazywingman-activity-hooks.json"
        );
    }

    #[test]
    fn hook_ignores_unknown_events_without_touching_tmux() {
        // No tmux server needed: an Ignore action must not shell out.
        let tmux = crate::tmux::Tmux::detect().expect("detect");
        let r = run_hook(
            "PreToolUse",
            "%nonexistent",
            &tmux,
            &serde_json::Value::Null,
        );
        assert!(r.is_ok(), "ignored events must be a no-op, got {r:?}");
    }

    #[test]
    fn write_settings_file_writes_under_state_dir() {
        crate::util::testutil::with_fresh_xdg(|| {
            let path = write_settings_file().expect("writes settings file");
            assert_eq!(path, crate::util::state_dir().join(SETTINGS_FILE_NAME));
            let contents = std::fs::read_to_string(&path).expect("file exists");
            let v: serde_json::Value = serde_json::from_str(&contents).expect("valid json");
            assert!(v["hooks"]["Stop"][0]["hooks"][0]["command"].is_string());
        });
    }
}
