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
  `config`, `theme`, `git`, `tmux`, `state`, `status`, `ansi`, `app`, `ui`.
- No async runtime: single-threaded crossterm event loop, status polling on a
  timer inside the loop (tmux CLI calls are fast enough at 500ms cadence).
- State file: JSON at `$XDG_STATE_HOME/lazywingman/state.json` (state, not
  data, is the semantically correct XDG dir for mutable runtime state).
- Status model: `dead | ended | failed | waiting | working | idle`.
  `dead` = pane gone from tmux (the shell exited — tmux destroys the pane
  unless `remain-on-exit` is set) or pane_dead; `ended` = live pane whose
  foreground command is a login shell
  (agent exited); failed/waiting/working = regex on `capture-pane` tail;
  `idle` = default.
- Feature deletion: kill windows → `git worktree remove` (with `--force`
  offered in the confirm dialog when dirty) → drop from state. The branch
  goes with the worktree unless `worktrees.delete_branch = false` — a
  feature branch whose worktree is gone is dead weight in the common case;
  keeping it is the exception, so it is the opt-in.
- `d` is contextual: on a feature row it deletes the feature, on a shell
  row it closes just that shell (kill the window, drop it from the
  feature). Deleting a feature from a shell row was the old behaviour and
  the wrong one — "close this agent" is the frequent action, "delete the
  whole feature" the rare one, and the two must not share a target.
- The delete confirm answers to `d` as well as `y`/`Enter`: `d` opened the
  popup, so `dd` completes the delete without a hand move — the vim reflex.
  `f` (force) stays a separate key: discarding uncommitted work must not be
  reachable by pressing the same key twice.
- Tree rows carry no counts (`(2 features)`, `[2 shells]` are gone): the
  tree is expanded by default, so the children are the count and the text
  only repeated what was one row below. The one thing a *folded* project
  row cannot show otherwise — whether it holds anything at all — moved into
  its icon (`▣` non-empty, `▢` empty), the same trick the feature icon
  already plays for the worktree's state.
- Shell indices are never reused: `Feature::next_shell_index` counts past
  the highest index in use, because the index names the tmux window and is
  what `reconcile` matches a live window back to.
- No "remove project" action: a project is a discovered repo, not app
  state worth curating — removing it only made the tree drift from the
  scope until the next scan brought it back.
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
- **M16 — dead entries are muted whole** (user feedback: "when a shell is
  closed/dead, mute the whole entry in the tree"). Colouring only the status
  word left a dead row as visually loud as a working one, so the eye had to
  read the end of every row to find what was still alive. A dead row now
  draws entirely in `status_dead`, and the rule aggregates upward (feature
  dead = all its shells dead; project dead = all its features dead) so the
  muting matches how the tree folds. Empty containers are excluded: a
  feature with no shells is unstarted, not finished.
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
  project still shows its row. Everything unfolds by default up to
  `UNFOLD_CAP = 30` projects in scope (see the M20 entry). The startup scan prompt and the
  discovered.json cache are GONE: finding `.git` entries is a few stats per
  dir, done live at boot and on `I` — no stale cache to trust, no prompt to
  dismiss, even at hundreds of folders. The expensive part — `git worktree
  list` per project — is lazy (`ensure_worktrees`): it runs ONCE per project
  when the project is expanded (or for the flattened single project at
  boot), and `prune` skips worktree verification for projects whose
  worktrees were never loaded, so boot never pays 200 git calls. Since M20
  the unfolded projects' listings are queued rather than run at boot.
- **M24 — a worktree with no shell is dimmed, and that is the whole
  signal.** `ensure_worktrees` lists every worktree the repo has, so rows
  appear for worktrees LazyWingman never created. The row state worth
  showing is not *who made it* but *whether there is a tmux window for it*:
  with no window there is nothing to attach to, send to or capture, whoever
  made the worktree. So the predicate is derived — `shells.is_empty()`,
  which `prune` keeps honest by dropping shells whose window vanished — and
  NOT a persisted `created_by_wingman` flag. A flag would have needed a
  schema field, a migration for existing state files, and would still have
  lied about a wingman-made worktree whose last shell was closed: same
  inertness, different provenance. Derived also self-heals — `a` starts a
  shell and the row joins the rest with no state to fix up. The row mutes
  *whole* (the rule dead rows already follow) and the preview says why it is
  empty instead of showing the generic "select a shell" hint, which would
  send the user hunting for a child row that does not exist. Deletion is
  deliberately unchanged: the confirm already reports dirty vs clean, and a
  provenance warning on a state that cannot distinguish "made by hand" from
  "shell closed" would cry wolf on the common case.
- **M23 — the New Feature popup names the base branch it cuts from.** A
  feature's branch has always been cut from the project's base branch
  (`worktrees.base_branch`, or the project's override), never from the
  branch the repo happens to have checked out — but the popup asked for a
  name and said nothing else, so the user had to know that. It now prints
  `branches from: <base> (base branch)` under the name field. The branch is
  resolved through the SAME `Git::resolve_base_branch` call `create_feature`
  makes, so the popup cannot promise a base the creation then ignores, and
  it is resolved ONCE when the popup opens — a git call per keystroke, or
  per frame, for a line that cannot change while the popup is up, would be
  the wrong trade. The fallback is spelled out rather than hidden: when the
  configured base is absent from the repo, creation falls back to the
  current branch, so the popup names that branch and warns
  `base branch '<configured>' not found — using current branch` instead of
  claiming a base branch that does not exist.
- **M22 — no `i import` / `I scan`: discovery is the only way in.** Both
  keys predate scope-based discovery (M9.3), which already imports every repo
  inside the scope at boot, live and without a cache. `i` (type an absolute
  path) and `I` (rescan the same root discovery just walked) were left over
  from the era of an explicit project list, and they cost two of the scarce
  top-level keys plus two of the four statusbar slots on a project row. What
  remains covers the cases: the scope is discovered at startup, and a repo
  outside it goes under `[[projects]]` in `config.toml`. The deliberate loss:
  a repo created WHILE LazyWingman runs no longer appears without a restart —
  `r` refreshes tmux state, not the repo list. Folding a rescan into `r` was
  considered and left out: `r` means "reconcile what I am showing against
  reality", and walking the filesystem is a different (and much more
  expensive) promise. It can be revisited if the restart proves annoying.
- **M21 — one reentrant lock for every process-global test fixture.** Two
  locks (`XDG_LOCK`, `TMUX_TEST_LOCK`) guarding the same kind of thing —
  process-wide env vars — is a lock-order inversion waiting to happen, and it
  happened: the fixtures nest in both orders, so parallel `cargo test` could
  hang forever. The alternative fix, "always take them in the same order",
  was rejected: it is a rule nobody can see from a call site, and the failure
  mode is a hang with no message. One lock removes the ordering question
  entirely. The cost is that tmux tests and XDG tests no longer run
  concurrently with each other — worth ~0s on an 8.7s suite, since both were
  already serialised within their own group. Reentrancy is per thread
  (`DEPTH` + a `DepthReset` drop guard, not a `ReentrantLock`) because the
  fixtures must keep nesting, and because the guard has to be released only
  after the depth is back to 0.
- **M20 — unfold by default; the cap is readability, not cost.** Folding
  hides the shells the dashboard exists to supervise, so the tree now
  starts fully unfolded. The old `≤5 projects` rule existed because
  unfolding a project triggers its `git worktree list`, and a scope with
  dozens of repos would have paid all of them before the first frame. That
  is now decoupled: boot loads the FIRST visible project synchronously and
  queues the rest, and the event loop drains one per iteration
  (`drain_pending_worktrees`), so the tree paints immediately and fills in
  over the next frames. No threads — the loop is single-threaded and one
  `git worktree list` per frame is invisible. `UNFOLD_CAP = 30` survives
  purely as a readability backstop: past it the tree is longer than any
  terminal and folding is the better default.
- **M20 — quick-send drafts are per shell, in memory, and dropped only on
  delivery.** A half-written prompt is expensive to retype, so `Esc` parks
  the buffer against the shell's pane id (the key `captures` and `statuses`
  already use) and `s` resumes it, cursor included. Two deliberate limits:
  drafts do NOT go into `state.json` — they are working state, not
  configuration, and a prompt half-written three days ago is noise, not a
  feature — and a draft is dropped only when the send actually SUCCEEDED.
  `submit_input` toasts and swallows its error, so clearing on submit would
  lose the text exactly when the user most needs it (dead pane, no server).
  `TEXTBUF_MAX_BYTES = 1_000_000` is a runaway-paste backstop, not a limit
  anyone types into; past it input is dropped silently, because an error
  toast per keystroke would be worse than a buffer that stops growing.
- **M20 — panel titles are Title Case names, body text is not.** Titles
  name a thing (`Preview`, `Quick Send`, `Projects`), so they read as
  proper names; hints, statusbar and body text stay lowercase, which is
  the LazyGit register this project follows. The preview title lost its
  `(capture-pane)` suffix: it named the implementation, not the panel. The
  tree title follows the tree's actual contents — `Features (n)` when a
  single repo is flattened, `Projects (n)` otherwise — and counts only
  in-scope projects, since out-of-scope ones have no rows.
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
- **Mouse capture is the app's job, not ratatui's.** `ratatui::init()`
  enables raw mode and the alternate screen only, so the whole mouse layer
  above was dead until `term_init()`/`term_restore()` added
  `EnableMouseCapture`/`DisableMouseCapture` around it (also on the attach
  handoff, and in a panic hook — mouse reporting left on would spray escape
  sequences into the shell we hand the terminal back to). Consequence,
  accepted: while the dashboard runs, the terminal's native text selection
  needs the usual Shift override, as in every other mouse-driven TUI.
  One event is read then matched — never matched inside the `read()`
  let-chain, which silently swallowed every mouse event.
- **M8.4a — small terminals**: layout tiers (Wide / Split / TreeOnly /
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
- **M10 — the detail panel is gone**: every field it showed was either
  already in the tree (name, status, shell count), config the user wrote
  themselves (path, base branch, agent command, worktree root), or noise
  (pane geometry, process name, a keys line duplicating the statusbar).
  The branch was redundant by construction — a feature owns its worktree
  and therefore its branch. The one fact it held alone, an uncommitted
  worktree, moved into the tree as the feature icon (`◇` clean, `◆`
  dirty), so the layout is now tree | preview. `PaneInfo` lost its
  width/height with its only reader.
- **M10 — the statusbar is shortcuts and nothing else**: the app name,
  the session chip and the project count never changed anything the user
  could act on, and the message slot was never cleared, so the last
  notice stayed on screen for the rest of the session. Notices are now
  toasts above the bar with their own TTL (4s, 6s for errors) — the log
  and the diagnostics overlay stay the permanent record.
- **M10 — dirty flags are polled, not rendered**: `git status` per
  feature per frame would fork a process per row per redraw, so the tree
  reads a cache refreshed on the poll tick at most every 3s (and
  immediately after a feature is created or deleted).
- **M11 — a one-shell feature is folded, not collapsed**: the nominal case
  is one worktree, one agent, so a feature with a single shell gets no
  child row at all — its own row carries the shell's status and acts as
  that shell (`Enter` attaches, `s` quick-sends, the preview follows it).
  Making it "collapsed by default" instead would still cost a keypress to
  see a row that repeats the feature's name and nothing else. The rule
  lives in one predicate, `App::solo_shell`, and is enforced in
  `visible_nodes()` so navigation, mouse hits and `clamp_sel` cannot
  disagree with what is drawn. `a` (add shell) is the way out: two shells
  bring the subtree back.
- **M11 — the tmux window id left the tree**: `@5` next to every shell was
  the only thing on that row the user cannot act on — a handle for our own
  `select-window` calls. It is still in `state.json`, which is where a
  debugging session should look for it.
- **M12 — predefined messages are a picker, not a second editor**: `p` on a
  shell opens `Mode::Messages`, a numbered list of the configured presets,
  and `Enter` (or the entry's own digit `1`-`9`) **sends immediately** —
  no detour through the textarea. The whole point of a canned prompt is
  that it needs no editing; anything that does is what `s` is for. The
  text goes through the same `quick_send` paste path as the textarea, so
  the agent cannot tell a preset from a typed prompt.
- **M12 — `[[messages]]` replaces the built-ins, like every other list**:
  a config that defines any entry defines the whole list (the layering
  rule "maps merge deeply, lists are replaced outright"). The two
  defaults ("continue", "merge to master, drop the worktree") therefore
  only apply when no layer mentions `[[messages]]` — and they are written
  out verbatim on first run, so editing them is editing a visible list,
  not fighting an invisible one. `label` is optional: an entry without it
  is listed by the text it sends.
- **M13 — the preview keeps the pane's colours, and the classifier does
  not**: `capture-pane -e` is the single capture, used twice. The preview
  parses the escapes (agents encode meaning in colour — a red diff, a
  yellow spinner — and throwing it away threw away information the user
  reads in the real pane), while status classification strips them first,
  because the pattern regexes describe words, not styled words. Capturing
  twice (once with `-e`, once without) would have doubled the tmux calls
  per shell per poll for the same result.
- **M13 — three colour modes, not a boolean**: "colours on/off" misses the
  case the panel actually suffers from, a pane background fighting the
  theme's. `[preview].colors = full | foreground | none` splits it: `full`
  is the default because the point is to see what the agent painted,
  `foreground` keeps the information (text colour, bold, underline) while
  the panel keeps the theme's background, `none` is the escape hatch.
  Config-only — the modes are a preference, not a per-moment decision, so
  they take no keybinding.
- **M17 — a tmux pane user option carries the agent's self-reported state,
  not a per-shell state file or a Unix socket.** Both alternatives were
  considered and rejected:
  - A per-shell state file (written by the hook, read by the poll loop)
    would be a *second* source of truth alongside tmux, when every other
    piece of live status data in the app — panes, captures, dirty flags —
    is already re-derived from tmux/git on each poll rather than trusted
    from a cache. It would also need its own garbage collection: nothing
    guarantees the file for a dead pane ever gets removed, unlike a tmux
    pane option, which is destroyed automatically with the pane it
    belongs to (the same "dead = gone from tmux" model M14 already
    established).
  - A Unix socket needs a listener thread in what is otherwise a
    synchronous TUI, plus a stale-socket lifecycle and a non-blocking
    client with a timeout — because a naive hook client would **block
    the agent** whenever LazyWingman was down (the hook fires from
    inside Claude's own turn; a client that blocks trying to connect
    blocks the agent it's reporting on). It is also the only one of the
    rejected options that loses events outright while the dashboard is
    closed, rather than merely deferring them.
  - The pane option needs no daemon and no cleanup: the hook subcommand's
    entire write is one `tmux set-option` call, matching the existing
    rule that all tmux interaction goes through the CLI. It survives a
    LazyWingman restart with zero recovery code (read straight back out
    of the next `list-panes` call) and disappears with the pane, so
    nothing is ever left to reconcile or expire.
- **M17 — instrumentation is automatic, with no consent modal.**
  Disclosure comes from the injected flag itself: a new shell shows its
  command at the top of the pane, and it reads `claude --settings
  '~/.local/state/lazywingman/lazywingman-activity-hooks.json'` — the
  settings file is named so that line is self-explanatory. A first-run
  "hello/docs" modal explaining it may come later; it was explicitly out
  of scope for this milestone. `[defaults] agent_hooks = true` is the
  opt-out.
