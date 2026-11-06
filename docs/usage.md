# Usage

Every action in full. The rule behind each key is in [`shortcuts.md`](shortcuts.md); `?` shows the same list in the app.

| Key | Action |
|-----|--------|
| `j` / `k` (or arrows) | move selection |
| `g` / `G`, `Home` / `End` | first / last item |
| `PgUp` / `PgDn`, `ctrl-u` / `ctrl-d` | half page up / down |
| `ctrl-f` / `ctrl-b` | full page up / down |
| `h` | collapse / jump to parent |
| `l` / `Enter` | expand, or **enter** the selected shell's docked pane (a one-shell worktree goes straight in — see [the folded row](ui.md)). Simply moving onto a shell already docks it; `Enter` is what puts the cursor there. On a **repository** row it only expands or collapses |
| `n` | new worktree (git worktree + branch + agent shell) — a prompt asks for the name and shows the **base branch** it will be cut from: the branch the repository's main checkout is on, unless `worktrees.base_branch` (or a per-repository override) names one. A second field says which **agent** the worktree's shells run, prefilled with the repository's own (`[defaults].agent`, unless the repository overrides it): `Tab` moves onto it, `↑`/`↓` walk the configured `[[agents]]` presets, and anything else typed there is taken as the command itself. So a name and `Enter` is still the whole flow, and `codex` beside a `claude` repository costs one `Tab` |
| `a` | add a shell to the selected worktree — a picker asks what it runs: the worktree's own agent command first (the one it was created with, else the repository's; `Enter` takes it), then the other configured `[[agents]]` presets, then a **plain shell** that just drops you at your own `$SHELL` prompt with nothing sent to it (`j`/`k` moves, `1`-`9` pick directly, `Esc` closes). Anything else you want to run, you type in that plain shell. On a **repository** row, `a` starts (or enters) the repository's own inline shell — no picker |
| `p` | **predefined messages** for the selected shell (or a one-shell worktree's row) — a modal of configured presets, each listed in full with its `<base_branch>`-style tokens already resolved (`j`/`k` moves, `Enter` sends, `1`-`9` send directly, `Esc` closes) |
| `t` | **repository todo pad** — a scratch pad for the next steps you cannot start yet ("rebase the API worktree once the schema lands"). One per repository, shared by its worktrees: on any row of a repository, `t` opens that repository's pad. It splits off the bottom of the tree's column and takes half of it while you type, so neither the tree above nor the preview — or the docked agent — beside it is covered. `Esc` saves and hands the keyboard back to the tree, leaving the note on screen collapsed to its own height (five lines of text, a five-line panel; never more than the open half) and following the selection from repository to repository; an emptied pad takes the panel away with it. `ctrl-s` saves without leaving, and `ctrl-e` hands the file to your `$EDITOR` in a tmux window of its own — closing the editor reads it back in. Parked as plain text in `$XDG_STATE_HOME/wingmux/todos/<escaped repository path>.md` (yours to edit with `$EDITOR` too) |
| `T` | **global todo pad** — the same panel, one pad for the whole machine, for notes written about one repository and needed while looking at another. Stored in `$XDG_STATE_HOME/wingmux/todo.md`. The parked panel shows whichever pad was opened last |
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
