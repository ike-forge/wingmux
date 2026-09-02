# LazyWingman

A control centre for supervising coding agents (Claude Code, Codex, Mistral
Vibe, …) across **multiple repositories and multiple worktrees in
parallel**. See everything at a glance, jump into any agent with one
keystroke, spin up and tear down worktrees instantly.

LazyWingman always runs inside tmux, as a pane of your existing session — and
starts one for you when you launch it from a bare shell. Agent shells never
pollute your tmux
session: they live in their own dedicated tmux sessions
(`wingman-session-<repository>`), invisible until you dock one beside the tree.

```
RepositoryA  (a git repo found by discovery)
  ├── master        (the main checkout — untouched)
  ├── worktreeA1    (git worktree + 1..n agent shells)
  ├── worktreeA2
  └── worktreeA3
RepositoryB
  ├── master
  ├── worktreeB1
  └── worktreeB2
```

- **Repository** = an existing local git repo. Imported and removed only;
  never created or deleted by LazyWingman.
- **Worktree** = a git worktree (created automatically) + at least one spawned
  shell running the default agent command.
- **Shell** = a tmux window running a command inside the worktree —
  usually an agent, but useful for any long-running command.

Non-goals (deliberate): no orchestration, no MCP/skill management, no web UI,
no Telegram, no repo creation/deletion, no merge/PR automation. Only worktree
teardown is automated.

## Install

```sh
cargo build --release
# binary at target/release/lazywingman
```

Requirements: Rust ≥ 1.85, `git`, `tmux` **≥ 3.0** — a shell's identity lives
on tmux pane options and is read back with `#{@user-option}` in a format,
which 3.0 is the first release to interpolate. Developed against 3.7b.

```sh
lazywingman
```

Run it from a pane of your tmux session, or from a bare shell — from a bare
shell it re-launches itself inside a tmux session of its own, so it is always
a tmux pane. Move the cursor onto a shell and that agent's **real** tmux pane
appears beside the tree; `Enter` puts the cursor in it, and getting back out
is your own tmux binding (`<prefix> + ←` by default).

First run writes an exhaustive, fully commented config to
`$XDG_CONFIG_HOME/lazywingman/config.toml` (never overwriting an existing
one) and copies the built-in themes to
`$XDG_CONFIG_HOME/lazywingman/themes/`.

LazyWingman finds your repositories for you (see [Repository
discovery](#repository-discovery)), and repositories can carry their own
committed `.lazywingman.toml` so a fresh clone is already configured (see
[Configuration](#configuration)).

## Keybindings

Every binding follows the global TUI conventions (vim/LazyGit tradition) and
is documented with the rule it satisfies in
[`docs/shortcuts.md`](docs/shortcuts.md) — also shown, grouped by context, in
the help overlay (`?`).

| Key | Action |
|-----|--------|
| `j` / `k` (or arrows) | move selection |
| `g` / `G`, `Home` / `End` | first / last item |
| `PgUp` / `PgDn`, `ctrl-u` / `ctrl-d` | half page up / down |
| `ctrl-f` / `ctrl-b` | full page up / down |
| `h` | collapse / jump to parent |
| `l` / `Enter` | expand, or **enter** the selected shell's docked pane (a one-shell worktree goes straight in — see the folded row below). Simply moving onto a shell already docks it; `Enter` is what puts the cursor there |
| `n` | new worktree (git worktree + branch + agent shell) — a prompt asks for the name and names the branch it will be cut from: the repository's **base branch** (`worktrees.base_branch`, per-repository override), never the branch the repo has checked out |
| `a` | add a shell to the selected worktree — a picker asks what it runs: the repository's default agent command first (`Enter` takes it), then the other configured `[[agents]]` presets, then a **plain shell** that just drops you at your own `$SHELL` prompt with nothing sent to it (`j`/`k` moves, `1`-`9` pick directly, `Esc` closes). Anything else you want to run, you type in that plain shell |
| `s` | **quick-send** a line to the selected shell (or to a one-shell worktree's row). Any way out of the editor that is not a send (`Esc`, a click outside the popup, the `ctrl-t` theme chord) keeps the unsent text as that shell's draft; `s` on it again picks up where you left off, cursor included. A draft is dropped once the message is actually delivered. It otherwise survives quitting — parked in `$XDG_STATE_HOME/lazywingman/drafts.json` and kept until its shell is gone |
| `p` | **predefined messages** for the selected shell (or a one-shell worktree's row) — a modal of configured presets, each listed in full with its `<base_branch>`-style tokens already resolved (`j`/`k` moves, `Enter` sends, `1`-`9` send directly, `Esc` closes) |
| `t` | **todo pad** — a scratch pad for the next steps you cannot start yet ("rebase the API worktree once the schema lands"). It opens in the tree's own panel and is edited in place, so the preview — or the docked agent — beside it stays put; `Esc` saves and hands the panel back to the tree, `ctrl-s` saves without leaving. One pad for the whole machine, not one per directory, parked as plain text in `$XDG_STATE_HOME/lazywingman/todo.md` (yours to edit with `$EDITOR` too) |
| `d` | on a worktree: delete it (kills windows, removes worktree; `f` = force when dirty). On a shell row: close that shell (kills its window only — the worktree and its other shells stay) |
| `r` | refresh — reconcile state against reality |
| `R` | **reset the layout** — the dashboard's strip, every agent window and every holder back to their default proportions, after a stray drag or `resize-pane` pulled them out of shape. Nothing is undocked |
| `?` | help overlay (grouped, scrollable: `j`/`k`, `PgUp`/`PgDn`) |
| `q` / `Esc` / `ctrl-c` | quit (ctrl-c works from any state) |
| `Esc` / `q` / `Enter` (startup) | dismiss the startup diagnostics overlay (shown when config problems were dropped at load) |

Mouse: click a row selects, double-click opens (same as `Enter`), the wheel
scrolls the **preview** (never the tree), statusbar entries are clickable, and
clicking outside an overlay dismisses it.

**Docking**: the pane beside the tree is the agent's own tmux pane, swapped
in — not a rendering of it. Moving the cursor onto a shell docks it without
taking your keyboard along, so the next `j` is still a `j`; `Enter` puts the
cursor *in* the pane. From there everything is tmux's own binding and
deliberately unbound here: moving between the panes (`<prefix> + ←` / `→` /
`o` / `;`), scrolling and searching the scrollback, selecting and copying,
zooming (`<prefix> + z`) and resizing. None of the agent's own keys —
`1`/`2`/`3`, typing, image paste — is intercepted on the way in.

Run a second dashboard in another folder and the two share the tmux server:
a shell one has docked shows in the other as `leased` and previews instead,
and `Enter` takes it, telling both sides.

## Configuration

`$XDG_CONFIG_HOME/lazywingman/config.toml` — written on first run, and only if
it isn't there already. It lists every setting LazyWingman understands, each
one commented out at its built-in default, so the file changes nothing until
you edit it. Two comment styles tell the two kinds apart: `#key = value` (no
space after the hash) is a real default — uncommenting it as-is is a no-op —
while `# text` is prose, or an example with no default behind it
(`[[repositories]]`). Section headers like `[preview]` are left active so
uncommenting a single key under one is enough. Example, uncommented:

```toml
theme = "catppuccin-mocha"  # 7 built-ins; ctrl-t switches live (saved to state)
poll_interval_ms = 500       # status/preview refresh
confirm_on_delete = true

[preview]
colors = "full"              # pane colours as-is; "foreground" drops backgrounds, "none" = plain text

[worktrees]
root = ""                    # default: <parent-of-repo>/.lazywingman-worktrees
base_branch = "master"       # falls back to the repo's current branch
delete_branch = true         # also delete the worktree branch on delete

[discovery]
scan_root = ""               # the scope: repositories inside this dir are listed; empty = cwd
max_depth = 4                # how deep a discovery scan walks
skip_dirs = [".git", "node_modules", "target", ".lazywingman-worktrees"]

[defaults]
agent_command = "claude"     # spawned in every new worktree shell by default
agent_shell = "bash"       # shell agent windows run under; a plain shell uses your $SHELL instead
agent_hooks = true           # instrument recognised `claude` commands for accurate status

[icons]                      # full key reference under "Layout" below
preset = "nerdfont"          # "nerdfont" | "unicode" (no patched font) | "none"
worktree_ahead = ""        # any key overrides the preset; "" draws nothing
border = "rounded"           # "rounded" | "plain"

[[messages]]                 # `p` modal: canned prompts for the selected shell
text = "continue"            # listed and sent verbatim, as if you had typed it

[[messages]]                 # tokens resolved against the selected shell:
                             # <base_branch> <branch> <worktree>
                             # <repository> <worktree_path>
text = "merge to LOCAL <base_branch>"

[[messages]]
text = "summarise what you've changed and what's still left"

[[agents]]                   # presets: patterns classify pane status
name = "codex"
command = "codex"
[agents.patterns]
working = ["Working", "Reading", "⠋", "▌"]
waiting = ["❯", "codex>"]
failed = ["Error:", "✖"]

[[repositories]]             # pre-declared repositories are imported at startup
path = "/home/you/work/repo-b"
agent = "codex"              # optional: preset name
agent_command = "codex exec --yolo"  # optional: inline override
base_branch = "develop"      # optional: per-repository override
```

### Layering

Configuration is layered, lowest to highest (closer wins key by key — maps
merge deeply, lists are replaced outright):

1. Built-in defaults
2. Your config — `$XDG_CONFIG_HOME/lazywingman/config.toml` (or
   `--config <path>` / `LW_CONFIG` for the same file elsewhere)
3. Repository-local — `.lazywingman.toml` in the working directory or the
   nearest ancestor holding one (the search stops at a git repository root)
4. Repository-local overrides — `.lazywingman.local.toml` next to it
   (gitignore this one; it wins over the committed repository file)

A repository-local file is meant to be committed — a teammate clones the repo
and is already configured. A few keys belong to the user, not the repository,
and are **ignored in repository files with a diagnostic**: `worktrees.root`
and `discovery.scan_root` (machine-specific paths). Set those in your user
config.

Inspect the resolved config any time:

```
lazywingman --config-dump
theme = "catppuccin-mocha"          # from user
poll_interval_ms = 500              # from user
worktrees.base_branch = "develop"   # from repository
```

### Failure behaviour

A malformed config never prevents startup. The smallest addressable unit is
dropped — one bad `[[agents]]` entry, one bad binding, one bad scalar — and
everything else is kept. Unknown keys are reported, not fatal (an older
binary runs a newer config). Every drop is shown once, at startup, in a
**diagnostics overlay** (`Esc`/`q` dismisses, `j`/`k` scrolls) and logged.

Repositories found by discovery live in the state file, not in the config;
config `[[repositories]]` entries are merged in at startup and consulted for
per-repository agent settings.

## Repository discovery

LazyWingman lists exactly the repositories **inside the current path** — the
scope — and nothing else. Different scopes never mix; repositories you worked
on elsewhere stay saved in state but are hidden until you start there
again. There is no cache and no prompt: finding `.git` entries is a few
stats per directory, done live.

- **Start inside a git repo**: that repo is the scope. It is auto-imported
  and, because it is the only repository, the tree shows its **worktrees
  flat** at the top level (no repository row — the repository is obvious). A
  lone *missing* repository still shows its row so the problem is visible.
- **Start in a folder of repos**: every repo inside it is listed (walk is
  bounded by `max_depth`, skips hidden dirs and `skip_dirs`; linked-worktree
  `.git` files are never treated as repos — their main repo covers them).
  Everything is unfolded so the shells are visible straight away; past 30
  repositories in scope the tree starts collapsed instead, because it would be
  longer than any terminal.
- **Worktrees load lazily, and in the background**: `git worktree list` is
  the expensive part, so it runs once per repository — when you expand it, or
  from the boot queue. Boot itself only waits for the first repository (the
  one the first frame draws); the rest are drained one per frame, so the
  tree appears immediately and fills in. Listing 200 repos costs 200
  directory stats, not 200 git calls.
- **Discovery runs at startup**, not on a key. A repo created while
  LazyWingman is running appears the next time you start it in that scope; `r`
  refreshes tmux state (windows, panes, worktrees), not the repo list. To add
  a repository that is not in the scope, declare it under `[[repositories]]`
  in `config.toml`.
- **Missing repositories** are kept, not pruned: a repository whose path is
  currently not a git repo shows `(missing)` dimmed in the tree and
  re-activates automatically when the path is a repo again. If it is gone
  for good, drop it from `state.json` (and from `[[repositories]]` in
  `config.toml` if it was declared there).

### Themes

`$XDG_CONFIG_HOME/lazywingman/themes/<name>.toml`. Colours are named roles
(`background`, `accent`, `tree_repository`, `status_waiting`, …) written as
`#rrggbb` hex, an ANSI-16 name (`cyan`, `bright_black`, `gray`, …), or
`default` (terminal default). Seven built-ins ship with the binary:

- **catppuccin-mocha** (default) — dark, highest contrast; the reference set
- **catppuccin-macchiato** — dark, middle contrast
- **catppuccin-frappe** — dark, warm, lowest contrast
- **catppuccin-latte** — the light theme
- **dracula** — truecolor; falls back to the nearest xterm-256 colour (cube or
  grey ramp) on terminals without 24-bit colour (`COLORTERM` when it is set,
  otherwise the terminal's own identity — `TERM`, `TERM_PROGRAM`, `VTE_VERSION`
  or a marker such as `WT_SESSION`)
- **gruvbox** — truecolor with the same fallback
- **ansi** — ANSI-16 palette only, so it inherits your terminal's colours

The Catppuccin flavours use the official hex values, unmodified, mapped onto
the app's roles (`base` → background, `text`/`subtext0` → text/dim,
`surface0-2` → selection/borders, flavour accents for status, `peach` for the
accent on the dark flavours and `lavender` on latte — `lavender` sits too
close to `text` on a dark background for a one-character key to stand out).

Press **`ctrl-t`** inside the TUI for a live theme picker: move with
`j`/`k` (or `PgUp`/`PgDn`, `Home`/`End`) and the whole screen repaints with
the new palette; `Enter` keeps the previewed theme, `Esc` restores the one
you opened with. The pick is saved to the state file (the config file is
never rewritten behind your back) and honoured on the next start; the
config's `theme = ...` line is the startup default.

`NO_COLOR` (present and non-empty) switches to a monochrome render — every
role resolves to the terminal default; the UI stays readable because meaning
is carried by symbols, labels and position, not by colour.

**A theme answers for the states you act on; the rest borrow.** The shipped
themes set four status colours — the ones a row's hue has to tell apart at
a glance — and leave the other three to a base role, so no theme can drift
away from what a state means:

| Role | Shipped value | Why |
|---|---|---|
| `status_waiting` | yellow | the agent finished its turn and is parked |
| `status_blocked` | orange / peach | the same "wants you" family as waiting, but its own hue: blocked owes you an answer *now* |
| `status_failed` | red | something went wrong |
| `status_ended` | purple / mauve | the agent is gone and a plain shell is left — not a state you act on, and not ordinary text either |

| Role | Borrows | Why |
|---|---|---|
| `status_working` | `foreground` | an agent doing its job is not an alarm — the spinner already says it is running |
| `status_idle` | `dim` | nothing to act on, so it recedes |
| `status_dead` | `dim` | same, and the whole row is muted anyway |
| `worktree_merged` | `tree_worktree` | the green of the worktree level: a merged worktree is done and safe to delete, and must not read as ordinary text |

Every one of the eight stays optional and overridable: set the key in your
theme and your value wins, omit it and it borrows (`blocked` falls back to
`status_waiting`, `ended` to `foreground`), so a theme file setting all
eight and one setting only two both load unchanged.

**The selected row is tinted, not repainted.** The cursor row takes
`selection_bg` as a background and leaves every foreground alone, because
those foregrounds *are* the information: the status colour, the dimming of a
dead shell, the worktree's state. Inverting them would erase what you
moved the cursor there to read. (`selection_fg` still applies inside modals,
where rows carry no colour of their own.)

### Agent status

Every `poll_interval_ms`, LazyWingman captures each agent pane
(`capture-pane`) and classifies it: `dead` → the agent's own hook-reported
state (`working` / `waiting` / `blocked`), while it is live → `ended` →
`failed` → `waiting` → `working` → `waiting` anywhere → `idle`. A shell
whose pane is gone from tmux — it exited (ctrl-d) or its window was killed
outside the dashboard — is `dead`, as is a pane whose process died with
`remain-on-exit` set; a live pane whose foreground command is a plain
login shell is `ended` — the agent has quit (or never started), so the
session still on screen is scrollback, not state. Below `dead`, the hook
overlay and that shell check, classification is per-agent regex patterns
over the pane's captured text — heuristics only, and the live preview
panel lets you confirm what's really happening.

**Claude Code shells report their own state.** `[defaults] agent_hooks`
(default `true`) injects `--settings <state_dir>/lazywingman-activity-hooks.json`
into any spawned command recognised as `claude` (the first token's file
name must be exactly `claude`; wrapper scripts and aliases stay on the
regex heuristics above, like any other agent). That settings file is
merged **additively** on top of your own Claude settings — nothing is
written there, nothing persists — and registers hooks that report
Claude's real state onto the pane as it happens, ahead of the regex
guess. Instrumentation is disclosed, not hidden: an instrumented pane's
command line literally shows `--settings '…/lazywingman-activity-hooks.json'`.
A hook-reported state is trusted for up to 60 seconds past its own
timestamp unconditionally; past that, it is only overridden if the pane's
own tail *also* looks like a prompt — a mid-turn agent, however long its
turn runs, is never second-guessed on age alone. This adds one status the
regex ladder alone cannot express:

- **`blocked`** — the agent needs **you**, right now (a permission
  prompt). Distinct from `waiting`, which means the agent finished its
  turn and is idle at the prompt with nothing owed.

Set `agent_hooks = false` to disable injection entirely; every agent,
Claude included, then classifies purely from `capture-pane`, exactly as
before this worktree existed. `failed` stays regex-based either way —
Claude has no "I failed" hook event — so a failing turn only surfaces
once the session ends and the overlay clears.

**Every state draws its own icon** in front of the status word — the glyph
is the fast scan across a full tree, the word is what settles it:

| State | `nerdfont` | `unicode` | Colour |
|---|---|---|---|
| `working` | spinner, `⣾⣷⣯⣟⡿⢿⣻⣽` | same | text |
| `waiting` | `󰏤` | `⏸` | `status_waiting` (yellow) |
| `blocked` | `󰓛` | `⏹` | `status_blocked` (orange) |
| `failed` | `` | `✖` | `status_failed` (red) |
| `ended` | `` | `⏸` | `status_ended` (purple) |
| `idle` | — | — | dim |
| `dead` | — | — | dim |

The spinner turns while an agent is working, in step across every row, and
costs nothing extra: it is a function of elapsed time, read off the redraws
the poll loop already does. `idle` and `dead` draw no glyph on purpose —
a state you cannot act on should not compete for attention — but they still
hold the column, so labels stay aligned. Every glyph is a `[icons]` key
(`status_working` is `status_spinner`, a string of frames); see the key
reference below.

A `dead` entry is **muted whole**: the row's guide, icon, name and status all
take the dead colour instead of only the status word. The rule climbs the tree
— a worktree is muted once every one of its shells is dead, a repository once
every one of its worktrees is (a worktree with no shells is not dead, it was
simply never started) — so a finished worktree reads as history at a glance
without hunting for the status word at the end of the row.

## How it works with tmux

- LazyWingman detects its own coordinates (`TMUX` env, `display-message -t
  pane`), and `exec`s into `tmux new-session -A -s lazywingman -- lazywingman`
  when there are none — a launch from a bare shell ends up a tmux pane like
  any other. A second launch in the same folder attaches the dashboard already
  showing it (its window is stamped with the scope) rather than starting a
  rival one. Agent shells live in **dedicated sessions**, one per repository, named
  deterministically `wingman-session-<repository>` — your own session is never
  touched. Shell windows are named `lw:<worktree>/<n>` inside that session, so
  everything is rediscoverable after a restart (`tmux list-sessions` →
  per-session `list-windows`), even if the state file is lost.
- All interaction goes through the tmux CLI (`new-session`, `new-window`,
  `send-keys`, `capture-pane`, `swap-pane`, `select-pane`, `kill-window`,
  `list-panes -a`); nothing is shell-interpreted.
- **Docking is one `swap-pane`.** Every agent window carries a sleeping
  *holder* and the dashboard window a matching *slot*, so a dock exchanges two
  panes that already exist: no window and no session is created or destroyed,
  and because the two are the same size tmux redraws from its own buffer —
  the agent sees no `SIGWINCH` and does not repaint. Undocking is the same
  swap backwards, and quitting sends the pane home before taking the slot
  down.
- **Pane navigation is yours.** LazyWingman binds no movement key and sets no
  chord in your tmux. The back-key it used to bind is retired: with the agent
  beside the tree there is no window to travel back from.
- **New worktree**: the branch and worktree are cut from the repository's
  **base branch** (`worktrees.base_branch`, or the repository's override), not
  from whatever the repo has checked out. The New Worktree popup names that
  branch under the name field; when the configured base branch does not
  exist in the repo it warns that creation falls back to the current branch.
- **Quick-send** = an in-TUI multiline editor (Enter = newline, Tab =
  tab, arrows/Home/End move the cursor, **ctrl-s sends** — Alt+Enter too
  where the terminal delivers it)
  that pastes the prompt to the selected pane via tmux paste-buffer with
  bracketed paste — multi-line prompts arrive as one paste, not as
  premature Enter presses.
- Unsent quick-send drafts (`$XDG_STATE_HOME/lazywingman/drafts.json`) are
  keyed by repository + branch + shell index, so they are restored onto the
  right shell after a restart even though tmux gives it a new pane id.
- State (`$XDG_STATE_HOME/lazywingman/state.json`) is reconciled against
  reality on startup: worktrees are checked against `git worktree list`,
  agent sessions and their windows against `tmux list-sessions` /
  `list-windows`; stale entries self-heal and running shells are
  rediscovered (even with a deleted state file, from the pane's cwd + git
  metadata).
- Worktree deletion kills the worktree's windows — a session whose last shell
  window is killed destroys itself — then `git worktree remove` (with
  `--force` offered when the worktree is dirty). The worktree branch goes
  with it unless `worktrees.delete_branch = false`. The confirm says
  whether the worktree is clean, and it counts committed work too: a clean
  working tree on a branch that is ahead of the base reads as "NOT clean:
  unmerged commits", with a warning line, because deleting the branch
  discards them.
  The confirm popup answers to `y`, `Enter` or `d` — pressing `d` twice
  deletes, the way `dd` does in vim.
- Shell deletion (`d` on a shell row) kills that one window and drops the
  shell from the worktree — the worktree, the branch and the sibling shells
  are untouched. A closed shell's index is never handed out again while the
  worktree lives, so a new shell cannot collide with a live window.

## Layout

Rounded corners, bare titles (no background fill), tree guides and icons.
Two panels and a bar — the tree says what exists, the preview says what
is happening, the bar says what you can press:

```
╭ Repositories (2) ────────╮╭ Preview ────────────────────────────╮
│󰝰 repo-a                  ││ live tail of the selected shell,    │
│  ├─  login  ⏸ waiting   ││ refreshed every poll                │
│  └─  search             ││                                     │
│     ├─ ● #0 ⠹ working    ││     ╭────────────────────────╮      │
│     └─ ● #1 ⏸ waiting    ││     │ worktree 'search' cre… │      │
│󰉖 repo-b                  ││     ╰────────────────────────╯      │
╰──────────────────────────╯╰─────────────────────────────────────╯
 n worktree · a shell · d delete · ? help
```

Panel titles are Title Case and name what the panel holds. The tree is
`Repositories (n)`, or `Worktrees (n)` when a single git repo is in scope and
its worktrees have replaced the repository row (the flat layout below).

**The preview panel** shows the tail of `capture-pane -e` for the selected
shell — escape sequences included, so the agent's own colouring (spinners,
diffs, syntax highlighting) is what you see. `[preview].colors` decides how
much of it survives: `full` (default) takes every colour exactly as the pane
emits it, `foreground` keeps the text colours but drops background colours
so the panel keeps the theme's background, and `none` renders raw text.
Status classification always runs on the stripped text, so colouring never
changes how a pane is classified.

**Scrolling the preview.** The mouse wheel walks back through the selected
shell's history. The first scroll fetches the pane's whole scrollback (a poll
keeps only enough lines to classify a status) and freezes the view there, so
the polls cannot move the text while you read. A parked panel says so three
ways — the offset in its title (`Preview ↑47`), a lit border, and a `live Esc`
entry leading the statusbar — because a panel that has quietly stopped
following its agent otherwise reads as an agent that stopped. `Esc`, or
selecting another row, returns to the live tail. The wheel never moves the
tree selection, so reading back through an agent's output cannot lose your
place; below the split tier, where the preview is not drawn at all, the wheel
falls back to the tree.

Icons come from `[icons]` (default preset `nerdfont`): a **folder per
repository** — `󰝰` open when the row is expanded, `󰉋` closed when it is
folded, `󰉖` outline when the repository has no worktrees at all — `●` shell,
and **four glyphs for a
worktree's state** — `` untouched, `` carrying work the base branch
does not have, `` all its work merged into the base, `󰗨` its
directory gone. "Carrying work" means commits *or* an unclean working
tree: a worktree an agent has edited but not committed is no longer
untouched, and a merged worktree picks the glyph back up as soon as
something is written into it. The four share one padded column, so names
stay aligned whichever one a row draws. The state is also colour-coded:
new is dim, ahead is the `waiting` colour, merged is `worktree_merged`
(green), and a deleted worktree is the `failed` colour — that row is broken, not
finished. Merged and deleted rows are *settled*: the state glyph keeps its
colour and the rest of the row — the worktree name and its shell's status —
is dimmed. Nothing there needs answering any more, so a leftover yellow
`waiting` must not compete with the rows that do want you. No patched font? Set
`preset = "unicode"` for `▣`/`▤`/`▢` and `○`/`◆`/`✓`/`⊘`. Rows carry
meaning in symbols, labels and position as well as colour.

Uncommitted changes are deliberately *not* in that icon — a branch with
commits reads the same whether or not there is unsaved work on top. They
still decide whether the delete confirm offers `f` (force).

A worktree whose directory has been removed — by hand, or by an agent
told to merge and clean up after itself — keeps its row until you press
`d`, and draws the deleted glyph while it waits: nothing can be measured
in a path that is not there, and the alternative is a row claiming to be a
fresh cut.

"Merged" is measured against the **local** base branch: a PR merged on the
forge shows as ahead until that merge is in your local `main`. A
squash-merged branch keeps commits the base does not have by object id, so
it also reads as ahead. LazyWingman records the commit a worktree was cut
from when it creates it — a worktree it merely discovered (or one from a
state file written before this existed) cannot tell "merged" from "new" and
reads as new until it has commits of its own.

> A Nerd Font glyph is drawn single-width by the Mono font variants, which
> is what the default assumes. If your font draws it double-width, add a
> trailing space to the `worktree_*` values — config values are used
> verbatim and the column pads around them.

#### Every `[icons]` key

`preset` picks the base set; any key below overrides it individually.

| Key | `nerdfont` | `unicode` | `none` | Draws |
|---|---|---|---|---|
| `repository` | `󰉋` | `▣` | — | repository with worktrees, folded |
| `repository_open` | `󰝰` | `▤` | — | ...the same repository, expanded |
| `repository_empty` | `󰉖` | `▢` | — | repository with no worktrees |
| `worktree_new` | `` | `○` | — | untouched: no commits, clean tree |
| `worktree_ahead` | `` | `◆` | — | commits not in the base branch, or a dirty tree |
| `worktree_merged` | `` | `✓` | — | commits all merged into the base, clean tree |
| `worktree_deleted` | `󰗨` | `⊘` | — | the worktree directory is gone |
| `status_spinner` | `⣾⣷⣯⣟⡿⢿⣻⣽` | same | — | frames cycled while an agent works |
| `status_waiting` | `󰏤` | `⏸` | — | agent parked at its prompt |
| `status_blocked` | `󰓛` | `⏹` | — | agent asking for permission |
| `status_failed` | `` | `✖` | — | the agent reported a failure |
| `status_ended` | `` | `⏸` | — | the agent exited, a shell is left |
| `status_idle` | — | — | — | agent running, state unrecognised |
| `status_dead` | — | — | — | the pane is gone |
| `shell` | `●` | `●` | — | before the shell number |
| `cursor` | `▌` | `▌` | `▌` | selection, tree and modals |
| `guide_branch` | `├─` | `├─` | `├─` | guide, non-last child |
| `guide_last` | `└─` | `└─` | `└─` | guide, last child |
| `guide_vertical` | `│` | `│` | `│` | guide, continuing parent |
| `separator` | ` · ` | ` · ` | ` · ` | between bar/help entries |
| `warn` | `⚠` | `⚠` | `⚠` | startup diagnostics |
| `ellipsis` | `…` | `…` | `…` | truncation marker |
| `enter` | `↵` | `↵` | `↵` | the Enter key, in shortcut hints |
| `arrows` | `↑↓` | `↑↓` | `↑↓` | the arrow keys, in shortcut hints |
| `border` | `rounded` | `rounded` | `rounded` | panel and popup frames |

An empty string draws nothing. `guide_branch`/`guide_last`, the three
`repository*` states, the three `worktree_*` states and the `status_*` set
share a rendered column each and are padded to equal width, so names stay
aligned when one of them is empty. `status_spinner` is one string split into
frames, one character per animation step, and every frame joins the status
column — a narrow frame would make the label jitter once per step.
The rest are used verbatim: add your own trailing space if you want a gap
after one (the default `shell = "● "` has one baked in).

Rows carry no counts — the tree is expanded by default, so the children *are*
the count, and the repository icon says whether a folded repository holds
anything. Missing repositories show `(missing)` dimmed. A worktree LazyWingman
has no tmux window for is dimmed *whole* — see **Worktrees without a shell**
below. When only ONE repository is in scope its worktree rows replace the
repository row entirely (the flat layout).

**Worktrees without a shell**: every worktree `git worktree list` reports
gets a row, whether LazyWingman created it or you did. A worktree it has no
tmux window for — one made by hand, or one whose agent shell you closed —
has nothing to dock, send to or capture, so its row is drawn dimmed and
selecting it says so in the preview instead of showing a pane. It is a
listing, not a dead end: `a` starts a shell in that worktree and it joins the
rest, and `d` deletes it like any other.

**The folded row** (`login` above): the nominal case is one worktree, one
agent, so a worktree with a single shell has no child row at all — its own row
*is* that shell. It carries the shell's status, and `Enter` / `s` enter its
docked pane and quick-send to it directly. `a` gives the worktree a
second shell, and the subtree appears. Only worktrees with several shells (or
none) are expandable.

The statusbar carries the contextual shortcuts and nothing else — no app
name, no session chip, no repository count. Every shortcut in the app —
status bar, modal footer, help palette — is written the same way, `<action>
<key>` ("cancel esc"), with only the key in the accent colour and glyphs
from `[icons]` where a key has one (`↵`, `↑↓`). On a frame too narrow to
hold them all the entries wrap onto a second bar row instead of losing
their tail at the border. Notices ("worktree 'x' created",
"dock failed: …") appear as a toast above the bar, wrapped to as many as
three rows so a long error stays readable, and fade after a few
seconds; the log and the diagnostics overlay (`D`) keep the record.

The tree takes a percentage of the frame width, capped at 54 columns — it
holds short names, so on a wide screen the extra width goes to the preview
instead of to empty tree padding. Below 64x12 the tree takes a larger share
of the width; below 44x9 the preview hides and the tree fills the frame;
below 26x7 a "terminal too small" message replaces the UI.

## Development

Driven by mise (the single task runner):

```sh
mise install        # ensure the declared toolchain (rust 1.97)
mise tasks          # list everything below
mise run build      # debug build
mise run fmt        # cargo fmt --check
mise run lint       # cargo clippy --all-targets -- -D warnings
mise run test       # unit tests + real-tmux integration tests (needs tmux)
mise run gate       # fmt + lint + test — the pre-commit gate
mise run install    # release build + link into ~/.local/bin (test from anywhere)
```

Logs go to `$XDG_STATE_HOME/lazywingman/lazywingman.log` — never stdout
(stdout belongs to the TUI).

Design decisions — why each subsystem works the way it does — live in
[`docs/architecture.md`](docs/architecture.md).
