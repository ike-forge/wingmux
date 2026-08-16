# LazyWingman — design decisions

Every decision not fixed by the spec, one line. Appended as decisions are made.

## Tooling / environment
- Rust toolchain: rustup minimal stable (container had none). tmux: Debian
  packages extracted into `~/.local` (no root). Both verified working.
- Delivery: Telegram send is impossible from this TUI session (no
  send_message tool) → zip lands in `/opt/data/`, path reported in final
  message and STATE.md.
- **Continuation ownership**: this build runs multi-day under a rolling
  token window. A Hermes cron job named "LazyWingman continuation"
  (job 4807eb670450, every 6h) OWNS resumption: it reads STATE.md, continues
  the current milestone, updates STATE.md, commits to git. If this TUI
  session is still alive the cron stays silent and lets it work — do NOT
  create duplicate schedulers or long-running loops; just keep STATE.md
  current so any tick can resume cleanly. When finished, write a
  completion marker at the top of STATE.md ("COMPLETE") so the cron goes
  permanently silent.

## Architecture
- Single binary crate, no lib.rs; modules: `error`, `util`, `logging`,
  `config`, `theme`, `git`, `tmux`, `state`, `status`, `app`, `ui`.
- No async runtime: single-threaded crossterm event loop, status polling on a
  timer inside the loop (tmux CLI calls are fast enough at 500ms cadence).
- State file: JSON at `$XDG_STATE_HOME/lazywingman/state.json` (state, not
  data, is the semantically correct XDG dir for mutable runtime state).
- Status model: `dead | ended | failed | waiting | working | idle`.
  `dead` = pane_dead; `ended` = pane's foreground command is a login shell
  (agent exited); failed/waiting/working = regex on `capture-pane` tail;
  `idle` = default.
- Feature deletion: kill windows → `git worktree remove` (with `--force`
  offered in the confirm dialog when dirty) → drop from state. Branch kept
  unless `worktrees.delete_branch = true`.
- Worktree root default: `<parent of repo>/.lazywingman-worktrees/<project>/<feature>`.
- Keybindings: hardcoded vim-ish set (documented in help overlay + README);
  only the tmux back-key is configurable (cheap win, avoids clashing with
  user's tmux bindings).
- Themes: TOML files in `themes/`, three built-ins (dracula, gruvbox-dark,
  ansi) embedded via `include_str!` and copied to the config dir on first run
  so the user can edit them. Truecolor when `COLORTERM` says so, else 24-bit
  values quantize to xterm-256; `ansi.toml` uses only ANSI-16 colours.
- ratatui 0.30 was used (the crate was rewritten around ratatui-core /
  ratatui-widgets / ratatui-crossterm): Color::Rgb is a tuple, 256-colour is
  Color::Indexed, ANSI colors render as `38;5;0..15` through the crossterm
  backend (still the terminal palette).
- Status classification order settled empirically during acceptance:
  dead > failed > working > waiting(last-line-priority) > ended > idle;
  tail is checked before the shell-foreground heuristic because agents are
  often launched FROM a shell.
- **M8 — agent shells move to dedicated tmux sessions** (`wingman-session-<proj>`),
  per user feedback ("do not pollute my tmux session"): creating a feature /
  adding a shell must not add windows to the user's session. One session per
  project, named `wingman-session-<sanitized-name>` (+ `-<hash8>` suffix on
  collision), windows inside named `lw:<feature>/<n>`; strict naming is what
  makes rediscovery after a crash/close possible (`list-sessions` + per-session
  `list-windows`). Killing the feature's last window auto-destroys the session
  (verified against tmux 3.5a).
- **M8 — attach/back across sessions**: inside tmux, attach = `switch-client -t
  <agent-session>` + `select-window -t @<shell-window>` (window IDs are
  server-global; clientless `select-window -t session:@win` verified to change
  the session's active window). Back-key `C-o` is bound in EVERY agent session
  to `switch-client -t <user-session> \; select-window -t @<dashboard-window>`
  (rebound at startup and at session creation). Supersedes the old
  same-session `select-window` back-key.
- **M8 — runs inside AND outside tmux** (per user feedback): the dashboard no
  longer requires tmux around itself. `Tmux` keeps optional `inside`
  coordinates (detected via TMUX/TMUX_PANE); all session/window/pane operations
  work standalone against the (auto-started) tmux server. Attach from outside
  tmux = temporary terminal handoff: TUI restores the terminal, runs
  `tmux attach -t <agent-session>` (after clientless select-window to the
  shell's window), then re-enters the TUI on detach. No back-key exists outside
  tmux (the user detaches with C-b d). `LAZYWINGMAN_NO_TMUX` is removed —
  there is no no-tmux mode anymore.
- **M8.2 — the tmux server is auto-started** for a standalone dashboard:
  tmux 3.5a exits a server as soon as it has no sessions and no clients, so
  `Tmux::ensure_server()` runs `start-server \; set -g exit-empty off` in ONE
  client invocation (two clients would race the server's exit). Only runs
  when `list-sessions` reports no server, so the user's own server is never
  modified; the dashboard's server then lingers empty after quit (harmless,
  standard tmux-with-exit-empty-off behaviour). `list-panes -a` on a
  zero-session server ("no current target") reads as an empty pane list.
- **M8 — project discovery** (per user feedback): on startup, if the working
  directory is inside a git repo, that repo is auto-imported as a project and
  all its worktrees become features (no shells spawned); if not, the user is
  prompted to scan subfolders recursively for repos. Scan results are cached in
  `$XDG_CACHE_HOME/lazywingman/` (regenerable data — not state). A project whose
  path vanished is kept in state and marked `missing` (shown "(missing)" in the
  tree) instead of being pruned; it re-activates when the path is a repo again.
- **M8 — state schema v2**: `State.version = 2`, `Project.session_name:
  Option<String>`. v1 files load via serde defaults; v1 shells (windows in the
  dashboard session) are not found by session-based reconcile and are pruned
  (worktrees and projects survive).
- **M8 — global instructions conformance** (per user feedback): all new
  bindings follow the agent-common-instructions TUI rules (one key table,
  LazyGit/vim precedent, `docs/shortcuts.md`, palette/bar wording, XDG
  placement); config gains `[discovery]` keys. The remaining guideline gaps
  (fuzzy command palette, Catppuccin built-in themes + theme picker, config
  layers, mouse) are closed in M8.4.
- tmux quirks hit & fixed: `list-panes -t <session>` only lists the active
  window (use `-a` + filter by session_id); `new-window` right after
  `new-session -d` races window index 0 (retry 3x); a fake TMUX env var makes
  tmux behave as a nested client and breaks window creation; `capture-pane`
  pads blank lines.
- Back-key interactive fire could not be simulated in this environment (pty
  byte-injection blocked by host guardrail) — binding registration and
  select-window both verified separately.
