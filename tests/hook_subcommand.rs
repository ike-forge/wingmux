//! Process-level test for the `lazywingman hook <Event>` subcommand.
//!
//! `PostToolUse` fires once per tool call, so a normal Claude session runs
//! this subcommand hundreds of times. `main::real_main` used to call
//! `logging::init()` (and log a "starting" banner) *before* parsing argv, so
//! every one of those invocations appended to the single unrotated
//! `lazywingman.log` — spamming the one diagnostic channel AGENTS.md
//! mandates (stdout is forbidden). Fixed by parsing argv and dispatching the
//! `hook` subcommand before `logging::init()` ever runs.
//!
//! This has to be a real subprocess: `log::set_boxed_logger` can only
//! succeed once per process, so an in-process unit test cannot observe
//! "was the global logger initialised" without interfering with every other
//! test in the same binary. An integration test target is also the only
//! place `CARGO_BIN_EXE_lazywingman` is available — Cargo does not set it
//! for the crate's own unit tests.

use std::process::{Command, Stdio};

#[test]
fn hook_subcommand_never_initialises_logging() {
    let state_dir = tempfile::tempdir().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_lazywingman");
    let status = Command::new(bin)
        .args(["hook", "SessionStart"])
        .env("XDG_STATE_HOME", state_dir.path())
        // Not inside tmux: the hook subcommand must still exit 0 without
        // touching the log, whether or not there is a pane to report to.
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("LAZYWINGMAN_TMUX_SOCKET")
        .stdin(Stdio::null())
        .status()
        .expect("run hook subcommand");
    assert!(status.success(), "hook subcommand must exit 0");
    let log_path = state_dir.path().join("lazywingman").join("lazywingman.log");
    assert!(
        !log_path.exists(),
        "hook subcommand must never initialise logging (found {})",
        log_path.display()
    );
}

/// `--help` must keep working as an argv-only decision, unaffected by the
/// reordering: it must not touch the log either (it exits before any TUI
/// setup) and must exit 0.
#[test]
fn help_never_initialises_logging_either() {
    let state_dir = tempfile::tempdir().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_lazywingman");
    let status = Command::new(bin)
        .arg("--help")
        .env("XDG_STATE_HOME", state_dir.path())
        .stdin(Stdio::null())
        .status()
        .expect("run --help");
    assert!(status.success(), "--help must exit 0");
    let log_path = state_dir.path().join("lazywingman").join("lazywingman.log");
    assert!(!log_path.exists(), "--help must not initialise logging");
}

/// An unknown argument must still be rejected with a non-zero exit and a
/// stderr message, exactly as before the reordering — `parse_cli` runs
/// first either way, so this is unaffected by where `logging::init` moved
/// to, but it is worth pinning down explicitly.
#[test]
fn unknown_argument_still_fails_the_same_way() {
    let state_dir = tempfile::tempdir().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_lazywingman");
    let out = Command::new(bin)
        .arg("--totally-bogus-flag")
        .env("XDG_STATE_HOME", state_dir.path())
        .stdin(Stdio::null())
        .output()
        .expect("run with a bogus flag");
    assert!(!out.status.success(), "an unknown argument must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown argument"),
        "stderr should explain the failure, got: {stderr}"
    );
}
