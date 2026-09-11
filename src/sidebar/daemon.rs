use crate::pane_writers::PaneWriters;
use crate::panes;
use crate::tmux::{command_status, Tmux};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{event_loop, new_sidebar, on_term, Sidebar};

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

/// Has `@agenmux-control-client` named someone else? Only a claim counts:
/// teardown.sh's unset can land after the next daemon claimed it, and treating
/// empty as "replaced" makes a reopened sidebar kill its own daemon.
fn superseded(mine: &str, current: &str) -> bool {
    let current = current.trim();
    !mine.is_empty() && !current.is_empty() && current != mine
}

/// Headless-mode state: frames → visible panes, keys ← FIFO, size ← panes.
pub(super) struct Daemon {
    pub(super) keys_path: PathBuf,
    pub(super) keys_fd: libc::c_int,
    pub(super) writers: PaneWriters,
    pub(super) size: (usize, usize), // narrowest pane x watched pane's height
    pub(super) seen_mirror: bool,    // suicide only arms after the first pane appears
    pub(super) empty_ticks: u32,     // consecutive measurements that found no pane
    pub(super) client: String,       // our control client, as published in the option
    pub(super) started: Instant,
    // window id -> (window size, pane count) at the last measure: a mirror
    // whose width changed while both stayed put is a user border-drag. The
    // pane count matters: a closing pane hands its columns to the mirror
    // (all of them, when the mirror is the last pane left) without changing
    // the window size — width-only would adopt that as the global width
    pub(super) win_sizes: HashMap<String, ((usize, usize), usize)>,
    // session the control client is attached to: layout/focus notifications
    // are session-scoped, so the client follows the user's active session
    pub(super) attached: String,
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
    let tmux = match Tmux::connect_monitoring() {
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

impl Sidebar {
    /// Refresh preserved-pane inventory: min pane size drives the render, zero
    /// panes (after at least one existed, or a 30s startup grace) = false.
    /// Also detects a user dragging a sidebar border — width changed while
    /// the window size and pane count did not, in a window the user can
    /// actually see — and
    /// adopts it as the global width. Serialization matters: one daemon
    /// doing this (instead of racing hook scripts) means no stale
    /// resize-pane ever fights the drag, and the dragged pane itself is
    /// never touched — only the hidden sidebars in other windows move.
    pub(super) fn superseded(&mut self) -> bool {
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

    pub(super) fn mirror_tick(&mut self) -> bool {
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

    pub(super) fn teardown(&mut self) {
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
