// Multiplexer seam: what the scan, sidebar and daemon need from the server
// hosting the agent panes. tmux and Rex are the backends. Code outside
// src/mux/ never names either: it calls `Multiplexer`, or the functions here
// for work that holds no connection.
#[cfg(test)]
pub mod fake;
pub mod rex;
mod runtime;
pub mod tmux;

pub use runtime::{prepare_dir, prepare_runtime_dir, runtime_dir};

use crate::app_config::{AppConfig, Color, ConfigError};
use crate::focus::ClientFocus;
use crate::scan::ParsedPane;
use crate::snapshot::{Closed, Live, Resume, Snapshot};
use std::collections::{BTreeMap, HashSet};
use std::os::fd::RawFd;
use std::path::Path;

/// The server hosting the agent panes. Ids are opaque: only the backend that
/// issued one reads meaning into it.
pub trait Multiplexer {
    /// Every pane in display order, except `skip` and sidebar panes.
    fn panes(&mut self, skip: Option<&str>) -> Result<Vec<ParsedPane>, Error>;
    /// The visible screen of `pane` as plain text.
    fn capture(&mut self, pane: &str) -> Result<String, Error>;
    /// Which panes clients look at, and the one the user acts in.
    fn focus(&mut self) -> Option<ClientFocus>;

    /// Readable when change events are queued; None: poll on the scan schedule.
    fn events_fd(&self) -> Option<RawFd>;
    /// Events already read off `events_fd`, which polling it would miss.
    fn events_buffered(&self) -> bool;
    /// Read queued events without blocking; true when focus moved.
    fn drain_events(&mut self) -> Result<bool, Error>;
    /// Changes seen so far, not yet taken.
    fn changes(&self) -> &Changes;
    /// Take before scanning: events seen during the scan belong to the next.
    fn take_changes(&mut self) -> Changes;
    /// Hand output panes back for a later scan.
    fn defer_output(&mut self, panes: HashSet<String>);
    /// The only session whose output events arrive; None: all of them.
    fn covered_session(&self) -> Option<&str>;
    /// Make output events cover `session`. false: they could not.
    fn follow_session(&mut self, session: &str) -> bool;

    /// Whether `ids` still name the same records; `kind` None checks all three.
    fn is_live(&mut self, kind: Option<Kind>, ids: Ids) -> bool;
    /// The record's current name, which a rename starts from.
    fn name(&mut self, kind: Kind, ids: Ids) -> Result<String, Error>;
    fn rename(&mut self, kind: Kind, ids: Ids, name: &str) -> Result<(), Error>;
    /// Refuses a session's last pane or window, and the last session.
    fn delete(&mut self, kind: Kind, ids: Ids) -> Result<(), Error>;
    /// The pane's working directory, once `ids` are confirmed current.
    fn pane_cwd(&mut self, ids: Ids) -> Result<String, Error>;
    fn session_cwd(&mut self, session: &str) -> Result<String, Error>;
    /// The new record's first pane.
    fn create(&mut self, new: New) -> Result<String, Error>;

    /// Bring `pane` to the front of the client the user last used.
    fn jump(&mut self, pane: &str) -> Result<(), Error>;
    /// Move `client` to `pane`'s window and return it. `stay` leaves input
    /// off the pane, for the sidebar to take back.
    fn show(&mut self, client: &str, pane: &str, stay: bool) -> Result<String, Error>;
    /// Put `client` on its window's sidebar, once it shows `expected_window`.
    fn focus_sidebar(&mut self, client: &str, expected_window: Option<&str>);
    /// Tell `client` something. false: it could not be shown.
    fn message(&mut self, client: &str, text: &str) -> bool;
    /// Put `text` on the clipboard of `client`'s terminal. Some: an escape
    /// the sidebar's own terminal must write instead.
    fn copy(&mut self, client: Option<&str>, text: &str) -> Result<Option<String>, Error>;
    /// What `yy` copies for a record.
    fn reference(&self, kind: Kind, id: &str) -> String;

    /// This server, told apart from the one before a restart.
    fn identity(&mut self) -> Option<Identity>;
    /// Every window, for the snapshot layout.
    fn windows(&mut self) -> Result<Vec<WindowMeta>, Error>;
    /// Every live session name and window index, window id and pane id.
    fn live(&mut self) -> Live;
    /// Recreate `snap`'s missing sessions and windows; never touch a live one.
    fn restore(&mut self, snap: &Snapshot, live: &Live, resume: &Resume);
    /// Split a closed pane back into its live window.
    fn restore_pane(&mut self, entry: &Closed, live: &Live, resume: &Resume);

    /// Live setting overrides as `@agenmux-<setting>` keys; empty where the
    /// server keeps none.
    fn options(&mut self) -> Result<BTreeMap<String, String>, ConfigError>;
    /// The token `config reload` bumps.
    fn reload_token(&mut self) -> Option<String>;
    /// Tell running sidebars to re-read the configuration.
    fn publish_reload(&mut self, token: &str) -> Result<(), Error>;

    /// Every sidebar pane, measured. Err: the server did not answer, which
    /// is not the same as having none.
    fn sidebar_panes(&mut self) -> Result<Vec<SidebarPane>, Error>;
    /// Windows a real client shows.
    fn visible_windows(&mut self) -> HashSet<String>;
    /// Set each pane's width in cells. false: one of them failed.
    fn resize_panes(&mut self, cols: &[(String, usize)]) -> bool;
    fn close_panes(&mut self, panes: &[String]);
    /// Close every sidebar pane, putting their windows back as they were.
    fn close_sidebars(&mut self);
    /// Keep a dragged width for sidebars opened later. false: not kept.
    fn remember_width(&mut self, cols: usize) -> bool;
    /// This connection's name, which the owner record carries.
    fn connection(&mut self) -> Option<String>;
    /// The daemon that owns the sidebars; None when it could not be read.
    fn owner(&mut self) -> Option<Owner>;
    /// Record `client` as the owner, and the runtime dir its helpers use.
    fn claim(&mut self, client: &str, runtime: &Path) -> Result<(), Error>;
    /// Clear the record, if `generation` (when given) still owns it.
    fn release(&mut self, generation: Option<&str>);

    /// The client a sidebar key acts for when the key names none. tmux keys
    /// always name theirs.
    fn key_client(&mut self) -> Option<String> {
        None
    }

    // Where the server routes keys itself (`routes_keys`), the daemon steers
    // that routing. The defaults suit a server that forwards terminal input.

    /// Deliver keys the server resolved on its own straight into `fd`, the
    /// daemon's key FIFO.
    fn key_sink(&mut self, _fd: RawFd) {}
    /// Route `client`'s keys, or every sidebar client's with None, as `mode`.
    fn input_mode(&mut self, _client: Option<&str>, _mode: InputMode) {}
    /// Install the key routing `config` asks for. false: it could not be.
    fn install_keys(&mut self, _plugin_dir: &Path, _config: &AppConfig) -> bool {
        true
    }

    /// Drop replies to earlier requests before measuring layout.
    fn settle(&mut self) {}
    /// Move the sidebars to the side the settings name, and the keys that
    /// follow the side with them.
    fn dock(&mut self, _plugin_dir: &Path, _settings: &AppConfig) {}
    /// Fill header colors the settings leave unset from the server's theme.
    /// Returns the active border's foreground.
    fn inherit_header(&mut self, _settings: &mut AppConfig) -> Option<Color> {
        None
    }
    /// Tell the user on this connection's client, where it has one.
    fn notice(&mut self, _text: &str) {}
}

/// How keys typed in a sidebar are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    /// Keys run sidebar actions.
    Normal,
    /// Keys type into a prompt (search, rename).
    Text,
    /// Keys edit a settings value.
    Settings,
}

/// Which record an action names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Session,
    Window,
    Pane,
}

/// A record and the records above it: a backend may need the whole path.
#[derive(Clone, Copy, Debug)]
pub struct Ids<'a> {
    pub session: &'a str,
    pub window: &'a str,
    pub pane: &'a str,
}

/// A record to create.
pub enum New<'a> {
    Session {
        name: &'a str,
        cwd: &'a str,
    },
    Window {
        session: &'a str,
        name: &'a str,
        cwd: &'a str,
    },
    /// `argv` in a new window of `session`.
    Launch {
        session: &'a str,
        cwd: &'a str,
        argv: &'a [&'a str],
    },
}

pub struct Identity {
    /// Changes when the server restarts.
    pub server: String,
    /// Names the server's snapshot file, across restarts.
    pub name: String,
}

/// A window as the snapshot records it.
pub struct WindowMeta {
    pub id: String,
    /// Panes in it, the sidebar included.
    pub panes: usize,
    pub automatic_rename: bool,
    /// The sidebar is its rightmost split.
    pub sidebar_right: bool,
    /// Opaque: only the same backend's restore reads it.
    pub layout: String,
}

/// A sidebar pane as the daemon measures it, in cells.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SidebarPane {
    pub pane: String,
    pub win: String,
    pub sess: String,
    pub w: usize,
    pub h: usize,
    pub win_size: (usize, usize),
    /// Panes in its window, the sidebar included.
    pub panes: usize,
    /// Its window is its session's current one.
    pub active: bool,
    /// The backend keeps it at the asked width and adopts drags itself:
    /// shared code renders it, never resizes it or reads a drag from it.
    pub self_sized: bool,
}

/// The daemon that owns the sidebars: the activation that started it, and
/// its connection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Owner {
    pub generation: String,
    pub client: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Tmux,
    Rex,
}

const AMBIGUOUS: &str = "both a tmux and a Rex server are running, and this \
shell is in neither; pass --mux tmux or --mux rex, or set AGENMUX_MUX";

/// Which multiplexer this process works with, decided once:
/// 1. `--mux` (main exports it as AGENMUX_MUX) or AGENMUX_MUX;
/// 2. the one whose pane or block this process runs in. Both variables can
///    be inherited stale (a Rex server started from tmux hands every block
///    TMUX), so with both set the nearer ancestor shell decides, else a
///    tmux server above this process (a tmux job runs in no pane);
/// 3. outside both, the only server running.
pub fn backend() -> Result<Backend, String> {
    static BACKEND: std::sync::OnceLock<Result<Backend, String>> = std::sync::OnceLock::new();
    BACKEND.get_or_init(resolve).clone()
}

pub fn is_rex() -> bool {
    backend() == Ok(Backend::Rex)
}

/// The backend's name, as users read it in errors.
pub fn name() -> &'static str {
    if is_rex() {
        "rex"
    } else {
        "tmux"
    }
}

fn resolve() -> Result<Backend, String> {
    match std::env::var("AGENMUX_MUX").as_deref() {
        Ok("rex") => return Ok(Backend::Rex),
        Ok("tmux") => return Ok(Backend::Tmux),
        Ok("") | Err(_) => {}
        Ok(other) => return Err(format!("AGENMUX_MUX={other}: expected tmux or rex")),
    }
    let in_tmux = std::env::var_os("TMUX").is_some();
    let in_rex = std::env::var_os("REX_SESSION").is_some();
    match (in_tmux, in_rex) {
        (true, false) => return Ok(Backend::Tmux),
        (false, true) => return Ok(Backend::Rex),
        (true, true) => {
            let chain = ancestors();
            if let Some(found) = nearest(&chain, tmux::pane_pid(), rex::block_pid()) {
                return Ok(found);
            }
            // a tmux job (run-shell, #()) runs under the server, in no pane
            if tmux::server_pid().is_some_and(|pid| chain.contains(&pid)) {
                return Ok(Backend::Tmux);
            }
        }
        (false, false) => {}
    }
    match (tmux::running(), rex::running()) {
        (true, true) => Err(AMBIGUOUS.into()),
        (false, true) => Ok(Backend::Rex),
        // with no server at all, tmux's own error says so
        _ => Ok(Backend::Tmux),
    }
}

/// The multiplexer whose shell is the nearest of `ancestors` (this process
/// first): the inner one when nested, and never one whose pane is gone.
fn nearest(ancestors: &[u32], tmux: Option<u32>, rex: Option<u32>) -> Option<Backend> {
    let depth = |pid: Option<u32>| pid.and_then(|pid| ancestors.iter().position(|&a| a == pid));
    match (depth(tmux), depth(rex)) {
        (Some(t), Some(r)) if r < t => Some(Backend::Rex),
        (Some(_), _) => Some(Backend::Tmux),
        (None, Some(_)) => Some(Backend::Rex),
        (None, None) => None,
    }
}

/// This process and its parents, nearest first.
fn ancestors() -> Vec<u32> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let mut sys = System::new();
    let mut pid = Pid::from_u32(std::process::id());
    let mut chain = vec![pid.as_u32()];
    while chain.len() < 64 {
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
        let Some(parent) = sys.process(pid).and_then(|p| p.parent()) else {
            break;
        };
        chain.push(parent.as_u32());
        pid = parent;
    }
    chain
}

/// `--mux tmux|rex` anywhere in `args`, removed and exported as AGENMUX_MUX
/// so child processes agree.
pub fn take_mux_flag(args: &mut Vec<String>) -> Result<(), String> {
    let Some(at) = args
        .iter()
        .position(|a| a == "--mux" || a.starts_with("--mux="))
    else {
        return Ok(());
    };
    let flag = args.remove(at);
    let value = match flag.strip_prefix("--mux=") {
        Some(value) => Some(value.to_string()),
        None => (at < args.len()).then(|| args.remove(at)),
    };
    match value.as_deref() {
        Some(value @ ("tmux" | "rex")) => {
            std::env::set_var("AGENMUX_MUX", value);
            Ok(())
        }
        _ => Err("--mux takes tmux or rex".into()),
    }
}

/// The server routes keys to the daemon itself (tmux key tables). Elsewhere
/// each sidebar pane forwards its terminal input.
pub fn routes_keys() -> bool {
    !is_rex()
}

/// A sidebar pane starting: bring up its daemon where nothing else will.
/// tmux's hooks start one; Rex restores a pane after a restart with none.
pub fn revive_daemon() {
    if is_rex() {
        rex::revive();
    }
}

/// The internal commands tmux bindings and Rex actions run. None: `args`
/// is none of them. `plugin_dir` is asked only by commands that need it.
pub fn run_command(args: &[&str], plugin_dir: fn() -> std::path::PathBuf) -> Option<i32> {
    Some(match args {
        ["rex-toggle", session, block, rest @ ..] => rex::toggle(
            session,
            block,
            rest.first().copied().unwrap_or("split"),
            rest.get(1).copied().unwrap_or(""),
        ),
        ["rex-reload", rest @ ..] => rex::reload(rest.first().copied().unwrap_or("")),
        ["rex-sidebars"] => rex::sidebars(),
        ["teardown"] if is_rex() => {
            rex::teardown();
            0
        }
        _ => return tmux::run_command(args, plugin_dir),
    })
}

/// Close the sidebars of a daemon that died without cleaning up. Rex
/// sidebars close with their daemon.
pub fn reap_dead_owner() {
    if !is_rex() {
        tmux::panes::reap_dead_owner();
    }
}

/// `config reload`: reinstall key routing `config` no longer matches.
/// Ok(true): reinstalled. Rex binds its keys in init.lua.
pub fn reinstall_keys(plugin_dir: &Path, config: &AppConfig) -> Result<bool, ()> {
    if is_rex() {
        Ok(false)
    } else {
        tmux::setup::reinstall(plugin_dir, config)
    }
}

/// Tell the user `message` on the tmux status line, else on stdout.
pub fn note(message: &str) {
    tmux::view::note(message);
}

/// Reopen an open tmux split view through the launch gate, which switches
/// to a prepared update first. Leaves popups and closed views alone.
pub fn restart_view(plugin_dir: &Path) {
    tmux::view::restart(plugin_dir);
}

/// The view `close_view` closed, to put back once an update is done.
pub enum ClosedView {
    None,
    Tmux(tmux::view::Closed),
    Rex(Vec<(String, String)>),
}

/// Close this server's view so its daemon releases the installation.
pub fn close_view() -> ClosedView {
    if is_rex() {
        ClosedView::Rex(rex::teardown())
    } else {
        tmux::view::close().map_or(ClosedView::None, ClosedView::Tmux)
    }
}

impl ClosedView {
    /// Reopen it on whichever release is installed now.
    pub fn reopen(self, plugin_dir: &Path) {
        match self {
            ClosedView::None => {}
            ClosedView::Tmux(closed) => closed.reopen(plugin_dir),
            ClosedView::Rex(windows) => rex::reopen(&windows),
        }
    }
}

/// The server a notification click returns to, and the app that shows it
/// (None: the terminal the server runs in).
#[cfg(target_os = "macos")]
pub fn notification_server() -> Option<(String, Option<&'static str>)> {
    if is_rex() {
        return Some((rex::NOTIFY_SERVER.into(), Some(rex::APP_BUNDLE)));
    }
    let tmux = std::env::var("TMUX").ok()?;
    let socket = tmux.split(',').next().filter(|socket| !socket.is_empty())?;
    Some((socket.into(), None))
}

/// `notification-open`: show `pane` on `server`, from `notification_server`.
/// `activate` brings forward the app bundle that shows it, once `pane` is
/// confirmed; `bundle` is the terminal's, recorded with the click.
pub fn open_notified(server: &str, pane: &str, bundle: &str, activate: impl FnOnce(&str)) {
    if server == rex::NOTIFY_SERVER {
        rex::open_notified(pane, activate);
    } else {
        tmux::notified::open_notified(server, pane, || activate(bundle));
    }
}

/// The pane this process runs in.
pub fn own_pane() -> Option<String> {
    if is_rex() {
        rex::own_pane()
    } else {
        tmux::own_pane()
    }
}

/// `Multiplexer::owner`, for a process not yet connected.
pub fn owner() -> Option<Owner> {
    if is_rex() {
        rex::owner()
    } else {
        tmux::owner()
    }
}

/// `Multiplexer::options`, for a command that holds no connection.
pub fn options() -> Result<BTreeMap<String, String>, ConfigError> {
    if is_rex() {
        Ok(rex::options())
    } else {
        tmux::options()
    }
}

/// `Multiplexer::publish_reload`, for a command that holds no connection.
pub fn publish_reload(token: &str) -> Result<(), Error> {
    if is_rex() {
        rex::publish_reload(token)
    } else {
        tmux::publish_reload(token)
    }
}

/// `monitoring`: the long-lived sidebar connection, which wants output events.
pub fn connect(monitoring: bool) -> Result<Box<dyn Multiplexer>, Error> {
    Ok(match (backend().map_err(Error::Failed)?, monitoring) {
        (Backend::Rex, monitoring) => Box::new(rex::Rex::connect(monitoring)?),
        (Backend::Tmux, true) => Box::new(tmux::Tmux::connect_monitoring()?),
        (Backend::Tmux, false) => Box::new(tmux::Tmux::connect()?),
    })
}

/// A request to the multiplexer failed.
pub enum Error {
    /// Server gone or client detached — caller must clean up and exit 0
    /// (toggle.sh's popup loop relies on a clean sidebar exit).
    Exited,
    Failed(String),
    Io(std::io::Error),
}

// Users see these verbatim, named for the backend in use.
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let name = name();
        match self {
            Error::Exited => write!(f, "{name} exited"),
            Error::Failed(e) => write!(f, "{name}: {e}"),
            Error::Io(e) => write!(f, "{name} pipe: {e}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// What changed on the server since the last scan took them.
#[derive(Default)]
pub struct Changes {
    /// Panes that wrote output.
    pub panes: HashSet<String>,
    /// Sessions or windows came or went: rescan everything.
    pub full: bool,
    pub focus: bool,
    pub layout: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearer_shell_wins_and_a_gone_pane_never_does() {
        let chain = [10, 20, 30, 40];
        assert_eq!(nearest(&chain, Some(20), Some(40)), Some(Backend::Tmux));
        assert_eq!(nearest(&chain, Some(40), Some(20)), Some(Backend::Rex));
        // a stale TMUX: its pane is no ancestor of this process
        assert_eq!(nearest(&chain, Some(99), Some(30)), Some(Backend::Rex));
        assert_eq!(nearest(&chain, None, Some(30)), Some(Backend::Rex));
        assert_eq!(nearest(&chain, None, None), None);
    }

    #[test]
    fn the_mux_flag_is_taken_from_anywhere() {
        let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        let mut list = args(&["list", "--mux", "tmux", "-s", "work"]);
        assert_eq!(take_mux_flag(&mut list), Ok(()));
        assert_eq!(list, args(&["list", "-s", "work"]));
        let mut status = args(&["--mux=tmux", "status"]);
        assert_eq!(take_mux_flag(&mut status), Ok(()));
        assert_eq!(status, args(&["status"]));
        assert!(take_mux_flag(&mut args(&["list", "--mux"])).is_err());
        assert!(take_mux_flag(&mut args(&["--mux", "screen", "list"])).is_err());
        let mut plain = args(&["list"]);
        assert_eq!(take_mux_flag(&mut plain), Ok(()));
    }
}
