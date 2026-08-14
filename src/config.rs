//! Configuration loading, defaults, and first-run bootstrap.
//!
//! Config lives at `$XDG_CONFIG_HOME/lazywingman/config.toml`. On first run
//! a fully commented default config is written (and built-in themes are
//! copied into `themes/`). Parsing is lenient — unknown keys are ignored —
//! but a structurally invalid file is a hard error so the user sees it.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// Fully commented default config, written on first run.
pub const DEFAULT_CONFIG: &str = r#"# LazyWingman configuration — written on first run, edit freely.
# Location: $XDG_CONFIG_HOME/lazywingman/config.toml (default ~/.config/lazywingman)

# Theme to use. Built-ins: "dracula", "gruvbox", "ansi".
# Drop your own theme into ~/.config/lazywingman/themes/<name>.toml
theme = "dracula"

# How often (ms) agent panes are polled for status + preview.
poll_interval_ms = 500

# Ask for confirmation before deleting a feature.
confirm_on_delete = true

[tmux]
# Keybinding (tmux syntax) pressed inside an agent window to jump straight
# back to the LazyWingman dashboard. Registered globally; pick something
# that doesn't clash with your other tmux bindings.
back_key = "C-o"

[worktrees]
# Root directory for feature worktrees. Absolute path, or relative to the
# project's parent directory. Empty = <project-parent>/.lazywingman-worktrees
root = ""
# Branch that feature branches are cut from. Falls back to the repository's
# current branch if this one doesn't exist in a project.
base_branch = "master"
# Also delete the feature branch when the feature is removed.
delete_branch = false

[defaults]
# Agent command spawned in a feature shell unless a project overrides it.
agent_command = "claude"
# Shell used for agent windows.
agent_shell = "bash"

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

# Imported projects. Add them from inside the TUI (`i` key) — they land here.
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
    pub tmux: TmuxConfig,
    pub worktrees: WorktreeConfig,
    pub defaults: DefaultsConfig,
    pub agents: Vec<AgentConfig>,
    pub projects: Vec<ProjectConfig>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DefaultsConfig {
    pub agent_command: String,
    pub agent_shell: String,
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
            theme: "dracula".into(),
            poll_interval_ms: 500,
            confirm_on_delete: true,
            tmux: TmuxConfig {
                back_key: "C-o".into(),
            },
            worktrees: WorktreeConfig {
                root: String::new(),
                base_branch: "master".into(),
                delete_branch: false,
            },
            defaults: DefaultsConfig {
                agent_command: "claude".into(),
                agent_shell: "bash".into(),
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
            delete_branch: false,
        }
    }
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        DefaultsConfig {
            agent_command: "claude".into(),
            agent_shell: "bash".into(),
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

impl Config {
    /// Load config, writing defaults (and built-in themes) on first run.
    pub fn load() -> Result<Config> {
        let path = crate::util::config_path();
        if !path.exists() {
            std::fs::create_dir_all(crate::util::config_dir())?;
            std::fs::write(&path, DEFAULT_CONFIG)?;
            log::info!("wrote default config to {}", path.display());
            crate::theme::ensure_builtin_themes()?;
        }
        let text = std::fs::read_to_string(&path)?;
        let cfg: Config =
            toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        cfg.validate();
        Ok(cfg)
    }

    /// Load from an explicit string (tests / previews).
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
            assert_eq!(cfg.theme, "dracula");
            assert_eq!(cfg.poll_interval_ms, 500);
            assert!(cfg.confirm_on_delete);
            assert_eq!(cfg.tmux.back_key, "C-o");
            assert_eq!(cfg.defaults.agent_command, "claude");
            assert_eq!(cfg.worktrees.base_branch, "master");
            assert!(crate::util::config_path().exists());
            // themes copied too
            assert!(crate::util::themes_dir().join("dracula.toml").exists());
            // written file parses back identically
            let again = Config::load().unwrap();
            assert_eq!(again.theme, "dracula");
        });
    }

    #[test]
    fn user_overrides_are_honoured() {
        with_fresh_xdg(|| {
            Config::load().unwrap();
            let mut text = std::fs::read_to_string(crate::util::config_path()).unwrap();
            text = text.replace("theme = \"dracula\"", "theme = \"ansi\"");
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
}
