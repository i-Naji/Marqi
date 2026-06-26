use super::*;
use crossterm::event::KeyEventKind;

fn app() -> App {
    let mut a = App::new(TextBuffer::empty());
    a.set_viewport(20, 10);
    a
}

fn app_with(src: &str) -> App {
    let mut a = App::new(TextBuffer::scratch(src, "test.md"));
    a.clipboard = Clipboard::internal_only();
    a.set_viewport(40, 10);
    a
}

fn press(app: &mut App, code: KeyCode) {
    press_mod(app, code, KeyModifiers::NONE);
}

fn press_mod(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    });
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        press(app, KeyCode::Char(c));
    }
}

#[test]
fn typing_inserts_and_advances_cursor() {
    let mut a = app();
    type_str(&mut a, "hi");
    assert_eq!(a.buffer.rope().to_string(), "hi");
    assert_eq!(a.cursor.byte, 2);
}

#[test]
fn standard_is_default_and_uses_common_shortcuts() {
    let mut a = app();
    a.clipboard = Clipboard::internal_only();
    assert_eq!(a.preset, Preset::Standard);
    assert_eq!(a.mode, Mode::Insert);

    type_str(&mut a, "hello");
    press_mod(&mut a, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert_eq!(a.selection_range(), Some((0, 5)));
    press_mod(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL);
    press_mod(&mut a, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert_eq!(a.buffer.rope().to_string(), "");
    press_mod(&mut a, KeyCode::Char('v'), KeyModifiers::CONTROL);
    assert_eq!(a.buffer.rope().to_string(), "hello");
    press_mod(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(a.buffer.rope().to_string(), "");
    press_mod(&mut a, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(a.buffer.rope().to_string(), "hello");
}

#[test]
fn ctrl_l_cycles_keybinding_presets_for_the_session() {
    let mut a = app();
    assert_eq!(a.preset, Preset::Standard);
    press_mod(&mut a, KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert_eq!(a.preset, Preset::Vim);
    assert_eq!(a.mode, Mode::Normal);
    press_mod(&mut a, KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert_eq!(a.preset, Preset::Nano);
    assert_eq!(a.mode, Mode::Insert);
    press_mod(&mut a, KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert_eq!(a.preset, Preset::Emacs);
    press_mod(&mut a, KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert_eq!(a.preset, Preset::Standard);
}

#[test]
fn ctrl_b_toggles_cursor_shape_globally() {
    let mut a = vim_app();
    press_mod(&mut a, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert_eq!(a.mode, Mode::Read);

    press_mod(&mut a, KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert_eq!(a.cursor_shape(), CursorShape::Line);
    assert_eq!(a.mode, Mode::Read);
    assert_eq!(a.status.as_deref(), Some("Cursor: line"));

    press_mod(&mut a, KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert_eq!(a.cursor_shape(), CursorShape::Block);
}

#[test]
fn backspace_removes_grapheme_clusters_whole() {
    let mut a = app();
    type_str(&mut a, "a中👨‍👩‍👧‍👦");
    let full = a.buffer.rope().to_string();
    assert_eq!(full, "a中👨‍👩‍👧‍👦");

    press(&mut a, KeyCode::Backspace); // removes the whole ZWJ family
    assert_eq!(a.buffer.rope().to_string(), "a中");
    press(&mut a, KeyCode::Backspace); // removes 中
    assert_eq!(a.buffer.rope().to_string(), "a");
}

#[test]
fn horizontal_motion_steps_clusters_not_bytes() {
    let mut a = app();
    type_str(&mut a, "中x");
    press(&mut a, KeyCode::Home);
    assert_eq!(a.cursor.byte, 0);
    press(&mut a, KeyCode::Right); // over 中 (3 bytes)
    assert_eq!(a.cursor.byte, "中".len());
    press(&mut a, KeyCode::Left);
    assert_eq!(a.cursor.byte, 0);
}

#[test]
fn vertical_motion_keeps_goal_column() {
    let mut a = app();
    type_str(&mut a, "long line");
    press(&mut a, KeyCode::Enter);
    type_str(&mut a, "ab");
    press(&mut a, KeyCode::Enter);
    type_str(&mut a, "another long line");
    // Cursor is at end of line 3; go to col via Home then Right x7.
    press(&mut a, KeyCode::Home);
    for _ in 0..7 {
        press(&mut a, KeyCode::Right);
    }
    let goal = a.cursor.goal_col as usize + 1; // cursor_line_col is 1-based
    press(&mut a, KeyCode::Up); // onto short "ab" -> clamps to end
    press(&mut a, KeyCode::Up); // back onto "long line" -> restores goal col
    assert_eq!(a.cursor_line_col().1, goal);
}

#[test]
fn enter_splits_into_two_lines() {
    let mut a = app();
    type_str(&mut a, "ab");
    press(&mut a, KeyCode::Enter);
    type_str(&mut a, "cd");
    assert_eq!(a.buffer.rope().to_string(), "ab\ncd");
    assert_eq!(a.buffer.rope().len_lines(), 2);
}

#[test]
fn asterisk_inserts_pair_and_skips_existing_closer() {
    let mut a = app();
    press(&mut a, KeyCode::Char('*'));
    assert_eq!(a.buffer.rope().to_string(), "**");
    assert_eq!(a.cursor.byte, 1);

    type_str(&mut a, "bold");
    assert_eq!(a.buffer.rope().to_string(), "*bold*");
    assert_eq!(a.cursor.byte, 5);

    press(&mut a, KeyCode::Char('*'));
    assert_eq!(a.buffer.rope().to_string(), "*bold*");
    assert_eq!(a.cursor.byte, 6);
}

#[test]
fn bracket_pair_skips_closer_and_redo_restores_inner_cursor() {
    let mut a = app();
    press(&mut a, KeyCode::Char('('));
    assert_eq!(a.buffer.rope().to_string(), "()");
    assert_eq!(a.cursor.byte, 1);

    press(&mut a, KeyCode::Char(')'));
    assert_eq!(a.buffer.rope().to_string(), "()");
    assert_eq!(a.cursor.byte, 2);

    a.undo();
    assert_eq!(a.buffer.rope().to_string(), "");
    a.redo();
    assert_eq!(a.buffer.rope().to_string(), "()");
    assert_eq!(a.cursor.byte, 1);
}

#[test]
fn smart_pairs_wrap_selection() {
    let mut bold = app_with("word");
    bold.select_all();
    press(&mut bold, KeyCode::Char('*'));
    assert_eq!(bold.buffer.rope().to_string(), "*word*");
    assert_eq!(bold.cursor.byte, 6);

    let mut bracket = app_with("word");
    bracket.select_all();
    press(&mut bracket, KeyCode::Char('['));
    assert_eq!(bracket.buffer.rope().to_string(), "[word]");
    assert_eq!(bracket.cursor.byte, 6);

    let mut quote = app_with("word");
    quote.select_all();
    press(&mut quote, KeyCode::Char('"'));
    assert_eq!(quote.buffer.rope().to_string(), "\"word\"");
    assert_eq!(quote.cursor.byte, 6);
}

#[test]
fn bullet_list_enter_continues_then_exits_empty_item() {
    let mut a = app();
    type_str(&mut a, "- one");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.buffer.rope().to_string(), "- one\n- ");
    assert_eq!(a.cursor.byte, "- one\n- ".len());

    // Enter on the empty item exits the list (drops the marker). The default
    // (soft_break = "space") leaves no extra blank line.
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.buffer.rope().to_string(), "- one\n");
    assert_eq!(a.cursor.byte, "- one\n".len());

    a.undo();
    assert_eq!(a.buffer.rope().to_string(), "- one\n- ");
}

#[test]
fn task_list_enter_continues_with_empty_checkbox_then_exits() {
    let mut a = app();
    type_str(&mut a, "- [x] done");
    press(&mut a, KeyCode::Enter);
    // A checked task continues as a fresh *unchecked* checkbox.
    assert_eq!(a.buffer.rope().to_string(), "- [x] done\n- [ ] ");
    assert_eq!(a.cursor.byte, "- [x] done\n- [ ] ".len());

    // Enter on the empty checkbox exits the list.
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.buffer.rope().to_string(), "- [x] done\n");
}

#[test]
fn empty_item_hard_break_mode_leaves_a_blank_line() {
    let cfg: Config = toml::from_str("[editor]\nsoft_break = \"break\"\n").unwrap();
    let mut a = App::with_config(TextBuffer::empty(), &cfg);
    a.set_viewport(40, 10);
    type_str(&mut a, "- one");
    press(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Enter); // exit on empty item
    assert_eq!(a.buffer.rope().to_string(), "- one\n\n");
}

#[test]
fn bare_marker_enter_drops_the_stub() {
    // A lone "-" with no space (an unfinished item) is dropped on Enter.
    let mut a = app();
    type_str(&mut a, "-");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.buffer.rope().to_string(), "");
}

#[test]
fn numbered_list_enter_increments_and_preserves_delimiter() {
    let mut dot = app();
    type_str(&mut dot, "1. one");
    press(&mut dot, KeyCode::Enter);
    assert_eq!(dot.buffer.rope().to_string(), "1. one\n2. ");
    assert_eq!(dot.cursor.byte, "1. one\n2. ".len());

    let mut paren = app();
    type_str(&mut paren, "9) nine");
    press(&mut paren, KeyCode::Enter);
    assert_eq!(paren.buffer.rope().to_string(), "9) nine\n10) ");
    assert_eq!(paren.cursor.byte, "9) nine\n10) ".len());
}

#[test]
fn list_continuation_preserves_indentation_and_undo_boundary() {
    let mut a = app();
    type_str(&mut a, "  + one");
    press(&mut a, KeyCode::Enter);
    type_str(&mut a, "two");
    assert_eq!(a.buffer.rope().to_string(), "  + one\n  + two");

    a.undo();
    assert_eq!(a.buffer.rope().to_string(), "  + one\n  + ");
    a.undo();
    assert_eq!(a.buffer.rope().to_string(), "  + one");
}

#[test]
fn undo_redo_round_trips_a_typing_burst() {
    let mut a = app();
    type_str(&mut a, "hello");
    a.undo();
    assert_eq!(
        a.buffer.rope().to_string(),
        "",
        "a typing burst undoes at once"
    );
    a.redo();
    assert_eq!(a.buffer.rope().to_string(), "hello");
}

#[test]
fn newline_and_cursor_move_break_undo_steps() {
    let mut a = app();
    type_str(&mut a, "ab");
    press(&mut a, KeyCode::Enter);
    type_str(&mut a, "cd");
    a.undo(); // "cd"
    assert_eq!(a.buffer.rope().to_string(), "ab\n");
    a.undo(); // "\n"
    assert_eq!(a.buffer.rope().to_string(), "ab");
    a.undo(); // "ab"
    assert_eq!(a.buffer.rope().to_string(), "");
}

#[test]
fn shift_selection_then_backspace_deletes_the_range() {
    let mut a = app();
    type_str(&mut a, "abcdef");
    press(&mut a, KeyCode::Home);
    for _ in 0..3 {
        press_mod(&mut a, KeyCode::Right, KeyModifiers::SHIFT);
    }
    assert_eq!(a.selection_range(), Some((0, 3)));
    press(&mut a, KeyCode::Backspace);
    assert_eq!(a.buffer.rope().to_string(), "def");
    assert_eq!(a.selection_range(), None);
}

#[test]
fn shift_arrows_extend_selection_across_lines() {
    let mut a = app();
    type_str(&mut a, "ab");
    press(&mut a, KeyCode::Enter);
    type_str(&mut a, "cd"); // "ab\ncd"
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::Home);
    assert_eq!(a.cursor.byte, 0);

    // Extend across the newline with shift+Right.
    for _ in 0..4 {
        press_mod(&mut a, KeyCode::Right, KeyModifiers::SHIFT);
    }
    assert_eq!(a.cursor.byte, 4);
    assert_eq!(
        a.selection_range(),
        Some((0, 4)),
        "selection spans the newline"
    );

    // Vertical: shift+Down from the top extends down a line.
    press(&mut a, KeyCode::Home);
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::Home);
    press_mod(&mut a, KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(
        a.selection_range(),
        Some((0, 3)),
        "shift+Down selects down a line"
    );
}

#[test]
fn cross_block_selection_full_flow() {
    let mut a = app();
    type_str(&mut a, "alpha");
    press(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Enter);
    type_str(&mut a, "bravo"); // "alpha\n\nbravo" — two paragraphs

    // Navigate to the very start.
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::Up);
    press(&mut a, KeyCode::Home);
    assert_eq!(a.cursor.byte, 0);

    // Shift+Down twice into "bravo", then Shift+Right to actually cover one
    // of its characters.
    press_mod(&mut a, KeyCode::Down, KeyModifiers::SHIFT);
    press_mod(&mut a, KeyCode::Down, KeyModifiers::SHIFT);
    press_mod(&mut a, KeyCode::Right, KeyModifiers::SHIFT);
    let sel = a.selection_range();
    assert!(
        sel.is_some_and(|(s, e)| s == 0 && e >= 8),
        "selection should span into the second block, got {sel:?}"
    );

    // The assembled view must highlight more than one row.
    a.set_viewport(20, 10);
    let rows = a.all_rows();
    let highlighted = rows
        .lines
        .iter()
        .filter(|l| {
            l.spans
                .iter()
                .any(|s| s.style.bg == Some(a.theme.selection))
        })
        .count();
    assert!(
        highlighted >= 2,
        "cross-block selection should highlight 2+ rows, got {highlighted}"
    );
}

#[test]
fn config_selects_preset_settings_and_theme() {
    let cfg: Config = toml::from_str(
        "[editor]\nkeybindings = \" Nano \"\nline_numbers = \"relative\"\nheading_glyphs = false\ntab_width = 2\n\n[theme.markdown]\nheading1 = \"#ff0000\"\n",
    )
    .unwrap();
    let a = App::with_config(TextBuffer::empty(), &cfg);
    assert_eq!(a.preset, Preset::Nano);
    assert_eq!(a.line_numbers, LineNumbers::Relative);
    assert_eq!(a.mode, Mode::Insert, "nano rests in insert mode");
    assert_eq!(a.tab_width, 2);
    assert!(!a.theme.heading_glyphs);
    assert_eq!(a.theme.heading(1).fg, crate::color::parse_hex("#ff0000"));
}

#[test]
fn help_toggles_from_ctrl_g_and_vim_question_mark() {
    let mut a = vim_app();
    press_mod(&mut a, KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert!(a.help_open);
    press(&mut a, KeyCode::Esc);
    assert!(!a.help_open);
    press(&mut a, KeyCode::Char('?'));
    assert!(a.help_open);
}

#[test]
fn read_mode_q_returns_to_focus() {
    let mut a = vim_app();
    press_mod(&mut a, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert_eq!(a.mode, Mode::Read);
    press(&mut a, KeyCode::Char('q'));
    assert_eq!(a.mode, Mode::Normal);
}

#[test]
fn nano_preset_inserts_on_plain_keys() {
    let cfg: Config = toml::from_str("[editor]\nkeybindings = \"nano\"\n").unwrap();
    let mut a = App::with_config(TextBuffer::empty(), &cfg);
    a.set_viewport(20, 10);
    type_str(&mut a, "hi");
    assert_eq!(a.buffer.rope().to_string(), "hi");
}

/// A Vim-preset app resting in Normal mode.
fn vim_app() -> App {
    let cfg: Config = toml::from_str("[editor]\nkeybindings = \"vim\"\n").unwrap();
    let mut a = App::with_config(TextBuffer::empty(), &cfg);
    a.clipboard = Clipboard::internal_only();
    a.set_viewport(20, 10);
    a
}

#[test]
fn vim_insert_escape_and_normal_motion() {
    let mut a = vim_app();
    assert_eq!(a.mode, Mode::Normal);
    press(&mut a, KeyCode::Char('h')); // a motion, not text
    assert_eq!(
        a.buffer.rope().to_string(),
        "",
        "normal-mode keys don't insert"
    );

    press(&mut a, KeyCode::Char('i'));
    assert_eq!(a.mode, Mode::Insert);
    type_str(&mut a, "hello");
    press(&mut a, KeyCode::Esc);
    assert_eq!(a.mode, Mode::Normal);
    assert_eq!(a.buffer.rope().to_string(), "hello");
}

#[test]
fn vim_visual_select_and_delete() {
    let mut a = vim_app();
    press(&mut a, KeyCode::Char('i'));
    type_str(&mut a, "hello");
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Char('0')); // line start
    press(&mut a, KeyCode::Char('v')); // visual
    assert_eq!(a.mode, Mode::Visual);
    press(&mut a, KeyCode::Char('l'));
    press(&mut a, KeyCode::Char('l'));
    assert_eq!(a.selection_range(), Some((0, 2)));
    press(&mut a, KeyCode::Char('d')); // delete selection
    assert_eq!(a.buffer.rope().to_string(), "llo");
    assert_eq!(a.mode, Mode::Normal);
}

#[test]
fn vim_dd_deletes_the_line() {
    let mut a = vim_app();
    press(&mut a, KeyCode::Char('i'));
    type_str(&mut a, "ab");
    press(&mut a, KeyCode::Enter);
    type_str(&mut a, "cd");
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Char('d'));
    press(&mut a, KeyCode::Char('d'));
    assert_eq!(a.buffer.rope().to_string(), "ab\n");
}

#[test]
fn cut_then_paste_round_trips_via_register() {
    let mut a = app();
    a.clipboard = Clipboard::internal_only();
    type_str(&mut a, "abcdef");
    press(&mut a, KeyCode::Home);
    for _ in 0..3 {
        press_mod(&mut a, KeyCode::Right, KeyModifiers::SHIFT);
    }
    a.cut();
    assert_eq!(a.buffer.rope().to_string(), "def");
    a.paste(); // cursor is at 0 after cut
    assert_eq!(a.buffer.rope().to_string(), "abcdef");
}

#[test]
fn table_mode_navigates_cells_and_inserts_deletes_rows() {
    let src = "| A | B |\n| - | - |\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut a = app_with(src);
    a.cursor.byte = src.find('1').unwrap();

    press_mod(&mut a, KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert!(a.table_mode);
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.cursor.byte, src.find('2').unwrap());

    press_mod(&mut a, KeyCode::Char('n'), KeyModifiers::CONTROL);
    assert!(a.buffer.rope().to_string().contains("|  |  |\n| 3 | 4 |"));
    press_mod(&mut a, KeyCode::Char('d'), KeyModifiers::CONTROL);
    assert_eq!(a.buffer.rope().to_string(), src);
}

#[test]
fn table_row_move_at_eof_without_trailing_newline() {
    // A table at EOF with no trailing newline: moving the last data row up must
    // keep the rows on separate physical lines and not glue them together or
    // introduce a spurious trailing newline.
    let src = "| A | B |\n| - | - |\n| 1 | 2 |\n| 3 | 4 |";
    let mut a = app_with(src);
    a.cursor.byte = src.rfind('3').unwrap();
    press_mod(&mut a, KeyCode::Char('t'), KeyModifiers::CONTROL);
    press_mod(&mut a, KeyCode::Up, KeyModifiers::ALT);
    assert_eq!(
        a.buffer.rope().to_string(),
        "| A | B |\n| - | - |\n| 3 | 4 |\n| 1 | 2 |"
    );
}

#[test]
fn table_mode_protects_header_and_moves_data_rows() {
    let src = "| A | B |\n| - | - |\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut a = app_with(src);
    a.cursor.byte = src.find('A').unwrap();
    press_mod(&mut a, KeyCode::Char('t'), KeyModifiers::CONTROL);
    press_mod(&mut a, KeyCode::Char('d'), KeyModifiers::CONTROL);
    assert_eq!(a.buffer.rope().to_string(), src);

    a.cursor.byte = a.buffer.rope().to_string().find('3').unwrap();
    press_mod(&mut a, KeyCode::Up, KeyModifiers::ALT);
    let out = a.buffer.rope().to_string();
    assert!(out.contains("| 3 | 4 |\n| 1 | 2 |"));
}

#[test]
fn save_as_prompts_for_a_name_then_writes_atomically() {
    let mut a = app();
    type_str(&mut a, "hello world");

    // An unnamed buffer opens a save-as prompt, pre-filled with ".md".
    press_mod(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
    let pv = a
        .prompt_view()
        .expect("save opens a prompt for an unnamed buffer");
    assert!(
        pv.text.starts_with("Save as: .md"),
        "prompt text: {:?}",
        pv.text
    );
    assert_eq!(
        pv.cursor_col,
        Some("Save as: ".len() as u16),
        "cursor before .md"
    );

    // Clear the ".md" and type a unique temp path.
    for _ in 0..3 {
        press(&mut a, KeyCode::Delete);
    }
    let path = std::env::temp_dir().join(format!("marqi_saveas_{}.md", std::process::id()));
    std::fs::remove_file(&path).ok();
    type_str(&mut a, path.to_str().unwrap());
    press(&mut a, KeyCode::Enter);

    assert!(
        a.prompt_view().is_none(),
        "prompt closes after a successful save"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
    assert_eq!(
        a.buffer.file_name(),
        path.file_name().unwrap().to_string_lossy()
    );
    std::fs::remove_file(&path).ok();
}

#[test]
fn save_as_confirms_before_overwriting() {
    let path = std::env::temp_dir().join(format!("marqi_overwrite_{}.md", std::process::id()));
    std::fs::write(&path, "old").unwrap();

    let mut a = app();
    type_str(&mut a, "new");
    press_mod(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
    for _ in 0..3 {
        press(&mut a, KeyCode::Delete);
    }
    type_str(&mut a, path.to_str().unwrap());
    press(&mut a, KeyCode::Enter);

    // Existing file -> overwrite confirmation, and "n" does not write.
    assert!(a.prompt_view().unwrap().text.contains("Overwrite"));
    press(&mut a, KeyCode::Char('n'));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    assert!(a.prompt_view().is_some(), "n returns to the name prompt");

    // Re-confirm the name, then "y" overwrites.
    press(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Char('y'));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    assert!(a.prompt_view().is_none());
    std::fs::remove_file(&path).ok();
}

#[test]
fn save_as_can_be_cancelled() {
    let mut a = app();
    type_str(&mut a, "draft");
    press_mod(&mut a, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(a.prompt_view().is_some());
    press(&mut a, KeyCode::Esc);
    assert!(a.prompt_view().is_none(), "Esc cancels the prompt");
    assert_eq!(a.buffer.rope().to_string(), "draft", "buffer is untouched");
}

#[test]
fn raw_view_shows_source_with_markers() {
    let mut a = app_with("# Title\n\nA **bold** word.\n");
    press_mod(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(a.raw_view(), "^R toggles raw view");
    a.set_viewport(40, 10);
    let rows = a.all_rows();
    let text: String = rows
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    // Every block is shown raw — markers are kept, not stripped.
    assert!(text.contains("# Title"), "raw heading kept: {text:?}");
    assert!(text.contains("**bold**"), "raw markers kept: {text:?}");

    // Toggling off returns to the hybrid focus view.
    press_mod(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(!a.raw_view());
}

#[test]
fn vim_normal_ctrl_r_is_redo_not_raw_view() {
    let mut a = vim_app();
    press(&mut a, KeyCode::Char('i'));
    type_str(&mut a, "x");
    press(&mut a, KeyCode::Esc); // normal mode
    a.undo();
    assert_eq!(a.buffer.rope().to_string(), "");
    // In Vim normal mode, ^R redoes (it does not toggle the raw view).
    press_mod(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(!a.raw_view());
    assert_eq!(a.buffer.rope().to_string(), "x");
}

#[test]
fn cursor_in_loose_list_gap_opens_only_the_blank() {
    // The cursor in the blank line between two loose-list items must not render
    // the whole list raw — only the blank is active; the items stay preview.
    let mut a = app_with("- a\n\n- c\n");
    a.cursor.byte = a.buffer.rope().line_to_byte(1); // the blank line
    a.set_viewport(30, 12);
    let rows = a.all_rows();
    let text: String = rows
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains('\u{2022}'),
        "items should preview as bullets:\n{text}"
    );
    assert!(
        !text.contains("- a"),
        "first item must not render raw:\n{text}"
    );
    assert!(
        !text.contains("- c"),
        "second item must not render raw:\n{text}"
    );
    // The active region is just the one blank line.
    assert_eq!(
        a.view().active_lines,
        (1, 1),
        "only the blank line is active"
    );
}

#[test]
fn inserting_in_a_numbered_list_renumbers_the_source() {
    let mut a = app();
    type_str(&mut a, "1. a");
    press(&mut a, KeyCode::Enter);
    type_str(&mut a, "b");
    press(&mut a, KeyCode::Enter);
    type_str(&mut a, "c");
    assert_eq!(a.buffer.rope().to_string(), "1. a\n2. b\n3. c");

    // Insert a new item after "1. a"; the source stays sequential (1,2,3,4).
    a.cursor.byte = "1. a".len();
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.buffer.rope().to_string(), "1. a\n2. \n3. b\n4. c");
    assert_eq!(a.cursor.byte, "1. a\n2. ".len());
}

#[test]
fn deleting_a_middle_numbered_item_renumbers() {
    let mut a = app_with("1. a\n2. b\n3. c\n");
    a.cursor.byte = a.buffer.rope().line_to_byte(1); // on "2. b"
    a.delete_line();
    assert_eq!(a.buffer.rope().to_string(), "1. a\n2. c\n");
}

#[test]
fn exiting_empty_ordered_item_renumbers_the_rest() {
    let mut a = app_with("1. a\n2. \n3. c\n");
    a.cursor.byte = "1. a\n2. ".len(); // end of the empty "2. "
    press(&mut a, KeyCode::Enter); // exit the list
    assert_eq!(a.buffer.rope().to_string(), "1. a\n\n2. c\n");
}

#[test]
fn empty_checkbox_renders_as_a_glyph_in_preview() {
    // An empty unchecked task shows the checkbox glyph (not raw "- [ ]").
    let mut a = app_with("- [x] done\n- [ ]\n");
    a.cursor.byte = 0; // cursor on item 0, so item 1 is preview
    a.set_viewport(30, 10);
    let assembled = a.all_rows();
    let rows: Vec<String> = assembled
        .lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    assert!(
        rows.iter().any(|r| r.trim() == "\u{25a1}"),
        "empty checkbox should show □:\n{rows:?}"
    );
}

#[test]
fn global_shortcuts_cancel_a_pending_chord() {
    let mut a = vim_app();
    press(&mut a, KeyCode::Char('i'));
    type_str(&mut a, "ab\ncd");
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Char('d')); // start a `dd` chord...
    press_mod(&mut a, KeyCode::Char('b'), KeyModifiers::CONTROL); // ...interrupted
    press(&mut a, KeyCode::Char('d')); // a fresh leader, not the chord's tail
    assert_eq!(
        a.buffer.rope().to_string(),
        "ab\ncd",
        "an interrupted chord must not fire"
    );
    press(&mut a, KeyCode::Char('d')); // complete `dd` normally
    assert_eq!(a.buffer.rope().to_string(), "ab\n");
}

#[test]
fn emacs_mark_extends_motions_until_cleared() {
    let mut a = app_with("hello");
    a.preset = Preset::Emacs;
    press_mod(&mut a, KeyCode::Char(' '), KeyModifiers::CONTROL); // set mark
    press_mod(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    press_mod(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert_eq!(
        a.selection_range(),
        Some((0, 2)),
        "motions extend the region"
    );
    press(&mut a, KeyCode::Esc);
    assert_eq!(a.selection_range(), None, "Esc deactivates the mark");
}

#[test]
fn typing_a_closer_replaces_the_selection() {
    let mut a = app_with("abc)");
    press(&mut a, KeyCode::Home);
    for _ in 0..3 {
        press_mod(&mut a, KeyCode::Right, KeyModifiers::SHIFT);
    }
    assert_eq!(a.selection_range(), Some((0, 3)));
    press(&mut a, KeyCode::Char(')'));
    assert_eq!(
        a.buffer.rope().to_string(),
        "))",
        "a closing char replaces the selection instead of type-over"
    );
}

#[test]
fn quit_with_unsaved_changes_asks_first() {
    let mut a = app_with("draft");
    press_mod(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(a.should_quit, "an unmodified buffer quits immediately");

    let mut a = app_with("draft");
    type_str(&mut a, "x");
    press_mod(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(!a.should_quit, "a modified buffer must not quit silently");
    assert!(a.prompt.is_some());
    press(&mut a, KeyCode::Char('n'));
    assert!(!a.should_quit);
    assert!(a.prompt.is_none());
    press_mod(&mut a, KeyCode::Char('q'), KeyModifiers::CONTROL);
    press(&mut a, KeyCode::Char('y'));
    assert!(a.should_quit);
}

#[test]
fn undo_back_to_the_loaded_state_clears_modified() {
    let mut a = app_with("hello");
    type_str(&mut a, "x");
    assert!(a.buffer.modified());
    press_mod(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(
        !a.buffer.modified(),
        "undoing the only edit returns to the saved state"
    );
    press_mod(&mut a, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert!(a.buffer.modified(), "redo makes it modified again");
}

#[test]
fn noop_delete_does_not_destroy_redo() {
    let mut a = vim_app();
    press(&mut a, KeyCode::Char('i'));
    type_str(&mut a, "x");
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Char('u')); // undo -> empty buffer
    assert_eq!(a.buffer.rope().to_string(), "");
    press(&mut a, KeyCode::Char('d'));
    press(&mut a, KeyCode::Char('d')); // dd on the empty buffer: a no-op
    press_mod(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL); // redo
    assert_eq!(
        a.buffer.rope().to_string(),
        "x",
        "a no-op edit must not clear the redo stack"
    );
}

#[test]
fn enter_in_a_crlf_file_inserts_crlf() {
    let mut a = app_with("ab\r\ncd\r\n");
    press(&mut a, KeyCode::End);
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.buffer.rope().to_string(), "ab\r\n\r\ncd\r\n");
}

#[test]
fn enter_inside_a_code_fence_never_continues_lists() {
    let mut a = app_with("```\n- item\n```\n");
    a.cursor.byte = "```\n- item".len(); // end of the bullet-looking line
    press(&mut a, KeyCode::Enter);
    assert_eq!(
        a.buffer.rope().to_string(),
        "```\n- item\n\n```\n",
        "list continuation must not fire inside a fence"
    );
}

#[test]
fn indented_table_cells_have_no_phantom_indent_cell() {
    let mut a = app_with("  | a | b |\n  | - | - |\n  | 1 | 2 |\n");
    a.cursor.byte = 4; // on "a" in the header row
    press_mod(&mut a, KeyCode::Char('t'), KeyModifiers::CONTROL);
    assert!(a.table_mode());
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.cursor.byte, 8, "Tab lands on the b cell, not the indent");
    press(&mut a, KeyCode::BackTab);
    assert_eq!(a.cursor.byte, 4, "BackTab returns to the a cell");
}

#[test]
fn table_row_insert_redoes_to_the_same_cursor() {
    let mut a = app_with("| a | b |\n| - | - |\n| 1 | 2 |");
    a.cursor.byte = a.buffer.rope().line_to_byte(2) + 2;
    press_mod(&mut a, KeyCode::Char('t'), KeyModifiers::CONTROL);
    press_mod(&mut a, KeyCode::Char('n'), KeyModifiers::CONTROL); // insert row
    let after = (a.buffer.rope().to_string(), a.cursor.byte);
    press_mod(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
    press_mod(&mut a, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(
        (a.buffer.rope().to_string(), a.cursor.byte),
        after,
        "redo restores both the text and the adjusted cursor"
    );
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn find_jumps_counts_and_wraps() {
    let mut a = app_with("alpha beta alpha gamma alpha");
    press_mod(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(a.prompt.is_some(), "^F opens the find prompt");
    type_str(&mut a, "alpha");
    assert_eq!(a.selection_range(), Some((0, 5)), "incremental first match");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.selection_range(), Some((11, 16)), "Enter steps forward");
    press(&mut a, KeyCode::Up);
    assert_eq!(a.selection_range(), Some((0, 5)), "Up steps back");
    press(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.selection_range(), Some((23, 28)));
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.selection_range(), Some((0, 5)), "stepping wraps around");
    press(&mut a, KeyCode::Esc);
    assert!(a.prompt.is_none());
    assert_eq!(a.last_search, "alpha");
}

#[test]
fn find_uses_smart_case() {
    let mut a = app_with("alpha Alpha");
    press_mod(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut a, "Alpha"); // has uppercase: exact match only
    assert_eq!(a.selection_range(), Some((6, 11)));
    assert_eq!(
        a.search_matches("alpha"),
        vec![0, 6],
        "lowercase matches both"
    );
    assert_eq!(a.search_matches("Alpha"), vec![6]);
}

#[test]
fn replace_replaces_current_and_advances() {
    let mut a = app_with("foo bar foo");
    press_mod(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut a, "foo");
    press(&mut a, KeyCode::Tab); // switch to replace
    type_str(&mut a, "X");
    press(&mut a, KeyCode::Enter); // replace first match
    assert_eq!(a.buffer.rope().to_string(), "X bar foo");
    assert_eq!(a.selection_range(), Some((6, 9)), "next match is selected");
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.buffer.rope().to_string(), "X bar X");
}

#[test]
fn replace_all_is_one_undo_step() {
    let mut a = app_with("foo bar foo");
    press_mod(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    type_str(&mut a, "foo");
    press(&mut a, KeyCode::Tab);
    type_str(&mut a, "longer");
    press_mod(&mut a, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert_eq!(a.buffer.rope().to_string(), "longer bar longer");
    assert!(a.prompt.is_none(), "replace-all closes the prompt");
    assert_eq!(a.status.as_deref(), Some("Replaced 2 occurrences"));
    press_mod(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(
        a.buffer.rope().to_string(),
        "foo bar foo",
        "replace-all undoes as a single step"
    );
}

#[test]
fn vim_slash_and_n_navigate_matches() {
    let mut a = vim_app();
    press(&mut a, KeyCode::Char('i'));
    type_str(&mut a, "one two one");
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Char('g'));
    press(&mut a, KeyCode::Char('g')); // doc start
    press(&mut a, KeyCode::Char('/'));
    assert!(a.prompt.is_some(), "/ opens find in vim normal mode");
    type_str(&mut a, "one");
    press(&mut a, KeyCode::Enter);
    press(&mut a, KeyCode::Esc);
    press(&mut a, KeyCode::Char('n'));
    assert_eq!(a.cursor.byte, 0, "n wraps to the first match, no selection");
    assert_eq!(a.selection_range(), None);
    press(&mut a, KeyCode::Char('n'));
    assert_eq!(a.cursor.byte, 8);
    press(&mut a, KeyCode::Char('N'));
    assert_eq!(a.cursor.byte, 0);
}

#[test]
fn click_positions_the_cursor() {
    let mut a = app_with("alpha\n\nbeta line\n");
    a.set_viewport(40, 10);
    a.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 3, 0));
    assert_eq!(a.cursor.byte, 3, "click in the active raw row is exact");

    // Row 2 renders the preview of the "beta line" paragraph; a click lands
    // at the same column inside its source line.
    a.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 2));
    assert_eq!(a.cursor.byte, 9);
}

#[test]
fn drag_selects_from_the_press_point() {
    let mut a = app_with("hello world");
    a.set_viewport(40, 10);
    a.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
    a.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 0));
    assert_eq!(a.selection_range(), Some((0, 5)));
}

#[test]
fn wheel_scroll_detaches_and_a_keypress_reattaches() {
    let src = "line\n".repeat(50);
    let mut a = app_with(&src);
    a.set_viewport(40, 10);
    a.scroll_to_cursor();
    assert_eq!(a.scroll_y, 0);
    a.handle_mouse(mouse(MouseEventKind::ScrollDown, 0, 0));
    assert_eq!(a.scroll_y, 3);
    assert!(!a.follows_cursor(), "wheel scrolling detaches the viewport");
    press(&mut a, KeyCode::Right);
    assert!(a.follows_cursor(), "a keypress snaps back to the cursor");
}

#[test]
fn auto_save_fires_after_the_idle_delay() {
    let mut path = std::env::temp_dir();
    path.push(format!("marqi_autosave_{}.md", std::process::id()));
    std::fs::write(&path, "x").unwrap();
    let cfg: Config = toml::from_str("[editor]\nauto_save = true\n").unwrap();
    let mut a = App::with_config(TextBuffer::from_path(&path).unwrap(), &cfg);
    a.clipboard = Clipboard::internal_only();
    a.set_viewport(40, 10);

    type_str(&mut a, "y");
    assert!(a.wants_tick());
    a.tick(); // just edited: still inside the idle delay
    assert!(a.buffer.modified(), "tick must not save mid-burst");

    a.last_edit = Some(Instant::now() - Duration::from_secs(3));
    a.tick();
    assert!(!a.buffer.modified(), "idle tick saves");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "yx");
    assert_eq!(a.status.as_deref(), Some("Auto-saved"));
    std::fs::remove_file(&path).ok();
}

#[test]
fn cmd_super_modifier_acts_as_ctrl() {
    let mut a = app_with("hello");
    type_str(&mut a, "x");
    press_mod(&mut a, KeyCode::Char('z'), KeyModifiers::SUPER); // Cmd+Z
    assert_eq!(a.buffer.rope().to_string(), "hello", "Cmd+Z undoes");
}

#[test]
fn click_moves_between_list_items() {
    let mut a = app_with("- one\n- two\n- three\n");
    a.set_viewport(40, 10);
    // Item one is the raw hole; two and three render as preview rows, which
    // carry no per-row line number — the click must still resolve them.
    a.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 2));
    assert_eq!(
        a.buffer.rope().byte_to_line(a.cursor.byte),
        2,
        "clicking the third item moves the cursor to its line"
    );
    a.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 1));
    assert_eq!(
        a.buffer.rope().byte_to_line(a.cursor.byte),
        1,
        "clicking the second item moves the cursor to its line"
    );
}

#[test]
fn click_into_a_preview_table_lands_inside_the_table() {
    let mut a = app_with("para\n\n| a | b |\n| - | - |\n| 1 | 2 |\n");
    a.set_viewport(40, 10);
    // Rows: 0 = "para" (raw hole), 1 = blank gap, 2.. = the rendered grid.
    a.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 4));
    let line = a.buffer.rope().byte_to_line(a.cursor.byte);
    assert!(
        (2..=4).contains(&line),
        "click lands inside the table source, got line {line}"
    );
}

#[test]
fn edit_rerenders_only_the_dirty_block() {
    let src = crate::testdoc::many_blocks(200);
    let mut a = app_with(&src);
    a.set_viewport(60, 20);
    let _ = a.visible_rows();
    let (_, before, _) = a.debug_stats();

    // Jump to a mid-document paragraph and type: only the block the cursor
    // left (now inactive, content unchanged but never rendered while active)
    // may render — everything else is height- and cache-hits.
    a.cursor.byte = src.find("Paragraph 25").unwrap();
    press(&mut a, KeyCode::Char('z'));
    a.scroll_to_cursor();
    let _ = a.visible_rows();
    let (_, after, _) = a.debug_stats();
    assert!(
        after.block_renders <= before.block_renders + 2,
        "a one-block edit re-renders at most the blocks it touched: {} -> {}",
        before.block_renders,
        after.block_renders
    );
}

#[test]
fn cursor_jump_to_offscreen_line_scrolls_and_assembles() {
    let src = crate::testdoc::many_blocks(120);
    let mut a = app_with(&src);
    a.set_viewport(40, 10);
    let _ = a.visible_rows();

    a.cursor.byte = src.find("# Heading 96").unwrap();
    a.scroll_to_cursor();
    let (_, row) = a.cursor_screen();
    assert!(
        row >= a.scroll_y && row < a.scroll_y + 10,
        "cursor row {row} visible from scroll {}",
        a.scroll_y
    );
    let rows = a.visible_rows();
    let text: String = rows
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("# Heading 96"),
        "the jumped-to block is in the assembled window:\n{text}"
    );
}

#[test]
fn gutter_numbers_stay_aligned_after_wheel_scroll() {
    // One unwrapped paragraph: every row is raw, labels are exact layout lines.
    let mut a = app_with(&crate::testdoc::ascii(200, 10));
    a.set_viewport(40, 10);
    for _ in 0..20 {
        a.handle_mouse(mouse(MouseEventKind::ScrollDown, 0, 0));
    }
    assert_eq!(a.scroll_y, 60, "20 wheel steps of 3 rows");
    let rows = a.visible_rows();
    let expected: Vec<Option<usize>> = (61..71).map(Some).collect();
    assert_eq!(
        rows.numbers, expected,
        "visible gutter labels match the scrolled-to source lines"
    );
}

fn assert_matches_fresh_app(a: &mut App) {
    let mut fresh = app_with(&a.buffer.rope().to_string());
    fresh.set_viewport(50, 15);
    fresh.cursor.byte = a.cursor.byte;
    let ours = a.all_rows();
    let theirs = fresh.all_rows();
    assert_eq!(ours.lines, theirs.lines, "incremental rows == fresh rows");
    assert_eq!(ours.numbers, theirs.numbers, "incremental labels == fresh");
}

#[test]
fn incremental_view_matches_a_fresh_app_after_edits_and_undo() {
    let src = crate::testdoc::many_blocks(30);
    let mut a = app_with(&src);
    a.set_viewport(50, 15);
    a.cursor.byte = src.find("Paragraph 25").unwrap();
    type_str(&mut a, "intro ");
    press(&mut a, KeyCode::Enter);
    type_str(&mut a, "- new item");
    press(&mut a, KeyCode::Enter);
    assert_matches_fresh_app(&mut a);

    press_mod(&mut a, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_matches_fresh_app(&mut a);
}

/// Manual perf probe, not a CI test:
/// `cargo test --release perf_probe -- --ignored --nocapture`
#[test]
#[ignore = "manual perf probe; run with --release --ignored --nocapture"]
fn perf_probe_keystroke_and_scroll() {
    let src = crate::testdoc::many_blocks(12_000);
    let mut a = app_with(&src);
    a.set_viewport(100, 40);

    let t = std::time::Instant::now();
    let _ = a.visible_rows();
    eprintln!(
        "doc {} KB, {} lines · first build+assemble {:?}",
        src.len() / 1024,
        a.buffer.rope().len_lines(),
        t.elapsed()
    );

    a.cursor.byte = src.find("Paragraph 6001").expect("mid-doc paragraph");
    a.scroll_to_cursor();
    let _ = a.visible_rows();

    let t = std::time::Instant::now();
    for _ in 0..20 {
        press(&mut a, KeyCode::Char('x'));
        a.scroll_to_cursor();
        let _ = a.visible_rows();
    }
    eprintln!("keystroke (edit+index+assemble) avg {:?}", t.elapsed() / 20);

    let t = std::time::Instant::now();
    for _ in 0..40 {
        press(&mut a, KeyCode::Down);
        a.scroll_to_cursor();
        let _ = a.visible_rows();
    }
    eprintln!("cursor-line move avg {:?}", t.elapsed() / 40);

    let t = std::time::Instant::now();
    for _ in 0..40 {
        a.handle_mouse(mouse(MouseEventKind::ScrollDown, 0, 0));
        let _ = a.visible_rows();
    }
    eprintln!("wheel step avg {:?}", t.elapsed() / 40);

    let (timings, cache, layout) = a.debug_stats();
    eprintln!(
        "last: layout {}us · parse {}us · view build {}us · assemble {}us",
        timings.last_layout_us,
        timings.last_parse_us,
        timings.last_view_build_us,
        timings.last_assemble_us
    );
    eprintln!(
        "cache: {} blocks, {} hits, {} renders, {} KB · layout rows live {}",
        cache.blocks_total,
        cache.block_hits,
        cache.block_renders,
        cache.rendered_bytes / 1024,
        layout.rows_live
    );
}

#[test]
fn debug_counters_track_pipeline_activity() {
    let mut a = app_with("# Title\n\nbody text\n\n- one\n- two\n");
    let (_, cache, layout) = a.debug_stats();
    assert!(cache.blocks_total > 0, "block partition ran on first view");
    assert!(layout.rows_live > 0, "layout holds the document's rows");
    assert!(
        layout.rows_built >= layout.rows_live && layout.cells_built > 0,
        "construction counters accumulate"
    );

    let hits_before = cache.block_hits;
    press(&mut a, KeyCode::Char('x'));
    a.scroll_to_cursor();
    let (_, cache, _) = a.debug_stats();
    assert_eq!(
        cache.block_hits, hits_before,
        "an edit alone fetches no rendered blocks (heights cover the index)"
    );
    let _ = a.visible_rows();
    let (_, cache, _) = a.debug_stats();
    assert!(
        cache.block_hits > hits_before,
        "assembling the viewport fetches the visible blocks from cache"
    );

    let line = a.stats_line();
    assert!(
        line.contains("blk") && line.contains("rows"),
        "stats line formats counters: {line}"
    );
    let dump = a.stats_dump();
    assert!(
        dump.contains("layout:") && dump.contains("view:"),
        "exit dump covers each subsystem: {dump}"
    );
}
