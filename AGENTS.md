# Agent instructions — Wingmux

Tmux-native control centre for supervising coding agents across
repositories and worktrees. Runs inside tmux as a normal pane.

## Read first

- `README.md` — what the app is and the repository → worktree → shell model
- `docs/architecture.md` — design decisions per subsystem, with the reason
- `docs/shortcuts.md` — every keybinding, the rule it satisfies, and every
  deliberately unbound key
- `docs/` — user-facing reference (usage, configuration, theming, …)

## Non-negotiables

- Never print to stdout while the TUI runs; diagnostics go to the log file.
- tmux floor is 3.0 (`#{@user-option}` in formats: shell identity lives on
  pane options). Developed against 3.7b.
- Keybindings follow the LazyGit/LazyDocker/vim tradition; deviations are
  stated in `docs/shortcuts.md` with the reason.

## Workflow

Driven by mise (`mise.toml`); toolchain Rust 1.97 with rustfmt and clippy.

- `mise run build` · `fmt` · `lint` · `test` (tmux integration tests run
  headless on `tmux -L wmtest`)
- **Gate:** `mise run gate` (fmt --check, clippy `-D warnings`, test)
- `mise run install` — release build, links `wingmux-local` and `wing-local`
  into `~/.local/bin`
- `mise run reset-config` — delete `$XDG_CONFIG_HOME/wingmux` so the next run
  rewrites the defaults and shipped themes (on-disk copies shadow new
  built-ins otherwise)

## Releases

Run the `Prepare release` workflow on `main` with a version: it bumps
`Cargo.toml`, tags, and dispatches `release.yml` (cargo-dist), which builds
the four targets, creates the GitHub release and pushes the formula to
`ike-forge/homebrew-tap` (needs the `HOMEBREW_TAP_TOKEN` secret).
`release.yml` is generated: edit `dist-workspace.toml`, run `dist generate`.

## Definition of done

- Gate green; no ignored tests, no warnings
- `README.md` and `docs/` updated in the same commit when the change
  invalidates them

## Git

- Conventional commits, one per finished change (`feat: …`, `fix: …`,
  `chore: …`); plain `git commit`, no identity overrides.
- Push at the end of each finished unit of work. Never force-push or
  rewrite history.
