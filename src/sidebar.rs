// Sidebar TUI — runs inside the sidebar pane/popup. Port of sidebar.sh:
// same keys, same frame bytes, same rows/cache/pin file protocol, but one
// process, one tmux pipe, zero forks per tick.
//
// Two entry points share the engine:
//  - run():        tty mode — popup pane, draws to stdout, keys from stdin.
//  - run_daemon(): headless preserved-pane mode — frames go directly to the
//    processless panes visible in attached clients; keys arrive over a FIFO.
//    The panes never move between windows, so switching causes no join-pane
//    reflow (the "bump").
use crate::app_config::{action_for, Action, KeyChord, KeyMode, Keymap, Palette};
use crate::attention::Tracker;
use crate::conf::AgentConf;
use crate::pane_writers::PaneWriters;
use crate::panes;
use crate::procs::IdentCache;
use crate::release;
use crate::scan::{self, PaneRow};
use crate::tmux::{command_spawn, command_status, Tmux, TmuxError};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static WINCH: AtomicBool = AtomicBool::new(false);
static QUIT: AtomicBool = AtomicBool::new(false);

const KEY_SEQUENCE_TIMEOUT: Duration = Duration::from_secs(1);
extern "C" fn on_winch(_: libc::c_int) {
    WINCH.store(true, Ordering::Relaxed);
}
extern "C" fn on_term(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

pub(crate) const E: &str = "\x1b";
const SPIN: [char; 8] = ['⠹', '⢸', '⣰', '⣤', '⣆', '⡇', '⠏', '⠛'];

/// " · "-separated hint segments, skipping unbound (empty) ones.
fn join(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn bar(line: &str, bg: &str, cols: usize, width: usize) -> String {
    if bg.is_empty() {
        return line.into();
    }
    let body = line.replace(&format!("{E}[0m"), &format!("{E}[0m{bg}"));
    format!("{bg}{body}{}{E}[0m", " ".repeat(cols.saturating_sub(width)))
}

fn cursor_mark(palette: &Palette, selected: bool, plugin_selected: bool, state: &str) -> String {
    if !selected {
        return "  ".into();
    }
    let fg = palette
        .state_fg(state)
        .fg(if plugin_selected { "1" } else { "" });
    format!("{fg}❯{E}[0m ")
}

/// Clip generated SGR/CSI frames without splitting an escape or wrapping a
/// logical click row. Layout elsewhere uses the same character-cell metric.
fn clip_frame(frame: &str, cols: usize, cap: usize) -> String {
    if cols == 0 || cap == 0 {
        return format!("{E}[H{E}[0m{E}[J");
    }
    let mut out = String::new();
    let mut row = 0;
    let mut col = 0;
    let mut chars = frame.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            if let Some(next) = chars.next() {
                out.push(next);
                if next == '[' {
                    for parameter in chars.by_ref() {
                        out.push(parameter);
                        if ('@'..='~').contains(&parameter) {
                            break;
                        }
                    }
                }
            }
        } else if c == '\n' {
            out.push(c);
            col = 0;
            row += 1;
            if row >= cap {
                let tail: String = chars.collect();
                // Keep the historical clear-to-end suffix when already fitted.
                if tail == format!("{E}[J") {
                    out.push_str(&tail);
                } else {
                    out.push_str(&format!("{E}[0m{E}[J"));
                }
                break;
            }
        } else {
            if col < cols {
                out.push(c);
            }
            col += 1;
        }
    }
    out
}

pub(crate) struct RawMode(Option<libc::termios>);

impl RawMode {
    pub(crate) fn enable() -> RawMode {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return RawMode(None); // not a tty (tests) — keys just won't work
            }
            let orig = t;
            t.c_lflag &= !(libc::ICANON | libc::ECHO);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
            libc::tcsetattr(0, libc::TCSANOW, &t);
            RawMode(Some(orig))
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(orig) = self.0 {
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &orig) };
        }
    }
}

pub(crate) fn term_size() -> (usize, usize) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            return (ws.ws_col as usize, ws.ws_row as usize);
        }
    }
    (30, 24)
}

/// poll one fd; returns true when readable. timeout None = wait forever.
pub(crate) fn poll_fd(fd: libc::c_int, timeout: Option<Duration>) -> bool {
    let mut fds = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
    unsafe { libc::poll(&mut fds, 1, ms) > 0 && fds.revents & libc::POLLIN != 0 }
}

/// poll the key fd + the tmux control pipe; returns (key_ready, pipe_ready).
/// pipe_buffered short-circuits the wait — data is already in the BufReader.
fn poll_inputs(
    key_fd: libc::c_int,
    pipe_fd: libc::c_int,
    pipe_buffered: bool,
    timeout: Duration,
) -> (bool, bool) {
    let mut fds = [
        libc::pollfd {
            fd: key_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: pipe_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let ms = if pipe_buffered {
        0
    } else {
        timeout.as_millis().min(i32::MAX as u128) as i32
    };
    let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
    let key = n > 0 && fds[0].revents & libc::POLLIN != 0;
    // a dead pipe sets only HUP/ERR/NVAL — POLLIN alone never reports it, and
    // the loop then sleeps forever instead of reading its way to EOF
    let dead = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    let pipe = pipe_buffered || (n > 0 && fds[1].revents & (libc::POLLIN | dead) != 0);
    (key, pipe)
}

/// Read one byte; None on EOF or error.
pub(crate) fn read_byte(fd: libc::c_int) -> Option<u8> {
    let mut b = [0u8; 1];
    let n = unsafe { libc::read(fd, b.as_mut_ptr().cast(), 1) };
    (n == 1).then_some(b[0])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StateFilter {
    Blocked,
    Working,
    Idle,
    Done,
}

impl StateFilter {
    fn label(self) -> &'static str {
        match self {
            StateFilter::Blocked => "blocked",
            StateFilter::Working => "working",
            StateFilter::Idle => "idle",
            StateFilter::Done => "done",
        }
    }

    fn cycle(current: Option<Self>) -> Option<Self> {
        match current {
            None => Some(Self::Blocked),
            Some(Self::Blocked) => Some(Self::Working),
            Some(Self::Working) => Some(Self::Idle),
            Some(Self::Idle) => Some(Self::Done),
            Some(Self::Done) => None,
        }
    }
}

#[derive(Clone)]
enum Key {
    First,
    Last,
    Sequence(char),
    Up,
    Select(usize),
    Down,
    WheelUp,
    WheelDown,
    Jump,
    Quit,
    Close,
    Help,
    Versions,
    Search,
    Backspace,
    ClearSearch,
    CycleState,
    AllStates,
    Text(String),
    Other,
}

#[derive(Default)]
struct KeySequence {
    pending: String,
    last: Option<Instant>,
}

enum SequenceResult {
    Pending,
    Match(Key),
    Miss,
}

impl KeySequence {
    fn push(&mut self, key: char, now: Instant, bindings: &[(&str, Key)]) -> SequenceResult {
        if self
            .last
            .is_some_and(|last| now.duration_since(last) > KEY_SEQUENCE_TIMEOUT)
        {
            self.clear();
        }
        self.pending.push(key);
        self.last = Some(now);
        if let Some((_, action)) = bindings
            .iter()
            .find(|(sequence, _)| *sequence == self.pending)
        {
            let action = action.clone();
            self.clear();
            SequenceResult::Match(action)
        } else if bindings
            .iter()
            .any(|(sequence, _)| sequence.starts_with(&self.pending))
        {
            SequenceResult::Pending
        } else {
            self.clear();
            SequenceResult::Miss
        }
    }

    fn clear(&mut self) {
        self.pending.clear();
        self.last = None;
    }
}

enum Overlay {
    Help,
    Versions { sel: usize, chosen: Option<String> },
}

#[derive(Debug, PartialEq, Eq)]
enum DispatchMode {
    Overlay,
    Search,
    Normal,
}

#[derive(Debug, PartialEq, Eq)]
enum DispatchResult {
    Continue,
    Break,
    QuietExit,
}

fn dispatch_mode(overlay: Option<&Overlay>, search_focused: bool) -> DispatchMode {
    if overlay.is_some() {
        DispatchMode::Overlay
    } else if search_focused {
        DispatchMode::Search
    } else {
        DispatchMode::Normal
    }
}

/// Physical byte(s) → chord. `next` yields escape-tail bytes, None once
/// nothing more arrives.
fn chord(first: u8, next: impl FnMut() -> Option<u8>) -> Option<KeyChord> {
    Some(match first {
        0x1b => return escape_chord(next),
        0x09 => KeyChord::Tab,
        0x0a | 0x0d => KeyChord::Enter,
        0x08 | 0x7f => KeyChord::Backspace,
        0x00..=0x1f => KeyChord::Control(first),
        0x20..=0x7e => KeyChord::Printable(first),
        _ => return None,
    })
}

/// Decode the tail of an escape sequence: CSI (ESC [ A) normally, SS3
/// (ESC O A) in application-cursor mode, tilde forms for Home/End/PgUp/PgDn.
/// No tail is a bare Esc; an incomplete or unknown tail is nothing.
fn escape_chord(mut next: impl FnMut() -> Option<u8>) -> Option<KeyChord> {
    let Some(a) = next() else {
        return Some(KeyChord::Escape);
    };
    if a != b'[' && a != b'O' {
        return None;
    }
    Some(match next()? {
        b'A' => KeyChord::Up,
        b'B' => KeyChord::Down,
        b'C' => KeyChord::Right,
        b'D' => KeyChord::Left,
        b'H' => KeyChord::Home,
        b'F' => KeyChord::End,
        digit @ b'1'..=b'8' if a == b'[' && next()? == b'~' => match digit {
            b'1' | b'7' => KeyChord::Home,
            b'4' | b'8' => KeyChord::End,
            b'5' => KeyChord::PageUp,
            b'6' => KeyChord::PageDown,
            _ => return None,
        },
        _ => return None,
    })
}

/// Logical action → dispatcher key. Popup input maps chords through the
/// user's keymap; the daemon maps fixed FIFO protocol bytes through defaults.
fn action_key(keys: &Keymap, chord: KeyChord) -> Option<Key> {
    Some(match action_for(keys, chord)? {
        Action::Down => Key::Down,
        Action::Up => Key::Up,
        Action::Jump | Action::Accept => Key::Jump,
        Action::Search => Key::Search,
        Action::Filter => Key::CycleState,
        Action::Reset | Action::Cancel => Key::AllStates,
        Action::Help => Key::Help,
        Action::Versions => Key::Versions,
        Action::Close => Key::Close,
        Action::Backspace => Key::Backspace,
        Action::Clear => Key::ClearSearch,
    })
}

/// Keys the daemon decodes with. Its FIFO carries the fixed `agenmux key`
/// protocol, not physical keys: the tmux tables already resolved the user's
/// chords into action names. The user's own keymap stays the one hints and
/// help are named from, so both modes advertise the keys that actually work.
fn protocol_keys(mode: KeyMode) -> &'static Keymap {
    static KEYS: std::sync::OnceLock<(Keymap, Keymap)> = std::sync::OnceLock::new();
    let (normal, search) = KEYS.get_or_init(|| {
        (
            crate::app_config::resolved_keys(KeyMode::Normal, None).unwrap(),
            crate::app_config::resolved_keys(KeyMode::Search, None).unwrap(),
        )
    });
    match mode {
        KeyMode::Normal => normal,
        KeyMode::Search => search,
    }
}

fn read_key(fd: libc::c_int, keys: &Keymap) -> Key {
    let Some(b) = read_byte(fd) else {
        return Key::Quit;
    }; // EOF: explicit close
    // Every byte of the tail goes through the same polling reader. In mirror
    // mode keys arrive over a non-blocking FIFO that the key sender feeds one
    // byte at a time, so the tail is routinely still in flight; reading it
    // without polling hit EAGAIN and dropped every other arrow.
    let next = || {
        poll_fd(fd, Some(Duration::from_millis(50)))
            .then(|| read_byte(fd))
            .flatten()
    };
    match b {
        0x01 => return Key::WheelUp,
        0x02 => return Key::WheelDown,
        0x0c => return Key::AllStates, // private clear packet used by tmux/click helpers
        // Search-table printable keys use a NUL-prefixed packet so normal-mode
        // actions such as `j`, `q`, and `f` remain query text while typing.
        0x00 => {
            return next()
                .filter(|b| (0x20..=0x7e).contains(b))
                .map(|b| Key::Text(char::from(b).to_string()))
                .unwrap_or(Key::Other)
        }
        // Click target: a four-byte row index the mouse helper sends.
        0x05 => {
            let mut index = [0u8; 4];
            for byte in &mut index {
                let Some(got) = next() else {
                    return Key::Other;
                };
                *byte = got;
            }
            return Key::Select(u32::from_be_bytes(index) as usize);
        }
        // Multi-key sequence such as `gg`, delivered as one packet.
        0x06 => {
            return next()
                .filter(|b| (0x20..=0x7e).contains(b))
                .map(|b| Key::Sequence(char::from(b)))
                .unwrap_or(Key::Other)
        }
        _ => {}
    }
    // Configured chords win; the fixed edge keys are the default underneath.
    chord(b, next)
        .and_then(|chord| action_key(keys, chord))
        .unwrap_or(match b {
            b'G' => Key::Last,
            b'g' => Key::Sequence('g'),
            0x03 | 0x04 => Key::Quit, // Ctrl-C, Ctrl-D: emergency exit
            _ => Key::Other,
        })
}

/// Popup/tty search owns printable input. Daemon search receives printable
/// bytes through NUL-prefixed packets decoded by read_key instead.
fn read_search_key(fd: libc::c_int, keys: &Keymap) -> Key {
    let Some(first) = read_byte(fd) else {
        return Key::Quit;
    };
    let next = || {
        poll_fd(fd, Some(Duration::from_millis(50)))
            .then(|| read_byte(fd))
            .flatten()
    };
    if first >= 0x20 && first != 0x7f {
        let len = if first < 0x80 {
            1
        } else if first & 0xe0 == 0xc0 {
            2
        } else if first & 0xf0 == 0xe0 {
            3
        } else if first & 0xf8 == 0xf0 {
            4
        } else {
            return Key::Other;
        };
        let mut bytes = vec![first];
        for _ in 1..len {
            let Some(b) = next() else {
                return Key::Other;
            };
            bytes.push(b);
        }
        return String::from_utf8(bytes)
            .map(Key::Text)
            .unwrap_or(Key::Other);
    }
    chord(first, next)
        .and_then(|chord| action_key(keys, chord))
        .unwrap_or(match first {
            0x03 | 0x04 => Key::Quit,
            _ => Key::Other,
        })
}

/// Deliver one key-table action to the daemon without waiting for a FIFO
/// reader. Each invocation is intentionally short-lived; the daemon remains
/// the only persistent agenmux process.
pub fn send_key(name: &str) -> i32 {
    let bytes: Vec<u8> = if let Some(hex) = name.strip_prefix("text-") {
        let Ok(byte) = u8::from_str_radix(hex, 16) else {
            return 2;
        };
        if !(0x20..=0x7e).contains(&byte) {
            return 2;
        }
        vec![0, byte]
    } else if let Some(hex) = name.strip_prefix("sequence-") {
        let Ok(byte) = u8::from_str_radix(hex, 16) else {
            return 2;
        };
        if !(0x20..=0x7e).contains(&byte) {
            return 2;
        }
        vec![0x06, byte]
    } else {
        match name {
            "last" => b"G".to_vec(),
            "up" => b"\x1b[A".to_vec(),
            "down" => b"\x1b[B".to_vec(),
            "enter" => b"\r".to_vec(),
            "escape" => b"\x1b".to_vec(),
            "backspace" => vec![0x7f],
            "clear-search" => vec![0x15],
            "search" => b"/".to_vec(),
            "filter" => b"f".to_vec(),
            "all" => vec![0x0c],
            "space" => b" ".to_vec(),
            "j" => b"j".to_vec(),
            "k" => b"k".to_vec(),
            "wheel-up" => vec![0x01],
            "wheel-down" => vec![0x02],
            "l" => b"l".to_vec(),
            // Legacy alias for the popup-only quit: the daemon ignores it like
            // Ctrl-C, whereas a printable `q` would now resolve to close.
            "q" => vec![0x03],
            "close" => b"Q".to_vec(),
            "help" => b"?".to_vec(),
            "versions" => b"u".to_vec(),
            _ => return 2,
        }
    };
    send_bytes(&bytes)
}

pub fn select(index: usize) -> i32 {
    let Ok(index) = u32::try_from(index) else {
        return 2;
    };
    let mut bytes = vec![0x05];
    bytes.extend(index.to_be_bytes());
    send_bytes(&bytes)
}

fn send_bytes(bytes: &[u8]) -> i32 {
    let path = crate::tmux::runtime_dir().join("agenmux-keys");
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return 1;
    };
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        return 1;
    }
    let wrote = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    unsafe { libc::close(fd) };
    (wrote != bytes.len() as isize) as i32
}

/// The release this engine belongs to. install-bin.sh installs the binary that
/// matches the checkout's Cargo.toml, so this is also the plugin's version.
fn current_tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

fn app_title() -> String {
    // The isolated renderer fixture child needs identical title geometry in
    // debug/release builds. Production builds have no test override.
    #[cfg(test)]
    if std::env::var_os("AGENMUX_THEME_TEST_CHILD").is_some() {
        return "agenmux dev (2000-01-01 00:00)".into();
    }
    if cfg!(debug_assertions) {
        format!(
            "agenmux dev ({})",
            option_env!("AGENMUX_BUILD_TIMESTAMP").unwrap_or("unknown")
        )
    } else {
        format!("agenmux {}", current_tag())
    }
}

/// Newest release, as recorded by install-bin.sh's (at most daily) check.
/// None unless it is strictly newer than what is running: a checkout ahead of
/// every release (master, or a just-bumped manifest) must not be told to
/// "update" to the older tag behind it.
fn update_available(plugin_dir: &PathBuf) -> Option<String> {
    let release_dir = plugin_dir.join("target/release");
    let latest = std::fs::read_to_string(release_dir.join(".agenmux-latest"))
        .or_else(|_| std::fs::read_to_string(release_dir.join(".agents-mon-latest")))
        .ok()?;
    let latest = latest.trim();
    (is_tag(latest) && newer_than(latest, &current_tag())).then(|| latest.to_string())
}

/// Numeric, component-wise tag compare: is `a` a later release than `b`?
/// String order is not enough — "v0.1.10" sorts before "v0.1.9".
fn newer_than(a: &str, b: &str) -> bool {
    let parts = |t: &str| -> Vec<u64> {
        t.trim_start_matches('v')
            .split(['.', '-'])
            .map(|s| s.parse().unwrap_or(0))
            .collect()
    };
    let (x, y) = (parts(a), parts(b));
    for i in 0..x.len().max(y.len()) {
        let (l, r) = (
            x.get(i).copied().unwrap_or(0),
            y.get(i).copied().unwrap_or(0),
        );
        if l != r {
            return l > r;
        }
    }
    false
}

/// Releases install-bin.sh saw on the remote, newest first.
fn known_tags(plugin_dir: &PathBuf) -> Vec<String> {
    let release_dir = plugin_dir.join("target/release");
    let raw = std::fs::read_to_string(release_dir.join(".agenmux-tags"))
        .or_else(|_| std::fs::read_to_string(release_dir.join(".agents-mon-tags")))
        .unwrap_or_default();
    let mut tags: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|t| is_tag(t))
        .map(String::from)
        .collect();
    tags.truncate(10);
    tags
}

fn picker_sel(tags: &[String], cur: &str, chosen: Option<&str>, sel: usize) -> usize {
    let selected = chosen
        .and_then(|tag| tags.iter().position(|t| t == tag))
        .or_else(|| {
            chosen
                .is_none()
                .then(|| tags.iter().position(|t| t == cur))
                .flatten()
        })
        .unwrap_or(sel);
    selected.min(tags.len().saturating_sub(1))
}

/// A tag is passed to update.sh as an argument — keep it boring.
fn is_tag(t: &str) -> bool {
    t.len() > 1
        && t.starts_with('v')
        && t[1..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// One preserved pane as measured by mirror_tick.
struct M {
    pane: String,
    win: String,
    sess: String,
    w: usize,
    h: usize,
    win_size: (usize, usize),
    panes: usize,
    active: bool,
}

/// Height to render the shared frame at. NOT the minimum: every visible pane
/// shows the same frame, so folding to the shortest let one stale 23-row window
/// in a session nobody is looking at clip the list everywhere. Size to the
/// mirror the user is actually watching — mirrors shorter than the frame clip
/// themselves in mirror::draw.
fn row_filter_text(row: &PaneRow) -> String {
    format!(
        "{} {} {} {} {}",
        row.agent, row.loc, row.cwd, row.title, row.state
    )
    .to_lowercase()
}

/// Filter projection: status matching is exact and separate from text search.
/// Matching a session keeps its whole agent subtree as context;
/// matching an agent keeps that session's header through normal rendering.
fn filtered_indices(
    rows: &[PaneRow],
    query: &str,
    state_filter: Option<StateFilter>,
) -> Vec<usize> {
    if let Some(filter) = state_filter {
        return rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.state == filter.label())
            .map(|(i, _)| i)
            .collect();
    }
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return (0..rows.len()).collect();
    }
    let matching_sessions: HashSet<&str> = rows
        .iter()
        .filter_map(|row| {
            let session = row.loc.split(':').next().unwrap_or("");
            session.to_lowercase().contains(&query).then_some(session)
        })
        .collect();
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            let session = row.loc.split(':').next().unwrap_or("");
            matching_sessions.contains(session) || row_filter_text(row).contains(&query)
        })
        .map(|(i, _)| i)
        .collect()
}

fn cursor_row(
    rows: &[PaneRow],
    visible: &[usize],
    selected: usize,
    plugin_selected: bool,
    active: &str,
) -> Option<usize> {
    if plugin_selected {
        return selected.checked_sub(1).filter(|&i| i < visible.len());
    }
    visible.iter().position(|&i| rows[i].pane == active)
}

fn watched_height(ms: &[M], active_session: &str) -> usize {
    ms.iter()
        .find(|m| m.active && m.sess == active_session)
        .map(|m| m.h)
        // several clients on several sessions, or no client measured yet
        .or_else(|| ms.iter().filter(|m| m.active).map(|m| m.h).max())
        .or_else(|| ms.iter().map(|m| m.h).max())
        .unwrap_or(24)
}

/// Should the daemon shut down after measuring no preserved panes? Only once
/// panes existed (or the startup grace ran out) AND the emptiness repeats:
/// a hook's run-shell block can desync the control pipe for exactly one
/// command, and a single garbage read must not tear down the whole mirror set.
fn suicide(seen_mirror: bool, since_start: Duration, empty_ticks: u32) -> bool {
    (seen_mirror || since_start >= Duration::from_secs(30)) && empty_ticks >= 2
}

/// Has `@agenmux-control-client` named someone else? Only a claim counts:
/// teardown.sh's unset can land after the next daemon claimed it, and treating
/// empty as "replaced" makes a reopened sidebar kill its own daemon.
fn superseded(mine: &str, current: &str) -> bool {
    let current = current.trim();
    !mine.is_empty() && !current.is_empty() && current != mine
}

/// Headless-mode state: frames → visible panes, keys ← FIFO, size ← panes.
struct Daemon {
    keys_path: PathBuf,
    keys_fd: libc::c_int,
    writers: PaneWriters,
    size: (usize, usize), // narrowest pane x watched pane's height
    seen_mirror: bool,    // suicide only arms after the first pane appears
    empty_ticks: u32,     // consecutive measurements that found no pane
    client: String,       // our control client, as published in the option
    started: Instant,
    // window id -> (window size, pane count) at the last measure: a mirror
    // whose width changed while both stayed put is a user border-drag. The
    // pane count matters: a closing pane hands its columns to the mirror
    // (all of them, when the mirror is the last pane left) without changing
    // the window size — width-only would adopt that as the global width
    win_sizes: HashMap<String, ((usize, usize), usize)>,
    // session the control client is attached to: layout/focus notifications
    // are session-scoped, so the client follows the user's active session
    attached: String,
}

pub struct Sidebar {
    tmux: Tmux,
    settings: crate::app_config::LiveConfig,
    palette: Palette, // immutable startup snapshot shared by popup and split
    normal_keys: Keymap, // startup snapshot: keys never change while running
    search_keys: Keymap,
    confs: Vec<AgentConf>,
    ident: IdentCache,
    subj: scan::SubjectCache,
    tracker: Tracker,
    rows: Vec<PaneRow>, // complete debounced view-model; never filter cache/status
    visible: Vec<usize>, // filtered indexes used by render/navigation/clicks
    query: String,
    state_filter: Option<StateFilter>,
    search_focused: bool,
    key_sequence: KeySequence,
    sel: usize,    // 1-based index into visible, like the bash script
    scroll: usize, // first visible list line
    follow_selection: bool,
    sel_pane: String,
    last_active: String,
    active: String,
    active_session: String,
    plugin_selected: bool,
    tick: u32,
    self_pane: String,
    pin: Option<String>,
    popup_client: String,
    plugin_dir: PathBuf,
    rows_file: PathBuf,
    cache_file: PathBuf,
    last_frame: String,
    update: Option<String>, // newer release to advertise in the header
    daemon: Option<Daemon>,
    overlay: Option<Overlay>,
}

/// `self_pane` is the pane the sidebar itself occupies, skipped by every scan.
/// The daemon is headless and owns no pane, so it MUST pass "" — inheriting
/// TMUX_PANE from whoever pressed the toggle key hid that pane's agent.
fn new_sidebar(
    tmux: Tmux,
    plugin_dir: PathBuf,
    cache_file: PathBuf,
    rows_file: PathBuf,
    self_pane: String,
    settings: crate::app_config::AppConfig,
) -> Sidebar {
    let confs = crate::conf::load_all(&plugin_dir);
    // read once: the check behind it runs at most daily, and switching version
    // restarts the engine anyway
    let update = update_available(&plugin_dir);
    let mut sb = Sidebar {
        tmux,
        palette: Palette::resolve(&settings.theme),
        normal_keys: settings.normal.clone(),
        search_keys: settings.search.clone(),
        settings: crate::app_config::LiveConfig::new(settings),
        confs,
        ident: IdentCache::new(),
        subj: scan::SubjectCache::new(),
        tracker: Tracker::default(),
        rows: Vec::new(),
        visible: Vec::new(),
        query: String::new(),
        state_filter: None,
        search_focused: false,
        key_sequence: KeySequence::default(),
        sel: 1,
        scroll: 0,
        sel_pane: String::new(),
        follow_selection: true,
        last_active: String::new(),
        active: String::new(),
        active_session: String::new(),
        plugin_selected: false,
        tick: 0,
        self_pane,
        pin: None,
        popup_client: crate::compat_env("AGENMUX_POPUP_CLIENT", "AGENTS_MON_POPUP_CLIENT")
            .unwrap_or_default(),
        plugin_dir,
        rows_file,
        cache_file,
        last_frame: String::new(),
        update,
        daemon: None,
        overlay: None,
    };
    // seed from the previous instance's scan for an instant first frame
    if let Ok(tsv) = std::fs::read_to_string(&sb.cache_file) {
        sb.rows = scan::from_tsv(&tsv);
        sb.rows.retain(|r| r.pane != sb.self_pane);
    }
    sb.rebuild_visible(false);
    sb
}

pub fn run(plugin_dir: PathBuf, cache_file: PathBuf) -> i32 {
    // Catch termination during startup validation too; no terminal or tmux
    // mutation happens until configuration has passed validation.
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
    }
    let settings = match crate::app_config::current(None) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return e.exit_code();
        }
    };
    let self_pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let pin = crate::compat_env("AGENMUX_PIN", "AGENTS_MON_PIN").filter(|p| !p.is_empty());
    let rows_file = std::env::temp_dir().join(format!(
        "agenmux-rows-{}",
        self_pane.trim_start_matches('%')
    ));

    if QUIT.load(Ordering::Relaxed) {
        cleanup(&rows_file, &pin);
        return 0;
    }
    let _raw = RawMode::enable();
    print!("{E}[?25l{E}[2J");
    let _ = std::io::stdout().flush();

    let tmux = match Tmux::connect() {
        Ok(t) => t,
        Err(_) => {
            cleanup(&rows_file, &pin);
            return 0;
        }
    };
    let mut sb = new_sidebar(tmux, plugin_dir, cache_file, rows_file, self_pane, settings);
    sb.pin = pin;
    // tty mode is the popup: while it is visible it owns input.
    sb.plugin_selected = true;
    sb.render(true);
    event_loop(&mut sb);
    cleanup(&sb.rows_file, &sb.pin);
    0
}

/// Headless engine for preserved-pane mode: renders through live pane writers,
/// reads keys from a FIFO, and sizes itself from the preserved panes. Exits
/// (with full teardown) when the last pane disappears.
pub fn run_daemon(plugin_dir: PathBuf, cache_file: PathBuf) -> i32 {
    let settings = match crate::app_config::current(None) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return e.exit_code();
        }
    };
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
    }
    let tmp = std::env::temp_dir();
    let keys_path = tmp.join("agenmux-keys");
    // A previous mirror-based daemon used this as its liveness heartbeat.
    let _ = std::fs::remove_file(tmp.join("agenmux-frame"));
    let _ = std::fs::remove_file(&keys_path);
    let c = std::ffi::CString::new(keys_path.as_os_str().as_encoded_bytes()).unwrap();
    // O_RDWR: the FIFO never hits EOF as key senders come and go
    let keys_fd = unsafe {
        libc::mkfifo(c.as_ptr(), 0o600);
        libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK)
    };
    if keys_fd < 0 {
        return 1;
    }
    let tmux = match Tmux::connect() {
        Ok(t) => t,
        Err(_) => return 1,
    };
    // "" not TMUX_PANE: toggle.sh launches the daemon from the pane the user
    // pressed the key in, and adopting that pane would hide its agent
    let mut sb = new_sidebar(
        tmux,
        plugin_dir,
        cache_file,
        tmp.join("agenmux-rows"),
        String::new(),
        settings,
    );
    // `display-message '#{client_name}'` can briefly be empty when a busy
    // server already has a focused terminal client. Match the control client
    // tmux just spawned by PID instead; that identity is unambiguous.
    let control_pid = sb.tmux.client_pid().to_string();
    // unescaped: show-option hands the value back unescaped too
    let mut control_client = String::new();
    for _ in 0..100 {
        let client = sb
            .tmux
            .run("list-clients -F '#{client_pid}\t#{client_name}'")
            .ok()
            .and_then(|clients| {
                clients.lines().find_map(|line| {
                    let (pid, name) = line.split_once('\t')?;
                    (pid == control_pid && !name.is_empty()).then(|| name.to_string())
                })
            });
        if let Some(client) = client {
            let quoted = client.replace('\'', "\\'");
            if sb.tmux.run(&format!("set-option -g @agenmux-control-client '{quoted}'")).is_err() {
                return 1;
            }
            control_client = client;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let quoted_tmp = tmp.to_string_lossy().replace('\'', "\\'");
    if control_client.is_empty() || sb.tmux.run(&format!(
        "set-option -g @agenmux-runtime-dir '{quoted_tmp}'"
    )).is_err() {
        return 1;
    }
    sb.daemon = Some(Daemon {
        keys_path,
        keys_fd,
        writers: PaneWriters::new(),
        size: (30, 24),
        seen_mirror: false,
        empty_ticks: 0,
        client: control_client,
        started: Instant::now(),
        win_sizes: HashMap::new(),
        attached: String::new(),
    });
    // Publish nothing until the first preserved pane has been measured: its
    // size IS the frame size, and a default-sized first frame visibly resizes
    // once the real measurement lands. Deriving the size instead of measuring it
    // does not work — pane-border-status silently costs a row. toggle.sh creates
    // the panes right after us, so this wait is short.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(3) {
        if !sb.mirror_tick() || sb.daemon.as_ref().unwrap().seen_mirror {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    sb.render(true);
    if std::env::var_os("AGENMUX_STARTUP_ACK").is_some() {
        use std::io::Write;
        if sb.daemon.as_ref().is_none_or(|d| d.client.is_empty() || !d.seen_mirror)
            || std::io::stdout().write_all(b"R").is_err()
            || std::io::stdout().flush().is_err()
        {
            sb.quiet_exit();
            return 1;
        }
    }
    if event_loop(&mut sb) {
        sb.quiet_exit();
    } else {
        sb.teardown();
    }
    0
}

/// Returns true when the loop ended because another daemon took ownership.
fn event_loop(sb: &mut Sidebar) -> bool {
    let key_fd = sb.daemon.as_ref().map_or(0, |d| d.keys_fd);
    let mut next_scan = Instant::now(); // scan immediately
    let mut next_tick = Instant::now();
    loop {
        if QUIT.load(Ordering::Relaxed) {
            break;
        }
        let mut now = Instant::now();
        if now >= next_scan {
            match sb.scan_tick() {
                Ok(()) => {}
                // a pipe I/O error can leave a response block half-read —
                // the pipe is desynced, restarting is the only safe move
                Err(TmuxError::Exited) | Err(TmuxError::Io(_)) => break,
                Err(TmuxError::Error(_)) => {} // e.g. pane died mid-scan
            }
            if sb.daemon.is_some() && sb.superseded() {
                return true; // a newer daemon owns the panes now
            }
            if sb.daemon.is_some() && !sb.mirror_tick() {
                break; // all preserved panes gone — nothing left to display
            }
            if sb.daemon.as_ref().is_some_and(|d| !d.keys_path.exists()) {
                break; // runtime dir vanished: deaf to keys, better gone than a zombie
            }
            sb.render(false);
            // a scan takes tens of ms — with the pre-scan `now`, a tick due
            // mid-scan is missed and the poll sleeps its full stale remainder
            now = Instant::now();
            next_scan = now + Duration::from_secs(2);
        }
        let animating = sb
            .visible
            .iter()
            .any(|&i| matches!(sb.rows[i].state.as_str(), "working" | "blocked" | "done"));
        // deadline-based tick: held keys keep poll_inputs returning early, so
        // advancing on poll timeout would freeze the spinner during key repeat
        if animating && now >= next_tick {
            sb.tick = (sb.tick + 1) % 40; // divisible by 8 (spin) and 4 (blink)
            next_tick = now + Duration::from_millis(250);
            sb.render(false);
        }
        // animated states need ticks; all-idle sleeps until the next scan
        let wake = if animating {
            next_tick.saturating_duration_since(now)
        } else {
            next_scan.saturating_duration_since(now)
        };
        let (key_ready, pipe_ready) = poll_inputs(key_fd, sb.tmux.fd(), sb.tmux.buffered(), wake);
        if pipe_ready {
            // focus notification (%window-pane-changed etc.) — rescan now so
            // the cursor snaps to the newly focused pane without the 2s wait
            match sb.tmux.drain_notifications() {
                Ok(true) => next_scan = Instant::now(),
                Ok(false) => {}
                Err(_) => break,
            }
        }
        if key_ready {
            let mode = if sb.search_focused {
                KeyMode::Search
            } else {
                KeyMode::Normal
            };
            let keys = if sb.daemon.is_some() {
                protocol_keys(mode)
            } else if sb.search_focused {
                &sb.search_keys
            } else {
                &sb.normal_keys
            };
            let key = if sb.search_focused && sb.daemon.is_none() {
                read_search_key(key_fd, keys)
            } else {
                read_key(key_fd, keys)
            };
            match sb.dispatch_key(key) {
                DispatchResult::Continue => {}
                DispatchResult::Break => break,
                DispatchResult::QuietExit => return true,
            }
            sb.render(false);
        }
        if sb.daemon.is_none() && WINCH.swap(false, Ordering::Relaxed) {
            print!("{E}[2J");
            sb.render(true);
        }
    }
    false // every other exit is a real close: the caller tears down
}

fn cleanup(rows_file: &PathBuf, pin: &Option<String>) {
    print!("{E}[?25h");
    let _ = std::io::stdout().flush();
    let _ = std::fs::remove_file(rows_file);
    if let Some(p) = pin {
        // keep the pin when a jump is pending — native toggle reopens the popup
        if !std::path::Path::new(&format!("{p}.jump")).exists() {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Sidebar {
    /// Route every logical key through the active UI mode. Mouse selection is
    /// handled before mode dispatch; overlays clear their row map, so they cannot
    /// receive a stale click on the hidden list.
    /// "<first chord> <what>", or nothing when the action is unbound.
    fn hint(&self, keys: &Keymap, action: Action, what: &str) -> String {
        keys[&action]
            .first()
            .map_or(String::new(), |c| format!("{} {what}", c.label(true)))
    }
    fn hints(&self, parts: &[(Action, &str)]) -> String {
        join(&parts
            .iter()
            .map(|(action, what)| self.hint(&self.normal_keys, *action, what))
            .collect::<Vec<_>>())
    }
    /// Every chord of an action, "/"-joined: "Enter/l".
    fn labels(&self, keys: &Keymap, action: Action, short: bool) -> String {
        keys[&action]
            .iter()
            .map(|c| c.label(short))
            .collect::<Vec<_>>()
            .join("/")
    }
    /// Down/up pairs: "j/k" (first pair) or "j/k ↓/↑" (all pairs).
    fn nav_label(&self, short: bool, all: bool) -> String {
        let (down, up) = (&self.normal_keys[&Action::Down], &self.normal_keys[&Action::Up]);
        let pairs = if all { down.len().max(up.len()) } else { 1 };
        (0..pairs)
            .filter_map(|i| match (down.get(i), up.get(i)) {
                (Some(d), Some(u)) => Some(format!("{}/{}", d.label(short), u.label(short))),
                (Some(c), None) | (None, Some(c)) => Some(c.label(short)),
                (None, None) => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn dispatch_key(&mut self, key: Key) -> DispatchResult {
        if let Key::Sequence(key) = key {
            return match self
                .key_sequence
                .push(key, Instant::now(), &[("gg", Key::First)])
            {
                SequenceResult::Match(action) => self.dispatch_key(action),
                SequenceResult::Pending | SequenceResult::Miss => DispatchResult::Continue,
            };
        }
        self.key_sequence.clear();
        if let Key::Select(index) = &key {
            self.select_index(*index);
            return DispatchResult::Continue;
        }
        match dispatch_mode(self.overlay.as_ref(), self.search_focused) {
            DispatchMode::Overlay => {
                self.overlay_key(key);
                return DispatchResult::Continue;
            }
            DispatchMode::Search => {
                self.search_key(key);
                return DispatchResult::Continue;
            }
            DispatchMode::Normal => {}
        }
        match key {
            Key::First => self.select_index(1),
            Key::Last => self.select_index(self.visible.len()),
            Key::Down => self.move_sel(1),
            Key::Up => self.move_sel(-1),
            Key::WheelUp => self.scroll_viewport(-1),
            Key::WheelDown => self.scroll_viewport(1),
            Key::Jump => {
                if self.jump() {
                    return DispatchResult::Break;
                }
            }
            Key::Help => self.help(),
            Key::Versions => self.versions(),
            Key::Search => self.focus_search(),
            Key::CycleState => self.cycle_state_filter(),
            Key::AllStates => self.clear_filter(),
            Key::Quit => {
                if self.daemon.is_none() {
                    // Popup/tty mode owns stdin, so q/Ctrl-C/Ctrl-D closes it.
                    if let Some(p) = &self.pin {
                        let _ = std::fs::remove_file(p);
                    }
                    return DispatchResult::Break;
                }
                // In preserved-pane mode close arrives as Key::Close from the
                // key table; Quit also covers FIFO EOF, which must not kill it.
            }
            Key::Close => {
                if self.daemon.is_some() {
                    // Finish teardown before a fast reopen can observe the
                    // dying control client and attach panes to it.
                    self.teardown();
                    self.daemon = None;
                    return DispatchResult::QuietExit;
                }
                return DispatchResult::Break;
            }
            Key::Sequence(_)
            | Key::Backspace
            | Key::ClearSearch
            | Key::Text(_)
            | Key::Select(_)
            | Key::Other => {}
        }
        DispatchResult::Continue
    }

    /// Adopt whatever a `config reload` just published. Keys the tmux tables
    /// own were reinstalled by the reload itself; these are the parts this
    /// process holds: how it paints, and which chords it names in its hints.
    fn adopt_reload(&mut self, refreshed: crate::app_config::Refreshed) {
        if !refreshed.reloaded {
            return;
        }
        self.palette = Palette::resolve(&self.settings.settings.theme);
        self.normal_keys = self.settings.settings.normal.clone();
        self.search_keys = self.settings.settings.search.clone();
        self.last_frame.clear(); // colors changed: no diff against old bytes
    }

    fn scan_tick(&mut self) -> Result<(), TmuxError> {
        if self.daemon.is_none() {
            let refreshed = self.settings.refresh(&mut self.tmux);
            self.adopt_reload(refreshed);
        }
        let t0 = Instant::now();
        let scanned = scan::scan(
            &mut self.tmux,
            &self.confs,
            &mut self.ident,
            &mut self.subj,
            Some(&self.self_pane),
        )?;
        crate::tmux::debug_note(&format!("scan {}ms", t0.elapsed().as_millis()));
        let _ = std::fs::write(&self.cache_file, scan::to_tsv(&scanned));
        let mut focus = self.client_focus().unwrap_or_default();
        // A popup owns the terminal's input even though tmux still reports the
        // pane underneath it as selected. That underlying agent is not being
        // viewed while the popup is open.
        if self.daemon.is_none() {
            focus.discount_client(&self.popup_client);
            focus.plugin_selected = true;
        }
        self.active = focus.active_pane;
        self.active_session = focus.active_session;
        self.plugin_selected = focus.plugin_selected;
        // notifications only cover the attached session — follow the user so
        // drags and focus changes where they're looking react instantly
        // (background sessions wait for the 2s scan, which nobody can see)
        if self.daemon.is_some()
            && !self.active_session.is_empty()
            && self.daemon.as_ref().unwrap().attached != self.active_session
        {
            let sid = self.active_session.clone();
            if self.tmux.run(&format!("switch-client -t '{sid}'")).is_ok() {
                // insurance: keep pane output off the control pipe
                let _ = self.tmux.run("refresh-client -f no-output");
                self.daemon.as_mut().unwrap().attached = sid;
            }
        }

        let update = self.tracker.update(scanned, &focus.focused_panes);
        self.rows = update.rows;
        for event in &update.events {
            let _ = crate::notifications::deliver(self.settings.settings.notifications, event);
        }
        self.rebuild_visible(false);
        // single cursor: focus landing on a visible agent pane snaps selection
        // to it; active filters never select a row they intentionally hid
        if !self.active.is_empty() && self.active != self.last_active {
            if let Some(i) = self
                .visible
                .iter()
                .position(|&i| self.rows[i].pane == self.active)
            {
                self.sel = i + 1;
                self.follow_selection = true;
                self.sel_pane = self.active.clone();
            }
            self.last_active = self.active.clone();
        }
        Ok(())
    }

    fn client_focus(&mut self) -> Option<crate::focus::ClientFocus> {
        // scan() only syncs at its START; a hook's run-shell block landing
        // during the capture loop leaves every later command paired with the
        // wrong response. Re-barrier before reading anything we act on.
        self.tmux.sync().ok()?;
        let focus_events = self
            .tmux
            .run("show-option -gqv focus-events")
            .ok()
            .is_some_and(|value| value.trim() == "on");
        let out = self
            .tmux
            .run("list-clients -F '#{client_activity}\t#{client_name}\t#{session_id}\t#{pane_id}\t#{pane_title}\t#{client_flags}'")
            .ok()?;
        Some(crate::focus::parse_clients(&out, focus_events))
    }

    fn select_index(&mut self, index: usize) {
        self.sel = index.max(1);
        self.follow_selection = true;
        self.clamp_sel();
        self.sync_sel_pane();
    }

    fn move_sel(&mut self, d: i64) {
        self.select_index((self.sel as i64 + d).max(1) as usize);
    }

    fn scroll_viewport(&mut self, d: i64) {
        self.scroll = (self.scroll as i64 + d).max(0) as usize;
        self.follow_selection = false;
    }

    fn clamp_sel(&mut self) {
        if self.sel > self.visible.len() {
            self.sel = self.visible.len();
        }
        if self.sel < 1 {
            self.sel = 1;
        }
    }

    fn sync_sel_pane(&mut self) {
        self.sel_pane = self
            .visible
            .get(self.sel.wrapping_sub(1))
            .and_then(|&i| self.rows.get(i))
            .map(|r| r.pane.clone())
            .unwrap_or_default();
    }

    fn restore_sel(&mut self) {
        // after a rescan/filter, follow the remembered pane when it remains
        // visible; otherwise keep the nearest valid result
        if self.sel_pane.is_empty() {
            self.sync_sel_pane();
            return;
        }
        match self
            .visible
            .iter()
            .position(|&i| self.rows[i].pane == self.sel_pane)
        {
            Some(i) => self.sel = i + 1,
            None => {
                self.clamp_sel();
                self.sync_sel_pane();
            }
        }
    }

    fn rebuild_visible(&mut self, select_first: bool) {
        self.visible = filtered_indices(&self.rows, &self.query, self.state_filter);
        if select_first {
            self.sel = 1;
            self.follow_selection = true;
            self.sync_sel_pane();
        } else {
            self.clamp_sel();
            self.restore_sel();
        }
        if self.visible.is_empty() {
            self.sel_pane.clear();
        }
    }

    fn focus_search(&mut self) {
        self.state_filter = None;
        self.search_focused = true;
        self.rebuild_visible(false);
    }

    fn cycle_state_filter(&mut self) {
        self.query.clear();
        self.state_filter = StateFilter::cycle(self.state_filter);
        self.search_focused = false;
        self.rebuild_visible(true);
    }

    fn clear_filter(&mut self) {
        self.query.clear();
        self.state_filter = None;
        self.search_focused = false;
        self.rebuild_visible(false);
    }

    fn search_key(&mut self, key: Key) {
        match key {
            Key::Quit | Key::Close => self.clear_filter(),
            // First Enter accepts query and hands j/k back to filtered
            // navigation. Enter in normal mode then jumps to selection.
            Key::Jump => self.search_focused = false,
            Key::Down => self.move_sel(1),
            Key::Up => self.move_sel(-1),
            Key::Backspace => {
                self.state_filter = None;
                self.query.pop();
                self.rebuild_visible(true);
            }
            Key::ClearSearch => {
                self.query.clear();
                self.state_filter = None;
                self.rebuild_visible(false);
            }
            Key::Text(text) => {
                self.state_filter = None;
                let room = 256usize.saturating_sub(self.query.chars().count());
                self.query
                    .extend(text.chars().filter(|c| !c.is_control()).take(room));
                self.rebuild_visible(true);
            }
            Key::WheelUp => self.scroll_viewport(-1),
            Key::WheelDown => self.scroll_viewport(1),
            Key::AllStates => self.clear_filter(),
            Key::First
            | Key::Last
            | Key::Select(_)
            | Key::Sequence(_)
            | Key::Search
            | Key::CycleState
            | Key::Help
            | Key::Versions
            | Key::Other => {}
        }
    }

    /// true = exit the loop (popup jump hands off to native toggle)
    fn jump(&mut self) -> bool {
        let Some(target) = self
            .visible
            .get(self.sel.wrapping_sub(1))
            .and_then(|&i| self.rows.get(i))
            .map(|r| r.pane.clone())
        else {
            return false;
        };
        if !target.starts_with('%') {
            return false;
        }
        // This sidebar stays mounted after a jump, so clear its transient
        // navigator state before leaving.
        self.clear_filter();
        if let Some(pin) = &self.pin {
            // popup holds the client — hand the target to native toggle, which
            // jumps after the popup closes
            let _ = std::fs::write(format!("{pin}.jump"), &target);
            return true;
        }
        // switch/select MUST NOT go over the control pipe: they fire the
        // plugin's select-window/session hooks, and tmux delivers each hook's
        // run-shell result to the triggering client as an extra %begin/%end
        // block — desyncing every later response. Fork plain tmux instead
        // (jump is rare and user-initiated).
        // pick the most recently active client — with several terminals
        // attached, the first listed one may not be the one the user is
        // looking at (and the 'focused' flag sticks on all of them)
        let client = self
            .tmux
            .run("list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{client_activity} #{client_name}'")
            .ok()
            .and_then(|c| {
                c.lines()
                    .filter_map(|l| {
                        let (act, name) = l.split_once(' ')?;
                        Some((act.parse::<u64>().ok()?, name.to_string()))
                    })
                    .max_by_key(|(act, _)| *act)
                    .map(|(_, name)| name)
            });
        let mut args = Vec::new();
        if let Some(client) = &client {
            args.extend([
                "switch-client",
                "-c",
                client,
                "-t",
                target.as_str(),
                ";",
                "switch-client",
                "-c",
                client,
                "-T",
                "root",
                ";",
            ]);
        }
        args.extend([
            "select-window",
            "-t",
            target.as_str(),
            ";",
            "select-pane",
            "-t",
            target.as_str(),
        ]);
        let _ = command_status(&args);
        false
    }

    fn dot(&self, state: &str) -> String {
        let on = (self.tick / 2).is_multiple_of(2);
        let fg = self.palette.state_fg(state).fg("");
        match state {
            "blocked" => {
                if on {
                    format!("{fg}⣿{E}[0m")
                } else {
                    " ".into()
                }
            }
            "working" => format!("{fg}{}{E}[0m", SPIN[(self.tick % 8) as usize]),
            "done" => {
                if on {
                    format!("{fg}⣿{E}[0m")
                } else {
                    " ".into()
                }
            }
            _ => format!("{fg}⣿{E}[0m"),
        }
    }

    /// Refresh preserved-pane inventory: min pane size drives the render, zero
    /// panes (after at least one existed, or a 30s startup grace) = false.
    /// Also detects a user dragging a sidebar border — width changed while
    /// the window size and pane count did not, in a window the user can
    /// actually see — and
    /// adopts it as the global width. Serialization matters: one daemon
    /// doing this (instead of racing hook scripts) means no stale
    /// resize-pane ever fights the drag, and the dragged pane itself is
    /// never touched — only the hidden sidebars in other windows move.
    fn superseded(&mut self) -> bool {
        let Some(mine) = self.daemon.as_ref().map(|d| d.client.clone()) else {
            return false;
        };
        // Ownership decides whether we abandon every pane without teardown.
        // A stale hook response must not look like a newer daemon's claim.
        if self.tmux.sync().is_err() {
            return false;
        }
        match self.tmux.run("show-option -gqv @agenmux-control-client") {
            Ok(current) => superseded(&mine, &current),
            Err(_) => false, // a broken pipe is not a takeover; the loop exits elsewhere
        }
    }

    fn mirror_tick(&mut self) -> bool {
        // same barrier as active_pane: reading zero panes off a desynced
        // pipe used to tear the whole mirror set down
        let _ = self.tmux.sync();
        let refreshed = self.settings.refresh(&mut self.tmux);
        self.adopt_reload(refreshed);
        let width_changed = refreshed.width_changed;
        let out = self
            .tmux
            .run("list-panes -a -f '#{==:#{pane_title},agenmux}' -F '#{pane_id}\t#{window_id}\t#{pane_width}\t#{pane_height}\t#{window_width} #{window_height}\t#{window_panes}\t#{window_active}\t#{session_id}'")
            .unwrap_or_default();
        let mut w = usize::MAX;
        let mut ms: Vec<M> = Vec::new();
        for l in out.lines() {
            let f: Vec<&str> = l.split('\t').collect();
            let [pane, win, pw, ph, ws, wp, act, sess] = f.as_slice() else {
                continue;
            };
            let (Ok(pw), Ok(ph), Ok(wp)) = (
                pw.parse::<usize>(),
                ph.parse::<usize>(),
                wp.parse::<usize>(),
            ) else {
                continue;
            };
            let Some((ww, wh)) = ws
                .split_once(' ')
                .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            else {
                continue;
            };
            ms.push(M {
                pane: pane.to_string(),
                win: win.to_string(),
                sess: sess.to_string(),
                w: pw,
                h: ph,
                win_size: (ww, wh),
                panes: wp,
                active: *act == "1",
            });
        }
        if ms.is_empty() {
            let d = self.daemon.as_mut().unwrap();
            d.empty_ticks += 1;
            return !suicide(d.seen_mirror, d.started.elapsed(), d.empty_ticks);
        }
        self.daemon.as_mut().unwrap().empty_ticks = 0;
        let wopt = self.settings.settings.sidebar_width as usize;
        if width_changed {
            for m in &mut ms {
                let width = wopt.min(m.win_size.0.saturating_sub(2).max(1));
                if command_status(&["resize-pane", "-t", &m.pane, "-x", &width.to_string()]).is_ok()
                {
                    m.w = width;
                }
            }
        }
        // One sidebar per window. mirror-add.sh claims atomically now, but servers
        // that ran the old racy version still carry duplicates. Keep the first —
        // a -hbf split takes index 0, so that's the newest and the one actually at
        // the requested width; the squeezed leftovers go. This runs before the
        // width fold and the drag probe below, and it puts the survivor back at
        // wopt: every extra split squeezed it, and that squeezed width would
        // otherwise look exactly like a border drag and get adopted globally.
        let mut seen: HashSet<String> = HashSet::new();
        let dup: Vec<usize> = (0..ms.len())
            .filter(|&i| !seen.insert(ms[i].win.clone()))
            .collect();
        if !dup.is_empty() {
            let hit: HashSet<String> = dup.iter().map(|&i| ms[i].win.clone()).collect();
            // forked tmux: kill-pane fires hooks whose run-shell output would
            // desync the control pipe (same reason as the drag resize below)
            let mut argv: Vec<String> = Vec::new();
            for &i in &dup {
                if !argv.is_empty() {
                    argv.push(";".into());
                }
                argv.extend(["kill-pane".into(), "-t".into(), ms[i].pane.clone()]);
            }
            for &i in dup.iter().rev() {
                ms.remove(i);
            }
            // unconditional: the survivor absorbs the columns the killed panes
            // give back, so even one that measured wopt a moment ago ends up wide
            for m in ms.iter_mut().filter(|m| hit.contains(&m.win)) {
                argv.push(";".into());
                argv.extend([
                    "resize-pane".into(),
                    "-t".into(),
                    m.pane.clone(),
                    "-x".into(),
                    wopt.to_string(),
                ]);
                m.w = wopt;
            }
            let args: Vec<&str> = argv.iter().map(String::as_str).collect();
            let _ = command_status(&args);
        }
        // Width DOES fold to the minimum: mirror::draw clips rows but NOT
        // columns, so a frame wider than some pane would wrap and shift every
        // row below it (breaking the click -> rows-file mapping). Widths are
        // uniform by construction anyway, so the fold costs nothing.
        for m in &ms {
            w = w.min(m.w);
        }
        let h = watched_height(&ms, &self.active_session);
        let drag: Option<(String, usize)> = {
            let d = self.daemon.as_ref().unwrap();
            ms.iter()
                .find(|m| {
                    !width_changed
                        && m.active
                        && m.w != wopt.min(m.win_size.0.saturating_sub(2).max(1))
                        && d.win_sizes.get(&m.win) == Some(&(m.win_size, m.panes))
                })
                .map(|m| (m.pane.clone(), m.w))
        };
        if let Some((src_pane, width)) = drag {
            if (1..=10000).contains(&width)
                && self
                    .tmux
                    .run(&format!("set-option -g @agenmux-width {width}"))
                    .is_ok()
            {
                self.settings.settings.sidebar_width = width as u16;
                self.settings.settings.popup_width = width as u16;
            }
            // resize the OTHER sidebars via forked tmux (hook run-shell
            // echoes on the control pipe would desync it); the dragged pane
            // stays untouched so nothing ever fights the user's drag
            let mut argv = Vec::new();
            for m in ms.iter().filter(|m| m.pane != src_pane && m.w != width) {
                if !argv.is_empty() {
                    argv.push(";".to_string());
                }
                argv.extend([
                    "resize-pane".to_string(),
                    "-t".to_string(),
                    m.pane.clone(),
                    "-x".to_string(),
                    width.to_string(),
                ]);
            }
            if !argv.is_empty() {
                let args: Vec<&str> = argv.iter().map(String::as_str).collect();
                let _ = command_status(&args);
            }
            w = width; // render for the adopted width now, not the stale min
        }
        let visible_sessions: HashSet<String> = self
            .tmux
            .run("list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{session_id}'")
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        let visible_panes = if visible_sessions.is_empty() {
            // Detached startup and integration tests have no real client yet.
            // Keep one session warm; the first real client notification
            // immediately replaces this fallback with the visible set.
            ms.iter()
                .filter(|m| m.active)
                .take(1)
                .map(|m| m.pane.clone())
                .collect::<Vec<_>>()
        } else {
            ms.iter()
                .filter(|m| m.active && visible_sessions.contains(&m.sess))
                .map(|m| m.pane.clone())
                .collect::<Vec<_>>()
        };
        let d = self.daemon.as_mut().unwrap();
        if d.writers.reconcile(visible_panes) {
            // A new empty pane has no copy of the last frame yet.
            self.last_frame.clear();
        }
        d.win_sizes = ms
            .iter()
            .map(|m| (m.win.clone(), (m.win_size, m.panes)))
            .collect();
        d.seen_mirror = true;
        d.size = (w, h);
        true
    }

    /// Preserved-pane shutdown: close visible writers, kill empty panes and
    /// restore layouts through the native pane lifecycle, then drop the key
    /// FIFO and row map.
    /// Drop only what is ours. The panes, the FIFO path and the rows file
    /// belong to the daemon that replaced us — teardown() would delete them.
    fn quiet_exit(&mut self) {
        if let Some(d) = &mut self.daemon {
            d.writers.clear();
        }
        if let Some(d) = &self.daemon {
            unsafe { libc::close(d.keys_fd) };
        }
    }

    fn teardown(&mut self) {
        if let Some(d) = &mut self.daemon {
            d.writers.clear();
        }
        let _ = panes::teardown();
        if let Some(d) = &self.daemon {
            let _ = std::fs::remove_file(&d.keys_path);
            unsafe { libc::close(d.keys_fd) };
        }
        let _ = std::fs::remove_file(&self.rows_file);
    }

    /// Frame and click-row sink: stdout in tty mode; direct writes to visible
    /// empty panes in daemon mode.
    fn emit(&mut self, frame: String, rows: &str, force: bool) {
        let (cols, height) = self
            .daemon
            .as_ref()
            .map(|d| d.size)
            .unwrap_or_else(term_size);
        let cap = if cols == 0 { 0 } else { height.saturating_sub(1) };
        let frame = clip_frame(&frame, cols, cap);
        let rows: String = rows
            .lines()
            .take(cap.saturating_sub(1))
            .map(|r| format!("{r}\n"))
            .collect();
        // Restore the normal foreground after glyph/attribute resets, including
        // inside filled rows. Inherited dark foreground adds no bytes.
        let fg = self.palette.text_fg.fg("");
        let frame = if fg.is_empty() {
            frame
        } else {
            format!(
                "{fg}{}",
                frame.replace(&format!("{E}[0m"), &format!("{E}[0m{fg}"))
            )
        };
        // Rows map must match what the click helper sees under each rendered row.
        let _ = std::fs::write(&self.rows_file, &rows);
        let changed = force || frame != self.last_frame;
        match &mut self.daemon {
            None => {
                if changed {
                    print!("{frame}");
                    let _ = std::io::stdout().flush();
                }
            }
            Some(d) => {
                if changed {
                    d.writers.emit(&frame);
                }
            }
        }
        if changed {
            self.last_frame = frame;
        }
    }

    fn render(&mut self, force: bool) {
        if self.overlay.is_some() {
            self.render_overlay(force);
            return;
        }
        let (cols, trows) = match &self.daemon {
            Some(d) => d.size,
            None => term_size(),
        };
        let cap = trows.saturating_sub(1); // last row's newline would scroll

        let muted = self.palette.muted_fg.fg("2");
        let header_fg = self.palette.header_fg.fg("");
        let header_bg = self.palette.header_bg.bg();
        let accent = self.palette.accent_fg.fg("1");
        // Update notice rides the header. Nonempty contextual/update hints add
        // one row; vis records it so mouse coordinates stay exact.
        let (notice, notice_len, update_hint) = match &self.update {
            Some(t) => {
                let plain = format!(" ↑{}", t.trim_start_matches('v'));
                (
                    format!(" {muted}↑{}{E}[0m", t.trim_start_matches('v')),
                    plain.chars().count(),
                    self.hints(&[(Action::Versions, "update"), (Action::Search, "search")]),
                )
            }
            None => (String::new(), 0, String::new()),
        };
        let filtering = self.state_filter.is_some() || !self.query.trim().is_empty();
        let mut filter = match self.state_filter {
            Some(state) => format!(" [{}]", state.label()),
            None if self.search_focused || !self.query.is_empty() => {
                let query: String = self.query.chars().filter(|c| !c.is_control()).collect();
                format!(" /{query}")
            }
            None => String::new(),
        };
        if filtering {
            filter.push_str(&format!(" {}/{}", self.visible.len(), self.rows.len()));
        }
        let filter: String = filter
            .chars()
            .take(cols.saturating_sub(notice_len))
            .collect();
        let filter_len = filter.chars().count();
        let title: String = app_title()
            .chars()
            .take(cols.saturating_sub(filter_len + notice_len))
            .collect();
        let title_len = title.chars().count();
        let nav = self.nav_label(true, false);
        let hint = if self.search_focused {
            join(&[
                self.hint(&self.search_keys, Action::Accept, "nav"),
                self.hint(&self.search_keys, Action::Clear, "clear"),
                self.hint(&self.search_keys, Action::Cancel, "clear"),
            ])
        } else if self.state_filter.is_some() {
            join(&[
                self.hint(&self.normal_keys, Action::Filter, "status"),
                nav,
                self.hint(&self.normal_keys, Action::Reset, "clear"),
            ])
        } else if !self.query.trim().is_empty() {
            join(&[
                nav,
                self.hint(&self.normal_keys, Action::Jump, "open"),
                self.hint(&self.normal_keys, Action::Reset, "clear"),
            ])
        } else {
            update_hint
        };
        let hint: String = hint.chars().take(cols).collect();
        let has_hint = !hint.is_empty();
        let space = cap.saturating_sub(1 + usize::from(has_hint));
        let (hdr, hdr_pad) = if self.plugin_selected {
            let used = title_len + filter.chars().count() + notice_len;
            (header_bg.as_str(), " ".repeat(cols.saturating_sub(used)))
        } else {
            ("", String::new())
        };
        // Preserve historical header bytes regardless of unrelated role overrides.
        // Only non-default header styles need restoration after the notice reset.
        let default_header = Palette::default();
        let notice = if self.palette.header_fg == default_header.header_fg
            && self.palette.header_bg == default_header.header_bg
        {
            notice
        } else {
            notice.replace(&format!("{E}[0m"), &format!("{E}[0m{hdr}{header_fg}"))
        };
        let mut frame = format!(
            "{E}[H{hdr}{header_fg}{E}[1m{title}{E}[22m{muted}{filter}{E}[22m{notice}{hdr_pad}{E}[0m{E}[K\n"
        );
        let mut vis = String::new();
        if has_hint {
            frame.push_str(&format!("{muted}{hint}{E}[0m{E}[K\n"));
            vis.push_str("-\n");
        }
        let cursor = cursor_row(
            &self.rows,
            &self.visible,
            self.sel,
            self.plugin_selected,
            &self.active,
        );
        if self.rows.is_empty() {
            frame.push_str(&format!("{muted}no agents{E}[0m{E}[K\n"));
        } else if self.visible.is_empty() {
            let reset = self.normal_keys[&Action::Reset]
                .first()
                .map(|c| format!(" · {} shows all", c.label(false)))
                .unwrap_or_default();
            frame.push_str(&format!("{muted}no matches{reset}{E}[0m{E}[K\n"));
        } else {
            // build filtered agents plus their session context, then window it
            let mut lines: Vec<(String, &str, usize, bool)> = Vec::new();
            let (mut sel_top, mut sel_bot) = (0usize, 0usize);
            let mut session = "";
            for (n, &row_i) in self.visible.iter().enumerate() {
                let r = &self.rows[row_i];
                let sess = r.loc.split(':').next().unwrap_or("");
                if sess != session {
                    session = sess;
                    // clip to pane width — a wrapped header shifts every row
                    // below it and breaks the click→rows-file mapping
                    let sess_clipped: String = sess.chars().take(cols).collect();
                    lines.push((
                        format!("{accent}{sess_clipped}{E}[0m{E}[K\n"),
                        "-",
                        0,
                        false,
                    ));
                }
                if Some(n) == cursor {
                    sel_top = lines.len();
                }
                let selected = Some(n) == cursor;
                let mark = cursor_mark(&self.palette, selected, self.plugin_selected, &r.state);
                let dot = self.dot(&r.state);
                let win = r.loc.split_once(':').map(|x| x.1).unwrap_or("");
                let mut rest = format!("{win} {}", r.cwd);
                let agent_len = r.agent.chars().count();
                let avail = cols.saturating_sub(6 + agent_len);
                if avail > 0 {
                    rest = rest.chars().take(avail).collect();
                }
                let row_bg = if selected {
                    self.palette.state_bg(&r.state, self.plugin_selected)
                } else {
                    String::new()
                };
                let row = format!(" {mark}{dot} {E}[1m{}{E}[0m {muted}{rest}{E}[0m", r.agent);
                let width = 6 + agent_len + rest.chars().count();
                lines.push((
                    format!("{}{E}[K\n", bar(&row, &row_bg, cols, width)),
                    &r.pane,
                    n + 1,
                    selected,
                ));
                if !r.title.is_empty() {
                    let t: String = r.title.chars().take(cols.saturating_sub(5)).collect();
                    let line = format!("     {muted}{t}{E}[0m");
                    let width = 5 + t.chars().count();
                    lines.push((
                        format!("{}{E}[K\n", bar(&line, &row_bg, cols, width)),
                        &r.pane,
                        n + 1,
                        selected,
                    ));
                }
                if Some(n) == cursor {
                    sel_bot = lines.len() - 1;
                }
            }
            // cursor's session header gives context — drag it into view
            if self.follow_selection && cursor.is_some() {
                if sel_top > 0 && lines[sel_top - 1].1 == "-" {
                    sel_top -= 1;
                }
                if space > 0 {
                    if sel_bot + 1 > self.scroll + space {
                        self.scroll = sel_bot + 1 - space;
                    }
                    if sel_top < self.scroll {
                        self.scroll = sel_top; // top wins when row + title exceed space
                    }
                }
            }
            self.follow_selection = false;
            if space > 0 {
                self.scroll = self.scroll.min(lines.len().saturating_sub(space));
            } else {
                self.scroll = 0;
            }
            let end = (self.scroll + space).min(lines.len());
            let overflow = lines.len() > space && space > 0 && cols > 0;
            let thumb_len = if overflow {
                space
                    .saturating_mul(space)
                    .checked_div(lines.len())
                    .unwrap_or(1)
                    .max(1)
            } else {
                0
            };
            let thumb_start = if overflow {
                self.scroll.saturating_mul(space - thumb_len) / lines.len().saturating_sub(space)
            } else {
                0
            };
            for (row, (text, pane, index, selected)) in lines[self.scroll..end].iter().enumerate() {
                if overflow {
                    frame.push_str(text.trim_end_matches('\n'));
                    let glyph = if (thumb_start..thumb_start + thumb_len).contains(&row) {
                        '▐'
                    } else {
                        '│'
                    };
                    frame.push_str(&format!("{E}[{cols}G{E}[2m{glyph}{E}[0m\n"));
                } else {
                    frame.push_str(text);
                }
                if *pane == "-" {
                    vis.push_str("-\n");
                } else {
                    vis.push_str(&format!("{pane}\t{index}\t{}\n", usize::from(*selected)));
                }
            }
        }
        frame.push_str(&format!("{E}[J"));
        self.emit(frame, &vis, force);
    }

    /// Version picker: update or roll back to any release the last check saw.
    /// Selecting one switches the source, the engine, and restarts the view.
    fn versions(&mut self) {
        // opening the picker is an explicit "what is out there?" — ask now
        // instead of serving a list that the daily check may have left a day
        // old. It lands in the file and normal scan renders pick it up live.
        let plugin_dir = self.plugin_dir.clone();
        std::thread::spawn(move || {
            release::refresh(&plugin_dir);
        });
        self.overlay = Some(Overlay::Versions {
            sel: 0,
            chosen: None,
        });
        self.last_frame.clear();
    }

    fn render_overlay(&mut self, force: bool) {
        let title = app_title();
        let header = self.palette.header_fg.fg("1");
        let muted = self.palette.muted_fg.fg("2");
        let idle = self.palette.idle_fg.fg("");
        let working = self.palette.working_fg.fg("");
        let blocked = self.palette.blocked_fg.fg("");
        let done = self.palette.done_fg.fg("");
        let error = self.palette.error_fg.fg("2");
        let text = match &mut self.overlay {
            Some(Overlay::Help) => {
                let accept = self.search_keys[&Action::Accept]
                    .first()
                    .map_or(String::new(), |c| format!("; {} enables {}", c.label(false), self.nav_label(false, false)));
                let mut keys = vec![(self.nav_label(false, true), "move selection".to_string())];
                for (action, what) in [
                    (Action::Jump, "jump to agent".to_string()),
                    (Action::Search, format!("live search{accept}")),
                    (Action::Filter, "select next state filter".into()),
                    (Action::Reset, "clear filters / show all".into()),
                    (Action::Versions, "update / switch version".into()),
                    (Action::Close, "close sidebar".into()),
                    (Action::Help, "this help".into()),
                ] {
                    keys.push((self.labels(&self.normal_keys, action, false), what));
                }
                let keys: String = keys
                    .iter()
                    .filter(|(label, _)| !label.is_empty())
                    .map(|(label, what)| format!("{label:<8} {what}\n"))
                    .collect();
                format!(
                    "{E}[2J{E}[H{header}{title} — help{E}[0m\n\n\
{E}[1mstatus{E}[0m\n\
 {idle}⣿{E}[0m  idle\n\
 {working}⠹{E}[0m  working (spinner)\n\
 {blocked}⣿{E}[0m  blocked, waiting for input (blinks)\n\
 {done}⣿{E}[0m  done, not viewed yet (blinks)\n\n\
{E}[1mkeys{E}[0m\n{keys}\n\
{muted}press any key to return{E}[0m"
                )
            }
            Some(Overlay::Versions { sel, chosen }) => {
                let cur = current_tag();
                let tags = known_tags(&self.plugin_dir);
                *sel = picker_sel(&tags, &cur, chosen.as_deref(), *sel);
                let mut text = format!("{E}[2J{E}[H{header}{title} — versions{E}[0m\n\n");
                if tags.is_empty() {
                    text.push_str(&format!(
                        " {error}no releases found — checking…{E}[0m\n\n\
                         {muted}{}{E}[0m",
                        self.hint(&self.normal_keys, Action::Close, "back")
                    ));
                } else {
                    for (i, t) in tags.iter().enumerate() {
                        let mark = cursor_mark(&self.palette, i == *sel, true, "idle");
                        let tail = if *t == cur {
                            format!(" {muted}(current){E}[0m")
                        } else {
                            String::new()
                        };
                        text.push_str(&format!("{mark}{t}{tail}\n"));
                    }
                    let hint = join(&[
                        self.hint(&self.normal_keys, Action::Jump, "switch"),
                        self.nav_label(true, true),
                        self.hint(&self.normal_keys, Action::Close, "back"),
                    ]);
                    text.push_str(&format!("\n{muted}{hint}{E}[0m"));
                }
                text
            }
            None => return,
        };
        let text = if self.palette.header_bg == Palette::default().header_bg {
            text
        } else {
            let (header, body) = text.split_once('\n').unwrap_or((&text, ""));
            // Clear with the canvas background, not the header fill.
            let header = header
                .strip_prefix(&format!("{E}[2J{E}[H"))
                .unwrap_or(header);
            let cols = self
                .daemon
                .as_ref()
                .map(|d| d.size.0)
                .unwrap_or_else(|| term_size().0);
            let width = title.chars().count()
                + if matches!(self.overlay, Some(Overlay::Help)) {
                    7
                } else {
                    11
                };
            format!(
                "{E}[2J{E}[H{}\n{body}",
                bar(header, &self.palette.header_bg.bg(), cols, width)
            )
        };
        self.emit(text, "", force);
    }

    fn overlay_key(&mut self, key: Key) {
        if matches!(self.overlay, Some(Overlay::Help)) {
            self.close_overlay();
            return;
        }
        let tags = known_tags(&self.plugin_dir);
        let cur = current_tag();
        let mut switch = None;
        let mut close = false;
        if let Some(Overlay::Versions { sel, chosen }) = &mut self.overlay {
            *sel = picker_sel(&tags, &cur, chosen.as_deref(), *sel);
            match key {
                Key::Down if !tags.is_empty() => *sel = (*sel + 1).min(tags.len() - 1),
                Key::Up => *sel = sel.saturating_sub(1),
                Key::Jump => {
                    switch = tags.get(*sel).filter(|t| **t != cur).cloned();
                    close = true;
                }
                Key::Quit | Key::Close => close = true,
                _ => {}
            }
            *chosen = tags.get(*sel).cloned();
        }
        if let Some(tag) = switch {
            self.switch_version(&tag);
            self.close_overlay();
        } else if close {
            self.close_overlay();
        }
    }

    fn close_overlay(&mut self) {
        self.overlay = None;
        if self.daemon.is_none() {
            print!("{E}[2J");
        }
        self.last_frame.clear();
        self.reclaim_key_table();
    }

    fn reclaim_key_table(&mut self) {
        if self.daemon.is_none() {
            return;
        }
        let clients = self
            .tmux
            .run("list-clients -F '#{client_name}\t#{pane_title}'")
            .unwrap_or_default();
        for line in clients.lines() {
            let Some((client, title)) = line.split_once('\t') else {
                continue;
            };
            if title == "agenmux" {
                let _ = command_spawn(&["switch-client", "-c", client, "-T", "agenmux"]);
            }
        }
    }

    /// nohup + no wait: update kills the panes this engine renders into, and a
    /// pane kill would otherwise SIGHUP the switch halfway through.
    fn switch_version(&mut self, tag: &str) {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let _ = std::process::Command::new("nohup")
            .arg(exe)
            .arg("update")
            .arg(tag)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    fn help(&mut self) {
        self.overlay = Some(Overlay::Help);
        self.last_frame.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Run the real renderer with a control connection isolated from every user
    // server. The child process owns TMUX, avoiding parallel-test env races.
    #[test]
    fn semantic_renderer_frames() {
        use std::process::Command;
        if std::env::var_os("AGENMUX_THEME_TEST_CHILD").is_none() {
            let socket =
                std::env::temp_dir().join(format!("agenmux-theme-{}.sock", std::process::id()));
            let socket = socket.to_str().unwrap();
            assert!(Command::new("tmux")
                .args([
                    "-S",
                    socket,
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    "theme",
                    "/bin/bash",
                    "-c",
                    "sleep 120"
                ])
                .status()
                .unwrap()
                .success());
            let result = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sidebar::tests::semantic_renderer_frames",
                    "--nocapture",
                ])
                .env("AGENMUX_THEME_TEST_CHILD", "1")
                .env("TMUX", format!("{socket},0,0"))
                .output()
                .unwrap();
            let _ = Command::new("tmux")
                .args(["-S", socket, "kill-server"])
                .status();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let dir = std::env::temp_dir().join(format!("agenmux-theme-frames-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("target/release")).unwrap();
        std::fs::write(dir.join("target/release/.agenmux-tags"), "v9.9.9\nv0.0.1\n").unwrap();
        let settings =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        let mut sb = new_sidebar(
            Tmux::connect().unwrap_or_else(|e| panic!("{e}")),
            dir.clone(),
            dir.join("cache"),
            dir.join("rows"),
            String::new(),
            settings,
        );
        sb.daemon = Some(Daemon {
            keys_path: dir.join("keys"),
            keys_fd: -1,
            writers: PaneWriters::new(),
            size: (80, 40),
            seen_mirror: false,
            empty_ticks: 0,
            client: String::new(),
            started: Instant::now(),
            win_sizes: HashMap::new(),
            attached: String::new(),
        });
        sb.rows = ["blocked", "working", "idle", "done"]
            .iter()
            .enumerate()
            .map(|(i, state)| {
                let mut r = row(&format!("%{}", i + 1));
                r.state = (*state).into();
                r.title = "Synthetic task".into();
                r
            })
            .collect();
        sb.visible = (0..4).collect();
        let mut frames = String::new();
        for focused in [false, true] {
            sb.plugin_selected = focused;
            for selected in 1..=4 {
                sb.sel = selected;
                sb.active = format!("%{selected}");
                for tick in [0, 2, 7] {
                    sb.tick = tick;
                    sb.render(true);
                    frames.push_str(&format!(
                        "focus={focused} selected={selected} tick={tick}\n{}\nrows={}\n",
                        sb.last_frame
                            .replace(&app_title(), "agenmux TEST")
                            .escape_default(),
                        std::fs::read_to_string(&sb.rows_file)
                            .unwrap()
                            .escape_default()
                    ));
                }
            }
        }
        for mode in 0..7 {
            sb.query.clear();
            sb.state_filter = None;
            sb.search_focused = false;
            sb.overlay = None;
            sb.update = None;
            match mode {
                0 => {
                    sb.query = "repo".into();
                    sb.search_focused = true;
                }
                1 => sb.state_filter = Some(StateFilter::Working),
                2 => sb.update = Some("v9.9.9".into()),
                3 => sb.overlay = Some(Overlay::Help),
                4 => {
                    sb.overlay = Some(Overlay::Versions {
                        sel: 0,
                        chosen: None,
                    })
                }
                5 => sb.query = "absent".into(),
                _ => sb.rows.clear(),
            }
            sb.rebuild_visible(false);
            sb.render(true);
            frames.push_str(&format!(
                "mode={mode}\n{}\nrows={}\n",
                sb.last_frame
                    .replace(&app_title(), "agenmux TEST")
                    .escape_default(),
                std::fs::read_to_string(&sb.rows_file)
                    .unwrap()
                    .escape_default()
            ));
        }
        if std::env::var_os("AGENMUX_UPDATE_FIXTURES").is_some() {
            std::fs::write("tests/fixtures/sidebar/dark.frames", &frames).unwrap();
        }
        assert_eq!(
            frames,
            std::fs::read_to_string("tests/fixtures/sidebar/dark.frames").unwrap()
        );
        themed_frames(&mut sb);
        custom_key_hints(&mut sb);
        if let Some(output) = std::env::var_os("AGENMUX_THEME_VISUAL_DIR") {
            visual_frames(&mut sb, &PathBuf::from(output));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Optional, synthetic-only private tmux screen inspection. The production
    // binary's activation/snapshot path is separately exercised in plugin.rs.
    fn visual_frames(sb: &mut Sidebar, output: &std::path::Path) {
        use std::process::Command;
        std::fs::create_dir_all(output).unwrap();
        for base in ["light", "terminal"] {
            let file = crate::app_config::parse(&format!("[theme]\nbase='{base}'")).unwrap();
            sb.palette = Palette::resolve(file.theme.as_ref().unwrap());
            sb.rows = ["blocked", "working", "idle", "done"]
                .iter()
                .enumerate()
                .map(|(i, state)| {
                    let mut r = row(&format!("%{}", i + 1));
                    r.state = (*state).into();
                    r.title = "Synthetic task".into();
                    r
                })
                .collect();
            sb.visible = (0..4).collect();
            sb.sel = 2;
            sb.active = "%2".into();
            sb.plugin_selected = true;
            sb.tick = 0;
            sb.query.clear();
            sb.state_filter = None;
            sb.search_focused = false;
            sb.update = Some("v9.9.9".into());
            sb.overlay = None;
            sb.daemon.as_mut().unwrap().size = (80, 40);
            sb.render(true);
            let result = Command::new("tmux")
                .args([
                    "new-window",
                    "-d",
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-n",
                    "theme-preview",
                    "/bin/bash",
                    "-c",
                    "printf '%s' \"$1\"; exec sleep 30",
                    "_",
                    &sb.last_frame,
                ])
                .output()
                .unwrap();
            assert!(result.status.success());
            let pane = String::from_utf8(result.stdout).unwrap();
            let pane = pane.trim();
            let mut captured = String::new();
            for _ in 0..50 {
                let result = Command::new("tmux")
                    .args(["capture-pane", "-p", "-e", "-t", pane])
                    .output()
                    .unwrap();
                assert!(result.status.success());
                captured = String::from_utf8(result.stdout).unwrap();
                if captured.contains("Synthetic task") {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(captured.contains("Synthetic task"));
            if base == "light" {
                assert!(captured.contains("38;2;32;32;32"));
                assert!(captured.contains("48;2;255;240;204"));
            } else {
                assert!(!captured.contains("48;2;") && !captured.contains("48;5;"));
            }
            // Normalize the build timestamp before retaining scratch evidence.
            std::fs::write(
                output.join(format!("{base}.capture")),
                captured.replace(&app_title(), "agenmux TEST"),
            )
            .unwrap();
            assert!(Command::new("tmux")
                .args(["kill-pane", "-t", pane])
                .status()
                .unwrap()
                .success());
        }
    }

    /// Split mode installs the user's chords into the tmux tables, so its help
    /// and hints must name those. The daemon decodes its FIFO with the protocol
    /// defaults, which must not leak back into what the sidebar advertises.
    fn custom_key_hints(sb: &mut Sidebar) {
        assert!(sb.daemon.is_some(), "this covers the daemon render path");
        let file = crate::app_config::parse(
            "[keys.normal]\ndown = ['n']\nup = ['e']\njump = ['Tab']\nclose = []\n",
        )
        .unwrap();
        let settings = crate::app_config::resolve(&file, &Default::default()).unwrap();
        sb.normal_keys = settings.normal.clone();
        sb.search_keys = settings.search.clone();
        sb.rows = vec![row("%1")];
        sb.rows[0].state = "working".into();
        sb.visible = vec![0];
        sb.sel = 1;
        sb.query.clear();
        sb.search_focused = false;

        sb.overlay = Some(Overlay::Help);
        sb.render(true);
        let help = sb.last_frame.clone();
        assert!(help.contains("n/e"), "{help}");
        assert!(help.contains("Tab"), "{help}");
        assert!(!help.contains("j/k"), "{help}");
        // An unbound action leaves no row behind rather than a stale default.
        assert!(!help.contains("close sidebar"), "{help}");

        sb.overlay = None;
        sb.state_filter = Some(StateFilter::Working);
        sb.rebuild_visible(false);
        sb.render(true);
        let footer = sb.last_frame.clone();
        assert!(footer.contains("n/e"), "{footer}");
        assert!(!footer.contains("j/k"), "{footer}");
        sb.state_filter = None;
    }

    fn themed_frames(sb: &mut Sidebar) {
        let tags = format!("v9.9.9\n{}\n", current_tag());
        std::fs::write(sb.plugin_dir.join("target/release/.agenmux-tags"), &tags).unwrap();
        let ansi = regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap();
        let plain = |frame: &str| {
            ansi.replace_all(frame, "")
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let sources = [
            "[theme]\nbase = 'light'",
            "[theme]\nbase = 'terminal'",
            "[theme.colors]\nworking_fg = '#123456'\nworking_bg = 123\nworking_bg_unfocused = 'default'",
            "[theme.colors]\nworking_bg = 123",
            "[theme.colors]\nheader_fg = 17",
            "[theme.colors]\nheader_bg = 123",
            "[theme]\nbase = 'light'\n[theme.colors]\nheader_fg = 17\nheader_bg = 'default'\ntext_fg = '#123456'\nmuted_fg = 99\naccent_fg = 100\nerror_fg = 101\nblocked_fg = 102\nblocked_bg = 103\nblocked_bg_unfocused = 104\nworking_fg = 105\nworking_bg = 106\nworking_bg_unfocused = 107\nidle_fg = 108\nidle_bg = 109\nidle_bg_unfocused = 110\ndone_fg = 111\ndone_bg = 112\ndone_bg_unfocused = 113",
        ];
        for source in sources {
            let file = crate::app_config::parse(source).unwrap();
            let p = Palette::resolve(file.theme.as_ref().unwrap());
            for focused in [false, true] {
                for state in ["blocked", "working", "idle", "done"] {
                    for tick in [0, 2, 7] {
                        sb.rows = vec![row("%1")];
                        sb.rows[0].state = state.into();
                        sb.rows[0].title = "Synthetic task".into();
                        sb.visible = vec![0];
                        sb.sel = 1;
                        sb.active = "%1".into();
                        sb.plugin_selected = focused;
                        sb.tick = tick;
                        for mode in 0..10 {
                            sb.query.clear();
                            sb.state_filter = None;
                            sb.search_focused = false;
                            sb.overlay = None;
                            sb.update = None;
                            match mode {
                                1 => {
                                    sb.query = "repo".into();
                                    sb.search_focused = true;
                                }
                                2 => sb.query = "repo".into(),
                                3 => sb.state_filter = Some(match state {
                                    "blocked" => StateFilter::Blocked,
                                    "working" => StateFilter::Working,
                                    "done" => StateFilter::Done,
                                    _ => StateFilter::Idle,
                                }),
                                4 => sb.update = Some("v9.9.9".into()),
                                5 => sb.overlay = Some(Overlay::Help),
                                6 => {
                                    sb.overlay = Some(Overlay::Versions {
                                        sel: 0,
                                        chosen: None,
                                    })
                                }
                                7 => sb.query = "absent".into(),
                                8 => sb.rows.clear(),
                                9 => {
                                    sb.overlay = Some(Overlay::Versions {
                                        sel: 0,
                                        chosen: None,
                                    });
                                    std::fs::remove_file(
                                        sb.plugin_dir.join("target/release/.agenmux-tags"),
                                    )
                                    .unwrap();
                                }
                                _ => {}
                            }
                            sb.rebuild_visible(false);
                            sb.palette = Palette::default();
                            sb.render(true);
                            let old_frame = sb.last_frame.clone();
                            let old_text = plain(&old_frame);
                            let old_map = std::fs::read_to_string(&sb.rows_file).unwrap();
                            sb.palette = p.clone();
                            sb.render(true);
                            assert_eq!(
                                plain(&sb.last_frame),
                                old_text,
                                "{source} {state} mode={mode}"
                            );
                            assert_eq!(std::fs::read_to_string(&sb.rows_file).unwrap(), old_map);
                            if mode == 0 {
                                if source == sources[2] && state != "working" {
                                    assert_eq!(sb.last_frame, old_frame, "working overrides leave other rows byte-identical");
                                }
                                assert!(sb.last_frame.contains(&p.state_bg(state, focused)));
                                assert!(sb.last_frame.contains(
                                    &p.state_fg(state).fg(if focused { "1" } else { "" })
                                ));
                                assert!(sb.last_frame.contains(&p.accent_fg.fg("1")));
                                let restore = format!(
                                    "{E}[0m{}{}",
                                    p.text_fg.fg(""),
                                    p.state_bg(state, focused)
                                );
                                assert!(
                                    sb.last_frame.contains(&restore),
                                    "row restores foreground and fill"
                                );
                            }
                            if [1, 2, 3, 4, 5, 6, 7, 8, 9].contains(&mode) {
                                assert!(sb.last_frame.contains(&p.muted_fg.fg("2")));
                            }
                            if mode == 5 || mode == 6 {
                                assert!(sb.last_frame.contains(&p.header_fg.fg("1")));
                                if p.header_bg != Palette::default().header_bg {
                                    assert!(sb.last_frame.starts_with(&format!("{}{E}[2J{E}[H{}", p.text_fg.fg(""), p.header_bg.bg())));
                                }
                            }
                            if mode == 6 { assert!(sb.last_frame.contains("(current)")); }
                            if mode == 9 {
                                assert!(sb.last_frame.contains(&p.error_fg.fg("2")));
                                std::fs::write(
                                    sb.plugin_dir.join("target/release/.agenmux-tags"),
                                    &tags,
                                )
                                .unwrap();
                            }
                            if mode == 4
                                && (source == sources[2] || source == sources[3])
                            {
                                assert_eq!(
                                    sb.last_frame.lines().next(),
                                    old_frame.lines().next(),
                                    "working-only overrides leave update-header bytes unchanged: {source} focused={focused}"
                                );
                            }
                            if mode == 4
                                && (p.header_fg != Palette::default().header_fg
                                    || p.header_bg != Palette::default().header_bg)
                            {
                                let hdr = if focused {
                                    p.header_bg.bg()
                                } else {
                                    String::new()
                                };
                                assert!(sb.last_frame.lines().next().unwrap().contains(&format!(
                                    "{E}[0m{}{hdr}{}",
                                    p.text_fg.fg(""),
                                    p.header_fg.fg("")
                                )));
                            }
                            // Same engine/output bytes for tty popup and daemon at
                            // matching dimensions. Only the sink and row file differ.
                            {
                                let d = sb.daemon.take().unwrap();
                                let size = term_size();
                                sb.render(true);
                                let popup = sb.last_frame.clone();
                                sb.daemon = Some(d);
                                sb.daemon.as_mut().unwrap().size = size;
                                sb.render(true);
                                assert_eq!(sb.last_frame, popup);
                                sb.daemon.as_mut().unwrap().size = (80, 40);
                            }
                            for size in [(0, 0), (0, 4), (1, 1), (1, 3), (5, 6), (12, 4)] {
                                sb.daemon.as_mut().unwrap().size = size;
                                sb.render(true);
                                let text = plain(&sb.last_frame);
                                assert!(text.lines().all(|l| l.chars().count() <= size.0));
                                assert!(text.lines().count() <= size.1.saturating_sub(1));
                                assert!(
                                    std::fs::read_to_string(&sb.rows_file)
                                        .unwrap()
                                        .lines()
                                        .count()
                                        <= size.1.saturating_sub(2)
                                );
                            }
                            sb.daemon.as_mut().unwrap().size = (80, 40);
                        }
                    }
                }
            }
        }
    }

    fn row(pane: &str) -> PaneRow {
        PaneRow {
            pane: pane.into(),
            loc: "s:1.1".into(),
            agent: "pi".into(),
            state: "idle".into(),
            cwd: "repo".into(),
            title: String::new(),
        }
    }

    fn m(sess: &str, h: usize, active: bool) -> M {
        M {
            pane: "%1".into(),
            win: "@1".into(),
            sess: sess.into(),
            w: 30,
            h,
            win_size: (200, h),
            panes: 2,
            active,
        }
    }

    #[test]
    fn app_title_identifies_dev_and_release_builds() {
        let expected = if cfg!(debug_assertions) {
            let timestamp = option_env!("AGENMUX_BUILD_TIMESTAMP").expect("debug timestamp");
            assert!(regex::Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$")
                .unwrap()
                .is_match(timestamp));
            format!("agenmux dev ({timestamp})")
        } else {
            format!("agenmux v{}", env!("CARGO_PKG_VERSION"))
        };
        assert_eq!(app_title(), expected);
    }

    #[test]
    fn one_garbage_measurement_does_not_kill_the_daemon() {
        let s = Duration::from_secs;
        // the regression: a hook block desyncs the pipe for one command, the
        // mirror list reads back empty, and every mirror pane got torn down
        assert!(!suicide(true, s(60), 1));
        assert!(suicide(true, s(60), 2)); // user really did close them all
                                          // startup grace: no mirror has ever appeared yet
        assert!(!suicide(false, s(5), 9));
        assert!(suicide(false, s(30), 2)); // none ever came — give up
    }

    #[test]
    fn a_replaced_daemon_stops_owning_the_panes() {
        assert!(!superseded("client-7", "client-7"));
        assert!(!superseded("client-7", "client-7\n")); // show-option keeps the newline
        assert!(superseded("client-7", "client-9"));
        assert!(!superseded("client-7", "")); // unset is not a claim
        assert!(!superseded("", "client-9")); // we never published a name
        assert!(!superseded("", ""));
    }

    #[test]
    fn cursor_uses_state_hue_and_focus_bold() {
        assert_eq!(
            cursor_mark(&Palette::default(), true, true, "idle"),
            format!("{E}[1;32m❯{E}[0m ")
        );
        assert_eq!(
            cursor_mark(&Palette::default(), true, false, "idle"),
            format!("{E}[32m❯{E}[0m ")
        );
        assert_eq!(
            cursor_mark(&Palette::default(), true, true, "working"),
            format!("{E}[1;33m❯{E}[0m ")
        );
        assert_eq!(
            cursor_mark(&Palette::default(), false, true, "blocked"),
            "  "
        );
    }

    #[test]
    fn cursor_follows_focus_outside_navigation() {
        let rows = [row("%1"), row("%2")];
        let visible = [0, 1];
        assert_eq!(cursor_row(&rows, &visible, 1, false, "%2"), Some(1));
        assert_eq!(cursor_row(&rows, &visible, 2, false, "%9"), None);
        assert_eq!(cursor_row(&rows, &visible, 2, true, "%1"), Some(1));
        assert_eq!(cursor_row(&rows, &[1], 1, false, "%2"), Some(0));
    }

    #[test]
    fn arrows_work_in_both_cursor_key_modes() {
        // the regression: only CSI was decoded, so arrows did nothing in panes
        // tmux had put in application-cursor mode (it sends SS3 there)
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let feed = |b: &[u8]| unsafe { libc::write(fds[1], b.as_ptr().cast(), b.len()) };

        let keys = default_keys(crate::app_config::KeyMode::Normal);
        for seq in [b"\x1b[A".as_slice(), b"\x1bOA".as_slice()] {
            feed(seq);
            assert!(matches!(read_key(fds[0], &keys), Key::Up), "up: {seq:?}");
        }
        for seq in [b"\x1b[B".as_slice(), b"\x1bOB".as_slice()] {
            feed(seq);
            assert!(matches!(read_key(fds[0], &keys), Key::Down), "down: {seq:?}");
        }
        feed(b"j");
        assert!(matches!(read_key(fds[0], &keys), Key::Down));
        feed(b"g");
        assert!(matches!(read_key(fds[0], &keys), Key::Sequence('g')));
        feed(&[0x06, b'g']);
        assert!(matches!(read_key(fds[0], &keys), Key::Sequence('g')));
        feed(b"G");
        assert!(matches!(read_key(fds[0], &keys), Key::Last));
        feed(&[0x01]);
        assert!(matches!(read_key(fds[0], &keys), Key::WheelUp));
        feed(&[0x02]);
        assert!(matches!(read_key(fds[0], &keys), Key::WheelDown));
        feed(b"u");
        assert!(matches!(read_key(fds[0], &keys), Key::Versions));
        feed(&[0, b'q']);
        assert!(matches!(read_key(fds[0], &keys), Key::Text(s) if s == "q"));
        feed(&[0x03]);
        assert!(matches!(read_key(fds[0], &keys), Key::Quit));

        // Remapped popup input: the user's chords decide, defaults are gone,
        // and search text/emergency keys stay outside the keymap.
        let file = crate::app_config::parse(
            "[keys.normal]\ndown = ['n', 'PageDown']\nup = ['p']\nclose = []\n[keys.search]\ncancel = ['C-g']\n",
        )
        .unwrap();
        let custom = crate::app_config::resolve(&file, &Default::default()).unwrap();
        feed(b"n");
        assert!(matches!(read_key(fds[0], &custom.normal), Key::Down));
        feed(b"\x1b[6~");
        assert!(matches!(read_key(fds[0], &custom.normal), Key::Down));
        feed(b"j");
        assert!(matches!(read_key(fds[0], &custom.normal), Key::Other));
        feed(b"q");
        assert!(matches!(read_key(fds[0], &custom.normal), Key::Other));
        feed(&[0x04]);
        assert!(matches!(read_key(fds[0], &custom.normal), Key::Quit));
        feed(&[0x07]);
        assert!(matches!(read_search_key(fds[0], &custom.search), Key::AllStates));
        feed(&[0x03]);
        assert!(matches!(read_search_key(fds[0], &custom.search), Key::Quit));
        feed("é".as_bytes());
        assert!(matches!(read_search_key(fds[0], &custom.search), Key::Text(s) if s == "é"));
        feed(b"n");
        assert!(matches!(read_search_key(fds[0], &custom.search), Key::Text(s) if s == "n"));
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn timed_key_sequences_support_arbitrary_bindings_and_expiry() {
        let bindings = [("gg", Key::First), ("gd", Key::Last)];
        let start = Instant::now();
        let mut sequence = KeySequence::default();

        assert!(matches!(
            sequence.push('g', start, &bindings),
            SequenceResult::Pending
        ));
        assert!(matches!(
            sequence.push('d', start + Duration::from_millis(10), &bindings),
            SequenceResult::Match(Key::Last)
        ));
        assert!(matches!(
            sequence.push('g', start + Duration::from_millis(20), &bindings),
            SequenceResult::Pending
        ));
        assert!(matches!(
            sequence.push('x', start + Duration::from_millis(30), &bindings),
            SequenceResult::Miss
        ));
        assert!(matches!(
            sequence.push('g', start + Duration::from_millis(40), &bindings),
            SequenceResult::Pending
        ));
        assert!(matches!(
            sequence.push(
                'g',
                start + KEY_SEQUENCE_TIMEOUT + Duration::from_millis(41),
                &bindings
            ),
            SequenceResult::Pending
        ));
        assert!(matches!(
            sequence.push(
                'g',
                start + KEY_SEQUENCE_TIMEOUT + Duration::from_millis(50),
                &bindings
            ),
            SequenceResult::Match(Key::First)
        ));
    }

    /// Feed escape_key a scripted tail. Every byte goes through the same
    /// reader, which is the point: mirror mode delivers keys over a
    /// non-blocking FIFO one byte at a time, so a tail byte that has not
    /// arrived yet must be waited for, not read blind. Reading the pair
    /// without polling hit EAGAIN and dropped every other arrow.
    fn decode(tail: &[Option<u8>]) -> Option<KeyChord> {
        let mut it = tail.iter().copied();
        escape_chord(move || it.next().flatten())
    }
    fn default_keys(mode: crate::app_config::KeyMode) -> Keymap {
        crate::app_config::resolved_keys(mode, None).unwrap()
    }

    #[test]
    fn escape_tails_decode_in_both_cursor_key_modes() {
        assert_eq!(decode(&[Some(b'['), Some(b'A')]), Some(KeyChord::Up));
        assert_eq!(decode(&[Some(b'['), Some(b'B')]), Some(KeyChord::Down));
        assert_eq!(decode(&[Some(b'O'), Some(b'A')]), Some(KeyChord::Up)); // SS3
        assert_eq!(decode(&[Some(b'O'), Some(b'B')]), Some(KeyChord::Down));
        assert_eq!(decode(&[Some(b'['), Some(b'H')]), Some(KeyChord::Home));
        assert_eq!(decode(&[Some(b'['), Some(b'1'), Some(b'~')]), Some(KeyChord::Home));
        assert_eq!(decode(&[Some(b'['), Some(b'5'), Some(b'~')]), Some(KeyChord::PageUp));
        assert_eq!(decode(&[]), Some(KeyChord::Escape)); // bare Esc
        assert_eq!(decode(&[None]), Some(KeyChord::Escape));
        // a tail that never completes is not a close — Esc already decided that
        assert_eq!(decode(&[Some(b'['), None]), None);
        assert_eq!(decode(&[Some(b'['), Some(b'Z')]), None);
        assert_eq!(decode(&[Some(b'['), Some(b'5'), None]), None);
        // bare Esc is the normal reset and the search cancel by default
        let normal = default_keys(crate::app_config::KeyMode::Normal);
        let search = default_keys(crate::app_config::KeyMode::Search);
        assert!(matches!(action_key(&normal, KeyChord::Escape), Some(Key::AllStates)));
        assert!(matches!(action_key(&search, KeyChord::Escape), Some(Key::AllStates)));
        assert!(matches!(action_key(&search, KeyChord::Control(21)), Some(Key::ClearSearch)));
        assert!(action_key(&normal, KeyChord::Control(21)).is_none());
    }

    #[test]
    fn input_modes_route_search_help_and_versions() {
        assert_eq!(dispatch_mode(None, true), DispatchMode::Search);
        assert_eq!(
            dispatch_mode(Some(&Overlay::Help), false),
            DispatchMode::Overlay
        );
        assert_eq!(
            dispatch_mode(
                Some(&Overlay::Versions {
                    sel: 0,
                    chosen: None,
                }),
                false,
            ),
            DispatchMode::Overlay
        );
        assert_eq!(dispatch_mode(None, false), DispatchMode::Normal);
    }

    #[test]
    fn tags_compare_numerically_not_as_strings() {
        assert!(newer_than("v0.1.7", "v0.1.6"));
        assert!(!newer_than("v0.1.6", "v0.1.7")); // the bug the user hit
        assert!(!newer_than("v0.1.7", "v0.1.7"));
        assert!(newer_than("v0.2.0", "v0.1.99"));
        assert!(newer_than("v1.0.0", "v0.99.99"));
        // string order puts v0.1.10 before v0.1.9 — numbers must not
        assert!(newer_than("v0.1.10", "v0.1.9"));
        assert!(!newer_than("v0.1.9", "v0.1.10"));
        // a shorter tag is the same as trailing zeros
        assert!(!newer_than("v0.1", "v0.1.0"));
        assert!(newer_than("v0.1.1", "v0.1"));
    }

    #[test]
    fn picker_selection_survives_refreshes() {
        let tags = vec!["v3".into(), "v2".into(), "v1".into()];
        assert_eq!(picker_sel(&tags, "v2", None, 0), 1);

        let reordered = vec!["v4".into(), "v3".into(), "v1".into(), "v2".into()];
        assert_eq!(picker_sel(&reordered, "v2", Some("v1"), 2), 2);
        assert_eq!(picker_sel(&reordered, "v2", Some("missing"), 1), 1);

        let shrunk = vec!["v3".into()];
        assert_eq!(picker_sel(&shrunk, "v2", Some("missing"), 9), 0);
        assert_eq!(picker_sel(&[], "v2", None, 9), 0);
    }

    #[test]
    fn bare_esc_clears_filters() {
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) };
        unsafe { libc::write(fds[1], [0x1bu8].as_ptr().cast(), 1) };
        assert!(matches!(read_key(fds[0], &default_keys(crate::app_config::KeyMode::Normal)), Key::AllStates));
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn update_notice_only_for_a_newer_release() {
        let dir = std::env::temp_dir().join(format!("agenmux-test-{}", std::process::id()));
        let file = dir.join("target/release/.agenmux-latest");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();

        assert_eq!(update_available(&dir), None); // no check has run yet
        std::fs::write(&file, format!("{}\n", current_tag())).unwrap();
        assert_eq!(update_available(&dir), None); // already on the newest
        std::fs::write(&file, "v9.9.9\n").unwrap();
        assert_eq!(update_available(&dir).as_deref(), Some("v9.9.9"));
        // the notice rides the header: a newline would push every list line
        // down one and break the click -> pane mapping
        assert!(!update_available(&dir).unwrap().contains('\n'));
        // the regression: any difference counted as an update, so a checkout
        // ahead of every release advertised "↑" for the older tag behind it
        std::fs::write(&file, "v0.0.1\n").unwrap();
        assert_eq!(update_available(&dir), None);
        // the tag is handed to update.sh as an argument
        std::fs::write(&file, "v1.0.0; rm -rf /\n").unwrap();
        assert_eq!(update_available(&dir), None);
        std::fs::write(&file, "garbage\n").unwrap();
        assert_eq!(update_available(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bar_reasserts_the_background_after_every_reset() {
        let line = format!("{E}[1mcodex{E}[0m {E}[2mwork{E}[0m");
        let bg = Palette::default().header_bg.bg();
        let painted = bar(&line, &bg, 14, 10);
        assert!(!painted
            .split(&format!("{E}[0m"))
            .any(|part| { !part.is_empty() && !part.starts_with(&bg) }));
        assert!(painted.ends_with(&format!("    {E}[0m")));
        assert_eq!(bar(&line, "", 14, 10), line);
        assert_eq!(
            Palette::default().state_bg("blocked", true),
            "\x1b[48;2;42;16;16m"
        );
        assert_eq!(
            Palette::default().state_bg("blocked", false),
            "\x1b[48;2;32;12;12m"
        );
    }

    #[test]
    fn watched_height_ignores_short_unwatched_windows() {
        // the regression: a 23-row window in a session nobody is looking at
        // used to clip the list in the 82-row window the user is watching
        let ms = [m("$0", 82, true), m("$1", 23, true), m("$0", 47, false)];
        assert_eq!(watched_height(&ms, "$0"), 82);
    }

    #[test]
    fn watched_height_falls_back_to_tallest_visible_then_tallest() {
        // no client measured yet: tallest among the active windows
        let ms = [m("$0", 40, true), m("$1", 23, true), m("$0", 99, false)];
        assert_eq!(watched_height(&ms, ""), 40);
        // nothing active at all: tallest overall
        let ms = [m("$0", 23, false), m("$1", 60, false)];
        assert_eq!(watched_height(&ms, "$0"), 60);
        assert_eq!(watched_height(&[], "$0"), 24);
    }

    fn filter_row(pane: &str, loc: &str, state: &str, title: &str) -> PaneRow {
        PaneRow {
            pane: pane.into(),
            loc: loc.into(),
            agent: "codex".into(),
            state: state.into(),
            cwd: "auth-service".into(),
            title: title.into(),
        }
    }

    #[test]
    fn text_search_matches_visible_fields_case_insensitively() {
        let rows = [filter_row("%1", "work:1.0", "idle", "Fix Login Race")];
        for query in ["CODEX", "work:1", "AUTH", "login", "idle"] {
            assert_eq!(filtered_indices(&rows, query, None), vec![0], "{query}");
        }
        assert!(filtered_indices(&rows, "payments", None).is_empty());
    }

    #[test]
    fn matching_session_keeps_its_agent_subtree() {
        let rows = [
            filter_row("%1", "api:1.0", "idle", "unrelated"),
            filter_row("%2", "api:2.0", "working", "also unrelated"),
            filter_row("%3", "web:1.0", "idle", "unrelated"),
        ];
        assert_eq!(filtered_indices(&rows, "api", None), vec![0, 1]);
    }

    #[test]
    fn state_filter_cycles_in_display_order() {
        let mut filter = None;
        for expected in [
            Some(StateFilter::Blocked),
            Some(StateFilter::Working),
            Some(StateFilter::Idle),
            Some(StateFilter::Done),
            None,
        ] {
            filter = StateFilter::cycle(filter);
            assert_eq!(filter, expected);
        }
    }

    #[test]
    fn state_filters_are_exact_and_separate_from_text() {
        let rows = [
            filter_row("%1", "s:1.0", "blocked", "working notes"),
            filter_row("%2", "s:2.0", "working", "blocked notes"),
            filter_row("%3", "s:3.0", "done", "done"),
        ];
        assert_eq!(
            filtered_indices(&rows, "ignored", Some(StateFilter::Blocked)),
            vec![0]
        );
        assert_eq!(
            filtered_indices(&rows, "", Some(StateFilter::Working)),
            vec![1]
        );
        assert_eq!(
            filtered_indices(&rows, "", Some(StateFilter::Done)),
            vec![2]
        );
    }
}
