// The Rex backend. Rex sessions hold windows, windows hold blocks; terminal
// blocks are the panes. Every call forks the `rex` CLI, which honours
// REX_SERVER. Placement and sidebar width belong to `rex/place.lua`: Rex sizes
// splits by ratio, so only a feedback loop against each terminal's reported
// columns lands a sidebar on a width in cells.

use super::{
    Changes, Error, Identity, Ids, Kind, Multiplexer, New, Owner, SidebarPane, WindowMeta,
};
use crate::app_config::{AppConfig, ConfigError, DisplayMode, PopupHeight, SidebarPosition};
use crate::focus::{Client, ClientFocus};
use crate::scan::{PaneMeta, ParsedPane};
use crate::snapshot::{Closed, Live, Resume, Snapshot};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const TERMINAL: &str = "com.superlogical.terminal";
const SHELL: &str = "com.superlogical.terminal.shell";
/// Label of the sidebar blocks agenmux places; scans skip them like tmux's
/// own sidebar panes.
const SIDEBAR: &str = "agenmux";
/// The floating popup's block: a sidebar that closes after one jump.
const POPUP: &str = "agenmux-popup";
const SCAN: &str = include_str!("rex/scan.lua");
const PLACE: &str = include_str!("rex/place.lua");
const POPUP_LAYER: &str = include_str!("rex/popup.lua");
/// The dragged sidebar width, in cells.
const WIDTH_FILE: &str = "rex-sidebar-width";
/// `generation|client` of the daemon that owns the sidebars.
const OWNER_FILE: &str = "agenmux-owner";

/// The `rex` CLI. Actions run from init.lua get the server's bare PATH, so
/// fall back to the CLI shipped beside the server, then the app's install.
fn rex_cli() -> &'static Path {
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

/// The rex CLI is the client: never start a server just to ask it something.
fn rex() -> Command {
    let mut command = Command::new(rex_cli());
    command.arg("--autostart=false").stdin(Stdio::null());
    command
}

/// A Rex server answers.
pub fn running() -> bool {
    rex()
        .arg("whoami")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The shell of the block this process says it runs in (`REX_BLOCK`).
pub fn block_pid() -> Option<u32> {
    let session = std::env::var("REX_SESSION").ok()?;
    let block = own_pane()?;
    let request = json!({"session_id": session, "block_id": block});
    let process: Process =
        serde_json::from_value(call(Some(&session), &format!("{TERMINAL}.process"), request).ok()?)
            .ok()?;
    process.child.map(|child| child.pid)
}

/// `REX_BLOCK`, when it is a block id: it names a frame FIFO.
pub fn own_pane() -> Option<String> {
    std::env::var("REX_BLOCK").ok().filter(|id| valid_id(id))
}

pub fn valid_id(id: &str) -> bool {
    id.split_once(':').is_some_and(|(kind, rest)| {
        !kind.is_empty()
            && kind.bytes().all(|b| b.is_ascii_lowercase())
            && !rest.is_empty()
            && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn run(args: &[&str]) -> Result<String, Error> {
    let output = rex().args(args).output()?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Failed(err.trim_end().to_string()));
    }
    String::from_utf8(output.stdout).map_err(|e| Error::Failed(e.to_string()))
}

fn json_of<T: DeserializeOwned>(args: &[&str]) -> Result<T, Error> {
    serde_json::from_str(&run(args)?).map_err(|e| Error::Failed(format!("{}: {e}", args[0])))
}

/// A server API method; `session` scopes it where the method needs one.
fn call(session: Option<&str>, method: &str, request: Value) -> Result<Value, Error> {
    let request = request.to_string();
    let mut args = vec!["api", "call"];
    if let Some(session) = session {
        args.extend(["-s", session]);
    }
    args.extend([method, request.as_str()]);
    Ok(serde_json::from_str(&run(&args)?).unwrap_or(Value::Null))
}

fn view(session: &str) -> Result<View, Error> {
    json_of(&["api", "call", "-s", session, "session.view"])
}

fn sessions() -> Result<Vec<String>, Error> {
    let listed = call(None, "session.list", json!({}))?;
    Ok(listed["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["session_id"].as_str().map(str::to_string))
        .collect())
}

/// Every session's windows and terminal blocks, in one `rex do`.
fn scan() -> Result<Listing, Error> {
    json_of(&["do", "-e", SCAN, "--json"])
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
    #[serde(default)]
    status: ProgramStatus,
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
    /// The state the program reports, the most urgent of its records. Rex
    /// drops a record when the program that wrote it exits.
    fn reported(&self) -> Option<&'static str> {
        let states = self
            .status
            .records
            .iter()
            .filter_map(|r| match r.state.as_str() {
                "idle" | "done" | "error" => Some((0, "idle")),
                "working" => Some((1, "working")),
                "blocked" => Some((2, "blocked")),
                _ => None,
            });
        states.max().map(|(_, state)| state)
    }
}

/// `program_status`: one record per reporter, the program and its subagents.
#[derive(Deserialize, Default)]
struct ProgramStatus {
    #[serde(default, deserialize_with = "list")]
    records: Vec<StatusRecord>,
}

#[derive(Deserialize)]
struct StatusRecord {
    #[serde(default)]
    state: String,
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
}

/// `session.view`, for calls that need no process or size.
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
    fn spawn(blocking: bool) -> Option<Events> {
        let mut child = rex()
            .args(["events", "--all", "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let out = child.stdout.take()?;
        if !blocking {
            let fd = out.as_raw_fd();
            // SAFETY: fcntl on a descriptor this process owns.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        Some(Events {
            child,
            out,
            partial: Vec::new(),
        })
    }

    /// Fold what has arrived into `changes`; false once the stream ended.
    fn read(&mut self, changes: &mut Changes, own: &HashSet<String>) -> bool {
        let mut buf = [0u8; 8192];
        let mut open = true;
        loop {
            match self.out.read(&mut buf) {
                Ok(0) => {
                    open = false;
                    break;
                }
                Ok(n) => {
                    self.partial.extend_from_slice(&buf[..n]);
                    if !self.partial.contains(&b'\n') {
                        continue;
                    }
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    open = e.kind() == std::io::ErrorKind::WouldBlock;
                    break;
                }
            }
        }
        while let Some(end) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            observe(changes, &line, own);
        }
        open
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Fold one event line into `changes`. `own`: agenmux's sidebar and popup
/// blocks. Their events change nothing a sidebar shows, and a popup opening
/// sends a burst of them.
fn observe(changes: &mut Changes, line: &[u8], own: &HashSet<String>) {
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
                        Some("child_exited") => changes.layout = true,
                        Some(_) => {
                            if let Some(block) = payload["block_id"].as_str() {
                                changes.panes.insert(block.to_string());
                            }
                        }
                        None => {}
                    }
                }
                // layout, labels, focus: session_view_changed follows them all
                Some(_) => {
                    changes.layout = true;
                    changes.focus = true;
                }
                None => {}
            }
        }
        Some(kind) if kind.starts_with("client_") => {}
        Some(_) => changes.full = true,
        None => {}
    }
}

#[derive(Default)]
pub struct Rex {
    listing: Listing,
    /// block -> (session, window) from the last listing: block targets
    /// resolve only inside their session.
    places: HashMap<String, (String, String)>,
    monitoring: bool,
    events: Option<Events>,
    pending: Changes,
    /// Each session's active window and its focused block, at the last scan.
    foci: HashMap<String, (Option<String>, Option<String>)>,
    /// The session whose focus moved last: where the user acts.
    acted: Option<String>,
    /// Last app preference observed; unchanged persisted data must not erase
    /// live focus.
    shown: Option<String>,
    /// Opaque state owned by place.lua.
    layout_state: Value,
    layout_started: Option<Instant>,
    /// The sidebar width place.lua works towards, in cells.
    width: usize,
    /// agenmux's own blocks at the last scan, whose events `observe` skips.
    own_blocks: HashSet<String>,
    /// Per window, the last real block focused: a focused sidebar's cursor row.
    beside: HashMap<String, String>,
    /// The launching block identifies the initial session before sidebar focus.
    origin: Option<String>,
}

impl Rex {
    pub fn connect(monitoring: bool) -> Result<Rex, Error> {
        if !running() {
            return Err(Error::Exited);
        }
        Ok(Rex {
            monitoring,
            origin: std::env::var("AGENMUX_REX_ORIGIN").ok(),
            events: if monitoring {
                Events::spawn(false)
            } else {
                None
            },
            pending: Changes {
                full: true,
                ..Changes::default()
            },
            width: sidebar_width(),
            ..Rex::default()
        })
    }

    /// `block`'s session and window, looking past the last listing for blocks
    /// created since.
    fn locate(&self, block: &str) -> Option<(String, String)> {
        if let Some(place) = self.places.get(block) {
            return Some(place.clone());
        }
        locate(block)
    }

    fn block_cwd(&self, block: &str) -> Option<String> {
        self.blocks()
            .find(|(_, _, b)| b.block_id == block)
            .and_then(|(_, _, b)| b.cwd())
            .map(str::to_string)
    }

    fn blocks(&self) -> impl Iterator<Item = (&Session, &Window, &Block)> {
        self.listing.sessions.iter().flat_map(|s| {
            s.windows
                .iter()
                .flat_map(move |w| w.blocks.iter().map(move |b| (s, w, b)))
        })
    }

    fn adopt(&mut self, listing: Listing) {
        self.listing = listing;
        self.own_blocks = self
            .blocks()
            .filter(|(_, _, b)| b.agenmux())
            .map(|(_, _, b)| b.block_id.clone())
            .collect();
    }

    /// Observe app selection changes and live server focus changes. The app's
    /// persisted record may lag; rereading the same record is not a new
    /// selection.
    fn update_acting(&mut self, shown: Option<String>) {
        if let Some(shown) = shown {
            if self.shown.as_ref() != Some(&shown)
                && self.listing.sessions.iter().any(|s| s.session_id == shown)
            {
                self.acted = Some(shown.clone());
            }
            self.shown = Some(shown);
        }
        // The app's saved selection can still name another session when the
        // toggle launches us: the launching block decides the first scan.
        if self.foci.is_empty() {
            let origin = self.origin.as_deref();
            let seeded = self
                .blocks()
                .find(|(_, _, b)| origin == Some(b.block_id.as_str()) && !b.agenmux())
                .map(|(s, _, _)| s.session_id.clone());
            if seeded.is_some() {
                self.acted = seeded;
            }
        }
        let mut moved = None;
        for s in &self.listing.sessions {
            let window = current_window(s);
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
        let live = |id: &String| self.listing.sessions.iter().any(|s| &s.session_id == id);
        self.foci.retain(|id, _| live(id));
        if self.acted.as_ref().is_none_or(|id| !live(id)) {
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

    /// The real block a session or window header, or a block, stands for.
    fn target(&self, id: &str) -> Option<(String, String, String)> {
        // a session goes to its current window, a window to its focused block
        let header = self.listing.sessions.iter().find_map(|s| {
            let window = if id == s.session_id {
                s.active_window_id.as_deref()?
            } else {
                id
            };
            let window = s.windows.iter().find(|w| w.window_id == window)?;
            let mut real = window.blocks.iter().filter(|b| !b.agenmux());
            let block = real
                .clone()
                .find(|b| window.focused_block_id.as_ref() == Some(&b.block_id))
                .or_else(|| real.next())?;
            Some((
                s.session_id.clone(),
                window.window_id.clone(),
                block.block_id.clone(),
            ))
        });
        header.or_else(|| {
            let (session, window) = self.locate(id)?;
            Some((session, window, id.to_string()))
        })
    }

    /// Run place.lua's reconcile: it places sidebars in windows that lack
    /// one, closes windows left holding only a sidebar, moves dividers toward
    /// `width`, and reports each sidebar's actual size.
    fn reconcile(&mut self) -> Result<Vec<SidebarPane>, Error> {
        #[derive(Deserialize)]
        struct Reconciled {
            state: Value,
            listing: Listing,
            #[serde(default, deserialize_with = "list")]
            panes: Vec<Measured>,
            saved_width: Option<usize>,
        }
        #[derive(Deserialize)]
        struct Measured {
            pane: String,
            win: String,
            sess: String,
            w: usize,
            h: usize,
            #[serde(default, deserialize_with = "list")]
            win_size: Vec<usize>,
            panes: usize,
            active: bool,
        }
        let session = self.acting().map(|s| s.session_id.clone());
        let args = json!({
            "op": "reconcile",
            "state": if self.layout_state.is_object() { self.layout_state.clone() } else { json!({}) },
            "width": self.width,
            "now": self.layout_started.get_or_insert_with(Instant::now).elapsed().as_secs_f64(),
            "session": session,
            "exe": sidebar_command(),
            "side": side(),
        });
        let script =
            format!("local scan = function()\n{SCAN}\nend\nrex.args.listing = scan()\n{PLACE}");
        let out: Reconciled =
            json_of(&["do", "-e", &script, "--args", &args.to_string(), "--json"])?;
        self.layout_state = out.state;
        self.adopt(out.listing);
        if let Some(width) = out.saved_width.filter(|w| (1..=10000).contains(w)) {
            self.width = width;
            let _ = std::fs::write(state_file(WIDTH_FILE), format!("{width}\n"));
        }
        Ok(out
            .panes
            .into_iter()
            .map(|m| SidebarPane {
                pane: m.pane,
                win: m.win,
                sess: m.sess,
                w: m.w,
                h: m.h,
                win_size: (
                    m.win_size.first().copied().unwrap_or(m.w),
                    m.win_size.get(1).copied().unwrap_or(m.h),
                ),
                panes: m.panes,
                active: m.active,
                self_sized: true,
            })
            .collect())
    }
}

/// `block`'s session and window, from a fresh view of every session.
fn locate(block: &str) -> Option<(String, String)> {
    sessions().ok()?.into_iter().find_map(|session| {
        let view = view(&session).ok()?;
        let window = view
            .windows
            .iter()
            .find(|w| w.blocks().any(|b| b.block_id == block))?;
        Some((session.clone(), window.window_id.clone()))
    })
}

/// A command for the app client this sidebar acts for, or, once that client
/// is gone (the app restarted), for whichever one Rex picks: `do` picks the
/// app client when exactly one is connected.
fn on_client(args: &[&str]) -> bool {
    if let Some(client) = client() {
        let mut targeted = vec!["-C", client.as_str()];
        targeted.extend(args);
        if run(&targeted).is_ok() {
            return true;
        }
    }
    run(args).is_ok()
}

fn focus_block(session: &str, window: &str, block: &str) -> bool {
    // focus moves the session's active window; the app shows another session
    // or window once its client is told to go there
    on_client(&["focus", "-s", session, block])
        && on_client(&[
            "do",
            "session.select",
            &format!("session_id={session}"),
            &format!("window_id={window}"),
        ])
}

fn new_window(session: &str, label: &str, block: Value) -> Result<String, Error> {
    first_block(&create_window(session, label, block)?["block_ids"])
}

/// `session.new_window`: its `window_id` and `block_ids`.
fn create_window(session: &str, label: &str, layout: Value) -> Result<Value, Error> {
    let mut request = json!({"session_id": session, "focus": false, "layout": layout});
    if !label.is_empty() {
        request["window_label"] = label.into();
    }
    call(Some(session), "session.new_window", request)
}

/// Move a restored window back to its saved place in the tab order.
fn place_window(session: &str, window: &str, index: u32) {
    let Ok(view) = view(session) else {
        return;
    };
    let at = (index as usize).saturating_sub(1);
    if let Some(before) = view.windows.get(at).filter(|w| w.window_id != window) {
        let request = json!({
            "session_id": session,
            "window_id": window,
            "before_window_id": before.window_id,
        });
        let _ = call(Some(session), "session.move_window", request);
    }
}

/// Type the agent's resume command into its restored shell.
fn resume_agent(session: &str, block: &str, pane: &crate::snapshot::Pane, resume: &Resume) {
    if let Some(command) = pane.agent.as_deref().and_then(|agent| resume.get(agent)) {
        let _ = run(&["send", "-s", session, "-b", block, &format!("{command}\r")]);
    }
}

fn first_block(ids: &Value) -> Result<String, Error> {
    ids[0]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Failed("rex created no block".into()))
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

/// The session's current window.
fn current_window(s: &Session) -> Option<&Window> {
    s.windows
        .iter()
        .find(|w| Some(&w.window_id) == s.active_window_id.as_ref())
}

/// Which side of a window the sidebar takes, as `session.new_split` names it.
fn side() -> &'static str {
    if config().is_some_and(|c| c.sidebar_position == SidebarPosition::Right) {
        "after"
    } else {
        "before"
    }
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

/// The app client that opened this sidebar, from Rex's `ctx.client_id`:
/// navigation then reaches the window the user is in.
fn client() -> Option<String> {
    std::env::var("AGENMUX_REX_CLIENT")
        .ok()
        .filter(|client| !client.is_empty())
}

fn config() -> Option<AppConfig> {
    crate::app_config::load()
        .and_then(|file| crate::app_config::resolve(&file, &options()))
        .ok()
}

/// The sidebar width in cells: the last dragged one, else the configured.
fn sidebar_width() -> usize {
    config().map_or(30, |c| c.sidebar_width as usize)
}

/// What a sidebar block runs. Blocks start with the server's environment, so
/// the server and runtime dir this process uses go with it.
fn sidebar_command() -> Vec<String> {
    let mut command = vec!["env".to_string()];
    for name in [
        "REX_SERVER",
        "AGENMUX_REX",
        "AGENMUX_RUNTIME_DIR",
        "XDG_STATE_HOME",
    ] {
        if let Ok(value) = std::env::var(name) {
            command.push(format!("{name}={value}"));
        }
    }
    command.push("AGENMUX_MUX=rex".into());
    if let Some(client) = client() {
        command.push(format!("AGENMUX_REX_CLIENT={client}"));
    }
    let exe = std::env::current_exe().map_or("agenmux".into(), |p| p.display().to_string());
    command.extend([exe, "sidebar-pane".into()]);
    command
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

/// The sidebar is the window's rightmost block.
fn sidebar_right(window: &Window) -> bool {
    let mut edge = &window.layout;
    while !edge["split"].is_null() {
        edge = &edge["split"]["after"];
    }
    let block = edge["block_id"].as_str();
    window
        .blocks
        .iter()
        .any(|b| b.label == SIDEBAR && Some(b.block_id.as_str()) == block)
        && window.blocks.len() > 1
}

impl Multiplexer for Rex {
    fn panes(&mut self, skip: Option<&str>) -> Result<Vec<ParsedPane>, Error> {
        if self.monitoring && self.events.is_none() {
            self.events = Events::spawn(false);
        }
        let listing = scan().map_err(|e| if running() { e } else { Error::Exited })?;
        self.adopt(listing);
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
                    if skip == Some(block.block_id.as_str()) {
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
                        meta: PaneMeta {
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
                        reported: block.reported(),
                    });
                }
            }
        }
        Ok(panes)
    }

    fn capture(&mut self, pane: &str) -> Result<String, Error> {
        let (session, _) = self.locate(pane).unwrap_or_default();
        let rows = self
            .blocks()
            .find(|(_, _, b)| b.block_id == pane)
            .map_or(0, |(_, _, b)| b.size.rows);
        let text = run(&["capture", "-s", &session, "-b", pane])?;
        // capture prints scrollback + screen; the last `rows` lines are the
        // screen, which is all tmux's capture-pane returns
        let lines: Vec<&str> = text.lines().collect();
        let screen = &lines[lines.len().saturating_sub(rows)..];
        Ok(screen.iter().map(|l| format!("{l}\n")).collect())
    }

    /// Rex clients do not say which session they show: what the acting
    /// session's active window focuses is what the user acts in.
    fn focus(&mut self) -> Option<ClientFocus> {
        let origin = self.origin.clone();
        // a popup follows its own session; the daemon, where the user acts
        let session = match std::env::var("REX_SESSION").ok() {
            Some(own) => self
                .listing
                .sessions
                .iter()
                .find(|s| s.session_id == own)
                .or(self.listing.sessions.first())?,
            None => self.acting()?,
        };
        let window = current_window(session)?;
        let mut block = window.focused_block_id.as_deref()?;
        // a popup has the focus it floats over: what the user acts on is the
        // block it opened from, as under a tmux popup
        let over = std::env::var("AGENMUX_REX_OVER").ok();
        if std::env::var("REX_BLOCK").ok().as_deref() == Some(block) {
            if let Some(over) = over
                .as_deref()
                .filter(|over| window.blocks.iter().any(|b| b.block_id == *over))
            {
                block = over;
            }
        }
        let on_sidebar = window
            .blocks
            .iter()
            .any(|b| b.block_id == block && b.agenmux());
        let real: Vec<String> = window
            .blocks
            .iter()
            .filter(|b| !b.agenmux())
            .map(|b| b.block_id.clone())
            .collect();
        let origin = origin.filter(|origin| real.contains(origin));
        let (session, window, block) = (
            session.session_id.clone(),
            window.window_id.clone(),
            block.to_string(),
        );
        // Rex keeps no focus history: a focused sidebar's cursor stays on the
        // block last focused beside it, or the one the toggle opened from.
        let pane = if on_sidebar {
            self.beside
                .get(&window)
                .filter(|id| real.contains(id))
                .cloned()
                .or(origin)
                .or_else(|| real.into_iter().next())
                .unwrap_or(block)
        } else {
            self.beside.insert(window, block.clone());
            block
        };
        Some(ClientFocus::from_clients(&[Client {
            activity: 1,
            name: "rex".into(),
            session,
            pane,
            on_sidebar,
            focused: true,
        }]))
    }

    fn events_fd(&self) -> Option<RawFd> {
        self.events.as_ref().map(|e| e.out.as_raw_fd())
    }
    /// Whole lines are drained: nothing waits in a buffer.
    fn events_buffered(&self) -> bool {
        false
    }
    fn drain_events(&mut self) -> Result<bool, Error> {
        let focus_before = self.pending.focus;
        let Some(events) = self.events.as_mut() else {
            return Ok(false);
        };
        if !events.read(&mut self.pending, &self.own_blocks) {
            // respawned by the next scan; polling covers the gap
            self.events = None;
            self.pending.full = true;
        }
        Ok(!focus_before && self.pending.focus)
    }
    fn changes(&self) -> &Changes {
        &self.pending
    }
    fn take_changes(&mut self) -> Changes {
        std::mem::take(&mut self.pending)
    }
    fn defer_output(&mut self, panes: HashSet<String>) {
        self.pending.panes.extend(panes);
    }
    /// Events cover every session.
    fn covered_session(&self) -> Option<&str> {
        None
    }
    fn follow_session(&mut self, _session: &str) -> bool {
        true
    }

    fn is_live(&mut self, kind: Option<Kind>, ids: Ids) -> bool {
        if kind == Some(Kind::Session) {
            return sessions().is_ok_and(|s| s.iter().any(|id| id == ids.session));
        }
        let Ok(view) = view(ids.session) else {
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

    fn name(&mut self, kind: Kind, ids: Ids) -> Result<String, Error> {
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
        name.ok_or_else(|| Error::Failed("target changed; nothing was modified".into()))
    }

    fn rename(&mut self, kind: Kind, ids: Ids, name: &str) -> Result<(), Error> {
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
        call(Some(ids.session), method, request).map(drop)
    }

    fn delete(&mut self, kind: Kind, ids: Ids) -> Result<(), Error> {
        match kind {
            Kind::Session => {
                if sessions()?.len() <= 1 {
                    return Err(Error::Failed(
                        "cannot delete the last Rex session; nothing was modified".into(),
                    ));
                }
                call(None, "session.destroy", json!({"session_id": ids.session}))
            }
            Kind::Window => {
                if view(ids.session)?.windows.len() <= 1 {
                    return Err(Error::Failed(
                        "cannot delete the last window in a Rex session; delete the session explicitly"
                            .into(),
                    ));
                }
                let request = json!({"session_id": ids.session, "window_id": ids.window});
                call(Some(ids.session), "session.close_window", request)
            }
            Kind::Pane => {
                let view = view(ids.session)?;
                if view.windows.iter().map(ViewWindow::real_blocks).sum::<usize>() <= 1 {
                    return Err(Error::Failed(
                        "cannot delete the last pane in a Rex session; delete the session explicitly"
                            .into(),
                    ));
                }
                let request = json!({"session_id": ids.session, "block_id": ids.pane});
                call(Some(ids.session), "block.close", request)
            }
        }
        .map(drop)
    }

    fn pane_cwd(&mut self, ids: Ids) -> Result<String, Error> {
        if !self.is_live(None, ids) {
            return Err(Error::Failed(
                "selected pane changed before launcher start".into(),
            ));
        }
        self.block_cwd(ids.pane)
            .ok_or_else(|| Error::Failed("selected pane has no working directory".into()))
    }

    /// Rex sessions have no directory of their own: the first pane's stands in.
    fn session_cwd(&mut self, session: &str) -> Result<String, Error> {
        self.blocks()
            .filter(|(s, _, b)| s.session_id == session && !b.agenmux())
            .find_map(|(_, _, b)| b.cwd())
            .map(str::to_string)
            .or_else(|| std::env::var("HOME").ok())
            .ok_or_else(|| Error::Failed("target session has no working directory".into()))
    }

    fn create(&mut self, new: New) -> Result<String, Error> {
        match new {
            New::Session { name, cwd } => {
                let mut request =
                    json!({"initial_windows": [{"layout": terminal(cwd, "shell", None)}]});
                if !name.is_empty() {
                    request["label"] = name.into();
                }
                let out = call(None, "session.create", request)?;
                first_block(&out["initial_windows"][0]["block_ids"])
            }
            New::Window { session, name, cwd } => {
                new_window(session, name, terminal(cwd, "shell", None))
            }
            New::Launch { session, cwd, argv } => {
                let label = argv.first().copied().unwrap_or("shell");
                new_window(session, "", terminal(cwd, label, Some(argv)))
            }
        }
    }

    fn jump(&mut self, pane: &str) -> Result<(), Error> {
        let (session, window, block) = self
            .target(pane)
            .ok_or_else(|| Error::Failed(format!("{pane} has no Rex location")))?;
        if focus_block(&session, &window, &block) {
            Ok(())
        } else {
            Err(Error::Failed(format!("could not focus {block}")))
        }
    }

    fn show(&mut self, _client: &str, pane: &str, stay: bool) -> Result<String, Error> {
        let (session, window) = self
            .locate(pane)
            .ok_or_else(|| Error::Failed("created target has no Rex location".into()))?;
        focus_block(&session, &window, pane);
        if stay {
            self.focus_sidebar("", Some(&window));
        }
        Ok(window)
    }

    /// `expected_window` may be in another session, and a window just created
    /// gets its sidebar from the daemon's next reconcile: wait for it briefly.
    /// With no window, the sidebar where the user acts.
    fn focus_sidebar(&mut self, _client: &str, expected_window: Option<&str>) {
        let Some(expected) = expected_window else {
            let own = std::env::var("REX_BLOCK").ok().or_else(|| {
                current_window(self.acting()?)?
                    .blocks
                    .iter()
                    .find(|b| b.label == SIDEBAR)
                    .map(|b| b.block_id.clone())
            });
            if let Some((session, window)) = own.as_deref().and_then(|own| self.locate(own)) {
                focus_block(&session, &window, own.as_deref().unwrap_or_default());
            }
            return;
        };
        let deadline = Instant::now() + Duration::from_millis(1500);
        loop {
            let found = sessions()
                .unwrap_or_default()
                .into_iter()
                .find_map(|session| {
                    let view = view(&session).ok()?;
                    let window = view.windows.into_iter().find(|w| w.window_id == expected)?;
                    Some((session, window))
                });
            let Some((session, window)) = found else {
                return;
            };
            if let Some(sidebar) = window.sidebar() {
                if window.focused_block_id.as_deref() != Some(sidebar) {
                    focus_block(&session, &window.window_id, sidebar);
                }
                return;
            }
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Rex has no message line: the sidebar shows it.
    fn message(&mut self, _client: &str, _text: &str) -> bool {
        false
    }

    /// The sidebar's own terminal carries the clipboard escape to the app.
    fn copy(&mut self, _client: Option<&str>, text: &str) -> Result<Option<String>, Error> {
        Ok(Some(format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))))
    }

    fn reference(&self, kind: Kind, id: &str) -> String {
        let kind = match kind {
            Kind::Session => "session",
            Kind::Window => "window",
            Kind::Pane => "block",
        };
        format!("rex {kind} {id}")
    }

    /// The server's instance id changes when it restarts; one file per host.
    fn identity(&mut self) -> Option<Identity> {
        let status = call(None, "server.status", json!({})).ok()?;
        let server = status["instance_id"].as_str()?.to_string();
        let host = status["host_id"].as_str().unwrap_or("local");
        Some(Identity {
            server,
            name: format!("rex-{host}"),
        })
    }

    /// The block count includes the sidebar, so the snapshot sees it; a label
    /// Rex made up follows the window the way tmux's automatic-rename does.
    fn windows(&mut self) -> Result<Vec<WindowMeta>, Error> {
        Ok(self
            .listing
            .sessions
            .iter()
            .flat_map(|s| &s.windows)
            .map(|w| WindowMeta {
                id: w.window_id.clone(),
                panes: w.blocks.len(),
                automatic_rename: w.label.starts_with("Window "),
                sidebar_right: sidebar_right(w),
                layout: w.layout.to_string(),
            })
            .collect())
    }

    fn live(&mut self) -> Live {
        let mut live = Live {
            by_id: true,
            ..Live::default()
        };
        let Ok(listing) = scan() else {
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
    fn restore(&mut self, snap: &Snapshot, live: &Live, resume: &Resume) {
        let labels: Vec<(String, String)> = call(None, "session.list", json!({}))
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
                    Some(id) => create_window(id, label, layout).map(|out| (id.clone(), out)),
                    None => {
                        let mut first = json!({"layout": layout});
                        if !label.is_empty() {
                            first["window_label"] = label.into();
                        }
                        let request = json!({"label": session.name, "initial_windows": [first]});
                        call(None, "session.create", request).map(|out| {
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
                    place_window(&session_id, window_id, window.index);
                }
                let blocks = out["block_ids"].as_array().into_iter().flatten();
                for (block, pane) in blocks.filter_map(Value::as_str).zip(panes) {
                    resume_agent(&session_id, block, pane, resume);
                }
            }
        }
    }

    fn restore_pane(&mut self, entry: &Closed, live: &Live, resume: &Resume) {
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
        if let Ok(block) = call(Some(&session), "session.new_split", request)
            .and_then(|out| first_block(&out["block_ids"]))
        {
            resume_agent(&session, &block, pane, resume);
        }
    }

    fn options(&mut self) -> Result<BTreeMap<String, String>, ConfigError> {
        Ok(options())
    }

    fn reload_token(&mut self) -> Option<String> {
        std::fs::read_to_string(state_file("reload-token"))
            .ok()
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
    }

    fn publish_reload(&mut self, token: &str) -> Result<(), Error> {
        publish_reload(token)
    }

    fn sidebar_panes(&mut self) -> Result<Vec<SidebarPane>, Error> {
        self.reconcile()
    }

    /// Rex does not say which session an app window shows: every session's
    /// current window counts.
    fn visible_windows(&mut self) -> HashSet<String> {
        self.listing
            .sessions
            .iter()
            .filter_map(|s| s.active_window_id.clone())
            .collect()
    }

    /// place.lua moves the dividers on its next reconcile.
    fn resize_panes(&mut self, cols: &[(String, usize)]) -> bool {
        if let Some(width) = cols.iter().map(|(_, w)| *w).max() {
            self.width = width;
        }
        true
    }

    fn close_panes(&mut self, panes: &[String]) {
        for block in panes {
            if let Some((session, _)) = self.locate(block) {
                let request = json!({"session_id": session, "block_id": block});
                let _ = call(Some(&session), "block.close", request);
            }
        }
    }

    fn close_sidebars(&mut self) {
        close_sidebars();
    }

    fn remember_width(&mut self, cols: usize) -> bool {
        self.width = cols;
        std::fs::write(state_file(WIDTH_FILE), format!("{cols}\n")).is_ok()
    }

    fn connection(&mut self) -> Option<String> {
        Some(format!("pid:{}", std::process::id()))
    }

    fn owner(&mut self) -> Option<Owner> {
        owner()
    }

    fn claim(&mut self, client: &str, runtime: &Path) -> Result<(), Error> {
        let generation = std::env::var("AGENMUX_GENERATION").unwrap_or_default();
        std::fs::write(runtime.join(OWNER_FILE), format!("{generation}|{client}\n"))?;
        Ok(())
    }

    fn release(&mut self, generation: Option<&str>) {
        if generation.is_none_or(|g| owner().is_some_and(|o| o.generation == g)) {
            let _ = std::fs::remove_file(runtime_dir().join(OWNER_FILE));
        }
    }
}

/// Where the daemon, its sidebar blocks and key senders meet: one per user
/// and server.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("AGENMUX_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
    {
        return dir;
    }
    let uid = unsafe { libc::geteuid() };
    let name = match std::env::var("REX_SERVER") {
        Ok(server) if !server.is_empty() => {
            // FNV-1a: stable across releases, unlike the std hasher
            let hash = server.bytes().fold(0xcbf29ce484222325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
            });
            format!("agenmux-rex-{uid}-{hash:016x}")
        }
        _ => format!("agenmux-rex-{uid}"),
    };
    std::env::temp_dir().join(name)
}

/// The daemon that owns the sidebars, from its record file.
pub fn owner() -> Option<Owner> {
    let record = std::fs::read_to_string(runtime_dir().join(OWNER_FILE)).unwrap_or_default();
    let (generation, client) = record.trim_end().split_once('|').unwrap_or_default();
    Some(Owner {
        generation: generation.to_string(),
        client: client.to_string(),
    })
}

/// Rex has no options: a dragged width is a state file.
pub fn options() -> BTreeMap<String, String> {
    std::fs::read_to_string(state_file(WIDTH_FILE))
        .ok()
        .map(|width| ("@agenmux-width".to_string(), width.trim().to_string()))
        .into_iter()
        .collect()
}

/// `config reload`: the token sidebars watch is a state file.
pub fn publish_reload(token: &str) -> Result<(), Error> {
    let path = state_file("reload-token");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, token)?;
    Ok(())
}

/// The live daemon its owner record names.
fn daemon_pid() -> Option<libc::pid_t> {
    let pid = owner()?.client.strip_prefix("pid:")?.parse().ok()?;
    (unsafe { libc::kill(pid, 0) } == 0).then_some(pid)
}

/// The daemon, run in no block: closing the block that asked must not end it.
fn daemon_command(origin: Option<&str>) -> std::io::Result<Command> {
    use std::os::unix::process::CommandExt;
    let mut daemon = Command::new(std::env::current_exe()?);
    daemon
        .arg("daemon")
        .env("AGENMUX_MUX", "rex")
        .env_remove("AGENMUX_GENERATION")
        .env_remove("REX_BLOCK")
        .env_remove("REX_SESSION")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(origin) = origin {
        daemon.env("AGENMUX_REX_ORIGIN", origin);
    }
    Ok(daemon)
}

/// Start the daemon unless its owner record names a live one. It places the
/// sidebars. Two racing starts are fine: the later claim wins and the earlier
/// daemon leaves.
fn ensure_daemon(origin: Option<&str>) -> Result<(), Error> {
    if daemon_pid().is_none() {
        daemon_command(origin)?.spawn()?;
    }
    Ok(())
}

/// Split the whole tiled layer (its top split, or its only block) so the
/// sidebar spans the window's full height. `focus`: the user opened it.
fn add_sidebar(session: &str, window: &ViewWindow, focus: bool) -> Result<(), Error> {
    let layout = &window
        .tiled()
        .ok_or_else(|| Error::Failed("window has no tiled layer".into()))?
        .layout;
    let anchor = layout["split"]["split_id"]
        .as_str()
        .or(layout["block_id"].as_str())
        .ok_or_else(|| Error::Failed("empty layout".into()))?;
    let side = side();
    // the border is set against the outermost block on the sidebar's side
    let mut edge = layout;
    while !edge["split"].is_null() {
        edge = &edge["split"][side];
    }
    let args = json!({
        "session": session,
        "anchor": anchor,
        "side": side,
        "edge": edge["block_id"].as_str().unwrap_or_default(),
        "share": 0.2,
        "label": SIDEBAR,
        "exe": sidebar_command(),
        "focus": focus,
    });
    run(&["do", "-e", PLACE, "--args", &args.to_string()]).map(drop)
}

fn report(result: Result<(), Error>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agenmux: {e}");
            1
        }
    }
}

/// `rex-toggle <session> <block> [split|popup] [client]`, bound in Rex by
/// `rex/agenmux.lua`: turn the sidebars on from `block`'s window or off
/// everywhere, or show the popup over it. `display.mode = "popup"` makes the
/// sidebar key a popup key too, as in tmux.
pub fn toggle(session: &str, block: &str, mode: &str, client: &str) -> i32 {
    if !client.is_empty() {
        std::env::set_var("AGENMUX_REX_CLIENT", client);
    }
    let config = config();
    if mode == "popup"
        || config
            .as_ref()
            .is_some_and(|c| c.mode == DisplayMode::Popup)
    {
        return report(open_popup(session, block, config.as_ref()));
    }
    // on and off everywhere, as in tmux: the daemon's teardown closes every
    // sidebar
    if let Some(daemon) = daemon_pid() {
        unsafe { libc::kill(daemon, libc::SIGTERM) };
        return 0;
    }
    let Ok(view) = view(session) else {
        return 1;
    };
    let Some(window) = view
        .windows
        .iter()
        .find(|w| w.blocks().any(|b| b.block_id == block))
    else {
        return 1;
    };
    report(
        ensure_daemon(Some(block).filter(|b| !b.is_empty()))
            .and_then(|()| add_sidebar(session, window, true)),
    )
}

/// What `popup.lua` decided.
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
/// one, or open one sized by `display.popup_width` and `popup_height`. Two
/// `rex` calls: a layer created from the attached script is never drawn.
fn open_popup(session: &str, block: &str, config: Option<&AppConfig>) -> Result<(), Error> {
    let width = config.map_or(40, |c| c.popup_width);
    let height = match config.map(|c| c.popup_height) {
        Some(PopupHeight::Cells(n)) => n,
        _ => 0,
    };
    let args = json!({
        "session": session,
        "block": block,
        "width": width,
        "height": height,
        "label": POPUP,
    });
    let plan: PopupPlan = json_of(&[
        "do",
        "-e",
        POPUP_LAYER,
        "--args",
        &args.to_string(),
        "--json",
    ])?;
    match plan.action.as_str() {
        "focus" => {
            focus_block(session, &plan.window, &plan.block);
            Ok(())
        }
        "open" => {
            let mut command = sidebar_command();
            command.pop();
            command.push("sidebar".into());
            command.insert(1, "AGENMUX_REX_POPUP=1".into());
            command.insert(1, format!("AGENMUX_REX_OVER={block}"));
            command.insert(1, "AGENMUX_POPUP_CLIENT=rex".into());
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
            call(Some(session), "session.new_layer", request).map(drop)
        }
        _ => Ok(()),
    }
}

/// Close every sidebar and popup. Returns the windows that had a sidebar.
pub fn close_sidebars() -> Vec<(String, String)> {
    let mut closed = Vec::new();
    for session in sessions().unwrap_or_default() {
        let Ok(view) = view(&session) else {
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
                let _ = call(Some(&session), "block.close", request);
            }
        }
    }
    closed
}

/// Put a sidebar back in each window `close_sidebars` emptied, running
/// whichever release is installed now.
pub fn reopen(windows: &[(String, String)]) {
    if windows.is_empty() || ensure_daemon(None).is_err() {
        return;
    }
    for (session, id) in windows {
        let Ok(view) = view(session) else {
            continue;
        };
        let window = view.windows.iter().find(|w| &w.window_id == id);
        if let Some(window) = window.filter(|w| w.sidebar().is_none()) {
            let _ = add_sidebar(session, window, false);
        }
    }
}

/// `rex-reload [client]`: replace the daemon without closing sidebar blocks
/// or changing split ratios. The old daemon sees the new claim and leaves.
pub fn reload(client: &str) -> i32 {
    if !client.is_empty() {
        std::env::set_var("AGENMUX_REX_CLIENT", client);
    }
    if daemon_pid().is_none() {
        eprintln!("agenmux: no Rex sidebar daemon to reload");
        return 1;
    }
    let path = runtime_dir().join(OWNER_FILE);
    let previous = std::fs::read(&path).ok();
    let result = (|| -> Result<(), Error> {
        let mut child = daemon_command(None)?
            .env("AGENMUX_STARTUP_ACK", "1")
            .stdout(Stdio::piped())
            .spawn()?;
        let mut ready = child.stdout.take().expect("piped stdout");
        let mut ack = [0];
        let deadline = Instant::now() + Duration::from_secs(10);
        if !crate::input::poll_by(ready.as_raw_fd(), libc::POLLIN, deadline)
            || ready.read_exact(&mut ack).is_err()
            || ack != *b"R"
        {
            // SIGKILL: SIGTERM would tear down the sidebars it may have claimed
            let _ = child.kill();
            let _ = child.wait();
            if let Some(previous) = &previous {
                let _ = std::fs::write(&path, previous);
            }
            let _ = ensure_daemon(None);
            return Err(Error::Failed("the new daemon did not become ready".into()));
        }
        Ok(())
    })();
    report(result)
}

/// `rex-sidebars`: keep a sidebar daemon running, which keeps a sidebar in
/// every window, as the tmux integration does. Returns once the server is
/// gone.
pub fn sidebars() -> i32 {
    let Some(mut events) = Events::spawn(true) else {
        return 1;
    };
    loop {
        if !running() {
            return 0;
        }
        if daemon_pid().is_none() {
            let first = sessions().ok().and_then(|sessions| {
                sessions.into_iter().find_map(|session| {
                    let view = view(&session).ok()?;
                    let window = view.windows.into_iter().find(|w| w.real_blocks() > 0)?;
                    Some((session, window))
                })
            });
            if let Some((session, window)) = first {
                if let Err(e) =
                    ensure_daemon(None).and_then(|()| add_sidebar(&session, &window, false))
                {
                    eprintln!("agenmux: {e}");
                }
            }
        }
        let mut changes = Changes::default();
        while !changes.layout && !changes.full {
            if !events.read(&mut changes, &HashSet::new()) {
                return 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing() -> Listing {
        serde_json::from_value(json!({"sessions": [{
            "session_id": "session:1", "label": "work", "active_window_id": "window:0",
            "windows": [
                {"window_id": "window:0", "label": "Window 1", "focused_block_id": "shell:0",
                 "layout": {"split": {"direction": "horizontal", "ratio": 0.2,
                    "before": {"block_id": "side:0"}, "after": {"block_id": "shell:0"}}},
                 "blocks": [
                    {"block_id": "side:0", "label": "agenmux", "size": {"columns": 30, "rows": 40}},
                    {"block_id": "shell:0", "label": "shell", "size": {"columns": 120, "rows": 40},
                     "process": {"child": {"pid": 7, "name": "login"},
                                 "foreground": {"pid": 8, "name": "claude-2.1", "argv0": "claude", "cwd": "/w"}},
                     "title": {"title": "✳ Claude Code"}}
                 ]},
                {"window_id": "window:1", "label": "notes", "focused_block_id": "shell:1",
                 "blocks": [{"block_id": "shell:1", "label": "vim"}]}
            ]
        }]}))
        .unwrap()
    }

    #[test]
    fn panes_skip_sidebars_and_read_the_typed_command() {
        let mut rex = Rex::default();
        rex.adopt(listing());
        assert!(rex.own_blocks.contains("side:0"));
        let w = &rex.listing.sessions[0].windows;
        assert!(!sidebar_right(&w[0]));
        assert_eq!(rex.block_cwd("shell:0").as_deref(), Some("/w"));
        assert_eq!(rex.session_cwd("session:1").ok().as_deref(), Some("/w"));
        let target = rex.target("window:1").unwrap();
        assert_eq!(target.2, "shell:1");
        assert_eq!(rex.target("session:1").unwrap().2, "shell:0");
    }

    #[test]
    fn a_focused_sidebar_keeps_the_cursor_on_the_block_beside_it() {
        let mut rex = Rex {
            acted: Some("session:1".into()),
            ..Rex::default()
        };
        rex.adopt(listing());
        let mut focus = |block: &str| {
            rex.listing.sessions[0].windows[0].focused_block_id = Some(block.into());
            let focus = rex.focus().unwrap();
            (focus.active_pane, focus.plugin_selected)
        };
        assert_eq!(focus("shell:0"), ("shell:0".into(), false));
        assert_eq!(focus("side:0"), ("shell:0".into(), true));
    }

    #[test]
    fn unchanged_app_preferences_do_not_erase_live_session_focus() {
        let mut rex = Rex::default();
        rex.adopt(listing());
        rex.listing.sessions.push(
            serde_json::from_value(json!({
                "session_id": "session:2", "active_window_id": "window:9",
                "windows": [{"window_id": "window:9", "blocks": []}]
            }))
            .unwrap(),
        );
        rex.update_acting(Some("session:1".into()));
        assert_eq!(rex.acted.as_deref(), Some("session:1"));
        rex.listing.sessions[1].windows[0].focused_block_id = Some("shell:9".into());
        for _ in 0..3 {
            rex.update_acting(Some("session:1".into()));
            assert_eq!(rex.acted.as_deref(), Some("session:2"));
        }
        // a newly changed app record still wins when focus has not moved
        rex.update_acting(Some("session:2".into()));
        rex.update_acting(Some("session:1".into()));
        assert_eq!(rex.acted.as_deref(), Some("session:1"));
        rex.listing.sessions.remove(0);
        rex.update_acting(Some("session:1".into()));
        assert_eq!(rex.acted.as_deref(), Some("session:2"));
        assert!(!rex.foci.contains_key("session:1"));
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
        // sidebar | (b over a): the sidebar's split drops out, a/b keep theirs
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
        let mut changes = Changes::default();
        let mut see = |line: &str| {
            observe(&mut changes, line.as_bytes(), &own);
            std::mem::take(&mut changes)
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
            r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"block_event","payload":{"block_id":"block:7","name":"title_changed","payload":{}}}}}"#,
        );
        assert!(seen.panes.contains("block:7") && !seen.layout);
        let seen = see(
            r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"session_view_changed","payload":{"revision":3}}}}"#,
        );
        assert!(seen.layout && seen.focus);
        assert!(see(r#"{"type":"session_created","payload":{}}"#).full);
    }

    #[test]
    fn a_block_reports_its_most_urgent_program_status() {
        let block = |status: Value| -> Block {
            serde_json::from_value(json!({"block_id": "block:1", "status": status})).unwrap()
        };
        let records = |states: &[&str]| {
            let records: Vec<Value> = states.iter().map(|s| json!({"state": s})).collect();
            json!({ "records": records })
        };
        assert_eq!(block(json!({})).reported(), None);
        assert_eq!(block(json!({"records": {}})).reported(), None);
        assert_eq!(block(records(&["done"])).reported(), Some("idle"));
        assert_eq!(block(records(&["error", "bogus"])).reported(), Some("idle"));
        assert_eq!(
            block(records(&["idle", "working"])).reported(),
            Some("working")
        );
        assert_eq!(
            block(records(&["working", "blocked"])).reported(),
            Some("blocked")
        );
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
    fn ids_and_references_name_rex_records() {
        assert!(valid_id("block:01a1aa71-aa89-72b3-a53b-0d1a3049ab23"));
        for bad in ["block:", ":x", "block:a/b", "%1", "Block:x"] {
            assert!(!valid_id(bad), "{bad}");
        }
        let rex = Rex::default();
        assert_eq!(
            rex.reference(Kind::Session, "session:1"),
            "rex session session:1"
        );
        assert_eq!(
            rex.reference(Kind::Window, "window:2"),
            "rex window window:2"
        );
        assert_eq!(rex.reference(Kind::Pane, "block:3"), "rex block block:3");
    }
}
