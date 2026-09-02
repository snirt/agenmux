use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct ClientFocus {
    pub active_pane: String,
    pub active_session: String,
    pub plugin_selected: bool,
    pub focused_panes: HashSet<String>,
    focused_counts: HashMap<String, usize>,
    focused_by_client: HashMap<String, String>,
}

impl ClientFocus {
    /// A tmux popup owns its client's input while tmux continues to report the
    /// pane underneath it. Remove only that client's focus contribution; a
    /// second real client viewing the same pane must still suppress alerts.
    pub fn discount_client(&mut self, client: &str) {
        let Some(pane) = self.focused_by_client.remove(client) else {
            return;
        };
        match self.focused_counts.get(&pane).copied() {
            Some(count) if count > 1 => {
                *self.focused_counts.get_mut(&pane).unwrap() -= 1;
            }
            Some(_) => {
                self.focused_counts.remove(&pane);
                self.focused_panes.remove(&pane);
            }
            None => {}
        }
    }
}

/// Parse rows formatted as activity, client, session, pane, title, flags.
pub fn parse_clients(rows: &str, focus_events: bool) -> ClientFocus {
    let mut parsed = Vec::new();
    for line in rows.lines() {
        let mut fields = line.splitn(6, '\t');
        let (Some(activity), Some(client), Some(session), Some(pane), Some(title), Some(flags)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let Ok(activity) = activity.parse::<u64>() else {
            continue;
        };
        let has_flag = |wanted: &str| flags.split(',').any(|flag| flag == wanted);
        if has_flag("control-mode") {
            continue;
        }
        parsed.push((
            activity,
            client.to_string(),
            session.to_string(),
            pane.to_string(),
            title.to_string(),
            has_flag("focused"),
        ));
    }

    let focused_counts: HashMap<String, usize> = parsed
        .iter()
        .filter(|(_, _, _, _, _, focused)| !focus_events || *focused)
        .fold(HashMap::new(), |mut counts, (_, _, _, pane, _, _)| {
            *counts.entry(pane.clone()).or_default() += 1;
            counts
        });
    let focused_by_client = parsed
        .iter()
        .filter(|(_, _, _, _, _, focused)| !focus_events || *focused)
        .map(|(_, client, _, pane, _, _)| (client.clone(), pane.clone()))
        .collect();
    let focused_panes = focused_counts.keys().cloned().collect();
    let Some((_, _, active_session, active_pane, title, _)) = parsed
        .iter()
        .max_by_key(|(activity, _, _, _, _, _)| *activity)
        .cloned()
    else {
        return ClientFocus {
            focused_panes,
            focused_counts,
            focused_by_client,
            ..ClientFocus::default()
        };
    };
    ClientFocus {
        active_pane,
        active_session,
        plugin_selected: title == "agenmux",
        focused_panes,
        focused_counts,
        focused_by_client,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_events_require_the_focused_flag_and_ignore_control_clients() {
        let rows = concat!(
            "20\tclient-1\t$1\t%2\twork\tattached,focused,utf8\n",
            "30\tclient-2\t$2\t%3\twork\tattached,utf8\n",
            "40\tcontrol\t$3\t%4\tagenmux\tattached,focused,control-mode,utf8\n",
        );
        let focus = parse_clients(rows, true);

        assert_eq!(focus.active_pane, "%3");
        assert_eq!(focus.active_session, "$2");
        assert!(!focus.plugin_selected);
        assert_eq!(focus.focused_panes, HashSet::from(["%2".to_string()]));
    }

    #[test]
    fn focus_events_off_treat_every_selected_real_client_pane_as_focused() {
        let rows = concat!(
            "20\tclient-1\t$1\t%2\twork\tattached,utf8\n",
            "30\tclient-2\t$2\t%3\tagenmux\tattached,utf8\n",
        );
        let focus = parse_clients(rows, false);

        assert_eq!(focus.active_pane, "%3");
        assert!(focus.plugin_selected);
        assert_eq!(
            focus.focused_panes,
            HashSet::from(["%2".to_string(), "%3".to_string()])
        );
    }

    #[test]
    fn popup_discounts_its_explicit_owner_and_preserves_other_viewers() {
        let rows = concat!(
            "20\tpopup\t$1\t%2\twork\tattached,focused,utf8\n",
            "30\tother\t$2\t%3\twork\tattached,focused,utf8\n",
        );
        let mut focus = parse_clients(rows, true);
        focus.discount_client("popup");
        assert_eq!(focus.focused_panes, HashSet::from(["%3".to_string()]));

        let same_pane = concat!(
            "20\tpopup\t$1\t%2\twork\tattached,focused,utf8\n",
            "30\tother\t$2\t%2\twork\tattached,focused,utf8\n",
        );
        let mut focus = parse_clients(same_pane, true);
        focus.discount_client("popup");
        assert_eq!(focus.focused_panes, HashSet::from(["%2".to_string()]));
    }
}
