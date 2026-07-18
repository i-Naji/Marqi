use super::App;

impl App {
    pub(super) fn toggle_inline(&mut self, marker: &str) {
        self.history.break_run();
        if let Some((start, end)) = self.selection_range() {
            let selected = self.buffer.slice(start, end);
            let replacement = selected
                .strip_prefix(marker)
                .and_then(|text| text.strip_suffix(marker))
                .map_or_else(|| format!("{marker}{selected}{marker}"), str::to_string);
            self.replace_range(start, end, &replacement);
        } else {
            let at = self.cursor.byte;
            let pair = format!("{marker}{marker}");
            self.replace_range_with_cursor(at, at, &pair, Some(at + marker.len()));
        }
        self.history.break_run();
    }

    pub(super) fn insert_link(&mut self) {
        self.history.break_run();
        if let Some((start, end)) = self.selection_range() {
            let label = self.buffer.slice(start, end);
            let replacement = format!("[{label}]()");
            let cursor = start + replacement.len() - 1;
            self.replace_range_with_cursor(start, end, &replacement, Some(cursor));
        } else {
            let at = self.cursor.byte;
            self.replace_range_with_cursor(at, at, "[]()", Some(at + 1));
        }
        self.history.break_run();
    }

    pub(super) fn toggle_line_prefix(&mut self, prefix: &str) {
        let line = self.buffer.rope().byte_to_line(self.cursor.byte);
        let (start, end) = self.line_range(line);
        let source = self.buffer.slice(start, end);
        let eol_start = source.trim_end_matches(['\r', '\n']).len();
        let (body, eol) = source.split_at(eol_start);
        let indent_len = body
            .bytes()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count();
        let (indent, content) = body.split_at(indent_len);
        let replacement = if let Some(content) = content.strip_prefix(prefix) {
            format!("{indent}{content}{eol}")
        } else {
            format!("{indent}{prefix}{content}{eol}")
        };
        let cursor = start
            + self
                .cursor
                .byte
                .saturating_sub(start)
                .min(replacement.trim_end_matches(['\r', '\n']).len());
        self.history.break_run();
        self.replace_range_with_cursor(start, end, &replacement, Some(cursor));
        self.history.break_run();
    }

    pub(super) fn cycle_heading(&mut self) {
        let line = self.buffer.rope().byte_to_line(self.cursor.byte);
        let (start, end) = self.line_range(line);
        let source = self.buffer.slice(start, end);
        let eol_start = source.trim_end_matches(['\r', '\n']).len();
        let (body, eol) = source.split_at(eol_start);
        let indent_len = body
            .bytes()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count();
        let (indent, content) = body.split_at(indent_len);
        let hashes = content.bytes().take_while(|byte| *byte == b'#').count();
        let has_heading = (1..=6).contains(&hashes)
            && content
                .as_bytes()
                .get(hashes)
                .is_some_and(u8::is_ascii_whitespace);
        let text = if has_heading {
            content[hashes..].trim_start()
        } else {
            content
        };
        let next = if has_heading { (hashes + 1) % 7 } else { 1 };
        let replacement = if next == 0 {
            format!("{indent}{text}{eol}")
        } else {
            format!("{indent}{} {text}{eol}", "#".repeat(next))
        };
        let cursor = start
            + self
                .cursor
                .byte
                .saturating_sub(start)
                .min(replacement.trim_end_matches(['\r', '\n']).len());
        self.history.break_run();
        self.replace_range_with_cursor(start, end, &replacement, Some(cursor));
        self.history.break_run();
    }
}
