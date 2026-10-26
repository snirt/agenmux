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
    /// Opaque state owned by rex_place.lua; Rust never interprets resize policy.
    layout_state: Value,
    layout_started: Option<Instant>,
    /// agenmux's own blocks at the last scan, whose events `observe` skips.
    own_blocks: HashSet<String>,
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

    /// Geometry only: report real terminal sizes, never mutate a split.
    fn measure_sidebars(&self) -> Vec<SidebarPane> {
        self.listing
            .sessions
            .iter()
            .flat_map(|s| {
                s.windows.iter().flat_map(move |w| {
                    w.blocks
                        .iter()
                        .filter(|b| b.label == SIDEBAR)
                        .map(move |b| SidebarPane {
                            pane: b.block_id.clone(),
                            win: w.window_id.clone(),
                            sess: s.session_id.clone(),
                            w: b.size.columns,
                            h: b.size.rows,
                            win_size: window_cells_in(w, &b.size)
                                .unwrap_or((b.size.columns, b.size.rows)),
                            panes: w.blocks.len() + w.others,
                            active: s.active_window_id.as_ref() == Some(&w.window_id),
                            settled: false,
                            width_drag: Some(false),
                        })
                })
            })
            .collect()
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
                    let share = placement_share();
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

    fn reconcile_sidebar_layout(&mut self, width: usize) -> Option<Vec<SidebarPane>> {
        #[derive(Deserialize)]
        struct Reconciled {
            state: Value,
            listing: Listing,
            #[serde(default, deserialize_with = "list")]
            panes: Vec<SidebarPane>,
            saved_width: Option<usize>,
        }
        let args = json!({
            "op": "reconcile",
            "state": if self.layout_state.is_object() { self.layout_state.clone() } else { json!({}) },
            "width": width,
            "now": self.layout_started.get_or_insert_with(Instant::now).elapsed().as_secs_f64(),
            "session": self.acting().map(|s| s.session_id.clone()),
            "exe": std::env::current_exe().ok().map(|p| p.display().to_string()),
            "client": client(),
            "side": if config().is_some_and(|c| c.sidebar_position == SidebarPosition::Right) { "after" } else { "before" },
        });
        let script =
            format!("local scan = function()\n{SCAN}\nend\nrex.args.listing = scan()\n{PLACE}");
        match Self::json::<Reconciled>(&[
            "do",
            "-e",
            &script,
            "--args",
            &args.to_string(),
            "--json",
        ]) {
            Ok(result) => {
                self.layout_state = result.state;
                self.listing = result.listing;
                if let Some(width) = result.saved_width.filter(|w| (1..=10000).contains(w)) {
                    self.save_width(width);
                }
                Some(result.panes)
            }
            Err(error) => {
                eprintln!("agenmux: Rex layout reconciliation failed: {error}");
                // A failed Lua owner must never enable the shared Rust resize policy.
                Some(self.measure_sidebars())
            }
        }
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

/// Replace only the rendering daemon. Readers, block IDs and split ratios stay
/// in place; Rex exposes no in-place terminal process replacement operation.
pub fn rex_reload(client: &str) -> i32 {
    use std::os::unix::process::CommandExt;
    if daemon_pid().is_none() {
        eprintln!("agenmux: no Rex sidebar daemon to reload");
        return 1;
    }
    let owner = crate::tmux::runtime_dir().join(OWNER_FILE);
    let previous = std::fs::read(&owner).ok();
    let result = (|| -> Result<(), TmuxError> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("daemon")
            .env("AGENMUX_MUX", "rex")
            .env("AGENMUX_STARTUP_ACK", "1")
            .env_remove("AGENMUX_GENERATION")
            .env_remove("REX_BLOCK")
            .env_remove("REX_SESSION")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if !client.is_empty() {
            command.env("AGENMUX_REX_CLIENT", client);
        }
        let mut child = command.spawn()?;
        let mut ready = child.stdout.take().unwrap();
        let mut ack = [0];
        let deadline = Instant::now() + Duration::from_secs(10);
        if !crate::tmux::poll_by(ready.as_raw_fd(), libc::POLLIN, deadline)
            || ready.read_exact(&mut ack).is_err()
            || ack != *b"R"
        {
            // SIGTERM would tear down the preserved blocks. Failure must not.
            let _ = child.kill();
            let _ = child.wait();
            // The child may already have claimed the sidebars: hand them back,
            // or restart rendering if the old daemon already stepped aside.
            if let Some(previous) = &previous {
                let _ = std::fs::write(&owner, previous);
            }
            let _ = ensure_daemon(Some(client).filter(|c| !c.is_empty()));
            return Err(std::io::Error::other("Rex sidebar reload did not become ready").into());
        }
        Ok(())
    })();
    report(result)
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
    fn render_geometry_is_actual_and_measurement_never_calibrates_a_split() {
        let rex = Rex {
            listing: geometry_listing(),
            ..Rex::default()
        };
        for _ in 0..3 {
            let panes = rex.measure_sidebars();
            assert_eq!(
                panes.iter().map(|p| (p.w, p.h)).collect::<Vec<_>>(),
                [(34, 46), (24, 18)]
            );
            assert!(panes
                .iter()
                .all(|p| !p.settled && p.width_drag == Some(false)));
            assert!(rex.layout_state.is_null());
        }
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
