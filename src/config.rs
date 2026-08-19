//! Configuration loading, defaults, and first-run bootstrap.
//!
//! Config lives at `$XDG_CONFIG_HOME/lazywingman/config.toml`, layered with
//! a project-local `.lazywingman.toml` (searched upward from the working
//! directory, stopping at the first hit / repository root) and its ignored
//! sibling `.lazywingman.local.toml`. On first run a fully commented default
//! config is written (and built-in themes are copied into `themes/`).
//!
//! Loading is lenient per the configuration guidelines (drop the smallest
//! addressable unit, never prevent startup): one bad agent/project entry,
//! one bad binding, one bad scalar — each is dropped with a diagnostic in
//! `Config.diagnostics` while everything else is kept. Unknown keys are
//! reported, not fatal. `from_str` stays strict for tests.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// Layer labels recorded in `Config.sources` for `--config-dump`
/// ("key = value  # from <layer>").
pub const LAYER_DEFAULTS: &str = "defaults";
pub const LAYER_USER: &str = "user";
pub const LAYER_PROJECT: &str = "project";
pub const LAYER_LOCAL: &str = "local";

/// Fully commented default config, written on first run.
pub const DEFAULT_CONFIG: &str = r#"# LazyWingman configuration — written on first run, edit freely.
# Location: $XDG_CONFIG_HOME/lazywingman/config.toml (default ~/.config/lazywingman)
#
# This file is the USER layer. A project can also carry a committed
# .lazywingman.toml (searched upward from the working directory, stopping at
# the repository root) and a gitignored .lazywingman.local.toml next to it;
# those layers override keys here. Some keys are user-only and are ignored
# in project files with a diagnostic: tmux.back_key, worktrees.root,
# discovery.scan_root. Inspect the effective config with `lazywingman
# --config-dump`; use --config <path> or LW_CONFIG to point at a different
# user file. A malformed config never prevents startup — the smallest bad
# unit is dropped and reported once in a startup diagnostics overlay.

# Theme to use. Built-ins: "catppuccin-mocha", "catppuccin-macchiato",
# "catppuccin-frappe", "catppuccin-latte", "dracula", "gruvbox", "ansi".
# Drop your own theme into ~/.config/lazywingman/themes/<name>.toml
# Press ctrl-t inside the TUI to switch themes live (picked themes are
# saved to the state file; this line is the startup default).
theme = "catppuccin-mocha"

# How often (ms) agent panes are polled for status + preview.
poll_interval_ms = 500

# Ask for confirmation before deleting a feature.
confirm_on_delete = true

[preview]
# How much of the agent's own colouring the preview panel shows. The pane is
# captured with its escape sequences, so by default you see exactly what the
# agent painted — spinners, diffs, syntax highlighting.
#   "full"       — every colour exactly as the pane emits it (default)
#   "foreground" — text colours kept, background colours dropped (the panel
#                  keeps the theme's background; also "no-background")
#   "none"       — raw text, no colours at all (also "text")
colors = "full"

[tmux]
# Keybinding (tmux syntax) pressed AFTER the tmux prefix (e.g. `C-b C-o`)
# inside an agent window to jump straight back to the LazyWingman
# dashboard. Bound in the PREFIX table, never the root table — a root
# binding would hijack the key for every session and other TUIs.
back_key = "C-o"

[worktrees]
# Root directory for feature worktrees. Absolute path, or relative to the
# project's parent directory. Empty = <project-parent>/.lazywingman-worktrees
root = ""
# Branch that feature branches are cut from. Falls back to the repository's
# current branch if this one doesn't exist in a project.
base_branch = "master"
# Also delete the feature branch when the feature is removed. Set to
# false to keep the branch behind after the worktree is gone.
delete_branch = true

[discovery]
# The scope: only projects inside this directory are listed (scopes never
# mix). Empty = the directory LazyWingman was started from. When you start
# inside a git repo, that repo is the scope and its features are shown
# flat (no project row). The scan is live (no cache) and runs at startup.
scan_root = ""
# Maximum subdirectory depth of a scan (0 = the scan root itself only).
max_depth = 4
# Directory names skipped during a scan. Hidden directories (names starting
# with '.') are always skipped; worktree `.git` files are never treated as
# repositories (their main repo covers them).
skip_dirs = [".git", "node_modules", "target", ".lazywingman-worktrees"]

[defaults]
# Agent command spawned in a feature shell unless a project overrides it.
agent_command = "claude"
# Shell used for agent windows.
agent_shell = "bash"
# Inject Claude Code hooks (--settings <generated file>) into recognised
# `claude` commands, so status is read from the agent's own hook events
# instead of guessed from regex patterns on the pane's output. Nothing is
# written to your own Claude config; the flag is visible in the pane's
# command line. Set to false to disable and always use the regex heuristics.
agent_hooks = true

# Predefined messages, offered by the `p` modal on a selected shell:
# pick one and it is sent to the agent exactly as if you had typed it.
# `label` is what the modal lists, `text` is what gets sent (label may be
# omitted — then the text is listed). Defining any [[messages]] entry
# replaces this whole list, like every other list in this file.
[[messages]]
label = "continue"
text = "continue"

[[messages]]
label = "merge + drop worktree"
text = "merge to master, drop the worktree"

# Agent presets, selectable per project via `agent = "<name>"`.
# The `patterns` regexes are matched against the tail of a pane's capture
# to classify its status (working / waiting / failed). Order of checks:
# dead > ended > failed > waiting > working > idle.
[[agents]]
name = "claude"
command = "claude"
[agents.patterns]
working = [
  "Thinking", "Running", "Reading", "Writing", "Searching", "Applying",
  "Planning", "Creating", "Editing", "Executing", "Progress",
  "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "●", "▰",
]
waiting = [
  "What would you like to do", "❯", "›", "(q) to quit", "q to quit",
  "> $",
]
failed = ["✖", "Error:", "error:", "exited with code", "Fatal", "failed"]

[[agents]]
name = "codex"
command = "codex"
[agents.patterns]
working = [
  "Working", "Reading", "Writing", "Running", "Thinking", "Exploring",
  "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "▌",
]
waiting = ["❯", "›", "codex>", "> $"]
failed = ["Error", "error:", "✖"]

# Projects declared by hand — the way to list a repo that discovery does not
# find (outside the scope). Discovered projects live in state.json instead.
# [[projects]]
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
    pub agents: Vec<AgentConfig>,
    pub projects: Vec<ProjectConfig>,
    /// Predefined messages offered by the `p` modal (quick-send presets).
    pub messages: Vec<MessageConfig>,
    /// Diagnostics collected while loading (per configuration rules: a
    /// malformed config never prevents startup — the smallest addressable
    /// unit is dropped and reported here). Filled by `load`; skipped by
    /// the strict `from_str` path used in tests.
    #[serde(skip)]
    pub diagnostics: Vec<String>,
    /// Layer provenance for `--config-dump`: "key = value  # from <layer>"
    /// — which layer (default/user/project/local) provided each key.
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
    /// Absolute, or relative to the project's parent directory.
    pub root: String,
    pub base_branch: String,
    pub delete_branch: bool,
}

/// Project discovery: recursive scans for git repositories.
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

/// One entry of the predefined-message modal (`p`): the label shown in
/// the list and the text pasted into the agent pane.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MessageConfig {
    /// Shown in the modal; falls back to `text` when left empty.
    pub label: String,
    /// What is sent to the shell — exactly as if it had been typed.
    pub text: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
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
            projects: Vec::new(),
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

/// The effective agent configuration for a project.
#[derive(Debug, Clone)]
pub struct EffectiveAgent {
    pub command: String,
    pub shell: String,
    /// Status patterns resolved for this agent (asserted in tests).
    #[allow(dead_code)]
    pub patterns: StatusPatterns,
}

/// Keys that belong to the user, not the project (configuration rule:
/// \"not every setting is local-able\"). A project-local file setting one of
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
    "agents",
    "projects",
    "messages",
];
const PREVIEW_KEYS: &[&str] = &["colors"];
const TMUX_KEYS: &[&str] = &["back_key"];
const WORKTREES_KEYS: &[&str] = &["root", "base_branch", "delete_branch"];
const DISCOVERY_KEYS: &[&str] = &["scan_root", "max_depth", "skip_dirs"];
const DEFAULTS_KEYS: &[&str] = &["agent_command", "agent_shell", "agent_hooks"];
const AGENT_KEYS: &[&str] = &["name", "command", "shell", "patterns"];
const PATTERN_KEYS: &[&str] = &["working", "waiting", "failed"];
const PROJECT_KEYS: &[&str] = &["path", "base_branch", "agent", "agent_command"];
const MESSAGE_KEYS: &[&str] = &["label", "text"];

impl Config {
    /// Load config, writing defaults (and built-in themes) on first run.
    /// Layered per the configuration rules: built-in defaults ← user config
    /// (`$XDG_CONFIG_HOME/lazywingman/config.toml`, or `LW_CONFIG`) ←
    /// project-local `.lazywingman.toml` (searched upward from the working
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

        // --- project layers ---------------------------------------------
        if let Some(dir) = find_project_dir(cwd) {
            let proj = dir.join(".lazywingman.toml");
            if proj.exists() {
                match parse_layer(&proj) {
                    Ok(table) => {
                        report_unknown_keys(&table, &proj, &mut diags);
                        merge_layer(
                            &mut merged,
                            table,
                            LAYER_PROJECT,
                            &proj,
                            &mut sources,
                            &mut diags,
                        );
                    }
                    Err(e) => diags.push(format!(
                        "{}: {e} — using defaults for this file",
                        proj.display()
                    )),
                }
                layer_paths.push((LAYER_PROJECT, proj.clone()));
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
        for p in &self.projects {
            if p.path.is_empty() {
                log::warn!("ignoring project with empty path");
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
            "projects",
            format!(
                "[{}]",
                self.projects
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
                    .map(|m| format!("\"{}\"", m.label))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        out
    }

    /// Projects with a non-empty path.
    pub fn usable_projects(&self) -> Vec<&ProjectConfig> {
        self.projects
            .iter()
            .filter(|p| !p.path.is_empty())
            .collect()
    }

    /// Effective agent (command, shell, status patterns) for a project.
    pub fn agent_for(&self, project: &ProjectConfig) -> EffectiveAgent {
        // Resolve preset by name first.
        let preset = project
            .agent
            .as_deref()
            .and_then(|n| self.agents.iter().find(|a| a.name == n));

        let command = project
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
        let patterns = if project.agent_command.is_some() {
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

    /// Root directory for a project's feature worktrees.
    pub fn worktree_root_for(&self, project_path: &Path) -> PathBuf {
        let parent = project_path.parent().unwrap_or(project_path);
        let root = self.worktrees.root.trim();
        let root = if root.is_empty() {
            parent.join(".lazywingman-worktrees")
        } else {
            let p = PathBuf::from(root);
            if p.is_absolute() { p } else { parent.join(p) }
        };
        crate::util::normalize_path(&root)
    }

    /// Base branch for a project (per-project override, else global).
    pub fn base_branch_for(&self, project: &ProjectConfig) -> String {
        project
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

/// The nearest project-local config directory working upward from `cwd`,
/// stopping at the first hit *and* at a repository root (the project
/// boundary — a repo-root file, if any, is checked first, then the search
/// stops so a sibling repo's config is never inherited).
fn find_project_dir(cwd: &Path) -> Option<PathBuf> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if d.join(".lazywingman.toml").is_file() || d.join(".lazywingman.local.toml").is_file() {
            return Some(d.to_path_buf());
        }
        // Repository root is the project boundary: check it (already did),
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
        if key == "projects"
            && let Some(arr) = value.as_array()
        {
            for entry in arr {
                let Some(t) = entry.as_table() else { continue };
                for pk in t.keys() {
                    if !PROJECT_KEYS.contains(&pk.as_str()) {
                        diags.push(format!(
                            "{}: unknown key 'projects.{pk}' ignored",
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
/// agent/project entries and status-pattern regexes with a diagnostic, keep
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

    // lists — per-entry salvage; a missing key keeps the built-in defaults
    // (a config file without [[agents]] still gets claude/codex presets,
    // exactly like the serde-default path it replaces)
    cfg.agents = take_agent_list(merged, layer_paths, sources, diags, cfg.agents);
    cfg.projects = take_project_list(merged, layer_paths, sources, diags);
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

fn take_project_list(
    merged: &toml::Table,
    layer_paths: &[(&'static str, PathBuf)],
    sources: &[(String, &'static str)],
    diags: &mut Vec<String>,
) -> Vec<ProjectConfig> {
    let mut projects =
        take_list::<ProjectConfig>(merged, "projects", Vec::new(), layer_paths, sources, diags);
    let file = where_file(layer_paths, sources, "projects");
    projects.retain(|p| {
        let ok = !p.path.is_empty();
        if !ok {
            diags.push(format!("{file}: project with empty path — entry dropped"));
        }
        ok
    });
    projects
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
    // A label is a convenience, not a requirement: an entry without one
    // is listed by the text it sends.
    for m in messages.iter_mut() {
        if m.label.trim().is_empty() {
            m.label = m.text.clone();
        }
    }
    messages
}

/// The built-in predefined messages, used when no config layer defines
/// `[[messages]]` (a list that IS defined replaces these, like every
/// other config list).
pub fn default_messages() -> Vec<MessageConfig> {
    vec![
        MessageConfig {
            label: "continue".into(),
            text: "continue".into(),
        },
        MessageConfig {
            label: "merge + drop worktree".into(),
            text: "merge to master, drop the worktree".into(),
        },
    ]
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
                "the feature branch goes with the worktree by default"
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
            let labels: Vec<&str> = cfg.messages.iter().map(|m| m.label.as_str()).collect();
            assert_eq!(labels, vec!["continue", "merge + drop worktree"]);
        });
    }

    #[test]
    fn user_overrides_are_honoured() {
        with_fresh_xdg(|| {
            Config::load().unwrap();
            let mut text = std::fs::read_to_string(crate::util::config_path()).unwrap();
            text = text.replace("theme = \"catppuccin-mocha\"", "theme = \"ansi\"");
            text = text.replace("poll_interval_ms = 500", "poll_interval_ms = 250");
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
    fn projects_and_overrides_parse() {
        let cfg = Config::from_str(
            r#"
            [[projects]]
            path = "/tmp/repo"
            base_branch = "develop"
            agent = "codex"
            "#,
        )
        .unwrap();
        let p = &cfg.projects[0];
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
            [[projects]]
            path = "/tmp/repo"
            agent = "claude"
            agent_command = "codex exec --yolo"
            "#,
        )
        .unwrap();
        let a = cfg.agent_for(&cfg.projects[0]);
        assert_eq!(a.command, "codex exec --yolo");
        // patterns picked from codex preset via prefix match
        assert!(a.patterns.waiting.iter().any(|w| w.contains("codex>")));
    }

    #[test]
    fn unknown_preset_falls_back_to_global_default() {
        let cfg = Config::from_str(
            r#"
            [[projects]]
            path = "/tmp/repo"
            agent = "no-such-agent"
            "#,
        )
        .unwrap();
        let a = cfg.agent_for(&cfg.projects[0]);
        assert_eq!(a.command, "claude");
        assert!(a.patterns.waiting.iter().any(|w| w.contains("❯")));
    }

    #[test]
    fn worktree_root_resolution() {
        let project = Path::new("/work/projects/my-repo");
        // default: sibling of the repo
        let cfg = Config::default();
        assert_eq!(
            cfg.worktree_root_for(project),
            PathBuf::from("/work/projects/.lazywingman-worktrees")
        );
        // absolute override
        let cfg = Config::from_str("[worktrees]\nroot = \"/wt\"\n").unwrap();
        assert_eq!(cfg.worktree_root_for(project), PathBuf::from("/wt"));
        // relative override → resolved against project parent
        let cfg = Config::from_str("[worktrees]\nroot = \"../wt\"\n").unwrap();
        assert_eq!(cfg.worktree_root_for(project), PathBuf::from("/work/wt"));
    }

    #[test]
    fn base_branch_falls_back_to_global() {
        let cfg = Config::from_str("[[projects]]\npath = \"/x\"\n").unwrap();
        assert_eq!(cfg.base_branch_for(&cfg.projects[0]), "master");
    }

    #[test]
    fn malformed_agents_are_skipped_not_fatal() {
        let cfg = Config::from_str("[[agents]]\nname = \"\"\ncommand = \"\"\n").unwrap();
        let a = cfg.agent_for(&ProjectConfig::default());
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

    // ---------------------------------------------- M8.4d: project layering

    #[test]
    fn project_config_layers_over_user() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            let cwd = root.path().join("sub").join("deep");
            std::fs::create_dir_all(&cwd).unwrap();
            // user config
            load_user(
                &cwd,
                "theme = \"dracula\"\npoll_interval_ms = 250\n[tmux]\nback_key = \"C-x\"\n",
            );
            // project-local config in an ancestor: overrides ONE key
            std::fs::write(
                root.path().join("sub/.lazywingman.toml"),
                "theme = \"ansi\"\n",
            )
            .unwrap();
            let cfg = Config::load_from(&cwd, None).unwrap();
            assert_eq!(cfg.theme, "ansi"); // project wins for theme
            assert_eq!(cfg.poll_interval_ms, 250); // user keeps poll
            assert_eq!(cfg.tmux.back_key, "C-x"); // user keeps tmux
            // provenance: theme came from the project layer
            assert!(
                cfg.sources
                    .iter()
                    .any(|(k, l)| k == "theme" && *l == LAYER_PROJECT)
            );
            assert!(
                cfg.sources
                    .iter()
                    .any(|(k, l)| k == "poll_interval_ms" && *l == LAYER_USER)
            );
        });
    }

    #[test]
    fn project_config_found_upward_stops_at_first_hit_and_repo_root() {
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
    fn local_sibling_config_wins_over_project_config() {
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
                "[[agents]]\nname=\"project-agent\"\ncommand=\"p\"\n",
            )
            .unwrap();
            let cfg = Config::load_from(root.path(), None).unwrap();
            // the project list REPLACES the user list outright
            assert_eq!(cfg.agents.len(), 1);
            assert_eq!(cfg.agents[0].name, "project-agent");
            assert!(
                cfg.sources
                    .iter()
                    .any(|(k, l)| k == "agents" && *l == LAYER_PROJECT)
            );
        });
    }

    #[test]
    fn user_only_keys_rejected_in_project_files() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            load_user(root.path(), "[tmux]\nback_key = \"C-x\"\n");
            std::fs::write(
                root.path().join(".lazywingman.toml"),
                "[tmux]\nback_key = \"C-b\"\n[worktrees]\nroot = \"/machine/path\"\n",
            )
            .unwrap();
            let cfg = Config::load_from(root.path(), None).unwrap();
            // user's binding wins; the project file's attempt is reported
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
    fn config_dump_lists_effective_values_with_layer_provenance() {
        with_fresh_xdg(|| {
            let root = tempfile::TempDir::new().unwrap();
            load_user(root.path(), "theme = \"dracula\"\npoll_interval_ms = 250\n");
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
            assert!(dump.contains("worktrees.base_branch = \"develop\"  # from project"));
            assert!(dump.contains("tmux.back_key = \"C-o\"  # from defaults"));
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
            let labels: Vec<&str> = cfg.messages.iter().map(|m| m.label.as_str()).collect();
            assert_eq!(labels, vec!["continue", "merge + drop worktree"]);
            assert_eq!(cfg.messages[0].text, "continue");
            assert_eq!(cfg.messages[1].text, "merge to master, drop the worktree");
        });
    }

    #[test]
    fn configured_messages_replace_the_defaults() {
        let cfg = Config::from_str(
            r#"
            [[messages]]
            label = "ship it"
            text = "run the gate and push"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.messages.len(), 1);
        assert_eq!(cfg.messages[0].label, "ship it");
        assert_eq!(cfg.messages[0].text, "run the gate and push");
    }

    #[test]
    fn message_without_label_falls_back_to_its_text() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(cwd.path(), "[[messages]]\ntext = \"continue\"\n");
            assert_eq!(cfg.messages.len(), 1);
            assert_eq!(cfg.messages[0].label, "continue");
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
                label = "empty"
                text = ""

                [[messages]]
                label = "ok"
                text = "continue"
                "#,
            );
            let labels: Vec<&str> = cfg.messages.iter().map(|m| m.label.as_str()).collect();
            assert_eq!(labels, vec!["ok"]);
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
                "[[messages]]\nlabel = \"go\"\ntext = \"continue\"\ncolour = \"red\"\n",
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
    fn config_dump_lists_message_labels() {
        with_fresh_xdg(|| {
            let cwd = tempfile::TempDir::new().unwrap();
            let cfg = load_user(cwd.path(), "theme = \"ansi\"\n");
            assert!(
                cfg.dump_string().contains(
                    "messages = [\"continue\", \"merge + drop worktree\"]  # from defaults"
                ),
                "dump: {}",
                cfg.dump_string()
            );
        });
    }
}
