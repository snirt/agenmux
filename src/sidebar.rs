// Sidebar TUI — runs inside the sidebar pane/popup. Port of sidebar.sh:
// same keys, same frame bytes, same rows/cache/pin file protocol, but one
// process, one tmux pipe, zero forks per tick.
use crate::conf::AgentConf;
use crate::procs::IdentCache;
use crate::scan::{self, PaneRow};
use crate::tmux::{Tmux, TmuxError};
use std::collections::HashMap;
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

const E: &str = "\x1b";
const SPIN: [char; 8] = ['⠹', '⢸', '⣰', '⣤', '⣆', '⡇', '⠏', '⠛'];

struct RawMode(Option<libc::termios>);

impl RawMode {
    fn enable() -> RawMode {
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

fn term_size() -> (usize, usize) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            return (ws.ws_col as usize, ws.ws_row as usize);
        }
    }
    (30, 24)
}

/// poll stdin; returns true when readable. timeout None = wait forever.
fn poll_stdin(timeout: Option<Duration>) -> bool {
    let mut fds = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
    unsafe { libc::poll(&mut fds, 1, ms) > 0 && fds.revents & libc::POLLIN != 0 }
}

/// Read one byte; None on EOF or error.
fn read_byte() -> Option<u8> {
    let mut b = [0u8; 1];
    let n = unsafe { libc::read(0, b.as_mut_ptr().cast(), 1) };
    (n == 1).then_some(b[0])
}

enum Key {
    Up,
    Down,
    Jump,
    Quit,
    Help,
    Other,
}

fn read_key() -> Key {
    let Some(b) = read_byte() else { return Key::Quit }; // EOF: explicit close
    match b {
        b'j' => Key::Down,
        b'k' => Key::Up,
        b'q' | 0x03 | 0x04 => Key::Quit, // q, Ctrl-C, Ctrl-D
        b'l' | b'\r' | b'\n' => Key::Jump,
        b'?' => Key::Help,
        0x1b => {
            // arrows deliver their bytes together; only bare Esc times out
            if !poll_stdin(Some(Duration::from_millis(50))) {
                return Key::Quit;
            }
            let (a, b2) = (read_byte(), read_byte());
            match (a, b2) {
                (Some(b'['), Some(b'A')) => Key::Up,
                (Some(b'['), Some(b'B')) => Key::Down,
                _ => Key::Other,
            }
        }
        _ => Key::Other,
    }
}

/// Per-pane memory across rescans (STATE_FILE equivalent).
struct Prev {
    state: String,
    ticks: u32,
    title: String,
}

pub struct Sidebar {
    tmux: Tmux,
    confs: Vec<AgentConf>,
    ident: IdentCache,
    prev: HashMap<String, Prev>,
    rows: Vec<PaneRow>, // debounced view-model
    sel: usize,         // 1-based like the bash script
    sel_pane: String,
    last_active: String,
    active: String,
    tick: u32,
    self_pane: String,
    pin: Option<String>,
    plugin_dir: PathBuf,
    rows_file: PathBuf,
    cache_file: PathBuf,
    last_frame: String,
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
    let confs = crate::conf::load_all(&plugin_dir);
    let mut sb = Sidebar {
        tmux,
        confs,
        ident: IdentCache::new(),
        prev: HashMap::new(),
        rows: Vec::new(),
        sel: 1,
        sel_pane: String::new(),
        last_active: String::new(),
        active: String::new(),
        tick: 0,
        self_pane,
        pin,
        plugin_dir,
        rows_file,
        cache_file,
        last_frame: String::new(),
    };

    // seed from the previous instance's scan for an instant first frame
    if let Ok(tsv) = std::fs::read_to_string(&sb.cache_file) {
        sb.rows = scan::from_tsv(&tsv);
        sb.rows.retain(|r| r.pane != sb.self_pane);
    }
    sb.render(true);

    let mut next_scan = Instant::now(); // scan immediately
    loop {
        if QUIT.load(Ordering::Relaxed) {
            break;
        }
        let now = Instant::now();
        if now >= next_scan {
            match sb.scan_tick() {
                Ok(()) => {}
                // a pipe I/O error can leave a response block half-read —
                // the pipe is desynced, restarting is the only safe move
                Err(TmuxError::Exited) | Err(TmuxError::Io(_)) => break,
                Err(TmuxError::Error(_)) => {} // e.g. pane died mid-scan
            }
            next_scan = now + Duration::from_secs(2);
            sb.render(false);
        }
        let animating = sb
            .rows
            .iter()
            .any(|r| matches!(r.state.as_str(), "working" | "blocked" | "done"));
        // animated states need ticks; all-idle sleeps until the next scan
        let wake = if animating {
            Duration::from_millis(250)
        } else {
            next_scan.saturating_duration_since(now)
        };
        if poll_stdin(Some(wake)) {
            match read_key() {
                Key::Down => sb.move_sel(1),
                Key::Up => sb.move_sel(-1),
                Key::Jump => {
                    if sb.jump() {
                        break;
                    }
                }
                Key::Help => sb.help(),
                Key::Quit => {
                    // q/Esc closes: popup pin removed so toggle.sh ends its loop
                    if let Some(p) = &sb.pin {
                        let _ = std::fs::remove_file(p);
                    }
                    break;
                }
                Key::Other => {}
            }
            sb.render(false);
        } else if animating {
            sb.tick = (sb.tick + 1) % 40; // divisible by 8 (spin) and 4 (blink)
            sb.render(false);
        }
        if WINCH.swap(false, Ordering::Relaxed) {
            print!("{E}[2J");
            sb.render(true);
        }
    }
    cleanup(&sb.rows_file, &sb.pin);
    0
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
        // remember current width so follow.sh can restore it after the
        // sidebar gets orphaned full-width (skip popup mode)
        if self.pin.is_none() && !self.self_pane.is_empty() {
            if let Ok(out) = self.tmux.run(&format!(
                "display-message -p -t '{}' '#{{pane_width}} #{{window_width}}'",
                self.self_pane
            )) {
                let mut it = out.split_whitespace();
                if let (Some(w), Some(ww)) = (it.next(), it.next()) {
                    if w != ww {
                        let _ = self
                            .tmux
                            .run(&format!("set-option -g @agents-mon-last-width {w}"));
                    }
                }
            }
        }

        let scanned = scan::scan(
            &mut self.tmux,
            &self.confs,
            &mut self.ident,
            Some(&self.self_pane),
        )?;
        let _ = std::fs::write(&self.cache_file, scan::to_tsv(&scanned));
        self.active = self.active_pane().unwrap_or_default();

        // idle debounce: show idle only after 2 consecutive idle ticks
        // (redraws flash idle-looking frames mid-render)
        let mut new_prev = HashMap::new();
        let mut rows = Vec::new();
        for mut r in scanned {
            let p = self.prev.get(&r.pane);
            let prev_state = p.map(|p| p.state.as_str()).unwrap_or("");
            let ticks = p.map(|p| p.ticks).unwrap_or(0);
            // agents like codex only title the pane while working
            if r.title.is_empty() {
                if let Some(p) = p {
                    r.title = p.title.clone();
                }
            }
            let (show, store, nticks) = if r.state == "idle"
                && !prev_state.is_empty()
                && prev_state != "idle"
                && prev_state != "done"
                && ticks < 1
            {
                // hold the previous state one tick before trusting idle
                (prev_state.to_string(), prev_state.to_string(), ticks + 1)
            } else if r.state == "idle"
                && r.pane != self.active
                && (prev_state == "working" || prev_state == "done")
            {
                // finished while unfocused — flag as done until viewed
                ("done".into(), "done".into(), 0)
            } else {
                (r.state.clone(), r.state.clone(), 0)
            };
            new_prev.insert(
                r.pane.clone(),
                Prev {
                    state: store,
                    ticks: nticks,
                    title: r.title.clone(),
                },
            );
            r.state = show;
            rows.push(r);
        }
        self.prev = new_prev;
        self.rows = rows;
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

    fn active_pane(&mut self) -> Option<String> {
        // first real (non-control-mode) client's current pane
        let sid = self
            .tmux
            .run("list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{session_id}'")
            .ok()?
            .lines()
            .next()?
            .to_string();
        let out = self
            .tmux
            .run(&format!("display-message -p -t '{sid}' '#{{pane_id}}'"))
            .ok()?;
        Some(out.trim().to_string())
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
        let client = self
            .tmux
            .run("list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{client_name}'")
            .ok()
            .and_then(|c| c.lines().next().map(str::to_string));
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

    fn render(&mut self, force: bool) {
        let (cols, trows) = term_size();
        let cap = trows.saturating_sub(1); // last row's newline would scroll
        let mut frame = format!("{E}[H{E}[1magents{E}[0m{E}[K\n{E}[K\n");
        let mut vis = String::new();
        let mut session = "";
        let mut used = 2usize; // header + blank line already emitted
        if self.rows.is_empty() {
            frame.push_str(&format!("{E}[2mno agents{E}[0m{E}[K\n"));
        } else {
            for (n, r) in self.rows.iter().enumerate() {
                let sess = r.loc.split(':').next().unwrap_or("");
                if sess != session {
                    if used + 2 > cap {
                        break; // no room for header + record
                    }
                    session = sess;
                    frame.push_str(&format!("{E}[1;34m{sess}{E}[0m{E}[K\n"));
                    vis.push_str("-\n");
                    used += 1;
                }
                if used >= cap {
                    break; // pane full — clip, never scroll
                }
                let mark = if n + 1 == self.sel {
                    format!("{E}[1m❯{E}[0m ")
                } else {
                    "  ".into()
                };
                let dot = self.dot(&r.state);
                let win = r.loc.splitn(2, ':').nth(1).unwrap_or("");
                let mut rest = format!("{win} {}", r.cwd);
                let agent_len = r.agent.chars().count();
                let avail = cols.saturating_sub(5 + agent_len);
                if avail > 0 {
                    rest = rest.chars().take(avail).collect();
                }
                frame.push_str(&format!(
                    "{mark}{dot} {E}[1m{}{E}[0m {E}[2m{rest}{E}[0m{E}[K\n",
                    r.agent
                ));
                vis.push_str(&r.pane);
                vis.push('\n');
                used += 1;
                if !r.title.is_empty() && used < cap {
                    let t: String = r.title.chars().take(cols.saturating_sub(4)).collect();
                    frame.push_str(&format!("    {E}[2m{t}{E}[0m{E}[K\n"));
                    vis.push_str(&r.pane);
                    vis.push('\n');
                    used += 1;
                }
            }
        }
        frame.push_str(&format!("{E}[J"));
        let _ = std::fs::write(&self.rows_file, &vis);
        if force || frame != self.last_frame {
            print!("{frame}");
            let _ = std::io::stdout().flush();
            self.last_frame = frame;
        }
    }

    fn help(&mut self) {
        print!(
            "{E}[2J{E}[H{E}[1magents — help{E}[0m\n\n\
{E}[1mstatus{E}[0m\n\
 {E}[32m⣿{E}[0m  idle\n\
 {E}[33m⠹{E}[0m  working (spinner)\n\
 {E}[31m⣿{E}[0m  blocked, waiting for input (blinks)\n\
 {E}[32m⣿{E}[0m  done, not viewed yet (blinks)\n\n\
{E}[1mkeys{E}[0m\n\
 j/k ↑/↓  move selection\n\
 Enter/l  jump to agent\n\
 q Esc    close sidebar\n\
 ?        this help\n\n\
{E}[2mpress any key to return{E}[0m"
        );
        let _ = std::io::stdout().flush();
        // blocks until a key; animations pause meanwhile
        if poll_stdin(None) {
            let _ = read_byte();
        }
        print!("{E}[2J");
        self.last_frame.clear();
    }
}
