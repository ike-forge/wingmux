# Configuration

`$XDG_CONFIG_HOME/wingmux/config.toml` — written on first run, and only if
it isn't there already. It lists every setting Wingmux understands, each
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
root = ""                    # default: <parent-of-repo>/.wingmux-worktrees
base_branch = ""             # empty = the repo's current branch; a named one must exist
delete_branch = true         # also delete the worktree branch on delete

[sync]
fetch_on_startup = true      # fetch each repository's upstream once, for its ↑ahead ↓behind
max_fetches = 4              # fetches running at once; the rest queue

[discovery]
scan_root = ""               # the scope: repositories inside this dir are listed; empty = cwd
max_depth = 4                # how deep a discovery scan walks
skip_dirs = [".git", "node_modules", "target", ".wingmux-worktrees"]

[defaults]
agent = "claude"             # `[[agents]]` preset new worktrees start with; the `n` popup's prefill
agent_command = "claude"     # command behind it, when `agent` names no preset
agent_shell = "bash"       # shell agent windows run under; a plain shell uses your $SHELL instead
agent_hooks = true           # instrument recognised `claude`/`codex` commands for accurate status

[icons]                      # full key reference in ui.md
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
text = "run CI, push <branch> to origin (no force-push)"

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

## Layering

Configuration is layered, lowest to highest (closer wins key by key — maps
merge deeply, lists are replaced outright):

1. Built-in defaults
2. Your config — `$XDG_CONFIG_HOME/wingmux/config.toml`
3. Repository-local — `.wingmux.toml` in the working directory or the
   nearest ancestor holding one (the search stops at a git repository root)
4. Repository-local overrides — `.wingmux.local.toml` next to it
   (gitignore this one; it wins over the committed repository file)
5. The file you name — `wingmux --config <path>` (or `WINGMUX_CONFIG=<path>`;
   the flag wins over the env var)

A repository-local file is meant to be committed — a teammate clones the repo
and is already configured. A few keys belong to the user, not the repository,
and are **ignored in repository files with a diagnostic**: `worktrees.root`
and `discovery.scan_root` (machine-specific paths), and `[sync]` (network
use is your call, not the repository's). Set those in your user
config — or in the file you pass to `--config`, which is you speaking too.

`--config` is a layer, not a replacement: it is read last, so it wins, but the
keys it leaves alone keep whatever the layers below resolved to. A file with
one line in it is a valid `--config`:

```
wingmux --config ~/configs/slow-poll.toml
```

Inspect the resolved config any time:

```
wingmux --config-dump
theme = "catppuccin-mocha"          # from user
poll_interval_ms = 500              # from user
worktrees.base_branch = "develop"   # from repository
```

## Failure behaviour

A malformed config never prevents startup. The smallest addressable unit is
dropped — one bad `[[agents]]` entry, one bad binding, one bad scalar — and
everything else is kept. Unknown keys are reported, not fatal (an older
binary runs a newer config). Every drop is shown once, at startup, in a
**diagnostics overlay** (`Esc`/`q` dismisses, `j`/`k` scrolls) and logged.

Repositories are not stored: each start scans the scope and merges in
config `[[repositories]]` entries. A repository's base branch and agent are
read from config whenever they are used, so a config edit applies at the
next start without touching any state.
