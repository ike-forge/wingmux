# Repository discovery

Wingmux lists exactly the repositories **inside the current path** — the
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
  Wingmux is running appears the next time you start it in that scope; `r`
  refreshes tmux state (windows, panes, worktrees), not the repo list. To add
  a repository that is not in the scope, declare it under `[[repositories]]`
  in `config.toml`.
- **Hidden repositories**: `.` folds a repository into the `N hidden` row at
  the bottom of the tree (see [shortcuts](shortcuts.md#hidden-repositories)).
  Discovery still lists it; it is only drawn there, starts collapsed and is
  not fetched at startup. The hidden set lives in the state file, keyed by
  path: a new or renamed folder is visible, a deleted one is dropped. When you
  start inside a repository, its own hide flag is ignored.
- **Missing repositories**: a repository whose path stops being a git repo
  while the dashboard runs shows `(missing)` dimmed after `r` and
  re-activates when the path is a repo again. The next start does not list
  it.
