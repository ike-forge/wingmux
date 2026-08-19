# LazyWingman

A control centre for supervising coding agents (Claude Code, Codex, Mistral
Vibe, …) across **multiple projects and multiple features in parallel**. See
everything at a glance, jump into any agent with one keystroke, spin up and
tear down feature workspaces instantly.

LazyWingman runs anywhere — inside tmux (as a pane of your existing session)
or standalone in a plain terminal. Agent shells never pollute your tmux
session: they live in their own dedicated tmux sessions
(`wingman-session-<project>`), invisible until you attach to them.

```
ProjectA  (a git repo, imported by path)
  ├── master        (the main checkout — untouched)
  ├── featureA1     (worktree + 1..n agent shells)
  ├── featureA2
  └── featureA3
ProjectB
  ├── master
  ├── featureB1
  └── featureB2
```

- **Project** = an existing local git repo. Imported and removed only; never
  created or deleted by LazyWingman.
- **Feature** = a git worktree (created automatically) + at least one spawned
  shell running the default agent command.
- **Shell** = a tmux window running a command in the feature's worktree —
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

First run writes a commented config to
`$XDG_CONFIG_HOME/lazywingman/config.toml` and copies the built-in themes to
`$XDG_CONFIG_HOME/lazywingman/themes/`.

LazyWingman finds your projects for you (see [Project discovery](#project-discovery)),
and projects can carry their own committed `.lazywingman.toml` so a fresh
clone is already configured (see [Configuration](#configuration)).

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
| `l` / `Enter` | expand, or **attach** to the selected shell (a one-shell feature attaches straight away — see the folded row below) |
| `i` | import a project (absolute repo path) |
| `I` | **scan** `[discovery].scan_root` for new projects (discovery rescan) |
| `n` | new feature (worktree + branch + agent shell) |
| `a` | add a shell to the selected feature |
| `s` | **quick-send** a line to the selected shell (or to a one-shell feature's row) |
| `p` | **predefined messages** for the selected shell (or a one-shell feature's row) — a modal of configured presets (`j`/`k` moves, `Enter` sends, `1`-`9` send directly, `Esc` closes) |
| `d` | on a feature: delete it (kills windows, removes worktree; `f` = force when dirty). On a shell row: close that shell (kills its window only — the feature and its other shells stay) |
| `r` | refresh — reconcile state against reality |
| `?` | help overlay (grouped, scrollable: `j`/`k`, `PgUp`/`PgDn`) |
| `q` / `Esc` / `ctrl-c` | quit (ctrl-c works from any state) |
| `Esc` / `q` / `Enter` (startup) | dismiss the startup diagnostics overlay (shown when config problems were dropped at load) |

Mouse: click a row selects, double-click opens (same as `Enter`), the wheel
scrolls the tree, statusbar entries are clickable, and clicking outside an
overlay dismisses it.

**Attach & return**: `Enter` on a shell switches your client to that agent's
window. Press the tmux prefix + the *back-key* to return to the dashboard —
`C-b C-o` by default, the second half configurable via `tmux.back_key`. The
chord is bound in the prefix table (never the root table, so no other TUI
ever loses the key).

## Configuration

`$XDG_CONFIG_HOME/lazywingman/config.toml` — everything is commented in the
file written on first run. Example:

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
delete_branch = true         # also delete the feature branch on delete

[discovery]
scan_root = ""               # the scope: projects inside this dir are listed; empty = cwd
max_depth = 4                # how deep a discovery scan walks
skip_dirs = [".git", "node_modules", "target", ".lazywingman-worktrees"]

[defaults]
agent_command = "claude"     # spawned in every new feature shell by default
agent_shell = "bash"

[[messages]]                 # `p` modal: canned prompts for the selected shell
label = "continue"           # what the modal lists (optional — defaults to text)
text = "continue"            # what is sent, exactly as if you had typed it

[[messages]]
label = "merge + drop worktree"
text = "merge to master, drop the worktree"

[[agents]]                   # presets: patterns classify pane status
name = "codex"
command = "codex"
[agents.patterns]
working = ["Working", "Reading", "⠋", "▌"]
waiting = ["❯", "codex>"]
failed = ["Error:", "✖"]

[[projects]]                 # pre-declared projects are imported at startup
path = "/home/you/work/project-b"
agent = "codex"              # optional: preset name
agent_command = "codex exec --yolo"  # optional: inline override
base_branch = "develop"      # optional: per-project override
```

### Layering

Configuration is layered, lowest to highest (closer wins key by key — maps
merge deeply, lists are replaced outright):

1. Built-in defaults
2. Your config — `$XDG_CONFIG_HOME/lazywingman/config.toml` (or
   `--config <path>` / `LW_CONFIG` for the same file elsewhere)
3. Project-local — `.lazywingman.toml` in the working directory or the
   nearest ancestor holding one (the search stops at a git repository root)
4. Project-local overrides — `.lazywingman.local.toml` next to it (gitignore
   this one; it wins over the committed project file)

A project-local file is meant to be committed — a teammate clones the repo
and is already configured. A few keys belong to the user, not the project,
and are **ignored in project files with a diagnostic**: `tmux.back_key`
(your global tmux binding), `worktrees.root` and `discovery.scan_root`
(machine-specific paths). Set those in your user config.

Inspect the resolved config any time:

```
lazywingman --config-dump
theme = "catppuccin-mocha"          # from user
poll_interval_ms = 500              # from user
worktrees.base_branch = "develop"   # from project
```

### Failure behaviour

A malformed config never prevents startup. The smallest addressable unit is
dropped — one bad `[[agents]]` entry, one bad binding, one bad scalar — and
everything else is kept. Unknown keys are reported, not fatal (an older
binary runs a newer config). Every drop is shown once, at startup, in a
**diagnostics overlay** (`Esc`/`q` dismisses, `j`/`k` scrolls) and logged.

Projects imported from inside the TUI (`i`, `I`) live in the state file, not
in the config; config `[[projects]]` entries are merged in at startup and
consulted for per-project agent settings.

## Project discovery

LazyWingman lists exactly the projects **inside the current path** — the
scope — and nothing else. Different scopes never mix; projects you worked
on elsewhere stay saved in state but are hidden until you start there
again. There is no cache and no prompt: finding `.git` entries is a few
stats per directory, done live.

- **Start inside a git repo**: that repo is the scope. It is auto-imported
  and, because it is the only project, the tree shows its **features flat**
  at the top level (no project row — the project is obvious). A lone
  *missing* project still shows its row so the problem is visible.
- **Start in a folder of repos**: every repo inside it is listed (walk is
  bounded by `max_depth`, skips hidden dirs and `skip_dirs`; linked-worktree
  `.git` files are never treated as repos — their main repo covers them).
  Up to 5 projects are expanded so their features are visible; more than 5
  stay collapsed — you unfold the ones you care about.
- **Worktrees load lazily**: `git worktree list` is the expensive part, so
  it runs once per project when you expand it (or for the flattened single
  project at boot). Listing 200 repos costs 200 directory stats, not 200
  git calls.
- **`I`** rescans the scope at any time and adds new repos. It is the
  stronger variant of `i` (single path).
- **Missing projects** are kept, not pruned: a project whose path is
  currently not a git repo shows `(missing)` dimmed in the tree and
  re-activates automatically when the path is a repo again. If it is gone
  for good, drop it from `state.json` (and from `[[projects]]` in
  `config.toml` if it was declared there).

### Themes

`$XDG_CONFIG_HOME/lazywingman/themes/<name>.toml`. Colours are named roles
(`background`, `accent`, `tree_project`, `status_working`, …) written as
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

### Agent status

Every `poll_interval_ms`, LazyWingman captures each agent pane
(`capture-pane`) and classifies it against the per-agent regex patterns:
`dead` → `failed` → `working` → `waiting` → `ended` → `idle`. A shell whose
pane is gone from tmux — it exited (ctrl-d) or its window was killed outside
the dashboard — is `dead`, as is a pane whose process died with
`remain-on-exit` set; a live shell with no agent activity is `ended`.
Heuristics only — the live preview panel lets you confirm what's really
happening.

A `dead` entry is **muted whole**: the row's guide, icon, name and status
all take the dead colour instead of only the status word. The rule climbs
the tree — a feature is muted once every one of its shells is dead, a
project once every one of its features is (a feature with no shells is not
dead, it was simply never started) — so a finished feature reads as history
at a glance without hunting for the status word at the end of the row.

## How it works with tmux

- LazyWingman detects its own coordinates (`TMUX` env, `display-message -t
  pane`) when it runs inside tmux; standalone it still manages the same tmux
  server via its CLI — the server is started automatically if none is
  running. Agent shells live in **dedicated sessions**, one per
  project, named deterministically `wingman-session-<project>` — your own
  session is never touched. Shell windows are named `lw:<feature>/<n>`
  inside that session, so everything is rediscoverable after a restart
  (`tmux list-sessions` → per-session `list-windows`), even if the state
  file is lost.
- All interaction goes through the tmux CLI (`new-session`, `new-window`,
  `send-keys`, `capture-pane`, `switch-client`, `select-window`,
  `kill-window`, `list-panes -a`, `bind-key`); nothing is shell-interpreted.
- **Attach & return (inside tmux)**: `Enter` on a shell = `switch-client` to
  the agent session + `select-window` to the shell's window. The back chord
  (prefix + `C-o` by default, configurable via `tmux.back_key`) is bound in
  the PREFIX table to `switch-client` back to the dashboard window, rebound
  on every startup; legacy root-table bindings from earlier versions are
  removed on startup.
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
- Feature deletion kills the feature's windows — a session whose last shell
  window is killed destroys itself — then `git worktree remove` (with
  `--force` offered when the worktree is dirty). The feature branch goes
  with it unless `worktrees.delete_branch = false`.
  The confirm popup answers to `y`, `Enter` or `d` — pressing `d` twice
  deletes, the way `dd` does in vim.
- Shell deletion (`d` on a shell row) kills that one window and drops the
  shell from the feature — the worktree, the branch and the sibling shells
  are untouched. A closed shell's index is never handed out again while the
  feature lives, so a new shell cannot collide with a live window.

## Layout

Rounded corners, bare titles (no background fill), tree guides and icons.
Two panels and a bar — the tree says what exists, the preview says what
is happening, the bar says what you can press:

```
╭ projects/features ───────╮╭ preview (capture-pane) ─────────────╮
│▾ ▣ repo-a                ││ live tail of the selected shell,    │
│  ├─   ◇ feat-a  waiting  ││ refreshed every poll                │
│  └─ ▾ ◆ feat-b           ││                                     │
│      ├─ ● #0 working     ││     ╭────────────────────────╮      │
│      └─ ● #1 waiting     ││     │ feature 'feat-b' crea… │      │
│▸ ▢ repo-b                ││     ╰────────────────────────╯      │
╰──────────────────────────╯╰─────────────────────────────────────╯
 n feature · a shell · d delete · ? help
```

**The preview panel** shows the tail of `capture-pane -e` for the selected
shell — escape sequences included, so the agent's own colouring (spinners,
diffs, syntax highlighting) is what you see. `[preview].colors` decides how
much of it survives: `full` (default) takes every colour exactly as the pane
emits it, `foreground` keeps the text colours but drops background colours
so the panel keeps the theme's background, and `none` renders raw text.
Status classification always runs on the stripped text, so colouring never
changes how a pane is classified.

Icons: `▣` project holding features, `▢` empty project, `◇` feature with a
clean worktree, `◆` feature with uncommitted changes, `●` shell. Rows carry
no counts — the tree is expanded by default, so the children *are* the
count, and the project icon says whether a folded project holds anything.
Missing projects show `(missing)` dimmed. When only ONE project is in scope its feature rows replace the
project row entirely (the flat layout).

**The folded row** (`feat-a` above): the nominal case is one worktree, one
agent, so a feature with a single shell has no child row at all — its own
row *is* that shell. It carries the shell's status, has no expand marker, and `Enter` / `s` attach and quick-send to it
directly. `a` gives the feature a second shell, and the subtree appears.
Only features with several shells (or none) are expandable.

The statusbar carries the contextual shortcuts and nothing else — no app
name, no session chip, no project count. Notices ("feature 'x' created",
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
