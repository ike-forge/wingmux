# LazyWingman — build state

Resume from this file alone. It is updated at the end of every milestone and
whenever a milestone runs long.

## Current milestone

**M1 — Scaffold** (in progress)

## Progress log

### M1 — Scaffold (in progress)
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
