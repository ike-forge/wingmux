//! LazyWingman — a control centre for supervising coding agents.
//!
//! Agent shells live in dedicated tmux sessions
//! (`wingman-session-<repository>`) that LazyWingman spawns, polls, and tears
//! down — never in the user's own session. The dashboard itself is always a
//! tmux pane: launched from a bare shell it execs into a tmux that runs it
//! again (see `should_wrap`), because docking swaps the agent's real pane in
//! beside the tree and a swap needs a pane to swap into.

mod ansi;
mod app;
mod config;
mod discovery;
mod dock;
mod drafts;
mod error;
mod git;
mod hooks;
mod logging;
mod state;
mod status;
mod theme;
mod tmux;
mod ui;
mod util;

use std::io::IsTerminal;
use std::path::PathBuf;

use error::Result;

fn main() {
    if let Err(e) = real_main() {
        // Before the TUI takes over, stderr is still safe.
        eprintln!("lazywingman: {e}");
        std::process::exit(1);
    }
}

/// Command-line options parsed on startup.
#[derive(Debug, Default)]
struct Cli {
    /// `--config <path>`: explicit user-config override (higher than
    /// `LW_CONFIG`, highest of the *files* for the user layer).
    config: Option<PathBuf>,
    /// `--config-dump`: print the effective config + layer provenance and
    /// exit without starting the TUI.
    dump: bool,
    /// `hook <Event>`: apply one Claude hook event to our own pane and exit.
    hook_event: Option<String>,
    /// `--socket <name>`: tmux socket for the hook subcommand, matching the
    /// dashboard's `$LAZYWINGMAN_TMUX_SOCKET`.
    socket: Option<String>,
}

/// Parse CLI args. Returns [`Cli`]; `--help`/`-h` prints usage and exits 0.
fn parse_cli() -> Result<Cli> {
    let mut cli = Cli::default();
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "--config" => {
                let path = args
                    .next()
                    .ok_or_else(|| error::Error::Config("--config requires a path".into()))?;
                cli.config = Some(PathBuf::from(path));
            }
            "--config-dump" => cli.dump = true,
            "--socket" => {
                let s = args
                    .next()
                    .ok_or_else(|| error::Error::Config("--socket requires a name".into()))?;
                cli.socket = Some(s);
            }
            "hook" => {
                let event = args
                    .next()
                    .ok_or_else(|| error::Error::Config("hook requires an event name".into()))?;
                cli.hook_event = Some(event);
            }
            other => {
                return Err(error::Error::Config(format!(
                    "unknown argument '{other}' (try --help)"
                )));
            }
        }
    }
    Ok(cli)
}

fn print_usage() {
    eprintln!(
        "lazywingman — a control centre for supervising coding agents\n\
         \n\
         usage: lazywingman [--config <path>] [--config-dump] [--help]\n\
                          | lazywingman hook <Event> [--socket <name>]\n\
         \n\
           --config <path>   use this file as the user config (default:\n\
                           $XDG_CONFIG_HOME/lazywingman/config.toml; LW_CONFIG\n\
                           env var is the same override, --config wins)\n\
         --config-dump      print the effective config with the layer each\n\
                           key came from, then exit (no TUI)\n\
         hook <Event>       internal: apply one Claude Code hook event to\n\
                           this pane's status (used by the generated\n\
                           lazywingman-activity-hooks.json; not for manual use)\n\
         --socket <name>    tmux socket name for `hook <Event>` to talk to,\n\
                           matching the dashboard's own socket; set\n\
                           automatically in the generated hook commands via\n\
                           $LAZYWINGMAN_TMUX_SOCKET, not meant to be passed\n\
                           by hand\n\
         -h, --help         show this help"
    );
}

fn real_main() -> Result<()> {
    // Argv is parsed, and the `hook` subcommand dispatched, before logging
    // is ever initialised: `PostToolUse` fires once per tool call, so a
    // normal Claude session runs `lazywingman hook <Event>` hundreds of
    // times. Initialising the file logger on that path would append a
    // session banner plus a "starting" line to the single unrotated
    // `lazywingman.log` on every one of those invocations, spamming the one
    // diagnostic channel AGENTS.md mandates. The hook subcommand's own
    // errors already go to stderr (see `run_hook_subcommand`), so nothing
    // is lost by skipping the log on this path.
    let cli = parse_cli()?;
    if let Some(event) = cli.hook_event {
        return run_hook_subcommand(&event, cli.socket);
    }

    if let Err(e) = logging::init() {
        eprintln!("lazywingman: warning: cannot open log file: {e}");
    }
    log::info!("lazywingman starting (pid {})", std::process::id());

    if let Some(ref p) = cli.config {
        log::info!("user config override: {}", p.display());
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // `--config` overrides the user file; without it `load()` honours
    // `LW_CONFIG` and the XDG default.
    let mut cfg = match cli.config {
        Some(ref p) => config::Config::load_from(&cwd, Some(p))?,
        None => config::Config::load()?,
    };

    // `--config-dump`: effective values + which layer each came from.
    if cli.dump {
        print!("{}", cfg.dump_string());
        return Ok(());
    }

    let truecolor = theme::supports_truecolor();
    // A theme picked in the TUI (ctrl-t) is persisted to state.json and
    // wins over the config line; the config file is never rewritten.
    let picked = state::State::load()?.theme;
    let theme_name = picked.as_deref().unwrap_or(&cfg.theme);
    // One bad theme never prevents startup: fall back to the default theme
    // and record a diagnostic for the startup diagnostics overlay.
    let theme = match theme::load(theme_name, truecolor) {
        Ok(t) => t,
        Err(e) => {
            cfg.diagnostics.push(format!(
                "theme '{theme_name}' failed to load ({e}) — using '{}'",
                theme::DEFAULT_THEME
            ));
            theme::load(theme::DEFAULT_THEME, truecolor)?
        }
    };
    log::info!(
        "config loaded: theme={} (config '{}') poll={}ms repositories={} truecolor={}",
        theme.name,
        cfg.theme,
        cfg.poll_interval_ms,
        cfg.repositories.len(),
        truecolor
    );
    log::info!("theme '{}' resolved", theme.name);
    for d in &cfg.diagnostics {
        log::warn!("config diagnostic: {d}");
    }
    let tmux = tmux::Tmux::detect()?;
    if should_wrap(tmux.inside.is_some(), std::io::stdin().is_terminal()) {
        return wrap_into_tmux(&tmux, &cfg, &cwd);
    }
    match &tmux.inside {
        Some(i) => log::info!(
            "dashboard inside tmux: session='{}' window={} pane={}",
            i.session_name,
            i.window_id,
            i.pane_id
        ),
        // Only reachable when there was no terminal to give tmux — a piped
        // or non-interactive launch that `should_wrap` declined to wrap.
        None => log::warn!("no tmux around us and no terminal to start one in"),
    }
    app::run(cfg, theme, tmux)
}

/// Should this process re-launch itself inside tmux?
///
/// LazyWingman docks the agent's *real* tmux pane beside its tree, so it has
/// to BE a tmux pane. Launched from a bare shell it therefore execs into a
/// tmux that runs it again — but only when there is a terminal to hand over:
/// with stdin piped or redirected (automation, a cron entry, a test harness)
/// there is nothing for tmux to attach, and wrapping would fail where not
/// wrapping merely degrades.
///
/// No recursion guard is needed. The tmux we exec into sets `TMUX` and
/// `TMUX_PANE`, which is exactly what makes `inside` `Some` next time round.
fn should_wrap(inside: bool, is_tty: bool) -> bool {
    !inside && is_tty
}

/// Re-launch this process inside tmux. Returns only on failure: on success
/// it `exec`s and this pid becomes the tmux client — one process, so signals
/// and the exit code stay the shell's own, which a spawn-and-wait would
/// swallow a layer deep.
fn wrap_into_tmux(tmux: &tmux::Tmux, cfg: &config::Config, cwd: &std::path::Path) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let scope = app::scope_for(cfg, cwd);
    let exe = std::env::current_exe()
        .map_err(|e| error::Error::Config(format!("cannot locate my own binary: {e}")))?;
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut cmd = tmux.client_command();

    match tmux.find_scope_window(&scope)? {
        // A dashboard is already showing this scope: attach to it instead of
        // racing a second one beside it. Selected clientlessly first, then
        // attached, so the two never have to be chained into one client.
        Some(w) if tmux.has_dashboard_pane(&w.window_id)? => {
            log::info!(
                "a dashboard already shows {} ({} in {}) — attaching",
                scope.display(),
                w.window_id,
                w.session_name
            );
            tmux.select_window(&w.window_id)?;
            cmd.args(["attach-session", "-t", &w.session_name]);
        }
        // The window outlived its dashboard: its slot, and whatever agent
        // pane was docked into it, are still there. Split a fresh dashboard
        // back beside them rather than abandoning panes that never died.
        Some(w) => {
            log::info!(
                "the dashboard of {} is gone — splitting a fresh one back into {}",
                scope.display(),
                w.window_id
            );
            let mut split = tmux.client_command();
            split.args(["split-window", "-t", &w.window_id, "-c"]);
            split.arg(cwd);
            split.arg("--").arg(&exe).args(&argv);
            let out = split
                .output()
                .map_err(|e| error::Error::Tmux(format!("cannot run tmux split-window: {e}")))?;
            if !out.status.success() {
                return Err(error::Error::Tmux(format!(
                    "tmux split-window failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            tmux.select_window(&w.window_id)?;
            cmd.args(["attach-session", "-t", &w.session_name]);
        }
        // Nothing showing this scope anywhere: our own session, created or
        // re-attached by the same `-A` call.
        None => {
            log::info!("no dashboard for {} — starting one", scope.display());
            cmd.args(["new-session", "-A", "-s", WRAPPER_SESSION, "-c"]);
            cmd.arg(cwd);
            cmd.arg("--").arg(&exe).args(&argv);
        }
    }
    // Past here the process is tmux, or the exec failed and we say why.
    let e = cmd.exec();
    Err(error::Error::Tmux(format!("cannot exec tmux: {e}")))
}

/// The session the self-wrap creates when nothing is showing this scope yet.
/// `new-session -A` makes it a re-attach when it already exists, so a second
/// launch in an unrelated folder shares it rather than starting a rival
/// server — `swap-pane` cannot cross tmux servers.
const WRAPPER_SESSION: &str = "lazywingman";

/// `lazywingman hook <Event>`: called by Claude Code, once per lifecycle
/// event, in the agent's own pane. Must be fast and must never write to
/// stdout — Claude reads hook stdout as protocol. Any failure is swallowed:
/// a hook that errors loudly interrupts the agent it observes.
fn run_hook_subcommand(event: &str, socket: Option<String>) -> Result<()> {
    if let Some(s) = socket {
        // Tmux::detect reads this; setting it keeps socket handling in one
        // place rather than threading an override through the API.
        // SAFETY: this call happens before any thread is spawned — hook
        // dispatch runs before `logging::init()` (and thus before any
        // other startup code) on this single-threaded path, so nothing has
        // had a chance to spawn a thread yet, which is the documented
        // safety condition for `std::env::set_var`.
        unsafe { std::env::set_var("LAZYWINGMAN_TMUX_SOCKET", s) };
    }
    let Some(pane_id) = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty()) else {
        // Not inside tmux: nothing to report to. Not an error.
        return Ok(());
    };
    // A hook is never legitimately fed JSON from a terminal — Claude always
    // pipes the payload and closes the pipe. Reading stdin when it is a tty
    // would block forever waiting for an EOF nobody sends, stalling the
    // agent this subcommand exists to observe. So skip the read entirely in
    // that case rather than trust an invocation contract this crate cannot
    // enforce.
    let payload: serde_json::Value = if std::io::stdin().is_terminal() {
        serde_json::Value::Null
    } else {
        serde_json::from_reader(std::io::stdin()).unwrap_or(serde_json::Value::Null)
    };
    // A tmux error here (bad socket, tmux missing, stale pane) must not
    // propagate: this subcommand must exit 0 no matter what, so it never
    // interrupts the agent it observes. Unlike the TUI path, we do not use
    // `?` on `detect()`.
    let tmux = match tmux::Tmux::detect() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lazywingman hook {event}: {e}");
            return Ok(());
        }
    };
    if let Err(e) = hooks::run_hook(event, &pane_id, &tmux, &payload) {
        eprintln!("lazywingman hook {event}: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::should_wrap;

    /// M18 Task 6 — the wrap decision. The `exec` itself is not testable
    /// here (it replaces the test binary); this is the whole of the choice
    /// that leads to it, and manual verification covers the rest.
    #[test]
    fn a_bare_interactive_shell_is_the_only_thing_worth_wrapping() {
        assert!(should_wrap(false, true), "bare shell: wrap into tmux");
        assert!(
            !should_wrap(true, true),
            "already a tmux pane: wrapping again would nest"
        );
        assert!(
            !should_wrap(false, false),
            "no terminal to attach: wrapping would fail where not wrapping degrades"
        );
        assert!(
            !should_wrap(true, false),
            "inside tmux wins regardless of the terminal"
        );
    }
}
