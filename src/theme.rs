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
    pub tree_project: Spec,
    pub tree_feature: Spec,
    pub tree_shell: Spec,
    pub status_working: Spec,
    pub status_waiting: Spec,
    pub status_idle: Spec,
    pub status_failed: Spec,
    pub status_dead: Spec,
    pub status_ended: Spec,
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
    pub tree_project: Color,
    pub tree_feature: Color,
    pub tree_shell: Color,
    pub status_working: Color,
    pub status_waiting: Color,
    pub status_idle: Color,
    pub status_failed: Color,
    pub status_dead: Color,
    pub status_ended: Color,
    pub title_bg: Color,
    pub title_fg: Color,
    pub popup_bg: Color,
    pub popup_fg: Color,
    pub popup_border: Color,
}

/// Whether the terminal supports 24-bit colour (`COLORTERM=truecolor|24bit`).
pub fn supports_truecolor() -> bool {
    std::env::var("COLORTERM")
        .map(|v| v.contains("truecolor") || v.contains("24bit"))
        .unwrap_or(false)
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

/// Quantise an RGB colour to the xterm-256 cube (16 + 36r + 6g + b),
/// mapping mid-greys to the 232-255 ramp. Pure black and white stay on the
/// cube (16 and 231).
fn quantize_256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b && r > 8 && r < 255 {
        return 232 + ((r as u16 * 23) / 255) as u8;
    }
    let q = |c: u8| -> u8 {
        if c < 48 {
            0
        } else if c < 115 {
            1
        } else {
            (c as u16 - 35) as u8 / 40
        }
    };
    16 + 36 * q(r) + 6 * q(g) + q(b)
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
            tree_project: resolve(c.tree_project),
            tree_feature: resolve(c.tree_feature),
            tree_shell: resolve(c.tree_shell),
            status_working: resolve(c.status_working),
            status_waiting: resolve(c.status_waiting),
            status_idle: resolve(c.status_idle),
            status_failed: resolve(c.status_failed),
            status_dead: resolve(c.status_dead),
            status_ended: resolve(c.status_ended),
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

    #[test]
    fn quantization_to_256() {
        assert_eq!(quantize_256(0, 0, 0), 16);
        assert_eq!(quantize_256(255, 255, 255), 231);
        assert_eq!(quantize_256(255, 0, 0), 196);
        assert_eq!(quantize_256(0, 255, 0), 46);
        assert_eq!(quantize_256(0, 0, 255), 21);
        // grey ramp
        let expected: u8 = 232 + (128u16 * 23 / 255) as u8;
        assert_eq!(quantize_256(128, 128, 128), expected);
        let s = Spec::parse("#bd93f9").unwrap();
        assert!(matches!(s.resolve(false), Color::Indexed(_)));
        // ansi theme ignores truecolor
        assert_eq!(Spec::Ansi(AnsiColor::Cyan).resolve(false), Color::Cyan);
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
                ("tree_project", c.tree_project),
                ("tree_feature", c.tree_feature),
                ("tree_shell", c.tree_shell),
                ("status_working", c.status_working),
                ("status_waiting", c.status_waiting),
                ("status_idle", c.status_idle),
                ("status_failed", c.status_failed),
                ("status_dead", c.status_dead),
                ("status_ended", c.status_ended),
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
            // base + text + one accent per flavour, straight from catppuccin.com
            let cases: [(&str, u32, u32, u32); 4] = [
                ("catppuccin-latte", 0xeff1f5, 0x4c4f69, 0x7287fd),
                ("catppuccin-frappe", 0x303446, 0xc6d0f5, 0xbabbf1),
                ("catppuccin-macchiato", 0x24273a, 0xcad3f5, 0xb7bdf8),
                ("catppuccin-mocha", 0x1e1e2e, 0xcdd6f4, 0xb4befe),
            ];
            for (name, base, text, accent) in cases {
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
                    "{name} accent must be the official lavender"
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
