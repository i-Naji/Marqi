use super::{Action, App};
use crossterm::event::{KeyCode, KeyEvent};
use unicode_segmentation::UnicodeSegmentation;

pub struct Palette {
    query: String,
    selected: usize,
}

pub struct PaletteItem {
    pub action: Action,
    pub selected: bool,
    pub enabled: bool,
}

impl App {
    pub(super) fn open_palette(&mut self) {
        self.menu = None;
        self.palette = Some(Palette {
            query: String::new(),
            selected: 0,
        });
    }

    pub fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    pub fn palette_query(&self) -> &str {
        self.palette
            .as_ref()
            .map_or("", |palette| palette.query.as_str())
    }

    pub fn palette_items(&self) -> Vec<PaletteItem> {
        let Some(palette) = self.palette.as_ref() else {
            return Vec::new();
        };
        palette
            .matches()
            .into_iter()
            .enumerate()
            .map(|(index, action)| PaletteItem {
                action,
                selected: index == palette.selected,
                enabled: self.action_enabled(action),
            })
            .collect()
    }

    pub(super) fn handle_palette_key(&mut self, key: KeyEvent) {
        let Some(mut palette) = self.palette.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Up => palette.move_selection(-1),
            KeyCode::Down => palette.move_selection(1),
            KeyCode::Enter => {
                let action = palette.matches().get(palette.selected).copied();
                if let Some(action) = action
                    && self.action_enabled(action)
                {
                    self.run_action(action);
                    return;
                }
            }
            KeyCode::Backspace => {
                if let Some((index, _)) = palette.query.grapheme_indices(true).next_back() {
                    palette.query.truncate(index);
                    palette.selected = 0;
                }
            }
            KeyCode::Char(ch) if !super::ctrl_like(key.modifiers) => {
                palette.query.push(ch);
                palette.selected = 0;
            }
            _ => {}
        }
        self.palette = Some(palette);
    }
}

impl Palette {
    fn matches(&self) -> Vec<Action> {
        let mut actions: Vec<_> = Action::ALL
            .into_iter()
            .filter_map(|action| {
                fuzzy_score(action.label(), &self.query).map(|score| (score, action))
            })
            .collect();
        actions.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.label().cmp(right.label()))
        });
        actions.into_iter().map(|(_, action)| action).collect()
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.matches().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.saturating_add_signed(delta).min(len - 1);
        }
    }
}

fn fuzzy_score(label: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let query = query.to_lowercase();
    let mut wanted = query.chars();
    let mut next = wanted.next()?;
    let mut score = 0;
    let mut consecutive = false;
    for (index, ch) in label.to_lowercase().chars().enumerate() {
        if ch != next {
            consecutive = false;
            continue;
        }
        score += if consecutive { 8 } else { 3 };
        if index == 0 || label.as_bytes().get(index.wrapping_sub(1)) == Some(&b' ') {
            score += 4;
        }
        consecutive = true;
        let Some(wanted_next) = wanted.next() else {
            return Some(score);
        };
        next = wanted_next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::fuzzy_score;

    #[test]
    fn fuzzy_matching_prefers_consecutive_letters() {
        assert!(fuzzy_score("Toggle raw source", "raw").is_some());
        assert!(fuzzy_score("Toggle raw source", "tgr").is_some());
        assert!(fuzzy_score("Save", "xyz").is_none());
        assert!(
            fuzzy_score("Raw", "raw").unwrap() > fuzzy_score("Redo action work", "raw").unwrap()
        );
    }
}
