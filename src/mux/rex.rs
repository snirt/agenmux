use super::{Ids, Kind, Multiplexer};
use crate::app_config::{AppConfig, DisplayMode, PopupHeight, SidebarPosition};
use crate::scan::ParsedPane;
use crate::snapshot::{Closed, Live, Snapshot};
use crate::tmux::{PendingChanges, TmuxError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const TERMINAL: &str = "com.superlogical.terminal";
const SHELL: &str = "com.superlogical.terminal.shell";
/// Label of the sidebar blocks agenmux places; scans skip them like tmux's
/// own sidebar panes.
const SIDEBAR: &str = "agenmux";
/// The floating popup's block: a sidebar that closes after one jump.
const POPUP: &str = "agenmux-popup";
const SCAN: &str = include_str!("rex_scan.lua");
const PLACE: &str = include_str!("rex_place.lua");
const POPUP_LAYER: &str = include_str!("rex_popup.lua");
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

/// A Rex server answers, without starting one.
pub(super) fn running() -> bool {
    Command::new(rex_cli())
        .args(["--autostart=false", "whoami"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The shell of the block this process says it runs in (`REX_BLOCK`).
pub(super) fn block_pid() -> Option<u32> {
    let session = std::env::var("REX_SESSION").ok()?;
    let block = std::env::var("REX_BLOCK").ok()?;
    let request = json!({"session_id": session, "block_id": block}).to_string();
    let method = format!("{TERMINAL}.process");
    let out = Rex::run(&["api", "call", "-s", &session, &method, &request]).ok()?;
    let process: Process = serde_json::from_str(&out).ok()?;
    process.child.map(|child| child.pid)
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
/// `own`: agenmux's sidebar and popup blocks. Their events change nothing a
/// sidebar shows, and a popup opening sends a burst of them to every sidebar.
fn observe(pending: &mut PendingChanges, line: &[u8], own: &HashSet<String>) {
    let Ok(event) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    let block = event["payload"]["event"]["payload"]["block_id"].as_str();
    if block.is_some_and(|block| own.contains(block)) {
        return;
    }
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
    /// The split sidebar that speaks for all of them; see `elect`.
    leader: bool,
    /// No split sidebar runs: a popup is all there is.
    alone: bool,
    /// This sidebar's window is its session's active one.
    shown: bool,
    /// agenmux's own blocks at the last scan, whose events `observe` skips.
    own_blocks: HashSet<String>,
}

/// The running sidebar's share of its window, scan to scan.
#[derive(Default)]
struct Width {
    ratio: Option<f64>,
    /// Placement is done: from here a change of `ratio` is the user's.
    settled: bool,
    /// The window's blocks at the last scan.
    blocks: Vec<String>,
    /// When `blocks` last changed. The app evens out the splits of a window
    /// it shows a moment after a pane opens or closes, often a scan later.
    reshaped: Option<Instant>,
    /// The saved share at the last scan.
    seen: Option<f64>,
}

/// How long after a pane opens or closes a change of share is still the
/// app's evening out, not the user.
const RESHAPE: Duration = Duration::from_secs(1);

#[derive(Debug, PartialEq)]
enum WidthChange {
    /// The user resized the sidebar: the next one opens this wide.
    Save(f64),
    /// A pane opened or closed and the sidebar's share went with it, as when
    /// a pane split beside the sidebar and its neighbour closes: back to this.
    Fit(f64),
}

impl Width {
    /// Fold in this scan's share of the window. Placement moves the border
    /// until it reaches the saved width, and the app may reset it meanwhile:
    /// none of that is the user. Rex may also refuse the saved width (a
    /// narrower window), so `waited` settles it regardless. A change within
    /// `RESHAPE` of the blocks changing is the layout reshaping; later, a resize.
    /// A change to the saved share, or with it, is another sidebar's resize
    /// reaching this one: saving it back would undo a narrower window's refusal.
    fn observe(
        &mut self,
        ratio: f64,
        saved: Option<f64>,
        waited: bool,
        blocks: Vec<String>,
        now: Instant,
    ) -> Option<WidthChange> {
        let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
        if self.blocks != blocks {
            self.reshaped = Some(now);
        }
        self.blocks = blocks;
        let reshaping = self.reshaped.is_some_and(|at| now - at < RESHAPE);
        let synced = std::mem::replace(&mut self.seen, saved) != saved
            || saved.is_some_and(|saved| near(ratio, saved));
        let moved = self.settled && self.ratio.is_some_and(|before| !near(ratio, before));
        self.settled |= waited || saved.is_some_and(|saved| near(ratio, saved));
        self.ratio = Some(ratio);
        match saved {
            _ if moved && !reshaping && !synced => {
                // this sidebar's own save: the next change is still the user's
                self.seen = Some(ratio);
                Some(WidthChange::Save(ratio))
            }
            Some(saved) if self.settled && reshaping && !near(ratio, saved) => {
                self.ratio = Some(saved);
                Some(WidthChange::Fit(saved))
            }
            _ => None,
        }
    }
}

/// Where the sidebar sits in a tiled layout: its share of the window, and
/// the border beside it when the split holding it is side by side.
#[derive(Debug, PartialEq)]
struct Seat {
    share: f64,
    border: Option<Border>,
}

#[derive(Debug, PartialEq)]
struct Border {
    before: String,
    after: String,
    sidebar_before: bool,
    /// The holding split's share of the window.
    span: f64,
}

/// Move the border beside a sidebar so it takes `share` of its window.
fn fit(session: &str, seat: &Seat, share: f64) {
    let Some(border) = &seat.border else {
        return;
    };
    let part = (share / border.span).clamp(0.05, 0.95);
    let request = json!({
        "session_id": session,
        "direction": "horizontal",
        "before_block_id": border.before,
        "after_block_id": border.after,
        "ratio": if border.sidebar_before { part } else { 1.0 - part },
    });
    let _ = Rex::call(Some(session), "session.resize_pane", request);
}

fn seat(node: &Value, own: &str) -> Option<Seat> {
    if node["block_id"].as_str() == Some(own) {
        let share = node["rect"][2].as_f64()?;
        return Some(Seat {
            share,
            border: None,
        });
    }
    let split = node.get("split")?;
    for (part, other, sidebar_before) in [
        (&split["before"], &split["after"], true),
        (&split["after"], &split["before"], false),
    ] {
        let Some(mut found) = seat(part, own) else {
            continue;
        };
        if part["block_id"].as_str() == Some(own) && split["direction"] == "horizontal" {
            // the other part's block on this border
            let mut beside = other;
            while let Some(split) = beside.get("split") {
                beside = &split[if sidebar_before { "before" } else { "after" }];
            }
            let beside = beside["block_id"].as_str()?.to_string();
            let (before, after) = if sidebar_before {
                (own.to_string(), beside)
            } else {
                (beside, own.to_string())
            };
            found.border = Some(Border {
                before,
                after,
                sidebar_before,
                span: node["rect"][2].as_f64()?,
            });
        }
        return Some(found);
    }
    None
}

impl Rex {
    pub fn connect(monitoring: bool) -> Result<Rex, TmuxError> {
        // a sidebar runs in a block, so the server is up: skip the round
        // trip before its first frame. The CLI checks, to fail plainly.
        if !monitoring {
            Self::run(&["whoami"])?;
        }
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

    /// A command for the app client this sidebar acts for, or, once that
    /// client is gone (the app restarted), for whichever one Rex picks: `do`
    /// picks the app client when exactly one is connected.
    fn on_client(args: &[&str]) -> bool {
        if let Some(client) = client() {
            let mut targeted = vec!["-C", client.as_str()];
            targeted.extend(args);
            if Self::run(&targeted).is_ok() {
                return true;
            }
        }
        Self::run(args).is_ok()
    }

    fn focus_block(session: &str, window: &str, block: &str) -> bool {
        // focus moves the session's active window; the app shows another
        // session or window once its client is told to go there
        Self::on_client(&["focus", "-s", session, block])
            && Self::on_client(&[
                "do",
                "session.select",
                &format!("session_id={session}"),
                &format!("window_id={window}"),
            ])
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

    /// A resize of this sidebar (keys or mouse) resizes the other open ones
    /// and becomes the size the next one opens at, however this one closes
    /// later. Only a change counts: every
    /// sidebar saving its unchanged width would undo the user's last resize.
    fn track_ratio(&mut self) {
        let Ok(own) = std::env::var("REX_BLOCK") else {
            return;
        };
        let found = self.listing.sessions.iter().find_map(|s| {
            let w = s
                .windows
                .iter()
                .find(|w| w.blocks.iter().any(|b| b.block_id == own))?;
            Some((s, w, seat(&w.layout, &own)?))
        });
        let Some((session, window, seat)) = found else {
            return;
        };
        let mut blocks: Vec<String> = window.blocks.iter().map(|b| b.block_id.clone()).collect();
        blocks.sort();
        blocks.push(window.others.to_string());
        let waited = self
            .started
            .is_some_and(|at| at.elapsed() > std::time::Duration::from_secs(3));
        match self
            .width
            .observe(seat.share, saved_ratio(), waited, blocks, Instant::now())
        {
            Some(WidthChange::Save(ratio)) => {
                save_ratio(ratio);
                // every open sidebar follows, as in tmux
                for s in &self.listing.sessions {
                    for w in &s.windows {
                        for b in w.blocks.iter().filter(|b| b.label == SIDEBAR) {
                            if b.block_id != own {
                                if let Some(other) = self::seat(&w.layout, &b.block_id) {
                                    fit(&s.session_id, &other, ratio);
                                }
                            }
                        }
                    }
                }
            }
            Some(WidthChange::Fit(saved)) => fit(&session.session_id, &seat, saved),
            None => {}
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

    /// The split sidebar with the lowest block id speaks for all of them: it
    /// places new windows' sidebars and sends the notifications, so two never
    /// race for a window or alert twice.
    fn elect(&mut self) {
        let own = std::env::var("REX_BLOCK").ok();
        let windows = || self.listing.sessions.iter().flat_map(|s| &s.windows);
        let first = windows()
            .flat_map(|w| &w.blocks)
            .filter(|b| b.label == SIDEBAR)
            .map(|b| &b.block_id)
            .min();
        self.leader = own.is_some() && first == own.as_ref();
        self.alone = first.is_none();
        self.shown = self.listing.sessions.iter().any(|s| {
            s.windows.iter().any(|w| {
                s.active_window_id.as_ref() == Some(&w.window_id)
                    && w.blocks.iter().any(|b| Some(&b.block_id) == own.as_ref())
            })
        });
    }

    /// A window that appears while a sidebar is open gets one too, as tmux's
    /// `@agenmux-on` gives every new window one. Only windows new since the
    /// last scan: one whose sidebar was toggled off stays without.
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
        if !self.leader {
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
            if let Some(window) = view
                .windows
                .iter()
                .find(|w| w.window_id == window.window_id)
            {
                if window.sidebar().is_none() {
                    let client = client();
                    let _ =
                        add_sidebar(&session.session_id, window, ratio, false, client.as_deref());
                }
            }
        }
    }

    fn new_window(session: &str, label: &str, block: Value) -> Result<String, TmuxError> {
        first_block(&Self::create_window_raw(session, label, block)?["block_ids"])
    }

    /// `session.new_window`: its `window_id` and `block_ids`.
    fn create_window_raw(session: &str, label: &str, layout: Value) -> Result<Value, TmuxError> {
        let mut request = json!({"session_id": session, "focus": false, "layout": layout});
        if !label.is_empty() {
            request["window_label"] = label.into();
        }
        Self::call(Some(session), "session.new_window", request)
    }

    /// Move a restored window back to its saved place in the tab order.
    fn place_window(session: &str, window: &str, index: u32) {
        let Ok(view) = Self::view(session) else {
            return;
        };
        let at = (index as usize).saturating_sub(1);
        if let Some(before) = view.windows.get(at).filter(|w| w.window_id != window) {
            let request = json!({
                "session_id": session,
                "window_id": window,
                "before_window_id": before.window_id,
            });
            let _ = Self::call(Some(session), "session.move_window", request);
        }
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
        self.own_blocks = self
            .listing
            .sessions
            .iter()
            .flat_map(|s| &s.windows)
            .flat_map(|w| &w.blocks)
            .filter(|b| b.agenmux())
            .map(|b| b.block_id.clone())
            .collect();
        self.elect();
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
        let mut block = window.focused_block_id.as_deref()?;
        // a popup has the focus it floats over: what the user acts on is the
        // pane it opened from, as under a tmux popup
        let over = std::env::var("AGENMUX_REX_OVER").ok();
        if std::env::var("REX_BLOCK").ok().as_deref() == Some(block) {
            if let Some(over) = over
                .as_deref()
                .filter(|over| window.blocks.iter().any(|b| b.block_id == *over))
            {
                block = over;
            }
        }
        let sidebar = window
            .blocks
            .iter()
            .any(|b| b.block_id == block && b.agenmux());
        let title = if sidebar { SIDEBAR } else { "" };
        let row = format!("1|rex|{}|{block}|{title}|attached\n", session.session_id);
        Some(crate::focus::parse_clients(&row, false))
    }

    fn focus(&mut self, id: &str) -> bool {
        // a session goes to its current window, a window to its focused pane
        let header = self.listing.sessions.iter().find_map(|s| {
            let window = if id == s.session_id {
                s.active_window_id.as_deref()?
            } else {
                id
            };
            let window = s.windows.iter().find(|w| w.window_id == window)?;
            let real = window.blocks.iter().filter(|b| !b.agenmux());
            let block = real
                .clone()
                .find(|b| window.focused_block_id.as_ref() == Some(&b.block_id))
                .or(real.clone().next())?;
            Some((
                s.session_id.clone(),
                window.window_id.clone(),
                block.block_id.clone(),
            ))
        });
        let target = header.or_else(|| {
            let (session, window) = self.locate(id)?;
            Some((session, window, id.to_string()))
        });
        target.is_some_and(|(session, window, block)| Self::focus_block(&session, &window, &block))
    }

    fn notifies(&self) -> bool {
        self.leader || self.alone
    }

    fn quiet(&self) -> bool {
        self.embedded() && !self.leader && !self.shown
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
            observe(&mut self.pending, &line, &self.own_blocks);
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

    /// `expected_window` may be in another session (`cs`), and a window just
    /// created gets its sidebar from the leader's next scan: wait for it
    /// briefly, then place one. With no window, this sidebar's own block.
    fn focus_sidebar(&mut self, _client: &str, expected_window: Option<&str>) {
        let Some(expected) = expected_window else {
            if let Some(own) = std::env::var("REX_BLOCK").ok() {
                if let Some((session, window)) = self.locate(&own) {
                    Self::focus_block(&session, &window, &own);
                }
            }
            return;
        };
        let deadline = Instant::now() + Duration::from_millis(1500);
        loop {
            let found = Self::sessions()
                .unwrap_or_default()
                .into_iter()
                .find_map(|session| {
                    let view = Self::view(&session).ok()?;
                    let window = view.windows.into_iter().find(|w| w.window_id == expected)?;
                    Some((session, window))
                });
            let Some((session, window)) = found else {
                return;
            };
            if let Some(sidebar) = window.sidebar() {
                if window.focused_block_id.as_deref() != Some(sidebar) {
                    Self::focus_block(&session, &window.window_id, sidebar);
                }
                return;
            }
            if Instant::now() >= deadline {
                if self.embedded() {
                    let ratio = saved_ratio().unwrap_or(DEFAULT_RATIO);
                    let _ = add_sidebar(&session, &window, ratio, true, client().as_deref());
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
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
        let mut live = Live {
            appends: true,
            ..Live::default()
        };
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

    /// Rex windows have no fixed index: a missing one is created at the end,
    /// then moved back to its saved place, with its saved splits and ratios.
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
                if window.panes.is_empty() || live.has_window(&session.name, window) {
                    continue;
                }
                let label = if window.automatic_rename {
                    ""
                } else {
                    window.name.as_str()
                };
                let (layout, panes) = window_layout(window);
                let created = match &id {
                    Some(id) => {
                        Self::create_window_raw(id, label, layout).map(|out| (id.clone(), out))
                    }
                    None => {
                        let mut first_window = json!({"layout": layout});
                        if !label.is_empty() {
                            first_window["window_label"] = label.into();
                        }
                        let request =
                            json!({"label": session.name, "initial_windows": [first_window]});
                        Self::call(None, "session.create", request).map(|out| {
                            let session =
                                out["session_id"].as_str().unwrap_or_default().to_string();
                            (session, out["initial_windows"][0].clone())
                        })
                    }
                };
                let Ok((session_id, out)) = created else {
                    continue;
                };
                id = Some(session_id.clone());
                if let Some(window_id) = out["window_id"].as_str() {
                    Self::place_window(&session_id, window_id, window.index);
                }
                let blocks = out["block_ids"].as_array().into_iter().flatten();
                for (block, pane) in blocks.filter_map(Value::as_str).zip(panes) {
                    resume_agent(&session_id, block, pane, resume);
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

/// The app client that opened this sidebar, from Rex's `ctx.client_id`:
/// navigation then reaches the window the user is in.
fn client() -> Option<String> {
    std::env::var("AGENMUX_REX_CLIENT")
        .ok()
        .filter(|client| !client.is_empty())
}

fn config() -> Option<AppConfig> {
    crate::app_config::load()
        .and_then(|file| crate::app_config::resolve(&file, &BTreeMap::new()))
        .ok()
}

/// A window's saved tiled layout as a creation tree, and its panes in the
/// order Rex creates their blocks. Blocks no pane records (the sidebar) drop
/// out; a layout that misses a pane puts them all side by side.
fn window_layout(window: &crate::snapshot::Window) -> (Value, Vec<&crate::snapshot::Pane>) {
    fn rebuild<'a>(
        node: &Value,
        panes: &'a [crate::snapshot::Pane],
        order: &mut Vec<&'a crate::snapshot::Pane>,
    ) -> Option<Value> {
        if let Some(split) = node.get("split") {
            let before = rebuild(&split["before"], panes, order);
            let after = rebuild(&split["after"], panes, order);
            let (Some(before), Some(after)) = (&before, &after) else {
                return before.or(after);
            };
            let mut tree =
                json!({"direction": split["direction"], "before": before, "after": after});
            if split["ratio"].is_number() {
                tree["ratio"] = split["ratio"].clone();
            }
            return Some(json!({ "split": tree }));
        }
        let pane = panes
            .iter()
            .find(|p| Some(p.id.as_str()) == node["block_id"].as_str())?;
        order.push(pane);
        Some(terminal(&pane.path, "shell", None))
    }
    let mut order = Vec::new();
    let saved = serde_json::from_str::<Value>(&window.layout)
        .ok()
        .and_then(|layout| rebuild(&layout, &window.panes, &mut order));
    if let Some(layout) = saved.filter(|_| order.len() == window.panes.len()) {
        return (layout, order);
    }
    let mut panes = window.panes.iter().rev();
    let last = panes
        .next()
        .map_or(Value::Null, |p| terminal(&p.path, "shell", None));
    let layout = panes.enumerate().fold(last, |after, (i, pane)| {
        // equal shares: the k-th pane from the right gets 1/k of its split
        json!({"split": {
            "direction": "horizontal",
            "ratio": 1.0 / (i as f64 + 2.0),
            "before": terminal(&pane.path, "shell", None),
            "after": after,
        }})
    });
    (layout, window.panes.iter().collect())
}

/// Split the whole tiled layer (its top split, or its only block) so the
/// sidebar spans the window's full height, at `display.sidebar_position`.
/// `focus`: the user opened it.
fn add_sidebar(
    session: &str,
    window: &ViewWindow,
    ratio: f64,
    focus: bool,
    client: Option<&str>,
) -> Result<(), TmuxError> {
    let layout = &window
        .tiled()
        .ok_or_else(|| TmuxError::Error("window has no tiled layer".into()))?
        .layout;
    let anchor = layout["split"]["split_id"]
        .as_str()
        .or(layout["block_id"].as_str())
        .ok_or_else(|| TmuxError::Error("empty layout".into()))?;
    let right = config().is_some_and(|c| c.sidebar_position == SidebarPosition::Right);
    let side = if right { "after" } else { "before" };
    // the border is set against the outermost block on the sidebar's side
    let mut edge = layout;
    while !edge["split"].is_null() {
        edge = &edge["split"][side];
    }
    let exe = std::env::current_exe()?.display().to_string();
    let mut args = vec![
        "do".to_string(),
        "-e".to_string(),
        PLACE.to_string(),
        format!("session={session}"),
        format!("anchor={anchor}"),
        format!("side={side}"),
        format!("edge={}", edge["block_id"].as_str().unwrap_or_default()),
        format!("ratio={ratio}"),
        format!("label={SIDEBAR}"),
        format!("exe={exe}"),
        format!("focus={focus}"),
    ];
    if let Some(client) = client {
        args.push(format!("client={client}"));
    }
    Rex::run(&args.iter().map(String::as_str).collect::<Vec<_>>()).map(drop)
}

/// `rex-toggle <session> <block> [split|popup] [client]`, bound in Rex by
/// `rex/agenmux.lua`: open or close the sidebar in `block`'s window, or show
/// the popup over it. `display.mode = "popup"` makes the sidebar key a popup
/// key too, as in tmux.
pub fn rex_toggle(session: &str, block: &str, mode: &str, client: &str) -> i32 {
    let client = Some(client).filter(|client| !client.is_empty());
    let config = config();
    let popup = mode == "popup"
        || config
            .as_ref()
            .is_some_and(|c| c.mode == DisplayMode::Popup);
    if popup {
        return report(open_popup(session, block, config.as_ref(), client));
    }
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
    report(match window.sidebar() {
        // the running sidebar already saved any resize of it
        Some(sidebar) => {
            let request = json!({"session_id": session, "block_id": sidebar});
            Rex::call(Some(session), "block.close", request).map(drop)
        }
        None => {
            let ratio = saved_ratio().unwrap_or(DEFAULT_RATIO);
            add_sidebar(session, window, ratio, true, client)
        }
    })
}

fn report(result: Result<(), TmuxError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agenmux: {e}");
            1
        }
    }
}

/// What `rex_popup.lua` decided.
#[derive(Deserialize)]
struct PopupPlan {
    action: String,
    #[serde(default)]
    window: String,
    #[serde(default)]
    block: String,
    #[serde(default)]
    bounds: Vec<f64>,
}

/// The popup key over `block`: close the focused popup, focus an unfocused
/// one, or open one sized by `display.popup_width` and `popup_height` (0:
/// auto). Two `rex` calls, since popups open on a keypress.
fn open_popup(
    session: &str,
    block: &str,
    config: Option<&AppConfig>,
    client: Option<&str>,
) -> Result<(), TmuxError> {
    let width = config.map_or(40, |c| c.popup_width);
    let height = match config.map(|c| c.popup_height) {
        Some(PopupHeight::Cells(n)) => n,
        _ => 0,
    };
    let args = [
        format!("session={session}"),
        format!("block={block}"),
        format!("width={width}"),
        format!("height={height}"),
        format!("label={POPUP}"),
    ];
    let mut call = vec!["do", "-e", POPUP_LAYER, "--json"];
    call.extend(args.iter().map(String::as_str));
    let plan: PopupPlan = Rex::json(&call)?;
    match plan.action.as_str() {
        "focus" => {
            Rex::focus_block(session, &plan.window, &plan.block);
            Ok(())
        }
        "open" => {
            let exe = std::env::current_exe()?.display().to_string();
            let mut command = vec![
                "env".to_string(),
                "AGENMUX_REX_POPUP=1".to_string(),
                format!("AGENMUX_REX_OVER={block}"),
            ];
            if let Some(client) = client {
                command.push(format!("AGENMUX_REX_CLIENT={client}"));
            }
            command.extend([exe, "sidebar".to_string()]);
            let request = json!({
                "session_id": session,
                "window_id": plan.window,
                "bounds": plan.bounds,
                "focus": true,
                "layout": {"block": {
                    "flavor": SHELL,
                    "label": POPUP,
                    "options": {"shell": "none", "command": command},
                }},
            });
            Rex::call(Some(session), "session.new_layer", request).map(drop)
        }
        _ => Ok(()),
    }
}

/// `update` under Rex: close every sidebar and popup, so their installation
/// leases go with them. Returns the windows that had a sidebar.
pub fn rex_close_sidebars() -> Vec<(String, String)> {
    let mut closed = Vec::new();
    for session in Rex::sessions().unwrap_or_default() {
        let Ok(view) = Rex::view(&session) else {
            continue;
        };
        for window in &view.windows {
            for block in window
                .blocks()
                .filter(|b| b.label == SIDEBAR || b.label == POPUP)
            {
                if block.label == SIDEBAR {
                    closed.push((session.clone(), window.window_id.clone()));
                }
                let request = json!({"session_id": session, "block_id": block.block_id});
                let _ = Rex::call(Some(&session), "block.close", request);
            }
        }
    }
    closed
}

/// Put a sidebar back in each window `rex_close_sidebars` emptied, running
/// whichever release is installed now.
pub fn rex_reopen(windows: &[(String, String)]) {
    let ratio = saved_ratio().unwrap_or(DEFAULT_RATIO);
    for (session, id) in windows {
        let Ok(view) = Rex::view(session) else {
            continue;
        };
        let window = view.windows.iter().find(|w| &w.window_id == id);
        if let Some(window) = window.filter(|w| w.sidebar().is_none()) {
            let _ = add_sidebar(session, window, ratio, false, client().as_deref());
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
                    if let Err(e) = add_sidebar(&session, window, ratio, false, None) {
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
                observe(&mut pending, &line, &HashSet::new());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_saves_only_user_resizes_after_placement() {
        let two = || vec!["a".to_string(), "side".to_string()];
        let three = || vec!["a".to_string(), "b".to_string(), "side".to_string()];
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut width = Width::default();
        // the app's half split, then placement reaching the saved 0.2
        assert_eq!(width.observe(0.5, Some(0.2), false, two(), at(0)), None);
        assert_eq!(width.observe(0.2, Some(0.2), false, two(), at(10)), None);
        assert_eq!(width.observe(0.2, Some(0.2), false, two(), at(20)), None);
        // a resize, and only the change itself
        assert_eq!(
            width.observe(0.26, Some(0.2), false, two(), at(2000)),
            Some(WidthChange::Save(0.26))
        );
        assert_eq!(width.observe(0.26, Some(0.26), false, two(), at(2100)), None);
        // a pane closing beside it reshapes the layout: back to the saved width
        assert_eq!(width.observe(0.26, Some(0.26), false, three(), at(3000)), None);
        assert_eq!(
            width.observe(0.5, Some(0.26), false, two(), at(3100)),
            Some(WidthChange::Fit(0.26))
        );
        assert_eq!(width.observe(0.26, Some(0.26), false, two(), at(3200)), None);
        // the app evens the splits out a scan after the blocks change
        assert_eq!(width.observe(0.26, Some(0.26), false, three(), at(5000)), None);
        assert_eq!(
            width.observe(0.3333, Some(0.26), false, three(), at(5200)),
            Some(WidthChange::Fit(0.26))
        );
        assert_eq!(width.observe(0.26, Some(0.26), false, three(), at(5300)), None);
        // a resize once that settles is the user's again
        assert_eq!(
            width.observe(0.3, Some(0.26), false, three(), at(7000)),
            Some(WidthChange::Save(0.3))
        );

        // the next scan after a save, while the user keeps resizing
        assert_eq!(
            width.observe(0.32, Some(0.3), false, three(), at(7100)),
            Some(WidthChange::Save(0.32))
        );
        // another sidebar's resize moves this one: not saved back, even
        // where a narrower window refuses it
        assert_eq!(width.observe(0.3, Some(0.35), false, three(), at(8000)), None);
        assert_eq!(width.observe(0.35, Some(0.35), false, three(), at(8100)), None);
        assert_eq!(width.observe(0.32, Some(0.4), false, three(), at(9000)), None);
        assert_eq!(width.observe(0.32, Some(0.4), false, three(), at(9100)), None);

        // a width Rex refused never matches: settles once waited
        let mut width = Width::default();
        assert_eq!(width.observe(0.31, Some(0.2), false, two(), at(0)), None);
        assert_eq!(width.observe(0.31, Some(0.2), true, two(), at(3000)), None);
        assert_eq!(
            width.observe(0.25, Some(0.2), true, two(), at(3100)),
            Some(WidthChange::Save(0.25))
        );
    }

    #[test]
    fn the_sidebar_seat_names_the_border_beside_it() {
        // ((side | a) | b): the sidebar's split covers 0.4 of the window
        let layout = json!({"rect": [0, 0, 1, 1], "split": {"direction": "horizontal", "ratio": 0.4,
            "before": {"rect": [0, 0, 0.4, 1], "split": {"direction": "horizontal", "ratio": 0.5,
                "before": {"rect": [0, 0, 0.2, 1], "block_id": "side"},
                "after": {"rect": [0.2, 0, 0.2, 1], "split": {"direction": "vertical", "ratio": 0.5,
                    "before": {"rect": [0.2, 0, 0.2, 0.5], "block_id": "a"},
                    "after": {"rect": [0.2, 0.5, 0.2, 0.5], "block_id": "c"}}}}},
            "after": {"rect": [0.4, 0, 0.6, 1], "block_id": "b"}}});
        let found = seat(&layout, "side").unwrap();
        assert_eq!(found.share, 0.2);
        let border = found.border.unwrap();
        assert_eq!(
            (border.before.as_str(), border.after.as_str()),
            ("side", "a")
        );
        assert!(border.sidebar_before);
        assert_eq!(border.span, 0.4);
        // docked right
        let layout = json!({"rect": [0, 0, 1, 1], "split": {"direction": "horizontal", "ratio": 0.8,
            "before": {"rect": [0, 0, 0.8, 1], "block_id": "a"},
            "after": {"rect": [0.8, 0, 0.2, 1], "block_id": "side"}}});
        let border = seat(&layout, "side").unwrap().border.unwrap();
        assert_eq!(
            (
                border.before.as_str(),
                border.after.as_str(),
                border.sidebar_before
            ),
            ("a", "side", false)
        );
    }

    #[test]
    fn restore_rebuilds_the_saved_layout_without_the_sidebar() {
        use crate::snapshot::{Pane, Window};
        let pane = |id: &str| Pane {
            id: id.into(),
            path: format!("/{id}"),
            agent: None,
        };
        let cwd = |node: &Value| node["block"]["options"]["cwd"].clone();
        // sidebar | (a over b): the sidebar's split drops out, a/b keep theirs
        let layout = json!({"split": {"direction": "horizontal", "ratio": 0.2,
            "before": {"block_id": "block:side"},
            "after": {"split": {"direction": "vertical", "ratio": 0.7,
                "before": {"block_id": "b"}, "after": {"block_id": "a"}}}}});
        let window = Window {
            layout: layout.to_string(),
            panes: vec![pane("a"), pane("b")],
            ..Window::default()
        };
        let (tree, order) = window_layout(&window);
        let split = &tree["split"];
        assert_eq!(
            (split["direction"].as_str(), split["ratio"].as_f64()),
            (Some("vertical"), Some(0.7))
        );
        assert_eq!(
            (cwd(&split["before"]), cwd(&split["after"])),
            (json!("/b"), json!("/a"))
        );
        let ids: Vec<&str> = order.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"], "panes in the order Rex creates the blocks");

        // a layout that misses a pane: all side by side, in equal shares
        let window = Window {
            layout: json!({"block_id": "a"}).to_string(),
            panes: vec![pane("a"), pane("b"), pane("c")],
            ..Window::default()
        };
        let (tree, order) = window_layout(&window);
        assert_eq!(order.len(), 3);
        assert_eq!(tree["split"]["ratio"].as_f64(), Some(1.0 / 3.0));
        assert_eq!(cwd(&tree["split"]["before"]), json!("/a"));
        assert_eq!(tree["split"]["after"]["split"]["ratio"].as_f64(), Some(0.5));
        assert_eq!(cwd(&tree["split"]["after"]["split"]["after"]), json!("/c"));
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
        let own: HashSet<String> = ["block:9".to_string()].into();
        let mut pending = PendingChanges::default();
        let mut see = |line: &str| {
            observe(&mut pending, line.as_bytes(), &own);
            std::mem::take(&mut pending)
        };
        for line in [
            r#"{"type":"client_joined","payload":{}}"#,
            r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"client_connected","payload":{}}}}"#,
            // a popup's own events
            r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"block_event","payload":{"block_id":"block:9","name":"process_changed","payload":{}}}}}"#,
            r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"block_closed","payload":{"block_id":"block:9"}}}}"#,
        ] {
            let seen = see(line);
            assert!(!seen.layout && !seen.focus && !seen.full && seen.panes.is_empty());
        }

        let seen = see(
            r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"block_event","payload":{"block_id":"block:7","creator_name":"com.superlogical.terminal","name":"title_changed","payload":{}}}}}"#,
        );
        assert!(seen.panes.contains("block:7") && !seen.layout);

        let seen = see(
            r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"session_view_changed","payload":{"revision":3}}}}"#,
        );
        assert!(seen.layout && seen.focus);

        assert!(see(r#"{"type":"session_created","payload":{}}"#).full);
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
