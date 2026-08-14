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

/// Built-in themes embedded in the binary.
pub const BUILTIN_THEMES: [(&str, &str); 3] = [
    ("dracula", include_str!("../themes/dracula.toml")),
    ("gruvbox", include_str!("../themes/gruvbox.toml")),
    ("ansi", include_str!("../themes/ansi.toml")),
];

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
pub fn load(name: &str, truecolor: bool) -> Result<Theme> {
    let file = load_file(name)?;
    resolve_theme(file, truecolor)
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
        "unknown theme '{name}' (built-ins: dracula, gruvbox, ansi)"
    )))
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

fn resolve_theme(file: ThemeFile, truecolor: bool) -> Result<Theme> {
    let c = file.colors;
    Ok(Theme {
        name: file.name,
        colors: ResolvedColors {
            background: c.background.resolve(truecolor),
            foreground: c.foreground.resolve(truecolor),
            dim: c.dim.resolve(truecolor),
            accent: c.accent.resolve(truecolor),
            selection_bg: c.selection_bg.resolve(truecolor),
            selection_fg: c.selection_fg.resolve(truecolor),
            border: c.border.resolve(truecolor),
            border_focused: c.border_focused.resolve(truecolor),
            tree_project: c.tree_project.resolve(truecolor),
            tree_feature: c.tree_feature.resolve(truecolor),
            tree_shell: c.tree_shell.resolve(truecolor),
            status_working: c.status_working.resolve(truecolor),
            status_waiting: c.status_waiting.resolve(truecolor),
            status_idle: c.status_idle.resolve(truecolor),
            status_failed: c.status_failed.resolve(truecolor),
            status_dead: c.status_dead.resolve(truecolor),
            status_ended: c.status_ended.resolve(truecolor),
            title_bg: c.title_bg.resolve(truecolor),
            title_fg: c.title_fg.resolve(truecolor),
            popup_bg: c.popup_bg.resolve(truecolor),
            popup_fg: c.popup_fg.resolve(truecolor),
            popup_border: c.popup_border.resolve(truecolor),
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
            let resolved = resolve_theme(t, true).unwrap();
            let _ = resolved.colors.background;
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
}
