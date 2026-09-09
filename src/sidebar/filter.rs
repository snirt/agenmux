use crate::input::Key;
use crate::scan::PaneRow;
use std::collections::HashSet;

use super::Sidebar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StateFilter {
    Blocked,
    Working,
    Idle,
    Done,
}

impl StateFilter {
    pub(super) fn label(self) -> &'static str {
        match self {
            StateFilter::Blocked => "blocked",
            StateFilter::Working => "working",
            StateFilter::Idle => "idle",
            StateFilter::Done => "done",
        }
    }

    fn cycle(current: Option<Self>) -> Option<Self> {
        match current {
            None => Some(Self::Blocked),
            Some(Self::Blocked) => Some(Self::Working),
            Some(Self::Working) => Some(Self::Idle),
            Some(Self::Idle) => Some(Self::Done),
            Some(Self::Done) => None,
        }
    }
}

fn row_filter_text(row: &PaneRow) -> String {
    format!(
        "{} {} {} {} {}",
        row.agent, row.loc, row.cwd, row.title, row.state
    )
    .to_lowercase()
}

/// Filter projection: status matching is exact and separate from text search.
/// Matching a session keeps its whole agent subtree as context;
/// matching an agent keeps that session's header through normal rendering.
fn filtered_indices(
    rows: &[PaneRow],
    query: &str,
    state_filter: Option<StateFilter>,
) -> Vec<usize> {
    if let Some(filter) = state_filter {
        return rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.state == filter.label())
            .map(|(i, _)| i)
            .collect();
    }
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return (0..rows.len()).collect();
    }
    let matching_sessions: HashSet<&str> = rows
        .iter()
        .filter_map(|row| {
            let session = row.loc.split(':').next().unwrap_or("");
            session.to_lowercase().contains(&query).then_some(session)
        })
        .collect();
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            let session = row.loc.split(':').next().unwrap_or("");
            matching_sessions.contains(session) || row_filter_text(row).contains(&query)
        })
        .map(|(i, _)| i)
        .collect()
}

pub(super) fn cursor_row(
    rows: &[PaneRow],
    visible: &[usize],
    selected: usize,
    plugin_selected: bool,
    active: &str,
) -> Option<usize> {
    if plugin_selected {
        return selected.checked_sub(1).filter(|&i| i < visible.len());
    }
    visible.iter().position(|&i| rows[i].pane == active)
}

impl Sidebar {
    pub(super) fn select_index(&mut self, index: usize) {
        self.sel = index.max(1);
        self.follow_selection = true;
        self.clamp_sel();
        self.sync_sel_pane();
    }

    pub(super) fn move_sel(&mut self, d: i64) {
        self.select_index((self.sel as i64 + d).max(1) as usize);
    }

    pub(super) fn scroll_viewport(&mut self, d: i64) {
        self.scroll = (self.scroll as i64 + d).max(0) as usize;
        self.follow_selection = false;
    }

    fn clamp_sel(&mut self) {
        if self.sel > self.visible.len() {
            self.sel = self.visible.len();
        }
        if self.sel < 1 {
            self.sel = 1;
        }
    }

    fn sync_sel_pane(&mut self) {
        self.sel_pane = self
            .visible
            .get(self.sel.wrapping_sub(1))
            .and_then(|&i| self.rows.get(i))
            .map(|r| r.pane.clone())
            .unwrap_or_default();
    }

    fn restore_sel(&mut self) {
        // after a rescan/filter, follow the remembered pane when it remains
        // visible; otherwise keep the nearest valid result
        if self.sel_pane.is_empty() {
            self.sync_sel_pane();
            return;
        }
        match self
            .visible
            .iter()
            .position(|&i| self.rows[i].pane == self.sel_pane)
        {
            Some(i) => self.sel = i + 1,
            None => {
                self.clamp_sel();
                self.sync_sel_pane();
            }
        }
    }

    pub(super) fn rebuild_visible(&mut self, select_first: bool) {
        self.visible = filtered_indices(&self.rows, &self.query, self.state_filter);
        if select_first {
            self.sel = 1;
            self.follow_selection = true;
            self.sync_sel_pane();
        } else {
            self.clamp_sel();
            self.restore_sel();
        }
        if self.visible.is_empty() {
            self.sel_pane.clear();
        }
    }

    pub(super) fn focus_search(&mut self) {
        self.state_filter = None;
        self.search_focused = true;
        self.rebuild_visible(false);
    }

    pub(super) fn cycle_state_filter(&mut self) {
        self.query.clear();
        self.state_filter = StateFilter::cycle(self.state_filter);
        self.search_focused = false;
        self.rebuild_visible(true);
    }

    pub(super) fn clear_filter(&mut self) {
        self.query.clear();
        self.state_filter = None;
        self.search_focused = false;
        self.rebuild_visible(false);
    }

    pub(super) fn search_key(&mut self, key: Key) {
        match key {
            Key::Quit | Key::Close => self.clear_filter(),
            // First Enter accepts query and hands j/k back to filtered
            // navigation. Enter in normal mode then jumps to selection.
            Key::Jump => self.search_focused = false,
            Key::Down => self.move_sel(1),
            Key::Up => self.move_sel(-1),
            Key::Backspace => {
                self.state_filter = None;
                self.query.pop();
                self.rebuild_visible(true);
            }
            Key::ClearSearch => {
                self.query.clear();
                self.state_filter = None;
                self.rebuild_visible(false);
            }
            Key::Text(text) => {
                self.state_filter = None;
                let room = 256usize.saturating_sub(self.query.chars().count());
                self.query
                    .extend(text.chars().filter(|c| !c.is_control()).take(room));
                self.rebuild_visible(true);
            }
            Key::WheelUp => self.scroll_viewport(-1),
            Key::WheelDown => self.scroll_viewport(1),
            Key::AllStates => self.clear_filter(),
            Key::First
            | Key::Last
            | Key::Select(_)
            | Key::Sequence(_)
            | Key::Search
            | Key::CycleState
            | Key::Help
            | Key::Versions
            | Key::Other => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn cursor_follows_focus_outside_navigation() {
        let rows = [row("%1"), row("%2")];
        let visible = [0, 1];
        assert_eq!(cursor_row(&rows, &visible, 1, false, "%2"), Some(1));
        assert_eq!(cursor_row(&rows, &visible, 2, false, "%9"), None);
        assert_eq!(cursor_row(&rows, &visible, 2, true, "%1"), Some(1));
        assert_eq!(cursor_row(&rows, &[1], 1, false, "%2"), Some(0));
    }

    fn filter_row(pane: &str, loc: &str, state: &str, title: &str) -> PaneRow {
        PaneRow {
            pane: pane.into(),
            loc: loc.into(),
            agent: "codex".into(),
            state: state.into(),
            cwd: "auth-service".into(),
            title: title.into(),
        }
    }

    #[test]
    fn text_search_matches_visible_fields_case_insensitively() {
        let rows = [filter_row("%1", "work:1.0", "idle", "Fix Login Race")];
        for query in ["CODEX", "work:1", "AUTH", "login", "idle"] {
            assert_eq!(filtered_indices(&rows, query, None), vec![0], "{query}");
        }
        assert!(filtered_indices(&rows, "payments", None).is_empty());
    }

    #[test]
    fn matching_session_keeps_its_agent_subtree() {
        let rows = [
            filter_row("%1", "api:1.0", "idle", "unrelated"),
            filter_row("%2", "api:2.0", "working", "also unrelated"),
            filter_row("%3", "web:1.0", "idle", "unrelated"),
        ];
        assert_eq!(filtered_indices(&rows, "api", None), vec![0, 1]);
    }

    #[test]
    fn state_filter_cycles_in_display_order() {
        let mut filter = None;
        for expected in [
            Some(StateFilter::Blocked),
            Some(StateFilter::Working),
            Some(StateFilter::Idle),
            Some(StateFilter::Done),
            None,
        ] {
            filter = StateFilter::cycle(filter);
            assert_eq!(filter, expected);
        }
    }

    #[test]
    fn state_filters_are_exact_and_separate_from_text() {
        let rows = [
            filter_row("%1", "s:1.0", "blocked", "working notes"),
            filter_row("%2", "s:2.0", "working", "blocked notes"),
            filter_row("%3", "s:3.0", "done", "done"),
        ];
        assert_eq!(
            filtered_indices(&rows, "ignored", Some(StateFilter::Blocked)),
            vec![0]
        );
        assert_eq!(
            filtered_indices(&rows, "", Some(StateFilter::Working)),
            vec![1]
        );
        assert_eq!(
            filtered_indices(&rows, "", Some(StateFilter::Done)),
            vec![2]
        );
    }
}
