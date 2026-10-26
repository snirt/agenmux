use super::{Ids, Kind, Multiplexer, Owner, SidebarPane};
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
    /// The client that sizes it; without one it keeps a default size.
    #[serde(default)]
    owner: Option<String>,
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

#[derive(Deserialize, Default, Debug)]
struct Size {
    #[serde(default)]
    columns: usize,
    #[serde(default)]
    rows: usize,
    #[serde(default)]
    cell_width_px: usize,
    #[serde(default)]
    cell_height_px: usize,
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
    /// Sessions seen holding a window: only those count as emptied.
    had_windows: std::collections::HashSet<String>,
    /// The split sidebar that speaks for all of them; see `elect`.
    leader: bool,
    /// No split sidebar runs: a popup is all there is.
    alone: bool,
    /// Each session's active window and its focused block, at the last scan.
    foci: HashMap<String, (Option<String>, Option<String>)>,
    /// The session whose focus moved last: where the user acts.
    acted: Option<String>,
    /// Last app preference observed; unchanged persisted data must not erase live focus.
    shown: Option<String>,
    /// Each window's layout at the last measure.
    shapes: HashMap<String, String>,
    /// Last applied divider share and terminal width; a font/window resize alone is not a drag.
    dividers: HashMap<String, (f64, usize)>,
    /// Initial placement estimate only; never used as a render size.
    app: Option<(usize, usize)>,
    /// Window dimensions in each sidebar's cell units, with rounding noise removed.
    window_sizes: HashMap<String, (usize, usize)>,
    /// Ratio requests waiting for the client to resize its terminal.
    resizing: HashMap<String, Resize>,
    /// agenmux's own blocks at the last scan, whose events `observe` skips.
    own_blocks: HashSet<String>,
}
/// One asynchronous, bounded width search in this terminal's own cell units.
/// PTY columns remain authoritative. Unresponsive clients park after one probe.
#[derive(Clone, Copy, Debug)]
struct Resize {
    target: usize,
    from_columns: usize,
    requested_share: f64,
    lower: (f64, Option<usize>),
    upper: (f64, Option<usize>),
    requested: Instant,
    probed: bool,
    parked: bool,
    measured_share: f64,
}

impl Resize {
    fn plan(target: usize, columns: usize, seat: &Seat, previous: Option<Self>) -> Option<Self> {
        let border = seat.border.as_ref()?;
        if columns == 0 || seat.share <= 0.0 || target == columns {
            return None;
        }
        let min = border.span * 0.05;
        let max = border.span * 0.95;
        let previous =
            previous.filter(|p| p.target == target && !(p.parked && columns != p.from_columns));
        let no_response = previous.is_some_and(|p| columns == p.from_columns);
        if no_response && previous.is_some_and(|p| p.probed) {
            return None; // one liveness probe only; wait for an actual PTY response
        }
        let mut lower = previous.map_or((min, None), |p| p.lower);
        let mut upper = previous.map_or((max, None), |p| p.upper);
        if columns < target {
            lower = (seat.share, Some(columns));
        } else {
            upper = (seat.share, Some(columns));
        }
        if upper.0 - lower.0 < 1e-6 {
            if ((seat.share - min).abs() < 1e-6 && columns > target)
                || ((seat.share - max).abs() < 1e-6 && columns < target)
            {
                return None; // physical layout/client limit; never send no-op resizes
            }
            // The window/font changed during the search: discard obsolete bounds.
            lower = (min, None);
            upper = (max, None);
        }
        let share = if previous.is_none() {
            // First estimate only. Minimum-size clamping can make it inaccurate.
            (seat.share * (target as f64 + 0.5) / (columns as f64 + 0.5)).clamp(min, max)
        } else if no_response {
            // A minimum-size plateau and a hidden client look identical. Probe
            // a known opposite bound (or halfway to an unknown one), then park
            // if the client still sends no new cell count.
            if columns < target {
                if upper.1.is_some() {
                    upper.0
                } else {
                    (seat.share + upper.0) / 2.0
                }
            } else if lower.1.is_some() {
                lower.0
            } else {
                (seat.share + lower.0) / 2.0
            }
        } else {
            let fraction = match (lower.1, upper.1) {
                (Some(lo), Some(hi)) if hi > lo => {
                    ((target as f64 - lo as f64) / (hi - lo) as f64).clamp(0.125, 0.875)
                }
                _ => 0.5,
            };
            lower.0 + (upper.0 - lower.0) * fraction
        };
        if (share - seat.share).abs() < 1e-6 {
            return None;
        }
        Some(Self {
            target,
            from_columns: columns,
            requested_share: share,
            lower,
            upper,
            requested: Instant::now(),
            probed: no_response,
            parked: false,
            measured_share: if no_response {
                previous.unwrap().measured_share
            } else {
                seat.share
            },
        })
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
fn fit(session: &str, seat: &Seat, share: f64) -> bool {
    let Some(border) = &seat.border else {
        return false;
    };
    let part = (share / border.span).clamp(0.05, 0.95);
    let request = json!({
        "session_id": session,
        "direction": "horizontal",
        "before_block_id": border.before,
        "after_block_id": border.after,
        "ratio": if border.sidebar_before { part } else { 1.0 - part },
    });
    Rex::call(Some(session), "session.resize_pane", request).is_ok()
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

    /// Closing works as in tmux. A window whose last real block closed
    /// takes its sidebar with it, and a session whose last window closed is
    /// destroyed: Rex would keep it empty, and an empty session has no row
    /// to show. Undo still lists it. A session never seen with a window may
    /// be one the app is still filling, so it is left alone.
    fn reap(&mut self) {
        for session in &self.listing.sessions {
            if session.windows.is_empty() {
                if self.had_windows.remove(&session.session_id) {
                    let request = json!({"session_id": session.session_id});
                    let _ = Self::call(None, "session.destroy", request);
                }
                continue;
            }
            self.had_windows.insert(session.session_id.clone());
            let lone: Vec<&Window> = session
                .windows
                .iter()
                .filter(|w| w.others == 0 && !w.blocks.is_empty())
                .filter(|w| w.blocks.iter().all(Block::agenmux))
                .collect();
            // a sidebar is all these windows hold: closing them all ends the
            // session
            if lone.is_empty() {
                continue;
            } else if lone.len() == session.windows.len() {
                let request = json!({"session_id": session.session_id});
                let _ = Self::call(None, "session.destroy", request);
            } else {
                for window in lone {
                    let request =
                        json!({"session_id": session.session_id, "window_id": window.window_id});
                    let _ = Self::call(Some(&session.session_id), "session.close_window", request);
                }
            }
        }
    }

    /// The daemon, which runs in no block, speaks for every sidebar: it
    /// places new windows' sidebars and sends the notifications. A popup
    /// sends them only while no sidebar is open.
    fn elect(&mut self) {
        self.leader = std::env::var_os("REX_BLOCK").is_none();
        self.alone = !self
            .listing
            .sessions
            .iter()
            .flat_map(|s| &s.windows)
            .flat_map(|w| &w.blocks)
            .any(|b| b.label == SIDEBAR);
    }

    /// Observe app selection changes and live server focus changes. The app's
    /// persisted record may lag; rereading the same record is not a new selection.
    fn update_acting(&mut self, shown: Option<String>) {
        if let Some(shown) = shown {
            if self.shown.as_ref() != Some(&shown)
                && self.listing.sessions.iter().any(|s| s.session_id == shown)
            {
                self.acted = Some(shown.clone());
            }
            self.shown = Some(shown);
        }
        let mut moved = None;
        for s in &self.listing.sessions {
            let window = s
                .windows
                .iter()
                .find(|w| Some(&w.window_id) == s.active_window_id.as_ref());
            let focus = (
                s.active_window_id.clone(),
                window.and_then(|w| w.focused_block_id.clone()),
            );
            let before = self.foci.insert(s.session_id.clone(), focus.clone());
            if before.is_some_and(|before| before != focus) {
                moved = Some(s.session_id.clone());
            }
        }
        if moved.is_some() {
            self.acted = moved;
        }
        self.foci
            .retain(|id, _| self.listing.sessions.iter().any(|s| &s.session_id == id));
        if self
            .acted
            .as_ref()
            .is_none_or(|id| !self.listing.sessions.iter().any(|s| &s.session_id == id))
        {
            self.acted = self.listing.sessions.first().map(|s| s.session_id.clone());
        }
    }

    fn acting(&mut self) -> Option<&Session> {
        self.update_acting(shown_session());
        self.listing
            .sessions
            .iter()
            .find(|s| Some(&s.session_id) == self.acted.as_ref())
    }

    fn request_width(&mut self, block: &str, width: usize) -> bool {
        let found = self.listing.sessions.iter().find_map(|s| {
            s.windows.iter().find_map(|w| {
                let b = w.blocks.iter().find(|b| b.block_id == block)?;
                // Unowned terminals still carry the server's default geometry.
                if b.owner.is_none() || s.active_window_id.as_ref() != Some(&w.window_id) {
                    return None;
                }
                Some((
                    s.session_id.clone(),
                    seat(&w.layout, block)?,
                    b.size.columns,
                ))
            })
        });
        let Some((session, seat, columns)) = found else {
            return false;
        };
        if width == columns {
            if let Some(mut pending) = self.resizing.get(block).copied() {
                // A new preference can cancel a request while a hidden PTY
                // still reports the old width. Restore its last measured ratio,
                // not the unacknowledged ratio currently in the layout.
                let share = if pending.from_columns == columns {
                    pending.measured_share
                } else {
                    seat.share
                };
                if (share - seat.share).abs() > 1e-6 {
                    if !fit(&session, &seat, share) {
                        return false;
                    }
                    pending.target = width;
                    pending.requested_share = share;
                    pending.requested = Instant::now();
                    pending.probed = false;
                    pending.parked = false;
                    self.resizing.insert(block.to_string(), pending);
                    return true;
                }
            }
            self.resizing.remove(block);
            self.dividers
                .insert(block.to_string(), (seat.share, columns));
            return true;
        }
        let Some(plan) = Resize::plan(width, columns, &seat, self.resizing.get(block).copied())
        else {
            // Park unresponsive clients/physical constraints, keeping the
            // desired target so a later real PTY resize can restart calibration.
            if let Some(pending) = self.resizing.get_mut(block) {
                if pending.from_columns != columns {
                    pending.measured_share = seat.share;
                    self.dividers
                        .insert(block.to_string(), (seat.share, columns));
                }
                pending.parked = true;
                pending.from_columns = columns;
                pending.requested_share = seat.share;
            }
            return false;
        };
        if !fit(&session, &seat, plan.requested_share) {
            return false;
        }
        self.resizing.insert(block.to_string(), plan);
        true
    }

    fn measure_sidebars(&mut self) -> Vec<SidebarPane> {
        let mut shapes = HashMap::new();
        let mut panes = Vec::new();
        let mut retry = Vec::new();
        let mut live = HashSet::new();
        for s in &self.listing.sessions {
            for w in &s.windows {
                let shape = format!(
                    "{}{:?}",
                    w.layout,
                    w.blocks
                        .iter()
                        .map(|b| (&b.block_id, &b.size))
                        .collect::<Vec<_>>()
                );
                let stable = self.shapes.get(&w.window_id) == Some(&shape);
                shapes.insert(w.window_id.clone(), shape);
                for b in w.blocks.iter().filter(|b| b.label == SIDEBAR) {
                    let Some(seat) = seat(&w.layout, &b.block_id) else {
                        continue;
                    };
                    live.insert(b.block_id.clone());
                    if let Some(pending) = self.resizing.get(&b.block_id).copied() {
                        if b.size.columns == pending.target {
                            self.resizing.remove(&b.block_id);
                            self.dividers
                                .insert(b.block_id.clone(), (seat.share, b.size.columns));
                        } else if (seat.share - pending.requested_share).abs() > 1e-6 {
                            // An external layout change superseded our request.
                            self.resizing.remove(&b.block_id);
                        } else {
                            if b.size.columns != pending.from_columns {
                                // Every applied step is a new divider baseline,
                                // even when the requested width cannot be granted.
                                self.dividers
                                    .insert(b.block_id.clone(), (seat.share, b.size.columns));
                            }
                            if stable
                                && s.active_window_id.as_ref() == Some(&w.window_id)
                                && (b.size.columns != pending.from_columns
                                    || (!pending.parked
                                        && pending.requested.elapsed()
                                            >= Duration::from_millis(250)))
                            {
                                retry.push((b.block_id.clone(), pending.target));
                            }
                        }
                    }
                    let before = self.dividers.get(&b.block_id).copied();
                    let width_drag = before.is_some_and(|(share, cols)| {
                        (share - seat.share).abs() > 1e-6 && cols != b.size.columns
                    });
                    // A ratio can arrive before its PTY size. Do not publish a
                    // settled baseline or recalibrate from this mixed snapshot.
                    let waiting = before.is_some_and(|(share, cols)| {
                        (share - seat.share).abs() > 1e-6 && cols == b.size.columns
                    });
                    let settled = stable
                        && b.owner.is_some()
                        && !self.resizing.contains_key(&b.block_id)
                        && !waiting;
                    // Window sizes are placement estimates only. In particular,
                    // padding makes them change as shares change, even in a
                    // fixed-size app window. Divider evidence is authoritative.
                    let measured =
                        window_cells_in(w, &b.size).unwrap_or((b.size.columns, b.size.rows));
                    let size = self
                        .window_sizes
                        .entry(w.window_id.clone())
                        .or_insert(measured);
                    if settled
                        && (size.0.abs_diff(measured.0) > 2 || size.1.abs_diff(measured.1) > 2)
                    {
                        *size = measured;
                    }
                    if settled && !width_drag {
                        self.dividers
                            .insert(b.block_id.clone(), (seat.share, b.size.columns));
                    }
                    panes.push(SidebarPane {
                        pane: b.block_id.clone(),
                        win: w.window_id.clone(),
                        sess: s.session_id.clone(),
                        w: b.size.columns,
                        h: b.size.rows,
                        win_size: *size,
                        panes: w.blocks.len() + w.others,
                        active: s.active_window_id.as_ref() == Some(&w.window_id),
                        settled,
                        width_drag: Some(width_drag),
                    });
                }
            }
        }
        self.window_sizes.retain(|w, _| shapes.contains_key(w));
        self.resizing.retain(|b, _| live.contains(b));
        self.dividers.retain(|b, _| live.contains(b));
        self.shapes = shapes;
        for (block, target) in retry {
            self.request_width(&block, target);
        }
        panes
    }
    /// While agenmux is on, the window the user moves to gets a sidebar, as
    /// tmux gives every window one when a client visits it.
    fn spread(&mut self) {
        if !self.leader {
            return;
        }
        let Some(session) = self.acting().map(|s| s.session_id.clone()) else {
            return;
        };
        let session = self
            .listing
            .sessions
            .iter()
            .find(|s| s.session_id == session);
        let window = session.and_then(|s| {
            s.windows
                .iter()
                .find(|w| Some(&w.window_id) == s.active_window_id.as_ref())
        });
        let Some((session, window)) = session.zip(window) else {
            return;
        };
        let real = window.others > 0 || window.blocks.iter().any(|b| !b.agenmux());
        if !real || window.blocks.iter().any(|b| b.label == SIDEBAR) {
            return;
        }
        let (session, window_id) = (session.session_id.clone(), window.window_id.clone());
        let share = self.share();
        let Ok(view) = Self::view(&session) else {
            return;
        };
        if let Some(window) = view.windows.iter().find(|w| w.window_id == window_id) {
            if window.sidebar().is_none() {
                let _ = add_sidebar(&session, window, share, false, client().as_deref());
            }
        }
    }

    /// The sidebar's share of an app window.
    fn share(&self) -> f64 {
        share_of(self.app, 0)
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
        // a popup follows its own session; the daemon, where the user acts
        let own = std::env::var("REX_SESSION").ok();
        let session = match own {
            Some(own) => self
                .listing
                .sessions
                .iter()
                .find(|s| s.session_id == own)
                .or(self.listing.sessions.first())?,
            None => self.acting()?,
        };
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
        self.embedded() && !self.leader
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
    /// briefly, then place one. With no window, the sidebar the user acts in.
    fn focus_sidebar(&mut self, _client: &str, expected_window: Option<&str>) {
        let Some(expected) = expected_window else {
            // the daemon's sidebar: the one where the user acts
            let own = std::env::var("REX_BLOCK").ok().or_else(|| {
                let session = self.acting()?;
                session
                    .windows
                    .iter()
                    .find(|w| Some(&w.window_id) == session.active_window_id.as_ref())?
                    .blocks
                    .iter()
                    .find(|b| b.label == SIDEBAR)
                    .map(|b| b.block_id.clone())
            });
            if let Some(own) = own {
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
                    let share = self.share();
                    let _ = add_sidebar(&session, &window, share, true, client().as_deref());
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

    /// Rex has no options: a dragged width is a state file.
    fn options(&mut self) -> Result<BTreeMap<String, String>, crate::app_config::ConfigError> {
        let width = std::fs::read_to_string(state_file(WIDTH_FILE)).ok();
        Ok(width
            .map(|width| ("@agenmux-width".to_string(), width.trim().to_string()))
            .into_iter()
            .collect())
    }

    /// Rendering uses the terminal's actual dimensions, not a layout estimate.
    /// The client may use different fonts and keep hidden terminals at old sizes.
    fn sidebar_panes(&mut self) -> Vec<SidebarPane> {
        if let Ok(listing) = Self::json(&["do", "-e", SCAN, "--json"]) {
            self.listing = listing;
        }
        let acting = self.acting().map(|s| s.session_id.clone());
        self.app = app_cells(&self.listing, acting.as_deref());
        self.measure_sidebars()
    }

    /// Rex does not say which session an app window shows: every
    /// session's current window counts.
    fn visible_windows(&mut self) -> HashSet<String> {
        self.listing
            .sessions
            .iter()
            .filter_map(|s| s.active_window_id.clone())
            .collect()
    }

    fn resize_panes(&mut self, widths: &[(String, usize)]) -> bool {
        let mut ok = true;
        for (block, width) in widths {
            ok &= self.request_width(block, *width);
        }
        ok
    }

    fn close_panes(&mut self, panes: &[String]) {
        for block in panes {
            if let Some((session, _)) = self.locate(block) {
                let request = json!({"session_id": session, "block_id": block});
                let _ = Self::call(Some(&session), "block.close", request);
            }
        }
    }

    fn close_sidebars(&mut self) {
        rex_close_sidebars();
    }

    fn save_width(&mut self, width: usize) -> bool {
        if std::fs::write(state_file(WIDTH_FILE), format!("{width}\n")).is_err() {
            return false;
        }
        // Geometry refreshes can run before the mirror tick handles a drag.
        // Only acknowledging it here consumes the divider change.
        self.dividers.clear();
        true
    }

    fn connection(&mut self) -> Option<String> {
        Some(format!("pid:{}", std::process::id()))
    }

    fn owner(&mut self) -> Option<Owner> {
        let record = std::fs::read_to_string(crate::tmux::runtime_dir().join(OWNER_FILE))
            .unwrap_or_default();
        let (generation, client) = record.trim_end().split_once('|').unwrap_or_default();
        Some(Owner {
            generation: generation.to_string(),
            client: client.to_string(),
        })
    }

    fn publish_owner(&mut self, client: &str, runtime: &std::path::Path) -> Result<(), TmuxError> {
        let generation = std::env::var("AGENMUX_GENERATION").unwrap_or_default();
        std::fs::write(runtime.join(OWNER_FILE), format!("{generation}|{client}\n"))?;
        Ok(())
    }

    fn clear_owner(&mut self, generation: Option<&str>) {
        if generation.is_none_or(|g| self.owner().is_some_and(|o| o.generation == g)) {
            let _ = std::fs::remove_file(crate::tmux::runtime_dir().join(OWNER_FILE));
        }
    }
}

/// The dragged sidebar width, in cells.
const WIDTH_FILE: &str = "rex-sidebar-width";
/// `generation|client` of the daemon that owns the sidebars.
const OWNER_FILE: &str = "agenmux-owner";

/// Where the daemon, its sidebar panes and key senders meet.
pub fn runtime_dir() -> PathBuf {
    std::env::temp_dir().join(format!("agenmux-rex-{}", unsafe { libc::geteuid() }))
}

/// Estimate a window in this sidebar's font, solely for resize classification.
/// Cell counts from different terminal fonts are not interchangeable.
fn window_cells_in(w: &Window, font: &Size) -> Option<(usize, usize)> {
    w.blocks
        .iter()
        .filter(|b| !b.agenmux())
        .filter_map(|b| Some((b, rect(&w.layout, &b.block_id)?)))
        .max_by(|(_, a), (_, b)| a[2].total_cmp(&b[2]))
        .map(|(b, r)| {
            let cols = b.size.columns.saturating_mul(b.size.cell_width_px.max(1))
                / font.cell_width_px.max(1);
            let rows = b.size.rows.saturating_mul(b.size.cell_height_px.max(1))
                / font.cell_height_px.max(1);
            (cells(cols, r[2]), cells(rows, r[3]))
        })
}

/// The window's size, from its widest real block: a block the app has not
/// sized yet says nothing about it.
fn window_cells(w: &Window) -> Option<(usize, usize)> {
    w.blocks
        .iter()
        .filter(|b| !b.agenmux())
        .filter_map(|b| Some((b, rect(&w.layout, &b.block_id)?)))
        .max_by(|(_, a), (_, b)| a[2].total_cmp(&b[2]))
        .map(|(b, r)| (cells(b.size.columns, r[2]), cells(b.size.rows, r[3])))
}

/// The app window's size in cells: the window it shows in `shown`, else in
/// any session, measured from its widest real block.
fn app_cells(listing: &Listing, shown: Option<&str>) -> Option<(usize, usize)> {
    let shown = listing
        .sessions
        .iter()
        .find(|s| Some(s.session_id.as_str()) == shown);
    shown
        .and_then(current_window)
        .and_then(window_cells)
        .or_else(|| {
            listing
                .sessions
                .iter()
                .filter_map(current_window)
                .find_map(window_cells)
        })
}

/// The session's current window.
fn current_window(s: &Session) -> Option<&Window> {
    s.windows
        .iter()
        .find(|w| Some(&w.window_id) == s.active_window_id.as_ref())
}

/// The sidebar's share of an app window `app` cells wide, which frames the
/// sidebar block with `frame` of them.
fn share_of(app: Option<(usize, usize)>, frame: usize) -> f64 {
    let width = (sidebar_width() + frame) as f64;
    app.map_or(0.2, |(cols, _)| width / cols.max(1) as f64)
        .clamp(0.05, 0.5)
}

/// The share for a sidebar placed outside the daemon, from a fresh scan.
fn placement_share() -> f64 {
    let listing = Rex::json(&["do", "-e", SCAN, "--json"]).ok();
    share_of(
        listing.and_then(|l| app_cells(&l, shown_session().as_deref())),
        0,
    )
}

/// The session the app shows. Rex reports none, but the app keeps its own
/// record in its preferences as you switch.
fn shown_session() -> Option<String> {
    let out = Command::new("defaults")
        .args(["export", "com.superlogical.rex", "-"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let plist = String::from_utf8_lossy(&out.stdout);
    let rest = plist
        .split_once("<key>Workspace.lastFocusedSession</key>")?
        .1;
    let data = rest.split_once("<data>")?.1.split_once("</data>")?.0;
    let record: Value = serde_json::from_slice(&unbase64(data)?).ok()?;
    record["sessionID"].as_str().map(str::to_string)
}

/// Standard base64, whitespace ignored.
fn unbase64(text: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut bits, mut count) = (0u32, 0);
    for byte in text
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
    {
        bits = bits << 6 | ALPHABET.iter().position(|&a| a == byte)? as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

/// A whole-window count of cells from a block's `count` and its `share`.
fn cells(count: usize, share: f64) -> usize {
    if share > 0.0 {
        (count as f64 / share).round() as usize
    } else {
        count
    }
}

/// `block`'s rect in `node`: [x, y, width, height] as window fractions.
fn rect(node: &Value, block: &str) -> Option<[f64; 4]> {
    if node["block_id"].as_str() == Some(block) {
        let r = node["rect"].as_array()?;
        return Some([
            r.first()?.as_f64()?,
            r.get(1)?.as_f64()?,
            r.get(2)?.as_f64()?,
            r.get(3)?.as_f64()?,
        ]);
    }
    let split = node.get("split")?;
    rect(&split["before"], block).or_else(|| rect(&split["after"], block))
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

/// The sidebar width in cells: the last dragged one, else the configured.
fn sidebar_width() -> usize {
    std::fs::read_to_string(state_file(WIDTH_FILE))
        .ok()
        .and_then(|width| width.trim().parse().ok())
        .or_else(|| config().map(|c| c.sidebar_width as usize))
        .unwrap_or(30)
}

/// The live daemon its owner record names.
fn daemon_pid() -> Option<libc::pid_t> {
    let record = std::fs::read_to_string(crate::tmux::runtime_dir().join(OWNER_FILE)).ok()?;
    let pid = record.trim_end().rsplit_once("|pid:")?.1.parse().ok()?;
    (unsafe { libc::kill(pid, 0) } == 0).then_some(pid)
}

/// Start the daemon that renders into the sidebar blocks, unless its owner
/// record names a live one. Two racing starts are fine: the later claim
/// wins and the earlier daemon leaves.
fn ensure_daemon(client: Option<&str>) -> Result<(), TmuxError> {
    if daemon_pid().is_some() {
        return Ok(());
    }
    use std::os::unix::process::CommandExt;
    let mut daemon = Command::new(std::env::current_exe()?);
    daemon
        .arg("daemon")
        .env("AGENMUX_MUX", "rex")
        // it runs in no block, whichever block asked for it
        .env_remove("REX_BLOCK")
        .env_remove("REX_SESSION")
        // its own group: closing the block that asked must not end it
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(client) = client {
        daemon.env("AGENMUX_REX_CLIENT", client);
    }
    daemon.spawn()?;
    Ok(())
}

/// Split the whole tiled layer (its top split, or its only block) so the
/// sidebar spans the window's full height, at `display.sidebar_position`.
/// `focus`: the user opened it.
fn add_sidebar(
    session: &str,
    window: &ViewWindow,
    share: f64,
    focus: bool,
    client: Option<&str>,
) -> Result<(), TmuxError> {
    ensure_daemon(client)?;
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
        format!("share={share}"),
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
    // on and off everywhere, as in tmux: the daemon's teardown closes every
    // sidebar
    if let Some(daemon) = daemon_pid() {
        unsafe { libc::kill(daemon, libc::SIGTERM) };
        return 0;
    }
    report(match window.sidebar() {
        Some(sidebar) => {
            let request = json!({"session_id": session, "block_id": sidebar});
            Rex::call(Some(session), "block.close", request).map(drop)
        }
        None => add_sidebar(session, window, placement_share(), true, client),
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
    let share = placement_share();
    for (session, id) in windows {
        let Ok(view) = Rex::view(session) else {
            continue;
        };
        let window = view.windows.iter().find(|w| &w.window_id == id);
        if let Some(window) = window.filter(|w| w.sidebar().is_none()) {
            let _ = add_sidebar(session, window, share, false, client().as_deref());
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
            for window in &view.windows {
                if window.real_blocks() == 0 {
                    // the window's last real block closed: the sidebar goes too
                    let request = json!({"session_id": session, "window_id": window.window_id});
                    let _ = Rex::call(Some(&session), "session.close_window", request);
                } else if window.sidebar().is_none() {
                    if let Err(e) = add_sidebar(&session, window, placement_share(), false, None) {
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

    fn geometry_listing() -> Listing {
        let windows: Vec<_> = [(34, 46, 147, 16), (24, 18, 148, 17)]
            .into_iter().enumerate().map(|(i, (cols, rows, other, font))| json!({
                "window_id": format!("window:{i}"),
                "layout": {"rect":[0,0,1,1], "split": {
                    "direction":"horizontal", "ratio":0.2,
                    "before":{"block_id":format!("side:{i}"), "rect":[0,0,0.2,1]},
                    "after":{"block_id":format!("shell:{i}"), "rect":[0.2,0,0.8,1]}
                }},
                "blocks":[
                    {"block_id":format!("side:{i}"),"label":"agenmux","owner":"client:1",
                     "size":{"columns":cols,"rows":rows,"cell_width_px":17,"cell_height_px":39}},
                    {"block_id":format!("shell:{i}"),"label":"shell",
                     "size":{"columns":other,"rows":46,"cell_width_px":font,"cell_height_px":39}}
                ]
            })).collect();
        serde_json::from_value(json!({"sessions":[{
            "session_id":"session:1","active_window_id":"window:0","windows":windows
        }]}))
        .unwrap()
    }

    #[test]
    fn unchanged_app_preferences_do_not_erase_live_session_focus() {
        let mut rex = Rex {
            listing: geometry_listing(),
            ..Rex::default()
        };
        rex.listing.sessions.push(
            serde_json::from_value(json!({
                "session_id":"session:2","active_window_id":"window:other","windows":[{
                    "window_id":"window:other","blocks":[]
                }]
            }))
            .unwrap(),
        );
        rex.update_acting(Some("session:1".into()));
        assert_eq!(rex.acted.as_deref(), Some("session:1"));
        rex.listing.sessions[1].windows[0].focused_block_id = Some("shell:other".into());
        rex.update_acting(Some("session:1".into()));
        for _ in 0..3 {
            rex.update_acting(Some("session:1".into()));
            assert_eq!(rex.acted.as_deref(), Some("session:2"));
        }
        // A newly changed app record still wins when focus has not moved.
        rex.update_acting(Some("session:2".into()));
        rex.update_acting(Some("session:1".into()));
        assert_eq!(rex.acted.as_deref(), Some("session:1"));
        rex.listing.sessions.remove(0);
        rex.update_acting(Some("session:1".into()));
        assert_eq!(rex.acted.as_deref(), Some("session:2"));
        assert!(!rex.foci.contains_key("session:1"));
    }

    #[test]
    fn client_resizes_do_not_become_saved_drags_and_delayed_dividers_do() {
        let mut rex = Rex {
            listing: geometry_listing(),
            ..Rex::default()
        };
        rex.measure_sidebars();
        rex.measure_sidebars();
        // A one-column client/font resize can fall within estimation rounding.
        rex.listing.sessions[0].windows[0].blocks[0].size.columns = 35;
        rex.measure_sidebars();
        let p = rex.measure_sidebars()[0].clone();
        assert!(p.settled);
        assert_eq!(p.width_drag, Some(false));
        // The divider moves first, but the terminal can lag by several scans.
        let layout = &mut rex.listing.sessions[0].windows[0].layout;
        layout["split"]["ratio"] = json!(0.25);
        layout["split"]["before"]["rect"] = json!([0, 0, 0.25, 1]);
        for _ in 0..3 {
            let p = rex.measure_sidebars()[0].clone();
            assert_eq!(p.width_drag, Some(false));
            assert!(
                !p.settled,
                "unchanged PTY columns cannot settle a moved ratio"
            );
        }
        rex.listing.sessions[0].windows[0].blocks[0].size.columns = 40;
        assert!(!rex.measure_sidebars()[0].settled);
        assert_eq!(rex.measure_sidebars()[0].width_drag, Some(true));
        assert_eq!(
            rex.measure_sidebars()[0].width_drag,
            Some(true),
            "refreshes cannot consume a drag before the mirror tick"
        );
        rex.dividers.clear(); // save_width acknowledges it after persisting
        assert_eq!(rex.measure_sidebars()[0].width_drag, Some(false));
    }

    #[test]
    fn render_geometry_is_the_actual_pty_even_with_identical_ratios() {
        let mut rex = Rex {
            listing: geometry_listing(),
            app: Some((1000, 1000)),
            ..Rex::default()
        };
        let first = rex.measure_sidebars();
        assert_eq!(
            first.iter().map(|p| (p.w, p.h)).collect::<Vec<_>>(),
            [(34, 46), (24, 18)]
        );
        assert!(first.iter().all(|p| !p.settled));
        assert!(rex.measure_sidebars().iter().all(|p| p.settled));
        // Sidebar font changes do not calibrate unrelated windows.
        let other = rex.window_sizes["window:1"];
        rex.listing.sessions[0].windows[0].blocks[0]
            .size
            .cell_width_px = 20;
        let changing = rex.measure_sidebars();
        assert!(!changing[0].settled);
        assert_eq!(changing[0].w, 34);
        let changed = rex.measure_sidebars();
        assert_ne!(changed[0].win_size, first[0].win_size);
        assert_eq!(changed[1].win_size, other);
        assert_eq!(changed[0].w, 34);
    }

    #[test]
    fn pending_resize_never_promotes_a_requested_width_to_render_geometry() {
        let mut rex = Rex {
            listing: geometry_listing(),
            ..Rex::default()
        };
        let pending = Resize {
            target: 40,
            from_columns: 34,
            lower: (0.2, Some(34)),
            upper: (0.95, None),
            requested: Instant::now(),
            probed: false,
            parked: false,
            measured_share: 0.2,
            requested_share: 0.25,
        };
        rex.resizing.insert("side:0".into(), pending);
        let layout = &mut rex.listing.sessions[0].windows[0].layout;
        layout["split"]["ratio"] = json!(0.25);
        layout["split"]["before"]["rect"] = json!([0, 0, 0.25, 1]);
        for _ in 0..5 {
            let p = &rex.measure_sidebars()[0];
            assert_eq!(p.w, 34);
            assert!(
                !p.settled,
                "two unchanged scans do not prove the client applied the ratio"
            );
        }
        rex.listing.sessions[0].windows[0].blocks[0].size.columns = 40;
        assert_eq!(rex.measure_sidebars()[0].w, 40);
        assert!(!rex.resizing.contains_key("side:0"));
        assert!(rex.measure_sidebars()[0].settled);
    }

    #[test]
    fn an_applied_constrained_step_does_not_hide_a_later_user_drag() {
        let mut rex = Rex {
            listing: geometry_listing(),
            ..Rex::default()
        };
        let initial = seat(&rex.listing.sessions[0].windows[0].layout, "side:0").unwrap();
        rex.resizing.insert(
            "side:0".into(),
            Resize::plan(1, 34, &initial, None).unwrap(),
        );
        let w = &mut rex.listing.sessions[0].windows[0];
        w.layout["split"]["before"]["rect"] = json!([0, 0, 0.05, 1]);
        w.blocks[0].size.columns = 16;
        rex.measure_sidebars();
        assert_eq!(rex.dividers["side:0"], (0.05, 16));
        // Dragging back to the original ratio must not look like "no change".
        let w = &mut rex.listing.sessions[0].windows[0];
        w.layout["split"]["before"]["rect"] = json!([0, 0, 0.2, 1]);
        w.blocks[0].size.columns = 34;
        rex.measure_sidebars();
        let moved = rex.measure_sidebars()[0].clone();
        assert!(moved.settled);
        assert_eq!(moved.width_drag, Some(true));
        assert!(!rex.resizing.contains_key("side:0"));
    }

    #[test]
    fn a_client_minimum_is_acknowledged_without_restoring_an_obsolete_ratio() {
        let mut rex = Rex {
            listing: geometry_listing(),
            ..Rex::default()
        };
        let initial = seat(&rex.listing.sessions[0].windows[0].layout, "side:0").unwrap();
        let pending = Resize::plan(1, 34, &initial, None).unwrap();
        assert_eq!(pending.requested_share, 0.05);
        rex.resizing.insert("side:0".into(), pending);
        let w = &mut rex.listing.sessions[0].windows[0];
        w.layout["split"]["before"]["rect"] = json!([0, 0, 0.05, 1]);
        w.blocks[0].size.columns = 16;
        // This path parks without a CLI call: the border is already at its limit.
        assert!(!rex.resize_panes(&[("side:0".into(), 1)]));
        let parked = rex.resizing["side:0"];
        assert!(parked.parked);
        assert_eq!(parked.measured_share, 0.05);
        // Choosing the realized minimum cancels the impossible target in place.
        assert!(rex.resize_panes(&[("side:0".into(), 16)]));
        assert!(!rex.resizing.contains_key("side:0"));
        assert_eq!(rex.dividers["side:0"], (0.05, 16));
    }

    #[test]
    fn unresponsive_width_search_parks_and_restarts_from_fresh_pty_data() {
        let mut seat = Seat {
            share: 0.2,
            border: Some(Border {
                before: "side".into(),
                after: "shell".into(),
                sidebar_before: true,
                span: 1.0,
            }),
        };
        let first = Resize::plan(17, 35, &seat, None).unwrap();
        seat.share = first.requested_share;
        let mut probe = Resize::plan(17, 35, &seat, Some(first)).unwrap();
        assert!(probe.probed);
        assert_eq!(
            probe.measured_share, 0.2,
            "unacknowledged ratios are not measurements"
        );
        seat.share = probe.requested_share;
        assert!(Resize::plan(17, 35, &seat, Some(probe)).is_none());
        probe.parked = true;
        let width = |share: f64| ((174.0 * share - 3.2).floor() as usize).max(16);
        let mut columns = width(seat.share);
        let mut previous = Some(probe);
        for _ in 0..40 {
            if columns == 17 {
                break;
            }
            let step = Resize::plan(17, columns, &seat, previous).unwrap();
            seat.share = step.requested_share;
            columns = width(seat.share);
            previous = Some(step);
        }
        assert_eq!(columns, 17);
    }

    #[test]
    fn width_feedback_crosses_minimum_size_plateaus_without_oscillation() {
        for scale in [100.0, 174.0, 240.0] {
            for start in [0.05, 0.08, 0.2, 0.7] {
                for target in [17, 18, 30, 60] {
                    let mut seat = Seat {
                        share: start,
                        border: Some(Border {
                            before: "side".into(),
                            after: "shell".into(),
                            sidebar_before: true,
                            span: 1.0,
                        }),
                    };
                    let mut previous = None;
                    let width = |share: f64| ((scale * share - 3.2_f64).floor() as usize).max(16);
                    let mut columns = width(seat.share);
                    for _ in 0..40 {
                        if columns == target {
                            break;
                        }
                        let plan = Resize::plan(target, columns, &seat, previous).unwrap();
                        seat.share = plan.requested_share;
                        columns = width(seat.share);
                        previous = Some(plan);
                    }
                    assert_eq!(columns, target, "scale={scale}, start={start}");
                }
            }
        }
    }

    #[test]
    fn width_feedback_converges_in_the_sidebars_own_font() {
        for scale in [100.0, 174.0, 240.0] {
            for target in [15, 30, 60] {
                let mut seat = Seat {
                    share: 0.2,
                    border: Some(Border {
                        before: "side".into(),
                        after: "shell".into(),
                        sidebar_before: true,
                        span: 1.0,
                    }),
                };
                let mut previous = None;
                let mut columns = (scale * seat.share - 3.2_f64).floor() as usize;
                for _ in 0..8 {
                    if columns == target {
                        break;
                    }
                    let p = Resize::plan(target, columns, &seat, previous).unwrap();
                    seat.share = p.requested_share;
                    columns = (scale * seat.share - 3.2_f64).floor() as usize;
                    previous = Some(p);
                }
                assert_eq!(columns, target, "scale={scale}");
            }
        }
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
    fn the_app_record_decodes_across_lines() {
        // as `defaults export` wraps a data value
        let data = "\n\teyJzZXNzaW9uSUQiOiJzZXNz\n\taW9uOjEifQ==\n\t";
        assert_eq!(
            unbase64(data).as_deref(),
            Some(br#"{"sessionID":"session:1"}"#.as_slice())
        );
        assert_eq!(unbase64("Zm8=").as_deref(), Some(b"fo".as_slice()));
        assert_eq!(unbase64("Z!=="), None);
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
