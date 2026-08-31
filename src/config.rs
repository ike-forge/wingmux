//! Configuration loading, defaults, and first-run bootstrap.
//!
//! Config lives at `$XDG_CONFIG_HOME/lazywingman/config.toml`, layered with
//! a repository-local `.lazywingman.toml` (searched upward from the working
//! directory, stopping at the first hit / repository root) and its ignored
//! sibling `.lazywingman.local.toml`. On first run a fully commented default
//! config is written (and built-in themes are copied into `themes/`).
//!
//! Loading is lenient per the configuration guidelines (drop the smallest
//! addressable unit, never prevent startup): one bad agent/repository entry,
//! one bad binding, one bad scalar — each is dropped with a diagnostic in
//! `Config.diagnostics` while everything else is kept. Unknown keys are
//! reported, not fatal. `from_str` stays strict for tests.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use unicode_width::UnicodeWidthStr;

use crate::error::{Error, Result};

/// Layer labels recorded in `Config.sources` for `--config-dump`
/// ("key = value  # from <layer>").
pub const LAYER_DEFAULTS: &str = "defaults";
pub const LAYER_USER: &str = "user";
pub const LAYER_REPOSITORY: &str = "repository";
pub const LAYER_LOCAL: &str = "local";

/// Fully commented default config, written on first run.
pub const DEFAULT_CONFIG: &str = r#"# LazyWingman configuration — written on first run, edit freely.
# Location: $XDG_CONFIG_HOME/lazywingman/config.toml (default ~/.config/lazywingman)
#
# Every setting LazyWingman understands is listed below, commented out at
# its built-in default — so this file changes nothing until you edit it.
# Two comment styles are used: `#key = value` (no space after the hash) is
# a real default, and uncommenting it as-is is a no-op; `# text` (hash then
# space) is prose, or an example with no default behind it. Section headers
# like [preview] are left active on purpose, so uncommenting a single key
# under one is enough.
#
# This file is the USER layer. A repository can also carry a committed
# .lazywingman.toml (searched upward from the working directory, stopping at
# the repository root) and a gitignored .lazywingman.local.toml next to it;
# those layers override keys here. Some keys are user-only and are ignored
# in repository files with a diagnostic: tmux.back_key, worktrees.root,
# discovery.scan_root. Inspect the effective config with `lazywingman
# --config-dump`; use --config <path> or LW_CONFIG to point at a different
# user file. A malformed config never prevents startup — the smallest bad
# unit is dropped and reported once in a startup diagnostics overlay.

# Theme to use. Built-ins: "catppuccin-mocha", "catppuccin-macchiato",
# "catppuccin-frappe", "catppuccin-latte", "dracula", "gruvbox", "ansi".
# Drop your own theme into ~/.config/lazywingman/themes/<name>.toml
# Press ctrl-t inside the TUI to switch themes live (picked themes are
# saved to the state file; this key is the startup default).
#theme = "catppuccin-mocha"

# How often (ms) agent panes are polled for status + preview.
#poll_interval_ms = 500

# Ask for confirmation before deleting a worktree.
#confirm_on_delete = true

[preview]
# How much of the agent's own colouring the preview panel shows. The pane is
# captured with its escape sequences, so by default you see exactly what the
# agent painted — spinners, diffs, syntax highlighting.
#   "full"       — every colour exactly as the pane emits it
#   "foreground" — text colours kept, background colours dropped (the panel
#                  keeps the theme's background; also "no-background")
#   "none"       — raw text, no colours at all (also "text")
#colors = "full"

[tmux]
# Keybinding (tmux syntax) pressed AFTER the tmux prefix (e.g. `C-b C-o`)
# inside an agent window to jump straight back to the LazyWingman
# dashboard. Bound in the PREFIX table, never the root table — a root
# binding would hijack the key for every session and other TUIs.
#back_key = "C-o"

[worktrees]
# Root directory for the worktrees. Absolute path, or relative to the
# repository's parent directory. Empty = <repository-parent>/.lazywingman-worktrees
#root = ""
# Branch that worktree branches are cut from. Falls back to the repository's
# current branch if this one doesn't exist in a repository.
#base_branch = "master"
# Also delete the worktree branch when the worktree is removed. Set to
# false to keep the branch behind after the worktree is gone.
#delete_branch = true

[discovery]
# The scope: only repositories inside this directory are listed (scopes never
# mix). Empty = the directory LazyWingman was started from. When you start
# inside a git repo, that repo is the scope and its worktrees are shown
# flat (no repository row). The scan is live (no cache) and runs at startup.
#scan_root = ""
# Maximum subdirectory depth of a scan (0 = the scan root itself only).
#max_depth = 4
# Directory names skipped during a scan. Hidden directories (names starting
# with '.') are always skipped; worktree `.git` files are never treated as
# repositories (their main repo covers them).
#skip_dirs = [".git", "node_modules", "target", ".lazywingman-worktrees"]

[defaults]
# Agent command spawned in a worktree shell unless a repository overrides it.
#agent_command = "claude"
# Shell used for agent windows.
#agent_shell = "bash"
# Inject Claude Code hooks (--settings <generated file>) into recognised
# `claude` commands, so status is read from the agent's own hook events
# instead of guessed from regex patterns on the pane's output. Nothing is
# written to your own Claude config; the flag is visible in the pane's
# command line. Set to false to disable and always use the regex heuristics.
#agent_hooks = true

[icons]
# Glyph set to start from: "nerdfont" (a patched font), "unicode" (no
# patched font needed) or "none" (no row icons).
#preset = "nerdfont"

# Every key below overrides the preset glyph by glyph. An empty string
# draws nothing. `guide_branch`/`guide_last` are padded as a pair, and the
# three `repository*` states, the four `worktree_*` states and the
# `status_*` icons (spinner frames included) each as a group, so names stay
# aligned when one of them is empty. `shell`,
# `cursor`, `separator`, `warn` and `ellipsis` have no sibling to pad
# against and are used verbatim — add your own
# trailing space if you want a gap after one (the default `shell` below
# already has one baked in). The values shown are the "nerdfont" preset.
#repository = "󰉋"              # repository holding worktrees, collapsed
#repository_open = "󰝰"         # ...the same repository, expanded
#repository_empty = "󰉖"        # repository with no worktrees yet
#worktree_new = ""            # untouched: no commits, clean tree
#worktree_ahead = ""          # commits not in the base yet, or dirty
#worktree_merged = ""         # all in the base branch, clean tree
#worktree_deleted = "󰗨"        # its directory is gone; only `d` is left
# One agent status icon each, drawn in front of the status label. The
# spinner is a string of frames, one character per animation step, cycled
# while an agent is working; `idle` and `dead` are blank on purpose, so a
# state you cannot act on stays quiet.
#status_spinner = "⣾⣷⣯⣟⡿⢿⣻⣽" # agent is working
#status_waiting = "󰏤"          # agent is parked at its prompt
#status_blocked = "󰓛"          # agent is asking for permission
#status_failed = ""           # the agent reported a failure
#status_ended = ""            # the agent exited, a shell is left
#status_idle = ""              # agent running, state unrecognised
#status_dead = ""              # the pane is gone
#shell = "● "                  # a shell inside a worktree
#cursor = "▌"                  # marks the selected row
#guide_branch = "├─"            # tree guide, more siblings below
#guide_last = "└─"              # tree guide, last sibling
#guide_vertical = "│"          # tree guide, carried down a column
#separator = " · "               # between status-bar fields
#warn = "⚠"                    # startup diagnostics indicator
#ellipsis = "…"                # marks text truncated to fit
#enter = "↵"                   # names the Enter key in shortcut hints
#arrows = "↑↓"                  # names the arrow keys in shortcut hints

# Popup and panel borders. Not a glyph: ratatui owns the corner set.
# "rounded" | "plain".
#border = "rounded"

# Predefined messages, offered by the `p` modal on a selected shell:
# pick one and it is sent to the agent exactly as if you had typed it.
# The modal lists each entry's `text` verbatim, so what you read is what
# the agent gets. These tokens are resolved against the selected shell
# first: <base_branch>, <branch>, <worktree>, <repository>,
# <worktree_path>; anything else in angle brackets is sent as written.
# Defining any [[messages]] entry replaces this whole list, like every
# other list in this file. Entries 1-9 keep a single-key shortcut in the
# modal. The three below are the built-in defaults.
#[[messages]]
#text = "continue"

#[[messages]]
#text = "merge to <base_branch>"

#[[messages]]
#text = "summarise what you've changed and what's still left"

# Agent presets, selectable per repository via `agent = "<name>"`.
# The `patterns` regexes are matched against the tail of a pane's capture
# to classify its status (working / waiting / failed). Order of checks:
# dead > ended > failed > waiting > working > idle. Defining any [[agents]]
# entry replaces this whole list; the two below are the built-in defaults.
#[[agents]]
#name = "claude"
#command = "claude"
# Optional per-preset shell; omitted, [defaults].agent_shell is used.
# shell = "zsh"
#[agents.patterns]
#working = ["Thinking", "Running", "Reading", "Writing", "Searching",
#"Applying", "Planning", "Creating", "Editing", "Executing", "Progress",
#"⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "●", "▰"]
#waiting = ["What would you like to do", "❯", "›", "(q) to quit",
#"q to quit", "> $"]
#failed = ["✖", "Error:", "error:", "exited with code", "Fatal", "failed"]

#[[agents]]
#name = "codex"
#command = "codex"
#[agents.patterns]
#working = ["Working", "Reading", "Writing", "Running", "Thinking",
#"Exploring", "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "▌"]
#waiting = ["❯", "›", "codex>", "> $"]
#failed = ["Error", "error:", "✖"]

# Repositories declared by hand — the way to list a repo that discovery does not
# find (outside the scope). Discovered repositories live in state.json instead.
# There is no default list; the block below is an example only.
# [[repositories]]
# path = "/absolute/path/to/repo"
# base_branch = "main"       # optional: override [worktrees].base_branch
# agent = "claude"           # optional: preset from [[agents]]
# agent_command = "codex exec --yolo"  # optional: inline override
"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: String,
    pub poll_interval_ms: u64,
    pub confirm_on_delete: bool,
    pub preview: PreviewConfig,
    pub tmux: TmuxConfig,
    pub worktrees: WorktreeConfig,
    pub discovery: DiscoveryConfig,
    pub defaults: DefaultsConfig,
    pub icons: IconsConfig,
    pub agents: Vec<AgentConfig>,
    pub repositories: Vec<RepositoryConfig>,
    /// Predefined messages offered by the `p` modal (quick-send presets).
    pub messages: Vec<MessageConfig>,
    /// Diagnostics collected while loading (per configuration rules: a
    /// malformed config never prevents startup — the smallest addressable
    /// unit is dropped and reported here). Filled by `load`; skipped by
    /// the strict `from_str` path used in tests.
    #[serde(skip)]
    pub diagnostics: Vec<String>,
    /// Layer provenance for `--config-dump`: "key = value  # from <layer>"
    /// — which layer (default/user/repository/local) provided each key.
    #[serde(skip)]
    pub sources: Vec<(String, &'static str)>,
}

/// The pane preview panel.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PreviewConfig {
    pub colors: PreviewColors,
}

/// How much of a pane's own colouring the preview shows. Panes are captured
/// with `capture-pane -e`, so the escapes are always there — this only says
/// how much of them survives into the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreviewColors {
    /// Every colour exactly as the pane emits it.
    #[default]
    Full,
    /// Text colours and modifiers kept, background colours dropped — the
    /// panel keeps the theme's background.
    #[serde(alias = "no-background", alias = "no_background")]
    Foreground,
    /// Raw text, no colours at all.
    #[serde(alias = "text", alias = "off")]
    None,
}

impl PreviewColors {
    /// The config spelling of this mode (used by `--config-dump`).
    pub fn as_str(self) -> &'static str {
        match self {
            PreviewColors::Full => "full",
            PreviewColors::Foreground => "foreground",
            PreviewColors::None => "none",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TmuxConfig {
    pub back_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WorktreeConfig {
    /// Absolute, or relative to the repository's parent directory.
    pub root: String,
    pub base_branch: String,
    pub delete_branch: bool,
}

/// Repository discovery: recursive scans for git repositories.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DiscoveryConfig {
    /// Root scanned by the `I` key and the startup scan prompt. Empty =
    /// the directory LazyWingman was started from.
    pub scan_root: String,
    /// Maximum subdirectory depth of a scan (0 = scan root only).
    pub max_depth: usize,
    /// Directory names skipped during a scan (hidden dirs are always
    /// skipped on top of this).
    pub skip_dirs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DefaultsConfig {
    pub agent_command: String,
    pub agent_shell: String,
    /// Inject Claude Code hooks into recognised `claude` commands for
    /// accurate status (M17). Nothing is written to the user's config;
    /// the flag is visible in the pane's own command line.
    pub agent_hooks: bool,
}

/// Border style for popups and panels. Not a glyph string: ratatui's
/// `BorderType` owns the corner set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BorderStyle {
    #[default]
    Rounded,
    Plain,
}

/// Preset names accepted by `[icons].preset`.
pub const ICON_PRESETS: &[&str] = &["nerdfont", "unicode", "none"];

/// Every glyph LazyWingman draws itself. Resolved: each field is the exact
/// string a render site emits, already padded to its column.
///
/// This replaced a `LW_ASCII` boolean. That boolean answered "can this
/// terminal draw box characters?", which is not the question users have —
/// theirs is "which glyphs suit my font?", and the two are independent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconsConfig {
    pub repository: String,
    pub repository_open: String,
    pub repository_empty: String,
    pub worktree_new: String,
    pub worktree_ahead: String,
    pub worktree_merged: String,
    pub worktree_deleted: String,
    /// Frames of the `working` spinner, one per animation step. A frame is
    /// picked by elapsed time at render, so an empty list (the `none`
    /// preset) simply draws nothing.
    pub status_spinner: Vec<String>,
    pub status_waiting: String,
    pub status_blocked: String,
    pub status_failed: String,
    pub status_ended: String,
    pub status_idle: String,
    pub status_dead: String,
    pub shell: String,
    pub cursor: String,
    pub guide_branch: String,
    pub guide_last: String,
    pub guide_vertical: String,
    pub separator: String,
    pub warn: String,
    pub ellipsis: String,
    /// Glyph standing for the Enter key in shortcut hints.
    pub enter: String,
    /// Glyph standing for the arrow keys in shortcut hints.
    pub arrows: String,
    pub border: BorderStyle,
}

impl Default for IconsConfig {
    fn default() -> Self {
        IconsConfig::preset("nerdfont")
    }
}

impl IconsConfig {
    /// The named preset, padded and ready to render. An unknown name falls
    /// back to `nerdfont`; the caller reports the diagnostic (this is also
    /// reached from `Deserialize`, which has no diagnostics vec).
    pub fn preset(name: &str) -> IconsConfig {
        let mut i = IconsConfig::raw_preset(name);
        i.pad();
        i
    }

    /// The preset before padding. Overrides are layered onto this, then
    /// padded once — padding an already-padded value would compound.
    fn raw_preset(name: &str) -> IconsConfig {
        let nerdfont = IconsConfig {
            repository: "󰉋".into(),
            repository_open: "󰝰".into(),
            repository_empty: "󰉖".into(),
            worktree_new: "".into(),
            worktree_ahead: "".into(),
            worktree_merged: "".into(),
            worktree_deleted: "󰗨".into(),
            status_spinner: frames("⣾⣷⣯⣟⡿⢿⣻⣽"),
            status_waiting: "󰏤".into(),
            status_blocked: "󰓛".into(),
            status_failed: "".into(),
            status_ended: "".into(),
            status_idle: String::new(),
            status_dead: String::new(),
            shell: "● ".into(),
            cursor: "▌".into(),
            guide_branch: "├─".into(),
            guide_last: "└─".into(),
            guide_vertical: "│".into(),
            separator: " · ".into(),
            warn: "⚠".into(),
            ellipsis: "…".into(),
            enter: "↵".into(),
            arrows: "↑↓".into(),
            border: BorderStyle::Rounded,
        };
        match name {
            "unicode" => IconsConfig {
                repository: "▣".into(),
                repository_open: "▤".into(),
                repository_empty: "▢".into(),
                worktree_new: "○".into(),
                worktree_ahead: "◆".into(),
                worktree_merged: "✓".into(),
                worktree_deleted: "⊘".into(),
                status_waiting: "⏸".into(),
                status_blocked: "⏹".into(),
                status_failed: "✖".into(),
                status_ended: "⏸".into(),
                ..nerdfont
            },
            "none" => IconsConfig {
                repository: String::new(),
                repository_open: String::new(),
                repository_empty: String::new(),
                worktree_new: String::new(),
                worktree_ahead: String::new(),
                worktree_merged: String::new(),
                worktree_deleted: String::new(),
                status_spinner: Vec::new(),
                status_waiting: String::new(),
                status_blocked: String::new(),
                status_failed: String::new(),
                status_ended: String::new(),
                shell: String::new(),
                ..nerdfont
            },
            _ => nerdfont,
        }
    }

    /// Right-pad the four groups that share a rendered column, so an
    /// empty icon still holds its column and names stay aligned. A
    /// non-empty column also gains one trailing space, so render sites
    /// concatenate icon and name directly and an empty column costs
    /// nothing. Keys with no sibling (`shell`, `cursor`, `separator`,
    /// `warn`, `ellipsis`) are used verbatim.
    ///
    /// Every spinner frame joins the status group: the spinner shares that
    /// one column with the static status icons, and a frame narrower than
    /// its neighbours would make the label jitter as the animation runs.
    fn pad(&mut self) {
        pad_group(&mut [
            &mut self.repository,
            &mut self.repository_open,
            &mut self.repository_empty,
        ]);
        pad_group(&mut [
            &mut self.worktree_new,
            &mut self.worktree_ahead,
            &mut self.worktree_merged,
            &mut self.worktree_deleted,
        ]);
        let mut status: Vec<&mut String> = vec![
            &mut self.status_waiting,
            &mut self.status_blocked,
            &mut self.status_failed,
            &mut self.status_ended,
            &mut self.status_idle,
            &mut self.status_dead,
        ];
        status.extend(self.status_spinner.iter_mut());
        pad_group(&mut status);
        pad_pair(&mut self.guide_branch, &mut self.guide_last);
    }
}

/// Split a spinner string into one frame per character. Multi-character
/// frames are not a thing: the column is one glyph wide.
fn frames(s: &str) -> Vec<String> {
    s.chars().map(|c| c.to_string()).collect()
}

fn pad_pair(a: &mut String, b: &mut String) {
    pad_group(&mut [a, b]);
}

/// Right-pad every icon sharing one rendered column to the widest of them,
/// plus the one trailing space that separates icon from name. All-empty
/// (the `none` preset) keeps the column at zero width.
fn pad_group(group: &mut [&mut String]) {
    let w = group
        .iter()
        .map(|s| UnicodeWidthStr::width(s.as_str()))
        .max()
        .unwrap_or(0);
    if w == 0 {
        return;
    }
    for s in group.iter_mut() {
        let pad = w - UnicodeWidthStr::width(s.as_str());
        s.push_str(&" ".repeat(pad + 1));
    }
}

/// Raw `[icons]` as written in the file: every key optional, so an
/// override can be told apart from a preset value.
#[derive(Deserialize)]
#[serde(default)]
struct IconsFile {
    preset: String,
    repository: Option<String>,
    repository_open: Option<String>,
    repository_empty: Option<String>,
    worktree_new: Option<String>,
    worktree_ahead: Option<String>,
    worktree_merged: Option<String>,
    worktree_deleted: Option<String>,
    status_spinner: Option<String>,
    status_waiting: Option<String>,
    status_blocked: Option<String>,
    status_failed: Option<String>,
    status_ended: Option<String>,
    status_idle: Option<String>,
    status_dead: Option<String>,
    shell: Option<String>,
    cursor: Option<String>,
    guide_branch: Option<String>,
    guide_last: Option<String>,
    guide_vertical: Option<String>,
    separator: Option<String>,
    warn: Option<String>,
    ellipsis: Option<String>,
    enter: Option<String>,
    arrows: Option<String>,
    border: Option<BorderStyle>,
}

impl Default for IconsFile {
    fn default() -> Self {
        IconsFile {
            preset: "nerdfont".into(),
            repository: None,
            repository_open: None,
            repository_empty: None,
            worktree_new: None,
            worktree_ahead: None,
            worktree_merged: None,
            worktree_deleted: None,
            status_spinner: None,
            status_waiting: None,
            status_blocked: None,
            status_failed: None,
            status_ended: None,
            status_idle: None,
            status_dead: None,
            shell: None,
            cursor: None,
            guide_branch: None,
            guide_last: None,
            guide_vertical: None,
            separator: None,
            warn: None,
            ellipsis: None,
            enter: None,
            arrows: None,
            border: None,
        }
    }
}

impl<'de> Deserialize<'de> for IconsConfig {
    // An unknown `preset` falls back to `nerdfont` silently here, while
    // `SectionSalvage::salvage` below pushes a diagnostic for the same
    // case. Not an oversight: both paths resolve to the same values: this
    // `Deserialize` impl is the strict `Config::from_str` path, which is
    // test-only and has no diagnostics vec by design; real config loading
    // always goes through `salvage`, which is where the diagnostic lives.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let f = IconsFile::deserialize(d)?;
        let b = IconsConfig::raw_preset(&f.preset);
        let mut i = IconsConfig {
            repository: f.repository.unwrap_or(b.repository),
            repository_open: f.repository_open.unwrap_or(b.repository_open),
            repository_empty: f.repository_empty.unwrap_or(b.repository_empty),
            worktree_new: f.worktree_new.unwrap_or(b.worktree_new),
            worktree_ahead: f.worktree_ahead.unwrap_or(b.worktree_ahead),
            worktree_merged: f.worktree_merged.unwrap_or(b.worktree_merged),
            worktree_deleted: f.worktree_deleted.unwrap_or(b.worktree_deleted),
            status_spinner: f
                .status_spinner
                .map(|s| frames(&s))
                .unwrap_or(b.status_spinner),
            status_waiting: f.status_waiting.unwrap_or(b.status_waiting),
            status_blocked: f.status_blocked.unwrap_or(b.status_blocked),
            status_failed: f.status_failed.unwrap_or(b.status_failed),
            status_ended: f.status_ended.unwrap_or(b.status_ended),
            status_idle: f.status_idle.unwrap_or(b.status_idle),
            status_dead: f.status_dead.unwrap_or(b.status_dead),
            shell: f.shell.unwrap_or(b.shell),
            cursor: f.cursor.unwrap_or(b.cursor),
            guide_branch: f.guide_branch.unwrap_or(b.guide_branch),
            guide_last: f.guide_last.unwrap_or(b.guide_last),
            guide_vertical: f.guide_vertical.unwrap_or(b.guide_vertical),
            separator: f.separator.unwrap_or(b.separator),
            warn: f.warn.unwrap_or(b.warn),
            ellipsis: f.ellipsis.unwrap_or(b.ellipsis),
            enter: f.enter.unwrap_or(b.enter),
            arrows: f.arrows.unwrap_or(b.arrows),
            border: f.border.unwrap_or(b.border),
        };
        i.pad();
        Ok(i)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub name: String,
    pub command: String,
    pub shell: Option<String>,
    pub patterns: StatusPatterns,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StatusPatterns {
    pub working: Vec<String>,
    pub waiting: Vec<String>,
    pub failed: Vec<String>,
}

/// One entry of the predefined-message modal (`p`). Only the text: the
/// modal lists it verbatim and pastes it into the agent pane, so there is
/// no second name that could describe it inaccurately.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MessageConfig {
    /// What is listed and sent — exactly as if it had been typed, after
    /// `App::expand_message` resolves the selected shell's tokens.
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RepositoryConfig {
    pub path: String,
    pub base_branch: Option<String>,
    /// Preset name from `[[agents]]`.
    pub agent: Option<String>,
    /// Inline agent command override.
    pub agent_command: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: crate::theme::DEFAULT_THEME.into(),
            poll_interval_ms: 500,
            confirm_on_delete: true,
            preview: PreviewConfig::default(),
            tmux: TmuxConfig {
                back_key: "C-o".into(),
            },
            worktrees: WorktreeConfig {
                root: String::new(),
                base_branch: "master".into(),
                delete_branch: true,
            },
            discovery: DiscoveryConfig {
                scan_root: String::new(),
                max_depth: 4,
                skip_dirs: vec![
                    ".git".into(),
                    "node_modules".into(),
                    "target".into(),
                    ".lazywingman-worktrees".into(),
                ],
            },
            defaults: DefaultsConfig {
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
                agent_hooks: true,
            },
            icons: IconsConfig::default(),
            agents: vec![
                AgentConfig {
                    name: "claude".into(),
                    command: "claude".into(),
                    shell: None,
                    patterns: builtin_patterns("claude").unwrap(),
                },
                AgentConfig {
                    name: "codex".into(),
                    command: "codex".into(),
                    shell: None,
                    patterns: builtin_patterns("codex").unwrap(),
                },
            ],
            repositories: Vec::new(),
            messages: default_messages(),
            diagnostics: Vec::new(),
            sources: Vec::new(),
        }
    }
}

impl Default for TmuxConfig {
    fn default() -> Self {
        TmuxConfig {
            back_key: "C-o".into(),
        }
    }
}

impl Default for WorktreeConfig {
    fn default() -> Self {
        WorktreeConfig {
            root: String::new(),
            base_branch: "master".into(),
            delete_branch: true,
        }
    }
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        DiscoveryConfig {
            scan_root: String::new(),
            max_depth: 4,
            skip_dirs: vec![
                ".git".into(),
                "node_modules".into(),
                "target".into(),
                ".lazywingman-worktrees".into(),
            ],
        }
    }
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        DefaultsConfig {
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
            agent_hooks: true,
        }
    }
}

/// Built-in status patterns for well-known agents, used when the config
/// doesn't define (or overrides) a preset.
pub fn builtin_patterns(name: &str) -> Option<StatusPatterns> {
    let p = match name {
        "claude" => StatusPatterns {
            working: vec![
                "Thinking",
                "Running",
                "Reading",
                "Writing",
                "Searching",
                "Applying",
                "Planning",
                "Creating",
                "Editing",
                "Executing",
                "Progress",
                "⠋",
                "⠙",
                "⠹",
                "⠸",
                "⠼",
                "⠴",
                "⠦",
                "⠧",
                "⠇",
                "⠏",
                "●",
                "▰",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            waiting: vec![
                "What would you like to do",
                "❯",
                "›",
                "(q) to quit",
                "q to quit",
                "> $",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            failed: vec![
                "✖",
                "Error:",
                "error:",
                "exited with code",
                "Fatal",
                "failed",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        },
        "codex" => StatusPatterns {
            working: vec![
                "Working",
                "Reading",
                "Writing",
                "Running",
                "Thinking",
                "Exploring",
                "⠋",
                "⠙",
                "⠹",
                "⠸",
                "⠼",
                "⠴",
                "⠦",
                "⠧",
                "⠇",
                "⠏",
                "▌",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            waiting: vec!["❯", "›", "codex>", "> $"]
                .into_iter()
                .map(String::from)
                .collect(),
            failed: vec!["Error", "error:", "✖"]
                .into_iter()
                .map(String::from)
                .collect(),
        },
        _ => return None,
    };
    Some(p)
}

/// Generic patterns for anything else (plain shell, unknown agents).
pub fn generic_patterns() -> StatusPatterns {
    StatusPatterns {
        working: Vec::new(),
        waiting: vec!["^[^#$]*[$#] ?$".into()],
        failed: vec![
            "command not found",
            "No such file",
            "Permission denied",
            "exited with code",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    }
}

/// The effective agent configuration for a repository.
#[derive(Debug, Clone)]
pub struct EffectiveAgent {
    pub command: String,
    pub shell: String,
    /// Status patterns resolved for this agent (asserted in tests).
    #[allow(dead_code)]
    pub patterns: StatusPatterns,
}

/// Keys that belong to the user, not the repository (configuration rule:
/// \"not every setting is local-able\"). A repository-local file setting one of
/// these gets a diagnostic and the key is ignored — never silently applied.
const USER_ONLY_KEYS: &[(&str, &str)] = &[
    ("tmux", "back_key"),       // a global tmux binding is the user's choice
    ("worktrees", "root"),      // machine-specific path
    ("discovery", "scan_root"), // machine-specific path
];

/// Known top-level keys (unknown keys are reported, never fatal).
const TOP_KEYS: &[&str] = &[
    "theme",
    "poll_interval_ms",
    "confirm_on_delete",
    "preview",
    "tmux",
    "worktrees",
    "discovery",
    "defaults",
    "icons",
    "agents",
    "repositories",
    "messages",
];
const PREVIEW_KEYS: &[&str] = &["colors"];
const TMUX_KEYS: &[&str] = &["back_key"];
const WORKTREES_KEYS: &[&str] = &["root", "base_branch", "delete_branch"];
const DISCOVERY_KEYS: &[&str] = &["scan_root", "max_depth", "skip_dirs"];
const DEFAULTS_KEYS: &[&str] = &["agent_command", "agent_shell", "agent_hooks"];
const ICONS_KEYS: &[&str] = &[
    "preset",
    "repository",
    "repository_open",
    "repository_empty",
    "worktree_new",
    "worktree_ahead",
    "worktree_merged",
    "status_spinner",
    "status_waiting",
    "status_blocked",
    "status_failed",
    "status_ended",
    "status_idle",
    "status_dead",
    "shell",
    "cursor",
    "guide_branch",
    "guide_last",
    "guide_vertical",
    "separator",
    "warn",
    "ellipsis",
    "enter",
    "arrows",
    "border",
];
const AGENT_KEYS: &[&str] = &["name", "command", "shell", "patterns"];
const PATTERN_KEYS: &[&str] = &["working", "waiting", "failed"];
const REPOSITORY_KEYS: &[&str] = &["path", "base_branch", "agent", "agent_command"];
const MESSAGE_KEYS: &[&str] = &["text"];

impl Config {
    /// Load config, writing defaults (and built-in themes) on first run.
    /// Layered per the configuration rules: built-in defaults ← user config
    /// (`$XDG_CONFIG_HOME/lazywingman/config.toml`, or `LW_CONFIG`) ←
    /// repository-local `.lazywingman.toml` (searched upward from the working
    /// directory, stopping at the first hit / repository root) ← its
    /// gitignored sibling `.lazywingman.local.toml`. Higher layers win key
    /// by key (maps merge deeply, lists replace). A malformed file never
    /// prevents startup — the smallest addressable unit is dropped and
    /// reported in `Config.diagnostics`.
    pub fn load() -> Result<Config> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let override_path = std::env::var_os("LW_CONFIG").map(PathBuf::from);
        Self::load_from(&cwd, override_path.as_deref())
    }

    /// [`Self::load`] with an explicit working directory + optional user
    /// config override (`--config <path>`); the cwd is injected so tests
    /// don't have to `chdir`.
    pub fn load_from(cwd: &Path, user_override: Option<&Path>) -> Result<Config> {
        // First-run bootstrap: write the commented default config + built-in
        // themes — only for the default location, never for an override.
        let default_path = crate::util::config_path();
        let user_path = user_override
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_path.clone());
        if user_override.is_none() && !default_path.exists() {
            std::fs::create_dir_all(crate::util::config_dir())?;
            std::fs::write(&default_path, DEFAULT_CONFIG)?;
            log::info!("wrote default config to {}", default_path.display());
            crate::theme::ensure_builtin_themes()?;
            if user_path == default_path {
                // fresh first run: the file we just wrote is the user layer
            }
        }

        let mut diags: Vec<String> = Vec::new();

        // Retired in favour of [icons]: a boolean answered "can this
        // terminal draw box characters?", not "which glyphs suit my font?".
        if std::env::var_os("LW_ASCII").is_some_and(|v| !v.is_empty()) {
            diags.push("LW_ASCII is no longer read; set [icons] glyphs in config.toml".into());
        }

        let mut sources: Vec<(String, &'static str)> = Vec::new();
        let mut merged = toml::Table::new();
        let mut layer_paths: Vec<(&'static str, PathBuf)> = Vec::new();

        // --- user layer -------------------------------------------------
        if user_path.exists() {
            match parse_layer(&user_path) {
                Ok(table) => {
                    report_unknown_keys(&table, &user_path, &mut diags);
                    merge_layer(
                        &mut merged,
                        table,
                        LAYER_USER,
                        &user_path,
                        &mut sources,
                        &mut diags,
                    );
                }
                Err(e) => diags.push(format!(
                    "{}: {e} — using defaults for this file",
                    user_path.display()
                )),
            }
            layer_paths.push((LAYER_USER, user_path.clone()));
        } else if user_override.is_some() {
            diags.push(format!(
                "{}: config file not found — using defaults (remove --config / LW_CONFIG to use the default location)",
                user_path.display()
            ));
        }

        // --- repository layers ---------------------------------------------
        if let Some(dir) = find_repository_dir(cwd) {
            let repo_file = dir.join(".lazywingman.toml");
            if repo_file.exists() {
                match parse_layer(&repo_file) {
                    Ok(table) => {
                        report_unknown_keys(&table, &repo_file, &mut diags);
                        merge_layer(
                            &mut merged,
                            table,
                            LAYER_REPOSITORY,
                            &repo_file,
                            &mut sources,
                            &mut diags,
                        );
                    }
                    Err(e) => diags.push(format!(
                        "{}: {e} — using defaults for this file",
                        repo_file.display()
                    )),
                }
                layer_paths.push((LAYER_REPOSITORY, repo_file.clone()));
            }
            // gitignored sibling: highest of the file layers
            let local = dir.join(".lazywingman.local.toml");
            if local.exists() {
                match parse_layer(&local) {
                    Ok(table) => {
                        report_unknown_keys(&table, &local, &mut diags);
                        merge_layer(
                            &mut merged,
                            table,
                            LAYER_LOCAL,
                            &local,
                            &mut sources,
                            &mut diags,
                        );
                    }
                    Err(e) => diags.push(format!(
                        "{}: {e} — using defaults for this file",
                        local.display()
                    )),
                }
                layer_paths.push((LAYER_LOCAL, local.clone()));
            }
        }

        let mut cfg = deserialize_lenient(&merged, &layer_paths, &sources, &mut diags);
        cfg.diagnostics = diags;
        cfg.sources = sources;
        Ok(cfg)
    }

    /// Load from an explicit string (tests / previews). Strict on purpose —
    /// the lenient layered path is [`Self::load`]/[`Self::load_from`].
    #[allow(dead_code)]
    pub fn from_str(text: &str) -> Result<Config> {
        let cfg: Config =
            toml::from_str(text).map_err(|e| Error::Config(format!("config parse: {e}")))?;
        cfg.validate();
        Ok(cfg)
    }

    /// Drop malformed agent entries (empty name/command) with a log line.
    fn validate(&self) {
        for a in &self.agents {
            if a.name.is_empty() || a.command.is_empty() {
                log::warn!("ignoring agent preset with empty name/command");
            }
        }
        for p in &self.repositories {
            if p.path.is_empty() {
                log::warn!("ignoring repository with empty path");
            }
        }
    }

    /// `--config-dump`: the effective values with the layer each key came
    /// from (\"key = value  # from <layer>\"). Turns any \"why is it doing
    /// that\" into a five-second answer.
    pub fn dump_string(&self) -> String {
        let layer_of = |key: &str| {
            self.sources
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, l)| *l)
                .unwrap_or(LAYER_DEFAULTS)
        };
        let mut out = String::new();
        let mut line = |key: &str, val: String| {
            out.push_str(&format!("{key} = {val}  # from {}\n", layer_of(key)));
        };
        line("theme", format!("\"{}\"", self.theme));
        line("poll_interval_ms", self.poll_interval_ms.to_string());
        line("confirm_on_delete", self.confirm_on_delete.to_string());
        line(
            "preview.colors",
            format!("\"{}\"", self.preview.colors.as_str()),
        );
        line("tmux.back_key", format!("\"{}\"", self.tmux.back_key));
        line("worktrees.root", format!("\"{}\"", self.worktrees.root));
        line(
            "worktrees.base_branch",
            format!("\"{}\"", self.worktrees.base_branch),
        );
        line(
            "worktrees.delete_branch",
            self.worktrees.delete_branch.to_string(),
        );
        line(
            "discovery.scan_root",
            format!("\"{}\"", self.discovery.scan_root),
        );
        line("discovery.max_depth", self.discovery.max_depth.to_string());
        line(
            "discovery.skip_dirs",
            format!(
                "[{}]",
                self.discovery
                    .skip_dirs
                    .iter()
                    .map(|d| format!("\"{d}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        line(
            "defaults.agent_command",
            format!("\"{}\"", self.defaults.agent_command),
        );
        line(
            "defaults.agent_shell",
            format!("\"{}\"", self.defaults.agent_shell),
        );
        line(
            "defaults.agent_hooks",
            self.defaults.agent_hooks.to_string(),
        );
        line(
            "agents",
            format!(
                "[{}]",
                self.agents
                    .iter()
                    .map(|a| format!("\"{}\"", a.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        line(
            "repositories",
            format!(
                "[{}]",
                self.repositories
                    .iter()
                    .map(|p| format!("\"{}\"", p.path))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        line(
            "messages",
            format!(
                "[{}]",
                self.messages
                    .iter()
                    .map(|m| format!("\"{}\"", m.text))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        line("icons.repository", format!("\"{}\"", self.icons.repository));
        line(
            "icons.repository_open",
            format!("\"{}\"", self.icons.repository_open),
        );
        line(
            "icons.repository_empty",
            format!("\"{}\"", self.icons.repository_empty),
        );
        line(
            "icons.worktree_new",
            format!("\"{}\"", self.icons.worktree_new),
        );
        line(
            "icons.worktree_ahead",
            format!("\"{}\"", self.icons.worktree_ahead),
        );
        line(
            "icons.worktree_merged",
            format!("\"{}\"", self.icons.worktree_merged),
        );
        line(
            "icons.status_spinner",
            format!("\"{}\"", self.icons.status_spinner.join("")),
        );
        line(
            "icons.status_waiting",
            format!("\"{}\"", self.icons.status_waiting),
        );
        line(
            "icons.status_blocked",
            format!("\"{}\"", self.icons.status_blocked),
        );
        line(
            "icons.status_failed",
            format!("\"{}\"", self.icons.status_failed),
        );
        line(
            "icons.status_ended",
            format!("\"{}\"", self.icons.status_ended),
        );
        line(
            "icons.status_idle",
            format!("\"{}\"", self.icons.status_idle),
        );
        line(
            "icons.status_dead",
            format!("\"{}\"", self.icons.status_dead),
        );
        line("icons.shell", format!("\"{}\"", self.icons.shell));
        line("icons.cursor", format!("\"{}\"", self.icons.cursor));
        line(
            "icons.guide_branch",
            format!("\"{}\"", self.icons.guide_branch),
        );
        line("icons.guide_last", format!("\"{}\"", self.icons.guide_last));
        line(
            "icons.guide_vertical",
            format!("\"{}\"", self.icons.guide_vertical),
        );
        line("icons.separator", format!("\"{}\"", self.icons.separator));
        line("icons.warn", format!("\"{}\"", self.icons.warn));
        line("icons.ellipsis", format!("\"{}\"", self.icons.ellipsis));
        line(
            "icons.border",
            format!(
                "\"{}\"",
                match self.icons.border {
                    BorderStyle::Rounded => "rounded",
                    BorderStyle::Plain => "plain",
                }
            ),
        );
        out
    }

    /// Repositories with a non-empty path.
    pub fn usable_repositories(&self) -> Vec<&RepositoryConfig> {
        self.repositories
            .iter()
            .filter(|p| !p.path.is_empty())
            .collect()
    }

    /// Effective agent (command, shell, status patterns) for a repository.
    pub fn agent_for(&self, repository: &RepositoryConfig) -> EffectiveAgent {
        // Resolve preset by name first.
        let preset = repository
            .agent
            .as_deref()
            .and_then(|n| self.agents.iter().find(|a| a.name == n));

        let command = repository
            .agent_command
            .clone()
            .or_else(|| preset.map(|a| a.command.clone()))
            .unwrap_or_else(|| self.defaults.agent_command.clone());

        let shell = preset
            .and_then(|a| a.shell.clone())
            .unwrap_or_else(|| self.defaults.agent_shell.clone());

        // An explicit inline command wins for pattern selection too
        // (e.g. `agent = "claude"` + `agent_command = "codex exec"` → codex
        // patterns). Otherwise the named preset's patterns apply, falling
        // back to prefix/builtin matching.
        let patterns = if repository.agent_command.is_some() {
            self.patterns_for(&command)
        } else {
            let named = preset
                .filter(|a| a.patterns.is_nonempty())
                .map(|a| a.patterns.clone());
            named.or_else(|| self.patterns_for(&command))
        }
        .unwrap_or_else(generic_patterns);

        EffectiveAgent {
            command,
            shell,
            patterns,
        }
    }

    /// Status patterns for a raw command string: matches the preset whose
    /// command is a prefix of it (e.g. `codex exec` → codex preset) or its
    /// file name (e.g. `/opt/bin/demo-agent.sh` → `demo-agent.sh` preset),
    /// else built-in defaults by command word, else generic.
    pub fn patterns_for(&self, command: &str) -> Option<StatusPatterns> {
        let file_name = Path::new(command)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        for a in &self.agents {
            if !a.command.is_empty()
                && a.patterns.is_nonempty()
                && (command == a.command
                    || command.starts_with(&format!("{} ", a.command))
                    || command.contains(&format!(" {} ", a.command))
                    || file_name == a.command
                    || file_name.starts_with(&format!("{} ", a.command)))
            {
                return Some(a.patterns.clone());
            }
        }
        let first_word = if file_name.is_empty() {
            command.split_whitespace().next().unwrap_or("")
        } else {
            &file_name
        };
        builtin_patterns(first_word).filter(|p| p.is_nonempty())
    }

    /// Root directory holding a repository's worktrees.
    pub fn worktree_root_for(&self, repository_path: &Path) -> PathBuf {
        let parent = repository_path.parent().unwrap_or(repository_path);
        let root = self.worktrees.root.trim();
        let root = if root.is_empty() {
            parent.join(".lazywingman-worktrees")
        } else {
            let p = PathBuf::from(root);
            if p.is_absolute() { p } else { parent.join(p) }
        };
        crate::util::normalize_path(&root)
    }

    /// Base branch for a repository (per-repository override, else global).
    pub fn base_branch_for(&self, repository: &RepositoryConfig) -> String {
        repository
            .base_branch
            .clone()
            .unwrap_or_else(|| self.worktrees.base_branch.clone())
    }
}

// ---------------------------------------------------------------- layering

/// Parse one config layer file into a TOML table. A whole-file TOML syntax
/// error is the one case we cannot salvage — it is reported by the caller
/// and that file's contribution is dropped (every other unit survives).
fn parse_layer(path: &Path) -> Result<toml::Table> {
    let text = std::fs::read_to_string(path)?;
    let value: toml::Value =
        toml::from_str(&text).map_err(|e| Error::Config(format!("invalid TOML: {e}")))?;
    match value {
        toml::Value::Table(t) => Ok(t),
        other => Err(Error::Config(format!(
            "expected a table, found {}",
            other.type_str()
        ))),
    }
}

/// The nearest repository-local config directory working upward from `cwd`,
/// stopping at the first hit *and* at a repository root (the repository
/// boundary — a repo-root file, if any, is checked first, then the search
/// stops so a sibling repo's config is never inherited).
fn find_repository_dir(cwd: &Path) -> Option<PathBuf> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if d.join(".lazywingman.toml").is_file() || d.join(".lazywingman.local.toml").is_file() {
            return Some(d.to_path_buf());
        }
        // Repository root is the repository boundary: check it (already did),
        // then stop — don't walk above the repo.
        if d.join(".git").exists() {
            return None;
        }
        dir = d.parent();
    }
    None
}

/// Merge `layer` into `merged` key by key: maps merge deeply (per subkey),
/// lists and scalars replace (the higher layer wins). Records the winning
/// layer per dotted key for `--config-dump`. User-only keys
/// (`tmux.back_key`, `worktrees.root`, `discovery.scan_root`) are rejected
/// outside the user layer with a diagnostic — never silently applied.
fn merge_layer(
    merged: &mut toml::Table,
    layer: toml::Table,
    layer_name: &'static str,
    file: &Path,
    sources: &mut Vec<(String, &'static str)>,
    diags: &mut Vec<String>,
) {
    for (key, value) in layer {
        match value {
            toml::Value::Table(over) => {
                // Sections merge subkey by subkey. If a lower layer stored a
                // non-table here, the winning layer's table replaces it.
                let base = merged
                    .entry(key.clone())
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                let Some(base_t) = base.as_table_mut() else {
                    let fresh = toml::Value::Table(over.clone());
                    for (sk, _) in &over {
                        set_source(sources, format!("{key}.{sk}"), layer_name);
                    }
                    *base = fresh;
                    continue;
                };
                for (subkey, subval) in over {
                    let dotted = format!("{key}.{subkey}");
                    if layer_name != LAYER_USER
                        && USER_ONLY_KEYS
                            .iter()
                            .any(|(s, k)| *s == key && *k == subkey)
                    {
                        diags.push(format!(
                            "{}: '{dotted}' is a user-level setting — ignored (set it in your user config)",
                            file.display()
                        ));
                        continue;
                    }
                    base_t.insert(subkey.clone(), subval);
                    set_source(sources, dotted, layer_name);
                }
            }
            // lists and scalars replace wholesale (\"a local file that
            // defines a list replaces that list outright\").
            v => {
                merged.insert(key.clone(), v);
                set_source(sources, key.clone(), layer_name);
            }
        }
    }
}

fn set_source(sources: &mut Vec<(String, &'static str)>, key: String, layer_name: &'static str) {
    if let Some(slot) = sources.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = layer_name; // overridden by a higher layer → newest wins
    } else {
        sources.push((key, layer_name));
    }
}

/// Report unknown keys (top-level and inside every known section) as
/// diagnostics. An old binary runs a newer config: unknown keys are
/// reported, never fatal, per the configuration rules.
fn report_unknown_keys(table: &toml::Table, file: &Path, diags: &mut Vec<String>) {
    for (key, value) in table {
        if !TOP_KEYS.contains(&key.as_str()) {
            diags.push(format!("{}: unknown key '{key}' ignored", file.display()));
        }
        let section_keys = match key.as_str() {
            "preview" => Some(PREVIEW_KEYS),
            "tmux" => Some(TMUX_KEYS),
            "worktrees" => Some(WORKTREES_KEYS),
            "discovery" => Some(DISCOVERY_KEYS),
            "defaults" => Some(DEFAULTS_KEYS),
            "icons" => Some(ICONS_KEYS),
            _ => None,
        };
        if let Some(known) = section_keys
            && let Some(sub) = value.as_table()
        {
            for sk in sub.keys() {
                if !known.contains(&sk.as_str()) {
                    diags.push(format!(
                        "{}: unknown key '{key}.{sk}' ignored",
                        file.display()
                    ));
                }
            }
        }
        if key == "agents"
            && let Some(arr) = value.as_array()
        {
            for entry in arr {
                let Some(t) = entry.as_table() else { continue };
                for ek in t.keys() {
                    if !AGENT_KEYS.contains(&ek.as_str()) {
                        diags.push(format!(
                            "{}: unknown key 'agents.{ek}' ignored",
                            file.display()
                        ));
                    }
                }
                if let Some(patterns) = t.get("patterns").and_then(|p| p.as_table()) {
                    for pk in patterns.keys() {
                        if !PATTERN_KEYS.contains(&pk.as_str()) {
                            diags.push(format!(
                                "{}: unknown key 'agents.patterns.{pk}' ignored",
                                file.display()
                            ));
                        }
                    }
                }
            }
        }
        if key == "repositories"
            && let Some(arr) = value.as_array()
        {
            for entry in arr {
                let Some(t) = entry.as_table() else { continue };
                for pk in t.keys() {
                    if !REPOSITORY_KEYS.contains(&pk.as_str()) {
                        diags.push(format!(
                            "{}: unknown key 'repositories.{pk}' ignored",
                            file.display()
                        ));
                    }
                }
            }
        }
        if key == "messages"
            && let Some(arr) = value.as_array()
        {
            for entry in arr {
                let Some(t) = entry.as_table() else { continue };
                for mk in t.keys() {
                    if !MESSAGE_KEYS.contains(&mk.as_str()) {
                        diags.push(format!(
                            "{}: unknown key 'messages.{mk}' ignored",
                            file.display()
                        ));
                    }
                }
            }
        }
    }
}

/// Smallest-addressable-unit validation: drop malformed scalars, sections,
/// agent/repository entries and status-pattern regexes with a diagnostic, keep
/// everything else — never prevent startup.
fn deserialize_lenient(
    merged: &toml::Table,
    layer_paths: &[(&'static str, PathBuf)],
    sources: &[(String, &'static str)],
    diags: &mut Vec<String>,
) -> Config {
    let mut cfg = Config::default();

    // scalars
    cfg.theme = take_str(merged, "theme", &cfg.theme, layer_paths, sources, diags);
    cfg.poll_interval_ms = take_u64(
        merged,
        "poll_interval_ms",
        cfg.poll_interval_ms,
        layer_paths,
        sources,
        diags,
    );
    cfg.confirm_on_delete = take_bool(
        merged,
        "confirm_on_delete",
        cfg.confirm_on_delete,
        layer_paths,
        sources,
        diags,
    );

    // sections — each field salvaged individually (one bad binding does not
    // drop the whole [tmux] block).
    cfg.preview = take_section(
        merged,
        "preview",
        PreviewConfig::default(),
        layer_paths,
        sources,
        diags,
    );
    cfg.tmux = take_section(
        merged,
        "tmux",
        TmuxConfig::default(),
        layer_paths,
        sources,
        diags,
    );
    cfg.worktrees = take_section(
        merged,
        "worktrees",
        WorktreeConfig::default(),
        layer_paths,
        sources,
        diags,
    );
    cfg.discovery = take_section(
        merged,
        "discovery",
        DiscoveryConfig::default(),
        layer_paths,
        sources,
        diags,
    );
    cfg.defaults = take_section(
        merged,
        "defaults",
        DefaultsConfig::default(),
        layer_paths,
        sources,
        diags,
    );
    cfg.icons = take_section(
        merged,
        "icons",
        IconsConfig::default(),
        layer_paths,
        sources,
        diags,
    );

    // lists — per-entry salvage; a missing key keeps the built-in defaults
    // (a config file without [[agents]] still gets claude/codex presets,
    // exactly like the serde-default path it replaces)
    cfg.agents = take_agent_list(merged, layer_paths, sources, diags, cfg.agents);
    cfg.repositories = take_repository_list(merged, layer_paths, sources, diags);
    cfg.messages = take_message_list(merged, layer_paths, sources, diags, cfg.messages);

    cfg
}

fn take_str(
    merged: &toml::Table,
    key: &str,
    default: &str,
    layer_paths: &[(&'static str, PathBuf)],
    sources: &[(String, &'static str)],
    diags: &mut Vec<String>,
) -> String {
    match merged.get(key) {
        None | Some(toml::Value::String(_)) => {}
        Some(other) => {
            diags.push(format!(
                "{}: '{key}' must be a string (found {}) — using \"{default}\"",
                where_file(layer_paths, sources, key),
                other.type_str()
            ));
        }
    }
    merged
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| default.to_string())
}

fn take_u64(
    merged: &toml::Table,
    key: &str,
    default: u64,
    layer_paths: &[(&'static str, PathBuf)],
    sources: &[(String, &'static str)],
    diags: &mut Vec<String>,
) -> u64 {
    match merged.get(key) {
        None | Some(toml::Value::Integer(_)) => {}
        Some(other) => {
            diags.push(format!(
                "{}: '{key}' must be an integer (found {}) — using {default}",
                where_file(layer_paths, sources, key),
                other.type_str()
            ));
        }
    }
    merged
        .get(key)
        .and_then(|v| v.as_integer())
        .and_then(|i| u64::try_from(i).ok())
        .unwrap_or(default)
}

fn take_bool(
    merged: &toml::Table,
    key: &str,
    default: bool,
    layer_paths: &[(&'static str, PathBuf)],
    sources: &[(String, &'static str)],
    diags: &mut Vec<String>,
) -> bool {
    match merged.get(key) {
        None | Some(toml::Value::Boolean(_)) => {}
        Some(other) => {
            diags.push(format!(
                "{}: '{key}' must be a boolean (found {}) — using {default}",
                where_file(layer_paths, sources, key),
                other.type_str()
            ));
        }
    }
    merged.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// Salvage one section block field-by-field: a section that is present but
/// not a table is dropped wholesale; a single bad field (e.g. one bad
/// binding) defaults on its own while the rest of the section survives —
/// the smallest addressable unit, per the configuration rules.
fn take_section<T>(
    merged: &toml::Table,
    key: &str,
    _default: T,
    layer_paths: &[(&'static str, PathBuf)],
    sources: &[(String, &'static str)],
    diags: &mut Vec<String>,
) -> T
where
    T: SectionSalvage,
{
    let file = where_file(layer_paths, sources, key);
    match merged.get(key) {
        None => T::default(),
        Some(toml::Value::Table(t)) => T::salvage(t, &file, diags),
        Some(other) => {
            diags.push(format!(
                "{}: '[{key}]' must be a table (found {}) — using defaults",
                file,
                other.type_str()
            ));
            T::default()
        }
    }
}

/// Sections implement field-by-field salvage (see [`take_section`]).
trait SectionSalvage: Default {
    fn salvage(table: &toml::Table, file: &str, diags: &mut Vec<String>) -> Self;
}

impl SectionSalvage for PreviewConfig {
    fn salvage(t: &toml::Table, file: &str, diags: &mut Vec<String>) -> Self {
        PreviewConfig {
            colors: salvage_field(t, "colors", PreviewColors::default(), file, diags),
        }
    }
}

impl SectionSalvage for TmuxConfig {
    fn salvage(t: &toml::Table, file: &str, diags: &mut Vec<String>) -> Self {
        TmuxConfig {
            back_key: salvage_field(t, "back_key", "C-o".to_string(), file, diags),
        }
    }
}

impl SectionSalvage for WorktreeConfig {
    fn salvage(t: &toml::Table, file: &str, diags: &mut Vec<String>) -> Self {
        WorktreeConfig {
            root: salvage_field(t, "root", String::new(), file, diags),
            base_branch: salvage_field(t, "base_branch", "master".to_string(), file, diags),
            delete_branch: salvage_field(t, "delete_branch", true, file, diags),
        }
    }
}

impl SectionSalvage for DiscoveryConfig {
    fn salvage(t: &toml::Table, file: &str, diags: &mut Vec<String>) -> Self {
        DiscoveryConfig {
            scan_root: salvage_field(t, "scan_root", String::new(), file, diags),
            max_depth: salvage_field(t, "max_depth", 4usize, file, diags),
            skip_dirs: salvage_str_list(t, "skip_dirs", &default_skip_dirs(), file, diags),
        }
    }
}

impl SectionSalvage for DefaultsConfig {
    fn salvage(t: &toml::Table, file: &str, diags: &mut Vec<String>) -> Self {
        DefaultsConfig {
            agent_command: salvage_field(t, "agent_command", "claude".to_string(), file, diags),
            agent_shell: salvage_field(t, "agent_shell", "bash".to_string(), file, diags),
            agent_hooks: salvage_field(t, "agent_hooks", true, file, diags),
        }
    }
}

impl SectionSalvage for IconsConfig {
    fn salvage(t: &toml::Table, file: &str, diags: &mut Vec<String>) -> Self {
        let preset = salvage_field(t, "preset", "nerdfont".to_string(), file, diags);
        if !ICON_PRESETS.contains(&preset.as_str()) {
            diags.push(format!(
                "{file}: '[icons].preset' unknown value '{preset}' — using 'nerdfont'"
            ));
        }
        let b = IconsConfig::raw_preset(&preset);
        let mut i = IconsConfig {
            repository: salvage_field(t, "repository", b.repository, file, diags),
            repository_open: salvage_field(t, "repository_open", b.repository_open, file, diags),
            repository_empty: salvage_field(t, "repository_empty", b.repository_empty, file, diags),
            worktree_new: salvage_field(t, "worktree_new", b.worktree_new, file, diags),
            worktree_ahead: salvage_field(t, "worktree_ahead", b.worktree_ahead, file, diags),
            worktree_merged: salvage_field(t, "worktree_merged", b.worktree_merged, file, diags),
            worktree_deleted: salvage_field(t, "worktree_deleted", b.worktree_deleted, file, diags),
            // `b` is the unpadded preset, so joining its frames back into
            // one string reproduces exactly what the key accepts.
            status_spinner: frames(&salvage_field(
                t,
                "status_spinner",
                b.status_spinner.join(""),
                file,
                diags,
            )),
            status_waiting: salvage_field(t, "status_waiting", b.status_waiting, file, diags),
            status_blocked: salvage_field(t, "status_blocked", b.status_blocked, file, diags),
            status_failed: salvage_field(t, "status_failed", b.status_failed, file, diags),
            status_ended: salvage_field(t, "status_ended", b.status_ended, file, diags),
            status_idle: salvage_field(t, "status_idle", b.status_idle, file, diags),
            status_dead: salvage_field(t, "status_dead", b.status_dead, file, diags),
            shell: salvage_field(t, "shell", b.shell, file, diags),
            cursor: salvage_field(t, "cursor", b.cursor, file, diags),
            guide_branch: salvage_field(t, "guide_branch", b.guide_branch, file, diags),
            guide_last: salvage_field(t, "guide_last", b.guide_last, file, diags),
            guide_vertical: salvage_field(t, "guide_vertical", b.guide_vertical, file, diags),
            separator: salvage_field(t, "separator", b.separator, file, diags),
            warn: salvage_field(t, "warn", b.warn, file, diags),
            ellipsis: salvage_field(t, "ellipsis", b.ellipsis, file, diags),
            enter: salvage_field(t, "enter", b.enter, file, diags),
            arrows: salvage_field(t, "arrows", b.arrows, file, diags),
            border: salvage_field(t, "border", b.border, file, diags),
        };
        i.pad();
        i
    }
}

/// Extract one section field; a bad value defaults with a diagnostic while
/// the rest of the section (and the rest of the config) is kept.
fn salvage_field<T>(
    t: &toml::Table,
    key: &str,
    default: T,
    file: &str,
    diags: &mut Vec<String>,
) -> T
where
    T: serde::de::DeserializeOwned + Clone,
{
    let Some(v) = t.get(key) else {
        return default;
    };
    match v.clone().try_into::<T>() {
        Ok(x) => x,
        Err(e) => {
            diags.push(format!("{file}: '{key}' invalid ({e}) — using default"));
            default
        }
    }
}

/// The canonical default discovery skip list (mirrors the `Default` impl).
fn default_skip_dirs() -> Vec<String> {
    vec![
        ".git".to_string(),
        "node_modules".to_string(),
        "target".to_string(),
        ".lazywingman-worktrees".to_string(),
    ]
}

/// Per-element salvage for a string list (skip_dirs): one non-string
/// element drops that element, not the whole list.
fn salvage_str_list(
    t: &toml::Table,
    key: &str,
    default: &[String],
    file: &str,
    diags: &mut Vec<String>,
) -> Vec<String> {
    let Some(v) = t.get(key) else {
        return default.to_vec();
    };
    let Some(arr) = v.as_array() else {
        diags.push(format!("{file}: '{key}' must be a list — using defaults"));
        return default.to_vec();
    };
    let mut out = Vec::new();
    for (i, e) in arr.iter().enumerate() {
        match e.as_str() {
            Some(s) => out.push(s.to_string()),
            None => diags.push(format!(
                "{file}: '{key}[{i}]' must be a string — entry dropped"
            )),
        }
    }
    out
}

fn take_agent_list(
    merged: &toml::Table,
    layer_paths: &[(&'static str, PathBuf)],
    sources: &[(String, &'static str)],
    diags: &mut Vec<String>,
    default: Vec<AgentConfig>,
) -> Vec<AgentConfig> {
    let mut agents = take_list(merged, "agents", default, layer_paths, sources, diags);
    let file = where_file(layer_paths, sources, "agents");
    // semantic validation (empty name/command, bad pattern regexes) —
    // smallest-unit drops on top of the structural ones
    agents.retain(|a| {
        let ok = !a.name.is_empty() && !a.command.is_empty();
        if !ok {
            diags.push(format!(
                "{file}: agent preset with empty name/command — entry dropped"
            ));
        }
        ok
    });
    for (i, a) in agents.iter_mut().enumerate() {
        *a = validate_patterns(a.clone(), &file, i, diags);
    }
    agents
}

/// Per-entry salvage of a config list: keep every entry that deserializes
/// and passes semantic validation, drop the rest with a diagnostic. A list
/// that is not a list at all falls back to `default` (never an error).
fn take_list<T>(
    merged: &toml::Table,
    key: &str,
    default: Vec<T>,
    layer_paths: &[(&'static str, PathBuf)],
    sources: &[(String, &'static str)],
    diags: &mut Vec<String>,
) -> Vec<T>
where
    T: serde::de::DeserializeOwned,
{
    let file = where_file(layer_paths, sources, key);
    let Some(arr) = merged.get(key) else {
        return default;
    };
    let Some(arr) = arr.as_array() else {
        diags.push(format!("{file}: '{key}' must be a list — using none"));
        return default;
    };
    let mut out = Vec::new();
    for (i, entry) in arr.iter().enumerate() {
        match entry.clone().try_into::<T>() {
            Ok(v) => out.push(v),
            Err(e) => diags.push(format!("{file}: {key}[{i}] invalid ({e}) — entry dropped")),
        }
    }
    out
}

/// Drop status patterns that don't compile as regexes (validate on load
/// into typed values — the rest of the code never sees a bad pattern).
fn validate_patterns(
    mut a: AgentConfig,
    file: &str,
    i: usize,
    diags: &mut Vec<String>,
) -> AgentConfig {
    for (label, field) in [
        ("working", &mut a.patterns.working),
        ("waiting", &mut a.patterns.waiting),
        ("failed", &mut a.patterns.failed),
    ] {
        field.retain(|p| match regex::Regex::new(p) {
            Ok(_) => true,
            Err(e) => {
                diags.push(format!(
                    "{file}: agents[{i}].patterns.{label} '{p}' is not a valid regex ({e}) — pattern dropped"
                ));
                false
            }
        });
    }
    a
}

fn take_repository_list(
    merged: &toml::Table,
    layer_paths: &[(&'static str, PathBuf)],
    sources: &[(String, &'static str)],
    diags: &mut Vec<String>,
) -> Vec<RepositoryConfig> {
    let mut repositories = take_list::<RepositoryConfig>(
        merged,
        "repositories",
        Vec::new(),
        layer_paths,
        sources,
        diags,
    );
    let file = where_file(layer_paths, sources, "repositories");
    repositories.retain(|p| {
        let ok = !p.path.is_empty();
        if !ok {
            diags.push(format!(
                "{file}: repository with empty path — entry dropped"
            ));
        }
        ok
    });
    repositories
}

fn take_message_list(
    merged: &toml::Table,
    layer_paths: &[(&'static str, PathBuf)],
    sources: &[(String, &'static str)],
    diags: &mut Vec<String>,
    default: Vec<MessageConfig>,
) -> Vec<MessageConfig> {
    let mut messages = take_list(merged, "messages", default, layer_paths, sources, diags);
    let file = where_file(layer_paths, sources, "messages");
    messages.retain(|m| {
        let ok = !m.text.trim().is_empty();
        if !ok {
            diags.push(format!(
                "{file}: predefined message with empty text — entry dropped"
            ));
        }
        ok
    });
    messages
}

/// The built-in predefined messages, used when no config layer defines
/// `[[messages]]` (a list that IS defined replaces these, like every
/// other config list).
pub fn default_messages() -> Vec<MessageConfig> {
    [
        "continue",
        "merge to <base_branch>",
        "summarise what you've changed and what's still left",
    ]
    .into_iter()
    .map(|text| MessageConfig { text: text.into() })
    .collect()
}

/// \"Which file\" for a merged key — the layer that won it, resolved to its
/// path (best effort, defaults to the first layer on unknown keys).
fn where_file(
    layer_paths: &[(&'static str, PathBuf)],
    sources: &[(String, &'static str)],
    key: &str,
) -> String {
    let layer = sources
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, l)| *l)
        .unwrap_or(LAYER_DEFAULTS);
    layer_paths
        .iter()
        .find(|(l, _)| *l == layer)
        .map(|(_, p)| p.display().to_string())
        .unwrap_or_else(|| "config".into())
}

impl StatusPatterns {
    pub fn is_nonempty(&self) -> bool {
        !(self.working.is_empty() && self.waiting.is_empty() && self.failed.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testutil::with_fresh_xdg;

    #[test]
    fn icon_presets_carry_the_documented_glyphs() {
        let nf = IconsConfig::preset("nerdfont");
        assert_eq!(
            nf.worktree_new.trim_end(),
            "\u{eafc}",
            "nerdfont new: nf-cod-git_commit"
        );
        assert_eq!(
            nf.worktree_ahead.trim_end(),
            "\u{f418}",
            "nerdfont ahead: nf-oct-git_branch"
        );
        assert_eq!(
            nf.worktree_merged.trim_end(),
            "\u{e727}",
            "nerdfont merged: nf-dev-git_merge"
        );
        assert_eq!(
            nf.worktree_deleted.trim_end(),
            "\u{f05e8}",
            "nerdfont deleted: nf-md-delete_forever"
        );
        assert_eq!(
            nf.status_waiting.trim_end(),
            "\u{f03e4}",
            "nerdfont waiting: nf-md-pause"
        );
        assert_eq!(
            nf.status_blocked.trim_end(),
            "\u{f04db}",
            "nerdfont blocked: nf-md-stop \u{2014} blocked must not look like waiting"
        );
        assert_eq!(
            nf.repository.trim_end(),
            "\u{f024b}",
            "nerdfont repository: nf-md-folder"
        );
        assert_eq!(
            nf.repository_open.trim_end(),
            "\u{f0770}",
            "nerdfont repository open: nf-md-folder_open"
        );
        assert_eq!(
            nf.repository_empty.trim_end(),
            "\u{f0256}",
            "nerdfont empty repository: nf-md-folder_outline"
        );
        assert_eq!(nf.border, BorderStyle::Rounded);

        let uni = IconsConfig::preset("unicode");
        assert_eq!(uni.repository.trim_end(), "▣");
        assert_eq!(uni.repository_open.trim_end(), "▤");
        assert_eq!(uni.repository_empty.trim_end(), "▢");
        assert_eq!(uni.worktree_new.trim_end(), "○");
        assert_eq!(uni.worktree_ahead.trim_end(), "◆");
        assert_eq!(uni.worktree_merged.trim_end(), "✓");
        assert_eq!(uni.worktree_deleted.trim_end(), "⊘");

        let none = IconsConfig::preset("none");
        assert_eq!(none.repository, "", "none: no type icon, no column at all");
        assert_eq!(none.repository_open, "");
        assert_eq!(none.repository_empty, "");
        assert_eq!(none.worktree_new, "");
        assert_eq!(none.worktree_merged, "");
        assert_eq!(none.worktree_deleted, "");
    }

    #[test]
    fn an_unknown_preset_falls_back_to_nerdfont() {
        assert_eq!(IconsConfig::preset("nope"), IconsConfig::preset("nerdfont"));
    }

    #[test]
    fn column_sharing_keys_are_padded_to_equal_width() {
        use unicode_width::UnicodeWidthStr;
        for name in ICON_PRESETS {
            let i = IconsConfig::preset(name);
            for (group, what) in [
                (
                    vec![&i.repository, &i.repository_open, &i.repository_empty],
                    "repository",
                ),
                (
                    vec![
                        &i.worktree_new,
                        &i.worktree_ahead,
                        &i.worktree_merged,
                        &i.worktree_deleted,
                    ],
                    "worktree",
                ),
                (vec![&i.guide_branch, &i.guide_last], "guide"),
            ] {
                let widths: Vec<usize> = group
                    .iter()
                    .map(|s| UnicodeWidthStr::width(s.as_str()))
                    .collect();
                assert!(
                    widths.iter().all(|w| *w == widths[0]),
                    "{name}: the {what} icons must share a column width, got {widths:?}"
                );
            }
        }
    }

    /// The status icons and every spinner frame share one column: a frame
    /// narrower than the static icons would shift the label sideways once
    /// per animation step, which is exactly the jitter the padding exists
    /// to prevent.
    #[test]
    fn spinner_frames_share_the_status_column_width() {
        use unicode_width::UnicodeWidthStr;
        for name in ICON_PRESETS {
            let i = IconsConfig::preset(name);
            let mut widths: Vec<usize> = [
                &i.status_waiting,
                &i.status_blocked,
                &i.status_failed,
                &i.status_ended,
                &i.status_idle,
                &i.status_dead,
            ]
            .iter()
            .map(|s| UnicodeWidthStr::width(s.as_str()))
            .collect();
            widths.extend(
                i.status_spinner
                    .iter()
                    .map(|f| UnicodeWidthStr::width(f.as_str())),
            );
            assert!(
                widths.iter().all(|w| *w == widths[0]),
                "{name}: status icons and spinner frames must share a width, got {widths:?}"
            );
        }
    }

    /// `idle` and `dead` are blank in every preset, but the column they
    /// share is not: a state the user cannot act on draws nothing while
    /// still lining its label up with the states that do draw.
    #[test]
    fn idle_and_dead_are_blank_but_hold_the_status_column() {
        let nf = IconsConfig::preset("nerdfont");
        assert_eq!(nf.status_idle.trim_end(), "");
        assert_eq!(nf.status_dead.trim_end(), "");
        assert_eq!(
            UnicodeWidthStr::width(nf.status_idle.as_str()),
            UnicodeWidthStr::width(nf.status_waiting.as_str()),
            "a blank status icon still holds its column"
        );
        let none = IconsConfig::preset("none");
        assert!(none.status_spinner.is_empty(), "no spinner without icons");
        assert_eq!(none.status_waiting, "", "the none preset costs no width");
    }

    /// The spinner is configured as one string and split per character, so
    /// a user swaps the whole animation with a single key.
    #[test]
    fn a_spinner_override_is_split_into_frames() {
        let cfg = Config::from_str("[icons]\nstatus_spinner = \"-\\\\|/\"\n").unwrap();
        let frames: Vec<String> = cfg
            .icons
            .status_spinner
            .iter()
            .map(|f| f.trim_end().to_string())
            .collect();
        assert_eq!(frames, vec!["-", "\\", "|", "/"]);
    }

    #[test]
    fn a_non_empty_icon_column_carries_its_own_trailing_space() {
        // Render sites concatenate icon + name directly, so the separating
        // space lives in the padded icon. An empty column adds no space.
        let nf = IconsConfig::preset("nerdfont");
        assert!(
            nf.repository.ends_with(' '),
            "repository: {:?}",
            nf.repository
        );
        assert!(
            nf.worktree_ahead.ends_with(' '),
            "ahead: {:?}",
            nf.worktree_ahead
        );
        assert_eq!(
            IconsConfig::preset("none").repository,
            "",
            "empty column: no space"
        );
    }

    #[test]
    fn icons_preset_and_per_key_override() {
        let cfg =
            Config::from_str("[icons]\npreset = \"unicode\"\nworktree_ahead = \"D\"\n").unwrap();
        // the override wins...
        assert_eq!(cfg.icons.worktree_ahead.trim_end(), "D");
        // ...and the rest of the preset survives
        assert_eq!(cfg.icons.worktree_new.trim_end(), "○");
        assert_eq!(cfg.icons.border, BorderStyle::Rounded);
        // padding is applied once, after the override is layered on
        assert_eq!(
            UnicodeWidthStr::width(cfg.icons.worktree_new.as_str()),
            UnicodeWidthStr::width(cfg.icons.worktree_ahead.as_str())
        );
    }

    #[test]
    fn icons_default_when_absent() {
        let cfg = Config::from_str("theme = \"gruvbox\"\n").unwrap();
        assert_eq!(cfg.icons, IconsConfig::preset("nerdfont"));
    }

    /// `ICONS_KEYS` is the one place a new icon key must be repeated that the
    /// compiler does NOT check — the struct literals in `raw_preset`,
    /// `Deserialize` and `salvage` are exhaustive, so they break loudly. Left
    /// to drift, a legitimate new key gets reported as unknown. Every key is
    /// round-tripped through the real parser rather than counted, so a typo
    /// fails too.
    #[test]
    fn every_icons_key_is_registered_for_unknown_key_reporting() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let body: String = ICONS_KEYS
                .iter()
                .map(|k| {
                    if *k == "border" {
                        format!("{k} = \"plain\"\n")
                    } else if *k == "preset" {
                        format!("{k} = \"unicode\"\n")
                    } else {
                        format!("{k} = \"x\"\n")
                    }
                })
                .collect();
            let cfg = load_user(cwd.path(), &format!("[icons]\n{body}"));
            let unknown: Vec<_> = cfg
                .diagnostics
                .iter()
                .filter(|d| d.contains("unknown key"))
                .collect();
            assert!(
                unknown.is_empty(),
                "ICONS_KEYS names a key the parser rejects: {unknown:?}"
            );
            // ...and the inverse: a key the struct has but ICONS_KEYS forgot
            // would be reported as unknown, so assert the count matches the
            // number of fields actually accepted.
            assert_eq!(
                ICONS_KEYS.len(),
                25,
                "adding an icon key means adding it to ICONS_KEYS too"
            );
        });
    }

    #[test]
    fn an_unknown_icons_preset_is_reported_and_defaults() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(cwd.path(), "[icons]\npreset = \"emoji\"\n");
            assert_eq!(cfg.icons, IconsConfig::preset("nerdfont"));
            assert!(
                cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("'[icons].preset'")),
                "diagnostics: {:?}",
                cfg.diagnostics
            );
        });
    }

    #[test]
    fn an_unknown_icons_key_is_reported_and_ignored() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(cwd.path(), "[icons]\nsparkle = \"x\"\n");
            assert!(
                cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("'icons.sparkle'")),
                "diagnostics: {:?}",
                cfg.diagnostics
            );
        });
    }

    #[test]
    fn first_run_writes_defaults() {
        with_fresh_xdg(|| {
            let cfg = Config::load().unwrap();
            assert_eq!(cfg.theme, crate::theme::DEFAULT_THEME);
            assert_eq!(cfg.poll_interval_ms, 500);
            assert!(cfg.confirm_on_delete);
            assert_eq!(cfg.tmux.back_key, "C-o");
            assert_eq!(cfg.defaults.agent_command, "claude");
            assert_eq!(cfg.worktrees.base_branch, "master");
            assert!(
                cfg.worktrees.delete_branch,
                "the worktree branch goes with the worktree by default"
            );
            assert!(crate::util::config_path().exists());
            // themes copied too
            assert!(crate::util::themes_dir().join("dracula.toml").exists());
            assert!(
                crate::util::themes_dir()
                    .join("catppuccin-mocha.toml")
                    .exists(),
                "catppuccin set must be copied on first run"
            );
            // written file parses back identically
            let again = Config::load().unwrap();
            assert_eq!(again.theme, crate::theme::DEFAULT_THEME);
        });
    }

    /// The shipped presets, in the order the modal numbers them.
    fn default_texts() -> Vec<&'static str> {
        vec![
            "continue",
            "merge to <base_branch>",
            "summarise what you've changed and what's still left",
        ]
    }

    /// The built-ins are the three moves that carry agent supervision:
    /// unblock, integrate, take stock. Each keeps a `1`-`9` shortcut and
    /// the merge one is a template resolved against the selected shell.
    #[test]
    fn built_in_messages_are_the_three_agent_staples() {
        let built_in = default_messages();
        let texts: Vec<&str> = built_in.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, default_texts());
    }

    #[test]
    fn first_run_config_documents_predefined_messages() {
        with_fresh_xdg(|| {
            Config::load().unwrap();
            let text = std::fs::read_to_string(crate::util::config_path()).unwrap();
            assert!(
                text.contains("[[messages]]"),
                "the written config must show how to configure predefined messages"
            );
            // and the file that was written parses back to the same list
            let cfg = Config::load().unwrap();
            let texts: Vec<&str> = cfg.messages.iter().map(|m| m.text.as_str()).collect();
            assert_eq!(texts, default_texts());
        });
    }

    #[test]
    fn user_overrides_are_honoured() {
        with_fresh_xdg(|| {
            Config::load().unwrap();
            let mut text = std::fs::read_to_string(crate::util::config_path()).unwrap();
            // The written file ships every key commented out, so "editing
            // it" means uncommenting as well as changing the value.
            text = text.replace("#theme = \"catppuccin-mocha\"", "theme = \"ansi\"");
            text = text.replace("#poll_interval_ms = 500", "poll_interval_ms = 250");
            std::fs::write(crate::util::config_path(), text).unwrap();
            let cfg = Config::load().unwrap();
            assert_eq!(cfg.theme, "ansi");
            assert_eq!(cfg.poll_interval_ms, 250);
        });
    }

    #[test]
    fn invalid_config_is_hard_error() {
        assert!(Config::from_str("this is not toml {{{").is_err());
        assert!(Config::from_str("theme = 42").is_err());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let cfg =
            Config::from_str("theme = \"gruvbox\"\nunknown_key = 1\n[nonexistent_section]\nx = 2")
                .unwrap();
        assert_eq!(cfg.theme, "gruvbox");
    }

    #[test]
    fn repositories_and_overrides_parse() {
        let cfg = Config::from_str(
            r#"
            [[repositories]]
            path = "/tmp/repo"
            base_branch = "develop"
            agent = "codex"
            "#,
        )
        .unwrap();
        let p = &cfg.repositories[0];
        assert_eq!(p.path, "/tmp/repo");
        assert_eq!(cfg.base_branch_for(p), "develop");
        // agent resolution: codex preset
        let a = cfg.agent_for(p);
        assert_eq!(a.command, "codex");
        assert!(a.patterns.waiting.iter().any(|w| w.contains("codex>")));
    }

    #[test]
    fn inline_agent_command_overrides_preset() {
        let cfg = Config::from_str(
            r#"
            [[repositories]]
            path = "/tmp/repo"
            agent = "claude"
            agent_command = "codex exec --yolo"
            "#,
        )
        .unwrap();
        let a = cfg.agent_for(&cfg.repositories[0]);
        assert_eq!(a.command, "codex exec --yolo");
        // patterns picked from codex preset via prefix match
        assert!(a.patterns.waiting.iter().any(|w| w.contains("codex>")));
    }

    #[test]
    fn unknown_preset_falls_back_to_global_default() {
        let cfg = Config::from_str(
            r#"
            [[repositories]]
            path = "/tmp/repo"
            agent = "no-such-agent"
            "#,
        )
        .unwrap();
        let a = cfg.agent_for(&cfg.repositories[0]);
        assert_eq!(a.command, "claude");
        assert!(a.patterns.waiting.iter().any(|w| w.contains("❯")));
    }

    #[test]
    fn worktree_root_resolution() {
        let repository = Path::new("/work/repositories/my-repo");
        // default: sibling of the repo
        let cfg = Config::default();
        assert_eq!(
            cfg.worktree_root_for(repository),
            PathBuf::from("/work/repositories/.lazywingman-worktrees")
        );
        // absolute override
        let cfg = Config::from_str("[worktrees]\nroot = \"/wt\"\n").unwrap();
        assert_eq!(cfg.worktree_root_for(repository), PathBuf::from("/wt"));
        // relative override → resolved against repository parent
        let cfg = Config::from_str("[worktrees]\nroot = \"../wt\"\n").unwrap();
        assert_eq!(cfg.worktree_root_for(repository), PathBuf::from("/work/wt"));
    }

    #[test]
    fn base_branch_falls_back_to_global() {
        let cfg = Config::from_str("[[repositories]]\npath = \"/x\"\n").unwrap();
        assert_eq!(cfg.base_branch_for(&cfg.repositories[0]), "master");
    }

    #[test]
    fn malformed_agents_are_skipped_not_fatal() {
        let cfg = Config::from_str("[[agents]]\nname = \"\"\ncommand = \"\"\n").unwrap();
        let a = cfg.agent_for(&RepositoryConfig::default());
        assert_eq!(a.command, "claude");
    }

    #[test]
    fn discovery_defaults_apply() {
        let cfg = Config::default();
        assert_eq!(cfg.discovery.scan_root, "");
        assert_eq!(cfg.discovery.max_depth, 4);
        assert!(cfg.discovery.skip_dirs.iter().any(|d| d == "node_modules"));
        // a config without [discovery] parses with defaults
        let cfg = Config::from_str("theme = \"ansi\"\n").unwrap();
        assert_eq!(cfg.discovery.max_depth, 4);
        assert_eq!(cfg.discovery.scan_root, "");
    }

    #[test]
    fn discovery_overrides_parse() {
        let cfg = Config::from_str(
            r#"
            [discovery]
            scan_root = "/work"
            max_depth = 2
            skip_dirs = ["vendor", "build"]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.discovery.scan_root, "/work");
        assert_eq!(cfg.discovery.max_depth, 2);
        assert_eq!(cfg.discovery.skip_dirs, vec!["vendor", "build"]);
    }

    // ------------------------------------------------ M8.4d: lenient load

    /// Write `text` as the user config file and load it from `cwd`.
    fn load_user(cwd: &Path, text: &str) -> Config {
        let path = crate::util::config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        Config::load_from(cwd, None).unwrap()
    }

    fn load_user_from(cwd: &Path, text: &str, override_path: Option<&Path>) -> Config {
        let path = override_path
            .map(Path::to_path_buf)
            .unwrap_or_else(crate::util::config_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        Config::load_from(cwd, override_path).unwrap()
    }

    #[test]
    fn agent_hooks_defaults_to_on() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(cwd.path(), "");
            assert!(
                cfg.defaults.agent_hooks,
                "instrumentation must be on unless the user turns it off"
            );
        });
    }

    #[test]
    fn agent_hooks_can_be_turned_off() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(cwd.path(), "[defaults]\nagent_hooks = false\n");
            assert!(!cfg.defaults.agent_hooks);
        });
    }

    #[test]
    fn agent_hooks_is_a_recognised_defaults_key() {
        assert!(DEFAULTS_KEYS.contains(&"agent_hooks"));
    }

    #[test]
    fn malformed_config_never_prevents_startup() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            // whole file is not even TOML → the file's contribution drops,
            // defaults survive, diagnostics report it
            let cfg = load_user(cwd.path(), "this is not toml {{{");
            assert_eq!(cfg.theme, crate::theme::DEFAULT_THEME);
            assert_eq!(cfg.poll_interval_ms, 500);
            assert!(cfg.diagnostics.iter().any(|d| d.contains("invalid TOML")));
        });
    }

    #[test]
    fn bad_scalar_is_dropped_not_fatal() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(
                cwd.path(),
                "theme = 42\npoll_interval_ms = \"fast\"\nconfirm_on_delete = 1\n",
            );
            // defaults used instead, with diagnostics saying so
            assert_eq!(cfg.theme, crate::theme::DEFAULT_THEME);
            assert_eq!(cfg.poll_interval_ms, 500);
            assert!(cfg.confirm_on_delete);
            assert_eq!(cfg.diagnostics.len(), 3);
            assert!(cfg.diagnostics.iter().any(|d| d.contains("'theme'")));
            assert!(
                cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("'poll_interval_ms'"))
            );
            assert!(
                cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("'confirm_on_delete'"))
            );
        });
    }

    #[test]
    fn bad_agent_entry_is_dropped_others_kept() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(
                cwd.path(),
                r#"
                [[agents]]
                name = "good"
                command = "ok"
                [[agents]]
                name = 7
                command = "bad"
                [[agents]]
                name = "also-good"
                command = "ok2"
                "#,
            );
            assert_eq!(cfg.agents.len(), 2);
            assert!(cfg.agents.iter().any(|a| a.name == "good"));
            assert!(cfg.agents.iter().any(|a| a.name == "also-good"));
            assert!(cfg.diagnostics.iter().any(|d| d.contains("agents[1]")));
        });
    }

    #[test]
    fn preview_colors_default_to_full_and_parse_every_mode() {
        assert_eq!(Config::default().preview.colors, PreviewColors::Full);
        let mode = |text: &str| Config::from_str(text).unwrap().preview.colors;
        assert_eq!(mode("[preview]\ncolors = \"full\"\n"), PreviewColors::Full);
        assert_eq!(
            mode("[preview]\ncolors = \"foreground\"\n"),
            PreviewColors::Foreground
        );
        assert_eq!(mode("[preview]\ncolors = \"none\"\n"), PreviewColors::None);
        // forgiving spellings for the same two modes
        assert_eq!(
            mode("[preview]\ncolors = \"no-background\"\n"),
            PreviewColors::Foreground
        );
        assert_eq!(mode("[preview]\ncolors = \"text\"\n"), PreviewColors::None);
    }

    /// Strip the leading `#` from the config lines of `DEFAULT_CONFIG`.
    ///
    /// The written file uses two comment styles: `#key = value` (hash, no
    /// space) is a real default — uncommenting it changes nothing — while
    /// `# prose` (hash + space) is explanation or an example that has no
    /// default (`[[repositories]]`). This keeps only the former.
    fn uncomment_defaults(text: &str) -> String {
        text.lines()
            .map(|line| match line.strip_prefix('#') {
                Some(rest) if !rest.starts_with(' ') && !rest.is_empty() => rest,
                Some(_) => "",
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Does `DEFAULT_CONFIG` document `key` — as `key = …`, `[key]`,
    /// `[[key]]` or a dotted sub-table ending in `.key]`?
    fn documents_key(key: &str) -> bool {
        DEFAULT_CONFIG.lines().any(|line| {
            let l = line.trim_start_matches('#').trim_start();
            l.starts_with(&format!("{key} ="))
                || l.starts_with(&format!("[{key}]"))
                || l.starts_with(&format!("[[{key}]]"))
                || (l.starts_with('[') && l.contains(&format!(".{key}]")))
        })
    }

    /// The written file is exhaustive: every key the loader recognises is
    /// in it, so the config is discoverable without reading the source.
    #[test]
    fn default_config_documents_every_recognised_key() {
        let registries: &[(&str, &[&str])] = &[
            ("top-level", TOP_KEYS),
            ("[preview]", PREVIEW_KEYS),
            ("[tmux]", TMUX_KEYS),
            ("[worktrees]", WORKTREES_KEYS),
            ("[discovery]", DISCOVERY_KEYS),
            ("[defaults]", DEFAULTS_KEYS),
            ("[icons]", ICONS_KEYS),
            ("[[agents]]", AGENT_KEYS),
            ("[agents.patterns]", PATTERN_KEYS),
            ("[[repositories]]", REPOSITORY_KEYS),
            ("[[messages]]", MESSAGE_KEYS),
        ];
        let missing: Vec<String> = registries
            .iter()
            .flat_map(|(section, keys)| {
                keys.iter()
                    .filter(|k| !documents_key(k))
                    .map(move |k| format!("{section} {k}"))
            })
            .collect();
        assert!(
            missing.is_empty(),
            "DEFAULT_CONFIG must document every recognised key; missing: {missing:?}"
        );
    }

    /// The documented values really are the defaults: uncommenting the
    /// whole file changes nothing. Guards against a default drifting in
    /// `Default for Config` without the written file following.
    #[test]
    fn uncommenting_default_config_reproduces_the_defaults() {
        let cfg = Config::from_str(&uncomment_defaults(DEFAULT_CONFIG)).unwrap();
        assert_eq!(format!("{cfg:?}"), format!("{:?}", Config::default()));
    }

    /// As shipped the file sets nothing — every value comes from the
    /// built-in defaults, so a first run behaves like no config at all.
    #[test]
    fn default_config_is_inert_as_written() {
        let cfg = Config::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(format!("{cfg:?}"), format!("{:?}", Config::default()));
    }

    /// The shipped default config parses and keeps the documented default.
    #[test]
    fn default_config_template_declares_full_preview_colors() {
        let cfg = Config::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(cfg.preview.colors, PreviewColors::Full);
    }

    #[test]
    fn unknown_preview_mode_is_dropped_not_fatal() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(cwd.path(), "[preview]\ncolors = \"rainbow\"\n");
            assert_eq!(cfg.preview.colors, PreviewColors::Full);
            assert!(cfg.diagnostics.iter().any(|d| d.contains("'colors'")));
        });
    }

    #[test]
    fn bad_section_is_dropped_per_field() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(
                cwd.path(),
                "[tmux]\nback_key = 12\n\n[worktrees]\nbase_branch = 5\nroot = \"/wt\"\n",
            );
            // one bad binding → that field defaults, the section survives
            assert_eq!(cfg.tmux.back_key, "C-o");
            assert_eq!(cfg.worktrees.root, "/wt");
            assert_eq!(cfg.worktrees.base_branch, "master");
            // per-field salvage: the bad fields are reported, the good one
            // (root) is NOT (it never fails)
            assert!(cfg.diagnostics.iter().any(|d| d.contains("'back_key'")));
            assert!(cfg.diagnostics.iter().any(|d| d.contains("'base_branch'")));
            assert!(!cfg.diagnostics.iter().any(|d| d.contains("'root'")));
        });
    }

    #[test]
    fn unknown_keys_reported_not_fatal() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(
                cwd.path(),
                "frobnicate = true\n[tmux]\nnot_a_key = 1\n[[agents]]\nname=\"a\"\ncommand=\"b\"\nwrong = 3\n",
            );
            assert_eq!(cfg.tmux.back_key, "C-o");
            assert_eq!(cfg.agents.len(), 1);
            assert!(cfg.diagnostics.iter().any(|d| d.contains("'frobnicate'")));
            assert!(
                cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("'tmux.not_a_key'"))
            );
            assert!(cfg.diagnostics.iter().any(|d| d.contains("'agents.wrong'")));
        });
    }

    #[test]
    fn bad_pattern_regex_dropped_at_load() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(
                cwd.path(),
                "[[agents]]\nname=\"a\"\ncommand=\"b\"\n[agents.patterns]\nworking = [\"ok\", \"[\"]\n",
            );
            assert_eq!(cfg.agents.len(), 1);
            assert_eq!(cfg.agents[0].patterns.working, vec!["ok".to_string()]);
            assert!(
                cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("not a valid regex"))
            );
        });
    }

    // ---------------------------------------------- M8.4d: repository layering

    #[test]
    fn repository_config_layers_over_user() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let cwd = root.path().join("sub").join("deep");
            std::fs::create_dir_all(&cwd).unwrap();
            // user config
            load_user(
                &cwd,
                "theme = \"dracula\"\npoll_interval_ms = 250\n[tmux]\nback_key = \"C-x\"\n",
            );
            // repository-local config in an ancestor: overrides ONE key
            std::fs::write(
                root.path().join("sub/.lazywingman.toml"),
                "theme = \"ansi\"\n",
            )
            .unwrap();
            let cfg = Config::load_from(&cwd, None).unwrap();
            assert_eq!(cfg.theme, "ansi"); // repository wins for theme
            assert_eq!(cfg.poll_interval_ms, 250); // user keeps poll
            assert_eq!(cfg.tmux.back_key, "C-x"); // user keeps tmux
            // provenance: theme came from the repository layer
            assert!(
                cfg.sources
                    .iter()
                    .any(|(k, l)| k == "theme" && *l == LAYER_REPOSITORY)
            );
            assert!(
                cfg.sources
                    .iter()
                    .any(|(k, l)| k == "poll_interval_ms" && *l == LAYER_USER)
            );
        });
    }

    #[test]
    fn repository_config_found_upward_stops_at_first_hit_and_repo_root() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let repo = root.path().join("repo");
            let deep = repo.join("a").join("b");
            std::fs::create_dir_all(&deep).unwrap();
            // repo root is a git repo → search stops there; a config above
            // the repo root must NOT be picked up
            std::fs::create_dir_all(repo.join(".git")).unwrap();
            std::fs::write(root.path().join(".lazywingman.toml"), "theme = \"ansi\"\n").unwrap();
            let cfg = Config::load_from(&deep, None).unwrap();
            assert_eq!(cfg.theme, crate::theme::DEFAULT_THEME);
            // first hit wins: put one in a/ and one in repo root
            std::fs::write(repo.join(".lazywingman.toml"), "theme = \"gruvbox\"\n").unwrap();
            std::fs::write(repo.join("a/.lazywingman.toml"), "theme = \"dracula\"\n").unwrap();
            let cfg = Config::load_from(&deep, None).unwrap();
            assert_eq!(cfg.theme, "dracula"); // nearest ancestor wins
        });
    }

    #[test]
    fn local_sibling_config_wins_over_repository_config() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            std::fs::write(
                root.path().join(".lazywingman.toml"),
                "theme = \"gruvbox\"\n",
            )
            .unwrap();
            std::fs::write(
                root.path().join(".lazywingman.local.toml"),
                "theme = \"dracula\"\n",
            )
            .unwrap();
            let cfg = Config::load_from(root.path(), None).unwrap();
            assert_eq!(cfg.theme, "dracula");
            assert!(
                cfg.sources
                    .iter()
                    .any(|(k, l)| k == "theme" && *l == LAYER_LOCAL)
            );
        });
    }

    #[test]
    fn lists_replace_on_merge_not_append() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            load_user(
                root.path(),
                "[[agents]]\nname=\"user-agent\"\ncommand=\"u\"\n",
            );
            std::fs::write(
                root.path().join(".lazywingman.toml"),
                "[[agents]]\nname=\"repository-agent\"\ncommand=\"p\"\n",
            )
            .unwrap();
            let cfg = Config::load_from(root.path(), None).unwrap();
            // the repository list REPLACES the user list outright
            assert_eq!(cfg.agents.len(), 1);
            assert_eq!(cfg.agents[0].name, "repository-agent");
            assert!(
                cfg.sources
                    .iter()
                    .any(|(k, l)| k == "agents" && *l == LAYER_REPOSITORY)
            );
        });
    }

    #[test]
    fn user_only_keys_rejected_in_repository_files() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            load_user(root.path(), "[tmux]\nback_key = \"C-x\"\n");
            std::fs::write(
                root.path().join(".lazywingman.toml"),
                "[tmux]\nback_key = \"C-b\"\n[worktrees]\nroot = \"/machine/path\"\n",
            )
            .unwrap();
            let cfg = Config::load_from(root.path(), None).unwrap();
            // user's binding wins; the repository file's attempt is reported
            assert_eq!(cfg.tmux.back_key, "C-x");
            assert_eq!(cfg.worktrees.root, "");
            assert!(
                cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("'tmux.back_key'"))
            );
            assert!(
                cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("'worktrees.root'"))
            );
        });
    }

    #[test]
    fn config_override_flag_and_env_take_precedence() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            // default XDG config exists with one theme
            load_user(root.path(), "theme = \"dracula\"\n");
            // --config points elsewhere
            let override_path = root.path().join("custom.toml");
            let cfg = load_user_from(root.path(), "theme = \"ansi\"\n", Some(&override_path));
            assert_eq!(cfg.theme, "ansi");
            // sources reflect the override file
            assert!(cfg.diagnostics.is_empty());
        });
    }

    #[test]
    fn missing_override_is_reported_not_fatal() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let cfg = Config::load_from(root.path(), Some(&root.path().join("nope.toml"))).unwrap();
            assert_eq!(cfg.theme, crate::theme::DEFAULT_THEME);
            assert!(cfg.diagnostics.iter().any(|d| d.contains("not found")));
        });
    }

    /// `LW_CONFIG` env var points the USER layer at another file (same
    /// override as `--config`, read by the no-arg `load()`).
    #[test]
    fn lw_config_env_overrides_user_layer() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            // the default XDG config exists with one theme
            load_user(root.path(), "theme = \"dracula\"\n");
            // LW_CONFIG points elsewhere
            let custom = root.path().join("custom.toml");
            std::fs::write(&custom, "theme = \"ansi\"\n").unwrap();
            unsafe { std::env::set_var("LW_CONFIG", &custom) };
            let cfg = Config::load().unwrap();
            unsafe { std::env::remove_var("LW_CONFIG") };
            assert_eq!(cfg.theme, "ansi", "LW_CONFIG must win over the XDG file");
        });
    }

    #[test]
    fn lw_ascii_is_reported_as_retired() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            unsafe { std::env::set_var("LW_ASCII", "1") };
            let cfg = load_user(cwd.path(), "");
            unsafe { std::env::remove_var("LW_ASCII") };
            assert!(
                cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("LW_ASCII") && d.contains("[icons]")),
                "diagnostics: {:?}",
                cfg.diagnostics
            );
        });
    }

    #[test]
    fn config_dump_lists_effective_values_with_layer_provenance() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            load_user(
                root.path(),
                "theme = \"dracula\"\npoll_interval_ms = 250\n[icons]\nworktree_ahead = \"D\"\n",
            );
            std::fs::write(
                root.path().join(".lazywingman.toml"),
                "[worktrees]\nbase_branch = \"develop\"\n",
            )
            .unwrap();
            let cfg = Config::load_from(root.path(), None).unwrap();
            let dump = cfg.dump_string();
            assert!(dump.contains("theme = \"dracula\"  # from user"));
            assert!(dump.contains("poll_interval_ms = 250  # from user"));
            assert!(dump.contains("preview.colors = \"full\"  # from defaults"));
            assert!(dump.contains("worktrees.base_branch = \"develop\"  # from repository"));
            assert!(dump.contains("tmux.back_key = \"C-o\"  # from defaults"));
            // [icons] must appear too: a per-key override shows its own
            // layer, and an untouched key still shows the resolved default.
            assert!(
                dump.contains("icons.worktree_ahead = \"D \"  # from user"),
                "dump: {dump}"
            );
            assert!(
                dump.contains("icons.border = \"rounded\"  # from defaults"),
                "dump: {dump}"
            );
        });
    }

    #[test]
    fn agents_defaults_kept_when_list_absent() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(cwd.path(), "theme = \"ansi\"\n");
            // no [[agents]] in the file → built-in presets remain
            assert!(cfg.agents.iter().any(|a| a.name == "claude"));
            assert!(cfg.agents.iter().any(|a| a.name == "codex"));
        });
    }

    #[test]
    fn message_defaults_kept_when_list_absent() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(cwd.path(), "theme = \"ansi\"\n");
            // no [[messages]] in the file → built-in predefined messages remain
            let texts: Vec<&str> = cfg.messages.iter().map(|m| m.text.as_str()).collect();
            assert_eq!(texts, default_texts());
        });
    }

    #[test]
    fn configured_messages_replace_the_defaults() {
        let cfg = Config::from_str(
            r#"
            [[messages]]
            text = "run the gate and push"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.messages.len(), 1);
        assert_eq!(cfg.messages[0].text, "run the gate and push");
    }

    /// `label` was retired when the modal started listing the message
    /// itself. A config still carrying one is reported like any other
    /// stray key rather than silently ignored.
    #[test]
    fn retired_label_key_is_reported() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(
                cwd.path(),
                "[[messages]]\nlabel = \"go\"\ntext = \"continue\"\n",
            );
            assert_eq!(cfg.messages.len(), 1, "the entry itself is kept");
            assert_eq!(cfg.messages[0].text, "continue");
            assert!(
                cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("unknown key 'messages.label'")),
                "the retired key must be reported: {:?}",
                cfg.diagnostics
            );
        });
    }

    #[test]
    fn message_with_empty_text_is_dropped_others_kept() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(
                cwd.path(),
                r#"
                [[messages]]
                text = ""

                [[messages]]
                text = "continue"
                "#,
            );
            let texts: Vec<&str> = cfg.messages.iter().map(|m| m.text.as_str()).collect();
            assert_eq!(texts, vec!["continue"]);
            assert!(
                cfg.diagnostics.iter().any(|d| d.contains("empty text")),
                "dropped message must be reported: {:?}",
                cfg.diagnostics
            );
        });
    }

    #[test]
    fn messages_section_is_known_but_its_stray_keys_are_reported() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(
                cwd.path(),
                "[[messages]]\ntext = \"continue\"\ncolour = \"red\"\n",
            );
            assert_eq!(cfg.messages.len(), 1, "the entry itself is kept");
            assert!(
                !cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("unknown key 'messages'")),
                "the section is known: {:?}",
                cfg.diagnostics
            );
            assert!(
                cfg.diagnostics
                    .iter()
                    .any(|d| d.contains("unknown key 'messages.colour'")),
                "stray entry key must be reported: {:?}",
                cfg.diagnostics
            );
        });
    }

    #[test]
    fn config_dump_lists_message_texts() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(cwd.path(), "theme = \"ansi\"\n");
            assert!(
                cfg.dump_string().contains(&format!(
                    "messages = [{}]  # from defaults",
                    default_texts()
                        .iter()
                        .map(|t| format!("\"{t}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                "dump: {}",
                cfg.dump_string()
            );
        });
    }
}
