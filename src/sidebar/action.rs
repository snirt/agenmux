// Intentions the list view acts on. Popup keys, daemon FIFO packets, clicks
// and coalesced wheel bursts all decode to a logical `Key`; each input mode
// resolves that key to one `Action` here, and `Sidebar::apply` is the single
// place an action becomes state changes and tmux effects. Nothing here draws:
// the event loop renders after applying, from state alone.
use super::{DispatchResult, Sidebar};
use crate::input::Key;

/// What the user asked the list view to do. Mode independent: normal and
/// search mode map different keys onto the same intentions, and the same
/// action drives both the popup and the preserved-pane daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Action {
    SelectFirst,
    SelectLast,
    /// 1-based row from a click; 0 selects the first row.
    SelectIndex(usize),
    MoveSelection(i64),
    ScrollViewport(i64),
    Jump,
    OpenHelp,
    OpenVersions,
    OpenSettings,
    FocusSearch,
    /// Enter in search mode: keep the query, hand j/k back to the list.
    AcceptSearch,
    SearchInput(String),
    SearchBackspace,
    SearchClear,
    ToggleAttentionFilter,
    ClearFilter,
    ToggleAllPanes,
    Quit,
    Close,
}

/// Normal list mode. Text and query-editing keys mean nothing here; owned
/// and sequence packets are unwrapped by the dispatcher before this runs.
pub(super) fn normal_action(key: Key) -> Option<Action> {
    Some(match key {
        Key::First => Action::SelectFirst,
        Key::Last => Action::SelectLast,
        Key::Select(index) => Action::SelectIndex(index),
        Key::Down => Action::MoveSelection(1),
        Key::Up => Action::MoveSelection(-1),
        Key::WheelUp => Action::ScrollViewport(-1),
        Key::WheelDown => Action::ScrollViewport(1),
        Key::Jump => Action::Jump,
        Key::Help => Action::OpenHelp,
        Key::Versions => Action::OpenVersions,
        Key::Settings => Action::OpenSettings,
        Key::Search => Action::FocusSearch,
        Key::ToggleAttention => Action::ToggleAttentionFilter,
        Key::AllStates => Action::ClearFilter,
        Key::TogglePanes => Action::ToggleAllPanes,
        Key::Quit => Action::Quit,
        Key::Close => Action::Close,
        Key::Owned(_, _)
        | Key::Sequence(_, _)
        | Key::Backspace
        | Key::ClearSearch
        | Key::Text(_)
        | Key::Other => return None,
    })
}

/// Live search. Printable input edits the query, Enter accepts it, and the
/// close/cancel keys clear it instead of leaving the view.
pub(super) fn search_action(key: Key) -> Option<Action> {
    Some(match key {
        Key::Quit | Key::Close | Key::AllStates => Action::ClearFilter,
        Key::Jump => Action::AcceptSearch,
        Key::Down => Action::MoveSelection(1),
        Key::Up => Action::MoveSelection(-1),
        Key::WheelUp => Action::ScrollViewport(-1),
        Key::WheelDown => Action::ScrollViewport(1),
        Key::Backspace => Action::SearchBackspace,
        Key::ClearSearch => Action::SearchClear,
        Key::Text(text) => Action::SearchInput(text),
        Key::TogglePanes => Action::ToggleAllPanes,
        Key::First
        | Key::Last
        | Key::Select(_)
        | Key::Sequence(_, _)
        | Key::Owned(_, _)
        | Key::Search
        | Key::ToggleAttention
        | Key::Help
        | Key::Versions
        | Key::Settings
        | Key::Other => return None,
    })
}

impl Sidebar {
    /// Apply one intention. Pure list state changes stay in memory; jump,
    /// quit and close are the only actions with tmux or filesystem effects,
    /// and they report those through the dispatch result.
    pub(super) fn apply(&mut self, action: Action) -> DispatchResult {
        match action {
            Action::SelectFirst => self.select_index(1),
            Action::SelectLast => self.select_index(self.visible.len()),
            Action::SelectIndex(index) => self.select_index(index),
            Action::MoveSelection(delta) => self.move_sel(delta),
            Action::ScrollViewport(delta) => self.scroll_viewport(delta),
            Action::Jump => {
                if self.jump() {
                    return DispatchResult::Break;
                }
            }
            Action::OpenHelp => self.help(),
            Action::OpenVersions => self.versions(),
            Action::OpenSettings => self.settings(),
            Action::FocusSearch => self.focus_search(),
            Action::AcceptSearch => self.search_focused = false,
            Action::SearchInput(text) => self.push_query(&text),
            Action::SearchBackspace => self.pop_query(),
            Action::SearchClear => self.clear_query(),
            Action::ToggleAttentionFilter => self.toggle_attention_filter(),
            Action::ClearFilter => self.clear_filter(),
            Action::ToggleAllPanes => self.toggle_all_panes(),
            Action::Quit => {
                if self.daemon.is_none() {
                    // Popup/tty mode owns stdin, so q/Ctrl-C/Ctrl-D closes it.
                    if let Some(p) = &self.pin {
                        let _ = std::fs::remove_file(p);
                    }
                    return DispatchResult::Break;
                }
                // In preserved-pane mode close arrives as Key::Close from the
                // key table; Quit also covers FIFO EOF, which must not kill it.
            }
            Action::Close => {
                if self.daemon.is_some() {
                    // Finish teardown before a fast reopen can observe the
                    // dying control client and attach panes to it.
                    self.teardown();
                    self.daemon = None;
                    return DispatchResult::QuietExit;
                }
                return DispatchResult::Break;
            }
        }
        DispatchResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::super::{new_sidebar, VisiblePane};
    use super::*;
    use crate::scan::PaneRow;
    use crate::tmux::Tmux;
    use std::process::Command;

    #[test]
    fn normal_keys_name_one_intention_each() {
        let cases = [
            (Key::First, Some(Action::SelectFirst)),
            (Key::Last, Some(Action::SelectLast)),
            (Key::Select(3), Some(Action::SelectIndex(3))),
            (Key::Down, Some(Action::MoveSelection(1))),
            (Key::Up, Some(Action::MoveSelection(-1))),
            (Key::WheelDown, Some(Action::ScrollViewport(1))),
            (Key::WheelUp, Some(Action::ScrollViewport(-1))),
            (Key::Jump, Some(Action::Jump)),
            (Key::Help, Some(Action::OpenHelp)),
            (Key::Versions, Some(Action::OpenVersions)),
            (Key::Settings, Some(Action::OpenSettings)),
            (Key::Search, Some(Action::FocusSearch)),
            (Key::ToggleAttention, Some(Action::ToggleAttentionFilter)),
            (Key::AllStates, Some(Action::ClearFilter)),
            (Key::TogglePanes, Some(Action::ToggleAllPanes)),
            (Key::Quit, Some(Action::Quit)),
            (Key::Close, Some(Action::Close)),
            (Key::Text("x".into()), None),
            (Key::Backspace, None),
            (Key::ClearSearch, None),
            (Key::Sequence('d', None), None),
            (Key::Other, None),
        ];
        for (key, expected) in cases {
            let label = format!("{key:?}");
            assert_eq!(normal_action(key), expected, "{label}");
        }
    }

    #[test]
    fn search_keys_edit_the_query_and_share_list_navigation() {
        let cases = [
            (Key::Text("é".into()), Some(Action::SearchInput("é".into()))),
            (Key::Backspace, Some(Action::SearchBackspace)),
            (Key::ClearSearch, Some(Action::SearchClear)),
            (Key::Jump, Some(Action::AcceptSearch)),
            (Key::Quit, Some(Action::ClearFilter)),
            (Key::Close, Some(Action::ClearFilter)),
            (Key::AllStates, Some(Action::ClearFilter)),
            (Key::Down, Some(Action::MoveSelection(1))),
            (Key::Up, Some(Action::MoveSelection(-1))),
            (Key::WheelUp, Some(Action::ScrollViewport(-1))),
            (Key::TogglePanes, Some(Action::ToggleAllPanes)),
            // Opening overlays or re-entering search is not a search edit.
            (Key::Search, None),
            (Key::Help, None),
            (Key::Settings, None),
            (Key::ToggleAttention, None),
            (Key::Select(1), None),
        ];
        for (key, expected) in cases {
            let label = format!("{key:?}");
            assert_eq!(search_action(key), expected, "{label}");
        }
    }

    fn row(pane: &str, cwd: &str, state: &str, title: &str) -> PaneRow {
        PaneRow {
            pane: pane.into(),
            loc: "s:1.1".into(),
            agent: "codex".into(),
            state: state.into(),
            cwd: cwd.into(),
            title: title.into(),
        }
    }

    // Drive the real state through actions alone: no terminal, no key bytes,
    // no render. The control connection is isolated from every user server;
    // the child process owns TMUX, avoiding parallel-test env races.
    #[test]
    fn actions_drive_selection_and_search_state() {
        if std::env::var_os("AGENMUX_ACTION_TEST_CHILD").is_none() {
            let socket =
                std::env::temp_dir().join(format!("agenmux-action-{}.sock", std::process::id()));
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
                    "action",
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
                    "sidebar::action::tests::actions_drive_selection_and_search_state",
                    "--nocapture",
                ])
                .env("AGENMUX_ACTION_TEST_CHILD", "1")
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
        let dir = std::env::temp_dir().join(format!("agenmux-action-{}", std::process::id()));
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
        sb.rows = vec![
            row("%1", "api", "idle", "Fix login"),
            row("%2", "web", "working", "Build sidebar"),
            row("%3", "docs", "idle", "Review notes"),
        ];
        sb.rebuild_visible(true);
        assert_eq!(sb.visible.len(), 3);
        assert_eq!(sb.sel, 1);

        assert_eq!(sb.apply(Action::MoveSelection(1)), DispatchResult::Continue);
        assert_eq!(sb.sel, 2);
        sb.apply(Action::MoveSelection(5));
        assert_eq!(sb.sel, 3, "selection clamps to the last row");
        sb.apply(Action::MoveSelection(-9));
        assert_eq!(sb.sel, 1, "selection clamps to the first row");
        sb.apply(Action::SelectLast);
        assert_eq!(sb.sel, 3);
        sb.apply(Action::SelectFirst);
        assert_eq!(sb.sel, 1);
        sb.apply(Action::SelectIndex(2));
        assert_eq!((sb.sel, sb.sel_pane.as_str()), (2, "%2"));

        sb.apply(Action::ScrollViewport(1));
        assert_eq!(sb.scroll, 1);
        assert!(
            !sb.follow_selection,
            "wheel scrolling detaches the viewport"
        );
        sb.apply(Action::ScrollViewport(-3));
        assert_eq!(sb.scroll, 0, "viewport never scrolls above the top");

        sb.apply(Action::FocusSearch);
        assert!(sb.search_focused);
        sb.apply(Action::SearchInput("we".into()));
        sb.apply(Action::SearchInput("b".into()));
        assert_eq!(sb.query, "web");
        assert_eq!(sb.visible, vec![VisiblePane::Agent(1)]);
        assert_eq!(sb.sel_pane, "%2", "a narrowed list selects its first row");
        sb.apply(Action::SearchBackspace);
        assert_eq!(sb.query, "we");
        sb.apply(Action::AcceptSearch);
        assert!(
            !sb.search_focused,
            "Enter hands navigation back to the list"
        );
        assert_eq!(sb.query, "we", "accepting keeps the filter");
        sb.apply(Action::SearchClear);
        assert!(sb.query.is_empty());
        assert_eq!(sb.visible.len(), 3);

        sb.apply(Action::ToggleAttentionFilter);
        assert!(sb.attention_filter);
        assert_eq!(sb.visible, vec![VisiblePane::Agent(1)]);
        sb.apply(Action::SearchInput("api".into()));
        assert!(
            !sb.attention_filter,
            "typing a query replaces the attention filter"
        );
        assert_eq!(sb.visible, vec![VisiblePane::Agent(0)]);
        sb.apply(Action::ClearFilter);
        assert!(sb.query.is_empty() && !sb.attention_filter && !sb.search_focused);
        assert_eq!(sb.visible.len(), 3);

        sb.apply(Action::OpenHelp);
        assert!(sb.overlay.is_some());
        sb.overlay = None;

        // The popup owns stdin, so quitting ends the loop; the daemon ignores
        // a quit because FIFO EOF also arrives as one.
        assert_eq!(sb.apply(Action::Quit), DispatchResult::Break);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
