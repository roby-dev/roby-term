use crate::complete::abbreviate_path;
use crate::theme::Rgb;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Intercepts `ls`, `ll`, and `la` (without shell pipes/redirections) to render a fast,
/// cross-platform, color-coded directory listing identically on Windows and macOS.
pub fn try_run_builtin(raw_cmd: &str, cwd: &Path, accent: Rgb) -> Option<String> {
    let trimmed = raw_cmd.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Let the real shell handle pipes, redirections, and chained commands.
    if trimmed.contains('|')
        || trimmed.contains('>')
        || trimmed.contains('<')
        || trimmed.contains('&')
        || trimmed.contains(';')
    {
        return None;
    }

    let args = split_args(trimmed);
    let head = args.first()?.to_ascii_lowercase();
    let (mut show_all, mut long_fmt) = match head.as_str() {
        "ls" => (false, false),
        "ll" => (false, true),
        "la" => (true, true),
        _ => return None,
    };

    let mut targets = Vec::new();
    for arg in &args[1..] {
        if arg == "--raw" {
            return None;
        }
        if arg == "--all" {
            show_all = true;
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            let mut recognized = true;
            for ch in arg[1..].chars() {
                match ch {
                    'a' | 'A' => show_all = true,
                    'l' => long_fmt = true,
                    'h' | 'F' | 'G' | '1' => {}
                    _ => {
                        recognized = false;
                        break;
                    }
                }
            }
            if recognized {
                continue;
            }
            // Unknown flag (e.g. PowerShell-specific `-Recurse`): fall back to real shell
            return None;
        }
        targets.push(arg.clone());
    }

    if targets.is_empty() {
        targets.push(".".to_string());
    }

    let mut out = String::new();
    for (ti, target_arg) in targets.iter().enumerate() {
        if ti > 0 {
            out.push_str("\r\n");
        }
        let target_path = resolve_target(target_arg, cwd);
        out.push_str(&format_ls_target(
            trimmed,
            target_arg,
            &target_path,
            show_all,
            long_fmt,
            accent,
        ));
    }
    Some(out)
}

fn format_ls_target(
    display_cmd: &str,
    target_arg: &str,
    target_path: &Path,
    show_all: bool,
    long_fmt: bool,
    accent: Rgb,
) -> String {
    let mut out = String::new();
    let muted = fg_rgb(Rgb::new(134, 144, 168));
    let accent_fg = fg_rgb(accent);
    let red_fg = fg_rgb(Rgb::new(243, 139, 168));
    let reset = "\x1b[0m";

    if !target_path.exists() {
        out.push_str(&format!(
            "\r\n{red_fg}ls: cannot access '{target_arg}': No such file or directory{reset}\r\n"
        ));
        return out;
    }

    if target_path.is_file() {
        let name = target_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| target_arg.to_string());
        let meta = target_path.metadata().ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let (tag, color) = classify_file(&name, target_path);
        out.push_str(&format!(
            "\r\n\x1b[1m{accent_fg}> {display_cmd}{reset}\r\n  {}{tag}{reset}  {:>8}  {}{name}{reset}\r\n",
            fg_rgb(color),
            format_size(size),
            fg_rgb(color),
        ));
        return out;
    }

    let Ok(read_dir) = fs::read_dir(target_path) else {
        out.push_str(&format!(
            "\r\n{red_fg}ls: permission denied reading '{target_arg}'{reset}\r\n"
        ));
        return out;
    };

    struct EntryRow {
        name: String,
        size: u64,
        modified: Option<SystemTime>,
        path: PathBuf,
    }

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !show_all && name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = if is_dir {
            0
        } else {
            meta.as_ref().map(|m| m.len()).unwrap_or(0)
        };
        let modified = meta.and_then(|m| m.modified().ok());
        let row = EntryRow {
            name,
            size,
            modified,
            path,
        };
        if is_dir {
            dirs.push(row);
        } else {
            files.push(row);
        }
    }

    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let dir_label = abbreviate_path(target_path);
    out.push_str(&format!(
        "\r\n\x1b[1m{accent_fg}> {display_cmd}{reset}  {muted}({dir_label} · {} dirs, {} files){reset}\r\n",
        dirs.len(),
        files.len()
    ));

    let sep = if cfg!(target_os = "windows") { '\\' } else { '/' };
    let now = SystemTime::now();

    for d in &dirs {
        let age = if long_fmt {
            format!("  {muted}{:>8}{reset}", format_age(d.modified, now))
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  \x1b[1m{accent_fg}DIR{reset}  {muted}       -{reset}{age}  \x1b[1m{accent_fg}{}{sep}{reset}\r\n",
            d.name
        ));
    }

    for f in &files {
        let (tag, color) = classify_file(&f.name, &f.path);
        let c_fg = fg_rgb(color);
        let is_hidden = f.name.starts_with('.');
        let name_fg = if is_hidden {
            muted.clone()
        } else {
            c_fg.clone()
        };
        let age = if long_fmt {
            format!("  {muted}{:>8}{reset}", format_age(f.modified, now))
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  {c_fg}{tag}{reset}  {muted}{:>8}{reset}{age}  {name_fg}{}{reset}\r\n",
            format_size(f.size),
            f.name
        ));
    }

    if dirs.is_empty() && files.is_empty() {
        out.push_str(&format!("  {muted}(empty directory){reset}\r\n"));
    }
    out
}

fn classify_file(name: &str, path: &Path) -> (&'static str, Rgb) {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if matches!(ext.as_str(), "exe" | "bat" | "cmd" | "ps1" | "sh" | "msi" | "app") {
        return ("EXE", Rgb::new(166, 227, 161)); // Green
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = path.metadata() {
            if m.permissions().mode() & 0o111 != 0 {
                return ("EXE", Rgb::new(166, 227, 161));
            }
        }
    }
    if matches!(
        ext.as_str(),
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "cs" | "c" | "cpp" | "h"
            | "hpp" | "html" | "css" | "scss" | "sql" | "rb" | "php" | "swift" | "kt"
    ) {
        return ("COD", Rgb::new(137, 220, 235)); // Cyan
    }
    if matches!(
        ext.as_str(),
        "toml" | "json" | "yaml" | "yml" | "ini" | "env" | "xml" | "lock" | "cfg" | "conf"
    ) || name.starts_with(".env")
        || name == ".gitignore"
    {
        return ("CFG", Rgb::new(249, 226, 175)); // Gold
    }
    if matches!(ext.as_str(), "md" | "txt" | "pdf" | "rst" | "log" | "csv") {
        return ("DOC", Rgb::new(203, 166, 247)); // Lavender
    }
    if matches!(
        ext.as_str(),
        "zip" | "tar" | "gz" | "7z" | "rar" | "png" | "jpg" | "jpeg" | "gif" | "svg" | "ico"
            | "webp" | "mp4" | "mp3"
    ) {
        return ("BIN", Rgb::new(243, 139, 168)); // Rose
    }
    ("FIL", Rgb::new(228, 233, 245)) // Primary white
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn format_age(modified: Option<SystemTime>, now: SystemTime) -> String {
    let Some(m) = modified else {
        return "-".to_string();
    };
    let Ok(dur) = now.duration_since(m) else {
        return "now".to_string();
    };
    let secs = dur.as_secs();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else if secs < 86_400 * 30 {
        format!("{}d ago", secs / 86_400)
    } else {
        let days = m
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() / 86_400)
            .unwrap_or(0);
        format!("{}mo ago", (secs / (86_400 * 30)).max(1).min(days.max(1)))
    }
}

fn fg_rgb(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.r, c.g, c.b)
}

fn resolve_target(arg: &str, cwd: &Path) -> PathBuf {
    if arg == "." {
        return cwd.to_path_buf();
    }
    if arg == "~" {
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            return PathBuf::from(home);
        }
    }
    if let Some(rest) = arg.strip_prefix("~/").or_else(|| arg.strip_prefix("~\\")) {
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            return PathBuf::from(home).join(rest);
        }
    }
    let p = PathBuf::from(arg);
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

fn split_args(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quote: Option<char> = None;

    for ch in input.chars() {
        if let Some(q) = in_quote {
            if ch == q {
                in_quote = None;
            } else {
                cur.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
        } else if ch.is_whitespace() {
            if !cur.is_empty() {
                args.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_colored_ls_with_dirs_first_and_skips_piped_ls() {
        let dir = std::env::temp_dir().join("my_warp_ls_builtin_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("alpha_dir")).unwrap();
        fs::write(dir.join("main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join(".hidden"), "secret").unwrap();

        let accent = Rgb::new(0xA8, 0x8B, 0xD6);
        let out = try_run_builtin("ls", &dir, accent).expect("should handle plain ls");
        assert!(out.contains("DIR"));
        assert!(out.contains("alpha_dir"));
        assert!(out.contains("COD"));
        assert!(out.contains("main.rs"));
        assert!(!out.contains(".hidden"));

        let out_all = try_run_builtin("ls -la", &dir, accent).expect("should handle ls -la");
        assert!(out_all.contains(".hidden"));

        // Piped commands must fall through to the real shell
        assert!(try_run_builtin("ls | findstr main", &dir, accent).is_none());

        let _ = fs::remove_dir_all(&dir);
    }
}
