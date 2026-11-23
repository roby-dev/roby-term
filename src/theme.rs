use crate::config::UserConfig;
use crate::input::TokenKind;
use crate::shell::ShellKind;

/// Compact 3-byte RGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Packs RGB into 0x00RRGGBB format expected by `softbuffer`.
    #[inline]
    pub const fn to_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// Alpha-blends `self` over `dst` with alpha in 0..=255.
    #[inline]
    pub fn blend_over(self, dst: u32, alpha: u8) -> u32 {
        if alpha == 0 {
            return dst;
        }
        if alpha == 255 {
            return self.to_u32();
        }
        let a = alpha as u32;
        let inv = 255 - a;
        let dr = (dst >> 16) & 0xFF;
        let dg = (dst >> 8) & 0xFF;
        let db = dst & 0xFF;

        let r = (self.r as u32 * a + dr * inv) / 255;
        let g = (self.g as u32 * a + dg * inv) / 255;
        let b = (self.b as u32 * a + db * inv) / 255;
        (r << 16) | (g << 8) | b
    }

    /// Linearly interpolates between `self` and `other` (`t` in 0..=255).
    #[inline]
    pub fn lerp(self, other: Rgb, t: u8) -> Rgb {
        let t = t as u16;
        let inv = 255 - t;
        Rgb {
            r: ((self.r as u16 * inv + other.r as u16 * t) / 255) as u8,
            g: ((self.g as u16 * inv + other.g as u16 * t) / 255) as u8,
            b: ((self.b as u16 * inv + other.b as u16 * t) / 255) as u8,
        }
    }
}

/// Detected OS user identity used to personalize colors and badges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserIdentity {
    pub username: String,
    pub hostname: String,
    pub accent: Rgb,
    pub badge_bg: Rgb,
}

impl UserIdentity {
    pub fn detect() -> Self {
        let username = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "user".to_string());
        let hostname = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "local".to_string());
        Self::from_parts(username, hostname)
    }

    pub fn from_parts(username: impl Into<String>, hostname: impl Into<String>) -> Self {
        let username = username.into();
        let hostname = hostname.into();
        let accent = user_accent_color(&username);
        let badge_bg = accent.lerp(Rgb::new(18, 20, 28), 195);
        Self {
            username,
            hostname,
            accent,
            badge_bg,
        }
    }

    pub fn with_accent_override(&self, accent_override: Option<Rgb>) -> Self {
        let accent = accent_override.unwrap_or_else(|| user_accent_color(&self.username));
        let badge_bg = accent.lerp(Rgb::new(18, 20, 28), 195);
        Self {
            username: self.username.clone(),
            hostname: self.hostname.clone(),
            accent,
            badge_bg,
        }
    }
}

/// Curated high-contrast accent palette for per-user terminal differentiation.
const USER_ACCENTS: [Rgb; 8] = [
    Rgb::new(86, 182, 245),  // Sky Blue
    Rgb::new(166, 227, 161), // Mint Green
    Rgb::new(250, 179, 135), // Peach Orange
    Rgb::new(203, 166, 247), // Lavender
    Rgb::new(243, 139, 168), // Rose Pink
    Rgb::new(148, 226, 213), // Teal
    Rgb::new(249, 226, 175), // Soft Gold
    Rgb::new(137, 180, 250), // Periwinkle
];

/// Deterministically maps a username to a distinct accent color (FNV-1a hash).
pub fn user_accent_color(username: &str) -> Rgb {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in username.as_bytes() {
        hash ^= b.to_ascii_lowercase() as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    USER_ACCENTS[(hash as usize) % USER_ACCENTS.len()]
}

/// Distinct accent color per shell type for instant visual recognition.
pub fn shell_accent_color(kind: ShellKind) -> Rgb {
    match kind {
        ShellKind::PowerShell7 => Rgb::new(83, 155, 245),
        ShellKind::WindowsPowerShell => Rgb::new(59, 130, 246),
        ShellKind::Cmd => Rgb::new(245, 158, 11),
        ShellKind::GitBash => Rgb::new(249, 115, 22),
        ShellKind::Wsl => Rgb::new(34, 197, 94),
        ShellKind::Zsh => Rgb::new(192, 132, 252),
        ShellKind::Bash => Rgb::new(74, 222, 128),
        ShellKind::Fish => Rgb::new(45, 212, 191),
        ShellKind::Custom => Rgb::new(148, 163, 184),
    }
}

/// Full UI & syntax theme derived from the current user's identity.
#[derive(Debug, Clone)]
pub struct Theme {
    pub bg_terminal: Rgb,
    pub bg_sidebar: Rgb,
    pub bg_sidebar_hover: Rgb,
    pub bg_sidebar_active: Rgb,
    pub bg_header: Rgb,
    pub bg_input: Rgb,
    pub border: Rgb,
    pub fg_primary: Rgb,
    pub fg_muted: Rgb,
    pub user_accent: Rgb,
    pub danger: Rgb,
    pub syntax: SyntaxPalette,
}

#[derive(Debug, Clone, Copy)]
pub struct SyntaxPalette {
    pub command: Rgb,
    pub flag: Rgb,
    pub string_literal: Rgb,
    pub path: Rgb,
    pub env_var: Rgb,
    pub operator: Rgb,
    pub argument: Rgb,
    pub comment: Rgb,
}

impl Theme {
    pub fn for_user(user: &UserIdentity) -> Self {
        Self::for_user_with_config(user, &UserConfig::default())
    }

    pub fn for_user_with_config(user: &UserIdentity, cfg: &UserConfig) -> Self {
        let base_bg = cfg.bg_terminal;
        let sidebar_bg = base_bg.lerp(Rgb::new(6, 7, 11), 65).lerp(user.accent, 10);
        Self {
            bg_terminal: base_bg,
            bg_sidebar: sidebar_bg,
            bg_sidebar_hover: base_bg.lerp(Rgb::new(45, 50, 68), 90),
            bg_sidebar_active: base_bg.lerp(user.accent, 34),
            bg_header: base_bg.lerp(Rgb::new(30, 34, 48), 55),
            bg_input: base_bg.lerp(Rgb::new(32, 36, 52), 70),
            border: base_bg.lerp(Rgb::new(90, 98, 128), 85),
            fg_primary: Rgb::new(228, 233, 245),
            fg_muted: Rgb::new(134, 144, 168),
            user_accent: user.accent,
            danger: Rgb::new(243, 139, 168),
            syntax: SyntaxPalette {
                command: cfg.syntax_command,
                flag: cfg.syntax_flag,
                string_literal: Rgb::new(166, 227, 161), // Green for "..."
                path: Rgb::new(249, 226, 175),          // Gold for paths
                env_var: Rgb::new(203, 166, 247),       // Purple for $VAR / %VAR%
                operator: Rgb::new(243, 139, 168),      // Pink for | && ; >
                argument: Rgb::new(228, 233, 245),      // Light text
                comment: Rgb::new(108, 118, 142),       // Muted gray
            },
        }
    }

    pub fn token_color(&self, kind: TokenKind) -> Rgb {
        match kind {
            TokenKind::Command => self.syntax.command,
            TokenKind::Flag => self.syntax.flag,
            TokenKind::StringLiteral => self.syntax.string_literal,
            TokenKind::Path => self.syntax.path,
            TokenKind::EnvVar => self.syntax.env_var,
            TokenKind::Operator => self.syntax.operator,
            TokenKind::Argument | TokenKind::Whitespace => self.syntax.argument,
            TokenKind::Comment => self.syntax.comment,
        }
    }
}

/// Standard 16 ANSI colors (Catppuccin-inspired dark readable palette).
const ANSI_16: [Rgb; 16] = [
    Rgb::new(30, 34, 45),    // 0 Black
    Rgb::new(243, 139, 168), // 1 Red
    Rgb::new(166, 227, 161), // 2 Green
    Rgb::new(249, 226, 175), // 3 Yellow
    Rgb::new(137, 180, 250), // 4 Blue
    Rgb::new(203, 166, 247), // 5 Magenta
    Rgb::new(148, 226, 213), // 6 Cyan
    Rgb::new(205, 214, 244), // 7 White
    Rgb::new(88, 91, 112),   // 8 Bright Black
    Rgb::new(247, 161, 186), // 9 Bright Red
    Rgb::new(186, 235, 181), // 10 Bright Green
    Rgb::new(252, 236, 198), // 11 Bright Yellow
    Rgb::new(165, 200, 252), // 12 Bright Blue
    Rgb::new(219, 191, 250), // 13 Bright Magenta
    Rgb::new(172, 235, 225), // 14 Bright Cyan
    Rgb::new(245, 247, 255), // 15 Bright White
];

/// Zero-allocation xterm 256-color palette lookup.
pub fn ansi_256_color(idx: u8) -> Rgb {
    match idx {
        0..=15 => ANSI_16[idx as usize],
        16..=231 => {
            let n = idx - 16;
            let r = n / 36;
            let g = (n % 36) / 6;
            let b = n % 6;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb::new(level(r), level(g), level(b))
        }
        232..=255 => {
            let gray = 8 + (idx - 232) * 10;
            Rgb::new(gray, gray, gray)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_color_is_deterministic_and_case_insensitive() {
        let c1 = user_accent_color("Alice");
        let c2 = user_accent_color("alice");
        assert_eq!(c1, c2);
    }

    #[test]
    fn different_users_can_produce_distinct_colors() {
        let colors: std::collections::HashSet<Rgb> = ["alice", "bob", "charlie", "root", "dev", "admin"]
            .iter()
            .map(|u| user_accent_color(u))
            .collect();
        assert!(colors.len() >= 3, "expected varied accents across usernames");
    }

    #[test]
    fn ansi_256_lookup_covers_cube_and_grayscale() {
        assert_eq!(ansi_256_color(16), Rgb::new(0, 0, 0));
        assert_eq!(ansi_256_color(231), Rgb::new(255, 255, 255));
        assert_eq!(ansi_256_color(232), Rgb::new(8, 8, 8));
        assert_eq!(ansi_256_color(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn rgb_pack_and_blend() {
        let c = Rgb::new(0x12, 0x34, 0x56);
        assert_eq!(c.to_u32(), 0x0012_3456);
        assert_eq!(c.blend_over(0x0000_0000, 0), 0x0000_0000);
        assert_eq!(c.blend_over(0x0000_0000, 255), 0x0012_3456);
    }
}
