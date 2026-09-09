use crate::app_config::{Action, Keymap, Palette};
use crate::input::term_size;
use std::io::Write;

use super::overlay::current_tag;
use super::{Sidebar, VisiblePane, E};

const SPIN: [char; 8] = ['⠹', '⢸', '⣰', '⣤', '⣆', '⡇', '⠏', '⠛'];

/// " · "-separated hint segments, skipping unbound (empty) ones.
pub(super) fn join(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

pub(super) fn bar(line: &str, bg: &str, cols: usize, width: usize) -> String {
    if bg.is_empty() {
        return line.into();
    }
    let body = line.replace(&format!("{E}[0m"), &format!("{E}[0m{bg}"));
    format!("{bg}{body}{}{E}[0m", " ".repeat(cols.saturating_sub(width)))
}

pub(super) fn cursor_mark(
    palette: &Palette,
    selected: bool,
    plugin_selected: bool,
    state: &str,
) -> String {
    if !selected {
        return "  ".into();
    }
    let fg = palette
        .state_fg(state)
        .fg(if plugin_selected { "1" } else { "" });
    format!("{fg}❯{E}[0m ")
}

/// Clip generated SGR/CSI frames without splitting an escape or wrapping a
/// logical click row. Layout elsewhere uses the same character-cell metric.
fn clip_frame(frame: &str, cols: usize, cap: usize) -> String {
    if cols == 0 || cap == 0 {
        return format!("{E}[H{E}[0m{E}[J");
    }
    let mut out = String::new();
    let mut row = 0;
    let mut col = 0;
    let mut chars = frame.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            if let Some(next) = chars.next() {
                out.push(next);
                if next == '[' {
                    let mut csi_column = 0usize;
                    let mut column_only = true;
                    let mut saw_digit = false;
                    for parameter in chars.by_ref() {
                        out.push(parameter);
                        if parameter.is_ascii_digit() && column_only {
                            saw_digit = true;
                            csi_column = csi_column
                                .saturating_mul(10)
                                .saturating_add(parameter as usize - '0' as usize);
                        } else if ('@'..='~').contains(&parameter) {
                            if parameter == 'G' && column_only {
                                col = if saw_digit { csi_column } else { 1 }.saturating_sub(1);
                            }
                            break;
                        } else {
                            column_only = false;
                        }
                    }
                }
            }
        } else if c == '\n' {
            out.push(c);
            col = 0;
            row += 1;
            if row >= cap {
                let tail: String = chars.collect();
                // Keep the historical clear-to-end suffix when already fitted.
                if tail == format!("{E}[J") {
                    out.push_str(&tail);
                } else {
                    out.push_str(&format!("{E}[0m{E}[J"));
                }
                break;
            }
        } else {
            if col < cols {
                out.push(c);
            }
            col += 1;
        }
    }
    out
}

pub(super) fn app_title() -> String {
    // The isolated renderer fixture child needs identical title geometry in
    // debug/release builds. Production builds have no test override.
    #[cfg(test)]
    if std::env::var_os("AGENMUX_THEME_TEST_CHILD").is_some() {
        return "agenmux dev (2000-01-01 00:00)".into();
    }
    if cfg!(debug_assertions) {
        format!(
            "agenmux dev ({})",
            option_env!("AGENMUX_BUILD_TIMESTAMP").unwrap_or("unknown")
        )
    } else {
        format!("agenmux {}", current_tag())
    }
}

impl Sidebar {
    /// "<first chord> <what>", or nothing when the action is unbound.
    pub(super) fn hint(&self, keys: &Keymap, action: Action, what: &str) -> String {
        keys[&action]
            .first()
            .map_or(String::new(), |c| format!("{} {what}", c.label(true)))
    }
    pub(super) fn hints(&self, parts: &[(Action, &str)]) -> String {
        join(
            &parts
                .iter()
                .map(|(action, what)| self.hint(&self.normal_keys, *action, what))
                .collect::<Vec<_>>(),
        )
    }
    /// Every chord of an action, "/"-joined: "Enter/l".
    pub(super) fn labels(&self, keys: &Keymap, action: Action, short: bool) -> String {
        keys[&action]
            .iter()
            .map(|c| c.label(short))
            .collect::<Vec<_>>()
            .join("/")
    }
    /// Down/up pairs: "j/k" (first pair) or "j/k ↓/↑" (all pairs).
    pub(super) fn nav_label(&self, short: bool, all: bool) -> String {
        let (down, up) = (
            &self.normal_keys[&Action::Down],
            &self.normal_keys[&Action::Up],
        );
        let pairs = if all { down.len().max(up.len()) } else { 1 };
        (0..pairs)
            .filter_map(|i| match (down.get(i), up.get(i)) {
                (Some(d), Some(u)) => Some(format!("{}/{}", d.label(short), u.label(short))),
                (Some(c), None) | (None, Some(c)) => Some(c.label(short)),
                (None, None) => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn dot(&self, state: &str) -> String {
        let on = (self.tick / 2).is_multiple_of(2);
        let fg = self.palette.state_fg(state).fg("");
        match state {
            "blocked" => {
                if on {
                    format!("{fg}⣿{E}[0m")
                } else {
                    " ".into()
                }
            }
            "working" => format!("{fg}{}{E}[0m", SPIN[(self.tick % 8) as usize]),
            "done" => {
                if on {
                    format!("{fg}⣿{E}[0m")
                } else {
                    " ".into()
                }
            }
            _ => format!("{fg}⣿{E}[0m"),
        }
    }

    /// Frame and click-row sink: stdout in tty mode; direct writes to visible
    /// empty panes in daemon mode.
    pub(super) fn emit(&mut self, frame: String, rows: &str, force: bool) {
        let (cols, height) = self
            .daemon
            .as_ref()
            .map(|d| d.size)
            .unwrap_or_else(term_size);
        let cap = if cols == 0 {
            0
        } else {
            height.saturating_sub(1)
        };
        let frame = clip_frame(&frame, cols, cap);
        let rows: String = rows
            .lines()
            .take(cap.saturating_sub(1))
            .map(|r| format!("{r}\n"))
            .collect();
        // Restore the normal foreground after glyph/attribute resets, including
        // inside filled rows. Inherited dark foreground adds no bytes.
        let fg = self.palette.text_fg.fg("");
        let frame = if fg.is_empty() {
            frame
        } else {
            format!(
                "{fg}{}",
                frame.replace(&format!("{E}[0m"), &format!("{E}[0m{fg}"))
            )
        };
        // Rows map must match what the click helper sees under each rendered row.
        let _ = std::fs::write(&self.rows_file, &rows);
        let changed = force || frame != self.last_frame;
        match &mut self.daemon {
            None => {
                if changed {
                    print!("{frame}");
                    let _ = std::io::stdout().flush();
                }
            }
            Some(d) => {
                if changed {
                    d.writers.emit(&frame);
                }
            }
        }
        if changed {
            self.last_frame = frame;
        }
    }

    fn inventory_lines(
        &self,
        cols: usize,
        cursor: Option<usize>,
        accent: &str,
        muted: &str,
    ) -> (Vec<(String, String, usize, bool)>, usize, usize) {
        let mut groups: Vec<(usize, Vec<(usize, Vec<(usize, usize)>)>)> = Vec::new();
        for (ordinal, visible) in self.visible.iter().copied().enumerate() {
            let VisiblePane::Inventory(pane_i) = visible else {
                continue;
            };
            let pane = &self.panes[pane_i];
            let new_session = groups
                .last()
                .is_none_or(|(i, _)| self.panes[*i].session_id != pane.session_id);
            if new_session {
                groups.push((pane_i, Vec::new()));
            }
            let windows = &mut groups.last_mut().unwrap().1;
            let new_window = windows
                .last()
                .is_none_or(|(i, _)| self.panes[*i].window_id != pane.window_id);
            if new_window {
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
        let mut lines = Vec::new();
        let (mut sel_top, mut sel_bot) = (0usize, 0usize);
        for (session_i, windows) in &groups {
            let session = &self.panes[*session_i];
            let name: String = session.session_name.chars().take(cols).collect();
            lines.push((format!("{accent}{name}{E}[0m{E}[K\n"), "-".into(), 0, false));
            for (window_pos, (window_i, panes)) in windows.iter().enumerate() {
                let window = &self.panes[*window_i];
                let last_window = window_pos + 1 == windows.len();
                let window_branch = if last_window { "└─" } else { "├─" };
                let expanded = counts[&(window.session_id.as_str(), window.window_id.as_str())] > 1;
                if expanded {
                    lines.push((
                        format!(
                            "  {accent}{window_branch} {} {}{E}[0m{E}[K\n",
                            window.window_index, window.window_name
                        ),
                        "-".into(),
                        0,
                        false,
                    ));
                }
                for (pane_pos, (ordinal, pane_i)) in panes.iter().enumerate() {
                    let pane = &self.panes[*pane_i];
                    let selected = Some(*ordinal) == cursor;
                    if selected {
                        sel_top = lines.len();
                    }
                    let agent = self.visible_agent_row(VisiblePane::Inventory(*pane_i));
                    let state = agent.map_or("idle", |row| row.state.as_str());
                    let mark = if agent.is_some() {
                        cursor_mark(&self.palette, selected, self.plugin_selected, state)
                    } else if selected {
                        format!("{accent}❯{E}[0m ")
                    } else {
                        "  ".into()
                    };
                    let pane_branch = if pane_pos + 1 == panes.len() {
                        "└─"
                    } else {
                        "├─"
                    };
                    let prefix = if expanded {
                        format!(
                            "{}  {pane_branch} {}",
                            if last_window { "  " } else { "│ " },
                            pane.pane_index
                        )
                    } else {
                        format!("{window_branch} {} {}", pane.window_index, pane.window_name)
                    };
                    let detail = if let Some(row) = agent {
                        format!(
                            " {} {E}[1m{}{E}[0m {muted}{} · {}{E}[0m",
                            self.dot(state),
                            row.agent,
                            state,
                            row.cwd
                        )
                    } else if expanded {
                        format!(" {muted}{}{E}[0m", pane.command)
                    } else {
                        format!(" {muted}· {}{E}[0m", pane.command)
                    };
                    let row = format!("{mark}{prefix}{detail}");
                    let row_bg = agent.filter(|_| selected).map_or(String::new(), |_| {
                        self.palette.state_bg(state, self.plugin_selected)
                    });
                    let mut chars = row.chars();
                    let mut width = 0;
                    while let Some(c) = chars.next() {
                        if c == '\x1b' && chars.next() == Some('[') {
                            for parameter in chars.by_ref() {
                                if ('@'..='~').contains(&parameter) {
                                    break;
                                }
                            }
                        } else {
                            width += 1;
                        }
                    }
                    lines.push((
                        format!("{}{E}[K\n", bar(&row, &row_bg, cols, width)),
                        pane.pane.clone(),
                        ordinal + 1,
                        selected,
                    ));
                    if let Some(row) = agent.filter(|row| !row.title.is_empty()) {
                        let title: String =
                            row.title.chars().take(cols.saturating_sub(10)).collect();
                        lines.push((
                            format!("          {muted}{title}{E}[0m{E}[K\n"),
                            "-".into(),
                            0,
                            false,
                        ));
                    }
                    if selected {
                        sel_bot = lines.len() - 1;
                    }
                }
            }
        }
        (lines, sel_top, sel_bot)
    }

    pub(super) fn render(&mut self, force: bool) {
        if self.overlay.is_some() {
            self.render_overlay(force);
            return;
        }
        let (cols, trows) = match &self.daemon {
            Some(d) => d.size,
            None => term_size(),
        };
        let cap = trows.saturating_sub(1); // last row's newline would scroll

        let muted = self.palette.muted_fg.fg("2");
        let header_fg = self.palette.header_fg.fg("");
        let header_bg = self.palette.header_bg.bg();
        let accent = self.palette.accent_fg.fg("1");
        // Update notice rides the header. Nonempty contextual/update hints add
        // one row; vis records it so mouse coordinates stay exact.
        let (notice, notice_len, update_hint) = match &self.update {
            Some(t) => {
                let plain = format!(" ↑{}", t.trim_start_matches('v'));
                (
                    format!(" {muted}↑{}{E}[0m", t.trim_start_matches('v')),
                    plain.chars().count(),
                    self.hints(&[(Action::Versions, "update"), (Action::Search, "search")]),
                )
            }
            None => (String::new(), 0, String::new()),
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
        let filter: String = filter
            .chars()
            .take(cols.saturating_sub(notice_len))
            .collect();
        let filter_len = filter.chars().count();
        let title: String = app_title()
            .chars()
            .take(cols.saturating_sub(filter_len + notice_len))
            .collect();
        let title_len = title.chars().count();
        let nav = self.nav_label(true, false);
        let hint = if self.search_focused {
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
        } else {
            update_hint
        };
        let hint: String = hint.chars().take(cols).collect();
        let has_hint = !hint.is_empty();
        let space = cap.saturating_sub(1 + usize::from(has_hint));
        let (hdr, hdr_pad) = if self.plugin_selected {
            let used = title_len + filter.chars().count() + notice_len;
            (header_bg.as_str(), " ".repeat(cols.saturating_sub(used)))
        } else {
            ("", String::new())
        };
        // Preserve historical header bytes regardless of unrelated role overrides.
        // Only non-default header styles need restoration after the notice reset.
        let default_header = Palette::default();
        let notice = if self.palette.header_fg == default_header.header_fg
            && self.palette.header_bg == default_header.header_bg
        {
            notice
        } else {
            notice.replace(&format!("{E}[0m"), &format!("{E}[0m{hdr}{header_fg}"))
        };
        let mut frame = format!(
            "{E}[H{hdr}{header_fg}{E}[1m{title}{E}[22m{muted}{filter}{E}[22m{notice}{hdr_pad}{E}[0m{E}[K\n"
        );
        let mut vis = String::new();
        if has_hint {
            frame.push_str(&format!("{muted}{hint}{E}[0m{E}[K\n"));
            vis.push_str("-\n");
        }
        let cursor = self.cursor_row();
        let inventory_mode = self.settings.settings.show_all_panes;
        if inventory_mode && self.panes.is_empty() {
            frame.push_str(&format!("{muted}no panes{E}[0m{E}[K\n"));
        } else if !inventory_mode && self.rows.is_empty() {
            frame.push_str(&format!("{muted}no agents{E}[0m{E}[K\n"));
        } else if self.visible.is_empty() {
            let reset = self.normal_keys[&Action::Reset]
                .first()
                .map(|c| format!(" · {} shows all", c.label(false)))
                .unwrap_or_default();
            frame.push_str(&format!("{muted}no matches{reset}{E}[0m{E}[K\n"));
        } else {
            // build selectable panes plus context, then window it
            let (mut lines, mut sel_top, mut sel_bot) = if inventory_mode {
                self.inventory_lines(cols, cursor, &accent, &muted)
            } else {
                (Vec::new(), 0usize, 0usize)
            };
            let mut session = "";
            if !inventory_mode {
                for (n, visible) in self.visible.iter().copied().enumerate() {
                    let VisiblePane::Agent(row_i) = visible else {
                        continue;
                    };
                    let r = &self.rows[row_i];
                    let sess = r.loc.split(':').next().unwrap_or("");
                    if sess != session {
                        session = sess;
                        // clip to pane width — a wrapped header shifts every row
                        // below it and breaks the click→rows-file mapping
                        let sess_clipped: String = sess.chars().take(cols).collect();
                        lines.push((
                            format!("{accent}{sess_clipped}{E}[0m{E}[K\n"),
                            "-".into(),
                            0,
                            false,
                        ));
                    }
                    if Some(n) == cursor {
                        sel_top = lines.len();
                    }
                    let selected = Some(n) == cursor;
                    let mark = cursor_mark(&self.palette, selected, self.plugin_selected, &r.state);
                    let dot = self.dot(&r.state);
                    let win = r.loc.split_once(':').map(|x| x.1).unwrap_or("");
                    let mut rest = format!("{win} {}", r.cwd);
                    let agent_len = r.agent.chars().count();
                    let avail = cols.saturating_sub(6 + agent_len);
                    if avail > 0 {
                        rest = rest.chars().take(avail).collect();
                    }
                    let row_bg = if selected {
                        self.palette.state_bg(&r.state, self.plugin_selected)
                    } else {
                        String::new()
                    };
                    let row = format!(" {mark}{dot} {E}[1m{}{E}[0m {muted}{rest}{E}[0m", r.agent);
                    let width = 6 + agent_len + rest.chars().count();
                    lines.push((
                        format!("{}{E}[K\n", bar(&row, &row_bg, cols, width)),
                        r.pane.clone(),
                        n + 1,
                        selected,
                    ));
                    if !r.title.is_empty() {
                        let t: String = r.title.chars().take(cols.saturating_sub(5)).collect();
                        let line = format!("     {muted}{t}{E}[0m");
                        let width = 5 + t.chars().count();
                        lines.push((
                            format!("{}{E}[K\n", bar(&line, &row_bg, cols, width)),
                            r.pane.clone(),
                            n + 1,
                            selected,
                        ));
                    }
                    if Some(n) == cursor {
                        sel_bot = lines.len() - 1;
                    }
                }
            }
            // cursor's session header gives context — drag it into view
            if self.follow_selection && cursor.is_some() {
                if sel_top > 0 && lines[sel_top - 1].1 == "-" {
                    sel_top -= 1;
                }
                if space > 0 {
                    if sel_bot + 1 > self.scroll + space {
                        self.scroll = sel_bot + 1 - space;
                    }
                    if sel_top < self.scroll {
                        self.scroll = sel_top; // top wins when row + title exceed space
                    }
                }
            }
            self.follow_selection = false;
            if space > 0 {
                self.scroll = self.scroll.min(lines.len().saturating_sub(space));
            } else {
                self.scroll = 0;
            }
            let end = (self.scroll + space).min(lines.len());
            let overflow = lines.len() > space && space > 0 && cols > 0;
            let thumb_len = if overflow {
                space
                    .saturating_mul(space)
                    .checked_div(lines.len())
                    .unwrap_or(1)
                    .max(1)
            } else {
                0
            };
            let thumb_start = if overflow {
                self.scroll.saturating_mul(space - thumb_len) / lines.len().saturating_sub(space)
            } else {
                0
            };
            for (row, (text, pane, index, selected)) in lines[self.scroll..end].iter().enumerate() {
                if overflow {
                    frame.push_str(text.trim_end_matches('\n'));
                    let glyph = if (thumb_start..thumb_start + thumb_len).contains(&row) {
                        '▐'
                    } else {
                        '│'
                    };
                    frame.push_str(&format!("{E}[{cols}G{E}[2m{glyph}{E}[0m\n"));
                } else {
                    frame.push_str(text);
                }
                if pane == "-" {
                    vis.push_str("-\n");
                } else {
                    vis.push_str(&format!("{pane}\t{index}\t{}\n", usize::from(*selected)));
                }
            }
        }
        frame.push_str(&format!("{E}[J"));
        self.emit(frame, &vis, force);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_writers::PaneWriters;
    use crate::scan::{PaneMeta, PaneRow};
    use crate::tmux::Tmux;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::super::filter::StateFilter;
    use super::super::{new_sidebar, Daemon, Overlay};

    fn row(pane: &str) -> PaneRow {
        PaneRow {
            pane: pane.into(),
            loc: "s:1.1".into(),
            agent: "pi".into(),
            state: "idle".into(),
            cwd: "repo".into(),
            title: String::new(),
        }
    }

    fn pane(
        session_id: &str,
        session_name: &str,
        window_id: &str,
        window_index: u32,
        window_name: &str,
        pane_id: &str,
        pane_index: u32,
        command: &str,
        agent_index: Option<usize>,
    ) -> PaneMeta {
        PaneMeta {
            pane: pane_id.into(),
            pane_index,
            pane_title: String::new(),
            command: command.into(),
            path: format!("/workspace/{window_name}"),
            window_id: window_id.into(),
            window_index,
            window_name: window_name.into(),
            session_id: session_id.into(),
            session_name: session_name.into(),
            agent_index,
        }
    }

    // Run the real renderer with a control connection isolated from every user
    // server. The child process owns TMUX, avoiding parallel-test env races.
    #[test]
    fn semantic_renderer_frames() {
        use std::process::Command;
        if std::env::var_os("AGENMUX_THEME_TEST_CHILD").is_none() {
            let socket =
                std::env::temp_dir().join(format!("agenmux-theme-{}.sock", std::process::id()));
            let socket = socket.to_str().unwrap();
            assert!(Command::new("tmux")
                .args([
                    "-S",
                    socket,
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    "theme",
                    "/bin/bash",
                    "-c",
                    "sleep 120"
                ])
                .status()
                .unwrap()
                .success());
            let result = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sidebar::render::tests::semantic_renderer_frames",
                    "--nocapture",
                ])
                .env("AGENMUX_THEME_TEST_CHILD", "1")
                .env("TMUX", format!("{socket},0,0"))
                .output()
                .unwrap();
            let _ = Command::new("tmux")
                .args(["-S", socket, "kill-server"])
                .status();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let dir = std::env::temp_dir().join(format!("agenmux-theme-frames-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("target/release")).unwrap();
        std::fs::write(dir.join("target/release/.agenmux-tags"), "v9.9.9\nv0.0.1\n").unwrap();
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
        sb.rows = ["blocked", "working", "idle", "done"]
            .iter()
            .enumerate()
            .map(|(i, state)| {
                let mut r = row(&format!("%{}", i + 1));
                r.state = (*state).into();
                r.title = "Synthetic task".into();
                r
            })
            .collect();
        sb.visible = (0..4).map(VisiblePane::Agent).collect();
        let mut frames = String::new();
        for focused in [false, true] {
            sb.plugin_selected = focused;
            for selected in 1..=4 {
                sb.sel = selected;
                sb.active = format!("%{selected}");
                for tick in [0, 2, 7] {
                    sb.tick = tick;
                    sb.render(true);
                    frames.push_str(&format!(
                        "focus={focused} selected={selected} tick={tick}\n{}\nrows={}\n",
                        sb.last_frame
                            .replace(&app_title(), "agenmux TEST")
                            .escape_default(),
                        std::fs::read_to_string(&sb.rows_file)
                            .unwrap()
                            .escape_default()
                    ));
                }
            }
        }
        for mode in 0..7 {
            sb.query.clear();
            sb.state_filter = None;
            sb.search_focused = false;
            sb.overlay = None;
            sb.update = None;
            match mode {
                0 => {
                    sb.query = "repo".into();
                    sb.search_focused = true;
                }
                1 => sb.state_filter = Some(StateFilter::Working),
                2 => sb.update = Some("v9.9.9".into()),
                3 => sb.overlay = Some(Overlay::Help),
                4 => {
                    sb.overlay = Some(Overlay::Versions {
                        sel: 0,
                        chosen: None,
                    })
                }
                5 => sb.query = "absent".into(),
                _ => sb.rows.clear(),
            }
            sb.rebuild_visible(false);
            sb.render(true);
            frames.push_str(&format!(
                "mode={mode}\n{}\nrows={}\n",
                sb.last_frame
                    .replace(&app_title(), "agenmux TEST")
                    .escape_default(),
                std::fs::read_to_string(&sb.rows_file)
                    .unwrap()
                    .escape_default()
            ));
        }
        let false_frames = frames.clone();
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
            pane("$1", "work", "@1", 1, "editor", "%11", 1, "nvim", None),
            pane("$1", "work", "@2", 2, "server", "%21", 1, "npm", None),
            pane("$1", "work", "@2", 2, "server", "%22", 2, "node", Some(0)),
            pane("$2", "personal", "@3", 3, "shell", "%31", 1, "zsh", None),
        ];
        sb.query.clear();
        sb.state_filter = None;
        sb.search_focused = false;
        sb.overlay = None;
        sb.update = None;
        sb.plugin_selected = true;
        sb.sel = 3;
        sb.active = "%22".into();
        sb.active_session = "$1".into();
        sb.rebuild_visible(false);
        sb.render(true);
        let selected_agent = sb
            .last_frame
            .lines()
            .find(|line| line.contains("claude"))
            .unwrap();
        let ansi = regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap();
        assert_eq!(
            ansi.replace_all(selected_agent, "").chars().count(),
            sb.daemon.as_ref().unwrap().size.0,
            "selected inventory agent background reaches the final column"
        );
        frames.push_str(&format!(
            "all-panes hierarchy\n{}\nrows={}\n",
            sb.last_frame
                .replace(&app_title(), "agenmux TEST")
                .escape_default(),
            std::fs::read_to_string(&sb.rows_file)
                .unwrap()
                .escape_default()
        ));

        for (label, query, state_filter) in [
            ("session query", "work", None),
            ("window query", "server", None),
            ("pane query", "%21", None),
            ("agent query", "claude", None),
            ("status filter", "ignored", Some(StateFilter::Working)),
            ("absent query", "absent", None),
        ] {
            sb.query = query.into();
            sb.state_filter = state_filter;
            sb.rebuild_visible(false);
            sb.render(true);
            if label == "pane query" {
                assert!(
                    sb.last_frame.contains("server") && sb.last_frame.contains("└─ 1"),
                    "a physical multi-pane window stays expanded after filtering"
                );
            }
            if label == "absent query" {
                assert!(sb.last_frame.contains("no matches"));
                assert!(!sb.last_frame.contains("work") && !sb.last_frame.contains("personal"));
            }
            frames.push_str(&format!(
                "all-panes {label}\n{}\nrows={}\n",
                sb.last_frame
                    .replace(&app_title(), "agenmux TEST")
                    .escape_default(),
                std::fs::read_to_string(&sb.rows_file)
                    .unwrap()
                    .escape_default()
            ));
        }
        sb.query.clear();
        sb.state_filter = None;
        sb.rebuild_visible(false);

        sb.panes.push(pane(
            "$2",
            "personal",
            "@4",
            4,
            "linked",
            "%22",
            1,
            "node",
            Some(0),
        ));
        sb.rebuild_visible(false);
        sb.plugin_selected = false;
        sb.active_session = "$2".into();
        assert_eq!(
            sb.cursor_row(),
            Some(4),
            "active linked pane prefers active session"
        );
        sb.select_index(5);
        assert_eq!(sb.sel_occurrence.as_ref().unwrap().session_id, "$2");
        sb.panes.retain(|pane| pane.window_id != "@4");
        sb.rebuild_visible(false);
        assert_eq!(sb.sel, 3, "selection falls back to the physical pane");
        assert_eq!(sb.sel_occurrence.as_ref().unwrap().session_id, "$1");
        sb.panes.retain(|pane| pane.pane != "%22");
        sb.rebuild_visible(false);
        assert_eq!(sb.sel, 3, "selection keeps the nearest valid ordinal");
        assert_eq!(sb.sel_pane, "%31");

        sb.plugin_selected = true;
        sb.panes.clear();
        sb.rebuild_visible(false);
        sb.render(true);
        frames.push_str(&format!(
            "all-panes empty\n{}\nrows={}\n",
            sb.last_frame
                .replace(&app_title(), "agenmux TEST")
                .escape_default(),
            std::fs::read_to_string(&sb.rows_file)
                .unwrap()
                .escape_default()
        ));

        if std::env::var_os("AGENMUX_UPDATE_FIXTURES").is_some() {
            std::fs::write("tests/fixtures/sidebar/dark.frames", &frames).unwrap();
        }
        let fixture = std::fs::read_to_string("tests/fixtures/sidebar/dark.frames").unwrap();
        assert!(
            fixture.starts_with(&false_frames),
            "false-mode fixture prefix changed"
        );
        assert_eq!(
            frames,
            std::fs::read_to_string("tests/fixtures/sidebar/dark.frames").unwrap()
        );
        sb.settings.settings.show_all_panes = false;
        sb.rebuild_visible(false);
        themed_frames(&mut sb);
        custom_key_hints(&mut sb);
        if let Some(output) = std::env::var_os("AGENMUX_THEME_VISUAL_DIR") {
            visual_frames(&mut sb, &PathBuf::from(output));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Optional, synthetic-only private tmux screen inspection. The production
    // binary's activation/snapshot path is separately exercised in plugin.rs.
    fn visual_frames(sb: &mut Sidebar, output: &std::path::Path) {
        use std::process::Command;
        std::fs::create_dir_all(output).unwrap();
        for base in ["light", "terminal"] {
            let file = crate::app_config::parse(&format!("[theme]\nbase='{base}'")).unwrap();
            sb.palette = Palette::resolve(file.theme.as_ref().unwrap());
            sb.rows = ["blocked", "working", "idle", "done"]
                .iter()
                .enumerate()
                .map(|(i, state)| {
                    let mut r = row(&format!("%{}", i + 1));
                    r.state = (*state).into();
                    r.title = "Synthetic task".into();
                    r
                })
                .collect();
            sb.visible = (0..4).map(VisiblePane::Agent).collect();
            sb.sel = 2;
            sb.active = "%2".into();
            sb.plugin_selected = true;
            sb.tick = 0;
            sb.query.clear();
            sb.state_filter = None;
            sb.search_focused = false;
            sb.update = Some("v9.9.9".into());
            sb.overlay = None;
            sb.daemon.as_mut().unwrap().size = (80, 40);
            sb.render(true);
            let result = Command::new("tmux")
                .args([
                    "new-window",
                    "-d",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-n",
                    "theme-preview",
                    "/bin/bash",
                    "-c",
                    "printf '%s' \"$1\"; exec sleep 30",
                    "_",
                    &sb.last_frame,
                ])
                .output()
                .unwrap();
            assert!(result.status.success());
            let pane = String::from_utf8(result.stdout).unwrap();
            let pane = pane.trim();
            let mut captured = String::new();
            for _ in 0..50 {
                let result = Command::new("tmux")
                    .args(["capture-pane", "-p", "-e", "-t", pane])
                    .output()
                    .unwrap();
                assert!(result.status.success());
                captured = String::from_utf8(result.stdout).unwrap();
                if captured.contains("Synthetic task") {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(captured.contains("Synthetic task"));
            if base == "light" {
                assert!(captured.contains("38;2;32;32;32"));
                assert!(captured.contains("48;2;255;240;204"));
            } else {
                assert!(!captured.contains("48;2;") && !captured.contains("48;5;"));
            }
            // Normalize the build timestamp before retaining scratch evidence.
            std::fs::write(
                output.join(format!("{base}.capture")),
                captured.replace(&app_title(), "agenmux TEST"),
            )
            .unwrap();
            assert!(Command::new("tmux")
                .args(["kill-pane", "-t", pane])
                .status()
                .unwrap()
                .success());
        }
    }

    /// Split mode installs the user's chords into the tmux tables, so its help
    /// and hints must name those. The daemon decodes its FIFO with the protocol
    /// defaults, which must not leak back into what the sidebar advertises.
    fn custom_key_hints(sb: &mut Sidebar) {
        assert!(sb.daemon.is_some(), "this covers the daemon render path");
        let file = crate::app_config::parse(
            "[keys.normal]\ndown = ['n']\nup = ['e']\njump = ['Tab']\nclose = []\n",
        )
        .unwrap();
        let settings = crate::app_config::resolve(&file, &Default::default()).unwrap();
        sb.normal_keys = settings.normal.clone();
        sb.search_keys = settings.search.clone();
        sb.rows = vec![row("%1")];
        sb.rows[0].state = "working".into();
        sb.visible = vec![VisiblePane::Agent(0)];
        sb.sel = 1;
        sb.query.clear();
        sb.search_focused = false;

        sb.overlay = Some(Overlay::Help);
        sb.render(true);
        let help = sb.last_frame.clone();
        assert!(help.contains("n/e"), "{help}");
        assert!(help.contains("Tab"), "{help}");
        assert!(!help.contains("j/k"), "{help}");
        // An unbound action leaves no row behind rather than a stale default.
        assert!(!help.contains("close sidebar"), "{help}");
        assert!(help.contains("jump to agent"), "{help}");
        sb.settings.settings.show_all_panes = true;
        sb.render(true);
        assert!(sb.last_frame.contains("jump to pane"), "{}", sb.last_frame);
        sb.settings.settings.show_all_panes = false;

        sb.overlay = None;
        sb.state_filter = Some(StateFilter::Working);
        sb.rebuild_visible(false);
        sb.render(true);
        let footer = sb.last_frame.clone();
        assert!(footer.contains("n/e"), "{footer}");
        assert!(!footer.contains("j/k"), "{footer}");
        sb.state_filter = None;
    }

    fn themed_frames(sb: &mut Sidebar) {
        let tags = format!("v9.9.9\n{}\n", current_tag());
        std::fs::write(sb.plugin_dir.join("target/release/.agenmux-tags"), &tags).unwrap();
        let ansi = regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap();
        let plain = |frame: &str| {
            ansi.replace_all(frame, "")
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let sources = [
            "[theme]\nbase = 'light'",
            "[theme]\nbase = 'terminal'",
            "[theme.colors]\nworking_fg = '#123456'\nworking_bg = 123\nworking_bg_unfocused = 'default'",
            "[theme.colors]\nworking_bg = 123",
            "[theme.colors]\nheader_fg = 17",
            "[theme.colors]\nheader_bg = 123",
            "[theme]\nbase = 'light'\n[theme.colors]\nheader_fg = 17\nheader_bg = 'default'\ntext_fg = '#123456'\nmuted_fg = 99\naccent_fg = 100\nerror_fg = 101\nblocked_fg = 102\nblocked_bg = 103\nblocked_bg_unfocused = 104\nworking_fg = 105\nworking_bg = 106\nworking_bg_unfocused = 107\nidle_fg = 108\nidle_bg = 109\nidle_bg_unfocused = 110\ndone_fg = 111\ndone_bg = 112\ndone_bg_unfocused = 113",
        ];
        for source in sources {
            let file = crate::app_config::parse(source).unwrap();
            let p = Palette::resolve(file.theme.as_ref().unwrap());
            for focused in [false, true] {
                for state in ["blocked", "working", "idle", "done"] {
                    for tick in [0, 2, 7] {
                        sb.rows = vec![row("%1")];
                        sb.rows[0].state = state.into();
                        sb.rows[0].title = "Synthetic task".into();
                        sb.visible = vec![VisiblePane::Agent(0)];
                        sb.sel = 1;
                        sb.active = "%1".into();
                        sb.plugin_selected = focused;
                        sb.tick = tick;
                        for mode in 0..10 {
                            sb.query.clear();
                            sb.state_filter = None;
                            sb.search_focused = false;
                            sb.overlay = None;
                            sb.update = None;
                            match mode {
                                1 => {
                                    sb.query = "repo".into();
                                    sb.search_focused = true;
                                }
                                2 => sb.query = "repo".into(),
                                3 => {
                                    sb.state_filter = Some(match state {
                                        "blocked" => StateFilter::Blocked,
                                        "working" => StateFilter::Working,
                                        "done" => StateFilter::Done,
                                        _ => StateFilter::Idle,
                                    })
                                }
                                4 => sb.update = Some("v9.9.9".into()),
                                5 => sb.overlay = Some(Overlay::Help),
                                6 => {
                                    sb.overlay = Some(Overlay::Versions {
                                        sel: 0,
                                        chosen: None,
                                    })
                                }
                                7 => sb.query = "absent".into(),
                                8 => sb.rows.clear(),
                                9 => {
                                    sb.overlay = Some(Overlay::Versions {
                                        sel: 0,
                                        chosen: None,
                                    });
                                    std::fs::remove_file(
                                        sb.plugin_dir.join("target/release/.agenmux-tags"),
                                    )
                                    .unwrap();
                                }
                                _ => {}
                            }
                            sb.rebuild_visible(false);
                            sb.palette = Palette::default();
                            sb.render(true);
                            let old_frame = sb.last_frame.clone();
                            let old_text = plain(&old_frame);
                            let old_map = std::fs::read_to_string(&sb.rows_file).unwrap();
                            sb.palette = p.clone();
                            sb.render(true);
                            assert_eq!(
                                plain(&sb.last_frame),
                                old_text,
                                "{source} {state} mode={mode}"
                            );
                            assert_eq!(std::fs::read_to_string(&sb.rows_file).unwrap(), old_map);
                            if mode == 0 {
                                if source == sources[2] && state != "working" {
                                    assert_eq!(
                                        sb.last_frame, old_frame,
                                        "working overrides leave other rows byte-identical"
                                    );
                                }
                                assert!(sb.last_frame.contains(&p.state_bg(state, focused)));
                                assert!(sb.last_frame.contains(
                                    &p.state_fg(state).fg(if focused { "1" } else { "" })
                                ));
                                assert!(sb.last_frame.contains(&p.accent_fg.fg("1")));
                                let restore = format!(
                                    "{E}[0m{}{}",
                                    p.text_fg.fg(""),
                                    p.state_bg(state, focused)
                                );
                                assert!(
                                    sb.last_frame.contains(&restore),
                                    "row restores foreground and fill"
                                );
                            }
                            if [1, 2, 3, 4, 5, 6, 7, 8, 9].contains(&mode) {
                                assert!(sb.last_frame.contains(&p.muted_fg.fg("2")));
                            }
                            if mode == 5 || mode == 6 {
                                assert!(sb.last_frame.contains(&p.header_fg.fg("1")));
                                if p.header_bg != Palette::default().header_bg {
                                    assert!(sb.last_frame.starts_with(&format!(
                                        "{}{E}[2J{E}[H{}",
                                        p.text_fg.fg(""),
                                        p.header_bg.bg()
                                    )));
                                }
                            }
                            if mode == 6 {
                                assert!(sb.last_frame.contains("(current)"));
                            }
                            if mode == 9 {
                                assert!(sb.last_frame.contains(&p.error_fg.fg("2")));
                                std::fs::write(
                                    sb.plugin_dir.join("target/release/.agenmux-tags"),
                                    &tags,
                                )
                                .unwrap();
                            }
                            if mode == 4 && (source == sources[2] || source == sources[3]) {
                                assert_eq!(
                                    sb.last_frame.lines().next(),
                                    old_frame.lines().next(),
                                    "working-only overrides leave update-header bytes unchanged: {source} focused={focused}"
                                );
                            }
                            if mode == 4
                                && (p.header_fg != Palette::default().header_fg
                                    || p.header_bg != Palette::default().header_bg)
                            {
                                let hdr = if focused {
                                    p.header_bg.bg()
                                } else {
                                    String::new()
                                };
                                assert!(sb.last_frame.lines().next().unwrap().contains(&format!(
                                    "{E}[0m{}{hdr}{}",
                                    p.text_fg.fg(""),
                                    p.header_fg.fg("")
                                )));
                            }
                            // Same engine/output bytes for tty popup and daemon at
                            // matching dimensions. Only the sink and row file differ.
                            {
                                let d = sb.daemon.take().unwrap();
                                let size = term_size();
                                sb.render(true);
                                let popup = sb.last_frame.clone();
                                sb.daemon = Some(d);
                                sb.daemon.as_mut().unwrap().size = size;
                                sb.render(true);
                                assert_eq!(sb.last_frame, popup);
                                sb.daemon.as_mut().unwrap().size = (80, 40);
                            }
                            for size in [(0, 0), (0, 4), (1, 1), (1, 3), (5, 6), (12, 4)] {
                                sb.daemon.as_mut().unwrap().size = size;
                                sb.render(true);
                                let text = plain(&sb.last_frame);
                                let final_column = format!("{E}[{}G", size.0);
                                assert!(sb
                                    .last_frame
                                    .lines()
                                    .zip(text.lines())
                                    .all(|(frame, text)| text.chars().count()
                                        <= size.0 + usize::from(frame.contains(&final_column))));
                                assert!(text.lines().count() <= size.1.saturating_sub(1));
                                assert!(
                                    std::fs::read_to_string(&sb.rows_file)
                                        .unwrap()
                                        .lines()
                                        .count()
                                        <= size.1.saturating_sub(2)
                                );
                            }
                            sb.daemon.as_mut().unwrap().size = (80, 40);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn app_title_identifies_dev_and_release_builds() {
        let expected = if cfg!(debug_assertions) {
            let timestamp = option_env!("AGENMUX_BUILD_TIMESTAMP").expect("debug timestamp");
            assert!(regex::Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$")
                .unwrap()
                .is_match(timestamp));
            format!("agenmux dev ({timestamp})")
        } else {
            format!("agenmux v{}", env!("CARGO_PKG_VERSION"))
        };
        assert_eq!(app_title(), expected);
    }

    #[test]
    fn cursor_uses_state_hue_and_focus_bold() {
        assert_eq!(
            cursor_mark(&Palette::default(), true, true, "idle"),
            format!("{E}[1;32m❯{E}[0m ")
        );
        assert_eq!(
            cursor_mark(&Palette::default(), true, false, "idle"),
            format!("{E}[32m❯{E}[0m ")
        );
        assert_eq!(
            cursor_mark(&Palette::default(), true, true, "working"),
            format!("{E}[1;33m❯{E}[0m ")
        );
        assert_eq!(
            cursor_mark(&Palette::default(), false, true, "blocked"),
            "  "
        );
    }

    #[test]
    fn bar_reasserts_the_background_after_every_reset() {
        let line = format!("{E}[1mcodex{E}[0m {E}[2mwork{E}[0m");
        let bg = Palette::default().header_bg.bg();
        let painted = bar(&line, &bg, 14, 10);
        assert!(!painted
            .split(&format!("{E}[0m"))
            .any(|part| { !part.is_empty() && !part.starts_with(&bg) }));
        assert!(painted.ends_with(&format!("    {E}[0m")));
        assert_eq!(bar(&line, "", 14, 10), line);
        assert_eq!(
            Palette::default().state_bg("blocked", true),
            "\x1b[48;2;42;16;16m"
        );
        assert_eq!(
            Palette::default().state_bg("blocked", false),
            "\x1b[48;2;32;12;12m"
        );
    }
}
