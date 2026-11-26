//! Rendering. Also records the screen regions the app uses for mouse hit-testing.

use crate::app::{App, CommentInput, Target, WtState};
use crate::comments::{Comment, Id};
use crate::diff::{Diff, LineKind};
use crate::keymap::{BINDINGS, MOUSE_HELP, Section};
use crate::theme::Theme;
use crate::tree::RowKind;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 12;
const TREE_MIN_WIDTH: u16 = 20;
const TREE_MID_WIDTH: u16 = 30;
const TREE_MAX_WIDTH: u16 = 50;
const MID_BREAKPOINT: u16 = 100;
const WIDE_BREAKPOINT: u16 = 120;
const COMMENTS_MIN_WIDTH: u16 = 24;
/// The comments pane is hidden when the diff would get narrower than this.
const DIFF_MIN_WIDTH: u16 = 40;

/// The file panel is pinned to a fixed width on narrow and medium terminals, with the diff taking
/// the rest. From `WIDE_BREAKPOINT` on the file panel takes a quarter of the width, capped at `TREE_MAX_WIDTH`.
fn panel_constraints(width: u16) -> [Constraint; 2] {
    if width < MID_BREAKPOINT {
        [Constraint::Length(TREE_MIN_WIDTH), Constraint::Min(0)]
    } else if width < WIDE_BREAKPOINT {
        [Constraint::Length(TREE_MID_WIDTH), Constraint::Min(0)]
    } else {
        [
            Constraint::Length((width / 4).min(TREE_MAX_WIDTH)),
            Constraint::Min(0),
        ]
    }
}

/// Width of the comments pane next to the tree in a `width` wide window, if it fits: like the
/// tree a quarter of the width, within `COMMENTS_MIN_WIDTH..=TREE_MAX_WIDTH`.
fn comments_width(width: u16) -> Option<u16> {
    let [_, rest] = Layout::horizontal(panel_constraints(width)).areas(Rect::new(0, 0, width, 1));
    let w = (width / 4).clamp(COMMENTS_MIN_WIDTH, TREE_MAX_WIDTH);
    (rest.width >= w + DIFF_MIN_WIDTH).then_some(w)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::new().style(app.theme.base), area);
    app.hit = Default::default();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let msg = format!(
            "Terminal too small ({}x{}); need {MIN_WIDTH}x{MIN_HEIGHT}",
            area.width, area.height
        );
        f.render_widget(Paragraph::new(msg).style(app.theme.error), area);
        return;
    }
    let commits_h = (area.height / 4).clamp(5, 12);
    let [tabs, main, commits, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(commits_h),
        Constraint::Length(1),
    ])
    .areas(area);
    let [tree, mut diff] = Layout::horizontal(panel_constraints(main.width)).areas(main);
    let mut comments = None;
    if let Some(w) = comments_width(main.width).filter(|_| app.comments_pane) {
        let [d, c] = Layout::horizontal([Constraint::Min(0), Constraint::Length(w)]).areas(diff);
        (diff, comments) = (d, Some(c));
    }

    draw_tabs(f, app, tabs);
    draw_tree(f, app, tree);
    draw_diff(f, app, diff);
    if let Some(area) = comments {
        draw_comments(f, app, area);
    }
    draw_commits(f, app, commits);
    draw_status(f, app, status);
    if app.help {
        draw_help(f, app, area);
    }
}

// ---- text helpers ----------------------------------------------------------------

/// Truncates styled spans to `width` display columns and pads the rest with spaces,
/// so a line-level style (e.g. the cursor highlight) spans the full row.
fn fit_spans<'a>(spans: Vec<Span<'a>>, width: usize) -> Vec<Span<'a>> {
    let mut out = Vec::with_capacity(spans.len() + 1);
    let mut used = 0;
    for span in spans {
        if used >= width {
            break;
        }
        let mut text = String::new();
        for c in span.content.chars() {
            let w = c.width().unwrap_or(0);
            if used + w > width {
                used = width;
                break;
            }
            used += w;
            text.push(c);
        }
        out.push(Span::styled(text, span.style));
    }
    if used < width {
        out.push(Span::raw(" ".repeat(width - used)));
    }
    out
}

/// Skips `skip` display columns of `s` (for horizontal scrolling).
fn skip_cols(s: &str, skip: usize) -> &str {
    let mut cols = 0;
    for (i, c) in s.char_indices() {
        if cols >= skip {
            return &s[i..];
        }
        cols += c.width().unwrap_or(0);
    }
    ""
}

fn pane_block<'a>(title: String, theme: &Theme) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.border)
        .title(Span::styled(title, theme.title))
}

/// Patches `style` onto every span: span styles may carry their own bg
/// (e.g. Dracula), which would otherwise mask a line-level highlight.
fn patch_row(spans: Vec<Span<'_>>, style: Style) -> Line<'_> {
    Line::from(
        spans
            .into_iter()
            .map(|s| Span::styled(s.content, s.style.patch(style)))
            .collect::<Vec<_>>(),
    )
}

fn message(f: &mut Frame, area: Rect, text: &str, style: Style) {
    f.render_widget(
        Paragraph::new(text.to_string())
            .style(style)
            .wrap(ratatui::widgets::Wrap { trim: true }),
        area,
    );
}

fn clamp_offset(offset: &mut usize, len: usize, height: usize) {
    *offset = (*offset).min(len.saturating_sub(height));
}

/// Keeps the end of `s` (the interesting part of a path), prefixing `…` when it has to cut.
fn shorten_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - max.saturating_sub(1)).collect();
    format!("…{tail}")
}

fn home_relative(p: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rel) = p.strip_prefix(&home)
    {
        return format!("~/{}", rel.display());
    }
    p.display().to_string()
}

// ---- panes ------------------------------------------------------------------------

fn draw_tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let labels: Vec<(String, Option<(String, Style)>)> = app
        .wts
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let key = if i < 9 {
                format!("{} ", i + 1)
            } else {
                String::new()
            };
            let badge = if !w.info.exists() {
                Some(("✗".to_string(), theme.error))
            } else if let Some(c) = &w.conflict {
                let changes = if w.changes.is_empty() {
                    String::new()
                } else {
                    format!(" ●{}", w.changes.len())
                };
                Some((format!("⚠{}{changes}", c.files.len()), theme.error))
            } else if !w.changes.is_empty() {
                Some((format!("●{}", w.changes.len()), theme.badge))
            } else {
                None
            };
            (format!(" {key}{}", w.info.label()), badge)
        })
        .collect();
    let widths: Vec<u16> = labels
        .iter()
        .map(|(l, b)| {
            (unicode_width::UnicodeWidthStr::width(l.as_str())
                + b.as_ref().map_or(0, |(b, _)| b.chars().count() + 1)
                + 1) as u16
        })
        .collect();
    // Scroll the tab strip so the active tab is visible.
    let mut start = 0;
    while start < app.active
        && widths[start..=app.active]
            .iter()
            .map(|w| w + 1)
            .sum::<u16>()
            > area.width
    {
        start += 1;
    }
    let mut spans = Vec::new();
    let mut x = area.x;
    for (i, (label, badge)) in labels.into_iter().enumerate().skip(start) {
        if x >= area.right() {
            break;
        }
        let style = if i == app.active {
            theme.tab_active
        } else {
            theme.tab
        };
        spans.push(Span::styled(label, style));
        if let Some((b, bs)) = badge {
            spans.push(Span::styled(" ", style));
            spans.push(Span::styled(b, if i == app.active { style } else { bs }));
        }
        spans.push(Span::styled(" ", style));
        spans.push(Span::raw(" "));
        let w = widths[i].min(area.right() - x);
        app.hit.tabs.push((Rect::new(x, area.y, w, 1), i));
        x = x.saturating_add(widths[i] + 1);
    }
    f.render_widget(Line::from(fit_spans(spans, area.width as usize)), area);
}

fn draw_tree(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let Some(st) = app.wts.get_mut(app.active) else {
        return;
    };
    let title = match &st.commit {
        Some(c) => format!(" Files @ {} · {} ", c.short, st.commit_files.len()),
        None => format!(" Changes · {} ", st.changes.len()),
    };
    let block = pane_block(title, &theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.hit.tree = inner;
    let st = &mut app.wts[app.active];

    if let Some(err) = &st.error {
        return message(f, inner, err, theme.error);
    }
    if st.rows.is_empty() {
        let text = match (&st.commit, st.loaded) {
            (_, false) => "Loading…",
            (Some(_), _) => "No file changes in this commit",
            (None, true) => "Working tree clean",
        };
        return message(f, inner, text, theme.dim);
    }
    let h = inner.height as usize;
    let w = inner.width as usize;
    clamp_offset(&mut st.tree_offset, st.rows.len(), h);
    let files = st.files();
    let lines: Vec<Line> = st
        .rows
        .iter()
        .enumerate()
        .skip(st.tree_offset)
        .take(h)
        .map(|(i, row)| {
            let mut spans = vec![Span::raw("  ".repeat(row.depth))];
            match &row.kind {
                RowKind::Dir { expanded, .. } => {
                    spans.push(Span::styled(if *expanded { "▾ " } else { "▸ " }, theme.dim));
                    spans.push(Span::styled(format!("{}/", row.name), theme.dir));
                }
                RowKind::File(idx) => {
                    let fc = &files[*idx];
                    spans.push(Span::styled(
                        format!("{} ", fc.status.letter()),
                        theme.file_status(fc.status),
                    ));
                    spans.push(Span::styled(row.name.clone(), theme.base));
                    if let Some(old) = &fc.old_path {
                        spans.push(Span::styled(format!(" ← {old}"), theme.dim));
                    }
                }
            }
            let spans = fit_spans(spans, w);
            if i == st.tree_cursor {
                patch_row(spans, theme.cursor)
            } else {
                Line::from(spans)
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_diff(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let wrap = app.wrap;
    let Some(st) = app.wts.get_mut(app.active) else {
        return;
    };
    let title = match &st.file {
        Some(file) => {
            let stats = if st.diff.lines.is_empty() {
                String::new()
            } else {
                format!(" +{} -{}", st.diff.added, st.diff.removed)
            };
            let comments = match (st.anchors.total(), st.anchors.outdated) {
                (0, _) => String::new(),
                (n, o) => {
                    let s = if n == 1 { "" } else { "s" };
                    let outdated = if o > 0 {
                        format!(" ({o} outdated)")
                    } else {
                        String::new()
                    };
                    format!(" · {n} comment{s}{outdated}")
                }
            };
            let wrapped = if wrap { " · wrap" } else { "" };
            format!(" {}{stats}{comments}{wrapped} ", file.path)
        }
        None => " Diff ".to_string(),
    };
    let block = pane_block(title, &theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.hit.diff = inner;
    let input = app.input.as_ref();
    let st = &mut app.wts[app.active];

    if let Some(err) = &st.diff_error {
        return message(f, inner, err, theme.error);
    }
    if st.file.is_none() {
        return message(
            f,
            inner,
            if st.rows.is_empty() {
                ""
            } else {
                "Select a file"
            },
            theme.dim,
        );
    }
    if st.diff.lines.is_empty() {
        let text = if st.diff_raw.is_some() {
            "No textual changes"
        } else {
            "Loading…"
        };
        return message(f, inner, text, theme.dim);
    }

    let h = inner.height as usize;
    let len = st.diff.lines.len();
    let num_w = num_width(&st.diff);
    let gutter_w = gutter_width(&st.diff);
    let text_w = text_width(&st.diff, inner.width);
    let width = inner.width as usize;
    let rows = |st: &WtState, i: usize| line_rows(st, input, wrap, text_w, width, i);

    let mut off = st.diff_offset.min(len - 1);
    // Without wrapping, don't scroll past the point where the last line reaches the bottom.
    let mut max = len - 1;
    if !wrap {
        let (mut first, mut total) = (len, 0);
        while first > 0 && total < h {
            first -= 1;
            total += rows(st, first);
        }
        max = first;
        off = off.min(max);
    }
    if let Some(sel) = input.map(|i| i.at).or(st.diff_sel) {
        // Scroll so the whole selected (or edited) line, with its comment, fits if it can.
        off = off.min(sel);
        let mut total: usize = (off..=sel).map(|i| rows(st, i)).sum();
        while total > h && off < sel {
            total -= rows(st, off);
            off += 1;
        }
    }
    st.diff_offset = off;
    let st = &*st;
    // Rows of the first line's block scrolled past, so a reply further down its thread shows.
    let skip = diff_skip(st, input, wrap, text_w, width, h);
    let lim = h + skip;

    let num = |n: Option<u32>| n.map_or_else(|| " ".repeat(num_w), |n| format!("{n:>num_w$}"));
    let build = |gutter: String,
                 mark: Option<Style>,
                 marker: &'static str,
                 text: &str,
                 style: Style,
                 selected: bool| {
        let gutter_style = if selected {
            theme.gutter_sel
        } else {
            theme.dim
        };
        let mut spans = vec![
            match mark {
                Some(style) => Span::styled("│", style),
                None => Span::raw(" "),
            },
            Span::styled(gutter, gutter_style),
        ];
        spans.extend(fit_spans(
            vec![
                Span::styled(marker, style),
                Span::styled(text.to_string(), style),
            ],
            text_w + 1,
        ));
        // Span styles carry their own bg (e.g. Dracula), so the selection
        // is patched onto the finished row (gutter and padding included).
        let row_style = if selected {
            theme.diff_sel
        } else {
            Style::new()
        };
        patch_row(spans, row_style)
    };
    let line_style = |kind: LineKind| match kind {
        LineKind::Add => ("+", theme.add),
        LineKind::Del => ("-", theme.del),
        LineKind::Context => (" ", theme.base),
        LineKind::Hunk => ("", theme.hunk),
        LineKind::FileHeader => ("", theme.file_header),
        LineKind::Meta => ("", theme.dim),
    };

    let sel = st.sel_range();
    let mut out = Vec::with_capacity(lim);
    let mut cursor = None;
    for (i, l) in st.diff.lines.iter().enumerate().skip(off) {
        if out.len() >= lim {
            break;
        }
        let (marker, style) = line_style(l.kind);
        let selected = sel.as_ref().is_some_and(|r| r.contains(&i));
        let note = note_at(st, input, i);
        // A bar along the lines a comment covers, running into its box below the last one.
        let being_written = input.is_some_and(|inp| inp.at == i || selected && i < inp.at);
        let mark = if being_written {
            Some(theme.popup_border)
        } else {
            match (&note, st.anchors.covering(i)) {
                (Some(n), _) if n.closed => Some(theme.dim),
                (Some(_), _) => Some(theme.comment),
                (None, Some(ci)) if is_closed(st, ci) => Some(theme.dim),
                (None, Some(_)) => Some(theme.comment),
                (None, None) => None,
            }
        };
        let chunks = if wrap {
            wrap_ranges(&l.text, text_w)
        } else {
            vec![skip_cols(&l.text, st.diff_hscroll)]
        };
        for (k, chunk) in chunks.into_iter().enumerate() {
            if out.len() >= lim {
                break;
            }
            out.push(if k == 0 {
                let gutter = format!("{} {} ", num(l.old), num(l.new));
                build(gutter, mark, marker, chunk, style, selected)
            } else {
                build(" ".repeat(gutter_w - 1), mark, " ", chunk, style, selected)
            });
        }
        let Some(note) = note else { continue };
        let frame = if note.editing {
            theme.popup_border
        } else if note.closed {
            theme.dim
        } else {
            theme.comment
        };
        // A full-width box open on the right: rules above and below separate it from the code.
        let rule = |corner: &str| {
            let rule = format!("{corner}{}", "─".repeat(width.saturating_sub(1)));
            Line::from(fit_spans(vec![Span::styled(rule, frame)], width))
        };
        let row = |text: String, style: Style| {
            let spans = vec![Span::styled("│ ", frame), Span::styled(text, style)];
            Line::from(fit_spans(spans, width))
        };
        let text_style = if note.closed {
            theme.dim
        } else {
            theme.comment_text
        };
        let mut rows = vec![rule("├")];
        for r in note.rows(width) {
            if let Some(col) = r.cursor
                && out.len() + rows.len() < lim
            {
                cursor = Some((2 + r.indent + col, out.len() + rows.len()));
            }
            let seg = &note.segs[r.seg];
            let style = if seg.cursor.is_some() {
                theme.base
            } else {
                text_style
            };
            rows.push(thread_row(&r, frame, style, seg.selected, width, &theme));
        }
        if note.editing {
            rows.push(row(
                "Enter save · Esc cancel · empty deletes".into(),
                theme.dim,
            ));
        }
        rows.push(rule("└"));
        out.extend(rows.into_iter().take(lim - out.len()));
    }
    out.drain(..skip.min(out.len()));
    f.render_widget(Paragraph::new(out), inner);
    if let Some((x, y)) = cursor.and_then(|(x, y): (usize, usize)| Some((x, y.checked_sub(skip)?)))
    {
        let x = (x as u16).min(inner.width.saturating_sub(1));
        f.set_cursor_position((inner.x + x, inner.y + y as u16));
    }

    // Fitting the selection may have scrolled a little further than `max`.
    let scroll_range = max.max(st.diff_offset);
    if scroll_range > 0 {
        let mut state = ScrollbarState::new(scroll_range).position(st.diff_offset);
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .style(theme.dim);
        f.render_stateful_widget(
            bar,
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut state,
        );
    }
}

/// One entry of a comment thread: the comment's body or a reply.
struct Seg<'a> {
    /// Author of a reply, shown above it as `↳ by`; `None` for the comment's body.
    by: Option<&'a str>,
    text: &'a str,
    /// Byte offset of the input cursor while this entry is edited.
    cursor: Option<usize>,
    /// The highlighted entry.
    selected: bool,
}

/// A comment thread, shown below a diff line or in the comments pane; while an input is open
/// in it, one entry is the edited text.
struct Note<'a> {
    segs: Vec<Seg<'a>>,
    /// An input is open in the thread.
    editing: bool,
    /// A closed comment, shown dimmed.
    closed: bool,
}

/// A screen row of a thread (after the `│ ` bar): a reply's author or a slice of an entry.
#[derive(Debug, PartialEq, Eq)]
struct ThreadRow<'a> {
    /// Index into `Note::segs`.
    seg: usize,
    /// The author line of a reply, rather than text.
    header: bool,
    text: &'a str,
    /// Columns before `text`.
    indent: usize,
    /// Column of the input cursor within `text`.
    cursor: Option<usize>,
}

/// Columns replies are indented by, below their `↳ ` author line.
const REPLY_INDENT: usize = 2;

impl<'a> Note<'a> {
    /// The thread of `c`; `sel` highlights its body (`Some(None)`) or a reply.
    fn thread(c: &'a Comment, sel: Option<Option<Id>>) -> Self {
        let body = Seg {
            by: None,
            text: &c.body,
            cursor: None,
            selected: sel == Some(None),
        };
        let replies = c.replies.iter().map(|r| Seg {
            by: Some(&r.by),
            text: &r.body,
            cursor: None,
            selected: sel == Some(Some(r.id)),
        });
        Note {
            segs: std::iter::once(body).chain(replies).collect(),
            editing: false,
            closed: c.closed.is_some(),
        }
    }

    /// Wrapped rows of the thread in a pane `width` columns wide.
    fn rows(&self, width: usize) -> Vec<ThreadRow<'a>> {
        let mut out = Vec::new();
        for (k, seg) in self.segs.iter().enumerate() {
            if let Some(by) = seg.by {
                out.push(ThreadRow {
                    seg: k,
                    header: true,
                    text: by,
                    indent: 0,
                    cursor: None,
                });
            }
            let indent = if seg.by.is_some() { REPLY_INDENT } else { 0 };
            let parts: Vec<&str> = seg
                .text
                .split('\n')
                .flat_map(|l| wrap_ranges(l, width.saturating_sub(2 + indent)))
                .collect();
            let at = seg.cursor.and_then(|b| cursor_at(seg.text, &parts, b));
            for (r, text) in parts.into_iter().enumerate() {
                out.push(ThreadRow {
                    seg: k,
                    header: false,
                    text,
                    indent,
                    cursor: at.filter(|&(row, _)| row == r).map(|(_, col)| col),
                });
            }
        }
        out
    }

    /// Screen rows below a diff line: the thread, the key hint while editing, and the rules
    /// above and below.
    fn height(&self, width: usize) -> usize {
        self.rows(width).len() + usize::from(self.editing) + 2
    }
}

/// Row and column of byte offset `b` of `text` within `rows`, consecutive slices of it.
fn cursor_at(text: &str, rows: &[&str], b: usize) -> Option<(usize, usize)> {
    let base = text.as_ptr() as usize;
    for (r, row) in rows.iter().enumerate() {
        let start = row.as_ptr() as usize - base;
        let end = start + row.len();
        // A cursor on a line break is drawn after the row it ends, not on the next one.
        let on_break = b == end && text[end..].starts_with('\n');
        if b < end || on_break || r + 1 == rows.len() {
            return Some((r, text[start..b.max(start)].width()));
        }
    }
    None
}

/// A thread row after the `│ ` bar (in `bar` style): an author line, or text in `style`.
fn thread_row<'a>(
    r: &ThreadRow<'a>,
    bar: Style,
    style: Style,
    selected: bool,
    width: usize,
    theme: &Theme,
) -> Line<'a> {
    let (text, style) = if r.header {
        (format!("↳ {}", r.text), theme.dim)
    } else {
        (format!("{}{}", " ".repeat(r.indent), r.text), style)
    };
    let spans = fit_spans(
        vec![Span::styled("│ ", bar), Span::styled(text, style)],
        width,
    );
    if selected {
        patch_row(spans, theme.diff_sel)
    } else {
        Line::from(spans)
    }
}

/// The thread shown below diff line `i`, with the open input if it belongs there.
fn note_at<'a>(st: &'a WtState, input: Option<&'a CommentInput>, i: usize) -> Option<Note<'a>> {
    let all = &st.comments.comments;
    let shown = st.anchors.by_line.get(&i).and_then(|a| all.get(a.comment));
    let Some(input) = input.filter(|inp| inp.at == i) else {
        let c = shown?;
        // Only a highlighted reply stands out here; the pane highlights the comment.
        let sel = st.reply_sel.filter(|_| st.comment_sel == Some(c.id));
        return Some(Note::thread(c, sel.map(Some)));
    };
    let edit = Seg {
        by: None,
        text: &input.edit.text,
        cursor: Some(input.edit.cursor_byte()),
        selected: false,
    };
    let find = |id: Id| all.iter().find(|c| c.id == id);
    let (thread, slot) = match &input.target {
        Target::Comment {
            comment,
            existing: true,
        } => (find(comment.id), Some(0)),
        Target::Comment { .. } => (None, None),
        Target::Reply { comment, reply } => {
            let c = find(*comment);
            let slot = reply.and_then(|r| Some(1 + c?.replies.iter().position(|x| x.id == r)?));
            (c, slot)
        }
    };
    let mut note = match thread {
        Some(c) => Note::thread(c, None),
        None => Note {
            segs: Vec::new(),
            editing: false,
            closed: false,
        },
    };
    note.editing = true;
    match slot {
        Some(k) if k < note.segs.len() => {
            note.segs[k] = Seg {
                by: note.segs[k].by,
                ..edit
            }
        }
        // A new comment, or a new reply at the end of its thread.
        _ => note.segs.push(Seg {
            by: matches!(input.target, Target::Reply { .. }).then_some(crate::app::USER),
            ..edit
        }),
    }
    Some(note)
}

/// Whether `st.comments.comments[ci]` is closed.
fn is_closed(st: &WtState, ci: usize) -> bool {
    st.comments
        .comments
        .get(ci)
        .is_some_and(|c| c.closed.is_some())
}

/// Screen rows of diff line `i`: the line itself (wrapped or not) and the note below it.
fn line_rows(
    st: &WtState,
    input: Option<&CommentInput>,
    wrap: bool,
    text_w: usize,
    width: usize,
    i: usize,
) -> usize {
    text_rows(st, wrap, text_w, i) + note_at(st, input, i).map_or(0, |n| n.height(width))
}

/// Screen rows of diff line `i`'s text.
fn text_rows(st: &WtState, wrap: bool, text_w: usize, i: usize) -> usize {
    if wrap {
        wrap_ranges(&st.diff.lines[i].text, text_w).len()
    } else {
        1
    }
}

/// Rows of the first shown diff line's block to skip, so that the reply being edited or
/// highlighted below the selected line fits in `h` rows when its thread is taller than that.
fn diff_skip(
    st: &WtState,
    input: Option<&CommentInput>,
    wrap: bool,
    text_w: usize,
    width: usize,
    h: usize,
) -> usize {
    let Some(line) = input.map(|i| i.at).or(st.diff_sel) else {
        return 0;
    };
    let Some(note) = note_at(st, input, line).filter(|_| st.diff_offset == line) else {
        return 0;
    };
    // The line's text and the rule above the thread.
    let lead = text_rows(st, wrap, text_w, line) + 1;
    focus_skip(&note, &note.rows(width), lead, h)
}

/// Rows to skip of a block of `lead` rows followed by `note`'s `rows`, so that the entry being
/// edited, or a highlighted reply, ends within `h` rows.
fn focus_skip(note: &Note, rows: &[ThreadRow], lead: usize, h: usize) -> usize {
    let focus = |r: &ThreadRow| {
        let seg = &note.segs[r.seg];
        seg.cursor.is_some() || seg.selected && seg.by.is_some()
    };
    rows.iter()
        .rposition(focus)
        .map_or(0, |last| (lead + last + 1).saturating_sub(h))
}

fn num_width(diff: &Diff) -> usize {
    diff.max_line_no().to_string().len().max(3)
}

/// Comment marker, old and new line numbers, each followed by a space except the marker.
fn gutter_width(diff: &Diff) -> usize {
    num_width(diff) * 2 + 3
}

/// Columns left for diff text after the gutter and the +/- marker.
fn text_width(diff: &Diff, inner_width: u16) -> usize {
    (inner_width as usize).saturating_sub(gutter_width(diff) + 1)
}

/// Splits `text` into rows of at most `width` display columns, preferring to break after
/// whitespace. Always returns at least one row and makes progress for any `width`.
pub fn wrap_ranges(text: &str, width: usize) -> Vec<&str> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let (mut cols, mut end, mut last_break) = (0, 0, None);
        for (i, c) in rest.char_indices() {
            let w = c.width().unwrap_or(0);
            if cols + w > width && end > 0 {
                break;
            }
            cols += w;
            end = i + c.len_utf8();
            if c.is_whitespace() {
                last_break = Some(end);
            }
        }
        if end == rest.len() {
            out.push(rest);
            return out;
        }
        let cut = last_break.unwrap_or(end);
        out.push(&rest[..cut]);
        rest = &rest[cut..];
    }
}

/// Maps a screen row (relative to the diff pane) to the diff line drawn there.
/// Returns `diff.lines.len()` when the row is past the end.
pub fn line_at_row(st: &WtState, wrap: bool, rel: usize, inner: Rect) -> usize {
    let text_w = text_width(&st.diff, inner.width);
    let width = inner.width as usize;
    let skip = diff_skip(st, None, wrap, text_w, width, inner.height as usize);
    row_to_line(st.diff_offset, st.diff.lines.len(), rel + skip, |i| {
        line_rows(st, None, wrap, text_w, width, i)
    })
}

/// The line among `offset..len` covering screen row `rel`, given each line's row count.
fn row_to_line(offset: usize, len: usize, rel: usize, rows: impl Fn(usize) -> usize) -> usize {
    let mut row = 0;
    for i in offset..len {
        row += rows(i);
        if rel < row {
            return i;
        }
    }
    len
}

/// A comments pane entry: a comment (index into `comments.comments`), its thread with the
/// highlighted entry marked, and the thread's rows.
struct PaneEntry<'a> {
    ci: usize,
    note: Note<'a>,
    rows: Vec<ThreadRow<'a>>,
}

impl PaneEntry<'_> {
    /// Screen rows: header, thread, blank separator.
    fn height(&self) -> usize {
        self.rows.len() + 2
    }

    /// Rows skipped when the entry is shown first, so its highlighted reply fits in `h` rows.
    fn skip(&self, h: usize) -> usize {
        focus_skip(&self.note, &self.rows, 1, h)
    }
}

/// The comments pane's entries in order, laid out `width` columns wide.
fn pane_entries(st: &WtState, show_closed: bool, width: usize) -> Vec<PaneEntry<'_>> {
    let all = &st.comments.comments;
    st.comment_order(show_closed)
        .into_iter()
        .map(|ci| {
            let c = &all[ci];
            let sel = (st.comment_sel == Some(c.id)).then_some(st.reply_sel);
            let note = Note::thread(c, sel);
            let rows = note.rows(width);
            PaneEntry { ci, note, rows }
        })
        .collect()
}

/// Id of the comment drawn at screen row `rel` of the comments pane (`inner`), and of the
/// reply if the row is one of its rows.
pub fn comment_at_row(
    st: &WtState,
    show_closed: bool,
    rel: usize,
    inner: Rect,
) -> Option<(Id, Option<Id>)> {
    let entries = pane_entries(st, show_closed, inner.width as usize);
    let off = st.comments_offset;
    let skip = entries
        .get(off)
        .map_or(0, |e| e.skip(inner.height as usize));
    let i = row_to_line(off, entries.len(), rel + skip, |i| entries[i].height());
    let e = entries.get(i)?;
    let top: usize = entries[off..i].iter().map(PaneEntry::height).sum();
    // Below the header: the thread's rows, then the blank separator.
    let seg = (rel + skip - top)
        .checked_sub(1)
        .and_then(|k| e.rows.get(k));
    let c = &st.comments.comments[e.ci];
    let reply = seg.and_then(|r| Some(c.replies.get(r.seg.checked_sub(1)?)?.id));
    Some((c.id, reply))
}

fn draw_comments(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let Some(st) = app.wts.get_mut(app.active) else {
        return;
    };
    let order = st.comment_order(app.show_closed);
    let block = pane_block(format!(" Comments · {} ", order.len()), &theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.hit.comments = inner;
    let st = &mut app.wts[app.active];

    if let Some(err) = &st.comments.error {
        return message(f, inner, err, theme.error);
    }
    if order.is_empty() {
        let text = if st.comments.comments.is_empty() || app.show_closed {
            "No comments. Press c on a diff line to add one."
        } else {
            "No open comments. X shows closed ones."
        };
        return message(f, inner, text, theme.dim);
    }
    let h = inner.height as usize;
    let w = inner.width as usize;
    let reveal = std::mem::take(&mut st.comments_reveal);
    let st = &*st;
    let all = &st.comments.comments;
    let entries = pane_entries(st, app.show_closed, w);
    let heights: Vec<usize> = entries.iter().map(PaneEntry::height).collect();
    let len = entries.len();
    // Don't scroll past the point where the last entry reaches the bottom.
    let (mut max, mut total) = (len, 0);
    while max > 0 && total < h {
        max -= 1;
        total += heights[max];
    }
    // The entry that overflowed doesn't fit; start after it unless it is the last one.
    if total > h && max + 1 < len {
        max += 1;
    }
    let mut off = st.comments_offset.min(max);
    let sel = st
        .comment_sel
        .and_then(|id| entries.iter().position(|e| all[e.ci].id == id));
    if let Some(sel) = sel.filter(|_| reveal) {
        // Scroll so the whole selected entry fits if it can.
        off = off.min(sel);
        let mut total: usize = heights[off..=sel].iter().sum();
        while total > h && off < sel {
            total -= heights[off];
            off += 1;
        }
    }
    // A highlighted reply below the bottom of a tall first entry is scrolled into view.
    let skip = entries[off].skip(h);

    let mut out: Vec<Line> = Vec::with_capacity(h + skip);
    for (i, e) in entries.iter().enumerate().skip(off) {
        if out.len() >= h + skip {
            break;
        }
        let c = &all[e.ci];
        let lines = match &c.start {
            Some(s) if s.line != c.line => format!(":{}-{}", s.line, c.line),
            _ => format!(":{}", c.line),
        };
        let (tag, tag_style) = match &c.commit {
            None if st.is_stale(c) => (" · stale".to_string(), theme.error),
            None => (String::new(), theme.dim),
            Some(sha) => match st.commits.iter().find(|x| &x.sha == sha) {
                Some(commit) => (format!(" @ {}", commit.short), theme.sha),
                None => {
                    let short = sha.get(..7).unwrap_or(sha);
                    let why = if st.is_stale(c) {
                        "stale"
                    } else {
                        "not in history"
                    };
                    (format!(" @ {short} · {why}"), theme.error)
                }
            },
        };
        let mut closed = match c.replies.len() {
            0 => String::new(),
            1 => " · 1 reply".into(),
            n => format!(" · {n} replies"),
        };
        if c.closed.is_some() {
            closed += " · closed";
        }
        let (file_style, bar_style, body_style) = if c.closed.is_some() {
            (theme.dim, theme.dim, theme.dim)
        } else {
            (theme.base, theme.comment, theme.comment_text)
        };
        let budget = w
            .saturating_sub(lines.width() + tag.width() + closed.width())
            .max(8);
        let header = vec![
            Span::styled(shorten_left(&c.file, budget), file_style),
            Span::styled(lines, theme.dim),
            Span::styled(tag, tag_style),
            Span::styled(closed, theme.dim),
        ];
        out.push(if sel == Some(i) {
            patch_row(fit_spans(header, w), theme.diff_sel)
        } else {
            Line::from(fit_spans(header, w))
        });
        // The highlighted entry: the comment itself or one of its replies.
        for r in &e.rows {
            let selected = e.note.segs[r.seg].selected;
            out.push(thread_row(r, bar_style, body_style, selected, w, &theme));
        }
        out.push(Line::raw(""));
    }
    out.drain(..skip.min(out.len()));
    out.truncate(h);
    f.render_widget(Paragraph::new(out), inner);

    if max > 0 {
        let mut state = ScrollbarState::new(max.max(off)).position(off);
        let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .style(theme.dim);
        f.render_stateful_widget(
            bar,
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut state,
        );
    }
    app.wts[app.active].comments_offset = off;
}

fn draw_commits(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let Some(st) = app.wts.get_mut(app.active) else {
        return;
    };
    let title = format!(" Commits · {} ", st.info.label());
    let block = pane_block(title, &theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.hit.commits = inner;
    let st = &mut app.wts[app.active];

    let h = inner.height as usize;
    let w = inner.width as usize;
    let rows = st.commits.len() + 1;
    clamp_offset(&mut st.commit_offset, rows, h);
    let selected_sha = st.commit.as_ref().map(|c| c.sha.as_str());
    let lines: Vec<Line> = (st.commit_offset..rows.min(st.commit_offset + h))
        .map(|i| {
            let spans = if i == 0 {
                let mark = if selected_sha.is_none() {
                    "● "
                } else {
                    "○ "
                };
                vec![
                    Span::styled(mark, theme.badge),
                    Span::styled("Working tree", theme.title),
                    Span::styled(
                        format!("  {} uncommitted changes", st.changes.len()),
                        theme.dim,
                    ),
                ]
            } else {
                let c = &st.commits[i - 1];
                let mark = if selected_sha == Some(c.sha.as_str()) {
                    "● "
                } else {
                    "  "
                };
                let mut s = vec![
                    Span::styled(mark, theme.badge),
                    Span::styled(format!("{} ", c.short), theme.sha),
                    Span::styled(format!("{:<14.14} ", c.date), theme.dim),
                    Span::styled(format!("{:<16.16} ", c.author), theme.dim),
                ];
                if !c.refs.is_empty() {
                    s.push(Span::styled(format!("({}) ", c.refs), theme.refs));
                }
                s.push(Span::styled(c.subject.clone(), theme.base));
                s
            };
            let spans = fit_spans(spans, w);
            let is_selected = match selected_sha {
                None => i == 0,
                Some(sha) => i > 0 && st.commits[i - 1].sha == sha,
            };
            if i == st.commit_cursor {
                patch_row(spans, theme.cursor)
            } else if is_selected {
                patch_row(spans, theme.diff_sel)
            } else {
                Line::from(spans)
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    // Everything after the path; the path is shortened first so the warning stays visible.
    let mut tail = vec![];
    if let Some(st) = app.active() {
        if let Some(c) = &st.conflict {
            tail.push(Span::styled(
                format!("  ⚠ conflicts with {} ({} files)", c.base, c.files.len()),
                theme.error,
            ));
        }
        if let Some(c) = &st.commit {
            tail.push(Span::styled(
                format!("  @ {} {}", c.short, c.subject),
                theme.sha,
            ));
        }
    }
    if let Some(p) = &app.pending_keys {
        tail.push(Span::styled(format!("  {p}…"), theme.badge));
    }
    let right = match app.visible_toast() {
        Some((msg, true)) => Span::styled(format!("{msg} "), theme.error),
        Some((msg, false)) => Span::styled(format!("{msg} "), theme.add),
        None => Span::styled("? help ", theme.dim),
    };
    let right_w = right.width() as u16;
    let [l, r] = Layout::horizontal([Constraint::Min(0), Constraint::Length(right_w)]).areas(area);
    let mut left = vec![];
    if let Some(st) = app.active() {
        let tail_w: usize = tail.iter().map(|s| s.width()).sum();
        let budget = (l.width as usize).saturating_sub(tail_w).max(12);
        left.push(Span::styled(
            shorten_left(&home_relative(&st.info.path), budget),
            theme.base,
        ));
    }
    left.extend(tail);
    f.render_widget(Line::from(fit_spans(left, l.width as usize)), l);
    f.render_widget(Line::from(right), r);
}

fn help_lines<'a>(sections: &[Section], theme: &Theme, mouse: bool) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    for &section in sections {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(section.title(), theme.title));
        for b in BINDINGS
            .iter()
            .filter(|b| b.section == section && !b.label.is_empty())
        {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<14}", b.label), theme.sha),
                Span::styled(b.desc, theme.base),
            ]));
        }
    }
    if mouse {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Mouse", theme.title));
        for (k, d) in MOUSE_HELP {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<14}"), theme.sha),
                Span::styled(*d, theme.base),
            ]));
        }
    }
    lines
}

fn draw_help(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let all = [
        Section::General,
        Section::Worktrees,
        Section::Comments,
        Section::Diff,
        Section::Tree,
    ];
    let width_of = |lines: &[Line]| lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let (gl, rest) = (
        help_lines(&all[..3], theme, false),
        help_lines(&all[3..], theme, true),
    );
    // Two columns when they fit side by side, otherwise one scrollable column.
    let cols = if width_of(&gl) + width_of(&rest) + 7 <= area.width {
        vec![gl, rest]
    } else {
        vec![help_lines(&all, theme, true)]
    };
    let col_widths: Vec<u16> = cols.iter().map(|c| width_of(c) + 3).collect();
    let content_h = cols.iter().map(Vec::len).max().unwrap_or(0);
    let width = (col_widths.iter().sum::<u16>() + 2).min(area.width);
    let height = (content_h as u16 + 2).min(area.height);
    let inner_h = height.saturating_sub(2) as usize;
    app.help_scroll = app.help_scroll.min(content_h.saturating_sub(inner_h));
    let title = if content_h > inner_h {
        " Keyboard shortcuts · j/k scroll · any other key closes "
    } else {
        " Keyboard shortcuts · any key closes "
    };
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.popup_border)
        .style(theme.base)
        .title(Span::styled(title, theme.title));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    let areas = Layout::horizontal(col_widths.iter().map(|w| Constraint::Length(*w))).split(inner);
    for (lines, area) in cols.into_iter().zip(areas.iter()) {
        let lines: Vec<Line> = lines.into_iter().skip(app.help_scroll).collect();
        f.render_widget(
            Paragraph::new(lines),
            area.inner(Margin {
                horizontal: 1,
                vertical: 0,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comments::test_id;

    #[test]
    fn panel_split() {
        let split = |w| {
            let [tree, diff] =
                Layout::horizontal(panel_constraints(w)).areas(Rect::new(0, 0, w, 10));
            (tree.width, diff.width)
        };
        assert_eq!(split(40), (20, 20));
        assert_eq!(split(99), (20, 79));
        assert_eq!(split(100), (30, 70));
        assert_eq!(split(119), (30, 89));
        assert_eq!(split(120), (30, 90));
        assert_eq!(split(121), (30, 91));
        assert_eq!(split(199), (49, 150));
        assert_eq!(split(200), (50, 150));
        assert_eq!(split(300), (50, 250));
        let mut prev = 0;
        for w in MIN_WIDTH..=400 {
            let (tree, diff) = split(w);
            assert_eq!(tree + diff, w);
            assert!(tree >= prev, "tree shrank at width {w}");
            prev = tree;
        }
    }

    #[test]
    fn comments_pane_fits_or_hides() {
        assert_eq!(comments_width(80), None);
        assert_eq!(comments_width(100), Some(25));
        assert_eq!(comments_width(120), Some(30));
        assert_eq!(comments_width(300), Some(50));
        for w in MIN_WIDTH..=400 {
            if let Some(c) = comments_width(w) {
                let [tree, rest] =
                    Layout::horizontal(panel_constraints(w)).areas(Rect::new(0, 0, w, 1));
                assert!(rest.width - c >= DIFF_MIN_WIDTH, "diff too narrow at {w}");
                assert!(tree.width + rest.width == w);
            }
        }
    }

    #[test]
    fn shortens_from_left() {
        assert_eq!(shorten_left("abc", 5), "abc");
        assert_eq!(shorten_left("~/a/b/wt-conflicting", 8), "…licting");
    }

    #[test]
    fn fits_and_pads() {
        let out = fit_spans(vec![Span::raw("ab"), Span::raw("cdé")], 4);
        let text: String = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcd");
        let out = fit_spans(vec![Span::raw("ab")], 4);
        let text: String = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "ab  ");
        assert_eq!(skip_cols("hello", 2), "llo");
        assert_eq!(skip_cols("hi", 5), "");
    }

    #[test]
    fn maps_rows_to_lines() {
        let rows = |i: usize| [3, 2, 1, 4][i];
        assert_eq!(row_to_line(1, 4, 0, rows), 1);
        assert_eq!(row_to_line(1, 4, 1, rows), 1);
        assert_eq!(row_to_line(1, 4, 2, rows), 2);
        assert_eq!(row_to_line(1, 4, 6, rows), 3);
        assert_eq!(row_to_line(1, 4, 7, rows), 4);
    }

    #[test]
    fn places_note_cursor() {
        let note = |text, cursor| Note {
            segs: vec![Seg {
                by: None,
                text,
                cursor: Some(cursor),
                selected: false,
            }],
            editing: true,
            closed: false,
        };
        let texts = |n: &Note| -> Vec<String> { n.rows(7).iter().map(|r| r.text.into()).collect() };
        let cursors =
            |n: &Note| -> Vec<Option<usize>> { n.rows(7).iter().map(|r| r.cursor).collect() };
        // Width 7 → 5 columns of note text after the bar.
        let n = note("abcd efgh", 5);
        assert_eq!(texts(&n), vec!["abcd ", "efgh"]);
        assert_eq!(cursors(&n), vec![None, Some(0)]);
        assert_eq!(n.height(7), 5);
        let n = note("abcd efgh", 9);
        assert_eq!(cursors(&n), vec![None, Some(4)]);
        let n = note("", 0);
        assert_eq!(cursors(&n), vec![Some(0)]);
        // On a line break: after the row it ends; past it: at the next row's start.
        let n = note("ab\ncd", 2);
        assert_eq!(texts(&n), vec!["ab", "cd"]);
        assert_eq!(cursors(&n), vec![Some(2), None]);
        let n = note("ab\ncd", 3);
        assert_eq!(cursors(&n), vec![None, Some(0)]);
        let n = note("ab\n\ncd", 3);
        assert_eq!(cursors(&n), vec![None, Some(0), None]);
    }

    #[test]
    fn lays_out_threads() {
        let mut c = crate::comments::Comment {
            id: test_id(1),
            file: "f".into(),
            side: crate::comments::Side::New,
            line: 1,
            line_text: "x".into(),
            start: None,
            commit: None,
            head: None,
            body: "abcd efgh".into(),
            created: 0,
            updated: 0,
            closed: None,
            replies: Vec::new(),
        };
        for (id, body) in [(7, "ok"), (8, "one\ntwo")] {
            c.replies.push(crate::comments::Reply {
                id: test_id(id),
                ..crate::comments::Reply::new("claude", body)
            });
        }
        let n = Note::thread(&c, Some(Some(test_id(8))));
        let rows: Vec<(usize, bool, &str, usize)> = n
            .rows(7)
            .iter()
            .map(|r| (r.seg, r.header, r.text, r.indent))
            .collect();
        assert_eq!(
            rows,
            vec![
                (0, false, "abcd ", 0),
                (0, false, "efgh", 0),
                (1, true, "claude", 0),
                (1, false, "ok", 2),
                (2, true, "claude", 0),
                (2, false, "one", 2),
                (2, false, "two", 2),
            ]
        );
        let sel: Vec<bool> = n.segs.iter().map(|s| s.selected).collect();
        assert_eq!(sel, vec![false, false, true]);
        assert_eq!(n.height(7), 9);

        // The highlighted reply ends on thread row 7, after `lead` rows: skip what doesn't fit.
        let rows = n.rows(7);
        assert_eq!(focus_skip(&n, &rows, 1, 10), 0);
        assert_eq!(focus_skip(&n, &rows, 1, 5), 3);
        // A highlighted comment body, or nothing highlighted, scrolls nothing.
        for sel in [Some(None), None] {
            let n = Note::thread(&c, sel);
            assert_eq!(focus_skip(&n, &n.rows(7), 1, 2), 0);
        }
    }

    #[test]
    fn wraps_text() {
        assert_eq!(wrap_ranges("", 5), vec![""]);
        assert_eq!(wrap_ranges("abc", 5), vec!["abc"]);
        assert_eq!(
            wrap_ranges("hello world foo", 8),
            vec!["hello ", "world ", "foo"]
        );
        assert_eq!(wrap_ranges("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap_ranges("日本語", 4), vec!["日本", "語"]);
        assert_eq!(wrap_ranges("日本", 1), vec!["日", "本"]);
        assert_eq!(wrap_ranges("abc", 0), vec!["a", "b", "c"]);
    }
}
