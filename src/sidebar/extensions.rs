//! Sidebar side of the Lua extension host: turns scan diffs into plugin events
//! and executes the requests plugins queue. Plugins never see tmux directly.
use super::{client_message, DispatchResult, LauncherTarget, Overlay, Sidebar};
use crate::ext::{Event, Extensions, PaneInfo, Request};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How often periodic scans stat plugin sources for hot reload.
const RELOAD_CHECK: Duration = Duration::from_secs(2);
/// Wake interval while plugin jobs are in flight.
pub(super) const JOB_WAKE: Duration = Duration::from_millis(50);

pub(super) struct ExtRuntime {
    host: Extensions,
    /// pane id -> agent state at the last sync (None for ordinary panes).
    seen: HashMap<String, Option<String>>,
    selected: Option<String>,
    checked: Instant,
}

impl ExtRuntime {
    pub(super) fn load(plugin_dir: &std::path::Path) -> Option<Self> {
        if !crate::ext::enabled() {
            return None;
        }
        let (files, dirs) = crate::ext::source_layout(plugin_dir);
        Some(Self {
            host: Extensions::load(&files, dirs, true),
            seen: HashMap::new(),
            selected: None,
            checked: Instant::now(),
        })
    }

    pub(super) fn jobs_pending(&self) -> bool {
        self.host.jobs_pending()
    }
}

impl Sidebar {
    fn pane_info(&self, index: usize) -> PaneInfo {
        let pane = &self.panes[index];
        let agent = pane.agent_index.and_then(|i| self.rows.get(i));
        PaneInfo {
            id: pane.pane.clone(),
            session: pane.session_name.clone(),
            session_id: pane.session_id.clone(),
            window: pane.window_name.clone(),
            window_id: pane.window_id.clone(),
            cwd: pane.path.clone(),
            command: pane.command.clone(),
            title: pane.pane_title.clone(),
            agent: agent.map(|row| row.agent.clone()),
            state: agent.map(|row| row.state.clone()),
        }
    }

    fn selected_info(&self) -> Option<PaneInfo> {
        let (_, pane) = self.selected_pane()?;
        let index = self.panes.iter().position(|p| p.pane == pane.pane)?;
        Some(self.pane_info(index))
    }

    /// Fire events for what changed since the last sync, collect finished
    /// jobs, and run queued requests. Returns true when a redraw is needed.
    pub(super) fn ext_sync(&mut self, periodic: bool) -> bool {
        let Some(mut ext) = self.ext.take() else {
            return false;
        };
        if periodic && ext.checked.elapsed() >= RELOAD_CHECK {
            ext.checked = Instant::now();
            if ext.host.sources_changed() {
                trace!("extensions: sources changed, reloading");
                if let Some(fresh) = ExtRuntime::load(&self.plugin_dir) {
                    ext = fresh; // empty `seen`: every pane is re-announced
                    self.last_frame.clear();
                }
            }
        }
        let mut current = HashMap::new();
        for index in 0..self.panes.len() {
            let info = self.pane_info(index);
            match ext.seen.get(&info.id) {
                None => ext.host.fire(Event::PaneAdded, &info, None),
                Some(old) if *old != info.state && info.state.is_some() => {
                    ext.host
                        .fire(Event::AgentStateChanged, &info, old.as_deref())
                }
                Some(_) => {}
            }
            current.insert(info.id.clone(), info.state.clone());
        }
        for gone in ext.seen.keys().filter(|id| !current.contains_key(*id)) {
            let info = PaneInfo {
                id: gone.clone(),
                ..PaneInfo::default()
            };
            ext.host.fire(Event::PaneRemoved, &info, None);
        }
        ext.host.retain_badges(|pane| current.contains_key(pane));
        ext.seen = current;
        let selected = self.selected_info();
        if selected.as_ref().map(|info| &info.id) != ext.selected.as_ref() {
            ext.selected = selected.as_ref().map(|info| info.id.clone());
            if let Some(info) = &selected {
                ext.host.fire(Event::SelectionChanged, info, None);
            }
        }
        ext.host.poll();
        self.ext = Some(ext);
        let requested = self.ext_apply();
        let badges = self
            .ext
            .as_ref()
            .is_some_and(|ext| ext.host.take_badges_changed());
        if badges {
            self.last_frame.clear();
        }
        requested || badges
    }

    pub(super) fn ext_key(&mut self, sequence: &str, client: Option<String>) -> DispatchResult {
        let client = client
            .filter(|value| !value.is_empty())
            .or_else(|| (!self.popup_client.is_empty()).then(|| self.popup_client.clone()));
        let pane = self.selected_info();
        if let Some(ext) = &self.ext {
            ext.host.run_key(sequence, pane.as_ref(), client.as_deref());
        }
        self.ext_apply_dispatch()
    }

    pub(super) fn ext_select(
        &mut self,
        view: crate::ext::ListView,
        index: usize,
    ) -> DispatchResult {
        if let Some(ext) = &self.ext {
            ext.host.select(view, index);
        }
        self.ext_apply_dispatch()
    }

    fn ext_apply(&mut self) -> bool {
        let before = self.overlay.is_some();
        // Asynchronous requests keep a popup open even when they launch.
        let _ = self.ext_apply_dispatch();
        self.overlay.is_some() != before
    }

    fn ext_apply_dispatch(&mut self) -> DispatchResult {
        let Some(ext) = &self.ext else {
            return DispatchResult::Continue;
        };
        for error in ext.host.take_errors() {
            eprintln!("agenmux ext: {error}");
        }
        let mut result = DispatchResult::Continue;
        for request in ext.host.take_requests() {
            match request {
                Request::Notify { message, client } => match client {
                    Some(client) => client_message(&client, &message),
                    None => eprintln!("agenmux ext: {message}"),
                },
                Request::ShowList(view) => {
                    self.overlay = Some(Overlay::List { view, sel: 0 });
                    self.last_frame.clear();
                }
                Request::SendKeys { pane, text } => {
                    if !self.settings.settings.tmux_management_enabled {
                        continue;
                    }
                    if self.panes.iter().any(|p| p.pane == pane) {
                        let _ = crate::tmux::command_status(&[
                            "send-keys",
                            "-t",
                            &pane,
                            "-l",
                            "--",
                            &text,
                        ]);
                    }
                }
                Request::OpenWindow {
                    pane,
                    command,
                    args,
                    client,
                } => {
                    if !self.settings.settings.tmux_management_enabled {
                        continue;
                    }
                    let (Some(client), Some(meta)) =
                        (client, self.panes.iter().find(|p| p.pane == pane).cloned())
                    else {
                        trace!("extension window ignored: unknown client or pane");
                        continue;
                    };
                    let target = LauncherTarget {
                        pane_id: meta.pane,
                        window_id: meta.window_id,
                        session_id: meta.session_id,
                        client,
                    };
                    let launcher = crate::app_config::QuickLauncher {
                        id: "extension".into(),
                        sequence: String::new(),
                        label: command.clone(),
                        command,
                        args,
                        working_directory: crate::app_config::LauncherWorkingDirectory::Selected,
                        enabled: true,
                    };
                    if self.execute_quick_launcher(&target, &launcher) == DispatchResult::Break {
                        result = DispatchResult::Break;
                    }
                }
            }
        }
        result
    }

    /// Badge text for a pane row, already colored, with its cell width.
    pub(super) fn ext_badges(&self, pane: &str) -> Option<(String, usize)> {
        use crate::ext::Highlight;
        let ext = self.ext.as_ref()?;
        let badges = ext.host.badges(pane);
        if badges.is_empty() {
            return None;
        }
        let mut text = String::new();
        let mut width = 0;
        for badge in badges {
            let color = match badge.hl {
                Highlight::Muted => self.palette.muted_fg.fg(""),
                Highlight::Accent => self.palette.accent_fg.fg(""),
                Highlight::Ok => self.palette.idle_fg.fg(""),
                Highlight::Warn => self.palette.working_fg.fg(""),
                Highlight::Error => self.palette.blocked_fg.fg(""),
            };
            text.push_str(&format!(" {color}{}{}[0m", badge.text, super::E));
            width += 1 + badge.text.chars().count();
        }
        Some((text, width))
    }
}
