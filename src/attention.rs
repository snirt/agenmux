use crate::scan::PaneRow;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttentionKind {
    Finished,
    NeedsAttention,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttentionEvent {
    pub kind: AttentionKind,
    pub pane: String,
    pub loc: String,
    pub agent: String,
    pub cwd: String,
    pub title: String,
}

pub struct Update {
    pub rows: Vec<PaneRow>,
    pub events: Vec<AttentionEvent>,
}

#[derive(Default)]
pub struct Tracker {
    panes: HashMap<String, PaneMemory>,
}

struct PaneMemory {
    state: String,
    ticks: u32,
    title: String,
}

impl Tracker {
    pub fn update(&mut self, scanned: Vec<PaneRow>, focused: &HashSet<String>) -> Update {
        let mut next = HashMap::new();
        let mut rows = Vec::with_capacity(scanned.len());
        let mut events = Vec::new();

        for mut row in scanned {
            let previous = self.panes.get(&row.pane);
            let previous_state = previous.map(|pane| pane.state.as_str()).unwrap_or("");
            let ticks = previous.map(|pane| pane.ticks).unwrap_or(0);
            if row.title.is_empty() {
                if let Some(previous) = previous {
                    row.title = previous.title.clone();
                }
            }
            if row.state == "blocked"
                && !previous_state.is_empty()
                && previous_state != "blocked"
                && !focused.contains(&row.pane)
            {
                events.push(event(AttentionKind::NeedsAttention, &row));
            }

            let (shown_state, stored_state, next_ticks) = if row.state == "idle"
                && !previous_state.is_empty()
                && previous_state != "idle"
                && previous_state != "done"
                && ticks < 1
            {
                (
                    previous_state.to_string(),
                    previous_state.to_string(),
                    ticks + 1,
                )
            } else if row.state == "idle"
                && !focused.contains(&row.pane)
                && (previous_state == "working" || previous_state == "done")
            {
                if previous_state == "working" {
                    events.push(event(AttentionKind::Finished, &row));
                }
                ("done".into(), "done".into(), 0)
            } else {
                (row.state.clone(), row.state.clone(), 0)
            };

            next.insert(
                row.pane.clone(),
                PaneMemory {
                    state: stored_state,
                    ticks: next_ticks,
                    title: row.title.clone(),
                },
            );
            row.state = shown_state;
            rows.push(row);
        }

        self.panes = next;
        Update { rows, events }
    }
}

fn event(kind: AttentionKind, row: &PaneRow) -> AttentionEvent {
    AttentionEvent {
        kind,
        pane: row.pane.clone(),
        loc: row.loc.clone(),
        agent: row.agent.clone(),
        cwd: row.cwd.clone(),
        title: row.title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: &str) -> PaneRow {
        PaneRow {
            pane: "%1".into(),
            loc: "work:2.1".into(),
            agent: "codex".into(),
            state: state.into(),
            cwd: "tmux-agents-mon".into(),
            title: "Implement notifications".into(),
        }
    }

    #[test]
    fn first_scan_establishes_a_silent_baseline() {
        let mut tracker = Tracker::default();
        let update = tracker.update(vec![row("blocked")], &HashSet::new());

        assert!(update.events.is_empty());
        assert_eq!(update.rows[0].state, "blocked");
    }

    #[test]
    fn hidden_working_agent_finishes_once_after_idle_is_stable() {
        let mut tracker = Tracker::default();
        tracker.update(vec![row("working")], &HashSet::new());

        let debounce = tracker.update(vec![row("idle")], &HashSet::new());
        assert!(debounce.events.is_empty());
        assert_eq!(debounce.rows[0].state, "working");

        let finished = tracker.update(vec![row("idle")], &HashSet::new());
        assert_eq!(finished.rows[0].state, "done");
        assert_eq!(finished.events.len(), 1);
        assert_eq!(finished.events[0].kind, AttentionKind::Finished);
        assert_eq!(finished.events[0].pane, "%1");
        assert_eq!(finished.events[0].title, "Implement notifications");

        let unchanged = tracker.update(vec![row("idle")], &HashSet::new());
        assert!(unchanged.events.is_empty());
        assert_eq!(unchanged.rows[0].state, "done");
    }

    #[test]
    fn hidden_agent_needing_attention_notifies_once() {
        let mut tracker = Tracker::default();
        tracker.update(vec![row("working")], &HashSet::new());

        let blocked = tracker.update(vec![row("blocked")], &HashSet::new());
        assert_eq!(blocked.events.len(), 1);
        assert_eq!(blocked.events[0].kind, AttentionKind::NeedsAttention);

        let unchanged = tracker.update(vec![row("blocked")], &HashSet::new());
        assert!(unchanged.events.is_empty());
    }

    #[test]
    fn focused_transitions_are_suppressed_without_a_delayed_notification() {
        let focused = HashSet::from(["%1".to_string()]);

        let mut finished = Tracker::default();
        finished.update(vec![row("working")], &HashSet::new());
        finished.update(vec![row("idle")], &focused);
        assert!(finished
            .update(vec![row("idle")], &focused)
            .events
            .is_empty());
        assert!(finished
            .update(vec![row("idle")], &HashSet::new())
            .events
            .is_empty());

        let mut blocked = Tracker::default();
        blocked.update(vec![row("working")], &HashSet::new());
        assert!(blocked
            .update(vec![row("blocked")], &focused)
            .events
            .is_empty());
        assert!(blocked
            .update(vec![row("blocked")], &HashSet::new())
            .events
            .is_empty());
    }
}
