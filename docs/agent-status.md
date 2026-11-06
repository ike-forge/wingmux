# Agent status

Every `poll_interval_ms`, Wingmux captures each agent pane
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

- **Claude**: `--settings <state_dir>/wingmux-activity-hooks.json`, a
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
