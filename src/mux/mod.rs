// Multiplexer seam: what the scan, sidebar and daemon need from the server
// hosting the agent panes. tmux is the only backend; tmux-only features reach
// it through `tmux` explicitly.
#[cfg(test)]
pub mod fake;
pub mod tmux;

use crate::app_config::ConfigError;
use crate::focus::ClientFocus;
use crate::scan::ParsedPane;
use crate::snapshot::{Closed, Live, Resume, Snapshot};
use std::collections::{BTreeMap, HashSet};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

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

    /// tmux-only features. Every other backend returns None.
    fn as_tmux(&mut self) -> Option<&mut tmux::Tmux> {
        None
    }
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
}

/// The daemon that owns the sidebars: the activation that started it, and
/// its connection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Owner {
    pub generation: String,
    pub client: String,
}

/// Where this server's daemon, sidebar panes and key senders meet.
pub fn runtime_dir() -> PathBuf {
    tmux::runtime_dir()
}

/// `runtime_dir`, created private to this user.
pub fn prepare_runtime_dir() -> std::io::Result<PathBuf> {
    tmux::prepare_runtime_dir()
}

/// The pane this process runs in.
pub fn own_pane() -> Option<String> {
    tmux::own_pane()
}

/// `Multiplexer::owner`, for a process not yet connected.
pub fn owner() -> Option<Owner> {
    tmux::owner()
}

/// `Multiplexer::options`, for a command that holds no connection.
pub fn options() -> Result<BTreeMap<String, String>, ConfigError> {
    tmux::options()
}

/// `Multiplexer::publish_reload`, for a command that holds no connection.
pub fn publish_reload(token: &str) -> Result<(), Error> {
    tmux::publish_reload(token)
}

/// `monitoring`: the long-lived sidebar connection, which wants output events.
pub fn connect(monitoring: bool) -> Result<Box<dyn Multiplexer>, Error> {
    Ok(Box::new(if monitoring {
        tmux::Tmux::connect_monitoring()?
    } else {
        tmux::Tmux::connect()?
    }))
}

/// A request to the multiplexer failed.
pub enum Error {
    /// Server gone or client detached — caller must clean up and exit 0
    /// (toggle.sh's popup loop relies on a clean sidebar exit).
    Exited,
    Failed(String),
    Io(std::io::Error),
}

// Users see these verbatim; worded for tmux, the only backend so far.
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Exited => write!(f, "tmux exited"),
            Error::Failed(e) => write!(f, "tmux: {e}"),
            Error::Io(e) => write!(f, "tmux pipe: {e}"),
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
