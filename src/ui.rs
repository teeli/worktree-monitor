//! Rendering. Also records the screen regions the app uses for mouse hit-testing.

use crate::app::App;
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
use unicode_width::UnicodeWidthChar;

const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 12;

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
    let [tree, diff] =
        Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)]).areas(main);

    draw_tabs(f, app, tabs);
    draw_tree(f, app, tree);
    draw_diff(f, app, diff);
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
            let wrapped = if wrap { " · wrap" } else { "" };
            format!(" {}{stats}{wrapped} ", file.path)
        }
        None => " Diff ".to_string(),
    };
    let block = pane_block(title, &theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.hit.diff = inner;
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
    if !wrap {
        clamp_offset(&mut st.diff_offset, len, h);
    }
    let num_w = st.diff.max_line_no().to_string().len().max(3);
    let gutter_w = num_w * 2 + 2;
    let text_w = (inner.width as usize).saturating_sub(gutter_w + 1);
    let num = |n: Option<u32>| n.map_or_else(|| " ".repeat(num_w), |n| format!("{n:>num_w$}"));
    let sel_style = theme.diff_sel;
    let hscroll = st.diff_hscroll;

    let build = |gutter: String, marker: &'static str, text: &str, style: Style, selected: bool| {
        let gutter_style = if selected {
            theme.gutter_sel
        } else {
            theme.dim
        };
        let mut spans = vec![Span::styled(gutter, gutter_style)];
        spans.extend(fit_spans(
            vec![
                Span::styled(marker, style),
                Span::styled(text.to_string(), style),
            ],
            text_w + 1,
        ));
        // Span styles carry their own bg (e.g. Dracula), so the selection
        // is patched onto the finished row (gutter and padding included).
        let row_style = if selected { sel_style } else { Style::new() };
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

    let lines: Vec<Line> = if wrap {
        st.diff_offset = st.diff_offset.min(len - 1);
        if let Some(sel) = st.diff_sel {
            // Scroll so the whole selected line fits, if it can.
            st.diff_offset = st.diff_offset.min(sel);
            let rows = |i: usize| wrap_ranges(&st.diff.lines[i].text, text_w).len();
            let mut total: usize = (st.diff_offset..=sel).map(rows).sum();
            while total > h && st.diff_offset < sel {
                total -= rows(st.diff_offset);
                st.diff_offset += 1;
            }
        }
        let mut out = Vec::with_capacity(h);
        for (i, l) in st.diff.lines.iter().enumerate().skip(st.diff_offset) {
            if out.len() >= h {
                break;
            }
            let (marker, style) = line_style(l.kind);
            let selected = st.diff_sel == Some(i);
            for (k, chunk) in wrap_ranges(&l.text, text_w).into_iter().enumerate() {
                if out.len() >= h {
                    break;
                }
                out.push(if k == 0 {
                    let gutter = format!("{} {} ", num(l.old), num(l.new));
                    build(gutter, marker, chunk, style, selected)
                } else {
                    build(" ".repeat(gutter_w), " ", chunk, style, selected)
                });
            }
        }
        out
    } else {
        st.diff
            .lines
            .iter()
            .enumerate()
            .skip(st.diff_offset)
            .take(h)
            .map(|(i, l)| {
                let (marker, style) = line_style(l.kind);
                let gutter = format!("{} {} ", num(l.old), num(l.new));
                let text = skip_cols(&l.text, hscroll);
                build(gutter, marker, text, style, st.diff_sel == Some(i))
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines), inner);

    let scroll_range = if wrap { len - 1 } else { len.saturating_sub(h) };
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

/// Maps a screen row (relative to the diff pane) to the diff line drawn there when wrapping.
/// Returns `diff.lines.len()` when the row is past the end.
pub fn line_at_row(diff: &Diff, offset: usize, rel: usize, inner_width: u16) -> usize {
    let num_w = diff.max_line_no().to_string().len().max(3);
    let text_w = (inner_width as usize).saturating_sub(num_w * 2 + 3);
    let mut row = 0;
    for (i, l) in diff.lines.iter().enumerate().skip(offset) {
        row += wrap_ranges(&l.text, text_w).len();
        if rel < row {
            return i;
        }
    }
    diff.lines.len()
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
        Section::Diff,
        Section::Tree,
    ];
    let width_of = |lines: &[Line]| lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let (gl, rest) = (
        help_lines(&all[..2], theme, false),
        help_lines(&all[2..], theme, true),
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
