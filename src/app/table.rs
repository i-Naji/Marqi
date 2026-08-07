use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthStr;

use super::App;

#[derive(Clone, Copy)]
struct TableInfo {
    start_line: usize,
    separator_line: usize,
    end_line: usize,
    columns: usize,
}

struct ParsedRow {
    indent: String,
    cells: Vec<String>,
    leading_pipe: bool,
    trailing_pipe: bool,
    eol: String,
}

impl ParsedRow {
    fn new(source: &str) -> Self {
        let body_len = source.trim_end_matches(['\r', '\n']).len();
        let (body, eol) = source.split_at(body_len);
        let indent_len = body
            .bytes()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count();
        let table = body[indent_len..].trim_end();
        let pipes = pipe_positions(table);
        let leading_pipe = pipes.first() == Some(&0);
        let trailing_pipe = pipes.last() == Some(&table.len().saturating_sub(1)) && table.len() > 1;
        let mut inner = table;
        if leading_pipe {
            inner = &inner[1..];
        }
        if trailing_pipe {
            inner = &inner[..inner.len() - 1];
        }
        Self {
            indent: body[..indent_len].to_string(),
            cells: split_table_cells(inner)
                .into_iter()
                .map(|cell| cell.trim().to_string())
                .collect(),
            leading_pipe,
            trailing_pipe,
            eol: eol.to_string(),
        }
    }

    fn format(&self, widths: &[usize], separator: bool) -> String {
        let mut output = self.indent.clone();
        if self.leading_pipe {
            output.push('|');
        }
        for (column, width) in widths.iter().copied().enumerate() {
            if self.leading_pipe || column > 0 {
                output.push(' ');
            }
            let source = self.cells.get(column).map_or("", String::as_str);
            let cell = if separator {
                separator_cell(source, width)
            } else {
                let padding = width.saturating_sub(UnicodeWidthStr::width(source));
                format!("{source}{}", " ".repeat(padding))
            };
            output.push_str(&cell);
            if column + 1 < widths.len() || self.trailing_pipe {
                output.push(' ');
                output.push('|');
            }
        }
        output.push_str(&self.eol);
        output
    }
}

fn separator_cell(source: &str, width: usize) -> String {
    let source = source.trim();
    let left = source.starts_with(':');
    let right = source.ends_with(':');
    let dashes = width.saturating_sub(usize::from(left) + usize::from(right));
    format!(
        "{}{}{}",
        if left { ":" } else { "" },
        "-".repeat(dashes),
        if right { ":" } else { "" }
    )
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

fn split_table_cells(text: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut start = 0;
    for pipe in pipe_positions(text) {
        cells.push(text[start..pipe].to_string());
        start = pipe + 1;
    }
    cells.push(text[start..].to_string());
    cells
}

impl App {
    pub(super) fn toggle_table_mode(&mut self) {
        if self.table_at_cursor().is_some() {
            self.table_mode = !self.table_mode;
            self.status = Some(if self.table_mode {
                "Table row mode".to_string()
            } else {
                "Table row mode off".to_string()
            });
        } else {
            self.table_mode = false;
            self.status = Some("No table at cursor".to_string());
        }
    }

    pub(super) fn format_table(&mut self) {
        let Some(info) = self.table_at_cursor() else {
            self.status = Some("No table at cursor".to_string());
            return;
        };
        let rows: Vec<ParsedRow> = (info.start_line..=info.end_line)
            .map(|line| {
                let (start, end) = self.line_range(line);
                ParsedRow::new(&self.buffer.slice(start, end))
            })
            .collect();
        let columns = rows.iter().map(|row| row.cells.len()).max().unwrap_or(0);
        if columns == 0 {
            self.status = Some("No table at cursor".to_string());
            return;
        }
        let mut widths = vec![3; columns];
        for (row_index, row) in rows.iter().enumerate() {
            for (column, cell) in row.cells.iter().enumerate() {
                if info.start_line + row_index != info.separator_line {
                    widths[column] = widths[column].max(UnicodeWidthStr::width(cell.as_str()));
                }
            }
        }
        let mut replacement = String::new();
        for (row_index, row) in rows.iter().enumerate() {
            replacement
                .push_str(&row.format(&widths, info.start_line + row_index == info.separator_line));
        }
        let start = self.buffer.rope().line_to_byte(info.start_line);
        let end = self.line_range(info.end_line).1;
        let cursor =
            start + super::floor_char_boundary(&replacement, self.cursor.byte.saturating_sub(start));
        self.history.break_run();
        self.replace_range_with_cursor(start, end, &replacement, Some(cursor));
        self.history.break_run();
        self.status = Some("Table formatted".to_string());
    }

    pub(super) fn handle_table_key(&mut self, key: KeyEvent, ctrl: bool) -> bool {
        if self.table_at_cursor().is_none() {
            self.table_mode = false;
            self.status = Some("Table row mode off".to_string());
            return false;
        }

        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Tab => {
                self.jump_table_cell(!shift);
                true
            }
            // Most terminals deliver Shift+Tab as `ESC [ Z` = BackTab.
            KeyCode::BackTab => {
                self.jump_table_cell(false);
                true
            }
            KeyCode::Char('n') if ctrl => {
                self.insert_table_row();
                true
            }
            KeyCode::Char('d') if ctrl => {
                self.delete_table_row();
                true
            }
            KeyCode::Up if alt => {
                self.move_table_row(false);
                true
            }
            KeyCode::Down if alt => {
                self.move_table_row(true);
                true
            }
            _ => false,
        }
    }

    fn table_at_cursor(&self) -> Option<TableInfo> {
        let rope = self.buffer.rope();
        let total = rope.len_lines();
        if total == 0 {
            return None;
        }
        let current = rope.byte_to_line(self.cursor.byte).min(total - 1);
        // Pipe detection is line-local; a `|` inside a fenced code block (a
        // shell pipeline, say) is not a table.
        if !self.is_table_like_line(current) || self.in_fenced_code(current) {
            return None;
        }

        let mut start = current;
        while start > 0 && self.is_table_like_line(start - 1) {
            start -= 1;
        }
        let mut end = current;
        while end + 1 < total && self.is_table_like_line(end + 1) {
            end += 1;
        }

        let separator_line = (start + 1..=end).find(|line| self.is_separator_line(*line))?;
        let columns = (start..=end)
            .map(|line| self.table_cells_text(line).len())
            .max()
            .unwrap_or(0);
        (columns > 0).then_some(TableInfo {
            start_line: start,
            separator_line,
            end_line: end,
            columns,
        })
    }

    fn is_table_like_line(&self, line: usize) -> bool {
        let text = self.line_without_eol(line);
        text.contains('|') && !text.trim().is_empty()
    }

    fn is_separator_line(&self, line: usize) -> bool {
        let cells = self.table_cells_text(line);
        !cells.is_empty()
            && cells.iter().all(|cell| {
                let cell = cell.trim();
                let core = cell.trim_matches(':');
                !core.is_empty() && core.chars().all(|c| c == '-')
            })
    }

    fn table_cells_text(&self, line: usize) -> Vec<String> {
        let text = self.line_without_eol(line);
        if text.trim().is_empty() || !text.contains('|') {
            return Vec::new();
        }
        ParsedRow::new(&text).cells
    }

    /// Byte ranges of the cells on `line`, as offsets into the full
    /// (untrimmed) line. Surrounding whitespace is ignored, so an indented
    /// table row does not grow a phantom "indent" cell — keeping these bounds
    /// consistent with `table_cells_text`, which counts trimmed cells.
    fn table_cell_bounds(&self, line: usize) -> Vec<(usize, usize)> {
        let text = self.line_without_eol(line);
        let indent = text.len() - text.trim_start().len();
        let trimmed = text.trim();
        let pipes = pipe_positions(trimmed);
        if pipes.is_empty() {
            return Vec::new();
        }

        let starts_with_pipe = pipes.first() == Some(&0);
        let ends_with_pipe = pipes.last() == Some(&trimmed.len().saturating_sub(1));
        let mut bounds = Vec::new();
        let mut start = if starts_with_pipe { pipes[0] + 1 } else { 0 };
        let first_pipe = usize::from(starts_with_pipe);
        for pipe in pipes.iter().skip(first_pipe) {
            bounds.push((indent + start, indent + *pipe));
            start = pipe + 1;
        }
        if !ends_with_pipe && start <= trimmed.len() {
            bounds.push((indent + start, indent + trimmed.len()));
        }
        bounds
    }

    fn current_table_cell(&self, line: usize) -> usize {
        let line_start = self.buffer.rope().line_to_byte(line);
        let rel = self.cursor.byte.saturating_sub(line_start);
        let bounds = self.table_cell_bounds(line);
        bounds
            .iter()
            // The end bound is inclusive: a cursor sitting on a `|` counts as
            // being in the cell that pipe closes.
            .position(|(start, end)| rel <= *end && rel >= *start)
            .unwrap_or_else(|| bounds.len().saturating_sub(1))
    }

    fn jump_table_cell(&mut self, forward: bool) {
        let Some(info) = self.table_at_cursor() else {
            return;
        };
        let mut line = self
            .buffer
            .rope()
            .byte_to_line(self.cursor.byte)
            .min(info.end_line);
        let mut cell = self.current_table_cell(line);

        loop {
            let cells = self.table_cell_bounds(line).len().max(1);
            if forward {
                if cell + 1 < cells {
                    cell += 1;
                } else {
                    line = if line >= info.end_line {
                        info.start_line
                    } else {
                        line + 1
                    };
                    if line == info.separator_line {
                        line = (line + 1).min(info.end_line);
                    }
                    cell = 0;
                }
            } else if cell > 0 {
                cell -= 1;
            } else {
                line = if line <= info.start_line {
                    info.end_line
                } else {
                    line - 1
                };
                if line == info.separator_line {
                    line = line.saturating_sub(1).max(info.start_line);
                }
                cell = self.table_cell_bounds(line).len().saturating_sub(1);
            }
            if line != info.separator_line {
                break;
            }
        }

        self.cursor.byte = self.table_cell_target(line, cell);
        self.ensure_layout();
        self.cursor.sync_goal(&self.layout);
        self.history.break_run();
    }

    fn table_cell_target(&self, line: usize, cell: usize) -> usize {
        let line_start = self.buffer.rope().line_to_byte(line);
        let bounds = self.table_cell_bounds(line);
        let Some((mut start, end)) = bounds.get(cell).copied().or_else(|| bounds.last().copied())
        else {
            return line_start;
        };
        let text = self.line_without_eol(line);
        while start < end
            && text
                .as_bytes()
                .get(start)
                .is_some_and(u8::is_ascii_whitespace)
        {
            start += 1;
        }
        line_start + start
    }

    fn insert_table_row(&mut self) {
        let Some(info) = self.table_at_cursor() else {
            return;
        };
        let cursor_line = self.buffer.rope().byte_to_line(self.cursor.byte);
        let base_line = cursor_line.max(info.separator_line);
        let row = self.empty_table_row(info.columns, base_line);
        let insert_at = if base_line + 1 < self.buffer.rope().len_lines() {
            self.buffer.rope().line_to_byte(base_line + 1)
        } else {
            self.buffer.len_bytes()
        };
        let nl = self.buffer.newline();
        let prefix = if self.line_has_eol(base_line) { "" } else { nl };
        let text = format!("{prefix}{row}{nl}");
        self.history.break_run();
        self.replace_range(insert_at, insert_at, &text);
        self.history.break_run();

        let new_line = base_line + 1;
        self.cursor.byte = self.table_cell_target(new_line, 0);
        self.history.set_last_cursor_after(self.cursor.byte);
        self.status = Some("Inserted table row".to_string());
    }

    fn delete_table_row(&mut self) {
        let Some(info) = self.table_at_cursor() else {
            return;
        };
        let line = self.buffer.rope().byte_to_line(self.cursor.byte);
        if line <= info.separator_line {
            self.status = Some("Header and separator rows are protected".to_string());
            return;
        }
        let (start, end) = self.line_range(line);
        self.history.break_run();
        self.replace_range(start, end, "");
        self.history.break_run();
        let target_line = line.min(self.buffer.rope().len_lines().saturating_sub(1));
        self.cursor.byte = self.buffer.rope().line_to_byte(target_line);
        self.history.set_last_cursor_after(self.cursor.byte);
        self.status = Some("Deleted table row".to_string());
    }

    fn move_table_row(&mut self, down: bool) {
        let Some(info) = self.table_at_cursor() else {
            return;
        };
        let line = self.buffer.rope().byte_to_line(self.cursor.byte);
        if line <= info.separator_line {
            self.status = Some("Header and separator rows are protected".to_string());
            return;
        }
        let target = if down {
            if line >= info.end_line {
                self.status = Some("Already at last table row".to_string());
                return;
            }
            line + 1
        } else {
            if line <= info.separator_line + 1 {
                self.status = Some("Already at first data row".to_string());
                return;
            }
            line - 1
        };

        let first = line.min(target);
        let second = line.max(target);
        let (first_start, first_end) = self.line_range(first);
        let (second_start, second_end) = self.line_range(second);
        let first_full = self.buffer.slice(first_start, first_end);
        let second_full = self.buffer.slice(second_start, second_end);
        let first_body = first_full.trim_end_matches(['\n', '\r']);
        let second_body = second_full.trim_end_matches(['\n', '\r']);
        // Swap the two line bodies but keep each newline at its original
        // position: `first` always carries a terminator (it is never the last
        // line), while `second` may be the unterminated final line. Moving the
        // terminators with the bodies would otherwise merge the rows onto one
        // physical line when the table sits at EOF without a trailing newline.
        let first_term = &first_full[first_body.len()..];
        let second_term = &second_full[second_body.len()..];
        let replacement = format!("{second_body}{first_term}{first_body}{second_term}");

        self.history.break_run();
        self.replace_range(first_start, second_end, &replacement);
        self.history.break_run();
        self.cursor.byte = self.buffer.rope().line_to_byte(target);
        self.history.set_last_cursor_after(self.cursor.byte);
        self.status = Some("Moved table row".to_string());
    }

    fn empty_table_row(&self, columns: usize, sample_line: usize) -> String {
        let sample = self.line_without_eol(sample_line);
        let trimmed = sample.trim();
        let leading = trimmed.starts_with('|');
        let trailing = trimmed.ends_with('|');
        // Keep the sample row's indentation so an indented table stays aligned.
        let mut row = sample[..sample.len() - sample.trim_start().len()].to_string();
        if leading {
            row.push('|');
        }
        for col in 0..columns {
            row.push(' ');
            row.push(' ');
            if col + 1 < columns || trailing {
                row.push('|');
            }
        }
        row
    }
}
