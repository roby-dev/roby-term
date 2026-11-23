use crate::theme::{shell_accent_color, Rgb};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShellKind {
    PowerShell7,
    WindowsPowerShell,
    Cmd,
    GitBash,
    Wsl,
    Zsh,
    Bash,
    Fish,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProfile {
    pub kind: ShellKind,
    pub name: String,
    pub badge: String,
    pub program: String,
    pub args: Vec<String>,
    pub accent: Rgb,
}

impl ShellProfile {
    pub fn new(
        kind: ShellKind,
        name: impl Into<String>,
        badge: impl Into<String>,
        program: impl Into<String>,
        args: Vec<String>,
    ) -> Self {
        Self {
            kind,
            name: name.into(),
            badge: badge.into(),
            program: program.into(),
            args,
            accent: shell_accent_color(kind),
        }
    }
}

/// Finds an executable in `PATH` without spawning a subprocess.
fn find_in_path(exe: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Discovers available shells on the current operating system.
pub fn discover_shells() -> Vec<ShellProfile> {
    #[cfg(target_os = "windows")]
    {
        discover_windows_shells()
    }
    #[cfg(target_os = "macos")]
    {
        discover_macos_shells()
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        discover_unix_shells()
    }
}

#[cfg(target_os = "windows")]
fn discover_windows_shells() -> Vec<ShellProfile> {
    let mut shells = Vec::new();

    if let Some(pwsh) = find_in_path("pwsh.exe") {
        shells.push(ShellProfile::new(
            ShellKind::PowerShell7,
            "PowerShell 7",
            "PS7",
            pwsh.to_string_lossy(),
            vec!["-NoLogo".to_string()],
        ));
    }

    if let Some(ps) = find_in_path("powershell.exe") {
        shells.push(ShellProfile::new(
            ShellKind::WindowsPowerShell,
            "PowerShell",
            "PS",
            ps.to_string_lossy(),
            vec!["-NoLogo".to_string()],
        ));
    }

    if let Some(cmd) = find_in_path("cmd.exe") {
        shells.push(ShellProfile::new(
            ShellKind::Cmd,
            "Command Prompt",
            "CMD",
            cmd.to_string_lossy(),
            vec![],
        ));
    }

    // Check common Git Bash locations on Windows.
    let git_bash_candidates = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ];
    for candidate in git_bash_candidates {
        if Path::new(candidate).is_file() {
            shells.push(ShellProfile::new(
                ShellKind::GitBash,
                "Git Bash",
                "SH",
                candidate,
                vec!["--login".to_string(), "-i".to_string()],
            ));
            break;
        }
    }

    if let Some(wsl) = find_in_path("wsl.exe") {
        shells.push(ShellProfile::new(
            ShellKind::Wsl,
            "WSL",
            "WSL",
            wsl.to_string_lossy(),
            vec![],
        ));
    }

    if shells.is_empty() {
        shells.push(ShellProfile::new(
            ShellKind::Cmd,
            "Command Prompt",
            "CMD",
            "cmd.exe",
            vec![],
        ));
    }

    shells
}

#[cfg(target_os = "macos")]
fn discover_macos_shells() -> Vec<ShellProfile> {
    let mut shells = Vec::new();

    if Path::new("/bin/zsh").is_file() {
        shells.push(ShellProfile::new(
            ShellKind::Zsh,
            "Zsh",
            "ZSH",
            "/bin/zsh",
            vec!["-l".to_string()],
        ));
    }

    if Path::new("/bin/bash").is_file() {
        shells.push(ShellProfile::new(
            ShellKind::Bash,
            "Bash",
            "BASH",
            "/bin/bash",
            vec!["-l".to_string()],
        ));
    }

    for fish_path in ["/opt/homebrew/bin/fish", "/usr/local/bin/fish"] {
        if Path::new(fish_path).is_file() {
            shells.push(ShellProfile::new(
                ShellKind::Fish,
                "Fish",
                "FISH",
                fish_path,
                vec!["-l".to_string()],
            ));
            break;
        }
    }

    if let Some(pwsh) = find_in_path("pwsh") {
        shells.push(ShellProfile::new(
            ShellKind::PowerShell7,
            "PowerShell",
            "PS",
            pwsh.to_string_lossy(),
            vec!["-NoLogo".to_string()],
        ));
    }

    if shells.is_empty() {
        let fallback = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        shells.push(ShellProfile::new(
            ShellKind::Custom,
            "Shell",
            "SH",
            fallback,
            vec![],
        ));
    }

    shells
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
fn discover_unix_shells() -> Vec<ShellProfile> {
    let mut shells = Vec::new();
    for (kind, name, badge, bin) in [
        (ShellKind::Zsh, "Zsh", "ZSH", "zsh"),
        (ShellKind::Bash, "Bash", "BASH", "bash"),
        (ShellKind::Fish, "Fish", "FISH", "fish"),
    ] {
        if let Some(path) = find_in_path(bin) {
            shells.push(ShellProfile::new(kind, name, badge, path.to_string_lossy(), vec![]));
        }
    }
    if shells.is_empty() {
        shells.push(ShellProfile::new(
            ShellKind::Custom,
            "Shell",
            "SH",
            "/bin/sh",
            vec![],
        ));
    }
    shells
}

pub fn default_shell(shells: &[ShellProfile]) -> ShellProfile {
    shells.first().cloned().unwrap_or_else(|| {
        #[cfg(target_os = "windows")]
        {
            ShellProfile::new(ShellKind::Cmd, "Command Prompt", "CMD", "cmd.exe", vec![])
        }
        #[cfg(not(target_os = "windows"))]
        {
            ShellProfile::new(ShellKind::Zsh, "Zsh", "ZSH", "/bin/zsh", vec![])
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_at_least_one_shell_on_current_os() {
        let shells = discover_shells();
        assert!(!shells.is_empty());
        let def = default_shell(&shells);
        assert!(!def.program.is_empty());
        assert!(!def.badge.is_empty());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_discovers_powershell_or_cmd() {
        let shells = discover_shells();
        let has_win_shell = shells.iter().any(|s| {
            matches!(
                s.kind,
                ShellKind::PowerShell7 | ShellKind::WindowsPowerShell | ShellKind::Cmd
            )
        });
        assert!(has_win_shell);
    }
}
