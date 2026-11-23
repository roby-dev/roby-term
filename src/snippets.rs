use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub name: String,
    pub command: String,
}

/// 100% local, zero-dependency snippet / workflow store (`snippets.tsv`).
pub struct SnippetStore {
    path: Option<PathBuf>,
    items: Vec<Snippet>,
}

impl SnippetStore {
    pub fn load_default() -> Self {
        match default_snippets_path() {
            Some(path) => Self::load_from_path(path),
            None => Self::with_defaults(None),
        }
    }

    pub fn in_memory() -> Self {
        Self::with_defaults(None)
    }

    pub fn load_from_path(path: PathBuf) -> Self {
        if !path.is_file() {
            let store = Self::with_defaults(Some(path));
            let _ = store.save();
            return store;
        }
        let mut items = Vec::new();
        if let Ok(content) = fs::read_to_string(&path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if let Some((name, cmd)) = trimmed.split_once('\t') {
                    let n = name.trim();
                    let c = cmd.trim();
                    if !n.is_empty() && !c.is_empty() {
                        items.push(Snippet {
                            name: n.to_string(),
                            command: c.to_string(),
                        });
                    }
                }
            }
        }
        Self {
            path: Some(path),
            items,
        }
    }

    fn with_defaults(path: Option<PathBuf>) -> Self {
        let mut items = vec![
            Snippet {
                name: "git-status".to_string(),
                command: "git status -sb".to_string(),
            },
            Snippet {
                name: "git-log".to_string(),
                command: "git log --oneline -n 15".to_string(),
            },
            Snippet {
                name: "cargo-test".to_string(),
                command: "cargo test".to_string(),
            },
            Snippet {
                name: "cargo-release".to_string(),
                command: "cargo build --release".to_string(),
            },
        ];
        #[cfg(target_os = "windows")]
        items.push(Snippet {
            name: "list-ports".to_string(),
            command: "netstat -ano | findstr LISTENING".to_string(),
        });
        #[cfg(not(target_os = "windows"))]
        items.push(Snippet {
            name: "list-ports".to_string(),
            command: "lsof -i -P -n | grep LISTEN".to_string(),
        });

        Self { path, items }
    }

    pub fn items(&self) -> &[Snippet] {
        &self.items
    }

    /// Filters snippets by case-insensitive substring match against name or command.
    pub fn filter(&self, query: &str) -> Vec<(usize, &Snippet)> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.items.iter().enumerate().collect();
        }
        self.items
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.name.to_lowercase().contains(&q) || s.command.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Adds or updates a snippet by name and persists to disk.
    pub fn upsert(&mut self, name: &str, command: &str) -> bool {
        let name = sanitize(name.trim());
        let command = sanitize(command.trim());
        if name.is_empty() || command.is_empty() {
            return false;
        }
        if let Some(existing) = self
            .items
            .iter_mut()
            .find(|s| s.name.eq_ignore_ascii_case(&name))
        {
            existing.command = command;
        } else {
            self.items.push(Snippet { name, command });
        }
        let _ = self.save();
        true
    }

    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        self.items.remove(index);
        let _ = self.save();
        true
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(ref path) = self.path else {
            return Ok(());
        };
        save_snippets(path, &self.items)
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\t' || c == '\r' || c == '\n' { ' ' } else { c })
        .collect()
}

fn save_snippets(path: &Path, items: &[Snippet]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = String::from("# my-warp local snippets (name<TAB>command)\n");
    for s in items {
        out.push_str(&s.name);
        out.push('\t');
        out.push_str(&s.command);
        out.push('\n');
    }
    fs::write(path, out)
}

pub fn default_snippets_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(|p| PathBuf::from(p).join("my-warp").join("snippets.tsv"))
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|p| PathBuf::from(p).join(".my-warp").join("snippets.tsv"))
            })
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p)
                .join("Library")
                .join("Application Support")
                .join("my-warp")
                .join("snippets.tsv")
        })
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p)
                .join(".local")
                .join("share")
                .join("my-warp")
                .join("snippets.tsv")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upserts_filters_persists_and_removes_snippets() {
        let dir = std::env::temp_dir().join("my_warp_snippets_test");
        let path = dir.join("snippets.tsv");
        let _ = fs::remove_dir_all(&dir);

        {
            let mut store = SnippetStore::load_from_path(path.clone());
            assert!(!store.items().is_empty());
            assert!(store.upsert("deploy-dev", "cargo build --release && echo done"));
            let hits = store.filter("deploy");
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].1.name, "deploy-dev");
        }

        let mut reloaded = SnippetStore::load_from_path(path.clone());
        let hits = reloaded.filter("deploy-dev");
        assert_eq!(hits.len(), 1);
        let idx = hits[0].0;
        assert!(reloaded.remove(idx));
        assert!(reloaded.filter("deploy-dev").is_empty());

        let _ = fs::remove_dir_all(&dir);
    }
}
