use std::collections::HashSet;

use comrak::nodes::{AstNode, ListType, NodeValue};
use comrak::{Arena, parse_document};

use crate::markdown::gfm_options;

use super::{App, Motion};

struct ListMarker {
    indent: String,
    kind: ListMarkerKind,
    spacing: String,
    /// Byte length of the full marker prefix (indent + marker + checkbox +
    /// spacing). The prefix is pure ASCII by construction, so slicing the line
    /// at this offset always lands on a char boundary.
    prefix_len: usize,
}

enum ListMarkerKind {
    Bullet(char),
    /// A GFM task item (`- [ ]` / `- [x]`); new items are always unchecked.
    Task(char),
    Ordered {
        number: u64,
        delimiter: char,
    },
}

impl ListMarker {
    fn continuation_prefix(&self) -> String {
        match self.kind {
            ListMarkerKind::Bullet(marker) => {
                format!("{}{}{}", self.indent, marker, self.spacing)
            }
            ListMarkerKind::Task(marker) => {
                format!("{}{} [ ] ", self.indent, marker)
            }
            ListMarkerKind::Ordered { number, delimiter } => {
                format!(
                    "{}{}{}{}",
                    self.indent,
                    number.saturating_add(1),
                    delimiter,
                    self.spacing
                )
            }
        }
    }
}

impl App {
    pub(super) fn insert_char_smart(&mut self, c: char) {
        if let Some((s, e)) = self.selection_range()
            && let Some(close) = opening_pair(c)
        {
            let selected = self.buffer.slice(s, e);
            let mut replacement =
                String::with_capacity(c.len_utf8() + selected.len() + close.len_utf8());
            replacement.push(c);
            replacement.push_str(&selected);
            replacement.push(close);
            self.replace_range(s, e, &replacement);
            return;
        }

        // Type-over only applies without a selection; with one, a closing
        // char must replace the selected text like any other typed char.
        if self.selection_range().is_none() && self.skip_existing_closer(c) {
            return;
        }

        if let Some(close) = opening_pair(c) {
            let at = self.cursor.byte;
            let mut replacement = String::with_capacity(c.len_utf8() + close.len_utf8());
            replacement.push(c);
            replacement.push(close);
            self.replace_range_with_cursor(at, at, &replacement, Some(at + c.len_utf8()));
            return;
        }

        self.insert(&c.to_string());
    }

    pub(super) fn insert_newline_smart(&mut self) {
        let nl = self.newline();
        if self.selection_range().is_some() {
            self.insert(nl);
            return;
        }

        let rope = self.buffer.rope();
        let line = rope.byte_to_line(self.cursor.byte);
        let line_start = rope.line_to_byte(line);
        let line_text = self.line_without_eol(line);
        let line_content_end = line_start + line_text.len();

        // List handling is line-local, so a `- item` line *inside a fenced
        // code block* would otherwise grow markers it must not have.
        if self.in_fenced_code(line) {
            self.insert(nl);
            return;
        }

        // A complete list/task/number marker with the cursor at or past it.
        if let Some(marker) = parse_list_marker(&line_text)
            && self.cursor.byte >= line_start + marker.prefix_len
        {
            if line_text[marker.prefix_len..].trim().is_empty() {
                // Empty item: Enter exits the list (matching Obsidian).
                let is_ordered = matches!(marker.kind, ListMarkerKind::Ordered { .. });
                self.exit_list(line_start, line_content_end, &marker.indent);
                if is_ordered {
                    self.renumber_ordered_run();
                }
            } else {
                // Non-empty item: continue the list with a fresh marker. A
                // task continues as an empty `- [ ] `, a number increments.
                let is_ordered = matches!(marker.kind, ListMarkerKind::Ordered { .. });
                let continuation = marker.continuation_prefix();
                self.insert(&format!("{nl}{continuation}"));
                if is_ordered {
                    // Keep the source numbers sequential (matching the preview).
                    self.renumber_ordered_run();
                }
            }
            return;
        }

        // A bare, space-less stub ("-", "*", "+", "2.") with the cursor at the
        // end: the user never started an item, so Enter just drops the stub.
        if self.cursor.byte >= line_content_end && is_bare_marker(&line_text) {
            self.exit_list(line_start, line_content_end, &leading_indent(&line_text));
            return;
        }

        self.insert(nl);
    }

    /// Exit a list: drop the marker on the current line, or — in hard-break mode
    /// (`soft_break = "break"`) — replace it with a blank line so the next text
    /// starts a new paragraph instead of a lazy continuation of the list.
    fn exit_list(&mut self, line_start: usize, line_content_end: usize, indent: &str) {
        let replacement = if self.theme.hard_breaks {
            self.newline()
        } else {
            indent
        };
        self.replace_range(line_start, line_content_end, replacement);
    }

    /// Whether `line` sits inside an (unclosed) fenced code block, judged by
    /// scanning the fence delimiters above it. Mirrors how the tokenizer and
    /// comrak pair fences: a closing fence uses the same character and at
    /// least the opening run length, with no info string.
    pub(super) fn in_fenced_code(&self, line: usize) -> bool {
        let mut fence: Option<(char, usize)> = None;
        for i in 0..line {
            let text = self.line_without_eol(i);
            let trimmed = text.trim_start();
            let Some(first) = trimmed.chars().next().filter(|c| matches!(c, '`' | '~')) else {
                continue;
            };
            let run = trimmed.chars().take_while(|&c| c == first).count();
            if run < 3 {
                continue;
            }
            match fence {
                None => fence = Some((first, run)),
                Some((open, open_run))
                    if first == open && run >= open_run && trimmed[run..].trim().is_empty() =>
                {
                    fence = None;
                }
                Some(_) => {}
            }
        }
        fence.is_some()
    }

    /// Renumber the contiguous ordered-list run the cursor is in so the source
    /// numbers are sequential (matching the rendered preview). No-op for bullet
    /// lists, a single item, or a run that is already sequential.
    pub(super) fn renumber_ordered_run(&mut self) {
        let total = self.buffer.rope().len_lines();
        let cursor_line = self
            .buffer
            .rope()
            .byte_to_line(self.cursor.byte)
            .min(total.saturating_sub(1));
        // Cheap pre-check: only do the full parse when on or next to an ordered
        // item, so ordinary edits elsewhere stay fast.
        let near = ordered_indent(self, cursor_line).is_some()
            || (cursor_line > 0 && ordered_indent(self, cursor_line - 1).is_some())
            || (cursor_line + 1 < total && ordered_indent(self, cursor_line + 1).is_some());
        if !near {
            return;
        }

        // Find the innermost ordered list whose source lines contain the cursor.
        // Using comrak's parse correctly spans the blank lines a loose list adds.
        let source = self.buffer.rope().to_string();
        let arena = Arena::new();
        let root = parse_document(&arena, &source, &gfm_options());
        let Some(list_node) = innermost_ordered_list(root, cursor_line) else {
            return;
        };
        let NodeValue::List(list) = list_node.data.borrow().value.clone() else {
            return;
        };
        let item_lines: HashSet<usize> = list_node
            .children()
            .map(|item| item.data.borrow().sourcepos.start.line.saturating_sub(1))
            .collect();
        let sp = list_node.data.borrow().sourcepos;
        let list_start = sp.start.line.saturating_sub(1).min(total - 1);
        let list_end = sp.end.line.saturating_sub(1).min(total - 1);

        let start_byte = self.buffer.rope().line_to_byte(list_start);
        let end_content =
            self.buffer.rope().line_to_byte(list_end) + self.line_without_eol(list_end).len();
        let cursor_col = self
            .cursor
            .byte
            .saturating_sub(self.buffer.rope().line_to_byte(cursor_line));

        let mut num = list.start as u64;
        let mut new_text = String::new();
        let mut new_cursor = self.cursor.byte;
        let mut changed = false;
        for line in list_start..=list_end {
            let (ls, le) = self.line_range(line);
            let full = self.buffer.slice(ls, le);
            let text = full.trim_end_matches(['\n', '\r']);
            // The original terminator (`\n`, `\r\n`, or none on the last line)
            // is reattached verbatim so a CRLF file keeps its endings.
            let terminator = &full[text.len()..];
            // Renumber only the item-marker lines; keep blank/continuation lines.
            let rebuilt = match parse_list_marker(text).filter(|_| item_lines.contains(&line)) {
                Some(m) => {
                    let delim = match m.kind {
                        ListMarkerKind::Ordered { delimiter, .. } => delimiter,
                        _ => '.',
                    };
                    let rest = &text[m.prefix_len..];
                    let out = format!("{}{}{}{}{}", m.indent, num, delim, m.spacing, rest);
                    num += 1;
                    out
                }
                None => text.to_string(),
            };
            changed |= rebuilt != text;
            if line == cursor_line {
                let delta = rebuilt.len() as isize - text.len() as isize;
                let col = (cursor_col as isize + delta).max(0) as usize;
                new_cursor = start_byte + new_text.len() + col;
            }
            new_text.push_str(&rebuilt);
            if line < list_end {
                new_text.push_str(terminator);
            }
        }
        if !changed {
            return;
        }
        self.history.break_run();
        self.replace_range(start_byte, end_content, &new_text);
        self.cursor.byte = new_cursor.min(self.buffer.len_bytes());
        // Redo must land where the cursor actually ended up, not at the end
        // of the renumbered span that `replace_range` recorded.
        self.history.set_last_cursor_after(self.cursor.byte);
        self.history.break_run();
    }

    fn skip_existing_closer(&mut self, c: char) -> bool {
        if !is_closing_pair_char(c) || self.char_at_cursor() != Some(c) {
            return false;
        }
        self.do_motion(Motion::Right, false);
        true
    }

    fn char_at_cursor(&self) -> Option<char> {
        let rope = self.buffer.rope();
        (self.cursor.byte < rope.len_bytes())
            .then(|| rope.char(rope.byte_to_char(self.cursor.byte)))
    }
}

fn opening_pair(c: char) -> Option<char> {
    match c {
        '*' => Some('*'),
        '_' => Some('_'),
        '~' => Some('~'),
        '`' => Some('`'),
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '<' => Some('>'),
        '"' => Some('"'),
        '\'' => Some('\''),
        _ => None,
    }
}

fn is_closing_pair_char(c: char) -> bool {
    matches!(
        c,
        '*' | '_' | '~' | '`' | ')' | ']' | '}' | '>' | '"' | '\''
    )
}

fn parse_list_marker(line: &str) -> Option<ListMarker> {
    let bytes = line.as_bytes();
    let mut marker_start = 0;
    while marker_start < bytes.len() && is_list_space(bytes[marker_start]) {
        marker_start += 1;
    }

    if marker_start + 1 < bytes.len()
        && matches!(bytes[marker_start], b'-' | b'*' | b'+')
        && is_list_space(bytes[marker_start + 1])
    {
        let bullet = bytes[marker_start] as char;
        let mut spaces_end = marker_start + 2;
        while spaces_end < bytes.len() && is_list_space(bytes[spaces_end]) {
            spaces_end += 1;
        }
        let spacing = line[marker_start + 1..spaces_end].to_string();
        // A `[ ]` / `[x]` checkbox right after the bullet makes it a task item.
        let (kind, prefix_end) = match task_checkbox_end(bytes, spaces_end) {
            Some(end) => (ListMarkerKind::Task(bullet), end),
            None => (ListMarkerKind::Bullet(bullet), spaces_end),
        };
        return Some(ListMarker {
            indent: line[..marker_start].to_string(),
            kind,
            spacing,
            prefix_len: prefix_end,
        });
    }

    let digits_start = marker_start;
    let mut delimiter_at = digits_start;
    while delimiter_at < bytes.len() && bytes[delimiter_at].is_ascii_digit() {
        delimiter_at += 1;
    }
    let digit_count = delimiter_at.saturating_sub(digits_start);
    if digit_count == 0 || digit_count > 9 || delimiter_at + 1 >= bytes.len() {
        return None;
    }
    let delimiter = bytes[delimiter_at];
    if !matches!(delimiter, b'.' | b')') || !is_list_space(bytes[delimiter_at + 1]) {
        return None;
    }

    let mut prefix_end = delimiter_at + 2;
    while prefix_end < bytes.len() && is_list_space(bytes[prefix_end]) {
        prefix_end += 1;
    }

    Some(ListMarker {
        indent: line[..marker_start].to_string(),
        kind: ListMarkerKind::Ordered {
            number: line[digits_start..delimiter_at].parse().ok()?,
            delimiter: delimiter as char,
        },
        spacing: line[delimiter_at + 1..prefix_end].to_string(),
        prefix_len: prefix_end,
    })
}

fn is_list_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t')
}

/// The indentation of `line` if it is an ordered-list item, else `None`.
fn ordered_indent(app: &App, line: usize) -> Option<String> {
    parse_list_marker(&app.line_without_eol(line))
        .filter(|m| matches!(m.kind, ListMarkerKind::Ordered { .. }))
        .map(|m| m.indent)
}

/// The innermost ordered-list node whose source lines contain `line` (0-based).
fn innermost_ordered_list<'a>(node: &'a AstNode<'a>, line: usize) -> Option<&'a AstNode<'a>> {
    let mut found = None;
    {
        let data = node.data.borrow();
        let sp = data.sourcepos;
        let contains =
            (sp.start.line.saturating_sub(1)..=sp.end.line.saturating_sub(1)).contains(&line);
        if contains
            && matches!(&data.value, NodeValue::List(l) if matches!(l.list_type, ListType::Ordered))
        {
            found = Some(node);
        }
    }
    for child in node.children() {
        if let Some(deeper) = innermost_ordered_list(child, line) {
            found = Some(deeper);
        }
    }
    found
}

/// If `bytes[at..]` starts with a GFM checkbox (`[ ]`, `[x]`, `[X]`) followed by
/// a space or end-of-content, return the byte index just past it (and any
/// trailing spaces); otherwise `None`.
fn task_checkbox_end(bytes: &[u8], at: usize) -> Option<usize> {
    if at + 2 < bytes.len()
        && bytes[at] == b'['
        && matches!(bytes[at + 1], b' ' | b'x' | b'X')
        && bytes[at + 2] == b']'
    {
        let after = at + 3;
        if after >= bytes.len() || is_list_space(bytes[after]) {
            let mut end = after;
            while end < bytes.len() && is_list_space(bytes[end]) {
                end += 1;
            }
            return Some(end);
        }
    }
    None
}

/// Whether `line` is only a space-less list/number stub (`-`, `*`, `+`, `2.`)
/// the user never completed into an item.
fn is_bare_marker(line: &str) -> bool {
    let body = line.trim();
    if matches!(body, "-" | "*" | "+") {
        return true;
    }
    let b = body.as_bytes();
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && digits + 1 == body.len() && matches!(b[digits], b'.' | b')')
}

fn leading_indent(line: &str) -> String {
    line[..line.len() - line.trim_start().len()].to_string()
}
