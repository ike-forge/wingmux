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
- Agent windows live in the SAME tmux session as the dashboard (simplest
  attach/back: `select-window` both ways, trivially rediscoverable). Window
  names `lw:<project>/<feature>/<n>`; targeting always via window IDs
  (`@id`) because `:` in names breaks tmux target parsing.
- "Back to wingman": server-wide `bind-key -n <key> select-window -t @dashboard`
  with default `C-o`, configurable (`tmux.back_key`). Rebound every startup.
- Outside tmux: refuse to run with a clear message, unless
  `LAZYWINGMAN_NO_TMUX=1` (dev/test escape hatch; TUI works, tmux actions are
  disabled/errors).
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
