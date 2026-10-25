use super::{Ids, Kind, Multiplexer};
use crate::scan::ParsedPane;
use crate::snapshot::{Closed, Live, Snapshot};
use crate::tmux::{PendingChanges, TmuxError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};

const TERMINAL: &str = "com.superlogical.terminal";
const SHELL: &str = "com.superlogical.terminal.shell";
/// Label of the sidebar blocks agenmux places; scans skip them like tmux's
/// own sidebar panes.
const SIDEBAR: &str = "agenmux";
/// The floating popup's block: a sidebar that closes after one jump.
const POPUP: &str = "agenmux-popup";
const SCAN: &str = include_str!("rex_scan.lua");
const PLACE: &str = include_str!("rex_place.lua");
/// Sidebar share of the window before the user ever resizes one.
const DEFAULT_RATIO: f64 = 0.15;

/// The `rex` CLI. Actions run from init.lua get the server's bare PATH, so
/// fall back to the CLI shipped beside the server, then the app's install.
fn rex_cli() -> &'static std::path::Path {
    static CLI: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    CLI.get_or_init(|| {
        let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let on_path = |name: &str| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join(name))
                .find(|path| path.is_file())
        };
        env("AGENMUX_REX")
            .or_else(|| env("REX_BIN_DIR").map(|dir| dir.join("rex")))
            .filter(|path| path.is_file())
            .or_else(|| on_path("rex"))
            .or_else(|| {
                env("HOME").map(|home| {
                    home.join("Library/Application Support/com.superlogical.rex/RexServer")
                        .join("Versions/Current/Rex Server.app/Contents/MacOS/rex")
                })
            })
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("rex"))
    })
}

/// Lua encodes an empty table as `{}`, not `[]`.
fn list<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    match Value::deserialize(d)? {
        Value::Array(items) => items
            .into_iter()
            .map(|item| serde_json::from_value(item).map_err(serde::de::Error::custom))
            .collect(),
        _ => Ok(Vec::new()),
    }
}

/// What the scan script returns.
#[derive(Deserialize, Default)]
struct Listing {
    #[serde(default, deserialize_with = "list")]
    sessions: Vec<Session>,
}

#[derive(Deserialize)]
struct Session {
    session_id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    active_window_id: Option<String>,
    #[serde(default, deserialize_with = "list")]
    windows: Vec<Window>,
}

#[derive(Deserialize)]
struct Window {
    window_id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    focused_block_id: Option<String>,
    /// The tiled layer's layout, kept in snapshots.
    #[serde(default)]
    layout: Value,
    #[serde(default)]
    others: usize,
    #[serde(default, deserialize_with = "list")]
    blocks: Vec<Block>,
}

#[derive(Deserialize)]
struct Block {
    block_id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    process: Process,
    #[serde(default)]
    title: Title,
    #[serde(default)]
    size: Size,
}

impl Block {
    fn agenmux(&self) -> bool {
        self.label == SIDEBAR || self.label == POPUP
    }
    fn cwd(&self) -> Option<&str> {
        let process = &self.process;
        process
            .foreground
            .as_ref()
            .or(process.child.as_ref())
            .and_then(|p| p.cwd.as_deref())
            .filter(|cwd| !cwd.is_empty())
    }
}

#[derive(Deserialize, Default)]
struct Process {
    child: Option<Proc>,
    foreground: Option<Proc>,
}

#[derive(Deserialize)]
struct Proc {
    pid: u32,
    #[serde(default)]
    name: String,
    argv0: Option<String>,
    cwd: Option<String>,
}

#[derive(Deserialize, Default)]
struct Title {
    #[serde(default)]
    title: String,
}

#[derive(Deserialize, Default)]
struct Size {
    #[serde(default)]
    columns: usize,
    #[serde(default)]
    rows: usize,
}

/// `session.view`, for the layout work the scan script does not report.
#[derive(Deserialize)]
struct View {
    #[serde(default)]
    active_window_id: Option<String>,
    #[serde(default, deserialize_with = "list")]
    windows: Vec<ViewWindow>,
}

#[derive(Deserialize)]
struct ViewWindow {
    window_id: String,
    #[serde(default)]
    focused_block_id: Option<String>,
    #[serde(default, deserialize_with = "list")]
    layers: Vec<Layer>,
}

#[derive(Deserialize)]
struct Layer {
    kind: String,
    /// {rect, block_id} or {rect, split: {split_id, ratio, before, after}}
    layout: Value,
    #[serde(default, deserialize_with = "list")]
    blocks: Vec<ViewBlock>,
}

#[derive(Deserialize)]
struct ViewBlock {
    block_id: String,
    creator_name: String,
    #[serde(default)]
    label: String,
}

impl ViewWindow {
    fn blocks(&self) -> impl Iterator<Item = &ViewBlock> {
        self.layers.iter().flat_map(|l| &l.blocks)
    }
    fn tiled(&self) -> Option<&Layer> {
        self.layers.iter().find(|l| l.kind == "tiled")
    }
    fn sidebar(&self) -> Option<&str> {
        self.blocks()
            .find(|b| b.label == SIDEBAR)
            .map(|b| b.block_id.as_str())
    }
    fn real_blocks(&self) -> usize {
        self.blocks()
            .filter(|b| b.creator_name == TERMINAL && b.label != SIDEBAR && b.label != POPUP)
            .count()
    }
}

/// `rex events --all --json`, read without blocking. The scan schedule still
/// polls: Rex reports titles, processes and layouts, not terminal output.
struct Events {
    child: Child,
    out: ChildStdout,
    partial: Vec<u8>,
}

impl Events {
    fn spawn() -> Option<Events> {
        let mut child = Command::new(rex_cli())
            .args(["--autostart=false", "events", "--all", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let out = child.stdout.take()?;
        let fd = out.as_raw_fd();
        // SAFETY: fcntl on a descriptor this process owns.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        Some(Events {
            child,
            out,
            partial: Vec::new(),
        })
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Fold one event line into the pending changes.
fn observe(pending: &mut PendingChanges, line: &[u8]) {
    let Ok(event) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    match event["type"].as_str() {
        Some("session_event") => {
            let inner = &event["payload"]["event"];
            match inner["type"].as_str() {
                // every rex CLI call, ours included, joins and leaves
                Some(kind) if kind.starts_with("client_") => {}
                Some("block_owner_changed" | "creator_changed") => {}
                Some("block_event") => {
                    let payload = &inner["payload"];
                    match payload["name"].as_str() {
                        Some("child_exited") => pending.layout = true,
                        Some(_) => {
                            if let Some(block) = payload["block_id"].as_str() {
                                pending.panes.insert(block.to_string());
                            }
                        }
                        None => {}
                    }
                }
                // layout, labels, focus: session_view_changed follows them all
                Some(_) => {
                    pending.layout = true;
                    pending.focus = true;
                }
                None => {}
            }
        }
        Some(kind) if kind.starts_with("client_") => {}
        Some(_) => pending.full = true,
        None => {}
    }
}

/// Rex: sessions hold windows, windows hold blocks; terminal blocks are the
/// panes. Every call forks the `rex` CLI, which honours REX_SERVER.
#[derive(Default)]
pub struct Rex {
    listing: Listing,
    /// block -> (session, window) from the last listing: block targets
    /// resolve only inside their session.
    places: HashMap<String, (String, String)>,
    monitoring: bool,
    events: Option<Events>,
    pending: PendingChanges,
    width: Width,
    started: Option<std::time::Instant>,
    /// Sessions seen holding a window: only those count as emptied.
    had_windows: std::collections::HashSet<String>,
    /// Windows holding a real block at the last scan; `None` before the first.
    known_windows: Option<std::collections::HashSet<String>>,
}

/// The running sidebar's share of its window, scan to scan.
#[derive(Default)]
struct Width {
    ratio: Option<f64>,
    /// Placement is done: from here a change of `ratio` is the user's.
    settled: bool,
}

impl Width {
    /// Fold in this scan's `ratio`; returns one to save. Placement moves the
    /// border until it reaches the saved width, and the app may reset it
    /// meanwhile: none of that is the user. Rex may also refuse the saved
    /// width (a narrower window), so `waited` settles it regardless.
    fn observe(&mut self, ratio: f64, saved: Option<f64>, waited: bool) -> Option<f64> {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
        let resized = self.settled && self.ratio.is_some_and(|before| !near(ratio, before));
        self.settled |= waited || saved.is_some_and(|saved| near(ratio, saved));
        self.ratio = Some(ratio);
        resized.then_some(ratio)
    }
}

impl Rex {
    pub fn connect(monitoring: bool) -> Result<Rex, TmuxError> {
        Self::run(&["whoami"])?;
        Ok(Rex {
            started: Some(std::time::Instant::now()),
            monitoring,
            events: monitoring.then(Events::spawn).flatten(),
            pending: PendingChanges {
                full: true,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    /// The rex CLI is the client: refuse to start a server just to scan it.
    fn run(args: &[&str]) -> Result<String, TmuxError> {
        let output = Command::new(rex_cli())
            .arg("--autostart=false")
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(TmuxError::Error(format!("rex: {}", err.trim_end())));
        }
        String::from_utf8(output.stdout).map_err(|e| TmuxError::Error(e.to_string()))
    }

    fn json<T: DeserializeOwned>(args: &[&str]) -> Result<T, TmuxError> {
        serde_json::from_str(&Self::run(args)?)
            .map_err(|e| TmuxError::Error(format!("rex {}: {e}", args[0])))
    }

    /// A server API method; `session` scopes it where the method needs one.
    fn call(session: Option<&str>, method: &str, request: Value) -> Result<Value, TmuxError> {
        let request = request.to_string();
        let mut args = vec!["api", "call"];
        if let Some(session) = session {
            args.extend(["-s", session]);
        }
        args.extend([method, request.as_str()]);
        let out = Self::run(&args)?;
        Ok(serde_json::from_str(&out).unwrap_or(Value::Null))
    }

    fn view(session: &str) -> Result<View, TmuxError> {
        Self::json(&["api", "call", "-s", session, "session.view"])
    }

    fn sessions() -> Result<Vec<String>, TmuxError> {
        let listed = Self::call(None, "session.list", json!({}))?;
        Ok(listed["sessions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["session_id"].as_str().map(str::to_string))
            .collect())
    }

    /// `block`'s session and window, looking past the last listing for blocks
    /// created since.
    fn locate(&self, block: &str) -> Option<(String, String)> {
        if let Some(place) = self.places.get(block) {
            return Some(place.clone());
        }
        Self::sessions().ok()?.into_iter().find_map(|session| {
            let view = Self::view(&session).ok()?;
            let window = view
                .windows
                .iter()
                .find(|w| w.blocks().any(|b| b.block_id == block))?;
            Some((session.clone(), window.window_id.clone()))
        })
    }

    fn focus_block(session: &str, window: &str, block: &str) {
        // focus moves the session's active window; the app shows another
        // session or window once its client is told to go there. `do` picks
        // the app client when exactly one is connected.
        let _ = Self::run(&["focus", "-s", session, block]);
        let _ = Self::run(&[
            "do",
            "session.select",
            &format!("session_id={session}"),
            &format!("window_id={window}"),
        ]);
    }

    fn block_cwd(&self, block: &str) -> Option<String> {
        self.listing
            .sessions
            .iter()
            .flat_map(|s| &s.windows)
            .flat_map(|w| &w.blocks)
            .find(|b| b.block_id == block)
            .and_then(Block::cwd)
            .map(str::to_string)
    }

    /// A resize of this sidebar (keys or mouse) becomes the size the next one
    /// opens at, however this one closes later. Only a change counts: every
    /// sidebar saving its unchanged width would undo the user's last resize.
    fn track_ratio(&mut self) {
        let Ok(own) = std::env::var("REX_BLOCK") else {
            return;
        };
        let ratio = self
            .listing
            .sessions
            .iter()
            .flat_map(|s| &s.windows)
            .map(|w| &w.layout["split"])
            .find(|split| split["before"]["block_id"].as_str() == Some(own.as_str()))
            .and_then(|split| split["ratio"].as_f64());
        let Some(ratio) = ratio else {
            return;
        };
        let waited = self
            .started
            .is_some_and(|at| at.elapsed() > std::time::Duration::from_secs(3));
        if let Some(ratio) = self.width.observe(ratio, saved_ratio(), waited) {
            save_ratio(ratio);
        }
    }

    /// Closing works as in tmux. A window whose last real block closed
    /// takes its sidebar with it, and a session whose last window closed is
    /// destroyed: Rex would keep it empty, and an empty session has no row
    /// to show. Undo still lists it. A session never seen with a window may
    /// be one the app is still filling, so it is left alone.
    fn reap(&mut self) {
        let own = std::env::var("REX_BLOCK").ok();
        for session in &self.listing.sessions {
            if session.windows.is_empty() {
                if self.had_windows.remove(&session.session_id) {
                    let request = json!({"session_id": session.session_id});
                    let _ = Self::call(None, "session.destroy", request);
                }
                continue;
            }
            self.had_windows.insert(session.session_id.clone());
            let Some(window) = session
                .windows
                .iter()
                .find(|w| w.blocks.iter().any(|b| Some(&b.block_id) == own.as_ref()))
            else {
                continue;
            };
            if window.others > 0 || window.blocks.iter().any(|b| !b.agenmux()) {
                continue;
            }
            // this sidebar is all its window holds: either call ends it
            if session.windows.len() == 1 {
                let request = json!({"session_id": session.session_id});
                let _ = Self::call(None, "session.destroy", request);
            } else {
                let request =
                    json!({"session_id": session.session_id, "window_id": window.window_id});
                let _ = Self::call(Some(&session.session_id), "session.close_window", request);
            }
        }
    }

    /// A window that appears while a sidebar is open gets one too, as tmux's
    /// `@agenmux-on` gives every new window one. Only windows new since the
    /// last scan: one whose sidebar was toggled off stays without. The sidebar
    /// with the lowest block id places them, so two never race for a window.
    fn spread(&mut self) {
        let real = |w: &Window| w.others > 0 || w.blocks.iter().any(|b| !b.agenmux());
        let windows = || {
            self.listing
                .sessions
                .iter()
                .flat_map(|s| s.windows.iter().map(move |w| (s, w)))
        };
        let now = windows()
            .filter(|(_, w)| real(w))
            .map(|(_, w)| w.window_id.clone())
            .collect();
        let Some(known) = self.known_windows.replace(now) else {
            return;
        };
        let leader = windows()
            .flat_map(|(_, w)| &w.blocks)
            .filter(|b| b.label == SIDEBAR)
            .map(|b| b.block_id.as_str())
            .min();
        if leader.is_none() || leader != std::env::var("REX_BLOCK").ok().as_deref() {
            return;
        }
        let ratio = saved_ratio().unwrap_or(DEFAULT_RATIO);
        for (session, window) in windows() {
            if !real(window)
                || known.contains(&window.window_id)
                || window.blocks.iter().any(|b| b.label == SIDEBAR)
            {
                continue;
            }
            let Ok(view) = Self::view(&session.session_id) else {
                continue;
            };
            if let Some(window) = view.windows.iter().find(|w| w.window_id == window.window_id) {
                if window.sidebar().is_none() {
                    let _ = add_sidebar(&session.session_id, window, ratio);
                }
            }
        }
    }

    fn new_window(session: &str, label: &str, block: Value) -> Result<String, TmuxError> {
        let mut request = json!({"session_id": session, "focus": false, "layout": block});
        if !label.is_empty() {
            request["window_label"] = label.into();
        }
        let out = Self::call(Some(session), "session.new_window", request)?;
        first_block(&out["block_ids"])
    }
}

/// Type the agent's resume command into its restored shell.
fn resume_agent(
    session: &str,
    block: &str,
    pane: &crate::snapshot::Pane,
    resume: &dyn Fn(&str) -> Option<String>,
) {
    if let Some(command) = pane.agent.as_deref().and_then(resume) {
        let _ = Rex::run(&["send", "-s", session, "-b", block, &format!("{command}\r")]);
    }
}

fn first_block(ids: &Value) -> Result<String, TmuxError> {
    ids[0]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| TmuxError::Error("rex created no block".into()))
}

/// A terminal block in `cwd` running the user's shell, or `argv` through it.
fn terminal(cwd: &str, label: &str, argv: Option<&[&str]>) -> Value {
    let mut options = json!({"cwd": cwd});
    if let Some(argv) = argv {
        options["command"] = json!(argv);
    }
    json!({"block": {"flavor": SHELL, "label": label, "options": options}})
}

/// `$XDG_STATE_HOME/agenmux/<name>`.
fn state_file(name: &str) -> PathBuf {
    crate::diag::daemon_log_path().with_file_name(name)
}

/// OSC 52 wants standard base64.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize])
            } else {
                '='
            });
        }
    }
    out
}

impl Multiplexer for Rex {
    fn panes(&mut self, self_pane: Option<&str>) -> Result<Vec<ParsedPane>, TmuxError> {
        if self.monitoring && self.events.is_none() {
            self.events = Events::spawn();
        }
        self.listing = Self::json(&["do", "-e", SCAN, "--json"])?;
        if self.monitoring && self.embedded() {
            self.track_ratio();
            self.reap();
            self.spread();
        }
        self.places.clear();
        let mut panes = Vec::new();
        for session in &self.listing.sessions {
            for (window_index, window) in session.windows.iter().enumerate() {
                let terminals = window.blocks.iter().filter(|b| !b.agenmux());
                for (pane_index, block) in terminals.enumerate() {
                    self.places.insert(
                        block.block_id.clone(),
                        (session.session_id.clone(), window.window_id.clone()),
                    );
                    if self_pane == Some(block.block_id.as_str()) {
                        continue;
                    }
                    let fg = block.process.foreground.as_ref();
                    // detection reads the program's title; a label the user
                    // set shows where the program sets none
                    let custom = !block.label.is_empty() && block.label != "shell";
                    let title = if block.title.title.is_empty() && custom {
                        block.label.clone()
                    } else {
                        block.title.title.clone()
                    };
                    panes.push(ParsedPane {
                        meta: crate::scan::PaneMeta {
                            pane: block.block_id.clone(),
                            pane_index: pane_index as u32 + 1,
                            pane_title: title,
                            // argv0 is the name the user typed ("claude"),
                            // not the versioned binary name
                            command: fg
                                .map(|p| {
                                    let bin = p.argv0.as_deref().unwrap_or(&p.name);
                                    bin.trim_start_matches('-').to_string()
                                })
                                .unwrap_or_default(),
                            path: block.cwd().unwrap_or_default().to_string(),
                            window_id: window.window_id.clone(),
                            window_index: window_index as u32 + 1,
                            window_name: window.label.clone(),
                            session_id: session.session_id.clone(),
                            session_name: session.label.clone(),
                            agent_index: None,
                        },
                        // the block's own process: identify() walks below it
                        pid: block.process.child.as_ref().or(fg).map_or(0, |p| p.pid),
                        width: block.size.columns,
                        height: block.size.rows,
                    });
                }
            }
        }
        Ok(panes)
    }

    fn capture(&mut self, pane: &str) -> Result<String, TmuxError> {
        let (session, _) = self.locate(pane).unwrap_or_default();
        let rows = self
            .listing
            .sessions
            .iter()
            .flat_map(|s| &s.windows)
            .flat_map(|w| &w.blocks)
            .find(|b| b.block_id == pane)
            .map_or(0, |b| b.size.rows);
        let text = Self::run(&["capture", "-s", &session, "-b", pane])?;
        // capture prints scrollback + screen; the last `rows` lines are the
        // screen, which is all tmux's capture-pane returns
        let lines: Vec<&str> = text.lines().collect();
        let screen = &lines[lines.len().saturating_sub(rows)..];
        Ok(screen.iter().map(|l| format!("{l}\n")).collect())
    }

    /// Rex clients do not say which session they show. The sidebar lives in
    /// one: what that session's active window focuses is what the user acts in.
    fn client_focus(&mut self) -> Option<crate::focus::ClientFocus> {
        let own = std::env::var("REX_SESSION").ok();
        let session = self
            .listing
            .sessions
            .iter()
            .find(|s| Some(&s.session_id) == own.as_ref())
            .or(self.listing.sessions.first())?;
        let window = session
            .windows
            .iter()
            .find(|w| Some(&w.window_id) == session.active_window_id.as_ref())?;
        let block = window.focused_block_id.as_deref()?;
        let sidebar = window
            .blocks
            .iter()
            .any(|b| b.block_id == block && b.agenmux());
        let title = if sidebar { SIDEBAR } else { "" };
        let row = format!("1|rex|{}|{block}|{title}|attached\n", session.session_id);
        Some(crate::focus::parse_clients(&row, false))
    }

    fn focus(&mut self, pane: &str) -> bool {
        let Some((session, window)) = self.locate(pane) else {
            return false;
        };
        Self::focus_block(&session, &window, pane);
        true
    }

    fn embedded(&self) -> bool {
        std::env::var_os("AGENMUX_REX_POPUP").is_none()
    }

    fn notify_fd(&self) -> libc::c_int {
        self.events.as_ref().map_or(-1, |e| e.out.as_raw_fd())
    }

    fn drain_notifications(&mut self) -> Result<bool, TmuxError> {
        let focus_before = self.pending.focus;
        let Some(events) = self.events.as_mut() else {
            return Ok(false);
        };
        let mut buf = [0u8; 8192];
        let mut gone = false;
        loop {
            match events.out.read(&mut buf) {
                Ok(0) => {
                    gone = true;
                    break;
                }
                Ok(n) => events.partial.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break, // WouldBlock: drained
            }
        }
        while let Some(end) = events.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = events.partial.drain(..=end).collect();
            observe(&mut self.pending, &line);
        }
        if gone {
            // respawned by the next scan; polling covers the gap
            self.events = None;
            self.pending.full = true;
        }
        Ok(!focus_before && self.pending.focus)
    }

    fn pending_changes(&self) -> Option<&PendingChanges> {
        Some(&self.pending)
    }
    fn take_pending_changes(&mut self) -> PendingChanges {
        std::mem::take(&mut self.pending)
    }
    fn defer_output(&mut self, panes: std::collections::HashSet<String>) {
        self.pending.panes.extend(panes);
    }

    fn is_live(&mut self, kind: Option<Kind>, ids: Ids) -> bool {
        if kind == Some(Kind::Session) {
            return Self::sessions().is_ok_and(|s| s.iter().any(|id| id == ids.session));
        }
        let Ok(view) = Self::view(ids.session) else {
            return false;
        };
        let has_block = |w: &ViewWindow| w.blocks().any(|b| b.block_id == ids.pane);
        match kind {
            Some(Kind::Window) => view.windows.iter().any(|w| w.window_id == ids.window),
            Some(Kind::Pane) => view.windows.iter().any(has_block),
            _ => view
                .windows
                .iter()
                .any(|w| w.window_id == ids.window && has_block(w)),
        }
    }

    fn create_window(&mut self, session: &str, name: &str, cwd: &str) -> Result<String, TmuxError> {
        Self::new_window(session, name, terminal(cwd, "shell", None))
    }

    fn create_session(&mut self, name: &str, cwd: &str) -> Result<String, TmuxError> {
        let mut request = json!({"initial_windows": [{"layout": terminal(cwd, "shell", None)}]});
        if !name.is_empty() {
            request["label"] = name.into();
        }
        let out = Self::call(None, "session.create", request)?;
        first_block(&out["initial_windows"][0]["block_ids"])
    }

    fn launch(&mut self, session: &str, cwd: &str, argv: &[&str]) -> Result<String, TmuxError> {
        let label = argv.first().copied().unwrap_or("shell");
        Self::new_window(session, "", terminal(cwd, label, Some(argv)))
    }

    fn name(&mut self, kind: Kind, ids: Ids) -> Result<String, TmuxError> {
        let session = self
            .listing
            .sessions
            .iter()
            .find(|s| s.session_id == ids.session);
        let window = session.and_then(|s| s.windows.iter().find(|w| w.window_id == ids.window));
        let name = match kind {
            Kind::Session => session.map(|s| s.label.clone()),
            Kind::Window => window.map(|w| w.label.clone()),
            Kind::Pane => window
                .and_then(|w| w.blocks.iter().find(|b| b.block_id == ids.pane))
                .map(|b| {
                    if b.title.title.is_empty() {
                        b.label.clone()
                    } else {
                        b.title.title.clone()
                    }
                }),
        };
        name.ok_or_else(|| TmuxError::Error("target changed; nothing was modified".into()))
    }

    fn rename(&mut self, kind: Kind, ids: Ids, name: &str) -> Result<(), TmuxError> {
        let (method, request) = match kind {
            Kind::Session => (
                "session.set_label",
                json!({"session_id": ids.session, "label": name}),
            ),
            Kind::Window => (
                "session.set_window_label",
                json!({"session_id": ids.session, "window_id": ids.window, "label": name}),
            ),
            Kind::Pane => (
                "session.set_block_label",
                json!({"session_id": ids.session, "block_id": ids.pane, "label": name}),
            ),
        };
        Self::call(Some(ids.session), method, request).map(drop)
    }

    fn delete(&mut self, kind: Kind, ids: Ids) -> Result<(), TmuxError> {
        match kind {
            Kind::Session => {
                if Self::sessions()?.len() <= 1 {
                    return Err(TmuxError::Error(
                        "cannot delete the last Rex session; nothing was modified".into(),
                    ));
                }
                Self::call(None, "session.destroy", json!({"session_id": ids.session}))
            }
            Kind::Window => {
                if Self::view(ids.session)?.windows.len() <= 1 {
                    return Err(TmuxError::Error(
                        "cannot delete the last window in a Rex session; delete the session explicitly"
                            .into(),
                    ));
                }
                let request = json!({"session_id": ids.session, "window_id": ids.window});
                Self::call(Some(ids.session), "session.close_window", request)
            }
            Kind::Pane => {
                let view = Self::view(ids.session)?;
                if view.windows.iter().map(ViewWindow::real_blocks).sum::<usize>() <= 1 {
                    return Err(TmuxError::Error(
                        "cannot delete the last pane in a Rex session; delete the session explicitly"
                            .into(),
                    ));
                }
                let request = json!({"session_id": ids.session, "block_id": ids.pane});
                Self::call(Some(ids.session), "block.close", request)
            }
        }
        .map(drop)
    }

    fn pane_cwd(&mut self, ids: Ids) -> Result<String, TmuxError> {
        if !self.is_live(None, ids) {
            return Err(TmuxError::Error(
                "selected pane changed before launcher start".into(),
            ));
        }
        self.block_cwd(ids.pane)
            .ok_or_else(|| TmuxError::Error("selected pane has no working directory".into()))
    }

    /// Rex sessions have no directory of their own: the first pane's stands in.
    fn session_cwd(&mut self, session: &str) -> Result<String, TmuxError> {
        self.listing
            .sessions
            .iter()
            .filter(|s| s.session_id == session)
            .flat_map(|s| &s.windows)
            .flat_map(|w| &w.blocks)
            .filter(|b| !b.agenmux())
            .find_map(Block::cwd)
            .map(str::to_string)
            .or_else(|| std::env::var("HOME").ok())
            .ok_or_else(|| TmuxError::Error("target session has no working directory".into()))
    }

    fn show(&mut self, _client: &str, pane: &str, stay: bool) -> Result<(), TmuxError> {
        let (session, window) = self
            .locate(pane)
            .ok_or_else(|| TmuxError::Error("created target has no Rex location".into()))?;
        Self::focus_block(&session, &window, pane);
        if stay {
            self.focus_sidebar("", Some(&window));
        }
        Ok(())
    }

    fn focus_sidebar(&mut self, _client: &str, expected_window: Option<&str>) {
        let Some(session) = std::env::var("REX_SESSION").ok() else {
            return;
        };
        let Ok(view) = Self::view(&session) else {
            return;
        };
        let window = expected_window
            .map(str::to_string)
            .or(view.active_window_id.clone());
        let Some(window) = view
            .windows
            .iter()
            .find(|w| Some(&w.window_id) == window.as_ref())
        else {
            return;
        };
        if let Some(sidebar) = window.sidebar() {
            if window.focused_block_id.as_deref() != Some(sidebar) {
                let _ = Self::run(&["focus", "-s", &session, sidebar]);
            }
        }
    }

    fn message(&mut self, _client: &str, _text: &str) -> bool {
        false
    }

    /// The sidebar's own terminal carries the clipboard escape to the app.
    fn copy(&mut self, _client: Option<&str>, text: &str) -> Result<(), TmuxError> {
        use std::io::Write;
        let mut out = std::io::stdout();
        write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
        out.flush()?;
        Ok(())
    }

    fn reference(&self, id: &str) -> String {
        let kind = id.split(':').next().unwrap_or("block");
        format!("rex {kind} {id}")
    }

    /// The server's instance id changes when it restarts; one file per host.
    fn snapshot_home(&mut self) -> Option<(String, PathBuf)> {
        let status = Self::call(None, "server.status", json!({})).ok()?;
        let server = status["instance_id"].as_str()?.to_string();
        let host = status["host_id"].as_str().unwrap_or("local");
        Some((
            server,
            crate::snapshot::state_file(&format!("snapshot-rex-{host}"))?,
        ))
    }

    /// Rows shaped like `snapshot::WINDOWS_FMT`: the block count includes the
    /// sidebar, so `layout` sees it; a label Rex made up follows the window
    /// the way tmux's automatic-rename does.
    fn windows(&mut self) -> Result<String, TmuxError> {
        let mut rows = String::new();
        for window in self.listing.sessions.iter().flat_map(|s| &s.windows) {
            let automatic = window.label.starts_with("Window ");
            rows.push_str(&format!(
                "{}|{}|{}|0|{}|{}\n",
                window.window_id,
                window.blocks.len(),
                u8::from(automatic),
                window.layout,
                window.label,
            ));
        }
        Ok(rows)
    }

    fn open_panes(&mut self) -> String {
        let blocks = self.listing.sessions.iter().flat_map(|s| &s.windows);
        let ids: Vec<&str> = blocks
            .flat_map(|w| &w.blocks)
            .map(|b| b.block_id.as_str())
            .collect();
        ids.join(" ")
    }

    fn live(&mut self) -> Live {
        let mut live = Live::default();
        let Ok(listing) = Self::json::<Listing>(&["do", "-e", SCAN, "--json"]) else {
            return live;
        };
        for session in &listing.sessions {
            for (index, window) in session.windows.iter().enumerate() {
                live.windows
                    .insert((session.label.clone(), index as u32 + 1));
                live.ids.insert(window.window_id.clone());
                live.ids
                    .extend(window.blocks.iter().map(|b| b.block_id.clone()));
            }
        }
        live
    }

    /// Rex windows have no fixed index: a missing one is appended. Panes come
    /// back side by side; Rex takes no saved layout string.
    fn replay(&mut self, snap: &Snapshot, live: &Live, resume: &dyn Fn(&str) -> Option<String>) {
        let labels: Vec<(String, String)> = Self::call(None, "session.list", json!({}))
            .ok()
            .and_then(|listed| {
                Some(
                    listed["sessions"]
                        .as_array()?
                        .iter()
                        .filter_map(|s| {
                            Some((
                                s["label"].as_str()?.into(),
                                s["session_id"].as_str()?.into(),
                            ))
                        })
                        .collect(),
                )
            })
            .unwrap_or_default();
        for session in &snap.sessions {
            let mut id = labels
                .iter()
                .find(|(label, _)| *label == session.name)
                .map(|(_, id)| id.clone());
            for window in &session.windows {
                let Some(first) = window.panes.first() else {
                    continue;
                };
                if live.windows.contains(&(session.name.clone(), window.index)) {
                    continue;
                }
                let label = if window.automatic_rename {
                    ""
                } else {
                    window.name.as_str()
                };
                let created = match &id {
                    Some(id) => Self::new_window(id, label, terminal(&first.path, "shell", None))
                        .map(|block| (id.clone(), block)),
                    None => {
                        let mut first_window =
                            json!({"layout": terminal(&first.path, "shell", None)});
                        if !label.is_empty() {
                            first_window["window_label"] = label.into();
                        }
                        let request =
                            json!({"label": session.name, "initial_windows": [first_window]});
                        Self::call(None, "session.create", request).and_then(|out| {
                            let session =
                                out["session_id"].as_str().unwrap_or_default().to_string();
                            Ok((
                                session,
                                first_block(&out["initial_windows"][0]["block_ids"])?,
                            ))
                        })
                    }
                };
                let Ok((session_id, block)) = created else {
                    continue;
                };
                id = Some(session_id.clone());
                let mut restored = vec![(block.clone(), first)];
                let mut last = block;
                for pane in &window.panes[1..] {
                    let request = json!({
                        "session_id": session_id,
                        "anchor_id": last,
                        "direction": "horizontal",
                        "side": "after",
                        "focus": false,
                        "layout": terminal(&pane.path, "shell", None),
                    });
                    let Ok(out) = Self::call(Some(&session_id), "session.new_split", request)
                    else {
                        break;
                    };
                    let Ok(block) = first_block(&out["block_ids"]) else {
                        break;
                    };
                    restored.push((block.clone(), pane));
                    last = block;
                }
                for (block, pane) in restored {
                    resume_agent(&session_id, &block, pane, resume);
                }
            }
        }
    }

    fn replay_pane(
        &mut self,
        entry: &Closed,
        live: &Live,
        resume: &dyn Fn(&str) -> Option<String>,
    ) {
        let (Some(window), Some(pane)) = (entry.session.windows.first(), entry.pane()) else {
            return;
        };
        let at = window
            .panes
            .iter()
            .position(|p| p.id == pane.id)
            .unwrap_or(0);
        let open = |p: &&crate::snapshot::Pane| live.ids.contains(&p.id);
        let (side, neighbour) = match window.panes[..at].iter().rev().find(open) {
            Some(prev) => ("after", prev.id.as_str()),
            None => match window.panes[at + 1..].iter().find(open) {
                Some(next) => ("before", next.id.as_str()),
                None => return,
            },
        };
        let Some((session, _)) = self.locate(neighbour) else {
            return;
        };
        let request = json!({
            "session_id": session,
            "anchor_id": neighbour,
            "direction": "horizontal",
            "side": side,
            "focus": false,
            "layout": terminal(&pane.path, "shell", None),
        });
        if let Ok(block) = Self::call(Some(&session), "session.new_split", request)
            .and_then(|out| first_block(&out["block_ids"]))
        {
            resume_agent(&session, &block, pane, resume);
        }
    }

    fn reload_token(&mut self) -> Option<String> {
        std::fs::read_to_string(state_file("reload-token"))
            .ok()
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
    }

    fn publish_reload(&mut self, token: &str) -> Result<(), TmuxError> {
        publish_reload(token)
    }

    fn options(&mut self) -> Result<BTreeMap<String, String>, crate::app_config::ConfigError> {
        Ok(BTreeMap::new())
    }
}

/// `config reload` outside tmux: the token sidebars watch is a state file.
pub fn publish_reload(token: &str) -> Result<(), TmuxError> {
    let path = state_file("reload-token");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, token)?;
    Ok(())
}

/// Saved sidebar share: a sidebar opens at the width the last one closed
/// with, however it was resized (keys or mouse).
fn saved_ratio() -> Option<f64> {
    std::fs::read_to_string(state_file("rex-sidebar-ratio"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn save_ratio(ratio: f64) {
    let path = state_file("rex-sidebar-ratio");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // {:?} round-trips, so an untouched live ratio compares equal to it
    let _ = std::fs::write(path, format!("{ratio:?}\n"));
}

/// The sidebar block: this binary's `sidebar`, labelled so scans skip it.
fn sidebar_block(label: &str, popup: bool) -> Result<Value, TmuxError> {
    let exe = std::env::current_exe()?.display().to_string();
    let mut command = vec![exe, "sidebar".to_string()];
    if popup {
        command.splice(0..0, ["env".to_string(), "AGENMUX_REX_POPUP=1".to_string()]);
    }
    Ok(json!({"block": {
        "flavor": SHELL,
        "label": label,
        "options": {"shell": "none", "command": command},
    }}))
}

/// Split the whole tiled layer (its top split, or its only block) so the
/// sidebar spans the window's full height.
fn add_sidebar(session: &str, window: &ViewWindow, ratio: f64) -> Result<(), TmuxError> {
    let layout = &window
        .tiled()
        .ok_or_else(|| TmuxError::Error("window has no tiled layer".into()))?
        .layout;
    let anchor = layout["split"]["split_id"]
        .as_str()
        .or(layout["block_id"].as_str())
        .ok_or_else(|| TmuxError::Error("empty layout".into()))?;
    // the border is set against the leftmost block the sidebar sits beside
    let mut right = layout;
    while !right["split"].is_null() {
        right = &right["split"]["before"];
    }
    let exe = std::env::current_exe()?.display().to_string();
    Rex::run(&[
        "do",
        "-e",
        PLACE,
        &format!("session={session}"),
        &format!("anchor={anchor}"),
        &format!("right={}", right["block_id"].as_str().unwrap_or_default()),
        &format!("ratio={ratio}"),
        &format!("label={SIDEBAR}"),
        &format!("exe={exe}"),
    ])
    .map(drop)
}

/// `rex-toggle <session> <block> [split|popup]`, bound in Rex by
/// `rex/agenmux.lua`: open or close the sidebar in `block`'s window, or show
/// the popup over it.
pub fn rex_toggle(session: &str, block: &str, mode: &str) -> i32 {
    let Ok(view) = Rex::view(session) else {
        return 1;
    };
    let Some(window) = view
        .windows
        .iter()
        .find(|w| w.blocks().any(|b| b.block_id == block))
    else {
        return 1;
    };
    let result = if mode == "popup" {
        let request = sidebar_block(POPUP, true).map(|layout| {
            json!({
                "session_id": session,
                "window_id": window.window_id,
                "bounds": [0.2, 0.1, 0.6, 0.8],
                "focus": true,
                "layout": layout,
            })
        });
        request.and_then(|r| Rex::call(Some(session), "session.new_layer", r).map(drop))
    } else {
        match window.sidebar() {
            // the running sidebar already saved any resize of it
            Some(sidebar) => {
                let request = json!({"session_id": session, "block_id": sidebar});
                Rex::call(Some(session), "block.close", request).map(drop)
            }
            None => add_sidebar(session, window, saved_ratio().unwrap_or(DEFAULT_RATIO)),
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agenmux: {e}");
            1
        }
    }
}

/// Keep a sidebar block at the left of every Rex window, as the tmux
/// integration does for every tmux window. Opt-in: Rex's default is the
/// alt+s toggle. Reconciles on every layout event; returns once the server
/// is gone.
pub fn rex_sidebars() -> i32 {
    let Some(mut events) = Events::spawn() else {
        return 1;
    };
    // blocking reads: this process does nothing between events
    let fd = events.out.as_raw_fd();
    // SAFETY: fcntl on a descriptor this process owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
    }
    let mut buf = [0u8; 8192];
    loop {
        let Ok(sessions) = Rex::sessions() else {
            return 0;
        };
        for session in sessions {
            let Ok(view) = Rex::view(&session) else {
                continue;
            };
            let ratio = saved_ratio().unwrap_or(DEFAULT_RATIO);
            for window in &view.windows {
                if window.real_blocks() == 0 {
                    // the window's last real block closed: the sidebar goes too
                    let request = json!({"session_id": session, "window_id": window.window_id});
                    let _ = Rex::call(Some(&session), "session.close_window", request);
                } else if window.sidebar().is_none() {
                    if let Err(e) = add_sidebar(&session, window, ratio) {
                        eprintln!("agenmux: {}: {e}", window.window_id);
                    }
                }
            }
        }
        let mut pending = PendingChanges::default();
        while !pending.layout && !pending.full {
            match events.out.read(&mut buf) {
                Ok(0) | Err(_) => return 0,
                Ok(n) => events.partial.extend_from_slice(&buf[..n]),
            }
            while let Some(end) = events.partial.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = events.partial.drain(..=end).collect();
                observe(&mut pending, &line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_saves_only_user_resizes_after_placement() {
        let mut width = Width::default();
        // the app's half split, then placement reaching the saved 0.2
        assert_eq!(width.observe(0.5, Some(0.2), false), None);
        assert_eq!(width.observe(0.2, Some(0.2), false), None);
        assert_eq!(width.observe(0.2, Some(0.2), false), None);
        // a resize, and only the change itself
        assert_eq!(width.observe(0.26, Some(0.2), false), Some(0.26));
        assert_eq!(width.observe(0.26, Some(0.26), false), None);

        // a width Rex refused never matches: settles once waited
        let mut width = Width::default();
        assert_eq!(width.observe(0.31, Some(0.2), false), None);
        assert_eq!(width.observe(0.31, Some(0.2), true), None);
        assert_eq!(width.observe(0.25, Some(0.2), true), Some(0.25));
    }

    #[test]
    fn base64_pads_like_the_standard_alphabet() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"rex block block:1"), "cmV4IGJsb2NrIGJsb2NrOjE=");
    }

    #[test]
    fn events_mark_layout_titles_and_ignore_client_churn() {
        let mut pending = PendingChanges::default();
        let lines = [
            r#"{"type":"client_joined","payload":{}}"#,
            r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"client_connected","payload":{}}}}"#,
        ];
        for line in lines {
            observe(&mut pending, line.as_bytes());
        }
        assert!(!pending.layout && !pending.focus && !pending.full && pending.panes.is_empty());

        observe(
            &mut pending,
            br#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"block_event","payload":{"block_id":"block:7","creator_name":"com.superlogical.terminal","name":"title_changed","payload":{}}}}}"#,
        );
        assert!(pending.panes.contains("block:7"));
        assert!(!pending.layout);

        observe(
            &mut pending,
            br#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"session_view_changed","payload":{"revision":3}}}}"#,
        );
        assert!(pending.layout && pending.focus);

        observe(&mut pending, br#"{"type":"session_created","payload":{}}"#);
        assert!(pending.full);
    }

    #[test]
    fn listing_accepts_lua_empty_tables() {
        let listing: Listing = serde_json::from_str(
            r#"{"sessions":[{"session_id":"session:1","label":"work","windows":[{"window_id":"window:1","label":"w","blocks":{}}]}]}"#,
        )
        .unwrap();
        assert!(listing.sessions[0].windows[0].blocks.is_empty());
        let empty: Listing = serde_json::from_str(r#"{"sessions":{}}"#).unwrap();
        assert!(empty.sessions.is_empty());
    }

    #[test]
    fn references_name_rex_records() {
        let rex = Rex::default();
        assert_eq!(rex.reference("session:1"), "rex session session:1");
        assert_eq!(rex.reference("window:2"), "rex window window:2");
        assert_eq!(rex.reference("block:3"), "rex block block:3");
    }
}
