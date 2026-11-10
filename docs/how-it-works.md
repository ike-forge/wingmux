# How it works with tmux

- Wingmux detects its own coordinates (`TMUX` env, `display-message -t
  pane`), and `exec`s into `tmux new-session -A -s wingmux -- wingmux`
  when there are none — a launch from a bare shell ends up a tmux pane like
  any other. A second launch in the same folder attaches the dashboard already
  showing it (its window is stamped with the scope) rather than starting a
  rival one. Agent shells live in **dedicated sessions**, one per repository, named
  deterministically `wingmux-session-<repository>` — your own session is never
  touched. Each shell's identity and launch command live on its tmux pane
  (`@wm_*` options), so shells are rediscovered after a restart even if the
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
- **Pane navigation is yours.** Wingmux binds no movement key and sets no
  chord in your tmux.
- **New worktree**: the branch and worktree are cut from the repository's
  **base branch**: the branch its main checkout is on, unless
  `worktrees.base_branch` (or the repository's override) names one. The New
  Worktree popup shows it as a `base branch` field under the agent; a
  configured branch missing from the repo reads `(not found)` and creation
  refuses.
- **Predefined messages** are pasted to the selected pane via tmux
  paste-buffer with bracketed paste — multi-line prompts arrive as one
  paste, not as premature Enter presses.
- State (`$XDG_STATE_HOME/wingmux/state.json`) holds only what
  Wingmux recorded about the worktrees it created (name, creation time,
  fork point, agent), keyed by path, and the repositories hidden with `.`.
  Worktrees come from `git worktree list`.
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
  deleting the branch discards them"). Only the warning names the reason.
  The confirm popup answers to `y`, `Enter` or `d` — pressing `d` twice
  deletes, the way `dd` does in vim.
- Shell deletion (`d` on a shell row) kills that one window and drops the
  shell from the worktree — the worktree, the branch and the sibling shells
  are untouched. A closed shell's index is never handed out again while the
  worktree lives, so a new shell cannot collide with a live window.
