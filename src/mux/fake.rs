// An in-memory multiplexer for shared-code tests: canned panes, screens and
// options in, every action it was asked to take out. Shared policy is tested
// once through it, never once per backend.
use super::*;
use crate::scan::PaneMeta;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Default)]
pub struct FakeMux {
    /// Each pane and the pid of its process; 0 is a plain shell.
    pub panes: Vec<(PaneMeta, u32)>,
    pub screens: HashMap<String, String>,
    pub options: BTreeMap<String, String>,
    pub changes: Changes,
    /// Actions taken, shared so a test can read them after handing the fake off.
    pub log: Rc<RefCell<Vec<String>>>,
}

pub fn pane(session: &str, window: &str, pane: &str, command: &str) -> PaneMeta {
    PaneMeta {
        pane: pane.into(),
        pane_index: 0,
        pane_title: String::new(),
        command: command.into(),
        path: format!("/work/{pane}"),
        window_id: window.into(),
        window_index: 1,
        window_name: format!("w{window}"),
        session_id: session.into(),
        session_name: format!("s{session}"),
        agent_index: None,
    }
}

impl FakeMux {
    fn act(&self, action: String) {
        self.log.borrow_mut().push(action);
    }

    fn id<'a>(kind: Kind, ids: Ids<'a>) -> &'a str {
        match kind {
            Kind::Session => ids.session,
            Kind::Window => ids.window,
            Kind::Pane => ids.pane,
        }
    }

    fn find(&self, kind: Kind, ids: Ids) -> Option<&PaneMeta> {
        self.panes
            .iter()
            .map(|(meta, _)| meta)
            .find(|meta| match kind {
                Kind::Session => meta.session_id == ids.session,
                Kind::Window => meta.window_id == ids.window,
                Kind::Pane => meta.pane == ids.pane,
            })
    }
}

impl Multiplexer for FakeMux {
    fn panes(&mut self, skip: Option<&str>) -> Result<Vec<ParsedPane>, Error> {
        Ok(self
            .panes
            .iter()
            .filter(|(meta, _)| skip != Some(meta.pane.as_str()))
            .map(|(meta, pid)| ParsedPane {
                meta: meta.clone(),
                pid: *pid,
                width: 80,
                height: 24,
            })
            .collect())
    }
    fn capture(&mut self, pane: &str) -> Result<String, Error> {
        self.screens
            .get(pane)
            .cloned()
            .ok_or_else(|| Error::Failed(format!("no pane {pane}")))
    }
    fn focus(&mut self) -> Option<ClientFocus> {
        None
    }

    fn events_fd(&self) -> Option<RawFd> {
        None
    }
    fn events_buffered(&self) -> bool {
        false
    }
    fn drain_events(&mut self) -> Result<bool, Error> {
        Ok(false)
    }
    fn changes(&self) -> &Changes {
        &self.changes
    }
    fn take_changes(&mut self) -> Changes {
        std::mem::take(&mut self.changes)
    }
    fn defer_output(&mut self, panes: HashSet<String>) {
        self.changes.panes.extend(panes);
    }
    fn covered_session(&self) -> Option<&str> {
        None
    }
    fn follow_session(&mut self, _session: &str) -> bool {
        true
    }

    fn is_live(&mut self, kind: Option<Kind>, ids: Ids) -> bool {
        match kind {
            Some(kind) => self.find(kind, ids).is_some(),
            None => self
                .find(Kind::Pane, ids)
                .is_some_and(|meta| meta.window_id == ids.window && meta.session_id == ids.session),
        }
    }
    fn name(&mut self, kind: Kind, ids: Ids) -> Result<String, Error> {
        let meta = self.find(kind, ids).ok_or(Error::Exited)?;
        Ok(match kind {
            Kind::Session => meta.session_name.clone(),
            Kind::Window => meta.window_name.clone(),
            Kind::Pane => meta.pane_title.clone(),
        })
    }
    fn rename(&mut self, kind: Kind, ids: Ids, name: &str) -> Result<(), Error> {
        self.act(format!("rename {kind:?} {} {name}", Self::id(kind, ids)));
        Ok(())
    }
    fn delete(&mut self, kind: Kind, ids: Ids) -> Result<(), Error> {
        self.act(format!("delete {kind:?} {}", Self::id(kind, ids)));
        Ok(())
    }
    fn pane_cwd(&mut self, ids: Ids) -> Result<String, Error> {
        self.find(Kind::Pane, ids)
            .map(|meta| meta.path.clone())
            .ok_or(Error::Exited)
    }
    fn session_cwd(&mut self, session: &str) -> Result<String, Error> {
        let ids = Ids {
            session,
            window: "",
            pane: "",
        };
        self.find(Kind::Session, ids)
            .map(|meta| meta.path.clone())
            .ok_or(Error::Exited)
    }
    fn create(&mut self, new: New) -> Result<String, Error> {
        self.act(match new {
            New::Session { name, cwd } => format!("create session {name} in {cwd}"),
            New::Window { session, name, cwd } => {
                format!("create window {session} {name} in {cwd}")
            }
            New::Launch { session, cwd, argv } => format!("launch {session} {argv:?} in {cwd}"),
        });
        Ok("%new".into())
    }

    fn jump(&mut self, pane: &str) -> Result<(), Error> {
        self.act(format!("jump {pane}"));
        Ok(())
    }
    fn show(&mut self, client: &str, pane: &str, stay: bool) -> Result<String, Error> {
        self.act(format!("show {pane} to {client} stay={stay}"));
        Ok("@new".into())
    }
    fn focus_sidebar(&mut self, client: &str, expected_window: Option<&str>) {
        self.act(format!("focus sidebar of {client} in {expected_window:?}"));
    }
    fn message(&mut self, client: &str, text: &str) -> bool {
        self.act(format!("message {client}: {text}"));
        true
    }
    fn copy(&mut self, _client: Option<&str>, text: &str) -> Result<Option<String>, Error> {
        self.act(format!("copy {text}"));
        Ok(None)
    }
    fn reference(&self, kind: Kind, id: &str) -> String {
        format!("{kind:?} {id}")
    }

    fn identity(&mut self) -> Option<Identity> {
        None
    }
    fn windows(&mut self) -> Result<Vec<WindowMeta>, Error> {
        Ok(Vec::new())
    }
    fn live(&mut self) -> Live {
        let mut live = Live::default();
        for (meta, _) in &self.panes {
            live.windows
                .insert((meta.session_name.clone(), meta.window_index));
            live.ids.insert(meta.window_id.clone());
            live.ids.insert(meta.pane.clone());
        }
        live
    }
    fn restore(&mut self, snap: &Snapshot, _live: &Live, _resume: &Resume) {
        self.act(format!("restore {} sessions", snap.sessions.len()));
    }
    fn restore_pane(&mut self, entry: &Closed, _live: &Live, _resume: &Resume) {
        self.act(format!("restore pane {}", entry.id));
    }

    fn options(&mut self) -> Result<BTreeMap<String, String>, ConfigError> {
        Ok(self.options.clone())
    }
    fn reload_token(&mut self) -> Option<String> {
        None
    }
    fn publish_reload(&mut self, _token: &str) -> Result<(), Error> {
        self.act("publish reload".into());
        Ok(())
    }

    fn sidebar_panes(&mut self) -> Result<Vec<SidebarPane>, Error> {
        Ok(Vec::new())
    }
    fn visible_windows(&mut self) -> HashSet<String> {
        HashSet::new()
    }
    fn resize_panes(&mut self, cols: &[(String, usize)]) -> bool {
        self.act(format!("resize {cols:?}"));
        true
    }
    fn close_panes(&mut self, panes: &[String]) {
        self.act(format!("close {panes:?}"));
    }
    fn close_sidebars(&mut self) {
        self.act("close sidebars".into());
    }
    fn remember_width(&mut self, cols: usize) -> bool {
        self.act(format!("remember width {cols}"));
        true
    }
    fn connection(&mut self) -> Option<String> {
        Some("fake".into())
    }
    fn owner(&mut self) -> Option<Owner> {
        None
    }
    fn claim(&mut self, client: &str, _runtime: &Path) -> Result<(), Error> {
        self.act(format!("claim {client}"));
        Ok(())
    }
    fn release(&mut self, generation: Option<&str>) {
        self.act(format!("release {generation:?}"));
    }
}
