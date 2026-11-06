//! Agent hook instrumentation — Claude Code and Codex (see
//! `docs/agent-status.md`).
//!
//! Both CLIs run a command per lifecycle event with the same JSON on stdin
//! and the agent's own `TMUX_PANE` in the environment, so one
//! `wingmux hook <Event>` subcommand serves both and the event names
//! mean the same thing in each. Only the way the hooks are *installed*
//! differs: Claude takes a settings file, Codex takes `-c` overrides.

use crate::status::Status;
use std::path::{Path, PathBuf};

/// tmux pane user option carrying the agent's self-reported state.
pub const OVERLAY_KEY: &str = "@wm_agent";

/// What a hook wrote onto a pane: a state plus when it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overlay {
    pub status: Status,
    /// Unix seconds. Recorded for age-out and time-in-state display; two
    /// staleness guards consume it today — the 60-second age-out and the
    /// login-shell check, both in `status::classify`.
    pub at: u64,
}

impl Overlay {
    pub fn encode(&self) -> String {
        format!("{}:{}", self.status.label(), self.at)
    }

    /// Parse a value read back from tmux. Only the three states a hook can
    /// emit are accepted: `dead`, `failed`, `ended` and `idle` are
    /// Wingmux's own judgements and must never arrive over the wire.
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
        // Codex's dedicated "the agent is asking" event. Claude has no
        // equivalent — there it is a `Notification` whose message has to be
        // read (below) — so this arm only ever fires for Codex.
        "PermissionRequest" => Action::Set(Status::Blocked),
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
pub const SETTINGS_FILE_NAME: &str = "wingmux-activity-hooks.json";

/// Claude events we subscribe to. `PreToolUse`, `SubagentStop` and
/// `PreCompact` are deliberately absent: they add nothing the three states
/// express, and tool hooks are the highest-frequency events.
const SUBSCRIBED: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "Notification",
    "Stop",
    "SessionEnd",
];

/// Codex events we subscribe to (verified against codex-cli 0.147's own
/// hook review screen). The same six states as Claude with one swap:
/// Codex has no `Notification`, and instead fires `PermissionRequest`
/// exactly when it is blocked on the user — which is the thing Claude's
/// notification text has to be sniffed for. `PreToolUse` stays out for the
/// reason it does above.
const SUBSCRIBED_CODEX: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PermissionRequest",
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
        Some(s) => format!(" --socket {}", shell_quote(s)),
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
                    // Without this, Claude falls back to its own 60s
                    // default per hook, and a hung tmux server would stall
                    // the agent's own turn for up to a minute.
                    "timeout": 5,
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
    let socket = std::env::var("WINGMUX_TMUX_SOCKET")
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
/// `instrument` always single-quotes the settings path it inserts, so a
/// naive `split_whitespace` would see only the first fragment of a path
/// containing whitespace and miss the match, double-instrumenting on every
/// subsequent spawn.
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
    let Some(split) = split_binary(command, "claude") else {
        return command.to_string();
    };
    if already_carries_our_settings_flag(command) {
        return command.to_string();
    }
    split.with_args(&format!(
        "--settings {}",
        shell_quote(&settings.display().to_string())
    ))
}

/// A command split around its first token, so flags can be inserted after
/// the binary without re-tokenising the rest (see [`instrument`]).
struct Split<'a> {
    leading: &'a str,
    bin: &'a str,
    remainder: &'a str,
}

impl Split<'_> {
    fn with_args(&self, args: &str) -> String {
        format!("{}{} {args}{}", self.leading, self.bin, self.remainder)
    }
}

/// Split `command` when its first token's file name is exactly `name`.
/// Deliberately narrow: wrapper scripts and aliases are not instrumented
/// and fall back to the regex heuristics.
fn split_binary<'a>(command: &'a str, name: &str) -> Option<Split<'a>> {
    let bin_start = command.find(|c: char| !c.is_whitespace())?;
    let after_bin_start = &command[bin_start..];
    let bin_len = after_bin_start
        .find(char::is_whitespace)
        .unwrap_or(after_bin_start.len());
    let bin = &after_bin_start[..bin_len];
    if Path::new(bin).file_name().and_then(|f| f.to_str()) != Some(name) {
        return None;
    }
    Some(Split {
        leading: &command[..bin_start],
        bin,
        remainder: &command[bin_start + bin_len..],
    })
}

/// [`shell_quote`], but only when the token would otherwise be split or
/// reinterpreted. The Codex overrides embed the hook command inside a TOML
/// string inside a shell word, so quoting a plain `/home/u/bin/wingmux`
/// that needs none turns into `'\''…'\''` in the pane's visible command
/// line — three levels of escaping for nothing.
fn shell_quote_if_needed(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        s.to_string()
    } else {
        shell_quote(s)
    }
}

/// Escape a string for a TOML basic (double-quoted) string.
fn toml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Seconds a Codex hook may take. Codex caps `SessionEnd` at 3 and prints
/// `clamping SessionEnd hook timeout` into the pane for anything higher —
/// a warning about our own instrumentation, in the user's agent pane, is
/// worse than the two seconds it buys.
const CODEX_HOOK_TIMEOUT: u32 = 3;

/// One `-c hooks.<Event>=…` override per subscribed event. Codex has no
/// settings-file flag: `-c` is the whole override surface, so the hooks go
/// on the command line, which also keeps them visible in the pane exactly
/// as Claude's `--settings` is.
///
/// One override per event, never a single `-c hooks=…`: that would replace
/// the whole `[hooks]` table, taking `hooks.state` — where Codex records
/// which hooks the user has trusted — with it, and every session would ask
/// again. Per-event overrides are *added* to whatever the user's own
/// `~/.codex/config.toml` defines for that event rather than replacing it
/// (Codex treats the CLI overrides as their own hook source), so nobody's
/// own hooks are lost.
pub fn codex_overrides(exe: &str, socket: Option<&str>) -> Vec<String> {
    let sock = match socket {
        Some(s) => format!(" --socket {}", shell_quote_if_needed(s)),
        None => String::new(),
    };
    let exe = shell_quote_if_needed(exe);
    SUBSCRIBED_CODEX
        .iter()
        .map(|event| {
            let cmd = toml_quote(&format!("{exe}{sock} hook {event}"));
            format!("-c {}", shell_quote(&format!(
                "hooks.{event}=[{{hooks=[{{type=\"command\",command={cmd},timeout={CODEX_HOOK_TIMEOUT}}}]}}]"
            )))
        })
        .collect()
}

/// Marker proving a command already carries our Codex overrides. The event
/// name is part of it, so a user's own unrelated `-c hooks.…` override is
/// not mistaken for ours.
fn already_carries_our_codex_overrides(command: &str) -> bool {
    command.contains("hooks.SessionStart=") && command.contains(" hook SessionStart")
}

/// Add the hook overrides to a command, if and only if it is recognisably
/// Codex. Like [`instrument`], the flags go immediately after the binary
/// token: `codex` takes a free-text positional prompt too.
pub fn instrument_codex(command: &str, exe: &str, socket: Option<&str>) -> String {
    let Some(split) = split_binary(command, "codex") else {
        return command.to_string();
    };
    if already_carries_our_codex_overrides(command) {
        return command.to_string();
    }
    split.with_args(&codex_overrides(exe, socket).join(" "))
}

/// What instrumentation needs to rewrite a spawn command, resolved once at
/// startup: the generated Claude settings file, plus this binary and the
/// tmux socket the Codex overrides name inline.
#[derive(Debug, Clone)]
pub struct Instrumentation {
    settings: PathBuf,
    exe: String,
    socket: Option<String>,
}

impl Instrumentation {
    /// Write the Claude settings file and record what the Codex overrides
    /// need. Fails only when the settings file cannot be written — the one
    /// part with a filesystem to disagree with.
    pub fn prepare() -> crate::error::Result<Instrumentation> {
        let exe = std::env::current_exe()
            .map_err(|e| crate::error::Error::Config(format!("cannot resolve own path: {e}")))?;
        let socket = std::env::var("WINGMUX_TMUX_SOCKET")
            .ok()
            .filter(|s| !s.is_empty());
        Ok(Instrumentation {
            settings: write_settings_file()?,
            exe: exe.to_string_lossy().into_owned(),
            socket,
        })
    }

    /// The same struct without touching the filesystem, for tests.
    #[cfg(test)]
    pub fn for_test(settings: &str, exe: &str, socket: Option<&str>) -> Instrumentation {
        Instrumentation {
            settings: PathBuf::from(settings),
            exe: exe.to_string(),
            socket: socket.map(str::to_string),
        }
    }

    /// The settings file, for the startup log line that discloses it.
    pub fn settings(&self) -> &Path {
        &self.settings
    }

    /// Instrument a spawn command for whichever agent it names, or hand it
    /// back untouched when it names neither.
    pub fn apply(&self, command: &str) -> String {
        let claude = instrument(command, &self.settings);
        if claude != command {
            return claude;
        }
        instrument_codex(command, &self.exe, self.socket.as_deref())
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

    #[test]
    fn settings_json_registers_exactly_the_subscribed_events() {
        let s = settings_json("/opt/bin/wingmux", None);
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
        assert_eq!(cmd, "'/opt/bin/wingmux' hook Stop");
        assert_eq!(
            hooks["Stop"][0]["hooks"][0]["type"].as_str(),
            Some("command")
        );
    }

    /// Without an explicit timeout, Claude falls back to its own 60s
    /// default per hook — a hung tmux server would stall the agent's own
    /// turn for up to a minute. Every generated entry gets a short timeout
    /// instead.
    #[test]
    fn settings_json_entries_carry_a_short_timeout() {
        let s = settings_json("/opt/bin/wingmux", None);
        let v: serde_json::Value = serde_json::from_str(&s).expect("valid json");
        let hooks = v["hooks"].as_object().expect("hooks object");
        for (event, entry) in hooks {
            assert_eq!(
                entry[0]["hooks"][0]["timeout"].as_i64(),
                Some(5),
                "{event} hook entry must carry timeout: 5"
            );
        }
    }

    #[test]
    fn settings_json_passes_the_socket_through() {
        let s = settings_json("/opt/bin/wingmux", Some("wmtest"));
        let v: serde_json::Value = serde_json::from_str(&s).expect("valid json");
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str(),
            Some("'/opt/bin/wingmux' --socket 'wmtest' hook Stop")
        );
    }

    /// `WINGMUX_TMUX_SOCKET` is user-controlled; a socket name
    /// containing a space must not split into two argv tokens once Claude
    /// runs the generated command through a shell.
    #[test]
    fn settings_json_quotes_a_socket_containing_a_space() {
        let s = settings_json("/opt/bin/wingmux", Some("my socket"));
        let v: serde_json::Value = serde_json::from_str(&s).expect("valid json");
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str(),
            Some("'/opt/bin/wingmux' --socket 'my socket' hook Stop")
        );
    }

    #[test]
    fn instrument_inserts_the_flag_after_the_binary_not_at_the_end() {
        let p = std::path::Path::new("/s/wingmux-activity-hooks.json");
        assert_eq!(
            instrument("claude", p),
            "claude --settings '/s/wingmux-activity-hooks.json'"
        );
        // A positional prompt must stay last.
        assert_eq!(
            instrument("claude --resume", p),
            "claude --settings '/s/wingmux-activity-hooks.json' --resume"
        );
        assert_eq!(
            instrument("/usr/local/bin/claude -c", p),
            "/usr/local/bin/claude --settings '/s/wingmux-activity-hooks.json' -c"
        );
    }

    /// Anything we do not positively recognise as Claude is left alone —
    /// injecting flags into a command we do not understand is worse than
    /// falling back to the heuristics.
    #[test]
    fn non_claude_commands_are_left_untouched() {
        let p = std::path::Path::new("/s/wingmux-activity-hooks.json");
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

    /// Codex takes its hooks as `-c` overrides — one per event, never a
    /// single `-c hooks=…`, which would replace the `[hooks]` table and
    /// take `hooks.state` (Codex's record of what the user has trusted)
    /// with it.
    #[test]
    fn codex_overrides_are_one_per_event_and_never_touch_the_hooks_table() {
        let flags = codex_overrides("/bin/wingmux", None);
        assert_eq!(flags.len(), SUBSCRIBED_CODEX.len());
        for (flag, event) in flags.iter().zip(SUBSCRIBED_CODEX) {
            assert!(
                flag.starts_with(&format!("-c 'hooks.{event}=[")),
                "one override per event: {flag}"
            );
            assert!(
                flag.contains(&format!("/bin/wingmux hook {event}")),
                "each runs its own event: {flag}"
            );
            assert!(
                flag.contains("timeout=3"),
                "a hung tmux must not stall the agent's turn: {flag}"
            );
        }
        assert!(
            !flags.iter().any(|f| f.starts_with("-c 'hooks=")),
            "the whole [hooks] table is never overridden"
        );
    }

    /// The socket is named inline, like Claude's generated file names it.
    #[test]
    fn codex_overrides_carry_the_socket() {
        let flags = codex_overrides("/bin/wingmux", Some("wmtest"));
        assert!(flags[0].contains("/bin/wingmux --socket wmtest hook SessionStart"));
    }

    /// A path needing no quoting is not quoted: the overrides are read by
    /// whoever looks at the pane's command line, and `'\''…'\''` for a
    /// plain path is three levels of escaping for nothing. One that does
    /// need it still gets it.
    #[test]
    fn codex_overrides_quote_only_what_needs_quoting() {
        let plain = codex_overrides("/home/u/.local/bin/wingmux", None);
        assert!(plain[0].contains("command=\"/home/u/.local/bin/wingmux hook SessionStart\""));
        let spaced = codex_overrides("/opt/my agents/wingmux", None);
        assert!(
            spaced[0].contains(r"'\''/opt/my agents/wingmux'\''"),
            "a path with a space is still quoted: {}",
            spaced[0]
        );
    }

    /// The flags go after the binary, before Codex's own arguments — it
    /// takes a free-text positional prompt, like Claude.
    #[test]
    fn codex_flags_go_after_the_binary_and_the_rest_is_copied_verbatim() {
        let out = instrument_codex("/usr/bin/codex  resume  --last", "/bin/wm", None);
        assert!(out.starts_with("/usr/bin/codex -c 'hooks.SessionStart="));
        assert!(
            out.ends_with("  resume  --last"),
            "the remainder is byte-for-byte: {out}"
        );
    }

    /// Recognition is as narrow as Claude's, and instrumenting twice is a
    /// no-op — a resumed command that already carries the overrides must
    /// not collect a second set.
    #[test]
    fn only_codex_is_instrumented_and_only_once() {
        for cmd in ["claude", "codex-wrapper", "notcodex", "my-wrapper.sh", ""] {
            assert_eq!(instrument_codex(cmd, "/bin/wm", None), cmd);
        }
        let once = instrument_codex("codex", "/bin/wm", None);
        assert_eq!(instrument_codex(&once, "/bin/wm", None), once);
    }

    /// One `apply` covers both agents, each getting the shape its CLI
    /// takes, and anything else is handed on untouched.
    #[test]
    fn apply_dispatches_on_the_agent() {
        let i = Instrumentation::for_test("/s/wingmux-activity-hooks.json", "/bin/wm", None);
        assert_eq!(
            i.apply("claude --resume"),
            "claude --settings '/s/wingmux-activity-hooks.json' --resume"
        );
        let codex = i.apply("codex");
        assert!(codex.starts_with("codex -c 'hooks.SessionStart="));
        assert!(!codex.contains("--settings"), "Codex has no settings flag");
        assert_eq!(i.apply("bash -l"), "bash -l");
    }

    /// Codex reports being blocked on the user with an event of its own,
    /// where Claude only has a `Notification` whose text has to be read.
    #[test]
    fn a_permission_request_blocks() {
        assert_eq!(
            action_for("PermissionRequest", &serde_json::Value::Null),
            Action::Set(Status::Blocked)
        );
    }

    /// The events Codex sends and the ones Claude sends mean the same
    /// things, so one mapping serves both — asserted for the four Codex
    /// events that are not Claude's own subscription.
    #[test]
    fn the_codex_lifecycle_maps_to_the_same_three_states() {
        let null = serde_json::Value::Null;
        for (event, want) in [
            ("SessionStart", Action::Set(Status::Waiting)),
            ("UserPromptSubmit", Action::Set(Status::Working)),
            ("PostToolUse", Action::Set(Status::Working)),
            ("PermissionRequest", Action::Set(Status::Blocked)),
            ("Stop", Action::Set(Status::Waiting)),
            ("SessionEnd", Action::Clear),
        ] {
            assert_eq!(action_for(event, &null), want, "{event}");
        }
    }

    #[test]
    fn already_instrumented_commands_are_not_instrumented_twice() {
        let p = std::path::Path::new("/s/wingmux-activity-hooks.json");
        let once = instrument("claude", p);
        assert_eq!(instrument(&once, p), once);
    }

    /// A command that literally already carries our `--settings <path>`
    /// pair must be left alone, however that path was spelled — this is
    /// the idempotence guard the substring check was protecting, now
    /// exercised directly rather than only via a self-produced string.
    #[test]
    fn genuinely_already_instrumented_command_is_unchanged() {
        let p = std::path::Path::new("/s/wingmux-activity-hooks.json");
        let cmd = "claude --settings /elsewhere/wingmux-activity-hooks.json --resume";
        assert_eq!(instrument(cmd, p), cmd);
    }

    /// A prompt that merely *mentions* the settings file name (e.g. the
    /// user asked Claude to explain the file) must NOT be mistaken for an
    /// already-instrumented command — a raw substring test over the whole
    /// command string would wrongly skip instrumentation here, silently
    /// defeating the milestone's core promise that hooks always fire.
    #[test]
    fn command_merely_mentioning_the_filename_still_gets_instrumented() {
        let p = std::path::Path::new("/s/wingmux-activity-hooks.json");
        let cmd = r#"claude "explain what wingmux-activity-hooks.json is for""#;
        assert_eq!(
            instrument(cmd, p),
            r#"claude --settings '/s/wingmux-activity-hooks.json' "explain what wingmux-activity-hooks.json is for""#
        );
    }

    /// `claude` takes a free-text positional prompt. Internal spacing
    /// (deliberate formatting, code, ASCII layout) inside that prompt must
    /// survive byte for byte — re-tokenising with `split_whitespace` and
    /// rejoining with a single space would silently collapse it.
    #[test]
    fn internal_spacing_in_the_prompt_is_preserved_byte_for_byte() {
        let p = std::path::Path::new("/s/wingmux-activity-hooks.json");
        let cmd = "claude \"line one    line two\"";
        assert_eq!(
            instrument(cmd, p),
            "claude --settings '/s/wingmux-activity-hooks.json' \"line one    line two\""
        );
    }

    /// Recognition edge cases the review flagged as untested: a
    /// whitespace-only command has no first token at all (left untouched);
    /// `./claude` and a trailing-slash path both resolve, via
    /// `Path::file_name`, to a final component of exactly `claude`, so both
    /// are recognised and instrumented.
    #[test]
    fn recognition_edge_cases() {
        let p = std::path::Path::new("/s/wingmux-activity-hooks.json");
        assert_eq!(instrument("   ", p), "   ");
        assert_eq!(
            instrument("./claude", p),
            "./claude --settings '/s/wingmux-activity-hooks.json'"
        );
        assert_eq!(
            instrument("/usr/local/bin/claude/", p),
            "/usr/local/bin/claude/ --settings '/s/wingmux-activity-hooks.json'"
        );
    }

    #[test]
    fn hook_ignores_unknown_events_without_touching_tmux() {
        // No tmux server needed: an Ignore action must not shell out. Built
        // via `testutil::tmux_inside` rather than `Tmux::detect()` so
        // this test never reads TMUX/TMUX_PANE/WINGMUX_TMUX_SOCKET —
        // other tests mutate those outside TMUX_TEST_LOCK, and this test
        // was the one place in the suite reading them unguarded.
        let tmux = crate::tmux::testutil::tmux_inside("wmtest-hooks-ignore");
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
        let p = Path::new("/home/my user/.local/state/wingmux/wingmux-activity-hooks.json");
        assert_eq!(
            instrument("claude", p),
            "claude --settings '/home/my user/.local/state/wingmux/wingmux-activity-hooks.json'"
        );
    }

    /// The exe path interpolated into the generated hook `command` string
    /// must also be a single shell token once Claude runs it through a
    /// shell — otherwise a space in the binary's own path (e.g. inside
    /// `$XDG_STATE_HOME`) breaks every hook invocation silently.
    #[test]
    fn settings_json_quotes_an_exe_path_containing_a_space() {
        let s = settings_json("/opt/my bin/wingmux", None);
        let v: serde_json::Value = serde_json::from_str(&s).expect("valid json");
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"].as_str(),
            Some("'/opt/my bin/wingmux' hook Stop")
        );
    }

    /// The idempotence guard must still recognise an already-instrumented
    /// command once its settings path is quoted — otherwise every spawn
    /// after the first would double-instrument.
    #[test]
    fn idempotence_guard_recognises_a_quoted_already_instrumented_command() {
        let p = Path::new("/home/my user/.local/state/wingmux/wingmux-activity-hooks.json");
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
