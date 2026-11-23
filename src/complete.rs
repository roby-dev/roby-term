use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    History,
    Directory,
    File,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    /// Text displayed in the suggestions popup.
    pub display: String,
    /// Full command-line text when this completion is accepted.
    pub replacement: String,
    /// Short right-aligned metadata tag (e.g. `DIR`, `FILE`, `CMD`, `PS7 4x`).
    pub meta: String,
    pub kind: CompletionKind,
}

/// Lightweight cache of executable names found in `PATH` (~30-60 KB RAM).
pub struct PathCommandIndex {
    commands: Vec<String>,
}

impl PathCommandIndex {
    pub fn scan_system_path() -> Self {
        let mut set = BTreeSet::new();

        // Common cross-platform & PowerShell/CMD built-ins
        for builtin in [
            "cd", "ls", "dir", "pwd", "cls", "clear", "echo", "cat", "type", "mkdir", "rmdir",
            "rm", "del", "cp", "copy", "mv", "move", "exit", "history", "export", "set", "env",
        ] {
            set.insert(builtin.to_string());
        }

        if let Some(path_var) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path_var) {
                let Ok(read_dir) = fs::read_dir(dir) else {
                    continue;
                };
                for entry in read_dir.flatten().take(300) {
                    let path = entry.path();
                    if !path.is_file() {
                        continue;
                    }
                    if let Some(name) = executable_command_name(&path) {
                        set.insert(name);
                    }
                }
            }
        }

        Self {
            commands: set.into_iter().collect(),
        }
    }

    pub fn from_list(list: Vec<String>) -> Self {
        Self { commands: list }
    }

    pub fn match_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        if prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        let p_lower = prefix.to_lowercase();
        let mut out = Vec::new();
        for cmd in &self.commands {
            let c_lower = cmd.to_lowercase();
            if c_lower.starts_with(&p_lower) && c_lower != p_lower {
                out.push(cmd.clone());
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    }
}

fn executable_command_name(path: &Path) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if matches!(ext.as_str(), "exe" | "cmd" | "bat" | "ps1" | "com") {
            return Some(path.file_stem()?.to_string_lossy().to_string());
        }
        None
    }
    #[cfg(not(target_os = "windows"))]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = path.metadata().ok()?;
        if meta.permissions().mode() & 0o111 != 0 {
            return Some(path.file_name()?.to_string_lossy().to_string());
        }
        None
    }
}

/// Computes filesystem path and/or `PATH` command completions for the token before the cursor.
pub fn complete_input(
    input: &str,
    cwd: &Path,
    cmd_index: &PathCommandIndex,
    limit: usize,
) -> Vec<CompletionItem> {
    if input.is_empty() || limit == 0 {
        return Vec::new();
    }

    let (token_start, token, is_first_word) = split_last_token(input);
    let prefix_before = &input[..token_start];

    let looks_like_path = !is_first_word
        || token.contains('/')
        || token.contains('\\')
        || token.starts_with('.')
        || token.starts_with('~');

    if looks_like_path {
        complete_path(prefix_before, token, cwd, limit)
    } else {
        // First word without path separator: include matching folders/files in `cwd`
        // AND matching commands from `PATH`, so typing e.g. `Tak` + Tab finds `TakanaCore\`.
        let mut results = complete_path(prefix_before, token, cwd, limit);
        if results.len() < limit {
            let remaining = limit - results.len();
            for cmd in cmd_index.match_prefix(token, remaining) {
                let replacement = format!("{prefix_before}{cmd}");
                if !results.iter().any(|r| r.replacement.eq_ignore_ascii_case(&replacement)) {
                    results.push(CompletionItem {
                        display: cmd,
                        replacement,
                        meta: "CMD".to_string(),
                        kind: CompletionKind::Command,
                    });
                }
            }
        }
        results.truncate(limit);
        results
    }
}

/// Returns true if the active token is a path/argument (so path completions should take priority).
pub fn is_completing_path(input: &str) -> bool {
    if input.is_empty() {
        return false;
    }
    let (_, token, is_first_word) = split_last_token(input);
    !is_first_word
        || token.contains('/')
        || token.contains('\\')
        || token.starts_with('.')
        || token.starts_with('~')
}

/// Computes the longest common prefix across candidate replacements (for classic Tab completion).
pub fn longest_common_prefix(items: &[CompletionItem]) -> Option<String> {
    let first = &items.first()?.replacement;
    let mut prefix: Vec<char> = first.chars().collect();
    for item in &items[1..] {
        let chars: Vec<char> = item.replacement.chars().collect();
        let mut shared = 0;
        for (a, b) in prefix.iter().zip(chars.iter()) {
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            let eq = a.to_ascii_lowercase() == b.to_ascii_lowercase();
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let eq = a == b;
            if eq {
                shared += 1;
            } else {
                break;
            }
        }
        prefix.truncate(shared);
        if prefix.is_empty() {
            return None;
        }
    }
    Some(prefix.into_iter().collect())
}

fn split_last_token(input: &str) -> (usize, &str, bool) {
    let bytes = input.as_bytes();
    let mut in_quote: Option<u8> = None;
    let mut last_token_start = 0;
    let mut word_count = 0;
    let mut in_word = false;

    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = in_quote {
            if b == q {
                in_quote = None;
            }
            continue;
        }
        if b == b'"' || b == b'\'' {
            if !in_word {
                last_token_start = i;
                word_count += 1;
                in_word = true;
            }
            in_quote = Some(b);
        } else if b.is_ascii_whitespace() {
            in_word = false;
        } else if !in_word {
            last_token_start = i;
            word_count += 1;
            in_word = true;
        }
    }

    if !in_word {
        // Cursor is right after trailing space -> starting a new argument token
        (input.len(), "", word_count == 0)
    } else {
        (last_token_start, &input[last_token_start..], word_count <= 1)
    }
}

fn complete_path(
    prefix_before: &str,
    raw_token: &str,
    cwd: &Path,
    limit: usize,
) -> Vec<CompletionItem> {
    let quoted = raw_token.starts_with('"') || raw_token.starts_with('\'');
    let unquoted = raw_token.trim_start_matches(['"', '\'']);

    let sep_pos = unquoted.rfind(['/', '\\']);
    let (dir_prefix, name_prefix) = match sep_pos {
        Some(pos) => (&unquoted[..=pos], &unquoted[pos + 1..]),
        None => ("", unquoted),
    };

    let search_dir = resolve_search_dir(dir_prefix, cwd);
    let Ok(read_dir) = fs::read_dir(&search_dir) else {
        return Vec::new();
    };

    let name_lower = name_prefix.to_lowercase();
    let sep_char = if dir_prefix.contains('/') {
        '/'
    } else if dir_prefix.contains('\\') {
        '\\'
    } else {
        std::path::MAIN_SEPARATOR
    };

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in read_dir.flatten() {
        let fname = entry.file_name().to_string_lossy().to_string();
        if !name_prefix.is_empty() && !fname.to_lowercase().starts_with(&name_lower) {
            continue;
        }
        // Skip hidden dotfiles unless explicitly prefixed with '.'
        if name_prefix.is_empty() && fname.starts_with('.') {
            continue;
        }

        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let mut completed = format!("{dir_prefix}{fname}");
        if is_dir {
            completed.push(sep_char);
        }

        let formatted_token = if quoted || completed.contains(' ') {
            format!("\"{completed}\"")
        } else {
            completed.clone()
        };

        let item = CompletionItem {
            display: if is_dir {
                format!("{fname}{sep_char}")
            } else {
                fname
            },
            replacement: format!("{prefix_before}{formatted_token}"),
            meta: if is_dir { "DIR" } else { "FILE" }.to_string(),
            kind: if is_dir {
                CompletionKind::Directory
            } else {
                CompletionKind::File
            },
        };

        if is_dir {
            dirs.push(item);
        } else {
            files.push(item);
        }
    }

    dirs.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
    files.sort_by(|a, b| a.display.to_lowercase().cmp(&b.display.to_lowercase()));
    dirs.extend(files);
    dirs.truncate(limit);
    dirs
}

fn resolve_search_dir(dir_prefix: &str, cwd: &Path) -> PathBuf {
    if dir_prefix.is_empty() {
        return cwd.to_path_buf();
    }
    if let Some(rest) = dir_prefix.strip_prefix("~/").or_else(|| dir_prefix.strip_prefix("~\\")) {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    let p = PathBuf::from(dir_prefix);
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

/// Strips Windows verbatim path prefixes (`\\?\D:\...` -> `D:\...`, `\\?\UNC\...` -> `\\...`)
/// so PowerShell and CMD receive normal drive paths instead of `FileSystem::\\?\...`.
pub fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let s = path.to_string_lossy();
        if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

/// Updates tracked session `cwd` when the user executes `cd <dir>`, `cd..`, `Set-Location`, etc.
pub fn maybe_update_cwd(current_cwd: &Path, submitted_cmd: &str) -> Option<PathBuf> {
    let trimmed = submitted_cmd.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Handle `cd..`, `cd\`, `cd/` without space (common on Windows CMD/PowerShell)
    let lower = trimmed.to_ascii_lowercase();
    let target_raw = if lower == "cd.." {
        ".."
    } else if lower == "cd\\" || lower == "cd/" {
        "\\"
    } else if lower == "cd" {
        "~"
    } else {
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let cmd = parts.next()?.to_ascii_lowercase();
        let rest = parts.next()?.trim();
        if matches!(cmd.as_str(), "cd" | "chdir" | "set-location" | "sl" | "pushd") {
            rest
        } else {
            return None;
        }
    };

    let target = target_raw.trim().trim_matches(['"', '\'']);
    if target.is_empty() || target == "~" {
        return home_dir().filter(|p| p.is_dir());
    }
    let candidate = if let Some(rest) = target
        .strip_prefix("~/")
        .or_else(|| target.strip_prefix("~\\"))
    {
        home_dir()?.join(rest)
    } else {
        let p = PathBuf::from(target);
        if p.is_absolute() {
            p
        } else {
            current_cwd.join(p)
        }
    };
    fs::canonicalize(&candidate)
        .ok()
        .filter(|p| p.is_dir())
        .map(strip_verbatim_prefix)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Detects the current Git branch by reading `.git/HEAD` directly (zero subprocesses, 0ms).
pub fn detect_git_branch(cwd: &Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(cur) = dir {
        let dot_git = cur.join(".git");
        if dot_git.is_dir() {
            return read_git_head(&dot_git.join("HEAD"));
        }
        if dot_git.is_file() {
            // Worktree or submodule: `.git` contains `gitdir: <path>`
            let content = fs::read_to_string(&dot_git).ok()?;
            let raw = content.trim().strip_prefix("gitdir:")?.trim();
            let gitdir = if Path::new(raw).is_absolute() {
                PathBuf::from(raw)
            } else {
                cur.join(raw)
            };
            return read_git_head(&gitdir.join("HEAD"));
        }
        dir = cur.parent();
    }
    None
}

fn read_git_head(head_path: &Path) -> Option<String> {
    let head = fs::read_to_string(head_path).ok()?;
    let trimmed = head.trim();
    if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
        Some(branch.to_string())
    } else if let Some(rest) = trimmed.strip_prefix("ref: ") {
        Some(rest.to_string())
    } else if trimmed.len() >= 7 {
        Some(trimmed[..7].to_string())
    } else {
        None
    }
}

/// Formats `cwd` cleanly for the header (strips Windows `\\?\` prefix and abbreviates home dir).
pub fn abbreviate_path(cwd: &Path) -> String {
    let raw = cwd.to_string_lossy();
    let cleaned = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    if let Some(home) = home_dir() {
        let home_str = home.to_string_lossy();
        if let Some(rest) = cleaned.strip_prefix(home_str.as_ref()) {
            return if rest.is_empty() {
                "~".to_string()
            } else {
                format!("~{rest}")
            };
        }
    }
    cleaned.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_files_and_directories_from_cwd() {
        let dir = std::env::temp_dir().join("my_warp_complete_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("Cargo.toml"), "").unwrap();
        fs::write(dir.join("Cargo.lock"), "").unwrap();

        let idx = PathCommandIndex::from_list(vec!["cargo".into(), "cat".into()]);

        let dir_hits = complete_input("cd s", &dir, &idx, 5);
        assert_eq!(dir_hits.len(), 1);
        assert_eq!(dir_hits[0].kind, CompletionKind::Directory);
        assert!(dir_hits[0].replacement.starts_with("cd src"));

        let file_hits = complete_input("cat Car", &dir, &idx, 5);
        assert_eq!(file_hits.len(), 2);
        let lcp = longest_common_prefix(&file_hits).unwrap();
        assert_eq!(lcp, "cat Cargo.");

        let cmd_hits = complete_input("ca", &dir, &idx, 5);
        assert!(cmd_hits.iter().any(|h| h.kind == CompletionKind::Command));
        assert!(cmd_hits.iter().any(|h| h.kind == CompletionKind::File));

        // Typing a folder prefix as the first word (e.g. `Tak` -> `TakanaCore\`) must also match!
        fs::create_dir_all(dir.join("TakanaCore")).unwrap();
        let tak_hits = complete_input("Tak", &dir, &idx, 5);
        assert_eq!(tak_hits.len(), 1);
        assert_eq!(tak_hits[0].kind, CompletionKind::Directory);
        assert!(tak_hits[0].display.starts_with("TakanaCore"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tracks_cd_for_session_cwd() {
        let dir = std::env::temp_dir().join("my_warp_cd_test");
        let sub = dir.join("nested");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&sub).unwrap();

        let updated = maybe_update_cwd(&dir, "cd nested").expect("should resolve nested dir");
        assert_eq!(updated, strip_verbatim_prefix(fs::canonicalize(&sub).unwrap()));
        assert!(!updated.to_string_lossy().starts_with(r"\\?\"));

        let back = maybe_update_cwd(&updated, "cd..").expect("should support cd.. without space");
        assert_eq!(back, strip_verbatim_prefix(fs::canonicalize(&dir).unwrap()));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_git_branch_from_dot_git_head_without_spawning_git() {
        let dir = std::env::temp_dir().join("my_warp_git_head_test");
        let sub = dir.join("nested").join("deep");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.join(".git").join("HEAD"), "ref: refs/heads/feat/awesome\n").unwrap();

        assert_eq!(detect_git_branch(&sub).as_deref(), Some("feat/awesome"));

        let _ = fs::remove_dir_all(&dir);
    }
}
