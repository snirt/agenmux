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
use crate::attention::Tracker;
use crate::conf::AgentConf;
use crate::pane_writers::PaneWriters;
use crate::procs::IdentCache;
use crate::scan::{self, PaneRow};
use crate::tmux::{Tmux, TmuxError};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static WINCH: AtomicBool = AtomicBool::new(false);
static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_: libc::c_int) {
    WINCH.store(true, Ordering::Relaxed);
}
extern "C" fn on_term(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

pub(crate) const E: &str = "\x1b";
const SPIN: [char; 8] = ['⠹', '⢸', '⣰', '⣤', '⣆', '⡇', '⠏', '⠛'];

fn cursor_mark(selected: bool, plugin_selected: bool) -> String {
    if !selected {
        return "  ".into();
    }
    if plugin_selected {
        format!("{E}[1;32m❯{E}[0m ")
    } else {
        format!("{E}[1m❯{E}[0m ")
    }
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

enum Key {
    Up,
    Down,
    Jump,
    Quit,
    Close,
    Help,
    Versions,
    Other,
}

enum Overlay {
    Help,
    Versions {
        sel: usize,
        chosen: Option<String>,
    },
}

fn read_key(fd: libc::c_int) -> Key {
    let Some(b) = read_byte(fd) else { return Key::Quit }; // EOF: explicit close
    match b {
        b'j' => Key::Down,
        b'k' => Key::Up,
        b'q' | 0x03 | 0x04 => Key::Quit, // q, Ctrl-C, Ctrl-D
        b'Q' => Key::Close,
        b'l' | b'\r' | b'\n' => Key::Jump,
        b'?' => Key::Help,
        b'u' => Key::Versions,
        // Every byte of the tail goes through the same polling reader. In mirror
        // mode keys arrive over a non-blocking FIFO that the key sender feeds one
        // byte at a time, so the tail is routinely still in flight; reading it
        // without polling hit EAGAIN and dropped every other arrow.
        0x1b => escape_key(|| {
            poll_fd(fd, Some(Duration::from_millis(50)))
                .then(|| read_byte(fd))
                .flatten()
        }),
        _ => Key::Other,
    }
}

/// Deliver one key-table action to the daemon without waiting for a FIFO
/// reader. Each invocation is intentionally short-lived; the daemon remains
/// the only persistent agents-mon process.
pub fn send_key(name: &str) -> i32 {
    let bytes: &[u8] = match name {
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "enter" => b"\r",
        "escape" => b"\x1b",
        "space" => b" ",
        "j" => b"j",
        "k" => b"k",
        "l" => b"l",
        "q" => b"q",
        "close" => b"Q",
        "help" => b"?",
        "versions" => b"u",
        _ => return 2,
    };
    let path = std::env::temp_dir().join("agents-mon-keys");
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

/// Decode the tail of an escape sequence. `next` yields the next byte, or None
/// once nothing more arrives — a bare Esc, which closes.
fn escape_key(mut next: impl FnMut() -> Option<u8>) -> Key {
    let Some(a) = next() else { return Key::Quit };
    // CSI (ESC [ A) normally, SS3 (ESC O A) in application-cursor mode
    match (a, next()) {
        (b'[' | b'O', Some(b'A')) => Key::Up,
        (b'[' | b'O', Some(b'B')) => Key::Down,
        _ => Key::Other,
    }
}

/// The release this engine belongs to. install-bin.sh installs the binary that
/// matches the checkout's Cargo.toml, so this is also the plugin's version.
fn current_tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Newest release, as recorded by install-bin.sh's (at most daily) check.
/// None unless it is strictly newer than what is running: a checkout ahead of
/// every release (master, or a just-bumped manifest) must not be told to
/// "update" to the older tag behind it.
fn update_available(plugin_dir: &PathBuf) -> Option<String> {
    let latest =
        std::fs::read_to_string(plugin_dir.join("target/release/.agents-mon-latest")).ok()?;
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
        let (l, r) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        if l != r {
            return l > r;
        }
    }
    false
}

/// Releases install-bin.sh saw on the remote, newest first.
fn known_tags(plugin_dir: &PathBuf) -> Vec<String> {
    let mut tags: Vec<String> =
        std::fs::read_to_string(plugin_dir.join("target/release/.agents-mon-tags"))
            .unwrap_or_default()
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
        .or_else(|| chosen.is_none().then(|| tags.iter().position(|t| t == cur)).flatten())
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

/// Has `@agents-mon-control-client` named someone else? Only a claim counts:
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
    client: String, // our control client, as published in the option
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
    confs: Vec<AgentConf>,
    ident: IdentCache,
    subj: scan::SubjectCache,
    tracker: Tracker,
    rows: Vec<PaneRow>, // debounced view-model
    sel: usize,         // 1-based like the bash script
    scroll: usize,      // first visible list line — follows the selection
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
) -> Sidebar {
    let confs = crate::conf::load_all(&plugin_dir);
    // read once: the check behind it runs at most daily, and switching version
    // restarts the engine anyway
    let update = update_available(&plugin_dir);
    let mut sb = Sidebar {
        tmux,
        confs,
        ident: IdentCache::new(),
        subj: scan::SubjectCache::new(),
        tracker: Tracker::default(),
        rows: Vec::new(),
        sel: 1,
        scroll: 0,
        sel_pane: String::new(),
        last_active: String::new(),
        active: String::new(),
        active_session: String::new(),
        plugin_selected: false,
        tick: 0,
        self_pane,
        pin: None,
        popup_client: std::env::var("AGENTS_MON_POPUP_CLIENT").unwrap_or_default(),
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
    sb
}

pub fn run(plugin_dir: PathBuf, cache_file: PathBuf) -> i32 {
    let self_pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let pin = std::env::var("AGENTS_MON_PIN").ok().filter(|p| !p.is_empty());
    let rows_file = std::env::temp_dir().join(format!(
        "agents-mon-rows-{}",
        self_pane.trim_start_matches('%')
    ));

    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_term as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as libc::sighandler_t);
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
    let mut sb = new_sidebar(tmux, plugin_dir, cache_file, rows_file, self_pane);
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
    unsafe {
        libc::signal(libc::SIGTERM, on_term as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as libc::sighandler_t);
    }
    let tmp = std::env::temp_dir();
    let keys_path = tmp.join("agents-mon-keys");
    // A previous mirror-based daemon used this as its liveness heartbeat.
    let _ = std::fs::remove_file(tmp.join("agents-mon-frame"));
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
        Err(_) => return 0,
    };
    // "" not TMUX_PANE: toggle.sh launches the daemon from the pane the user
    // pressed the key in, and adopting that pane would hide its agent
    let mut sb = new_sidebar(
        tmux,
        plugin_dir,
        cache_file,
        tmp.join("agents-mon-rows"),
        String::new(),
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
            let _ = sb.tmux.run(&format!(
                "set-option -g @agents-mon-control-client '{quoted}'"
            ));
            control_client = client;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
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
            sb.render(false);
            // a scan takes tens of ms — with the pre-scan `now`, a tick due
            // mid-scan is missed and the poll sleeps its full stale remainder
            now = Instant::now();
            next_scan = now + Duration::from_secs(2);
        }
        let animating = sb
            .rows
            .iter()
            .any(|r| matches!(r.state.as_str(), "working" | "blocked" | "done"));
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
            let key = read_key(key_fd);
            if sb.overlay.is_some() {
                sb.overlay_key(key);
                sb.render(false);
                continue;
            }
            match key {
                Key::Down => sb.move_sel(1),
                Key::Up => sb.move_sel(-1),
                Key::Jump => {
                    if sb.jump() {
                        break;
                    }
                }
                Key::Help => sb.help(),
                Key::Versions => sb.versions(),
                Key::Quit => {
                    if sb.daemon.is_none() {
                        // Popup/tty mode owns stdin, so q/Esc still closes it.
                        if let Some(p) = &sb.pin {
                            let _ = std::fs::remove_file(p);
                        }
                        break;
                    }
                    // In preserved-pane mode close arrives as Key::Close from
                    // the key table; Quit also covers FIFO EOF, which must not
                    // kill the sidebar.
                }
                Key::Close => {
                    break;
                }
                Key::Other => {}
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
        // keep the pin when a jump is pending — toggle.sh reopens the popup
        if !std::path::Path::new(&format!("{p}.jump")).exists() {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Sidebar {
    fn scan_tick(&mut self) -> Result<(), TmuxError> {
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
            let _ = crate::notifications::deliver(&mut self.tmux, event);
        }
        self.clamp_sel();
        self.restore_sel();
        // single cursor: focus landing on an agent pane snaps selection to it
        if !self.active.is_empty() && self.active != self.last_active {
            if let Some(i) = self.rows.iter().position(|r| r.pane == self.active) {
                self.sel = i + 1;
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

    fn move_sel(&mut self, d: i64) {
        self.sel = (self.sel as i64 + d).max(1) as usize;
        self.clamp_sel();
        self.sync_sel_pane();
    }

    fn clamp_sel(&mut self) {
        if self.sel > self.rows.len() {
            self.sel = self.rows.len();
        }
        if self.sel < 1 {
            self.sel = 1;
        }
    }

    fn sync_sel_pane(&mut self) {
        self.sel_pane = self
            .rows
            .get(self.sel.wrapping_sub(1))
            .map(|r| r.pane.clone())
            .unwrap_or_default();
    }

    fn restore_sel(&mut self) {
        // after a rescan, follow the remembered pane's new position
        if self.sel_pane.is_empty() {
            self.sync_sel_pane();
            return;
        }
        match self.rows.iter().position(|r| r.pane == self.sel_pane) {
            Some(i) => self.sel = i + 1,
            None => {
                self.clamp_sel();
                self.sync_sel_pane();
            }
        }
    }

    /// true = exit the loop (popup jump hands off to toggle.sh)
    fn jump(&mut self) -> bool {
        let Some(target) = self
            .rows
            .get(self.sel.wrapping_sub(1))
            .map(|r| r.pane.clone())
        else {
            return false;
        };
        if !target.starts_with('%') {
            return false;
        }
        if let Some(pin) = &self.pin {
            // popup holds the client — hand the target to toggle.sh, which
            // jumps after the popup closes
            let _ = std::fs::write(format!("{pin}.jump"), &target);
            return true;
        }
        // move the sidebar into the target window BEFORE switching the view —
        // the join-pane reflow happens off-screen (no flash on arrival)
        let follow = self.plugin_dir.join("scripts/follow.sh");
        let _ = std::process::Command::new("bash")
            .arg(follow)
            .arg(&target)
            .status();
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
        let mut cmd = std::process::Command::new("tmux");
        if let Some(client) = &client {
            cmd.args(["switch-client", "-c", client, "-t", &target, ";"]);
        }
        let _ = cmd
            .args(["select-window", "-t", &target, ";", "select-pane", "-t", &target])
            .status();
        false
    }

    fn dot(&self, state: &str) -> String {
        let on = self.tick / 2 % 2 == 0;
        match state {
            "blocked" => {
                if on {
                    format!("{E}[31m⣿{E}[0m")
                } else {
                    " ".into()
                }
            }
            "working" => format!("{E}[33m{}{E}[0m", SPIN[(self.tick % 8) as usize]),
            "done" => {
                if on {
                    format!("{E}[32m⣿{E}[0m")
                } else {
                    " ".into()
                }
            }
            _ => format!("{E}[32m⣿{E}[0m"),
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
        match self.tmux.run("show-option -gqv @agents-mon-control-client") {
            Ok(current) => superseded(&mine, &current),
            Err(_) => false, // a broken pipe is not a takeover; the loop exits elsewhere
        }
    }

    fn mirror_tick(&mut self) -> bool {
        // same barrier as active_pane: reading zero panes off a desynced
        // pipe used to tear the whole mirror set down
        let _ = self.tmux.sync();
        let out = self
            .tmux
            .run("list-panes -a -f '#{==:#{pane_title},agents-mon}' -F '#{pane_id}\t#{window_id}\t#{pane_width}\t#{pane_height}\t#{window_width} #{window_height}\t#{window_panes}\t#{window_active}\t#{session_id}'")
            .unwrap_or_default();
        let mut w = usize::MAX;
        let mut ms: Vec<M> = Vec::new();
        for l in out.lines() {
            let f: Vec<&str> = l.split('\t').collect();
            let [pane, win, pw, ph, ws, wp, act, sess] = f.as_slice() else { continue };
            let (Ok(pw), Ok(ph), Ok(wp)) =
                (pw.parse::<usize>(), ph.parse::<usize>(), wp.parse::<usize>())
            else {
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
        let wopt: usize = self
            .tmux
            .run("show-option -gqv @agents-mon-width")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(30);
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
            let _ = std::process::Command::new("tmux").args(&argv).status();
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
                    m.active
                        && m.w != wopt
                        && d.win_sizes.get(&m.win) == Some(&(m.win_size, m.panes))
                })
                .map(|m| (m.pane.clone(), m.w))
        };
        if let Some((src_pane, width)) = drag {
            let _ = self
                .tmux
                .run(&format!("set-option -g @agents-mon-width {width}"));
            // resize the OTHER sidebars via forked tmux (hook run-shell
            // echoes on the control pipe would desync it); the dragged pane
            // stays untouched so nothing ever fights the user's drag
            let mut cmd = std::process::Command::new("tmux");
            let mut any = false;
            for m in ms.iter().filter(|m| m.pane != src_pane && m.w != width) {
                if any {
                    cmd.arg(";");
                }
                cmd.args(["resize-pane", "-t", &m.pane, "-x", &width.to_string()]);
                any = true;
            }
            if any {
                let _ = cmd.status();
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
    /// restore layouts via a
    /// forked script (hook run-shell echoes would desync the control pipe),
    /// then drop the key FIFO and row map.
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
        let script = self.plugin_dir.join("scripts/teardown.sh");
        let _ = std::process::Command::new("bash").arg(script).status();
        if let Some(d) = &self.daemon {
            let _ = std::fs::remove_file(&d.keys_path);
            unsafe { libc::close(d.keys_fd) };
        }
        let _ = std::fs::remove_file(&self.rows_file);
    }

    /// Frame sink: stdout in tty mode; direct writes to the visible empty panes
    /// in daemon mode.
    fn emit(&mut self, frame: String, force: bool) {
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
        let space = cap.saturating_sub(2); // header + blank line

        // update notice rides the header, never a list line: rows below it map
        // to panes by line number (see vis/rows_file), an extra row would shift
        // every click by one
        // the hint goes on the blank line that already sits under the header,
        // so the frame keeps exactly two lines above the list either way
        let (notice, hint) = match &self.update {
            Some(t) => (
                format!(" {E}[2m↑{}{E}[0m", t.trim_start_matches('v')),
                format!("{E}[2mpress u to update{E}[0m"),
            ),
            None => (String::new(), String::new()),
        };
        let mut frame = format!("{E}[H{E}[1magents{E}[0m{notice}{E}[K\n{hint}{E}[K\n");
        let mut vis = String::new();
        if self.rows.is_empty() {
            frame.push_str(&format!("{E}[2mno agents{E}[0m{E}[K\n"));
        } else {
            // build the full list, then window it so the selection stays visible
            let mut lines: Vec<(String, &str)> = Vec::new(); // (text, vis pane)
            let (mut sel_top, mut sel_bot) = (0usize, 0usize);
            let mut session = "";
            for (n, r) in self.rows.iter().enumerate() {
                let sess = r.loc.split(':').next().unwrap_or("");
                if sess != session {
                    session = sess;
                    // clip to pane width — a wrapped header shifts every row
                    // below it and breaks the click→rows-file mapping
                    let sess_clipped: String = sess.chars().take(cols).collect();
                    lines.push((format!("{E}[1;34m{sess_clipped}{E}[0m{E}[K\n"), "-"));
                }
                if n + 1 == self.sel {
                    sel_top = lines.len();
                }
                let mark = cursor_mark(n + 1 == self.sel, self.plugin_selected);
                let dot = self.dot(&r.state);
                let win = r.loc.splitn(2, ':').nth(1).unwrap_or("");
                let mut rest = format!("{win} {}", r.cwd);
                let agent_len = r.agent.chars().count();
                let avail = cols.saturating_sub(5 + agent_len);
                if avail > 0 {
                    rest = rest.chars().take(avail).collect();
                }
                lines.push((
                    format!("{mark}{dot} {E}[1m{}{E}[0m {E}[2m{rest}{E}[0m{E}[K\n", r.agent),
                    &r.pane,
                ));
                if !r.title.is_empty() {
                    let t: String = r.title.chars().take(cols.saturating_sub(4)).collect();
                    lines.push((format!("    {E}[2m{t}{E}[0m{E}[K\n"), &r.pane));
                }
                if n + 1 == self.sel {
                    sel_bot = lines.len() - 1;
                }
            }
            // selection's session header gives context — drag it into view
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
                self.scroll = self.scroll.min(lines.len().saturating_sub(space));
            } else {
                self.scroll = 0;
            }
            let end = (self.scroll + space).min(lines.len());
            for (text, pane) in &lines[self.scroll..end] {
                frame.push_str(text);
                vis.push_str(pane);
                vis.push('\n');
            }
        }
        frame.push_str(&format!("{E}[J"));
        let _ = std::fs::write(&self.rows_file, &vis);
        self.emit(frame, force);
    }

    /// Version picker: update or roll back to any release the last check saw.
    /// Selecting one hands off to update.sh, which switches the source, the
    /// engine, and restarts the view.
    fn versions(&mut self) {
        // opening the picker is an explicit "what is out there?" — ask now
        // instead of serving a list that the daily check may have left a day
        // old. It lands in the file and normal scan renders pick it up live.
        let _ = std::process::Command::new("bash")
            .arg(self.plugin_dir.join("scripts/install-bin.sh"))
            .arg("refresh")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        self.overlay = Some(Overlay::Versions {
            sel: 0,
            chosen: None,
        });
        self.last_frame.clear();
    }

    fn render_overlay(&mut self, force: bool) {
        let text = match &mut self.overlay {
            Some(Overlay::Help) => {
                let quit_keys = " q Esc    close sidebar";
                format!(
                    "{E}[2J{E}[H{E}[1magents — help{E}[0m {E}[2m{}{E}[0m\n\n\
{E}[1mstatus{E}[0m\n\
 {E}[32m⣿{E}[0m  idle\n\
 {E}[33m⠹{E}[0m  working (spinner)\n\
 {E}[31m⣿{E}[0m  blocked, waiting for input (blinks)\n\
 {E}[32m⣿{E}[0m  done, not viewed yet (blinks)\n\n\
{E}[1mkeys{E}[0m\n\
 j/k ↑/↓  move selection\n\
 Enter/l  jump to agent\n\
 u        update / switch version\n\
{quit_keys}\n\
 ?        this help\n\n\
{E}[2mpress any key to return{E}[0m",
                    current_tag()
                )
            }
            Some(Overlay::Versions { sel, chosen }) => {
                let cur = current_tag();
                let tags = known_tags(&self.plugin_dir);
                *sel = picker_sel(&tags, &cur, chosen.as_deref(), *sel);
                let mut text = format!(
                    "{E}[2J{E}[H{E}[1magents — versions{E}[0m {E}[2m{cur}{E}[0m\n\n"
                );
                if tags.is_empty() {
                    text.push_str(&format!(
                        " {E}[2mno releases found — checking…{E}[0m\n\n\
                         {E}[2mq back{E}[0m"
                    ));
                } else {
                    for (i, t) in tags.iter().enumerate() {
                        let mark = cursor_mark(i == *sel, true);
                        let tail = if *t == cur {
                            format!(" {E}[2m(current){E}[0m")
                        } else {
                            String::new()
                        };
                        text.push_str(&format!("{mark}{t}{tail}\n"));
                    }
                    text.push_str(&format!("\n{E}[2m↵ switch · j/k ↑/↓ · q back{E}[0m"));
                }
                text
            }
            None => return,
        };
        self.emit(text, force);
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
            if title == "agents-mon" {
                let _ = std::process::Command::new("tmux")
                    .args(["switch-client", "-c", client, "-T", "agents-mon"])
                    .spawn();
            }
        }
    }

    /// nohup + no wait: update.sh kills the panes this engine renders into,
    /// and a pane kill would otherwise SIGHUP the switch halfway through.
    fn switch_version(&mut self, tag: &str) {
        let script = self.plugin_dir.join("scripts/update.sh");
        let _ = std::process::Command::new("nohup")
            .arg("bash")
            .arg(script)
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
    fn cursor_is_green_only_while_plugin_is_selected() {
        assert_eq!(cursor_mark(true, true), format!("{E}[1;32m❯{E}[0m "));
        assert_eq!(cursor_mark(true, false), format!("{E}[1m❯{E}[0m "));
        assert_eq!(cursor_mark(false, true), "  ");
    }

    #[test]
    fn arrows_work_in_both_cursor_key_modes() {
        // the regression: only CSI was decoded, so arrows did nothing in panes
        // tmux had put in application-cursor mode (it sends SS3 there)
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let feed = |b: &[u8]| unsafe { libc::write(fds[1], b.as_ptr().cast(), b.len()) };

        for seq in [b"\x1b[A".as_slice(), b"\x1bOA".as_slice()] {
            feed(seq);
            assert!(matches!(read_key(fds[0]), Key::Up), "up: {seq:?}");
        }
        for seq in [b"\x1b[B".as_slice(), b"\x1bOB".as_slice()] {
            feed(seq);
            assert!(matches!(read_key(fds[0]), Key::Down), "down: {seq:?}");
        }
        feed(b"j");
        assert!(matches!(read_key(fds[0]), Key::Down));
        feed(b"u");
        assert!(matches!(read_key(fds[0]), Key::Versions));
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    /// Feed escape_key a scripted tail. Every byte goes through the same
    /// reader, which is the point: mirror mode delivers keys over a
    /// non-blocking FIFO one byte at a time, so a tail byte that has not
    /// arrived yet must be waited for, not read blind. Reading the pair
    /// without polling hit EAGAIN and dropped every other arrow.
    fn decode(tail: &[Option<u8>]) -> Key {
        let mut it = tail.iter().copied();
        escape_key(move || it.next().flatten())
    }

    #[test]
    fn escape_tails_decode_in_both_cursor_key_modes() {
        assert!(matches!(decode(&[Some(b'['), Some(b'A')]), Key::Up));
        assert!(matches!(decode(&[Some(b'['), Some(b'B')]), Key::Down));
        assert!(matches!(decode(&[Some(b'O'), Some(b'A')]), Key::Up)); // SS3
        assert!(matches!(decode(&[Some(b'O'), Some(b'B')]), Key::Down));
        assert!(matches!(decode(&[]), Key::Quit)); // bare Esc closes
        assert!(matches!(decode(&[None]), Key::Quit));
        // a tail that never completes is not a close — Esc already decided that
        assert!(matches!(decode(&[Some(b'['), None]), Key::Other));
        assert!(matches!(decode(&[Some(b'['), Some(b'Z')]), Key::Other));
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
    fn bare_esc_still_quits() {
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) };
        unsafe { libc::write(fds[1], [0x1bu8].as_ptr().cast(), 1) };
        assert!(matches!(read_key(fds[0]), Key::Quit));
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn update_notice_only_for_a_newer_release() {
        let dir = std::env::temp_dir().join(format!("agents-mon-test-{}", std::process::id()));
        let file = dir.join("target/release/.agents-mon-latest");
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
}
