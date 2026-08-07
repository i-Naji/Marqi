use super::App;
use crossterm::event::{KeyCode, KeyEvent};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

pub struct Diagnostics {
    items: Vec<Diagnostic>,
    selected: usize,
}

#[derive(Clone)]
struct Diagnostic {
    line: usize,
    message: String,
}

pub struct DiagnosticItem {
    pub line: usize,
    pub message: String,
    pub selected: bool,
}

impl App {
    pub(super) fn open_diagnostics(&mut self) {
        self.palette = None;
        let items = diagnose(&self.buffer.rope().to_string());
        if items.is_empty() {
            self.status = Some("No Markdown diagnostics".to_string());
            return;
        }
        self.diagnostics = Some(Diagnostics { items, selected: 0 });
    }

    pub fn diagnostics_open(&self) -> bool {
        self.diagnostics.is_some()
    }

    pub fn diagnostic_items(&self) -> Vec<DiagnosticItem> {
        let Some(diagnostics) = self.diagnostics.as_ref() else {
            return Vec::new();
        };
        diagnostics
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| DiagnosticItem {
                line: item.line,
                message: item.message.clone(),
                selected: index == diagnostics.selected,
            })
            .collect()
    }

    pub(super) fn handle_diagnostics_key(&mut self, key: KeyEvent) {
        let Some(mut diagnostics) = self.diagnostics.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Up => diagnostics.selected = diagnostics.selected.saturating_sub(1),
            KeyCode::Down => {
                diagnostics.selected =
                    (diagnostics.selected + 1).min(diagnostics.items.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                if let Some(item) = diagnostics.items.get(diagnostics.selected) {
                    if self.mode == super::Mode::Read {
                        self.leave_preview();
                    }
                    self.cursor.byte = self.buffer.rope().line_to_byte(item.line);
                    self.selection_anchor = None;
                    self.follow_cursor = true;
                    self.ensure_layout();
                    self.cursor.sync_goal(&self.layout);
                }
                return;
            }
            _ => {}
        }
        self.diagnostics = Some(diagnostics);
    }
}

fn diagnose(source: &str) -> Vec<Diagnostic> {
    let lines: Vec<&str> = source.lines().collect();
    let fenced = fenced_lines(&lines);
    let mut items = fence_diagnostics(&lines);
    items.extend(reference_diagnostics(&lines, &fenced));
    items.extend(table_diagnostics(&lines, &fenced));
    items.extend(heading_diagnostics(&lines, &fenced));
    items.sort_by_key(|item| item.line);
    items
}

fn fence_marker(line: &str) -> Option<(char, usize, bool)> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let count = trimmed.chars().take_while(|ch| *ch == marker).count();
    (count >= 3).then_some((marker, count, trimmed[count..].trim().is_empty()))
}

fn fenced_lines(lines: &[&str]) -> Vec<bool> {
    let mut mask = vec![false; lines.len()];
    let mut open: Option<(char, usize)> = None;
    for (line, source) in lines.iter().enumerate() {
        if let Some((marker, count, closing)) = fence_marker(source) {
            if let Some((open_marker, open_count)) = open {
                mask[line] = true;
                if closing && marker == open_marker && count >= open_count {
                    open = None;
                }
            } else {
                open = Some((marker, count));
                mask[line] = true;
            }
        } else if open.is_some() {
            mask[line] = true;
        }
    }
    mask
}

fn fence_diagnostics(lines: &[&str]) -> Vec<Diagnostic> {
    let mut open: Option<(char, usize, usize)> = None;
    for (line, source) in lines.iter().enumerate() {
        let Some((marker, count, closing)) = fence_marker(source) else {
            continue;
        };
        if let Some((open_marker, open_count, _)) = open {
            if closing && marker == open_marker && count >= open_count {
                open = None;
            }
        } else {
            open = Some((marker, count, line));
        }
    }
    open.map_or_else(Vec::new, |(_, _, line)| {
        vec![Diagnostic {
            line,
            message: "Unmatched code fence".to_string(),
        }]
    })
}

fn normalize_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn reference_diagnostics(lines: &[&str], fenced: &[bool]) -> Vec<Diagnostic> {
    static DEFINITION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\[([^]]+)\]:").unwrap());
    static REFERENCE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[([^]]+)\]\[([^]]*)\]").unwrap());
    static FOOTNOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\^([^]]+)\]").unwrap());
    let definitions: HashSet<String> = lines
        .iter()
        .enumerate()
        .filter(|(line, _)| !fenced[*line])
        .filter_map(|(_, source)| DEFINITION.captures(source))
        .map(|capture| normalize_label(&capture[1]))
        .collect();
    let mut items = Vec::new();
    for (line, source) in lines.iter().enumerate().filter(|(line, _)| !fenced[*line]) {
        if DEFINITION.is_match(source) {
            continue;
        }
        for capture in REFERENCE.captures_iter(source) {
            let target = if capture[2].is_empty() {
                &capture[1]
            } else {
                &capture[2]
            };
            if !definitions.contains(&normalize_label(target)) {
                items.push(Diagnostic {
                    line,
                    message: format!("Broken reference: [{target}]"),
                });
            }
        }
        for capture in FOOTNOTE.captures_iter(source) {
            let target = format!("^{}", &capture[1]);
            if !definitions.contains(&normalize_label(&target)) {
                items.push(Diagnostic {
                    line,
                    message: format!("Broken footnote: [^{0}]", &capture[1]),
                });
            }
        }
    }
    items
}

fn table_diagnostics(lines: &[&str], fenced: &[bool]) -> Vec<Diagnostic> {
    let mut items = Vec::new();
    let mut line = 0;
    while line + 1 < lines.len() {
        if fenced[line] || !pipe_line(lines[line]) || !pipe_line(lines[line + 1]) {
            line += 1;
            continue;
        }
        let start = line;
        let mut end = line + 1;
        while end + 1 < lines.len() && !fenced[end + 1] && pipe_line(lines[end + 1]) {
            end += 1;
        }
        let header_columns = cells(lines[start]).len();
        if !separator_line(lines[start + 1]) {
            items.push(Diagnostic {
                line: start + 1,
                message: "Malformed table separator".to_string(),
            });
        } else {
            for (row, source) in lines.iter().enumerate().take(end + 1).skip(start + 1) {
                if cells(source).len() != header_columns {
                    items.push(Diagnostic {
                        line: row,
                        message: "Malformed table: inconsistent column count".to_string(),
                    });
                }
            }
        }
        line = end + 1;
    }
    items
}

fn pipe_line(line: &str) -> bool {
    !pipe_positions(line.trim()).is_empty()
}

fn separator_line(line: &str) -> bool {
    let cells = cells(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let core = cell.trim().trim_matches(':');
            core.len() >= 3 && core.chars().all(|ch| ch == '-')
        })
}

fn cells(line: &str) -> Vec<&str> {
    let trimmed = line.trim();
    let pipes = pipe_positions(trimmed);
    let leading = pipes.first() == Some(&0);
    let trailing = pipes.last() == Some(&trimmed.len().saturating_sub(1)) && trimmed.len() > 1;
    let start = usize::from(leading);
    let end = trimmed.len().saturating_sub(usize::from(trailing));
    let inner = &trimmed[start..end];
    let mut cells = Vec::new();
    let mut cell_start = 0;
    for pipe in pipe_positions(inner) {
        cells.push(&inner[cell_start..pipe]);
        cell_start = pipe + 1;
    }
    cells.push(&inner[cell_start..]);
    cells
}

fn pipe_positions(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    bytes
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| {
            if *byte != b'|' {
                return None;
            }
            let slashes = bytes[..index]
                .iter()
                .rev()
                .take_while(|byte| **byte == b'\\')
                .count();
            slashes.is_multiple_of(2).then_some(index)
        })
        .collect()
}

fn heading_diagnostics(lines: &[&str], fenced: &[bool]) -> Vec<Diagnostic> {
    let mut seen = HashMap::new();
    let mut items = Vec::new();
    for line in 0..lines.len() {
        if fenced[line] {
            continue;
        }
        let Some(title) = heading_title(lines, fenced, line) else {
            continue;
        };
        let key = title.to_lowercase();
        if let Some(first) = seen.insert(key, line) {
            items.push(Diagnostic {
                line,
                message: format!("Duplicate heading (first at line {})", first + 1),
            });
        }
    }
    items
}

fn heading_title<'a>(lines: &'a [&str], fenced: &[bool], line: usize) -> Option<&'a str> {
    let trimmed = lines[line].trim();
    let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if (1..=6).contains(&hashes)
        && trimmed
            .as_bytes()
            .get(hashes)
            .is_some_and(u8::is_ascii_whitespace)
    {
        return Some(trimmed[hashes..].trim().trim_end_matches('#').trim());
    }
    let underline = lines.get(line + 1)?.trim();
    if fenced.get(line + 1).copied().unwrap_or(false)
        || trimmed.is_empty()
        || trimmed.contains('|')
        || underline.len() < 3
    {
        return None;
    }
    let marker = underline.as_bytes()[0];
    (matches!(marker, b'=' | b'-') && underline.bytes().all(|byte| byte == marker))
        .then_some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::diagnose;

    #[test]
    fn reports_supported_markdown_problems() {
        let source =
            "# Same\n# Same\n\n[link][missing]\n\n| A | B |\n| -- | nope |\n\n```rust\ncode\n";
        let items = diagnose(source);
        assert!(items.iter().any(|item| item.message.contains("Duplicate")));
        assert!(
            items
                .iter()
                .any(|item| item.message.contains("Broken reference"))
        );
        assert!(
            items
                .iter()
                .any(|item| item.message.contains("Malformed table"))
        );
        assert!(items.iter().any(|item| item.message.contains("Unmatched")));
    }

    #[test]
    fn tolerates_bare_pipe_lines() {
        diagnose("|\n| x |\n");
        diagnose("|\n|\n");
        diagnose("  |  \n| a | b |\n");
    }
}
