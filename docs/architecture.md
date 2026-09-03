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
- Drafts file: JSON at `$XDG_STATE_HOME/lazywingman/drafts.json` — unsent
  quick-send prompts (`src/drafts.rs`). State, not cache: a half-written
  prompt cannot be regenerated.
- Todo file: **plain text** at `$XDG_STATE_HOME/lazywingman/todo.md` — the
  todo pad (`src/todo.rs`). Not JSON, and not keyed to anything: see "The
  todo pad" below.
- **State schema v4**: `State.version = 4`. `load` reads `version` first and
  discards a well-formed file of any other version, starting empty and
  letting `reconcile` rediscover the repositories and reconstruct their
  worktrees. Malformed JSON is still an error — the user is told rather than
  silently losing the tree. (v3 introduced that rule, renaming the on-disk
  keys with no migration path; v4 keeps it.)
- **No live tmux coordinates in `state.json`.** v4 drops the persisted
  `window_id` / `pane_id`: they are `#[serde(skip)]` runtime fields that
  `reconcile` fills in from one `list-panes`. A coordinate that survived a
  restart names a window that no longer holds the shell — or holds someone
  else's — and the file is a recipe for the tree, never a restore of tmux.
- **A pane carries its own identity.** `dock::ShellId` (repository slug,
  worktree, shell index) is written onto the pane's `@lw_repo` /
  `@lw_worktree` / `@lw_index` options at spawn, and pane options travel with
  the pane across windows and sessions. So `reconcile` groups panes by
  identity rather than walking window names, and a window renamed by hand —
  or a pane sitting in a session that is not its own — costs the tree
  nothing. `Tmux::window_name` stays, purely as the human-facing label.
- **`Repository.shell` is additive, not a schema bump.** The repository's
  inline shell is a `#[serde(default)]` field, so a v4 file written before
  it loads unchanged and simply has none — and the pane, if it is still
  alive, is adopted back by the next `reconcile`. A version bump would have
  thrown away every discovered repository to add one optional field.
- Shell indices are never reused: `Worktree::next_shell_index` counts past
  the highest index in use, because the index is part of a shell's identity
  and is what `reconcile` matches a live pane back to.

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
- **The agent is chosen per worktree, in the popup that creates it.** A
  worktree is one unit of work, and which agent runs it is a property of
  that work, not of the repository holding it — a codex worktree beside a
  claude one is the ordinary case, not a reconfiguration. So `n` grew a
  second field (`Mode::Input.agent`, `Tab` between the two) rather than a
  setting elsewhere: the choice is made where the worktree is made, and
  costs nothing to anyone who does not make it, because the field is
  prefilled with what the repository already runs. What it holds is read
  the way `[[repositories]]` reads its pair of keys — a `[[agents]]` preset
  name first, otherwise the command itself (`Config::command_for_agent`),
  so one field covers both without a second one to explain. The resolved
  command is stored on the worktree (`Worktree.agent_command`, `None` =
  follow the repository, which is what every pre-existing and every
  reconciled worktree says) and from then on it is what the first shell
  runs, what the `a` picker offers first, and which patterns the poll
  classifies its panes by. The window's *shell* stays the repository's
  `agent_shell`: a preset's optional `shell` is a per-repository choice and
  a worktree's shells all share one.
- **`[defaults].agent` names a preset, `[defaults].agent_command` a
  command.** The default agent had only ever been a bare command word, so
  "which agent do I want by default" could not be answered by naming one of
  the `[[agents]]` presets — the very list that carries its status
  patterns. `agent` is now the preset a repository without its own falls
  back to (and the popup's prefill); `agent_command` is what is left when
  that name matches no preset, so an existing config keeps meaning what it
  meant.
- `d` is contextual: on a worktree row it deletes the worktree, on a shell
  row it closes just that shell (kill the window, drop it from the
  worktree). Deleting a worktree from a shell row was the old behaviour and
  the wrong one — "close this agent" is the frequent action, "delete the
  whole worktree" the rare one, and the two must not share a target.
- The delete confirm answers to `d` as well as `y`/`Enter`: `d` opened the
  popup, so `dd` completes the delete without a hand move — the vim reflex.
  `f` (force) stays a separate key: discarding uncommitted work must not be
  reachable by pressing the same key twice. `Enter` is **not** an alias:
  everywhere else it means "open / drill into", and the reflex key must not
  delete anything.
- **An answer that cannot succeed is not offered.** `Confirm::yes_label` is
  `None` on a dirty worktree, so `y`/`d` are unbound and absent from the
  footer — `git worktree remove` refuses without `--force`, and a key whose
  only outcome is an error toast teaches the user nothing. `f` is then the
  single way through, which is also what makes the discard deliberate.
- **The footer names `n`, not `n/esc`.** `Esc` still cancels, as it closes
  every overlay in the app; spelling it out in one dialog implies it is
  local to that dialog.
- **A confirm's caveat is drawn in the failure colour, not as body prose.**
  `Confirm::warning` is a field of its own, rendered bold in
  `status_failed` under the question ("uncommitted changes — force delete
  discards them"). It is the one line that changes which key the user
  should press, so it may not read like the rest of the sentence: the
  colour is what makes the eye stop before the fingers do.
- **"clean" means no work at all, committed or not.** The delete confirm
  used to read the working tree alone (`git status --porcelain`), so a
  worktree with a clean tree and a branch full of unmerged commits was
  announced as "worktree is clean" — the one lie the dialog must not tell,
  since `worktrees.delete_branch` (on by default) then deletes that branch
  with `branch -D`. `App::worktree_has_unmerged_commits` asks the commit
  half separately (`Git::worktree_committed_ahead`, the `fork..HEAD` count
  plus the ancestor test out of `Git::worktree_state`), because `Ahead`
  merges "dirty" and "committed" into one verdict and the dialog has to
  name which. It is asked fresh rather than read off the `wt_state` cache,
  which is polled on an interval and would answer for a worktree as it was
  minutes ago. Unmerged commits do NOT remove the plain `y`: `git worktree
  remove` succeeds on them, so the answer can succeed — it is the warning
  line, not the offered keys, that carries the risk. The body says only
  *whether* the tree is clean, in three beats one blank line apart
  (question / path / state): naming the reason there as well as in the
  warning below ("NOT clean: unmerged commits" over "unmerged commits —
  deleting the branch discards them") read as two findings instead of one.
- **A warning mark is followed by two spaces**, and its wrapped rows indent
  to the same column (`warn_prefix`). `⚠` is East-Asian *ambiguous* width:
  unicode-width — and so ratatui's layout — counts one cell, while plenty
  of terminals draw two, swallowing a single separator and printing
  `⚠unmerged commits`. Two spaces cost one column where the glyph is
  narrow and are correct where it is wide.
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
- **The back-key is retired** (M18). It bound a chord — `switch-client` +
  `select-window` — that jumped from an agent window back to the dashboard
  window. Docking answers that question differently: the agent's pane is
  *already* beside the tree, so "back to the dashboard" is a move between two
  panes of one window, which is `<prefix> + ←` and the user's own binding. It
  also ends a latent bug: tmux key tables are server-wide, so one binding
  served every dashboard on the server and the newest one to start silently
  retargeted the chord for all the others. Gone with it: `bind_back_key`,
  `unbind_legacy_root_back_key`, `legacy_root_back_key`, `App::bind_back_keys`
  and `TmuxConfig`. `[tmux]` stays a *known* config section with no keys, so a
  file still carrying `back_key` is told which key is stray rather than that
  the whole section is — the treatment the retired message `label` key
  established.
- **`a` is a picker, not a command prompt.** Adding a shell used to open a
  single-line prompt for the command, defaulting to the repository's agent
  command. That made the *frequent* choices — the configured agents, and a
  plain interactive shell — cost as much typing as the rare one, and starting
  a normal `zsh` in a worktree meant typing over a prompt pre-explained as an
  agent command. `Mode::AddShell` lists them instead: the repository's agent
  command first (so the old path is still `a` then `Enter`) — the selected
  worktree's own, when it was created with one — the other
  `[[agents]]` presets after it (deduplicated against the default), and a
  plain shell last. The free-text prompt is not replaced by anything, on
  purpose: a one-off command is a command you type in the plain shell, and a
  recurring one belongs in `[[agents]]`, where it also gets status patterns.
- **A plain shell is `$SHELL`, and nothing is sent to it.** `ShellSpawn`
  distinguishes the two spawn paths. `Agent(cmd)` opens the window under
  `[defaults].agent_shell` and sends the (hook-instrumented) command;
  `Plain` opens it under the user's own `$SHELL` — the point of the entry is
  landing in the shell you actually use, which `agent_shell` (`bash`, chosen
  for agents) is not — and sends nothing at all. Sending even an empty line
  would be wrong: the window already *is* the shell. `util::login_shell`
  falls back to `bash` when `$SHELL` is unset or empty (a bare container,
  `env -i`). Status classification needs no change: a login shell as the
  pane's foreground command is exactly the `ended` heuristic's input, and a
  plain shell reading `ended` — "no agent running here" — is accurate.
- **The spawn command is queued, not typed straight away.** `new-window`
  returns as soon as tmux has forked the shell, which is long before that
  shell has read its rc files and started its line editor — a couple of
  seconds on a cold `zsh` with a plugin manager. Keys sent into that gap land
  in the tty's line buffer: the line discipline echoes them raw, the holder
  split that follows reflows the echo at half the width, and the line editor
  draws the line a SECOND time when it finally reads it. That is the messy
  first screen — the command shown twice, or once, or not at all when an rc
  clears the display. So `App::queue_launch` parks the command in
  `pending_launches` and `drain_pending_launches` types it on the first poll
  that finds the pane ready. Readiness is "the pane has drawn something",
  read off `#{cursor_x}`/`#{cursor_y}` in the poll's existing `list-panes`
  format (`PaneInfo::blank`), so waiting costs no extra tmux call: a prompt
  on screen means the line editor is up and will take the keys as a line,
  echo them once and run them. `LAUNCH_GRACE` (8s) is the backstop — a shell
  with no prompt to draw gets typed into blind rather than never starting.
  Only the two *first* commands of a fresh shell go through the queue;
  quick-send and the predefined messages talk to an agent that is already
  running and need no such wait.
- **Always inside tmux — it starts one for you** (M18, replacing "runs inside
  AND outside tmux"). The old answer was that the dashboard did not require
  tmux around itself: it ran standalone against an auto-started server, and
  attaching handed the terminal over to `tmux attach` and came back on detach.
  Docking retires that. The selected shell's *real* pane is swapped in beside
  the tree, and a `swap-pane` needs a pane of ours to swap into — which a plain
  terminal has not got. So `main::should_wrap` decides on two facts, not inside
  tmux and stdin is a tty, and `exec`s into
  `tmux new-session -A -s lazywingman -- lazywingman <argv>`. `exec`, not
  spawn: one process, so signals and the exit code stay the shell's own. No
  recursion guard is needed — the tmux we exec into sets `TMUX`/`TMUX_PANE`,
  which is exactly what `Tmux::detect` reads.
  - A second launch in the same folder must not race a rival dashboard beside
    the first, so the dashboard's window announces its scope (`@lw_scope`,
    stamped at startup) and the wrapper looks it up with one
    `list-windows -a`.
  - **The mark is not evidence that a dashboard is alive**, and reading it as
    if it were was a real bug: nothing unwrites `@lw_scope`, and a TUI that
    quits in a shell pane hands that pane back to the shell — the window, the
    pane and the option all survive. Launching from a bare shell then attached
    the user to a tmux with no dashboard in it. `Tmux::scope_window_state`
    therefore judges the window by what it HOLDS: a pane whose
    `#{pane_current_command}` is our binary → `Live`, attach it (tmux
    recomputes that field from the foreground process every time, so it cannot
    go stale); no dashboard but panes of ours still there — a slot, a holder,
    an agent pane docked in when the dashboard died → `Furniture`,
    `split-window` one back beside panes that never died; neither → `Stale`,
    retract the mark and start somewhere of our own.
  - **`new-session -A` was the same bug by another door.** Given an existing
    session `-A` attaches to it and silently drops the command, so a wrapper
    session that outlived its dashboard would be attached to empty-handed. An
    existing `lazywingman` session gets `new-window` instead; only a session
    that does not exist yet is created with the command inline.
  - `Tmux::ensure_server` went with standalone mode: the wrapper session *is*
    the server, so there is no longer a boot with no server. `list-panes -a` on
    a zero-session server ("no current target") still reads as an empty pane
    list, which is what a poll before the first agent session sees.
  - `Tmux::inside` stays an `Option` even though it is `Some` for the whole
    life of the TUI: the `hook` subcommand runs on the same type, in panes that
    are not ours.
  - The one thing not wrapped is a launch with no terminal to hand tmux —
    piped, redirected, cron. Wrapping there would fail where not wrapping
    merely degrades.

## The repository's inline shell

A repository row used to leave the panel empty: it had no shell, so there
was nothing to dock and nothing to preview. It is also the one row that
names a checkout nobody is agenting in — the base branch, the working copy
the worktrees were cut from — which is exactly where `make`, `mise run …`
and `cat config.toml` belong. So the row got a shell of its own.

- **One shell, on the repository, not a worktree row.** `Repository.shell`
  is an `Option<Shell>`. Modelling the main checkout as a `Worktree` was
  the obvious alternative and is wrong twice over: it would put a row in
  the tree for something the app never created and must never delete, and
  `d` on that row would then read as "delete the main checkout".
- **The identity scheme is reused, not extended.** The pane is stamped
  `@lw_repo` / `@lw_worktree` / `@lw_index` like every other shell, with
  `dock::REPO_WORKTREE` (`.repo`) as the worktree component. The sentinel
  is safe by construction: git refuses a ref whose path component begins
  with a dot, so no branch can sanitise to `.repo` and no worktree can
  collide with it. Reconcile, prune, the leases, the holder/slot furniture
  and `swap-pane` therefore all work on it unchanged — there is no second
  mechanism beside the first.
- **Lazy, and only on `Enter`.** A shell costs two or three seconds, and
  most repository rows are only ever expanded to look at what hangs below
  them. Docking on *look* would pay that for every row the cursor passes
  over, so `App::auto_dock` docks an inline shell that already exists and
  never starts one. `Enter` on a repository row keeps toggling the
  disclosure and additionally ensures the shell: the disclosure is about
  the rows below, the shell about the panel beside them, and the two do not
  compete for the key.
- **It is `$SHELL`, and nothing is sent to it.** `[defaults].agent_shell`
  (`bash`, chosen for agents) is not the shell anyone wants to type `make`
  in — the same reasoning that made a plain worktree shell use
  `util::login_shell`. Nothing is sent for the same reason as there: the
  window already *is* the shell.
- **The directory is checked at the moment of the spawn.** `missing` is a
  poll's verdict and can be seconds old; a shell opened in a path that is
  not there lands wherever tmux decides to put it, which is the one answer
  nobody wants from "run a command in this repository". So
  `ensure_repo_shell` asks `path.is_dir()` itself.
- **`selected_shell_pane` answers on a repository row;
  `selected_shell` does not.** The pane is what docking, the preview and
  the lease need, and giving the row a pane makes all three follow it for
  free. The `(repository, worktree, shell)` triple is what the *send* paths
  take, and it stays `None` — so `s` and `p` are unbound on the row. The
  presets are agent prompts, and a command you want a shell to run is a
  command you type in it.
- **`d` closes it, and only appears when there is one.** The bar offers
  `close d` on a repository row exactly while `repo.shell` is live; with
  none, `d` falls through to the worktree path, which says what to select
  instead. `Enter` afterwards starts a fresh one — closing is not a state
  you can get stuck in.
- **The empty panel is where the feature is discoverable.** A repository
  row with no shell yet is the one row whose panel is blank because nothing
  has been started, so the preview says so and names the key. It is the
  same treatment a worktree with no shell gets, and for the same reason:
  never a silent hole.
- **Test fallout, and what it revealed.** Headless tests used a single
  shared tmux socket named `unused`, on the assumption that nothing would
  ever run a command on it. `Enter` on a repository row falsified that: a
  navigation test now legitimately started a session there, and the next
  test to `capture-pane` on that socket found it. The assumption was never
  the test's to make, so `tmux::testutil::tmux_nowhere` hands out a socket
  of its own per call (`dead_socket`), and `kill_socket` cleans up after
  the one test that deliberately spawns.

## Docking

Docking is the answer to "stop emulating the agent's terminal": the selected
shell's **real** tmux pane sits beside the tree, so direct keys, copy-mode
scrolling, search, selection and resize are tmux's own, confined to that pane,
with nothing re-implemented. Four invariants hold the whole thing up.

- **Pane options are identity.** A shell is its `@lw_repo` / `@lw_worktree` /
  `@lw_index`, written at spawn and travelling with the pane across windows
  and sessions. Window names are labels; a pane's own options are what
  `reconcile` groups by, which is why a docked pane sitting in the dashboard's
  session is still found, and why renaming a window by hand costs nothing.
- **Windows are storage.** A tmux window is the only place a pane can be alive
  and off-screen, and the nominal case is many hidden agents. What M18 retired
  is the window *name* as an identity mechanism, not the window.
- **No live tmux coordinates in `state.json`.** Panes, window ids and indices
  are re-derived from one `list-panes -a` per poll; the file keeps
  repositories and worktrees. A coordinate that survived a restart would name
  a window that no longer holds this shell — or holds someone else's.
- **Anything shared between instances lives in tmux, and is re-read every
  poll.** Two dashboards have no channel to each other except the tmux server.
  So the lease is a pane option (`@lw_docked`), never a field in the state
  file, and it is read fresh on every poll rather than cached across one.

### Why `swap-pane`, and not `join-pane`/`break-pane`

Moving a pane out of a single-pane window destroys the window, and destroying
an agent session's last window destroys the session — which would take out
rediscovery, undock's target, and the "dead = gone from tmux" model all at
once. A swap never changes any window's pane count, so that entire failure
class does not exist.

That is why every agent window carries a **holder** and the dashboard window
carries a **slot**: two sleeping panes so that docking is one `swap-pane`
between panes that already exist. Agent sessions are set to `window-size
manual` and their windows held at the dashboard window's size, so the two
panes are the same size and tmux redraws from its own buffer — no `SIGWINCH`,
no repaint. The holder also does UX work: someone who attaches to an agent
session directly finds a pane saying where their agent went.

Two degradations are accepted rather than fixed. Attaching an agent session
directly resizes its windows, so the next dock costs one repaint. And a holder
is one sleeping process per shell, visible in `list-panes` and in the user's
own pane navigation — marked once and filtered once.

Those sizes are only re-cut when the dashboard's *window* size changes
(`App::refresh_dash_window`), so a pane the user dragged — or a stray
`resize-pane` — stays dragged: nothing in a steady-state poll disagrees with
it. `R` (`App::reset_layout`) is the explicit way back: it re-cuts the
dashboard's own strip to `dock::dash_width`, puts every agent window at the
dashboard window's size, re-cuts every holder and backfills a missing one.
It is a repair of the furniture only — what is docked stays docked — and it
leaves the dashboard's pane alone when nothing of ours sits beside it, since
an undocked dashboard is a guest in the user's own window and its width is
theirs to set.

### The lease: looking yields, pressing steals

Eager stealing plus automatic docking would ping-pong: two idle instances with
the same shell selected would take it from each other forever, with nobody
pressing a key. So the trigger decides. A selection-driven dock that meets a
live lease **yields** and shows the preview instead; an explicit `Enter`
**steals**, unconfirmed, with a toast on both sides. The steal is
undock-then-dock rather than a direct swap, so each instance keeps its own
slot.

A `@lw_docked` mark is only a lease while the dashboard pane it names is still
alive: a mark left by an instance that crashed is wreckage, and treating it as
a lease would fence the shell off for good.

### The preview stays

Both renderers are maintained on purpose. The preview is the browse mode, the
fallback for a shell another dashboard holds, and the only view below the
split tier. What a docked dashboard does *not* do is draw both: a second,
narrower, one-poll-behind copy of the agent beside the agent is the exact
duplication docking exists to end.

### The rest of the decisions

- **Furniture is never a shell, whatever identity is written on it** (M18).
  `Tmux::list_panes` selects panes by identity, and the exclusion of holders
  and slots used to rest on an assumption: furniture carries no `@lw_repo`,
  so it cannot parse as a shell. A pre-v4 identity backfill falsified it —
  it stamped every unstamped pane sitting in a window still named
  `lw:<worktree>/<n>`, and a holder is exactly that. Two panes then claimed
  one shell, `reconcile` kept whichever `list-panes -a` listed last, and a
  dashboard that picked the holder docked a sleeping placeholder and read the
  shell as `ended`. Which one won depended on session ordering, so the shell
  that broke was a *docked* one — its agent pane had moved to another session.
  That backfill is gone (see below), but the assumption it broke is not worth
  re-betting on: `list_panes` now treats a `@lw_holder`/`@lw_slot` mark as
  outranking any identity on the same pane, so the exclusion is structural and
  no future stamping path can reintroduce the bug.
- **No migration path, by decision** (M18). There was a one-off upgrade —
  `state::backfill_identities` — that read identity out of a pre-v4 window
  name for a tmux server started by an older build. It is deleted, along with
  `parse_window_name` and `Tmux::list_unstamped_panes`. Agent panes die with
  their tmux server anyway (see "Reboot restores nothing"), so the cost of
  not migrating is restarting the shells once, and the cost of migrating was
  a stamping path that judged panes by where they sat — which is what wrote a
  shell's identity onto a holder.
- **The dock follows the selection, and the cursor does not follow the dock**
  (M18). Landing on a shell row docks its pane; `Enter` is then only needed to
  *enter* it. An auto-dock that also ran `select-pane` would move the keyboard
  into the agent and the next `j` would be typed at the agent, so `App::dock_pane`
  takes a `focus` flag: `Enter` sets it, looking does not.
  - **No settle timer — the input queue is the debouncer.** The plan called for
    a ~150 ms settle so held `j` autorepeat could not fire one swap per row.
    A settle has a cost the plan did not price: for its whole length the panel
    shows the row the cursor has already left, and a docked dashboard has no
    preview to fall back to in the meantime (the slot is where the preview
    used to be). So the burst is *coalesced* instead: `App::auto_dock` takes
    the event loop's own `event::poll(ZERO)` answer to "are more keys already
    waiting", which autorepeat keeps true for as long as the key is held. One
    dock at the end of the run, and zero added latency for a single keypress.
  - **The panel owes the selection one of two things** — the selected shell's
    real pane, or the preview — and never a third shell's pane left over from
    a row the cursor has left. A leftover reads as the selected agent, which
    is the one thing a live pane must never be mistaken for. So a row with
    nothing to dock (no shell, or a shell another dashboard holds) drops the
    slot and gives the space back to the preview, whose title names the
    holder when there is one.
  - **No undocked mode.** Dock-on-look used to wait to be armed by the first
    keypress, so that startup did not split furniture into what may well be
    the user's own window. That bought an "undocked" state that offers
    nothing the docked one does not, that no key returns to, and that hid a
    second dashboard's lease from the first until someone pressed `j`. The
    selected shell is docked from the first frame.
  - **One refusal.** Nothing moves — neither a dock nor the undock above —
    while the docked pane holds the keyboard: `Tmux::pane_active` on our own
    pane, asked at the moment of the decision rather than cached from the
    last poll, because the answer changes the instant the user moves between
    panes.
  - **The lease is a pane option, re-read every poll.** `@lw_docked` on the
    agent pane holds the docking dashboard's own pane id — the only thing two
    instances that know nothing of each other can both see. `App::refresh_leases`
    rebuilds `App::leases` from the poll's pane list, keeps only marks whose
    named dashboard pane is still alive (a mark from an instance that crashed
    is wreckage, and treating it as a lease would fence the shell off for
    good), and resolves the holder's `@lw_scope` so the UI can say *whose* it
    is. A leased shell's row is muted whole and marked `leased`, the way a
    dead row mutes and for the same reason — it is not a row this instance
    brings on screen by looking at it. The preview keeps working (`capture-pane`
    does not care where a pane sits) and its title names the holder, because a
    panel that silently follows a shell someone else is typing in invites an
    `Enter` that yanks it away.
  - **`Enter` steals, unconfirmed — and the steal is undock-then-dock.** The
    case is rare and the answer is always yes: someone pressed the key on the
    row. A dialog would cost every ordinary dock the habit of dismissing one,
    so both sides are toasted instead — the taker names the scope it took the
    shell from, the loser's next poll names the scope that took it. The pane
    is sent *home* before it is taken rather than swapped straight out of the
    other dashboard's window: a direct swap would leave their slot in our
    window and ours in the agent's, crossing the furniture over and letting
    one instance kill a pane in another's window on quit. Undock-then-dock
    keeps the invariant that a slot is only ever in its own dashboard's window
    or in the home window of the pane that dashboard has docked.
  - **The slot exists exactly while something is docked.** `ui::tier` tests
    `App::docked`, not `App::slot`: when a dock ends without a replacement —
    the agent exited, the pane was killed, another instance took it —
    `App::drop_slot` gives the dashboard its whole window back and the preview
    returns with it. The preview is the browse mode and the only view that
    works for a shell we do not hold, so it must not be a slot-shaped hole.
  - **Deleting a docked shell undocks it first.** `d` on a shell or worktree
    kills the shell's *home* window — which, while that shell is docked,
    holds the slot and not the agent pane. The kill would take the furniture
    and leave the agent pane living in the dashboard's own window, still
    drawn beside the tree with no row left to name it, and `reconcile_docked`
    would not object because the lease is still ours and still valid. So
    `App::release_dock_before_kill` runs first, on the window id rather than
    the pane id (`docked_home` is exactly "the window the kill is about to
    remove"), and the agent pane dies with its own window.

- **A poll stamps its own clock** (M18 follow-up). `App::poll` did not touch
  `App::last_poll`, so the event loop's "is a poll due" test stayed true for
  ever once the first interval had passed: `until_poll` saturated to zero, the
  input timeout with it, and the dashboard span-polled — a `capture-pane` per
  shell per frame, and a failing dock retried about ninety times a second (the
  log is how this was found). The stamp is the first statement in `poll`, and
  unconditional: every early return below it is a reason to wait for the next
  tick, not to try again on the next frame.

## Status classification

- Status model: `dead | ended | failed | blocked | waiting | working | idle`.
  `dead` = pane gone from tmux (the shell exited — tmux destroys the pane
  unless `remain-on-exit` is set) or pane_dead; `ended` = live pane whose
  foreground command is a login shell (agent exited); failed/waiting/working =
  regex on `capture-pane` tail; `blocked` comes from an instrumented agent's
  own hook report; `idle` = default.
- Classification order (see the module doc on `src/status.rs`): dead → the
  agent's own hook-reported state, when the pane carries a live `@lw_agent`
  overlay AND an agent is still the foreground command → ended (the
  foreground command is a login shell) → failed → waiting if the LAST line
  matches a prompt pattern (the freshest signal) → working → waiting
  anywhere → idle. Two guards keep the overlay honest: the
  login-shell check drops a value stranded by an agent that died without
  firing SessionEnd (the option lives in the tmux server, so it would
  otherwise outlive the process that wrote it), and an overlay past
  `OVERLAY_STALE_AFTER_SECS` is only distrusted when the tail ALSO looks idle
  — either condition alone would misclassify a legitimately long turn.
- The shell-foreground heuristic is checked BEFORE the regex ladder (it was
  the other way round until the `ended` state turned out to be unreachable
  in practice). tmux's `pane_current_command` is a fact about what runs
  now; the captured tail is scrollback that outlives the process that wrote
  it, and a finished agent leaves its prompt glyph, spinner frames and the
  word "error" on screen, so the ladder kept answering `waiting`/`working`/
  `failed` for a pane whose agent had quit minutes ago. The trade is
  accepted deliberately: a failure the agent printed on its way out now
  reads `ended`, and the preview panel is where that text is still legible.
  An agent launched FROM a shell is unaffected — while it runs it *is* the
  pane's foreground command; a shell in front means nothing else is left.
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
  docks it, `s` quick-sends, the preview follows it). Making it "collapsed by
  default" instead would still cost a keypress to see a row that repeats the
  worktree's name and nothing else. The rule lives in one predicate,
  `App::solo_shell`, and is enforced in `visible_nodes()` so navigation, mouse
  hits and `clamp_sel` cannot disagree with what is drawn. `a` (add shell) is
  the way out: two shells bring the subtree back.
- **Creating something reveals it.** `create_worktree` unfolds the
  repository it just added to, and gives the new worktree the same unfolded
  flag discovery gives the rows it finds. A collapsed repository emits no
  worktree rows at all (`visible_nodes`), so without this the worktree just
  created would not be drawn and `clamp_sel` would drop the selection back to
  the first row — the creation would read as a no-op. It bites hardest on an
  *empty* repository: its icon is the outline glyph whatever its fold flag
  says, so `l` on it toggles a state nothing renders, and `n` right after
  would then create into a repository the user believes is open.

- **Tree rows carry no counts** (`(2 worktrees)`, `[2 shells]` are gone): the
  tree is expanded by default, so the children are the count and the text
  only repeated what was one row below. The one thing a *folded* repository
  row cannot show otherwise — whether it holds anything at all — moved into
  its icon, the same trick the worktree icon uses. **Expand markers are gone
  from every row** (repositories first, then worktrees): the icon already
  occupies that slot, and the guides below a row say whether it is open far
  more loudly than a 1-column arrow did — a collapsed row simply has no
  children under it.
- **The repository icon is a folder in three states**: `󰝰` open (expanded),
  `󰉋` closed (folded), `󰉖` outline (no worktrees). Folding is the one state
  change that leaves no other trace once the children are gone, so the glyph
  that replaced the expand marker had to absorb it; a folder is the shape
  every file tree already uses for exactly this, so it needs no legend. An
  empty repository has nothing to open, which is why it is one glyph and not
  two. `h`/`l` still fold and
  unfold; the arrow was a second answer to a question the row already
  answered.
- **Worktree state: four states, one icon.** A worktree row's icon
  answers "has anything happened here yet?" — `worktree_new` (untouched:
  no commits of its own and a clean tree), `worktree_ahead` (work the base
  branch does not have), `worktree_merged` (all its commits are in the
  base and the tree is clean), `worktree_deleted` (the directory is gone).
  Colour follows from the status palette rather than new theme keys: dim /
  `waiting` / green / `failed`, so every theme already answers for it.
  Merged is the one exception, and it earned a key of its own: it used to
  borrow `working`, and once `status_working` started borrowing
  `foreground` (agents at work need no alarm colour) "merged" was drawn in
  ordinary text and vanished into the tree. `worktree_merged` restores the
  green it is supposed to have, and itself borrows `tree_worktree` — green
  in every shipped theme — so no theme file had to change and a user's
  existing theme copy picks it up unedited. `deleted` borrows `failed` because it is the one status role every
  shipped theme keeps distinct from the other three (`dead` and `idle`
  both fall back to `dim`, which is `new`), and the row *is* broken: it
  cannot be docked, sent to or previewed, only removed.
- **A settled worktree is muted whole and drops its status.** Once a
  worktree is merged or its directory is gone, the only decision left is
  whether to remove it — what the agent inside was doing is history. So
  those two states dim the worktree name and omit the solo shell's status
  label entirely (a dimmed `waiting` still said something the user can act
  on, and there is nothing to act on), leaving the state glyph as the
  single span that keeps a colour (it also keeps it when the row would
  otherwise mute for having no shells or only dead ones — muting it too
  would leave nothing saying why the row is quiet). The shells of a
  multi-shell settled worktree still show their own status on their own
  rows: that is where a live agent is still addressable. The rule this
  fixes: colour is for what needs the user, and a merged worktree parked
  on `waiting` was pulling attention with the brightest role in the
  palette.
  - **Uncommitted changes ARE folded in** (user's call, reversing the
    earlier rule): an agent that has edited a worktree but not committed
    has plainly done something there, and drawing it with the "nothing
    here yet" glyph hid live work. Dirty therefore reads as `ahead`
    whether or not there are commits, and a merged worktree with new
    uncommitted work goes back to `ahead` — it is no longer free to
    delete. The cost is one `git status --porcelain` per worktree per
    poll, and only when the commit verdict is `new` or `merged`: a
    worktree already `ahead` cannot be moved by the answer, so the common
    case skips the call. Off the render path either way (`poll_worktree_
    state`, every 5s). The same check still runs on demand for the delete
    confirm's `--force` option.
  - **The fork point is recorded at creation** (`Worktree::base_commit`).
    This is not incidental: once a branch is merged, `base..HEAD` is empty
    *exactly* as it is for a fresh cut, so the two are indistinguishable
    from the base branch alone. Storing the commit the worktree was cut
    from makes "has it committed anything of its own?" answerable. A
    reconciled worktree (one LazyWingman discovered rather than created)
    has no fork point, falls back to the base branch, and reads a merged
    worktree as new — stated in the README rather than papered over.
  - **A vanished directory is a state, not an error** (user's call: "a
    gone directory should not just be unknown but rather deleted").
    `Git::worktree_state` checks the path before it runs git and answers
    `Deleted`; without that check every git call in a path that is not
    there fails, `poll_worktree_state`'s `unwrap_or_default()` collapses
    the failure to `WorktreeState::default()` — which is `New` — and a
    worktree an agent merged and cleaned up after itself draws as a fresh
    cut. That is the worst possible lie for this icon: it is the one state
    that means "nothing has happened here". The row survives its directory
    on purpose (its shells may still be alive), so `d` stays the way out.
  - **Merged is measured against the LOCAL base branch**, no fetching: a
    background `git fetch` is network I/O the user did not ask for and can
    hang on a slow remote. The cost is that a PR merged on the forge shows
    as ahead until the merge reaches the local base.
  - **Squash-merged reads as ahead.** Object ids differ after a squash, and
    the alternative (patch-id matching via `git cherry`) is wrong as often
    as it is right on a squashed history. Under-claiming beats calling a
    branch merged when it is not.
  - Cached in `App::wt_state`, refilled by `poll_worktree_state` every
    `WT_STATE_INTERVAL` (5s) and forced after a create/delete — two or
    three git calls per worktree must never sit on the render path. The
    base branch is resolved once per repository, not once per worktree.
- **Dead entries are muted whole** (user feedback: "when a shell is
  closed/dead, mute the whole entry in the tree"). Colouring only the status
  word left a dead row as visually loud as a working one, so the eye had to
  read the end of every row to find what was still alive. A dead row draws
  entirely in `status_dead`, and the rule aggregates upward (worktree dead =
  all its shells dead; repository dead = all its worktrees dead) so the muting
  matches how the tree folds. Empty containers are excluded: a worktree with
  no shells is unstarted, not finished.
- **One icon per agent status, in front of the label** (user's call:
  "improve focus/highlights"). Seven words down a column all read alike at a
  glance; a glyph does not. `working` is a spinner (`status_spinner`, a
  string of frames), `waiting`/`ended` a pause mark, `blocked` a stop mark,
  `failed` an error mark. The label stays next to it — pause and stop are
  not self-evident, and the word is what settles an ambiguous glyph. `idle`
  and `dead` draw nothing on purpose: a state the user cannot act on must
  not compete for attention. All of them (spinner frames included) share one
  padded column via the existing `pad_group`, so a blank icon still holds
  its slot and the animation cannot make the label jitter.
- **The spinner runs off the redraw the loop already does.** Its frame is
  `elapsed / SPINNER_FRAME_MS % frames`, read once per frame from
  `App::started`, so there is no timer, no extra wakeup and no per-row
  state — and every working row animates in step, which reads as one
  dashboard ticking rather than N unrelated ones. The frame lasts 120 ms
  against the event loop's ~100 ms redraw floor (`event_loop` caps its poll
  timeout); a shorter frame would only stutter.
- **Status colours are borrowed unless the state is one you act on**
  (user's call: "rework the colors", then "waiting yellow, blocked
  yellow/orange, ended purple"). Every role but `waiting` and `failed` is
  an `Option<Spec>` with a fallback — `working`/`ended` → `foreground`,
  `idle`/`dead` → `dim`, `blocked` → `status_waiting` — so a theme file can
  set all seven or only two and both load. The shipped themes then spend a
  colour on exactly the two extra states a user has to spot from across the
  tree: `blocked` gets the palette's orange (next to waiting's yellow: same
  "wants you" family, still its own hue, because blocked owes an answer now)
  and `ended` its purple (the agent is gone; reading as plain `foreground`
  made a finished shell look like a working one). `working` stays borrowed:
  the spinner already says it is running, so it stopped shouting in green.
- **The selected row is tinted, not repainted** (user's call: "when a
  worktree is hovered, it should not lose its text color"). The tree's
  `highlight_style` sets `selection_bg` only. Every row in the tree carries
  meaning in its foreground — the status role, the muting of a dead shell,
  the worktree's state — so `on(selection_fg, selection_bg)` erased
  exactly the signal the cursor was moved there to read. Modal lists keep
  the full inversion: their rows have no colour of their own to protect,
  which is why `selection_fg` survives as a role.
- **A worktree with no shell is dimmed, and that is the whole signal.**
  `ensure_worktrees` lists every worktree the repo has, so rows appear for
  worktrees LazyWingman never created. The row state worth showing is not *who
  made it* but *whether there is a tmux window for it*: with no window there
  is nothing to dock, send to or capture, whoever made the worktree. So
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
  `select-window` calls. It is no longer in `state.json` either (v4 keeps no
  live tmux coordinates); the log is where a debugging session should look
  for it.

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

- **The wheel belongs to the preview, and once a pane is docked it belongs to
  tmux** (rewritten for M18). The original reasoning still holds for the
  preview: the tree is the only panel with a cursor, so a wheel that moves it
  loses your place the moment you look back at what an agent printed, and the
  preview is the panel with something to scroll. In `Mode::Normal` the wheel
  therefore always drives the preview, wherever the pointer is — no hover
  hit-testing, no focus concept to learn. An open overlay still owns the
  viewport (help, picker, diagnostics, messages), and below the split tier —
  where the layout has already dropped the preview — the wheel falls back to
  the tree rather than becoming dead.

  What M18 changes is the claim underneath it, that there is "no focus concept
  to learn". Once a shell is docked there really are two focusable things on
  screen, and the second is a real tmux pane. That focus is **tmux's**, not one
  we invented: the divider is a pane divider, the mouse lands in whichever pane
  it is over because tmux's own mouse handling puts it there, and the wheel
  scrolls the agent's scrollback through copy mode without this code seeing the
  event at all. So the rule is unchanged where it applies — the dashboard's own
  strip, where the preview is the only scrollable panel — and simply does not
  reach across the divider. The keyboard is untouched either way: `j`/`k` never
  change meaning under you, and they never reach a docked agent, because
  looking at a row does not move the cursor into it.
- **Parking takes its own deep capture.** The poll captures 120 lines per pane
  because that is what `status::classify` needs; scrolling through four
  screenfuls and hitting a wall would read as a bug. Leaving the live tail
  therefore fetches `capture-pane -S -` **once**, for the selected pane only
  (`Tmux::capture_pane_full`), into `App::preview_buf`. Polls never touch that
  buffer, which is also what freezes the view: the text cannot move under the
  reader while an agent keeps printing. A failed deep capture is not an error
  — the poll's shallow capture is still scrollable, just shorter.
- **The parked view belongs to a pane, and says so.** `sel` is assigned in
  half a dozen places; resetting the scroll at each of them is a bug waiting
  for the seventh. Instead `App::preview_pane` records which pane the buffer
  came from and `App::preview_offset()` reports `0` whenever the selection has
  moved on, so "the preview follows the tree" is a property of the read, not a
  duty of every writer. A panel that has silently stopped following the agent
  reads as a stalled agent, so the parked state is marked three ways: the
  offset in the title, the focused border, and a `live Esc` entry that leads
  the statusbar. `Esc` leaves the parked preview before it leaves the app
  (innermost first); `q` and `ctrl-c` still quit outright.

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
  multi-line text as one paste, then a trailing Enter submits. The
  new-worktree input stays single-line (`Enter` submits) — it is a name, not
  a prompt.
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
  and `s` resumes it, cursor included. Every other way out of the editor
  parks it too, through the one `close_textarea` helper: the ctrl-t theme
  chord (it dispatches over the textarea) and a click outside the popup —
  both used to replace the mode and take the text with it. Two deliberate
  limits: drafts do NOT go
  into `state.json` — they are working state, not the tree, so they live in
  their own `$XDG_STATE_HOME/lazywingman/drafts.json` (`src/drafts.rs`) that
  can be dropped whole when it is corrupt or stale without taking the
  repositories with it — and a draft is
  dropped only when the send actually SUCCEEDED. `submit_input` toasts and
  swallows its error, so clearing on submit would lose the text exactly when
  the user most needs it (dead pane, no server). `TEXTBUF_MAX_BYTES =
  1_000_000` is a runaway-paste backstop, not a limit anyone types into; past
  it input is dropped silently, because an error toast per keystroke would be
  worse than a buffer that stops growing.
- **A draft's on-disk key is the shell, not the pane.** Drafts persist across
  runs, so the key has to survive one: pane ids do not (tmux hands out new
  ones, and `reconcile` rediscovers shells from the panes' own identity), so
  `drafts.json` is keyed by that identity — repository, worktree branch,
  shell index — and `App::load_drafts` resolves it back to the
  current pane id AFTER `reconcile` has filled them in. Lifetime follows
  from the same choice: `save_drafts` writes by walking the tree, so a draft
  whose shell or worktree is gone has no row to be written under and simply
  dies with it — no timestamps, no expiry. It is written on every change
  (park, delivery, and every `save_state`) rather than on quit alone,
  because a dashboard killed with its terminal never reaches a quit path.
  `Drafts::load` never errors: a missing, malformed, or wrong-version file
  means "no drafts" and a log line — refusing to start over a parked prompt
  would be the bigger loss.
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
- **The merge preset names the LOCAL base**: `merge to LOCAL <base_branch>`.
  A bare "merge to main" reads as satisfied by fast-forwarding the remote
  (`git push origin <branch>:main`), which is a real temptation for an agent
  working in a worktree — the base branch is usually checked out in another
  worktree, so git refuses to move it from here. The result looks merged on
  the forge and is not merged for the dashboard: `worktree_state` asks
  `merge-base --is-ancestor HEAD <base>` against the LOCAL branch, so the row
  keeps reading `Ahead` until someone pulls. The preset asks for the thing
  the tree can actually see, and shouts the word the agent has to notice.

## The todo pad

- **What it is for.** Supervising several agents means constantly noticing
  work that cannot start yet — "rebase the API worktree once the schema
  lands", "delete wt3 after the release". That note has nowhere to go: it is
  not a message to an agent (nothing can act on it), and it is not state the
  tree can derive. `t` opens a scratch pad for exactly it.
- **It is global, deliberately.** One file for the machine, not one per
  scope. A note is written while looking at one repository and needed while
  looking at another; a per-directory pad would hide it exactly then. This is
  the opposite call from `drafts.json`, and for the opposite reason: a draft
  belongs to the shell it was typed at and dies with it, while a todo belongs
  to the user.
- **Plain text, no schema.** `todo.md` holds the buffer and nothing else — no
  cursor, no version. A file with no schema cannot have the wrong one, and
  the point of a global pad is that it is also reachable without the
  dashboard (`$EDITOR ~/.local/state/lazywingman/todo.md`); the next `t`
  re-reads it, so an edit made outside is picked up rather than clobbered.
  The cursor is a within-run convenience kept in `App::todo`. Written the way
  state is (temp file, rename over) — two dashboards share it, so
  last-writer-wins is the rule, but a half-written pad is not something the
  user can get back.
- **A panel split off the tree's column — not an overlay.** The pad is a
  `Mode` like any other, but `draw_left` splits the left column and draws it
  under the tree instead of on top of anything: a modal would cover the
  preview, or the docked agent, that the note is being written *about*, and
  taking the tree's whole panel covered the tree the note is written *from*.
  The panel is the editor, so it owns every printable key (`q` types a `q`)
  and the accessibility layer yields its motions the same way it does for the
  textarea. `Esc` is the way back, and it saves — a scratch pad that can lose
  a note is not one.
- **Two heights, one rule: a panel costs what it holds.** `ui::todo_panel_h`
  is the single source for the geometry — rendering, the caret's window
  (`app::todo_visible_lines`), the tree's page size (`ui::tree_rows`) and
  mouse hit-testing (`ui::todo_panel_rows`) all read it, so a click always
  lands on the panel the eye sees. Open, the pad gets **half the column**,
  because that is what writing needs. Parked, it gets **as many rows as the
  note has lines** (five lines of text is a five-line panel), capped at that
  same half so a long note cannot push the tree off the screen; past the cap
  the title owns up to what is below the fold (`Todo ↓12`). Empty and parked,
  it is not drawn at all.
- **`Esc` collapses the pad, it does not close it.** The note stays on
  screen, unfocused and caretless, because a reminder you have to press a key
  to see is not a reminder — and unlike every other `Mode`, leaving this one
  leaves something behind to look at. Only an emptied pad disappears: there
  is nothing left to stand for. A column with no room for two panels is not
  split — an open pad takes it whole (the pre-split behaviour), a parked one
  yields to the tree, which is the panel the user steers with.
- **Saved on every exit, and on the poll.** `close_todo` writes it; so does
  `App::poll` while the dirty flag is up, and so does the event loop on
  `quit`, because ctrl-c quits from any state. The worst a `kill -9` costs is
  one poll interval of typing. `save_todo` reads through the open buffer
  rather than a copy, so the panel never has to clone its text per keystroke.
- **`t`, not `2`.** The LazyGit-style numbered panel stack (`[1]` files,
  `[2]` branches, …) buys nothing with two panels: a number is a position,
  which goes stale the moment a third thing wants the panel, while `t` names
  what it opens. The statusbar carries `todo t` from every row, since the pad
  is not about the selection.

## Layout, wording and rendering

- **The detail panel is gone**: every field it showed was either already in
  the tree (name, status, shell count), config the user wrote themselves
  (path, base branch, agent command, worktree root), or noise (pane geometry,
  process name, a keys line duplicating the statusbar). The branch was
  redundant by construction — a worktree row owns its git worktree and
  therefore its branch. The one fact it held alone, an uncommitted worktree,
  moved into the tree as the worktree icon, which now carries the branch's
  state instead (see "Worktree state" under The tree), so the
  layout is tree | preview.
- **The statusbar is shortcuts and nothing else**: the app name, the session
  chip and the repository count never changed anything the user could act on,
  and the message slot was never cleared, so the last notice stayed on screen
  for the rest of the session. Notices are toasts above the bar with their own
  TTL (4s, 6s for errors) and only errors wear `status_failed`; an ordinary
  notice takes `title_fg` (it used to take `accent`, which is peach in the
  Catppuccin flavours and made "worktree deleted" read like a failure —
  and `accent`/`border_focused` equal `title_bg` in three shipped themes,
  so `title_fg` is the one role guaranteed legible on the toast's own
  background) — the log and the diagnostics overlay stay the
  permanent record. The bar's entries are contextual, clickable, and computed
  per selection; hjkl/Esc/q deliberately take no slot (assumed knowledge).
  The bar wraps onto a second row when the entries do not fit its width
  (two rows is the ceiling — beyond that the tree loses more than the bar
  gains), and each entry's hit range carries the row it landed on, so a
  click on the wrapped row still runs its action. A toast wraps the same
  way, up to three rows: errors are sentences, and a cut one names neither
  what failed nor why.
- **One shape for every shortcut hint**: `<action> <key>` ("cancel esc"),
  with only the key in the accent colour, everywhere the app names a key —
  status bar, modal footers, help palette (`ui::Hint`, `ui::hint_line`). The
  three used to disagree: the bar led with the key (`n worktree`), modal
  footers led with the key and spelled it out in prose (`Enter to submit ·
  Esc to cancel`), the help palette put keys in a left-hand column. Action
  first is what a reader scans for, and one shape means learning the bar
  teaches the modals. Keys with a glyph form (`enter`, `arrows`) resolve
  through `[icons]`, so the `ascii` preset spells them out.
- **Every modal is padded and ends with its shortcuts**: `MODAL_PADDING`
  (one column left and right, nothing vertically) lives on `popup_block`, so
  no overlay can forget it, and every content-sizing calculation adds the
  shared `MODAL_CHROME_W`/`MODAL_CHROME_H`. The padding is horizontal only:
  a top row pushed every modal's first line away from its title for no gain,
  and a bottom row would float the shortcut footer — the last content row —
  off the frame. `MODAL_PADDING` is the single source of truth for that
  geometry: `MODAL_CHROME_W`/`MODAL_CHROME_H` and `modal_inner` (the rect the
  input caret is placed in) are all derived from it, and
  `modal_inner_matches_the_block` asserts the derivation still equals what
  ratatui hands the block. Hand-written offsets drifted from the padding
  twice, each time drawing the caret a row below the text it belonged to.
- **The footer is fenced off by a rule, not a gap**: `draw_footer_rule` paints
  `├`–`─`–`┤` border to border on the row above the shortcuts, so the footer
  reads as its own compartment the way a table header rule separates the
  header from the rows. A line inside the padding would have floated, ending
  a column short of the frame on both sides. Mechanically it paints *over* the
  blank row every modal already reserves above its hint line, so no sizing
  math changed (the footer's height is now measured per modal — see the
  narrow-strip entry below); it writes the symbol and foreground only,
  leaving each cell's background alone, because some overlays fill `popup_bg`
  and some do not. It is called once in `draw`, off `app.popup_area`, so a new
  overlay cannot forget it — the same discipline as `MODAL_PADDING`. The
  "frame too small for this dialog" fallback is the one overlay without it:
  its hint is not on the last content row, so there is nothing to fence.
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
  sits next to each row: the three `[icons].repository*` states on
  repositories, the three `worktree_*` states on worktrees, `shell` on
  shells.
  Mouse hit-testing uses the same tier math as rendering, so the wider guide
  prefixes changed no hit areas.
- **Small terminals**: layout tiers (Wide / Split / TreeOnly / message) at
  fixed thresholds; overlays that don't fit are refused with a "frame too
  small" box instead of drawing garbage; the input cursor is only set when
  on-buffer.
- **Dialog geometry**: the input prompt (new worktree, quick send) and the
  confirm dialog (delete worktree/repository) are sized to
  their content by `dialog_rect` — widest line and line count, plus one
  column/row of `DIALOG_PADDING` inside the border on every side — clamped
  to a `DASH_MIN_WIDTH` floor and the `MODAL_MAX_W` ceiling below. They used
  to take a fixed percentage of the frame (62%×26% for confirms), so a
  two-line question drew a box a quarter of the screen tall with text
  jammed against the border.
- **Overlays are built for the narrow strip, and capped there** (post-M18).
  Docking left the dashboard living in 26–54 columns while a pane is docked
  and in the whole window while none is, and the overlays were built for the
  second one: the help palette was a fixed 82-column table, the dialogs took
  60% of the frame, and every `overlay_min` was wider than the strip — so
  the same dialog was two different dialogs, and `?` was refused outright on
  the layout the app spends its life in. Now:
  - `MODAL_MAX_W` (= `dock::DASH_MAX_WIDTH`) caps *every* overlay, docked or
    not, and every `overlay_min` width is `dock::DASH_MIN_WIDTH`. What is
    left to refuse an overlay is height. A dialog that asks one question
    never needed 200 columns, so the cap costs the undocked layout nothing
    and buys one dialog instead of two.
  - **The palette reflows instead of tabulating.** `help_row` writes
    `<description> <key>` on one line while it fits and stacks the key
    underneath when it does not, with the notes wrapped in dim below —
    always in that order, so the shape is learnable at any width. The
    fixed `HELP_DESC_W`/`HELP_KEY_W` columns are gone with the table.
  - **Footers wrap, and the modal that owns one sizes itself to it.**
    `hint_packing` splits the hints across rows; `footer_h` reports how many
    rows that took, and each modal adds it instead of the old constant. A
    single truncated row would have dropped `cancel esc` — the one hint a
    dialog cannot lose — at exactly the width where it is hardest to guess.
    `draw_footer_rule` takes the row count so the rule still lands directly
    above the hints, which is why the overlay draw functions return a
    `Popup { area, hint_rows }` rather than a bare `Rect`.
  - **Long text wraps, long rows are cut.** Prose (the confirm body, a
    diagnostic, the base-branch hint) is wrapped by `wrap` *before* the
    dialog is sized; list rows (themes, messages, add-shell) are truncated
    with the configured ellipsis, because a wrapped list row would break the
    one-row-per-`1-9`-key correspondence the picker teaches.
  - **The input field carries its label on its own row and scrolls to its
    tail.** "worktree name: " alone is wider than a 26-column field, and the
    caret is always at the end of what was typed, so `tail_to_width` shows
    the end and the caret stays inside the border.
- **Icons**: `[icons]` (`src/config.rs`) covers the tree's type icons and
  guides, the seven agent-status icons, the cursor marker, the separator,
  the warn mark, the ellipsis, the Enter glyph and the border style, chosen
  by a preset (`nerdfont` default, plus `unicode`, `none`) and overridable
  key by key. This
  replaced an `LW_ASCII` boolean, which conflated two independent
  questions — whether the terminal draws box characters, and which glyphs
  the user's font has. Keys that share a rendered column are padded to
  equal width at resolution time, so an empty icon still holds its column.
  `LW_ASCII` is reported as retired in the startup diagnostics.
- **No ASCII fallback** (user's call). There used to be a third dimension
  to all of this: a `preset = "ascii"` glyph set, a `border = "ascii"`
  style, and `ui::asciify` — a post-render pass rewriting box-drawing to
  `+ - |` in every rect, because ratatui has no ASCII `BorderType`. It
  bought support for terminals that cannot draw UTF-8, cost a rewrite hook
  at fourteen render sites plus five whole-frame purity tests, and reached
  further than the app's own frame: over the preview panel and the
  quick-send textarea it rewrote the captured agent output and the user's
  typed buffer too. A tmux-native dashboard for coding agents does not run
  on a terminal that cannot draw `│`. `unicode` is the no-patched-font
  fallback; `none` is the no-icons one.

## Input, mouse and accessibility

- Keybindings: hardcoded vim-ish set (documented in the help overlay,
  `docs/shortcuts.md` and README); no keybinding is configurable
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
  `EnableMouseCapture`/`DisableMouseCapture` around it (and in a panic hook — mouse reporting left on would spray escape
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
  the config dir on first run so the user can edit them. Truecolor when the
  terminal supports it, else 24-bit values quantize to xterm-256; `ansi.toml`
  uses only ANSI-16 colours.
- **Truecolor detection does not stop at `COLORTERM`.** tmux exports it in its
  own panes, so a terminal that sets it nowhere else is the case where it is
  usually absent —
  and treating absence as "256 only" put every theme through the quantiser on
  terminals (Windows Terminal, kitty, VTE, …) that had 24-bit colour all along:
  the colours came out approximated, not respected. With `COLORTERM` unset the
  terminal identifies itself instead — a `-direct` terminfo entry, a
  `TERM`/`TERM_PROGRAM` naming an always-24-bit terminal, a marker variable only
  such a terminal exports (`WT_SESSION`, `KONSOLE_VERSION`, …), or
  `VTE_VERSION >= 3600`. A *set* `COLORTERM` stays final in both directions, and
  anything unrecognised keeps the fallback: over-claiming paints unreadable
  colours, under-claiming only approximate ones. The lists live next to
  `supports_truecolor`, and the decision takes its env lookup as a closure so
  tests need no process-global env mutation.
- **The 256-colour fallback searches the whole palette, cube *and* grey ramp.**
  The fallback now runs only on terminals that really are 256-colour, but a bad
  quantiser is unreadable there. The
  6x6x6 cube's two darkest levels are 0 and 95, so rounding per channel put a
  dark background and a dim foreground on the same index and the text vanished
  into it; `quantize_256` now picks whichever of the nearest cube entry and the
  nearest grey-ramp step (8 + 10i) is closer in squared RGB distance. A test
  asserts no built-in theme collapses a role onto its background in 256
  colours.
- **The Catppuccin set is the reference palette**: all four flavours ship as
  built-ins (official hex values, unmodified, mapped onto the app's roles:
  base → background, text/subtext0 → foreground/dim, surface0-2 →
  selection/borders, flavour accents → status roles, peach → accent on the
  dark flavours and lavender → accent on latte).
  `catppuccin-mocha` is the default (the TUI guideline names it the usual
  default; config files written before this keep their old `theme = "dracula"`
  line and load exactly as before).
- **An accent must be measurably far from the foreground**: `accent` prints
  directly beside `foreground` in every shortcut hint (`<action> <key>`), on
  one or two characters with no space of its own, so the two have to separate
  at a glance. The three dark Catppuccin flavours originally took lavender —
  the swatch that sits next to text in the palette — and measured ΔE 12, 15
  and 20 from their own foreground, which is invisible; they now take peach
  (ΔE 52-58), the only free swatch not already spoken for by a status or tree
  role. `theme::tests::every_builtin_accent_is_distinguishable_from_its_foreground`
  holds every built-in to ΔE ≥ 30. The measure is CIELAB ΔE and not a
  luminance ratio on purpose: gruvbox's yellow accent on cream text rates 1.24
  and is unmistakable, Catppuccin's lavender on text rates 1.24 and is
  invisible — the difference is hue, which only ΔE sees. ANSI-named roles
  (`ansi.toml`) have no measurable value and are skipped.
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
  files with a diagnostic (never silently applied): `worktrees.root` and
  `discovery.scan_root` (machine paths).
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
  (clip + a pinned `scroll j/k` footer instead of a fixed-size overlay). The
  footer is pinned rather than scrolled with the body: a row that says how to
  leave must not be the row that scrolls away, and it is only offered when
  something is actually below the fold.
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
- Interactive key firing inside a *docked* pane cannot be simulated here (pty
  byte-injection is blocked by the host guardrail). What is verified instead is
  everything up to the pane: that the swap puts the right pane in the slot,
  that identity and the lease travel with it, and that `select-pane` lands.
  What happens after that is tmux's, and tmux is not under test.

## tmux quirks hit and worked around

- `list-panes -t <session>` only lists the active window (use `-a` + filter by
  session_id).
- `new-window` right after `new-session -d` races window index 0 (retry 3x).
- A fake `TMUX` env var makes tmux behave as a nested client and breaks window
  creation.
- `capture-pane` pads blank lines.
