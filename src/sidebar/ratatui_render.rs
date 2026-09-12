//! Experimental Ratatui frame construction (`--features ratatui`).
//!
//! Draws the same list, inventory, header, hint, empty-state and overlay
//! cells as `render.rs` into a `ratatui::buffer::Buffer`, then serializes the
//! buffer with the frame conventions `emit`/`PaneWriters` already expect: one
//! `\e[H`, per-line `\e[K`, trailing `\e[J`, complete frame every time. Only
//! frame construction changes; scroll math, the rows file, delivery, keys and
//! config reload are untouched.
//
// ponytail: this deliberately duplicates render.rs/overlay.rs line logic so
// the experiment can be compared against the stable path and deleted whole.
use crate::app_config::{Action, Color as InkColor, Ink, Palette};
use crate::input::term_size;
use ratatui::buffer::{Buffer, Cell, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use std::fmt::Write as _;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::overlay::{current_tag, known_tags, picker_sel, Overlay};
use super::render::{app_title, join};
use super::{Sidebar, VisiblePane, E};

/// Nerd-font window icon, as in render.rs.
const WINDOW: char = '\u{eb7f}';
const SPIN: [char; 8] = ['⠹', '⢸', '⣰', '⣤', '⣆', '⡇', '⠏', '⠛'];

fn color(ink: &Ink) -> Color {
    match ink {
        Ink::Inherited | Ink::Typed(InkColor::Default) => Color::Reset,
        Ink::Basic(n) => match n {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            _ => Color::Gray,
        },
        Ink::Typed(InkColor::Indexed(n)) => Color::Indexed(*n),
        Ink::Typed(InkColor::Rgb(r, g, b)) => Color::Rgb(*r, *g, *b),
    }
}

/// `Ink::fg(attributes)` as a style: "1" bold, "2" dim, "" plain.
fn fg(ink: &Ink, attributes: &str) -> Style {
    let mut s = Style::default().fg(color(ink));
    match attributes {
        "1" => s = s.add_modifier(Modifier::BOLD),
        "2" => s = s.add_modifier(Modifier::DIM),
        _ => {}
    }
    s
}

/// Clip to display cells, not chars: a wide glyph costs two columns, so
/// char-counting would push CJK/emoji rows past `cols` and wrap them.
fn clip(text: &str, max: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|c| {
            used += UnicodeWidthChar::width(*c).unwrap_or(0);
            used <= max
        })
        .collect()
}

/// One frame row: content, full-row fill, and its rows-file record.
struct Row {
    line: Line<'static>,
    bg: Color,
    pane: String,
    index: usize,
    selected: bool,
}

fn header_row(line: Line<'static>, bg: Color) -> Row {
    Row {
        line,
        bg,
        pane: "-".into(),
        index: 0,
        selected: false,
    }
}

/// Foreground, background and the attributes the frame uses (bold/dim).
type Sgr = (Color, Color, Modifier);
const PLAIN: Sgr = (Color::Reset, Color::Reset, Modifier::empty());

fn style_of(c: &Cell) -> Sgr {
    // Other modifiers are never set here, and masking them keeps `cur != to`
    // from asking for an escape with no parameters.
    (c.fg, c.bg, c.modifier & (Modifier::BOLD | Modifier::DIM))
}

fn push_color(out: &mut String, c: Color, base: u8) {
    let _ = match c {
        Color::Reset => Ok(()),
        Color::Black => write!(out, "{base}"),
        Color::Red => write!(out, "{}", base + 1),
        Color::Green => write!(out, "{}", base + 2),
        Color::Yellow => write!(out, "{}", base + 3),
        Color::Blue => write!(out, "{}", base + 4),
        Color::Magenta => write!(out, "{}", base + 5),
        Color::Cyan => write!(out, "{}", base + 6),
        Color::Gray => write!(out, "{}", base + 7),
        Color::Indexed(n) => write!(out, "{};5;{n}", base + 8),
        Color::Rgb(r, g, b) => write!(out, "{};2;{r};{g};{b}", base + 8),
        _ => write!(out, "{}", base + 9),
    };
}

/// Move the terminal from `cur` to `to` with the shortest SGR: additions are
/// emitted alone, but nothing turns an attribute or a color off except `\e[0m`,
/// so a removal costs a reset plus the full style. `emit` re-applies the theme
/// foreground after every `\e[0m`, which is why resets must stay explicit.
fn push_sgr(out: &mut String, cur: &mut Sgr, to: Sgr) {
    if *cur == to {
        return;
    }
    let dropped = |old: Color, new: Color| old != Color::Reset && new == Color::Reset;
    if cur
        .2
        .difference(to.2)
        .intersects(Modifier::BOLD | Modifier::DIM)
        || dropped(cur.0, to.0)
        || dropped(cur.1, to.1)
    {
        out.push_str(E);
        out.push_str("[0m");
        *cur = PLAIN;
        if *cur == to {
            return;
        }
    }
    out.push_str(E);
    out.push('[');
    let mut sep = false;
    for (m, p) in [(Modifier::BOLD, '1'), (Modifier::DIM, '2')] {
        if to.2.contains(m) && !cur.2.contains(m) {
            if sep {
                out.push(';');
            }
            out.push(p);
            sep = true;
        }
    }
    for (from, want, base) in [(cur.0, to.0, 30u8), (cur.1, to.1, 40)] {
        if from != want {
            if sep {
                out.push(';');
            }
            push_color(out, want, base);
            sep = true;
        }
    }
    out.push('m');
    *cur = to;
}

/// Complete frame in the byte shape `clip_frame`/`emit` already understand.
/// Blank trailing cells become `\e[K`; blank trailing rows become `\e[J`.
fn serialize(buf: &Buffer, clear_all: bool) -> String {
    let area = buf.area();
    // `Skip` marks cells a line explicitly wrote: the ANSI path emits those
    // spaces as text, so they carry the frame foreground on screen. Unwritten
    // cells before later content (the scrollbar column) are cleared and jumped
    // over, as `\e[K\e[<col>G` does today.
    let blank = |c: &Cell| {
        c.diff_option != CellDiffOption::Skip
            && c.symbol() == " "
            && c.fg == Color::Reset
            && c.bg == Color::Reset
            && c.modifier.is_empty()
    };
    let eol = format!("{E}[0m{E}[K\n");
    let mut out = String::with_capacity(area.area() as usize * 2 + 16);
    if clear_all {
        out.push_str(&format!("{E}[2J"));
    }
    out.push_str(&format!("{E}[H"));
    // Blank trailing cells and rows are never emitted: `\e[K` and `\e[J` clear
    // them, so both are held back until later content proves they are interior.
    let mut row = String::new();
    let mut pending = 0usize;
    for y in 0..area.height {
        row.clear();
        let mut cur = PLAIN;
        let mut gap = false;
        let mut x = 0;
        while x < area.width {
            let c = &buf[(x, y)];
            // A wide glyph owns the cells ratatui `reset()`s behind it;
            // emitting those as spaces would shift the rest of the row.
            let step = UnicodeWidthStr::width(c.symbol()).max(1) as u16;
            if blank(c) {
                gap = true;
                x += step;
                continue;
            }
            if gap {
                let _ = write!(row, "{E}[0m{E}[K{E}[{}G", x + 1);
                cur = PLAIN;
                gap = false;
            }
            push_sgr(&mut row, &mut cur, style_of(c));
            row.push_str(c.symbol());
            x += step;
        }
        if row.is_empty() {
            pending += 1;
            continue;
        }
        for _ in 0..pending {
            out.push_str(&eol);
        }
        pending = 0;
        out.push_str(&row);
        out.push_str(&eol);
    }
    out.push_str(&format!("{E}[J"));
    out
}

impl Sidebar {
    fn size(&self) -> (usize, usize) {
        self.daemon
            .as_ref()
            .map(|d| d.size)
            .unwrap_or_else(term_size)
    }

    /// The reused frame buffer, cleared and sized for this frame. Taken out of
    /// `self` so drawing can still borrow the sidebar; put back by the caller.
    fn take_buf(&mut self, area: Rect) -> Buffer {
        let mut buf = std::mem::replace(&mut self.frame_buf, Buffer::empty(Rect::ZERO));
        if buf.area != area {
            buf.resize(area);
        }
        buf.reset();
        buf
    }

    fn dot_span(&self, state: &str) -> Span<'static> {
        let on = (self.tick / 2).is_multiple_of(2);
        let style = fg(self.palette.state_fg(state), "");
        match state {
            "blocked" | "done" if !on => Span::raw(" "),
            "working" => Span::styled(SPIN[(self.tick % 8) as usize].to_string(), style),
            _ => Span::styled("⣿", style),
        }
    }

    fn mark_spans(&self, selected: bool, state: &str) -> Vec<Span<'static>> {
        if !selected {
            return vec![Span::raw("  ")];
        }
        let style = fg(
            self.palette.state_fg(state),
            if self.plugin_selected { "1" } else { "" },
        );
        vec![Span::styled("❯", style), Span::raw(" ")]
    }

    fn agent_rows(&self, cols: usize, cursor: Option<usize>) -> (Vec<Row>, usize, usize) {
        let muted = fg(&self.palette.muted_fg, "2");
        let accent = fg(&self.palette.accent_fg, "1");
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut rows = Vec::new();
        let (mut sel_top, mut sel_bot) = (0usize, 0usize);
        let mut session = "";
        for (n, visible) in self.visible.iter().copied().enumerate() {
            let VisiblePane::Agent(row_i) = visible else {
                continue;
            };
            let r = &self.rows[row_i];
            let sess = r.loc.split(':').next().unwrap_or("");
            if sess != session {
                session = sess;
                rows.push(header_row(
                    Line::from(Span::styled(clip(sess, cols), accent)),
                    Color::Reset,
                ));
            }
            let selected = Some(n) == cursor;
            if selected {
                sel_top = rows.len();
            }
            let win = r.loc.split_once(':').map(|x| x.1).unwrap_or("");
            let mut rest = format!("{win} {}", r.cwd);
            let avail = cols.saturating_sub(6 + UnicodeWidthStr::width(r.agent.as_str()));
            if avail > 0 {
                rest = clip(&rest, avail);
            }
            let bg = if selected {
                color(self.palette.state_bg_ink(&r.state, self.plugin_selected))
            } else {
                Color::Reset
            };
            let mut spans = vec![Span::raw(" ")];
            spans.extend(self.mark_spans(selected, &r.state));
            spans.extend([
                self.dot_span(&r.state),
                Span::raw(" "),
                Span::styled(r.agent.clone(), bold),
                Span::raw(" "),
                Span::styled(rest, muted),
            ]);
            rows.push(Row {
                line: Line::from(spans),
                bg,
                pane: r.pane.clone(),
                index: n + 1,
                selected,
            });
            if !r.title.is_empty() {
                let t = clip(&r.title, cols.saturating_sub(5));
                rows.push(Row {
                    line: Line::from(vec![Span::raw("     "), Span::styled(t, muted)]),
                    bg,
                    pane: r.pane.clone(),
                    index: n + 1,
                    selected,
                });
            }
            if selected {
                sel_bot = rows.len() - 1;
            }
        }
        (rows, sel_top, sel_bot)
    }

    fn inventory_rows(&self, cols: usize, cursor: Option<usize>) -> (Vec<Row>, usize, usize) {
        let muted = fg(&self.palette.muted_fg, "2");
        let accent = fg(&self.palette.accent_fg, "1");
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut groups: Vec<(usize, Vec<(usize, Vec<(usize, usize)>)>)> = Vec::new();
        for (ordinal, visible) in self.visible.iter().copied().enumerate() {
            let VisiblePane::Inventory(pane_i) = visible else {
                continue;
            };
            let pane = &self.panes[pane_i];
            if groups
                .last()
                .is_none_or(|(i, _)| self.panes[*i].session_id != pane.session_id)
            {
                groups.push((pane_i, Vec::new()));
            }
            let windows = &mut groups.last_mut().unwrap().1;
            if windows
                .last()
                .is_none_or(|(i, _)| self.panes[*i].window_id != pane.window_id)
            {
                windows.push((pane_i, Vec::new()));
            }
            windows.last_mut().unwrap().1.push((ordinal, pane_i));
        }
        let mut counts = std::collections::HashMap::new();
        for pane in &self.panes {
            *counts
                .entry((pane.session_id.as_str(), pane.window_id.as_str()))
                .or_insert(0usize) += 1;
        }
        let mut rows = Vec::new();
        let (mut sel_top, mut sel_bot) = (0usize, 0usize);
        for (session_i, windows) in &groups {
            let session = &self.panes[*session_i];
            rows.push(header_row(
                Line::from(Span::styled(clip(&session.session_name, cols), accent)),
                Color::Reset,
            ));
            for (window_i, panes) in windows {
                let window = &self.panes[*window_i];
                let expanded = counts[&(window.session_id.as_str(), window.window_id.as_str())] > 1;
                if expanded {
                    rows.push(header_row(
                        Line::from(vec![
                            Span::raw("   "),
                            Span::styled(format!("{WINDOW} {}", window.window_name), accent),
                        ]),
                        Color::Reset,
                    ));
                }
                for (ordinal, pane_i) in panes {
                    let pane = &self.panes[*pane_i];
                    let selected = Some(*ordinal) == cursor;
                    if selected {
                        sel_top = rows.len();
                    }
                    let agent = self.visible_agent_row(VisiblePane::Inventory(*pane_i));
                    let state = agent.map_or("idle", |row| row.state.as_str());
                    let mut spans = vec![Span::raw(" ")];
                    if agent.is_some() {
                        spans.extend(self.mark_spans(selected, state));
                    } else if selected {
                        spans.extend([Span::styled("❯", muted), Span::raw(" ")]);
                    } else {
                        spans.push(Span::raw("  "));
                    }
                    if expanded {
                        spans.push(Span::raw("  "));
                    }
                    if let Some(row) = agent {
                        spans.extend([
                            self.dot_span(state),
                            Span::raw(" "),
                            Span::styled(row.agent.clone(), bold),
                            Span::raw(" "),
                            Span::styled(pane.command.clone(), muted),
                        ]);
                    } else if expanded {
                        spans.push(Span::styled(format!("▦ {}", pane.command), muted));
                    } else {
                        spans.push(Span::styled(
                            format!("{WINDOW} {}", window.window_name),
                            muted,
                        ));
                    }
                    let bg = match (agent, selected) {
                        (Some(_), true) => {
                            color(self.palette.state_bg_ink(state, self.plugin_selected))
                        }
                        (None, true) => color(&self.palette.pane_bg),
                        _ => Color::Reset,
                    };
                    rows.push(Row {
                        line: Line::from(spans),
                        bg,
                        pane: pane.pane.clone(),
                        index: ordinal + 1,
                        selected,
                    });
                    if let Some(row) = agent.filter(|row| !row.title.is_empty()) {
                        let prefix = if expanded { "       " } else { "     " };
                        let title = clip(
                            &row.title,
                            cols.saturating_sub(UnicodeWidthStr::width(prefix)),
                        );
                        rows.push(Row {
                            line: Line::from(vec![Span::raw(prefix), Span::styled(title, muted)]),
                            bg,
                            pane: pane.pane.clone(),
                            index: ordinal + 1,
                            selected,
                        });
                    }
                    if selected {
                        sel_bot = rows.len() - 1;
                    }
                }
            }
        }
        (rows, sel_top, sel_bot)
    }

    /// Header line: title, filter/search indicator, update notice, focus fill.
    fn header_line(&self, cols: usize) -> Line<'static> {
        let header_fg = &self.palette.header_fg;
        let muted_ink = if self.palette.muted_fg == Ink::Inherited {
            header_fg
        } else {
            &self.palette.muted_fg
        };
        let hdr = if self.plugin_selected {
            color(&self.palette.header_bg)
        } else {
            Color::Reset
        };
        let (notice, notice_len) = match &self.update {
            Some(t) => {
                let plain = format!("↑{}", t.trim_start_matches('v'));
                (
                    Some(plain.clone()),
                    UnicodeWidthStr::width(plain.as_str()) + 1,
                )
            }
            None => (None, 0),
        };
        let filtering = self.state_filter.is_some() || !self.query.trim().is_empty();
        let mut filter = match self.state_filter {
            Some(state) => format!(" [{}]", state.label()),
            None if self.search_focused || !self.query.is_empty() => {
                let query: String = self.query.chars().filter(|c| !c.is_control()).collect();
                format!(" /{query}")
            }
            None => String::new(),
        };
        if filtering {
            let total = if self.settings.settings.show_all_panes {
                self.panes.len()
            } else {
                self.rows.len()
            };
            filter.push_str(&format!(" {}/{}", self.visible.len(), total));
        }
        let filter = clip(&filter, cols.saturating_sub(notice_len));
        let filter_len = UnicodeWidthStr::width(filter.as_str());
        let title = clip(&app_title(), cols.saturating_sub(filter_len + notice_len));
        let used = UnicodeWidthStr::width(title.as_str()) + filter_len + notice_len;
        let mut spans = vec![
            Span::styled(title, fg(header_fg, "1").bg(hdr)),
            Span::styled(filter, fg(muted_ink, "2").bg(hdr)),
        ];
        // The stock header resets after the notice, so its fill stops there.
        let default_header = Palette::default();
        let stock = self.palette.header_fg == default_header.header_fg
            && self.palette.header_bg == default_header.header_bg;
        // The pad inherits whatever SGR state the ANSI header left behind.
        let mut pad = fg(muted_ink, "").bg(hdr);
        if let Some(notice) = notice {
            // `\e[22m` precedes the notice: its leading space is not dim.
            spans.push(Span::styled(" ", fg(muted_ink, "").bg(hdr)));
            spans.push(Span::styled(notice, fg(muted_ink, "2").bg(hdr)));
            pad = if stock {
                Style::default()
            } else {
                fg(header_fg, "").bg(hdr)
            };
        }
        if self.plugin_selected {
            spans.push(Span::styled(" ".repeat(cols.saturating_sub(used)), pad));
        }
        Line::from(spans)
    }

    fn hint_line(&self) -> String {
        let nav = self.nav_label(true, false);
        if self.search_focused {
            join(&[
                self.hint(&self.search_keys, Action::Accept, "nav"),
                self.hint(&self.search_keys, Action::Clear, "clear"),
                self.hint(&self.search_keys, Action::Cancel, "clear"),
            ])
        } else if self.state_filter.is_some() {
            join(&[
                self.hint(&self.normal_keys, Action::Filter, "status"),
                nav,
                self.hint(&self.normal_keys, Action::Reset, "clear"),
            ])
        } else if !self.query.trim().is_empty() {
            join(&[
                nav,
                self.hint(&self.normal_keys, Action::Jump, "open"),
                self.hint(&self.normal_keys, Action::Reset, "clear"),
            ])
        } else if self.update.is_some() {
            self.hints(&[(Action::Versions, "update"), (Action::Search, "search")])
        } else {
            String::new()
        }
    }

    pub(super) fn render_ratatui(&mut self, force: bool) {
        if self.overlay.is_some() {
            self.render_overlay_ratatui(force);
            return;
        }
        let (cols, trows) = self.size();
        let cap = trows.saturating_sub(1);
        let mut buf = self.take_buf(Rect::new(0, 0, cols as u16, cap as u16));
        let muted = fg(&self.palette.muted_fg, "2");
        let mut vis = String::new();
        let mut y = 0u16;
        let put = |buf: &mut Buffer, line: &Line, bg: Color, y: u16| {
            if (y as usize) >= cap || cols == 0 {
                return;
            }
            if bg != Color::Reset {
                for x in 0..cols as u16 {
                    buf[(x, y)].set_bg(bg);
                }
            }
            let (end, _) = buf.set_line(0, y, line, cols as u16);
            for x in 0..end {
                buf[(x, y)].set_diff_option(CellDiffOption::Skip);
            }
        };
        put(&mut buf, &self.header_line(cols), Color::Reset, y);
        y += 1;
        let hint = clip(&self.hint_line(), cols);
        let has_hint = !hint.is_empty();
        if has_hint {
            put(
                &mut buf,
                &Line::from(Span::styled(hint, muted)),
                Color::Reset,
                y,
            );
            vis.push_str("-\n");
            y += 1;
        }
        let space = cap.saturating_sub(1 + usize::from(has_hint));
        let cursor = self.cursor_row();
        let inventory_mode = self.settings.settings.show_all_panes;
        let empty = if inventory_mode && self.panes.is_empty() {
            Some("no panes".to_string())
        } else if !inventory_mode && self.rows.is_empty() {
            Some("no agents".to_string())
        } else if self.visible.is_empty() {
            let reset = self.normal_keys[&Action::Reset]
                .first()
                .map(|c| format!(" · {} shows all", c.label(false)))
                .unwrap_or_default();
            Some(format!("no matches{reset}"))
        } else {
            None
        };
        if let Some(text) = empty {
            put(
                &mut buf,
                &Line::from(Span::styled(text, muted)),
                Color::Reset,
                y,
            );
        } else {
            let (rows, mut sel_top, sel_bot) = if inventory_mode {
                self.inventory_rows(cols, cursor)
            } else {
                self.agent_rows(cols, cursor)
            };
            if self.follow_selection && cursor.is_some() {
                if sel_top > 0 && rows[sel_top - 1].pane == "-" {
                    sel_top -= 1;
                }
                if space > 0 {
                    if sel_bot + 1 > self.scroll + space {
                        self.scroll = sel_bot + 1 - space;
                    }
                    if sel_top < self.scroll {
                        self.scroll = sel_top;
                    }
                }
            }
            self.follow_selection = false;
            self.scroll = if space > 0 {
                self.scroll.min(rows.len().saturating_sub(space))
            } else {
                0
            };
            let end = (self.scroll + space).min(rows.len());
            let overflow = rows.len() > space && space > 0 && cols > 0;
            let thumb_len = if overflow {
                (space * space).checked_div(rows.len()).unwrap_or(1).max(1)
            } else {
                0
            };
            let thumb_start = if overflow {
                self.scroll * (space - thumb_len) / rows.len().saturating_sub(space)
            } else {
                0
            };
            for (i, row) in rows[self.scroll..end].iter().enumerate() {
                put(&mut buf, &row.line, row.bg, y);
                if overflow {
                    let glyph = if (thumb_start..thumb_start + thumb_len).contains(&i) {
                        "▐"
                    } else {
                        "│"
                    };
                    let cell = &mut buf[(cols as u16 - 1, y)];
                    cell.set_symbol(glyph);
                    cell.set_style(Style::default().fg(Color::Reset).bg(Color::Reset));
                    cell.modifier = Modifier::DIM;
                }
                if row.pane == "-" {
                    vis.push_str("-\n");
                } else {
                    vis.push_str(&format!(
                        "{}\t{}\t{}\n",
                        row.pane,
                        row.index,
                        usize::from(row.selected)
                    ));
                }
                y += 1;
            }
        }
        self.emit(serialize(&buf, false), &vis, force);
        self.frame_buf = buf;
    }

    fn render_overlay_ratatui(&mut self, force: bool) {
        let title = app_title();
        let header = fg(&self.palette.header_fg, "1");
        let muted = fg(&self.palette.muted_fg, "2");
        let error = fg(&self.palette.error_fg, "2");
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let (cols, trows) = self.size();
        let mut lines: Vec<Line<'static>> = Vec::new();
        match &mut self.overlay {
            Some(Overlay::Help) => {
                lines.push(Line::from(Span::styled(format!("{title} — help"), header)));
                lines.push(Line::default());
                lines.push(Line::from(Span::styled("status", bold)));
                for (ink, glyph, what) in [
                    (&self.palette.idle_fg, "⣿", "idle"),
                    (&self.palette.working_fg, "⠹", "working (spinner)"),
                    (
                        &self.palette.blocked_fg,
                        "⣿",
                        "blocked, waiting for input (blinks)",
                    ),
                    (&self.palette.done_fg, "⣿", "done, not viewed yet (blinks)"),
                ] {
                    // The ANSI literal's `\` continuation eats its leading space.
                    lines.push(Line::from(vec![
                        Span::styled(glyph, fg(ink, "")),
                        Span::raw(format!("  {what}")),
                    ]));
                }
                lines.push(Line::default());
                lines.push(Line::from(Span::styled("keys", bold)));
                let accept = self.search_keys[&Action::Accept]
                    .first()
                    .map_or(String::new(), |c| {
                        format!(
                            "; {} enables {}",
                            c.label(false),
                            self.nav_label(false, false)
                        )
                    });
                let mut keys = vec![(self.nav_label(false, true), "move selection".to_string())];
                let jump = if self.settings.settings.show_all_panes {
                    "jump to pane"
                } else {
                    "jump to agent"
                };
                for (action, what) in [
                    (Action::Jump, jump.to_string()),
                    (Action::Search, format!("live search{accept}")),
                    (Action::Filter, "select next state filter".into()),
                    (Action::Reset, "clear filters / show all".into()),
                    (Action::Versions, "update / switch version".into()),
                    (Action::Close, "close sidebar".into()),
                    (Action::Help, "this help".into()),
                ] {
                    keys.push((self.labels(&self.normal_keys, action, false), what));
                }
                for (label, what) in keys.iter().filter(|(label, _)| !label.is_empty()) {
                    lines.push(Line::from(Span::raw(format!("{label:<8} {what}"))));
                }
                lines.push(Line::default());
                lines.push(Line::from(Span::styled("press any key to return", muted)));
            }
            Some(Overlay::Versions { sel, chosen }) => {
                let cur = current_tag();
                let tags = known_tags(&self.plugin_dir);
                *sel = picker_sel(&tags, &cur, chosen.as_deref(), *sel);
                lines.push(Line::from(Span::styled(
                    format!("{title} — versions"),
                    header,
                )));
                lines.push(Line::default());
                if tags.is_empty() {
                    lines.push(Line::from(vec![
                        Span::raw(" "),
                        Span::styled("no releases found — checking…", error),
                    ]));
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        self.hint(&self.normal_keys, Action::Close, "back"),
                        muted,
                    )));
                } else {
                    let mark_style = fg(self.palette.state_fg("idle"), "1");
                    for (i, t) in tags.iter().enumerate() {
                        let mut spans = if i == *sel {
                            vec![Span::styled("❯", mark_style), Span::raw(" ")]
                        } else {
                            vec![Span::raw("  ")]
                        };
                        spans.push(Span::raw(t.clone()));
                        if *t == cur {
                            spans.push(Span::raw(" "));
                            spans.push(Span::styled("(current)", muted));
                        }
                        lines.push(Line::from(spans));
                    }
                    lines.push(Line::default());
                    let hint = join(&[
                        self.hint(&self.normal_keys, Action::Jump, "switch"),
                        self.nav_label(true, true),
                        self.hint(&self.normal_keys, Action::Close, "back"),
                    ]);
                    lines.push(Line::from(Span::styled(hint, muted)));
                }
            }
            None => return,
        }
        let cap = trows.saturating_sub(1);
        // Help wraps: its longest hints outgrow a narrow sidebar, and clipping
        // them loses the text entirely. The versions list stays one line a row.
        let help = matches!(self.overlay, Some(Overlay::Help));
        let height = if help { cap } else { lines.len().min(cap) } as u16;
        let area = Rect::new(0, 0, cols as u16, height);
        let mut buf = self.take_buf(area);
        if help {
            // Render one row past the pane to see overflow: wrapping can outgrow
            // a narrow sidebar, and the footer beats the blank spacers, so drop
            // those instead of letting the text fall off the bottom.
            let probe = Rect::new(0, 0, cols as u16, height + 1);
            buf.resize(probe);
            let wrap = |lines: Vec<Line<'static>>, buf: &mut Buffer| {
                Paragraph::new(Text::from(lines))
                    .wrap(Wrap { trim: false })
                    .render(probe, buf);
            };
            wrap(lines.clone(), &mut buf);
            if (0..cols as u16).any(|x| buf[(x, height)].symbol() != " ") {
                buf.reset();
                lines.retain(|line| line.width() > 0);
                wrap(lines, &mut buf);
            }
            buf.resize(area);
        } else {
            for (y, line) in lines.iter().take(height as usize).enumerate() {
                let (end, _) = buf.set_line(0, y as u16, line, cols as u16);
                for x in 0..end {
                    buf[(x, y as u16)].set_diff_option(CellDiffOption::Skip);
                }
            }
        }
        // A non-default header fill spans the row, as `bar` does in the ANSI path.
        if height > 0 && self.palette.header_bg != Palette::default().header_bg {
            let bg = color(&self.palette.header_bg);
            for x in 0..cols as u16 {
                buf[(x, 0)].set_bg(bg);
            }
        }
        self.emit(serialize(&buf, true), "", force);
        self.frame_buf = buf;
    }
}

#[cfg(test)]
mod tests {
    use super::super::filter::StateFilter;
    use super::super::{new_sidebar, Daemon, Overlay};
    use super::*;
    use crate::pane_writers::PaneWriters;
    use crate::scan::{PaneMeta, PaneRow};
    use crate::tmux::Tmux;
    use std::collections::HashMap;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn row(pane: &str, state: &str, title: &str) -> PaneRow {
        PaneRow {
            pane: pane.into(),
            loc: "s:1.1".into(),
            agent: "pi".into(),
            state: state.into(),
            cwd: "repo".into(),
            title: title.into(),
        }
    }

    fn pane(
        session: (&str, &str),
        window: (&str, u32, &str),
        pane: (&str, u32, &str),
        agent: Option<usize>,
    ) -> PaneMeta {
        PaneMeta {
            pane: pane.0.into(),
            pane_index: pane.1,
            pane_title: String::new(),
            command: pane.2.into(),
            path: format!("/workspace/{}", window.2),
            window_id: window.0.into(),
            window_index: window.1,
            window_name: window.2.into(),
            session_id: session.0.into(),
            session_name: session.1.into(),
            agent_index: agent,
        }
    }

    /// Screen text without SGR/cursor escapes, so widths can be measured.
    fn plain(s: &str) -> String {
        regex::Regex::new(&format!("{E}\\[[0-9;?]*[A-Za-z]"))
            .unwrap()
            .replace_all(s, "")
            .into_owned()
    }

    /// What tmux shows for a frame at the given size, attributes included.
    fn screen(socket: &str, session: &str, frame: &str) -> String {
        let out = Command::new("tmux")
            .args([
                "-S",
                socket,
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                session,
                "/bin/bash",
                "-c",
                "printf '%s' \"$1\"; exec sleep 30",
                "_",
                frame,
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let pane = String::from_utf8(out.stdout).unwrap();
        let pane = pane.trim().to_string();
        let capture = || {
            let out = Command::new("tmux")
                .args(["-S", socket, "capture-pane", "-p", "-e", "-t", &pane])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        let mut last = capture();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            let next = capture();
            if next == last && !next.trim().is_empty() {
                break;
            }
            last = next;
        }
        let _ = Command::new("tmux")
            .args(["-S", socket, "kill-pane", "-t", &pane])
            .status();
        last.lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    }

    // Cell-level parity gate: every state the byte fixture covers must land on
    // the same tmux screen whether built as ANSI strings or via ratatui.
    #[test]
    fn ratatui_matches_ansi_screens() {
        // The child has its own pid; the socket path must travel with it.
        let socket = std::env::var("AGENMUX_RT_TEST_CHILD")
            .unwrap_or_else(|_| format!("/tmp/agenmux-rt-{}.sock", std::process::id()));
        // Screens live on a second server: hundreds of window add/close
        // notifications would otherwise fill the unread control pipe and block
        // the sidebar's control client from ever detaching.
        let screens = format!("{socket}.screen");
        if std::env::var_os("AGENMUX_RT_TEST_CHILD").is_none() {
            for (sock, name, w, h) in [
                (&socket, "ctl", "80", "40"),
                (&screens, "wide", "80", "40"),
                (&screens, "narrow", "30", "24"),
            ] {
                assert!(Command::new("tmux")
                    .args([
                        "-S",
                        sock,
                        "-f",
                        "/dev/null",
                        "new-session",
                        "-d",
                        "-s",
                        name,
                        "-x",
                        w,
                        "-y",
                        h,
                        "/bin/bash",
                        "-c",
                        "sleep 3600"
                    ])
                    .status()
                    .unwrap()
                    .success());
            }
            let result = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sidebar::ratatui_render::tests::ratatui_matches_ansi_screens",
                    "--nocapture",
                ])
                .env("AGENMUX_RT_TEST_CHILD", &socket)
                .env("AGENMUX_THEME_TEST_CHILD", "1")
                .env("TMUX", format!("{socket},0,0"))
                .output()
                .unwrap();
            for sock in [&socket, &screens] {
                let _ = Command::new("tmux")
                    .args(["-S", sock, "kill-server"])
                    .status();
            }
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let dir = std::env::temp_dir().join(format!("agenmux-rt-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("target/release")).unwrap();
        std::fs::write(
            dir.join("target/release/.agenmux-tags"),
            format!("v9.9.9\n{}\n", current_tag()),
        )
        .unwrap();
        let settings =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        let mut sb = new_sidebar(
            Tmux::connect().unwrap_or_else(|e| panic!("{e}")),
            dir.clone(),
            dir.join("cache"),
            dir.join("rows"),
            String::new(),
            settings,
        );
        sb.daemon = Some(Daemon {
            keys_path: dir.join("keys"),
            keys_fd: -1,
            writers: PaneWriters::new(),
            size: (80, 40),
            seen_mirror: false,
            empty_ticks: 0,
            client: String::new(),
            started: Instant::now(),
            win_sizes: HashMap::new(),
            attached: String::new(),
        });
        let agents = |n: usize| -> Vec<PaneRow> {
            (0..n)
                .map(|i| {
                    row(
                        &format!("%{}", i + 1),
                        ["blocked", "working", "idle", "done"][i % 4],
                        "Synthetic task",
                    )
                })
                .collect()
        };
        // Wide glyphs cost two columns each: rows must still fit `cols` without
        // wrapping, and each drawn row keeps exactly one rows-file line.
        sb.daemon.as_mut().unwrap().size = (12, 24);
        sb.rows = vec![PaneRow {
            pane: "%1".into(),
            loc: "s:1.1".into(),
            agent: "矩阵".into(),
            state: "idle".into(),
            cwd: "repo".into(),
            title: "很长的会话很长的会话很长的会话".into(),
        }];
        sb.sel = 1;
        sb.active = "%1".into();
        sb.rebuild_visible(false);
        sb.render_ratatui(true);
        let wide = sb.last_frame.clone();
        let wide_plain = plain(&wide);
        for line in wide_plain.lines() {
            assert!(
                UnicodeWidthStr::width(line) <= 12,
                "row {line:?} exceeds 12 cells: {wide:?}"
            );
        }
        assert!(
            !wide_plain.contains("很长的会话很长的会话很长的会话"),
            "title not clipped: {wide:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&sb.rows_file)
                .unwrap()
                .lines()
                .count(),
            wide.matches('\n').count() - 1, // the app header has no rows-file line
            "rows file vs drawn rows: {wide:?}"
        );

        let themes = [
            "",
            "[theme]\nbase = 'light'",
            "[theme]\nbase = 'terminal'",
            "[theme.colors]\nheader_fg = 17\nheader_bg = 123\nmuted_fg = 99",
        ];
        let mut checked = 0;
        for theme in themes {
            sb.palette = if theme.is_empty() {
                Palette::default()
            } else {
                Palette::resolve(
                    crate::app_config::parse(theme)
                        .unwrap()
                        .theme
                        .as_ref()
                        .unwrap(),
                )
            };
            for (size, name, count) in [
                ((80usize, 40usize), "wide", 4usize),
                ((30, 24), "narrow", 4),
                ((30, 24), "narrow", 40),
            ] {
                sb.daemon.as_mut().unwrap().size = size;
                for mode in 0..12 {
                    sb.settings.settings.show_all_panes = false;
                    sb.rows = agents(count);
                    sb.panes.clear();
                    sb.query.clear();
                    sb.state_filter = None;
                    sb.search_focused = false;
                    sb.overlay = None;
                    sb.update = None;
                    sb.plugin_selected = mode % 2 == 0;
                    sb.sel = 1 + mode % count.min(4);
                    sb.active = format!("%{}", sb.sel);
                    sb.tick = [0, 2, 7][mode % 3];
                    sb.scroll = 0;
                    sb.follow_selection = true;
                    match mode {
                        4 => {
                            sb.query = "repo".into();
                            sb.search_focused = true;
                        }
                        5 => sb.state_filter = Some(StateFilter::Working),
                        6 => sb.update = Some("v9.9.9".into()),
                        7 => sb.overlay = Some(Overlay::Help),
                        8 => {
                            sb.overlay = Some(Overlay::Versions {
                                sel: 0,
                                chosen: None,
                            })
                        }
                        9 => sb.query = "absent".into(),
                        10 => sb.rows.clear(),
                        11 => {
                            sb.settings.settings.show_all_panes = true;
                            sb.rows = vec![PaneRow {
                                pane: "%22".into(),
                                loc: "work:2.2".into(),
                                agent: "claude".into(),
                                state: "working".into(),
                                cwd: "repo".into(),
                                title: "Implement sidebar tree".into(),
                            }];
                            sb.panes = vec![
                                pane(
                                    ("$1", "work"),
                                    ("@1", 1, "editor"),
                                    ("%11", 1, "nvim"),
                                    None,
                                ),
                                pane(("$1", "work"), ("@2", 2, "server"), ("%21", 1, "npm"), None),
                                pane(
                                    ("$1", "work"),
                                    ("@2", 2, "server"),
                                    ("%22", 2, "node"),
                                    Some(0),
                                ),
                                pane(
                                    ("$2", "personal"),
                                    ("@3", 3, "shell"),
                                    ("%31", 1, "zsh"),
                                    None,
                                ),
                            ];
                            sb.sel = 3;
                            sb.active = "%22".into();
                            sb.active_session = "$1".into();
                        }
                        _ => {}
                    }
                    sb.rebuild_visible(false);
                    let scroll = sb.scroll;
                    sb.follow_selection = true;
                    sb.render(true);
                    let ansi = sb.last_frame.clone();
                    let ansi_rows = std::fs::read_to_string(&sb.rows_file).unwrap();
                    sb.scroll = scroll;
                    sb.follow_selection = true;
                    sb.render_ratatui(true);
                    let rt = sb.last_frame.clone();
                    let rt_rows = std::fs::read_to_string(&sb.rows_file).unwrap();
                    let label = format!("theme={theme:?} size={size:?} agents={count} mode={mode}");
                    assert_eq!(rt_rows, ansi_rows, "rows file: {label}");
                    let (rt_screen, ansi_screen) =
                        (screen(&screens, name, &rt), screen(&screens, name, &ansi));
                    if mode == 7 {
                        // Help wraps in the ratatui path, so the screens differ
                        // by design: check fit, content and the header instead.
                        let text = plain(&rt_screen);
                        for line in text.lines() {
                            assert!(
                                UnicodeWidthStr::width(line) <= size.0,
                                "help row {line:?} exceeds {} cells: {label}",
                                size.0
                            );
                        }
                        let joined = text.lines().collect::<Vec<_>>().join(" ");
                        let flat = joined.split_whitespace().collect::<Vec<_>>().join(" ");
                        for want in [
                            "blocked, waiting for input (blinks)",
                            "press any key to return",
                        ] {
                            assert!(
                                flat.contains(want),
                                "help missing {want:?}: {label}\n{flat}"
                            );
                        }
                        assert_eq!(
                            rt_screen.lines().next(),
                            ansi_screen.lines().next(),
                            "help header: {label}"
                        );
                    } else {
                        assert_eq!(
                            rt_screen, ansi_screen,
                            "screen: {label}\nansi={ansi:?}\nrt={rt:?}"
                        );
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked >= 100, "{checked}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// µs/frame and frame bytes for the 30×24, 40-agent focused state:
    /// `cargo test --features ratatui --release -- --ignored --nocapture bench`.
    #[test]
    #[ignore = "benchmark, not a gate"]
    fn bench_render_frames() {
        let socket = format!("/tmp/agenmux-rt-bench-{}.sock", std::process::id());
        assert!(Command::new("tmux")
            .args([
                "-S",
                &socket,
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "bench",
                "-x",
                "30",
                "-y",
                "24",
                "/bin/bash",
                "-c",
                "sleep 600"
            ])
            .status()
            .unwrap()
            .success());
        // `#[ignore]`d, so `--ignored` runs this alone: no other test races TMUX.
        std::env::set_var("TMUX", format!("{socket},0,0"));
        let dir = std::env::temp_dir().join(format!("agenmux-rt-bench-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        let mut sb = new_sidebar(
            Tmux::connect().unwrap_or_else(|e| panic!("{e}")),
            dir.clone(),
            dir.join("cache"),
            dir.join("rows"),
            String::new(),
            settings,
        );
        sb.daemon = Some(Daemon {
            keys_path: dir.join("keys"),
            keys_fd: -1,
            writers: PaneWriters::new(),
            size: (30, 24),
            seen_mirror: false,
            empty_ticks: 0,
            client: String::new(),
            started: Instant::now(),
            win_sizes: HashMap::new(),
            attached: String::new(),
        });
        sb.rows = (0..40)
            .map(|i| {
                row(
                    &format!("%{}", i + 1),
                    ["blocked", "working", "idle", "done"][i % 4],
                    "Synthetic task",
                )
            })
            .collect();
        sb.plugin_selected = true;
        sb.sel = 3;
        sb.active = "%3".into();
        sb.rebuild_visible(false);
        let n = 2000;
        // The rows-file write dominates both paths, so also measure without it.
        for (sink, note) in [
            (dir.join("rows"), "with rows file"),
            (std::path::PathBuf::from("/dev/null"), "frame only"),
        ] {
            sb.rows_file = sink;
            for (label, ansi) in [("ratatui", false), ("ansi", true)] {
                let scroll = sb.scroll;
                let start = Instant::now();
                for _ in 0..n {
                    sb.scroll = scroll;
                    if ansi {
                        sb.render(true); // test builds dispatch `render` to ANSI
                    } else {
                        sb.render_ratatui(true);
                    }
                }
                let us = start.elapsed().as_secs_f64() * 1e6 / f64::from(n);
                println!(
                    "{label} ({note}): {us:.1} µs/frame, {} frame bytes",
                    sb.last_frame.len()
                );
            }
        }
        let _ = Command::new("tmux")
            .args(["-S", &socket, "kill-server"])
            .status();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
