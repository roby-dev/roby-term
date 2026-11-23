use crate::theme::Rgb;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_FONT_SIZE: f32 = 15.0;
pub const MIN_FONT_SIZE: f32 = 11.0;
pub const MAX_FONT_SIZE: f32 = 26.0;

pub const DEFAULT_BG_TERMINAL: Rgb = Rgb::new(17, 19, 26);
pub const DEFAULT_SYNTAX_COMMAND: Rgb = Rgb::new(137, 220, 235);
pub const DEFAULT_SYNTAX_FLAG: Rgb = Rgb::new(250, 179, 135);

/// Curated accent swatches shown in the Settings panel (includes #A88BD6).
pub const PRESET_ACCENTS: &[(&str, Rgb)] = &[
    ("Violet", Rgb::new(0xA8, 0x8B, 0xD6)),
    ("Sky", Rgb::new(0x56, 0xB6, 0xF5)),
    ("Mint", Rgb::new(0xA6, 0xE3, 0xA1)),
    ("Peach", Rgb::new(0xFA, 0xB3, 0x87)),
    ("Rose", Rgb::new(0xF3, 0x8B, 0xA8)),
    ("Teal", Rgb::new(0x94, 0xE2, 0xD5)),
    ("Gold", Rgb::new(0xF9, 0xE2, 0xAF)),
    ("Indigo", Rgb::new(0x89, 0xB4, 0xFA)),
];

/// Parses `#RRGGBB`, `RRGGBB`, `#RGB`, or `RGB` into an `Rgb` color.
pub fn parse_hex_color(input: &str) -> Option<Rgb> {
    let s = input.trim().trim_start_matches('#');
    match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some(Rgb::new(r, g, b))
        }
        3 => {
            let r = u8::from_str_radix(&s[0..1], 16).ok()? * 17;
            let g = u8::from_str_radix(&s[1..2], 16).ok()? * 17;
            let b = u8::from_str_radix(&s[2..3], 16).ok()? * 17;
            Some(Rgb::new(r, g, b))
        }
        _ => None,
    }
}

pub fn format_hex_color(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

/// Persistent user preferences stored locally in `config.ini` (zero dependencies).
#[derive(Debug, Clone, PartialEq)]
pub struct UserConfig {
    pub path: Option<PathBuf>,
    pub accent: Option<Rgb>,
    pub bg_terminal: Rgb,
    pub syntax_command: Rgb,
    pub syntax_flag: Rgb,
    pub font_name: Option<String>,
    pub font_size: f32,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            path: None,
            accent: None,
            bg_terminal: DEFAULT_BG_TERMINAL,
            syntax_command: DEFAULT_SYNTAX_COMMAND,
            syntax_flag: DEFAULT_SYNTAX_FLAG,
            font_name: None,
            font_size: DEFAULT_FONT_SIZE,
        }
    }
}

impl UserConfig {
    pub fn load_default() -> Self {
        match default_config_path() {
            Some(path) => Self::load_from_path(path),
            None => Self::default(),
        }
    }

    pub fn load_from_path(path: PathBuf) -> Self {
        let mut cfg = Self {
            path: Some(path.clone()),
            ..Self::default()
        };
        let Ok(contents) = fs::read_to_string(&path) else {
            return cfg;
        };
        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let key = k.trim();
            let val = v.trim();
            match key {
                "accent" => {
                    if val.is_empty() || val.eq_ignore_ascii_case("auto") {
                        cfg.accent = None;
                    } else if let Some(c) = parse_hex_color(val) {
                        cfg.accent = Some(c);
                    }
                }
                "bg_terminal" => {
                    if let Some(c) = parse_hex_color(val) {
                        cfg.bg_terminal = c;
                    }
                }
                "syntax_command" => {
                    if let Some(c) = parse_hex_color(val) {
                        cfg.syntax_command = c;
                    }
                }
                "syntax_flag" => {
                    if let Some(c) = parse_hex_color(val) {
                        cfg.syntax_flag = c;
                    }
                }
                "font_name" => {
                    if val.is_empty() || val.eq_ignore_ascii_case("auto") {
                        cfg.font_name = None;
                    } else {
                        cfg.font_name = Some(val.to_string());
                    }
                }
                "font_size" => {
                    if let Ok(sz) = val.parse::<f32>() {
                        cfg.font_size = sz.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
                    }
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn reset_to_defaults(&mut self) {
        let path = self.path.clone();
        *self = Self {
            path,
            ..Self::default()
        };
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(ref path) = self.path else {
            return Ok(());
        };
        save_to_path(self, path)
    }
}

fn save_to_path(cfg: &UserConfig, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let accent_str = cfg
        .accent
        .map(format_hex_color)
        .unwrap_or_else(|| "auto".to_string());
    let font_str = cfg.font_name.as_deref().unwrap_or("auto");
    let body = format!(
        "# my-warp user preferences\naccent={}\nbg_terminal={}\nsyntax_command={}\nsyntax_flag={}\nfont_name={}\nfont_size={:.1}\n",
        accent_str,
        format_hex_color(cfg.bg_terminal),
        format_hex_color(cfg.syntax_command),
        format_hex_color(cfg.syntax_flag),
        font_str,
        cfg.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE),
    );
    fs::write(path, body)
}

pub fn default_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(|p| PathBuf::from(p).join("my-warp").join("config.ini"))
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|p| PathBuf::from(p).join(".my-warp").join("config.ini"))
            })
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p)
                .join("Library")
                .join("Application Support")
                .join("my-warp")
                .join("config.ini")
        })
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p)
                .join(".config")
                .join("my-warp")
                .join("config.ini")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_colors_with_and_without_hash() {
        assert_eq!(parse_hex_color("#a88BD6"), Some(Rgb::new(0xA8, 0x8B, 0xD6)));
        assert_eq!(parse_hex_color("A88BD6"), Some(Rgb::new(0xA8, 0x8B, 0xD6)));
        assert_eq!(parse_hex_color("#f0a"), Some(Rgb::new(0xFF, 0x00, 0xAA)));
        assert_eq!(parse_hex_color("not-hex"), None);
        assert_eq!(format_hex_color(Rgb::new(0xA8, 0x8B, 0xD6)), "#A88BD6");
    }

    #[test]
    fn saves_and_reloads_user_config() {
        let dir = std::env::temp_dir().join("my_warp_cfg_test");
        let path = dir.join("config.ini");
        let _ = fs::remove_file(&path);

        let mut cfg = UserConfig::load_from_path(path.clone());
        cfg.accent = parse_hex_color("#a88BD6");
        cfg.bg_terminal = Rgb::new(12, 14, 20);
        cfg.font_name = Some("Consolas".to_string());
        cfg.font_size = 17.0;
        cfg.save().expect("save config");

        let reloaded = UserConfig::load_from_path(path.clone());
        assert_eq!(reloaded.accent, Some(Rgb::new(0xA8, 0x8B, 0xD6)));
        assert_eq!(reloaded.bg_terminal, Rgb::new(12, 14, 20));
        assert_eq!(reloaded.font_name.as_deref(), Some("Consolas"));
        assert_eq!(reloaded.font_size, 17.0);

        let _ = fs::remove_dir_all(&dir);
    }
}
