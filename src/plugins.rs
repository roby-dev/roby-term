use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const LOOKBACK_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
}

impl TokenTotals {
    pub fn total(&self) -> u64 {
        self.input.saturating_add(self.output)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentUsage {
    pub agy: Option<TokenTotals>,
    pub claude: Option<TokenTotals>,
    pub codex: Option<TokenTotals>,
}

impl AgentUsage {
    pub fn any_enabled(&self) -> bool {
        self.agy.is_some() || self.claude.is_some() || self.codex.is_some()
    }
}

/// Formats token counts compactly (`950`, `42.3k`, `1.8M`).
pub fn format_compact_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

/// Collects local 24h token usage for whichever CLI agent plugins are enabled.
/// Only reads `.jsonl` files modified within the last 24 hours so it finishes in a few ms.
pub fn collect_today_usage(enable_agy: bool, enable_claude: bool, enable_codex: bool) -> AgentUsage {
    let cutoff = SystemTime::now()
        .checked_sub(LOOKBACK_WINDOW)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let Some(home) = home_dir() else {
        return AgentUsage::default();
    };

    let agy = if enable_agy {
        let dir = home.join(".gemini").join("antigravity-cli").join("brain");
        Some(scan_dir_for_tokens(&dir, cutoff, "transcript.jsonl", 4))
    } else {
        None
    };

    let claude = if enable_claude {
        let dir = home.join(".claude").join("projects");
        Some(scan_dir_for_tokens(&dir, cutoff, ".jsonl", 3))
    } else {
        None
    };

    let codex = if enable_codex {
        let dir = home.join(".codex").join("sessions");
        Some(scan_dir_for_tokens(&dir, cutoff, ".jsonl", 4))
    } else {
        None
    };

    AgentUsage { agy, claude, codex }
}

fn scan_dir_for_tokens(
    dir: &Path,
    cutoff: SystemTime,
    suffix: &str,
    max_depth: usize,
) -> TokenTotals {
    let mut totals = TokenTotals::default();
    let mut files = Vec::new();
    collect_recent_files(dir, cutoff, suffix, max_depth, &mut files);
    for path in files {
        let file_totals = scan_jsonl_file(&path);
        totals.input = totals.input.saturating_add(file_totals.input);
        totals.output = totals.output.saturating_add(file_totals.output);
    }
    totals
}

fn collect_recent_files(
    dir: &Path,
    cutoff: SystemTime,
    suffix: &str,
    depth: usize,
    out: &mut Vec<PathBuf>,
) {
    if depth == 0 || !dir.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            collect_recent_files(&path, cutoff, suffix, depth - 1, out);
        } else if meta.is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(suffix) {
                continue;
            }
            if let Ok(modified) = meta.modified() {
                if modified >= cutoff {
                    out.push(path);
                }
            }
        }
    }
}

fn scan_jsonl_file(path: &Path) -> TokenTotals {
    let Ok(file) = File::open(path) else {
        return TokenTotals::default();
    };
    let reader = BufReader::new(file);
    let mut totals = TokenTotals::default();
    for line in reader.lines().map_while(Result::ok) {
        let t = parse_line_tokens(&line);
        totals.input = totals.input.saturating_add(t.input);
        totals.output = totals.output.saturating_add(t.output);
    }
    totals
}

/// Fast zero-allocation scanner for `"input_tokens":<u64>` and `"output_tokens":<u64>` fields.
pub fn parse_line_tokens(line: &str) -> TokenTotals {
    // Quick bailout for lines that don't mention tokens at all
    if !line.contains("_tokens") {
        return TokenTotals::default();
    }
    let input = extract_u64_after(line, "\"input_tokens\":").unwrap_or(0);
    let output = extract_u64_after(line, "\"output_tokens\":").unwrap_or(0);
    TokenTotals { input, output }
}

fn extract_u64_after(haystack: &str, key: &str) -> Option<u64> {
    let idx = haystack.find(key)?;
    let rest = haystack[idx + key.len()..].trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u64>().ok()
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agy_claude_and_codex_token_lines() {
        let agy = r#"{"step_index":1,"model":"gemini-4-neon-medium","input_tokens":27498,"cache_read_tokens":0,"output_tokens":954}"#;
        let t_agy = parse_line_tokens(agy);
        assert_eq!(t_agy, TokenTotals { input: 27498, output: 954 });

        let claude = r#"{"usage":{"input_tokens":1200,"cache_read_input_tokens":500,"output_tokens":340}}"#;
        let t_claude = parse_line_tokens(claude);
        assert_eq!(t_claude, TokenTotals { input: 1200, output: 340 });

        let codex = r#"{"type":"token_usage_record","payload":{"input_tokens": 8000, "output_tokens": 450}}"#;
        let t_codex = parse_line_tokens(codex);
        assert_eq!(t_codex, TokenTotals { input: 8000, output: 450 });

        assert_eq!(format_compact_tokens(950), "950");
        assert_eq!(format_compact_tokens(28_452), "28.5k");
        assert_eq!(format_compact_tokens(1_460_000), "1.5M");
    }
}
