use crate::input::Key;
use crate::scan::{PaneMeta, PaneRow};
use std::collections::HashSet;

use super::{PaneOccurrence, Sidebar, VisiblePane};

fn needs_attention(row: &PaneRow) -> bool {
    matches!(row.state.as_str(), "done" | "working" | "blocked")
}

fn row_filter_text(row: &PaneRow) -> String {
    format!(
        "{} {} {} {} {}",
        row.agent, row.loc, row.cwd, row.title, row.state
    )
    .to_lowercase()
}

/// User attention matching is exact and separate from text search.
/// Matching a session keeps its whole agent subtree as context;
/// matching an agent keeps that session's header through normal rendering.
fn inventory_filtered_indices(
    panes: &[PaneMeta],
    rows: &[PaneRow],
    query: &str,
    attention_filter: bool,
) -> Vec<usize> {
    if attention_filter {
        return panes
            .iter()
            .enumerate()
            .filter(|(_, pane)| {
                pane.agent_index
                    .and_then(|i| rows.get(i))
                    .is_some_and(needs_attention)
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

fn filtered_indices(rows: &[PaneRow], query: &str, attention_filter: bool) -> Vec<usize> {
    if attention_filter {
        return rows
            .iter()
            .enumerate()
            .filter(|(_, row)| needs_attention(row))
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

impl VisiblePane {
    pub(super) fn is_pane(self) -> bool {
        matches!(self, VisiblePane::Agent(_) | VisiblePane::Inventory(_))
    }
}

/// Session and split-window header rows: the branches that collapse, and the
/// cursor targets `dd` deletes as a whole. `collapsed` is None while filtering,
/// so every match shows under its ancestors.
fn with_record_rows(
    panes: &[PaneMeta],
    indices: Vec<usize>,
    collapsed: Option<&HashSet<String>>,
) -> Vec<VisiblePane> {
    let hidden = |id: &str| collapsed.is_some_and(|set| set.contains(id));
    let mut out = Vec::with_capacity(indices.len() * 2);
    let (mut session, mut window) = ("", "");
    for i in indices {
        let pane = &panes[i];
        if pane.session_id != session {
            session = &pane.session_id;
            window = "";
            out.push(VisiblePane::Session(i));
        }
        if hidden(session) {
            continue;
        }
        if pane.window_id != window {
            window = &pane.window_id;
            let expanded = panes
                .iter()
                .filter(|p| p.window_id == pane.window_id)
                .count()
                > 1;
            if expanded {
                out.push(VisiblePane::Window(i));
            }
        }
        if !hidden(window) {
            out.push(VisiblePane::Inventory(i));
        }
    }
    out
}

impl Sidebar {
    pub(super) fn visible_pane_id(&self, pane: VisiblePane) -> &str {
        match pane {
            VisiblePane::Agent(i) => &self.rows[i].pane,
            VisiblePane::Inventory(i) | VisiblePane::Session(i) | VisiblePane::Window(i) => {
                &self.panes[i].pane
            }
        }
    }

    pub(super) fn visible_occurrence(&self, pane: VisiblePane) -> Option<PaneOccurrence> {
        let (i, window, pane_id) = match pane {
            VisiblePane::Agent(_) => return None,
            VisiblePane::Session(i) => (i, false, false),
            VisiblePane::Window(i) => (i, true, false),
            VisiblePane::Inventory(i) => (i, true, true),
        };
        let pane = &self.panes[i];
        Some(PaneOccurrence {
            session_id: pane.session_id.clone(),
            window_id: if window {
                pane.window_id.clone()
            } else {
                String::new()
            },
            pane: if pane_id {
                pane.pane.clone()
            } else {
                String::new()
            },
        })
    }

    pub(super) fn visible_agent_row(&self, pane: VisiblePane) -> Option<&PaneRow> {
        let i = match pane {
            VisiblePane::Agent(i) => Some(i),
            VisiblePane::Inventory(i) => self.panes[i].agent_index,
            VisiblePane::Session(_) | VisiblePane::Window(_) => None,
        }?;
        self.rows.get(i)
    }

    pub(super) fn visible_state(&self, pane: VisiblePane) -> &str {
        self.visible_agent_row(pane)
            .map_or("idle", |row| row.state.as_str())
    }

    pub(super) fn active_visible_index(&self) -> Option<usize> {
        let matches =
            |pane: VisiblePane| pane.is_pane() && self.visible_pane_id(pane) == self.active;
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
                .position(|&pane| pane.is_pane() && self.visible_pane_id(pane) == self.sel_pane)
        };
        // A pane hidden by a collapsed branch hands the cursor to its nearest
        // visible header: the split window's, else the session's.
        let ancestor = || {
            let selected = self.sel_occurrence.as_ref()?;
            if !self.panes.iter().any(|pane| pane.pane == self.sel_pane) {
                return None;
            }
            [selected.window_id.as_str(), ""].iter().find_map(|window| {
                self.visible.iter().position(|&pane| {
                    matches!(pane, VisiblePane::Session(_) | VisiblePane::Window(_))
                        && self.visible_occurrence(pane).is_some_and(|occurrence| {
                            occurrence.session_id == selected.session_id
                                && occurrence.window_id == *window
                        })
                })
            })
        };
        match exact.or_else(physical).or_else(ancestor) {
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
        let before = self.visible.len();
        self.visible = if self.settings.settings.show_all_panes {
            let indices = inventory_filtered_indices(
                &self.panes,
                &self.rows,
                &self.query,
                self.attention_filter,
            );
            let collapsed = self.collapsing().then_some(&self.collapsed);
            with_record_rows(&self.panes, indices, collapsed)
        } else {
            filtered_indices(&self.rows, &self.query, self.attention_filter)
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
        // A record appeared or vanished: the viewport clamp can otherwise
        // leave the cursor off screen, so follow it on the next render.
        if self.visible.len() != before {
            self.follow_selection = true;
        }
    }

    pub(super) fn focus_search(&mut self) {
        self.attention_filter = false;
        self.search_focused = true;
        self.rebuild_visible(false);
    }

    pub(super) fn toggle_attention_filter(&mut self) {
        self.query.clear();
        self.attention_filter = !self.attention_filter;
        self.search_focused = false;
        self.rebuild_visible(true);
    }

    pub(super) fn clear_filter(&mut self) {
        self.query.clear();
        self.attention_filter = false;
        self.search_focused = false;
        self.rebuild_visible(false);
    }

    /// Live view toggle between the agent list and the full tmux tree. Not
    /// written to config; a reload restores the configured default.
    pub(super) fn toggle_all_panes(&mut self) {
        // Flip the shown value directly and remember it as the override; the
        // next refresh re-applies it over the re-resolved config via
        // sync_panes_view, so a reload cannot revert it.
        let effective = !self.adopted_show_all_panes;
        self.panes_override = Some(effective);
        self.settings.settings.show_all_panes = effective;
        self.adopted_show_all_panes = effective;
        self.rebuild_visible(false);
        if let Some(index) = self.active_visible_index() {
            self.select_index(index + 1);
        }
        self.last_frame.clear();
    }

    /// Collapse state shapes only the unfiltered all-pane tree; the agent list
    /// has no branches.
    pub(super) fn collapsing(&self) -> bool {
        self.settings.settings.show_all_panes
            && !self.attention_filter
            && self.query.trim().is_empty()
    }

    pub(super) fn branch_collapsed(&self, id: &str) -> bool {
        self.collapsing() && self.collapsed.contains(id)
    }

    fn branch_id(&self, row: VisiblePane) -> Option<&str> {
        match row {
            VisiblePane::Session(i) => Some(&self.panes[i].session_id),
            VisiblePane::Window(i) => Some(&self.panes[i].window_id),
            VisiblePane::Agent(_) | VisiblePane::Inventory(_) => None,
        }
    }

    /// Nearest header above `index` that contains its row.
    fn parent_header(&self, index: usize) -> Option<usize> {
        let (i, below_window) = match self.visible[index] {
            VisiblePane::Inventory(i) => (i, true),
            VisiblePane::Window(i) => (i, false),
            VisiblePane::Session(_) | VisiblePane::Agent(_) => return None,
        };
        let pane = &self.panes[i];
        (0..index).rev().find(|&j| match self.visible[j] {
            VisiblePane::Window(q) => below_window && self.panes[q].window_id == pane.window_id,
            VisiblePane::Session(q) => self.panes[q].session_id == pane.session_id,
            _ => false,
        })
    }

    fn set_collapsed(&mut self, header: usize, collapse: bool) {
        let Some(id) = self.branch_id(self.visible[header]).map(str::to_string) else {
            return;
        };
        if collapse {
            self.collapsed.insert(id);
        } else {
            self.collapsed.remove(&id);
        }
        self.select_index(header + 1);
        self.rebuild_visible(false);
    }

    /// Toggle the selected header, or the header of the selected pane.
    pub(super) fn toggle_branch(&mut self) {
        let Some(index) = self.cursor_row().filter(|_| self.collapsing()) else {
            return;
        };
        let header = match self.branch_id(self.visible[index]) {
            Some(_) => Some(index),
            None => self.parent_header(index),
        };
        if let Some(header) = header {
            let collapsed = self
                .branch_id(self.visible[header])
                .is_some_and(|id| self.collapsed.contains(id));
            self.set_collapsed(header, !collapsed);
        }
    }

    /// Collapse an open header; from a pane or a closed header, step out to
    /// the parent header.
    pub(super) fn collapse_branch(&mut self) {
        let Some(index) = self.cursor_row().filter(|_| self.collapsing()) else {
            return;
        };
        match self.branch_id(self.visible[index]) {
            Some(id) if !self.collapsed.contains(id) => self.set_collapsed(index, true),
            _ => {
                if let Some(parent) = self.parent_header(index) {
                    self.select_index(parent + 1);
                }
            }
        }
    }

    pub(super) fn expand_branch(&mut self) {
        let Some(index) = self.cursor_row().filter(|_| self.collapsing()) else {
            return;
        };
        if self
            .branch_id(self.visible[index])
            .is_some_and(|id| self.collapsed.contains(id))
        {
            self.set_collapsed(index, false);
        }
    }

    /// `z` folds every session; `Z` opens every branch.
    pub(super) fn set_all_collapsed(&mut self, collapse: bool) {
        if !self.collapsing() {
            return;
        }
        if collapse {
            self.collapsed = self
                .panes
                .iter()
                .map(|pane| pane.session_id.clone())
                .collect();
        } else {
            self.collapsed.clear();
        }
        self.rebuild_visible(false);
    }

    pub(super) fn search_key(&mut self, key: Key) {
        if self.query.handle(&key) {
            if matches!(
                key,
                Key::Text(_) | Key::Backspace | Key::Delete | Key::ClearSearch
            ) {
                self.attention_filter = false;
                self.rebuild_visible(!matches!(key, Key::ClearSearch));
            }
            return;
        }
        match key {
            Key::Quit | Key::Close => self.clear_filter(),
            // First Enter accepts query and hands j/k back to filtered
            // navigation. Enter in normal mode then jumps to selection.
            Key::Jump => self.search_focused = false,
            Key::Down => self.move_sel(1),
            Key::Up => self.move_sel(-1),
            Key::WheelUp => self.scroll_viewport(-1),
            Key::WheelDown => self.scroll_viewport(1),
            Key::AllStates => self.clear_filter(),
            Key::TogglePanes => self.toggle_all_panes(),
            Key::First
            | Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::Delete
            | Key::Text(_)
            | Key::Backspace
            | Key::ClearSearch
            | Key::Last
            | Key::Select(_)
            | Key::Sequence(_, _)
            | Key::Owned(_, _)
            | Key::Search
            | Key::ToggleAttention
            | Key::Help
            | Key::Versions
            | Key::Settings
            | Key::ToggleBranch
            | Key::CollapseAll
            | Key::ExpandAll
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
            assert_eq!(filtered_indices(&rows, query, false), vec![0], "{query}");
        }
        assert!(filtered_indices(&rows, "payments", false).is_empty());
    }

    #[test]
    fn matching_session_keeps_its_agent_subtree() {
        let rows = [
            filter_row("%1", "api:1.0", "idle", "unrelated"),
            filter_row("%2", "api:2.0", "working", "also unrelated"),
            filter_row("%3", "web:1.0", "idle", "unrelated"),
        ];
        assert_eq!(filtered_indices(&rows, "api", false), vec![0, 1]);
    }

    #[allow(clippy::too_many_arguments)]
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
            inventory_filtered_indices(&panes, &rows, "team", false),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "$1", false),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn inventory_window_name_query_returns_full_matching_window_subtrees() {
        let (panes, rows) = inventory();
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "same", false),
            vec![0, 1, 3, 5]
        );
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "@3", false),
            vec![3]
        );
    }

    #[test]
    fn inventory_window_index_query_respects_session_occurrences() {
        let (panes, rows) = inventory();
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "42", false),
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
                inventory_filtered_indices(&panes, &rows, query, false),
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
                inventory_filtered_indices(&panes, &rows, query, false),
                expected,
                "{query}"
            );
        }
    }

    #[test]
    fn inventory_matching_is_case_insensitive() {
        let (panes, rows) = inventory();
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "fIx LoGiN", false),
            vec![1, 5]
        );
    }

    #[test]
    fn inventory_attention_filter_ignores_query_and_non_agent_metadata() {
        let (panes, rows) = inventory();
        assert!(panes[0].command.contains("working"));
        assert!(panes[2].pane_title.contains("working"));
        assert_eq!(
            inventory_filtered_indices(&panes, &rows, "ignored", true),
            vec![1, 3, 5]
        );
    }

    #[test]
    fn inventory_absent_query_returns_no_panes_for_rendering() {
        let (panes, rows) = inventory();
        assert!(inventory_filtered_indices(&panes, &rows, "absent", false).is_empty());
    }

    #[test]
    fn attention_filter_includes_done_working_and_blocked_but_not_idle() {
        let rows = [
            filter_row("%1", "s:1.0", "blocked", "working notes"),
            filter_row("%2", "s:2.0", "working", "blocked notes"),
            filter_row("%3", "s:3.0", "done", "done"),
            filter_row("%4", "s:4.0", "idle", "idle"),
        ];
        assert_eq!(filtered_indices(&rows, "ignored", true), vec![0, 1, 2]);
    }
}
