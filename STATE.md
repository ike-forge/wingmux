# LazyWingman — build state

Resume from this file alone. It is updated at the end of every milestone and
whenever a milestone runs long.

## Current milestone

**M4 — tmux layer** (complete, committed)

## Progress log

### M4 — tmux layer (committed)
- [x] Tmux::detect() (in-tmux detection via TMUX/TMUX_PANE + display-message -t pane).
- [x] Window spawn (new-window -d -P -F), send-keys -l, capture-pane, select-window, kill-window, bind-key -n back-key.
- [x] list-panes -a (NOT -t session — that only lists the active window!) filtered by session_id.
- [x] Deterministic window names lw:<proj>/<feat>/<n>, sanitised.
- [x] Real-tmux integration tests (5), with the pitfalls documented:
  * Fake TMUX env breaks window creation (nested-client behavior) — keep unset in tests.
  * `new-window` right after `new-session -d` races tmux server startup
    ("index 0 in use") — new_window now retries 3x; test harness sleeps 150ms.
  * static Mutex inside a GENERIC fn is per-monomorphization → put at module level.

### M2 — Config + themes (committed)
- [x] XDG config loading, commented defaults written on first run.
- [x] Three built-in themes (dracula, gruvbox, ansi) embedded + copied to themes/.
- [x] Truecolor detection + xterm-256 quantisation fallback; ansi theme uses only ANSI-16.
- [x] Agent presets (claude/codex) with per-agent status regexes; per-project overrides; generic fallback.
- [x] 21 unit tests green (config parse, theme load, colour parse/quantise, path normalisation).
- [x] First-run smoke test: config.toml + themes + log file all written, exit clean.

### M1 — Scaffold (done)
- [x] Rust toolchain installed (`~/.cargo`, rustc 1.97.1) — container had none.
- [x] tmux 3.5a installed as non-root: `apt-get download` + `dpkg -x` into
      `~/.local/tmux-prefix`; run with
      `LD_LIBRARY_PATH=$HOME/.local/tmux-prefix/usr/lib/x86_64-linux-gnu`.
      `tmux -L lwtest` verified working headless. (libevent-core/extra/pthreads,
      libjemalloc2 pulled the same way.)
- [x] Repo at `/opt/data/lazywingman` (git, branch `main`, local user config
      `LazyWingman Builder <builder@lazywingman.local>`).
- [x] Deps: ratatui 0.30.2, crossterm 0.29.0, serde/serde_json, toml 1.1.4,
      thiserror 2.0.20, regex 1.13.1, log 0.4.33.
- [x] Module skeleton: `error.rs` (thiserror enum + context), `util.rs`
      (XDG paths), `logging.rs` (file logger, never stdout), `app.rs` stub,
      `main.rs`.
- [ ] `cargo build` green, M1 commit.

## Gotchas / environment notes

- **PATH**: `~/.cargo/bin` and `~/.local/bin` must be on PATH; tmux needs
  `LD_LIBRARY_PATH=$HOME/.local/tmux-prefix/usr/lib/x86_64-linux-gnu`.
- **TERM**: container TERM=dumb; use `TERM=xterm-256color` for tmux commands.
- **No root**: everything installed into `$HOME/.local` via deb extraction.
- zip: use `python3 -m zipfile -c out.zip dir` (no zip binary).
- User communicates via this TUI session; Telegram send is NOT available from
  a TUI session (no send_message tool). Backup zip will be dropped in
  `/opt/data/` and its path reported in the final message instead.
- Do not print anything to stdout while the TUI is running — it corrupts the
  screen. All diagnostics go to the log file.
