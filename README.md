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
  usually an agent, but useful for any long-running command. The tree names
  a shell row by that command (`claude`, `zsh`); shells running the same
  program keep their index (`zsh #1`, `zsh #2`) so two rows still read
  apart.
- A repository also gets **one shell of its own**, in its main checkout on
  the base branch, for the commands you run by hand: `make`, `mise run …`,
  `cat config.toml`. It starts the first time you press `a` on the
  repository row and is docked beside the tree like any other shell.

Non-goals (deliberate): no orchestration, no MCP/skill management, no web UI,
no Telegram, no repo creation/deletion, no merge/PR automation. Only worktree
teardown is automated.

## Install

With Homebrew (macOS or Linux):

```sh
brew install ike-forge/tap/lazywingman
```

From source:

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
| `l` / `Enter` | expand, or **enter** the selected shell's docked pane (a one-shell worktree goes straight in — see the folded row below). Simply moving onto a shell already docks it; `Enter` is what puts the cursor there. On a **repository** row it only expands or collapses |
| `n` | new worktree (git worktree + branch + agent shell) — a prompt asks for the name and shows the **base branch** it will be cut from: the branch the repository's main checkout is on, unless `worktrees.base_branch` (or a per-repository override) names one. A second field says which **agent** the worktree's shells run, prefilled with the repository's own (`[defaults].agent`, unless the repository overrides it): `Tab` moves onto it, `↑`/`↓` walk the configured `[[agents]]` presets, and anything else typed there is taken as the command itself. So a name and `Enter` is still the whole flow, and `codex` beside a `claude` repository costs one `Tab` |
| `a` | add a shell to the selected worktree — a picker asks what it runs: the worktree's own agent command first (the one it was created with, else the repository's; `Enter` takes it), then the other configured `[[agents]]` presets, then a **plain shell** that just drops you at your own `$SHELL` prompt with nothing sent to it (`j`/`k` moves, `1`-`9` pick directly, `Esc` closes). Anything else you want to run, you type in that plain shell. On a **repository** row, `a` starts (or enters) the repository's own inline shell — no picker |
| `p` | **predefined messages** for the selected shell (or a one-shell worktree's row) — a modal of configured presets, each listed in full with its `<base_branch>`-style tokens already resolved (`j`/`k` moves, `Enter` sends, `1`-`9` send directly, `Esc` closes) |
| `t` | **repository todo pad** — a scratch pad for the next steps you cannot start yet ("rebase the API worktree once the schema lands"). One per repository, shared by its worktrees: on any row of a repository, `t` opens that repository's pad. It splits off the bottom of the tree's column and takes half of it while you type, so neither the tree above nor the preview — or the docked agent — beside it is covered. `Esc` saves and hands the keyboard back to the tree, leaving the note on screen collapsed to its own height (five lines of text, a five-line panel; never more than the open half) and following the selection from repository to repository; an emptied pad takes the panel away with it. `ctrl-s` saves without leaving, and `ctrl-e` hands the file to your `$EDITOR` in a tmux window of its own — closing the editor reads it back in. Parked as plain text in `$XDG_STATE_HOME/lazywingman/todos/<escaped repository path>.md` (yours to edit with `$EDITOR` too) |
| `T` | **global todo pad** — the same panel, one pad for the whole machine, for notes written about one repository and needed while looking at another. Stored in `$XDG_STATE_HOME/lazywingman/todo.md`. The parked panel shows whichever pad was opened last |
| `d` | on a worktree: delete it (kills windows, removes worktree; `f` = force when dirty or locked). On a shell row: close that shell (kills its window only — the worktree and its other shells stay). On a repository row: close its inline shell |
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
base_branch = ""             # empty = the repo's current branch; a named one must exist
delete_branch = true         # also delete the worktree branch on delete

[discovery]
scan_root = ""               # the scope: repositories inside this dir are listed; empty = cwd
max_depth = 4                # how deep a discovery scan walks
skip_dirs = [".git", "node_modules", "target", ".lazywingman-worktrees"]

[defaults]
agent = "claude"             # `[[agents]]` preset new worktrees start with; the `n` popup's prefill
agent_command = "claude"     # command behind it, when `agent` names no preset
agent_shell = "bash"       # shell agent windows run under; a plain shell uses your $SHELL instead
agent_hooks = true           # instrument recognised `claude`/`codex` commands for accurate status

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
2. Your config — `$XDG_CONFIG_HOME/lazywingman/config.toml`
3. Repository-local — `.lazywingman.toml` in the working directory or the
   nearest ancestor holding one (the search stops at a git repository root)
4. Repository-local overrides — `.lazywingman.local.toml` next to it
   (gitignore this one; it wins over the committed repository file)
5. The file you name — `lazywingman --config <path>` (or `LW_CONFIG=<path>`;
   the flag wins over the env var)

A repository-local file is meant to be committed — a teammate clones the repo
and is already configured. A few keys belong to the user, not the repository,
and are **ignored in repository files with a diagnostic**: `worktrees.root`
and `discovery.scan_root` (machine-specific paths). Set those in your user
config — or in the file you pass to `--config`, which is you speaking too.

`--config` is a layer, not a replacement: it is read last, so it wins, but the
keys it leaves alone keep whatever the layers below resolved to. A file with
one line in it is a valid `--config`:

```
lazywingman --config ~/configs/slow-poll.toml
```

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

Repositories are not stored: each start scans the scope and merges in
config `[[repositories]]` entries. A repository's base branch and agent are
read from config whenever they are used, so a config edit applies at the
next start without touching any state.

## Repository discovery

LazyWingman lists exactly the repositories **inside the current path** — the
scope — and nothing else. Different scopes never mix; repositories you worked
on elsewhere stay saved in state but are hidden until you start there
again. There is no cache and no prompt: finding `.git` entries is a few
stats per directory, done live.

- **Start inside a git repo**: that repo is the scope. It is auto-imported
  and its row always shows at the top of the tree, so you can see where
  you are.
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
- **Missing repositories**: a repository whose path stops being a git repo
  while the dashboard runs shows `(missing)` dimmed after `r` and
  re-activates when the path is a repo again. The next start does not list
  it.

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
you opened with. The pick is written to the `theme = ...` line of
`~/.config/lazywingman/config.toml`, leaving the rest of the file as it was.
A repository `.lazywingman.toml` or `--config` that sets `theme` still wins.

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

**Claude Code and Codex shells report their own state.** `[defaults]
agent_hooks` (default `true`) registers lifecycle hooks in any spawned
command recognised as one of the two — the first token's file name must be
exactly `claude` or `codex`; wrapper scripts and aliases stay on the regex
heuristics above, like any other agent. Each gets the shape its CLI takes:

- **Claude**: `--settings <state_dir>/lazywingman-activity-hooks.json`, a
  file merged **additively** on top of your own Claude settings — nothing
  is written there, nothing persists.
- **Codex**: one `-c hooks.<Event>=…` override per event, which Codex adds
  to whatever your own `~/.codex/config.toml` defines rather than replacing
  it, so your own hooks keep running. The **first** Codex shell asks you to
  trust them (*"Hooks need review"* → `t` for trust all); trust is keyed to
  the command, so it is asked once, not once per worktree. Until you answer,
  Codex declines to run them and the pane falls back to the regex guess.

Either way the hooks report the agent's real state onto the pane as it
happens, ahead of the regex guess, and instrumentation is disclosed rather
than hidden: an instrumented pane's command line shows the flags it was
given.
A hook-reported state is trusted for up to 60 seconds past its own
timestamp unconditionally; past that, it is only overridden if the pane's
own tail *also* looks like a prompt — a mid-turn agent, however long its
turn runs, is never second-guessed on age alone. This adds one status the
regex ladder alone cannot express:

- **`blocked`** — the agent needs **you**, right now (a permission
  prompt). Distinct from `waiting`, which means the agent finished its
  turn and is idle at the prompt with nothing owed. Codex reports this
  with an event of its own (`PermissionRequest`); for Claude it is read
  out of the notification's wording, so a phrasing we do not recognise
  costs a `blocked`, never invents one.

Set `agent_hooks = false` to disable injection entirely; every agent,
Claude and Codex included, then classifies purely from `capture-pane`.
`failed` stays regex-based either way — neither has an "I failed" hook
event — so a failing turn only surfaces once the session ends and the
overlay clears.

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
  touched. Each shell's identity and launch command live on its tmux pane
  (`@lw_*` options), so shells are rediscovered after a restart even if the
  state file is lost.
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
  **base branch**: the branch its main checkout is on, unless
  `worktrees.base_branch` (or the repository's override) names one. The New
  Worktree popup shows it as a `base branch` field under the agent; a
  configured branch missing from the repo reads `(not found)` and creation
  refuses.
- **Predefined messages** are pasted to the selected pane via tmux
  paste-buffer with bracketed paste — multi-line prompts arrive as one
  paste, not as premature Enter presses.
- State (`$XDG_STATE_HOME/lazywingman/state.json`) holds only what
  LazyWingman recorded about the worktrees it created (name, creation time,
  fork point, agent), keyed by path. Worktrees come from `git worktree list`.
  Shells are never written: every poll rebuilds them from `tmux list-panes`,
  so a shell is on screen exactly while its pane exists (and a worktree with
  a live shell but no entry is rebuilt from its pane's cwd + git metadata).
- Worktree deletion kills the worktree's windows — a session whose last shell
  window is killed destroys itself — then `git worktree remove` (with
  `--force` offered when the worktree is dirty). The worktree branch goes
  with it unless `worktrees.delete_branch = false`. The removal runs in the
  background: the UI stays usable and the row reads `deleting` with a
  spinner until it is gone. Quitting waits for pending removals. The confirm is three beats a
  blank line apart — the question, the worktree path, then whether the
  worktree is clean — and "clean" counts committed work too: a clean
  working tree on a branch that is ahead of the base reads as "worktree is
  NOT clean", and the warning line below names why ("unmerged commits —
  deleting the branch discards them"). Only the warning names the reason;
  the state line used to repeat it, which read as two separate problems.
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
│     ├─ ● claude ⠹ working││     ╭────────────────────────╮      │
│     └─ ● zsh ⏸ waiting   ││     │ worktree 'search' cre… │      │
│󰉖 repo-b                  ││     ╰────────────────────────╯      │
╰──────────────────────────╯╰─────────────────────────────────────╯
 n worktree · a shell · d delete · ? help
```

A third panel joins them at the foot of the tree's column once a todo pad
(`t` for the selected repository, `T` global) has something in it — as tall
as the note is, and gone again when the note is:

```
╭ Repositories (2) ────────╮╭ Preview ────────────────────────────╮
│󰝰 repo-a                  ││ live tail of the selected shell,    │
│  ├─  login  ⏸ waiting   ││ refreshed every poll                │
│  └─  search             ││                                     │
╰──────────────────────────╯│                                     │
╭ Todo · repo-a ───────────╮│                                     │
│ rebase search once the   ││                                     │
│ schema lands             ││                                     │
╰──────────────────────────╯╰─────────────────────────────────────╯
 n worktree · a shell · t todo · ? help
```

Panel titles are Title Case and name what the panel holds. The tree is
`Repositories (n)`.

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
colour, the worktree name is dimmed, and the shell's status is dropped from
the row altogether. Nothing there needs answering any more — the next step
is deletion — so a leftover `waiting` must not compete with the rows that do
want you. No patched font? Set
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
| `separator` | ` | ` | ` | ` | ` | ` | between bar/help entries |
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
below.

**Worktrees without a shell**: every worktree `git worktree list` reports
gets a row, whether LazyWingman created it or you did. A worktree it has no
tmux window for — one made by hand, or one whose agent shell you closed —
has nothing to dock, send to or capture, so its row is drawn dimmed and
selecting it says so in the preview instead of showing a pane. It is a
listing, not a dead end: `a` starts a shell in that worktree and it joins the
rest, and `d` deletes it like any other.

**The folded row** (`login` above): the nominal case is one worktree, one
agent, so a worktree with a single shell has no child row at all — its own row
*is* that shell. It carries the shell's status, and `Enter` / `p` enter its
docked pane and send it a predefined message directly. `a` gives the worktree
a second shell, and the subtree appears. Only worktrees with several shells (or
none) are expandable.

The statusbar carries the contextual shortcuts and nothing else — no app
name, no session chip, no repository count. Every shortcut in the app —
status bar, modal footer, help palette — is written the same way, `<key>
<action>` ("esc cancel"), with only the key in the accent colour and glyphs
from `[icons]` where a key has one (`↵`, `↑↓`). On a frame too narrow to
hold them all the entries wrap onto a second bar row instead of losing
their tail at the border. Notices ("worktree 'x' created",
"dock failed: …") appear as a toast above the bar, wrapped to as many as
three rows so a long error stays readable, and fade after a few
seconds. Only errors are drawn in the failure colour; an ordinary notice
takes `title_fg`, so "worktree deleted" never reads as a problem; the log and the diagnostics overlay (`D`) keep the record.

The tree takes a percentage of the frame width, capped at 60 columns — it
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
