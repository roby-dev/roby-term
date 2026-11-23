use crate::complete::strip_verbatim_prefix;
use std::fs;
use std::path::{Path, PathBuf};

/// Saved metadata + transcript for a single terminal session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSession {
    pub id: usize,
    pub title: String,
    pub shell_program: String,
    pub cwd: PathBuf,
    pub transcript: String,
}

/// Complete persisted workspace snapshot (open sessions, active index, split panes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    pub active: usize,
    pub split_pair: Option<(usize, usize)>,
    pub sessions: Vec<SavedSession>,
}

/// Returns the default local directory where workspace state and transcripts are stored.
pub fn default_workspace_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(|p| PathBuf::from(p).join("my-warp").join("workspace"))
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|p| PathBuf::from(p).join(".my-warp").join("workspace"))
            })
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p)
                .join("Library")
                .join("Application Support")
                .join("my-warp")
                .join("workspace")
        })
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p)
                .join(".local")
                .join("share")
                .join("my-warp")
                .join("workspace")
        })
    }
}

/// Saves the workspace snapshot (session list, active index, split state, and transcripts) locally.
pub fn save_workspace(dir: &Path, snapshot: &WorkspaceSnapshot) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;

    // Remove stale transcript files so closed sessions don't accumulate on disk
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("session_") && name.ends_with(".txt") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    let split_str = match snapshot.split_pair {
        Some((l, r)) => format!("{l},{r}"),
        None => "none".to_string(),
    };
    let mut state = format!(
        "# my-warp workspace state\nactive={}\nsplit={}\n",
        snapshot.active, split_str
    );

    for (idx, sess) in snapshot.sessions.iter().enumerate() {
        let transcript_file = format!("session_{idx}.txt");
        fs::write(dir.join(&transcript_file), &sess.transcript)?;
        state.push_str(&format!(
            "session\t{}\t{}\t{}\t{}\t{}\n",
            sess.id,
            sanitize(&sess.title),
            sanitize(&sess.shell_program),
            sanitize(&sess.cwd.to_string_lossy()),
            transcript_file
        ));
    }

    fs::write(dir.join("state.tsv"), state)
}

/// Loads a previously saved workspace snapshot, if present and non-empty.
pub fn load_workspace(dir: &Path) -> Option<WorkspaceSnapshot> {
    let state_path = dir.join("state.tsv");
    let content = fs::read_to_string(&state_path).ok()?;

    let mut active = 0usize;
    let mut split_pair = None;
    let mut sessions = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(val) = line.strip_prefix("active=") {
            if let Ok(a) = val.trim().parse::<usize>() {
                active = a;
            }
            continue;
        }
        if let Some(val) = line.strip_prefix("split=") {
            if let Some((l, r)) = val.trim().split_once(',') {
                if let (Ok(li), Ok(ri)) = (l.parse::<usize>(), r.parse::<usize>()) {
                    split_pair = Some((li, ri));
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("session\t") {
            let parts: Vec<&str> = rest.splitn(5, '\t').collect();
            if parts.len() == 5 {
                let id = parts[0].parse::<usize>().unwrap_or(sessions.len() + 1);
                let title = parts[1].to_string();
                let shell_program = parts[2].to_string();
                let cwd = strip_verbatim_prefix(PathBuf::from(parts[3]));
                let transcript = fs::read_to_string(dir.join(parts[4])).unwrap_or_default();
                sessions.push(SavedSession {
                    id,
                    title,
                    shell_program,
                    cwd,
                    transcript,
                });
            }
        }
    }

    if sessions.is_empty() {
        return None;
    }

    if active >= sessions.len() {
        active = sessions.len() - 1;
    }
    if let Some((l, r)) = split_pair {
        if l >= sessions.len() || r >= sessions.len() || l == r {
            split_pair = None;
        }
    }

    Some(WorkspaceSnapshot {
        active,
        split_pair,
        sessions,
    })
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\t' || c == '\r' || c == '\n' { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_reloads_multiple_sessions_with_transcripts_and_split() {
        let dir = std::env::temp_dir().join("my_warp_workspace_test");
        let _ = fs::remove_dir_all(&dir);

        let snap = WorkspaceSnapshot {
            active: 2,
            split_pair: Some((1, 2)),
            sessions: vec![
                SavedSession {
                    id: 1,
                    title: "API".into(),
                    shell_program: "pwsh.exe".into(),
                    cwd: PathBuf::from(r"D:\FUENTES\api"),
                    transcript: "cargo run\nListening on :8080".into(),
                },
                SavedSession {
                    id: 2,
                    title: "Frontend".into(),
                    shell_program: "powershell.exe".into(),
                    cwd: PathBuf::from(r"D:\FUENTES\web"),
                    transcript: "npm run dev\nhttp://localhost:3000".into(),
                },
                SavedSession {
                    id: 3,
                    title: "Git".into(),
                    shell_program: "cmd.exe".into(),
                    cwd: PathBuf::from(r"D:\FUENTES\my-warp"),
                    transcript: "git status -sb\n## feat/my-warp-terminal".into(),
                },
                SavedSession {
                    id: 4,
                    title: "DB".into(),
                    shell_program: "pwsh.exe".into(),
                    cwd: PathBuf::from(r"D:\FUENTES\db"),
                    transcript: "docker ps".into(),
                },
            ],
        };

        save_workspace(&dir, &snap).expect("save workspace");
        let loaded = load_workspace(&dir).expect("load workspace");
        assert_eq!(loaded, snap);

        let _ = fs::remove_dir_all(&dir);
    }
}
