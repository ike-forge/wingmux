//! LazyWingman — a control centre for supervising coding agents.
//!
//! Agent shells live in dedicated tmux sessions
//! (`wingman-session-<project>`) that LazyWingman spawns, polls, and tears
//! down — never in the user's own session. The dashboard itself runs either
//! inside tmux (as a pane of the user's session) or standalone in a plain
//! terminal; both are supported.

mod ansi;
mod app;
mod config;
mod discovery;
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
         \n\
           --config <path>   use this file as the user config (default:\n\
                           $XDG_CONFIG_HOME/lazywingman/config.toml; LW_CONFIG\n\
                           env var is the same override, --config wins)\n\
         --config-dump      print the effective config with the layer each\n\
                           key came from, then exit (no TUI)\n\
         hook <Event>       internal: apply one Claude Code hook event to\n\
                           this pane's status (used by the generated\n\
                           lazywingman-activity-hooks.json; not for manual use)\n\
         -h, --help         show this help"
    );
}

fn real_main() -> Result<()> {
    if let Err(e) = logging::init() {
        eprintln!("lazywingman: warning: cannot open log file: {e}");
    }
    log::info!("lazywingman starting (pid {})", std::process::id());

    let cli = parse_cli()?;
    if let Some(event) = cli.hook_event {
        return run_hook_subcommand(&event, cli.socket);
    }
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
        "config loaded: theme={} (config '{}') poll={}ms projects={} truecolor={}",
        theme.name,
        cfg.theme,
        cfg.poll_interval_ms,
        cfg.projects.len(),
        truecolor
    );
    log::info!("theme '{}' resolved", theme.name);
    for d in &cfg.diagnostics {
        log::warn!("config diagnostic: {d}");
    }
    let tmux = tmux::Tmux::detect()?;
    match &tmux.inside {
        Some(i) => log::info!(
            "dashboard inside tmux: session='{}' window={} pane={}",
            i.session_name,
            i.window_id,
            i.pane_id
        ),
        None => log::info!("dashboard standalone (no tmux around it)"),
    }
    app::run(cfg, theme, tmux)
}

/// `lazywingman hook <Event>`: called by Claude Code, once per lifecycle
/// event, in the agent's own pane. Must be fast and must never write to
/// stdout — Claude reads hook stdout as protocol. Any failure is swallowed:
/// a hook that errors loudly interrupts the agent it observes.
fn run_hook_subcommand(event: &str, socket: Option<String>) -> Result<()> {
    if let Some(s) = socket {
        // Tmux::detect reads this; setting it keeps socket handling in one
        // place rather than threading an override through the API.
        // SAFETY: this call happens before any thread is spawned (we are
        // still on the single-threaded startup path: only `logging::init`
        // and a `log::info!` have run before it, neither of which spawns a
        // thread), which is the documented safety condition for
        // `std::env::set_var`.
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
