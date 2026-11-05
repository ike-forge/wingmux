//! Theme loading and colour resolution.
//!
//! Themes are TOML files describing named colour roles. Three built-ins ship
//! with the binary (`dracula`, `gruvbox`, `ansi`) and are copied into
//! `$XDG_CONFIG_HOME/lazywingman/themes/` on first run so the user can edit
//! them; any `<name>.toml` in that directory overrides the built-in of the
//! same name.
//!
//! Colours may be specified as `"#rrggbb"` hex, an ANSI-16 name
//! (`black`..`white`, `bright_black`.., `gray`), or `"default"` (terminal
//! default). Hex colours are emitted as truecolor RGB when the terminal
//! advertises 24-bit support and quantised to the xterm-256 cube otherwise.
//! The `ansi` theme uses only ANSI-16 colours, so it always inherits the
//! terminal's palette.

use std::path::Path;

use ratatui::style::Color;
use serde::Deserialize;

use crate::error::{Error, Result};

/// A colour as written in a theme file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spec {
    /// `#rrggbb`
    Hex(u32),
    /// ANSI-16 palette colour.
    Ansi(AnsiColor),
    /// Terminal default (no colour set).
    Default,
}

/// The ANSI-16 palette (plus grey, an alias for `bright_black`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsiColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    Gray,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
}

/// All colour roles a theme must define.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ThemeColors {
    pub background: Spec,
    pub foreground: Spec,
    pub dim: Spec,
    pub accent: Spec,
    pub selection_bg: Spec,
    pub selection_fg: Spec,
    pub border: Spec,
    pub border_focused: Spec,
    pub tree_repository: Spec,
    pub tree_worktree: Spec,
    pub tree_shell: Spec,
    pub status_waiting: Spec,
    pub status_failed: Spec,
    /// The five roles below need no value: each borrows a base colour when
    /// a theme leaves it out, so a theme only has to answer for the states
    /// that must be unmistakable (`status_waiting`, `status_failed`). Every
    /// one stays overridable, and the shipped themes do override two of
    /// them — the states a user acts on get their own hue from the
    /// palette:
    ///
    /// - `blocked` — orange/peach. It shares the "stopped, wants you"
    ///   family with waiting's yellow while staying its own colour, because
    ///   blocked owes the user an answer *now* and waiting does not.
    /// - `ended` — purple/mauve. The agent is gone and the shell is
    ///   ordinary: it must not read as a state you can act on.
    ///
    /// The fallbacks are what a theme file that omits them gets: `working`
    /// and `ended` read as `foreground` (an agent doing its job needs no
    /// alarm colour, and the spinner already says it is running), `idle`
    /// and `dead` as `dim` (nothing to act on, so they recede), `blocked`
    /// as `status_waiting`.
    #[serde(default)]
    pub status_working: Option<Spec>,
    #[serde(default)]
    pub status_blocked: Option<Spec>,
    #[serde(default)]
    pub status_idle: Option<Spec>,
    #[serde(default)]
    pub status_dead: Option<Spec>,
    #[serde(default)]
    pub status_ended: Option<Spec>,
    /// The icon of a worktree whose commits are all in the base branch:
    /// done, safe to delete. It is the one worktree state with a colour of
    /// its own rather than a borrowed status role — `working` is ordinary
    /// text, which left merged indistinguishable from the tree around it.
    /// Unset, it borrows `tree_worktree`, green in every shipped theme.
    #[serde(default)]
    pub worktree_merged: Option<Spec>,
    /// The key in a shortcut hint. Unset, it borrows `accent`; a theme sets
    /// it when the hue that suits borders and labels is not the one that
    /// makes a key letter stand out beside body text.
    #[serde(default)]
    pub shortcut: Option<Spec>,
    pub title_bg: Spec,
    pub title_fg: Spec,
    pub popup_bg: Spec,
    pub popup_fg: Spec,
    pub popup_border: Spec,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThemeFile {
    pub name: String,
    pub colors: ThemeColors,
}

/// A fully resolved theme: every role is a concrete [`Color`].
#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub colors: ResolvedColors,
}

#[derive(Debug, Clone)]
pub struct ResolvedColors {
    pub background: Color,
    pub foreground: Color,
    pub dim: Color,
    pub accent: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub border: Color,
    pub border_focused: Color,
    pub tree_repository: Color,
    pub tree_worktree: Color,
    pub tree_shell: Color,
    pub status_working: Color,
    pub status_waiting: Color,
    pub status_blocked: Color,
    pub status_idle: Color,
    pub status_failed: Color,
    pub status_dead: Color,
    pub status_ended: Color,
    pub worktree_merged: Color,
    pub shortcut: Color,
    pub title_bg: Color,
    pub title_fg: Color,
    pub popup_bg: Color,
    pub popup_fg: Color,
    pub popup_border: Color,
}

/// Whether the terminal supports 24-bit colour.
///
/// `COLORTERM=truecolor|24bit` is the answer when it is there, but plenty of
/// 24-bit terminals never export it: tmux sets it inside its own panes, so a
/// standalone dashboard is exactly the case where it goes missing and every
/// theme colour needlessly drops through [`quantize_256`]. So when
/// `COLORTERM` is silent the terminal identifies itself instead — a
/// `-direct` terminfo entry, a `TERM`/`TERM_PROGRAM` naming a terminal that
/// has always been 24-bit, or a marker variable one of them exports.
/// Anything unrecognised stays on the 256-colour fallback: over-claiming
/// paints unreadable colours, under-claiming only paints approximate ones.
pub fn supports_truecolor() -> bool {
    truecolor_from(|k| std::env::var(k).ok())
}

/// `TERM` values (matched as substrings) that mean 24-bit colour.
const TRUECOLOR_TERMS: &[&str] = &[
    "direct", // xterm-direct, tmux-direct, … — terminfo's own 24-bit entries
    "truecolor",
    "kitty",
    "alacritty",
    "wezterm",
    "contour",
    "ghostty",
    "foot",
];

/// `TERM_PROGRAM` values (compared case-insensitively) that mean 24-bit
/// colour. `Apple_Terminal` is deliberately absent: Terminal.app is 256.
const TRUECOLOR_TERM_PROGRAMS: &[&str] = &[
    "iterm.app",
    "wezterm",
    "vscode",
    "hyper",
    "ghostty",
    "rio",
    "tabby",
];

/// Presence alone is the signal: each is exported only by a 24-bit terminal.
const TRUECOLOR_MARKERS: &[&str] = &[
    "WT_SESSION",      // Windows Terminal
    "KONSOLE_VERSION", // Konsole
    "KITTY_WINDOW_ID", // kitty, even under a TERM it does not own
    "GHOSTTY_RESOURCES_DIR",
    "ALACRITTY_WINDOW_ID",
    "WEZTERM_EXECUTABLE",
];

/// The env lookup is a parameter so the decision is testable without
/// mutating the process environment (which no test may do concurrently).
fn truecolor_from(env: impl Fn(&str) -> Option<String>) -> bool {
    if let Some(ct) = env("COLORTERM") {
        // An explicit answer is final, in both directions: a terminal that
        // sets COLORTERM to something else is saying it is not 24-bit.
        return ct.contains("truecolor") || ct.contains("24bit");
    }
    let term = env("TERM").unwrap_or_default().to_ascii_lowercase();
    if term.is_empty() || term == "dumb" {
        return false;
    }
    if TRUECOLOR_TERMS.iter().any(|t| term.contains(t)) {
        return true;
    }
    if let Some(prog) = env("TERM_PROGRAM") {
        let prog = prog.to_ascii_lowercase();
        if TRUECOLOR_TERM_PROGRAMS.iter().any(|p| *p == prog) {
            return true;
        }
    }
    if TRUECOLOR_MARKERS
        .iter()
        .any(|k| env(k).is_some_and(|v| !v.is_empty()))
    {
        return true;
    }
    // VTE (GNOME Terminal, Tilix, …) gained 24-bit colour in 0.36.
    env("VTE_VERSION")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .is_some_and(|v| v >= 3600)
}

/// `NO_COLOR` (no-color.org): when present and not an empty string, the app
/// must not emit colour at all. Everything meaningful is carried by symbols,
/// labels and position anyway, so a monochrome render stays usable — the UI
/// is never colour-only.
pub fn no_color() -> bool {
    std::env::var("NO_COLOR")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

impl<'de> serde::Deserialize<'de> for Spec {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Spec::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl Spec {
    /// Parse a colour string from a theme file.
    pub fn parse(s: &str) -> Result<Spec> {
        let s = s.trim();
        if s == "default" {
            return Ok(Spec::Default);
        }
        if let Some(hex) = s.strip_prefix('#') {
            if hex.len() != 6 {
                return Err(Error::Theme(format!(
                    "bad hex colour '{s}' (expected #rrggbb)"
                )));
            }
            let v = u32::from_str_radix(hex, 16)
                .map_err(|_| Error::Theme(format!("bad hex colour '{s}'")))?;
            return Ok(Spec::Hex(v));
        }
        Ok(Spec::Ansi(parse_ansi(s).ok_or_else(|| {
            Error::Theme(format!(
                "unknown colour '{s}' (expected #rrggbb, ansi name, or 'default')"
            ))
        })?))
    }

    /// Resolve to a concrete colour, honouring terminal capabilities.
    pub fn resolve(&self, truecolor: bool) -> Color {
        match self {
            Spec::Default => Color::Reset,
            Spec::Ansi(c) => c.to_color(),
            Spec::Hex(rgb) => {
                if truecolor {
                    Color::Rgb(
                        ((rgb >> 16) & 0xff) as u8,
                        ((rgb >> 8) & 0xff) as u8,
                        (rgb & 0xff) as u8,
                    )
                } else {
                    Color::Indexed(quantize_256(
                        ((rgb >> 16) & 0xff) as u8,
                        ((rgb >> 8) & 0xff) as u8,
                        (rgb & 0xff) as u8,
                    ))
                }
            }
        }
    }
}

impl AnsiColor {
    pub fn to_color(self) -> Color {
        use AnsiColor::*;
        match self {
            Black => Color::Black,
            Red => Color::Red,
            Green => Color::Green,
            Yellow => Color::Yellow,
            Blue => Color::Blue,
            Magenta => Color::Magenta,
            Cyan => Color::Cyan,
            White => Color::White,
            Gray | BrightBlack => Color::DarkGray,
            BrightRed => Color::LightRed,
            BrightGreen => Color::LightGreen,
            BrightYellow => Color::LightYellow,
            BrightBlue => Color::LightBlue,
            BrightMagenta => Color::LightMagenta,
            BrightCyan => Color::LightCyan,
        }
    }
}

fn parse_ansi(s: &str) -> Option<AnsiColor> {
    use AnsiColor::*;
    Some(match s.to_ascii_lowercase().as_str() {
        "black" => Black,
        "red" => Red,
        "green" => Green,
        "yellow" => Yellow,
        "blue" => Blue,
        "magenta" => Magenta,
        "cyan" => Cyan,
        "white" => White,
        "gray" | "grey" => Gray,
        "bright_black" | "light_black" | "dark_gray" | "dark_grey" => BrightBlack,
        "bright_red" | "light_red" => BrightRed,
        "bright_green" | "light_green" => BrightGreen,
        "bright_yellow" | "light_yellow" => BrightYellow,
        "bright_blue" | "light_blue" => BrightBlue,
        "bright_magenta" | "light_magenta" => BrightMagenta,
        "bright_cyan" | "light_cyan" => BrightCyan,
        _ => return None,
    })
}

/// Quantise an RGB colour to the nearest xterm-256 palette entry.
///
/// Both halves of the palette are candidates: the 6×6×6 cube (levels 0, 95,
/// 135, 175, 215, 255) and the 24-step grey ramp (8, 18, … 238), and the
/// closer of the two wins by squared RGB distance. Considering the ramp for
/// *every* colour, not just exact greys, is what keeps near-grey theme
/// colours apart: the cube's lowest levels are 0 and 95, so a dark
/// background (#343646) and a dim foreground (#51576d) both round to
/// (95, 95, 95) — the same index — and the text vanishes into its own
/// background. The ramp separates them.
fn quantize_256(r: u8, g: u8, b: u8) -> u8 {
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let dist = |a: (u8, u8, u8)| -> i32 {
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
        d(a.0, r) + d(a.1, g) + d(a.2, b)
    };
    let level = |c: u8| -> usize {
        CUBE.iter()
            .enumerate()
            .min_by_key(|&(_, &v)| (v as i32 - c as i32).abs())
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube_index = 16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8;
    let cube_dist = dist((CUBE[ri], CUBE[gi], CUBE[bi]));

    // Grey ramp: index 232 + i holds 8 + 10i, so the nearest step to the
    // colour's mean channel is (mean - 8) / 10, rounded and clamped.
    let mean = (r as i32 + g as i32 + b as i32) / 3;
    let step = ((mean - 8 + 5) / 10).clamp(0, 23);
    let grey = (8 + 10 * step) as u8;
    if dist((grey, grey, grey)) < cube_dist {
        232 + step as u8
    } else {
        cube_index
    }
}

/// Built-in themes embedded in the binary. The Catppuccin set is the
/// reference palette (theming guideline); dracula/gruvbox/ansi predate it.
/// Order here is the order the theme picker lists built-ins in.
pub const BUILTIN_THEMES: [(&str, &str); 7] = [
    (
        "catppuccin-mocha",
        include_str!("../themes/catppuccin-mocha.toml"),
    ),
    (
        "catppuccin-macchiato",
        include_str!("../themes/catppuccin-macchiato.toml"),
    ),
    (
        "catppuccin-frappe",
        include_str!("../themes/catppuccin-frappe.toml"),
    ),
    (
        "catppuccin-latte",
        include_str!("../themes/catppuccin-latte.toml"),
    ),
    ("dracula", include_str!("../themes/dracula.toml")),
    ("gruvbox", include_str!("../themes/gruvbox.toml")),
    ("ansi", include_str!("../themes/ansi.toml")),
];

/// The default theme (config `theme = ...` when unset).
pub const DEFAULT_THEME: &str = "catppuccin-mocha";

/// Copy built-in themes into the user themes dir if they don't exist yet.
pub fn ensure_builtin_themes() -> Result<()> {
    let dir = crate::util::themes_dir();
    std::fs::create_dir_all(&dir)?;
    for (name, content) in BUILTIN_THEMES {
        let path = dir.join(format!("{name}.toml"));
        if !path.exists() {
            std::fs::write(&path, content)?;
            log::info!("wrote built-in theme to {}", path.display());
        }
    }
    Ok(())
}

/// Resolve and load a theme by name: user themes dir first, then built-ins.
/// Honour `NO_COLOR`: when set, every role resolves to the terminal default.
pub fn load(name: &str, truecolor: bool) -> Result<Theme> {
    load_with(name, truecolor, no_color())
}

/// [`Self::load`] with an explicit monochrome decision (tests pass it
/// directly instead of touching `NO_COLOR`, which is process-global).
pub fn load_with(name: &str, truecolor: bool, monochrome: bool) -> Result<Theme> {
    let file = load_file(name)?;
    resolve_theme(file, truecolor, monochrome)
}

fn load_file(name: &str) -> Result<ThemeFile> {
    let user_path = crate::util::themes_dir().join(format!("{name}.toml"));
    if let Some(content) = read_if_exists(&user_path)? {
        return parse_theme(&content, &user_path);
    }
    for (builtin, content) in BUILTIN_THEMES {
        if builtin == name {
            return parse_theme(content, Path::new(&format!("<builtin:{name}>")));
        }
    }
    Err(Error::Theme(format!(
        "unknown theme '{name}' (built-ins: {}, …)",
        builtin_names().join(", ")
    )))
}

/// Names of all embedded themes, in [`BUILTIN_THEMES`] order.
pub fn builtin_names() -> Vec<String> {
    BUILTIN_THEMES.iter().map(|(n, _)| n.to_string()).collect()
}

/// Every theme the picker can offer: built-ins first (the order is the
/// same in every project, per the theming guideline), then the user's
/// themes from the themes dir, sorted and deduplicated. A user theme that
/// shadows a built-in name is listed once (the user copy wins on load).
pub fn available_names() -> Result<Vec<String>> {
    let mut names = builtin_names();
    let dir = crate::util::themes_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut user: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "toml") {
                    p.file_stem().map(|s| s.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .collect();
        user.sort();
        for n in user {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    Ok(names)
}

fn read_if_exists(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

fn parse_theme(content: &str, source: &Path) -> Result<ThemeFile> {
    toml::from_str::<ThemeFile>(content)
        .map_err(|e| Error::Theme(format!("{}: {e}", source.display())))
}

fn resolve_theme(file: ThemeFile, truecolor: bool, monochrome: bool) -> Result<Theme> {
    let c = file.colors;
    // NO_COLOR: no colour at all — every role falls back to the terminal
    // default. This is not a low-colour mode; it is a no-colour mode, and
    // everything the theme conveyed semantically is still readable because
    // the UI carries meaning in symbols, labels and position too.
    let resolve = |s: Spec| -> Color {
        if monochrome {
            Color::Reset
        } else {
            s.resolve(truecolor)
        }
    };
    Ok(Theme {
        name: file.name,
        colors: ResolvedColors {
            background: resolve(c.background),
            foreground: resolve(c.foreground),
            dim: resolve(c.dim),
            accent: resolve(c.accent),
            selection_bg: resolve(c.selection_bg),
            selection_fg: resolve(c.selection_fg),
            border: resolve(c.border),
            border_focused: resolve(c.border_focused),
            tree_repository: resolve(c.tree_repository),
            tree_worktree: resolve(c.tree_worktree),
            tree_shell: resolve(c.tree_shell),
            status_working: resolve(c.status_working.unwrap_or(c.foreground)),
            status_waiting: resolve(c.status_waiting),
            status_blocked: resolve(c.status_blocked.unwrap_or(c.status_waiting)),
            status_idle: resolve(c.status_idle.unwrap_or(c.dim)),
            status_failed: resolve(c.status_failed),
            status_dead: resolve(c.status_dead.unwrap_or(c.dim)),
            status_ended: resolve(c.status_ended.unwrap_or(c.foreground)),
            worktree_merged: resolve(c.worktree_merged.unwrap_or(c.tree_worktree)),
            shortcut: resolve(c.shortcut.unwrap_or(c.accent)),
            title_bg: resolve(c.title_bg),
            title_fg: resolve(c.title_fg),
            popup_bg: resolve(c.popup_bg),
            popup_fg: resolve(c.popup_fg),
            popup_border: resolve(c.popup_border),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_colours() {
        assert_eq!(Spec::parse("#ff8800").unwrap(), Spec::Hex(0xff8800));
        assert_eq!(Spec::parse("#000000").unwrap(), Spec::Hex(0));
        assert!(Spec::parse("#12345").is_err());
        assert!(Spec::parse("#gggggg").is_err());
    }

    #[test]
    fn parse_ansi_names() {
        assert_eq!(Spec::parse("cyan").unwrap(), Spec::Ansi(AnsiColor::Cyan));
        assert_eq!(
            Spec::parse("Bright_Blue").unwrap(),
            Spec::Ansi(AnsiColor::BrightBlue)
        );
        assert_eq!(Spec::parse("grey").unwrap(), Spec::Ansi(AnsiColor::Gray));
        assert_eq!(Spec::parse("default").unwrap(), Spec::Default);
        assert!(Spec::parse("chartreuse").is_err());
    }

    #[test]
    fn truecolor_resolution() {
        let s = Spec::parse("#ff0000").unwrap();
        assert_eq!(s.resolve(true), Color::Rgb(255, 0, 0));
    }

    /// `COLORTERM` is the fast path, and its absence is not a "no": a
    /// standalone dashboard usually has no `COLORTERM` at all (tmux is what
    /// sets it in its panes), so the terminal's own identity has to answer.
    #[test]
    fn truecolor_detection() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_string())
            }
        };

        // COLORTERM answers on its own, either way.
        assert!(truecolor_from(env(&[("COLORTERM", "truecolor")])));
        assert!(truecolor_from(env(&[("COLORTERM", "24bit")])));
        assert!(!truecolor_from(env(&[
            ("COLORTERM", "8bit"),
            ("WT_SESSION", "abc")
        ])));

        // No COLORTERM: the terminal identifies itself.
        assert!(truecolor_from(env(&[
            ("TERM", "xterm-256color"),
            ("WT_SESSION", "0525855c")
        ])));
        assert!(truecolor_from(env(&[("TERM", "xterm-direct")])));
        assert!(truecolor_from(env(&[("TERM", "xterm-kitty")])));
        assert!(truecolor_from(env(&[
            ("TERM", "xterm-256color"),
            ("TERM_PROGRAM", "WezTerm")
        ])));
        assert!(truecolor_from(env(&[
            ("TERM", "xterm-256color"),
            ("VTE_VERSION", "7600")
        ])));

        // Unrecognised, empty or hostile terminals keep the 256 fallback.
        assert!(!truecolor_from(env(&[("TERM", "xterm-256color")])));
        assert!(!truecolor_from(env(&[("TERM", "screen")])));
        assert!(!truecolor_from(env(&[
            ("TERM", "dumb"),
            ("WT_SESSION", "x")
        ])));
        assert!(!truecolor_from(env(&[])));
        assert!(!truecolor_from(env(&[
            ("TERM", "xterm-256color"),
            ("TERM_PROGRAM", "Apple_Terminal")
        ])));
        assert!(!truecolor_from(env(&[
            ("TERM", "xterm-256color"),
            ("VTE_VERSION", "3400")
        ])));
        // A marker present but empty says nothing.
        assert!(!truecolor_from(env(&[
            ("TERM", "xterm-256color"),
            ("WT_SESSION", "")
        ])));
    }

    #[test]
    fn quantization_to_256() {
        assert_eq!(quantize_256(0, 0, 0), 16);
        assert_eq!(quantize_256(255, 255, 255), 231);
        assert_eq!(quantize_256(255, 0, 0), 196);
        assert_eq!(quantize_256(0, 255, 0), 46);
        assert_eq!(quantize_256(0, 0, 255), 21);
        // grey ramp: 232 + i holds 8 + 10i, so 128 is exactly index 244
        assert_eq!(quantize_256(128, 128, 128), 244);
        // A near-grey colour takes the ramp too, not just an exact grey:
        // the cube would round both of these to (95, 95, 95).
        assert_ne!(
            quantize_256(0x34, 0x36, 0x46),
            quantize_256(0x51, 0x57, 0x6d)
        );
        let s = Spec::parse("#bd93f9").unwrap();
        assert!(matches!(s.resolve(false), Color::Indexed(_)));
        // ansi theme ignores truecolor
        assert_eq!(Spec::Ansi(AnsiColor::Cyan).resolve(false), Color::Cyan);
    }

    /// Outside tmux a terminal often leaves `COLORTERM` unset, so every
    /// theme colour goes through [`quantize_256`]. A quantiser that collapses
    /// neighbouring roles onto one index makes text disappear into its own
    /// background, so no built-in theme may lose the separation between the
    /// background and anything drawn on top of it.
    #[test]
    fn no_builtin_theme_loses_a_role_against_its_background_in_256_colours() {
        for (name, content) in BUILTIN_THEMES {
            let t = parse_theme(content, Path::new("builtin")).unwrap();
            let c = resolve_theme(t, false, false).unwrap().colors;
            for (role, color) in [
                ("foreground", c.foreground),
                ("dim", c.dim),
                ("accent", c.accent),
                ("border", c.border),
                ("border_focused", c.border_focused),
                ("tree_repository", c.tree_repository),
                ("tree_worktree", c.tree_worktree),
                ("tree_shell", c.tree_shell),
                ("status_working", c.status_working),
                ("status_waiting", c.status_waiting),
                ("status_blocked", c.status_blocked),
                ("status_idle", c.status_idle),
                ("status_failed", c.status_failed),
                ("status_dead", c.status_dead),
                ("status_ended", c.status_ended),
                ("worktree_merged", c.worktree_merged),
                ("shortcut", c.shortcut),
                ("selection_bg", c.selection_bg),
            ] {
                // `ansi.toml` leaves roles on the terminal default, which is
                // one shade by definition — nothing to quantise there.
                if color == Color::Reset {
                    continue;
                }
                assert_ne!(
                    color, c.background,
                    "{name}: {role} quantises onto the background"
                );
            }
        }
    }

    #[test]
    fn all_builtin_themes_parse() {
        for (name, content) in BUILTIN_THEMES {
            let t = parse_theme(content, Path::new("builtin")).unwrap();
            assert_eq!(t.name, name);
            // every role resolves without panicking
            let resolved = resolve_theme(t, true, false).unwrap();
            let _ = resolved.colors.background;
        }
    }

    /// NO_COLOR must wipe every role to the terminal default — a monochrome
    /// render, not a low-colour one. The UI stays readable because meaning
    /// is carried by symbols/labels/position, not by colour.
    #[test]
    fn no_color_resolves_every_role_to_reset() {
        for (_, content) in BUILTIN_THEMES {
            let t = parse_theme(content, Path::new("builtin")).unwrap();
            let resolved = resolve_theme(t, true, true).unwrap();
            let c = &resolved.colors;
            for (name, role) in [
                ("background", c.background),
                ("foreground", c.foreground),
                ("dim", c.dim),
                ("accent", c.accent),
                ("selection_bg", c.selection_bg),
                ("selection_fg", c.selection_fg),
                ("border", c.border),
                ("border_focused", c.border_focused),
                ("tree_repository", c.tree_repository),
                ("tree_worktree", c.tree_worktree),
                ("tree_shell", c.tree_shell),
                ("status_working", c.status_working),
                ("status_waiting", c.status_waiting),
                ("status_blocked", c.status_blocked),
                ("status_idle", c.status_idle),
                ("status_failed", c.status_failed),
                ("status_dead", c.status_dead),
                ("status_ended", c.status_ended),
                ("worktree_merged", c.worktree_merged),
                ("shortcut", c.shortcut),
                ("title_bg", c.title_bg),
                ("title_fg", c.title_fg),
                ("popup_bg", c.popup_bg),
                ("popup_fg", c.popup_fg),
                ("popup_border", c.popup_border),
            ] {
                assert_eq!(
                    role,
                    Color::Reset,
                    "role {name} must be Reset under NO_COLOR"
                );
            }
        }
    }

    #[test]
    fn builtins_copied_to_user_dir() {
        crate::util::testutil::with_fresh_xdg(|| {
            ensure_builtin_themes().unwrap();
            for (name, _) in BUILTIN_THEMES {
                let p = crate::util::themes_dir().join(format!("{name}.toml"));
                assert!(p.exists(), "theme {name} should be copied");
            }
            // idempotent
            ensure_builtin_themes().unwrap();
        });
    }

    #[test]
    fn user_theme_overrides_builtin() {
        crate::util::testutil::with_fresh_xdg(|| {
            ensure_builtin_themes().unwrap();
            let dir = crate::util::themes_dir();
            std::fs::write(
                dir.join("dracula.toml"),
                include_str!("../themes/ansi.toml")
                    .replace("name = \"ansi\"", "name = \"dracula\""),
            )
            .unwrap();
            let t = load("dracula", true).unwrap();
            // background of ansi theme is "default" → Reset
            assert_eq!(t.colors.background, Color::Reset);
        });
    }

    #[test]
    fn unknown_theme_errors() {
        assert!(load("nope", true).is_err());
    }

    // ---- M8.4c — Catppuccin set + theme picker listing ----

    /// The reference set ships all four Catppuccin flavours with the
    /// official hex values (theming guideline: never hand-tune a
    /// Catppuccin colour), and mocha is the default.
    #[test]
    fn catppuccin_set_ships_with_official_hexes() {
        crate::util::testutil::with_fresh_xdg(|| {
            // base + text + the accent swatch per flavour, straight from
            // catppuccin.com: peach on the dark flavours, where lavender is
            // too close to text to read as an accent, and lavender on latte,
            // whose text is a dark grey it separates from cleanly.
            let cases: [(&str, u32, u32, u32, &str); 4] = [
                ("catppuccin-latte", 0xeff1f5, 0x4c4f69, 0x7287fd, "lavender"),
                ("catppuccin-frappe", 0x303446, 0xc6d0f5, 0xef9f76, "peach"),
                (
                    "catppuccin-macchiato",
                    0x24273a,
                    0xcad3f5,
                    0xf5a97f,
                    "peach",
                ),
                ("catppuccin-mocha", 0x1e1e2e, 0xcdd6f4, 0xfab387, "peach"),
            ];
            for (name, base, text, accent, swatch) in cases {
                let t = load(name, true).unwrap();
                assert_eq!(
                    spec_hex(&t, |c| c.background),
                    base,
                    "{name} background must be the official base"
                );
                assert_eq!(
                    spec_hex(&t, |c| c.foreground),
                    text,
                    "{name} foreground must be the official text"
                );
                assert_eq!(
                    spec_hex(&t, |c| c.accent),
                    accent,
                    "{name} accent must be the official {swatch}"
                );
            }
        });
        assert_eq!(DEFAULT_THEME, "catppuccin-mocha");
    }

    /// A helper: re-resolve a resolved theme's role to its hex value.
    fn spec_hex(t: &Theme, pick: fn(&ResolvedColors) -> Color) -> u32 {
        let c = pick(&t.colors);
        match c {
            Color::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
            other => panic!("expected an RGB colour, got {other:?}"),
        }
    }

    /// An accent that reads like body text is not an accent. `accent` is
    /// printed directly beside `foreground` in every shortcut hint
    /// (`<action> <key>`), so the eye has to separate the two at a glance,
    /// on one or two characters, with no space of its own.
    ///
    /// CIELAB ΔE is the measure that matches what the eye does here. A
    /// luminance ratio does not: gruvbox's yellow accent on its cream text
    /// rates 1.24 and is unmistakable, while Catppuccin's lavender on its
    /// text rates 1.24 too and is invisible — the difference is hue, which
    /// only ΔE sees. Below ~25 a short key stops standing out; 30 is the
    /// floor, with room to spare.
    ///
    /// A theme whose roles are ANSI names (`ansi`) carries no measurable
    /// value — the terminal decides what `cyan` is — and is skipped.
    #[test]
    fn every_builtin_accent_is_distinguishable_from_its_foreground() {
        /// The smallest ΔE at which a one-or-two-character key still reads
        /// as a different colour from the word beside it.
        const MIN_DELTA_E: f64 = 30.0;

        for (name, content) in BUILTIN_THEMES {
            let file = parse_theme(content, Path::new(&format!("<builtin:{name}>")))
                .unwrap_or_else(|e| panic!("theme {name}: {e}"));
            let (Some(accent), Some(text)) = (
                rgb(file
                    .colors
                    .shortcut
                    .unwrap_or(file.colors.accent)
                    .resolve(true)),
                rgb(file.colors.foreground.resolve(true)),
            ) else {
                continue;
            };
            let d = delta_e(accent, text);
            assert!(
                d >= MIN_DELTA_E,
                "{name}: accent is ΔE {d:.1} from foreground, under the {MIN_DELTA_E} floor — \
                 the shortcut keys will wash into their descriptions",
            );
        }
    }

    /// The sRGB components of a colour, or `None` for a colour the terminal
    /// defines (ANSI names, `default`), which has no value to measure.
    fn rgb(c: Color) -> Option<(f64, f64, f64)> {
        match c {
            Color::Rgb(r, g, b) => Some((r as f64, g as f64, b as f64)),
            _ => None,
        }
    }

    /// CIE76 colour difference between two sRGB colours, via CIE XYZ and
    /// CIELAB with a D65 white point. CIE76 is the crude one of the ΔE
    /// family, but the threshold here is coarse and the inputs are
    /// saturated UI colours, where its weaknesses (near-neutral blues) do
    /// not bite.
    fn delta_e(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
        let (l1, a1, b1) = lab(a);
        let (l2, a2, b2) = lab(b);
        ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
    }

    /// sRGB (0-255) to CIELAB, D65.
    fn lab(c: (f64, f64, f64)) -> (f64, f64, f64) {
        // Undo the sRGB transfer function, then the XYZ matrix.
        let lin = |v: f64| {
            let v = v / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, b) = (lin(c.0), lin(c.1), lin(c.2));
        let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
        let f = |t: f64| {
            if t > 0.008856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        let (fx, fy, fz) = (f(x), f(y), f(z));
        (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
    }

    // ---- the borrowed status colour roles ----

    /// `working`, `idle` and `dead` are the states a user cannot act on,
    /// so no shipped theme spends a colour on them — they borrow, and a
    /// theme cannot drift away from what they are supposed to mean.
    /// Reached hermetically through `BUILTIN_THEMES`/`parse_theme`/
    /// `resolve_theme` rather than `load`, which would consult the user's
    /// themes dir first and could pick up a shadowing user file.
    #[test]
    fn shipped_themes_leave_the_borrowed_status_roles_unset() {
        for (name, content) in BUILTIN_THEMES {
            let file = parse_theme(content, Path::new(&format!("<builtin:{name}>")))
                .unwrap_or_else(|e| panic!("theme {name}: {e}"));
            let c = file.colors;
            for (role, set) in [
                ("status_working", c.status_working.is_some()),
                ("status_idle", c.status_idle.is_some()),
                ("status_dead", c.status_dead.is_some()),
                ("worktree_merged", c.worktree_merged.is_some()),
            ] {
                assert!(!set, "theme {name} must let {role} borrow its base colour");
            }
        }
    }

    /// The three states that want the user carry three different hues in
    /// every shipped theme: yellow (waiting), orange (blocked), red
    /// (failed) — and `ended` a purple that reads as "nothing to do here",
    /// not as ordinary text. A row's colour alone must answer "does this
    /// need me?", so no two of them may collapse onto one value.
    #[test]
    fn shipped_themes_give_blocked_and_ended_their_own_hue() {
        for (name, content) in BUILTIN_THEMES {
            let file = parse_theme(content, Path::new(&format!("<builtin:{name}>")))
                .unwrap_or_else(|e| panic!("theme {name}: {e}"));
            assert!(
                file.colors.status_blocked.is_some(),
                "theme {name} must set status_blocked"
            );
            assert!(
                file.colors.status_ended.is_some(),
                "theme {name} must set status_ended"
            );
            // Both the truecolor and the 256-index rendering must keep them
            // apart: quantising is where two near hues collapse.
            for truecolor in [true, false] {
                let file = parse_theme(content, Path::new(&format!("<builtin:{name}>"))).unwrap();
                let c = resolve_theme(file, truecolor, false).unwrap().colors;
                for (a, b, what) in [
                    (c.status_blocked, c.status_waiting, "blocked vs waiting"),
                    (c.status_blocked, c.status_failed, "blocked vs failed"),
                    (c.status_ended, c.foreground, "ended vs foreground"),
                    (c.status_ended, c.status_waiting, "ended vs waiting"),
                ] {
                    assert_ne!(a, b, "{name} (truecolor={truecolor}): {what}");
                }
            }
        }
    }

    /// The borrowed roles resolve to the base colour they stand for:
    /// working reads as ordinary text, idle/dead recede. A user theme that
    /// omits `blocked` and `ended` too still loads and still says something
    /// sane — blocked shares waiting's colour and is told apart by its
    /// icon, ended reads as ordinary text.
    #[test]
    fn borrowed_status_roles_resolve_to_their_base_colour() {
        for (name, content) in BUILTIN_THEMES {
            let file = parse_theme(content, Path::new(&format!("<builtin:{name}>")))
                .unwrap_or_else(|e| panic!("theme {name}: {e}"));
            let c = resolve_theme(file, true, false).unwrap().colors;
            assert_eq!(c.status_working, c.foreground, "{name}: working");
            assert_eq!(c.status_idle, c.dim, "{name}: idle");
            assert_eq!(c.status_dead, c.dim, "{name}: dead");
            assert_eq!(
                c.worktree_merged, c.tree_worktree,
                "{name}: merged borrows the worktree colour, not plain text"
            );
            assert_ne!(
                c.worktree_merged, c.foreground,
                "{name}: a merged worktree must not read as ordinary text"
            );
        }
        // A minimal theme sets neither: the fallbacks answer for it.
        let bare = std::fs::read_to_string("themes/ansi.toml")
            .expect("read ansi.toml")
            .lines()
            .filter(|l| !l.starts_with("status_blocked") && !l.starts_with("status_ended"))
            .collect::<Vec<_>>()
            .join("\n");
        let c = resolve_theme(
            parse_theme(&bare, Path::new("ansi.toml")).unwrap(),
            true,
            false,
        )
        .unwrap()
        .colors;
        assert_eq!(c.status_blocked, c.status_waiting, "borrowed blocked");
        assert_eq!(c.status_ended, c.foreground, "borrowed ended");
    }

    /// A theme that does set the borrowed roles — every theme written
    /// before the split did — must keep loading, and its own values win.
    #[test]
    fn a_theme_setting_the_borrowed_roles_still_loads() {
        let src = std::fs::read_to_string("themes/ansi.toml")
            .expect("read ansi.toml")
            .lines()
            .filter(|l| !l.starts_with("status_blocked") && !l.starts_with("status_ended"))
            .collect::<Vec<_>>()
            .join("\n");
        let older = format!(
            "{src}\nstatus_working = \"green\"\nstatus_blocked = \"bright_yellow\"\n\
             status_idle = \"bright_black\"\nstatus_dead = \"bright_black\"\n\
             status_ended = \"magenta\"\n"
        );
        let parsed = parse_theme(&older, Path::new("ansi.toml"))
            .expect("a theme setting every status role must still parse");
        assert!(parsed.colors.status_working.is_some());
        let c = resolve_theme(parsed, true, false).unwrap().colors;
        assert_eq!(c.status_working, Color::Green, "an explicit role wins");
        assert_eq!(c.status_ended, Color::Magenta);
        assert_ne!(
            c.status_blocked, c.status_waiting,
            "an explicit blocked is not overridden by the fallback"
        );
    }

    /// The picker list: built-ins first in the stable order, then user
    /// themes sorted; a user theme shadowing a built-in appears once.
    #[test]
    fn available_names_list_builtins_first_then_user_themes() {
        crate::util::testutil::with_fresh_xdg(|| {
            ensure_builtin_themes().unwrap();
            let dir = crate::util::themes_dir();
            std::fs::write(dir.join("mytheme.toml"), "name = \"mytheme\"\n").unwrap();
            std::fs::write(dir.join("ztheme.toml"), "name = \"ztheme\"\n").unwrap();
            // a user file shadowing a built-in name must not be duplicated
            std::fs::write(dir.join("ansi.toml"), include_str!("../themes/ansi.toml")).unwrap();
            let names = available_names().unwrap();
            assert_eq!(
                &names[0..4],
                &[
                    "catppuccin-mocha",
                    "catppuccin-macchiato",
                    "catppuccin-frappe",
                    "catppuccin-latte"
                ],
                "catppuccin set leads the list, stable order"
            );
            assert!(
                names.contains(&"ansi".to_string())
                    && names.iter().filter(|n| n.as_str() == "ansi").count() == 1,
                "shadowed built-in listed once"
            );
            let my = names.iter().position(|n| n == "mytheme").unwrap();
            let z = names.iter().position(|n| n == "ztheme").unwrap();
            let ansi = names.iter().position(|n| n == "ansi").unwrap();
            assert!(
                ansi < my && my < z,
                "user themes come after built-ins, sorted: {names:?}"
            );
        });
    }
}
