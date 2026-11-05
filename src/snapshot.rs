//! Per-server sidebar snapshot: collapse state and layout outlive the sidebar
//! and the tmux server. Keys use names, not `$n`/`@n` ids, so they still match
//! sessions recreated after a restart.

use crate::mux::{Multiplexer, WindowMeta};
use crate::scan::{PaneMeta, PaneRow};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Keys of absent sessions are kept for when they come back; this bounds them.
const MAX_COLLAPSED: usize = 256;

/// Agent conf name -> the command typed into its restored shell.
pub type Resume = HashMap<String, String>;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Tells this server from the one before a restart.
    pub server: String,
    /// Session names and `session:window_index` keys of collapsed branches.
    #[serde(default)]
    pub collapsed: Vec<String>,
    #[serde(default)]
    pub sessions: Vec<Session>,
    /// Sessions and windows closed on this server, newest first.
    #[serde(default)]
    pub closed: Vec<Closed>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// `$n`: tells a close from a rename. Meaningless on another server.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub windows: Vec<Window>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// `@n`, like `Session::id`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub index: u32,
    pub name: String,
    pub automatic_rename: bool,
    pub layout: String,
    /// The layout includes the agenmux pane: the full-height leftmost split,
    /// or the rightmost one with `sidebar_right`.
    pub sidebar: bool,
    #[serde(default)]
    pub sidebar_right: bool,
    #[serde(default)]
    pub panes: Vec<Pane>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    /// `%n`, like `Session::id`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub path: String,
    /// Agent conf name; absent for ordinary panes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Closed {
    /// Unix seconds.
    pub time: u64,
    /// The closed session's `$n`, window's `@n` or pane's `%n`.
    pub id: String,
    /// The closed session, or the closed window inside its session. A pane
    /// entry holds its whole window as it was, for its neighbours and layout.
    pub session: Session,
}

/// What restore checks entries against.
#[derive(Default)]
pub struct Live {
    /// Every `(session name, window index)`.
    pub windows: HashSet<(String, u32)>,
    /// Every window and pane id.
    pub ids: HashSet<String>,
    /// Windows are live by id alone: the server renumbers the windows after
    /// a closed one (Rex), so an index says nothing about which window it is.
    pub by_id: bool,
}

impl Live {
    pub fn has_window(&self, session: &str, window: &Window) -> bool {
        if self.by_id {
            self.ids.contains(&window.id)
        } else {
            self.windows.contains(&(session.to_string(), window.index))
        }
    }
}

impl Closed {
    /// A window entry holds that window; a pane entry, the pane's window.
    pub fn is_session(&self) -> bool {
        !self.is_pane() && !self.session.windows.iter().any(|w| w.id == self.id)
    }

    pub fn is_pane(&self) -> bool {
        self.session
            .windows
            .first()
            .is_some_and(|w| w.panes.iter().any(|p| p.id == self.id))
    }

    /// The closed pane of a pane entry.
    pub fn pane(&self) -> Option<&Pane> {
        let window = self.session.windows.first()?;
        window
            .panes
            .iter()
            .find(|p| self.is_pane() && p.id == self.id)
    }

    /// Restore never replaces a live branch: a session or window entry is
    /// blocked while its session name or `session:index` is in use; a pane
    /// entry needs its window still open (the window's own entry covers it
    /// otherwise).
    pub fn blocked(&self, live: &Live) -> bool {
        let name = &self.session.name;
        if self.is_session() {
            live.windows.iter().any(|(session, _)| session == name)
        } else if self.is_pane() {
            self.session
                .windows
                .iter()
                .all(|w| !live.ids.contains(&w.id))
        } else {
            self.session
                .windows
                .iter()
                .any(|w| live.has_window(name, w))
        }
    }
}

impl Snapshot {
    pub fn agents(&self) -> usize {
        agents(&self.sessions)
    }
}

pub fn agents(sessions: &[Session]) -> usize {
    sessions
        .iter()
        .flat_map(|s| &s.windows)
        .flat_map(|w| &w.panes)
        .filter(|p| p.agent.is_some())
        .count()
}

/// `<state dir>/snapshot-<name>`: one file per server (`Identity::name`).
pub fn path(name: &str) -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_STATE_HOME");
    let home = std::env::var_os("HOME");
    crate::diag::state_dir(
        xdg.as_deref().map(Path::new),
        home.as_deref().map(Path::new),
    )
    .map(|dir| dir.join(format!("snapshot-{name}")))
}

pub fn prev_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".prev");
    PathBuf::from(name)
}

fn read(path: &Path) -> Option<Snapshot> {
    toml::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Collapse keys to restore, plus a previous server's layout to offer. Another
/// server's snapshot moves to `.prev` first: this server's first write would
/// otherwise replace it.
pub fn open(path: &Path, server: &str) -> (Vec<String>, Option<Snapshot>) {
    let current = read(path);
    if current.as_ref().is_some_and(|snap| snap.server != server) {
        let _ = std::fs::rename(path, prev_path(path));
    }
    let collapsed = current.map(|snap| snap.collapsed).unwrap_or_default();
    let prev = read(&prev_path(path)).filter(|snap| !snap.sessions.is_empty());
    (collapsed, prev)
}

/// Atomic: a reader never sees a half-written file. An empty snapshot removes it.
pub fn write(path: &Path, snap: &Snapshot) -> std::io::Result<()> {
    if snap.collapsed.is_empty() && snap.sessions.is_empty() && snap.closed.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let text = toml::to_string(snap).map_err(std::io::Error::other)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// A sidebar's handle on its server's snapshot.
pub struct Store {
    path: PathBuf,
    server: String,
    /// Loaded collapse keys whose session or window has not appeared yet.
    pending: HashSet<String>,
    sessions: Vec<Session>,
    saved: Option<Snapshot>,
    /// A previous server's layout, offered until restored or dismissed.
    pub prev: Option<Snapshot>,
    /// Closed entries kept: `tmux_management.undo_history`; 0 logs nothing.
    pub limit: usize,
}

impl Store {
    pub fn open(mux: &mut dyn Multiplexer) -> Option<Store> {
        let identity = mux.identity()?;
        let path = path(&identity.name)?;
        let server = identity.server;
        let (collapsed, prev) = open(&path, &server);
        Some(Store {
            path,
            server,
            pending: collapsed.into_iter().collect(),
            sessions: Vec::new(),
            saved: None,
            prev,
            limit: 20,
        })
    }

    pub fn adopt(&mut self, collapsed: &mut HashSet<String>, panes: &[PaneMeta]) {
        adopt(&mut self.pending, collapsed, panes);
    }

    // An empty scan keeps the last layout; a desynced read or a
    // dying server lists nothing, and that must not erase the restore offer.
    // A branch gone since the last capture was closed. The first capture
    // after open has nothing to compare with: closes while no sidebar ran are
    // not logged. A pane also leaves the scan when it becomes a sidebar pane,
    // so `open` (every live id, queried only then) confirms pane closes.
    // Windows and sessions trust the scan: one left holding only a sidebar
    // pane is closing.
    pub fn set_layout(
        &mut self,
        panes: &[PaneMeta],
        agents: &[PaneRow],
        windows: &[WindowMeta],
        open: impl FnOnce() -> HashSet<String>,
    ) {
        if panes.is_empty() {
            return;
        }
        let sessions = layout(panes, agents, windows);
        let mut closed = closed_between(&self.sessions, &sessions, now());
        self.sessions = sessions;
        if closed.iter().any(Closed::is_pane) {
            let open = open();
            closed.retain(|entry| !entry.is_pane() || !open.contains(&entry.id));
        }
        if !closed.is_empty() {
            let limit = self.limit;
            self.edit_closed(|log| record(log, closed, limit));
        }
    }

    /// Write when collapse state or layout changed since the last write. The
    /// closed log is taken from the file: another sidebar may have edited it.
    // Last writer wins when a popup and a split sidebar both run.
    pub fn save(&mut self, collapsed: &HashSet<String>, panes: &[PaneMeta]) {
        let collapsed = collapsed_keys(collapsed, &self.pending, panes);
        if self
            .saved
            .as_ref()
            .is_some_and(|saved| saved.collapsed == collapsed && saved.sessions == self.sessions)
        {
            return;
        }
        let snap = Snapshot {
            server: self.server.clone(),
            collapsed,
            sessions: self.sessions.clone(),
            closed: self.closed(),
        };
        if write(&self.path, &snap).is_ok() {
            self.saved = Some(snap);
        }
    }

    /// This server's recently closed sessions and windows, newest first.
    pub fn closed(&self) -> Vec<Closed> {
        read(&self.path)
            .filter(|snap| snap.server == self.server)
            .map(|snap| snap.closed)
            .unwrap_or_default()
    }

    pub fn forget(&mut self, id: &str) {
        self.edit_closed(|log| log.retain(|entry| entry.id != id));
    }

    /// Read-modify-write of the file's log alone, so two sidebars logging the
    /// same close, or one forgetting an entry, do not undo each other.
    // No file lock; two edits racing between read and rename lose one.
    fn edit_closed(&mut self, edit: impl FnOnce(&mut Vec<Closed>)) {
        let mut snap = read(&self.path)
            .filter(|snap| snap.server == self.server)
            .or_else(|| self.saved.clone())
            .unwrap_or_else(|| Snapshot {
                server: self.server.clone(),
                ..Snapshot::default()
            });
        edit(&mut snap.closed);
        let _ = write(&self.path, &snap);
    }

    /// Forget the previous server's layout, after a restore or on `x`.
    pub fn dismiss(&mut self) -> Option<Snapshot> {
        let _ = std::fs::remove_file(prev_path(&self.path));
        self.prev.take()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Branches of `old` whose id is gone from `new`. A closed session is one
/// entry, not one per window or pane; a renamed or moved branch keeps its id.
pub fn closed_between(old: &[Session], new: &[Session], time: u64) -> Vec<Closed> {
    let live: HashSet<&str> = new
        .iter()
        .flat_map(|s| {
            std::iter::once(&s.id).chain(
                s.windows
                    .iter()
                    .flat_map(|w| std::iter::once(&w.id).chain(w.panes.iter().map(|p| &p.id))),
            )
        })
        .map(String::as_str)
        .collect();
    let mut closed = Vec::new();
    for session in old {
        if !live.contains(session.id.as_str()) {
            closed.push(Closed {
                time,
                id: session.id.clone(),
                session: session.clone(),
            });
            continue;
        }
        for window in &session.windows {
            let gone: Vec<&String> = if live.contains(window.id.as_str()) {
                let panes = window.panes.iter().map(|p| &p.id);
                panes.filter(|id| !live.contains(id.as_str())).collect()
            } else {
                vec![&window.id]
            };
            for id in gone {
                closed.push(Closed {
                    time,
                    id: id.clone(),
                    session: Session {
                        windows: vec![window.clone()],
                        ..session.clone()
                    },
                });
            }
        }
    }
    closed
}

/// Newest first, each close once (every running sidebar sees it), capped. A
/// pane entry goes once its window closes: it can never be restored again.
pub fn record(log: &mut Vec<Closed>, closed: Vec<Closed>, limit: usize) {
    let windows: HashSet<&str> = closed
        .iter()
        .filter(|new| !new.is_pane())
        .flat_map(|new| new.session.windows.iter().map(|w| w.id.as_str()))
        .collect();
    log.retain(|entry| {
        closed.iter().all(|new| new.id != entry.id)
            && !(entry.is_pane()
                && entry
                    .session
                    .windows
                    .iter()
                    .any(|w| windows.contains(w.id.as_str())))
    });
    log.splice(0..0, closed);
    log.truncate(limit);
}

pub fn window_key(session: &str, index: u32) -> String {
    format!("{session}:{index}")
}

/// Move pending keys that now name a live session or window into `collapsed`
/// as that branch's id. Returns whether any matched.
pub fn adopt(
    pending: &mut HashSet<String>,
    collapsed: &mut HashSet<String>,
    panes: &[PaneMeta],
) -> bool {
    let before = pending.len();
    for pane in panes {
        if pending.remove(&pane.session_name) {
            collapsed.insert(pane.session_id.clone());
        }
        if pending.remove(&window_key(&pane.session_name, pane.window_index)) {
            collapsed.insert(pane.window_id.clone());
        }
    }
    pending.len() != before
}

/// Live collapsed ids under their current names (so a rename re-keys), then
/// keys still waiting for their session to return.
pub fn collapsed_keys(
    collapsed: &HashSet<String>,
    pending: &HashSet<String>,
    panes: &[PaneMeta],
) -> Vec<String> {
    let mut live = BTreeSet::new();
    for pane in panes {
        if collapsed.contains(&pane.session_id) {
            live.insert(pane.session_name.clone());
        }
        if collapsed.contains(&pane.window_id) {
            live.insert(window_key(&pane.session_name, pane.window_index));
        }
    }
    let waiting: BTreeSet<_> = pending
        .iter()
        .filter(|key| !live.contains(*key))
        .cloned()
        .collect();
    live.into_iter()
        .chain(waiting)
        .take(MAX_COLLAPSED)
        .collect()
}

/// Sessions, windows and panes in scan order. `panes` excludes the sidebar,
/// so a window reporting more panes than listed still holds an agenmux pane.
pub fn layout(panes: &[PaneMeta], agents: &[PaneRow], windows: &[WindowMeta]) -> Vec<Session> {
    let windows: HashMap<&str, &WindowMeta> = windows.iter().map(|w| (w.id.as_str(), w)).collect();
    let mut listed: HashMap<&str, usize> = HashMap::new();
    for pane in panes {
        *listed.entry(pane.window_id.as_str()).or_default() += 1;
    }
    let mut sessions: Vec<Session> = Vec::new();
    for pane in panes {
        if sessions.last().is_none_or(|s| s.name != pane.session_name) {
            sessions.push(Session {
                id: pane.session_id.clone(),
                name: pane.session_name.clone(),
                windows: Vec::new(),
            });
        }
        let session = sessions.last_mut().unwrap();
        if session
            .windows
            .last()
            .is_none_or(|w| w.index != pane.window_index)
        {
            let info = windows.get(pane.window_id.as_str());
            let sidebar = info.is_some_and(|w| w.panes > listed[pane.window_id.as_str()]);
            session.windows.push(Window {
                id: pane.window_id.clone(),
                index: pane.window_index,
                name: pane.window_name.clone(),
                automatic_rename: info.is_none_or(|w| w.automatic_rename),
                layout: info.map_or_else(String::new, |w| w.layout.clone()),
                sidebar,
                sidebar_right: sidebar && info.is_some_and(|w| w.sidebar_right),
                panes: Vec::new(),
            });
        }
        let window = session.windows.last_mut().unwrap();
        window.panes.push(Pane {
            id: pane.pane.clone(),
            path: pane.path.clone(),
            agent: pane
                .agent_index
                .and_then(|i| agents.get(i))
                .map(|row| row.agent.clone()),
        });
    }
    sessions
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn meta(
        session: (&str, &str),
        window: (&str, u32),
        pane: &str,
        agent: Option<usize>,
    ) -> PaneMeta {
        PaneMeta {
            pane: pane.into(),
            pane_index: 0,
            pane_title: String::new(),
            command: "zsh".into(),
            path: format!("/work/{pane}"),
            window_id: window.0.into(),
            window_index: window.1,
            window_name: format!("w{}", window.1),
            session_id: session.0.into(),
            session_name: session.1.into(),
            agent_index: agent,
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("agenmux-snapshot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("snapshot-default")
    }

    pub(crate) fn sample(server: &str) -> Snapshot {
        Snapshot {
            server: server.into(),
            collapsed: vec!["work".into(), "work:2".into()],
            sessions: vec![Session {
                name: "work".into(),
                windows: vec![Window {
                    id: "@4".into(),
                    index: 2,
                    name: "editor".into(),
                    automatic_rename: false,
                    layout: "abcd,80x24,0,0,1".into(),
                    sidebar: true,
                    sidebar_right: false,
                    panes: vec![
                        Pane {
                            id: "%1".into(),
                            path: "/repo".into(),
                            agent: Some("claude".into()),
                        },
                        Pane {
                            id: "%2".into(),
                            path: "/tmp".into(),
                            agent: None,
                        },
                    ],
                }],
                ..Session::default()
            }],
            ..Snapshot::default()
        }
    }

    #[test]
    fn snapshot_round_trips_and_empty_removes_the_file() {
        let path = temp("roundtrip");
        write(&path, &sample("1|1")).unwrap();
        assert_eq!(read(&path), Some(sample("1|1")));
        write(
            &path,
            &Snapshot {
                server: "1|1".into(),
                ..Snapshot::default()
            },
        )
        .unwrap();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn same_server_restores_collapse_and_keeps_the_file() {
        let path = temp("same");
        write(&path, &sample("1|1")).unwrap();
        let (collapsed, prev) = open(&path, "1|1");
        assert_eq!(collapsed, ["work", "work:2"]);
        assert_eq!(prev, None);
        assert!(path.exists() && !prev_path(&path).exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn new_server_moves_the_snapshot_to_prev_before_any_write() {
        let path = temp("new");
        write(&path, &sample("1|1")).unwrap();
        let (collapsed, prev) = open(&path, "2|2");
        assert_eq!(
            collapsed,
            ["work", "work:2"],
            "collapse still applies by name"
        );
        assert_eq!(prev, Some(sample("1|1")));
        assert!(!path.exists());
        // the fresh server's first write must not clobber the offer
        write(
            &path,
            &Snapshot {
                server: "2|2".into(),
                collapsed: vec!["x".into()],
                ..Snapshot::default()
            },
        )
        .unwrap();
        let (_, prev) = open(&path, "2|2");
        assert_eq!(
            prev,
            Some(sample("1|1")),
            "reopening the sidebar keeps the offer"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn pending_keys_adopt_live_ids_and_absent_ones_are_kept() {
        let panes = [
            meta(("$1", "work"), ("@4", 2), "%1", None),
            meta(("$2", "play"), ("@5", 0), "%2", None),
        ];
        let mut pending: HashSet<String> = ["work".into(), "play:0".into(), "gone".into()].into();
        let mut collapsed = HashSet::new();
        assert!(adopt(&mut pending, &mut collapsed, &panes));
        assert_eq!(
            collapsed,
            HashSet::from(["$1".to_string(), "@5".to_string()])
        );
        assert_eq!(pending, HashSet::from(["gone".to_string()]));
        assert_eq!(
            collapsed_keys(&collapsed, &pending, &panes),
            ["play:0", "work", "gone"]
        );
        // a live rename re-keys
        let renamed = [meta(("$1", "job"), ("@4", 2), "%1", None)];
        assert_eq!(
            collapsed_keys(&collapsed, &HashSet::new(), &renamed),
            ["job"]
        );
    }

    #[test]
    fn layout_groups_panes_and_detects_the_sidebar_cell() {
        let agents = crate::scan::from_tsv("%1\twork:2.0\tclaude\tidle\trepo\t\n");
        let panes = [
            meta(("$1", "work"), ("@4", 2), "%1", Some(0)),
            meta(("$1", "work"), ("@4", 2), "%2", None),
            meta(("$1", "work"), ("@6", 3), "%3", None),
        ];
        let windows = [
            window("@4", 3, false, true, "L4"),
            window("@6", 1, true, false, "L6"),
        ];
        let sessions = layout(&panes, &agents, &windows);
        assert_eq!(sessions.len(), 1);
        let [w2, w3] = &sessions[0].windows[..] else {
            panic!()
        };
        assert_eq!(
            (
                w2.index,
                w2.layout.as_str(),
                w2.sidebar,
                w2.automatic_rename
            ),
            (2, "L4", true, false)
        );
        assert!(w2.sidebar_right && !w3.sidebar_right);
        assert_eq!(w2.panes[0].agent.as_deref(), Some("claude"));
        assert_eq!(
            w2.panes[1],
            Pane {
                id: "%2".into(),
                path: "/work/%2".into(),
                agent: None
            }
        );
        assert_eq!((w3.sidebar, w3.automatic_rename), (false, true));
    }

    fn store(name: &str) -> Store {
        Store {
            path: temp(name),
            server: "1|1".into(),
            pending: HashSet::new(),
            sessions: Vec::new(),
            saved: None,
            prev: None,
            limit: 20,
        }
    }

    fn window(id: &str, panes: usize, auto: bool, right: bool, layout: &str) -> WindowMeta {
        WindowMeta {
            id: id.into(),
            panes,
            automatic_rename: auto,
            sidebar_right: right,
            layout: layout.into(),
        }
    }

    fn windows() -> [WindowMeta; 3] {
        [
            window("@4", 1, true, false, "L4"),
            window("@6", 2, true, false, "L6"),
            window("@7", 1, true, false, "L7"),
        ]
    }

    fn open_ids(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn ids(log: &[Closed]) -> Vec<&str> {
        log.iter().map(|entry| entry.id.as_str()).collect()
    }

    #[test]
    fn closes_are_logged_by_id_and_renames_are_not() {
        let all = [
            meta(("$1", "work"), ("@4", 2), "%1", None),
            meta(("$1", "work"), ("@6", 3), "%2", None),
            meta(("$1", "work"), ("@6", 3), "%3", None),
            meta(("$2", "play"), ("@7", 0), "%4", None),
        ];
        let mut a = store("closes");
        let mut b = store("closes");
        a.set_layout(&all, &[], &windows(), HashSet::new);
        b.set_layout(&all, &[], &windows(), HashSet::new);
        assert!(a.closed().is_empty(), "the first capture logs nothing");

        // renamed session and window keep their ids: not a close
        let mut renamed = all.clone();
        for pane in &mut renamed {
            pane.session_name = pane.session_name.replace("work", "job");
            pane.window_name = "renamed".into();
        }
        a.set_layout(&renamed, &[], &windows(), HashSet::new);
        assert!(a.closed().is_empty());

        // close window @6, then session $2; an empty scan logs nothing
        a.set_layout(
            &[renamed[0].clone(), renamed[3].clone()],
            &[],
            &windows(),
            HashSet::new,
        );
        a.set_layout(&renamed[..1], &[], &windows(), HashSet::new);
        a.set_layout(&[], &[], &windows(), HashSet::new);
        let log = a.closed();
        assert_eq!(ids(&log), ["$2", "@6"], "newest first");
        let window = &log[1];
        assert!(!window.is_session());
        assert_eq!(window.session.name, "job");
        let [w] = &window.session.windows[..] else {
            panic!()
        };
        assert_eq!((w.index, w.panes.len(), w.layout.as_str()), (3, 2, "L6"));
        assert!(log[0].is_session());
        assert_eq!(log[0].session.windows[0].index, 0);

        // a second sidebar seeing the same closes adds nothing
        b.set_layout(&all[..1], &[], &windows(), HashSet::new);
        assert_eq!(b.closed().len(), 2);

        // a save keeps the log; forget removes an entry for every sidebar
        a.save(&HashSet::new(), &renamed[..1]);
        assert_eq!(b.closed().len(), 2);
        b.forget("@6");
        a.save(&HashSet::from(["$1".to_string()]), &renamed[..1]);
        assert_eq!(ids(&a.closed()), ["$2"]);
        let _ = std::fs::remove_dir_all(a.path.parent().unwrap());
    }

    #[test]
    fn log_is_newest_first_capped_and_keeps_its_file() {
        let entry = |id: usize| Closed {
            time: id as u64,
            id: format!("@{id}"),
            session: Session::default(),
        };
        let mut log = Vec::new();
        for id in 0..25 {
            record(&mut log, vec![entry(id)], 20);
        }
        record(&mut log, vec![entry(24)], 20);
        assert_eq!(log.len(), 20);
        assert_eq!((log[0].id.as_str(), log[19].id.as_str()), ("@24", "@5"));
        // a lowered limit trims on the next close; 0 keeps nothing
        let mut short = log.clone();
        record(&mut short, vec![entry(25)], 3);
        assert_eq!(ids(&short), ["@25", "@24", "@23"]);
        record(&mut short, vec![entry(26)], 0);
        assert!(short.is_empty());

        let path = temp("closed-only");
        let snap = Snapshot {
            server: "1|1".into(),
            closed: log,
            ..Snapshot::default()
        };
        write(&path, &snap).unwrap();
        assert_eq!(read(&path), Some(snap));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn restore_is_blocked_while_the_name_is_in_use() {
        let mut session = sample("1|1").sessions.remove(0);
        let closed_session = Closed {
            time: 0,
            id: "$1".into(),
            session: session.clone(),
        };
        session.windows[0].index = 3;
        let closed_window = Closed {
            time: 0,
            id: "@4".into(),
            session,
        };
        let closed_pane = Closed {
            id: "%2".into(),
            ..closed_window.clone()
        };
        let live = |windows: &[(&str, u32)], ids: &[&str]| Live {
            windows: windows.iter().map(|(s, i)| (s.to_string(), *i)).collect(),
            ids: ids.iter().map(|id| id.to_string()).collect(),
            by_id: false,
        };
        let work2 = live(&[("work", 2)], &["@9"]);
        assert!(closed_session.blocked(&work2));
        assert!(!closed_window.blocked(&work2), "work:3 is free");
        assert!(closed_window.blocked(&live(&[("work", 3)], &[])));
        assert!(!closed_session.blocked(&Live::default()));
        assert!(closed_pane.blocked(&work2), "its window @4 is gone");
        assert!(!closed_pane.blocked(&live(&[], &["@4"])));

        // Rex renumbers the windows after a closed one: only its id counts
        let by_id = |live: Live| Live {
            by_id: true,
            ..live
        };
        let index = closed_window.session.windows[0].index;
        let renumbered = by_id(live(&[("work", index)], &["@9"]));
        assert!(!closed_window.blocked(&renumbered));
        let id = closed_window.session.windows[0].id.clone();
        assert!(closed_window.blocked(&by_id(live(&[], &[id.as_str()]))));
    }

    #[test]
    fn pane_close_is_its_own_entry_holding_its_window() {
        let old = [
            meta(("$1", "work"), ("@4", 2), "%1", None),
            meta(("$1", "work"), ("@4", 2), "%2", None),
            meta(("$1", "work"), ("@4", 2), "%3", None),
        ];
        let before = layout(&old, &[], &[]);
        let after = layout(&old[..1], &[], &[]);
        let log = closed_between(&before, &after, 7);
        assert_eq!(ids(&log), ["%2", "%3"]);
        let entry = &log[0];
        assert!(entry.is_pane() && !entry.is_session());
        assert_eq!(entry.pane().map(|p| p.path.as_str()), Some("/work/%2"));
        assert_eq!(entry.session.windows[0].panes.len(), 3);
        // a pane moved to another window keeps its id: not a close
        let mut moved = old.to_vec();
        moved[2].window_id = "@5".into();
        moved[2].window_index = 3;
        assert!(closed_between(&before, &layout(&moved, &[], &[]), 7).is_empty());

        // a pane that became a sidebar pane left the scan but is still open
        let mut store = store("sidebar-pane");
        store.set_layout(&old, &[], &[], HashSet::new);
        store.set_layout(&old[..2], &[], &[], || open_ids(&["%1", "%2", "%3"]));
        assert!(store.closed().is_empty());
        store.set_layout(&old[..1], &[], &[], || open_ids(&["%1", "%3"]));
        assert_eq!(ids(&store.closed()), ["%2"]);
        // once its window closes, the pane entry can never be restored
        store.set_layout(
            &[meta(("$1", "work"), ("@5", 3), "%4", None)],
            &[],
            &[],
            HashSet::new,
        );
        assert_eq!(ids(&store.closed()), ["@4"]);
        let _ = std::fs::remove_dir_all(store.path.parent().unwrap());
    }
}
