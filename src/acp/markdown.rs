//! Lightweight streaming-safe Markdown line layout for the ACP chat panel.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdLineKind {
    Heading,
    Body,
    Bullet,
    Quote,
    Code,
    CodeHeader,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdLine {
    pub kind: MdLineKind,
    pub text: String,
}

/// Parses Markdown (including unclosed fenced code blocks while streaming) into wrapped lines.
pub fn parse_markdown(raw: &str, max_cols: usize) -> Vec<MdLine> {
    let max_cols = max_cols.max(8);
    let mut out: Vec<MdLine> = Vec::new();
    let mut in_code = false;

    for raw_line in raw.lines() {
        let trimmed_start = raw_line.trim_start();
        if let Some(fence_rest) = trimmed_start.strip_prefix("```") {
            if !in_code {
                in_code = true;
                let lang = fence_rest.trim();
                if !lang.is_empty() {
                    for chunk in wrap_text(lang, max_cols) {
                        out.push(MdLine {
                            kind: MdLineKind::CodeHeader,
                            text: chunk,
                        });
                    }
                }
            } else {
                in_code = false;
            }
            continue;
        }

        if in_code {
            if raw_line.is_empty() {
                out.push(MdLine {
                    kind: MdLineKind::Code,
                    text: String::new(),
                });
            } else {
                for chunk in wrap_text(raw_line, max_cols) {
                    out.push(MdLine {
                        kind: MdLineKind::Code,
                        text: chunk,
                    });
                }
            }
            continue;
        }

        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            if out.last().is_some_and(|l| !l.text.is_empty()) {
                out.push(MdLine {
                    kind: MdLineKind::Body,
                    text: String::new(),
                });
            }
            continue;
        }

        if let Some(heading) = strip_heading(trimmed) {
            let clean = strip_inline_emphasis(heading);
            for chunk in wrap_text(&clean, max_cols) {
                out.push(MdLine {
                    kind: MdLineKind::Heading,
                    text: chunk,
                });
            }
            continue;
        }

        if let Some(bullet) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            let clean = strip_inline_emphasis(bullet.trim_start());
            let inner_cols = max_cols.saturating_sub(2).max(6);
            let chunks = wrap_text(&clean, inner_cols);
            for (idx, chunk) in chunks.into_iter().enumerate() {
                let prefix = if idx == 0 { "• " } else { "  " };
                out.push(MdLine {
                    kind: MdLineKind::Bullet,
                    text: format!("{prefix}{chunk}"),
                });
            }
            continue;
        }

        if let Some(quote) = trimmed.strip_prefix('>') {
            let clean = strip_inline_emphasis(quote.trim_start());
            let inner_cols = max_cols.saturating_sub(2).max(6);
            for chunk in wrap_text(&clean, inner_cols) {
                out.push(MdLine {
                    kind: MdLineKind::Quote,
                    text: chunk,
                });
            }
            continue;
        }

        let clean = strip_inline_emphasis(trimmed);
        for chunk in wrap_text(&clean, max_cols) {
            out.push(MdLine {
                kind: MdLineKind::Body,
                text: chunk,
            });
        }
    }

    while out.last().is_some_and(|l| l.text.is_empty() && l.kind == MdLineKind::Body) {
        out.pop();
    }
    out
}

fn strip_heading(line: &str) -> Option<&str> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if rest.starts_with(' ') || rest.starts_with('\t') {
            return Some(rest.trim_start());
        }
    }
    None
}

fn strip_inline_emphasis(text: &str) -> String {
    text.replace("**", "").replace("__", "")
}

pub fn wrap_text(text: &str, max_cols: usize) -> Vec<String> {
    let max_cols = max_cols.max(1);
    if text.chars().count() <= max_cols {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;

    for segment in text.split_inclusive(' ') {
        let seg_chars = segment.chars().count();
        if current_chars > 0 && current_chars + seg_chars > max_cols {
            lines.push(current.trim_end().to_string());
            current.clear();
            current_chars = 0;
        }
        if seg_chars > max_cols {
            for ch in segment.chars() {
                if current_chars >= max_cols {
                    lines.push(current.trim_end().to_string());
                    current.clear();
                    current_chars = 0;
                }
                current.push(ch);
                current_chars += 1;
            }
        } else {
            current.push_str(segment);
            current_chars += seg_chars;
        }
    }

    if !current.is_empty() {
        lines.push(current.trim_end().to_string());
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headings_bullets_quotes_and_closed_code_blocks() {
        let md = "# Title\n\n- **First** item\n> quote line\n```rust\nlet x = 1;\n```\nDone";
        let lines = parse_markdown(md, 40);
        assert_eq!(
            lines,
            vec![
                MdLine { kind: MdLineKind::Heading, text: "Title".into() },
                MdLine { kind: MdLineKind::Body, text: String::new() },
                MdLine { kind: MdLineKind::Bullet, text: "• First item".into() },
                MdLine { kind: MdLineKind::Quote, text: "quote line".into() },
                MdLine { kind: MdLineKind::CodeHeader, text: "rust".into() },
                MdLine { kind: MdLineKind::Code, text: "let x = 1;".into() },
                MdLine { kind: MdLineKind::Body, text: "Done".into() },
            ]
        );
    }

    #[test]
    fn renders_unclosed_streaming_code_block_and_wraps_utf8() {
        let md = "```python\nprint('áéíóúáéíóú')";
        let lines = parse_markdown(md, 10);
        assert_eq!(lines[0], MdLine { kind: MdLineKind::CodeHeader, text: "python".into() });
        assert!(lines.len() >= 3);
        assert!(lines[1..].iter().all(|l| l.kind == MdLineKind::Code));
        let reconstructed: String = lines[1..].iter().map(|l| l.text.as_str()).collect();
        assert_eq!(reconstructed, "print('áéíóúáéíóú')");
    }
}
