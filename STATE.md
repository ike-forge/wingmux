# LazyWingman — build state

Resume from this file alone. It is updated at the end of every milestone and
whenever a milestone runs long.

## Current milestone

**M7 — Harden & verify — COMPLETE. Project done.**
**M7.1 — Feedback fix (input popup cursor) — COMPLETE.**

## Progress log

### M7.1 — Feedback fix: input popup cursor offset (committed)
- [x] Bug: the block cursor in the input popups (new feature, quick-send,
      import) was drawn one row BELOW the typed text — `cursor_y =
      area.y + 3` while the input line (`lines[1]`, after the border +
      one spacer line) renders at `area.y + 2`. The caret visually
      belonged to the empty spacer row under the text.
- [x] Fix: `inner` rect's `y` corrected to `area.y + 1` (content starts
      below the border) and `cursor_y = inner.y + 1` (the input row).
      One shared code path (`draw_input`) served all three popup kinds,
      so a single fix covers new feature / quick-send / import.
- [x] Regression test `ui::tests::input_popup_cursor_sits_on_input_row`:
      renders all three input kinds headless and asserts the exact cursor
      position. Verified it FAILS on the old offset and passes on the fix.
- [x] Gating green: fmt clean, clippy --all-targets -D warnings,
      51 tests (50 + 1 new).

### M5+M6 — TUI + actions (committed)
- [x] ratatui 0.30 (rewritten crate: ratatui-core/widgets/crossterm split; Color::Rgb is tuple, Indexed not AnsiValue; Layout::vertical/horizontal).
- [x] Layout: left tree (projects/features/shells), right detail + live capture-pane preview, status bar.
- [x] Modes: Normal / Input (import, new feature, quick-send) / Confirm (delete y/n/f) / Help overlay.
- [x] Keys: j/k/h/l/Enter/i/n/a/s/d/x/r/?/q.
- [x] Actions: import (state+config merge), create feature (worktree add + window + agent cmd), add shell, attach (select-window), quick-send (send-keys), delete (kill windows + worktree remove + optional branch delete), remove project (kill windows, keep worktrees).
- [x] Status polling: list-panes -a + capture-pane per shell → classify (dead>ended>failed>waiting>working>idle).
- [x] Persistence: state.json, reconcile+prune on startup/refresh, window rediscovery + feature reconstruction from pane cwd.
- [x] Back-key binding C-o → dashboard window (configurable).
- [x] Smoke test: TUI renders inside tmux, in-tmux detection works.

### M7 — Harden & verify (complete)
- [x] Acceptance loop EXECUTED for real against a live tmux server
      (session 'acc', XDG dirs /opt/data/.lw-accept, demo repos
      /opt/data/lw-demo/repo-{a,b}, simulated agent demo-agent.sh):
  1. Started in a tmux pane ✓
  2. Imported 2 repos: repo-a via 'i' key, repo-b via config merge ✓
  3. Feature on each: worktree on disk + shell spawned with default cmd ✓
  4. Second shell added to a feature (lw:.../1) ✓
  5. Attach: Enter → select-window to agent window ✓; typed a message into
     the agent ✓; back: binding registered (bind-key -n C-o), select-window
     mechanism proven — interactive keypress simulation was blocked by the
     host security guardrail (couldn't inject bytes into the client pty),
     noted as the only unverified micro-step.
  6. Quick-send 'busy 2' from the dashboard ✓ (agent executed it)
  7. Status flipped waiting→working→waiting live as agent went busy/idle ✓
  8. Restart: state + windows rediscovered ✓ (incl. clean quit, LW_EXIT=0)
  9. Delete feature: confirm popup → windows killed, worktree removed,
     tree updated ✓
  10. Theme switch via config + restart: ansi theme uses only palette
      indices 0-15 (terminal palette); dracula used 38;5;NNN up to 231 ✓
- [x] Bugs found & fixed during the loop:
  * centered_rect destructured 3 rects into [v] → panicked on first popup
    (added headless render test for all modes)
  * capture-pane pads blank lines → recent-tail was empty → 'ended' status
    (trim trailing blanks before classifying)
  * banner text in the 10-line window → stale 'working' (last-line-prompt
    priority in classify)
  * shell-foreground agents (bash running a script) were 'ended' before
    their tail was read (tail checks moved before the shell heuristic)
- [x] clippy --all-targets -- -D warnings CLEAN, cargo fmt applied,
      50 tests green (incl. real-tmux integration tests).
- [x] README.md written (install, keybindings, config, themes, tmux model).
- [x] Delivery zip → /opt/data/lazywingman-<date>.zip (Telegram send not
      possible from this TUI session; path reported in final message).

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
