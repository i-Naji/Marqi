//! Preview renderer: comrak AST → styled ratatui [`Line`]s with markdown
//! markers stripped.
//!
//! Each block is rendered into lines at most `width` columns wide with *no*
//! outer indentation; containers (lists, block quotes) render their children
//! into a fresh buffer and then prefix every produced line. This keeps wrapping
//! correct under arbitrary nesting. Inline text is word-wrapped with awareness
//! of CJK / emoji display width.

use std::cell::Cell;

use comrak::nodes::{AstNode, ListDelimType, ListType, NodeValue, TableAlignment};
use comrak::{Arena, Options, parse_document};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::highlight::CodeHighlighter;
use super::theme::MarkdownTheme;

/// A run of text sharing one style, produced while walking inline nodes.
struct Seg {
    text: String,
    style: Style,
}

/// One rendered preview row plus the 1-based source line it begins, when the
/// renderer knows it exactly: lists label every item, tables every grid row
/// (and the header separator with the `|---|` delimiter line). Wrapped
/// continuation rows and pure chrome (table borders) carry `None`.
pub struct Row {
    pub line: Line<'static>,
    pub source: Option<usize>,
}

impl Row {
    fn unlabeled(line: Line<'static>) -> Self {
        Self { line, source: None }
    }
}

/// Wrap plain lines as unlabeled rows (the caller labels what it knows).
fn unlabeled(lines: Vec<Line<'static>>) -> impl Iterator<Item = Row> {
    lines.into_iter().map(Row::unlabeled)
}

/// The block that should render as raw source instead of preview (focus mode's
/// "hole"): its 0-based inclusive line range, the precomputed raw lines to emit
/// in its place, and where those lines landed in the output.
///
/// `start_index` is written once during a render and never reset, so an
/// `ActiveLeaf` is single-use: construct a fresh one per render pass.
pub struct ActiveLeaf {
    lines: (usize, usize),
    raw: Vec<Line<'static>>,
    start_index: Cell<Option<usize>>,
}

impl ActiveLeaf {
    pub fn new(lines: (usize, usize), raw: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            raw,
            start_index: Cell::new(None),
        }
    }

    /// Output index where the raw lines were emitted, if reached.
    pub fn start_index(&self) -> Option<usize> {
        self.start_index.get()
    }
}

const MAX_QUOTE_DEPTH: usize = 64;

struct Renderer<'r> {
    theme: &'r MarkdownTheme,
    highlighter: &'r CodeHighlighter,
    active: Option<&'r ActiveLeaf>,
    depth: Cell<usize>,
}

/// Parse `source` and render the whole document to styled lines for `width`.
pub fn render(
    source: &str,
    width: usize,
    theme: &MarkdownTheme,
    highlighter: &CodeHighlighter,
) -> Vec<Line<'static>> {
    render_rows(source, width, theme, highlighter)
        .into_iter()
        .map(|row| row.line)
        .collect()
}

/// Like [`render`], but each row also carries the source line it begins.
pub fn render_rows(
    source: &str,
    width: usize,
    theme: &MarkdownTheme,
    highlighter: &CodeHighlighter,
) -> Vec<Row> {
    let arena = Arena::new();
    let root = parse_document(&arena, source, &gfm_options());
    let renderer = Renderer {
        theme,
        highlighter,
        active: None,
        depth: Cell::new(0),
    };

    let mut out = Vec::new();
    renderer.render_block_children(root, width.max(1), &mut out, true);
    if out.is_empty() {
        out.push(Row::unlabeled(Line::default()));
    }
    out
}

/// Render a single already-parsed block node to preview rows. Used by the
/// hybrid view to render inactive blocks (the active block renders raw instead).
pub fn render_block_node<'a>(
    node: &'a AstNode<'a>,
    width: usize,
    theme: &MarkdownTheme,
    highlighter: &CodeHighlighter,
) -> Vec<Row> {
    let renderer = Renderer {
        theme,
        highlighter,
        active: None,
        depth: Cell::new(0),
    };
    let mut out = Vec::new();
    renderer.render_block(node, width.max(1), &mut out);
    out
}

/// Render a block as preview, but with one descendant block (`active`) emitted
/// as raw lines in place ("preview with a hole"). Returns the rendered rows and
/// the index where the raw block landed (for cursor placement).
pub fn render_block_with_hole<'a>(
    node: &'a AstNode<'a>,
    width: usize,
    theme: &MarkdownTheme,
    highlighter: &CodeHighlighter,
    active: &ActiveLeaf,
) -> (Vec<Row>, Option<usize>) {
    let renderer = Renderer {
        theme,
        highlighter,
        active: Some(active),
        depth: Cell::new(0),
    };
    let mut out = Vec::new();
    renderer.render_block(node, width.max(1), &mut out);
    (out, active.start_index())
}

/// GFM-flavoured parse options.
pub fn gfm_options() -> Options<'static> {
    let mut o = Options::default();
    o.extension.strikethrough = true;
    o.extension.table = true;
    o.extension.tasklist = true;
    o.extension.autolink = true;
    o.extension.footnotes = true;
    o
}

impl<'r> Renderer<'r> {
    /// Render every block child of `parent`, separated by a blank line when
    /// `loose`.
    fn render_block_children<'a>(
        &self,
        parent: &'a AstNode<'a>,
        width: usize,
        out: &mut Vec<Row>,
        loose: bool,
    ) {
        let mut first = true;
        for child in parent.children() {
            if !first && loose {
                out.push(Row::unlabeled(Line::default()));
            }
            first = false;
            self.render_block(child, width, out);
        }
    }

    /// If `node` is the focus-mode active leaf, emit its raw lines in place and
    /// return true (so the caller skips its normal preview rendering).
    fn try_emit_raw<'a>(&self, node: &'a AstNode<'a>, out: &mut Vec<Row>) -> bool {
        let sp = node.data.borrow().sourcepos;
        let range = (
            sp.start.line.saturating_sub(1),
            sp.end.line.saturating_sub(1),
        );
        self.try_emit_raw_range(range, out)
    }

    /// Emit the active leaf's raw lines in place if `range` is exactly its line
    /// range (used for the active node, or for a blank run between list items).
    /// The rows stay unlabeled: the hybrid view overwrites the hole with exact
    /// per-row numbers from the full layout.
    fn try_emit_raw_range(&self, range: (usize, usize), out: &mut Vec<Row>) -> bool {
        let Some(active) = self.active else {
            return false;
        };
        if range == active.lines {
            active.start_index.set(Some(out.len()));
            out.extend(unlabeled(active.raw.clone()));
            true
        } else {
            false
        }
    }

    fn render_block<'a>(&self, node: &'a AstNode<'a>, width: usize, out: &mut Vec<Row>) {
        if self.try_emit_raw(node, out) {
            return;
        }
        let first = out.len();
        let value = node.data.borrow().value.clone();
        match &value {
            NodeValue::Heading(h) => {
                let mut segs = self.inline_segs(node, self.theme.heading(h.level));
                if self.theme.heading_glyphs {
                    segs.insert(
                        0,
                        Seg {
                            text: format!("{} ", heading_glyph(h.level)),
                            style: self.theme.heading(h.level),
                        },
                    );
                }
                out.extend(unlabeled(wrap(&segs, width, true)));
            }
            NodeValue::Paragraph => {
                let segs = self.inline_segs(node, self.theme.text);
                out.extend(unlabeled(wrap(&segs, width, true)));
            }
            NodeValue::List(list) => self.render_list(node, list, width, out),
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                let inner_w = width.saturating_sub(2).max(1);
                let mut inner = Vec::new();
                if self.depth.get() < MAX_QUOTE_DEPTH {
                    self.depth.set(self.depth.get() + 1);
                    self.render_block_children(node, inner_w, &mut inner, true);
                    self.depth.set(self.depth.get() - 1);
                } else {
                    let ellipsis = Span::styled("\u{2026}", self.theme.quote);
                    inner.push(Row::unlabeled(Line::from(ellipsis)));
                }
                let bar = vec![Span::styled("\u{2503} ", self.theme.quote_bar)];
                let inner = restyle(inner, self.theme.text, self.theme.quote);
                out.extend(prefix_lines(inner, bar.clone(), bar));
            }
            NodeValue::CodeBlock(cb) => {
                let lang = cb.info.split_whitespace().next().unwrap_or("");
                // Literal lines map 1:1 to source lines (a fenced block's
                // literal starts after the opening fence); the highlighter says
                // which rows begin one, and hard-wrapped continuations stay
                // unlabeled.
                let start = node.data.borrow().sourcepos.start.line + usize::from(cb.fenced);
                out.extend(
                    self.highlighter
                        .highlight(lang, &cb.literal, width)
                        .into_iter()
                        .map(|(line, src)| Row {
                            line,
                            source: src.map(|i| start + i),
                        }),
                );
            }
            NodeValue::ThematicBreak => {
                out.push(Row::unlabeled(Line::from(Span::styled(
                    "\u{2501}".repeat(width),
                    self.theme.rule,
                ))));
            }
            NodeValue::Table(table) => self.render_table(node, &table.alignments, width, out),
            NodeValue::HtmlBlock(html) => {
                // The literal is the source lines verbatim, so they map 1:1.
                let start = node.data.borrow().sourcepos.start.line;
                for (i, line) in html.literal.lines().enumerate() {
                    out.push(Row {
                        line: Line::from(Span::styled(line.to_string(), self.theme.html)),
                        source: Some(start + i),
                    });
                }
            }
            NodeValue::FootnoteDefinition(d) => {
                // Render the definition body behind a "[name]: " label, mirroring
                // the list-marker layout so wrapped lines stay aligned.
                let marker = format!("[{}]: ", d.name);
                let marker_w = UnicodeWidthStr::width(marker.as_str());
                let inner_w = width.saturating_sub(marker_w).max(1);
                let mut inner = Vec::new();
                self.render_block_children(node, inner_w, &mut inner, false);
                let first_prefix = vec![Span::styled(marker, self.theme.list_marker)];
                let cont = vec![Span::raw(" ".repeat(marker_w))];
                out.extend(prefix_lines(inner, first_prefix, cont));
            }
            // Containers we render through (items handled by render_list).
            _ => self.render_block_children(node, width, out, false),
        }
        // Label the block's first rendered row with its starting source line
        // when nothing more precise was recorded. A table is exempt: its first
        // row is the top border, which deliberately stays unlabeled.
        if !matches!(value, NodeValue::Table(_))
            && let Some(row) = out.get_mut(first)
            && row.source.is_none()
        {
            row.source = Some(node.data.borrow().sourcepos.start.line);
        }
    }

    fn render_list<'a>(
        &self,
        node: &'a AstNode<'a>,
        list: &comrak::nodes::NodeList,
        width: usize,
        out: &mut Vec<Row>,
    ) {
        let ordered = matches!(list.list_type, ListType::Ordered);
        let delim = match list.delimiter {
            ListDelimType::Paren => ')',
            ListDelimType::Period => '.',
        };
        let bullet = list.bullet_char as char;
        let mut number = list.start;
        let mut first = true;
        let mut prev_end: Option<usize> = None;

        for item in node.children() {
            let item_start = item.data.borrow().sourcepos.start.line.saturating_sub(1);
            if !first && !list.tight {
                // The blank line(s) between loose items. If the cursor sits in
                // that gap (with no item to open), emit it as the raw hole;
                // otherwise it is a plain blank separator.
                let gap = (
                    prev_end.map_or(item_start, |e| e + 1),
                    item_start.saturating_sub(1),
                );
                if gap.0 > gap.1 || !self.try_emit_raw_range(gap, out) {
                    // The separator stands in for the blank source line(s).
                    out.push(Row {
                        line: Line::default(),
                        source: (gap.0 <= gap.1).then_some(gap.0 + 1),
                    });
                }
            }
            first = false;
            prev_end = Some(item.data.borrow().sourcepos.end.line.saturating_sub(1));

            // Advance the ordinal once per item, whether it renders raw or preview.
            let ordinal = number;
            if ordered {
                number += 1;
            }
            // The active item renders raw (its source already carries its marker).
            if self.try_emit_raw(item, out) {
                continue;
            }
            let item_first = out.len();

            let value = item.data.borrow().value.clone();
            let (marker, marker_style) = match &value {
                NodeValue::TaskItem(task) => {
                    if task.symbol.is_some() {
                        ("\u{25a0} ".to_string(), self.theme.task_done)
                    } else {
                        ("\u{25a1} ".to_string(), self.theme.task_todo)
                    }
                }
                _ if ordered => (format!("{ordinal}{delim} "), self.theme.list_marker),
                _ => ("\u{2022} ".to_string(), self.theme.list_marker),
            };

            let marker_w = UnicodeWidthStr::width(marker.as_str());
            let inner_w = width.saturating_sub(marker_w).max(1);
            let mut item_lines = Vec::new();
            self.render_item(item, inner_w, &mut item_lines);

            // An empty item stays visible: bullets and numbers show their raw
            // marker (`-`, `1.`), while a task keeps its checkbox glyph (☐/☑).
            let is_empty = item_lines
                .iter()
                .all(|r| r.line.spans.iter().all(|s| s.content.trim().is_empty()));
            if is_empty {
                let (text, style) = match &value {
                    NodeValue::TaskItem(task) if task.symbol.is_some() => {
                        ("\u{25a0}".to_string(), self.theme.task_done)
                    }
                    NodeValue::TaskItem(_) => ("\u{25a1}".to_string(), self.theme.task_todo),
                    _ if ordered => (format!("{ordinal}{delim}"), self.theme.list_marker),
                    _ => (bullet.to_string(), self.theme.list_marker),
                };
                out.push(Row {
                    line: Line::from(Span::styled(text, style)),
                    source: Some(item_start + 1),
                });
                continue;
            }

            let first_prefix = vec![Span::styled(marker, marker_style)];
            let cont_prefix = vec![Span::raw(" ".repeat(marker_w))];
            out.extend(prefix_lines(item_lines, first_prefix, cont_prefix));
            // Label the item's marker row, unless its first child recorded
            // something more precise (e.g. an item starting with a sub-block).
            if let Some(row) = out.get_mut(item_first)
                && row.source.is_none()
            {
                row.source = Some(item_start + 1);
            }
        }
    }

    /// Render a list item's blocks: the first paragraph inline (so it sits on
    /// the marker line); everything else as nested blocks.
    fn render_item<'a>(&self, item: &'a AstNode<'a>, width: usize, out: &mut Vec<Row>) {
        for (i, child) in item.children().enumerate() {
            let is_paragraph = matches!(child.data.borrow().value, NodeValue::Paragraph);
            if i == 0 && is_paragraph {
                let segs = self.inline_segs(child, self.theme.text);
                out.extend(unlabeled(wrap(&segs, width, true)));
            } else {
                self.render_block(child, width, out);
            }
        }
    }

    fn render_table<'a>(
        &self,
        node: &'a AstNode<'a>,
        alignments: &[TableAlignment],
        width: usize,
        out: &mut Vec<Row>,
    ) {
        // Collect rows of cells as styled inline segments so per-cell formatting
        // (bold, code, links…) is preserved, not flattened to plain text. Each
        // pipe row keeps its source line so the grid rows can be labeled.
        let mut rows: Vec<(bool, usize, Vec<Vec<Seg>>)> = Vec::new();
        for row in node.children() {
            let is_header = matches!(row.data.borrow().value, NodeValue::TableRow(true));
            let source_line = row.data.borrow().sourcepos.start.line;
            let cells = row
                .children()
                .map(|cell| self.inline_segs(cell, self.theme.text))
                .collect();
            rows.push((is_header, source_line, cells));
        }
        let columns = alignments
            .len()
            .max(rows.iter().map(|r| r.2.len()).max().unwrap_or(0));
        if columns == 0 {
            return;
        }

        // Natural column widths, then shrink to fit `width` (accounting for
        // borders "+ x +" and one space of padding each side).
        let mut col_w = vec![0usize; columns];
        for (_, _, cells) in &rows {
            for (i, c) in cells.iter().enumerate() {
                col_w[i] = col_w[i].max(segs_width(c));
            }
        }
        let chrome = columns + 1 + 2 * columns; // separators + padding
        let budget = width.saturating_sub(chrome).max(columns);
        shrink_to_fit(&mut col_w, budget);

        let border = self.theme.table_border;
        let line = |l: &str, m: &str, r: &str| -> Line<'static> {
            let mut s = String::from(l);
            for (i, w) in col_w.iter().enumerate() {
                if i > 0 {
                    s.push_str(m);
                }
                s.push_str(&"\u{2500}".repeat(w + 2));
            }
            s.push_str(r);
            Line::from(Span::styled(s, border))
        };

        // Borders are chrome with no source line; each grid row carries its
        // pipe row's line, and the header separator the `|---|` delimiter's.
        out.push(Row::unlabeled(line("\u{256d}", "\u{252c}", "\u{256e}")));
        let empty: Vec<Seg> = Vec::new();
        for (ri, (is_header, source_line, cells)) in rows.iter().enumerate() {
            let mut spans = vec![Span::styled("\u{2502}", border)];
            // Header cells layer the theme's header modifier (bold) over their
            // own inline styles.
            let extra = is_header.then_some(self.theme.table_header.add_modifier);
            for (ci, w) in col_w.iter().enumerate() {
                let cell = cells.get(ci).unwrap_or(&empty);
                let align = alignments.get(ci).copied().unwrap_or(TableAlignment::None);
                spans.push(Span::raw(" "));
                spans.extend(fit_segs(cell, *w, align, extra));
                spans.push(Span::raw(" "));
                spans.push(Span::styled("\u{2502}", border));
            }
            out.push(Row {
                line: Line::from(spans),
                source: Some(*source_line),
            });
            if *is_header && ri == 0 {
                out.push(Row {
                    line: line("\u{251c}", "\u{253c}", "\u{2524}"),
                    source: Some(source_line + 1),
                });
            }
        }
        out.push(Row::unlabeled(line("\u{2570}", "\u{2534}", "\u{256f}")));
    }

    // --- inline ---

    fn inline_segs<'a>(&self, node: &'a AstNode<'a>, base: Style) -> Vec<Seg> {
        let mut out = Vec::new();
        self.walk_inline(node, base, &mut out);
        out
    }

    fn walk_inline<'a>(&self, node: &'a AstNode<'a>, style: Style, out: &mut Vec<Seg>) {
        for child in node.children() {
            let value = child.data.borrow().value.clone();
            match value {
                NodeValue::Text(t) => push_keyword_segs(t.as_ref(), style, self.theme, out),
                NodeValue::Code(c) => out.push(Seg {
                    text: c.literal,
                    style: self.theme.code_inline,
                }),
                // A single source newline renders as a space (markdown reflow),
                // or as a visible line break when `hard_breaks` is enabled.
                NodeValue::SoftBreak => out.push(Seg {
                    text: if self.theme.hard_breaks { "\n" } else { " " }.into(),
                    style,
                }),
                NodeValue::LineBreak => out.push(Seg {
                    text: "\n".into(),
                    style,
                }),
                NodeValue::Emph => {
                    self.walk_inline(child, style.add_modifier(Modifier::ITALIC), out)
                }
                NodeValue::Strong => {
                    self.walk_inline(child, style.add_modifier(Modifier::BOLD), out)
                }
                NodeValue::Strikethrough => {
                    self.walk_inline(child, style.add_modifier(Modifier::CROSSED_OUT), out)
                }
                NodeValue::Underline => {
                    self.walk_inline(child, style.add_modifier(Modifier::UNDERLINED), out)
                }
                NodeValue::Link(_) => self.walk_inline(child, self.theme.link, out),
                NodeValue::Image(_) => {
                    // ASCII marker: picture glyphs (e.g. U+1F5BC) have
                    // ambiguous terminal width and break table alignment.
                    out.push(Seg {
                        text: "[img] ".into(),
                        style: self.theme.link,
                    });
                    self.walk_inline(child, self.theme.link, out);
                }
                NodeValue::TaskItem(task) => {
                    let (glyph, st) = if task.symbol.is_some() {
                        ("\u{25a0} ", self.theme.task_done)
                    } else {
                        ("\u{25a1} ", self.theme.task_todo)
                    };
                    out.push(Seg {
                        text: glyph.into(),
                        style: st,
                    });
                }
                NodeValue::HtmlInline(s) => out.push(Seg {
                    text: s,
                    style: self.theme.html,
                }),
                NodeValue::Raw(s) => out.push(Seg { text: s, style }),
                NodeValue::Escaped => self.walk_inline(child, style, out),
                // Use the source label, not comrak's numeric index, so the
                // reference visibly matches its `[name]:` definition.
                NodeValue::FootnoteReference(r) => out.push(Seg {
                    text: format!("[{}]", r.name),
                    style: self.theme.link,
                }),
                _ => self.walk_inline(child, style, out),
            }
        }
    }
}

/// Replace the `from` style with `to` on every span that uses it, leaving other
/// styles (emphasis, code, links) intact. Used to tint plain block-quote text.
fn restyle(rows: Vec<Row>, from: Style, to: Style) -> Vec<Row> {
    rows.into_iter()
        .map(|row| {
            let spans: Vec<Span<'static>> = row
                .line
                .spans
                .into_iter()
                .map(|mut s| {
                    if s.style == from {
                        s.style = to;
                    }
                    s
                })
                .collect();
            Row {
                line: Line::from(spans),
                source: row.source,
            }
        })
        .collect()
}

fn heading_glyph(level: u8) -> &'static str {
    // Two tiers of descending visual weight, all width-1 and text-only (no
    // emoji-presentation): diamonds for H1-H3 (solid -> pip -> hollow),
    // circles for H4-H6 (solid -> hollow -> small bullet).
    match level {
        1 => "\u{25c6}", // ◆
        2 => "\u{25c8}", // ◈
        3 => "\u{25c7}", // ◇
        4 => "\u{25cf}", // ●
        5 => "\u{25cb}", // ○
        _ => "\u{25e6}", // ◦
    }
}

fn push_keyword_segs(text: &str, base: Style, theme: &MarkdownTheme, out: &mut Vec<Seg>) {
    let mut last = 0;
    let mut i = 0;
    while i < text.len() {
        let Some((start, end, style)) = keyword_at(text, i, theme) else {
            i += text[i..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        if start > last {
            out.push(Seg {
                text: text[last..start].to_string(),
                style: base,
            });
        }
        out.push(Seg {
            text: text[start..end].to_string(),
            style,
        });
        last = end;
        i = end;
    }
    if last < text.len() {
        out.push(Seg {
            text: text[last..].to_string(),
            style: base,
        });
    }
}

fn keyword_at(text: &str, at: usize, theme: &MarkdownTheme) -> Option<(usize, usize, Style)> {
    if !text.is_char_boundary(at) || !is_word_boundary_before(text, at) {
        return None;
    }
    for (word, style) in [
        ("IMPORTANT", theme.keyword_misc),
        ("WARNING", theme.keyword_warn),
        ("FIXME", theme.keyword_error),
        ("ERROR", theme.keyword_error),
        ("WARN", theme.keyword_warn),
        ("TODO", theme.keyword_note),
        ("NOTE", theme.keyword_note),
        ("HACK", theme.keyword_misc),
    ] {
        let end = at + word.len();
        if text[at..].starts_with(word) && is_word_boundary_after(text, end) {
            return Some((at, end, style));
        }
    }
    None
}

fn is_word_boundary_before(text: &str, at: usize) -> bool {
    at == 0
        || text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn is_word_boundary_after(text: &str, at: usize) -> bool {
    at >= text.len()
        || text[at..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

/// Prefix each row of `rows`: the first with `first`, the rest with `cont`.
fn prefix_lines(rows: Vec<Row>, first: Vec<Span<'static>>, cont: Vec<Span<'static>>) -> Vec<Row> {
    rows.into_iter()
        .enumerate()
        .map(|(i, mut row)| {
            let mut spans = if i == 0 { first.clone() } else { cont.clone() };
            spans.append(&mut row.line.spans);
            Row {
                line: Line::from(spans),
                source: row.source,
            }
        })
        .collect()
}

/// Word-wrap (or hard-wrap) styled segments to `width`, returning styled lines.
fn wrap(segs: &[Seg], width: usize, word_wrap: bool) -> Vec<Line<'static>> {
    let width = width.max(1);
    // Flatten to graphemes, carrying style and width.
    struct G {
        s: String,
        style: Style,
        w: usize,
    }
    let mut graphemes = Vec::new();
    for seg in segs {
        for g in seg.text.graphemes(true) {
            if g == "\n" {
                graphemes.push(G {
                    s: "\n".into(),
                    style: seg.style,
                    w: 0,
                });
            } else {
                graphemes.push(G {
                    s: g.to_string(),
                    style: seg.style,
                    w: UnicodeWidthStr::width(g),
                });
            }
        }
    }

    let mut rows: Vec<Vec<G>> = Vec::new();
    let mut cur: Vec<G> = Vec::new();
    let mut col = 0usize;
    let mut last_break: Option<usize> = None;
    for g in graphemes {
        if g.s == "\n" {
            rows.push(std::mem::take(&mut cur));
            col = 0;
            last_break = None;
            continue;
        }
        if col + g.w > width && !cur.is_empty() {
            if word_wrap && let Some(bp) = last_break {
                let rest = cur.split_off(bp);
                rows.push(std::mem::take(&mut cur));
                col = rest.iter().map(|x| x.w).sum();
                cur = rest;
            } else {
                rows.push(std::mem::take(&mut cur));
                col = 0;
            }
            last_break = None;
        }
        let is_space = g.s == " ";
        col += g.w;
        cur.push(g);
        if is_space {
            last_break = Some(cur.len());
        }
    }
    rows.push(cur);

    rows.into_iter()
        .map(|row| merge_spans(row.into_iter().map(|g| (g.s, g.style))))
        .collect()
}

/// Merge a stream of (text, style) into a `Line`, coalescing equal styles.
/// Shared by the preview, hybrid-view, and code-block renderers.
pub(crate) fn merge_spans(items: impl Iterator<Item = (String, Style)>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut iter = items.peekable();
    while let Some((mut text, style)) = iter.next() {
        while let Some((next_text, next_style)) = iter.peek() {
            if *next_style == style {
                text.push_str(next_text);
                iter.next();
            } else {
                break;
            }
        }
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

/// Total display width of a cell's segments.
fn segs_width(segs: &[Seg]) -> usize {
    segs.iter()
        .map(|s| UnicodeWidthStr::width(s.text.as_str()))
        .sum()
}

/// Fit a cell's styled segments into `w` display columns with the given
/// alignment — truncating with an ellipsis when too long, padding with spaces
/// when short — while preserving each segment's style. `extra` (bold for header
/// cells) is layered on top of every segment's own style.
fn fit_segs(
    segs: &[Seg],
    w: usize,
    align: TableAlignment,
    extra: Option<Modifier>,
) -> Vec<Span<'static>> {
    let style = |s: Style| extra.map_or(s, |m| s.add_modifier(m));
    let total = segs_width(segs);

    if total > w {
        // Truncate to leave one column for the ellipsis.
        let mut spans = Vec::new();
        let mut used = 0;
        let mut full = false;
        for seg in segs {
            if full {
                break;
            }
            let mut text = String::new();
            for g in seg.text.graphemes(true) {
                let gw = UnicodeWidthStr::width(g);
                if used + gw + 1 > w {
                    full = true;
                    break;
                }
                text.push_str(g);
                used += gw;
            }
            if !text.is_empty() {
                spans.push(Span::styled(text, style(seg.style)));
            }
        }
        spans.push(Span::styled("\u{2026}", style(Style::default())));
        used += 1;
        if used < w {
            spans.push(Span::raw(" ".repeat(w - used)));
        }
        return spans;
    }

    let pad = w - total;
    let (left, right) = match align {
        TableAlignment::Right => (pad, 0),
        TableAlignment::Center => (pad / 2, pad - pad / 2),
        _ => (0, pad),
    };
    let mut spans = Vec::new();
    if left > 0 {
        spans.push(Span::raw(" ".repeat(left)));
    }
    for seg in segs {
        if !seg.text.is_empty() {
            spans.push(Span::styled(seg.text.clone(), style(seg.style)));
        }
    }
    if right > 0 {
        spans.push(Span::raw(" ".repeat(right)));
    }
    spans
}

/// Shrink column widths until their sum fits `budget` by lowering the widest
/// columns to a common ceiling (narrow columns keep their width). Computed by
/// binary-searching the ceiling rather than shaving one cell per iteration, so
/// a pasted multi-kilobyte cell cannot stall every render.
fn shrink_to_fit(col_w: &mut [usize], budget: usize) {
    if col_w.iter().sum::<usize>() <= budget {
        return;
    }
    let fits = |cap: usize| col_w.iter().map(|w| (*w).min(cap)).sum::<usize>() <= budget;
    // Below a ceiling of 1 there is nothing left to shrink.
    if !fits(1) {
        for w in col_w.iter_mut() {
            *w = (*w).min(1);
        }
        return;
    }
    let (mut lo, mut hi) = (1, *col_w.iter().max().unwrap());
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let cap = lo;
    // Hand any leftover budget back to the clamped columns, one cell each.
    let mut slack = budget - col_w.iter().map(|w| (*w).min(cap)).sum::<usize>();
    for w in col_w.iter_mut().filter(|w| **w > cap) {
        *w = cap + usize::from(slack > 0);
        slack = slack.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deeply_nested_quotes_render_without_overflowing() {
        let src = format!("{} deep", ">".repeat(20_000));
        let out = render_plain(&src, 40);
        assert!(out.contains('\u{2026}'));
    }

    fn render_plain(src: &str, width: usize) -> String {
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        render(src, width, &theme, &hl)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn body_text_has_an_explicit_foreground() {
        // Regression: body text must not defer to the terminal default (which
        // can render as a dim gray), so that text after a styled run (e.g. an
        // inline code span) stays clearly readable.
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let lines = render("and `code`, then more text", 40, &theme, &hl);
        let after = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("then more"))
            .expect("trailing text span");
        assert_ne!(after.style.fg, Some(ratatui::style::Color::Reset));
        assert!(after.style.fg.is_some());
    }

    #[test]
    fn heading_markers_are_stripped() {
        let text = render_plain("# Hello world", 40);
        assert!(text.contains("Hello world"));
        assert!(!text.contains('#'));
    }

    #[test]
    fn heading_glyphs_are_preview_only() {
        let text = render_plain("# Hello world", 40);
        assert!(text.contains('\u{25c6}'));
        assert!(text.contains("Hello world"));
    }

    #[test]
    fn emphasis_markers_are_stripped() {
        let text = render_plain("a **bold** and *italic* word", 40);
        assert!(!text.contains('*'));
        assert!(text.contains("bold"));
        assert!(text.contains("italic"));
    }

    #[test]
    fn task_list_shows_checkboxes_not_brackets() {
        let text = render_plain("- [x] done\n- [ ] todo", 40);
        assert!(text.contains('\u{25a0}')); // checked box ■
        assert!(text.contains('\u{25a1}')); // empty box □
        assert!(!text.contains("[x]"));
    }

    #[test]
    fn long_paragraph_wraps_to_width() {
        let text = render_plain("one two three four five six seven eight nine ten", 12);
        for line in text.lines() {
            assert!(
                UnicodeWidthStr::width(line) <= 12,
                "line too wide: {line:?}"
            );
        }
    }

    #[test]
    fn table_renders_a_grid() {
        let src = "| A | B |\n| - | - |\n| 1 | 2 |";
        let text = render_plain(src, 40);
        assert!(text.contains('\u{2502}')); // vertical border │
        assert!(text.contains('\u{256d}')); // top-left corner ╭
    }

    #[test]
    fn table_cells_keep_inline_styles() {
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let src = "| Name | Note |\n| --- | --- |\n| **bob** | `x` |\n";
        let (mut bold_bob, mut code_x) = (false, false);
        for line in render(src, 40, &theme, &hl) {
            for s in &line.spans {
                if s.content.as_ref() == "bob" && s.style.add_modifier.contains(Modifier::BOLD) {
                    bold_bob = true;
                }
                if s.content.as_ref() == "x" && s.style == theme.code_inline {
                    code_x = true;
                }
            }
        }
        assert!(bold_bob, "a **bold** table cell should render bold");
        assert!(code_x, "a `code` table cell should use the code style");
    }

    #[test]
    fn keywords_receive_special_styles() {
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        let lines = render("TODO and ERROR", 40, &theme, &hl);
        let mut saw_todo = false;
        let mut saw_error = false;
        for span in &lines[0].spans {
            if span.content.as_ref() == "TODO" && span.style == theme.keyword_note {
                saw_todo = true;
            }
            if span.content.as_ref() == "ERROR" && span.style == theme.keyword_error {
                saw_error = true;
            }
        }
        assert!(saw_todo);
        assert!(saw_error);
    }

    #[test]
    fn footnote_reference_and_definition_are_visible() {
        let text = render_plain("Here is a note[^1].\n\n[^1]: the note body.", 60);
        // The reference marker renders as a visible index rather than vanishing,
        // and the definition keeps a matching label.
        assert!(
            text.contains("[1]"),
            "footnote reference should render: {text:?}"
        );
        assert!(
            text.contains("[1]:"),
            "footnote definition should keep its label: {text:?}"
        );
        assert!(text.contains("the note body"));
    }

    #[test]
    fn empty_list_items_show_their_raw_marker() {
        // An empty bullet / number / checkbox item shows its raw source marker
        // instead of a prettified glyph, so the empty item stays visible.
        let bullet = render_plain("- a\n- \n- b", 40);
        assert!(
            bullet.lines().any(|l| l.trim() == "-"),
            "empty bullet -> '-':\n{bullet}"
        );
        let num = render_plain("1. x\n2. \n3. z", 40);
        assert!(
            num.lines().any(|l| l.trim() == "2."),
            "empty number -> '2.':\n{num}"
        );
        let task = render_plain("- [ ] a\n- [ ] ", 40);
        assert!(
            task.lines().any(|l| l.trim() == "\u{25a1}"),
            "empty unchecked task -> checkbox glyph:\n{task}"
        );
    }

    fn rows_with_sources(src: &str, width: usize) -> Vec<(String, Option<usize>)> {
        let theme = MarkdownTheme::default();
        let hl = CodeHighlighter::new(None);
        render_rows(src, width, &theme, &hl)
            .iter()
            .map(|r| {
                let text: String = r.line.spans.iter().map(|s| s.content.as_ref()).collect();
                (text, r.source)
            })
            .collect()
    }

    #[test]
    fn every_list_item_carries_its_source_line() {
        let rows = rows_with_sources("- a\n- b\n- c", 40);
        let sources: Vec<Option<usize>> = rows.iter().map(|(_, s)| *s).collect();
        assert_eq!(sources, vec![Some(1), Some(2), Some(3)], "{rows:?}");
    }

    #[test]
    fn loose_and_nested_list_items_carry_source_lines() {
        // 1: "1. one", 2: blank, 3: "2. two", 4: nested "- sub"
        let rows = rows_with_sources("1. one\n\n2. two\n   - sub", 40);
        for expect in [1, 2, 3, 4] {
            assert!(
                rows.iter().any(|(_, s)| *s == Some(expect)),
                "line {expect} should label a row: {rows:?}"
            );
        }
    }

    #[test]
    fn table_rows_carry_source_lines_but_borders_do_not() {
        // 1: header, 2: delimiter, 3-4: data rows.
        let src = "| A | B |\n| - | - |\n| 1 | 2 |\n| 3 | 4 |";
        let rows = rows_with_sources(src, 40);
        let sources: Vec<Option<usize>> = rows.iter().map(|(_, s)| *s).collect();
        assert_eq!(
            sources,
            vec![None, Some(1), Some(2), Some(3), Some(4), None],
            "top border, header, separator, two data rows, bottom border: {rows:?}"
        );
    }

    #[test]
    fn wrapped_paragraph_labels_only_its_first_row() {
        let rows = rows_with_sources("one two three four five six seven eight nine ten", 12);
        assert!(rows.len() > 1, "paragraph should wrap: {rows:?}");
        assert_eq!(rows[0].1, Some(1));
        assert!(
            rows[1..].iter().all(|(_, s)| s.is_none()),
            "continuation rows stay unlabeled: {rows:?}"
        );
    }

    #[test]
    fn code_block_rows_label_every_literal_line() {
        // 1: fence, 2-3: literal lines, 4: fence.
        let rows = rows_with_sources("```rust\nlet a = 1;\nlet b = 2;\n```", 40);
        let sources: Vec<Option<usize>> = rows.iter().map(|(_, s)| *s).collect();
        assert_eq!(sources, vec![Some(2), Some(3)], "{rows:?}");
    }

    #[test]
    fn hard_wrapped_code_labels_only_the_row_starting_the_line() {
        // Line 2 is 20 columns and hard-wraps at width 10 into two rows.
        let rows = rows_with_sources("```\naaaaaaaaaaaaaaaaaaaa\nbb\n```", 10);
        let sources: Vec<Option<usize>> = rows.iter().map(|(_, s)| *s).collect();
        assert_eq!(sources, vec![Some(2), None, Some(3)], "{rows:?}");
    }

    #[test]
    fn soft_break_mode_controls_line_breaks() {
        let hl = CodeHighlighter::new(None);
        let joined = |theme: &MarkdownTheme| -> bool {
            render("first line\nsecond line", 40, theme, &hl)
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .any(|row| row.contains("first line") && row.contains("second line"))
        };
        // Default: a single newline reflows onto one row (collapses to a space).
        assert!(joined(&MarkdownTheme::default()), "default should reflow");
        // hard_breaks: the newline becomes a visible line break.
        let hard = MarkdownTheme {
            hard_breaks: true,
            ..MarkdownTheme::default()
        };
        assert!(
            !joined(&hard),
            "hard_breaks should split onto separate rows"
        );
    }
}
