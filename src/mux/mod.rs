// Multiplexer seam: what a scan and the sidebar need from the server hosting
// the agent panes. tmux answers over its control-mode pipe (and forked tmux for
// commands whose hooks would desync it); Rex over its CLI. Features that only
// tmux has (key tables, popups, the daemon's mirrored panes) still reach the
// control client through `as_tmux` or `run`, which other servers refuse.
mod rex;
mod tmux;

pub use rex::{
    publish_reload, rex_close_sidebars, rex_reload, rex_reopen, rex_sidebars, rex_toggle,
    runtime_dir as rex_runtime_dir, Rex,
};

use crate::app_config::AppConfig;
use crate::scan::ParsedPane;
use crate::snapshot::{Closed, Live, Snapshot};
use crate::tmux::{PendingChanges, Tmux, TmuxError};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Which record a management action names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Session,
    Window,
    Pane,
}

/// A sidebar pane as the daemon measures it, in cells.
#[derive(serde::Deserialize, Clone, Debug, Default, PartialEq)]
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
    /// Its size is the one its layout gives it: no placement or client
    /// resize is still on the way.
    pub settled: bool,
    /// Some: the backend knows whether a divider moved. None: compare exact
    /// window cell sizes (tmux). Estimated sizes must never veto a known drag.
    pub width_drag: Option<bool>,
}

/// The daemon that owns the sidebars: the activation that started it, and
/// its connection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Owner {
    pub generation: String,
    pub client: String,
}

/// A record and the records above it, as the scan reported them.
#[derive(Clone, Copy, Debug)]
pub struct Ids<'a> {
    pub session: &'a str,
    pub window: &'a str,
    pub pane: &'a str,
}

pub trait Multiplexer {
    /// Every pane on the server except `self_pane`, in display order.
    fn panes(&mut self, self_pane: Option<&str>) -> Result<Vec<ParsedPane>, TmuxError>;
    /// The visible screen of `pane` as plain text.
    fn capture(&mut self, pane: &str) -> Result<String, TmuxError>;
    /// Which panes clients look at, and the one the user acts in.
    fn client_focus(&mut self) -> Option<crate::focus::ClientFocus>;

    /// Bring a session, window or pane to the front. false: the caller jumps
    /// the tmux way.
    fn focus(&mut self, _id: &str) -> bool {
        false
    }
    /// The tty sidebar lives in a pane of its own (a Rex block) rather than a
    /// popup: closing, jumping and launching keep it running.
    fn embedded(&self) -> bool {
        false
    }
    /// This sidebar sends notifications. Where every sidebar scans the
    /// server on its own, one of them speaks for all.
    fn notifies(&self) -> bool {
        true
    }
    /// Nobody sees this sidebar and another one tracks the server: periodic
    /// scans can wait for a change event.
    fn quiet(&self) -> bool {
        false
    }

    /// Change notifications. A server without them is polled on the scan
    /// schedule alone.
    fn notify_fd(&self) -> libc::c_int {
        -1 // poll(2) skips negative descriptors
    }
    fn notify_buffered(&self) -> bool {
        false
    }
    fn drain_notifications(&mut self) -> Result<bool, TmuxError> {
        Ok(false)
    }
    fn pending_changes(&self) -> Option<&PendingChanges> {
        None
    }
    fn take_pending_changes(&mut self) -> PendingChanges {
        PendingChanges::default()
    }
    fn defer_output(&mut self, _panes: std::collections::HashSet<String>) {}
    fn attached_session(&self) -> Option<&str> {
        None
    }

    /// Whether `ids` still name the same records; `kind` None checks all three.
    fn is_live(&mut self, kind: Option<Kind>, ids: Ids) -> bool;
    /// The new window's first pane.
    fn create_window(&mut self, session: &str, name: &str, cwd: &str) -> Result<String, TmuxError>;
    /// The new session's first pane.
    fn create_session(&mut self, name: &str, cwd: &str) -> Result<String, TmuxError>;
    /// Run `argv` in a new window of `session`; returns its pane.
    fn launch(&mut self, session: &str, cwd: &str, argv: &[&str]) -> Result<String, TmuxError>;
    /// The record's current name, which a rename starts from.
    fn name(&mut self, kind: Kind, ids: Ids) -> Result<String, TmuxError>;
    fn rename(&mut self, kind: Kind, ids: Ids, name: &str) -> Result<(), TmuxError>;
    /// Never removes a session implicitly: the last pane or window of a
    /// session, and the last session, are refused.
    fn delete(&mut self, kind: Kind, ids: Ids) -> Result<(), TmuxError>;
    /// The pane's working directory, once `ids` are confirmed still current.
    fn pane_cwd(&mut self, ids: Ids) -> Result<String, TmuxError>;
    fn session_cwd(&mut self, session: &str) -> Result<String, TmuxError>;
    /// Move `client` to `pane`'s window. `stay` keeps input on the sidebar.
    fn show(&mut self, client: &str, pane: &str, stay: bool) -> Result<(), TmuxError>;
    /// Put `client` back on the sidebar, once `expected_window` is current.
    fn focus_sidebar(&mut self, client: &str, expected_window: Option<&str>);
    /// Tell `client` something. false: the caller shows it in the sidebar.
    fn message(&mut self, client: &str, text: &str) -> bool;
    /// Put `text` on the clipboard of the terminal that asked.
    fn copy(&mut self, client: Option<&str>, text: &str) -> Result<(), TmuxError>;
    /// What `yy` copies for a session, window or pane id.
    fn reference(&self, id: &str) -> String;

    /// This server's identity, which tells it from the one before a
    /// restart, and its snapshot file.
    fn snapshot_home(&mut self) -> Option<(String, std::path::PathBuf)>;
    /// One `snapshot::WINDOWS_FMT` row per window, for `snapshot::layout`.
    fn windows(&mut self) -> Result<String, TmuxError>;
    /// Every live pane id, whitespace separated.
    fn open_panes(&mut self) -> String;
    /// Every live session name and window index, window id and pane id.
    fn live(&mut self) -> Live;
    /// Recreate `snap`'s missing sessions and windows; never touch a live one.
    fn replay(&mut self, snap: &Snapshot, live: &Live, resume: &dyn Fn(&str) -> Option<String>);
    /// Split a closed pane back into its live window.
    fn replay_pane(&mut self, entry: &Closed, live: &Live, resume: &dyn Fn(&str) -> Option<String>);

    /// The token `config reload` bumps.
    fn reload_token(&mut self) -> Option<String>;
    fn publish_reload(&mut self, token: &str) -> Result<(), TmuxError>;
    /// Live `@agenmux-*` overrides; empty where the server has none.
    fn options(&mut self) -> Result<BTreeMap<String, String>, crate::app_config::ConfigError>;

    // The daemon: one process scans and renders into every sidebar pane. It
    // decides; these only report and act. The defaults suit a server no
    // daemon runs on.

    /// Native layout owner, if any. Returning Some bypasses shared Rust width
    /// policy entirely; geometry reporting remains available through sidebar_panes.
    fn reconcile_sidebar_layout(&mut self, _width: usize) -> Option<Vec<SidebarPane>> {
        None
    }
    /// Every sidebar pane.
    fn sidebar_panes(&mut self) -> Vec<SidebarPane> {
        Vec::new()
    }
    /// Windows a real client shows.
    fn visible_windows(&mut self) -> HashSet<String> {
        HashSet::new()
    }
    /// Set each pane's width in cells. false: one of them failed.
    fn resize_panes(&mut self, _widths: &[(String, usize)]) -> bool {
        false
    }
    fn close_panes(&mut self, _panes: &[String]) {}
    /// The settings moved the sidebars to the other side.
    fn dock_sidebars(&mut self, _plugin_dir: &Path, _settings: &AppConfig) {}
    /// Close every sidebar pane, putting their windows' layouts back.
    fn close_sidebars(&mut self) {}
    /// Make the width a dragged sidebar settled on the configured one.
    fn save_width(&mut self, _width: usize) -> bool {
        false
    }
    /// This connection's name, which a daemon's ownership record carries.
    fn connection(&mut self) -> Option<String> {
        None
    }
    /// The ownership record; None when it could not be read.
    fn owner(&mut self) -> Option<Owner> {
        None
    }
    /// Record `client` as the owner, and the runtime dir its helpers use.
    fn publish_owner(&mut self, _client: &str, _runtime: &Path) -> Result<(), TmuxError> {
        Err(TmuxError::Error("no daemon on this server".into()))
    }
    /// Clear the record, if `generation` (when given) still owns it.
    fn clear_owner(&mut self, _generation: Option<&str>) {}
    /// Where keys the server delivers itself go.
    fn set_key_sink(&mut self, _fd: libc::c_int) {}
    /// Change events cover the session the user acts in.
    fn follow_session(&mut self, _session: &str) -> bool {
        true
    }
    /// `client` types text into the sidebar, or stops.
    fn text_input(&mut self, _client: &str, _on: bool) {}

    /// A tmux command over the control pipe; other servers refuse it, so
    /// tmux-only features (key tables, layouts, clients) fail soft.
    fn run(&mut self, cmd: &str) -> Result<String, TmuxError> {
        Err(TmuxError::Error(format!("not tmux: {cmd}")))
    }
    fn sync(&mut self) -> Result<(), TmuxError> {
        Ok(())
    }

    fn as_tmux(&mut self) -> Option<&mut Tmux> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
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
///    TMUX), so with both set the nearer ancestor shell decides;
/// 3. outside both, the only server running.
pub fn backend() -> Result<Backend, String> {
    static BACKEND: std::sync::OnceLock<Result<Backend, String>> = std::sync::OnceLock::new();
    BACKEND.get_or_init(resolve).clone()
}

pub fn is_rex() -> bool {
    backend() == Ok(Backend::Rex)
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
            let found = nearest(&ancestors(), tmux_pane_pid(), rex::block_pid());
            if let Some(found) = found {
                return Ok(found);
            }
        }
        (false, false) => {}
    }
    match (tmux_running(), rex::running()) {
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

/// Plain tmux forks: the crate's tmux helpers ask `is_rex` first.
fn tmux_quiet(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("tmux")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn tmux_pane_pid() -> Option<u32> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    tmux_quiet(&["display-message", "-p", "-t", &pane, "#{pane_pid}"])?
        .trim()
        .parse()
        .ok()
}

fn tmux_running() -> bool {
    tmux_quiet(&["list-sessions"]).is_some()
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

/// The pane or block this process runs in, as an id safe in a file name.
pub fn own_pane() -> Option<String> {
    let (name, prefix, valid): (_, _, fn(u8) -> bool) = match backend().ok()? {
        Backend::Tmux => ("TMUX_PANE", "%", |b| b.is_ascii_digit()),
        Backend::Rex => ("REX_BLOCK", "block:", |b| {
            b.is_ascii_alphanumeric() || b == b'-'
        }),
    };
    std::env::var(name).ok().filter(|id| {
        id.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(valid))
    })
}

/// The server sends the daemon its keys itself (tmux key tables). Elsewhere
/// each sidebar pane forwards its terminal input.
pub fn routes_keys() -> bool {
    backend() == Ok(Backend::Tmux)
}

/// `monitoring`: the long-lived sidebar client, which wants pane output.
pub fn connect(monitoring: bool) -> Result<Box<dyn Multiplexer>, TmuxError> {
    Ok(match (backend().map_err(TmuxError::Choose)?, monitoring) {
        (Backend::Rex, monitoring) => Box::new(Rex::connect(monitoring)?),
        (Backend::Tmux, true) => Box::new(Tmux::connect_monitoring()?),
        (Backend::Tmux, false) => Box::new(Tmux::connect()?),
    })
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
        let mut list = args(&["list", "--mux", "rex", "-s", "work"]);
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
