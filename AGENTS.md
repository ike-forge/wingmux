# Agent instructions — LazyWingman

Tmux-native control centre for supervising coding agents (Claude Code,
Codex, Mistral Vibe, …) across multiple repositories and worktrees in
parallel. Runs inside tmux as a normal pane of the user's existing session.

## Read these first

- `README.md` — what the app is; the core model (repository → worktree →
  shell) and the UI model
- `docs/architecture.md` — how the app is built and why: the design
  decisions per subsystem, with the reason each one was made
- `docs/shortcuts.md` — every keybinding, the TUI rule it satisfies, and
  every deliberately unbound key
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
  (installed non-root; currently tmux 3.7b — `list-keys` output is
  column-aligned and its padding differs from 3.5, so never parse it by
  fixed-width prefix).
- **Do not re-run `hermes verify --save`**: `.hermes/environment.json` has
  `start: null` on purpose (a TUI with no HTTP endpoint), and detection
  regenerates `start: cargo run`, whose readiness poll then fails.
- Container `TERM=dumb`: use `TERM=xterm-256color` for tmux commands.
- No `zip` binary: `python3 -m zipfile -c out.zip dir`.
- `~/.cargo/bin` and `~/.local/bin` must be on PATH.

## Workflow

Driven by **mise** (`mise.toml` at the repo root — the single task runner
per the global task-runner rules; task commands below are also documented
there). Toolchain: `[tools] rust = "1.97"`.

- Build: `mise run build` (cargo build)
- Format: `mise run fmt` (cargo fmt --check)
- Lint: `mise run lint` (cargo clippy -- -D warnings)
- Test: `mise run test` (cargo test; tmux integration tests run headless
  with `tmux -L lwtest`)
- **Gating command:** `mise run gate` (fmt --check && clippy -D warnings
  && test)
- Install for local testing from anywhere: `mise run install` (release
  build + symlink into `~/.local/bin`)
- Start from a clean config: `mise run reset-config` (deletes
  `$XDG_CONFIG_HOME/lazywingman` — `config.toml` and the theme copies —
  and lists what it removed first; the next run writes the defaults and
  the shipped themes back). `state.json` is untouched, so discovered
  repositories survive. Needed whenever a shipped theme or the default
  config changes: the on-disk copies are only written when absent, so an
  existing one shadows the new built-in for good.

The underlying cargo commands (from the pre-mise workflow, kept for
reference): `cargo build`, `cargo fmt --check`, `cargo clippy -- -D
warnings`, `cargo test`.

## Definition of done

- Gating command green: fmt clean, clippy zero warnings, all tests pass
- `README.md`, `docs/architecture.md` and `docs/shortcuts.md` updated in
  the same commit when the change invalidates them
- No ignored tests, no warnings

## Git and commit conventions

- Conventional-ish commits, one per finished change (`feat: …`, `fix: …`,
  `chore: …`)
- Identity: `ike <ike.dev@pm.me>` — it comes from the ambient git
  config for this directory, so plain `git commit` is correct; never pass
  `-c user.name=…`/`-c user.email=…` overrides.
- Commit freely; **push at the end of every finished unit of work**
  (feature/change). Doc/planning commits may be pushed midway (optional).
  Never force-push or rewrite history.
- Remote: `origin` → `git@github.com:ikempf/lazywingman.git` (deploy key)

## Links out

- Global rules: `/opt/data/agents-common-instructions/` (`general/` +
  `tui/`) — read them at task time, they are the single source of truth and
  may change.
- Task runner convention: `general/task-runner.md` (mise as the one task
  runner) — this project follows it; the choice is stated in the Workflow
  section above.
