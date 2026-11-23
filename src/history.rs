use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum unique commands kept in RAM (~200 KB worst case).
pub const MAX_UNIQUE_COMMANDS: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub command: String,
    pub count: u32,
    pub last_used: u64,
    pub shell_badge: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub command: String,
    pub count: u32,
    pub shell_badge: String,
    pub is_prefix: bool,
    pub score: f64,
}

/// Local, append-only command history with frecency-ranked predictions.
pub struct HistoryStore {
    path: Option<PathBuf>,
    entries: Vec<HistoryEntry>,
}

impl HistoryStore {
    /// Loads history from the default per-user local data directory.
    pub fn load_default() -> Self {
        match default_history_path() {
            Some(path) => Self::load_from_path(path),
            None => Self::in_memory(),
        }
    }

    pub fn in_memory() -> Self {
        Self {
            path: None,
            entries: Vec::new(),
        }
    }

    pub fn load_from_path(path: PathBuf) -> Self {
        let mut store = Self {
            path: Some(path.clone()),
            entries: Vec::new(),
        };
        if let Ok(file) = fs::File::open(&path) {
            let reader = BufReader::new(file);
            for line in reader.lines().map_while(Result::ok) {
                if let Some((ts, badge, cmd)) = parse_log_line(&line) {
                    store.insert_in_memory(&cmd, &badge, ts);
                }
            }
        }
        store
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records a command locally (skips empty commands and leading-space secrets).
    pub fn record(&mut self, raw_cmd: &str, shell_badge: &str) {
        self.record_at(raw_cmd, shell_badge, now_unix_secs());
    }

    pub fn record_at(&mut self, raw_cmd: &str, shell_badge: &str, timestamp: u64) {
        // Honor classic `ignorespace` convention so commands starting with a space are never saved.
        if raw_cmd.starts_with(' ') || raw_cmd.starts_with('\t') {
            return;
        }
        let cmd = raw_cmd.trim();
        if cmd.is_empty() {
            return;
        }

        let sanitized = sanitize_field(cmd);
        let badge = sanitize_field(shell_badge);
        self.insert_in_memory(&sanitized, &badge, timestamp);

        if let Some(ref path) = self.path {
            let _ = append_log_line(path, timestamp, &badge, &sanitized);
        }
    }

    /// Returns recent unique commands ordered oldest -> newest (for Up/Down history seeding).
    pub fn recent_commands(&self, limit: usize) -> Vec<String> {
        let mut sorted: Vec<&HistoryEntry> = self.entries.iter().collect();
        sorted.sort_by_key(|e| e.last_used);
        let start = sorted.len().saturating_sub(limit);
        sorted[start..].iter().map(|e| e.command.clone()).collect()
    }

    /// Returns up to `limit` frecency-ranked suggestions matching `query`.
    pub fn suggest(&self, query: &str, shell_badge: &str, limit: usize) -> Vec<Suggestion> {
        self.suggest_at(query, shell_badge, limit, now_unix_secs())
    }

    /// Multi-token fuzzy history search for `Ctrl+R` modal.
    /// Empty query returns the most recently used commands; non-empty matches all space-separated tokens.
    pub fn search_fuzzy(&self, query: &str, limit: usize) -> Vec<Suggestion> {
        if limit == 0 {
            return Vec::new();
        }
        let now = now_unix_secs();
        let tokens: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .collect();

        let mut matches: Vec<Suggestion> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let lower = entry.command.to_lowercase();
                if !tokens.iter().all(|tok| lower.contains(tok)) {
                    return None;
                }
                let mut score = if tokens.is_empty() {
                    entry.last_used as f64
                } else {
                    frecency_score(entry.count, entry.last_used, now)
                };
                if !tokens.is_empty() && lower.starts_with(&tokens[0]) {
                    score += 300.0;
                }
                Some(Suggestion {
                    command: entry.command.clone(),
                    count: entry.count,
                    shell_badge: entry.shell_badge.clone(),
                    is_prefix: !tokens.is_empty() && lower.starts_with(&tokens[0]),
                    score,
                })
            })
            .collect();

        matches.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        matches.truncate(limit);
        matches
    }

    pub fn suggest_at(
        &self,
        query: &str,
        shell_badge: &str,
        limit: usize,
        now: u64,
    ) -> Vec<Suggestion> {
        let q = query.trim_start();
        if q.is_empty() || limit == 0 {
            return Vec::new();
        }
        let q_lower = q.to_lowercase();
        let mut matches = Vec::new();

        for entry in &self.entries {
            let cmd_lower = entry.command.to_lowercase();
            if cmd_lower == q_lower {
                continue;
            }
            let is_prefix = cmd_lower.starts_with(&q_lower);
            if !is_prefix && !cmd_lower.contains(&q_lower) {
                continue;
            }

            let mut score = frecency_score(entry.count, entry.last_used, now);
            if is_prefix {
                score += 1000.0;
            }
            if !shell_badge.is_empty() && entry.shell_badge.eq_ignore_ascii_case(shell_badge) {
                score += 150.0;
            }

            matches.push(Suggestion {
                command: entry.command.clone(),
                count: entry.count,
                shell_badge: entry.shell_badge.clone(),
                is_prefix,
                score,
            });
        }

        matches.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.command.len().cmp(&b.command.len()))
        });
        matches.truncate(limit);
        matches
    }

    /// Returns the inline ghost-text suffix for the top prefix match, if any.
    pub fn ghost_suffix(&self, query: &str, shell_badge: &str) -> Option<String> {
        if query.is_empty() {
            return None;
        }
        let q_lower = query.to_lowercase();
        let now = now_unix_secs();

        let best = self
            .entries
            .iter()
            .filter(|e| {
                let lower = e.command.to_lowercase();
                lower.starts_with(&q_lower) && lower.len() > q_lower.len()
            })
            .max_by(|a, b| {
                let mut sa = frecency_score(a.count, a.last_used, now);
                let mut sb = frecency_score(b.count, b.last_used, now);
                if a.shell_badge.eq_ignore_ascii_case(shell_badge) {
                    sa += 150.0;
                }
                if b.shell_badge.eq_ignore_ascii_case(shell_badge) {
                    sb += 150.0;
                }
                sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
            })?;

        let skip_chars = query.chars().count();
        let suffix: String = best.command.chars().skip(skip_chars).collect();
        if suffix.is_empty() {
            None
        } else {
            Some(suffix)
        }
    }

    fn insert_in_memory(&mut self, cmd: &str, badge: &str, ts: u64) {
        if let Some(existing) = self.entries.iter_mut().find(|e| e.command == cmd) {
            existing.count = existing.count.saturating_add(1);
            if ts >= existing.last_used {
                existing.last_used = ts;
                if !badge.is_empty() {
                    existing.shell_badge = badge.to_string();
                }
            }
            return;
        }

        if self.entries.len() >= MAX_UNIQUE_COMMANDS {
            let now = ts.max(now_unix_secs());
            if let Some((worst_idx, _)) = self
                .entries
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    frecency_score(a.count, a.last_used, now)
                        .partial_cmp(&frecency_score(b.count, b.last_used, now))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            {
                self.entries.swap_remove(worst_idx);
            }
        }

        self.entries.push(HistoryEntry {
            command: cmd.to_string(),
            count: 1,
            last_used: ts,
            shell_badge: badge.to_string(),
        });
    }
}

fn frecency_score(count: u32, last_used: u64, now: u64) -> f64 {
    let freq = (count as f64).ln_1p() * 45.0;
    let age_secs = now.saturating_sub(last_used) as f64;
    let age_days = age_secs / 86_400.0;
    let recency = 120.0 / (1.0 + age_days);
    freq + recency
}

fn sanitize_field(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\t' || c == '\r' || c == '\n' { ' ' } else { c })
        .collect()
}

fn parse_log_line(line: &str) -> Option<(u64, String, String)> {
    let mut parts = line.splitn(3, '\t');
    let ts: u64 = parts.next()?.trim().parse().ok()?;
    let badge = parts.next()?.trim().to_string();
    let cmd = parts.next()?.trim().to_string();
    if cmd.is_empty() {
        return None;
    }
    Some((ts, badge, cmd))
}

fn append_log_line(path: &Path, ts: u64, badge: &str, cmd: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{ts}\t{badge}\t{cmd}")?;
    Ok(())
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn default_history_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(|p| PathBuf::from(p).join("my-warp").join("history.log"))
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|p| PathBuf::from(p).join(".my-warp").join("history.log"))
            })
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p)
                .join("Library")
                .join("Application Support")
                .join("my-warp")
                .join("history.log")
        })
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        std::env::var_os("HOME").map(|p| {
            PathBuf::from(p)
                .join(".local")
                .join("share")
                .join("my-warp")
                .join("history.log")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_and_reloads_history_locally() {
        let dir = std::env::temp_dir().join(format!("my_warp_hist_test_{}", now_unix_secs()));
        let path = dir.join("history.log");
        let _ = fs::remove_file(&path);

        {
            let mut store = HistoryStore::load_from_path(path.clone());
            store.record_at("cargo test", "PS7", 1_000);
            store.record_at("cargo build --release", "PS7", 1_010);
            store.record_at("cargo test", "PS7", 1_020);
            // Leading space must be ignored (privacy / secrets)
            store.record_at(" export SECRET=123", "PS7", 1_030);
        }

        let reloaded = HistoryStore::load_from_path(path.clone());
        assert_eq!(reloaded.len(), 2);
        let suggestions = reloaded.suggest_at("cargo", "PS7", 5, 1_050);
        assert_eq!(suggestions.len(), 2);
        // "cargo test" was used twice and more recently -> ranked #1
        assert_eq!(suggestions[0].command, "cargo test");
        assert_eq!(suggestions[0].count, 2);
        assert!(suggestions[0].is_prefix);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ranks_prefix_over_substring_and_computes_ghost_suffix() {
        let mut store = HistoryStore::in_memory();
        store.record_at("echo git status", "PS7", 2_000);
        store.record_at("git status -sb", "PS7", 2_000);
        store.record_at("git commit -m \"init\"", "PS7", 2_005);

        let suggestions = store.suggest_at("git", "PS7", 5, 2_010);
        assert_eq!(suggestions.len(), 3);
        assert!(suggestions[0].is_prefix);
        assert!(suggestions[1].is_prefix);
        assert!(!suggestions[2].is_prefix);

        let ghost = store.ghost_suffix("git com", "PS7");
        assert_eq!(ghost.as_deref(), Some("mit -m \"init\""));

        // Multi-token fuzzy search for Ctrl+R
        store.record_at("cargo build --release", "PS7", 2_020);
        let fuzzy = store.search_fuzzy("cargo rel", 5);
        assert_eq!(fuzzy.len(), 1);
        assert_eq!(fuzzy[0].command, "cargo build --release");
    }
}
