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

pub fn settings_path() -> PathBuf {
    crate::util::state_dir().join(SETTINGS_FILE_NAME)
}

/// Single-quote `s` for a POSIX shell: wrap it in `'...'`, escaping any
/// embedded `'` with the standard `'\''` sequence (close quote, escaped
/// quote, reopen quote). `settings_json`'s `command` is run through a
/// shell by Claude, and `instrument`'s output is typed into a live shell
/// by `send_keys` — without this, a space in `$HOME` or
/// `$XDG_STATE_HOME` breaks both silently (worse than pre-M17, where the
/// plain unquoted command simply worked).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The `--settings` payload: one matcher-less group per subscribed event,
/// each running `<exe> [--socket <s>] hook <Event>`.
pub fn settings_json(exe: &str, socket: Option<&str>) -> String {
    let sock = match socket {
        Some(s) => format!(" --socket {s}"),
        None => String::new(),
    };
    let exe = shell_quote(exe);
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

/// Read one shell-quote-aware token from the start of `s`. Handles the one
/// quoting form this crate ever emits — single quotes, with the standard
/// `'\''` escape for an embedded `'` — and otherwise falls back to a plain
/// whitespace-delimited token, so a pre-quoting-fix (or hand-typed) unquoted
/// path still parses. Returns the unescaped token and how many bytes of `s`
/// it consumed; `None` if `s` is empty.
fn next_shell_token(s: &str) -> Option<(String, usize)> {
    if s.is_empty() {
        return None;
    }
    if !s.starts_with('\'') {
        let end = s.find(char::is_whitespace).unwrap_or(s.len());
        return Some((s[..end].to_string(), end));
    }
    let mut token = String::new();
    let mut chars = s.char_indices();
    chars.next(); // opening quote
    while let Some((i, c)) = chars.next() {
        if c != '\'' {
            token.push(c);
            continue;
        }
        // `'\''`: closing quote + escaped quote + reopening quote — the
        // standard way to embed a literal `'` inside a single-quoted run.
        if s[i + 1..].starts_with("\\''") {
            token.push('\'');
            chars.next(); // backslash
            chars.next(); // escaped quote
            chars.next(); // reopening quote
            continue;
        }
        // Closing quote: the token ends here.
        return Some((token, i + 1));
    }
    // Unterminated quote: never produced by our own quoting, but treat the
    // rest as the token rather than panicking.
    Some((token, s.len()))
}

/// Whether `command` already carries a `--settings <path>` pair whose path
/// ends in our settings file name. Token-precise: unlike a raw substring
/// search, this does not false-positive on a command that merely *mentions*
/// the file name (e.g. inside a quoted prompt), which would otherwise cause
/// `instrument` to silently skip a command that has never been instrumented.
///
/// The path token is read shell-quote-aware (`next_shell_token`):
/// `instrument` single-quotes a settings path that contains whitespace, so
/// a naive `split_whitespace` would see only the first fragment of that
/// path and miss the match, double-instrumenting on every subsequent spawn.
fn already_carries_our_settings_flag(command: &str) -> bool {
    let mut rest = command;
    while let Some(idx) = rest.find("--settings") {
        let before_ok = idx == 0
            || rest[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        let after = &rest[idx + "--settings".len()..];
        let flag_is_whole_token = after.chars().next().is_none_or(char::is_whitespace);
        if before_ok
            && flag_is_whole_token
            && let Some((path_tok, _)) = next_shell_token(after.trim_start())
            && Path::new(&path_tok).file_name().and_then(|f| f.to_str()) == Some(SETTINGS_FILE_NAME)
        {
            return true;
        }
        rest = after;
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
        shell_quote(&settings.display().to_string())
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
        assert_eq!(cmd, "'/opt/bin/lazywingman' hook Stop");
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
            Some("'/opt/bin/lazywingman' --socket lwtest hook Stop")
        );
    }

    #[test]
    fn instrument_inserts_the_flag_after_the_binary_not_at_the_end() {
        let p = std::path::Path::new("/s/lazywingman-activity-hooks.json");
        assert_eq!(
            instrument("claude", p),
            "claude --settings '/s/lazywingman-activity-hooks.json'"
        );
        // A positional prompt must stay last.
        assert_eq!(
            instrument("claude --resume", p),
            "claude --settings '/s/lazywingman-activity-hooks.json' --resume"
        );
        assert_eq!(
            instrument("/usr/local/bin/claude -c", p),
            "/usr/local/bin/claude --settings '/s/lazywingman-activity-hooks.json' -c"
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
            r#"claude --settings '/s/lazywingman-activity-hooks.json' "explain what lazywingman-activity-hooks.json is for""#
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
            "claude --settings '/s/lazywingman-activity-hooks.json' \"line one    line two\""
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
            "./claude --settings '/s/lazywingman-activity-hooks.json'"
        );
        assert_eq!(
            instrument("/usr/local/bin/claude/", p),
            "/usr/local/bin/claude/ --settings '/s/lazywingman-activity-hooks.json'"
        );
    }

    #[test]
    fn hook_ignores_unknown_events_without_touching_tmux() {
        // No tmux server needed: an Ignore action must not shell out. Built
        // via `testutil::tmux_standalone` rather than `Tmux::detect()` so
        // this test never reads TMUX/TMUX_PANE/LAZYWINGMAN_TMUX_SOCKET —
        // other tests mutate those outside TMUX_TEST_LOCK, and this test
        // was the one place in the suite reading them unguarded.
        let tmux = crate::tmux::testutil::tmux_standalone("lwtest-hooks-ignore");
        let r = run_hook(
            "PreToolUse",
            "%nonexistent",
            &tmux,
            &serde_json::Value::Null,
        );
        assert!(r.is_ok(), "ignored events must be a no-op, got {r:?}");
    }

    /// A settings path with a space (e.g. `$HOME` contains one) must
    /// round-trip through `instrument` as a single shell token — unquoted,
    /// Claude would see `--settings /home/my` plus a stray positional
    /// argument, which is worse than pre-M17, where the plain command
    /// simply worked.
    #[test]
    fn instrument_quotes_a_settings_path_containing_a_space() {
        let p = Path::new("/home/my user/.local/state/lazywingman/lazywingman-activity-hooks.json");
        assert_eq!(
            instrument("claude", p),
            "claude --settings '/home/my user/.local/state/lazywingman/lazywingman-activity-hooks.json'"
        );
    }

    /// The exe path interpolated into the generated hook `command` string
    /// must also be a single shell token once Claude runs it through a
    /// shell — otherwise a space in the binary's own path (e.g. inside
    /// `$XDG_STATE_HOME`) breaks every hook invocation silently.
    #[test]
    fn settings_json_quotes_an_exe_path_containing_a_space() {
        let s = settings_json("/opt/my bin/lazywingman", None);
        let v: serde_json::Value = serde_json::from_str(&s).expect("valid json");
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str(),
            Some("'/opt/my bin/lazywingman' hook Stop")
        );
    }

    /// The idempotence guard must still recognise an already-instrumented
    /// command once its settings path is quoted — otherwise every spawn
    /// after the first would double-instrument.
    #[test]
    fn idempotence_guard_recognises_a_quoted_already_instrumented_command() {
        let p = Path::new("/home/my user/.local/state/lazywingman/lazywingman-activity-hooks.json");
        let once = instrument("claude", p);
        assert_eq!(
            instrument(&once, p),
            once,
            "must not double-instrument a quoted settings path"
        );
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
