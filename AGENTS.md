# Agent instructions — LazyWingman

Tmux-native control centre for supervising coding agents (Claude Code,
Codex, Mistral Vibe, …) across multiple projects and features in parallel.
Runs inside tmux as a normal pane of the user's existing session.

## Read these first

- `README.md` — what the app is; the core model (project → feature →
  shell) and the UI model
- `STATE.md` — milestone history, the verification recipe, environment
  gotchas and decisions; kept current per milestone
- `DECISIONS.md` — design decisions and their reasons
- `/opt/data/agents-common-instructions/tui/README.md` — the global TUI
  design rules this project opts into (LazyGit/LazyDocker tradition)
- `/opt/data/agents-common-instructions/general/README.md` — global rules
  applying to every project (AGENTS.md discipline, project hygiene)

Hierarchy: project-level rules here win over the global instructions; the
global instructions win over generic defaults. When deviating from the
global TUI rules on purpose, say so here with the reason.

## Non-negotiables

- **Never print to stdout while the TUI is running** — it corrupts the
  screen. All diagnostics go to the log file.
- **No root in this environment**: install into `$HOME/.local` (deb
  extraction), never `apt-get install`.
- tmux needs `LD_LIBRARY_PATH=$HOME/.local/tmux-prefix/usr/lib/x86_64-linux-gnu`
  (tmux 3.5a installed non-root).
- Container `TERM=dumb`: use `TERM=xterm-256color` for tmux commands.
- No `zip` binary: `python3 -m zipfile -c out.zip dir`.
- `~/.cargo/bin` and `~/.local/bin` must be on PATH.

## Workflow

- Build: `cargo build`
- Format: `cargo fmt` — check: `cargo fmt --check`
- Lint: `cargo clippy -- -D warnings`
- Test: `cargo test` (tmux integration tests run headless with `tmux -L lwtest`;
  the full verify recipe lives in `STATE.md`)
- **Gating command:** `cargo fmt --check && cargo clippy -- -D warnings && cargo test`

## Definition of done

- Gating command green: fmt clean, clippy zero warnings, all tests pass
- `STATE.md` milestone section updated in the same commit as the change
- `README.md` / `DECISIONS.md` updated in the same commit when the change
  invalidates them
- No ignored tests, no warnings

## Git and commit conventions

- Conventional-ish commits; milestone-prefixed for milestones (`M7: …`,
  `chore: …`)
- Identity: `LazyWingman Builder <builder@lazywingman.local>`
- Commit freely; **push at the end of every finished unit of work**
  (feature/change). Doc/planning commits may be pushed midway (optional).
  Never force-push or rewrite history.
- Remote: `origin` → `git@github.com:ikempf/lazywingman.git` (deploy key)

## Links out

- Global rules: `/opt/data/agents-common-instructions/` (`general/` +
  `tui/`) — read them at task time, they are the single source of truth and
  may change.
