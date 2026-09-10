use crate::input::Key;
use crate::scan::{PaneMeta, PaneRow};
use std::collections::HashSet;

use super::{PaneOccurrence, Sidebar, VisiblePane};

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
fn inventory_filtered_indices(
    panes: &[PaneMeta],
    rows: &[PaneRow],
    query: &str,
    state_filter: Option<StateFilter>,
) -> Vec<usize> {
    if let Some(filter) = state_filter {
        return panes
            .iter()
            .enumerate()
            .filter(|(_, pane)| {
                pane.agent_index
                    .and_then(|i| rows.get(i))
                    .is_some_and(|row| row.state == filter.label())
            })
            .map(|(i, _)| i)
            .collect();
    }
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return (0..panes.len()).collect();
    }
    panes
        .iter()
        .enumerate()
        .filter(|(_, pane)| {
            pane.session_name.to_lowercase().contains(&query)
                || pane.session_id.to_lowercase().contains(&query)
                || pane.window_name.to_lowercase().contains(&query)
                || pane.window_id.to_lowercase().contains(&query)
                || pane.window_index.to_string().contains(&query)
                || pane.pane_index.to_string().contains(&query)
                || pane.pane.to_lowercase().contains(&query)
                || pane.pane_title.to_lowercase().contains(&query)
                || pane.command.to_lowercase().contains(&query)
                || pane.path.to_lowercase().contains(&query)
                || pane
                    .agent_index
                    .and_then(|i| rows.get(i))
                    .is_some_and(|row| row_filter_text(row).contains(&query))
        })
        .map(|(i, _)| i)
        .collect()
}

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

impl Sidebar {
    pub(super) fn visible_pane_id(&self, pane: VisiblePane) -> &str {
        match pane {
            VisiblePane::Agent(i) => &self.rows[i].pane,
            VisiblePane::Inventory(i) => &self.panes[i].pane,
        }
    }

    pub(super) fn visible_occurrence(&self, pane: VisiblePane) -> Option<PaneOccurrence> {
        let VisiblePane::Inventory(i) = pane else {
            return None;
        };
        let pane = &self.panes[i];
        Some(PaneOccurrence {
            session_id: pane.session_id.clone(),
            window_id: pane.window_id.clone(),
            pane: pane.pane.clone(),
        })
    }

    pub(super) fn visible_agent_row(&self, pane: VisiblePane) -> Option<&PaneRow> {
        let i = match pane {
            VisiblePane::Agent(i) => Some(i),
            VisiblePane::Inventory(i) => self.panes[i].agent_index,
        }?;
        self.rows.get(i)
    }

    pub(super) fn visible_state(&self, pane: VisiblePane) -> &str {
        self.visible_agent_row(pane)
            .map_or("idle", |row| row.state.as_str())
    }

    pub(super) fn active_visible_index(&self) -> Option<usize> {
        let matches = |pane: VisiblePane| self.visible_pane_id(pane) == self.active;
        self.visible
            .iter()
            .position(|&pane| {
                matches(pane)
                    && matches!(pane, VisiblePane::Inventory(i) if self.panes[i].session_id == self.active_session)
            })
            .or_else(|| self.visible.iter().position(|&pane| matches(pane)))
    }

    pub(super) fn cursor_row(&self) -> Option<usize> {
        if self.plugin_selected {
            return self.sel.checked_sub(1).filter(|&i| i < self.visible.len());
        }
        self.active_visible_index()
    }
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
        let selected = self.visible.get(self.sel.wrapping_sub(1)).copied();
        self.sel_pane = selected
            .map(|pane| self.visible_pane_id(pane).to_string())
            .unwrap_or_default();
        self.sel_occurrence = selected.and_then(|pane| self.visible_occurrence(pane));
    }

    fn restore_sel(&mut self) {
        // after a rescan/filter, follow the remembered pane when it remains
        // visible; otherwise keep the nearest valid result
        if self.sel_pane.is_empty() {
            self.sync_sel_pane();
            return;
        }
        let exact = self.sel_occurrence.as_ref().and_then(|selected| {
            self.visible
                .iter()
                .position(|&pane| self.visible_occurrence(pane).as_ref() == Some(selected))
        });
        let physical = || {
            self.visible
                .iter()
                .position(|&pane| self.visible_pane_id(pane) == self.sel_pane)
        };
        match exact.or_else(physical) {
            Some(i) => {
                self.sel = i + 1;
                self.sync_sel_pane();
            }
            None => {
                self.clamp_sel();
                self.sync_sel_pane();
            }
        }
    }

    pub(super) fn rebuild_visible(&mut self, select_first: bool) {
        self.visible = if self.settings.settings.show_all_panes {
            inventory_filtered_indices(&self.panes, &self.rows, &self.query, self.state_filter)
                .into_iter()
                .map(VisiblePane::Inventory)
                .collect()
        } else {
            filtered_indices(&self.rows, &self.query, self.state_filter)
                .into_iter()
                .map(VisiblePane::Agent)
                .collect()
        };
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
            self.sel_occurrence = None;
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
    use crate::scan::PaneMeta;

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

    fn inventory_pane(
        session_id: &str,
        session_name: &str,
        window_id: &str,
        window_index: u32,
        window_name: &str,
        pane: &str,
        pane_index: u32,
        pane_title: &str,
        command: &str,
        path: &str,
        agent_index: Option<usize>,
    ) -> PaneMeta {
        PaneMeta {
            pane: pane.into(),
            pane_index,
            pane_title: pane_title.into(),
            command: command.into(),
            path: path.into(),
            window_id: window_id.into(),
            window_index,
            window_name: window_name.into(),
            session_id: session_id.into(),
            session_name: session_name.into(),
            agent_index,
        }
    }

    fn inventory() -> (Vec<PaneMeta>, Vec<PaneRow>) {
        let rows = vec![
            PaneRow {
                pane: "%2".into(),
                loc: "alpha:1.1".into(),
                agent: "claude".into(),
                state: "blocked".into(),
                cwd: "/workspace/api".into(),
                title: "Fix login".into(),
            },
            PaneRow {
                pane: "%4".into(),
                loc: "alpha:2.1".into(),
                agent: "codex".into(),
                state: "working".into(),
                cwd: "/workspace/web".into(),
                title: "Build sidebar".into(),
            },
            PaneRow {
                pane: "%5".into(),
                loc: "other:3.1".into(),
                agent: "pi".into(),
                state: "idle".into(),
                cwd: "/workspace/docs".into(),
                title: "Review notes".into(),
            },
        ];
        let panes = vec![
            inventory_pane(
                "$1",
                "team",
                "@1",
                1,
                "same",
                "%1",
                1,
                "ordinary editor",
                "working",
                "/workspace/api",
                None,
            ),
            inventory_pane(
                "$1",
                "team",
                "@1",
                1,
                "same",
                "%2",
                2,
                "agent host",
                "claude",
                "/workspace/api",
                Some(0),
            ),
            inventory_pane(
                "$1",
                "team",
                "@2",
                2,
                "tools",
                "%3",
                1,
                "working notes",
                "zsh",
                "/workspace/tools",
                None,
            ),
            inventory_pane(
                "$2",
                "team",
                "@3",
                1,
                "same",
                "%4",
                1,
                "agent host",
                "codex",
                "/workspace/web",
                Some(1),
            ),
            inventory_pane(
                "$3",
                "other",
                "@4",
                3,
                "notes",
                "%5",
                1,
                "agent host",
                "pi",
                "/workspace/docs",
                Some(2),
            ),
            inventory_pane(
                "$3",
                "other",
                "@1",
                42,
                "same",
                "%2",
                7,
                "linked host",
                "claude",
                "/workspace/api",
                Some(0),
            ),
        ];
        (panes, rows)
    }

    #[test]
    fn inventory_session_name_or_id_query_returns_only_matching_occurrence_descendants() {
        let (panes, rows) = inventory();
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "team", None),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "$1", None),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn inventory_window_name_query_returns_full_matching_window_subtrees() {
        let (panes, rows) = inventory();
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "same", None),
            vec![0, 1, 3, 5]
        );
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "@3", None),
            vec![3]
        );
    }

    #[test]
    fn inventory_window_index_query_respects_session_occurrences() {
        let (panes, rows) = inventory();
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "42", None),
            vec![5]
        );
    }

    #[test]
    fn inventory_pane_fields_return_only_matching_panes() {
        let (panes, rows) = inventory();
        for (query, expected) in [
            ("7", vec![5]),
            ("%3", vec![2]),
            ("working notes", vec![2]),
            ("zsh", vec![2]),
            ("/workspace/tools", vec![2]),
        ] {
            assert_eq!(
                inventory_filtered_indices(&panes, &rows, query, None),
                expected,
                "{query}"
            );
        }
    }

    #[test]
    fn inventory_agent_fields_return_only_host_pane_occurrences() {
        let (panes, rows) = inventory();
        for (query, expected) in [
            ("claude", vec![1, 5]),
            ("blocked", vec![1, 5]),
            ("/workspace/web", vec![3]),
            ("Fix login", vec![1, 5]),
            ("alpha:2.1", vec![3]),
        ] {
            assert_eq!(
                inventory_filtered_indices(&panes, &rows, query, None),
                expected,
                "{query}"
            );
        }
    }

    #[test]
    fn inventory_matching_is_case_insensitive() {
        let (panes, rows) = inventory();
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "fIx LoGiN", None),
            vec![1, 5]
        );
    }

    #[test]
    fn inventory_status_filter_is_exact_and_ignores_query_and_non_agent_metadata() {
        let (panes, rows) = inventory();
        for (filter, expected) in [
            (StateFilter::Blocked, vec![1, 5]),
            (StateFilter::Working, vec![3]),
            (StateFilter::Idle, vec![4]),
        ] {
            assert_eq!(
                inventory_filtered_indices(&panes, &rows, "alpha", Some(filter)),
                expected
            );
        }
    }

    #[test]
    fn ordinary_working_metadata_never_matches_working_status() {
        let (panes, rows) = inventory();
        assert!(panes[0].command.contains("working"));
        assert!(panes[2].pane_title.contains("working"));
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "", Some(StateFilter::Working)),
            vec![3]
        );
    }

    #[test]
    fn inventory_absent_query_returns_no_panes_for_rendering() {
        let (panes, rows) = inventory();
        assert!(inventory_filtered_indices(&panes, &rows, "absent", None).is_empty());
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
