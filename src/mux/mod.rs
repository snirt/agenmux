// Multiplexer seam: what a scan and the sidebar need from the server hosting
// the agent panes. tmux answers over its control-mode pipe (and forked tmux for
// commands whose hooks would desync it); Rex over its CLI. Features that only
// tmux has (key tables, popups, the daemon's mirrored panes) still reach the
// control client through `as_tmux` or `run`, which other servers refuse.
mod rex;
mod tmux;

pub use rex::{publish_reload, rex_sidebars, rex_toggle, Rex};

use crate::scan::ParsedPane;
use crate::snapshot::{Closed, Live, Snapshot};
use crate::tmux::{PendingChanges, Tmux, TmuxError};
use std::collections::BTreeMap;

/// Which record a management action names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Session,
    Window,
    Pane,
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

    /// Bring `pane` to the front. false: the caller jumps the tmux way.
    fn focus(&mut self, _pane: &str) -> bool {
        false
    }
    /// The tty sidebar lives in a pane of its own (a Rex block) rather than a
    /// popup: closing, jumping and launching keep it running.
    fn embedded(&self) -> bool {
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

/// AGENMUX_MUX=tmux|rex picks the server; otherwise Rex when running inside a
/// Rex block outside tmux.
pub fn is_rex() -> bool {
    match std::env::var("AGENMUX_MUX").as_deref() {
        Ok("rex") => true,
        Ok("tmux") => false,
        _ => std::env::var_os("TMUX").is_none() && std::env::var_os("REX_SESSION").is_some(),
    }
}

/// `monitoring`: the long-lived sidebar client, which wants pane output.
pub fn connect(monitoring: bool) -> Result<Box<dyn Multiplexer>, TmuxError> {
    Ok(match (is_rex(), monitoring) {
        (true, monitoring) => Box::new(Rex::connect(monitoring)?),
        (false, true) => Box::new(Tmux::connect_monitoring()?),
        (false, false) => Box::new(Tmux::connect()?),
    })
}
