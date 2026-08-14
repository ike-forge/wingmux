# LazyWingman

A tmux-native control centre for supervising coding agents (Claude Code,
Codex, Mistral Vibe, …) across **multiple projects and multiple features in
parallel**. See everything at a glance, jump into any agent with one
keystroke, spin up and tear down feature workspaces instantly.

LazyWingman runs **inside** tmux, as a normal pane of your existing session —
it cooperates with the session you're already in instead of trying to own the
terminal.

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

Requirements: Rust ≥ 1.85, `git`, `tmux` (≥ 3.0). Run it from a pane of the
tmux session you want to supervise:

```sh
lazywingman
```

Outside tmux it refuses to start with a clear message; set
`LAZYWINGMAN_NO_TMUX=1` to run the UI anyway (tmux actions will fail).

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

Projects imported from inside the TUI (`i`) live in the state file, not in the
config; config `[[projects]]` entries are merged in at startup and consulted
for per-project agent settings.

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
  pane`) and stays in its pane; it never creates or switches sessions.
- Agent shells live in **sibling windows of your session**, named
  deterministically `lw:<project>/<feature>/<n>` — rediscoverable after a
  restart, and safe to touch from plain tmux.
- All interaction goes through the tmux CLI (`new-window`, `send-keys`,
  `capture-pane`, `select-window`, `kill-window`, `list-panes -a`,
  `bind-key`); nothing is shell-interpreted.
- **Attach** = `select-window` to the agent's window (the client follows).
  **Back** = a global `bind-key -n <key> select-window` to the dashboard
  window, rebound on every startup.
- **Quick-send** = an in-TUI popup that `send-keys -l` the line to the
  selected pane.
- State (`$XDG_STATE_HOME/lazywingman/state.json`) is reconciled against
  reality on startup: worktrees are checked against `git worktree list`,
  windows against `tmux list-windows`; stale entries self-heal and running
  shells are rediscovered (even with a deleted state file, from the pane's
  cwd + git metadata).
- Feature deletion kills the feature's windows, then `git worktree remove`
  (with `--force` offered when the worktree is dirty). The branch is kept
  unless `worktrees.delete_branch = true`.

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
 lazywingman · session work   …messages…   poll 500ms · ? help · q quit
```

## Development

```sh
cargo test          # unit tests (config, themes, git, status) + real-tmux integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Logs go to `$XDG_STATE_HOME/lazywingman/lazywingman.log` — never stdout
(stdout belongs to the TUI).
