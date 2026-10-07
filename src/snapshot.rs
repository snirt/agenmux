//! Per-server sidebar snapshot: collapse state and layout outlive the sidebar
//! and the tmux server. Keys use names, not `$n`/`@n` ids, so they still match
//! sessions recreated after a restart.

use crate::scan::{PaneMeta, PaneRow};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Keys of absent sessions are kept for when they come back; this bounds them.
const MAX_COLLAPSED: usize = 256;

/// The `list-windows` query `layout` reads. `window_name` goes last: it may
/// contain the delimiter.
pub const WINDOWS_FMT: &str = "list-windows -a -F '#{window_id}|#{window_panes}|#{automatic-rename}|#{window_layout}|#{window_name}'";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// `#{pid}|#{start_time}`: tells this server from the one before a restart.
    pub server: String,
    /// Session names and `session:window_index` keys of collapsed branches.
    #[serde(default)]
    pub collapsed: Vec<String>,
    #[serde(default)]
    pub sessions: Vec<Session>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    #[serde(default)]
    pub windows: Vec<Window>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub index: u32,
    pub name: String,
    pub automatic_rename: bool,
    pub layout: String,
    /// The layout includes the agenmux pane: the full-height leftmost split.
    pub sidebar: bool,
    #[serde(default)]
    pub panes: Vec<Pane>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pane {
    pub path: String,
    /// Agent conf name; absent for ordinary panes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl Snapshot {
    pub fn agents(&self) -> usize {
        self.sessions
            .iter()
            .flat_map(|s| &s.windows)
            .flat_map(|w| &w.panes)
            .filter(|p| p.agent.is_some())
            .count()
    }
}

/// `<state dir>/snapshot-<socket name>`: one file per tmux server socket.
pub fn path(socket_path: &str) -> Option<PathBuf> {
    let socket = Path::new(socket_path.trim()).file_name()?.to_str()?;
    let xdg = std::env::var_os("XDG_STATE_HOME");
    let home = std::env::var_os("HOME");
    crate::diag::state_dir(
        xdg.as_deref().map(Path::new),
        home.as_deref().map(Path::new),
    )
    .map(|dir| dir.join(format!("snapshot-{socket}")))
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
    if snap.collapsed.is_empty() && snap.sessions.is_empty() {
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
}

impl Store {
    pub fn open(tmux: &mut crate::tmux::Tmux) -> Option<Store> {
        // A fresh control client answers empty until its attach completes.
        let mut out = String::new();
        for _ in 0..100 {
            out = tmux
                .run("display-message -p '#{pid}|#{start_time}|#{socket_path}'")
                .ok()?;
            if !out.trim().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut fields = out.trim().splitn(3, '|');
        let server = format!("{}|{}", fields.next()?, fields.next()?);
        let path = path(fields.next()?)?;
        let (collapsed, prev) = open(&path, &server);
        Some(Store {
            path,
            server,
            pending: collapsed.into_iter().collect(),
            sessions: Vec::new(),
            saved: None,
            prev,
        })
    }

    pub fn adopt(&mut self, collapsed: &mut HashSet<String>, panes: &[PaneMeta]) {
        adopt(&mut self.pending, collapsed, panes);
    }

    // ponytail: an empty scan keeps the last layout; a desynced read or a
    // dying server lists nothing, and that must not erase the restore offer.
    pub fn set_layout(&mut self, panes: &[PaneMeta], agents: &[PaneRow], windows: &str) {
        if !panes.is_empty() {
            self.sessions = layout(panes, agents, windows);
        }
    }

    /// Write when the content changed since the last write.
    // ponytail: last writer wins when a popup and a split sidebar both run.
    pub fn save(&mut self, collapsed: &HashSet<String>, panes: &[PaneMeta]) {
        let snap = Snapshot {
            server: self.server.clone(),
            collapsed: collapsed_keys(collapsed, &self.pending, panes),
            sessions: self.sessions.clone(),
        };
        if self.saved.as_ref() != Some(&snap) && write(&self.path, &snap).is_ok() {
            self.saved = Some(snap);
        }
    }

    /// Forget the previous server's layout, after a restore or on `x`.
    pub fn dismiss(&mut self) -> Option<Snapshot> {
        let _ = std::fs::remove_file(prev_path(&self.path));
        self.prev.take()
    }
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
pub fn layout(panes: &[PaneMeta], agents: &[PaneRow], windows: &str) -> Vec<Session> {
    let windows: HashMap<&str, Vec<&str>> = windows
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.splitn(5, '|').collect();
            (fields.len() == 5).then(|| (fields[0], fields))
        })
        .collect();
    let mut listed: HashMap<&str, usize> = HashMap::new();
    for pane in panes {
        *listed.entry(pane.window_id.as_str()).or_default() += 1;
    }
    let mut sessions: Vec<Session> = Vec::new();
    for pane in panes {
        if sessions.last().is_none_or(|s| s.name != pane.session_name) {
            sessions.push(Session {
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
            let field = |i: usize| info.map_or("", |f| f[i]);
            session.windows.push(Window {
                index: pane.window_index,
                name: pane.window_name.clone(),
                automatic_rename: field(2) != "0",
                layout: field(3).to_string(),
                sidebar: field(1)
                    .parse::<usize>()
                    .is_ok_and(|n| n > listed[pane.window_id.as_str()]),
                panes: Vec::new(),
            });
        }
        let window = session.windows.last_mut().unwrap();
        window.panes.push(Pane {
            path: pane.path.clone(),
            agent: pane
                .agent_index
                .and_then(|i| agents.get(i))
                .map(|row| row.agent.clone()),
        });
    }
    sessions
}

/// Recreate `prev`'s missing sessions, and its missing windows inside existing
/// sessions; never touch a live window. `live` is every live
/// `(session name, window index)`. `run` executes one tmux command and returns
/// its trimmed output, or `None` on failure. `resume` gives the command typed
/// into an agent's restored shell, if any.
pub fn replay(
    prev: &Snapshot,
    live: &HashSet<(String, u32)>,
    resume: impl Fn(&str) -> Option<String>,
    run: &mut impl FnMut(&[String]) -> Option<String>,
) {
    let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    for session in &prev.sessions {
        let target = format!("={}", session.name);
        let mut exists = live.iter().any(|(name, _)| *name == session.name);
        for window in &session.windows {
            if window.panes.is_empty() || live.contains(&(session.name.clone(), window.index)) {
                continue;
            }
            let first = &window.panes[0].path;
            let mut create = if exists {
                args(&[
                    "new-window",
                    "-d",
                    "-t",
                    &format!("{target}:{}", window.index),
                ])
            } else {
                args(&["new-session", "-d", "-s", &session.name])
            };
            create.extend(args(&[
                "-c",
                first,
                "-P",
                "-F",
                "#{window_index} #{pane_id}",
            ]));
            if !window.automatic_rename {
                create.extend(args(&["-n", &window.name]));
            }
            let Some(created) = run(&create) else {
                continue;
            };
            let Some((index, p0)) = created.split_once(' ') else {
                continue;
            };
            let p0 = p0.to_string();
            if !exists && index != window.index.to_string() {
                run(&args(&[
                    "move-window",
                    "-d",
                    "-s",
                    &p0,
                    "-t",
                    &format!("{target}:{}", window.index),
                ]));
            }
            exists = true;
            // The agenmux pane was index 0; a placeholder holds its cell until
            // select-layout has placed the rest.
            let (split, mut resumes) = if window.sidebar {
                (&window.panes[..], Vec::new())
            } else {
                (&window.panes[1..], vec![(p0.clone(), &window.panes[0])])
            };
            let mut last = p0.clone();
            for pane in split {
                let Some(id) = run(&args(&[
                    "split-window",
                    "-t",
                    &last,
                    "-c",
                    &pane.path,
                    "-P",
                    "-F",
                    "#{pane_id}",
                ])) else {
                    break;
                };
                resumes.push((id.clone(), pane));
                last = id;
            }
            if !window.layout.is_empty() {
                run(&args(&["select-layout", "-t", &p0, &window.layout]));
            }
            if window.sidebar {
                run(&args(&["kill-pane", "-t", &p0]));
            }
            // After the layout, so agents start at their final size.
            for (id, pane) in resumes {
                if let Some(command) = pane.agent.as_deref().and_then(&resume) {
                    run(&args(&["send-keys", "-t", &id, "-l", &command]));
                    run(&args(&["send-keys", "-t", &id, "Enter"]));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
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

    fn sample(server: &str) -> Snapshot {
        Snapshot {
            server: server.into(),
            collapsed: vec!["work".into(), "work:2".into()],
            sessions: vec![Session {
                name: "work".into(),
                windows: vec![Window {
                    index: 2,
                    name: "editor".into(),
                    automatic_rename: false,
                    layout: "abcd,80x24,0,0,1".into(),
                    sidebar: true,
                    panes: vec![
                        Pane {
                            path: "/repo".into(),
                            agent: Some("claude".into()),
                        },
                        Pane {
                            path: "/tmp".into(),
                            agent: None,
                        },
                    ],
                }],
            }],
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
        let windows = "@4|3|0|L4|my|name\n@6|1|1|L6|zsh\n";
        let sessions = layout(&panes, &agents, windows);
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
        assert_eq!(w2.panes[0].agent.as_deref(), Some("claude"));
        assert_eq!(
            w2.panes[1],
            Pane {
                path: "/work/%2".into(),
                agent: None
            }
        );
        assert_eq!((w3.sidebar, w3.automatic_rename), (false, true));
    }

    #[test]
    fn replay_creates_only_missing_branches_in_order() {
        let mut prev = sample("1|1");
        prev.sessions[0].windows.push(Window {
            index: 4,
            name: "shell".into(),
            automatic_rename: true,
            layout: "efgh,80x24,0,0,9".into(),
            sidebar: false,
            panes: vec![
                Pane {
                    path: "/a".into(),
                    agent: Some("codex".into()),
                },
                Pane {
                    path: "/b".into(),
                    agent: None,
                },
            ],
        });
        prev.sessions.push(Session {
            name: "keep".into(),
            windows: vec![Window {
                index: 0,
                panes: vec![Pane::default()],
                ..Window::default()
            }],
        });
        let live = HashSet::from([("keep".to_string(), 0)]);
        let mut log = Vec::new();
        let mut next = 10;
        replay(
            &prev,
            &live,
            |agent| (agent == "claude").then(|| "claude --continue".into()),
            &mut |cmd: &[String]| {
                log.push(cmd.join(" "));
                next += 1;
                Some(match cmd[0].as_str() {
                    "new-session" => format!("0 %{next}"),
                    "new-window" => format!("4 %{next}"),
                    _ => format!("%{next}"),
                })
            },
        );
        assert_eq!(
            log,
            [
                "new-session -d -s work -c /repo -P -F #{window_index} #{pane_id} -n editor",
                "move-window -d -s %11 -t =work:2",
                "split-window -t %11 -c /repo -P -F #{pane_id}",
                "split-window -t %13 -c /tmp -P -F #{pane_id}",
                "select-layout -t %11 abcd,80x24,0,0,1",
                "kill-pane -t %11",
                "send-keys -t %13 -l claude --continue",
                "send-keys -t %13 Enter",
                "new-window -d -t =work:4 -c /a -P -F #{window_index} #{pane_id}",
                "split-window -t %19 -c /b -P -F #{pane_id}",
                "select-layout -t %19 efgh,80x24,0,0,9",
            ]
        );
    }
}
