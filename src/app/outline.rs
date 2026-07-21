use super::App;
use super::palette::fuzzy_score;
use crossterm::event::{KeyCode, KeyEvent};
use unicode_segmentation::UnicodeSegmentation;

pub struct Outline {
    query: String,
    selected: usize,
}

#[derive(Clone)]
struct Heading {
    line: usize,
    level: usize,
    title: String,
}

pub struct OutlineItem {
    pub line: usize,
    pub level: usize,
    pub title: String,
    pub selected: bool,
    pub current: bool,
}

impl App {
    pub(super) fn open_outline(&mut self) {
        self.palette = None;
        self.outline = Some(Outline {
            query: String::new(),
            selected: 0,
        });
    }

    pub fn outline_open(&self) -> bool {
        self.outline.is_some()
    }

    pub fn outline_query(&self) -> &str {
        self.outline
            .as_ref()
            .map_or("", |outline| outline.query.as_str())
    }

    pub fn outline_items(&mut self) -> Vec<OutlineItem> {
        let headings = self.headings();
        let Some(outline) = self.outline.as_ref() else {
            return Vec::new();
        };
        let current_line = self.buffer.rope().byte_to_line(self.cursor.byte);
        let current = headings
            .iter()
            .rev()
            .find(|heading| heading.line <= current_line)
            .map(|heading| heading.line);
        outline
            .matches(&headings)
            .into_iter()
            .enumerate()
            .map(|(index, heading)| OutlineItem {
                line: heading.line,
                level: heading.level,
                title: heading.title.clone(),
                selected: index == outline.selected,
                current: current == Some(heading.line),
            })
            .collect()
    }

    pub(super) fn handle_outline_key(&mut self, key: KeyEvent) {
        let headings = self.headings();
        let Some(mut outline) = self.outline.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Up => outline.move_selection(&headings, -1),
            KeyCode::Down => outline.move_selection(&headings, 1),
            KeyCode::Enter => {
                if let Some(heading) = outline.matches(&headings).get(outline.selected) {
                    self.mode = self.resting_mode();
                    self.cursor.byte = self.buffer.rope().line_to_byte(heading.line);
                    self.selection_anchor = None;
                    self.follow_cursor = true;
                    self.ensure_layout();
                    self.cursor.sync_goal(&self.layout);
                    return;
                }
            }
            KeyCode::Backspace => {
                if let Some((index, _)) = outline.query.grapheme_indices(true).next_back() {
                    outline.query.truncate(index);
                    outline.selected = 0;
                }
            }
            KeyCode::Char(ch) if !super::ctrl_like(key.modifiers) => {
                outline.query.push(ch);
                outline.selected = 0;
            }
            _ => {}
        }
        self.outline = Some(outline);
    }

    fn headings(&mut self) -> Vec<Heading> {
        self.view_cache
            .heading_blocks(self.buffer.rope(), self.version)
            .into_iter()
            .filter_map(|block| {
                let source = self.buffer.slice(block.start_byte, block.end_byte);
                parse_heading(&source).map(|(level, title)| Heading {
                    line: block.start_line,
                    level,
                    title,
                })
            })
            .collect()
    }
}

impl Outline {
    fn matches<'a>(&self, headings: &'a [Heading]) -> Vec<&'a Heading> {
        if self.query.is_empty() {
            return headings.iter().collect();
        }
        let mut matches: Vec<_> = headings
            .iter()
            .filter_map(|heading| {
                fuzzy_score(&heading.title, &self.query).map(|score| (score, heading))
            })
            .collect();
        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.line.cmp(&right.line))
        });
        matches.into_iter().map(|(_, heading)| heading).collect()
    }

    fn move_selection(&mut self, headings: &[Heading], delta: isize) {
        let len = self.matches(headings).len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.saturating_add_signed(delta).min(len - 1);
        }
    }
}

fn parse_heading(source: &str) -> Option<(usize, String)> {
    let mut lines = source.lines();
    let first = lines.next()?.trim();
    let hashes = first.bytes().take_while(|byte| *byte == b'#').count();
    if (1..=6).contains(&hashes)
        && first
            .as_bytes()
            .get(hashes)
            .is_some_and(u8::is_ascii_whitespace)
    {
        let title = first[hashes..]
            .trim()
            .trim_end_matches('#')
            .trim()
            .to_string();
        return Some((hashes, title));
    }
    let underline = lines.next()?.trim();
    let level = match underline.as_bytes().first()? {
        b'=' => 1,
        b'-' => 2,
        _ => return None,
    };
    Some((level, first.to_string()))
}

#[cfg(test)]
mod tests {
    use super::parse_heading;

    #[test]
    fn parses_atx_and_setext_headings() {
        assert_eq!(parse_heading("### Title ###\n"), Some((3, "Title".into())));
        assert_eq!(parse_heading("Title\n=====\n"), Some((1, "Title".into())));
    }
}
