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
- **Run length (user directive)**: consume close to 80–95% of the 5h rolling
  token window per run — unused quota is wasted money. Multiple milestones
  per run; stop only when everything is done (marker written) or the budget
  is spent. Mid-run stop must leave STATE.md precise enough to resume.

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
- **M9 — back-key moves behind the prefix.** The M8 back-key used
  `bind-key -n` (root table), which is server-wide in tmux: it hijacked `C-o`
  for every session and every TUI running inside (vim's `C-o`, readline, …).
  The chord is now bound in the PREFIX table (`bind-key -T prefix <key>`), so
  it only fires after the user's tmux prefix (`C-b C-o` by default) and no
  application ever loses the key. Legacy root bindings are removed on startup,
  but only the exact command we bound — a user's own root binding of the same
  key is untouched. `tmux.back_key` still configures the second half of the
  chord.
- **M9 — quick-send is a multiline textarea.** The main way to talk to
  agents became a full editor (`Mode::Textarea` + `TextBuf`): Enter inserts a
  newline, Tab a real tab, arrows/Home/End move the cursor (wrapping at line
  ends), PgUp/PgDn/ctrl-u/d/f/b page the cursor, and the send key is
  **ctrl-s** (Alt+Enter still works where the terminal delivers it) — a
  plain Enter is needed for editing, so it cannot be the submit key. While the textarea is open the accessibility
  layer yields ALL motion keys to it (only ctrl-c and ctrl-t stay global);
  the tree selection never moves while editing. Delivery switched from
  `send-keys -l` to tmux **paste-buffer with bracketed paste** (`paste_keys`):
  `send-keys -l "a\nb"` would translate the newline into an Enter press and
  submit the first line early; bracketed paste delivers multi-line text as
  one paste, then a trailing Enter submits. Import/new-feature inputs stay
  single-line (`Enter` submits) — they are names, not prompts.
- **M9.6 — the send key is ctrl-s, and the scroll window is the popup's.**
  Two bugs made quick-send unusable in a real tmux. (1) *Nothing could be
  submitted*: both send chords are undeliverable in a default setup — tmux
  ships `extended-keys off`, so **Ctrl+Enter is not encodable at all** and
  arrives as a bare Enter (a newline), while **Alt+Enter** depends on the
  outer terminal emulator forwarding `ESC CR` (Windows Terminal binds it to
  fullscreen). The send key is now **ctrl-s** — a plain control byte, which
  raw mode always delivers; Alt+Enter is kept as a bonus, and Ctrl+Enter is
  still accepted for terminals that do encode it. Lesson: a chord that
  needs the kitty/CSI-u protocol cannot be the ONLY way to perform an
  action. (2) *Only one line was ever visible*: every `TextBuf` edit and
  motion called `ensure_visible(1)` — a hardcoded one-line viewport — so
  each Enter scrolled the previous line out of a 10–18 row popup. `TextBuf`
  no longer scrolls at all; the key handler calls `ensure_visible` once per
  key with the popup's real content height (`textarea_visible_lines`),
  which is the only place that knows it.
- **M9 — discovery is scope-based, live, and lazy.** The tree no longer
  lists EVERY project ever discovered — that mixed scopes. The scope is the
  startup directory (or `[discovery].scan_root`): only projects inside it
  get rows; projects from other scopes stay in state but are hidden. When
  you start INSIDE a git repo, that repo is the scope and its features are
  shown flat at the top level (no project row — `flatten`); a lone MISSING
  project still shows its row. >5 projects collapse by default (≤5 expand,
  so the user sees features immediately). The startup scan prompt and the
  discovered.json cache are GONE: finding `.git` entries is a few stats per
  dir, done live at boot and on `I` — no stale cache to trust, no prompt to
  dismiss, even at hundreds of folders. The expensive part — `git worktree
  list` per project — is lazy (`ensure_worktrees`): it runs ONCE per project
  when the project is expanded (or for the flattened single project at
  boot), and `prune` skips worktree verification for projects whose
  worktrees were never loaded, so boot never pays 200 git calls.
- **M9 — UI pass from the screenshots (rounded corners, bare titles,
  tree guides, icons).** Panels and popups switch to `BorderType::Rounded`
  (╭ ╮ ╰ ╯). Titles render foreground-only — the background fill under
  every panel/popup title is gone ("simple title display without
  background"). The tree now draws proper guides: features hang off the
  project with `├─ `/`└─ `, shells get continuation `│` (parent branch
  keeps running until its last child), and ASCII mode maps them to
  `|- `/``- `/`|`. Per-type icons appear next to the expand marker:
  `▣` projects, `◇` features, `●` shells (ASCII keeps `*`/`-`). Mouse
  hit-testing uses the same tier math as rendering, so the wider guide
  prefixes changed no hit areas.
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
- **M8.3 — discovery semantics, settled**: the scan reports a directory whose
  `.git` is a DIRECTORY as a repo and does not descend into it (repos nested
  inside a repo are that repo's business); a `.git` FILE (linked worktree /
  submodule) is skipped, covered by its main repo; hidden dirs are always
  skipped on top of `skip_dirs`; symlinks are never followed (cycle safety);
  depth is bounded by `[discovery].max_depth` (0 = scan root only). The
  startup auto-import resolves to the MAIN repo (`git worktree list` first
  entry) even when booting from inside a linked worktree. The scan cache
  (`discovered.json`: root + timestamp + paths) is written after every scan
  and read at startup to tell the prompt how many repos the last scan found —
  it never auto-imports from a stale cache. `I` (shifted = stronger variant
  of `i`, per the global TUI keybinding rules) rescans `scan_root` and also
  re-prunes so `missing` projects re-activate. Config-declared projects that
  are not repos are still skipped with a warning (explicit config entries are
  the user's statement; the missing-flag is for projects that WERE imported
  and later vanished). Missing projects keep their features and shells
  untouched (they cannot be verified while gone); pruning only removes
  features/shells of present projects. Startup discovery is gated behind an
  `interactive_boot` flag (`run()` sets it; tests boot headless without it),
  so tests never import the crate's own repo by accident.
- **M8 — state schema v2**: `State.version = 2`, `Project.session_name:
  Option<String>`. v1 files load via serde defaults; v1 shells (windows in the
  dashboard session) are not found by session-based reconcile and are pruned
  (worktrees and projects survive).
- **M8.4b — the help overlay is the palette**: the app has one index of
  everything (`?`), grouped by context (navigate / projects / features /
  shells / global) with the same words as the contextual bar (shared
  `HELP_GROUPS` strings). A fuzzy-search palette was deliberately not
  built: the action set is small enough that a grouped reference +
  contextual bar covers discoverability; the palette is scrollable and
  the popup sizes to content (clip + "j/k scrolls" hint instead of a
  fixed-size overlay).
- **M8.4b — motions while the palette is open**: the accessibility layer
  keeps ONE code path (PageUp/ctrl-d/Home/g/G...) and routes it to the
  overlay when the overlay is open, tree otherwise — mode-dependent
  target, never two implementations. Tree selection never moves while
  browsing help.
- **M8.4b — everything documented**: `docs/shortcuts.md` lists every
  binding with the TUI rule it satisfies and every deliberately unbound
  key with its reason; updated in the same commit as any binding change.
- **M8.4c — the Catppuccin set is the reference palette**: all four
  flavours ship as built-ins (official hex values, unmodified, mapped onto
  the app's roles: base → background, text/subtext0 → foreground/dim,
  surface0-2 → selection/borders, flavour accents → status roles,
  lavender → accent). `catppuccin-mocha` is the new default (the guideline
  names it the usual default; config files written before this keep their
  old `theme = "dracula"` line and load exactly as before).
- **M8.4c — `ctrl-t` live theme picker**: previews as the cursor moves
  (the whole screen repaints via the same load path), Enter keeps, Esc
  restores the theme it opened with. Built-ins list first (stable order),
  then user themes. The pick persists to `state.json` (configuration
  guideline: the app never rewrites the user's config file; `state.theme`
  wins over the config line at startup, and the config line stays the
  documented startup default).
- **M8.4c — picker motions**: j/k/arrows step by one theme, g/G and
  Home/End jump to first/last, PgUp/PgDn + ctrl-d/u move 5, the wheel
  moves 3, and a mouse click on a row selects + previews while a
  double-click keeps — all through the one accessibility code path (the
  tree selection never moves while the picker is open).
- **M8.4d — config layering**: defaults ← user config (config.toml /
  `--config` / `LW_CONFIG`) ← `.lazywingman.toml` ← `.lazywingman.local.toml`
  (gitignored sibling, highest of the file layers). Merge is key by key:
  maps merge deeply per subkey, lists/scalars replace outright. The
  project file is found walking upward from cwd, stopping at the first hit
  *and* at a repository root (`.git`) so a sibling repo's config is never
  inherited. Per the configuration rules, some keys are user-only and are
  rejected in project files with a diagnostic (never silently applied):
  `tmux.back_key` (a global tmux binding), `worktrees.root`,
  `discovery.scan_root` (machine paths).
- **M8.4d — drop the smallest unit, never prevent startup**: loading is
  lenient — a whole-file TOML syntax error drops that file's contribution,
  a bad `[[agents]]`/`[[projects]]` entry drops that entry, a bad scalar or
  a bad section field (one bad binding) defaults on its own, a bad status
  pattern regex is dropped at load, unknown keys are reported not fatal.
  Every drop lands in `Config.diagnostics` with what/where/used-instead and
  is shown ONCE in a startup diagnostics overlay (Esc/q/Enter dismiss,
  j/k/g/G/PageUp/Home/End/wheel scroll — same accessibility code path as
  help/picker; the tree selection never moves while it is open). A startup
  confirm (discovery scan prompt) parked behind the overlay resumes after
  dismissal.
- **M8.4d — `--config-dump`**: prints the effective values with the layer
  each key came from ("key = value  # from <layer>") and exits before the
  TUI — the configuration rule "make the resolved config inspectable".
- **M8.4d — theme failure is lenient too**: a theme that fails to load
  (picked or configured) no longer prevents startup — it falls back to the
  built-in default and records a diagnostic.
- tmux quirks hit & fixed: `list-panes -t <session>` only lists the active
  window (use `-a` + filter by session_id); `new-window` right after
  `new-session -d` races window index 0 (retry 3x); a fake TMUX env var makes
  tmux behave as a nested client and breaks window creation; `capture-pane`
  pads blank lines.
- Back-key interactive fire could not be simulated in this environment (pty
  byte-injection blocked by host guardrail) — binding registration and
  select-window both verified separately.
- **M8.4a — accessibility**: one dispatch layer above all modes handles
  PageUp/PageDown/Home/End + ctrl-d/u/f/b + ctrl-c; `g`/`G` stay normal-mode
  motions. Handled keys are consumed — ctrl-d is navigation, never "delete".
- **M8.4a — crossterm here reports no `click_count`** on MouseEvent, so
  double-click = two left clicks at the same spot within 400 ms, tracked on
  the App.
- **M8.4a — small terminals**: layout tiers (Full / NoPreview / TreeOnly /
  message) at fixed thresholds; overlays that don't fit are refused with a
  "frame too small" box instead of drawing garbage; the input cursor is only
  set when on-buffer.
- **M8.4a — NO_COLOR** (no-color.org, present + non-empty) resolves every
  role to the terminal default via `theme::load_with`; a monochrome render,
  not a low-colour one.
- **M8.4a — ASCII fallback**: `LW_ASCII=1` or an exact C/POSIX locale
  (LC_ALL > LC_CTYPE > LANG; `C.UTF-8` excluded) switches to `+ - |` borders
  (buffer cell rewrite — styles untouched) and plain markers. Only the app's
  own labels; user pane content is never rewritten.
- **M8.4a — statusbar**: contextual, clickable shortcut entries computed per
  selection; hjkl/Esc/q deliberately take no slot (assumed knowledge).
