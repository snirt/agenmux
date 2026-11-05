use std::collections::{HashMap, HashSet};

/// A client of the server, as the backend reports it.
pub struct Client {
    /// Orders clients by last use.
    pub activity: u64,
    pub name: String,
    pub session: String,
    pub pane: String,
    /// It shows a sidebar pane.
    pub on_sidebar: bool,
    /// It has input focus, or the server cannot tell.
    pub focused: bool,
}

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
    /// The most recently used client acts; every focused one looks.
    pub fn from_clients(clients: &[Client]) -> ClientFocus {
        let focused = clients.iter().filter(|client| client.focused);
        let focused_counts: HashMap<String, usize> =
            focused.clone().fold(HashMap::new(), |mut counts, client| {
                *counts.entry(client.pane.clone()).or_default() += 1;
                counts
            });
        let focused_by_client = focused
            .map(|client| (client.name.clone(), client.pane.clone()))
            .collect();
        let focused_panes = focused_counts.keys().cloned().collect();
        let active = clients.iter().max_by_key(|client| client.activity);
        ClientFocus {
            active_pane: active.map(|c| c.pane.clone()).unwrap_or_default(),
            active_session: active.map(|c| c.session.clone()).unwrap_or_default(),
            plugin_selected: active.is_some_and(|c| c.on_sidebar),
            focused_panes,
            focused_counts,
            focused_by_client,
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn client(activity: u64, name: &str, pane: &str, focused: bool) -> Client {
        Client {
            activity,
            name: name.into(),
            session: format!("s-{name}"),
            pane: pane.into(),
            on_sidebar: false,
            focused,
        }
    }

    #[test]
    fn the_latest_client_acts_and_every_focused_one_looks() {
        let mut sidebar = client(30, "b", "p3", false);
        sidebar.on_sidebar = true;
        let focus = ClientFocus::from_clients(&[client(20, "a", "p2", true), sidebar]);
        assert_eq!(
            (focus.active_pane.as_str(), focus.active_session.as_str()),
            ("p3", "s-b")
        );
        assert!(focus.plugin_selected);
        assert_eq!(focus.focused_panes, HashSet::from(["p2".to_string()]));
        let none = ClientFocus::from_clients(&[]);
        assert!(none.active_pane.is_empty() && none.focused_panes.is_empty());
    }

    #[test]
    fn popup_discounts_its_explicit_owner_and_preserves_other_viewers() {
        let mut focus = ClientFocus::from_clients(&[
            client(20, "popup", "p2", true),
            client(30, "other", "p3", true),
        ]);
        focus.discount_client("popup");
        assert_eq!(focus.focused_panes, HashSet::from(["p3".to_string()]));

        let mut focus = ClientFocus::from_clients(&[
            client(20, "popup", "p2", true),
            client(30, "other", "p2", true),
        ]);
        focus.discount_client("popup");
        assert_eq!(focus.focused_panes, HashSet::from(["p2".to_string()]));
    }
}
