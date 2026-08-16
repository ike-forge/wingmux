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
`$XDG_CONFIG_HOME/lazywingman/config.toml` and copies the three built-in
themes to `$XDG_CONFIG_HOME/lazywingman/themes/`.

## Keybindings

| Key | Action |
|-----|--------|
| `j` / `k` (or arrows) | move selection |
| `h` | collapse / jump to parent |
| `l` / `Enter` | expand, or **attach** to the selected shell |
| `i` | import a project (absolute repo path) |
| `I` | **scan** `[discovery].scan_root` for new projects (discovery rescan) |
| `n` | new feature (worktree + branch + agent shell) |
| `a` | add a shell to the selected feature |
| `s` | **quick-send** a line to the selected shell (no leaving the dashboard) |
| `d` | delete feature (kills windows, removes worktree; `f` = force when dirty) |
| `x` | remove project from LazyWingman (windows killed, worktrees kept) |
| `r` | refresh — reconcile state against reality |
| `?` | help overlay |
| `q` / `Esc` | quit / close popup |

**Attach & return**: `Enter` on a shell switches your client to that agent's
window. Press the *back-key* to return to the dashboard — default
`C-o`, configurable via `tmux.back_key`. It is registered globally in tmux on
startup.

## Configuration

`$XDG_CONFIG_HOME/lazywingman/config.toml` — everything is commented in the
file written on first run. Example:

```toml
theme = "dracula"            # dracula | gruvbox | ansi | your own
poll_interval_ms = 500       # status/preview refresh
confirm_on_delete = true

[tmux]
back_key = "C-o"             # jump back to the dashboard from any agent window

[worktrees]
root = ""                    # default: <parent-of-repo>/.lazywingman-worktrees
base_branch = "master"       # falls back to the repo's current branch
delete_branch = false        # also delete the feature branch on delete

[discovery]
scan_root = ""               # `I` key / startup scan prompt root; empty = cwd
max_depth = 4                # how deep a discovery scan walks
skip_dirs = [".git", "node_modules", "target", ".lazywingman-worktrees"]

[defaults]
agent_command = "claude"     # spawned in every new feature shell by default
agent_shell = "bash"

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

Projects imported from inside the TUI (`i`, `I`) live in the state file, not
in the config; config `[[projects]]` entries are merged in at startup and
consulted for per-project agent settings.

## Project discovery

LazyWingman finds your repos so you don't have to import them one by one:

- **Start in a git repo**: if the working directory (or an ancestor) is
  inside a git repository, that repo's **main checkout** is auto-imported as
  a project and every linked worktree becomes a feature with 0 shells (press
  `n` on it to spawn an agent). Booting from inside a linked worktree
  resolves to its main repo.
- **Start outside a repo**: a startup prompt asks whether to scan subfolders
  for git repos. Accepting walks the tree (bounded by `max_depth`, skipping
  hidden dirs and `skip_dirs`; linked-worktree `.git` files are never
  treated as repos — their main repo covers them) and imports everything
  found.
- **`I`** rescans `[discovery].scan_root` (default: the directory
  LazyWingman was started from) at any time and adds new repos. It is the
  stronger variant of `i` (single path).
- Scan results are cached in `$XDG_CACHE_HOME/lazywingman/discovered.json`
  (regenerable data) and shown as context in the startup prompt.
- **Missing projects** are kept, not pruned: a project whose path is
  currently not a git repo shows `(missing)` dimmed in the tree and
  re-activates automatically when the path is a repo again. Remove it
  explicitly with `x` if it is gone for good.

### Themes

`$XDG_CONFIG_HOME/lazywingman/themes/<name>.toml`. Colours are named roles
(`background`, `accent`, `tree_project`, `status_working`, …) written as
`#rrggbb` hex, an ANSI-16 name (`cyan`, `bright_black`, `gray`, …), or
`default` (terminal default). Three built-ins ship with the binary:

- **dracula** — truecolor; falls back to xterm-256 when `COLORTERM` doesn't
  advertise 24-bit support
- **gruvbox** — truecolor with the same fallback
- **ansi** — ANSI-16 palette only, so it inherits your terminal's colours

### Agent status

Every `poll_interval_ms`, LazyWingman captures each agent pane
(`capture-pane`) and classifies it against the per-agent regex patterns:
`dead` → `failed` → `working` → `waiting` → `ended` → `idle`. A pane whose
process died is `dead`; a shell with no agent activity is `ended`. Heuristics
only — the live preview panel lets you confirm what's really happening.

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
  the agent session + `select-window` to the shell's window. The back-key
  (default `C-o`, configurable via `tmux.back_key`) is bound server-wide to
  `switch-client` back to the dashboard window, rebound on every startup.
- **Attach (standalone)**: the terminal is handed over to `tmux attach -t
  <agent-session>` (the shell's window is preselected); detaching with `C-b
  d` returns you to the dashboard.
- **Quick-send** = an in-TUI popup that `send-keys -l` the line to the
  selected pane.
- State (`$XDG_STATE_HOME/lazywingman/state.json`) is reconciled against
  reality on startup: worktrees are checked against `git worktree list`,
  agent sessions and their windows against `tmux list-sessions` /
  `list-windows`; stale entries self-heal and running shells are
  rediscovered (even with a deleted state file, from the pane's cwd + git
  metadata).
- Feature deletion kills the feature's windows — a session whose last shell
  window is killed destroys itself — then `git worktree remove` (with
  `--force` offered when the worktree is dirty). The branch is kept unless
  `worktrees.delete_branch = true`.

## Layout

```
┌ projects/features ──────┐┌ detail ────────────────────────────┐
│▾ repo-a (2 features)    ││ feat-x                              │
│  ▸ feat-a [1 shell]     ││ branch: feat-x                      │
│▾ ▾ feat-b [2 shells]    ││ worktree: …/.lazywingman-worktrees… │
│    #0 working  @5       ││ status: clean                       │
│    #1 waiting  @6       ││ shells: #0 working bash …           │
└─────────────────────────┘└─────────────────────────────────────┘
┌ preview (capture-pane) ──────────────────────────────────────────┐
│ live tail of the selected shell, refreshed every poll            │
└───────────────────────────────────────────────────────────────────┘
 lazywingman · dashboard: work   …messages…   poll 500ms · ? help · q quit
```

## Development

```sh
cargo test          # unit tests (config, themes, git, status) + real-tmux integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Logs go to `$XDG_STATE_HOME/lazywingman/lazywingman.log` — never stdout
(stdout belongs to the TUI).
