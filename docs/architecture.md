# LazyWingman — architecture and design decisions

Why the app is built the way it is. Every entry is a decision that was not
fixed by the spec, kept with its reason so it is not re-litigated by
accident. Grouped by subsystem; add to the section a change belongs to, in
the same commit as the change.

## Crate and runtime

- Single binary crate, no `lib.rs`; modules: `error`, `util`, `logging`,
  `config`, `theme`, `git`, `tmux`, `state`, `status`, `ansi`, `app`, `ui`.
- No async runtime: single-threaded crossterm event loop, status polling on a
  timer inside the loop (tmux CLI calls are fast enough at 500ms cadence).
- ratatui 0.30 is the rendering stack (the crate was rewritten around
  ratatui-core / ratatui-widgets / ratatui-crossterm): `Color::Rgb` is a
  tuple, 256-colour is `Color::Indexed`, ANSI colors render as `38;5;0..15`
  through the crossterm backend (still the terminal palette).

## State and persistence

- State file: JSON at `$XDG_STATE_HOME/lazywingman/state.json` (state, not
  data, is the semantically correct XDG dir for mutable runtime state).
- **State schema v3**: `State.version = 3`. v3 renamed the on-disk keys
  (`repositories`/`worktrees` → `repositories`/`worktrees`) with no migration
  path, so `load` reads `version` first and discards a well-formed file of
  any other version, starting empty and letting `reconcile` rediscover the
  repositories and reconstruct their worktrees. Malformed JSON is still an
  error — the user is told rather than silently losing the tree.
- Shell indices are never reused: `Worktree::next_shell_index` counts past
  the highest index in use, because the index names the tmux window and is
  what `reconcile` matches a live window back to.

## Discovery

- **Discovery is scope-based, live, and lazy.** The tree does not list every
  repository ever discovered — that mixed scopes. The scope is the startup
  directory (or `[discovery].scan_root`): only repositories inside it get
  rows; repositories from other scopes stay in state but are hidden. When you
  start INSIDE a git repo, that repo is the scope and its worktrees are shown
  flat at the top level (no repository row — `flatten`); a lone MISSING
  repository still shows its row. Everything unfolds by default up to
  `UNFOLD_CAP = 30` repositories in scope (see "The tree"). There is no
  startup scan prompt and no `discovered.json` cache: finding `.git` entries
  is a few stats per dir, done live at boot — no stale cache to trust, no
  prompt to dismiss, even at hundreds of folders. The expensive part —
  `git worktree list` per repository — is lazy (`ensure_worktrees`): it runs
  ONCE per repository when the repository is expanded (or for the flattened
  single repository at boot), and `prune` skips worktree verification for
  repositories whose worktrees were never loaded, so boot never pays 200 git
  calls. The unfolded repositories' listings are queued rather than run at
  boot.
- **Discovery semantics, settled**: the scan reports a directory whose `.git`
  is a DIRECTORY as a repo and does not descend into it (repos nested inside a
  repo are that repo's business); a `.git` FILE (linked worktree / submodule)
  is skipped, covered by its main repo; hidden dirs are always skipped on top
  of `skip_dirs`; symlinks are never followed (cycle safety); depth is bounded
  by `[discovery].max_depth` (0 = scan root only). The startup auto-import
  resolves to the MAIN repo (`git worktree list` first entry) even when
  booting from inside a linked worktree. Config-declared repositories that are
  not repos are skipped with a warning (explicit config entries are the user's
  statement; the missing-flag is for repositories that WERE imported and later
  vanished). A repository whose path vanished is kept in state and marked
  `missing` (shown "(missing)" in the tree) instead of being pruned; it
  re-activates when the path is a repo again, and keeps its worktrees and
  shells untouched while gone (they cannot be verified). Pruning only removes
  worktrees/shells of present repositories. Startup discovery is gated behind
  an `interactive_boot` flag (`run()` sets it; tests boot headless without
  it), so tests never import the crate's own repo by accident.
- **No `i import` / `I scan`: discovery is the only way in.** Both keys
  predate scope-based discovery, which already imports every repo inside the
  scope at boot, live and without a cache. `i` (type an absolute path) and `I`
  (rescan the same root discovery just walked) were left over from the era of
  an explicit repository list, and they cost two of the scarce top-level keys
  plus two of the four statusbar slots on a repository row. What remains
  covers the cases: the scope is discovered at startup, and a repo outside it
  goes under `[[repositories]]` in `config.toml`. The deliberate loss: a repo
  created WHILE LazyWingman runs no longer appears without a restart — `r`
  refreshes tmux state, not the repo list. Folding a rescan into `r` was
  considered and left out: `r` means "reconcile what I am showing against
  reality", and walking the filesystem is a different (and much more
  expensive) promise. It can be revisited if the restart proves annoying.
- **No "remove repository" action**: a repository is a discovered repo, not
  app state worth curating — removing it only made the tree drift from the
  scope until the next scan brought it back.

## Worktrees and deletion

- Worktree root default:
  `<parent of repo>/.lazywingman-worktrees/<repository>/<worktree>`.
- Worktree deletion: kill windows → `git worktree remove` (with `--force`
  offered in the confirm dialog when dirty) → drop from state. The branch
  goes with the worktree unless `worktrees.delete_branch = false` — a
  worktree branch whose worktree is gone is dead weight in the common case;
  keeping it is the exception, so it is the opt-in.
- `d` is contextual: on a worktree row it deletes the worktree, on a shell
  row it closes just that shell (kill the window, drop it from the
  worktree). Deleting a worktree from a shell row was the old behaviour and
  the wrong one — "close this agent" is the frequent action, "delete the
  whole worktree" the rare one, and the two must not share a target.
- The delete confirm answers to `d` as well as `y`/`Enter`: `d` opened the
  popup, so `dd` completes the delete without a hand move — the vim reflex.
  `f` (force) stays a separate key: discarding uncommitted work must not be
  reachable by pressing the same key twice.
- **The New Worktree popup names the base branch it cuts from.** A worktree's
  branch has always been cut from the repository's base branch
  (`worktrees.base_branch`, or the repository's override), never from the
  branch the repo happens to have checked out — but the popup asked for a name
  and said nothing else, so the user had to know that. It now prints `branches
  from: <base> (base branch)` under the name field. The branch is resolved
  through the SAME `Git::resolve_base_branch` call `create_worktree` makes, so
  the popup cannot promise a base the creation then ignores, and it is
  resolved ONCE when the popup opens — a git call per keystroke, or per frame,
  for a line that cannot change while the popup is up, would be the wrong
  trade. The fallback is spelled out rather than hidden: when the configured
  base is absent from the repo, creation falls back to the current branch, so
  the popup names that branch and warns `base branch '<configured>' not found
  — using current branch` instead of claiming a base branch that does not
  exist.

## Shells and tmux sessions

- **Agent shells live in dedicated tmux sessions** (`wingman-session-<repo>`),
  per user feedback ("do not pollute my tmux session"): creating a worktree /
  adding a shell must not add windows to the user's session. One session per
  repository, named `wingman-session-<sanitized-name>` (+ `-<hash8>` suffix on
  collision), windows inside named `lw:<worktree>/<n>`; strict naming is what
  makes rediscovery after a crash/close possible (`list-sessions` +
  per-session `list-windows`). Killing the worktree's last window
  auto-destroys the session (verified against tmux 3.5a).
- **Attach/back across sessions**: inside tmux, attach = `switch-client -t
  <agent-session>` + `select-window -t @<shell-window>` (window IDs are
  server-global; clientless `select-window -t session:@win` verified to change
  the session's active window). The back-key is bound in every agent session to
  `switch-client -t <user-session> \; select-window -t @<dashboard-window>`
  (rebound at startup and at session creation).
- **The back-key lives behind the prefix.** It was first bound with
  `bind-key -n` (root table), which is server-wide in tmux: it hijacked `C-o`
  for every session and every TUI running inside (vim's `C-o`, readline, …).
  The chord is now bound in the PREFIX table (`bind-key -T prefix <key>`), so
  it only fires after the user's tmux prefix (`C-b C-o` by default) and no
  application ever loses the key. Legacy root bindings are removed on startup,
  but only the exact command we bound — a user's own root binding of the same
  key is untouched. `tmux.back_key` configures the second half of the chord.
- **Runs inside AND outside tmux** (per user feedback): the dashboard does not
  require tmux around itself. `Tmux` keeps optional `inside` coordinates
  (detected via TMUX/TMUX_PANE); all session/window/pane operations work
  standalone against the (auto-started) tmux server. Attach from outside tmux =
  temporary terminal handoff: TUI restores the terminal, runs
  `tmux attach -t <agent-session>` (after clientless select-window to the
  shell's window), then re-enters the TUI on detach. No back-key exists outside
  tmux (the user detaches with C-b d). There is no no-tmux mode.
- **The tmux server is auto-started** for a standalone dashboard: tmux 3.5a
  exits a server as soon as it has no sessions and no clients, so
  `Tmux::ensure_server()` runs `start-server \; set -g exit-empty off` in ONE
  client invocation (two clients would race the server's exit). It only runs
  when `list-sessions` reports no server, so the user's own server is never
  modified; the dashboard's server then lingers empty after quit (harmless,
  standard tmux-with-exit-empty-off behaviour). `list-panes -a` on a
  zero-session server ("no current target") reads as an empty pane list.

## Status classification

- Status model: `dead | ended | failed | blocked | waiting | working | idle`.
  `dead` = pane gone from tmux (the shell exited — tmux destroys the pane
  unless `remain-on-exit` is set) or pane_dead; `ended` = live pane whose
  foreground command is a login shell (agent exited); failed/waiting/working =
  regex on `capture-pane` tail; `blocked` comes from an instrumented agent's
  own hook report; `idle` = default.
- Classification order (see the module doc on `src/status.rs`): dead → the
  agent's own hook-reported state, when the pane carries a live `@lw_agent`
  overlay AND an agent is still the foreground command → failed → waiting if
  the LAST line matches a prompt pattern (the freshest signal) → working →
  waiting anywhere → ended → idle. Two guards keep the overlay honest: the
  login-shell check drops a value stranded by an agent that died without
  firing SessionEnd (the option lives in the tmux server, so it would
  otherwise outlive the process that wrote it), and an overlay past
  `OVERLAY_STALE_AFTER_SECS` is only distrusted when the tail ALSO looks idle
  — either condition alone would misclassify a legitimately long turn.
- The regex ladder's tail is checked before the shell-foreground heuristic
  because agents are often launched FROM a shell: the tail carries the
  agent's real state, the foreground command only says "a shell is in
  charge".
- **Dirty flags are polled, not rendered**: `git status` per worktree per
  frame would fork a process per row per redraw, so the tree reads a cache
  refreshed on the poll tick at most every 3s (and immediately after a
  worktree is created or deleted).

## Agent instrumentation (Claude hooks)

- **A tmux pane user option carries the agent's self-reported state, not a
  per-shell state file or a Unix socket.** Both alternatives were considered
  and rejected:
  - A per-shell state file (written by the hook, read by the poll loop) would
    be a *second* source of truth alongside tmux, when every other piece of
    live status data in the app — panes, captures, dirty flags — is already
    re-derived from tmux/git on each poll rather than trusted from a cache. It
    would also need its own garbage collection: nothing guarantees the file for
    a dead pane ever gets removed, unlike a tmux pane option, which is
    destroyed automatically with the pane it belongs to (the same "dead = gone
    from tmux" model the status layer already establishes).
  - A Unix socket needs a listener thread in what is otherwise a synchronous
    TUI, plus a stale-socket lifecycle and a non-blocking client with a timeout
    — because a naive hook client would **block the agent** whenever
    LazyWingman was down (the hook fires from inside Claude's own turn; a
    client that blocks trying to connect blocks the agent it's reporting on).
    It is also the only one of the rejected options that loses events outright
    while the dashboard is closed, rather than merely deferring them.
  - The pane option needs no daemon and no cleanup: the hook subcommand's
    entire write is one `tmux set-option` call, matching the existing rule that
    all tmux interaction goes through the CLI. It survives a LazyWingman
    restart with zero recovery code (read straight back out of the next
    `list-panes` call) and disappears with the pane, so nothing is ever left to
    reconcile or expire.
- **Instrumentation is automatic, with no consent modal.** Disclosure comes
  from the injected flag itself: a new shell shows its command at the top of
  the pane, and it reads `claude --settings
  '~/.local/state/lazywingman/lazywingman-activity-hooks.json'` — the settings
  file is named so that line is self-explanatory. A first-run "hello/docs"
  modal explaining it may come later; it was explicitly out of scope.
  `[defaults] agent_hooks = true` is the opt-out. The enabling fact is that
  `claude --settings <file>` takes per-invocation settings that merge
  additively with the user's own and persist nothing, so instrumenting a
  shell changes no file the user owns.
- **Two limitations are known and accepted.** `failed` stays heuristic —
  Claude fires no error event, so there is nothing for a hook to report and
  the regex ladder remains the only signal. And the `Notification` handler
  matches `BLOCKING_NOTIFICATIONS` substrings against the message text, which
  is the one place the app is coupled to Claude's wording — it distinguishes
  "blocked on the user" from the 60-second idle nudge. Wording that stops
  matching is `Ignore`d rather than misapplied, so the pane keeps its last
  reported state instead of flipping to `blocked`; the failure mode is a
  missed `blocked`, never a wrong one.

## The tree

- **Unfold by default; the cap is readability, not cost.** Folding hides the
  shells the dashboard exists to supervise, so the tree starts fully unfolded.
  The old `≤5 repositories` rule existed because unfolding a repository
  triggers its `git worktree list`, and a scope with dozens of repos would
  have paid all of them before the first frame. That is now decoupled: boot
  loads the FIRST visible repository synchronously and queues the rest, and
  the event loop drains one per iteration (`drain_pending_worktrees`), so the
  tree paints immediately and fills in over the next frames. No threads — the
  loop is single-threaded and one `git worktree list` per frame is invisible.
  `UNFOLD_CAP = 30` survives purely as a readability backstop: past it the
  tree is longer than any terminal and folding is the better default.
- **A one-shell worktree is folded, not collapsed**: the nominal case is one
  worktree, one agent, so a worktree with a single shell gets no child row at
  all — its own row carries the shell's status and acts as that shell (`Enter`
  attaches, `s` quick-sends, the preview follows it). Making it "collapsed by
  default" instead would still cost a keypress to see a row that repeats the
  worktree's name and nothing else. The rule lives in one predicate,
  `App::solo_shell`, and is enforced in `visible_nodes()` so navigation, mouse
  hits and `clamp_sel` cannot disagree with what is drawn. `a` (add shell) is
  the way out: two shells bring the subtree back.
- **Tree rows carry no counts** (`(2 worktrees)`, `[2 shells]` are gone): the
  tree is expanded by default, so the children are the count and the text
  only repeated what was one row below. The one thing a *folded* repository
  row cannot show otherwise — whether it holds anything at all — moved into
  its icon (`▣` non-empty, `▢` empty), the same trick the worktree icon
  uses. **Expand markers are gone from every row** (repositories first, then
  worktrees): the icon already occupies that slot, and the guides below a row
  say whether it is open far more loudly than a 1-column arrow did — a
  collapsed row simply has no children under it. `h`/`l` still fold and
  unfold; the arrow was a second answer to a question the row already
  answered.
- **Dead entries are muted whole** (user feedback: "when a shell is
  closed/dead, mute the whole entry in the tree"). Colouring only the status
  word left a dead row as visually loud as a working one, so the eye had to
  read the end of every row to find what was still alive. A dead row draws
  entirely in `status_dead`, and the rule aggregates upward (worktree dead =
  all its shells dead; repository dead = all its worktrees dead) so the muting
  matches how the tree folds. Empty containers are excluded: a worktree with
  no shells is unstarted, not finished.
- **A worktree with no shell is dimmed, and that is the whole signal.**
  `ensure_worktrees` lists every worktree the repo has, so rows appear for
  worktrees LazyWingman never created. The row state worth showing is not *who
  made it* but *whether there is a tmux window for it*: with no window there
  is nothing to attach to, send to or capture, whoever made the worktree. So
  the predicate is derived — `shells.is_empty()`, which `prune` keeps honest
  by dropping shells whose window vanished — and NOT a persisted
  `created_by_wingman` flag. A flag would have needed a schema field, a
  migration for existing state files, and would still have lied about a
  wingman-made worktree whose last shell was closed: same inertness, different
  provenance. Derived also self-heals — `a` starts a shell and the row joins
  the rest with no state to fix up. The row mutes *whole* (the rule dead rows
  already follow) and the preview says why it is empty instead of showing the
  generic "select a shell" hint, which would send the user hunting for a child
  row that does not exist. Deletion is deliberately unchanged: the confirm
  already reports dirty vs clean, and a provenance warning on a state that
  cannot distinguish "made by hand" from "shell closed" would cry wolf on the
  common case.
- **The tmux window id left the tree**: `@5` next to every shell was the only
  thing on that row the user cannot act on — a handle for our own
  `select-window` calls. It is still in `state.json`, which is where a
  debugging session should look for it.

## Preview panel

- **The preview keeps the pane's colours, and the classifier does not**:
  `capture-pane -e` is the single capture, used twice. The preview parses the
  escapes (agents encode meaning in colour — a red diff, a yellow spinner — and
  throwing it away threw away information the user reads in the real pane),
  while status classification strips them first, because the pattern regexes
  describe words, not styled words. Capturing twice (once with `-e`, once
  without) would have doubled the tmux calls per shell per poll for the same
  result.
- **Three colour modes, not a boolean**: "colours on/off" misses the case the
  panel actually suffers from, a pane background fighting the theme's.
  `[preview].colors = full | foreground | none` splits it: `full` is the
  default because the point is to see what the agent painted, `foreground`
  keeps the information (text colour, bold, underline) while the panel keeps
  the theme's background, `none` is the escape hatch. Config-only — the modes
  are a preference, not a per-moment decision, so they take no keybinding.

## Quick send and predefined messages

- **Quick-send is a multiline textarea.** The main way to talk to agents is a
  full editor (`Mode::Textarea` + `TextBuf`): Enter inserts a newline, Tab a
  real tab, arrows/Home/End move the cursor (wrapping at line ends),
  PgUp/PgDn/ctrl-u/d/f/b page the cursor, and the send key is **ctrl-s**
  (Alt+Enter still works where the terminal delivers it) — a plain Enter is
  needed for editing, so it cannot be the submit key. While the textarea is
  open the accessibility layer yields ALL motion keys to it (only ctrl-c and
  ctrl-t stay global); the tree selection never moves while editing. Delivery
  uses tmux **paste-buffer with bracketed paste** (`paste_keys`) rather than
  `send-keys -l`: `send-keys -l "a\nb"` would translate the newline into an
  Enter press and submit the first line early; bracketed paste delivers
  multi-line text as one paste, then a trailing Enter submits. New-worktree
  and add-shell inputs stay single-line (`Enter` submits) — they are names,
  not prompts.
- **Why the send key is ctrl-s, and why the scroll window is the popup's.**
  Two bugs made quick-send unusable in a real tmux. (1) *Nothing could be
  submitted*: both original send chords are undeliverable in a default setup —
  tmux ships `extended-keys off`, so **Ctrl+Enter is not encodable at all** and
  arrives as a bare Enter (a newline), while **Alt+Enter** depends on the outer
  terminal emulator forwarding `ESC CR` (Windows Terminal binds it to
  fullscreen). The send key is **ctrl-s** — a plain control byte, which raw
  mode always delivers; Alt+Enter is kept as a bonus, and Ctrl+Enter is still
  accepted for terminals that do encode it. Lesson: a chord that needs the
  kitty/CSI-u protocol cannot be the ONLY way to perform an action. (2) *Only
  one line was ever visible*: every `TextBuf` edit and motion called
  `ensure_visible(1)` — a hardcoded one-line viewport — so each Enter scrolled
  the previous line out of a 10–18 row popup. `TextBuf` no longer scrolls at
  all; the key handler calls `ensure_visible` once per key with the popup's
  real content height (`textarea_visible_lines`), which is the only place that
  knows it.
- **Drafts are per shell, in memory, and dropped only on delivery.** A
  half-written prompt is expensive to retype, so `Esc` parks the buffer
  against the shell's pane id (the key `captures` and `statuses` already use)
  and `s` resumes it, cursor included. Two deliberate limits: drafts do NOT go
  into `state.json` — they are working state, not configuration, and a prompt
  half-written three days ago is noise, not a worktree — and a draft is
  dropped only when the send actually SUCCEEDED. `submit_input` toasts and
  swallows its error, so clearing on submit would lose the text exactly when
  the user most needs it (dead pane, no server). `TEXTBUF_MAX_BYTES =
  1_000_000` is a runaway-paste backstop, not a limit anyone types into; past
  it input is dropped silently, because an error toast per keystroke would be
  worse than a buffer that stops growing.
- **Predefined messages are a picker, not a second editor**: `p` on a shell
  opens `Mode::Messages`, a numbered list of the configured presets, and
  `Enter` (or the entry's own digit `1`-`9`) **sends immediately** — no detour
  through the textarea. The whole point of a canned prompt is that it needs no
  editing; anything that does is what `s` is for. The text goes through the
  same `quick_send` paste path as the textarea, so the agent cannot tell a
  preset from a typed prompt.
- **The list shows the message, not a name for it**: an entry is only its
  `text`, and the modal renders that text in full. An earlier `label` field
  let the list say "merge + drop worktree" while the pane received something
  else; the one thing a send-immediately picker must get right is that you can
  read what you are about to send. The retired key is reported as a stray key
  rather than ignored, so a config still carrying one says so.
- **Tokens are resolved once, for both the list and the send**:
  `App::expand_message` substitutes `<base_branch>`, `<branch>`, `<worktree>`,
  `<repository>` and `<worktree_path>` from the selected shell's repository
  and worktree, and both `draw_messages` and `send_predefined` call it — which
  is what makes the list honest. It reads state only (the base branch was resolved at import
  time), so it is safe to call every frame. Unknown tokens pass through as
  written, and so does everything when no shell is selected: there is no
  context to resolve against, and blanks would be worse than the template.

## Layout, wording and rendering

- **The detail panel is gone**: every field it showed was either already in
  the tree (name, status, shell count), config the user wrote themselves
  (path, base branch, agent command, worktree root), or noise (pane geometry,
  process name, a keys line duplicating the statusbar). The branch was
  redundant by construction — a worktree row owns its git worktree and
  therefore its branch. The one fact it held alone, an uncommitted worktree,
  moved into the tree as the worktree icon (nothing when clean, `󰘬` when
  dirty, per `[icons]`), so the layout is tree | preview.
- **The statusbar is shortcuts and nothing else**: the app name, the session
  chip and the repository count never changed anything the user could act on,
  and the message slot was never cleared, so the last notice stayed on screen
  for the rest of the session. Notices are toasts above the bar with their own
  TTL (4s, 6s for errors) — the log and the diagnostics overlay stay the
  permanent record. The bar's entries are contextual, clickable, and computed
  per selection; hjkl/Esc/q deliberately take no slot (assumed knowledge).
- **Panel titles are Title Case names, body text is not.** Titles name a thing
  (`Preview`, `Quick Send`, `Repositories`), so they read as proper names;
  hints, statusbar and body text stay lowercase, which is the LazyGit register
  this repository follows. The preview title carries no `(capture-pane)`
  suffix: it named the implementation, not the panel. The tree title follows
  the tree's actual contents — `Worktrees (n)` when a single repo is
  flattened, `Repositories (n)` otherwise — and counts only in-scope
  repositories, since out-of-scope ones have no rows.
- **UI pass from the screenshots (rounded corners, bare titles, tree guides,
  icons).** Panels and popups use `BorderType::Rounded` (╭ ╮ ╰ ╯). Titles
  render foreground-only — no background fill under a panel/popup title
  ("simple title display without background"). The tree draws proper guides:
  worktrees hang off the repository with `├─ `/`└─ `, shells get continuation
  `│` (parent branch keeps running until its last child), and every preset
  maps to the shapes named by `[icons]`. The six composites (`child`,
  `last_child`, and the four grandchild forms) are built in `draw_tree` from
  three keys — `guide_branch`, `guide_last`, `guide_vertical` — since six
  near-identical constants is not a surface worth configuring. A type icon
  sits next to each row: `[icons].repository`/`repository_empty` on
  repositories, `worktree`/`worktree_dirty` on worktrees, `shell` on shells.
  Mouse hit-testing uses the same tier math as rendering, so the wider guide
  prefixes changed no hit areas.
- **Small terminals**: layout tiers (Wide / Split / TreeOnly / message) at
  fixed thresholds; overlays that don't fit are refused with a "frame too
  small" box instead of drawing garbage; the input cursor is only set when
  on-buffer.
- **Dialog geometry**: the input prompt (new worktree, add shell, quick
  send) and the confirm dialog (delete worktree/repository) are sized to
  their content by `dialog_rect` — widest line and line count, plus one
  column/row of `DIALOG_PADDING` inside the border on every side — clamped
  to a 34-column floor and 60%-of-frame ceiling. They used to take a fixed
  percentage of the frame (62%×26% for confirms), so a two-line question
  drew a box a quarter of the screen tall with text jammed against the
  border.
- **Icons**: `[icons]` (`src/config.rs`) covers the tree's type icons and
  guides, the cursor marker, the separator, the warn mark, the
  ellipsis, the Enter glyph and the border style, chosen by a preset
  (`nerdfont` default, plus `unicode`, `ascii`, `none`) and overridable key
  by key. This
  replaced an `LW_ASCII` boolean, which conflated two independent
  questions — whether the terminal draws box characters, and which glyphs
  the user's font has. Keys that share a rendered column are padded to
  equal width at resolution time, so an empty icon still holds its column.
  The statusbar's `↵` is the `enter` key (`ascii` spells it `Enter`); the
  em dashes that used to leak into rendered help and hint text were reworded
  to ASCII punctuation rather than made configurable, since mid-sentence
  typography is not an icon. `asciify` rewrites only box-drawing, so
  anything outside `[icons]` that is non-ASCII would leak: the four
  `*_ascii_preset_is_clean` tests assert the whole rendered frame is ASCII
  rather than checking a denylist, which is what let `↵` and `—` survive
  unnoticed under the old `LW_ASCII` mode. `border = "ascii"` rewrites buffer
  cells to `+ - |` (`ui::asciify`) because ratatui has no ASCII
  `BorderType`; it rewrites box-drawing across whichever rect it is given,
  which for the preview panel and the quick-send textarea includes the
  captured pane content and the user's typed buffer, not only the app's
  own frame. `LW_ASCII` is reported as retired in the startup diagnostics.

## Input, mouse and accessibility

- Keybindings: hardcoded vim-ish set (documented in the help overlay,
  `docs/shortcuts.md` and README); only the tmux back-key is configurable
  (cheap win, avoids clashing with the user's tmux bindings).
- **Accessibility**: one dispatch layer above all modes handles
  PageUp/PageDown/Home/End + ctrl-d/u/f/b + ctrl-c; `g`/`G` stay normal-mode
  motions. Handled keys are consumed — ctrl-d is navigation, never "delete".
- **Motions while an overlay is open**: the accessibility layer keeps ONE code
  path (PageUp/ctrl-d/Home/g/G…) and routes it to the overlay when one is open,
  tree otherwise — a mode-dependent target, never two implementations. The tree
  selection never moves while browsing help, the theme picker or the
  diagnostics overlay.
- **Mouse capture is the app's job, not ratatui's.** `ratatui::init()` enables
  raw mode and the alternate screen only, so the whole mouse layer above it was
  dead until `term_init()`/`term_restore()` added
  `EnableMouseCapture`/`DisableMouseCapture` around it (also on the attach
  handoff, and in a panic hook — mouse reporting left on would spray escape
  sequences into the shell we hand the terminal back to). Consequence,
  accepted: while the dashboard runs, the terminal's native text selection
  needs the usual Shift override, as in every other mouse-driven TUI. One event
  is read then matched — never matched inside the `read()` let-chain, which
  silently swallowed every mouse event.
- **crossterm here reports no `click_count`** on MouseEvent, so double-click =
  two left clicks at the same spot within 400 ms, tracked on the App.
- **NO_COLOR** (no-color.org, present + non-empty) resolves every role to the
  terminal default via `theme::load_with` — a monochrome render, not a
  low-colour one.

## Themes

- Themes are TOML files in `themes/`, embedded via `include_str!` and copied to
  the config dir on first run so the user can edit them. Truecolor when
  `COLORTERM` says so, else 24-bit values quantize to xterm-256; `ansi.toml`
  uses only ANSI-16 colours.
- **The Catppuccin set is the reference palette**: all four flavours ship as
  built-ins (official hex values, unmodified, mapped onto the app's roles:
  base → background, text/subtext0 → foreground/dim, surface0-2 →
  selection/borders, flavour accents → status roles, lavender → accent).
  `catppuccin-mocha` is the default (the TUI guideline names it the usual
  default; config files written before this keep their old `theme = "dracula"`
  line and load exactly as before).
- **`ctrl-t` is a live theme picker**: it previews as the cursor moves (the
  whole screen repaints via the same load path), Enter keeps, Esc restores the
  theme it opened with. Built-ins list first (stable order), then user themes.
  The pick persists to `state.json` (configuration guideline: the app never
  rewrites the user's config file; `state.theme` wins over the config line at
  startup, and the config line stays the documented startup default).
- **Picker motions**: j/k/arrows step by one theme, g/G and Home/End jump to
  first/last, PgUp/PgDn + ctrl-d/u move 5, the wheel moves 3, and a mouse click
  on a row selects + previews while a double-click keeps — all through the one
  accessibility code path.
- **Theme failure is lenient**: a theme that fails to load (picked or
  configured) does not prevent startup — it falls back to the built-in default
  and records a diagnostic.

## Configuration

- **Config layering**: defaults ← user config (config.toml / `--config` /
  `LW_CONFIG`) ← `.lazywingman.toml` ← `.lazywingman.local.toml` (gitignored
  sibling, highest of the file layers). Merge is key by key: maps merge deeply
  per subkey, lists/scalars replace outright. The repository file is found
  walking upward from cwd, stopping at the first hit *and* at a repository
  root (`.git`) so a sibling repo's config is never inherited. Per the
  configuration rules, some keys are user-only and are rejected in repository
  files with a diagnostic (never silently applied): `tmux.back_key` (a global
  tmux binding), `worktrees.root`, `discovery.scan_root` (machine paths).
- **The written config is exhaustive and inert**: on first run — and only when
  the file is absent — `DEFAULT_CONFIG` is written out with *every* key the
  loader recognises, each commented at its built-in default. Defaults
  therefore live in exactly one place (`Default for Config`); the file is
  documentation, not a second source of truth, so a user who never edits it
  silently inherits later default changes. Two comment styles keep this
  machine-checkable: `#key = value` (no space) is a real default, `# text` is
  prose or an example with no default (`[[repositories]]`,
  `[[agents]].shell`). Plain section headers (`[preview]`, `[icons]`, …) are
  left active — an empty table parses to the default, and it means
  uncommenting one key is enough; array-of-table headers (`[[agents]]`,
  `[[messages]]`) stay commented, since an empty entry would *replace* the
  built-in list with a blank one. Three tests hold the file honest: every
  registered key appears in it, uncommenting the whole file reproduces
  `Config::default()` exactly, and the file as shipped parses to the same
  defaults. A generator was rejected — `Config` is not `Serialize`, and no
  generator writes the per-key prose that makes the file worth reading.
- **Drop the smallest unit, never prevent startup**: loading is lenient — a
  whole-file TOML syntax error drops that file's contribution, a bad
  `[[agents]]`/`[[repositories]]` entry drops that entry, a bad scalar or a
  bad section field (one bad binding) defaults on its own, a bad status
  pattern regex is dropped at load, unknown keys are reported not fatal. Every
  drop lands in `Config.diagnostics` with what/where/used-instead and is shown
  ONCE in a startup diagnostics overlay (Esc/q/Enter dismiss,
  j/k/g/G/PageUp/Home/End/wheel scroll — the same accessibility code path as
  help and the picker). A startup confirm parked behind the overlay resumes
  after dismissal.
- **`--config-dump`**: prints the effective values with the layer each key came
  from ("key = value  # from <layer>") and exits before the TUI — the
  configuration rule "make the resolved config inspectable".
- **`[[messages]]` replaces the built-ins, like every other list**: a config
  that defines any entry defines the whole list (the layering rule "maps merge
  deeply, lists are replaced outright"). The three defaults — continue, merge
  to `<base_branch>` and drop the worktree, summarise the work so far: unblock,
  integrate, take stock — therefore only apply when no layer mentions
  `[[messages]]`, and they are written out (commented) on first run, so editing
  them is editing a visible list, not fighting an invisible one. Three because
  the modal gives entries `1`-`9` a single-key shortcut and a picker earns
  nothing by listing near-duplicates.

## Help and docs

- **The help overlay is the palette**: the app has one index of everything
  (`?`), grouped by context (navigate / repositories / worktrees / shells /
  global) with the same words as the contextual bar (shared `HELP_GROUPS`
  strings). A fuzzy-search palette was deliberately not built: the action set
  is small enough that a grouped reference + contextual bar covers
  discoverability; the palette is scrollable and the popup sizes to content
  (clip + "j/k scrolls" hint instead of a fixed-size overlay).
- **Everything is documented**: `docs/shortcuts.md` lists every binding with
  the TUI rule it satisfies and every deliberately unbound key with its reason;
  it is updated in the same commit as any binding change.

## Testing

- **One reentrant lock for every process-global test fixture.** Two locks
  (`XDG_LOCK`, `TMUX_TEST_LOCK`) guarding the same kind of thing — process-wide
  env vars — is a lock-order inversion waiting to happen, and it happened: the
  fixtures nest in both orders, so parallel `cargo test` could hang forever.
  The alternative fix, "always take them in the same order", was rejected: it
  is a rule nobody can see from a call site, and the failure mode is a hang
  with no message. One lock (`util::testutil::TEST_LOCK`, taken through
  `with_test_lock`) removes the ordering question entirely. The cost is that
  tmux tests and XDG tests no longer run concurrently with each other — worth
  ~0s on an 8.7s suite, since both were already serialised within their own
  group. Reentrancy is per thread (`DEPTH` + a `DepthReset` drop guard, not a
  `ReentrantLock`) because the fixtures must keep nesting, and because the
  guard has to be released only after the depth is back to 0.
- Interactive back-key firing cannot be simulated here (pty byte-injection is
  blocked by the host guardrail) — binding registration and `select-window` are
  verified separately instead.

## tmux quirks hit and worked around

- `list-panes -t <session>` only lists the active window (use `-a` + filter by
  session_id).
- `new-window` right after `new-session -d` races window index 0 (retry 3x).
- A fake `TMUX` env var makes tmux behave as a nested client and breaks window
  creation.
- `capture-pane` pads blank lines.
