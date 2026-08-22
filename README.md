# LazyWingman

A control centre for supervising coding agents (Claude Code, Codex, Mistral
Vibe, …) across **multiple repositories and multiple worktrees in
parallel**. See everything at a glance, jump into any agent with one
keystroke, spin up and tear down worktrees instantly.

LazyWingman runs anywhere — inside tmux (as a pane of your existing session)
or standalone in a plain terminal. Agent shells never pollute your tmux
session: they live in their own dedicated tmux sessions
(`wingman-session-<repository>`), invisible until you attach to them.

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

Requirements: Rust ≥ 1.85, `git`, `tmux` (≥ 3.0 — agent shells are tmux
sessions; the server is started on demand).

```sh
lazywingman
```

Run it from a pane of your tmux session, or from any plain terminal — both
work. Inside tmux you get instant attach/return (`Enter` to attach, the
back-key `C-o` to return to the dashboard); standalone, attach hands the
terminal over to `tmux attach` and drops you back into the dashboard when you
detach (`C-b d`).

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
| `l` / `Enter` | expand, or **attach** to the selected shell (a one-shell worktree attaches straight away — see the folded row below) |
| `n` | new worktree (git worktree + branch + agent shell) — a prompt asks for the name and names the branch it will be cut from: the repository's **base branch** (`worktrees.base_branch`, per-repository override), never the branch the repo has checked out |
| `a` | add a shell to the selected worktree — a prompt asks which command it runs (empty = the repository's default agent command, shown as a hint) |
| `s` | **quick-send** a line to the selected shell (or to a one-shell worktree's row). `Esc` keeps the unsent text as that shell's draft — `s` on it again picks up where you left off. The draft is dropped once the message is actually delivered, and lives only for this run of LazyWingman |
| `p` | **predefined messages** for the selected shell (or a one-shell worktree's row) — a modal of configured presets, each listed in full with its `<base_branch>`-style tokens already resolved (`j`/`k` moves, `Enter` sends, `1`-`9` send directly, `Esc` closes) |
| `d` | on a worktree: delete it (kills windows, removes worktree; `f` = force when dirty). On a shell row: close that shell (kills its window only — the worktree and its other shells stay) |
| `r` | refresh — reconcile state against reality |
| `?` | help overlay (grouped, scrollable: `j`/`k`, `PgUp`/`PgDn`) |
| `q` / `Esc` / `ctrl-c` | quit (ctrl-c works from any state) |
| `Esc` / `q` / `Enter` (startup) | dismiss the startup diagnostics overlay (shown when config problems were dropped at load) |

Mouse: click a row selects, double-click opens (same as `Enter`), the wheel
scrolls the **preview** (never the tree), statusbar entries are clickable, and
clicking outside an overlay dismisses it.

**Attach & return**: `Enter` on a shell switches your client to that agent's
window. Press the tmux prefix + the *back-key* to return to the dashboard —
`C-b C-o` by default, the second half configurable via `tmux.back_key`. The
chord is bound in the prefix table (never the root table, so no other TUI
ever loses the key).

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

[tmux]
back_key = "C-o"             # prefix + C-o jumps back to the dashboard (prefix table)

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
agent_shell = "bash"
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
text = "merge to <base_branch>, drop the current worktree and branch"

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
and are **ignored in repository files with a diagnostic**: `tmux.back_key`
(your global tmux binding), `worktrees.root` and `discovery.scan_root`
(machine-specific paths). Set those in your user config.

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
- **dracula** — truecolor; falls back to xterm-256 when `COLORTERM` doesn't
  advertise 24-bit support
- **gruvbox** — truecolor with the same fallback
- **ansi** — ANSI-16 palette only, so it inherits your terminal's colours

The Catppuccin flavours use the official hex values, unmodified, mapped onto
the app's roles (`base` → background, `text`/`subtext0` → text/dim,
`surface0-2` → selection/borders, flavour accents for status, `lavender` for
the accent).

Press **`ctrl-t`** inside the TUI for a live theme picker: move with
`j`/`k` (or `PgUp`/`PgDn`, `Home`/`End`) and the whole screen repaints with
the new palette; `Enter` keeps the previewed theme, `Esc` restores the one
you opened with. The pick is saved to the state file (the config file is
never rewritten behind your back) and honoured on the next start; the
config's `theme = ...` line is the startup default.

`NO_COLOR` (present and non-empty) switches to a monochrome render — every
role resolves to the terminal default; the UI stays readable because meaning
is carried by symbols, labels and position, not by colour.

**A theme answers for two status colours, not seven.** Only `status_waiting`
and `status_failed` are a theme's to choose; the other five borrow a base
role, so no theme can drift away from what a state means:

| Role | Borrows | Why |
|---|---|---|
| `status_working` | `foreground` | an agent doing its job is not an alarm — the spinner already says it is running |
| `status_ended` | `foreground` | the agent is gone, the shell is ordinary |
| `status_blocked` | `status_waiting` | both mean the agent stopped and wants you; the icon tells them apart |
| `status_idle` | `dim` | nothing to act on, so it recedes |
| `status_dead` | `dim` | same, and the whole row is muted anyway |

Each stays overridable: set the key in your theme and your value wins.
Themes written before this split — which set all seven — keep loading
unchanged.

**The selected row is tinted, not repainted.** The cursor row takes
`selection_bg` as a background and leaves every foreground alone, because
those foregrounds *are* the information: the status colour, the dimming of a
dead shell, the worktree's commit state. Inverting them would erase what you
moved the cursor there to read. (`selection_fg` still applies inside modals,
where rows carry no colour of their own.)

### Agent status

Every `poll_interval_ms`, LazyWingman captures each agent pane
(`capture-pane`) and classifies it: `dead` → the agent's own hook-reported
state (`working` / `waiting` / `blocked`), while it is live → `failed` →
`working` → `waiting` → `ended` → `idle`. A shell whose pane is gone from
tmux — it exited (ctrl-d) or its window was killed outside the dashboard —
is `dead`, as is a pane whose process died with `remain-on-exit` set; a
live shell with no agent activity is `ended`. Below `dead` and the hook
overlay, classification is per-agent regex patterns over the pane's
captured text — heuristics only, and the live preview panel lets you
confirm what's really happening.

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
| `working` | spinner, `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` | same | text |
| `waiting` | `` | `⏸` | `status_waiting` |
| `blocked` | `` | `⏹` | `status_waiting` |
| `failed` | `` | `✖` | `status_failed` |
| `ended` | `` | `⏸` | text |
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
  pane`) when it runs inside tmux; standalone it still manages the same tmux
  server via its CLI — the server is started automatically if none is running.
  Agent shells live in **dedicated sessions**, one per repository, named
  deterministically `wingman-session-<repository>` — your own session is never
  touched. Shell windows are named `lw:<worktree>/<n>` inside that session, so
  everything is rediscoverable after a restart (`tmux list-sessions` →
  per-session `list-windows`), even if the state file is lost.
- All interaction goes through the tmux CLI (`new-session`, `new-window`,
  `send-keys`, `capture-pane`, `switch-client`, `select-window`,
  `kill-window`, `list-panes -a`, `bind-key`); nothing is shell-interpreted.
- **Attach & return (inside tmux)**: `Enter` on a shell = `switch-client` to
  the agent session + `select-window` to the shell's window. The back chord
  (prefix + `C-o` by default, configurable via `tmux.back_key`) is bound in
  the PREFIX table to `switch-client` back to the dashboard window, rebound
  on every startup; legacy root-table bindings from earlier versions are
  removed on startup.
- **New worktree**: the branch and worktree are cut from the repository's
  **base branch** (`worktrees.base_branch`, or the repository's override), not
  from whatever the repo has checked out. The New Worktree popup names that
  branch under the name field; when the configured base branch does not
  exist in the repo it warns that creation falls back to the current branch.
- **Attach (standalone)**: the terminal is handed over to `tmux attach -t
  <agent-session>` (the shell's window is preselected); detaching with `C-b
  d` returns you to the dashboard.
- **Quick-send** = an in-TUI multiline editor (Enter = newline, Tab =
  tab, arrows/Home/End move the cursor, **ctrl-s sends** — Alt+Enter too
  where the terminal delivers it)
  that pastes the prompt to the selected pane via tmux paste-buffer with
  bracketed paste — multi-line prompts arrive as one paste, not as
  premature Enter presses.
- State (`$XDG_STATE_HOME/lazywingman/state.json`) is reconciled against
  reality on startup: worktrees are checked against `git worktree list`,
  agent sessions and their windows against `tmux list-sessions` /
  `list-windows`; stale entries self-heal and running shells are
  rediscovered (even with a deleted state file, from the pane's cwd + git
  metadata).
- Worktree deletion kills the worktree's windows — a session whose last shell
  window is killed destroys itself — then `git worktree remove` (with
  `--force` offered when the worktree is dirty). The worktree branch goes
  with it unless `worktrees.delete_branch = false`.
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
and **three glyphs for a
worktree's commit state** — `` no commits of its own yet, `` commits the
base branch does not have, `` all its commits merged into the base. The
three share one padded column, so names stay aligned whichever one a row
draws. The state is also colour-coded: new is dim, ahead is the `waiting`
colour, merged is the `working` colour. No patched font? Set
`preset = "unicode"` for `▣`/`▤`/`▢` and `○`/`◆`/`✓`. Rows carry
meaning in symbols, labels and position as well as colour.

Uncommitted changes are deliberately *not* in that icon — a branch with
commits reads the same whether or not there is unsaved work on top. They
still decide whether the delete confirm offers `f` (force).

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
| `worktree_new` | `` | `○` | — | worktree with no commits of its own |
| `worktree_ahead` | `` | `◆` | — | commits not in the base branch |
| `worktree_merged` | `` | `✓` | — | commits all merged into the base |
| `status_spinner` | `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` | same | — | frames cycled while an agent works |
| `status_waiting` | `` | `⏸` | — | agent parked at its prompt |
| `status_blocked` | `` | `⏹` | — | agent asking for permission |
| `status_failed` | `` | `✖` | — | the agent reported a failure |
| `status_ended` | `` | `⏸` | — | the agent exited, a shell is left |
| `status_idle` | — | — | — | agent running, state unrecognised |
| `status_dead` | — | — | — | the pane is gone |
| `shell` | `●` | `●` | — | before the shell number |
| `cursor` | `›` | `›` | `›` | selection, tree and modals |
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
has nothing to attach to, send to or capture, so its row is drawn dimmed and
selecting it says so in the preview instead of showing a pane. It is a
listing, not a dead end: `a` starts a shell in that worktree and it joins the
rest, and `d` deletes it like any other.

**The folded row** (`login` above): the nominal case is one worktree, one
agent, so a worktree with a single shell has no child row at all — its own row
*is* that shell. It carries the shell's status, and `Enter` / `s` attach and
quick-send to it directly. `a` gives the worktree a
second shell, and the subtree appears. Only worktrees with several shells (or
none) are expandable.

The statusbar carries the contextual shortcuts and nothing else — no app
name, no session chip, no repository count. Every shortcut in the app —
status bar, modal footer, help palette — is written the same way, `<action>
<key>` ("cancel esc"), with only the key in the accent colour and glyphs
from `[icons]` where a key has one (`↵`, `↑↓`). Notices ("worktree 'x' created",
"attach failed: …") appear as a toast above the bar and fade after a few
seconds; the log and the diagnostics overlay (`D`) keep the record.

The tree takes a percentage of the frame width, capped at 48 columns — it
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
